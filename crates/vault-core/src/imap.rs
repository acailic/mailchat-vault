//! IMAP ingest: incremental, UID-based sync of mail folders into the vault.
//!
//! This is the first "live" mail source: it connects to an IMAP server —
//! Gmail via app password in this first round (TODO: XOAUTH2/OAuth2 login
//! later), or any other server over host/port/user/password — and pulls new
//! messages into the archive.
//!
//! # Source identity (durable source instances)
//!
//! Each synced account is a **durable source instance** in the vault
//! database, registered from the canonical connector configuration:
//! case-folded host, port, TLS mode, and exact username
//! ([`ImapConfig::connector_key`]). Re-registering the same configuration
//! resolves to the same instance, so re-syncing — or reconfiguring the
//! *password*, which is never part of the stored configuration — preserves
//! identity. Two accounts on different hosts stay distinct instances even
//! with the same username, because the host (unlike the pre-canonical
//! `imap:<username>` namespace) is part of the key. Message identities are
//! connector-scoped to the instance: the normalized `Message-ID` (or the
//! SHA-256 of the raw bytes when absent) with a semantic revision hash, so
//! the same mail seen in several folders is one archived message with
//! several occurrences, while two conflicting payloads behind one
//! `Message-ID` are preserved as two revisions (see [`crate::storage`]).
//!
//! # Incremental sync model
//!
//! Per folder, a sync cursor in the vault database stores the folder's
//! `UIDVALIDITY` and the highest already-fetched UID, keyed by the source
//! instance. On each sync:
//!
//! - if `UIDVALIDITY` is unchanged, only messages with UIDs above the stored
//!   cursor are searched and fetched (`UID SEARCH last+1:*`, explicit UID
//!   sets for the `FETCH`es);
//! - if `UIDVALIDITY` changed (the folder was recreated; all UIDs are
//!   reassigned), the cursor resets and the folder is re-scanned from UID 1
//!   — re-fetched messages simply dedup by their connector identity, so a
//!   reset never duplicates rows.
//!
//! The IMAP folder name maps to the message's `channel` field, so archived
//! messages are grouped per folder.
//!
//! # Credentials
//!
//! [`ImapConfig`] holds the password only in memory for the duration of a
//! sync. It is never written to the vault database (the source instance
//! stores host/port/TLS/username configuration and folder cursors only)
//! and `Debug` output is redacted. The Tauri shell keeps entered passwords
//! in the OS keyring when available and otherwise in process memory only —
//! see `src-tauri/src/secrets.rs`.
//!
//! # Testing
//!
//! The production path connects over implicit TLS (rustls). Tests inject an
//! unencrypted session instead ([`sync_session`] is generic over any
//! `Read + Write` stream) and talk to an in-process mock IMAP server; see
//! `tests/imap_sync.rs` and `tests/imap_mbox_equivalence.rs`. A manual
//! real-server test (`tests/imap_gmail_manual.rs`) is `#[ignore]`d and not
//! run in CI.

use std::collections::HashSet;
use std::fmt;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use imap::Session;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::mail::{amend_raw_json, parse_raw_mail_canonical};
use crate::message::NewMessage;
use crate::storage::{
    ConnectorIdentity, MailInsertOutcome, NewSourceInstance, Occurrence, Storage,
};
use crate::Result;

/// Gmail IMAP endpoint (implicit TLS).
pub const GMAIL_IMAP_HOST: &str = "imap.gmail.com";
/// Port for implicit-TLS IMAP (`imaps`).
pub const GMAIL_IMAP_PORT: u16 = 993;
/// Folders synced when none are configured.
pub const DEFAULT_FOLDERS: &[&str] = &["INBOX"];

/// How many messages one `UID FETCH` request covers (large folders are
/// fetched in batches so partial progress survives an interrupted sync).
const FETCH_BATCH: usize = 50;

/// Socket timeouts for the production connection.
const IO_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound on the TCP connect phase, so a blackholed host fails fast instead
/// of hanging on the OS default (which can take minutes).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// TLS stream type used by [`connect`].
pub type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// Dedup source name for a legacy IMAP account: `imap:<username>`.
///
/// Pre-canonical namespace kept by migrated archives; it deliberately
/// carries no host, so the old schema could not tell two hosts apart for
/// one username. New syncs register host/port/TLS/username-accurate durable
/// source instances instead ([`ensure_source_instance`]); this name remains
/// only to read the legacy snapshot's namespace.
pub fn source_name(username: &str) -> String {
    format!("imap:{username}")
}

/// The one TLS mode this release speaks: implicit TLS on a dedicated port
/// (993). It is part of the connector identity key, so a future STARTTLS
/// mode will be a distinct configuration, never a silent reinterpretation
/// of an existing instance.
pub const TLS_MODE: &str = "implicit-tls";

/// Connection settings for one IMAP account.
///
/// The password lives only in memory for the duration of a sync; it is
/// never persisted and never printed (manual `Debug` redacts it).
#[derive(Clone)]
pub struct ImapConfig {
    /// IMAP host, e.g. `imap.gmail.com`.
    pub host: String,
    /// IMAP port; 993 (implicit TLS) for Gmail.
    pub port: u16,
    /// Account username (full address for Gmail).
    pub username: String,
    /// App password (Gmail) or account password. Never persisted.
    pub password: String,
    /// Folders to sync; defaults to `["INBOX"]` when empty.
    pub folders: Vec<String>,
}

impl ImapConfig {
    /// Gmail preset: host/port fixed, folders default to `INBOX`.
    pub fn gmail(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            host: GMAIL_IMAP_HOST.to_string(),
            port: GMAIL_IMAP_PORT,
            username: username.into(),
            password: password.into(),
            folders: Vec::new(),
        }
    }

    /// The canonical connector identity key: kind, case-folded host, port,
    /// TLS mode, and the exact username.
    ///
    /// Hostnames are case-folded (`IMAP.Gmail.COM` and `imap.gmail.com` are
    /// one server); usernames are not — the identity contract keeps them
    /// exactly as configured. The password is never part of the key, so
    /// reconfiguring credentials preserves source identity, and two
    /// accounts on different hosts remain distinct instances even with the
    /// same username.
    pub fn connector_key(&self) -> String {
        format!(
            "imap|{}|{}|{TLS_MODE}|{}",
            self.host.trim().to_lowercase(),
            self.port,
            self.username
        )
    }

    /// The non-secret connector configuration stored on the source
    /// instance: canonical host, port, TLS mode, exact username, and the
    /// folder selection. Credentials are never included.
    pub fn config_json(&self) -> String {
        serde_json::json!({
            "host": self.host.trim().to_lowercase(),
            "port": self.port,
            "tls_mode": TLS_MODE,
            "username": self.username,
            "folders": self.effective_folders(),
        })
        .to_string()
    }

    /// The folders to sync (`DEFAULT_FOLDERS` when none configured,
    /// duplicates removed).
    pub fn effective_folders(&self) -> Vec<String> {
        let folders = if self.folders.is_empty() {
            DEFAULT_FOLDERS.iter().map(|s| s.to_string()).collect()
        } else {
            self.folders.clone()
        };
        let mut seen = HashSet::new();
        folders
            .into_iter()
            .filter(|f| seen.insert(f.clone()))
            .collect()
    }
}

impl fmt::Debug for ImapConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImapConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("folders", &self.effective_folders())
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Outcome of syncing one folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderSyncSummary {
    /// Folder (mailbox) name; stored as the messages' `channel`.
    pub folder: String,
    /// The folder's current `UIDVALIDITY` (0 when the server reports none).
    pub uidvalidity: u32,
    /// True when the stored cursor was absent or its `UIDVALIDITY`
    /// mismatched, so the folder was re-scanned from UID 1.
    pub reset: bool,
    /// Messages encountered: `inserted + skipped + failed`.
    pub seen: u64,
    /// Messages newly stored.
    pub inserted: u64,
    /// Messages skipped because their connector identity (same external
    /// key *and* revision) was already archived — including the same mail
    /// already seen in another folder.
    pub skipped: u64,
    /// Messages that could not be parsed (not stored).
    pub failed: u64,
    /// Newly archived rows that are a **conflicting revision**: another
    /// revision of the same `Message-ID` was already archived under this
    /// account (both stay searchable; see [`crate::storage`]).
    pub conflicting_revisions: u64,
    /// Why syncing this folder failed, when it did; the remaining folders
    /// are still synced and reported.
    pub error: Option<String>,
}

/// Connect to the configured server over implicit TLS and log in.
///
/// Uses rustls with the webpki root store, so no system TLS libraries are
/// needed.
pub fn connect(config: &ImapConfig) -> Result<Session<TlsStream>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(config.host.clone())?;
    let conn = rustls::ClientConnection::new(Arc::new(tls_config), server_name)?;
    let mut tcp = None;
    let mut last_err = None;
    for addr in (config.host.as_str(), config.port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                tcp = Some(stream);
                break;
            }
            Err(err) => last_err = Some(err),
        }
    }
    let tcp = tcp.ok_or_else(|| {
        last_err.unwrap_or_else(|| {
            std::io::Error::other(format!(
                "no address resolved for {}:{}",
                config.host, config.port
            ))
        })
    })?;
    tcp.set_read_timeout(Some(IO_TIMEOUT))?;
    tcp.set_write_timeout(Some(IO_TIMEOUT))?;
    let client = imap::Client::new(rustls::StreamOwned::new(conn, tcp));
    client
        .login(&config.username, &config.password)
        .map_err(|(e, _client)| e)
        .map_err(crate::Error::from)
}

/// Register (or resolve) the durable source instance for `config` in
/// `storage`: idempotent on the connector key, so the same account —
/// host, port, TLS mode, username; never the password — always maps to
/// one instance with its own identity scope and folder cursors.
pub fn ensure_source_instance(storage: &Storage, config: &ImapConfig) -> Result<String> {
    storage.ensure_source_instance(&NewSourceInstance {
        kind: "imap",
        label: &config.username,
        connector_key: &config.connector_key(),
        config_json: &config.config_json(),
    })
}

/// Sync every configured folder over an already-authenticated session.
///
/// Generic over the stream so the production TLS path ([`sync`]) and the
/// test path (plain TCP against a mock server) share one implementation.
/// A failing folder never aborts the others: each folder reports its own
/// outcome, with [`FolderSyncSummary::error`] set on failure.
///
/// `instance_id` is the durable source instance (see
/// [`ensure_source_instance`]); identities, occurrences, and cursors are
/// scoped to it.
pub fn sync_session<T: Read + Write>(
    session: &mut Session<T>,
    storage: &Storage,
    instance_id: &str,
    folders: &[String],
) -> Vec<FolderSyncSummary> {
    folders
        .iter()
        .map(|folder| sync_folder(session, storage, instance_id, folder))
        .collect()
}

/// Connect, sync every configured folder, and log out. The account's
/// durable source instance is registered first (idempotently), each
/// attempt/success is stamped on it, and the run reports per-folder
/// summaries.
pub fn sync(config: &ImapConfig, storage: &Storage) -> Result<Vec<FolderSyncSummary>> {
    let instance = ensure_source_instance(storage, config)?;
    storage.record_source_attempt(&instance)?;
    let mut session = connect(config)?;
    let folders = config.effective_folders();
    let summaries = sync_session(&mut session, storage, &instance, &folders);
    // Best-effort logout; a failed logout must not mask a sync error.
    let _ = session.logout();
    storage.record_source_success(&instance)?;
    Ok(summaries)
}

/// Incrementally sync one folder. See the [module documentation](self)
/// for the UIDVALIDITY/UID cursor model and the connector identity scope.
///
/// The outcome is always a summary: when syncing fails partway, the counts
/// already accumulated stay in it, alongside [`FolderSyncSummary::error`].
pub fn sync_folder<T: Read + Write>(
    session: &mut Session<T>,
    storage: &Storage,
    instance_id: &str,
    folder: &str,
) -> FolderSyncSummary {
    fn run<T: Read + Write>(
        session: &mut Session<T>,
        storage: &Storage,
        instance_id: &str,
        folder: &str,
        summary: &mut FolderSyncSummary,
    ) -> Result<()> {
        // EXAMINE (read-only SELECT): fetching must never set \Seen or other
        // flags on the user's mailbox.
        let mailbox = session.examine(folder)?;
        let uidvalidity = mailbox.uid_validity.unwrap_or(0);
        summary.uidvalidity = uidvalidity;

        let saved = storage.get_imap_checkpoint(instance_id, folder)?;
        let (start_uid, reset) = match &saved {
            Some(state) if state.uidvalidity == uidvalidity => {
                (state.last_uid.saturating_add(1), false)
            }
            _ => (1, true),
        };
        summary.reset = reset;

        // UIDNEXT bounds the interesting range: no UID >= start can exist when
        // the server has not handed it out yet (this also skips the UID SEARCH
        // entirely on an unchanged mailbox).
        let nothing_new = mailbox
            .uid_next
            .is_some_and(|uid_next| start_uid >= uid_next);

        let mut uids = Vec::new();
        if mailbox.exists > 0 && !nothing_new {
            let found = session.uid_search(format!("{start_uid}:*"))?;
            // "x:*" always includes the mailbox's highest existing UID even
            // when it is below x (RFC 3501 §6.4.8) — keep only new UIDs.
            uids = found.into_iter().filter(|uid| *uid >= start_uid).collect();
            uids.sort_unstable();
        }

        let mut last_uid = if reset {
            0
        } else {
            saved.map_or(0, |s| s.last_uid)
        };
        for batch in uids.chunks(FETCH_BATCH) {
            let set = batch
                .iter()
                .map(|uid| uid.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let fetches = session.uid_fetch(set, "RFC822")?;
            let mut batch_max_uid = 0;
            for fetch in fetches.iter() {
                let (uid, raw) = match (fetch.uid, fetch.body()) {
                    (Some(uid), Some(raw)) => (uid, raw),
                    _ => {
                        summary.failed += 1;
                        continue;
                    }
                };
                summary.seen += 1;
                match parse_raw_mail_canonical(raw, instance_id, "") {
                    Some(mut parsed) => {
                        apply_folder_fields(&mut parsed.message, folder, uid, uidvalidity);
                        let outcome = storage.insert_mail_message(
                            instance_id,
                            &parsed.message,
                            &parsed.subject,
                            &ConnectorIdentity {
                                namespace: "imap",
                                external_key: &parsed.identity.external_key,
                                revision_hash: &parsed.identity.revision_hash,
                            },
                            &Occurrence {
                                scope: folder,
                                provider_item_id: &uid.to_string(),
                                uidvalidity: Some(uidvalidity),
                            },
                        )?;
                        match outcome {
                            MailInsertOutcome::Inserted {
                                conflicting_revision,
                                ..
                            } => {
                                summary.inserted += 1;
                                if conflicting_revision {
                                    summary.conflicting_revisions += 1;
                                }
                            }
                            MailInsertOutcome::Duplicate { .. } => summary.skipped += 1,
                        }
                    }
                    None => summary.failed += 1,
                }
                batch_max_uid = batch_max_uid.max(uid);
            }
            // Persist the cursor after every batch so an interrupted sync
            // resumes where it stopped instead of refetching the folder.
            if batch_max_uid > last_uid {
                last_uid = batch_max_uid;
                storage.set_imap_checkpoint(instance_id, folder, uidvalidity, last_uid)?;
            }
        }

        // Record the cursor even for empty/unchanged folders (the UIDVALIDITY
        // may have appeared or reset).
        storage.set_imap_checkpoint(instance_id, folder, uidvalidity, last_uid)?;
        Ok(())
    }

    let mut summary = FolderSyncSummary {
        folder: folder.to_string(),
        uidvalidity: 0,
        reset: false,
        seen: 0,
        inserted: 0,
        skipped: 0,
        failed: 0,
        conflicting_revisions: 0,
        error: None,
    };
    if let Err(e) = run(session, storage, instance_id, folder, &mut summary) {
        summary.error = Some(e.to_string());
    }
    summary
}

/// Set the folder-specific fields on an IMAP message: the folder becomes the
/// `channel` (folder → channel mapping), and folder/UID provenance is kept
/// in `raw_json`.
fn apply_folder_fields(msg: &mut NewMessage, folder: &str, uid: u32, uidvalidity: u32) {
    msg.channel = folder.to_string();
    amend_raw_json(
        msg,
        &[
            ("folder", json!(folder)),
            ("uid", json!(uid)),
            ("uidvalidity", json!(uidvalidity)),
        ],
    );
}

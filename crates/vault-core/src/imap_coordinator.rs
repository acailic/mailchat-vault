//! The IMAP coordinator (M2.3): sync *rounds* over every configured IMAP
//! source, on top of the M2.1 job system.
//!
//! Where [`crate::imap::sync_with`] syncs one account the caller names,
//! the coordinator owns the "what should be synced" half: it enumerates
//! the durable IMAP source instances stored in the vault, **skips
//! excluded sources** (per-source exclusion — an excluded source keeps
//! its archive and cursors, it is simply not synced until re-included;
//! see [`crate::storage::Storage::set_source_excluded`]), resolves each
//! source's credentials through an injected resolver (the OS keyring in
//! the app, a closure in tests — the vault database never stores
//! credentials), and drives each source through the standard
//! [`crate::imap::sync_with`] flow over the pool adapter
//! ([`crate::jobs::PoolSync`]).
//!
//! The M2.1 structural contract carries over unchanged: the coordinator
//! runs on the caller's thread (the job runner), every network wait
//! happens there, and each storage effect — instance bookkeeping, batch
//! commits, checkpoint reads — round-trips the pool's bounded write
//! queue. No network operation ever runs inside the write lane, a
//! storage lock, or a transaction, and searches keep answering while a
//! round is parked on the network (proven by the coordinator regression
//! tests).
//!
//! A round never aborts on one source: a source without stored
//! credentials is reported ([`SourceSyncStatus::NoCredentials`]), a
//! source that fails syncs carries its error
//! ([`SourceSyncStatus::Failed`]) while the remaining sources still
//! sync, and the failed-item ledger inside each folder's batches keeps
//! the durable "what fell and why" accounting visible through
//! [`crate::storage::Storage::list_failed_items`].

use std::io::{Read, Write};

pub use crate::imap::Session;
use crate::imap::{self, ImapAuth, ImapConfig, SyncStorage, TLS_MODE};
use crate::jobs::{DbPool, PoolSync};
use crate::storage::SourceInstance;
use serde::{Deserialize, Serialize};

/// The stored non-secret configuration of one IMAP source instance,
/// parsed back out of `config_json` (see [`ImapConfig::config_json`] —
/// the same fields, minus credentials). Credentials are resolved
/// separately at round time; they never live in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImapConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub folders: Vec<String>,
}

impl StoredImapConfig {
    /// Parse one instance's stored configuration. Returns `None` for
    /// anything the IMAP connector cannot own: non-`imap` kinds, legacy
    /// instances without a connector key, foreign TLS modes (a future
    /// STARTTLS configuration is a distinct instance, never a silent
    /// reinterpretation), or malformed JSON.
    pub fn of(instance: &SourceInstance) -> Option<Self> {
        if instance.kind != "imap" || instance.connector_key.is_none() {
            return None;
        }
        let json: serde_json::Value = serde_json::from_str(&instance.config_json).ok()?;
        if json.get("tls_mode")?.as_str()? != TLS_MODE {
            return None;
        }
        Some(Self {
            host: json.get("host")?.as_str()?.to_string(),
            port: json.get("port")?.as_u64()? as u16,
            username: json.get("username")?.as_str()?.to_string(),
            folders: json
                .get("folders")?
                .as_array()?
                .iter()
                .filter_map(|f| f.as_str().map(str::to_string))
                .collect(),
        })
    }

    /// The full sync configuration once credentials are resolved.
    pub fn with_auth(&self, auth: ImapAuth) -> ImapConfig {
        ImapConfig {
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            auth,
            folders: self.folders.clone(),
        }
    }
}

/// What happened to one source in a round.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SourceSyncStatus {
    /// The source synced; the per-folder summaries report the round.
    Synced {
        folders: Vec<imap::FolderSyncSummary>,
    },
    /// The resolver had no credentials for the source (none stored in the
    /// keyring). Reported, not errored: the source stays configured and
    /// will sync once credentials exist again.
    NoCredentials,
    /// The sync attempted and failed (network, auth, storage); the error
    /// string is the typed error's message. Other sources still synced.
    Failed { error: String },
}

/// One source's outcome in a round (see [`sync_round`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSyncOutcome {
    /// The durable source instance id.
    pub instance_id: String,
    /// Human-readable label (`imap:host:port:username`).
    pub label: String,
    pub status: SourceSyncStatus,
}

/// A whole coordinator round: one outcome per included source, plus the
/// labels of sources skipped by exclusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoundSummary {
    pub outcomes: Vec<SourceSyncOutcome>,
    /// Labels of configured sources skipped because they are excluded
    /// (per-source exclusion; see the [module documentation](self)).
    pub excluded: Vec<String>,
}

/// Run one coordinator round over every configured IMAP source.
///
/// - `resolve` supplies each source's credentials: `Ok(None)` when none
///   are stored (the app resolves from the OS keyring here; the vault
///   database is never asked and never told), `Err` when stored
///   credentials cannot be used (e.g. a failed Google token refresh) —
///   reported as [`SourceSyncStatus::Failed`], never conflated with
///   having nothing stored.
/// - `is_cancelled` is checked between sources: a cancelled round stops
///   early (the sources already finished keep their outcomes).
///
/// Reads run on the pool's read lane (the source list), each source's
/// sync runs on the calling thread with storage effects marshaled
/// through the write queue — the round holds nothing while it waits on
/// the network.
pub fn sync_round(
    pool: &DbPool,
    resolve: impl Fn(&SourceInstance) -> Result<Option<ImapAuth>, String>,
    is_cancelled: impl Fn() -> bool,
    media_data_dir: Option<&std::path::Path>,
) -> RoundSummary {
    // The production session builder: implicit-TLS connect + auth
    // (tests inject the plain-TCP mock path through [`sync_round_with`]).
    sync_round_with(pool, resolve, is_cancelled, imap::connect, media_data_dir)
}

/// [`sync_round`] with the session builder injected — how the
/// regression suite runs the same round over the in-process mock IMAP
/// server (plain TCP, no TLS, no network access).
pub fn sync_round_with<T: Read + Write>(
    pool: &DbPool,
    resolve: impl Fn(&SourceInstance) -> Result<Option<ImapAuth>, String>,
    is_cancelled: impl Fn() -> bool,
    connect: impl Fn(&ImapConfig) -> crate::Result<Session<T>>,
    media_data_dir: Option<&std::path::Path>,
) -> RoundSummary {
    let instances: Vec<SourceInstance> =
        pool.with_read(|s| s.list_source_instances().unwrap_or_default());
    let (included, excluded): (Vec<_>, Vec<_>) = instances
        .into_iter()
        .filter(|i| StoredImapConfig::of(i).is_some())
        .partition(|i| !i.excluded);

    let sink = PoolSync(pool);
    let mut outcomes = Vec::new();
    for instance in included {
        if is_cancelled() {
            break;
        }
        let stored = StoredImapConfig::of(&instance).expect("filtered above");
        let status = match resolve(&instance) {
            Ok(Some(auth)) => {
                match sync_one(
                    &stored.with_auth(auth),
                    &sink,
                    &instance.id,
                    &connect,
                    media_data_dir,
                ) {
                    Ok(folders) => SourceSyncStatus::Synced { folders },
                    Err(e) => SourceSyncStatus::Failed {
                        error: e.to_string(),
                    },
                }
            }
            Ok(None) => SourceSyncStatus::NoCredentials,
            Err(error) => SourceSyncStatus::Failed { error },
        };
        outcomes.push(SourceSyncOutcome {
            instance_id: instance.id,
            label: instance.label,
            status,
        });
    }
    RoundSummary {
        outcomes,
        excluded: excluded.into_iter().map(|i| i.label).collect(),
    }
}

/// The per-source flow of [`crate::imap::sync_with`] with the session
/// builder injected: instance bookkeeping on the write queue, the
/// network connect + sync on the calling thread, one atomic batch
/// commit per fetch batch (see [`crate::imap`]).
fn sync_one<T: Read + Write, S: SyncStorage>(
    config: &ImapConfig,
    sink: &S,
    instance_id: &str,
    connect: &impl Fn(&ImapConfig) -> crate::Result<Session<T>>,
    media_data_dir: Option<&std::path::Path>,
) -> crate::Result<Vec<imap::FolderSyncSummary>> {
    // The listed instance already exists; ensure refreshes its stored
    // configuration (label/folders) idempotently.
    let instance = sink.ensure_instance(config)?;
    debug_assert_eq!(instance, instance_id, "connector key maps to one instance");
    sink.record_attempt(&instance)?;
    let mut session = connect(config)?;
    let folders = config.effective_folders();
    // One media budget per source round (M4.2): attachment stores dedup
    // against what earlier folders of the same account already stored.
    let mut budget = crate::object_store::Capacity::default();
    let mut media = media_data_dir.map(|dir| crate::media::MailMedia::new(dir, &mut budget));
    let summaries = imap::sync_session(&mut session, sink, &instance, &folders, media.as_mut());
    // Best-effort logout; a failed logout must not mask a sync error.
    let _ = session.logout();
    sink.record_success(&instance)?;
    Ok(summaries)
}

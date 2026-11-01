//! IMAP scenario suite: multi-round, lifecycle-style end-to-end tests over
//! the mock IMAP server (`../imap_mock`).
//!
//! Where `tests/imap_sync.rs` pins single behaviors one at a time, this
//! suite drives whole *scenarios* — an account synced round after round
//! while the server changes underneath it (mail arriving, folders
//! recreated with new UIDVALIDITY, fetches failing mid-folder, connections
//! dying between batches). Every scenario runs against the real protocol
//! path (greeting → LOGIN → EXAMINE → UID SEARCH → UID FETCH with
//! literals → LOGOUT) over plain TCP, with no network access and no real
//! credentials.
//!
//! Coverage required by the task brief:
//!
//! - **per-folder error collection** — one failing folder never aborts
//!   the others ([`error_collection`]);
//! - **UIDVALIDITY reset** — what happens to the local UID map when a
//!   folder is recreated ([`uidvalidity_reset`]);
//! - **partial sync resume** — an interrupted sync continues without
//!   duplicates ([`partial_resume`]);
//! - **incremental sync** — only new messages are fetched
//!   ([`incremental`]);
//! - **folder → channel mapping** ([`folder_channel`]);
//! - **credential no-persist** — no trace of the password in the database
//!   or any file after syncing ([`credentials_no_persist`]).

#[path = "../imap_mock/mod.rs"]
mod imap_mock;

mod credentials_no_persist;
mod error_collection;
mod folder_channel;
mod incremental;
mod oauth_sync;
mod partial_resume;
mod uidvalidity_reset;

use std::net::TcpStream;

use imap_mock::{MockImap, MockMailbox};
use vault_core::imap as vimap;
use vault_core::message::Message;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

/// The account every scenario syncs as (durable source instance for
/// `alice@example.com` on the mock server).
pub(crate) const USER: &str = "alice@example.com";
/// A second, distinct account for source-scoping scenarios.
pub(crate) const OTHER_USER: &str = "bob@example.com";
/// The IMAP password scenarios log in with. Distinctive on purpose: the
/// no-persist scenario byte-scans every storage file for it.
pub(crate) const PASSWORD: &str = "ziggy42-app-password";

/// The mock server's hostname in connector configurations.
pub(crate) const MOCK_HOST: &str = "mock.test";

/// The connector configuration an account syncs as on the mock server.
pub(crate) fn config_for(user: &str) -> vimap::ImapConfig {
    vimap::ImapConfig {
        host: MOCK_HOST.to_string(),
        port: 993,
        username: user.to_string(),
        password: PASSWORD.to_string(),
        folders: Vec::new(),
    }
}

/// The durable source instance for a user (`vimap::ensure_source_instance`,
/// idempotent): identities, occurrences, and cursors scope to it.
pub(crate) fn instance_of(storage: &Storage, user: &str) -> String {
    vimap::ensure_source_instance(storage, &config_for(user)).unwrap()
}

/// One raw test mail: a Message-ID (or `None` for the content-hash path),
/// a subject, and a body used as the FTS search handle.
pub(crate) fn mail(message_id: Option<&str>, subject: &str, body: &str) -> Vec<u8> {
    let id = message_id
        .map(|m| format!("Message-ID: <{m}>\r\n"))
        .unwrap_or_default();
    format!(
        "From: Alice Smith <alice@example.com>\r\n\
         To: Bob <bob@example.com>\r\n\
         Subject: {subject}\r\n\
         {id}\
         Date: Sat, 1 Nov 2026 10:00:00 +0100\r\n\
         \r\n\
         {body}"
    )
    .into_bytes()
}

/// `n` distinct mails: ids `<{tag}-{i}@scenario.test>`, each body a unique
/// search handle (`{tag}body{i}`).
pub(crate) fn mails(n: usize, tag: &str) -> Vec<Vec<u8>> {
    (1..=n)
        .map(|i| {
            mail(
                Some(&format!("{tag}-{i}@scenario.test")),
                &format!("{tag} mail {i}"),
                &format!("{tag}body{i}"),
            )
        })
        .collect()
}

/// An IMAP account served by the in-process mock. Every [`Account::sync_round`]
/// is one complete real-world sync: fresh connection, LOGIN, sync of the
/// requested folders, LOGOUT — like `vimap::sync` does, minus TLS.
pub(crate) struct Account {
    pub server: MockImap,
    pub user: String,
}

impl Account {
    /// Start a fresh mock server for one account.
    pub fn start(user: &str) -> Self {
        Self {
            server: MockImap::start(),
            user: user.to_string(),
        }
    }

    /// Replace a mailbox wholesale (also how UIDVALIDITY resets are staged).
    pub fn set_mailbox(&self, name: &str, mailbox: MockMailbox) {
        self.server.set_mailbox(name, mailbox);
    }

    /// Append a message to an existing mailbox, assigning the next UID.
    pub fn add_message(&self, name: &str, raw: Vec<u8>) -> u32 {
        self.server.add_message(name, raw)
    }

    /// One sync round; returns one summary per folder, in folder order.
    pub fn sync_round(&self, storage: &Storage, folders: &[&str]) -> Vec<vimap::FolderSyncSummary> {
        self.sync_round_as(&self.user.clone(), storage, folders)
    }

    /// One sync round logged in as another user against the same server:
    /// how two-accounts-on-one-backend scenarios are staged. The durable
    /// source instance follows the login, not the server.
    pub fn sync_round_as(
        &self,
        user: &str,
        storage: &Storage,
        folders: &[&str],
    ) -> Vec<vimap::FolderSyncSummary> {
        let folders: Vec<String> = folders.iter().map(|f| f.to_string()).collect();
        let instance = crate::instance_of(storage, user);
        let mut session = self.session_as(user);
        let summaries = vimap::sync_session(&mut session, storage, &instance, &folders);
        let _ = session.logout();
        summaries
    }

    /// Open a logged-in session against the mock server.
    fn session_as(&self, user: &str) -> imap::Session<TcpStream> {
        let stream = TcpStream::connect(self.server.addr).unwrap();
        let mut client = imap::Client::new(stream);
        client.read_greeting().unwrap();
        client
            .login(user, PASSWORD)
            .map_err(|(e, _client)| e)
            .unwrap()
    }
}

/// The summary for `folder`, panicking with a clear message when absent.
pub(crate) fn summary_for<'a>(
    summaries: &'a [vimap::FolderSyncSummary],
    folder: &str,
) -> &'a vimap::FolderSyncSummary {
    summaries
        .iter()
        .find(|s| s.folder == folder)
        .unwrap_or_else(|| panic!("no summary for folder {folder:?} in {summaries:?}"))
}

/// The folder's persisted sync cursor as `(uidvalidity, last_uid)`, or
/// `None` when the folder has never completed (or attempted) a scan —
/// scoped to the account's durable source instance.
pub(crate) fn cursor(storage: &Storage, user: &str, folder: &str) -> Option<(u32, u32)> {
    storage
        .get_imap_checkpoint(&crate::instance_of(storage, user), folder)
        .unwrap()
        .map(|s| (s.uidvalidity, s.last_uid))
}

/// Search the archive for one phrase; the search-handle bodies from
/// [`mails`] make each mail individually addressable.
pub(crate) fn find(storage: &Storage, phrase: &str) -> Vec<Message> {
    storage
        .search_filtered(&build_match_query(phrase), None, None, 100)
        .unwrap()
}

/// The exact UID set string a `UID FETCH` batch carries, e.g. `"1,2,3"`.
pub(crate) fn uid_set(uids: impl IntoIterator<Item = u32>) -> String {
    uids.into_iter()
        .map(|u| u.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

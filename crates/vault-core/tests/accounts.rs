//! Multi-account overview tests (roadmap: the accounts panel): several
//! IMAP accounts (distinct host+user pairs) living in one vault — their
//! incremental-sync cursors coexist per source instance, per-account
//! counts answer from the bounded counter query, and failures attribute
//! to the account that produced them.
//!
//! Uses the in-process mock IMAP server (`imap_mock`) like
//! `imap_sync.rs`; one mock per account so each has its own UIDVALIDITY
//! and message set.

mod imap_mock;

use std::collections::HashMap;

use imap_mock::{MockImap, MockMailbox};
use vault_core::imap as vimap;
use vault_core::storage::Storage;

/// Offline-mock login fixture — an arbitrary string, never a real credential.
const TEST_FIXTURE: &str = "multi-account-proof";

/// One raw test mail with per-account unique Message-Ids and bodies.
fn mail(tag: &str, i: usize) -> Vec<u8> {
    format!(
        "From: {tag} <{tag}@example.com>\r\n\
         To: Vault <vault@example.com>\r\n\
         Subject: {tag} mail {i}\r\n\
         Message-ID: <{tag}-{i}@accounts.test>\r\n\
         Date: Sat, 1 Nov 2026 10:00:00 +0100\r\n\
         \r\n\
         {tag}body{i}"
    )
    .into_bytes()
}

/// A raw message that fails to parse (a torn header continuation line).
const BROKEN_MAIL: &[u8] = b"   torn header continuation\r\nSubject: never parses\r\n\r\nbody";

/// The connector configuration of one account (host + username is the
/// account identity; the port rides along into the connector key).
fn config(host: &str, username: &str) -> vimap::ImapConfig {
    vimap::ImapConfig {
        host: host.to_string(),
        port: 993,
        username: username.to_string(),
        auth: vimap::ImapAuth::Password(TEST_FIXTURE.to_string()),
        folders: Vec::new(),
    }
}

/// Open a logged-in session against one mock server as `username` (the
/// mock accepts any credentials; the vault-side account identity is the
/// configuration, not the login).
fn session(server: &MockImap, username: &str) -> imap::Session<std::net::TcpStream> {
    let stream = std::net::TcpStream::connect(server.addr).unwrap();
    let mut client = imap::Client::new(stream);
    client.read_greeting().unwrap();
    client
        .login(username, TEST_FIXTURE)
        .map_err(|(e, _client)| e)
        .unwrap()
}

/// Sync one account's INBOX once against its mock server.
fn sync_account(
    server: &MockImap,
    storage: &Storage,
    cfg: &vimap::ImapConfig,
) -> Vec<vimap::FolderSyncSummary> {
    let instance = vimap::ensure_source_instance(storage, cfg).unwrap();
    let mut s = session(server, &cfg.username);
    let folders = vec!["INBOX".to_string()];
    let summaries = vimap::sync_session(&mut s, storage, &instance, &folders, None);
    let _ = s.logout();
    summaries
}

/// The durable instance id of one account's configuration.
fn instance_id(storage: &Storage, cfg: &vimap::ImapConfig) -> String {
    vimap::ensure_source_instance(storage, cfg).unwrap()
}

#[test]
fn two_imap_accounts_keep_independent_cursors_and_counts() {
    // Two accounts: distinct host + user pairs, distinct UIDVALIDITY, and
    // the same folder name — the setup most likely to expose cursor
    // collisions if cursors were ever keyed by folder alone.
    let server_a = MockImap::start();
    server_a.set_mailbox(
        "INBOX",
        MockMailbox::new(7, vec![mail("alice", 1), mail("alice", 2)]),
    );
    let server_b = MockImap::start();
    server_b.set_mailbox("INBOX", MockMailbox::new(11, vec![mail("bob", 1)]));

    let storage = Storage::open_in_memory().unwrap();
    let cfg_a = config("imap.a.test", "alice@a.test");
    let cfg_b = config("imap.b.test", "bob@b.test");

    let summaries_a = sync_account(&server_a, &storage, &cfg_a);
    let summaries_b = sync_account(&server_b, &storage, &cfg_b);
    assert_eq!(summaries_a[0].inserted, 2);
    assert_eq!(summaries_b[0].inserted, 1);

    // Two distinct durable instances — the multi-account namespace.
    let inst_a = instance_id(&storage, &cfg_a);
    let inst_b = instance_id(&storage, &cfg_b);
    assert_ne!(inst_a, inst_b, "each account is its own source instance");

    // The cursors coexist keyed by instance + folder: each account keeps
    // its own UIDVALIDITY and high-water UID, the same folder name on
    // both never collides.
    let cursor_a = storage.get_imap_checkpoint(&inst_a, "INBOX").unwrap();
    let cursor_b = storage.get_imap_checkpoint(&inst_b, "INBOX").unwrap();
    assert_eq!(cursor_a.unwrap().uidvalidity, 7);
    assert_eq!(cursor_a.unwrap().last_uid, 2);
    assert_eq!(cursor_b.unwrap().uidvalidity, 11);
    assert_eq!(cursor_b.unwrap().last_uid, 1);

    // Re-syncing account B (one new mail) advances only B's cursor; A's
    // stays exactly where it was.
    server_b.add_message("INBOX", mail("bob", 2));
    sync_account(&server_b, &storage, &cfg_b);
    assert_eq!(
        storage
            .get_imap_checkpoint(&inst_a, "INBOX")
            .unwrap()
            .unwrap(),
        vault_core::message::ImapSyncState {
            uidvalidity: 7,
            last_uid: 2
        }
    );
    assert_eq!(
        storage
            .get_imap_checkpoint(&inst_b, "INBOX")
            .unwrap()
            .unwrap(),
        vault_core::message::ImapSyncState {
            uidvalidity: 11,
            last_uid: 2
        }
    );

    // Per-account counts: one bounded row per account, counts separated.
    let counts: HashMap<String, vault_core::storage::AccountCounts> = storage
        .account_counts()
        .unwrap()
        .into_iter()
        .map(|c| (c.source_id.clone(), c))
        .collect();
    assert_eq!(counts.len(), 2, "exactly one row per account");
    assert_eq!(counts[&inst_a].messages, 2);
    assert_eq!(counts[&inst_a].failed_items, 0);
    assert_eq!(counts[&inst_b].messages, 2);
    assert_eq!(counts[&inst_b].failed_items, 0);
}

#[test]
fn failures_attribute_to_the_account_that_produced_them() {
    let server_a = MockImap::start();
    server_a.set_mailbox(
        "INBOX",
        MockMailbox::new(3, vec![mail("carol", 1), BROKEN_MAIL.to_vec()]),
    );
    let server_b = MockImap::start();
    server_b.set_mailbox("INBOX", MockMailbox::new(5, vec![mail("dave", 1)]));

    let storage = Storage::open_in_memory().unwrap();
    let cfg_a = config("imap.carol.test", "carol@c.test");
    let cfg_b = config("imap.dave.test", "dave@d.test");
    sync_account(&server_a, &storage, &cfg_a);
    sync_account(&server_b, &storage, &cfg_b);

    let inst_a = instance_id(&storage, &cfg_a);
    let inst_b = instance_id(&storage, &cfg_b);
    let counts: HashMap<String, vault_core::storage::AccountCounts> = storage
        .account_counts()
        .unwrap()
        .into_iter()
        .map(|c| (c.source_id.clone(), c))
        .collect();
    assert_eq!(counts[&inst_a].messages, 1, "the parseable mail only");
    assert_eq!(
        counts[&inst_a].failed_items, 1,
        "the broken mail is ledgered under its own account"
    );
    assert_eq!(counts[&inst_b].messages, 1);
    assert_eq!(counts[&inst_b].failed_items, 0);

    // The ledger itself reports the failed item under account A only
    // (what the accounts panel's per-account sync state summarizes).
    let failed = storage.list_failed_items(None).unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].source_instance_id, inst_a);
}

#[test]
fn import_accounts_and_empty_imap_accounts_appear_with_zero_failures() {
    // An empty IMAP account (connected, nothing synced yet) and a legacy
    // import namespace must both appear in the overview with zero
    // failures — zero-message accounts are still accounts.
    let storage = Storage::open_in_memory().unwrap();
    let cfg_empty = config("imap.empty.test", "erin@e.test");
    let inst_empty = instance_id(&storage, &cfg_empty);

    let mut msg = vault_core::message::NewMessage::new(
        "mbox:archive.mbox",
        "import-1",
        "ana",
        "2026-01-01 10:00:00",
        "imported body",
    );
    msg.channel = "email".into();
    storage
        .commit_import_batch("imp-1", "mbox:archive.mbox", &[msg], &[], &[], 1, true)
        .unwrap();

    let counts: HashMap<String, vault_core::storage::AccountCounts> = storage
        .account_counts()
        .unwrap()
        .into_iter()
        .map(|c| (c.source_id.clone(), c))
        .collect();
    assert_eq!(counts.len(), 2, "the import and the empty account");
    assert_eq!(counts[&inst_empty].messages, 0);
    assert_eq!(counts[&inst_empty].failed_items, 0);
    // The import account is listed by its durable instance id, and its
    // count matches the archive-wide per-source stats (reuse-stats
    // contract: the panel sums to what the source chips show).
    let import_row = counts.values().find(|c| c.source_id != inst_empty).unwrap();
    assert_eq!(import_row.messages, 1);
    let stats = storage.stats().unwrap();
    assert_eq!(stats.total, 1);
}

#[test]
fn channel_flags_report_mail_vs_chat_per_source() {
    // The taxonomy behind the source icons: a chat export stores
    // `"chat"`-channel messages, a mail-file import `"email"`-channel
    // ones, and an empty account nothing — the flags read the messages'
    // own channel column, never the label.
    let storage = Storage::open_in_memory().unwrap();
    let cfg_empty = config("imap.empty.test", "erin@e.test");
    let inst_empty = instance_id(&storage, &cfg_empty);

    let mut chat = vault_core::message::NewMessage::new(
        "whatsapp",
        "w1",
        "ana",
        "2026-01-01 10:00:00",
        "chat body",
    );
    chat.channel = "chat".into();
    storage
        .commit_import_batch("imp-w", "whatsapp", &[chat], &[], &[], 1, true)
        .unwrap();
    let mut mail = vault_core::message::NewMessage::new(
        "mbox:archive.mbox",
        "m1",
        "bruno",
        "2026-01-02 10:00:00",
        "mail body",
    );
    mail.channel = "email".into();
    storage
        .commit_import_batch("imp-m", "mbox:archive.mbox", &[mail], &[], &[], 1, true)
        .unwrap();

    let flags: HashMap<String, vault_core::storage::AccountChannels> = storage
        .account_channel_flags()
        .unwrap()
        .into_iter()
        .map(|c| (c.source_id.clone(), c))
        .collect();
    assert_eq!(flags.len(), 3, "one row per source instance");
    // Exactly one of each mix, independent of instance-id ordering: the
    // chat export (chat-only), the mail import (email-only), and the
    // empty account (neither).
    let mut chat_only = 0;
    let mut email_only = 0;
    let mut neither = 0;
    for row in flags.values() {
        match (row.has_chat, row.has_email) {
            (true, false) => chat_only += 1,
            (false, true) => email_only += 1,
            (false, false) => neither += 1,
            (true, true) => panic!("no source mixes channels in this vault"),
        }
    }
    assert_eq!((chat_only, email_only, neither), (1, 1, 1));
    assert!(!flags[&inst_empty].has_chat, "nothing synced yet");
    assert!(!flags[&inst_empty].has_email, "nothing synced yet");
}

#[test]
fn legacy_namespace_cursors_for_two_accounts_coexist() {
    // The legacy `imap_sync_state` surface (pre-canonical namespaces)
    // also keys per account: two namespaces' cursors never overwrite
    // each other.
    let storage = Storage::open_in_memory().unwrap();
    storage
        .set_imap_state("imap:a.test/alice", "INBOX", 4, 10)
        .unwrap();
    storage
        .set_imap_state("imap:b.test/bob", "INBOX", 9, 3)
        .unwrap();
    assert_eq!(
        storage
            .get_imap_state("imap:a.test/alice", "INBOX")
            .unwrap(),
        Some(vault_core::message::ImapSyncState {
            uidvalidity: 4,
            last_uid: 10
        })
    );
    assert_eq!(
        storage.get_imap_state("imap:b.test/bob", "INBOX").unwrap(),
        Some(vault_core::message::ImapSyncState {
            uidvalidity: 9,
            last_uid: 3
        })
    );
}

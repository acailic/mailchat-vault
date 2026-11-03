//! M1.2 proof tests over the canonical tables from M1.1: durable source
//! instances, host/port-aware IMAP configuration, multi-folder occurrences,
//! connector-scoped identities, and preserved conflicting revisions.
//!
//! The scenarios assert directly against the canonical store
//! (`source_instances`, `message_identities`, `message_occurrences`) —
//! the completion evidence for the roadmap's M1.2 slice:
//!
//! - same username on different hosts and the same mail in multiple
//!   folders remain correct;
//! - conflicting `Message-ID` payloads survive as separate revisions;
//! - reconfiguring credentials preserves source identity;
//! - the stored configuration never contains credentials.

mod imap_mock;

use imap_mock::{MockImap, MockMailbox};
use vault_core::imap as vimap;
use vault_core::search::build_match_query;
use vault_core::storage::{MailInsertOutcome, Storage};

const PASSWORD: &str = "s3cret-app-password";

fn config(host: &str, username: &str) -> vimap::ImapConfig {
    vimap::ImapConfig {
        host: host.to_string(),
        port: 993,
        username: username.to_string(),
        auth: vimap::ImapAuth::Password(PASSWORD.to_string()),
        folders: Vec::new(),
    }
}

/// One raw test mail; `None` message-id builds the fallback-key path.
fn mail(message_id: Option<&str>, subject: &str, body: &str) -> Vec<u8> {
    let id = message_id
        .map(|m| format!("Message-ID: <{m}>\r\n"))
        .unwrap_or_default();
    format!(
        "From: Ana Ilic <ana@example.com>\r\n\
         To: Bob <bob@example.com>\r\n\
         Subject: {subject}\r\n\
         {id}\
         Date: Sat, 1 Nov 2026 10:00:00 +0100\r\n\
         \r\n\
         {body}"
    )
    .into_bytes()
}

fn session(server: &MockImap, user: &str) -> imap::Session<std::net::TcpStream> {
    let stream = std::net::TcpStream::connect(server.addr).unwrap();
    let mut client = imap::Client::new(stream);
    client.read_greeting().unwrap();
    client.login(user, PASSWORD).map_err(|(e, _)| e).unwrap()
}

/// One folder sync round for one account against the mock server.
fn sync_round(
    server: &MockImap,
    storage: &Storage,
    cfg: &vimap::ImapConfig,
    folders: &[&str],
) -> Vec<vimap::FolderSyncSummary> {
    let instance = vimap::ensure_source_instance(storage, cfg).unwrap();
    let folders: Vec<String> = folders.iter().map(|f| f.to_string()).collect();
    let mut s = session(server, &cfg.username);
    let summaries = vimap::sync_session(&mut s, storage, &instance, &folders);
    let _ = s.logout();
    summaries
}

fn find_one(storage: &Storage, phrase: &str) -> vault_core::message::Message {
    let hits = storage
        .search_filtered(&build_match_query(phrase), None, None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1, "expected exactly one hit for {phrase:?}");
    hits.into_iter().next().unwrap()
}

// ---------------------------------------------------------------------
// Durable source instances
// ---------------------------------------------------------------------

/// The same connector configuration resolves to one durable instance;
/// registering is idempotent and refreshes non-identity config only.
#[test]
fn ensure_source_instance_is_idempotent_on_the_connector_key() {
    let storage = Storage::open_in_memory().unwrap();

    let a = vimap::ensure_source_instance(&storage, &config("imap.a.test", "ana@a.test")).unwrap();
    let b = vimap::ensure_source_instance(&storage, &config("imap.a.test", "ana@a.test")).unwrap();
    assert_eq!(a, b, "same connector key must resolve to one instance");

    // Only one instance row exists.
    let instances = storage.list_source_instances().unwrap();
    assert_eq!(instances.len(), 1);
    let inst = &instances[0];
    assert_eq!(inst.kind, "imap");
    assert_eq!(
        inst.connector_key.as_deref(),
        Some("imap|imap.a.test|993|implicit-tls|ana@a.test")
    );
    assert_eq!(
        inst.state, "disconnected",
        "a new instance starts disconnected"
    );
    assert_eq!(inst.last_attempt_at, None);
    assert_eq!(inst.last_success_at, None);

    // The stored configuration is the non-secret connector config.
    let cfg: serde_json::Value = serde_json::from_str(&inst.config_json).unwrap();
    assert_eq!(cfg["host"], "imap.a.test");
    assert_eq!(cfg["port"], 993);
    assert_eq!(cfg["tls_mode"], "implicit-tls");
    assert_eq!(cfg["username"], "ana@a.test");
    assert!(!inst.config_json.contains(PASSWORD));
}

/// Hostnames fold case; usernames do not: one server spelled differently
/// is one instance, one username spelled differently is two.
#[test]
fn host_folds_case_but_username_stays_exact() {
    let storage = Storage::open_in_memory().unwrap();
    let lower =
        vimap::ensure_source_instance(&storage, &config("imap.gmail.com", "Ana@X.com")).unwrap();
    let upper =
        vimap::ensure_source_instance(&storage, &config("IMAP.Gmail.COM", "Ana@X.com")).unwrap();
    assert_eq!(lower, upper, "hosts are case-folded in the identity");

    let cased =
        vimap::ensure_source_instance(&storage, &config("imap.gmail.com", "ana@x.com")).unwrap();
    assert_ne!(
        lower, cased,
        "usernames stay exact in the identity — two casings are two accounts"
    );
    assert_eq!(storage.list_source_instances().unwrap().len(), 2);
}

/// Reconfiguring credentials (the password) preserves source identity —
/// the password is not part of the connector key nor the stored config.
#[test]
fn credential_reconfiguration_preserves_source_identity() {
    let storage = Storage::open_in_memory().unwrap();
    let before =
        vimap::ensure_source_instance(&storage, &config("imap.a.test", "ana@a.test")).unwrap();
    let mut reconfigured = config("imap.a.test", "ana@a.test");
    reconfigured.auth = vimap::ImapAuth::Password("a-brand-new-app-password".to_string());
    let after = vimap::ensure_source_instance(&storage, &reconfigured).unwrap();
    assert_eq!(before, after);
    assert!(!storage.list_source_instances().unwrap()[0]
        .config_json
        .contains("a-brand-new-app-password"));
}

/// Same username, different hosts: two durable instances, and the same
/// mailing-list mail synced into both stays two correctly-attributed rows.
#[test]
fn same_username_on_different_hosts_stays_two_instances() {
    let server = MockImap::start();
    let shared = mail(Some("list-777@ml.example"), "List mail", "sharedlist body");
    server.set_mailbox("INBOX", MockMailbox::new(7, vec![shared]));

    let storage = Storage::open_in_memory().unwrap();
    let summaries_a = sync_round(
        &server,
        &storage,
        &config("imap.fastmail.example", "ana@provider.test"),
        &["INBOX"],
    );
    let summaries_b = sync_round(
        &server,
        &storage,
        &config("imap.gmail.example", "ana@provider.test"),
        &["INBOX"],
    );
    assert_eq!(summaries_a[0].inserted, 1);
    assert_eq!(
        summaries_b[0].inserted, 1,
        "the second host archives its own copy"
    );

    // Two durable instances, same username, different hosts.
    let instances = storage.list_source_instances().unwrap();
    assert_eq!(instances.len(), 2);
    assert_ne!(instances[0].id, instances[1].id);
    assert!(instances.iter().all(|i| i.kind == "imap"));

    // Two rows, one per instance — each correctly attributed under its
    // human-readable connector label, never the raw instance id.
    let hits = storage
        .search_filtered(&build_match_query("sharedlist"), None, None, 10)
        .unwrap();
    assert_eq!(hits.len(), 2);
    let mut sources: Vec<String> = hits.iter().map(|m| m.source.clone()).collect();
    sources.sort();
    let mut expected: Vec<String> = instances.iter().map(|i| i.label.clone()).collect();
    expected.sort();
    assert_eq!(sources, expected);

    // Re-syncing either host never leaks into the other: no duplicates.
    sync_round(
        &server,
        &storage,
        &config("imap.fastmail.example", "ana@provider.test"),
        &["INBOX"],
    );
    assert_eq!(storage.stats().unwrap().total, 2);
}

/// Successful syncs stamp attempt/success and connect the instance;
/// disconnect marks it disconnected without touching the archive.
#[test]
fn sync_attempts_stamp_the_instance_state() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(7, vec![mail(Some("s-1@x"), "One", "body one")]),
    );

    let storage = Storage::open_in_memory().unwrap();
    let instance =
        vimap::ensure_source_instance(&storage, &config("imap.a.test", "ana@a.test")).unwrap();
    assert_eq!(
        storage.list_source_instances().unwrap()[0].state,
        "disconnected"
    );

    storage.record_source_attempt(&instance).unwrap();
    storage.record_source_success(&instance).unwrap();
    let inst = &storage.list_source_instances().unwrap()[0];
    assert_eq!(inst.state, "connected");
    assert!(inst.last_attempt_at.is_some(), "attempt stamped");
    assert!(inst.last_success_at.is_some(), "success stamped");

    storage.disconnect_source_instance(&instance).unwrap();
    let inst = &storage.list_source_instances().unwrap()[0];
    assert_eq!(inst.state, "disconnected");
    assert!(
        inst.last_success_at.is_some(),
        "history survives disconnect"
    );

    // The archive survives disconnect untouched.
    sync_round(
        &server,
        &storage,
        &config("imap.a.test", "ana@a.test"),
        &["INBOX"],
    );
    storage.disconnect_source_instance(&instance).unwrap();
    assert_eq!(storage.stats().unwrap().total, 1);
}

// ---------------------------------------------------------------------
// Multi-folder occurrences
// ---------------------------------------------------------------------

/// The same mail in several folders: one archived row, one occurrence per
/// folder (scope + UID + UIDVALIDITY), and per-folder cursors.
#[test]
fn same_mail_in_multiple_folders_keeps_one_row_with_folder_occurrences() {
    let server = MockImap::start();
    let shared = mail(Some("multi-1@x"), "Multi", "multifolder body");
    server.set_mailbox("INBOX", MockMailbox::new(7, vec![shared.clone()]));
    server.set_mailbox("All Mail", MockMailbox::new(5, vec![shared.clone()]));
    server.set_mailbox("Archive", MockMailbox::new(11, vec![shared]));

    let storage = Storage::open_in_memory().unwrap();
    let cfg = config("imap.a.test", "ana@a.test");
    let instance = vimap::ensure_source_instance(&storage, &cfg).unwrap();
    let summaries = sync_round(&server, &storage, &cfg, &["INBOX", "All Mail", "Archive"]);
    assert_eq!(summaries.iter().map(|s| s.inserted).sum::<u64>(), 1);
    assert_eq!(summaries.iter().map(|s| s.skipped).sum::<u64>(), 2);
    assert_eq!(storage.stats().unwrap().total, 1, "one archived row");

    let m = find_one(&storage, "multifolder");
    let mut occurrences = storage.message_occurrences(m.id).unwrap();
    occurrences.sort_by(|a, b| a.scope.cmp(&b.scope));
    let scopes: Vec<(&str, &str, u32)> = occurrences
        .iter()
        .map(|o| {
            (
                o.scope.as_str(),
                o.provider_item_id.as_str(),
                o.uidvalidity.unwrap(),
            )
        })
        .collect();
    assert_eq!(
        scopes,
        vec![
            ("All Mail", "1", 5),
            ("Archive", "1", 11),
            ("INBOX", "1", 7),
        ],
        "each folder observation recorded with its UID and UIDVALIDITY"
    );

    // One cursor per folder, all under the same durable instance.
    for (folder, uv) in [("INBOX", 7u32), ("All Mail", 5), ("Archive", 11)] {
        let state = storage
            .get_imap_checkpoint(&instance, folder)
            .unwrap()
            .unwrap_or_else(|| panic!("no cursor for {folder}"));
        assert_eq!(state.uidvalidity, uv);
        assert_eq!(state.last_uid, 1);
    }
}

/// A UIDVALIDITY reset re-observes the same mail under new UIDs without
/// duplicating the archived row.
#[test]
fn uidvalidity_reset_reobserves_without_duplicating() {
    let server = MockImap::start();
    let shared = mail(Some("reset-1@x"), "Reset", "reset body");
    server.set_mailbox("INBOX", MockMailbox::new(7, vec![shared.clone()]));

    let storage = Storage::open_in_memory().unwrap();
    let cfg = config("imap.a.test", "ana@a.test");
    sync_round(&server, &storage, &cfg, &["INBOX"]);

    server.set_mailbox("INBOX", MockMailbox::new(42, vec![shared]));
    let summaries = sync_round(&server, &storage, &cfg, &["INBOX"]);
    assert_eq!(summaries[0].inserted, 0);
    assert_eq!(summaries[0].skipped, 1);
    assert_eq!(storage.stats().unwrap().total, 1);
}

// ---------------------------------------------------------------------
// Connector-scoped identities and preserved conflicting revisions
// ---------------------------------------------------------------------

/// Two different decoded payloads behind one `Message-ID` are both
/// archived as separate revisions — nothing silently wins, and every
/// revision stays searchable.
#[test]
fn conflicting_message_id_payloads_survive_as_revisions() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(
            7,
            vec![
                mail(Some("conflict-9@x"), "Draft", "original draft body"),
                mail(Some("conflict-9@x"), "Draft", "tampered alternate body"),
            ],
        ),
    );

    let storage = Storage::open_in_memory().unwrap();
    let summaries = sync_round(
        &server,
        &storage,
        &config("imap.a.test", "ana@a.test"),
        &["INBOX"],
    );
    assert_eq!(summaries[0].inserted, 2, "both revisions archived");
    assert_eq!(
        summaries[0].conflicting_revisions, 1,
        "the conflict is counted"
    );
    assert_eq!(storage.stats().unwrap().total, 2);

    // Both revisions are searchable, each with its own body.
    assert_eq!(
        storage
            .search_filtered(&build_match_query("original draft"), None, None, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        storage
            .search_filtered(&build_match_query("tampered alternate"), None, None, 10)
            .unwrap()
            .len(),
        1
    );

    // Both share the external key; the revisions query links them.
    let a = find_one(&storage, "original draft");
    let b = find_one(&storage, "tampered alternate");
    assert_eq!(a.external_id, b.external_id, "same Message-ID key");
    assert_ne!(a.id, b.id, "separate archived rows");
    let revisions_a = storage.message_revisions(a.id).unwrap();
    let revisions_b = storage.message_revisions(b.id).unwrap();
    assert_eq!(revisions_a.len(), 2);
    assert_eq!(
        revisions_a, revisions_b,
        "both rows see the same revision set"
    );
    assert_ne!(revisions_a[0].revision_hash, revisions_a[1].revision_hash);

    // An exact repeat of one revision dedups onto its own row only.
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(
            9,
            vec![mail(Some("conflict-9@x"), "Draft", "original draft body")],
        ),
    );
    let summaries = sync_round(
        &server,
        &storage,
        &config("imap.a.test", "ana@a.test"),
        &["INBOX"],
    );
    assert_eq!(summaries[0].inserted, 0);
    assert_eq!(summaries[0].skipped, 1);
    assert_eq!(
        storage.stats().unwrap().total,
        2,
        "still exactly two revisions"
    );
}

/// Folder state never enters the revision hash: the same mail in another
/// folder (different UID/UIDVALIDITY provenance) is the same revision.
#[test]
fn revision_hash_excludes_folder_state() {
    let server = MockImap::start();
    let shared = mail(Some("rev-1@x"), "Rev", "stable body");
    server.set_mailbox("INBOX", MockMailbox::new(7, vec![shared.clone()]));
    server.set_mailbox("Archive", MockMailbox::new(900, vec![shared]));

    let storage = Storage::open_in_memory().unwrap();
    let summaries = sync_round(
        &server,
        &storage,
        &config("imap.a.test", "ana@a.test"),
        &["INBOX", "Archive"],
    );
    assert_eq!(
        summaries[1].conflicting_revisions, 0,
        "folder change is not a revision"
    );
    assert_eq!(storage.stats().unwrap().total, 1);
}

/// Without a `Message-ID`, the SHA-256 of the raw message bytes is the
/// identity key: identical bytes dedup across syncs, different bytes are
/// separate messages.
#[test]
fn idless_mail_falls_back_to_raw_byte_sha256_identity() {
    let server = MockImap::start();
    let no_id = mail(None, "No id", "idless body text");
    server.set_mailbox("INBOX", MockMailbox::new(7, vec![no_id]));

    let storage = Storage::open_in_memory().unwrap();
    let cfg = config("imap.a.test", "ana@a.test");
    sync_round(&server, &storage, &cfg, &["INBOX"]);

    let m = find_one(&storage, "idless");
    assert_eq!(m.external_id.len(), 64, "sha256 hex fallback key");
    assert!(
        !m.external_id.starts_with("generated-"),
        "the connector path does not use the legacy fnv id"
    );

    // The same bytes re-fetched (UIDVALIDITY reset) dedup onto that key.
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(8, vec![mail(None, "No id", "idless body text")]),
    );
    let summaries = sync_round(&server, &storage, &cfg, &["INBOX"]);
    assert_eq!(summaries[0].skipped, 1);
    assert_eq!(summaries[0].inserted, 0);
    assert_eq!(storage.stats().unwrap().total, 1);
}

/// Connector identities are scoped to the instance: the same `Message-ID`
/// synced through two accounts is one row per account (cross-instance
/// merging is not this release), and within one account still one row.
#[test]
fn connector_identities_are_instance_scoped() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(7, vec![mail(Some("scoped-1@x"), "S", "scoped body")]),
    );

    let storage = Storage::open_in_memory().unwrap();
    sync_round(
        &server,
        &storage,
        &config("imap.a.test", "ana@a.test"),
        &["INBOX"],
    );
    sync_round(
        &server,
        &storage,
        &config("imap.b.test", "bob@b.test"),
        &["INBOX"],
    );
    assert_eq!(storage.stats().unwrap().total, 2);

    // Within one account the mail is exactly once; re-syncs dedup.
    sync_round(
        &server,
        &storage,
        &config("imap.a.test", "ana@a.test"),
        &["INBOX"],
    );
    assert_eq!(storage.stats().unwrap().total, 2);
}

/// The v1 search source facet uses the same string `Message.source`
/// reports; for connector-owned instances that is the connector label, so
/// a facet value taken from stats/search filters exactly its own rows.
#[test]
fn connector_sources_filter_by_their_reported_source_string() {
    use vault_core::query::SearchQuery;

    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(7, vec![mail(Some("facet-1@x"), "F", "facet body")]),
    );
    let storage = Storage::open_in_memory().unwrap();
    sync_round(
        &server,
        &storage,
        &config("imap.a.test", "ana@a.test"),
        &["INBOX"],
    );
    sync_round(
        &server,
        &storage,
        &config("imap.b.test", "bob@b.test"),
        &["INBOX"],
    );

    let sources = storage.stats().unwrap().by_source;
    let ana_label = "imap:imap.a.test:993:ana@a.test";
    assert!(
        sources.iter().any(|s| s.source == ana_label),
        "stats surface the connector label, not the instance id: {sources:?}"
    );
    let hits = storage
        .search_page(&SearchQuery {
            text: "facet".into(),
            sources: vec![ana_label.to_string()],
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(hits.total, 1);
    assert_eq!(hits.hits[0].message.source, ana_label);
}

/// The storage-level insert API reports conflicts and preserves both
/// revisions with a visible marker, exercised directly.
#[test]
fn insert_mail_message_outcome_flags_conflicts() {
    use vault_core::storage::{ConnectorIdentity, Occurrence};

    let storage = Storage::open_in_memory().unwrap();
    let instance =
        vimap::ensure_source_instance(&storage, &config("imap.a.test", "ana@a.test")).unwrap();

    let msg = |body: &str| {
        vault_core::message::NewMessage::new(
            instance.clone(),
            "conf@x",
            "ana",
            "2026-11-01T09:00:00Z",
            body,
        )
    };

    let identity = ConnectorIdentity {
        namespace: "imap",
        external_key: "conf@x",
        revision_hash: "rev-a",
    };
    let occurrence = Occurrence {
        scope: "INBOX",
        provider_item_id: "1",
        uidvalidity: Some(7),
    };

    let first = storage
        .insert_mail_message(&instance, &msg("first body"), "S", &identity, &occurrence)
        .unwrap();
    assert!(matches!(
        first,
        MailInsertOutcome::Inserted {
            conflicting_revision: false,
            ..
        }
    ));

    // Same revision seen in another folder: duplicate + new occurrence.
    let other_folder = Occurrence {
        scope: "Archive",
        provider_item_id: "9",
        uidvalidity: Some(3),
    };
    let second = storage
        .insert_mail_message(&instance, &msg("first body"), "S", &identity, &other_folder)
        .unwrap();
    match second {
        MailInsertOutcome::Duplicate { id } => {
            let occurrences = storage.message_occurrences(id).unwrap();
            assert_eq!(occurrences.len(), 2, "both folder observations recorded");
        }
        other => panic!("expected duplicate, got {other:?}"),
    }

    // A different revision of the same external key: preserved + flagged.
    let revised = ConnectorIdentity {
        namespace: "imap",
        external_key: "conf@x",
        revision_hash: "rev-b",
    };
    let third = storage
        .insert_mail_message(&instance, &msg("second body"), "S", &revised, &occurrence)
        .unwrap();
    assert!(matches!(
        third,
        MailInsertOutcome::Inserted {
            conflicting_revision: true,
            ..
        }
    ));
    assert_eq!(storage.stats().unwrap().total, 2);
}

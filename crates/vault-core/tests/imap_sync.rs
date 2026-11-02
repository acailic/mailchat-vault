//! Incremental IMAP sync tests against the in-process mock server
//! (`imap_mock`). These exercise the real protocol path — greeting, LOGIN,
//! EXAMINE, UID SEARCH, UID FETCH with literals, LOGOUT — without any
//! network access or real credentials.

mod imap_mock;

use imap_mock::{MockImap, MockMailbox};
use vault_core::imap as vimap;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

const USER: &str = "alice@example.com";
const PASSWORD: &str = "hunter2-app-password";
const FOLDERS: &[&str] = &["INBOX"];

/// The canonical connector configuration every test here syncs as: the
/// mock server is a plain-TCP stand-in, but the durable instance it
/// registers carries a real host/port/TLS/username identity.
fn config() -> vimap::ImapConfig {
    vimap::ImapConfig {
        host: "mock.test".to_string(),
        port: 993,
        username: USER.to_string(),
        password: PASSWORD.to_string(),
        folders: Vec::new(),
    }
}

/// The durable source instance for [`config`] (idempotent).
fn instance(storage: &Storage) -> String {
    vimap::ensure_source_instance(storage, &config()).unwrap()
}

/// One raw test mail.
fn mail(message_id: Option<&str>, subject: &str, body: &str) -> Vec<u8> {
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

/// A raw message that fails to parse (a torn header continuation line,
/// like a truncated download).
const BROKEN_MAIL: &[u8] = b"   torn header continuation\r\nSubject: never parses\r\n\r\nbody";

/// Open a logged-in session against the mock server.
fn session(server: &MockImap) -> imap::Session<std::net::TcpStream> {
    let stream = std::net::TcpStream::connect(server.addr).unwrap();
    let mut client = imap::Client::new(stream);
    client.read_greeting().unwrap();
    client
        .login(USER, PASSWORD)
        .map_err(|(e, _client)| e)
        .unwrap()
}

/// Sync the given folders once against the mock server.
fn sync(server: &MockImap, storage: &Storage, folders: &[&str]) -> Vec<vimap::FolderSyncSummary> {
    let folders: Vec<String> = folders.iter().map(|f| f.to_string()).collect();
    let instance = instance(storage);
    let mut s = session(server);
    let summaries = vimap::sync_session(&mut s, storage, &instance, &folders);
    let _ = s.logout();
    summaries
}

#[test]
fn login_uses_configured_credentials() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(7, vec![mail(Some("m-1@x"), "One", "hi")]),
    );
    let storage = Storage::open_in_memory().unwrap();
    sync(&server, &storage, FOLDERS);

    let log = server.log();
    let login = log.logins.first().expect("a LOGIN happened");
    assert!(login.contains(USER), "login line {login:?} lacks user");
    assert!(
        login.contains(PASSWORD),
        "login line {login:?} lacks password"
    );
}

#[test]
fn initial_sync_stores_messages_with_folder_as_channel() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(
            7,
            vec![
                mail(Some("m-1@example.com"), "First mail", "hello vault world"),
                mail(Some("m-2@example.com"), "Second mail", "second body text"),
            ],
        ),
    );
    let storage = Storage::open_in_memory().unwrap();
    let summaries = sync(&server, &storage, FOLDERS);

    assert_eq!(summaries.len(), 1);
    let s = &summaries[0];
    assert_eq!(s.folder, "INBOX");
    assert_eq!(s.uidvalidity, 7);
    assert!(s.reset, "first sync is a full scan");
    assert_eq!(s.seen, 2);
    assert_eq!(s.inserted, 2);
    assert_eq!(s.skipped, 0);
    assert_eq!(s.failed, 0);

    // Cursor saved at the highest fetched UID, keyed by the durable
    // source instance.
    let state = storage
        .get_imap_checkpoint(&instance(&storage), "INBOX")
        .unwrap()
        .unwrap();
    assert_eq!(state.uidvalidity, 7);
    assert_eq!(state.last_uid, 2);

    // Messages stored under the account's durable source instance (the
    // instance id is the source of connector-archived rows), folder as
    // channel, Message-ID as external id, and full-text searchable.
    assert_eq!(storage.stats().unwrap().total, 2);
    let hits = storage
        .search_filtered(&build_match_query("hello vault world"), None, None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    let m = &hits[0];
    assert_eq!(m.source, instance(&storage));
    assert_eq!(m.external_id, "m-1@example.com");
    assert_eq!(m.channel, "INBOX");
    assert_eq!(m.author, "Alice Smith <alice@example.com>");
    assert_eq!(m.sent_at, "2026-11-01T09:00:00Z");
    // Folder/UID provenance kept in raw_json.
    let raw: serde_json::Value = serde_json::from_str(&m.raw_json).unwrap();
    assert_eq!(raw["folder"], "INBOX");
    assert_eq!(raw["uid"], 1);
    assert_eq!(raw["uidvalidity"], 7);
    assert_eq!(raw["subject"], "First mail");
}

#[test]
fn resync_without_changes_fetches_nothing() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(
            7,
            vec![
                mail(Some("m-1@x"), "One", "hi"),
                mail(Some("m-2@x"), "Two", "ho"),
            ],
        ),
    );
    let storage = Storage::open_in_memory().unwrap();
    sync(&server, &storage, FOLDERS);
    let fetches_after_first = server.log().fetch_sets.len();
    let searches_after_first = server.log().searches.len();

    let summaries = sync(&server, &storage, FOLDERS);
    let s = &summaries[0];
    assert!(!s.reset);
    assert_eq!(s.seen, 0);
    assert_eq!(s.inserted, 0);
    assert_eq!(s.skipped, 0);
    assert_eq!(storage.stats().unwrap().total, 2);

    // No new SEARCH or FETCH reached the server (UIDNEXT short-circuit).
    let log = server.log();
    assert_eq!(log.fetch_sets.len(), fetches_after_first);
    assert_eq!(log.searches.len(), searches_after_first);
}

#[test]
fn incremental_sync_fetches_only_new_messages() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(
            7,
            vec![
                mail(Some("m-1@x"), "One", "first"),
                mail(Some("m-2@x"), "Two", "second"),
            ],
        ),
    );
    let storage = Storage::open_in_memory().unwrap();
    sync(&server, &storage, FOLDERS);

    // A new mail arrives.
    server.add_message("INBOX", mail(Some("m-3@x"), "Three", "third body"));

    let summaries = sync(&server, &storage, FOLDERS);
    let s = &summaries[0];
    assert!(!s.reset);
    assert_eq!(s.seen, 1);
    assert_eq!(s.inserted, 1);
    assert_eq!(storage.stats().unwrap().total, 3);

    // Only the new UID was fetched, and the search ranged past the cursor.
    let log = server.log();
    assert_eq!(log.searches.last().unwrap(), "3:*");
    assert_eq!(log.fetch_sets.last().unwrap(), "3");

    let state = storage
        .get_imap_checkpoint(&instance(&storage), "INBOX")
        .unwrap()
        .unwrap();
    assert_eq!((state.uidvalidity, state.last_uid), (7, 3));

    // The new mail is searchable.
    let hits = storage
        .search_filtered(&build_match_query("third body"), None, None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].external_id, "m-3@x");
}

#[test]
fn uidvalidity_change_rescans_without_duplicates() {
    let server = MockImap::start();
    let mails = vec![
        mail(Some("m-1@x"), "One", "first"),
        mail(Some("m-2@x"), "Two", "second"),
    ];
    server.set_mailbox("INBOX", MockMailbox::new(7, mails.clone()));
    let storage = Storage::open_in_memory().unwrap();
    sync(&server, &storage, FOLDERS);

    // The folder is recreated: same messages, new UIDVALIDITY (UIDs
    // reassigned from 1).
    server.set_mailbox("INBOX", MockMailbox::new(99, mails));

    let summaries = sync(&server, &storage, FOLDERS);
    let s = &summaries[0];
    assert_eq!(s.uidvalidity, 99);
    assert!(s.reset, "UIDVALIDITY change must trigger a full rescan");
    assert_eq!(s.seen, 2);
    assert_eq!(
        s.inserted, 0,
        "rescanned mails must dedup by their connector identity"
    );
    assert_eq!(s.skipped, 2);
    assert_eq!(storage.stats().unwrap().total, 2);

    // Cursor now tracks the new validity.
    let state = storage
        .get_imap_checkpoint(&instance(&storage), "INBOX")
        .unwrap()
        .unwrap();
    assert_eq!((state.uidvalidity, state.last_uid), (99, 2));
}

#[test]
fn messages_without_message_id_get_stable_generated_ids() {
    let no_id = mail(None, "No id", "idless body");
    let server = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(7, vec![no_id.clone()]));
    let storage = Storage::open_in_memory().unwrap();
    sync(&server, &storage, FOLDERS);
    assert_eq!(storage.stats().unwrap().total, 1);

    // Force a refetch via UIDVALIDITY reset: the content-hash id must
    // dedup the mail instead of storing it twice.
    server.set_mailbox("INBOX", MockMailbox::new(8, vec![no_id]));
    let summaries = sync(&server, &storage, FOLDERS);
    assert_eq!(summaries[0].skipped, 1);
    assert_eq!(summaries[0].inserted, 0);
    assert_eq!(storage.stats().unwrap().total, 1);
}

#[test]
fn unparseable_messages_are_counted_not_dropped() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(
            7,
            vec![
                mail(Some("m-1@x"), "Good", "good body"),
                BROKEN_MAIL.to_vec(),
            ],
        ),
    );
    let storage = Storage::open_in_memory().unwrap();
    let summaries = sync(&server, &storage, FOLDERS);

    let s = &summaries[0];
    assert_eq!(s.inserted, 1);
    assert_eq!(s.failed, 1);
    assert_eq!(s.seen, 2);
    assert_eq!(storage.stats().unwrap().total, 1);
}

#[test]
fn folders_map_to_separate_channels_and_cursors() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(7, vec![mail(Some("in-1@x"), "In", "inbox body")]),
    );
    server.set_mailbox(
        "Archive",
        MockMailbox::new(3, vec![mail(Some("ar-1@x"), "Ar", "archive body")]),
    );

    let storage = Storage::open_in_memory().unwrap();
    let summaries = sync(&server, &storage, &["INBOX", "Archive"]);
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].folder, "INBOX");
    assert_eq!(summaries[1].folder, "Archive");

    // Each folder's messages carry their folder as channel; one cursor per
    // folder.
    let inbox_hits = storage
        .search_filtered(&build_match_query("inbox body"), None, None, 10)
        .unwrap();
    let archive_hits = storage
        .search_filtered(&build_match_query("archive body"), None, None, 10)
        .unwrap();
    assert_eq!((inbox_hits.len(), archive_hits.len()), (1, 1));
    assert_eq!(inbox_hits[0].channel, "INBOX");
    assert_eq!(archive_hits[0].channel, "Archive");

    let inbox_state = storage
        .get_imap_checkpoint(&instance(&storage), "INBOX")
        .unwrap()
        .unwrap();
    let archive_state = storage
        .get_imap_checkpoint(&instance(&storage), "Archive")
        .unwrap()
        .unwrap();
    assert_eq!((inbox_state.uidvalidity, inbox_state.last_uid), (7, 1));
    assert_eq!((archive_state.uidvalidity, archive_state.last_uid), (3, 1));
}

/// The same Message-ID in two folders (e.g. INBOX and All Mail) is one
/// archived message with **two folder occurrences**: the first folder
/// writes the row, the second only records its observation (folder, UID,
/// UIDVALIDITY) in `message_occurrences` — the multi-folder provenance the
/// canonical store carries.
#[test]
fn same_mail_in_two_folders_dedups_within_one_account() {
    let shared = mail(Some("shared-1@x"), "Shared", "shared body");
    let server = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(7, vec![shared.clone()]));
    server.set_mailbox("All Mail", MockMailbox::new(5, vec![shared]));

    let storage = Storage::open_in_memory().unwrap();
    let summaries = sync(&server, &storage, &["INBOX", "All Mail"]);
    assert_eq!(summaries[0].inserted, 1);
    assert_eq!(summaries[1].inserted, 0);
    assert_eq!(summaries[1].skipped, 1);
    assert_eq!(storage.stats().unwrap().total, 1);

    // One row, two occurrences: INBOX/UID 1/UV 7 and All Mail/UID 1/UV 5.
    let hits = storage
        .search_filtered(&build_match_query("shared body"), None, None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    let occurrences = storage.message_occurrences(hits[0].id).unwrap();
    let mut scopes: Vec<(&str, &str, u32)> = occurrences
        .iter()
        .map(|o| {
            (
                o.scope.as_str(),
                o.provider_item_id.as_str(),
                o.uidvalidity.unwrap(),
            )
        })
        .collect();
    scopes.sort();
    assert_eq!(
        scopes,
        vec![("All Mail", "1", 5), ("INBOX", "1", 7)],
        "both folder observations must be recorded on the one row"
    );

    // Re-syncing either folder adds no further occurrence rows (the
    // occurrence tuple is unique).
    sync(&server, &storage, &["INBOX", "All Mail"]);
    assert_eq!(storage.message_occurrences(hits[0].id).unwrap().len(), 2);
}

#[test]
fn password_is_never_written_to_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("vault.db");
    let storage = Storage::open(&db_path).unwrap();

    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(7, vec![mail(Some("m-1@x"), "One", "hi")]),
    );
    sync(&server, &storage, FOLDERS);

    // Scan every file SQLite may have written (db + WAL sidecar).
    for path in [
        db_path.clone(),
        dir.path().join("vault.db-wal"),
        dir.path().join("vault.db-shm"),
    ] {
        if let Ok(bytes) = std::fs::read(&path) {
            assert!(
                !bytes
                    .windows(PASSWORD.len())
                    .any(|w| w == PASSWORD.as_bytes()),
                "password leaked into {}",
                path.display()
            );
        }
    }
}

#[test]
fn missing_folder_reports_error_without_breaking_others() {
    let server = MockImap::start();
    server.set_mailbox(
        "INBOX",
        MockMailbox::new(7, vec![mail(Some("m-1@x"), "One", "hi")]),
    );
    server.set_mailbox(
        "Archive",
        MockMailbox::new(3, vec![mail(Some("ar-1@x"), "Ar", "archive body")]),
    );
    let storage = Storage::open_in_memory().unwrap();

    let mut s = session(&server);
    let summaries = vimap::sync_session(
        &mut s,
        &storage,
        &instance(&storage),
        &[
            "INBOX".to_string(),
            "NoSuch".to_string(),
            "Archive".to_string(),
        ],
    );
    let _ = s.logout();

    // The failing folder reports its error in its own summary...
    assert_eq!(summaries.len(), 3);
    let err = summaries[1]
        .error
        .as_deref()
        .expect("the missing folder must carry its error");
    assert!(
        err.to_lowercase().contains("no such mailbox"),
        "unexpected error: {err}"
    );
    assert_eq!(summaries[1].seen, 0);

    // ...while the folders before and after it still fully synced.
    assert_eq!(summaries[0].error, None);
    assert_eq!(summaries[0].inserted, 1);
    assert_eq!(summaries[2].error, None);
    assert_eq!(summaries[2].inserted, 1);
    assert_eq!(storage.stats().unwrap().total, 2);
}

/// The server breaks mid-folder (one batch stored, the next FETCH fails):
/// the folder summary keeps the counts already accumulated, the batch
/// cursor already persisted is kept so a retry resumes instead of
/// refetching, and the folders after the failure still sync.
#[test]
fn partial_folder_failure_keeps_counts_and_resume_cursor() {
    let server = MockImap::start();
    let sixty_mails: Vec<Vec<u8>> = (1..=60)
        .map(|i| {
            mail(
                Some(&format!("p-{i}@x")),
                &format!("Mail {i}"),
                &format!("body {i}"),
            )
        })
        .collect();
    server.set_mailbox("INBOX", MockMailbox::new(7, sixty_mails));
    server.set_mailbox(
        "Archive",
        MockMailbox::new(3, vec![mail(Some("ar-1@x"), "Ar", "archive body")]),
    );
    // Fail exactly the 2nd UID FETCH: the first 50-message batch stores,
    // the second batch (UIDs 51..=60) fails.
    server.fail_uid_fetch_once(Some(2));

    let storage = Storage::open_in_memory().unwrap();
    let summaries = sync(&server, &storage, &["INBOX", "Archive"]);

    // The failing folder keeps its partial counts alongside the error.
    assert_eq!(summaries.len(), 2);
    let inbox = &summaries[0];
    let err = inbox
        .error
        .as_deref()
        .expect("the broken folder must carry its error");
    assert_eq!(inbox.folder, "INBOX");
    assert_eq!(
        inbox.seen, 50,
        "counts stored before the failure stay in the summary"
    );
    assert_eq!(inbox.inserted, 50);
    assert_eq!(inbox.skipped, 0);
    assert_eq!(inbox.failed, 0);
    assert_eq!(inbox.uidvalidity, 7);
    let _ = err;

    // The folder after the broken one still fully synced.
    assert_eq!(summaries[1].folder, "Archive");
    assert_eq!(summaries[1].error, None);
    assert_eq!(summaries[1].inserted, 1);
    assert_eq!(storage.stats().unwrap().total, 51);

    // The cursor persisted at the batch boundary survives the failure.
    let state = storage
        .get_imap_checkpoint(&instance(&storage), "INBOX")
        .unwrap()
        .unwrap();
    assert_eq!(state.last_uid, 50);

    // With the server failure gone, a retry resumes from the persisted
    // cursor: only UIDs 51..=60 are fetched — no refetch of the first
    // batch, no duplicates.
    let summaries = sync(&server, &storage, &["INBOX"]);
    let inbox = &summaries[0];
    assert_eq!(inbox.error, None);
    assert_eq!(inbox.seen, 10);
    assert_eq!(inbox.inserted, 10);
    assert_eq!(storage.stats().unwrap().total, 61);
    // The retry fetched exactly the ten remaining UIDs — the first batch
    // was not refetched.
    assert_eq!(
        server.log().fetch_sets.last().unwrap(),
        "51,52,53,54,55,56,57,58,59,60"
    );
}

/// A blackholed host (SYN silently dropped) must fail within the bounded
/// connect phase instead of hanging on the OS default, which can take
/// minutes.
#[test]
fn connect_to_blackholed_host_fails_within_the_connect_timeout() {
    let mut config = vimap::ImapConfig::gmail("user@example.com", "not-the-real-password");
    config.host = "10.255.255.1".to_string();
    let start = std::time::Instant::now();
    let result = vimap::connect(&config);
    let elapsed = start.elapsed();
    assert!(result.is_err(), "a blackholed host must fail, not hang");
    // On a host that blackholes SYNs to this address, the 30 s
    // CONNECT_TIMEOUT engages instead of the OS default (which would hang
    // well past this); on a host with no route, the connect fails fast with
    // ENETUNREACH — both are passes. Only an unbounded hang must fail.
    assert!(
        elapsed < std::time::Duration::from_secs(90),
        "connect must be bounded (well under the OS default), took {elapsed:?}"
    );
}

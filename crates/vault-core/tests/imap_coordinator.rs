//! M2.3 coordinator regression suite: whole rounds over the real M2.1
//! pool (writer worker, two readers, job runner) against the in-process
//! mock IMAP server.
//!
//! The proofs this suite owns:
//!
//! - **interruption mid-fetch** — the connection dies between batches;
//!   the atomic batch commit leaves the checkpoint exactly at the
//!   boundary of what is durably stored, and the next round resumes
//!   without duplicates;
//! - **mid-batch storage failure** — a fault injected *inside* the batch
//!   transaction rolls back the whole batch: no messages, no checkpoint
//!   advance, no ledger rows from it;
//! - **checkpoint recovery** — a pool restart (the process-crash
//!   equivalent) resumes from the persisted checkpoint through a fresh
//!   pool over the same file;
//! - **missing fetch** — a searched uid the server never delivers is
//!   ledgered durably and retried by later rounds;
//! - **per-source exclusion** — an excluded source is skipped while its
//!   archive and cursors are kept;
//! - **search works while the network stands still** — during a round
//!   parked on a stalled fetch, searches and writes keep completing.

#[path = "imap_mock/mod.rs"]
mod imap_mock;

use std::sync::Arc;
use std::time::{Duration, Instant};

use imap_mock::{MockImap, MockMailbox};
use vault_core::imap::ImapAuth;
use vault_core::imap_coordinator::{
    sync_round_with, RoundSummary, SourceSyncStatus, StoredImapConfig,
};
use vault_core::jobs::{DbPool, PoolConfig};
use vault_core::message::Message;
use vault_core::search::build_match_query;
use vault_core::storage::SourceInstance;

const USER: &str = "alice@example.com";
const OTHER_USER: &str = "bob@example.com";
const PASSWORD: &str = "coordinator-proof-password";
const MOCK_HOST: &str = "mock.test";

/// One raw test mail with a unique search-handle body.
fn mail(tag: &str, i: usize) -> Vec<u8> {
    format!(
        "From: Alice Smith <alice@example.com>\r\n\
         To: Bob <bob@example.com>\r\n\
         Subject: {tag} mail {i}\r\n\
         Message-ID: <{tag}-{i}@coordinator.test>\r\n\
         Date: Sat, 1 Nov 2026 10:00:00 +0100\r\n\
         \r\n\
         {tag}body{i}"
    )
    .into_bytes()
}

fn mails(n: usize, tag: &str) -> Vec<Vec<u8>> {
    (1..=n).map(|i| mail(tag, i)).collect()
}

/// A mail whose subject detonates the rollback-bomb trigger (see
/// `mid_batch_storage_failure_rolls_back_the_whole_batch`).
fn bomb_mail(tag: &str, i: usize) -> Vec<u8> {
    format!(
        "From: Alice Smith <alice@example.com>\r\n\
         To: Bob <bob@example.com>\r\n\
         Subject: {tag} bomb {i}\r\n\
         Message-ID: <{tag}-{i}@coordinator.test>\r\n\
         Date: Sat, 1 Nov 2026 10:00:00 +0100\r\n\
         \r\n\
         {tag}bombbody{i}"
    )
    .into_bytes()
}

/// A pool over a fresh temp vault file.
fn pool(tmp: &tempfile::TempDir) -> Arc<DbPool> {
    DbPool::spawn(tmp.path().join("vault.db"), PoolConfig::default(), |_| {}).unwrap()
}

/// Poll until `cond` holds or `timeout` elapses.
fn eventually(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

/// The credential resolver every plain round uses: the password for
/// anyone (tests), like the app's keyring resolver does.
fn password_for(_instance: &SourceInstance) -> Option<ImapAuth> {
    Some(ImapAuth::Password(PASSWORD.to_string()))
}

/// Run one round synchronously on this thread (no cancellation).
fn run_round(pool: &Arc<DbPool>) -> RoundSummary {
    sync_round_with(pool, password_for, || false, mock_connect)
}

/// The mock session builder: plain TCP to the mock server at the
/// config's port, LOGIN with the config's carried credential — the
/// no-TLS equivalent of the production implicit-TLS connect.
fn mock_connect(
    config: &vault_core::imap::ImapConfig,
) -> vault_core::Result<imap::Session<std::net::TcpStream>> {
    let stream = std::net::TcpStream::connect(("127.0.0.1", config.port))?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;
    let mut client = imap::Client::new(stream);
    client.read_greeting()?;
    let password = match &config.auth {
        ImapAuth::Password(password) => password.clone(),
        ImapAuth::Xoauth2 { access_token } => access_token.clone(),
    };
    Ok(client
        .login(config.username.clone(), password)
        .map_err(|(e, _client)| e)?)
}

/// The stored source instances the coordinator sees.
fn sources(pool: &Arc<DbPool>) -> Vec<SourceInstance> {
    pool.with_read(|s| s.list_source_instances().unwrap())
}

/// Search the archive for one phrase.
fn find(pool: &Arc<DbPool>, phrase: &str) -> Vec<Message> {
    let query = build_match_query(phrase);
    pool.with_read(|s| s.search_filtered(&query, None, None, 100).unwrap())
}

fn total(pool: &Arc<DbPool>) -> u64 {
    pool.with_read(|s| s.stats().unwrap().total)
}

fn config_for(user: &str, port: u16) -> vault_core::imap::ImapConfig {
    vault_core::imap::ImapConfig {
        host: MOCK_HOST.to_string(),
        port,
        username: user.to_string(),
        auth: ImapAuth::Password(PASSWORD.to_string()),
        folders: Vec::new(),
    }
}

/// Register a configured IMAP source instance in the vault without
/// syncing it (the dead-port source needs this — it can never connect).
fn register_source(pool: &Arc<DbPool>, config: &vault_core::imap::ImapConfig) {
    let config = config.clone();
    pool.submit_write(move |s| vault_core::imap::ensure_source_instance(s, &config))
        .unwrap();
}

fn first_synced(round: &RoundSummary) -> &vault_core::imap::FolderSyncSummary {
    let outcome = round
        .outcomes
        .iter()
        .find(|o| matches!(o.status, SourceSyncStatus::Synced { .. }))
        .expect("one synced source");
    let SourceSyncStatus::Synced { folders } = &outcome.status else {
        unreachable!()
    };
    &folders[0]
}

// ---------------------------------------------------------------------------
// Per-source exclusion
// ---------------------------------------------------------------------------

#[test]
fn a_round_syncs_every_configured_source_and_exclusion_skips_one() {
    let server = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(7, mails(3, "alice")));
    server.set_mailbox("INBOX_bob", MockMailbox::new(5, mails(2, "bob")));
    // Two accounts on one backend; bob's folders list selects his box.
    let alice_port = server.addr.port();
    let bob_port = server.addr.port();

    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    register_source(&pool, &config_for(USER, alice_port));
    register_source(
        &pool,
        &vault_core::imap::ImapConfig {
            folders: vec!["INBOX_bob".into()],
            ..config_for(OTHER_USER, bob_port)
        },
    );

    // Round 1: both sources sync (each user's login sees the shared
    // server; the folder list picks the mailbox).
    let round = run_round(&pool);
    assert!(round.excluded.is_empty());
    assert_eq!(round.outcomes.len(), 2);
    assert_eq!(total(&pool), 5);

    // Exclude bob: the round skips him, keeps his archive and cursors,
    // and reports the exclusion.
    let bob = sources(&pool)
        .into_iter()
        .find(|i| username_of(i).as_deref() == Some(OTHER_USER))
        .unwrap();
    let id = bob.id.clone();
    pool.submit_write(move |s| s.set_source_excluded(&id, true))
        .unwrap();

    server.add_message("INBOX", mail("alice", 4));
    server.add_message("INBOX_bob", mail("bob", 3));
    let round = run_round(&pool);
    assert_eq!(round.excluded.len(), 1, "bob must be reported excluded");
    assert_eq!(round.outcomes.len(), 1, "only alice syncs");
    // Alice's new mail arrived; bob's did not (excluded, not synced).
    assert_eq!(total(&pool), 6);
    assert_eq!(find(&pool, "alicebody4").len(), 1);
    assert!(find(&pool, "bobbody3").is_empty());

    // Re-include: bob syncs again, and his missed mail lands — plus the
    // exclusion deleted nothing.
    let bob = sources(&pool)
        .into_iter()
        .find(|i| username_of(i).as_deref() == Some(OTHER_USER))
        .unwrap();
    let id = bob.id.clone();
    pool.submit_write(move |s| s.set_source_excluded(&id, false))
        .unwrap();
    let round = run_round(&pool);
    assert!(round.excluded.is_empty());
    assert_eq!(round.outcomes.len(), 2);
    assert_eq!(total(&pool), 7);
    assert_eq!(find(&pool, "bobbody3").len(), 1);
    pool.shutdown();
}

/// The username of a stored instance's configuration (None for anything
/// the IMAP connector cannot own).
fn username_of(instance: &SourceInstance) -> Option<String> {
    StoredImapConfig::of(instance).map(|c| c.username)
}

// ---------------------------------------------------------------------------
// Credential resolution outcomes
// ---------------------------------------------------------------------------

#[test]
fn a_source_without_credentials_is_reported_not_errored() {
    let server = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(7, mails(2, "creds")));

    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    register_source(&pool, &config_for(USER, server.addr.port()));

    // The resolver has nothing stored (the keyring emptied).
    let round = sync_round_with(&pool, |_| None, || false, mock_connect);
    assert_eq!(round.outcomes.len(), 1);
    assert_eq!(round.outcomes[0].status, SourceSyncStatus::NoCredentials);
    assert_eq!(total(&pool), 0, "no credentials means no sync attempt");

    // Credentials return: the same round syncs.
    let round = run_round(&pool);
    assert!(matches!(
        round.outcomes[0].status,
        SourceSyncStatus::Synced { .. }
    ));
    assert_eq!(total(&pool), 2);
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Interruption mid-fetch: atomic batches leave no disagreement
// ---------------------------------------------------------------------------

/// The connection dies after the first batch of 110 mails. The atomic
/// batch commit guarantees: every stored uid is covered by the
/// checkpoint, no checkpoint value exceeds the stored content, and the
/// next round resumes exactly at the batch boundary.
#[test]
fn interruption_mid_fetch_leaves_checkpoint_and_archive_in_agreement() {
    let server = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(7, mails(110, "cut")));
    server.drop_after_uid_fetch_once(Some(1));

    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    register_source(&pool, &config_for(USER, server.addr.port()));

    let round = run_round(&pool);
    let s = first_synced(&round);
    assert!(s.error.is_some(), "the severed connection surfaces");
    assert_eq!(s.inserted, 50);
    assert_eq!(total(&pool), 50);

    // The atomicity invariant, directly: the checkpoint is exactly the
    // highest durably stored uid — neither behind (stored-but-uncovered,
    // the pre-M2.3 crash window) nor ahead of it.
    let instance = &sources(&pool)[0];
    let state = pool
        .with_read(|st| st.get_imap_checkpoint(&instance.id, "INBOX").unwrap())
        .expect("batch 1's checkpoint survived");
    assert_eq!((state.uidvalidity, state.last_uid), (7, 50));
    // The atomicity invariant, directly: the checkpoint covers exactly
    // the durably stored content — the batch's last mail is in, the
    // next batch's first mail is not (neither side disagrees).
    assert_eq!(find(&pool, "cutbody50").len(), 1);
    assert_eq!(
        find(&pool, "cutbody51").len(),
        0,
        "no stored mail past the checkpoint"
    );

    // The next round resumes from the persisted cursor: exactly the two
    // remaining batches, no duplicates.
    let fetches_before = server.log().fetch_sets.len();
    let round = run_round(&pool);
    let s = first_synced(&round);
    assert_eq!(s.error, None);
    assert_eq!(s.inserted, 60);
    let fetches = &server.log().fetch_sets[fetches_before..];
    assert_eq!(fetches, &["51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100", "101,102,103,104,105,106,107,108,109,110"]);
    assert_eq!(total(&pool), 110, "no duplicates after the resume");
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Mid-batch storage failure: the whole batch rolls back
// ---------------------------------------------------------------------------

/// A trigger detonates inside the batch transaction (uid 53 of the
/// second batch). The atomic commit rolls back the *entire* batch: no
/// messages from it, no checkpoint advance, no ledger rows — where the
/// pre-M2.3 per-message commits would have left uids 51 and 52 stored
/// with a cursor still at 50.
#[test]
fn mid_batch_storage_failure_rolls_back_the_whole_batch() {
    let server = MockImap::start();
    // Batch 1: uids 1..=50 clean. Batch 2: 51, 52 clean, 53 the bomb.
    let mut batch = mails(50, "clean");
    batch.push(mail("torn", 51));
    batch.push(mail("torn", 52));
    batch.push(bomb_mail("torn", 53));
    batch.extend((54..=110).map(|i| mail("torn", i as usize))); // uids 54..=110
    server.set_mailbox("INBOX", MockMailbox::new(7, batch));

    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    register_source(&pool, &config_for(USER, server.addr.port()));

    // The rollback bomb: an ordinary SQLite trigger that aborts the
    // insert of the bomb-subject row — a fault *inside* the batch
    // transaction, on the writer lane, from outside the pool.
    {
        let conn = rusqlite::Connection::open(tmp.path().join("vault.db")).unwrap();
        conn.execute(
            "CREATE TRIGGER rollback_bomb BEFORE INSERT ON archive_messages
             WHEN NEW.subject = 'torn bomb 53'
             BEGIN SELECT RAISE(ABORT, 'batch bomb'); END",
            [],
        )
        .unwrap();
    }

    let round = run_round(&pool);
    let s = first_synced(&round);
    assert!(s.error.is_some(), "the injected fault surfaces as an error");
    assert_eq!(s.inserted, 50, "batch 1 fully stored before the fault");

    // THE PROOF: the aborted batch left *nothing* — not uids 51/52, not
    // a cursor past 50, not a single ledger row.
    assert_eq!(total(&pool), 50);
    let instance = &sources(&pool)[0];
    let state = pool
        .with_read(|st| st.get_imap_checkpoint(&instance.id, "INBOX").unwrap())
        .unwrap();
    assert_eq!(
        state.last_uid, 50,
        "no checkpoint advance from a dead batch"
    );
    assert!(
        pool.with_read(|st| st.list_failed_items(None).unwrap().is_empty()),
        "no ledger rows from a dead batch"
    );

    // Defuse the bomb; the next round refetches the whole second batch
    // and stores every mail exactly once.
    {
        let conn = rusqlite::Connection::open(tmp.path().join("vault.db")).unwrap();
        conn.execute("DROP TRIGGER rollback_bomb", []).unwrap();
    }
    let round = run_round(&pool);
    let s = first_synced(&round);
    assert_eq!(s.error, None);
    assert_eq!(s.inserted, 60);
    assert_eq!(total(&pool), 110, "exactly the 110 mails, no duplicates");
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Checkpoint recovery across a pool restart
// ---------------------------------------------------------------------------

/// The pool is torn down mid-folder (after batch 1) and a fresh pool
/// opens the same file: the persisted checkpoint drives the resume —
/// the crash-recovery path of the atomic batch commits.
#[test]
fn checkpoint_recovery_across_a_pool_restart() {
    let server = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(7, mails(110, "restart")));
    server.drop_after_uid_fetch_once(Some(1));

    let tmp = tempfile::tempdir().unwrap();
    {
        let pool = pool(&tmp);
        register_source(&pool, &config_for(USER, server.addr.port()));
        let round = run_round(&pool);
        assert!(first_synced(&round).error.is_some());
        assert_eq!(total(&pool), 50);
        pool.shutdown();
    } // the "process" dies here

    // A fresh pool over the same vault file: the checkpoint (7, 50)
    // persisted by batch 1's atomic commit drives the resume.
    let pool = pool(&tmp);
    let fetches_before = server.log().fetch_sets.len();
    let round = run_round(&pool);
    let s = first_synced(&round);
    assert_eq!(s.error, None);
    assert_eq!(s.inserted, 60);
    assert_eq!(
        &server.log().fetch_sets[fetches_before..],
        &["51,52,53,54,55,56,57,58,59,60,61,62,63,64,65,66,67,68,69,70,71,72,73,74,75,76,77,78,79,80,81,82,83,84,85,86,87,88,89,90,91,92,93,94,95,96,97,98,99,100", "101,102,103,104,105,106,107,108,109,110"],
        "the restarted round resumes from the persisted checkpoint"
    );
    assert_eq!(total(&pool), 110);
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Missing fetch over the pool: durable, retried, resolved
// ---------------------------------------------------------------------------

#[test]
fn a_missing_fetch_over_the_pool_is_ledgered_and_heals_next_round() {
    let server = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(7, mails(3, "poolmiss")));
    server.hide_uid_from_fetch_once(3);

    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    register_source(&pool, &config_for(USER, server.addr.port()));
    let round = run_round(&pool);
    let s = first_synced(&round);
    assert_eq!(s.error, None);
    assert_eq!(s.inserted, 2);
    assert_eq!(s.failed, 1);

    let instance = sources(&pool).remove(0);
    let rows = pool.with_read(|st| st.list_failed_items(Some(&instance.id)).unwrap());
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].uid, rows[0].reason.as_str()),
        (3, vault_core::imap::REASON_MISSING_FETCH)
    );

    let round = run_round(&pool);
    let s = first_synced(&round);
    assert_eq!(s.resolved_failures, 1);
    assert_eq!(total(&pool), 3);
    assert!(pool.with_read(|st| st.list_failed_items(Some(&instance.id)).unwrap().is_empty()));
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// A failing source never blocks the others
// ---------------------------------------------------------------------------

#[test]
fn an_unreachable_source_fails_alone_and_the_rest_still_sync() {
    let server = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(7, mails(2, "alive")));

    // A port that refuses connections: bind, note the port, close.
    let dead_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        port
    };

    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    register_source(&pool, &config_for(OTHER_USER, dead_port));
    register_source(&pool, &config_for(USER, server.addr.port()));

    let round = run_round(&pool);
    assert_eq!(round.outcomes.len(), 2);
    let dead = round
        .outcomes
        .iter()
        .find(|o| matches!(o.status, SourceSyncStatus::Failed { .. }))
        .expect("the unreachable source fails");
    let SourceSyncStatus::Failed { error } = &dead.status else {
        unreachable!()
    };
    assert!(!error.is_empty());
    // The healthy source archived fully despite its neighbor being down,
    // and the archive still answers searches.
    assert_eq!(total(&pool), 2);
    assert_eq!(find(&pool, "alivebody2").len(), 1);
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Search works while the network stands still
// ---------------------------------------------------------------------------

/// The M2.1 contract at coordinator scale: a round parked on a stalled
/// fetch occupies neither the write lane nor a read connection — a
/// search and a plain write complete while the network is silent, and
/// the round finishes correctly once it moves again.
#[test]
fn searches_and_writes_work_while_a_round_is_parked_on_the_network() {
    let server = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(7, mails(120, "stall")));
    // The second fetch batch stalls: the round has committed batch 1
    // (50 mails) and sits on the network from then on.
    server.stall_uid_fetch_once(2);

    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(&tmp);
    register_source(&pool, &config_for(USER, server.addr.port()));

    let round_pool = Arc::clone(&pool);
    let handle = std::thread::spawn(move || run_round(&round_pool));

    // Wait until the round is parked: batch 1 is durably stored.
    assert!(
        eventually(Duration::from_secs(10), || total(&pool) >= 50),
        "the stalled round committed its first batch"
    );

    // THE PROOF: while the network stands still, the write lane is free…
    let start = Instant::now();
    pool.submit_write(|s| {
        s.insert_message(&vault_core::message::NewMessage::new(
            "pool-test",
            "during-stall",
            "ana",
            "2026-11-05T10:00:00Z",
            "needle written during the coordinator stall",
        ))?;
        Ok(())
    })
    .expect("a write must complete while the round is parked on the network");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the write waited {:?} behind a parked round — the network is occupying the write lane",
        start.elapsed()
    );
    // …and searches answer over the same file.
    assert_eq!(find(&pool, "stallbody50").len(), 1);
    assert_eq!(find(&pool, "needle written during").len(), 1);

    // Release the network: the round completes with every mail stored.
    server.release_stall();
    let round = handle.join().unwrap();
    assert_eq!(first_synced(&round).error, None);
    assert_eq!(total(&pool), 120 + 1);
    pool.shutdown();
}

// ---------------------------------------------------------------------------
// Stored configuration parsing
// ---------------------------------------------------------------------------

#[test]
fn stored_config_parses_only_imap_connector_instances() {
    let mut instance = SourceInstance {
        id: "i".into(),
        kind: "imap".into(),
        label: "imap:mock.test:993:alice@example.com".into(),
        connector_key: Some("imap|mock.test|993|implicit-tls|alice@example.com".into()),
        config_json: serde_json::json!({
            "host": "mock.test",
            "port": 993,
            "tls_mode": "implicit-tls",
            "username": "alice@example.com",
            "folders": ["INBOX", "Archive"],
        })
        .to_string(),
        state: "connected".into(),
        excluded: false,
        last_attempt_at: None,
        last_success_at: None,
        created_at: "2026-11-01T00:00:00Z".into(),
    };
    let stored = StoredImapConfig::of(&instance).unwrap();
    assert_eq!(
        (stored.host.as_str(), stored.port, stored.username.as_str()),
        ("mock.test", 993, "alice@example.com")
    );
    assert_eq!(stored.folders, vec!["INBOX", "Archive"]);

    // Not an IMAP connector instance: skipped.
    instance.kind = "legacy".into();
    instance.connector_key = None;
    assert!(StoredImapConfig::of(&instance).is_none());

    // A foreign TLS mode is a different connector, not a reinterpretation.
    instance.kind = "imap".into();
    instance.connector_key = Some("k".into());
    instance.config_json = serde_json::json!({
        "host": "mock.test", "port": 143, "tls_mode": "starttls",
        "username": "a", "folders": []
    })
    .to_string();
    assert!(StoredImapConfig::of(&instance).is_none());

    // Malformed JSON is skipped, not fatal.
    instance.config_json = "{not json".into();
    assert!(StoredImapConfig::of(&instance).is_none());
}

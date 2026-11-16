//! Schema migrations over databases from previous schema versions: fresh
//! applies, reopens apply nothing new, a pre-migration ("version 0")
//! database upgrades in place, a half-applied or blocked migration is either
//! detected with a clear error or rolled back completely (all pending
//! migrations commit in one single transaction, M1.4), and a database from
//! a *newer* version is refused (the deeper safe-migration coverage lives
//! in `safe_migration.rs`).

use rusqlite::Connection;
use vault_core::message::InsertOutcome;
use vault_core::storage::Storage;

use crate::util::{migrations_table, msg, raw, version1_db_with_data};

/// The schema version this build knows; bump expectation when a migration
/// is added (the assertions below are version-agnostic where possible).
const KNOWN_VERSION: i64 = 20;
// The applied ledger is contiguous: 1-19 (18 saved searches, 19
// collections) plus 20 (people) — the reservation notes no longer apply.
const APPLIED_VERSIONS: &[i64] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
];

fn migrations_rows(db: &std::path::Path) -> Vec<i64> {
    let conn = raw(db);
    let mut stmt = conn
        .prepare("SELECT version FROM schema_migrations ORDER BY version")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, i64>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// Craft a database in the "version 0" state: the migrations bookkeeping
/// table exists (as an older release would have left it) but no schema
/// version has been applied yet.
fn version0_db(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    migrations_table(&conn);
}

/// The named indexes present on `archive_messages` (raw sqlite_master
/// read — the upgrade path's end state, not any accessor).
fn archive_message_indexes(db: &std::path::Path) -> Vec<String> {
    let conn = raw(db);
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'index' AND tbl_name = 'archive_messages'",
        )
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn fresh_database_applies_every_migration_exactly_once() {
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("v.db");

    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), KNOWN_VERSION);
    assert_eq!(migrations_rows(&db), APPLIED_VERSIONS.to_vec());

    // The migrated schema is immediately functional.
    let out = s
        .insert_message(&msg("integrity", "m1", "migration smoke body"))
        .unwrap();
    assert!(matches!(out, InsertOutcome::Inserted(_)));
    drop(s);

    // Reopening applies nothing new, records nothing extra, keeps the data.
    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), KNOWN_VERSION);
    assert_eq!(
        migrations_rows(&db),
        APPLIED_VERSIONS.to_vec(),
        "no duplicate bookkeeping rows"
    );
    assert_eq!(s.stats().unwrap().total, 1);
    assert_eq!(
        s.search_filtered("smoke", None, None, None, false, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn version0_database_upgrades_in_place() {
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("old.db");
    version0_db(&db);

    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), KNOWN_VERSION);
    assert_eq!(migrations_rows(&db), APPLIED_VERSIONS.to_vec());
    // Migration 17's cleanup holds on the upgraded schema: migration 15's
    // composite stays, and the single-column index it made redundant
    // (a strict prefix, created by migration 4) is gone.
    let indexes = archive_message_indexes(&db);
    assert!(
        indexes
            .iter()
            .any(|n| n == "idx_archive_messages_source_channel"),
        "the composite covering index must survive the upgrade, got {indexes:?}"
    );
    assert!(
        !indexes.iter().any(|n| n == "idx_archive_messages_source"),
        "migration 17 must drop the redundant prefix index, got {indexes:?}"
    );
    // The upgraded database works end to end: write, index, read.
    s.insert_message(&msg("integrity", "up-1", "upgraded archive body"))
        .unwrap();
    assert_eq!(
        s.search_filtered("upgraded", None, None, None, false, 10)
            .unwrap()
            .len(),
        1
    );
}

/// The frozen v1 schema and the v1/v2 fixture builders now live in
/// `util.rs`, shared with the safe-migration suite (`safe_migration.rs`).

#[test]
fn v1_database_with_data_upgrades_in_place_to_current() {
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("v1.db");
    version1_db_with_data(&db, 7);

    // Opening with the current build applies every newer migration (2–7:
    // IMAP sync state, embeddings, the canonical store, watched folders,
    // the stats counters, the conversation-key backfill)
    // on top of the untouched v1 schema - in place, keeping every row (ids
    // included) in the canonical store.
    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), KNOWN_VERSION);
    assert_eq!(migrations_rows(&db), APPLIED_VERSIONS.to_vec());

    // The v1 archive is fully intact and searchable (through the canonical
    // tables the migration copied it into).
    let stats = s.stats().unwrap();
    assert_eq!(stats.total, 7);
    assert_eq!(stats.by_source.len(), 1);
    assert_eq!(
        s.search_filtered("vintage", None, None, None, false, 10)
            .unwrap()
            .len(),
        7
    );
    assert_eq!(
        s.search_filtered("\"v1 era\"", None, None, None, false, 10)
            .unwrap()
            .len(),
        7
    );
    let ids: Vec<i64> = {
        let conn = raw(&db);
        let mut stmt = conn
            .prepare("SELECT id FROM archive_messages ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(ids, (1..=7).collect::<Vec<_>>(), "v1 row ids preserved");

    // The schema v2 addition is live: the IMAP cursor round-trips.
    assert!(s.get_imap_state("imap/gmail", "INBOX").unwrap().is_none());
    s.set_imap_state("imap/gmail", "INBOX", 42, 1234).unwrap();
    let state = s
        .get_imap_state("imap/gmail", "INBOX")
        .unwrap()
        .expect("cursor present after upsert");
    assert_eq!(state.uidvalidity, 42);
    assert_eq!(state.last_uid, 1234);

    // The schema v3 addition is live: the embedding table round-trips.
    {
        use vault_core::vector::{run_embedding_job, Embedder, HashEmbedder};
        let embedder = HashEmbedder::new();
        let summary = run_embedding_job(&s, &embedder, None).unwrap();
        assert_eq!(summary.embedded, 7, "every upgraded row embeds");
        let hits = s
            .vector_scan(
                embedder.id(),
                embedder.dim(),
                &embedder.embed("vintage body of the v1 era"),
                None,
                None,
                None,
                false,
                3,
            )
            .unwrap();
        assert!(!hits.is_empty(), "the scan reads migrated rows");
    }

    // The schema v5 addition is live: watched folders round-trip.
    assert!(s.watched_folders().unwrap().is_empty());
    s.add_watched_folder("/tmp/exports").unwrap();
    assert_eq!(
        s.watched_folders().unwrap(),
        vec!["/tmp/exports".to_string()]
    );

    // The v6 additions are live too: the counter tables exist and their
    // backfilled counts match the migrated rows.
    let raw_conn = raw(&db);
    let tables: Vec<String> = {
        let mut stmt = raw_conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'stats_%'")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    for expected in ["stats_source", "stats_day", "stats_chat", "stats_author"] {
        assert!(
            tables.iter().any(|n| n == expected),
            "migration 6 must create {expected}, got {tables:?}"
        );
    }
    drop(raw_conn);
    let dash = s.archive_stats(10).unwrap();
    assert_eq!(dash.total, 7);
    assert_eq!(dash.first_sent_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    assert_eq!(dash.by_source.len(), 1);

    // Idempotence: reopening applies nothing new and everything is still there.
    drop(s);
    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), KNOWN_VERSION);
    assert_eq!(migrations_rows(&db), APPLIED_VERSIONS.to_vec());
    assert_eq!(s.stats().unwrap().total, 7);
    let state = s
        .get_imap_state("imap/gmail", "INBOX")
        .unwrap()
        .expect("cursor survives reopen");
    assert_eq!(state.last_uid, 1234);
    assert_eq!(
        s.watched_folders().unwrap(),
        vec!["/tmp/exports".to_string()]
    );
}

#[test]
fn half_applied_schema_fails_with_a_clear_error() {
    // Simulate a database stuck between DDL and bookkeeping: the messages
    // table exists but version 1 was never recorded. (The real migration
    // runner wraps each migration in a transaction, so this state is
    // unreachable in practice — but if it ever occurs, opening must fail
    // loudly instead of silently half-working.)
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("half.db");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version    INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE messages (id INTEGER PRIMARY KEY, source TEXT NOT NULL, external_id TEXT NOT NULL);",
        )
        .unwrap();
    }

    let err = Storage::open(&db).expect_err("half-applied schema must refuse to open");
    let text = err.to_string();
    assert!(
        text.contains("already exists"),
        "error must say what collides, got: {text}"
    );
}

#[test]
fn failed_migration_rolls_back_completely() {
    // Block migration 1 mid-flight with a name collision (a view occupying
    // `messages_fts`): the migration fails, and — because it ran inside a
    // transaction — leaves *no* partial schema behind. Removing the blocker
    // lets the very next open succeed.
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("blocked.db");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version    INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE VIEW messages_fts AS SELECT 1;",
        )
        .unwrap();
    }

    let err = Storage::open(&db).expect_err("blocked migration must fail");
    assert!(
        err.to_string().contains("already exists"),
        "error must name the colliding object, got: {err}"
    );

    // Atomicity: nothing of the failed migration survived — no messages
    // table, no index, no triggers, no version row.
    let conn = raw(&db);
    let leftovers: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','index','trigger') AND name NOT LIKE 'sqlite_%'")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(
        leftovers,
        vec!["schema_migrations".to_string()],
        "failed migration must leave no partial schema"
    );
    drop(conn);

    // Clear the blocker; the next open applies the migration cleanly.
    let conn = raw(&db);
    conn.execute_batch("DROP VIEW messages_fts;").unwrap();
    drop(conn);
    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), KNOWN_VERSION);
    s.insert_message(&msg("integrity", "after-unblock", "post rollback body"))
        .unwrap();
    assert_eq!(
        s.search_filtered("rollback", None, None, None, false, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn newer_schema_version_is_refused() {
    // M1.4 newer-schema refusal: a database written by a *newer* build
    // (a version beyond anything this binary knows) must be refused
    // with a clear error instead of being silently opened by an older
    // tool that could mis-read or half-write it. Forward compatibility
    // is no longer claimed - upgrade the app first. The refusal happens
    // before any pragma or migration, so the file is left exactly as it
    // was; the byte-level proof lives in safe_migration.rs.
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("future.db");
    {
        let conn = Connection::open(&db).unwrap();
        migrations_table(&conn);
        conn.execute("INSERT INTO schema_migrations (version) VALUES (99)", [])
            .unwrap();
    }

    let err = Storage::open(&db).expect_err("newer schema must be refused");
    assert!(
        matches!(err, vault_core::Error::SchemaTooNew { db: 99, .. }),
        "expected SchemaTooNew, got: {err}"
    );
    assert_eq!(
        migrations_rows(&db),
        vec![99],
        "refusal must not modify the database"
    );
    // The refusal is stable: retrying changes nothing.
    assert!(Storage::open(&db).is_err());
    assert_eq!(migrations_rows(&db), vec![99]);
}

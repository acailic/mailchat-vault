//! Safe migration (M1.4): pre-migration snapshot, capacity preflight,
//! single-transaction conversion, post-conversion validation, rollback/
//! reopen, and newer-schema refusal.
//!
//! The core contract, asserted with failures injected at every stage
//! ([`vault_core::safe_migration::OpenOptions`]): a failure **before**,
//! **within**, or **after** the migration never strands or modifies the
//! original archive — the database file stays byte-identical (when a
//! snapshot was taken it is restored from it), the snapshot copy stays
//! readable, and the next open retries the conversion on the intact
//! original. A failed post-conversion *validation*, however, is recorded
//! durably: the next open refuses with that recorded failure instead of
//! re-running the same deterministic failure forever.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use vault_core::safe_migration::{
    failed_marker_path, snapshot_path, MigrationFailPoint, OpenOptions,
};
use vault_core::storage::Storage;
use vault_core::Error;

use crate::util::{hash_file, raw, scratch_dir, version2_db_with_data};

const KNOWN_VERSION: i64 = 12;
const ROWS: usize = 9;

fn v2_fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = scratch_dir();
    let db = dir.path().join("legacy.db");
    version2_db_with_data(&db, ROWS);
    (dir, db)
}

fn opts(fail_at: MigrationFailPoint) -> OpenOptions {
    OpenOptions {
        fail_at,
        ..OpenOptions::default()
    }
}

/// Every user-visible row of the legacy archive, straight from the frozen
/// legacy table (the fixture's ground truth).
fn legacy_rows(db: &Path) -> Vec<(i64, String)> {
    let conn = raw(db);
    let mut stmt = conn
        .prepare("SELECT id, external_id FROM messages ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// The snapshot must be a complete, independently openable copy of the
/// pre-migration archive.
fn assert_snapshot_readable(snapshot: &Path) {
    assert!(
        snapshot.exists(),
        "snapshot must be kept: {}",
        snapshot.display()
    );
    let conn = Connection::open_with_flags(snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("snapshot opens read-only");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("snapshot legacy table readable: {e}"));
    assert_eq!(n as usize, ROWS, "snapshot holds the whole archive");
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("snapshot bookkeeping readable");
    assert_eq!(version, 2, "snapshot is the pre-migration original");
}

/// The live database is back to the exact pre-migration original: schema
/// version 2, all legacy rows, no canonical tables.
fn assert_live_db_is_original(db: &Path) {
    let conn = raw(db);
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(version, 2, "live database restored to the original version");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n as usize, ROWS, "every original row is still there");
    let canonical: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'archive_messages')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!canonical, "no canonical leftovers after rollback");
}

/// The reopen path: after a failed conversion, the next open retries on
/// the intact original and completes the whole conversion.
fn assert_reopen_completes_migration(db: &Path) {
    let s = Storage::open(db).expect("reopen after rollback must succeed");
    assert_eq!(s.schema_version().unwrap(), KNOWN_VERSION);
    assert_eq!(s.stats().unwrap().total, ROWS as u64);
    assert_eq!(
        s.search_filtered("vintage", None, None, 100).unwrap().len(),
        ROWS
    );
    // The IMAP cursor carried over from the legacy table, not lost.
    let state = s
        .get_imap_state("imap/gmail", "INBOX")
        .unwrap()
        .expect("legacy cursor migrated");
    assert_eq!(state.last_uid, 42);
    drop(s);
    // The successful, validated conversion consumed the snapshot.
    assert!(
        !snapshot_path(db, 2).exists(),
        "validated conversion removes the snapshot"
    );
}

// ----------------------------------------------------------------------
// Newer-schema refusal
// ----------------------------------------------------------------------

#[test]
fn newer_schema_is_refused_without_touching_the_file() {
    let dir = scratch_dir();
    let db = dir.path().join("future.db");
    {
        let conn = raw(&db);
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version    INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE from_the_future (x);
            INSERT INTO schema_migrations (version) VALUES (99);",
        )
        .unwrap();
    }
    let before = hash_file(&db);

    let err = Storage::open(&db).expect_err("newer schema must be refused");
    assert!(
        matches!(
            err,
            Error::SchemaTooNew {
                db: 99,
                supported: 12
            }
        ),
        "expected SchemaTooNew, got: {err}"
    );

    // Not a single byte changed - the refusal happens before any pragma,
    // journal-mode flip, or migration could write.
    assert_eq!(hash_file(&db), before, "refusal must not touch the file");
    // No snapshot was attempted for a refusal either.
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "future.db")
        .collect();
    assert!(leftovers.is_empty(), "no side files: {leftovers:?}");
    // And the refusal is stable across retries.
    assert!(matches!(
        Storage::open(&db),
        Err(Error::SchemaTooNew { .. })
    ));
    assert_eq!(hash_file(&db), before);
}

// ----------------------------------------------------------------------
// Capacity preflight
// ----------------------------------------------------------------------

#[test]
fn insufficient_space_refuses_before_touching_the_archive() {
    let (_dir, db) = v2_fixture();
    let before = hash_file(&db);

    let options = OpenOptions {
        min_free_bytes: Some(u64::MAX), // provably more than any disk has
        ..OpenOptions::default()
    };
    let err = Storage::open_with(&db, &options).expect_err("capacity preflight must refuse");
    assert!(
        matches!(err, Error::InsufficientSpace { .. }),
        "expected InsufficientSpace, got: {err}"
    );

    // The refusal happens before the WAL checkpoint or any copy: the
    // archive is byte-identical and no snapshot/temp file was left.
    assert_eq!(hash_file(&db), before, "preflight refusal must not write");
    let snap = snapshot_path(&db, 2);
    assert!(!snap.exists(), "no snapshot on a refused preflight");
    assert!(!Path::new(&format!("{}.tmp", snap.display())).exists());

    // Without the artificial floor the very same database opens fine.
    assert!(Storage::open(&db).is_ok());
}

// ----------------------------------------------------------------------
// Failure before / within / after the migration
// ----------------------------------------------------------------------

#[test]
fn failure_before_the_snapshot_leaves_the_original_untouched() {
    let (_dir, db) = v2_fixture();
    let before = hash_file(&db);

    let err = Storage::open_with(&db, &opts(MigrationFailPoint::BeforeSnapshot))
        .expect_err("injected failure must propagate");
    assert!(
        matches!(err, Error::MigrationFault("before-snapshot")),
        "expected the injected fault, got: {err}"
    );

    assert_eq!(hash_file(&db), before, "original untouched");
    assert!(!snapshot_path(&db, 2).exists(), "no snapshot yet");
    assert_reopen_completes_migration(&db);
}

#[test]
fn failure_during_the_snapshot_cleans_up_and_keeps_the_original() {
    let (dir, db) = v2_fixture();
    let before = hash_file(&db);

    let err = Storage::open_with(&db, &opts(MigrationFailPoint::DuringSnapshot))
        .expect_err("injected failure must propagate");
    assert!(
        matches!(err, Error::MigrationFault("during-snapshot")),
        "expected the injected fault, got: {err}"
    );

    assert_eq!(hash_file(&db), before, "original untouched");
    // The partial temp copy is gone; no snapshot was published.
    let mut entries: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.retain(|name| name != "legacy.db");
    assert!(
        entries.is_empty(),
        "partial snapshot must be cleaned up: {entries:?}"
    );
    assert_reopen_completes_migration(&db);
}

#[test]
fn failure_within_the_migration_rolls_back_and_keeps_a_readable_snapshot() {
    let (_dir, db) = v2_fixture();
    let before = hash_file(&db);

    // Fail twice in a row: every failed attempt must restore the original
    // and keep a valid snapshot, never degrading the archive.
    for _ in 0..2 {
        let err = Storage::open_with(&db, &opts(MigrationFailPoint::DuringMigration))
            .expect_err("injected failure must propagate");
        assert!(matches!(err, Error::MigrationFault("during-migration")));

        let snapshot = snapshot_path(&db, 2);
        assert_snapshot_readable(&snapshot);
        // Rollback: the live file is byte-identical to the fixture, i.e.
        // the transaction left nothing behind and the restore replaced
        // nothing with something different.
        assert_eq!(hash_file(&db), before, "original restored byte-for-byte");
        assert_live_db_is_original(&db);
    }

    // The reopen path: the next open retries on the intact original.
    assert_reopen_completes_migration(&db);
}

#[test]
fn failure_after_the_migration_restores_the_original_from_the_snapshot() {
    let (_dir, db) = v2_fixture();
    let before = hash_file(&db);
    let snapshot = snapshot_path(&db, 2);

    // The conversion *committed* (schema_version would say so), but the
    // injected failure hits during post-conversion validation: the
    // committed-but-unvalidated state must not survive.
    let err = Storage::open_with(&db, &opts(MigrationFailPoint::AfterMigration))
        .expect_err("injected failure must propagate");
    assert!(matches!(err, Error::MigrationFault("after-migration")));

    assert_snapshot_readable(&snapshot);
    assert_eq!(hash_file(&db), before, "original restored byte-for-byte");
    assert_live_db_is_original(&db);
    assert_reopen_completes_migration(&db);
}

// ----------------------------------------------------------------------
// Pre-drifted legacy index → durable refusal
// ----------------------------------------------------------------------

/// A legacy archive whose frozen `messages_fts` had *already* lost
/// docsize entries before this build ever opened it (e.g. written by an
/// older, buggy build): validation catches the drift, the original is
/// restored, and the failure is recorded durably so every later open
/// refuses with a clear error instead of looping convert-fail-restore
/// forever.
#[test]
fn pre_drifted_legacy_index_records_a_durable_refusal() {
    let (_dir, db) = v2_fixture();
    let snapshot = snapshot_path(&db, 2);
    {
        let conn = raw(&db);
        conn.execute("DELETE FROM messages_fts_docsize WHERE rowid <= 3", [])
            .unwrap();
    }
    let before = hash_file(&db);

    // First open: the conversion commits, validation catches the drift,
    // the original is restored from the snapshot.
    let err = Storage::open(&db).expect_err("pre-drifted legacy index must fail validation");
    assert!(
        matches!(err, Error::MigrationValidationFailed(_)),
        "expected a validation failure, got: {err}"
    );
    assert_eq!(hash_file(&db), before, "original restored byte-for-byte");
    assert_live_db_is_original(&db);
    assert_snapshot_readable(&snapshot);
    assert!(
        failed_marker_path(&db).exists(),
        "failure must be recorded durably"
    );

    // Later opens refuse immediately with the recorded failure — never
    // another convert-fail-restore cycle (the error is the marker's, not
    // a fresh validation error).
    for _ in 0..2 {
        let err = Storage::open(&db).expect_err("the durable marker must refuse the reopen");
        assert!(
            matches!(err, Error::MigrationFailed { .. }),
            "expected the recorded failure, got: {err}"
        );
        let message = err.to_string();
        assert!(
            message.contains("messages_fts"),
            "error names the recorded reason: {message}"
        );
        assert!(
            message.contains(snapshot.to_string_lossy().as_ref()),
            "error points at the recoverable snapshot: {message}"
        );
    }

    assert_live_db_is_original(&db);
    assert!(
        failed_marker_path(&db).exists(),
        "the marker survives refused reopens"
    );
}

// ----------------------------------------------------------------------
// Happy paths
// ----------------------------------------------------------------------

#[test]
fn successful_conversion_validates_and_removes_the_snapshot() {
    let (dir, db) = v2_fixture();
    let legacy_before = legacy_rows(&db);

    let s = Storage::open(&db).expect("conversion succeeds");
    assert_eq!(s.schema_version().unwrap(), KNOWN_VERSION);

    // Index validation is part of the path; prove the index is complete
    // right after open (every migrated row is searchable).
    assert_eq!(s.stats().unwrap().total, ROWS as u64);
    assert_eq!(
        s.search_filtered("vintage", None, None, 100).unwrap().len(),
        ROWS
    );
    // Row ids survived the canonical copy.
    let canonical: Vec<(i64, String)> = {
        let conn = raw(&db);
        let mut stmt = conn
            .prepare("SELECT id, external_id FROM archive_messages ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    assert_eq!(canonical, legacy_before, "ids and keys preserved 1:1");

    drop(s);
    // The validated conversion consumed its snapshot and left nothing else.
    let mut leftovers: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    leftovers.retain(|name| name != "legacy.db" && !name.starts_with("legacy.db-"));
    assert!(
        leftovers.is_empty(),
        "no snapshot or temp files after success: {leftovers:?}"
    );
}

#[test]
fn fresh_database_never_snapshots() {
    let dir = scratch_dir();
    let db = dir.path().join("fresh.db");

    let s = Storage::open(&db).expect("fresh database opens");
    assert_eq!(s.schema_version().unwrap(), KNOWN_VERSION);
    drop(s);

    let entries: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec!["fresh.db".to_string()], "only the database");
}

#[test]
fn wal_only_legacy_rows_survive_the_snapshot_path() {
    // A legacy database mid-WAL (an older build killed before its WAL was
    // checkpointed): committed rows live only in the -wal file. The safe
    // path must fold them in (checkpoint) *before* snapshotting, so both
    // the snapshot and the converted archive hold every committed row.
    let dir = scratch_dir();
    let live = dir.path().join("wal-live.db");
    version2_db_with_data(&live, ROWS);
    let recovered = dir.path().join("wal-recovered.db");
    {
        let conn = raw(&live);
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute(
            "INSERT INTO messages (source, external_id, author, sent_at, body)
             VALUES ('mbox', 'wal-only-1', 'ana', '2026-01-01T00:00:00Z',
                     'wal vintage body')",
            [],
        )
        .unwrap();
        // Copy while the connection is open: the main file still lacks
        // the row; the WAL copy carries it (a simulated crash image).
        crate::util::copy_db_and_wal(&live, &recovered);
    } // clean close checkpoints live's own files; the copy is frozen as-is

    let s = Storage::open(&recovered).expect("wal-mid-flight legacy database opens");
    assert_eq!(s.schema_version().unwrap(), KNOWN_VERSION);
    assert_eq!(s.stats().unwrap().total, (ROWS + 1) as u64);
    assert_eq!(
        s.search_filtered("vintage", None, None, 100).unwrap().len(),
        ROWS + 1,
        "the WAL-only row must be archived too"
    );
    drop(s);
    assert!(
        !snapshot_path(&recovered, 2).exists(),
        "validated conversion removes the snapshot"
    );
}

#[test]
fn up_to_date_database_reopens_without_a_snapshot() {
    let dir = scratch_dir();
    let db = dir.path().join("current.db");
    {
        let s = Storage::open(&db).unwrap();
        s.insert_message(&crate::util::msg("integrity", "m1", "current era body"))
            .unwrap();
    } // clean close

    let reopened = Storage::open(&db).expect("reopen applies nothing new");
    assert_eq!(reopened.schema_version().unwrap(), KNOWN_VERSION);
    assert_eq!(reopened.stats().unwrap().total, 1);
    drop(reopened); // clean close removes the WAL/SHM sidecars

    let mut entries: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec!["current.db".to_string()],
        "no snapshot for an up-to-date database"
    );
}

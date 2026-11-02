//! VACUUM / ANALYZE behavior on a large database: compaction must shrink the
//! file, keep every surviving row, keep the FTS index in sync (the delete
//! triggers ran, VACUUM did not disturb the external-content index), and
//! leave triggers working for new writes. ANALYZE must not change results.

use std::fs;
use vault_core::message::InsertOutcome;
use vault_core::storage::Storage;

use crate::util::{file_len, fts_integrity_check, integrity_check, msg, raw, wal_path};

const ROWS: u32 = 3_000;

fn seeded(dir: &std::path::Path) -> std::path::PathBuf {
    let db = dir.join("big.db");
    {
        let s = Storage::open(&db).unwrap();
        for i in 0..ROWS {
            s.insert_message(&msg(
                "integrity",
                &format!("m{i}"),
                &format!("unique{i} body padding padding padding padding padding"),
            ))
            .unwrap();
        }
    }
    db
}

#[test]
fn vacuum_compacts_keeps_data_and_keeps_fts_in_sync() {
    let dir = crate::util::scratch_dir();
    let db = seeded(dir.path());
    let size_before = file_len(&db);
    assert!(
        size_before > 200_000,
        "seeded db must be non-trivially large"
    );

    // Delete half the archive through SQL (the canonical FTS delete
    // trigger fires), then compact and refresh planner statistics.
    {
        let conn = raw(&db);
        conn.execute("DELETE FROM archive_messages WHERE id % 2 = 1", [])
            .unwrap();
        conn.execute("VACUUM", []).unwrap();
        conn.execute("ANALYZE", []).unwrap();
    } // close: WAL checkpoint folds the compacted pages into the main file
    let size_after = file_len(&db);
    assert!(
        size_after < size_before,
        "VACUUM must compact the file (before {size_before}, after {size_after})"
    );
    // The WAL from the vacuum/checkpoint is not left growing forever.
    if let Ok(meta) = fs::metadata(wal_path(&db)) {
        assert!(
            meta.len() <= size_after,
            "post-close WAL must be empty or small"
        );
    }

    // Full verification through the app's own path.
    let s = Storage::open(&db).unwrap();
    let stats = s.stats().unwrap();
    assert_eq!(
        stats.total,
        ROWS as u64 / 2,
        "exactly the surviving half remains"
    );

    assert_eq!(integrity_check(&db).as_deref(), Some("ok"));
    assert!(
        fts_integrity_check(&db).is_ok(),
        "VACUUM must not desync the external-content FTS index"
    );

    // Deleted rows are gone from search; surviving rows all searchable.
    // (Rows are inserted as i = 0..ROWS with id = i+1, so `id % 2 = 1`
    // deletes the even-i tokens and keeps the odd-i ones.)
    assert!(
        s.search_filtered("unique0", None, None, 10)
            .unwrap()
            .is_empty(),
        "even-i rows deleted"
    );
    assert!(
        s.search_filtered("unique998", None, None, 10)
            .unwrap()
            .is_empty(),
        "even-i rows deleted"
    );
    for probe in ["unique1", "unique999", "unique2999"] {
        assert_eq!(
            s.search_filtered(probe, None, None, 10).unwrap().len(),
            1,
            "{probe} must survive"
        );
    }
    let total_hits = s
        .search_filtered("padding", None, None, u32::MAX)
        .unwrap()
        .len();
    assert_eq!(total_hits as u64, stats.total, "search must see every row");

    // Triggers survived VACUUM: a fresh insert lands in table and index.
    let out = s
        .insert_message(&msg(
            "integrity",
            "post-vacuum",
            "unique3000 post vacuum body",
        ))
        .unwrap();
    assert!(matches!(out, InsertOutcome::Inserted(_)));
    assert_eq!(
        s.search_filtered("unique3000", None, None, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn vacuum_without_deletions_preserves_everything() {
    let dir = crate::util::scratch_dir();
    let db = seeded(dir.path());

    {
        let conn = raw(&db);
        conn.execute("VACUUM", []).unwrap();
        conn.execute("ANALYZE", []).unwrap();
        // Planner statistics were gathered, not just tolerated.
        let stats_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_stat1", [], |r| r.get(0))
            .unwrap();
        assert!(stats_rows > 0, "ANALYZE must populate sqlite_stat1");
    }

    let s = Storage::open(&db).unwrap();
    assert_eq!(s.stats().unwrap().total, ROWS as u64);
    assert_eq!(integrity_check(&db).as_deref(), Some("ok"));
    assert!(fts_integrity_check(&db).is_ok());
    assert_eq!(
        s.search_filtered("padding", None, None, u32::MAX)
            .unwrap()
            .len() as u64,
        ROWS as u64
    );
}

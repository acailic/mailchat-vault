//! Corrupted-database scenarios: truncated and partially-written files must
//! be detected — `Storage::open` (or the first query on the opened handle)
//! fails with a clear error, or the file opens with fully consistent data.
//! Silent partial or wrong data is never acceptable.

use std::fs;
use vault_core::storage::Storage;

use crate::util::{
    corrupt_bytes, fts_integrity_check, integrity_check, msg, raw, seeded_file, truncate_file,
    PAGE_SIZE,
};

/// Assert the core contract for a possibly-corrupt file: opening must not
/// panic; it either fails cleanly, or succeeds with the *complete* committed
/// data set and passes both SQLite's and FTS5's consistency checks.
fn assert_detected_or_fully_consistent(db: &std::path::Path, expected_total: u64) {
    match Storage::open(db) {
        Err(e) => {
            // Detection: a clear error surfaced to the caller.
            assert!(
                !e.to_string().is_empty(),
                "corruption must produce a non-empty error message"
            );
        }
        Ok(s) => {
            // Recovery: only acceptable when nothing was silently lost.
            let stats = s
                .stats()
                .unwrap_or_else(|e| panic!("open succeeded but stats failed: {e}"));
            assert_eq!(
                stats.total, expected_total,
                "opened corrupt file must expose all committed rows or fail"
            );
            assert_eq!(
                integrity_check(db).as_deref(),
                Some("ok"),
                "opened corrupt file must pass integrity_check"
            );
            assert!(
                fts_integrity_check(db).is_ok(),
                "opened corrupt file must keep the FTS index in sync"
            );
        }
    }
}

#[test]
fn empty_file_is_a_fresh_valid_database() {
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("empty.db");
    fs::write(&db, b"").unwrap();
    // An empty (or missing) file is the documented "create when missing"
    // path, not corruption.
    let s = Storage::open(&db).unwrap();
    assert_eq!(s.stats().unwrap().total, 0);
    assert_eq!(s.schema_version().unwrap(), 9);
}

#[test]
fn truncated_header_is_rejected() {
    let dir = crate::util::scratch_dir();
    let db = seeded_file(dir.path(), "v.db", 50);
    let full = fs::metadata(&db).unwrap().len();
    assert!(full >= 4 * PAGE_SIZE, "seeded db must span several pages");

    // Cut inside the 100-byte header and just past it: the file cannot be
    // interpreted as a database at all. (A 0-byte file is different — it is
    // a fresh database; see `empty_file_is_a_fresh_valid_database`.)
    for keep in [16u64, 90, 100, 101, PAGE_SIZE / 2] {
        let t = dir.path().join(format!("t{keep}.db"));
        fs::copy(&db, &t).unwrap();
        truncate_file(&t, keep);
        let err = Storage::open(&t)
            .err()
            .unwrap_or_else(|| panic!("truncation to {keep} bytes must be rejected"));
        assert!(
            !err.to_string().is_empty(),
            "truncation to {keep} bytes must give a clear error"
        );
    }
}

#[test]
fn truncated_body_pages_are_detected() {
    let dir = crate::util::scratch_dir();
    let db = seeded_file(dir.path(), "v.db", 50);
    let full = fs::metadata(&db).unwrap().len();

    // Cut whole pages and mid-page at many points between the header and
    // the end. Every cut must be detected or recover without losing rows.
    let mut points: Vec<u64> = vec![];
    for page in 2..(full / PAGE_SIZE) {
        points.push(page * PAGE_SIZE);
        points.push(page * PAGE_SIZE + 123); // ragged, mid-page cut
    }
    points.push(full - PAGE_SIZE / 2); // inside the last page
    for keep in points {
        let t = dir.path().join(format!("t{keep}.db"));
        fs::copy(&db, &t).unwrap();
        truncate_file(&t, keep);
        assert_detected_or_fully_consistent(&t, 50);
    }
}

#[test]
fn garbage_over_live_pages_is_detectable() {
    let dir = crate::util::scratch_dir();
    let db = seeded_file(dir.path(), "v.db", 50);

    // Smash the interior of several early pages (b-tree roots and leaves).
    // SQLite opens lazily, so detection may surface at open, at first query,
    // or via integrity_check — but it must surface somewhere.
    for page in [2u64, 3, 5, 7] {
        let t = dir.path().join(format!("g{page}.db"));
        fs::copy(&db, &t).unwrap();
        corrupt_bytes(&t, page * PAGE_SIZE + 1, 96);
        let detected = Storage::open(&t).is_err()
            || Storage::open(&t)
                .and_then(|s| s.stats().map(|_| ()))
                .is_err()
            || integrity_check(&t).as_deref() != Some("ok");
        assert!(
            detected,
            "garbage over page {page} must be detected (open error, query error, or integrity_check)"
        );
    }
}

#[test]
fn overdeclared_page_count_never_yields_half_state() {
    let dir = crate::util::scratch_dir();
    let db = seeded_file(dir.path(), "v.db", 50);

    // Classic torn write: the header claims one more page than the file
    // holds (page count is a big-endian u32 at offset 28). Opening must
    // either fail or serve the complete committed data set.
    let t = dir.path().join("hdr.db");
    fs::copy(&db, &t).unwrap();
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut f = fs::File::options().write(true).open(&t).unwrap();
        f.seek(SeekFrom::Start(28)).unwrap();
        let n: u32 = (50u64 + 1) as u32; // one page beyond any plausible content
        f.write_all(&n.to_be_bytes()).unwrap();
    }
    // The exact verdict depends on which page went missing: an error, or a
    // fully consistent database. Both acceptable; half-state is not.
    let _ = Storage::open(&t).map(|s| s.stats());
    assert_detected_or_fully_consistent(&t, 50);
}

#[test]
fn fts_index_corruption_is_detected_and_rebuildable() {
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("v.db");
    let n = 40u64;
    {
        let s = Storage::open(&db).unwrap();
        for i in 0..n {
            s.insert_message(&msg(
                "integrity",
                &format!("m{i}"),
                &format!("tok{i} unique body"),
            ))
            .unwrap();
        }
        // Simulate a lost/destroyed index: the table keeps everything, the
        // index keeps nothing.
        s.wipe_fts_for_tests().unwrap();
    }

    // Detection: search goes blind while the table still has all rows —
    // every indexed term stops matching, so the loss is observable through
    // the app's own read path, not just by inspecting the file.
    let s = Storage::open(&db).unwrap();
    assert_eq!(s.stats().unwrap().total, n);
    for i in 0..n {
        assert!(
            s.search_filtered(&format!("tok{i}"), None, None, 1)
                .unwrap()
                .is_empty(),
            "wiped index must not return hits"
        );
    }

    // Recovery path: rebuild restores the index exactly.
    s.rebuild_fts().unwrap();
    assert_eq!(s.search_filtered("tok17", None, None, 10).unwrap().len(), 1);
    for i in 0..n {
        assert_eq!(
            s.search_filtered(&format!("tok{i}"), None, None, 1)
                .unwrap()
                .len(),
            1,
            "every row must be searchable after rebuild"
        );
    }
    assert!(fts_integrity_check(&db).is_ok());
    // And rows themselves were never touched.
    assert_eq!(s.stats().unwrap().total, n);
    let raw_conn = raw(&db);
    let bodies: usize = raw_conn
        .query_row(
            "SELECT COUNT(*) FROM archive_messages WHERE body LIKE 'tok%'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap() as usize;
    assert_eq!(bodies, n as usize);
}

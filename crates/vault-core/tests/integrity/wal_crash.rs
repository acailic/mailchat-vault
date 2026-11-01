//! WAL crash scenarios. A killed process leaves committed transactions in
//! the `-wal` file, un-checkpointed. The crash state is simulated by copying
//! `db` + `wal` (never `-shm`, which SQLite rebuilds) while the writing
//! connection is still open — byte-for-byte what a `kill -9` leaves behind.

use std::fs;
use vault_core::message::InsertOutcome;
use vault_core::storage::Storage;

use crate::util::{
    copy_db_and_wal, file_len, fts_integrity_check, integrity_check, msg, truncate_file, wal_path,
    PAGE_SIZE,
};

#[test]
fn committed_transactions_survive_crash_before_checkpoint() {
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("v.db");

    // Three separate transactions, all still only in the WAL.
    let s = Storage::open(&db).unwrap();
    for id in ["a", "b", "c"] {
        let out = s
            .insert_message(&msg("integrity", id, &format!("body of {id} survives")))
            .unwrap();
        assert!(matches!(out, InsertOutcome::Inserted(_)));
    }
    assert!(wal_path(&db).exists(), "commits must be sitting in the WAL");

    // Kill -9: snapshot the on-disk state, writer never gets to close.
    let copy = dir.path().join("crash.db");
    copy_db_and_wal(&db, &copy);
    drop(s);

    // Reopen: WAL recovery replays every committed frame.
    let s = Storage::open(&copy).unwrap();
    assert_eq!(s.stats().unwrap().total, 3);
    for id in ["a", "b", "c"] {
        assert_eq!(s.search(id, 10).unwrap().len(), 1, "row {id} searchable");
    }
    assert_eq!(integrity_check(&copy).as_deref(), Some("ok"));
    assert!(fts_integrity_check(&copy).is_ok());
}

#[test]
fn torn_wal_tail_discards_only_the_incomplete_transaction() {
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("v.db");

    // Two small committed transactions, then one large multi-page
    // transaction (a ~200 KB body spans dozens of WAL frames).
    let s = Storage::open(&db).unwrap();
    s.insert_message(&msg("integrity", "small-1", "first committed body"))
        .unwrap();
    s.insert_message(&msg("integrity", "small-2", "second committed body"))
        .unwrap();
    let big_body = "x".repeat(200_000);
    s.insert_message(&msg("integrity", "big", &big_body))
        .unwrap();

    let wal = wal_path(&db);
    let wal_len = file_len(&wal);
    // A WAL frame is 24 bytes of header + one page; cutting two frames plus
    // slack guarantees the big transaction's commit frame is gone while the
    // small transactions' frames (written long before the tail) stay valid.
    let frame = PAGE_SIZE + 24;
    let cut = 2 * frame + 100;
    assert!(
        wal_len > cut + 10 * frame,
        "the big transaction must span many frames for a deterministic tear"
    );

    let copy = dir.path().join("torn.db");
    copy_db_and_wal(&db, &copy);
    drop(s);
    truncate_file(&wal_path(&copy), wal_len - cut);

    // Reopen the torn state: complete transactions are replayed, the torn
    // one is discarded whole — never applied halfway.
    let s = Storage::open(&copy).unwrap();
    let stats = s.stats().unwrap();
    assert_eq!(
        stats.total, 2,
        "only the two fully committed transactions survive"
    );
    let hits = s.search("committed", 10).unwrap();
    let mut ids: Vec<&str> = hits.iter().map(|m| m.external_id.as_str()).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["small-1", "small-2"]);
    assert!(
        s.search("xxxxx", 10).unwrap().is_empty(),
        "the torn transaction must leave no searchable residue"
    );
    assert_eq!(integrity_check(&copy).as_deref(), Some("ok"));
    assert!(
        fts_integrity_check(&copy).is_ok(),
        "recovery must not leave the FTS index half-updated"
    );
}

#[test]
fn losing_the_wal_file_loses_recency_but_never_corrupts() {
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("v.db");

    // Commits that were never checkpointed into the main file.
    let s = Storage::open(&db).unwrap();
    s.insert_message(&msg("integrity", "gone", "only ever in the wal"))
        .unwrap();

    // Copy the main file alone: the WAL is destroyed (worst-case crash
    // where the WAL volume is lost). With synchronous=NORMAL this is
    // documented as losing the most recent commits — but the database
    // itself must stay structurally sound.
    let copy = dir.path().join("nowal.db");
    fs::copy(&db, &copy).unwrap();
    drop(s);

    let s = Storage::open(&copy).unwrap();
    assert_eq!(
        s.stats().unwrap().total,
        0,
        "un-checkpointed commits are gone"
    );
    assert_eq!(integrity_check(&copy).as_deref(), Some("ok"));
    // And the database remains fully usable afterwards.
    let out = s
        .insert_message(&msg("integrity", "fresh", "written after the loss"))
        .unwrap();
    assert!(matches!(out, InsertOutcome::Inserted(_)));
    assert_eq!(s.search("after", 10).unwrap().len(), 1);
}

#[test]
fn clean_close_checkpoints_and_does_not_need_the_wal() {
    let dir = crate::util::scratch_dir();
    let db = dir.path().join("v.db");
    {
        let s = Storage::open(&db).unwrap();
        for id in ["a", "b", "c", "d"] {
            s.insert_message(&msg("integrity", id, "checkpointed body"))
                .unwrap();
        }
    } // clean close: WAL is checkpointed back into the main file

    // Even if the (now stale or removed) WAL vanishes, everything is there.
    let stale = wal_path(&db);
    if stale.exists() {
        fs::remove_file(&stale).unwrap();
    }
    let s = Storage::open(&db).unwrap();
    assert_eq!(s.stats().unwrap().total, 4);
    assert_eq!(integrity_check(&db).as_deref(), Some("ok"));
}

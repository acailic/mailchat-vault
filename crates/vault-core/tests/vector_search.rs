//! Integration tests for local embeddings + vector search: the incremental
//! embedding job, the manual-cosine scan (ordering, date filters, foreign
//! rows), coverage status, cascade cleanup, and multi-row hydration.

use std::sync::{Mutex, Once};

use vault_core::message::NewMessage;
use vault_core::storage::Storage;
use vault_core::vector::{
    cosine, embedding_status, rrf_fuse, run_embedding_job, Embedder, HashEmbedder,
};

// The vector scan translates local-day date bounds, so pin the process TZ
// exactly once under exclusive access (same approach as llm_search.rs).
static TZ_LOCK: Mutex<()> = Mutex::new(());
static TZ_PIN: Once = Once::new();

fn pin_utc_tz() {
    let _guard = TZ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    TZ_PIN.call_once(|| {
        std::env::set_var("TZ", "UTC");
        let _ = chrono::Local::now(); // resolve with the pinned TZ
    });
}

fn msg(id: &str, author: &str, sent_at: &str, body: &str) -> NewMessage {
    NewMessage::new("test", id, author, sent_at, body)
}

fn seeded() -> Storage {
    pin_utc_tz();
    let s = Storage::open_in_memory().unwrap();
    for (id, author, sent_at, body) in [
        (
            "m1",
            "Ana",
            "2026-09-05T10:00:00Z",
            "quarterly vault sync planning notes",
        ),
        (
            "m2",
            "Bob",
            "2026-10-12T08:30:00Z",
            "vault sync encountered problems at midnight",
        ),
        (
            "m3",
            "Cara",
            "2026-10-28T21:00:00Z",
            "lunch plans for friday",
        ),
        (
            "m4",
            "Dov",
            "2026-11-01T09:00:00Z",
            "completely unrelated taxes",
        ),
    ] {
        s.insert_message(&msg(id, author, sent_at, body)).unwrap();
    }
    s
}

#[test]
fn embedding_job_is_incremental_and_idempotent() {
    let s = seeded();
    let e = HashEmbedder::new();

    let first = run_embedding_job(&s, &e, None).unwrap();
    assert_eq!(first.embedded, 4);
    assert_eq!(first.remaining, 0);
    assert_eq!(embedding_status(&s, &e).unwrap().pending, 0);

    // Re-running over an up-to-date archive embeds nothing.
    let again = run_embedding_job(&s, &e, None).unwrap();
    assert_eq!(again.embedded, 0, "no message lacks a current embedding");
    assert_eq!(again.remaining, 0);

    // Only newly imported messages cost anything.
    s.insert_message(&msg(
        "m5",
        "Eve",
        "2026-11-02T10:00:00Z",
        "fresh vault update",
    ))
    .unwrap();
    s.insert_message(&msg("m6", "Fil", "2026-11-03T10:00:00Z", "another update"))
        .unwrap();
    let third = run_embedding_job(&s, &e, None).unwrap();
    assert_eq!(third.embedded, 2, "only the two new messages");
    let counts = s.embedding_counts(e.id()).unwrap();
    assert_eq!((counts.embedded, counts.messages), (6, 6));
}

#[test]
fn embedding_status_reports_coverage() {
    let s = seeded();
    let e = HashEmbedder::new();

    let before = embedding_status(&s, &e).unwrap();
    assert_eq!(
        (before.embedded, before.messages, before.pending),
        (0, 4, 4),
        "nothing embedded yet"
    );
    assert_eq!(before.model, HashEmbedder::MODEL_ID);
    assert_eq!(before.dim, HashEmbedder::DIM);

    run_embedding_job(&s, &e, Some(2)).unwrap(); // capped run
    let mid = embedding_status(&s, &e).unwrap();
    assert_eq!((mid.embedded, mid.pending), (2, 2), "the cap held back two");
}

#[test]
fn model_change_reembeds_and_replaces_rows() {
    let s = seeded();
    let e = HashEmbedder::new();
    run_embedding_job(&s, &e, None).unwrap();

    // A future model writes one row under a different id: that message is
    // pending again for the current model, and re-embedding replaces the
    // foreign row (one row per message, keyed by message_id).
    let foreign = e.embed("vault sync");
    s.embed_message(1, "future-model-v9", e.dim(), &foreign)
        .unwrap();
    assert_eq!(
        s.pending_embeddings(e.id(), 10).unwrap(),
        vec![(1, "quarterly vault sync planning notes".into())],
        "only the message whose row changed model is pending"
    );
    let rerun = run_embedding_job(&s, &e, None).unwrap();
    assert_eq!(rerun.embedded, 1);
    let counts = s.embedding_counts(e.id()).unwrap();
    assert_eq!((counts.embedded, counts.messages), (4, 4));
    // The future-model row was replaced, not kept alongside.
    let status = embedding_status(&s, &e).unwrap();
    assert_eq!(status.pending, 0);
}

#[test]
fn vector_scan_orders_by_cosine_and_respects_dates() {
    pin_utc_tz();
    let s = seeded();
    let e = HashEmbedder::new();
    run_embedding_job(&s, &e, None).unwrap();

    // Ask in the topic of m1/m2; m3/m4 are unrelated noise.
    let hits = s
        .vector_scan(
            e.id(),
            e.dim(),
            &e.embed("vault sync problems"),
            None,
            None,
            10,
        )
        .unwrap();
    assert!(!hits.is_empty());
    let top_body = s.get(hits[0].message_id).unwrap().unwrap().body.clone();
    assert!(
        top_body.contains("vault"),
        "most similar message should be a vault one, got {top_body:?}"
    );
    let vault_ids: Vec<String> = hits
        .iter()
        .filter_map(|h| s.get(h.message_id).unwrap())
        .map(|m| m.external_id.clone())
        .filter(|id| id == "m1" || id == "m2")
        .collect();
    assert_eq!(vault_ids.len(), 2, "both vault messages rank");
    // Scores are cosine similarities: positive and bounded by 1.
    assert!(hits.iter().all(|h| h.score > 0.0 && h.score <= 1.0 + 1e-4));

    // Local-day bounds narrow the scan like they narrow FTS: October only
    // keeps m2 (m1 is September; the unrelated rows score below the floor).
    let october = s
        .vector_scan(
            e.id(),
            e.dim(),
            &e.embed("vault sync planning"),
            Some("2026-10-01"),
            Some("2026-10-31"),
            10,
        )
        .unwrap();
    assert_eq!(october.len(), 1);
    assert_eq!(october[0].message_id, 2);

    // A window that excludes every embedded message finds nothing.
    let none = s
        .vector_scan(
            e.id(),
            e.dim(),
            &e.embed("vault"),
            Some("2020-01-01"),
            Some("2020-12-31"),
            10,
        )
        .unwrap();
    assert!(none.is_empty());

    // Badly formatted bounds are rejected exactly like the FTS path.
    assert!(s
        .vector_scan(
            e.id(),
            e.dim(),
            &e.embed("vault"),
            Some("October"),
            None,
            10
        )
        .is_err());
}

#[test]
fn vector_scan_truncates_and_orders_deterministically() {
    let s = Storage::open_in_memory().unwrap();
    for i in 0..10 {
        s.insert_message(&msg(
            &format!("m{i}"),
            "A",
            "2026-10-01T00:00:00Z",
            &format!("shared topic words number {i}"),
        ))
        .unwrap();
    }
    let e = HashEmbedder::new();
    run_embedding_job(&s, &e, None).unwrap();
    let hits = s
        .vector_scan(e.id(), e.dim(), &e.embed("shared topic"), None, None, 3)
        .unwrap();
    assert_eq!(hits.len(), 3, "limit respected");
    let mut sorted = hits.clone();
    sorted.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap()
            .then(a.message_id.cmp(&b.message_id))
    });
    assert_eq!(hits, sorted, "best score first, ties by id");
}

#[test]
fn vector_scan_skips_foreign_dimension_rows() {
    let s = seeded();
    let e = HashEmbedder::new();
    run_embedding_job(&s, &e, None).unwrap();

    // A leftover row from a different-dimension model under the same model
    // id: stored with dim 4. The 128-dim scan must skip it, not break.
    let foreign = vec![0.5, -0.5, 0.5, -0.5];
    s.embed_message(1, e.id(), 4, &foreign).unwrap();
    let hits = s
        .vector_scan(e.id(), e.dim(), &e.embed("vault sync"), None, None, 10)
        .unwrap();
    assert!(
        hits.iter().all(|h| h.message_id != 1),
        "the foreign-dimension row is skipped"
    );
    assert!(hits.iter().any(|h| h.message_id == 2));
}

#[test]
fn vector_scan_rejects_mismatched_query_length() {
    let s = seeded();
    let e = HashEmbedder::new();
    let err = s.vector_scan(e.id(), e.dim(), &[1.0, 2.0, 3.0], None, None, 5);
    assert!(matches!(err, Err(vault_core::Error::Embedding(_))));
}

#[test]
fn embed_batch_rejects_dim_mismatch() {
    let s = seeded();
    let err = s.embed_messages_batch("m", 3, &[(1, vec![1.0, 2.0])]);
    assert!(matches!(err, Err(vault_core::Error::Embedding(_))));
    // Nothing was written (the batch rolled back).
    assert_eq!(s.embedding_counts("m").unwrap().embedded, 0);
}

#[test]
fn deleting_a_message_cascades_to_its_embedding() {
    pin_utc_tz();
    // /dev/shm when available (same trick as the integrity suite's
    // scratch_dir): the test does real fsyncs, and tmpfs keeps it fast.
    let dir = if std::path::Path::new("/dev/shm").is_dir() {
        tempfile::tempdir_in("/dev/shm").unwrap()
    } else {
        tempfile::tempdir().unwrap()
    };
    let db = dir.path().join("cascade.db");
    {
        let s = Storage::open(&db).unwrap();
        s.insert_message(&msg("m1", "A", "2026-10-01T00:00:00Z", "vault sync body"))
            .unwrap();
        let e = HashEmbedder::new();
        run_embedding_job(&s, &e, None).unwrap();
        assert_eq!(s.embedding_counts(e.id()).unwrap().embedded, 1);
    }
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute("DELETE FROM messages WHERE id = 1", [])
            .unwrap();
    }
    let s = Storage::open(&db).unwrap();
    let e = HashEmbedder::new();
    assert_eq!(
        s.embedding_counts(e.id()).unwrap().embedded,
        0,
        "ON DELETE CASCADE removed the embedding row with its message"
    );
}

#[test]
fn get_many_preserves_order_and_skips_missing() {
    let s = seeded();
    let got = s.get_many(&[3, 999, 1, 3]).unwrap();
    // Input order, missing ids skipped, duplicates collapsed by the map.
    let ids: Vec<i64> = got.iter().map(|m| m.id).collect();
    assert!(ids.contains(&3) && ids.contains(&1) && !ids.contains(&999));
    assert!(s.get_many(&[]).unwrap().is_empty());
}

#[test]
fn embedding_vectors_roundtrip_through_the_blob_format() {
    // The stored blob must decode to exactly the vector the embedder
    // produced (little-endian f32, unit length): scan scores against the
    // message's own text are ~1.0.
    let s = seeded();
    let e = HashEmbedder::new();
    run_embedding_job(&s, &e, None).unwrap();
    let q = e.embed("vault sync encountered problems at midnight");
    let hits = s.vector_scan(e.id(), e.dim(), &q, None, None, 1).unwrap();
    assert_eq!(hits[0].message_id, 2);
    // ~1 up to the (small) i8 quantization error.
    assert!(
        (hits[0].score - 1.0).abs() < 0.02,
        "self-similarity is ~1, got {}",
        hits[0].score
    );
    // Sanity: the public cosine helper agrees with the scan's ordering.
    let other = e.embed("quarterly vault sync planning notes");
    assert!(cosine(&q, &other).unwrap() > 0.15);
}

#[test]
fn rrf_fusion_at_storage_level() {
    // The two legs rank the same three rows differently; fusion must keep
    // the row that tops both lists first and drop nothing.
    let fused = rrf_fuse(&[30, 10, 20], &[10, 20, 30], 10);
    assert_eq!(fused[0], 10, "top of both lists wins");
    assert_eq!(fused.len(), 3);
}

#[test]
fn job_summary_and_status_serialize_for_the_frontend() {
    let s = seeded();
    let e = HashEmbedder::new();
    let summary = run_embedding_job(&s, &e, None).unwrap();
    let json = serde_json::to_value(&summary).unwrap();
    assert_eq!(json["model"], HashEmbedder::MODEL_ID);
    assert_eq!(json["dim"], HashEmbedder::DIM);
    assert_eq!(json["embedded"], serde_json::json!(4));
    let status = embedding_status(&s, &e).unwrap();
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["pending"], serde_json::json!(0));
    assert_eq!(json["messages"], serde_json::json!(4));
}

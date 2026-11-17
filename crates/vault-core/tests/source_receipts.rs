//! Durable source receipts (migration 22) — the committed-facts read
//! model for completed imports and syncs.
//!
//! The job ledger (migration 16) keeps only the newest terminal jobs, so
//! long-lived source health cannot rest on it. These scenarios prove the
//! receipt facts the plan's §6 `source_receipts` row promises:
//!
//! - a recorded receipt survives close/reopen with every count intact
//!   (receipts are rows, not memory — partial failures already committed
//!   survive process restart);
//! - a completed import after a completed sync yields two distinct
//!   receipts, newest first, with the observed date range exposed as
//!   separate nullable bounds and no field that could claim full
//!   provider coverage;
//! - an interrupted multi-batch import reports run totals (counts
//!   accumulated through the checkpoint, failures and observed range
//!   read from committed rows), so a resumed run reports the same facts
//!   a clean run would.

use vault_core::message::NewMessage;
use vault_core::storage::{
    ImportIssueRecord, NewSourceInstance, SourceReceiptKind, SourceReceiptRecord, Storage,
};

/// One plain message; media carried via `raw_json` where a scenario
/// needs file counts.
fn msg(source: &str, external_id: &str, sent_at: &str, body: &str) -> NewMessage {
    NewMessage::new(source, external_id, "Ana <ana@example.com>", sent_at, body)
}

fn issue(position: u64, external_ref: &str) -> ImportIssueRecord<'_> {
    ImportIssueRecord {
        position,
        external_ref,
        reason: "unparseable entry",
    }
}

/// The canonical source-instance id a legacy import under `namespace`
/// resolves to (importing first makes the instance exist; the label
/// defaults to the namespace).
fn instance_for(storage: &Storage, namespace: &str) -> String {
    storage
        .list_source_instances()
        .unwrap()
        .into_iter()
        .find(|i| i.kind == "legacy" && i.label == namespace)
        .unwrap_or_else(|| panic!("no source instance for namespace {namespace}"))
        .id
}

/// The receipt keys the serialized row exposes — deliberately nothing
/// that could express "full provider coverage" between the observed
/// bounds.
const RECEIPT_KEYS: &[&str] = &[
    "id",
    "source_instance_id",
    "kind",
    "completed_at",
    "observed_min",
    "observed_max",
    "new_records",
    "present_records",
    "failed_records",
    "files_preserved",
    "files_missing",
    "facts_json",
];

#[test]
fn receipts_survive_close_and_reopen_with_counts_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("vault.db");
    let instance;
    let before;
    {
        let storage = Storage::open(&db).unwrap();
        let mut with_media = msg("mbox:exports", "m2", "2026-11-02T10:00:00Z", "media mail");
        with_media.raw_json = r#"{"media": [
            {"name": "photo.jpg", "kind": "image", "sha256": "a1b2c3"},
            {"name": "clip.mp4", "kind": "video", "missing": true, "reason": "not-in-export"}
        ]}"#
        .to_string();
        storage
            .commit_import_batch(
                "imp-reopen",
                "mbox:exports",
                &[
                    msg("mbox:exports", "m1", "2026-11-01T09:00:00Z", "plain"),
                    with_media,
                ],
                &[issue(2, "broken-entry")],
                &[],
                3,
                true,
            )
            .unwrap();
        instance = instance_for(&storage, "mbox:exports");
        let receipts = storage.list_source_receipts(&instance, 10).unwrap();
        assert_eq!(receipts.len(), 1, "one completed import, one receipt");
        before = receipts.into_iter().next().unwrap();
        assert_eq!(before.kind, SourceReceiptKind::Import);
        assert_eq!(before.new_records, 2);
        assert_eq!(before.present_records, 0);
        assert_eq!(before.failed_records, 1);
        assert_eq!(before.files_preserved, 1);
        assert_eq!(before.files_missing, 1);
        assert_eq!(before.observed_min.as_deref(), Some("2026-11-01T09:00:00Z"));
        assert_eq!(before.observed_max.as_deref(), Some("2026-11-02T10:00:00Z"));
        assert!(
            before.facts_json.contains("imp-reopen"),
            "the receipt names its import: {}",
            before.facts_json
        );
        assert!(
            !before.facts_json.contains("media mail"),
            "no message bodies"
        );
    }

    // Close and reopen the vault: the receipt is a committed row, so it
    // lists again with every count intact.
    let storage = Storage::open(&db).unwrap();
    let after = storage.list_source_receipts(&instance, 10).unwrap();
    assert_eq!(after, vec![before], "receipt survives reopen unchanged");
}

#[test]
fn completed_import_after_completed_sync_yields_two_distinct_receipts_newest_first() {
    let storage = Storage::open_in_memory().unwrap();

    // A completed sync through the real seam (`record_source_success`)
    // leaves a kind-'sync' receipt with honest zeros: this seam does not
    // thread run counters, and none are invented.
    let imap_instance = storage
        .ensure_source_instance(&NewSourceInstance {
            kind: "imap",
            label: "Ana's mail",
            connector_key: "imap|imap.a.test|993|implicit-tls|ana@a.test",
            config_json: "{}",
        })
        .unwrap();
    storage.record_source_success(&imap_instance).unwrap();
    let sync_receipts = storage.list_source_receipts(&imap_instance, 10).unwrap();
    assert_eq!(sync_receipts.len(), 1);
    let sync = sync_receipts[0].clone();
    assert_eq!(sync.kind, SourceReceiptKind::Sync);
    assert_eq!(
        (sync.new_records, sync.present_records, sync.failed_records),
        (0, 0, 0),
        "the sync seam threads no counters; zeros are facts, not silence"
    );
    assert_eq!((sync.observed_min, sync.observed_max), (None, None));

    // The same source later receives a completed import (the public
    // record API is the seam future import connectors use; the bounded
    // import path writes its receipt inside commit_import_batch).
    storage
        .record_source_receipt(&SourceReceiptRecord {
            source_instance_id: &imap_instance,
            kind: SourceReceiptKind::Import,
            observed_min: Some("2026-10-01T00:00:00Z"),
            observed_max: Some("2026-10-31T23:59:59Z"),
            new_records: 5,
            present_records: 2,
            failed_records: 1,
            files_preserved: 3,
            files_missing: 1,
            facts_json: r#"{"import_id": "imp-adjacent"}"#,
        })
        .unwrap();

    let receipts = storage.list_source_receipts(&imap_instance, 10).unwrap();
    assert_eq!(receipts.len(), 2, "sync then import, two receipts");
    assert_eq!(
        receipts[0].kind,
        SourceReceiptKind::Import,
        "newest first: the import completed after the sync"
    );
    assert_eq!(receipts[1].kind, SourceReceiptKind::Sync);
    assert!(receipts[0].id > receipts[1].id);

    // Neither receipt claims full provider coverage: the observed range
    // is a pair of separate nullable bounds, and the serialized row has
    // no field that could express completeness between them.
    for receipt in &receipts {
        let value = serde_json::to_value(receipt).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort_unstable();
        let mut expected = RECEIPT_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected, "no coverage/completeness field exists");
    }

    // Receipts are per source instance: another instance's import lists
    // nothing under this one.
    storage
        .commit_import_batch(
            "imp-elsewhere",
            "telegram",
            &[msg("telegram", "t1", "2026-11-03T08:00:00Z", "zdravo")],
            &[],
            &[],
            1,
            true,
        )
        .unwrap();
    assert_eq!(
        storage
            .list_source_receipts(&imap_instance, 10)
            .unwrap()
            .len(),
        2,
        "another source's import adds no receipt here"
    );
    assert_eq!(
        storage
            .list_source_receipts(&instance_for(&storage, "telegram"), 10)
            .unwrap()
            .len(),
        1,
        "and exactly one under its own instance"
    );
}

#[test]
fn interrupted_import_reports_run_totals_after_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("vault.db");

    // Batch one commits (2 new, 1 failed) and the process "crashes": the
    // checkpoint row carries the accumulated counts.
    {
        let storage = Storage::open(&db).unwrap();
        storage
            .commit_import_batch(
                "imp-resume",
                "signal",
                &[
                    msg("signal", "s1", "2026-10-05T10:00:00Z", "first"),
                    msg("signal", "s2", "2026-10-07T10:00:00Z", "second"),
                ],
                &[issue(2, "mid-file-break")],
                &[],
                3,
                false,
            )
            .unwrap();
        assert_eq!(storage.import_checkpoint("imp-resume").unwrap(), Some(3));
    }

    // Reopen (restart), then the resumed run's final batch: one more new
    // message and one duplicate of batch one.
    let storage = Storage::open(&db).unwrap();
    storage
        .commit_import_batch(
            "imp-resume",
            "signal",
            &[
                msg("signal", "s1", "2026-10-05T10:00:00Z", "first"),
                msg("signal", "s3", "2026-10-09T10:00:00Z", "third"),
            ],
            &[],
            &[],
            5,
            true,
        )
        .unwrap();
    assert_eq!(storage.import_checkpoint("imp-resume").unwrap(), None);

    // The receipt reports the whole run — including the parts committed
    // before the restart — because every count comes from committed
    // rows, not memory.
    let receipts = storage
        .list_source_receipts(&instance_for(&storage, "signal"), 10)
        .unwrap();
    assert_eq!(receipts.len(), 1);
    let receipt = &receipts[0];
    assert_eq!(receipt.kind, SourceReceiptKind::Import);
    assert_eq!(receipt.new_records, 3, "two before the restart, one after");
    assert_eq!(receipt.present_records, 1, "the resumed duplicate");
    assert_eq!(receipt.failed_records, 1, "the pre-restart parse failure");
    assert_eq!(
        receipt.observed_min.as_deref(),
        Some("2026-10-05T10:00:00Z")
    );
    assert_eq!(
        receipt.observed_max.as_deref(),
        Some("2026-10-09T10:00:00Z")
    );
}

#[test]
fn completing_commit_returns_the_receipt_it_wrote() {
    // A2b: the completing batch's counts carry the receipt the same
    // transaction wrote, so an importer's outcome can show kept work
    // without re-reading storage — and what they carry is exactly the
    // row the listing serves. Intermediate batches carry no receipt.
    let storage = Storage::open_in_memory().unwrap();
    let mut with_media = msg("mbox:exports", "m2", "2026-11-02T10:00:00Z", "media mail");
    with_media.raw_json = r#"{"media": [
        {"name": "photo.jpg", "kind": "image", "sha256": "a1b2c3"},
        {"name": "clip.mp4", "kind": "video", "missing": true, "reason": "not-in-export"}
    ]}"#
    .to_string();

    // Intermediate batch: one message, no receipt yet.
    let mid = storage
        .commit_import_batch(
            "imp-carry",
            "mbox:exports",
            &[msg("mbox:exports", "m1", "2026-11-01T09:00:00Z", "plain")],
            &[],
            &[],
            1,
            false,
        )
        .unwrap();
    assert!(mid.receipt.is_none(), "an intermediate batch completes nothing");

    // Completing batch: the duplicate of m1 (present), one new message
    // carrying a preserved and a missing file, and one failing record.
    let counts = storage
        .commit_import_batch(
            "imp-carry",
            "mbox:exports",
            &[
                msg("mbox:exports", "m1", "2026-11-01T09:00:00Z", "plain"),
                with_media,
            ],
            &[issue(3, "broken-entry")],
            &[],
            4,
            true,
        )
        .unwrap();
    let receipt = counts
        .receipt
        .expect("the completing commit carries its receipt");
    assert_eq!(receipt.kind, SourceReceiptKind::Import);
    assert_eq!(receipt.new_records, 2);
    assert_eq!(receipt.present_records, 1, "the resumed duplicate");
    assert!(receipt.failed_records > 0, "the failing record is counted");
    assert_eq!(receipt.failed_records, 1);
    assert_eq!(receipt.files_preserved, 1);
    assert_eq!(receipt.files_missing, 1);
    assert_eq!(receipt.observed_min.as_deref(), Some("2026-11-01T09:00:00Z"));
    assert_eq!(receipt.observed_max.as_deref(), Some("2026-11-02T10:00:00Z"));
    assert!(
        receipt.facts_json.contains("imp-carry"),
        "the receipt names its import: {}",
        receipt.facts_json
    );

    // What the caller received is the stored row, verbatim: the listing
    // serves the same receipt (id and completed_at included).
    let instance = instance_for(&storage, "mbox:exports");
    assert_eq!(
        storage.list_source_receipts(&instance, 10).unwrap(),
        vec![receipt]
    );
}

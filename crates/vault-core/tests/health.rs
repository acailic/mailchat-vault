//! Persisted archive-health evidence: the media verification summary
//! and the per-import-source last-import records (see
//! `vault_core::health`). Absence is an explicit not-checked answer; a
//! failed or cancelled attempt never overwrites the recorded last
//! success.

use vault_core::bounded::BoundedOutcome;
use vault_core::health::{
    import_source_records, media_verification, record_import_attempt, record_media_verification,
    ImportAttempt, ImportCounts, MediaVerificationSummary, SourceImportRecord, IMPORTS_KEY,
    MEDIA_VERIFICATION_KEY,
};
use vault_core::ingest::IngestSummary;
use vault_core::object_store::ReconcileReport;
use vault_core::storage::Storage;

fn storage() -> Storage {
    Storage::open_in_memory().expect("open in-memory storage")
}

/// A finished reconciliation report: referenced objects, nothing
/// damaged.
fn clean_report() -> ReconcileReport {
    ReconcileReport {
        objects: 3,
        referenced: 3,
        ..ReconcileReport::default()
    }
}

fn damaged_report() -> ReconcileReport {
    ReconcileReport {
        objects: 5,
        referenced: 4,
        missing: vec!["aa..ff".into()],
        corrupt: vec!["bb..ff (hashes to cc..ff)".into()],
        orphans: vec!["dd..ff.jpg".into()],
        ..ReconcileReport::default()
    }
}

#[test]
fn media_verification_summary_round_trips_and_overwrites() {
    let s = storage();

    // Never reconciled: the explicit not-checked answer, not an error.
    assert!(media_verification(&s).unwrap().is_none());

    let first = MediaVerificationSummary::of_report(&clean_report(), "2026-11-01T10:00:00Z".into());
    record_media_verification(&s, &first).unwrap();
    assert_eq!(media_verification(&s).unwrap(), Some(first.clone()));

    // A newer verification overwrites: one last-verification, not a
    // history.
    let second =
        MediaVerificationSummary::of_report(&damaged_report(), "2026-11-02T10:00:00Z".into());
    record_media_verification(&s, &second).unwrap();
    assert_eq!(media_verification(&s).unwrap(), Some(second));

    // A malformed stored record must not take the panel down: it reads
    // as not checked, and the next finished verification overwrites it.
    s.set_archive_meta(MEDIA_VERIFICATION_KEY, "not json")
        .unwrap();
    assert!(media_verification(&s).unwrap().is_none());
    record_media_verification(&s, &first).unwrap();
    assert_eq!(media_verification(&s).unwrap(), Some(first));
}

#[test]
fn media_summary_tallies_the_report_and_knows_when_it_was_clean() {
    let clean = MediaVerificationSummary::of_report(&clean_report(), "2026-11-01T10:00:00Z".into());
    assert_eq!(clean.checked_at, "2026-11-01T10:00:00Z");
    assert_eq!(clean.objects, 3);
    assert_eq!(clean.referenced, 3);
    assert_eq!(clean.missing, 0);
    assert!(clean.is_clean());

    // Missing references (external damage) stay distinct from counts a
    // healthy pass reports.
    let damaged =
        MediaVerificationSummary::of_report(&damaged_report(), "2026-11-02T10:00:00Z".into());
    assert_eq!(damaged.missing, 1);
    assert_eq!(damaged.corrupt, 1);
    assert_eq!(damaged.orphans, 1);
    assert_eq!(damaged.referenced, 4);
    assert_eq!(damaged.objects, 5);
    assert!(!damaged.is_clean());
}

/// The counts side of [`ImportCounts::of_summary`] against a real
/// bounded outcome's summary shape.
fn outcome_summary(source: &str, inserted: u64, skipped: u64, failed: u64) -> BoundedOutcome {
    BoundedOutcome {
        summary: IngestSummary {
            source: source.to_string(),
            seen: inserted + skipped + failed,
            inserted,
            skipped,
            failed,
        },
        media: None,
        import_id: "import-1".into(),
        cancelled: false,
        resumed_from: 0,
        committed_items: inserted + skipped,
        receipt: None,
    }
}

#[test]
fn import_evidence_updates_when_an_import_finishes() {
    let s = storage();

    // No import ever finished: every source reads as never imported.
    assert!(import_source_records(&s).unwrap().is_empty());

    // A first import finishes: the source's evidence appears, attempt
    // and success recorded together.
    let outcome = outcome_summary("Telegram export", 10, 2, 0);
    record_import_attempt(
        &s,
        &outcome.summary.source,
        "2026-11-01T09:00:00Z",
        &ImportAttempt::Succeeded {
            counts: ImportCounts::of_summary(&outcome.summary),
        },
    )
    .unwrap();
    let records = import_source_records(&s).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0],
        SourceImportRecord {
            source: "Telegram export".into(),
            last_attempt_at: "2026-11-01T09:00:00Z".into(),
            last_attempt: ImportAttempt::Succeeded {
                counts: ImportCounts {
                    seen: 12,
                    inserted: 10,
                    skipped: 2,
                    failed: 0
                }
            },
            last_success: Some(vault_core::health::ImportSuccess {
                at: "2026-11-01T09:00:00Z".into(),
                counts: ImportCounts {
                    seen: 12,
                    inserted: 10,
                    skipped: 2,
                    failed: 0
                }
            }),
        }
    );

    // A second source imports later: per-source records, sorted by key.
    let other = outcome_summary("WhatsApp with Ana", 4, 0, 0);
    record_import_attempt(
        &s,
        &other.summary.source,
        "2026-11-03T09:00:00Z",
        &ImportAttempt::Succeeded {
            counts: ImportCounts::of_summary(&other.summary),
        },
    )
    .unwrap();
    let records = import_source_records(&s).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].source, "Telegram export");
    assert_eq!(records[1].source, "WhatsApp with Ana");

    // A later import on the first source replaces only that source's
    // evidence.
    let again = outcome_summary("Telegram export", 0, 12, 0);
    record_import_attempt(
        &s,
        &again.summary.source,
        "2026-11-04T09:00:00Z",
        &ImportAttempt::Succeeded {
            counts: ImportCounts::of_summary(&again.summary),
        },
    )
    .unwrap();
    let records = import_source_records(&s).unwrap();
    assert_eq!(records.len(), 2);
    let telegram = records
        .iter()
        .find(|r| r.source == "Telegram export")
        .unwrap();
    assert_eq!(telegram.last_attempt_at, "2026-11-04T09:00:00Z");
    assert_eq!(
        telegram.last_success.as_ref().unwrap().at,
        "2026-11-04T09:00:00Z"
    );
}

#[test]
fn a_partial_import_records_its_failed_items_as_counts() {
    let s = storage();
    // A run that finished with items that failed to parse: the outcome
    // is a completed import whose counts carry the failures — the
    // overview can say "partial" from the evidence without a second
    // state.
    let outcome = outcome_summary("Signal export", 8, 1, 3);
    record_import_attempt(
        &s,
        &outcome.summary.source,
        "2026-11-01T09:00:00Z",
        &ImportAttempt::Succeeded {
            counts: ImportCounts::of_summary(&outcome.summary),
        },
    )
    .unwrap();
    let record = &import_source_records(&s).unwrap()[0];
    let ImportAttempt::Succeeded { counts } = &record.last_attempt else {
        panic!("a finished run is a succeeded attempt");
    };
    assert_eq!(counts.failed, 3);
    assert_eq!(counts.seen, 12);
    assert_eq!(record.last_success.as_ref().unwrap().counts.failed, 3);
}

#[test]
fn failed_attempt_leaves_the_previous_success_time_intact() {
    let s = storage();

    // A healthy import at 10:00.
    let ok = outcome_summary("Telegram export", 10, 0, 0);
    record_import_attempt(
        &s,
        &ok.summary.source,
        "2026-11-01T10:00:00Z",
        &ImportAttempt::Succeeded {
            counts: ImportCounts::of_summary(&ok.summary),
        },
    )
    .unwrap();

    // The next attempt fails at 11:00: the attempt side moves, the
    // success side does not — a failed run never reads as a successful
    // refresh nor overwrites the last success.
    record_import_attempt(
        &s,
        &ok.summary.source,
        "2026-11-02T11:00:00Z",
        &ImportAttempt::Failed {
            error: "unreadable chat export directory".into(),
        },
    )
    .unwrap();
    let record = &import_source_records(&s).unwrap()[0];
    assert_eq!(record.last_attempt_at, "2026-11-02T11:00:00Z");
    assert!(matches!(
        record.last_attempt,
        ImportAttempt::Failed { ref error } if error == "unreadable chat export directory"
    ));
    let success = record.last_success.as_ref().expect("success survives");
    assert_eq!(success.at, "2026-11-01T10:00:00Z");
    assert_eq!(success.counts.inserted, 10);

    // A cancelled run commits nothing new to the success side either:
    // its committed counts are durable, the time of the last success
    // stays the completed run's.
    record_import_attempt(
        &s,
        &ok.summary.source,
        "2026-11-03T12:00:00Z",
        &ImportAttempt::Cancelled {
            counts: ImportCounts {
                seen: 5,
                inserted: 4,
                skipped: 1,
                failed: 0,
            },
        },
    )
    .unwrap();
    let record = &import_source_records(&s).unwrap()[0];
    assert_eq!(record.last_attempt_at, "2026-11-03T12:00:00Z");
    assert!(matches!(
        record.last_attempt,
        ImportAttempt::Cancelled { .. }
    ));
    assert_eq!(
        record.last_success.as_ref().unwrap().at,
        "2026-11-01T10:00:00Z"
    );

    // The source's first-ever attempt failing reads as attempted, never
    // imported successfully — not as an absent record.
    record_import_attempt(
        &s,
        "Fresh export",
        "2026-11-04T09:00:00Z",
        &ImportAttempt::Failed {
            error: "no vault database".into(),
        },
    )
    .unwrap();
    let fresh = import_source_records(&s)
        .unwrap()
        .into_iter()
        .find(|r| r.source == "Fresh export")
        .unwrap();
    assert_eq!(fresh.last_attempt_at, "2026-11-04T09:00:00Z");
    assert!(fresh.last_success.is_none());
}

#[test]
fn unreadable_import_evidence_reads_as_never_imported_and_is_replaced() {
    let s = storage();
    s.set_archive_meta(IMPORTS_KEY, "not json").unwrap();
    assert!(import_source_records(&s).unwrap().is_empty());

    // The next finished attempt overwrites the junk wholesale.
    let ok = outcome_summary("Telegram export", 1, 0, 0);
    record_import_attempt(
        &s,
        &ok.summary.source,
        "2026-11-01T10:00:00Z",
        &ImportAttempt::Succeeded {
            counts: ImportCounts::of_summary(&ok.summary),
        },
    )
    .unwrap();
    let records = import_source_records(&s).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].source, "Telegram export");
}

/// The counts helper reads a plain ingest summary too (the non-bounded
/// paths share the shape).
#[test]
fn import_counts_mirror_the_ingest_summary() {
    let summary = IngestSummary {
        source: "demo".into(),
        seen: 7,
        inserted: 5,
        skipped: 1,
        failed: 1,
    };
    assert_eq!(
        ImportCounts::of_summary(&summary),
        ImportCounts {
            seen: 7,
            inserted: 5,
            skipped: 1,
            failed: 1
        }
    );
}

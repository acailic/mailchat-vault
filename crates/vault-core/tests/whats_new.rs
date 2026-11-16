//! The what's-new boot digest (the M7 follow-up): per-source counts of
//! newly archived messages since a timestamp, finished/failed
//! import-and-sync ledger rows since a timestamp, the vault-scoped
//! last-visit record, and the one-read-one-advance contract the app
//! command performs across the pool.

use vault_core::message::NewMessage;
use vault_core::stats::{WhatsNewDigest, WhatsNewJob};
use vault_core::storage::{JobLedgerEntry, Storage};
use vault_core::whats_new::{self, LastVisitRecord, LAST_VISIT_KEY};

fn storage() -> Storage {
    Storage::open_in_memory().expect("open in-memory storage")
}

/// Insert one message (the chat-shaped helper; the digest only reads
/// `created_at` and the source join, so the payload can stay minimal).
fn put(s: &Storage, source: &str, id: &str) {
    s.insert_message(&NewMessage {
        source: source.into(),
        external_id: id.into(),
        channel: "chat".into(),
        chat_id: "alpha".into(),
        author: "ana".into(),
        sent_at: "2026-01-01T09:00:00Z".into(),
        body: format!("body of {id}"),
        raw_json: "{}".into(),
        conversation_key: String::new(),
        reply_refs: Vec::new(),
        original_timestamp: String::new(),
        timezone: String::new(),
        time_confidence: String::new(),
        attachments: Vec::new(),
        source_label: String::new(),
    })
    .unwrap();
}

/// Overwrite a message row's `created_at` with the column's own
/// `YYYY-MM-DD HH:MM:SS` form, so "since" windows are deterministic
/// (the insert path stamps `datetime('now')`, which tests cannot pin).
fn set_created_at(s: &Storage, id: &str, at: &str) {
    s.connection()
        .prepare("UPDATE archive_messages SET created_at = ?1 WHERE external_id = ?2")
        .and_then(|mut stmt| stmt.execute([at, id]))
        .unwrap();
}

fn ledger_entry(n: u64, kind: &str, state: &str, finished_at: &str) -> JobLedgerEntry {
    JobLedgerEntry {
        job_id: n,
        kind: kind.into(),
        submitted_at: "2026-11-01T10:00:00+00:00".into(),
        finished_at: finished_at.into(),
        state: state.into(),
        summary: format!("summary {n}"),
        payload: "{}".into(),
    }
}

// ---------------------------------------------------------------------------
// Storage: per-source counts since a timestamp
// ---------------------------------------------------------------------------

#[test]
fn counts_messages_archived_since_grouped_by_source() {
    let s = storage();
    put(&s, "wa", "a1");
    put(&s, "wa", "a2");
    put(&s, "wa", "a3");
    put(&s, "tg", "t1");
    set_created_at(&s, "a1", "2026-11-10 08:00:00");
    set_created_at(&s, "a2", "2026-11-11 09:00:00");
    set_created_at(&s, "a3", "2026-11-12 10:00:00");
    set_created_at(&s, "t1", "2026-11-12 10:00:00");

    // Strictly newer than the cutoff: a1 (at the cutoff) is excluded.
    let counts = s.messages_archived_since("2026-11-10 08:00:00").unwrap();
    let pairs: Vec<(String, u64)> = counts
        .iter()
        .map(|c| (c.label.clone(), c.count))
        .collect();
    assert_eq!(
        pairs,
        vec![("wa".to_string(), 2), ("tg".to_string(), 1)],
        "heaviest first, label joined, at-cutoff rows excluded"
    );
    // Every row carries a stable, non-empty, distinct instance id.
    assert!(counts.iter().all(|c| !c.source_id.is_empty()));
    assert_ne!(counts[0].source_id, counts[1].source_id);

    // Nothing newer than a future cutoff.
    assert!(s
        .messages_archived_since("2026-11-13 00:00:00")
        .unwrap()
        .is_empty());
}

#[test]
fn counts_mean_newly_archived_not_re_sightings() {
    let s = storage();
    put(&s, "wa", "a1");
    set_created_at(&s, "a1", "2026-11-01 08:00:00");
    // A re-import of the same external id dedups: the row (and its
    // original created_at) stays untouched.
    put(&s, "wa", "a1");

    let counts = s.messages_archived_since("2026-11-15 00:00:00").unwrap();
    assert!(counts.is_empty(), "a deduped re-sighting never counts");
}

// ---------------------------------------------------------------------------
// Storage: ledger rows since a timestamp
// ---------------------------------------------------------------------------

#[test]
fn ledger_rows_since_are_strictly_newer_newest_first() {
    let s = storage();
    s.record_job_ledger(&ledger_entry(
        1,
        "import_mbox",
        "succeeded",
        "2026-11-10T10:00:00+00:00",
    ))
    .unwrap();
    s.record_job_ledger(&ledger_entry(
        2,
        "imap_sync",
        "failed",
        "2026-11-11T11:00:00+00:00",
    ))
    .unwrap();
    s.record_job_ledger(&ledger_entry(
        3,
        "imap_sync_all",
        "succeeded",
        "2026-11-12T12:00:00+00:00",
    ))
    .unwrap();

    // The cutoff itself is excluded; rows newer than it come back
    // newest first.
    let rows = s
        .job_ledger_since("2026-11-10T10:00:00+00:00", 10)
        .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.job_id).collect::<Vec<_>>(),
        vec![3, 2],
        "newest first, at-cutoff row excluded"
    );

    // The limit caps the read.
    assert_eq!(s
        .job_ledger_since("2026-11-01T00:00:00+00:00", 1)
        .unwrap()
        .len(), 1);
    assert!(s
        .job_ledger_since("2026-11-01T00:00:00+00:00", 0)
        .unwrap()
        .is_empty());
    assert!(s
        .job_ledger_since("2026-11-13T00:00:00+00:00", 10)
        .unwrap()
        .is_empty());
}

// ---------------------------------------------------------------------------
// The last-visit record
// ---------------------------------------------------------------------------

#[test]
fn last_visit_round_trips_and_overwrites() {
    let s = storage();
    assert_eq!(whats_new::last_visit(&s).unwrap(), None, "never read");

    whats_new::record_last_visit(&s, "2026-11-12T09:00:00+00:00").unwrap();
    assert_eq!(
        whats_new::last_visit(&s).unwrap(),
        Some(LastVisitRecord {
            last_visit_at: "2026-11-12T09:00:00+00:00".into(),
        })
    );

    // One last-visit, not a history: the second read overwrites.
    whats_new::record_last_visit(&s, "2026-11-13T09:00:00+00:00").unwrap();
    assert_eq!(
        whats_new::last_visit(&s).unwrap().map(|r| r.last_visit_at),
        Some("2026-11-13T09:00:00+00:00".into())
    );

    // The record is vault metadata: it sits in the vault's own store.
    let raw = s.archive_meta(LAST_VISIT_KEY).unwrap().unwrap();
    assert!(raw.contains("2026-11-13T09:00:00+00:00"), "{raw}");
}

#[test]
fn an_unreadable_last_visit_record_reads_as_no_visit() {
    let s = storage();
    s.set_archive_meta(LAST_VISIT_KEY, "not json").unwrap();
    assert_eq!(
        whats_new::last_visit(&s).unwrap(),
        None,
        "junk degrades to the quiet first-visit answer"
    );
}

// ---------------------------------------------------------------------------
// The digest derivation
// ---------------------------------------------------------------------------

/// An empty vault: the digest exists, says nothing, and carries no
/// `since` — the quiet first boot.
#[test]
fn first_visit_on_an_empty_vault_is_an_empty_digest() {
    let s = storage();
    let digest = whats_new::read_and_advance(&s, "2026-11-16T10:00:00+00:00").unwrap();
    assert_eq!(
        digest,
        WhatsNewDigest {
            since: None,
            read_at: "2026-11-16T10:00:00+00:00".into(),
            new_messages: Vec::new(),
            jobs: Vec::new(),
        }
    );
    assert!(digest.is_empty());
}

/// A seeded vault's first visit is just as quiet: no stored visit means
/// nothing to compare against, never a whole-archive dump.
#[test]
fn first_visit_on_a_seeded_vault_is_an_empty_digest_too() {
    let s = storage();
    put(&s, "wa", "a1");
    s.record_job_ledger(&ledger_entry(
        1,
        "import_mbox",
        "succeeded",
        "2026-11-01T10:00:00+00:00",
    ))
    .unwrap();

    let digest = whats_new::read_and_advance(&s, "2026-11-16T10:00:00+00:00").unwrap();
    assert_eq!(digest.since, None);
    assert!(digest.is_empty());
}

/// The seeded happy path: messages landed while away, an import
/// finished, a sync failed — and kinds/states outside the digest's
/// scope stay out.
#[test]
fn digest_reports_new_messages_and_import_sync_jobs_since_the_visit() {
    let s = storage();
    whats_new::record_last_visit(&s, "2026-11-15T09:00:00+00:00").unwrap();

    // While away: two wa messages and one tg message landed...
    put(&s, "wa", "a1");
    put(&s, "wa", "a2");
    put(&s, "tg", "t1");
    // ...but one wa message predates the visit (an older import) and
    // must not count.
    put(&s, "wa", "old");
    set_created_at(&s, "a1", "2026-11-15 12:00:00");
    set_created_at(&s, "a2", "2026-11-16 07:00:00");
    set_created_at(&s, "t1", "2026-11-16 08:00:00");
    set_created_at(&s, "old", "2026-11-01 00:00:00");

    // Jobs while away: the digest's kinds and outcomes only.
    s.record_job_ledger(&ledger_entry(
        1,
        "import_mbox",
        "succeeded",
        "2026-11-15T10:00:00+00:00",
    ))
    .unwrap();
    s.record_job_ledger(&ledger_entry(
        2,
        "imap_sync",
        "failed",
        "2026-11-15T11:00:00+00:00",
    ))
    .unwrap();
    // Before the visit: excluded even though in scope.
    s.record_job_ledger(&ledger_entry(
        3,
        "import_chat_export",
        "succeeded",
        "2026-11-14T10:00:00+00:00",
    ))
    .unwrap();
    // Out of scope: a cancelled import, a seed job, an index build.
    s.record_job_ledger(&ledger_entry(
        4,
        "import_chat_export",
        "cancelled",
        "2026-11-15T12:00:00+00:00",
    ))
    .unwrap();
    s.record_job_ledger(&ledger_entry(
        5,
        "seed_demo",
        "succeeded",
        "2026-11-15T13:00:00+00:00",
    ))
    .unwrap();
    s.record_job_ledger(&ledger_entry(
        6,
        "embed_messages",
        "succeeded",
        "2026-11-15T14:00:00+00:00",
    ))
    .unwrap();

    let digest = whats_new::read_and_advance(&s, "2026-11-16T10:00:00+00:00").unwrap();
    assert_eq!(digest.since.as_deref(), Some("2026-11-15T09:00:00+00:00"));
    assert_eq!(digest.read_at, "2026-11-16T10:00:00+00:00");

    let mut labels: Vec<_> = digest.new_messages.iter().map(|m| &m.label).collect();
    labels.sort();
    assert_eq!(labels, vec!["tg", "wa"]);
    let wa = digest
        .new_messages
        .iter()
        .find(|m| m.label == "wa")
        .unwrap();
    assert_eq!(wa.count, 2, "the pre-visit message never counts");

    assert_eq!(
        digest.jobs,
        vec![
            WhatsNewJob {
                kind: "imap_sync".into(),
                state: "failed".into(),
                finished_at: "2026-11-15T11:00:00+00:00".into(),
                summary: "summary 2".into(),
            },
            WhatsNewJob {
                kind: "import_mbox".into(),
                state: "succeeded".into(),
                finished_at: "2026-11-15T10:00:00+00:00".into(),
                summary: "summary 1".into(),
            },
        ],
        "newest first, in-kind, terminal-only"
    );
}

/// One read advances the timestamp exactly once: the second read counts
/// only newer changes, and a digest read with nothing new is empty.
#[test]
fn one_read_advances_the_cutoff_exactly_once() {
    let s = storage();
    whats_new::record_last_visit(&s, "2026-11-15T09:00:00+00:00").unwrap();
    put(&s, "wa", "a1");
    set_created_at(&s, "a1", "2026-11-15 12:00:00");
    s.record_job_ledger(&ledger_entry(
        1,
        "import_mbox",
        "succeeded",
        "2026-11-15T10:00:00+00:00",
    ))
    .unwrap();

    // Visit one: everything since the cutoff, and the cutoff advances.
    let first = whats_new::read_and_advance(&s, "2026-11-16T10:00:00+00:00").unwrap();
    assert!(!first.is_empty());
    assert_eq!(first.new_messages.len(), 1);
    assert_eq!(first.jobs.len(), 1);
    assert_eq!(
        whats_new::last_visit(&s).unwrap().map(|r| r.last_visit_at),
        Some("2026-11-16T10:00:00+00:00".into())
    );

    // Nothing new lands. Visit two: quiet.
    let second = whats_new::read_and_advance(&s, "2026-11-17T10:00:00+00:00").unwrap();
    assert_eq!(second.since.as_deref(), Some("2026-11-16T10:00:00+00:00"));
    assert!(second.is_empty(), "nothing newer than the advanced cutoff");

    // Something lands after the second visit: visit three sees it.
    put(&s, "wa", "a2");
    set_created_at(&s, "a2", "2026-11-18 09:00:00");
    let third = whats_new::read_and_advance(&s, "2026-11-18T10:00:00+00:00").unwrap();
    assert_eq!(third.new_messages.len(), 1);
    assert_eq!(third.jobs.len(), 0);
}

/// A stored record whose timestamp cannot be resolved reads as no
/// usable visit: quiet digest, and the read overwrites the junk.
#[test]
fn a_malformed_timestamp_reads_as_first_visit_and_self_heals() {
    let s = storage();
    s.set_archive_meta(
        LAST_VISIT_KEY,
        "{\"last_visit_at\":\"not a timestamp\"}",
    )
    .unwrap();
    put(&s, "wa", "a1");

    let digest = whats_new::read_and_advance(&s, "2026-11-16T10:00:00+00:00").unwrap();
    assert_eq!(digest.since, None);
    assert!(digest.is_empty(), "no usable cutoff, nothing counted");
    assert_eq!(
        whats_new::last_visit(&s).unwrap().map(|r| r.last_visit_at),
        Some("2026-11-16T10:00:00+00:00".into()),
        "the read overwrote the junk record"
    );
}

// ---------------------------------------------------------------------------
// The RFC 3339 → created_at conversion, and the migration's index
// ---------------------------------------------------------------------------

#[test]
fn rfc3339_instants_convert_to_the_created_at_column_format() {
    assert_eq!(
        whats_new::sqlite_instant("2026-11-16T10:39:05+00:00").as_deref(),
        Some("2026-11-16 10:39:05")
    );
    // Fractional seconds floor to the column's whole seconds; offsets
    // resolve to UTC.
    assert_eq!(
        whats_new::sqlite_instant("2026-11-16T10:39:05.75+00:00").as_deref(),
        Some("2026-11-16 10:39:05")
    );
    assert_eq!(
        whats_new::sqlite_instant("2026-11-16T12:39:05+02:00").as_deref(),
        Some("2026-11-16 10:39:05")
    );
    assert_eq!(whats_new::sqlite_instant("garbage"), None);
}

#[test]
fn the_since_query_rides_the_created_at_index_not_a_scan() {
    let s = storage();
    let plan: String = s
        .connection()
        .query_row(
            "EXPLAIN QUERY PLAN \
             SELECT si.id, COUNT(*) FROM archive_messages AS m \
             JOIN source_instances AS si ON si.id = m.source_instance_id \
             WHERE m.created_at > '2026-01-01 00:00:00' \
             GROUP BY m.source_instance_id",
            [],
            |row| row.get("detail"),
        )
        .unwrap();
    assert!(
        plan.contains("idx_archive_messages_created_source"),
        "the digest query must seek the migration-20 index, plan was: {plan}"
    );
}

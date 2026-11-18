//! The attachment projection (A6): one indexed row per attachment
//! reference the archive holds, filled by a resumable backfill and read
//! through a files search and a references query — the core of the
//! archive-wide Files view (docs/archive-experience-plan.md §6
//! `attachment_refs`, §9).
//!
//! # Why a projection
//!
//! Linked attachments live only inside each message's `raw_json`
//! `media` array; unlinked export files live only in the
//! `media_objects` registry (migration 13). No index spans both, so an
//! archive-wide files view would scan raw records to answer even simple
//! questions. Migration 22 adds the `attachment_refs` projection — one
//! row per reference, anchored to the message with source and import
//! provenance — and this module is its single writer
//! ([`run_backfill`]) plus its read models ([`files_search`],
//! [`file_references`], [`thumbnail_get`]). Raw imported records are
//! never mutated; the projection is derived and repairable by
//! re-running the backfill.
//!
//! # Availability is projection-honest
//!
//! A reference with a recorded digest is `preserved` — preserved *by
//! reference*. Whether the bytes are still on disk is the existing
//! reconcile surface's question ([`crate::object_store`]), not this
//! projection's. A raw entry that recorded no digest carries its
//! recorded `missing` reason verbatim and stays inspectable through the
//! explicit availability filter of [`files_search`].
//!
//! # Resumable backfill
//!
//! The backfill scans `archive_messages` `raw_json` media entries in
//! bounded batches ordered by id (the scan pattern of
//! [`crate::storage::Storage::media_digests`]), upserting refs
//! idempotently (`UNIQUE (message_id, ref_index)`). The checkpoint —
//! the last message id whose refs are committed — is one keyed
//! `archive_meta` row written inside each batch's transaction, the same
//! shape the archive revision uses, so an interrupted run resumes
//! exactly where it stopped and a completed run re-scans nothing. When
//! the scan reaches the end, a coverage check compares projection rows
//! against a fresh count of raw references; only a matching projection
//! reports success, so no reader depends on a half-filled index.
//!
//! The job form ([`run_backfill_job`]) runs the same batches through
//! the existing job runner ([`crate::jobs::DbPool::submit_job`], kind
//! [`FILES_BACKFILL_KIND`]) — one batch per write round trip, progress
//! and cancellation between batches; no new scheduler exists or is
//! needed. IPC wiring in the Tauri shell is a later slice.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as CURSOR_BASE64;
use base64::Engine;
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::jobs::{JobContext, JobError};
use crate::query::{MAX_LIMIT, MAX_OFFSET};
use crate::storage::{RegisteredMediaObject, Storage};
use crate::{Error, Result};

/// The `job_submit` kind of the attachment backfill (the string the
/// later IPC slice registers; this module keeps it the single constant).
pub const FILES_BACKFILL_KIND: &str = "files_backfill";

/// The `archive_meta` key holding the backfill checkpoint: the last
/// `archive_messages.id` whose media references are committed to the
/// projection. Written inside each batch's transaction; `0`/absent
/// means nothing is committed yet.
pub const BACKFILL_CHECKPOINT_KEY: &str = "files_backfill_checkpoint";

/// Messages per backfill batch. Each batch is one transaction (upserts
/// plus checkpoint), small enough that the writer lane is never held
/// long and a stop/resume boundary is never far away.
pub const BACKFILL_BATCH: u64 = 200;

/// Default page size of [`files_search`].
pub const FILES_DEFAULT_LIMIT: u32 = 50;

/// How long a digest group key may be considered — missing-reference
/// groups key on name+kind; this bounds neither (names are export
/// strings) but documents that keys are opaque and ordered.
const MISSING_KEY_PREFIX: &str = "missing";

// ---------------------------------------------------------------------------
// Backfill
// ---------------------------------------------------------------------------

/// What one backfill run did. A run stopped by `batch_limit` reports
/// `completed: false` — committed work stands, and the next run resumes
/// from the checkpoint; only a run whose scan reached the end reports
/// `completed: true`, and only then is `coverage` checked and present.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesBackfillOutcome {
    /// Messages examined this run (checkpoint-skipped ranges count 0).
    pub scanned_messages: u64,
    /// Reference rows upserted this run (idempotent re-writes included).
    pub refs_upserted: u64,
    /// The checkpoint this run started from (already-committed last id).
    pub resumed_from: i64,
    /// True when the scan reached the end of `archive_messages`.
    pub completed: bool,
    /// Present iff `completed`: the projection-vs-raw comparison the
    /// run verified before reporting success.
    pub coverage: Option<FilesCoverage>,
}

/// The coverage check's two counts: raw `media` entries across every
/// archived message versus rows in the projection. A completed,
/// trustworthy backfill has them equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesCoverage {
    /// Fresh count of `media` entries (object elements) in every
    /// message's `raw_json`.
    pub raw_references: u64,
    /// Rows in `attachment_refs`.
    pub projection_rows: u64,
}

impl FilesCoverage {
    /// Whether the two counts agree — the gate a completed backfill
    /// must pass before any reader depends on the projection.
    pub fn is_complete(&self) -> bool {
        self.raw_references == self.projection_rows
    }
}

/// What one batch did: how many messages it upserted refs for, how many
/// rows it wrote, and the new checkpoint (inside the committed
/// transaction; unchanged when `scanned == 0`).
struct BatchOutcome {
    scanned: u64,
    refs_upserted: u64,
    last_id: i64,
}

/// Run the attachment backfill on one storage handle (the single-writer
/// form tests and maintenance paths use; the job-runner form is
/// [`run_backfill_job`]). `batch_limit` caps how many messages this run
/// examines — the test-side simulation of a killed run — and `None`
/// runs to the end. Every batch commits its upserts and its checkpoint
/// atomically, so an interrupted run loses nothing and re-runs skip
/// completed ranges. A run that reaches the end verifies coverage and
/// fails (leaving the committed work intact) when the projection does
/// not match the raw reference count.
///
/// Transcription enqueue stays off (the `transcribe_audio` setting's
/// default); pass `run_backfill_opt(.., true)` when it is enabled.
pub fn run_backfill(storage: &Storage, batch_limit: Option<u64>) -> Result<FilesBackfillOutcome> {
    run_backfill_opt(storage, batch_limit, false)
}

/// [`run_backfill`] with the transcription setting threaded in: with
/// `enqueue_transcripts` set, every batch also records `pending`
/// `transcripts` rows for preserved audio references
/// ([`crate::transcription::enqueue_in_tx`]) — enqueue-only, never
/// sidecar work, so the backfill stays exactly as fast.
pub fn run_backfill_opt(
    storage: &Storage,
    batch_limit: Option<u64>,
    enqueue_transcripts: bool,
) -> Result<FilesBackfillOutcome> {
    let mut outcome = FilesBackfillOutcome::default();
    outcome.resumed_from = read_checkpoint(storage.connection())?;
    let mut after = outcome.resumed_from;
    loop {
        let Some(take) = next_batch_size(batch_limit, outcome.scanned_messages) else {
            break;
        };
        let batch = backfill_one_batch(storage.connection(), after, take, enqueue_transcripts)?;
        if batch.scanned == 0 {
            outcome.completed = true;
            break;
        }
        outcome.scanned_messages += batch.scanned;
        outcome.refs_upserted += batch.refs_upserted;
        after = batch.last_id;
        if batch.scanned < take {
            outcome.completed = true;
            break;
        }
    }
    finish_coverage(storage.connection(), &mut outcome)?;
    Ok(outcome)
}

/// The job-runner body of the backfill (submitted as kind
/// [`FILES_BACKFILL_KIND`]):
///
/// ```ignore
/// pool.submit_job(files::FILES_BACKFILL_KIND, |ctx| {
///     files::run_backfill_job(ctx, None, false)
/// })
/// ```
///
/// Each batch is one write round trip through the pool (the writer lane
/// is held only for that batch's transaction); the checkpoint read,
/// progress reporting, and the cancellation check happen between
/// batches, on the runner thread. `batch_limit` and the outcome
/// semantics match [`run_backfill`]; `enqueue_transcripts` threads the
/// `transcribe_audio` setting through (enqueue-only — see
/// [`run_backfill_opt`]).
pub fn run_backfill_job(
    ctx: &JobContext<'_>,
    batch_limit: Option<u64>,
    enqueue_transcripts: bool,
) -> std::result::Result<Value, JobError> {
    let mut outcome = FilesBackfillOutcome::default();
    outcome.resumed_from = ctx.read(|s| read_checkpoint(s.connection()))?;
    // Progress positions are message ids (checkpoint vs the newest id):
    // monotone, and the denominator stays true while imports run.
    let total = ctx.read(|s| {
        s.connection()
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM archive_messages",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map_err(Error::from)
    })?;
    let mut after = outcome.resumed_from;
    loop {
        if ctx.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let Some(take) = next_batch_size(batch_limit, outcome.scanned_messages) else {
            break;
        };
        let batch = ctx
            .write(move |s| backfill_one_batch(s.connection(), after, take, enqueue_transcripts))?;
        if batch.scanned == 0 {
            outcome.completed = true;
            break;
        }
        outcome.scanned_messages += batch.scanned;
        outcome.refs_upserted += batch.refs_upserted;
        after = batch.last_id;
        ctx.progress(
            after.max(0) as u64,
            total.max(0) as u64,
            "building the attachment index",
        );
        if batch.scanned < take {
            outcome.completed = true;
            break;
        }
    }
    if outcome.completed {
        ctx.read(|s| finish_coverage(s.connection(), &mut outcome))?;
    }
    serde_json::to_value(&outcome).map_err(|e| JobError::Internal {
        message: e.to_string(),
    })
}

/// How many messages the next batch may scan: `None` stops the run
/// (limit reached), else `min(batch_limit remaining, BACKFILL_BATCH)`.
fn next_batch_size(batch_limit: Option<u64>, scanned: u64) -> Option<u64> {
    let budget = batch_limit.map(|limit| limit.saturating_sub(scanned));
    match budget {
        Some(0) => None,
        Some(n) => Some(n.min(BACKFILL_BATCH)),
        None => Some(BACKFILL_BATCH),
    }
}

/// Run the coverage check for a completed run and stamp it on the
/// outcome. A completed run whose projection disagrees with the raw
/// reference count is an error (the committed work stands; the failure
/// says "do not read this projection yet").
fn finish_coverage(
    conn: &Connection,
    outcome: &mut FilesBackfillOutcome,
) -> Result<Option<FilesCoverage>> {
    if !outcome.completed {
        return Ok(None);
    }
    let coverage = coverage_check(conn)?;
    if !coverage.is_complete() {
        return Err(Error::Jobs(format!(
            "files backfill coverage mismatch: {} raw references, {} projection rows",
            coverage.raw_references, coverage.projection_rows
        )));
    }
    outcome.coverage = Some(coverage);
    Ok(outcome.coverage)
}

/// The backfill checkpoint: the last message id whose refs are
/// committed. Absent, non-numeric, or negative values read as 0 — a
/// damaged checkpoint restarts the scan, and the idempotent upserts
/// make that safe (just slower).
pub fn read_checkpoint(conn: &Connection) -> Result<i64> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM archive_meta WHERE key = ?1",
            [BACKFILL_CHECKPOINT_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value
        .and_then(|v| v.parse().ok())
        .filter(|n| *n >= 0)
        .unwrap_or(0))
}

/// One backfill batch, inside one transaction: scan up to `take`
/// media-carrying messages after `after_id`, upsert their refs, and —
/// when anything was scanned — advance the checkpoint to the last id
/// committed. `scanned == 0` means the scan reached the end (nothing
/// written, checkpoint unchanged). With `enqueue_transcripts` the
/// batch also enqueues pending transcript rows for preserved audio
/// references (the opt-in setting's import/reconcile hook; the
/// sidecar never runs here).
fn backfill_one_batch(
    conn: &Connection,
    after_id: i64,
    take: u64,
    enqueue_transcripts: bool,
) -> Result<BatchOutcome> {
    let tx = conn.unchecked_transaction()?;
    let mut scanned = 0u64;
    let mut refs_upserted = 0u64;
    let mut last_id = after_id;
    {
        let mut stmt = tx.prepare(
            "SELECT id, source_instance_id, raw_json FROM archive_messages
             WHERE id > ?1 AND raw_json LIKE '%\"media\"%'
             ORDER BY id LIMIT ?2",
        )?;
        let mut rows = stmt.query(params![after_id, take as i64])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let source: String = row.get(1)?;
            let raw_json: String = row.get(2)?;
            refs_upserted += upsert_message_refs(&tx, id, &source, &raw_json)?;
            last_id = id;
            scanned += 1;
        }
    }
    if enqueue_transcripts {
        crate::transcription::enqueue_in_tx(&tx)?;
    }
    if scanned > 0 {
        tx.execute(
            "INSERT INTO archive_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![BACKFILL_CHECKPOINT_KEY, last_id.to_string()],
        )?;
    }
    tx.commit()?;
    Ok(BatchOutcome {
        scanned,
        refs_upserted,
        last_id,
    })
}

/// Upsert one message's attachment refs: one row per object element of
/// the message's `raw_json` `media` array (index within the array =
/// `ref_index`, the uniqueness that makes re-runs idempotent). The
/// import job provenance is the message's earliest recorded occurrence
/// (`''` for pre-M2.2 rows that never recorded one). Returns the number
/// of rows written.
fn upsert_message_refs(
    tx: &Transaction<'_>,
    message_id: i64,
    source_instance_id: &str,
    raw_json: &str,
) -> Result<u64> {
    let entries = media_entry_objects(raw_json);
    if entries.is_empty() {
        return Ok(0);
    }
    let import_id: String = tx
        .query_row(
            "SELECT import_id FROM message_occurrences
             WHERE message_id = ?1 AND import_id <> ''
             ORDER BY observed_at, rowid LIMIT 1",
            params![message_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default();
    let mut written = 0u64;
    for (index, entry) in entries.iter().enumerate() {
        let name = str_field(entry, "name").unwrap_or_default();
        let kind = str_field(entry, "kind").unwrap_or_default();
        let size = entry.get("size").and_then(Value::as_u64).unwrap_or(0);
        let digest = str_field(entry, "sha256").filter(|s| !s.is_empty());
        let flagged_missing = entry.get("missing").and_then(Value::as_bool) == Some(true);
        let recorded_reason = str_field(entry, "reason").unwrap_or_default();
        // Availability is projection-honest: a digest that was never
        // preserved (or was explicitly flagged missing) is `missing`
        // with its recorded reason; verifying bytes on disk stays with
        // the reconcile surface.
        let (availability, sha256, reason) = match digest.filter(|_| !flagged_missing) {
            Some(sha) => ("preserved", Some(sha), String::new()),
            None => (
                "missing",
                None,
                if recorded_reason.is_empty() {
                    "unpreserved".to_string()
                } else {
                    recorded_reason
                },
            ),
        };
        tx.execute(
            "INSERT INTO attachment_refs
                 (message_id, ref_index, source_instance_id, import_id,
                  name, kind, size, sha256, availability, reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(message_id, ref_index) DO UPDATE SET
                 source_instance_id = excluded.source_instance_id,
                 import_id = excluded.import_id,
                 name = excluded.name,
                 kind = excluded.kind,
                 size = excluded.size,
                 sha256 = excluded.sha256,
                 availability = excluded.availability,
                 reason = excluded.reason",
            params![
                message_id,
                index as i64,
                source_instance_id,
                import_id,
                name,
                kind,
                size as i64,
                sha256,
                availability,
                reason
            ],
        )?;
        written += 1;
    }
    Ok(written)
}

/// The object elements of a message's `raw_json` `media` array (the
/// same read [`crate::storage`] applies; non-object elements are not
/// references and are skipped everywhere — upsert and coverage count
/// alike — so the two can never disagree).
fn media_entry_objects(raw_json: &str) -> Vec<serde_json::Map<String, Value>> {
    serde_json::from_str::<Value>(raw_json)
        .ok()
        .and_then(|value| value.get("media").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| entry.as_object().cloned())
        .collect()
}

/// The coverage check: a fresh count of raw references versus the
/// projection's row count. Run before a completed backfill reports
/// success, so no reader depends on a projection that misses rows.
pub fn coverage_check(conn: &Connection) -> Result<FilesCoverage> {
    let mut raw_references = 0u64;
    {
        let mut stmt = conn
            .prepare("SELECT raw_json FROM archive_messages WHERE raw_json LIKE '%\"media\"%'")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let raw_json: String = row.get(0)?;
            raw_references += media_entry_objects(&raw_json).len() as u64;
        }
    }
    let projection_rows: u64 = conn.query_row("SELECT COUNT(*) FROM attachment_refs", [], |r| {
        r.get::<_, i64>(0).map(|n| n.max(0) as u64)
    })?;
    Ok(FilesCoverage {
        raw_references,
        projection_rows,
    })
}

fn str_field(entry: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    entry.get(field).and_then(Value::as_str).map(str::to_string)
}

// ---------------------------------------------------------------------------
// files_search
// ---------------------------------------------------------------------------

/// One page of the files search (see [`files_search`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesSearchPage {
    /// Unique-file groups, group key ascending, at most `limit` of them.
    pub files: Vec<FileSummary>,
    /// The cursor the next page passes back; `None` when this page
    /// exhausted every group.
    pub next_cursor: Option<String>,
}

/// One unique file in the files search: identical bytes appear once
/// regardless of how many messages reference them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSummary {
    /// The content digest grouping the references. `None` for a group
    /// of missing references: no bytes were preserved, so there is no
    /// content identity — the group key is the recorded name and kind.
    pub sha256: Option<String>,
    /// Representative original name (the first reference's, by group).
    pub name: String,
    /// Coarse kind (`image`, `video`, `audio`, `document`, …).
    pub kind: String,
    /// Recorded size in bytes (the largest reference's; 0 for missing).
    pub size: u64,
    /// How many message attachment references matched the filters in
    /// this group — the occurrence count, distinct from the unique-file
    /// grouping: the same bytes attached to N messages are one file
    /// with N occurrences.
    pub occurrences: u64,
    /// `preserved` or `missing` (projection-honest; see the module doc).
    pub availability: String,
    /// The recorded reason when `availability` is `missing` (`''`
    /// otherwise).
    pub reason: String,
    /// True when this group comes from the `media_objects` registry and
    /// no message references it: a source-level unlinked export file,
    /// listed with registry provenance and **no invented message
    /// association**.
    pub unlinked: bool,
}

/// The files search request (see [`files_search`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesSearchRequest {
    /// Exact coarse-kind match (`image`, `video`, …); `None` = all.
    #[serde(default)]
    pub kind: Option<String>,
    /// One source instance id; `None`/empty = all sources. (A person
    /// filter is deliberately absent — it waits for A5b.)
    #[serde(default)]
    pub source_instance_id: Option<String>,
    /// Inclusive lower message-date bound, `YYYY-MM-DD` UTC. Messages
    /// with unknown dates are excluded while any date bound is set;
    /// unlinked registry objects carry no message date, so they are
    /// excluded too.
    #[serde(default)]
    pub from_date: Option<String>,
    /// Inclusive upper message-date bound, `YYYY-MM-DD` UTC.
    #[serde(default)]
    pub to_date: Option<String>,
    /// `Some("preserved")` or `Some("missing")` — the explicit
    /// availability filter that keeps missing references inspectable;
    /// `None` = both.
    #[serde(default)]
    pub availability: Option<String>,
    /// Page size, clamped `1..=MAX_LIMIT`; default 50.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Opaque keyset cursor from a previous page's `next_cursor`.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Search the attachment projection as unique files: message references
/// grouped by digest, unioned with the `media_objects` registry's
/// unlinked objects (which appear with registry provenance and no
/// message association). Pages are keyset-paginated over the group key
/// (digest, or name+kind for missing-reference groups) — the cursor
/// pattern of [`crate::search2`], so deep pages stay stable while the
/// archive grows at the tail.
///
/// This reads the projection, not the raw records: run the backfill
/// (whose completion includes the coverage check) before depending on
/// it.
pub fn files_search(storage: &Storage, request: &FilesSearchRequest) -> Result<FilesSearchPage> {
    let limit = request
        .limit
        .unwrap_or(FILES_DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);
    let availability = match request.availability.as_deref().map(str::trim) {
        None | Some("") => None,
        Some("preserved") => Some("preserved"),
        Some("missing") => Some("missing"),
        Some(other) => {
            return Err(Error::InvalidQuery(format!(
                "availability filter must be 'preserved' or 'missing', not {other:?}"
            )))
        }
    };
    let kind = normalized_exact(&request.kind);
    let source = normalized_exact(&request.source_instance_id);
    let from_date = validate_day(&request.from_date, "from_date")?;
    let to_date = validate_day(&request.to_date, "to_date")?;
    if let (Some(from), Some(to)) = (&from_date, &to_date) {
        if from > to {
            return Err(Error::InvalidQuery(format!(
                "from_date {from} is after to_date {to}"
            )));
        }
    }
    let cursor = match &request.cursor {
        Some(raw) => Some(decode_files_cursor(raw)?),
        None => None,
    };
    let dated = from_date.is_some() || to_date.is_some();

    // Three key-ordered group sources, merged and re-truncated: digest
    // groups (message refs), missing-reference groups, and the unlinked
    // registry. Each query fetches limit+1 so "any source returned more
    // than the page" is detectable after the merge.
    let fetch = (limit + 1) as i64;
    let mut groups: Vec<(String, FileSummary)> = Vec::new();

    if availability != Some("missing") {
        for group in digest_groups(
            storage,
            GroupFilters {
                kind: kind.as_deref(),
                source: source.as_deref(),
                from_date: from_date.as_deref(),
                to_date: to_date.as_deref(),
                after_key: cursor.as_deref(),
                fetch,
                dated,
            },
        )? {
            let key = group.sha256.clone().expect("digest group");
            groups.push((key, group));
        }
    }
    if availability != Some("preserved") {
        for group in missing_groups(
            storage,
            GroupFilters {
                kind: kind.as_deref(),
                source: source.as_deref(),
                from_date: from_date.as_deref(),
                to_date: to_date.as_deref(),
                after_key: cursor.as_deref(),
                fetch,
                dated,
            },
        )? {
            let name = group.name.clone();
            let kind_key = group.kind.clone();
            groups.push((missing_group_key(&name, &kind_key), group));
        }
    }
    // Registry objects are preserved by registration; they carry no
    // message, so message-anchored filters (source, dates) exclude
    // them rather than inventing an association, and a digest already
    // grouped by its message references keeps that (richer) group.
    if availability != Some("missing") && source.is_none() && !dated {
        let grouped: std::collections::HashSet<String> =
            groups.iter().map(|(key, _)| key.clone()).collect();
        for group in registry_groups(storage, kind.as_deref(), cursor.as_deref(), fetch)? {
            let key = group.sha256.clone().expect("registry group");
            if grouped.contains(&key) {
                continue;
            }
            groups.push((key, group));
        }
    }

    groups.sort_by(|a, b| a.0.cmp(&b.0));
    let mut files: Vec<FileSummary> = Vec::with_capacity(limit as usize);
    let mut last_emitted: Option<String> = None;
    for (key, group) in &groups {
        if files.len() == limit as usize {
            break;
        }
        files.push(group.clone());
        last_emitted = Some(key.clone());
    }
    // More groups remain beyond the page exactly when some group was
    // never emitted; the cursor is the last emitted group's key, and
    // the next page starts strictly after it (the search2 keyset
    // semantics).
    let next_cursor = if groups.len() > files.len() {
        last_emitted
            .map(|key| encode_files_cursor(&key))
            .transpose()?
    } else {
        None
    };
    Ok(FilesSearchPage { files, next_cursor })
}

/// Filters shared by the two message-anchored group queries.
struct GroupFilters<'a> {
    kind: Option<&'a str>,
    source: Option<&'a str>,
    from_date: Option<&'a str>,
    to_date: Option<&'a str>,
    /// Keyset cursor: skip groups keyed at or before it.
    after_key: Option<&'a str>,
    fetch: i64,
    dated: bool,
}

/// Message-referenced digests, grouped: one row per digest with its
/// occurrence count under the filters.
fn digest_groups(storage: &Storage, f: GroupFilters<'_>) -> Result<Vec<FileSummary>> {
    let conn = storage.connection();
    let mut filter = String::new();
    let mut values: Vec<SqlValue> = Vec::new();
    filter.push_str(" AND r.sha256 IS NOT NULL");
    if let Some(kind) = f.kind {
        filter.push_str(" AND r.kind = ?");
        values.push(SqlValue::from(kind.to_string()));
    }
    if let Some(source) = f.source {
        filter.push_str(" AND r.source_instance_id = ?");
        values.push(SqlValue::from(source.to_string()));
    }
    let mut date_join = String::new();
    if f.dated {
        date_join = " JOIN archive_messages m ON m.id = r.message_id".to_string();
        if let Some(from) = f.from_date {
            filter.push_str(" AND substr(m.sent_instant, 1, 10) >= ?");
            values.push(SqlValue::from(from.to_string()));
        }
        if let Some(to) = f.to_date {
            filter.push_str(" AND substr(m.sent_instant, 1, 10) <= ?");
            values.push(SqlValue::from(to.to_string()));
        }
    }
    if let Some(after) = f.after_key {
        filter.push_str(" AND r.sha256 > ?");
        values.push(SqlValue::from(after.to_string()));
    }
    values.push(SqlValue::Integer(f.fetch));
    let sql = format!(
        "SELECT r.sha256, MIN(r.name), MIN(r.kind), MAX(r.size), COUNT(*)
         FROM attachment_refs r{date_join}
         WHERE 1=1 {filter}
         GROUP BY r.sha256
         ORDER BY r.sha256
         LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
        Ok(FileSummary {
            sha256: Some(row.get(0)?),
            name: row.get(1)?,
            kind: row.get(2)?,
            size: row.get::<_, i64>(3)?.max(0) as u64,
            occurrences: row.get::<_, i64>(4)?.max(0) as u64,
            availability: "preserved".to_string(),
            reason: String::new(),
            unlinked: false,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Missing references, grouped by recorded name+kind (no digest exists
/// to group by). Each group carries its recorded reason and stays
/// inspectable through the explicit availability filter.
fn missing_groups(storage: &Storage, f: GroupFilters<'_>) -> Result<Vec<FileSummary>> {
    let conn = storage.connection();
    let mut filter = String::new();
    let mut values: Vec<SqlValue> = Vec::new();
    filter.push_str(" AND r.sha256 IS NULL");
    if let Some(kind) = f.kind {
        filter.push_str(" AND r.kind = ?");
        values.push(SqlValue::from(kind.to_string()));
    }
    if let Some(source) = f.source {
        filter.push_str(" AND r.source_instance_id = ?");
        values.push(SqlValue::from(source.to_string()));
    }
    let mut date_join = String::new();
    if f.dated {
        date_join = " JOIN archive_messages m ON m.id = r.message_id".to_string();
        if let Some(from) = f.from_date {
            filter.push_str(" AND substr(m.sent_instant, 1, 10) >= ?");
            values.push(SqlValue::from(from.to_string()));
        }
        if let Some(to) = f.to_date {
            filter.push_str(" AND substr(m.sent_instant, 1, 10) <= ?");
            values.push(SqlValue::from(to.to_string()));
        }
    }
    // The group key is built in SQL exactly as [`missing_group_key`]
    // builds it in Rust, so the keyset predicate and the Rust-side
    // sort agree (char(31) separates name and kind; a name containing
    // the separator cannot shift another group's boundary because the
    // key is only ever compared for order, never parsed).
    let mut having = String::new();
    if let Some(after) = f.after_key {
        having = " HAVING 'missing' || char(31) || r.name || char(31) || r.kind > ?".to_string();
        values.push(SqlValue::from(after.to_string()));
    }
    values.push(SqlValue::Integer(f.fetch));
    let sql = format!(
        "SELECT r.name, r.kind, MAX(r.size), COUNT(*), MIN(r.reason)
         FROM attachment_refs r{date_join}
         WHERE 1=1 {filter}
         GROUP BY r.name, r.kind{having}
         ORDER BY r.name, r.kind
         LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
        Ok(FileSummary {
            sha256: None,
            name: row.get(0)?,
            kind: row.get(1)?,
            size: row.get::<_, i64>(2)?.max(0) as u64,
            occurrences: row.get::<_, i64>(3)?.max(0) as u64,
            availability: "missing".to_string(),
            reason: row.get(4)?,
            unlinked: false,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Unlinked registry objects: stored export files no message
/// references, listed with registry provenance and zero occurrences.
fn registry_groups(
    storage: &Storage,
    kind: Option<&str>,
    after_key: Option<&str>,
    fetch: i64,
) -> Result<Vec<FileSummary>> {
    let conn = storage.connection();
    let mut filter = String::new();
    let mut values: Vec<SqlValue> = Vec::new();
    if let Some(kind) = kind {
        filter.push_str(" AND o.kind = ?");
        values.push(SqlValue::from(kind.to_string()));
    }
    if let Some(after) = after_key {
        filter.push_str(" AND o.sha256 > ?");
        values.push(SqlValue::from(after.to_string()));
    }
    values.push(SqlValue::Integer(fetch));
    let sql = format!(
        "SELECT o.sha256, o.name, o.kind, o.size
         FROM media_objects o
         WHERE 1=1 {filter}
         ORDER BY o.sha256
         LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
        Ok(FileSummary {
            sha256: Some(row.get(0)?),
            name: row.get(1)?,
            kind: row.get(2)?,
            size: row.get::<_, i64>(3)?.max(0) as u64,
            occurrences: 0,
            availability: "preserved".to_string(),
            reason: String::new(),
            unlinked: true,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The stable group key of a missing-reference group. Built with the
/// same separator the SQL side uses (`char(31)`), prefixed so missing
/// groups order deterministically after digest groups.
fn missing_group_key(name: &str, kind: &str) -> String {
    format!("{MISSING_KEY_PREFIX}\x1f{name}\x1f{kind}")
}

/// Trimmed non-empty string, else `None` (an empty filter means "all").
fn normalized_exact(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// A `YYYY-MM-DD` day bound (validated: exactly 10 chars, digits in
/// position, honest parse of month/day ranges via `chrono`).
fn validate_day(value: &Option<String>, field: &str) -> Result<Option<String>> {
    let Some(day) = normalized_exact(value) else {
        return Ok(None);
    };
    if chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").is_err() {
        return Err(Error::InvalidQuery(format!(
            "{field} must be YYYY-MM-DD, not {day:?}"
        )));
    }
    Ok(Some(day))
}

// ---------------------------------------------------------------------------
// Cursors (the search2 keyset pattern)
// ---------------------------------------------------------------------------

/// Cursor format version this build reads and writes.
const FILES_CURSOR_VERSION: u32 = 1;

/// Wire form of a files cursor (JSON, then base64): the last emitted
/// group key. `v` guards future format changes.
#[derive(Serialize, Deserialize)]
struct FilesCursorWire {
    v: u32,
    key: String,
}

fn encode_files_cursor(key: &str) -> Result<String> {
    let json = serde_json::to_string(&FilesCursorWire {
        v: FILES_CURSOR_VERSION,
        key: key.to_string(),
    })
    .map_err(|err| Error::InvalidQuery(format!("malformed cursor: {err}")))?;
    Ok(CURSOR_BASE64.encode(json))
}

fn decode_files_cursor(raw: &str) -> Result<String> {
    let malformed = |detail: &str| Error::InvalidQuery(format!("malformed cursor: {detail}"));
    let json = CURSOR_BASE64
        .decode(raw.trim())
        .map_err(|_| malformed("not valid cursor text"))?;
    let wire: FilesCursorWire =
        serde_json::from_slice(&json).map_err(|_| malformed("not valid cursor text"))?;
    if wire.v != FILES_CURSOR_VERSION {
        return Err(malformed("unknown cursor version"));
    }
    if wire.key.is_empty() {
        return Err(malformed("cursor carries no group key"));
    }
    Ok(wire.key)
}

// ---------------------------------------------------------------------------
// file_references
// ---------------------------------------------------------------------------

/// One message's reference to one digest, with its full provenance (the
/// row behind [`file_references`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReferenceRow {
    pub message_id: i64,
    /// The source instance the message belongs to.
    pub source_instance_id: String,
    /// The bounded import run that first observed the message (`''`
    /// when never recorded).
    pub import_id: String,
    /// Original display name recorded by the reference.
    pub name: String,
    pub kind: String,
    pub size: u64,
    /// `preserved` or `missing`.
    pub availability: String,
    /// Recorded reason when missing (`''` otherwise).
    pub reason: String,
    /// Message context (as stored; `''` when unknown or the message row
    /// is gone — the projection keeps the reference honest instead of
    /// dropping it).
    pub author: String,
    pub chat_id: String,
    pub conversation: String,
    pub date: String,
}

/// Every referring message of one digest (see [`file_references`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReferences {
    pub sha256: String,
    /// Total message references, ignoring the page window.
    pub total: u64,
    pub offset: u32,
    pub limit: u32,
    /// The page, message id ascending (one row per reference, so a
    /// digest one message attaches twice lists that message twice —
    /// every occurrence is a distinct reference).
    pub references: Vec<FileReferenceRow>,
    /// The `media_objects` registration for this digest, when it has
    /// one: an unlinked export file carries its registry provenance
    /// here and no message association.
    pub registry: Option<RegisteredMediaObject>,
}

/// List every referring message and source of one digest — the details
/// side of a gallery tile: a tile groups identical bytes, this query
/// lists each message that attached them. Includes each reference's
/// availability (a digest can be preserved in one message and recorded
/// missing — without a digest — elsewhere; only digest-carrying rows
/// match by construction). Bounded pages with the full total, oldest
/// message first (the paging convention of
/// [`crate::storage::Storage::media_digest_references`]). For an
/// unlinked digest, `references` is empty and `registry` carries the
/// provenance — no message is ever invented.
pub fn file_references(
    storage: &Storage,
    sha256: &str,
    offset: u32,
    limit: u32,
) -> Result<FileReferences> {
    let sha256 = sha256.trim();
    if sha256.is_empty() {
        return Err(Error::InvalidQuery("digest must not be empty".into()));
    }
    let conn = storage.connection();
    let mut all = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT r.message_id, r.source_instance_id, r.import_id, r.name, r.kind,
                    r.size, r.availability, r.reason,
                    COALESCE(m.author, ''), COALESCE(m.chat_id, ''),
                    COALESCE(c.label, ''),
                    COALESCE(m.sent_instant, m.original_timestamp, '')
             FROM attachment_refs r
             LEFT JOIN archive_messages m ON m.id = r.message_id
             LEFT JOIN conversations c ON c.id = m.conversation_id
             WHERE r.sha256 = ?1
             ORDER BY r.message_id, r.ref_index",
        )?;
        let rows = stmt.query_map(params![sha256], |row| {
            Ok(FileReferenceRow {
                message_id: row.get(0)?,
                source_instance_id: row.get(1)?,
                import_id: row.get(2)?,
                name: row.get(3)?,
                kind: row.get(4)?,
                size: row.get::<_, i64>(5)?.max(0) as u64,
                availability: row.get(6)?,
                reason: row.get(7)?,
                author: row.get(8)?,
                chat_id: row.get(9)?,
                conversation: row.get(10)?,
                date: row.get(11)?,
            })
        })?;
        all.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
    }
    let total = all.len() as u64;
    let limit = limit.clamp(1, MAX_LIMIT);
    let offset = offset.min(MAX_OFFSET);
    let references = all
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    let registry = conn
        .query_row(
            "SELECT sha256, size, name, kind, origin, import_id, first_seen
             FROM media_objects WHERE sha256 = ?1",
            params![sha256],
            |row| {
                Ok(RegisteredMediaObject {
                    sha256: row.get(0)?,
                    size: row.get::<_, i64>(1)?.max(0) as u64,
                    name: row.get(2)?,
                    kind: row.get(3)?,
                    origin: row.get(4)?,
                    import_id: row.get(5)?,
                    first_seen: row.get(6)?,
                })
            },
        )
        .optional()?;
    Ok(FileReferences {
        sha256: sha256.to_string(),
        total,
        offset,
        limit,
        references,
        registry,
    })
}

// ---------------------------------------------------------------------------
// Thumbnail stub (A9-adjacent)
// ---------------------------------------------------------------------------

/// The availability fact [`thumbnail_get`] returns. Real thumbnail
/// generation is out of scope here (A9); the stub answers only whether
/// the file the thumbnail would be made of is preserved-by-reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThumbnailStatus {
    pub available: bool,
}

/// The thumbnail stub: no bytes are generated or read — this answers
/// the availability fact for one digest (`true` when the projection or
/// the registry records the digest as preserved-by-reference).
pub fn thumbnail_get(storage: &Storage, sha256: &str) -> Result<ThumbnailStatus> {
    let sha256 = sha256.trim();
    if sha256.is_empty() {
        return Ok(ThumbnailStatus { available: false });
    }
    let available: bool = storage.connection().query_row(
        "SELECT EXISTS(
             SELECT 1 FROM attachment_refs
             WHERE sha256 = ?1 AND availability = 'preserved')
            OR EXISTS(
             SELECT 1 FROM media_objects WHERE sha256 = ?1)",
        params![sha256],
        |row| row.get(0),
    )?;
    Ok(ThumbnailStatus { available })
}

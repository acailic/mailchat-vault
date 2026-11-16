//! Persisted archive-health evidence: the two records the archive
//! health overview composes that no live table already carries.
//!
//! Both follow the [`crate::backup`] "last verified backup" seam (U05):
//! a serde struct written through [`Storage::set_archive_meta`] and read
//! back with [`Storage::archive_meta`], where an absent key is an
//! explicit **not checked** answer rather than an error, and an
//! unreadable stored value degrades to the same answer (a malformed
//! record must never take the panel down; the next finished operation
//! overwrites it).
//!
//! - [`MediaVerificationSummary`] — media reconciliation is a live
//!   on-demand check; once it completes, its timestamp and coverage
//!   counts are persisted so the overview can say *when* the store was
//!   last verified and what that pass saw, across restarts.
//! - [`SourceImportRecord`] — job records carry no source identity, so
//!   each import source keeps its own last-import evidence: last
//!   attempt (any outcome), last success, and the counts of both. A
//!   failed or partial attempt updates the attempt fields and leaves
//!   the success fields untouched — a failed recent run can never
//!   overwrite the recorded time of the last success or read as a
//!   successful refresh.
//!
//! Because both records ride `vault.db`, a restore naturally replaces
//! them with the restored vault's own history, exactly like the backup
//! record.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::object_store::ReconcileReport;
use crate::storage::Storage;
use crate::{Error, Result};

/// The `archive_meta` key holding the persisted media verification
/// summary: when the store was last reconciled and what that pass saw.
pub const MEDIA_VERIFICATION_KEY: &str = "media_verification_summary";

/// The `archive_meta` key holding the per-import-source last-import
/// evidence map (source key → record, one entry per source).
pub const IMPORTS_KEY: &str = "import_source_last_imports";

// ---------------------------------------------------------------------------
// Media verification summary
// ---------------------------------------------------------------------------

/// The persisted outcome of one media-store reconciliation: when it ran
/// and its coverage counts. The counts are the [`ReconcileReport`]
/// tallies (findings are counted, not listed — the summary stays small
/// however damaged the store is; the live report remains the full
/// enumeration).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaVerificationSummary {
    /// RFC 3339 UTC — when the reconciliation finished.
    pub checked_at: String,
    /// Objects in the store (valid `<sha256>.<ext>` names).
    pub objects: u64,
    /// Distinct digests the archive references.
    pub referenced: u64,
    /// Referenced digests with no object in the store (external damage).
    pub missing: u64,
    /// Stored objects nothing references (harmless; later imports reuse).
    pub orphans: u64,
    /// Objects whose contents hash to something other than their name
    /// digest (only counted when the pass verified content).
    pub corrupt: u64,
    /// Staging-area leftovers from interrupted writes.
    pub stale_staging: u64,
    /// Foreign files in the store directory.
    pub foreign: u64,
}

impl MediaVerificationSummary {
    /// Tallies a finished reconciliation into its persisted summary.
    /// `checked_at` is the caller's finish time (RFC 3339 UTC).
    pub fn of_report(report: &ReconcileReport, checked_at: String) -> Self {
        Self {
            checked_at,
            objects: report.objects,
            referenced: report.referenced as u64,
            missing: report.missing.len() as u64,
            orphans: report.orphans.len() as u64,
            corrupt: report.corrupt.len() as u64,
            stale_staging: report.stale_staging.len() as u64,
            foreign: report.foreign.len() as u64,
        }
    }

    /// The checked store was exactly consistent with the references
    /// (mirrors [`ReconcileReport::is_clean`]).
    pub fn is_clean(&self) -> bool {
        self.objects == self.referenced
            && self.missing == 0
            && self.orphans == 0
            && self.corrupt == 0
            && self.stale_staging == 0
            && self.foreign == 0
    }
}

/// Persist a finished reconciliation's summary (overwrites any earlier
/// one — one last-verification, not a history).
pub fn record_media_verification(
    storage: &Storage,
    summary: &MediaVerificationSummary,
) -> Result<()> {
    let json = serde_json::to_string(summary).map_err(Error::Json)?;
    storage.set_archive_meta(MEDIA_VERIFICATION_KEY, &json)
}

/// Read the persisted media verification summary. `Ok(None)` when the
/// store was never reconciled — the explicit "not checked" answer — and,
/// deliberately, when the stored record is unreadable.
pub fn media_verification(storage: &Storage) -> Result<Option<MediaVerificationSummary>> {
    let Some(json) = storage.archive_meta(MEDIA_VERIFICATION_KEY)? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&json).ok())
}

// ---------------------------------------------------------------------------
// Per-import-source last-import evidence
// ---------------------------------------------------------------------------

/// The counts of one import run: `seen == inserted + skipped + failed`
/// (mirrors [`crate::ingest::IngestSummary`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ImportCounts {
    pub seen: u64,
    pub inserted: u64,
    pub skipped: u64,
    /// Items that failed to parse and were not stored (a completed run
    /// with `failed > 0` is a partial import — committed, but not every
    /// item made it).
    pub failed: u64,
}

impl ImportCounts {
    /// The counts of a finished [`crate::bounded::BoundedOutcome`]'s
    /// summary.
    pub fn of_summary(summary: &crate::ingest::IngestSummary) -> Self {
        Self {
            seen: summary.seen,
            inserted: summary.inserted,
            skipped: summary.skipped,
            failed: summary.failed,
        }
    }
}

/// The terminal outcome of one import attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ImportAttempt {
    /// The run drove the source to completion. `failed > 0` in the
    /// counts still means partial items, but the run itself finished.
    Succeeded { counts: ImportCounts },
    /// The run stopped early by cancellation: everything in `counts`
    /// was committed and is durable, the checkpoint keeps the resume
    /// position — not a success, never recorded as one.
    Cancelled { counts: ImportCounts },
    /// The run failed outright; the message is the job error text.
    Failed { error: String },
}

impl ImportAttempt {
    /// The run's counts when it got far enough to commit anything.
    pub fn counts(&self) -> Option<ImportCounts> {
        match self {
            ImportAttempt::Succeeded { counts } | ImportAttempt::Cancelled { counts } => {
                Some(*counts)
            }
            ImportAttempt::Failed { .. } => None,
        }
    }
}

/// The last run that drove a source to completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportSuccess {
    /// RFC 3339 UTC — when that run finished.
    pub at: String,
    pub counts: ImportCounts,
}

/// One import source's persisted last-import evidence. Last attempt and
/// last success are kept distinct: every finished attempt updates the
/// attempt side, only a completed run updates the success side, so a
/// failed or cancelled recent attempt can never overwrite the recorded
/// time of the last success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceImportRecord {
    /// The source key: the import's dedup namespace (the `source` of
    /// its [`crate::ingest::IngestSummary`]).
    pub source: String,
    /// RFC 3339 UTC — when the last attempt finished, any outcome.
    pub last_attempt_at: String,
    /// The last attempt's terminal outcome and counts (or error).
    pub last_attempt: ImportAttempt,
    /// The last completed run. `None` until one succeeds on this
    /// source; later failed/cancelled attempts leave it untouched.
    pub last_success: Option<ImportSuccess>,
}

/// Record one finished import attempt on a source. `at` is the
/// attempt's finish time (RFC 3339 UTC — the caller passes the job's
/// own finish stamp). A succeeded attempt also becomes the source's
/// last success; cancelled and failed attempts update only the attempt
/// fields, leaving any earlier success intact.
pub fn record_import_attempt(
    storage: &Storage,
    source: &str,
    at: &str,
    attempt: &ImportAttempt,
) -> Result<()> {
    let mut records = import_records_map(storage)?;
    let entry = records
        .entry(source.to_string())
        .or_insert_with(|| SourceImportRecord {
            source: source.to_string(),
            last_attempt_at: String::new(),
            last_attempt: ImportAttempt::Failed {
                error: String::new(),
            },
            last_success: None,
        });
    entry.last_attempt_at = at.to_string();
    entry.last_attempt = attempt.clone();
    if let ImportAttempt::Succeeded { counts } = attempt {
        entry.last_success = Some(ImportSuccess {
            at: at.to_string(),
            counts: *counts,
        });
    }
    let json = serde_json::to_string(&records).map_err(Error::Json)?;
    storage.set_archive_meta(IMPORTS_KEY, &json)
}

/// Every source's last-import evidence, sorted by source key. Empty —
/// the explicit "never imported" answer — when no import has finished
/// (or the stored map is unreadable).
pub fn import_source_records(storage: &Storage) -> Result<Vec<SourceImportRecord>> {
    Ok(import_records_map(storage)?.into_values().collect())
}

/// The stored evidence map; an absent or unreadable key reads as empty
/// (the next finished attempt overwrites the junk, mirroring the
/// tolerant backup-record read).
fn import_records_map(storage: &Storage) -> Result<BTreeMap<String, SourceImportRecord>> {
    let Some(json) = storage.archive_meta(IMPORTS_KEY)? else {
        return Ok(BTreeMap::new());
    };
    Ok(serde_json::from_str(&json).unwrap_or_default())
}

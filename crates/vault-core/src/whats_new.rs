//! The "what's new since your last visit" boot digest (the M7
//! follow-up): what changed in the vault while the app was closed or
//! hidden in the tray — new messages per source, plus finished or
//! failed imports and syncs.
//!
//! The last-visit record follows the persisted last-backup seam (see
//! [`crate::backup::LAST_BACKUP_KEY`] and [`crate::health`]): a small
//! serde record written through [`Storage::set_archive_meta`] and read
//! with [`Storage::archive_meta`], where an absent key is the explicit
//! first-visit answer and an unreadable stored value degrades to the
//! same quiet answer (a malformed record must never take the boot
//! surface down; the next read overwrites it). Because the record
//! rides `vault.db`, it is vault-scoped — switching or restoring a
//! vault never crosses digests.
//!
//! The derivation is read-only: [`read_digest`] derives what changed
//! since the stored visit; [`record_last_visit`] advances the cutoff.
//! The app layer calls the two through the pool's read connection and
//! write queue respectively (a long digest query must never queue
//! behind writes, and meta writes must never run on a read
//! connection); [`read_and_advance`] is the same pair as one call on a
//! single storage handle, which is what the tests use to prove that
//! one read advances the timestamp exactly once.
//!
//! Message counts mean **newly archived** messages: the query filters
//! on `archive_messages.created_at` (set once at insert), so a message
//! that re-imports and dedups keeps its original stamp and never
//! counts twice. Job rows are limited to the import and sync kinds
//! ([`DIGEST_JOB_KINDS`]) in terminal `succeeded`/`failed` states —
//! cancelled runs were the user's own action, and seed/index/backup
//! jobs are not archive-content changes the digest is about.

use chrono::{DateTime, Utc};

use serde::{Deserialize, Serialize};

use crate::stats::{WhatsNewDigest, WhatsNewJob};
use crate::storage::{JobLedgerRow, Storage, JOB_LEDGER_KEEP};
use crate::{Error, Result};

/// The `archive_meta` key holding the persisted last-visit record: when
/// the boot digest was last read (the cutoff the next digest counts
/// from). Vault-scoped by construction — the key lives in the vault's
/// own database.
pub const LAST_VISIT_KEY: &str = "whats_new_last_visit";

/// The job kinds the digest reports: every import kind plus the two
/// IMAP sync kinds. A kind not listed here is not an archive-content
/// change the boot surface speaks about.
pub const DIGEST_JOB_KINDS: [&str; 4] = [
    "import_mbox",
    "import_chat_export",
    "imap_sync",
    "imap_sync_all",
];

/// The persisted record of the last digest read: one RFC 3339 UTC
/// timestamp (overwritten on every read — one last-visit, not a
/// history).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastVisitRecord {
    /// RFC 3339 UTC — when the digest was last read.
    pub last_visit_at: String,
}

/// Convert an RFC 3339 instant into the `YYYY-MM-DD HH:MM:SS` UTC form
/// the `archive_messages.created_at` column stores (SQLite's
/// `datetime('now')` default), so the digest's since-comparison is an
/// apples-to-apples string compare the `created_at`-leading index
/// serves. Sub-second precision is floored to the column's whole
/// seconds; with the strictly-newer comparison that bounds the edge to
/// at most one skipped second around a visit, never a double count.
/// `None` when the text is not an RFC 3339 instant.
pub fn sqlite_instant(rfc3339: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(rfc3339)
        .ok()
        .map(|at| {
            at.with_timezone(&Utc)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
}

/// Read the persisted last-visit record. `Ok(None)` when the digest
/// was never read on this vault — the explicit first-visit answer —
/// and, deliberately, when the stored record is unreadable (mirroring
/// the tolerant backup/health record reads: junk degrades to a quiet
/// first visit, and the next read overwrites it).
pub fn last_visit(storage: &Storage) -> Result<Option<LastVisitRecord>> {
    let Some(json) = storage.archive_meta(LAST_VISIT_KEY)? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&json).ok())
}

/// Record the last visit (idempotent overwrite). `at` is the read's
/// own instant (RFC 3339 UTC).
pub fn record_last_visit(storage: &Storage, at: &str) -> Result<()> {
    let record = LastVisitRecord {
        last_visit_at: at.to_string(),
    };
    let json = serde_json::to_string(&record).map_err(Error::Json)?;
    storage.set_archive_meta(LAST_VISIT_KEY, &json)
}

/// Derive the digest of changes since the stored last visit, without
/// advancing anything. `now` is the caller's read instant (RFC 3339
/// UTC), echoed as the digest's `read_at`. A vault with no usable
/// last-visit record (never read, or the stored record is malformed)
/// answers an empty digest with `since: None` — a quiet first boot,
/// never a whole-archive dump.
pub fn read_digest(storage: &Storage, now: &str) -> Result<WhatsNewDigest> {
    let empty = || WhatsNewDigest {
        since: None,
        read_at: now.to_string(),
        new_messages: Vec::new(),
        jobs: Vec::new(),
    };
    let Some(record) = last_visit(storage)? else {
        return Ok(empty());
    };
    // The record parsed but its timestamp must also convert: one whose
    // instant cannot be resolved against the message column's format
    // reads as no usable visit, not an error on the boot path.
    let Some(cutoff) = sqlite_instant(&record.last_visit_at) else {
        return Ok(empty());
    };
    let new_messages = storage.messages_archived_since(&cutoff)?;
    let jobs = storage
        .job_ledger_since(&record.last_visit_at, JOB_LEDGER_KEEP as u32)?
        .into_iter()
        .filter(|row| row.state == "succeeded" || row.state == "failed")
        .filter(|row| DIGEST_JOB_KINDS.contains(&row.kind.as_str()))
        .map(WhatsNewJob::from)
        .collect();
    Ok(WhatsNewDigest {
        since: Some(record.last_visit_at),
        read_at: now.to_string(),
        new_messages,
        jobs,
    })
}

/// The one-call form: derive the digest and advance the last-visit
/// record to `now` — one read advances the cutoff exactly once, and
/// the next call counts only newer changes. This is the unit the app
/// layer's command performs across the pool (read connection for the
/// derivation, write queue for the advance); on a single storage
/// handle it is exact.
pub fn read_and_advance(storage: &Storage, now: &str) -> Result<WhatsNewDigest> {
    let digest = read_digest(storage, now)?;
    record_last_visit(storage, now)?;
    Ok(digest)
}

impl From<JobLedgerRow> for WhatsNewJob {
    fn from(row: JobLedgerRow) -> Self {
        Self {
            kind: row.kind,
            state: row.state,
            finished_at: row.finished_at,
            summary: row.summary,
        }
    }
}

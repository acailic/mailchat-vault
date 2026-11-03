//! Safe migration machinery (M1.4): pre-migration snapshot, capacity
//! preflight, post-conversion index validation, and the rollback/reopen
//! path.
//!
//! [`crate::storage::Storage::open`] routes every *existing* database
//! file with pending schema migrations through this module:
//!
//! 1. **Newer-schema refusal** — a database whose recorded schema version
//!    is newer than this build knows is refused before anything on disk
//!    is touched ([`Error::SchemaTooNew`]).
//! 2. **Capacity preflight** — before copying, the free space on the
//!    database's filesystem must cover the snapshot (main file plus any
//!    journal sidecars plus slack); a refusal leaves the file untouched
//!    ([`Error::InsufficientSpace`]).
//! 3. **Pre-migration snapshot** — the WAL is checkpointed and the file
//!    copied to `<db>.pre-migrate-v<from>.snapshot` (atomic
//!    temp-file-and-rename, fsynced), so the original archive stays
//!    recoverable even if the process dies mid-conversion.
//! 4. **Transactional conversion** — all pending migrations commit in a
//!    single SQLite transaction (all or nothing; see
//!    [`crate::storage`]'s `migrate`).
//! 5. **Validation after conversion** — `PRAGMA quick_check` plus FTS
//!    integrity and row-count parity over the canonical `archive_fts`
//!    (and the frozen legacy `messages_fts` when present).
//! 6. **Rollback/reopen** — any failure after the snapshot exists closes
//!    the database and restores the original file from the snapshot copy
//!    (which is kept), so the next open retries the conversion on the
//!    intact original. A validated conversion deletes the snapshot. When
//!    the *post-conversion validation* itself failed, a durable marker is
//!    also recorded (see [`failed_marker_path`]) so the archive is refused
//!    on every later open instead of re-running the same failing
//!    conversion forever.
//!
//! Failures can be injected at every stage
//! ([`MigrationFailPoint`]/[`OpenOptions`]) and the suite in
//! `tests/integrity/safe_migration.rs` proves the contract: a failure
//! before, within, or after migration never strands or modifies the
//! original archive.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::{Error, Result};

/// Slack on top of the copied bytes a snapshot needs: file-system
/// metadata for the temp copy, and the atomic rename.
const SNAPSHOT_SLACK_BYTES: u64 = 1024 * 1024;

/// Where the safe-migration path can inject a failure (test hook; the
/// production path always passes [`MigrationFailPoint::None`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MigrationFailPoint {
    /// No injected failure (the default).
    #[default]
    None,
    /// Before any snapshot work: nothing has touched the database yet.
    BeforeSnapshot,
    /// Mid snapshot copy: the temp file exists, the snapshot does not.
    DuringSnapshot,
    /// Inside the migration transaction, after every pending migration
    /// ran, before the commit.
    DuringMigration,
    /// After the conversion committed, during post-migration validation.
    AfterMigration,
}

impl MigrationFailPoint {
    fn label(self) -> &'static str {
        match self {
            MigrationFailPoint::None => "none",
            MigrationFailPoint::BeforeSnapshot => "before-snapshot",
            MigrationFailPoint::DuringSnapshot => "during-snapshot",
            MigrationFailPoint::DuringMigration => "during-migration",
            MigrationFailPoint::AfterMigration => "after-migration",
        }
    }
}

/// Knobs for [`crate::storage::Storage::open_with`]; the production
/// [`crate::storage::Storage::open`] uses the default (never fails
/// on purpose).
#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Inject a failure at the given point (test hook).
    pub fail_at: MigrationFailPoint,
    /// Force the capacity preflight to demand at least this many free
    /// bytes, regardless of the computed snapshot size (test hook for
    /// provoking [`Error::InsufficientSpace`] on any real disk).
    pub min_free_bytes: Option<u64>,
}

/// Fail when the options inject a failure at `point`.
pub(crate) fn inject(opts: &OpenOptions, point: MigrationFailPoint) -> Result<()> {
    if point != MigrationFailPoint::None && opts.fail_at == point {
        Err(Error::MigrationFault(point.label()))
    } else {
        Ok(())
    }
}

/// `<db>.pre-migrate-v<from_version>.snapshot` — the pre-migration
/// snapshot of a database being converted from `from_version`. Public so
/// tests and recovery tooling can locate the kept snapshot.
pub fn snapshot_path(db: &Path, from_version: i64) -> PathBuf {
    append_file_name(db, &format!(".pre-migrate-v{from_version}.snapshot"))
}

/// Remove a (validated, no longer needed) snapshot and its sidecar.
/// Best effort: a failure to delete never invalidates the converted
/// database.
pub fn remove_snapshot(snapshot: &Path) {
    let _ = fs::remove_file(snapshot);
    let _ = fs::remove_file(sidecar_path(snapshot, "-wal"));
}

/// `<db>.migration-failed` — the durable sidecar record of a failed
/// post-migration validation, written *after* the original was restored
/// from its snapshot. Fail-closed must terminate: every later
/// [`crate::storage::Storage::open`] reads this marker and refuses with a
/// clear error (the validation failure and the snapshot location) instead
/// of silently re-attempting the same deterministic failure forever. It
/// is a sidecar file, never a row in the archive: recording the failure
/// must not modify the restored original. Recovery is manual by design —
/// fix or replace the archive (e.g. from the kept snapshot), then delete
/// the marker.
pub fn failed_marker_path(db: &Path) -> PathBuf {
    append_file_name(db, ".migration-failed")
}

/// Write the failure marker atomically (temp file, fsync, rename), so a
/// crash mid-write can never leave a half marker.
pub(crate) fn write_failed_marker(db: &Path, reason: &str, snapshot: &Path) -> Result<()> {
    let marker = failed_marker_path(db);
    let tmp = append_file_name(&marker, ".tmp");
    let mut file = File::create(&tmp).map_err(Error::Io)?;
    write!(
        file,
        "reason: {reason}\nsnapshot: {}\n",
        snapshot.display()
    )
    .map_err(Error::Io)?;
    file.sync_all().map_err(Error::Io)?;
    fs::rename(&tmp, &marker).map_err(Error::Io)?;
    Ok(())
}

/// Read the failure marker: `Some((reason, snapshot_path))` when a
/// previous conversion failed its validation, `None` when there is none.
/// Lenient parsing: the reason falls back to the whole file content, the
/// snapshot pointer to empty, so even a truncated marker still refuses.
pub(crate) fn read_failed_marker(db: &Path) -> Result<Option<(String, String)>> {
    let content = match fs::read_to_string(failed_marker_path(db)) {
        Ok(content) => content,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut reason = content.trim().to_string();
    let mut snapshot = String::new();
    for line in content.lines() {
        if let Some(r) = line.strip_prefix("reason: ") {
            reason = r.to_string();
        } else if let Some(s) = line.strip_prefix("snapshot: ") {
            snapshot = s.to_string();
        }
    }
    Ok(Some((reason, snapshot)))
}

/// Take the pre-migration snapshot of `db` (currently at `from_version`),
/// returning its path. Order matters for the guarantees:
///
/// 1. size the copy and preflight free space — an insufficient-space
///    refusal must happen before *any* byte of the database is touched;
/// 2. checkpoint the WAL so the main file is self-contained;
/// 3. copy to a temp file, fsync, then atomically rename into place.
pub(crate) fn take_snapshot(
    conn: &Connection,
    db: &Path,
    from_version: i64,
    opts: &OpenOptions,
) -> Result<PathBuf> {
    inject(opts, MigrationFailPoint::BeforeSnapshot)?;

    // 1. Preflight: the snapshot needs the main file, any journal
    //    sidecars, and slack — measured before anything is written.
    let needed = file_len(db)?
        + sidecar_len(db, "-wal")?
        + sidecar_len(db, "-journal")?
        + SNAPSHOT_SLACK_BYTES;
    let needed = opts
        .min_free_bytes
        .map_or(needed, |floor| needed.max(floor));
    let dir = db.parent().filter(|p| !p.as_os_str().is_empty());
    let available = fs4::available_space(dir.unwrap_or(Path::new(".")))
        .map_err(|e| Error::UnsafeMigration(format!("cannot measure free space: {e}")))?;
    if available < needed {
        return Err(Error::InsufficientSpace { needed, available });
    }

    // 2. Fold any WAL content into the main file so the copy alone is a
    //    complete, openable database. (On a rollback-journal database
    //    this is a no-op.)
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;

    // 3. Copy, fsync, rename. The temp file keeps a partial copy from
    //    ever being mistaken for the snapshot.
    let snapshot = snapshot_path(db, from_version);
    let tmp = append_file_name(&snapshot, ".tmp");
    copy_synced(db, &tmp)?;
    if inject(opts, MigrationFailPoint::DuringSnapshot).is_err() {
        let _ = fs::remove_file(&tmp);
        return Err(Error::MigrationFault(
            MigrationFailPoint::DuringSnapshot.label(),
        ));
    }
    fs::rename(&tmp, &snapshot).map_err(Error::Io)?;

    // A single-connection TRUNCATE checkpoint leaves the WAL empty, but
    // capture a non-empty one anyway — restoring without it could lose
    // committed frames.
    if sidecar_len(db, "-wal")? > 0 {
        copy_synced(&sidecar_path(db, "-wal"), &sidecar_path(&snapshot, "-wal"))?;
    }
    Ok(snapshot)
}

/// Restore the database file at `db` from its pre-migration `snapshot`.
/// The caller must have closed every connection to `db` first. The
/// snapshot itself is kept — it is the reopen path of last resort.
pub(crate) fn restore_from_snapshot(db: &Path, snapshot: &Path) -> Result<()> {
    // Stale journals belong to the post-conversion state, not the
    // restored one.
    for suffix in ["-wal", "-shm", "-journal"] {
        let _ = fs::remove_file(sidecar_path(db, suffix));
    }
    let tmp = append_file_name(db, ".restore-tmp");
    copy_synced(snapshot, &tmp)?;
    fs::rename(&tmp, db).map_err(Error::Io)?;
    if sidecar_len(snapshot, "-wal")? > 0 {
        copy_synced(&sidecar_path(snapshot, "-wal"), &sidecar_path(db, "-wal"))?;
    }
    Ok(())
}

/// Validate a freshly converted database: whole-file integrity, and every
/// content-linked FTS index in agreement with its content table — by
/// document-count parity over the FTS shadow tables (`_docsize` holds one
/// row per indexed document; `COUNT(*)` on the FTS table itself reads the
/// content table and would hide drift) and by the FTS5 `integrity-check`
/// command (internal segment consistency; in external-content mode SQLite
/// does not cross-check tokens against the content table, so the count
/// parity above is what catches a lost or partial index).
pub(crate) fn validate_post_migration(conn: &Connection) -> Result<()> {
    let verdict: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|e| Error::UnsafeMigration(format!("integrity check failed to run: {e}")))?;
    if verdict != "ok" {
        return Err(Error::UnsafeMigration(format!(
            "integrity check failed after migration: {verdict}"
        )));
    }
    validate_fts(conn, "archive_fts", "archive_messages")?;
    // The legacy FTS pair is frozen (nothing writes it since schema
    // version 4); when both halves are present they must still agree.
    if table_exists(conn, "messages")? && table_exists(conn, "messages_fts")? {
        validate_fts(conn, "messages_fts", "messages")?;
    }
    Ok(())
}

fn validate_fts(conn: &Connection, fts: &str, table: &str) -> Result<()> {
    let mismatch = |index_n: i64, table_n: i64| {
        Error::UnsafeMigration(format!(
            "full-text index {fts} holds {index_n} documents but its content table {table} holds {table_n}"
        ))
    };
    let index_n: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM {fts}_docsize"), [], |r| {
            r.get(0)
        })
        .map_err(Error::Sqlite)?;
    let table_n: i64 = conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .map_err(Error::Sqlite)?;
    if index_n != table_n {
        return Err(mismatch(index_n, table_n));
    }
    conn.execute(
        &format!("INSERT INTO {fts} ({fts}) VALUES ('integrity-check')"),
        [],
    )
    .map_err(|e| {
        Error::UnsafeMigration(format!(
            "full-text index {fts} failed its integrity-check: {e}"
        ))
    })?;
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )?;
    Ok(exists)
}

/// Copy `src` to `dst` and fsync it, so a rename over `dst` afterwards
/// publishes a complete file even across a power loss.
fn copy_synced(src: &Path, dst: &Path) -> Result<()> {
    let mut reader = File::open(src).map_err(Error::Io)?;
    let mut writer = File::create(dst).map_err(Error::Io)?;
    io::copy(&mut reader, &mut writer).map_err(Error::Io)?;
    writer.sync_all().map_err(Error::Io)?;
    Ok(())
}

fn file_len(path: &Path) -> Result<u64> {
    Ok(fs::metadata(path).map_err(Error::Io)?.len())
}

fn sidecar_len(db: &Path, suffix: &str) -> Result<u64> {
    let sidecar = sidecar_path(db, suffix);
    if sidecar.try_exists().map_err(Error::Io)? {
        file_len(&sidecar)
    } else {
        Ok(0)
    }
}

fn sidecar_path(db: &Path, suffix: &str) -> PathBuf {
    append_file_name(db, suffix)
}

fn append_file_name(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

//! M4.4: the versioned backup container and the staged, journaled
//! restore.
//!
//! # The container
//!
//! A backup is a versioned ZIP64 archive (`.mcbackup`) holding, in one
//! self-contained file:
//!
//! - `vault.db` — a **consistent SQLite snapshot** taken with SQLite's
//!   backup API ([`rusqlite::backup`]); copying only a live `vault.db`
//!   is unsafe under WAL, so the snapshot is made *through* a
//!   connection, never by reading the file. The snapshot's referenced
//!   digests — not the live database's — decide which objects belong in
//!   the backup, so the archive is an exact point-in-time vault.
//! - `settings.json` — present exactly when the vault has one (the
//!   settings file never contains secrets; keys live in the OS keyring
//!   and are never backed up).
//! - `media/<sha256>.<ext>` — one entry per media object the snapshot
//!   references (message media entries plus the M4.2 unlinked-file
//!   registry).
//! - `manifest.json` — written **last**, after every entry's bytes have
//!   been hashed while streaming into the archive: format and schema
//!   versions, source counts, and every entry's relative path, size,
//!   and SHA-256 checksum.
//!
//! Media entries are stored uncompressed (they are already-compressed
//! images/video/audio — re-deflating them wastes CPU for nothing); the
//! database, settings, and manifest are deflated. Creation streams
//! everything in bounded chunks, verifies the finished archive by
//! re-reading and re-hashing every entry against the manifest, and only
//! then renames the temporary file into place. A failure at any point
//! removes only the incomplete backup — the vault is never touched.
//!
//! # The restore
//!
//! Restoring never replaces the active vault directly:
//!
//! 1. **stage** ([`stage_restore`]) — the archive is validated and
//!    extracted into a separate staging vault under the recovery
//!    directory: manifest versions supported (newer formats are
//!    refused), every entry checksummed against the manifest, no
//!    unexpected or missing entries, the staged database opened
//!    read-only for `integrity_check` + `foreign_key_check`, the
//!    settings file version-checked, and every digest the staged
//!    database references present in the manifest. A staging record
//!    binds the preview id to the manifest and to the active vault's
//!    [`VaultFingerprint`].
//! 2. **commit** ([`commit_restore`]) — re-verifies the staged bytes
//!    against the manifest (the staged archive cannot have changed
//!    since the preview), re-reads the live vault's fingerprint (a
//!    vault that changed since the preview is a refusal, not a
//!    surprise switch), then replaces the vault payload through a
//!    **two-phase switch journal**:
//!    - journal `switching`: every current payload item
//!      (`vault.db`, `settings.json`, `media/`) is moved into a fresh
//!      previous-vault slot under the recovery directory;
//!    - journal `staged`: the staged items are moved into the active
//!      vault;
//!    - journal `completed`.
//!
//!    A crash during either phase is recovered deterministically by
//!    [`recover_interrupted_switch`]: `switching` rolls **back** to the
//!    intact previous vault, `staged` rolls **forward** to the new one
//!    — an interrupted switch always ends at one verified whole vault,
//!    never a mix. The previous vault is kept (never auto-deleted);
//!    [`return_to_previous_vault`] switches back to it through the same
//!    journaled process, and [`recovery_forget`] removes one slot only
//!    on explicit request.
//!
//! Credentials are never restored: the keyring is not part of any
//! backup or switch, so restored sources require reconnection before
//! scheduled access can run (the restore outcome reports the source
//! count so the UI can say so).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::object_store::ObjectStore;
use crate::storage::{
    media_digests_on, read_schema_version, vault_fingerprint_on, SourceInstance, VaultFingerprint,
    SUPPORTED_SCHEMA_VERSION,
};
use crate::{Error, Result};

/// The backup container format version this build writes. A future
/// incompatible change bumps it and documents the migration/reading
/// path; restore refuses archives with a higher version.
pub const BACKUP_FORMAT_VERSION: u32 = 1;

/// The file extension for backup containers (`vault-2026-11-27.mcbackup`).
pub const BACKUP_EXTENSION: &str = "mcbackup";

/// The manifest entry name.
const MANIFEST_ENTRY: &str = "manifest.json";
/// The database snapshot entry name.
const DB_ENTRY: &str = "vault.db";
/// The settings entry name (present exactly when the vault has one).
const SETTINGS_ENTRY: &str = "settings.json";
/// The switch journal file inside the recovery directory.
const JOURNAL_FILE: &str = "switch-journal.json";
/// The staging area (validation + extraction target) inside recovery.
const STAGING_DIR: &str = "staging";
/// Prefix of every previous-vault slot directory.
const PREVIOUS_PREFIX: &str = "previous-";
/// The staging preview record.
const PREVIEW_FILE: &str = "preview.json";
/// The staged manifest copy (commit re-reads and re-hashes it).
const STAGED_MANIFEST: &str = "manifest.json";

/// Free-space slack on top of measured bytes (filesystem metadata,
/// atomic temp files and renames).
const SLACK_BYTES: u64 = 4 * 1024 * 1024;

/// The vault payload items a switch moves, in journal order.
const SWITCH_ITEMS: [&str; 3] = [DB_ENTRY, SETTINGS_ENTRY, "media"];

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// One archived entry: its container-relative path, size in bytes, and
/// SHA-256 checksum of its contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// The counts a backup records about the vault it captured (the restore
/// UI shows them; tests assert the round-trip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BackupCounts {
    pub messages: u64,
    pub sources: u64,
    pub media_objects: u64,
}

impl BackupCounts {
    fn of(conn: &Connection) -> Result<Self> {
        let fingerprint = vault_fingerprint_on(conn)?;
        Ok(Self {
            messages: fingerprint.messages,
            sources: fingerprint.sources,
            media_objects: fingerprint.media_objects,
        })
    }
}

/// The versioned manifest of one backup container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    /// The container format version ([`BACKUP_FORMAT_VERSION`]).
    pub format_version: u32,
    /// The schema version of the archived `vault.db` (restore refuses a
    /// newer schema, exactly like opening one).
    pub schema_version: i64,
    /// When the backup was created (RFC 3339 UTC).
    pub created_at: String,
    /// Counts of the captured vault.
    pub counts: BackupCounts,
    /// The database snapshot entry.
    pub db: BackupEntry,
    /// The settings entry, when the vault had a settings file.
    pub settings: Option<BackupEntry>,
    /// Every referenced media object, sorted by digest.
    pub objects: Vec<BackupEntry>,
}

impl BackupManifest {
    /// Every entry the container must hold (the manifest excluded).
    fn content_entries(&self) -> Vec<&BackupEntry> {
        let mut entries = vec![&self.db];
        if let Some(settings) = &self.settings {
            entries.push(settings);
        }
        entries.extend(self.objects.iter());
        entries
    }

    /// The media digests the manifest carries.
    fn carried_digests(&self) -> HashSet<String> {
        self.objects.iter().map(|o| o.sha256.clone()).collect()
    }
}

/// What [`create_backup`] reports back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupSummary {
    /// The finished archive path.
    pub path: PathBuf,
    /// The archive's total size in bytes.
    pub size: u64,
    /// The manifest embedded in it.
    pub manifest: BackupManifest,
}

// ---------------------------------------------------------------------------
// Persisted "last verified backup" (U05)
// ---------------------------------------------------------------------------

/// The `archive_meta` key holding the persisted last-backup record
/// (U05): the Storage & backup panel reads it at boot so the vault's
/// safety state survives restarts instead of living only in one
/// session's job-event stream.
pub const LAST_BACKUP_KEY: &str = "last_verified_backup";

/// The persisted record of the last backup a `backup_create` run
/// verified: written into the vault's own `archive_meta` after the
/// container finished verifying (every entry re-hashed) and was
/// renamed into place. Because the record lives in `vault.db`, a
/// restore naturally replaces it — the restored vault reports the
/// backup state it carried at snapshot time, never the replaced
/// vault's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastBackupRecord {
    /// RFC 3339 UTC — when the container finished verifying.
    pub created_at: String,
    /// The destination the container was written to.
    pub path: PathBuf,
    /// The container's size in bytes.
    pub size: u64,
    /// The counts of the vault the snapshot captured.
    pub counts: BackupCounts,
    /// The snapshot's schema version (a future restore refuses newer).
    pub schema_version: i64,
}

/// Record a finished, verified backup in the vault's `archive_meta`
/// (overwrites any earlier record — one last-backup, not a history).
pub fn record_last_backup(
    storage: &crate::storage::Storage,
    summary: &BackupSummary,
) -> Result<()> {
    let record = LastBackupRecord {
        created_at: summary.manifest.created_at.clone(),
        path: summary.path.clone(),
        size: summary.size,
        counts: summary.manifest.counts,
        schema_version: summary.manifest.schema_version,
    };
    let json = serde_json::to_string(&record).map_err(Error::Json)?;
    storage
        .set_archive_meta(LAST_BACKUP_KEY, &json)
        .map_err(|e| bad(format!("cannot record the finished backup: {e}")))
}

/// Read the persisted last-backup record. `Ok(None)` when this vault
/// was never backed up, and — deliberately — when the stored record is
/// unreadable: a malformed record must not take the panel down, and
/// the next finished backup overwrites it.
pub fn last_backup_record(storage: &crate::storage::Storage) -> Result<Option<LastBackupRecord>> {
    let Some(json) = storage
        .archive_meta(LAST_BACKUP_KEY)
        .map_err(|e| bad(format!("cannot read the last-backup record: {e}")))?
    else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&json).ok())
}

// ---------------------------------------------------------------------------
// Progress and options
// ---------------------------------------------------------------------------

/// One progress note: a phase label plus a position (`done` of `total`,
/// `total` 0 when unknown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgressNote {
    pub phase: &'static str,
    pub done: u64,
    pub total: u64,
}

/// Where a backup run can inject a failure (test hook; production always
/// passes [`BackupFailPoint::None`]). Every point proves a boundary: an
/// interrupted backup leaves no destination file and no vault damage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackupFailPoint {
    #[default]
    None,
    /// After preflight, before the snapshot exists.
    BeforeSnapshot,
    /// After the i-th object was written into the container.
    DuringObjectCopy(usize),
    /// After the verified container exists, before it is renamed into
    /// place (the destination must not exist).
    BeforeRename,
}

/// Knobs for [`create_backup`].
#[derive(Debug, Clone, Default)]
pub struct BackupOptions {
    /// Inject a failure at the given point (test hook).
    pub fail_at: BackupFailPoint,
    /// Force the capacity preflight to demand at least this many free
    /// bytes (test hook for provoking [`Error::InsufficientSpace`] on
    /// any real disk).
    pub min_free_bytes: Option<u64>,
}

/// Where a restore can inject a failure (test hook). Each boundary
/// leaves a state [`recover_interrupted_switch`] must resolve into one
/// verified vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestoreFailPoint {
    #[default]
    None,
    /// After validation, with the staging vault built but nothing
    /// switched (no journal yet).
    AfterStagedValidate,
    /// With the journal at `switching`, after the i-th payload item was
    /// moved into the previous-vault slot.
    DuringMoveToPrevious(usize),
    /// With every item moved to the previous slot but the journal still
    /// `switching` (the `staged` mark not yet written).
    AfterMovesToPrevious,
    /// With the journal at `staged`, after the i-th payload item was
    /// moved into the active vault.
    DuringMoveToActive(usize),
    /// With every item in place but the journal still `staged`.
    BeforeCompleted,
}

/// Knobs for [`stage_restore`] / [`commit_restore`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RestoreOptions {
    /// Inject a failure at the given point (test hook).
    pub fail_at: RestoreFailPoint,
    /// Force the capacity preflight to demand at least this many free
    /// bytes (test hook).
    pub min_free_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// Recovery layout
// ---------------------------------------------------------------------------

/// The sibling recovery directory holding the switch journal, the
/// staging vault, and previous-vault slots — deliberately outside the
/// vault root so no ordinary vault operation (object cleanup, imports)
/// can touch recovery data.
pub fn recovery_dir(data_dir: &Path) -> PathBuf {
    let mut suffix = data_dir
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("vault"));
    suffix.push("-recovery");
    data_dir.with_file_name(suffix)
}

/// The staging vault directory (inside the recovery directory).
fn staging_dir(data_dir: &Path) -> PathBuf {
    recovery_dir(data_dir).join(STAGING_DIR)
}

/// The switch journal path.
fn journal_path(data_dir: &Path) -> PathBuf {
    recovery_dir(data_dir).join(JOURNAL_FILE)
}

/// The state of one journaled switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchState {
    /// Phase A: moving the current vault into the previous slot. A crash
    /// here rolls **back**.
    Switching,
    /// Phase A done; phase B: moving the staged vault into place. A
    /// crash here rolls **forward**.
    Staged,
    /// The switch finished; the previous slot is kept for recovery.
    Completed,
}

/// What one journal records. Everything needed to resolve an
/// interrupted switch without guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwitchJournal {
    pub version: u32,
    pub state: SwitchState,
    /// RFC 3339 UTC timestamp of when the switch started.
    pub created_at: String,
    /// The archive being restored, when this switch is a restore.
    pub archive: Option<PathBuf>,
    /// The previous-vault slot (absolute).
    pub previous: PathBuf,
    /// Where switched-in items come from: the staging vault (restore)
    /// or a previous-vault slot (return to previous).
    pub source: PathBuf,
}

// ---------------------------------------------------------------------------
// Backup creation
// ---------------------------------------------------------------------------

/// The plan a [`snapshot_for_backup`] produces: everything the
/// container writer needs, derived **from the snapshot** (not the live
/// database) so the archive is an exact point-in-time vault. Consumed
/// by [`write_backup_container`].
#[derive(Debug, Clone)]
pub struct BackupPlan {
    /// The snapshot file (already fsynced; a complete, self-contained
    /// database).
    pub snapshot: PathBuf,
    pub schema_version: i64,
    pub counts: BackupCounts,
    /// The objects to carry: store-relative path, size, digest (the
    /// digest is the content hash by construction).
    pub objects: Vec<BackupEntry>,
    /// Whether the vault has a settings file to carry.
    pub has_settings: bool,
}

/// Take the consistent snapshot and derive the backup plan. Runs on
/// the vault's **writer** connection (through the pool's write queue at
/// the app layer): the SQLite backup API copies the committed pages
/// into a fresh file with new writes paused behind the queue — a
/// consistent snapshot even though the live file sits in WAL mode.
/// Referenced digests are read from the snapshot, so writes that land
/// after the snapshot can never add content the container would miss.
///
/// `snapshot` is the temp file to write (next to the eventual
/// destination, so the container write stays on one filesystem); the
/// caller hands both to [`write_backup_container`].
pub fn snapshot_for_backup(
    data_dir: &Path,
    conn: &Connection,
    snapshot: &Path,
) -> Result<BackupPlan> {
    let db_path = data_dir.join(DB_ENTRY);
    if !db_path.is_file() {
        return Err(bad(format!("no vault database at {}", db_path.display())));
    }
    snapshot_database(conn, snapshot)?;
    let snapshot_conn =
        Connection::open_with_flags(snapshot, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let schema_version = read_schema_version(&snapshot_conn)?;
    let counts = BackupCounts::of(&snapshot_conn)?;
    let digests = media_digests_on(&snapshot_conn)?;
    drop(snapshot_conn);
    // The read-only probe may leave WAL bookkeeping sidecars next to
    // the snapshot; they belong to nobody once it is closed.
    remove_journal_sidecars(snapshot);

    let store = ObjectStore::new(data_dir);
    let media_dir = data_dir.join("media");
    let mut objects = Vec::with_capacity(digests.len());
    for digest in &digests {
        let Some(name) = store.object_name(digest) else {
            return Err(bad(format!(
                "the archive references media object {digest} but the store does not hold it \
                 (run the media-store reconciliation before backing up)"
            )));
        };
        let path = media_dir.join(&name);
        let size = fs::metadata(&path).map_err(Error::Io)?.len();
        objects.push(BackupEntry {
            path: format!("media/{name}"),
            size,
            sha256: digest.clone(),
        });
    }
    objects.sort_by(|a, b| a.sha256.cmp(&b.sha256));
    Ok(BackupPlan {
        snapshot: snapshot.to_path_buf(),
        schema_version,
        counts,
        objects,
        has_settings: data_dir.join(crate::settings::SETTINGS_FILE).is_file(),
    })
}

/// Stream the container described by `plan` to `dest` (the snapshot
/// temp is consumed afterwards). No database connection is held:
/// progress and cancellation are honored between entries, and a
/// failure leaves **no** destination file (only cleaned-up
/// temporaries) and never touches the vault.
///
/// The destination must not lie inside the vault root or the recovery
/// directory (a backup must not be moved by a later switch), and its
/// filesystem must have room for the snapshot plus the uncompressed
/// container plus slack.
pub fn write_backup_container(
    data_dir: &Path,
    plan: &BackupPlan,
    dest: &Path,
    progress: &dyn Fn(ProgressNote),
    cancel: &dyn Fn() -> bool,
    opts: &BackupOptions,
) -> Result<BackupSummary> {
    // Destination fence: outside the vault root and the recovery
    // directory, with an existing parent.
    let dest_parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            bad(format!(
                "backup destination has no directory: {}",
                dest.display()
            ))
        })?;
    let file_name = dest.file_name().ok_or_else(|| {
        bad(format!(
            "backup destination is a directory: {}",
            dest.display()
        ))
    })?;
    let canonical_dest = dest_parent
        .canonicalize()
        .map_err(Error::Io)?
        .join(file_name);
    let vault_root = data_dir.canonicalize().map_err(Error::Io)?;
    if canonical_dest.starts_with(&vault_root) {
        return Err(bad(format!(
            "backup destination {} is inside the vault root {}",
            canonical_dest.display(),
            vault_root.display()
        )));
    }
    let recovery = recovery_dir(data_dir);
    if recovery.exists() {
        let canonical_recovery = recovery.canonicalize().map_err(Error::Io)?;
        if canonical_dest.starts_with(&canonical_recovery) {
            return Err(bad(format!(
                "backup destination {} is inside the recovery directory {}",
                canonical_dest.display(),
                canonical_recovery.display()
            )));
        }
    }

    // Preflight: the destination filesystem must cover the snapshot
    // (written there), the container (media stored uncompressed), and
    // slack — measured before anything is written.
    let snapshot_len = fs::metadata(&plan.snapshot).map_err(Error::Io)?.len();
    let store_bytes: u64 = plan.objects.iter().map(|o| o.size).sum();
    let needed = snapshot_len
        .saturating_mul(2)
        .saturating_add(store_bytes)
        .saturating_add(SLACK_BYTES);
    let needed = opts
        .min_free_bytes
        .map_or(needed, |floor| needed.max(floor));
    let available = fs4::available_space(dest_parent).map_err(|e| {
        bad(format!(
            "cannot measure free space at {}: {e}",
            dest_parent.display()
        ))
    })?;
    if available < needed {
        return Err(Error::InsufficientSpace { needed, available });
    }

    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "backup".to_string());
    let container_tmp = dest_parent.join(format!(".{stem}-{}.part", std::process::id()));
    let _container_guard = TempPath(&container_tmp);
    let zip_file = File::create(&container_tmp).map_err(Error::Io)?;
    let mut zip = ZipWriter::new(zip_file);
    let total_entries = plan.objects.len() as u64 + 1 + u64::from(plan.has_settings);

    progress(ProgressNote {
        phase: "container",
        done: 0,
        total: total_entries,
    });
    let streamed = write_entry(
        &mut zip,
        DB_ENTRY,
        &mut File::open(&plan.snapshot).map_err(Error::Io)?,
        true,
    )?;
    let db_entry = BackupEntry {
        path: DB_ENTRY.to_string(),
        size: streamed.size,
        sha256: streamed.sha256,
    };
    progress(ProgressNote {
        phase: "container",
        done: 1,
        total: total_entries,
    });

    let mut settings_entry = None;
    if plan.has_settings {
        let streamed = write_entry(
            &mut zip,
            SETTINGS_ENTRY,
            &mut File::open(data_dir.join(crate::settings::SETTINGS_FILE)).map_err(Error::Io)?,
            true,
        )?;
        settings_entry = Some(BackupEntry {
            path: SETTINGS_ENTRY.to_string(),
            size: streamed.size,
            sha256: streamed.sha256,
        });
        progress(ProgressNote {
            phase: "container",
            done: 2,
            total: total_entries,
        });
    }

    for (i, object) in plan.objects.iter().enumerate() {
        if cancel() {
            return Err(cancelled());
        }
        let path = data_dir.join(&object.path);
        let streamed = write_entry(
            &mut zip,
            &object.path,
            &mut File::open(&path).map_err(Error::Io)?,
            false,
        )?;
        // The object's content hash is its name digest by construction;
        // the streamed hash must agree or the store is inconsistent.
        if streamed.sha256 != object.sha256 || streamed.size != object.size {
            return Err(bad(format!(
                "media object {} changed while backing up (store is inconsistent)",
                object.path
            )));
        }
        progress(ProgressNote {
            phase: "container",
            done: i as u64 + 2 + u64::from(plan.has_settings),
            total: total_entries,
        });
        if opts.fail_at == BackupFailPoint::DuringObjectCopy(i) {
            return Err(fault("backup failure injected during object copy"));
        }
    }

    let manifest = BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        schema_version: plan.schema_version,
        created_at: now_rfc3339(),
        counts: plan.counts,
        db: db_entry,
        settings: settings_entry,
        objects: plan.objects.clone(),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(Error::Json)?;
    zip.start_file(MANIFEST_ENTRY, FileOptions::<()>::default())
        .map_err(zip_error)?;
    zip.write_all(&manifest_bytes).map_err(Error::Io)?;
    zip.finish().map_err(zip_error)?;
    fsync_file(&container_tmp)?;

    // Verify the finished archive by re-reading every entry and
    // re-hashing it against the manifest, then publish by rename.
    verify_container(&container_tmp, &manifest, progress)?;
    if opts.fail_at == BackupFailPoint::BeforeRename {
        return Err(fault("backup failure injected before rename"));
    }
    fs::rename(&container_tmp, dest).map_err(Error::Io)?;
    let size = fs::metadata(dest).map_err(Error::Io)?.len();
    Ok(BackupSummary {
        path: dest.to_path_buf(),
        size,
        manifest,
    })
}

/// Create a versioned backup in one call (snapshot + container), for
/// tools and tests; the app layer drives the two halves separately so
/// the long copy runs on the job runner with progress and cancellation
/// (see [`snapshot_for_backup`] / [`write_backup_container`]).
pub fn create_backup(
    data_dir: &Path,
    conn: &Connection,
    dest: &Path,
    progress: &dyn Fn(ProgressNote),
    cancel: &dyn Fn() -> bool,
    opts: &BackupOptions,
) -> Result<BackupSummary> {
    if opts.fail_at == BackupFailPoint::BeforeSnapshot {
        return Err(fault("backup failure injected before snapshot"));
    }
    let dest_parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "backup".to_string());
    let snapshot_tmp = dest_parent.join(format!(".{stem}-snapshot-{}", std::process::id()));
    let _snapshot_guard = TempPath(&snapshot_tmp);
    progress(ProgressNote {
        phase: "snapshot",
        done: 0,
        total: 0,
    });
    let plan = snapshot_for_backup(data_dir, conn, &snapshot_tmp)?;
    let summary = write_backup_container(data_dir, &plan, dest, progress, cancel, opts)?;
    // The snapshot's job is done once the container is verified.
    discard_snapshot(&snapshot_tmp);
    Ok(summary)
}

/// Snapshot a live database into `dest` using SQLite's backup API.
/// The destination must not exist (a fresh, empty target is required).
fn snapshot_database(conn: &Connection, dest: &Path) -> Result<()> {
    let mut target = Connection::open(dest)?;
    {
        let backup = rusqlite::backup::Backup::new(conn, &mut target)
            .map_err(|e| bad(format!("cannot start the database snapshot: {e}")))?;
        backup
            .run_to_completion(64, std::time::Duration::from_millis(1), None)
            .map_err(|e| bad(format!("database snapshot failed: {e}")))?;
    }
    target.close().map_err(|(_, e)| Error::from(e))?;
    fsync_file(dest)?;
    Ok(())
}

/// The streamed result of one container entry write: size and SHA-256.
struct StreamedEntry {
    size: u64,
    sha256: String,
}

/// Stream one file into the container under `name`, hashing while
/// writing. `compressed` picks deflation (database, settings, manifest);
/// media entries are stored byte-exact without a second compression
/// pass (they are already compressed content).
fn write_entry(
    zip: &mut ZipWriter<File>,
    name: &str,
    reader: &mut dyn Read,
    compressed: bool,
) -> Result<StreamedEntry> {
    let mut options = FileOptions::<()>::default().large_file(true);
    if compressed {
        options = options.compression_method(CompressionMethod::Deflated);
    } else {
        options = options.compression_method(CompressionMethod::Stored);
    }
    zip.start_file(name, options).map_err(zip_error)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut size: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(Error::Io)?;
        if n == 0 {
            break;
        }
        zip.write_all(&buf[..n]).map_err(Error::Io)?;
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok(StreamedEntry {
        size,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

// ---------------------------------------------------------------------------
// Container verification (shared by verify and staging)
// ---------------------------------------------------------------------------

/// Read the manifest out of a container without hashing every entry.
pub fn read_manifest(archive: &Path) -> Result<BackupManifest> {
    let file = File::open(archive).map_err(Error::Io)?;
    let mut zip =
        ZipArchive::new(file).map_err(|e| bad(format!("not a readable backup container: {e}")))?;
    read_manifest_from(&mut zip)
}

/// Read and structurally validate the manifest of an open container.
fn read_manifest_from(zip: &mut ZipArchive<File>) -> Result<BackupManifest> {
    let mut entry = zip
        .by_name(MANIFEST_ENTRY)
        .map_err(|_| bad("the container has no manifest.json — not a vault backup"))?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).map_err(Error::Io)?;
    drop(entry);
    let manifest: BackupManifest =
        serde_json::from_slice(&bytes).map_err(|e| bad(format!("unreadable manifest: {e}")))?;
    validate_manifest_shape(&manifest)?;
    Ok(manifest)
}

/// Structural validation of a manifest: supported format and schema
/// versions, correct entry names, well-formed object digests.
fn validate_manifest_shape(manifest: &BackupManifest) -> Result<()> {
    if manifest.format_version > BACKUP_FORMAT_VERSION {
        return Err(Error::SchemaTooNew {
            db: manifest.format_version as i64,
            supported: BACKUP_FORMAT_VERSION as i64,
        });
    }
    if manifest.schema_version > SUPPORTED_SCHEMA_VERSION {
        return Err(Error::SchemaTooNew {
            db: manifest.schema_version,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    if manifest.db.path != DB_ENTRY {
        return Err(bad(format!(
            "manifest names the database entry {} (expected {DB_ENTRY})",
            manifest.db.path
        )));
    }
    if let Some(settings) = &manifest.settings {
        if settings.path != SETTINGS_ENTRY {
            return Err(bad(format!(
                "manifest names the settings entry {} (expected {SETTINGS_ENTRY})",
                settings.path
            )));
        }
    }
    let mut seen = HashSet::new();
    for object in &manifest.objects {
        if !is_media_object_path(&object.path) {
            return Err(bad(format!(
                "manifest lists a non-object entry {} — refusing",
                object.path
            )));
        }
        let digest = object_digest_of(&object.path).expect("checked by is_media_object_path");
        if object.sha256 != digest {
            return Err(bad(format!(
                "manifest entry {} disagrees with its own digest",
                object.path
            )));
        }
        if !seen.insert(digest) {
            return Err(bad(format!("manifest lists {} twice", object.path)));
        }
    }
    Ok(())
}

/// Whether `path` is exactly one media object name (`media/<64-hex>.<ext>`).
fn is_media_object_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("media/") else {
        return false;
    };
    if rest.contains('/') || rest.contains('\\') {
        return false;
    }
    let Some((stem, ext)) = rest.rsplit_once('.') else {
        return false;
    };
    !ext.is_empty()
        && stem.len() == 64
        && stem.chars().all(|c| c.is_ascii_hexdigit())
        && crate::object_store::sanitize_ext(ext) == ext.to_ascii_lowercase()
}

/// The digest of a media object path (`media/<digest>.<ext>`).
fn object_digest_of(path: &str) -> Option<String> {
    let rest = path.strip_prefix("media/")?;
    let (stem, _) = rest.rsplit_once('.')?;
    Some(stem.to_ascii_lowercase())
}

/// Fully verify a container: manifest shape, exact entry set, and every
/// entry re-hashed against its recorded checksum and size.
fn verify_container(
    archive: &Path,
    manifest: &BackupManifest,
    progress: &dyn Fn(ProgressNote),
) -> Result<()> {
    let file = File::open(archive).map_err(Error::Io)?;
    let mut zip =
        ZipArchive::new(file).map_err(|e| bad(format!("not a readable backup container: {e}")))?;
    let expected: std::collections::HashMap<String, &BackupEntry> = manifest
        .content_entries()
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();
    let total = expected.len() as u64 + 1; // + the manifest itself
    if zip.len() as u64 != total {
        return Err(bad(format!(
            "the container holds {} entries but the manifest describes {total}",
            zip.len()
        )));
    }
    let mut verified = HashSet::new();
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| bad(format!("container entry {i} unreadable: {e}")))?;
        let name = entry
            .name()
            .map_err(|e| bad(format!("container entry {i} has an unusable name: {e}")))?
            .to_string();
        if name == MANIFEST_ENTRY {
            verified.insert(name);
            continue;
        }
        let Some(expected_entry) = expected.get(&name) else {
            return Err(bad(format!(
                "container holds an entry the manifest does not describe: {name}"
            )));
        };
        let (sha256, size) = hash_reader(&mut entry)?;
        if sha256 != expected_entry.sha256 || size != expected_entry.size {
            return Err(bad(format!(
                "container entry {name} fails its checksum — the backup is damaged"
            )));
        }
        verified.insert(name);
        progress(ProgressNote {
            phase: "verify",
            done: verified.len() as u64,
            total,
        });
    }
    if verified.len() as u64 != total {
        let missing: Vec<&str> = expected
            .keys()
            .filter(|k| !verified.contains(*k))
            .map(|s| s.as_str())
            .collect();
        return Err(bad(format!(
            "the container is missing manifest entries: {}",
            missing.join(", ")
        )));
    }
    Ok(())
}

/// Verify a backup archive end to end (manifest shape + every entry
/// checksum). Read-only; never touches any vault.
pub fn verify_backup(archive: &Path, progress: &dyn Fn(ProgressNote)) -> Result<BackupManifest> {
    // Read the manifest first (shape validation), then hash everything.
    let manifest = read_manifest(archive)?;
    verify_container(archive, &manifest, progress)?;
    Ok(manifest)
}

// ---------------------------------------------------------------------------
// Staged restore
// ---------------------------------------------------------------------------

/// The staged, validated result of [`stage_restore`] — what the UI
/// shows before a commit is allowed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestorePreview {
    /// The staging record id ([`commit_restore`] requires it back).
    pub preview_id: String,
    /// The archive that was staged.
    pub archive: PathBuf,
    /// The staged manifest's identity (versions, counts, created_at).
    pub manifest: BackupManifest,
    /// Whether a settings file is part of the staged restore.
    pub restored_settings: bool,
    /// The media objects the staged restore carries.
    pub objects: u64,
}

/// The on-disk staging record binding the preview id to the staged
/// bytes and the vault fingerprint the preview was validated against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PreviewRecord {
    preview_id: String,
    created_at: String,
    archive: PathBuf,
    /// SHA-256 of the staged manifest bytes (commit re-checks it).
    manifest_sha256: String,
    /// The active vault's fingerprint when the preview ran.
    active_fingerprint: VaultFingerprint,
    /// The staged vault's fingerprint.
    staged_fingerprint: VaultFingerprint,
}

/// Stage and validate a restore: verify the archive, extract it into
/// the staging vault, and validate the staged database (SQLite
/// integrity, foreign keys, schema ceiling, settings version, object
/// presence). Nothing in the active vault is touched; on any failure
/// the staging area is left clean.
///
/// `active_fingerprint` is the live vault's fingerprint right now (the
/// app reads it through the pool); [`commit_restore`] refuses to switch
/// when the vault changed since.
pub fn stage_restore(
    data_dir: &Path,
    archive: &Path,
    active_fingerprint: &VaultFingerprint,
    progress: &dyn Fn(ProgressNote),
    opts: &RestoreOptions,
) -> Result<RestorePreview> {
    let archive = archive.canonicalize().map_err(Error::Io)?;
    if !archive.is_file() {
        return Err(bad(format!("no backup container at {}", archive.display())));
    }
    // The archive must not live inside the vault being restored into.
    let vault_root = data_dir.canonicalize().map_err(Error::Io)?;
    if archive.starts_with(&vault_root) {
        return Err(bad(format!(
            "the backup container {} is inside the vault it would replace",
            archive.display()
        )));
    }
    // An unresolved switch must be recovered before anything stages.
    ensure_no_open_switch(data_dir)?;
    let recovery = recovery_dir(data_dir);
    fs::create_dir_all(&recovery).map_err(Error::Io)?;
    let staging = staging_dir(data_dir);
    // A fresh staging area every time: an older preview's bytes never
    // mix with a new one's.
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(Error::Io)?;
    }
    fs::create_dir_all(staging.join("media")).map_err(Error::Io)?;

    // Preflight: room for the uncompressed payload plus slack on the
    // recovery filesystem.
    let file = File::open(&archive).map_err(Error::Io)?;
    let mut zip =
        ZipArchive::new(file).map_err(|e| bad(format!("not a readable backup container: {e}")))?;
    let manifest = read_manifest_from(&mut zip)?;
    let payload: u64 = manifest
        .content_entries()
        .iter()
        .map(|e| e.size)
        .sum::<u64>()
        .saturating_add(SLACK_BYTES);
    let needed = opts
        .min_free_bytes
        .map_or(payload, |floor| payload.max(floor));
    let available = fs4::available_space(&recovery).map_err(|e| {
        bad(format!(
            "cannot measure free space at {}: {e}",
            recovery.display()
        ))
    })?;
    if available < needed {
        return Err(Error::InsufficientSpace { needed, available });
    }

    // Extract + checksum every entry into the staging vault; the entry
    // set must be exactly the manifest's.
    let expected: std::collections::HashMap<String, &BackupEntry> = manifest
        .content_entries()
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();
    let total = expected.len() as u64 + 1;
    let mut extracted = HashSet::new();
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| bad(format!("container entry {i} unreadable: {e}")))?;
        let name = entry
            .name()
            .map_err(|e| bad(format!("container entry {i} has an unusable name: {e}")))?
            .to_string();
        if name == MANIFEST_ENTRY {
            extracted.insert(name);
            continue;
        }
        let Some(expected_entry) = expected.get(&name) else {
            return Err(bad(format!(
                "container holds an entry the manifest does not describe: {name}"
            )));
        };
        // The extraction path comes only from the validated manifest
        // name; the zip's own name is never used to build a path.
        let target = staging.join(&name);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(Error::Io)?;
        }
        let mut out = File::create(&target).map_err(Error::Io)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut size: u64 = 0;
        loop {
            let n = entry.read(&mut buf).map_err(Error::Io)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(Error::Io)?;
            hasher.update(&buf[..n]);
            size += n as u64;
            // Bounded expansion (the M4.1 invariant): a crafted entry
            // cannot grow past its manifest-declared size and fill the
            // staging disk before any later comparison fires — the
            // declared size is the preflight's budget, so it is the
            // extraction's budget too.
            if size > expected_entry.size {
                drop(out);
                fs::remove_file(&target).ok();
                return Err(bad(format!(
                    "container entry {name} expands past its manifest size — the backup is damaged"
                )));
            }
        }
        out.sync_all().map_err(Error::Io)?;
        drop(out);
        let sha256 = format!("{:x}", hasher.finalize());
        if sha256 != expected_entry.sha256 || size != expected_entry.size {
            fs::remove_file(&target).ok();
            return Err(bad(format!(
                "container entry {name} fails its checksum — the backup is damaged"
            )));
        }
        extracted.insert(name);
        progress(ProgressNote {
            phase: "stage",
            done: extracted.len() as u64,
            total,
        });
    }
    for name in expected.keys() {
        if !extracted.contains(name) {
            return Err(bad(format!("the container is missing the entry {name}")));
        }
    }

    // Validate the staged database: openable read-only, whole-file
    // integrity, no foreign-key violations, supported schema, and
    // every referenced digest present in the manifest.
    validate_staged_database(&staging.join(DB_ENTRY), &manifest)?;
    let staged_conn = open_read_only(&staging.join(DB_ENTRY))?;
    let staged_fingerprint = vault_fingerprint_on(&staged_conn)?;
    drop(staged_conn);
    if staged_fingerprint.messages != manifest.counts.messages
        || staged_fingerprint.sources != manifest.counts.sources
        || staged_fingerprint.media_objects != manifest.counts.media_objects
    {
        return Err(bad(
            "the staged database's row counts disagree with the manifest — refusing",
        ));
    }
    // Settings version gate: a settings file this build cannot read is
    // a refusal, not a silent default.
    let staged_settings = staging.join(SETTINGS_ENTRY);
    let restored_settings = staged_settings.is_file();
    if restored_settings {
        crate::settings::Settings::load(&staged_settings)?;
    }

    // Keep the validated manifest with the staging bytes and write the
    // staging record that binds the preview id to both.
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(Error::Json)?;
    write_bytes_synced(&staging.join(STAGED_MANIFEST), &manifest_bytes)?;
    let record = PreviewRecord {
        preview_id: new_preview_id(),
        created_at: now_rfc3339(),
        archive: archive.clone(),
        manifest_sha256: format!("{:x}", Sha256::digest(&manifest_bytes)),
        active_fingerprint: *active_fingerprint,
        staged_fingerprint,
    };
    write_json_atomic(&staging.join(PREVIEW_FILE), &record)?;

    if opts.fail_at == RestoreFailPoint::AfterStagedValidate {
        return Err(fault("restore failure injected after staging validation"));
    }
    Ok(RestorePreview {
        preview_id: record.preview_id,
        archive,
        restored_settings,
        objects: manifest.objects.len() as u64,
        manifest,
    })
}

/// Open a staged database read-only and run the SQLite-level
/// validations: whole-file integrity, foreign-key check, schema
/// ceiling, manifest/schema agreement, and object presence (every
/// referenced digest carried by the manifest).
fn validate_staged_database(db: &Path, manifest: &BackupManifest) -> Result<()> {
    let conn = open_read_only(db)?;
    let verdict: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| {
            bad(format!(
                "staged database integrity check failed to run: {e}"
            ))
        })?;
    if verdict != "ok" {
        return Err(bad(format!(
            "the staged database fails its integrity check: {verdict}"
        )));
    }
    let fk_violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .map_err(|e| {
            bad(format!(
                "staged database foreign-key check failed to run: {e}"
            ))
        })?;
    if fk_violations > 0 {
        return Err(bad(format!(
            "the staged database reports {fk_violations} foreign-key violations"
        )));
    }
    let schema = read_schema_version(&conn)?;
    if schema > SUPPORTED_SCHEMA_VERSION {
        return Err(Error::SchemaTooNew {
            db: schema,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    if schema != manifest.schema_version {
        return Err(bad(format!(
            "the staged database records schema {schema} but its manifest says {}",
            manifest.schema_version
        )));
    }
    let digests = media_digests_on(&conn)?;
    let carried = manifest.carried_digests();
    let missing: Vec<&String> = digests.iter().filter(|d| !carried.contains(*d)).collect();
    if !missing.is_empty() {
        return Err(bad(format!(
            "the staged database references {} media object(s) the backup does not carry \
             (first: {})",
            missing.len(),
            missing[0]
        )));
    }
    Ok(())
}

fn open_read_only(db: &Path) -> Result<Connection> {
    Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| bad(format!("cannot open the staged database: {e}")))
}

// ---------------------------------------------------------------------------
// Commit (the journaled switch)
// ---------------------------------------------------------------------------

/// What [`commit_restore`] / [`return_to_previous_vault`] report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreOutcome {
    /// The staged manifest's identity (versions, counts, created_at).
    pub manifest: BackupManifest,
    /// The id of the previous-vault slot holding the vault that was
    /// replaced (kept; see [`recovery_status`]).
    pub previous_slot: String,
    /// Whether a settings file was restored.
    pub restored_settings: bool,
    /// Restored sources the caller's credential-presence check flags
    /// (the app checks the OS keyring's `user@host` entries): a
    /// same-machine restore whose keyring still holds every account
    /// reports 0; credentials are never part of a backup, so only the
    /// accounts missing from *this* machine count.
    pub sources_needing_reauth: u64,
}

/// Commit a staged restore: re-verify the staged bytes, refuse a vault
/// that changed since the preview, then switch through the journal.
/// `needs_reauth` decides, per restored source, whether its credentials
/// are missing on this machine (the app passes a keyring lookup) — the
/// outcome counts exactly those sources, never the manifest's total.
///
/// The caller must have stopped every writer and closed the pool before
/// this runs — the switch moves `vault.db` itself. Every refusal this
/// function can raise *before* the switch is also available read-only
/// through [`verify_committable`], so a caller that drains its backend
/// can refuse while everything is still running.
pub fn commit_restore(
    data_dir: &Path,
    preview_id: &str,
    current_fingerprint: &VaultFingerprint,
    progress: &dyn Fn(ProgressNote),
    opts: &RestoreOptions,
    needs_reauth: &dyn Fn(&SourceInstance) -> bool,
) -> Result<RestoreOutcome> {
    let staging = staging_dir(data_dir);
    let (record, manifest) = precheck_commit(
        data_dir,
        &staging,
        preview_id,
        current_fingerprint,
        progress,
    )?;

    let previous_slot = new_previous_slot(data_dir)?;
    write_journal(data_dir, |journal| {
        *journal = Some(SwitchJournal {
            version: 1,
            state: SwitchState::Switching,
            created_at: now_rfc3339(),
            archive: Some(record.archive.clone()),
            previous: previous_slot.clone(),
            source: staging.clone(),
        })
    })?;
    run_switch(
        data_dir,
        &previous_slot,
        &staging,
        &manifest,
        progress,
        opts,
        needs_reauth,
    )
}

/// Every refusal a [`commit_restore`] can raise before the switch
/// starts, checked read-only against the still-active vault. Callers
/// whose backend must stay usable (the app drains its pool around the
/// switch) run this first: a refusal surfaces while every writer is
/// still live, so nothing is left stopped behind a "usable" UI.
/// [`commit_restore`] re-runs the identical checks — a switch only
/// ever publishes re-verified bytes.
pub fn verify_committable(
    data_dir: &Path,
    preview_id: &str,
    current_fingerprint: &VaultFingerprint,
    progress: &dyn Fn(ProgressNote),
) -> Result<()> {
    let staging = staging_dir(data_dir);
    precheck_commit(
        data_dir,
        &staging,
        preview_id,
        current_fingerprint,
        progress,
    )
    .map(|_| ())
}

/// The shared pre-switch refusal checks of [`commit_restore`]: the
/// staged preview must still be the validated one, the staged bytes
/// must still hash to their manifest, the active vault must be the
/// fingerprint the preview validated, and the staged database must be
/// the one that was validated.
fn precheck_commit(
    data_dir: &Path,
    staging: &Path,
    preview_id: &str,
    current_fingerprint: &VaultFingerprint,
    progress: &dyn Fn(ProgressNote),
) -> Result<(PreviewRecord, BackupManifest)> {
    let record: PreviewRecord = read_json(&staging.join(PREVIEW_FILE)).map_err(|e| {
        bad(format!(
            "no staged restore to commit (run the validation first): {e}"
        ))
    })?;
    if record.preview_id != preview_id {
        return Err(bad(
            "the staged restore was replaced by a newer preview — validate again and commit that one",
        ));
    }
    // An unresolved switch must be recovered before committing a new one.
    ensure_no_open_switch(data_dir)?;
    // The staged bytes must still be exactly what was validated.
    let manifest: BackupManifest = read_json(&staging.join(STAGED_MANIFEST)).map_err(|e| {
        bad(format!(
            "the staged manifest is unreadable — restage the restore: {e}"
        ))
    })?;
    let manifest_bytes = fs::read(staging.join(STAGED_MANIFEST)).map_err(Error::Io)?;
    let manifest_sha = format!("{:x}", Sha256::digest(&manifest_bytes));
    if manifest_sha != record.manifest_sha256 {
        return Err(bad(
            "the staged restore changed since its validation — restage it before committing",
        ));
    }
    reverify_staged(staging, &manifest, progress)?;
    // The active vault must be the one the preview validated.
    if record.active_fingerprint != *current_fingerprint {
        return Err(bad(
            "the vault changed since the restore was validated — restage the restore and try again",
        ));
    }
    let staged_conn = open_read_only(&staging.join(DB_ENTRY))?;
    let staged_fingerprint = vault_fingerprint_on(&staged_conn)?;
    drop(staged_conn);
    if staged_fingerprint != record.staged_fingerprint {
        return Err(bad(
            "the staged database changed since validation — refusing to switch",
        ));
    }
    Ok((record, manifest))
}

/// Refuse while a switch is unresolved (a journal that is not
/// `completed`): recover it first.
fn ensure_no_open_switch(data_dir: &Path) -> Result<()> {
    if let Some(journal) = read_journal(data_dir)? {
        if journal.state != SwitchState::Completed {
            return Err(bad(
                "a vault switch is unresolved — restart the app (or run recovery) before \
                 staging or committing another",
            ));
        }
    }
    Ok(())
}

/// Re-hash the staged payload against its manifest (the "no unverified
/// replacement" gate: a switch only ever publishes re-verified bytes).
fn reverify_staged(
    staging: &Path,
    manifest: &BackupManifest,
    progress: &dyn Fn(ProgressNote),
) -> Result<()> {
    let entries = manifest.content_entries();
    let total = entries.len() as u64;
    for (i, entry) in entries.iter().enumerate() {
        let path = staging.join(&entry.path);
        let (sha256, size) = hash_file(&path)?;
        if sha256 != entry.sha256 || size != entry.size {
            return Err(bad(format!(
                "staged entry {} fails its checksum — the staging area changed; restage the restore",
                entry.path
            )));
        }
        progress(ProgressNote {
            phase: "reverify",
            done: i as u64 + 1,
            total,
        });
    }
    Ok(())
}

/// Switch the active vault to the staged/source payload through the
/// two-phase journal. Shared by restore commit and return-to-previous.
fn run_switch(
    data_dir: &Path,
    previous_slot: &Path,
    source: &Path,
    manifest: &BackupManifest,
    progress: &dyn Fn(ProgressNote),
    opts: &RestoreOptions,
    needs_reauth: &dyn Fn(&SourceInstance) -> bool,
) -> Result<RestoreOutcome> {
    // Phase A: active → previous (journal `switching`; a crash rolls back).
    for (i, item) in SWITCH_ITEMS.iter().enumerate() {
        move_switch_item(data_dir, previous_slot, item)?;
        if opts.fail_at == RestoreFailPoint::DuringMoveToPrevious(i) {
            return Err(fault(
                "restore failure injected while moving the vault aside",
            ));
        }
    }
    fsync_dir(previous_slot)?;
    if opts.fail_at == RestoreFailPoint::AfterMovesToPrevious {
        return Err(fault(
            "restore failure injected after moving the vault aside",
        ));
    }
    write_journal(data_dir, |journal| {
        if let Some(journal) = journal.as_mut() {
            journal.state = SwitchState::Staged;
        }
    })?;

    // Phase B: source → active (journal `staged`; a crash rolls forward).
    for (i, item) in SWITCH_ITEMS.iter().enumerate() {
        move_switch_item(source, data_dir, item)?;
        if opts.fail_at == RestoreFailPoint::DuringMoveToActive(i) {
            return Err(fault(
                "restore failure injected while switching the new vault in",
            ));
        }
    }
    fsync_dir(data_dir)?;
    // The switch consumed its source: if anything removed the source
    // mid-switch (a slot Forget racing this return), the active vault
    // would be silently missing — refuse loudly instead. The journal
    // stays `staged`; restart recovery re-resolves it (or reports the
    // failure) rather than completing a vault-less switch.
    if !data_dir.join(DB_ENTRY).is_file() {
        return Err(bad(
            "the switch source vanished mid-switch — the active vault is missing; \
             restart the app to resolve the switch",
        ));
    }
    if opts.fail_at == RestoreFailPoint::BeforeCompleted {
        return Err(fault(
            "restore failure injected before completing the switch",
        ));
    }
    write_journal(data_dir, |journal| {
        if let Some(journal) = journal.as_mut() {
            journal.state = SwitchState::Completed;
        }
    })?;
    // The source (staging or the consumed previous slot) is now empty;
    // keep the recovery tree tidy but never touch the previous slot.
    fs::remove_dir_all(source).ok();
    fsync_dir(&recovery_dir(data_dir))?;

    progress(ProgressNote {
        phase: "done",
        done: 1,
        total: 1,
    });
    // Reauth accounting reads the *restored* vault's own source list
    // against the caller's credential-presence check (the OS keyring):
    // a same-machine restore whose keyring still holds every user@host
    // reports 0. The switch is already complete here — accounting must
    // never fail it, so an unreadable fresh vault falls back to the
    // manifest's (conservative) source count.
    let sources_needing_reauth =
        restored_sources_needing_reauth(data_dir, needs_reauth).unwrap_or(manifest.counts.sources);
    Ok(RestoreOutcome {
        manifest: manifest.clone(),
        previous_slot: previous_slot
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        restored_settings: data_dir.join(SETTINGS_ENTRY).is_file(),
        sources_needing_reauth,
    })
}

/// Count the restored vault's sources the caller flags as needing
/// reconnection — credentials absent on this machine. Reads the fresh
/// active database read-only; `None` means it could not be opened.
fn restored_sources_needing_reauth(
    data_dir: &Path,
    needs_reauth: &dyn Fn(&SourceInstance) -> bool,
) -> Option<u64> {
    let conn = open_read_only(&data_dir.join(DB_ENTRY)).ok()?;
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, label, connector_key, config_json, state, excluded, \
                     last_attempt_at, last_success_at, created_at, \
                     last_error_class, last_error, last_error_at \
             FROM source_instances",
        )
        .ok()?;
    let instances = stmt
        .query_map([], restored_source_instance)
        .ok()?
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    Some(instances.iter().filter(|i| needs_reauth(i)).count() as u64)
}

/// One `source_instances` row as a [`SourceInstance`] (read-only; the
/// storage layer's own mapper lives behind its write paths).
fn restored_source_instance(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceInstance> {
    Ok(SourceInstance {
        id: row.get(0)?,
        kind: row.get(1)?,
        label: row.get(2)?,
        connector_key: row.get(3)?,
        config_json: row.get(4)?,
        state: row.get(5)?,
        excluded: row.get::<_, i64>(6)? != 0,
        last_attempt_at: row.get(7)?,
        last_success_at: row.get(8)?,
        created_at: row.get(9)?,
        last_error_class: row.get(10)?,
        last_error: row.get(11)?,
        last_error_at: row.get(12)?,
    })
}

/// Move one payload item (`vault.db` — with its journal sidecars —
/// `settings.json`, or `media/`) from `from_dir` to `to_dir`. A missing
/// item is skipped (settings and media the source does not carry); a
/// leftover at the target is replaced, never merged into.
fn move_switch_item(from_dir: &Path, to_dir: &Path, item: &str) -> Result<()> {
    move_path(&from_dir.join(item), &to_dir.join(item))?;
    if item == DB_ENTRY {
        // WAL/journal sidecars travel with the database so committed
        // frames are never orphaned.
        for suffix in ["-wal", "-shm", "-journal"] {
            move_path(
                &from_dir.join(format!("{DB_ENTRY}{suffix}")),
                &to_dir.join(format!("{DB_ENTRY}{suffix}")),
            )?;
        }
    }
    Ok(())
}

fn move_path(from: &Path, to: &Path) -> Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    if to.exists() {
        if to.is_dir() {
            fs::remove_dir_all(to).map_err(Error::Io)?;
        } else {
            fs::remove_file(to).map_err(Error::Io)?;
        }
    }
    fs::rename(from, to).map_err(Error::Io)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// One-shot restore (stage + commit) — used by tests and tools
// ---------------------------------------------------------------------------

/// Stage and commit a restore in one call (each phase with its own
/// validation). The caller must have stopped every writer first.
pub fn restore_backup(
    data_dir: &Path,
    archive: &Path,
    active_fingerprint: &VaultFingerprint,
    progress: &dyn Fn(ProgressNote),
    opts: &RestoreOptions,
    needs_reauth: &dyn Fn(&SourceInstance) -> bool,
) -> Result<RestoreOutcome> {
    let preview = stage_restore(data_dir, archive, active_fingerprint, progress, opts)?;
    commit_restore(
        data_dir,
        &preview.preview_id,
        active_fingerprint,
        progress,
        opts,
        needs_reauth,
    )
}

// ---------------------------------------------------------------------------
// Return to a previous vault
// ---------------------------------------------------------------------------

/// Switch the active vault back to a recorded previous-vault slot,
/// through the same journaled process (the vault being replaced becomes
/// a new previous slot; no archives are merged).
///
/// `slot_id` is the exact directory name under the recovery directory
/// (see [`recovery_status`]). The caller must have stopped every writer
/// first, and must confirm with the user — this replaces the current
/// vault. `needs_reauth` is the per-source credential-presence check
/// (see [`commit_restore`]). Every refusal this can raise before the
/// switch is also available read-only through [`verify_returnable`].
pub fn return_to_previous_vault(
    data_dir: &Path,
    slot_id: &str,
    progress: &dyn Fn(ProgressNote),
    needs_reauth: &dyn Fn(&SourceInstance) -> bool,
) -> Result<RestoreOutcome> {
    let (slot, schema, counts) = validate_previous_slot(data_dir, slot_id)?;
    let manifest = BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        schema_version: schema,
        created_at: now_rfc3339(),
        counts,
        db: BackupEntry {
            path: DB_ENTRY.to_string(),
            size: 0,
            sha256: String::new(),
        },
        settings: None,
        objects: Vec::new(),
    };

    let previous_slot = new_previous_slot(data_dir)?;
    write_journal(data_dir, |journal| {
        *journal = Some(SwitchJournal {
            version: 1,
            state: SwitchState::Switching,
            created_at: now_rfc3339(),
            archive: None,
            previous: previous_slot.clone(),
            source: slot.clone(),
        })
    })?;
    run_switch(
        data_dir,
        &previous_slot,
        &slot,
        &manifest,
        progress,
        &RestoreOptions::default(),
        needs_reauth,
    )
}

/// Every refusal a [`return_to_previous_vault`] can raise before the
/// switch starts, checked read-only — the drained-backend precheck
/// counterpart of [`verify_committable`].
pub fn verify_returnable(data_dir: &Path, slot_id: &str) -> Result<()> {
    validate_previous_slot(data_dir, slot_id).map(|_| ())
}

/// Validate a previous-vault slot exactly as the switch would: openable,
/// integral, its schema supported, and every referenced media object
/// present. Returns the slot path, its schema version, and its counts.
fn validate_previous_slot(data_dir: &Path, slot_id: &str) -> Result<(PathBuf, i64, BackupCounts)> {
    if !is_slot_id(slot_id) {
        return Err(bad(format!("not a previous-vault id: {slot_id}")));
    }
    ensure_no_open_switch(data_dir)?;
    let slot = recovery_dir(data_dir).join(slot_id);
    if !slot.is_dir() {
        return Err(bad(format!("no previous vault recorded as {slot_id}")));
    }
    // Validate what is about to become active: openable, integral, its
    // schema supported, and every referenced media object present.
    if !slot.join(DB_ENTRY).is_file() {
        return Err(bad(format!(
            "previous vault {slot_id} holds no database — it cannot be switched back in"
        )));
    }
    let conn = open_read_only(&slot.join(DB_ENTRY))?;
    let verdict: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| {
            bad(format!(
                "previous vault {slot_id} failed its integrity check: {e}"
            ))
        })?;
    if verdict != "ok" {
        return Err(bad(format!(
            "previous vault {slot_id} fails its integrity check: {verdict}"
        )));
    }
    let schema = read_schema_version(&conn)?;
    if schema > SUPPORTED_SCHEMA_VERSION {
        return Err(Error::SchemaTooNew {
            db: schema,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    let digests = media_digests_on(&conn)?;
    let counts = BackupCounts::of(&conn)?;
    drop(conn);
    let media_dir = slot.join("media");
    for digest in &digests {
        let present = fs::read_dir(&media_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .any(|e| e.file_name().to_string_lossy().starts_with(digest.as_str()))
            })
            .unwrap_or(false);
        if !present {
            return Err(bad(format!(
                "previous vault {slot_id} references media object {digest} it does not hold"
            )));
        }
    }
    Ok((slot, schema, counts))
}

// ---------------------------------------------------------------------------
// Switch recovery
// ---------------------------------------------------------------------------

/// What [`recover_interrupted_switch`] found and did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwitchRecovery {
    /// No journal exists: nothing was in flight.
    None,
    /// The last switch completed; its previous slot is kept.
    AlreadyCompleted { previous_slot: Option<String> },
    /// An interrupted switch was rolled **back**: the active vault is
    /// the pre-switch one, verified.
    RolledBack {
        previous_slot: String,
        /// The `created_at` of the journal this outcome resolved —
        /// carried out so callers never re-read the journal to learn
        /// which one was resolved (a re-read can fail and silently
        /// degrade the U05 origin line).
        journal_created_at: String,
    },
    /// An interrupted switch was rolled **forward**: the active vault
    /// is the switched-in one, verified.
    RolledForward {
        previous_slot: String,
        /// See [`SwitchRecovery::RolledBack`].
        journal_created_at: String,
    },
}

/// Resolve an interrupted switch at startup (before any connection is
/// opened). `switching` rolls back to the intact previous vault;
/// `staged` rolls forward to the switched-in one. Either way the
/// journal is marked completed and the resulting vault verified.
pub fn recover_interrupted_switch(data_dir: &Path) -> Result<SwitchRecovery> {
    let Some(journal) = read_journal(data_dir)? else {
        return Ok(SwitchRecovery::None);
    };
    let slot_id = || {
        journal
            .previous
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    match journal.state {
        SwitchState::Completed => {
            // The previous slot may since have been forgotten.
            let kept = journal.previous.is_dir().then(slot_id);
            Ok(SwitchRecovery::AlreadyCompleted {
                previous_slot: kept,
            })
        }
        SwitchState::Switching => {
            // Roll back: whatever reached the previous slot returns.
            for item in SWITCH_ITEMS {
                move_switch_item(&journal.previous, data_dir, item)?;
            }
            write_journal(data_dir, |j| {
                if let Some(j) = j.as_mut() {
                    j.state = SwitchState::Completed;
                }
            })?;
            // Phase B never ran, so the source is intact: for a return
            // switch it is a recorded previous vault (only
            // `recovery_forget` deletes slots), for a restore the
            // staging area the next stage_restore re-wipes.
            fs::remove_dir_all(&journal.previous).ok();
            fsync_dir(data_dir)?;
            verify_active_vault(data_dir)?;
            Ok(SwitchRecovery::RolledBack {
                previous_slot: slot_id(),
                journal_created_at: journal.created_at.clone(),
            })
        }
        SwitchState::Staged => {
            // Roll forward: whatever the source still holds arrives.
            for item in SWITCH_ITEMS {
                move_switch_item(&journal.source, data_dir, item)?;
            }
            write_journal(data_dir, |j| {
                if let Some(j) = j.as_mut() {
                    j.state = SwitchState::Completed;
                }
            })?;
            fs::remove_dir_all(&journal.source).ok();
            fsync_dir(data_dir)?;
            verify_active_vault(data_dir)?;
            Ok(SwitchRecovery::RolledForward {
                previous_slot: slot_id(),
                journal_created_at: journal.created_at.clone(),
            })
        }
    }
}

/// Quick verification that the active vault's database is openable and
/// integral (used after switch recovery).
fn verify_active_vault(data_dir: &Path) -> Result<()> {
    let db = data_dir.join(DB_ENTRY);
    if !db.is_file() {
        return Err(bad(format!(
            "switch recovery left no active database at {}",
            db.display()
        )));
    }
    let conn = open_read_only(&db)?;
    let verdict: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| bad(format!("recovered database fails its integrity check: {e}")))?;
    if verdict != "ok" {
        return Err(bad(format!(
            "recovered database fails its integrity check: {verdict}"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Recovery status
// ---------------------------------------------------------------------------

/// One recorded previous-vault slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousVaultSlot {
    /// The exact slot id (what [`return_to_previous_vault`] and
    /// [`recovery_forget`] take).
    pub id: String,
    /// RFC 3339 UTC timestamp encoded in the slot name.
    pub created_at: String,
    /// Total bytes held by the slot.
    pub total_bytes: u64,
    /// Number of files in the slot.
    pub files: u64,
}

/// The recovery state of a vault: the journal, staging presence, and
/// the recorded previous-vault slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryStatus {
    /// `None` when no journal exists (no switch ever ran).
    pub journal_state: Option<SwitchState>,
    /// When the recorded journal was created.
    pub journal_created_at: Option<String>,
    /// Whether a staged (validated, uncommitted) restore exists.
    pub staged_preview: bool,
    /// Previous-vault slots, newest first.
    pub previous: Vec<PreviousVaultSlot>,
    /// What the most recent *completed* switch switched in from — the
    /// "what is active now, since when, from where" of the U05 restart
    /// UX. `None` when no switch ever ran, or while the journal is
    /// unresolved (startup recovery resolves it before the app opens
    /// the vault). The journal alone cannot say whether this session's
    /// startup recovery rolled the switch back — the app layer merges
    /// that in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_switch: Option<LastSwitchOrigin>,
}

/// How the active vault came to be: the origin of the most recent
/// completed switch, read from its journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSwitchOrigin {
    /// Whether the vault switched in from a backup container
    /// (a restore) or a recorded previous-vault slot (a return).
    pub kind: SwitchOriginKind,
    /// The restore's container path (a return has no archive).
    pub archive: Option<PathBuf>,
    /// RFC 3339 UTC — when the switch started.
    pub created_at: String,
}

/// Which kind of source a completed switch switched the vault in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchOriginKind {
    /// A validated restore from a backup container (`archive` set).
    Restored,
    /// A return to a recorded previous-vault slot.
    Returned,
}

/// Read the recovery state (read-only).
pub fn recovery_status(data_dir: &Path) -> Result<RecoveryStatus> {
    let journal = read_journal(data_dir)?;
    let mut previous = Vec::new();
    let recovery = recovery_dir(data_dir);
    if let Ok(entries) = fs::read_dir(&recovery) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) || !is_slot_id(&name) {
                continue;
            }
            let (total_bytes, files) = dir_usage(&entry.path());
            let stamp = name
                .trim_start_matches(PREVIOUS_PREFIX)
                .split('-')
                .next()
                .unwrap_or("")
                .to_string();
            previous.push(PreviousVaultSlot {
                id: name,
                created_at: raw_stamp_to_rfc3339(&stamp),
                total_bytes,
                files,
            });
        }
    }
    previous.sort_by(|a, b| b.id.cmp(&a.id));
    let last_switch = match &journal {
        Some(journal) if journal.state == SwitchState::Completed => Some(LastSwitchOrigin {
            kind: if journal.archive.is_some() {
                SwitchOriginKind::Restored
            } else {
                SwitchOriginKind::Returned
            },
            archive: journal.archive.clone(),
            created_at: journal.created_at.clone(),
        }),
        _ => None,
    };
    Ok(RecoveryStatus {
        journal_state: journal.as_ref().map(|j| j.state),
        journal_created_at: journal.map(|j| j.created_at),
        staged_preview: staging_dir(data_dir).join(PREVIEW_FILE).is_file(),
        previous,
        last_switch,
    })
}

/// Explicitly forget one previous-vault slot (the user-facing release
/// of the recovery pin). Refused while a switch is unresolved.
pub fn recovery_forget(data_dir: &Path, slot_id: &str) -> Result<()> {
    if !is_slot_id(slot_id) {
        return Err(bad(format!("not a previous-vault id: {slot_id}")));
    }
    ensure_no_open_switch(data_dir)?;
    let slot = recovery_dir(data_dir).join(slot_id);
    if !slot.is_dir() {
        return Err(bad(format!("no previous vault recorded as {slot_id}")));
    }
    fs::remove_dir_all(&slot).map_err(Error::Io)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

/// Whether an error is "file not found" (journal/preview absence).
fn is_missing(err: &Error) -> bool {
    matches!(err, Error::Io(e) if e.kind() == std::io::ErrorKind::NotFound)
}

fn read_journal(data_dir: &Path) -> Result<Option<SwitchJournal>> {
    match read_json(&journal_path(data_dir)) {
        Ok(journal) => Ok(Some(journal)),
        Err(e) if is_missing(&e) => Ok(None),
        Err(e) => Err(e),
    }
}

/// A well-formed previous-slot id (exact shape, no separators — it is
/// joined into the recovery directory): `previous-<yyyymmddhhmmss>-<6 hex>`.
fn is_slot_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix(PREVIOUS_PREFIX) else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() == 2
        && parts[0].len() == 14
        && parts[0].chars().all(|c| c.is_ascii_digit())
        && parts[1].len() == 6
        && parts[1].chars().all(|c| c.is_ascii_hexdigit())
}

fn new_previous_slot(data_dir: &Path) -> Result<PathBuf> {
    let recovery = recovery_dir(data_dir);
    fs::create_dir_all(&recovery).map_err(Error::Io)?;
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    for _ in 0..64 {
        let hex: String = (0..6).map(|_| format!("{:x}", rand_nibble())).collect();
        let slot = recovery.join(format!("{PREVIOUS_PREFIX}{stamp}-{hex}"));
        if !slot.exists() {
            fs::create_dir_all(&slot).map_err(Error::Io)?;
            return Ok(slot);
        }
    }
    Err(bad("cannot allocate a previous-vault slot"))
}

/// Best-effort randomness (slot-name uniqueness only; not security).
fn rand_nibble() -> u8 {
    let mut buf = [0u8; 1];
    if getrandom::fill(&mut buf).is_err() {
        // Fall back to the clock: uniqueness, not unpredictability.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        buf[0] = (nanos % 16) as u8;
    }
    buf[0] % 16
}

fn new_preview_id() -> String {
    let fallback = || {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        nanos.to_le_bytes()
    };
    let mut buf = [0u8; 8];
    if getrandom::fill(&mut buf).is_err() {
        let bytes = fallback();
        buf.copy_from_slice(&bytes);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// RFC 3339 UTC now (shared with the A7 move journal).
pub(crate) fn now_rfc3339_utc() -> String {
    now_rfc3339()
}

/// `20261127153045` → `2026-11-27T15:30:45Z` (best effort).
fn raw_stamp_to_rfc3339(raw: &str) -> String {
    if raw.len() == 14 && raw.chars().all(|c| c.is_ascii_digit()) {
        format!(
            "{}-{}-{}T{}:{}:{}Z",
            &raw[0..4],
            &raw[4..6],
            &raw[6..8],
            &raw[8..10],
            &raw[10..12],
            &raw[12..14]
        )
    } else {
        raw.to_string()
    }
}

/// Total size and file count of a directory tree (best effort).
fn dir_usage(dir: &Path) -> (u64, u64) {
    let mut totals = (0u64, 0u64);
    fn walk(dir: &Path, totals: &mut (u64, u64)) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(&entry.path(), totals);
            } else if meta.is_file() {
                totals.0 += meta.len();
                totals.1 += 1;
            }
        }
    }
    walk(dir, &mut totals);
    totals
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(Error::Io)?;
    serde_json::from_slice(&bytes).map_err(Error::Json)
}

/// Staged-rename JSON write (shared with the A7 move journal).
pub(crate) fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(Error::Json)?;
    write_bytes_synced(path, &bytes)
}

fn write_bytes_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp).map_err(Error::Io)?;
        file.write_all(bytes).map_err(Error::Io)?;
        file.sync_all().map_err(Error::Io)?;
    }
    fs::rename(&tmp, path).map_err(Error::Io)?;
    Ok(())
}

/// Read-modify-write the switch journal. The closure sees the current
/// journal (`None` = absent) and replaces it.
fn write_journal(data_dir: &Path, f: impl FnOnce(&mut Option<SwitchJournal>)) -> Result<()> {
    let path = journal_path(data_dir);
    let mut journal = read_journal(data_dir)?;
    f(&mut journal);
    if let Some(j) = &journal {
        let recovery = recovery_dir(data_dir);
        fs::create_dir_all(&recovery).map_err(Error::Io)?;
        write_json_atomic(&path, j)?;
        fsync_dir(&recovery)?;
    }
    Ok(())
}

/// Streaming SHA-256 + size of a file.
fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path).map_err(Error::Io)?;
    hash_reader(&mut file)
}

fn hash_reader(reader: &mut dyn Read) -> Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut size: u64 = 0;
    loop {
        let n = reader.read(&mut buf).map_err(Error::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((format!("{:x}", hasher.finalize()), size))
}

fn fsync_file(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(Error::Io)
}

#[cfg(unix)]
pub(crate) fn fsync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(Error::Io)
}

#[cfg(not(unix))]
pub(crate) fn fsync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

/// Remove a finished-or-failed snapshot temp (the file plus its WAL
/// sidecars). Every exit path of a backup uses this — the one-shot
/// [`create_backup`] and the app's job alike — so a failed or cancelled
/// run leaves nothing beside the user's destination.
pub fn discard_snapshot(path: &Path) {
    let _ = fs::remove_file(path);
    remove_journal_sidecars(path);
}

/// A temporary path removed on drop (best effort — a hard crash's
/// leftovers are dot-prefixed and never mistaken for real outputs).
struct TempPath<'a>(&'a Path);

impl Drop for TempPath<'_> {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.0);
        remove_journal_sidecars(self.0);
    }
}

/// Remove `<path>-wal` / `-shm` / `-journal` sidecars (best effort).
fn remove_journal_sidecars(path: &Path) {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(suffix);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_file(parent.join(name));
        }
    }
}

fn bad(message: impl Into<String>) -> Error {
    Error::Backup(message.into())
}

fn fault(message: &'static str) -> Error {
    Error::Backup(message.to_string())
}

fn cancelled() -> Error {
    Error::Backup("cancelled".to_string())
}

fn zip_error(e: zip::result::ZipError) -> Error {
    bad(format!("backup container write failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_object_paths_are_validated_strictly() {
        let sha = "a".repeat(64);
        assert!(is_media_object_path(&format!("media/{sha}.jpg")));
        assert!(is_media_object_path(&format!("media/{sha}.BIN")));
        assert!(!is_media_object_path(&format!("media/{sha}")));
        assert!(!is_media_object_path(&format!(
            "media/{}.jpg/x",
            "a".repeat(63)
        )));
        assert!(!is_media_object_path(&format!("media/../{sha}.jpg")));
        assert!(!is_media_object_path(&format!("media/{sha}/x.jpg")));
        assert!(!is_media_object_path(&format!(
            "media/{}.jpg",
            "z".repeat(64)
        )));
        assert!(!is_media_object_path("vault.db"));
        assert!(!is_media_object_path("settings.json"));
        assert_eq!(object_digest_of(&format!("media/{sha}.jpg")).unwrap(), sha);
    }

    #[test]
    fn slot_ids_have_one_exact_shape() {
        assert!(is_slot_id("previous-20261127153045-abc123"));
        assert!(!is_slot_id("previous-20261127153045-abc12"));
        assert!(!is_slot_id("previous-20261127153045-abc123z"));
        assert!(!is_slot_id("previous-20261127153-abc123"));
        assert!(!is_slot_id("previous-20261127153045"));
        assert!(!is_slot_id("20261127153045-abc123"));
        assert!(!is_slot_id("../../etc"));
        assert!(!is_slot_id("previous-..-..-.."));
        assert_eq!(
            raw_stamp_to_rfc3339("20261127153045"),
            "2026-11-27T15:30:45Z"
        );
    }

    #[test]
    fn recovery_dir_is_a_sibling_of_the_vault_root() {
        let dir = Path::new("/x/y/mailchat-vault");
        assert_eq!(recovery_dir(dir), Path::new("/x/y/mailchat-vault-recovery"));
        assert_eq!(
            recovery_dir(Path::new("/vault")),
            Path::new("/vault-recovery")
        );
    }
}

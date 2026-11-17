//! A7: the vault-location engine — candidate inspection, move preview,
//! and the crash-recoverable move journal.
//!
//! This module is the protocol engine behind the plan's move steps
//! (docs/archive-experience-plan.md section 8). The desktop folder
//! picker and the Tauri wiring are a thin follow-up; nothing here has
//! UI dependencies.
//!
//! # Inspection
//!
//! [`inspect_location`] examines a candidate directory and **refuses**
//! everything this build must not treat as a vault destination: a
//! missing or non-directory path, a directory without write access, a
//! network filesystem (SQLite WAL requires all participants on the
//! same host — the active vault never lives on one), an unrelated
//! directory (content but no vault identity is never prepared for
//! overwrite), an identity belonging to a different vault, or a
//! `vault.db` with a schema newer than this build supports. A
//! successful inspection reports free space, the filesystem type, and
//! whether the directory is empty or already holds a compatible vault.
//!
//! Filesystem-type detection parses a mount table that the caller
//! supplies ([`inspect_location`] takes it as a string) so tests can
//! fixture network mounts without privileges;
//! [`host_mount_table`] reads `/proc/self/mounts` on Linux. macOS and
//! Windows detection is a documented follow-up for the desktop ticket:
//! with no mount-table text, detection reports an unknown filesystem,
//! which inspection does not refuse (only *known network* filesystems
//! are refused).
//!
//! # Preview
//!
//! [`preview_move`] produces the phased plan (quiesce → snapshot and
//! object inventory → staged verified copy → pointer switch → restart
//! with the old copy retained) with a byte estimate derived from the
//! same counting path the backup uses ([`crate::backup::snapshot_for_backup`]):
//! an exact snapshot of the database plus every referenced object plus
//! the portable settings file when present. A move is always a
//! **verified copy**; cross-volume is detected and reported, but the
//! engine never assumes a rename is possible even on one volume.
//!
//! The quiesce contract is caller-managed, exactly as the backup
//! snapshot path documents its writer-connection contract:
//! [`preview_move`] runs on the vault's **writer** connection, with new
//! writes paused behind the write queue, so the SQLite backup API
//! yields a consistent snapshot of a WAL database. This module never
//! stops jobs or watchers itself.
//!
//! # Journal
//!
//! [`MoveJournal`] is device-local serde JSON under the existing
//! recovery-directory convention ([`crate::backup::recovery_dir`]) with
//! a distinct filename (`move-journal.json`, never the restore switch
//! journal's). Every record carries **both** vault endpoints —
//! identity and path — plus the intent, so an interruption can be
//! resolved without guessing which vault was moving where.
//!
//! [`resolve_interrupted_move`] turns any journal into exactly one
//! defined action **before a pool opens**: every pre-pointer phase
//! resolves with the **origin** vault authoritative (this slice ships
//! no `vault_move_commit` executor, and the origin is the last copy
//! known complete and writable), a crash at the atomic pointer switch
//! rolls **forward** to the already-verified destination, and a
//! completed journal reports completion. Each outcome names exactly
//! one writable vault ([`MoveRecovery::writable_vault`]) — an
//! interrupted move must never leave two.
//!
//! Executing the copy, the pointer swap, and the per-phase crash
//! matrix is the separately mergeable `vault_move_commit` ticket; this
//! module never switches the active vault, never writes a `vault.json`
//! (only tests and the commit slice do), and never removes the old
//! copy.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::backup::{recovery_dir, snapshot_for_backup};
use crate::storage::SUPPORTED_SCHEMA_VERSION;
use crate::vault_session::VaultIdentity;

/// The move-journal file inside the recovery directory — deliberately
/// distinct from the restore switch journal (`switch-journal.json`),
/// so an interrupted move and an interrupted restore can never be
/// confused for one another.
pub const MOVE_JOURNAL_FILE: &str = "move-journal.json";

/// The move-journal format version this build writes.
pub const MOVE_JOURNAL_VERSION: u32 = 1;

/// Free-space slack on top of measured bytes for a move destination
/// (filesystem metadata, atomic temp files and renames) — mirrors the
/// backup preflight's slack.
pub const MOVE_SLACK_BYTES: u64 = 4 * 1024 * 1024;

/// Filesystem types that cannot host the **active** vault: SQLite WAL
/// requires every participant on the same host, and these filesystems
/// move pages across hosts. Backup *destinations* may be network
/// storage; the live vault may not.
/// <https://www.sqlite.org/wal.html>
pub const NETWORK_FILESYSTEMS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb",
    "smbfs",
    "smb2",
    "ncpfs",
    "afs",
    "ceph",
    "lustre",
    "gpfs",
    "beegfs",
    "9p",
    "fuse.sshfs",
    "fuse.s3fs",
    "fuse.davfs2",
    "davfs",
    "davfs2",
];

/// Why a candidate location (or a move over it) was refused. Every
/// refusal is a typed fact the UI can render; none of them ever
/// leaves the candidate directory prepared for overwrite.
#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    /// The candidate path does not exist. A native picker normally
    /// yields existing directories; a missing path is a refusal, not a
    /// creation.
    #[error("the vault location does not exist: {path}")]
    Missing { path: PathBuf },
    /// The recorded active vault location is missing — an unplugged or
    /// unmounted external drive. This is the recovery-screen fact:
    /// the vault is **never** recreated as an empty replacement
    /// archive, and this app never writes under the missing path.
    #[error(
        "the vault location is missing (an external drive may be unplugged): {path} — no \
         replacement archive was created; reconnect the drive or pick a recovery path"
    )]
    ExternalVaultMissing { path: PathBuf },
    /// The candidate path exists but is not a directory.
    #[error("the vault location is not a directory: {path}")]
    NotADirectory { path: PathBuf },
    /// The write probe inside the candidate directory failed.
    #[error("the vault location is not writable: {path}")]
    NotWritable { path: PathBuf },
    /// The candidate lives on a network filesystem, which the active
    /// WAL vault must never do (SQLite WAL requires all participants
    /// on one host). Copying a *backup* there remains allowed.
    #[error(
        "network filesystem {fs:?} at {path} cannot host the active vault (SQLite WAL \
         requires all participants on the same host); use it for backups only"
    )]
    NetworkFilesystem { fs: String, path: PathBuf },
    /// The directory holds content but no vault identity: an unrelated
    /// directory is never prepared for overwrite.
    #[error(
        "the directory {path} holds unrelated content and no vault identity — it is never \
         prepared for overwrite"
    )]
    UnrelatedDirectory { path: PathBuf },
    /// A `vault.json` exists but is not a valid identity for this
    /// build (unknown format marker, unsupported version, unparseable
    /// UUID, or a recorded directory name that binds it elsewhere).
    #[error("the vault identity at this location is invalid: {0}")]
    InvalidIdentity(String),
    /// The directory already holds a different vault; a move over it
    /// would overwrite that vault's identity.
    #[error(
        "the directory {path} already holds vault {existing}, not the moving vault {active} — \
         it is never overwritten"
    )]
    IdentityMismatch {
        path: PathBuf,
        existing: String,
        active: String,
    },
    /// A `vault.db` exists but cannot be opened/read as a schema
    /// version.
    #[error("cannot read the vault database at {path}: {reason}")]
    InvalidVaultDb { path: PathBuf, reason: String },
    /// The existing vault's schema version is newer than this build
    /// supports (same refusal as opening one).
    #[error(
        "vault at {path} uses schema version {db}, newer than this build supports \
         ({supported}); upgrade the app before opening it"
    )]
    SchemaTooNew {
        path: PathBuf,
        db: i64,
        supported: i64,
    },
    /// The destination filesystem cannot hold the estimated move.
    #[error(
        "not enough free space to move the vault: need {needed} bytes, only {available} \
         available"
    )]
    InsufficientSpace { needed: u64, available: u64 },
    /// An error from the reused vault machinery (snapshot/backup
    /// counting, storage reads).
    #[error("vault move error: {0}")]
    Vault(String),
    /// An I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// A JSON error while reading or writing the journal.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

fn vault_err(context: &str, e: crate::Error) -> MoveError {
    MoveError::Vault(format!("{context}: {e}"))
}

// ---------------------------------------------------------------------------
// Filesystem detection
// ---------------------------------------------------------------------------

/// What detection learned about a candidate's filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemInfo {
    /// The mount-table filesystem type (`ext4`, `nfs`, …); `"unknown"`
    /// when no mount entry matched or the platform provides no mount
    /// table (macOS/Windows detection is a documented desktop
    /// follow-up).
    pub fs_type: String,
    /// Whether the type is one of [`NETWORK_FILESYSTEMS`]. Only a
    /// *known network* type is refused; `unknown` is not.
    pub network: bool,
}

/// Read this host's mount table. On Linux this is
/// `/proc/self/mounts`; elsewhere it returns an empty string (detection
/// then reports `unknown`, which inspection does not refuse — see the
/// module docs for the OS-specific follow-up note).
pub fn host_mount_table() -> String {
    fs::read_to_string("/proc/self/mounts").unwrap_or_default()
}

/// Detect the filesystem backing `path` from a mount table's text
/// (each line: `device mountpoint fstype options dump pass`). The
/// entry whose mountpoint is the longest prefix of the canonicalized
/// path wins; `\040`-style escapes in mountpoints are decoded. An
/// empty or non-matching table yields `fs_type: "unknown"`.
pub fn detect_filesystem(path: &Path, mount_table: &str) -> FilesystemInfo {
    let candidate = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut best: Option<(usize, String)> = None;
    for line in mount_table.lines() {
        let mut fields = line.split_whitespace();
        let (Some(_dev), Some(mount), Some(fstype)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let mount = unescape_mount_point(mount);
        let mount = Path::new(&mount);
        // `Path::starts_with` compares whole components, so `/a/b`
        // matches mount `/a` but never `/abc`.
        if candidate.starts_with(mount) {
            let len = mount.as_os_str().len();
            if best.as_ref().is_none_or(|(best_len, _)| len > *best_len) {
                best = Some((len, fstype.to_string()));
            }
        }
    }
    match best {
        Some((_, fs_type)) => {
            let network = NETWORK_FILESYSTEMS
                .iter()
                .any(|n| n.eq_ignore_ascii_case(&fs_type));
            FilesystemInfo { fs_type, network }
        }
        None => FilesystemInfo {
            fs_type: "unknown".to_string(),
            network: false,
        },
    }
}

/// Decode the octal escapes `/proc/self/mounts` uses for spaces and
/// tabs in mount points (`\040`, `\011`, …).
fn unescape_mount_point(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut digits = String::new();
        for _ in 0..3 {
            match chars.clone().next() {
                Some(d) if d.is_ascii_digit() => {
                    digits.push(d);
                    chars.next();
                }
                _ => break,
            }
        }
        if digits.len() == 3 {
            if let Ok(value) = u8::from_str_radix(&digits, 8) {
                out.push(value as char);
                continue;
            }
        }
        out.push('\\');
        out.push_str(&digits);
    }
    out
}

// ---------------------------------------------------------------------------
// Inspection
// ---------------------------------------------------------------------------

/// What inspection found in the candidate directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocationDisposition {
    /// The directory holds nothing at all (ignoring this inspection's
    /// own probe file, which is always removed): a safe destination
    /// for a new copy of the vault.
    Empty,
    /// The directory already holds a vault this build can open: a
    /// validated identity bound to this directory name, a supported
    /// schema version when a database exists. Opening or moving over
    /// it is the caller's explicit choice; inspection never writes.
    ExistingVault {
        vault_id: Uuid,
        /// The schema version of the found `vault.db`, or `None` when
        /// the identity exists without a database yet.
        schema_version: Option<i64>,
    },
}

/// A successful inspection of a candidate vault location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationInspection {
    /// The inspected directory (canonicalized by detection; the path
    /// itself is reported as given).
    pub path: PathBuf,
    /// Free space on the location's filesystem, measured through the
    /// same `fs4` usage the backup preflight uses.
    pub free_bytes: u64,
    /// The detected filesystem (see [`detect_filesystem`]).
    pub filesystem: FilesystemInfo,
    /// Always `true` on success — a failed write probe is the
    /// [`MoveError::NotWritable`] refusal.
    pub writable: bool,
    /// What the directory holds today.
    pub disposition: LocationDisposition,
}

/// The entries a directory may hold and still count as a vault (or an
/// empty candidate): the identity/database/settings/media/lock files
/// plus SQLite's WAL sidecars (`vault.db-wal`, `vault.db-shm`,
/// `vault.db-journal`), which a live or interrupted database can leave
/// behind. Everything else means unrelated content. The probe file
/// this inspection creates is cleaned up before scanning.
const VAULT_ENTRIES: &[&str] = &[
    "vault.json",
    "vault.db",
    "vault.db-wal",
    "vault.db-shm",
    "vault.db-journal",
    "settings.json",
    "media",
    "vault.lock",
];

/// Inspect a candidate vault directory. `active_vault_id` is the vault
/// being moved (or opened) when inspection serves a move/open flow;
/// `None` inspects without a move context. `mount_table` is the
/// injectable mount-table text ([`host_mount_table`] on Linux).
///
/// Inspection is read-only apart from its own probe file, which is
/// always removed: it never writes a `vault.json`, never creates a
/// database, and never prepares an unrelated directory for overwrite.
pub fn inspect_location(
    path: &Path,
    active_vault_id: Option<Uuid>,
    mount_table: &str,
) -> Result<LocationInspection, MoveError> {
    let meta = fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => MoveError::Missing {
            path: path.to_path_buf(),
        },
        _ => MoveError::Io(e),
    })?;
    if !meta.is_dir() {
        return Err(MoveError::NotADirectory {
            path: path.to_path_buf(),
        });
    }

    let filesystem = detect_filesystem(path, mount_table);
    if filesystem.network {
        return Err(MoveError::NetworkFilesystem {
            fs: filesystem.fs_type,
            path: path.to_path_buf(),
        });
    }

    probe_writable(path)?;

    let free_bytes = fs4::available_space(path).map_err(|e| {
        MoveError::Vault(format!(
            "cannot measure free space at {}: {e}",
            path.display()
        ))
    })?;

    let disposition = classify_contents(path, active_vault_id)?;
    Ok(LocationInspection {
        path: path.to_path_buf(),
        free_bytes,
        filesystem,
        writable: true,
        disposition,
    })
}

/// Prove write access by creating, writing, and removing a probe file
/// inside the directory. A failure refuses the location.
fn probe_writable(dir: &Path) -> Result<(), MoveError> {
    let probe = dir.join(format!(".mailchat-inspect-{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&probe)?;
        file.write_all(b"probe")?;
        file.sync_all()
    })();
    let _ = fs::remove_file(&probe);
    result.map_err(|_| MoveError::NotWritable {
        path: dir.to_path_buf(),
    })
}

/// Decide what the directory holds: nothing at all, an existing vault,
/// or an unrelated directory that must never be overwritten. Vault
/// files **without** an identity (e.g. a bare `vault.db`, or a legacy
/// vault never opened through a session) are a refusal too: a copy may
/// not overwrite a directory that could hold someone's data.
fn classify_contents(
    dir: &Path,
    active_vault_id: Option<Uuid>,
) -> Result<LocationDisposition, MoveError> {
    let mut has_identity = false;
    let mut has_entries = false;
    for entry in fs::read_dir(dir).map_err(MoveError::Io)? {
        let entry = entry.map_err(MoveError::Io)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !VAULT_ENTRIES.contains(&name.as_str()) {
            return Err(MoveError::UnrelatedDirectory {
                path: dir.to_path_buf(),
            });
        }
        has_entries = true;
        has_identity |= name == "vault.json";
    }
    if !has_entries {
        return Ok(LocationDisposition::Empty);
    }
    if !has_identity {
        // Vault-shaped content without a governing identity: never
        // prepared for overwrite.
        return Err(MoveError::UnrelatedDirectory {
            path: dir.to_path_buf(),
        });
    }

    // A vault identity governs this directory: validate it against the
    // directory itself (format, version, UUID, directory-name binding).
    let identity = VaultIdentity::read(dir)
        .map_err(|e| MoveError::InvalidIdentity(e.to_string()))?
        .expect("identity entry was just seen");
    let vault_id = identity
        .validate(dir)
        .map_err(|e| MoveError::InvalidIdentity(e.to_string()))?;
    if let Some(active) = active_vault_id {
        if vault_id != active {
            return Err(MoveError::IdentityMismatch {
                path: dir.to_path_buf(),
                existing: vault_id.to_string(),
                active: active.to_string(),
            });
        }
    }

    // Schema compatibility: a database newer than this build refuses
    // exactly like an open would.
    let db = dir.join("vault.db");
    let schema_version = if db.is_file() {
        Some(schema_version_of(&db, dir)?)
    } else {
        None
    };
    Ok(LocationDisposition::ExistingVault {
        vault_id,
        schema_version,
    })
}

/// Read the schema version of a candidate database read-only and
/// refuse a newer one.
fn schema_version_of(db: &Path, dir_for_error: &Path) -> Result<i64, MoveError> {
    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| MoveError::InvalidVaultDb {
            path: dir_for_error.to_path_buf(),
            reason: format!("cannot open read-only: {e}"),
        })?;
    let version =
        crate::storage::read_schema_version(&conn).map_err(|e| MoveError::InvalidVaultDb {
            path: dir_for_error.to_path_buf(),
            reason: e.to_string(),
        })?;
    if version > SUPPORTED_SCHEMA_VERSION {
        return Err(MoveError::SchemaTooNew {
            path: dir_for_error.to_path_buf(),
            db: version,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    Ok(version)
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

/// One planned phase of the move, in execution order (plan section 8,
/// steps 2–5): what the executor will do and what a crash at that
/// boundary means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovePhase {
    /// Step 2a: lifecycle ownership acquired — new jobs/watch events
    /// stopped, current work finished or cancelled, writers held
    /// stable. Caller-managed. Crash: nothing has been copied; the
    /// origin is authoritative.
    Quiescing,
    /// Step 2b: a consistent SQLite snapshot and the referenced object
    /// inventory exist. Crash: origin authoritative.
    SnapshotReady,
    /// Step 3a: copying snapshot, objects, and portable metadata into
    /// staging **on the destination volume**, verifying checksums
    /// (always a copy; never an assumed rename, cross-volume or not).
    /// Crash: origin authoritative; destination staging is incomplete
    /// by definition.
    Copying,
    /// Step 3b: the destination holds a checksum-verified, integrity-
    /// checked copy in staging. Crash: origin still authoritative (the
    /// pointer has not moved); the verified staging is kept for the
    /// pointer switch.
    CopiedVerified,
    /// Step 4: the device-local move journal/bootstrap pointer is being
    /// atomically updated. Crash: rolls forward — the destination was
    /// verified before this phase began.
    SwitchingPointer,
    /// Step 5: restarted into the selected vault; the old copy is
    /// retained until the user separately removes it.
    Completed,
}

impl MovePhase {
    /// The ordered phases of a full move (for previews and docs).
    pub const PLAN: [MovePhase; 6] = [
        MovePhase::Quiescing,
        MovePhase::SnapshotReady,
        MovePhase::Copying,
        MovePhase::CopiedVerified,
        MovePhase::SwitchingPointer,
        MovePhase::Completed,
    ];

    /// One-line summary of the phase.
    pub fn summary(&self) -> &'static str {
        match self {
            MovePhase::Quiescing => {
                "quiesce: stop jobs and watch events, hold writers stable (caller-managed)"
            }
            MovePhase::SnapshotReady => {
                "snapshot: consistent SQLite snapshot plus the referenced object inventory"
            }
            MovePhase::Copying => {
                "copy: staged copy on the destination volume with checksum verification (never an assumed rename)"
            }
            MovePhase::CopiedVerified => "verified: destination staging passed checksums and database integrity",
            MovePhase::SwitchingPointer => "pointer: atomically update the device-local move journal/bootstrap pointer",
            MovePhase::Completed => "restart: open the selected vault; retain the old copy until separately removed",
        }
    }
}

/// One endpoint of a move: the vault's stable identity and the path it
/// sits at. The identity is the same UUID on both ends — a move copies
/// the vault, it never mints a new identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultEndpoint {
    /// The vault UUID recorded in that copy's `vault.json`.
    pub vault_id: String,
    /// The directory holding that copy.
    pub path: PathBuf,
}

/// Why the journal exists. `Move` is the only intent today; the enum
/// exists so future intents (e.g. a restore-into-location) carry their
/// own recovery semantics without reshaping the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveIntent {
    /// Relocate the active vault to the destination directory, keeping
    /// the old copy until separately removed.
    Move,
}

/// The byte estimate a preview reports. `database_bytes` is the exact
/// consistent-snapshot size (the copy carries the snapshot, not the
/// live WAL file); `object_bytes` covers every referenced object; the
/// portable settings file counts when present; `overhead_bytes` is
/// filesystem slack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MoveEstimate {
    pub database_bytes: u64,
    pub object_bytes: u64,
    pub settings_bytes: u64,
    pub overhead_bytes: u64,
    /// The sum of the above (saturating).
    pub total_bytes: u64,
}

impl MoveEstimate {
    fn total(&self) -> u64 {
        self.database_bytes
            .saturating_add(self.object_bytes)
            .saturating_add(self.settings_bytes)
            .saturating_add(self.overhead_bytes)
    }
}

/// A previewed move: the phased plan, the byte estimate, and the
/// destination facts the plan was checked against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovePreview {
    /// The vault being moved.
    pub origin: VaultEndpoint,
    /// The inspected, accepted destination.
    pub destination: VaultEndpoint,
    /// The ordered phases ([`MovePhase::PLAN`] with summaries).
    pub phases: Vec<MovePhase>,
    /// The byte estimate for the copy.
    pub estimate: MoveEstimate,
    /// Whether origin and destination sit on different filesystem
    /// devices. The move is a verified copy either way — this flag
    /// informs the UI and the estimate, never an optimization that
    /// assumes a rename.
    pub cross_volume: bool,
    /// Free space measured at the destination when the preview ran.
    pub destination_free_bytes: u64,
}

/// Knobs for [`preview_move`].
#[derive(Debug, Clone, Copy, Default)]
pub struct MovePreviewOptions {
    /// Force the capacity preflight to demand at least this many free
    /// bytes (test hook for provoking [`MoveError::InsufficientSpace`]
    /// on any real disk).
    pub min_free_bytes: Option<u64>,
}

/// Preview a move of the vault at `data_dir` into `destination`.
///
/// Runs on the vault's **writer** connection (`conn`) with new writes
/// paused behind the write queue — the same caller-managed quiesce
/// contract as [`crate::backup::snapshot_for_backup`]. The estimate is
/// derived from that consistent snapshot and the referenced-object
/// inventory (the backup counting path), so it describes an exact
/// point-in-time vault, not a live-file guess.
///
/// The destination is inspected first (all refusals apply: unrelated
/// directory, foreign identity, network filesystem, write access), the
/// capacity preflight compares the estimate against measured free
/// space, and the preview snapshot is removed again — a preview leaves
/// nothing behind but the (pre-existing) recovery directory.
pub fn preview_move(
    data_dir: &Path,
    conn: &Connection,
    destination: &Path,
    mount_table: &str,
    opts: &MovePreviewOptions,
) -> Result<MovePreview, MoveError> {
    // The origin must be a session-opened vault: the journal carries
    // its stable identity.
    let identity = VaultIdentity::read(data_dir)
        .map_err(|e| MoveError::InvalidIdentity(e.to_string()))?
        .ok_or_else(|| {
            MoveError::Vault(format!(
                "the origin vault at {} has no vault.json identity — open it once through a \
                 VaultSession before moving it",
                data_dir.display()
            ))
        })?;
    let origin_id = identity
        .validate(data_dir)
        .map_err(|e| MoveError::InvalidIdentity(e.to_string()))?;

    let inspection = inspect_location(destination, Some(origin_id), mount_table)?;

    // The estimate's database side is the consistent snapshot; objects
    // and settings come from the same plan. Written under the recovery
    // directory and removed afterwards.
    let recovery = recovery_dir(data_dir);
    fs::create_dir_all(&recovery).map_err(MoveError::Io)?;
    let snapshot = recovery.join(format!(".move-preview-{}.db", std::process::id()));
    let result = (|| -> Result<MoveEstimate, MoveError> {
        let plan = snapshot_for_backup(data_dir, conn, &snapshot)
            .map_err(|e| vault_err("cannot snapshot the vault for the move estimate", e))?;
        let database_bytes = fs::metadata(&snapshot).map_err(MoveError::Io)?.len();
        let object_bytes = plan.objects.iter().map(|o| o.size).sum();
        let settings_bytes = if plan.has_settings {
            fs::metadata(data_dir.join(crate::settings::SETTINGS_FILE))
                .map_err(MoveError::Io)?
                .len()
        } else {
            0
        };
        Ok(MoveEstimate {
            database_bytes,
            object_bytes,
            settings_bytes,
            overhead_bytes: MOVE_SLACK_BYTES,
            total_bytes: 0,
        })
    })();
    // Remove the preview snapshot (and any WAL sidecars the read-only
    // probe left) whatever happened.
    let _ = fs::remove_file(snapshot.with_extension("db-wal"));
    let _ = fs::remove_file(snapshot.with_extension("db-shm"));
    let _ = fs::remove_file(&snapshot);
    let mut estimate = result?;
    estimate.total_bytes = estimate.total();

    let needed = opts.min_free_bytes.map_or(estimate.total_bytes, |floor| {
        estimate.total_bytes.max(floor)
    });
    if inspection.free_bytes < needed {
        return Err(MoveError::InsufficientSpace {
            needed,
            available: inspection.free_bytes,
        });
    }

    Ok(MovePreview {
        origin: VaultEndpoint {
            vault_id: origin_id.to_string(),
            path: data_dir.to_path_buf(),
        },
        destination: VaultEndpoint {
            vault_id: origin_id.to_string(),
            path: destination.to_path_buf(),
        },
        phases: MovePhase::PLAN.to_vec(),
        estimate,
        cross_volume: cross_volume(data_dir, destination),
        destination_free_bytes: inspection.free_bytes,
    })
}

/// Whether two paths sit on different filesystem devices. Detection
/// uses `st_dev`; when either side cannot be stat'ed the answer is
/// conservatively `true` (a cross-volume answer forces the copy path —
/// the engine never assumes a rename).
fn cross_volume(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => match (a.metadata(), b.metadata()) {
            (Ok(am), Ok(bm)) => am.dev() != bm.dev(),
            _ => true,
        },
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Move journal
// ---------------------------------------------------------------------------

/// The device-local journal of one vault move: every record carries
/// both endpoints (identity and path) and the intent, so an
/// interruption can be resolved without guessing which vault was
/// moving where. Written atomically (temp + fsync + rename) under the
/// recovery directory; see [`resolve_interrupted_move`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveJournal {
    /// The journal format version ([`MOVE_JOURNAL_VERSION`]).
    pub version: u32,
    /// The phase the move had reached when the journal was last
    /// written — the recovery action is a function of this alone.
    pub phase: MovePhase,
    /// RFC 3339 UTC — when the move started.
    pub created_at: String,
    /// What the move intends (see [`MoveIntent`]).
    pub intent: MoveIntent,
    /// The vault being moved (authoritative for every pre-pointer
    /// phase).
    pub origin: VaultEndpoint,
    /// Where the vault is being moved to.
    pub destination: VaultEndpoint,
}

impl MoveJournal {
    /// A fresh journal for a move of `origin` into `destination`,
    /// starting at [`MovePhase::Quiescing`]. The executor (the later
    /// `vault_move_commit` slice) advances the phase; tests and that
    /// slice are the only writers.
    pub fn new_move(origin: VaultEndpoint, destination: VaultEndpoint) -> Self {
        Self {
            version: MOVE_JOURNAL_VERSION,
            phase: MovePhase::Quiescing,
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            intent: MoveIntent::Move,
            origin,
            destination,
        }
    }
}

/// The move journal's path under the recovery directory.
pub fn move_journal_path(data_dir: &Path) -> PathBuf {
    recovery_dir(data_dir).join(MOVE_JOURNAL_FILE)
}

/// Write the move journal atomically: the JSON lands in a private temp
/// sibling first, is fsynced, and only a fully written file is renamed
/// into place, then the recovery directory is fsynced — a crash
/// mid-write can never leave a half-written journal.
pub fn write_move_journal(data_dir: &Path, journal: &MoveJournal) -> Result<(), MoveError> {
    let recovery = recovery_dir(data_dir);
    fs::create_dir_all(&recovery).map_err(MoveError::Io)?;
    let path = move_journal_path(data_dir);
    let tmp = recovery.join(format!(".{MOVE_JOURNAL_FILE}.tmp"));
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(MoveError::Io)?;
        file.write_all(&serde_json::to_vec_pretty(journal)?)
            .map_err(MoveError::Io)?;
        file.sync_all().map_err(MoveError::Io)?;
    }
    fs::rename(&tmp, &path).map_err(MoveError::Io)?;
    fsync_dir(&recovery)?;
    Ok(())
}

/// Read the move journal; `Ok(None)` when none exists (no move was in
/// flight). A corrupt journal is an error, never a silent restart —
/// startup must resolve it before any pool opens.
pub fn read_move_journal(data_dir: &Path) -> Result<Option<MoveJournal>, MoveError> {
    match fs::read(move_journal_path(data_dir)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(MoveError::Io(e)),
    }
}

/// The one defined action an interrupted (or completed) move journal
/// resolves to at startup, **before a pool opens**. Resolution is a
/// pure function of the journal: it performs no file operations — the
/// caller performs the action. Every action names exactly one writable
/// vault ([`MoveRecovery::writable_vault`]); an interrupted move never
/// leaves two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MoveRecovery {
    /// No journal exists: nothing was in flight.
    None,
    /// Keep the **origin**: every pre-pointer phase resolves with the
    /// origin authoritative — it is the last copy known complete and
    /// writable, and (until the `vault_move_commit` executor exists)
    /// the only copy this build ever switched pointers away from. The
    /// action: open the origin as the only writable vault; any staging
    /// left at the destination is incomplete or at most pre-pointer
    /// and must be discarded before a retry; the origin is never
    /// recreated as an empty archive.
    KeepOrigin {
        origin: VaultEndpoint,
        destination: VaultEndpoint,
    },
    /// Adopt the **destination**: a crash at the atomic pointer switch
    /// rolls forward — the destination copy was verified
    /// ([`MovePhase::CopiedVerified`]) before that phase began, and
    /// the pointer update itself is atomic. The action: open the
    /// destination as the only writable vault and retain the origin
    /// read-only until the user separately removes it.
    AdoptDestination {
        origin: VaultEndpoint,
        destination: VaultEndpoint,
    },
    /// The journal records a finished move: the destination is the
    /// selected vault, the origin copy is retained until separately
    /// removed.
    Completed {
        origin: VaultEndpoint,
        destination: VaultEndpoint,
    },
}

impl MoveRecovery {
    /// The exactly one vault that may be writable after resolution:
    /// `None` when no journal existed. An interrupted move must never
    /// resolve to two writable vaults.
    pub fn writable_vault(&self) -> Option<&Path> {
        match self {
            MoveRecovery::None => None,
            MoveRecovery::KeepOrigin { origin, .. } => Some(&origin.path),
            MoveRecovery::AdoptDestination { destination, .. }
            | MoveRecovery::Completed { destination, .. } => Some(&destination.path),
        }
    }
}

/// Resolve the move journal at startup into one defined action (see
/// [`MoveRecovery`]). Runs before any pool opens; a present-but-
/// unreadable journal is an error the caller must surface (the
/// recovery screen), never a silent empty-vault restart.
pub fn resolve_interrupted_move(data_dir: &Path) -> Result<MoveRecovery, MoveError> {
    let Some(journal) = read_move_journal(data_dir)? else {
        return Ok(MoveRecovery::None);
    };
    let MoveJournal {
        origin,
        destination,
        ..
    } = journal;
    Ok(match journal.phase {
        MovePhase::Quiescing
        | MovePhase::SnapshotReady
        | MovePhase::Copying
        | MovePhase::CopiedVerified => MoveRecovery::KeepOrigin {
            origin,
            destination,
        },
        MovePhase::SwitchingPointer => MoveRecovery::AdoptDestination {
            origin,
            destination,
        },
        MovePhase::Completed => MoveRecovery::Completed {
            origin,
            destination,
        },
    })
}

/// fsync a directory so a rename inside it is durable (the same
/// convention the backup journal uses).
fn fsync_dir(dir: &Path) -> Result<(), MoveError> {
    let file = fs::File::open(dir).map_err(MoveError::Io)?;
    file.sync_all().map_err(MoveError::Io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_mounts() -> String {
        "proc /proc proc rw 0 0\n/dev/sda1 / ext4 rw,relatime 0 0\n".to_string()
    }

    #[test]
    fn detects_local_and_network_filesystems_from_the_mount_table() {
        let dir = tempfile::tempdir().unwrap();
        let local = detect_filesystem(dir.path(), &local_mounts());
        assert_eq!(local.fs_type, "ext4");
        assert!(!local.network);

        let network = local_mounts().replace("/ ext4", "/ nfs");
        let refused = detect_filesystem(dir.path(), &network);
        assert_eq!(refused.fs_type, "nfs");
        assert!(refused.network, "nfs is a network filesystem");

        // A cifs mount below `/` wins by longest prefix.
        let nested_path = dir
            .path()
            .canonicalize()
            .unwrap()
            .strip_prefix("/")
            .unwrap()
            .display()
            .to_string();
        let nested = format!("{}dev/sdb /{} cifs rw 0 0\n", local_mounts(), nested_path);
        let nested_info = detect_filesystem(dir.path(), &nested);
        assert_eq!(nested_info.fs_type, "cifs");
        assert!(nested_info.network);

        let unknown = detect_filesystem(dir.path(), "");
        assert_eq!(unknown.fs_type, "unknown");
        assert!(!unknown.network);
    }

    #[test]
    fn mount_point_escapes_decode() {
        assert_eq!(unescape_mount_point("/mnt/my\\040share"), "/mnt/my share");
        assert_eq!(unescape_mount_point("/plain"), "/plain");
    }
}

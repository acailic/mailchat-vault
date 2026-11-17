//! The A7 vault-location engine: inspect a candidate directory,
//! preview a move with an estimate, and keep a crash-recoverable move
//! journal whose every phase has one defined recovery action.
//!
//! This slice is the protocol engine only — the desktop folder picker
//! and the Tauri wiring are a thin follow-up, and the executing copy
//! (a future `vault_move_commit`, plan section 8 steps 2–5) is the
//! next ticket. Nothing here switches the active vault or removes the
//! old copy.
//!
//! # Inspection
//!
//! [`vault_location_inspect`] answers "could the active vault live
//! here?": the directory must exist, be writable (a probe file), sit
//! on a **local** filesystem (network filesystems are refused for an
//! active WAL vault — [SQLite WAL](https://www.sqlite.org/wal.html)
//! requires every participant on the same host; backup *destinations*
//! are validated separately and may be remote), and report free space
//! through the existing `fs4` usage. An existing identity
//! (`vault.json`) is read and validated: a directory holding another
//! vault's identity is never prepared for overwrite, and an existing
//! database must not carry a schema newer than this build. A directory
//! with content but no vault identity or database is refused as
//! unrelated — never silently taken over.
//!
//! Filesystem-type detection reads `/proc/self/mounts` on Linux; the
//! mount-table source is injectable so tests fixture network mounts
//! without privileges. macOS and Windows detection is the desktop
//! follow-up (an unreadable mount table reports
//! [`FilesystemKind::Unknown`], never a guess).
//!
//! # Preview
//!
//! [`vault_move_preview`] produces the phased plan of the move
//! protocol (quiesce → snapshot → staged verified copy → pointer
//! switch → restart with the old copy retained) plus a byte estimate
//! derived from the backup snapshot path ([`backup::snapshot_for_backup`]):
//! database, every referenced object, and the portable settings file
//! when present. The copy is always a **verified copy** — a
//! cross-volume move is never assumed to be an atomic rename, and the
//! same-volume case makes no rename assumption either.
//!
//! The quiesce contract is caller-managed, exactly as the backup
//! snapshot path documents its writer-connection contract: the caller
//! stops new jobs and watch events, finishes or cancels current work,
//! and holds writers stable before the snapshot phase runs.
//!
//! # The move journal
//!
//! The journal is device-local JSON under the existing
//! recovery-directory convention ([`backup::recovery_dir`]) with its
//! own file name ([`MOVE_JOURNAL_FILE`]); it never lives on the
//! destination volume, which may disappear. Every phase carries both
//! vault endpoints (identity and path — the same stable vault UUID
//! moving from origin to destination), the staging paths, and the
//! move intent, so an interrupted move can be resolved without
//! guessing:
//!
//! | phase | meaning | recovery action |
//! |---|---|---|
//! | [`MovePhase::Intended`] | journal written, nothing started | cancel the move; the origin stays authoritative |
//! | [`MovePhase::Quiesced`] | jobs/watch stopped, writers held | resume the origin; discard any staging |
//! | [`MovePhase::Snapshotted`] | consistent snapshot + object inventory staged | discard the staging snapshot; resume the origin |
//! | [`MovePhase::Copying`] | staged copy in progress on the destination | discard the partial destination staging; resume the origin |
//! | [`MovePhase::Copied`] | copy finished and checksum-verified | roll **back** to the origin: the verified duplicate is removed so no second writable vault exists (a future executor slice may offer roll-forward) |
//! | [`MovePhase::PointerSwitched`] | the bootstrap pointer names the destination | activate the destination; the origin copy is retained (read-only through this app) until separately removed |
//! | [`MovePhase::Completed`] | the move finished | nothing to do |
//!
//! [`recover_interrupted_move`] runs at startup **before a pool is
//! opened** and enacts the action for the phase it finds, marking the
//! journal completed once resolved. The origin vault is authoritative
//! whenever the commit executor is absent — this build has no
//! executor, so every pre-pointer interruption resolves to the intact
//! origin and a cleaned destination. A journal at
//! [`MovePhase::PointerSwitched`] whose destination is gone resolves
//! to the typed [`MoveError::VaultUnavailable`] fact (the recovery
//! screen path) with the journal left unresolved — the user reattaches
//! the location; an empty replacement archive is never created.

use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::backup::{self, fsync_dir, now_rfc3339_utc, write_json_atomic};
use crate::settings::SETTINGS_FILE;
use crate::storage::{read_schema_version, SUPPORTED_SCHEMA_VERSION};
use crate::vault_session::{self, SessionError};

/// The move-journal file inside the recovery directory — distinct from
/// the backup switch journal so a restore and a move never collide.
pub const MOVE_JOURNAL_FILE: &str = "move-journal.json";

/// The move-journal format version this build writes; a journal with a
/// newer version refuses to resolve (an older build cannot interpret
/// it).
pub const MOVE_JOURNAL_VERSION: u32 = 1;

/// The staging directory a move creates **inside** the destination
/// root: the staged, checksum-verified copy lands here and is only
/// finalized into the destination root by the pointer-switch phase.
pub const DESTINATION_STAGING_DIR: &str = ".mailchat-move-staging";

/// The staging area (snapshot + object inventory) a move creates
/// inside the origin's recovery directory — device-local, like the
/// journal itself.
pub const ORIGIN_STAGING_DIR: &str = "move-staging";

// ---------------------------------------------------------------------------
// Errors — typed facts for the UI and the recovery screen
// ---------------------------------------------------------------------------

/// Why a location inspection, preview, or journal resolution refused.
/// Every variant is a fact to show (the later recovery screen consumes
/// [`MoveError::VaultUnavailable`]), never a silent fallback.
#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the candidate vault location does not exist: {0}")]
    DirectoryMissing(PathBuf),
    #[error("the candidate vault location is not a directory: {0}")]
    NotADirectory(PathBuf),
    #[error("the candidate vault location is not writable: {0}")]
    NotWritable(PathBuf),
    #[error(
        "refusing the network filesystem {fstype} at {path}: an active WAL vault must live on \
         local storage (https://www.sqlite.org/wal.html)"
    )]
    NetworkFilesystem { fstype: String, path: PathBuf },
    #[error(
        "refusing to prepare the unrelated directory {0} for overwrite: it holds content but no \
         vault identity or database"
    )]
    UnrelatedDirectory(PathBuf),
    #[error("the vault identity at {path} is invalid: {reason}")]
    InvalidIdentity { path: PathBuf, reason: String },
    #[error(
        "the destination already holds vault {destination_id} while the active vault is \
         {active_id} — another vault is never overwritten"
    )]
    DestinationVaultMismatch {
        path: PathBuf,
        destination_id: String,
        active_id: String,
    },
    #[error(
        "archive schema version {db} is newer than this build supports ({supported}); upgrade \
         the app before opening it"
    )]
    SchemaTooNew { db: i64, supported: i64 },
    #[error(
        "the vault this journal switched to is unavailable: {0} — reattach the location and \
         restart; no replacement archive was created"
    )]
    VaultUnavailable(PathBuf),
    #[error(
        "an unresolved vault move journal exists (phase {state:?}); resolve it before planning \
         another move"
    )]
    UnresolvedJournal { state: MovePhase },
    #[error("vault move error: {0}")]
    Vault(String),
}

fn move_err(e: crate::Error) -> MoveError {
    MoveError::Vault(e.to_string())
}

// ---------------------------------------------------------------------------
// Filesystem-type detection (Linux; injectable mount table)
// ---------------------------------------------------------------------------

/// The filesystem a candidate location sits on. `Local` and `Network`
/// carry the mount-table filesystem type (`ext4`, `nfs4`, …);
/// `Unknown` means the mount table could not be read (a non-Linux OS
/// today; the desktop follow-up adds macOS/Windows detection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemKind {
    Local(String),
    Network(String),
    Unknown,
}

/// Filesystem types refused for an active WAL vault: every network
/// filesystem the Linux mount table can name that a personal machine
/// plausibly mounts. Cloud-synchronized *directories* on a local
/// filesystem are a supported-configuration matter, not a detectable
/// mount type (see the plan: copying completed backups is the
/// supported route onto such storage).
pub const NETWORK_FILESYSTEMS: &[&str] = &[
    "nfs",
    "nfs4",
    "nfsd",
    "cifs",
    "smb",
    "smbfs",
    "smb2",
    "ncpfs",
    "afs",
    "ceph",
    "fuse.cephfs",
    "lustre",
    "fuse.lustre",
    "glusterfs",
    "fuse.glusterfs",
    "sshfs",
    "fuse.sshfs",
    "9p",
    "v9fs",
    "davfs",
    "fuse.davfs2",
];

/// The injectable mount-table source: returns `/proc/self/mounts`
/// text. Tests fixture network mounts through a closure; production
/// passes [`proc_mounts`].
pub type MountTable<'a> = &'a dyn Fn() -> std::io::Result<String>;

/// The production mount-table source: `/proc/self/mounts` on Linux,
/// an unsupported error elsewhere (reported as
/// [`FilesystemKind::Unknown`], never a guess).
pub fn proc_mounts() -> std::io::Result<String> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/self/mounts")
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(std::io::Error::other(
            "mount-table detection is implemented for Linux only (desktop follow-up covers \
             macOS and Windows)",
        ))
    }
}

/// Classify a (canonical) path against mount-table text: the entry
/// whose mount point is the longest component-wise prefix of the path
/// wins, exactly as mount resolution works; its filesystem type is
/// checked against [`NETWORK_FILESYSTEMS`]. No matching entry means
/// `Unknown`.
pub fn filesystem_kind(canonical_path: &Path, mounts: &str) -> FilesystemKind {
    let mut best: Option<(usize, String)> = None;
    for line in mounts.lines() {
        let mut fields = line.split(' ');
        let (Some(_device), Some(mount_point), Some(fstype)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let mount_point = unescape_mount_point(mount_point);
        let mount_path = PathBuf::from(mount_point);
        if canonical_path.starts_with(&mount_path) {
            let depth = mount_path.components().count();
            // Prefer the last of equal-depth entries: a later line in
            // the mount table is the overmounted, visible filesystem.
            let better = best.as_ref().is_none_or(|(current, _)| depth >= *current);
            if better {
                best = Some((depth, fstype.to_string()));
            }
        }
    }
    match best {
        Some((_, fstype)) if NETWORK_FILESYSTEMS.contains(&fstype.as_str()) => {
            FilesystemKind::Network(fstype)
        }
        Some((_, fstype)) => FilesystemKind::Local(fstype),
        None => FilesystemKind::Unknown,
    }
}

/// Decode the octal escapes `/proc/self/mounts` uses in mount points
/// (`\040` space, `\011` tab, `\012` newline, `\134` backslash).
fn unescape_mount_point(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let rest: String = chars.clone().take(3).collect();
        let decoded = match rest.as_str() {
            "040" => Some(' '),
            "011" => Some('\t'),
            "012" => Some('\n'),
            "134" => Some('\\'),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                for _ in 0..3 {
                    chars.next();
                }
            }
            None => out.push('\\'),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Location inspection
// ---------------------------------------------------------------------------

/// What the candidate directory already holds, from the identity file
/// and database found there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExistingContent {
    /// Nothing (ignoring dot-prefixed leftovers like an aborted move's
    /// staging): a fresh location the move may populate.
    Empty,
    /// A vault: `vault_id` from a validated `vault.json` (`None` for a
    /// legacy vault with a database but no identity yet), the
    /// database's schema version when a database exists, and the
    /// identity's recorded directory name when present.
    Vault {
        vault_id: Option<String>,
        schema_version: Option<i64>,
        directory_name: Option<String>,
    },
}

/// The result of a successful [`vault_location_inspect`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationInspection {
    /// The canonical candidate directory.
    pub path: PathBuf,
    /// The write probe succeeded (an unwritable location is a
    /// refusal, so this is always `true` in an `Ok` result — kept
    /// explicit for the UI fact).
    pub writable: bool,
    /// Free space on the candidate's filesystem (`fs4`, the same
    /// measurement the backup preflights use).
    pub free_bytes: u64,
    /// The filesystem type the mount table reports.
    pub filesystem: FilesystemKind,
    /// What the directory already holds.
    pub existing: ExistingContent,
}

/// Inspect a candidate vault location for the active vault: existence,
/// write access, filesystem type (network filesystems refused — WAL
/// constraint), free space, existing identity, and schema
/// compatibility. `active_vault_id` is the moving vault's UUID (from
/// the origin's `vault.json`); a destination already holding a
/// *different* vault's identity is refused, never overwritten.
///
/// Refusals are typed ([`MoveError`]); successes report the
/// measurements. The inspection never writes anything except its own
/// removed probe file and never mutates the directory's contents.
pub fn vault_location_inspect(
    path: &Path,
    active_vault_id: Option<&str>,
    mounts: MountTable<'_>,
) -> Result<LocationInspection, MoveError> {
    if !path.exists() {
        return Err(MoveError::DirectoryMissing(path.to_path_buf()));
    }
    if !path.is_dir() {
        return Err(MoveError::NotADirectory(path.to_path_buf()));
    }
    let canonical = path.canonicalize()?;
    let table =
        mounts().map_err(|e| MoveError::Vault(format!("cannot read the mount table: {e}")))?;
    let filesystem = filesystem_kind(&canonical, &table);
    if let FilesystemKind::Network(fstype) = &filesystem {
        return Err(MoveError::NetworkFilesystem {
            fstype: fstype.clone(),
            path: canonical,
        });
    }

    // Write probe: create, write, drop, remove. Any failure is a
    // not-writable refusal.
    let probe = canonical.join(format!(".vault-inspect-{}", std::process::id()));
    let probe_result = (|| -> std::io::Result<()> {
        let mut file = File::create(&probe)?;
        file.write_all(b"probe")?;
        file.sync_all()?;
        drop(file);
        Ok(())
    })();
    let _ = fs::remove_file(&probe);
    probe_result.map_err(|_| MoveError::NotWritable(canonical.clone()))?;

    let free_bytes = fs4::available_space(&canonical)
        .map_err(|e| MoveError::Vault(format!("cannot measure free space: {e}")))?;

    // Existing identity: read and validate when present.
    let identity_path = canonical.join(vault_session::IDENTITY_FILE);
    let identity = match fs::read_to_string(&identity_path) {
        Ok(text) => {
            let identity =
                vault_session::parse_identity(&text).map_err(|e| MoveError::InvalidIdentity {
                    path: identity_path.clone(),
                    reason: e.to_string(),
                })?;
            identity
                .validate_format()
                .map_err(|e| MoveError::InvalidIdentity {
                    path: identity_path.clone(),
                    reason: e.to_string(),
                })?;
            Some(identity)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(MoveError::Io(e)),
    };
    if let (Some(identity), Some(active)) = (&identity, active_vault_id) {
        if identity.vault_id != active {
            return Err(MoveError::DestinationVaultMismatch {
                path: canonical,
                destination_id: identity.vault_id.clone(),
                active_id: active.to_string(),
            });
        }
    }

    // Schema compatibility through the existing supported-version
    // refusal: a database this build cannot open refuses the location.
    let db_path = canonical.join(vault_session::DB_FILE);
    let schema_version = if db_path.is_file() {
        let conn =
            Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let version = read_schema_version(&conn).map_err(move_err)?;
        if version > SUPPORTED_SCHEMA_VERSION {
            return Err(MoveError::SchemaTooNew {
                db: version,
                supported: SUPPORTED_SCHEMA_VERSION,
            });
        }
        Some(version)
    } else {
        None
    };

    let existing = match (&identity, schema_version) {
        (Some(identity), schema_version) => ExistingContent::Vault {
            vault_id: Some(identity.vault_id.clone()),
            schema_version,
            directory_name: identity.directory_name.clone(),
        },
        (None, Some(schema_version)) => ExistingContent::Vault {
            vault_id: None,
            schema_version: Some(schema_version),
            directory_name: None,
        },
        (None, None) => {
            let unrelated = fs::read_dir(&canonical)?
                .flatten()
                .any(|entry| !entry.file_name().to_string_lossy().starts_with('.'));
            if unrelated {
                return Err(MoveError::UnrelatedDirectory(canonical));
            }
            ExistingContent::Empty
        }
    };

    Ok(LocationInspection {
        path: canonical,
        writable: true,
        free_bytes,
        filesystem,
        existing,
    })
}

// ---------------------------------------------------------------------------
// Move preview
// ---------------------------------------------------------------------------

/// One step of the phased move plan (plan section 8, steps 2–5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MovePhaseStep {
    /// The phase key (`quiesce`, `snapshot`, `copy`, `switch`,
    /// `restart`).
    pub name: &'static str,
    /// What the phase does, in the protocol's own words.
    pub description: &'static str,
}

/// The phased plan every move follows; the journal phase set mirrors
/// these steps.
pub fn move_plan_phases() -> Vec<MovePhaseStep> {
    vec![
        MovePhaseStep {
            name: "quiesce",
            description: "stop new jobs and watch events, finish or cancel current work, and \
                          hold writers stable (caller-managed, like the backup snapshot \
                          contract)",
        },
        MovePhaseStep {
            name: "snapshot",
            description: "create a consistent SQLite snapshot and the referenced object \
                          inventory through the backup snapshot path",
        },
        MovePhaseStep {
            name: "copy",
            description: "staged copy on the destination volume with checksum verification — \
                          always a verified copy, never an assumed rename",
        },
        MovePhaseStep {
            name: "switch",
            description: "close pool and media handles, then atomically update the \
                          device-local move journal and bootstrap pointer to the destination",
        },
        MovePhaseStep {
            name: "restart",
            description: "restart into the selected vault; the old copy is retained until \
                          separately removed",
        },
    ]
}

/// How the copy phase moves bytes. A single variant states the
/// invariant: cross-volume moves are verified copies, and the
/// same-volume case makes no rename assumption either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveStrategy {
    /// Copy into destination staging, checksum every byte against the
    /// inventory, only then finalize.
    VerifiedCopy,
}

/// The byte estimate of a move, derived from the backup snapshot
/// plan: the database snapshot, every referenced object, and the
/// portable settings file when present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MoveEstimate {
    pub db_bytes: u64,
    pub object_count: u64,
    pub object_bytes: u64,
    /// `Some` only when the vault carries a portable settings file.
    pub settings_bytes: Option<u64>,
    /// `db_bytes + object_bytes + settings_bytes`.
    pub total_bytes: u64,
}

/// What [`vault_move_preview`] reports: the phased plan, the estimate,
/// and the destination facts the plan was validated against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MovePreview {
    /// The canonical origin (active vault) directory.
    pub origin: PathBuf,
    /// The canonical destination directory.
    pub destination: PathBuf,
    /// The phased plan (quiesce → snapshot → copy → switch → restart).
    pub phases: Vec<MovePhaseStep>,
    /// The byte estimate for the copy phase.
    pub estimate: MoveEstimate,
    /// Whether origin and destination sit on different volumes
    /// (`st_dev` comparison; conservative `true` when undeterminable).
    pub cross_volume: bool,
    /// The copy strategy — always [`MoveStrategy::VerifiedCopy`].
    pub strategy: MoveStrategy,
    /// Free space measured at the destination when the preview ran.
    pub destination_free_bytes: u64,
}

/// Preview a move of the vault at `origin` into `destination`:
/// re-inspect the destination (all [`vault_location_inspect`]
/// refusals apply), take the consistent snapshot + object inventory
/// through the backup snapshot path, and derive the estimate from it.
///
/// `conn` is the vault's **writer** connection (through the pool's
/// write queue at the app layer), exactly as
/// [`backup::snapshot_for_backup`] documents: the snapshot runs with
/// new writes paused behind the queue. The quiesce phase itself is
/// caller-managed — this preview does not stop jobs.
///
/// Refuses while an unresolved move journal exists. The snapshot is
/// taken into the origin's recovery directory and discarded once
/// counted; nothing at the destination is written.
pub fn vault_move_preview(
    origin: &Path,
    destination: &Path,
    conn: &Connection,
    mounts: MountTable<'_>,
) -> Result<MovePreview, MoveError> {
    ensure_no_open_move(origin)?;
    let origin_canonical = origin
        .canonicalize()
        .map_err(|_| MoveError::DirectoryMissing(origin.to_path_buf()))?;
    let active_vault_id = read_origin_identity(&origin_canonical)?;

    let inspection = vault_location_inspect(destination, active_vault_id.as_deref(), mounts)?;

    // The estimate comes from the snapshot path itself (the same
    // counting a backup trusts), staged in the device-local recovery
    // directory and discarded once counted.
    let recovery = backup::recovery_dir(&origin_canonical);
    fs::create_dir_all(&recovery)?;
    let snapshot_tmp = recovery.join(format!(".move-preview-snapshot-{}", std::process::id()));
    let result = (|| -> Result<MoveEstimate, MoveError> {
        let plan = backup::snapshot_for_backup(&origin_canonical, conn, &snapshot_tmp)
            .map_err(move_err)?;
        let db_bytes = fs::metadata(&plan.snapshot)?.len();
        let object_bytes: u64 = plan.objects.iter().map(|o| o.size).sum();
        let settings_bytes = plan.has_settings.then(|| {
            fs::metadata(origin_canonical.join(SETTINGS_FILE))
                .map(|m| m.len())
                .unwrap_or(0)
        });
        let total_bytes = db_bytes + object_bytes + settings_bytes.unwrap_or(0);
        Ok(MoveEstimate {
            db_bytes,
            object_count: plan.objects.len() as u64,
            object_bytes,
            settings_bytes,
            total_bytes,
        })
    })();
    backup::discard_snapshot(&snapshot_tmp);
    let estimate = result?;

    Ok(MovePreview {
        origin: origin_canonical.clone(),
        destination: inspection.path.clone(),
        phases: move_plan_phases(),
        estimate,
        cross_volume: !same_volume(&origin_canonical, &inspection.path),
        strategy: MoveStrategy::VerifiedCopy,
        destination_free_bytes: inspection.free_bytes,
    })
}

/// The origin's vault UUID from its `vault.json` (`None` for a legacy
/// vault the session has not opened yet — the executor opens a session
/// first, which mints the identity).
fn read_origin_identity(origin: &Path) -> Result<Option<String>, MoveError> {
    match fs::read_to_string(origin.join(vault_session::IDENTITY_FILE)) {
        Ok(text) => {
            let identity =
                vault_session::parse_identity(&text).map_err(|e| MoveError::InvalidIdentity {
                    path: origin.join(vault_session::IDENTITY_FILE),
                    reason: e.to_string(),
                })?;
            identity.validate_format().map_err(identity_err(origin))?;
            Ok(Some(identity.vault_id))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(MoveError::Io(e)),
    }
}

fn identity_err(origin: &Path) -> impl Fn(SessionError) -> MoveError + '_ {
    move |e: SessionError| MoveError::InvalidIdentity {
        path: origin.join(vault_session::IDENTITY_FILE),
        reason: e.to_string(),
    }
}

/// Whether two existing paths sit on the same volume (`st_dev`).
/// `None` (treated as "not same volume" — a copy) when either stat
/// fails or the platform has no device ids.
#[cfg(unix)]
fn same_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => ma.dev() == mb.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_volume(_a: &Path, _b: &Path) -> bool {
    false
}

// ---------------------------------------------------------------------------
// The move journal
// ---------------------------------------------------------------------------

/// One phase of a journaled move. Every variant has exactly one
/// recovery action (see the module table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MovePhase {
    /// The journal is written; nothing has started. Recovery: cancel.
    Intended,
    /// Jobs/watch stopped, writers held (plan step 2 begun). Recovery:
    /// resume the origin.
    Quiesced,
    /// Consistent snapshot + object inventory staged (plan step 2
    /// done). Recovery: discard the staging snapshot, resume origin.
    Snapshotted,
    /// The staged copy is in progress on the destination (plan step
    /// 3). Recovery: discard the partial copy, resume origin.
    Copying,
    /// The staged copy finished and every checksum verified (plan step
    /// 3 done). Recovery: roll back to the origin and remove the
    /// verified duplicate (no second writable vault).
    Copied,
    /// The bootstrap pointer names the destination (plan step 4).
    /// Recovery: activate the destination; retain the origin copy.
    PointerSwitched,
    /// The move finished (plan step 5). Recovery: nothing.
    Completed,
}

/// Both endpoints of a move: the vault's stable identity and the path
/// it moves from/to. Both sides carry the same UUID — one vault, two
/// locations — so a recovered journal never guesses which identity is
/// authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultEndpoint {
    /// The stable vault UUID (as recorded in `vault.json`).
    pub vault_id: String,
    /// The vault directory path at this end.
    pub path: PathBuf,
}

/// The device-local move journal, serde JSON under the recovery
/// directory with its own file name — never on the destination volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveJournal {
    /// The journal format version ([`MOVE_JOURNAL_VERSION`]).
    pub version: u32,
    /// The phase the move reached.
    pub state: MovePhase,
    /// RFC 3339 UTC — when the move started.
    pub created_at: String,
    /// The moving vault's endpoint at the origin.
    pub origin: VaultEndpoint,
    /// The moving vault's endpoint at the destination.
    pub destination: VaultEndpoint,
    /// The staged-copy directory inside the destination root.
    pub destination_staging: PathBuf,
    /// The snapshot/inventory staging inside the origin's recovery
    /// directory.
    pub origin_staging: PathBuf,
    /// The recoverable intent, in words: which vault moves where.
    pub intent: String,
}

/// The move journal's path (inside the origin's recovery directory).
pub fn move_journal_path(origin: &Path) -> PathBuf {
    backup::recovery_dir(origin).join(MOVE_JOURNAL_FILE)
}

/// Read the move journal; `Ok(None)` when none exists. A journal with
/// a newer format version refuses to resolve.
pub fn read_move_journal(origin: &Path) -> Result<Option<MoveJournal>, MoveError> {
    let path = move_journal_path(origin);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let journal: MoveJournal = serde_json::from_str(&text)?;
            if journal.version > MOVE_JOURNAL_VERSION {
                return Err(MoveError::Vault(format!(
                    "move journal version {} is newer than this build supports ({})",
                    journal.version, MOVE_JOURNAL_VERSION
                )));
            }
            Ok(Some(journal))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(MoveError::Io(e)),
    }
}

/// Write the move journal atomically (staged rename + directory
/// fsync), creating the recovery directory when needed. Public for the
/// later commit executor and tests.
pub fn write_move_journal(origin: &Path, journal: &MoveJournal) -> Result<(), MoveError> {
    let recovery = backup::recovery_dir(origin);
    fs::create_dir_all(&recovery)?;
    write_json_atomic(&move_journal_path(origin), journal).map_err(move_err)?;
    fsync_dir(&recovery).map_err(move_err)?;
    Ok(())
}

/// Build a fresh journal at [`MovePhase::Intended`] for moving the
/// vault at `origin` (whose `vault.json` must already exist — open a
/// session first for a legacy vault) into `destination`. The later
/// commit executor writes this before touching anything.
pub fn new_move_journal(origin: &Path, destination: &Path) -> Result<MoveJournal, MoveError> {
    let origin_canonical = origin
        .canonicalize()
        .map_err(|_| MoveError::DirectoryMissing(origin.to_path_buf()))?;
    let destination_canonical = destination
        .canonicalize()
        .map_err(|_| MoveError::DirectoryMissing(destination.to_path_buf()))?;
    let Some(vault_id) = read_origin_identity(&origin_canonical)? else {
        return Err(MoveError::Vault(format!(
            "the vault at {} has no identity yet — open a session first (a legacy vault gains \
             its identity on first open)",
            origin_canonical.display()
        )));
    };
    Ok(MoveJournal {
        version: MOVE_JOURNAL_VERSION,
        state: MovePhase::Intended,
        created_at: now_rfc3339_utc(),
        origin: VaultEndpoint {
            vault_id: vault_id.clone(),
            path: origin_canonical.clone(),
        },
        destination: VaultEndpoint {
            vault_id: vault_id.clone(),
            path: destination_canonical.clone(),
        },
        destination_staging: destination_canonical.join(DESTINATION_STAGING_DIR),
        origin_staging: backup::recovery_dir(&origin_canonical).join(ORIGIN_STAGING_DIR),
        intent: format!(
            "move vault {vault_id} from {} to {}",
            origin_canonical.display(),
            destination_canonical.display()
        ),
    })
}

/// Refuse while a move journal is unresolved (anything but
/// [`MovePhase::Completed`]): resolve it first.
pub fn ensure_no_open_move(origin: &Path) -> Result<(), MoveError> {
    if let Some(journal) = read_move_journal(origin)? {
        if journal.state != MovePhase::Completed {
            return Err(MoveError::UnresolvedJournal {
                state: journal.state,
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Journal resolution (startup, before any pool opens)
// ---------------------------------------------------------------------------

/// What [`recover_interrupted_move`] found and did. Every interrupted
/// phase resolves to exactly one variant, and exactly one vault is
/// authoritative afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveRecovery {
    /// No journal exists: nothing was in flight.
    None,
    /// The last move completed; the destination is the active vault.
    AlreadyCompleted { journal: MoveJournal },
    /// An `intended` journal was cancelled before anything started;
    /// the origin is authoritative and untouched.
    Cancelled { journal: MoveJournal },
    /// A `quiesced`/`snapshotted`/`copying` journal was resolved to
    /// the intact origin; the listed staging artifacts were discarded.
    ResumedOrigin {
        journal: MoveJournal,
        discarded: Vec<PathBuf>,
    },
    /// A `copied` journal rolled **back**: the verified duplicate at
    /// the destination was removed, so the origin stays the one
    /// writable vault.
    RolledBackToOrigin {
        journal: MoveJournal,
        discarded_copy: PathBuf,
    },
    /// A `pointer_switched` journal rolled **forward**: the
    /// destination (verified present with the journal's identity) is
    /// the vault to open; the origin copy is retained until separately
    /// removed and never writable through this app afterwards.
    ActivatedDestination { journal: MoveJournal },
}

/// Resolve an interrupted move at startup, **before a pool is
/// opened**. Each phase has one defined action (see the module
/// table); the origin is authoritative whenever the commit executor
/// is absent — this build has none, so every pre-pointer phase
/// resolves to the origin with the destination staging removed (a
/// duplicate writable vault is never left behind).
///
/// A `pointer_switched` journal whose destination is missing or holds
/// the wrong identity returns [`MoveError::VaultUnavailable`] /
/// identity refusals with the journal left unresolved: the recovery
/// screen shows the fact, the user reattaches the location, and no
/// empty replacement archive is ever created.
pub fn recover_interrupted_move(origin: &Path) -> Result<MoveRecovery, MoveError> {
    let Some(journal) = read_move_journal(origin)? else {
        return Ok(MoveRecovery::None);
    };
    match journal.state {
        MovePhase::Completed => Ok(MoveRecovery::AlreadyCompleted { journal }),
        MovePhase::Intended => {
            // Nothing started; drop any leftover staging best-effort.
            let _ = fs::remove_dir_all(&journal.destination_staging);
            let _ = fs::remove_dir_all(&journal.origin_staging);
            complete_journal(origin, journal.clone())?;
            Ok(MoveRecovery::Cancelled { journal })
        }
        MovePhase::Quiesced | MovePhase::Snapshotted | MovePhase::Copying => {
            // The origin was never touched. Discard both staging areas
            // — a partial copy must not survive as a second vault —
            // then resume the origin. Failures here keep the journal
            // unresolved so the next startup retries.
            let mut discarded = Vec::new();
            for staging in [&journal.origin_staging, &journal.destination_staging] {
                if fs::remove_dir_all(staging).is_ok() {
                    discarded.push(staging.clone());
                } else if staging.exists() {
                    return Err(MoveError::Io(std::io::Error::other(format!(
                        "cannot discard the interrupted move's staging at {}",
                        staging.display()
                    ))));
                }
            }
            complete_journal(origin, journal.clone())?;
            Ok(MoveRecovery::ResumedOrigin { journal, discarded })
        }
        MovePhase::Copied => {
            // The copy verified, but the pointer never switched and
            // this build has no executor to adopt it: the origin stays
            // authoritative and the duplicate is removed — never two
            // writable vaults.
            if journal.destination_staging.exists() {
                fs::remove_dir_all(&journal.destination_staging)?;
            }
            let _ = fs::remove_dir_all(&journal.origin_staging);
            let discarded_copy = journal.destination_staging.clone();
            complete_journal(origin, journal.clone())?;
            Ok(MoveRecovery::RolledBackToOrigin {
                journal,
                discarded_copy,
            })
        }
        MovePhase::PointerSwitched => {
            // The pointer already names the destination: it is the
            // vault to open, provided it is really there and really
            // this vault. Otherwise the journal stays unresolved and
            // the caller shows the typed fact.
            if !journal.destination.path.is_dir()
                || !journal
                    .destination
                    .path
                    .join(vault_session::DB_FILE)
                    .is_file()
            {
                return Err(MoveError::VaultUnavailable(
                    journal.destination.path.clone(),
                ));
            }
            let identity_path = journal.destination.path.join(vault_session::IDENTITY_FILE);
            let identity = vault_session::parse_identity(&fs::read_to_string(&identity_path)?)
                .and_then(|i| i.validate_format().map(|_| i))
                .map_err(|e| MoveError::InvalidIdentity {
                    path: identity_path,
                    reason: e.to_string(),
                })?;
            if identity.vault_id != journal.destination.vault_id {
                return Err(MoveError::InvalidIdentity {
                    path: journal.destination.path.join(vault_session::IDENTITY_FILE),
                    reason: format!(
                        "the destination holds vault {} but the journal moved {}",
                        identity.vault_id, journal.destination.vault_id
                    ),
                });
            }
            // Staging leftovers are worthless once finalized.
            let _ = fs::remove_dir_all(&journal.destination_staging);
            let _ = fs::remove_dir_all(&journal.origin_staging);
            complete_journal(origin, journal.clone())?;
            Ok(MoveRecovery::ActivatedDestination { journal })
        }
    }
}

/// Mark the journal completed (its phases are over; the record stays
/// for the "what happened" UX, like the switch journal does).
fn complete_journal(origin: &Path, mut journal: MoveJournal) -> Result<(), MoveError> {
    journal.state = MovePhase::Completed;
    write_move_journal(origin, &journal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_mount_point_prefix_wins() {
        let mounts = "\
proc /proc proc rw 0 0
/dev/sda1 / ext4 rw 0 0
nas:/export/shared /mnt/nas nfs4 rw 0 0
";
        assert_eq!(
            filesystem_kind(Path::new("/home/user/vault"), mounts),
            FilesystemKind::Local("ext4".to_string())
        );
        assert_eq!(
            filesystem_kind(Path::new("/mnt/nas/vault"), mounts),
            FilesystemKind::Network("nfs4".to_string())
        );
        // A path no mount point covers is unknown, never a guess.
        assert_eq!(
            filesystem_kind(Path::new("/somewhere/else"), "tmpfs /mnt/x tmpfs rw 0 0\n"),
            FilesystemKind::Unknown
        );
    }

    #[test]
    fn mount_point_escapes_decode_before_matching() {
        let mounts = "nas:/x /mnt/with\\040space nfs4 rw 0 0\n/dev/sda1 / ext4 rw 0 0\n";
        assert_eq!(
            filesystem_kind(Path::new("/mnt/with space/vault"), mounts),
            FilesystemKind::Network("nfs4".to_string())
        );
    }

    #[test]
    fn every_network_filesystem_name_is_refused() {
        let fstype = "cifs";
        assert!(NETWORK_FILESYSTEMS.contains(&fstype));
        let mounts = format!("//server/share /mnt/share {fstype} rw 0 0\n");
        assert_eq!(
            filesystem_kind(Path::new("/mnt/share/vault"), &mounts),
            FilesystemKind::Network(fstype.to_string())
        );
    }
}

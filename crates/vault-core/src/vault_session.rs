//! The vault session seam (A4): one owner of vault identity, the
//! exclusive session lock, and the derived vault paths.
//!
//! Every consumer that needs the vault's database file, media store, or
//! settings file resolves them through one [`VaultSession`] so later
//! tickets can introduce user-selected vault locations without touching
//! call sites again. This module has no Tauri (or any UI) dependencies;
//! the desktop app opens exactly one session at startup and delegates
//! path resolution to it.
//!
//! [`VaultSession::open`] resolves a vault directory as follows:
//!
//! 1. Acquire the exclusive advisory lock first: a `vault.lock` file
//!    inside the vault directory, held for the session lifetime (the
//!    open file handle is the guard — dropping the session releases
//!    the lock). A second open of the same directory fails with the
//!    distinct [`SessionError::Locked`].
//! 2. If the identity file (`vault.json`) exists, validate it: known
//!    format marker, supported format version, a parseable UUID, and a
//!    recorded directory name that matches the opened directory's name.
//!    A mismatch refuses the open — that directory is a copy of a vault
//!    under a different name, not the recorded vault. The identity
//!    records the directory *name*, never an absolute path, so a future
//!    move protocol can rewrite it.
//! 3. If the identity file is absent (a legacy vault created before
//!    this seam existed), open in place and — after the lock is held —
//!    write the file with a fresh v4 UUID. An absent file is therefore
//!    always a legacy vault, never an error.
//!
//! The default vault location stays exactly where it was before this
//! seam: the OS application-data directory, bootstrapped once by the
//! desktop app's setup and opened here.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::settings::SETTINGS_FILE;

/// The vault identity file: a stable UUID plus format metadata, written
/// on the first successful open of a legacy vault.
const IDENTITY_FILE: &str = "vault.json";

/// The exclusive advisory session lock file. Its bytes are never
/// meaningful — the lock lives in the open file handle.
const LOCK_FILE: &str = "vault.lock";

/// The canonical archive database inside the vault directory.
const DB_FILE: &str = "vault.db";

/// The immutable media object store directory inside the vault
/// directory (see [`crate::object_store`]).
const MEDIA_DIR: &str = "media";

/// The identity-file format marker. A file carrying a different marker
/// is not a mailchat vault identity and refuses to open.
const IDENTITY_FORMAT: &str = "mailchat-vault";

/// The current identity-file format version. Identities with a newer
/// version refuse to open (an older build cannot interpret them).
const IDENTITY_FORMAT_VERSION: u32 = 1;

/// Why a vault directory refused to open.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The exclusive advisory lock is already held — the vault is open
    /// in another session (another app instance, or this one).
    #[error("this vault is already open in another session (its advisory vault.lock is held) — close the other session first")]
    Locked,
    /// The identity file exists but is not a valid identity for this
    /// build: unknown format marker, unsupported version, or an
    /// unparseable vault UUID.
    #[error("the vault identity file is invalid: {0}")]
    InvalidIdentity(String),
    /// The identity file records a different directory name than the
    /// one being opened: this directory is a copy of a vault under a
    /// different name, not the recorded vault, and must not open in
    /// place.
    #[error(
        "the vault identity records directory name {recorded:?} but this directory is \
         {opened:?} — a vault copied under a different name must not open in place"
    )]
    DirectoryNameMismatch {
        /// The directory name recorded in the identity file.
        recorded: String,
        /// The name of the directory whose open was refused.
        opened: String,
    },
    /// An I/O error while opening the vault.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    /// A JSON error while reading or writing the identity file.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// The identity file's contents (`vault.json`). The vault UUID is a
/// string on disk ([`Uuid`] has no serde feature in this workspace) and
/// is parsed on read; the recorded location is the directory *name*
/// only, so an identity survives parent renames and a future move
/// protocol can rewrite it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultIdentity {
    /// The format marker; must be [`IDENTITY_FORMAT`].
    format: String,
    /// The identity format version; must be exactly the version this
    /// build writes (newer refuses).
    format_version: u32,
    /// The stable vault UUID (v4 when minted here), as a string.
    vault_id: String,
    /// The directory name at the time this identity was last written
    /// (`None` only for a path with no file name, e.g. a filesystem
    /// root — never the case for a real vault).
    directory_name: Option<String>,
}

impl VaultIdentity {
    /// A fresh identity for a legacy vault opened in place.
    fn minted(dir: &Path) -> Self {
        Self {
            format: IDENTITY_FORMAT.to_string(),
            format_version: IDENTITY_FORMAT_VERSION,
            vault_id: Uuid::new_v4().to_string(),
            directory_name: directory_name_of(dir),
        }
    }

    /// Validate an identity read from disk against the directory being
    /// opened; returns the parsed vault UUID.
    fn validate(&self, dir: &Path) -> Result<Uuid, SessionError> {
        if self.format != IDENTITY_FORMAT {
            return Err(SessionError::InvalidIdentity(format!(
                "unknown format marker {:?} (expected {:?})",
                self.format, IDENTITY_FORMAT
            )));
        }
        if self.format_version != IDENTITY_FORMAT_VERSION {
            return Err(SessionError::InvalidIdentity(format!(
                "unsupported format version {} (this build writes {})",
                self.format_version, IDENTITY_FORMAT_VERSION
            )));
        }
        let vault_id = Uuid::parse_str(&self.vault_id).map_err(|e| {
            SessionError::InvalidIdentity(format!("vault UUID does not parse: {e}"))
        })?;
        // The name check binds the identity to this directory *name*,
        // never an absolute path: the same vault directory name under a
        // different parent still opens (a move rewrites the name only
        // when it changed).
        if let (Some(recorded), Some(opened)) = (&self.directory_name, directory_name_of(dir)) {
            if recorded != &opened {
                return Err(SessionError::DirectoryNameMismatch {
                    recorded: recorded.clone(),
                    opened,
                });
            }
        }
        Ok(vault_id)
    }
}

/// The directory's own name (`"Vault"` in `/a/b/Vault`), lossily; `None`
/// for a path with no file name (a filesystem root).
fn directory_name_of(dir: &Path) -> Option<String> {
    dir.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// One open vault: the single owner of the vault identity, the
/// exclusive advisory lock, and the derived vault paths.
///
/// The session holds the lock for its lifetime — dropping it releases
/// the vault for the next open. Cloning is deliberately absent: exactly
/// one session per vault per process.
#[derive(Debug)]
pub struct VaultSession {
    vault_id: Uuid,
    dir: PathBuf,
    db_path: PathBuf,
    media_dir: PathBuf,
    settings_path: PathBuf,
    /// The advisory-lock guard: the open handle *is* the held lock
    /// (flock/LockFileEx), so it must live as long as the session.
    _lock: File,
}

impl VaultSession {
    /// Open (or bootstrap) the vault at `dir`, per the module docs:
    /// lock first, then validate or mint the identity, never creating
    /// anything beyond the lock and identity files.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let dir = dir.into();
        let lock = acquire_lock(&dir)?;
        let identity_path = dir.join(IDENTITY_FILE);
        let vault_id = match fs::read_to_string(&identity_path) {
            Ok(text) => {
                let identity: VaultIdentity = serde_json::from_str(&text)
                    .map_err(|e| SessionError::InvalidIdentity(format!("not valid JSON: {e}")))?;
                identity.validate(&dir)?
            }
            // Absent identity: a legacy vault — open in place and gain
            // an identity with a fresh UUID on this first open.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let identity = VaultIdentity::minted(&dir);
                write_identity(&identity_path, &identity)?;
                Uuid::parse_str(&identity.vault_id).map_err(|e| {
                    SessionError::InvalidIdentity(format!("minted UUID does not parse: {e}"))
                })?
            }
            Err(e) => return Err(SessionError::Io(e)),
        };
        Ok(Self {
            vault_id,
            db_path: dir.join(DB_FILE),
            media_dir: dir.join(MEDIA_DIR),
            settings_path: dir.join(SETTINGS_FILE),
            dir,
            _lock: lock,
        })
    }

    /// The stable vault UUID: recorded in `vault.json`, minted on the
    /// first open of a legacy vault, stable across sessions.
    pub fn vault_id(&self) -> Uuid {
        self.vault_id
    }

    /// The vault root directory — the path media and object-store code
    /// receives (`ObjectStore::new(&dir)` resolves `media/` under it).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The canonical archive database path (`vault.db`).
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// The media object-store directory (`media/`).
    pub fn media_dir(&self) -> &Path {
        &self.media_dir
    }

    /// The portable vault settings path (`settings.json`).
    pub fn settings_path(&self) -> &Path {
        &self.settings_path
    }
}

/// Acquire the exclusive advisory lock on the vault's `vault.lock`,
/// creating the file if needed. The returned handle owns the held
/// lock; dropping it releases it.
fn acquire_lock(dir: &Path) -> Result<File, SessionError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILE))?;
    // Fully qualified: `File` also has an inherent std `try_lock` since
    // Rust 1.89; this seam stays on fs4's lock (same call site style as
    // the rest of the workspace).
    match fs4::FileExt::try_lock(&file) {
        Ok(()) => Ok(file),
        Err(fs4::TryLockError::WouldBlock) => Err(SessionError::Locked),
        Err(fs4::TryLockError::Error(e)) => Err(SessionError::Io(e)),
    }
}

/// Write the identity file by staged rename: the JSON lands in a
/// private temp sibling first and only a fully written file is renamed
/// into place, so a crash mid-write can never leave a half-written
/// identity that would refuse the next open. The lock is already held
/// when this runs, so the temp name needs no uniquifier.
fn write_identity(path: &Path, identity: &VaultIdentity) -> Result<(), SessionError> {
    let tmp = path.with_file_name(format!("{IDENTITY_FILE}.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(identity)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_identity_passes_validation_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let identity = VaultIdentity::minted(dir.path());
        let id = identity.validate(dir.path()).expect("minted validates");
        assert_eq!(id.to_string(), identity.vault_id);
        assert_eq!(identity.format, IDENTITY_FORMAT);
        assert_eq!(identity.format_version, IDENTITY_FORMAT_VERSION);
    }

    #[test]
    fn validation_refuses_foreign_markers_versions_and_uuids() {
        let dir = tempfile::tempdir().unwrap();
        let base = VaultIdentity::minted(dir.path());
        let mut foreign = base.clone();
        foreign.format = "some-other-product".to_string();
        assert!(matches!(
            foreign.validate(dir.path()),
            Err(SessionError::InvalidIdentity(_))
        ));
        let mut future = base.clone();
        future.format_version = IDENTITY_FORMAT_VERSION + 1;
        assert!(matches!(
            future.validate(dir.path()),
            Err(SessionError::InvalidIdentity(_))
        ));
        let mut bad_uuid = base.clone();
        bad_uuid.vault_id = "not-a-uuid".to_string();
        assert!(matches!(
            bad_uuid.validate(dir.path()),
            Err(SessionError::InvalidIdentity(_))
        ));
    }
}

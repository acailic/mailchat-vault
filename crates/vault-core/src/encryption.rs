//! At-rest encryption for the archive database (SQLCipher).
//!
//! The bundled SQLite is compiled as SQLCipher (the workspace `rusqlite`
//! dependency uses the `bundled-sqlcipher-vendored-openssl` feature), so
//! the archive file can be stored AES-256 encrypted per page with an
//! HMAC over every page. Encryption is **opt-in**: it only engages when
//! this crate is built with the `encryption` feature *and* a key is
//! supplied on open — a plaintext vault, a build without the feature,
//! or an open without a key behave exactly as before.
//!
//! Key custody belongs to the OS keyring at the app layer; this module
//! deliberately stays keyring-free. It provides:
//!
//! - [`VaultKey`]: the 256-bit key as an inert byte carrier, with hex
//!   forms for the user-printable restore envelope ([`VaultKey::as_hex`],
//!   [`VaultKey::from_hex`]) — the only "escrow" this project offers is
//!   the envelope the user prints and stores themselves;
//! - file classification from the first database page ([`VaultFileKind`]):
//!   a plaintext SQLite file starts with the standard 16-byte magic
//!   string, an encrypted one does not;
//! - [`open_connection`]: the one keyed-open helper every connection
//!   site routes through. It applies `PRAGMA key` immediately after
//!   opening — before `journal_mode`, custom functions, or any migration
//!   pragma — classifies the file first so unkeyed plaintext files open
//!   normally, refuses an encrypted vault without its key with a clear
//!   error (no read-only fallback, by design), and verifies a supplied
//!   key by reading `sqlite_master` before any write can happen.
//!
//! What encryption does **not** protect: process memory, OS swap, and
//! every plaintext vault or backup made before a vault is converted —
//! see docs/security.md for the full honest statement.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::{Error, Result};

/// The standard 16-byte magic every plaintext SQLite database starts
/// with (including the trailing NUL).
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\x00";

/// The length of a raw SQLCipher key in bytes (AES-256).
pub const KEY_LEN: usize = 32;

/// A 256-bit vault key for the SQLCipher-encrypted archive.
///
/// An inert byte carrier: generating, storing, and fetching the key are
/// the callers' concern (the app keeps it in the OS keyring; tests
/// inject raw keys). `Debug` is implemented manually and redacts the
/// bytes so a key can never leak into a log line by accident.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VaultKey([u8; KEY_LEN]);

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey(<redacted>)")
    }
}

impl VaultKey {
    /// Adopt 32 raw key bytes.
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Parse the canonical lowercase-hex form produced by
    /// [`VaultKey::as_hex`] (and by the printable envelope, whose
    /// grouping whitespace is ignored).
    pub fn from_hex(hex: &str) -> Result<Self> {
        let hex: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
        if hex.len() != KEY_LEN * 2 {
            return Err(Error::Encryption(format!(
                "a vault key is {KEY_LEN} bytes ({} hex characters); got {} characters",
                KEY_LEN * 2,
                hex.len()
            )));
        }
        let mut bytes = [0u8; KEY_LEN];
        for (i, slot) in bytes.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| {
                Error::Encryption("vault key hex contains a non-hex character".into())
            })?;
        }
        Ok(Self(bytes))
    }

    /// Generate a fresh key from the OS cryptographically secure
    /// randomness source. Only available with the `encryption` feature:
    /// a build without the feature never mints keys.
    #[cfg(feature = "encryption")]
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; KEY_LEN];
        getrandom::fill(&mut bytes).map_err(|e| {
            Error::Encryption(format!("cannot read OS randomness for a vault key: {e}"))
        })?;
        Ok(Self(bytes))
    }

    /// The raw key bytes.
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// The canonical lowercase-hex form (64 characters).
    pub fn as_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// The user-printable key-restore envelope: the one form in which a
/// vault key may leave the machine, because the user printed it and
/// stores it themselves. There is no escrow, no cloud sync, and no
/// recovery without this text — losing it means losing the archive.
pub fn printable_key_envelope(key: &VaultKey) -> String {
    let hex = key.as_hex();
    let mut groups = Vec::with_capacity(16);
    for chunk in hex.as_bytes().chunks(8) {
        groups.push(String::from_utf8_lossy(chunk).into_owned());
    }
    format!(
        "MAILCHAT VAULT KEY — print this and store it somewhere safe.\n\
         It is the only way to open the encrypted archive (and its\n\
         encrypted backups) if the computer's keyring is lost.\n\
         Without it the archive cannot be opened by anyone, including us.\n\n\
         {}\n\n\
         To restore: re-enter the {KEY_LEN} byte key exactly, in hex,\n\
         when the app asks for the vault key.\n",
        groups.join(" ")
    )
}

/// What the first database page says a file is. The classification is
/// honest but coarse: `Encrypted` means "not a plaintext SQLite header"
/// — normally a SQLCipher database, and every downstream open refuses
/// clearly if it turns out to be something else instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultFileKind {
    /// No file at the path: an open would create a new database
    /// (encrypted when a key is supplied, plaintext otherwise).
    Missing,
    /// The file starts with the standard SQLite magic string.
    Plaintext,
    /// The file exists with a non-SQLite header — an encrypted
    /// (SQLCipher) database as far as this build can tell.
    Encrypted,
}

/// Classify a database file from its first page: a plaintext SQLite
/// database starts with the standard 16-byte magic string, an encrypted
/// one does not. Reading only the header keeps this cheap and safe on
/// files this build cannot open. A zero-byte file is the documented
/// "create when missing" shape, so it classifies as [`VaultFileKind::Missing`].
pub fn classify_vault_file(path: &Path) -> Result<VaultFileKind> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(VaultFileKind::Missing);
        }
        Err(e) => return Err(e.into()),
    };
    let mut header = [0u8; 16];
    let mut read = 0;
    loop {
        let n = file.read(&mut header[read..])?;
        if n == 0 {
            break;
        }
        read += n;
        if read == header.len() {
            break;
        }
    }
    Ok(match read {
        // Zero bytes: a fresh database, exactly like a missing file.
        0 => VaultFileKind::Missing,
        // A file this short cannot carry a plaintext header — report
        // it as encrypted-looking and let the keyed open (which will
        // fail its key check) produce the honest error.
        _ if read < header.len() => VaultFileKind::Encrypted,
        _ if &header == SQLITE_MAGIC => VaultFileKind::Plaintext,
        _ => VaultFileKind::Encrypted,
    })
}

/// Convenience wrapper over [`classify_vault_file`] for status
/// reporting: `false` for a plaintext (or not-yet-existing) file.
pub fn vault_file_is_encrypted(path: &Path) -> Result<bool> {
    Ok(matches!(
        classify_vault_file(path)?,
        VaultFileKind::Encrypted
    ))
}

/// A connection opened by [`open_connection`], with the facts the open
/// established.
pub(crate) struct OpenedConnection {
    pub conn: Connection,
    /// Whether the opened file is encrypted — the honest fact the read
    /// models surface (`archive_health`, `vault_location_inspect`).
    pub encrypted: bool,
}

/// The one keyed-open helper every archive connection routes through,
/// applied **before** `journal_mode`, custom functions, or any migration
/// pragma:
///
/// - a missing file opens with the key applied (a new encrypted vault)
///   when one is supplied, plaintext otherwise;
/// - a plaintext file opens normally, key or no key — an existing
///   plaintext vault is never silently encrypted (conversion is a
///   separate, explicit operation);
/// - an encrypted file without a key is refused with a clear error —
///   no read-only fallback, by design;
/// - an encrypted file with a key gets `PRAGMA key` applied and the key
///   verified by reading `sqlite_master`, so a wrong key fails here,
///   before any write.
///
/// Without the `encryption` feature the helper behaves exactly as a
/// plain open for plaintext and new files, and refuses clearly when
/// anything would need encryption.
pub(crate) fn open_connection(
    path: &Path,
    flags: OpenFlags,
    key: Option<&VaultKey>,
) -> Result<OpenedConnection> {
    match classify_vault_file(path)? {
        VaultFileKind::Missing => match key {
            Some(key) => {
                #[cfg(feature = "encryption")]
                {
                    let conn = Connection::open_with_flags(path, flags)?;
                    apply_key(&conn, key)?;
                    Ok(OpenedConnection {
                        conn,
                        encrypted: true,
                    })
                }
                #[cfg(not(feature = "encryption"))]
                {
                    let _ = key;
                    Err(Error::KeyWithoutEncryption {
                        path: path.display().to_string(),
                    })
                }
            }
            None => Ok(OpenedConnection {
                conn: Connection::open_with_flags(path, flags)?,
                encrypted: false,
            }),
        },
        VaultFileKind::Plaintext => Ok(OpenedConnection {
            conn: Connection::open_with_flags(path, flags)?,
            encrypted: false,
        }),
        VaultFileKind::Encrypted => {
            #[cfg(feature = "encryption")]
            {
                let key = key.ok_or_else(|| Error::EncryptedVaultLocked {
                    path: path.display().to_string(),
                })?;
                let conn = Connection::open_with_flags(path, flags)?;
                apply_key(&conn, key)?;
                verify_key(&conn, path)?;
                Ok(OpenedConnection {
                    conn,
                    encrypted: true,
                })
            }
            #[cfg(not(feature = "encryption"))]
            {
                Err(Error::EncryptionUnsupported {
                    path: path.display().to_string(),
                })
            }
        }
    }
}

/// Hand the raw key to SQLCipher as a blob literal (`PRAGMA key =
/// "x'…'"`, the documented raw-key form): exactly the 256 key bytes,
/// no passphrase derivation on top.
#[cfg(feature = "encryption")]
fn apply_key(conn: &Connection, key: &VaultKey) -> Result<()> {
    let statement = format!("PRAGMA key = \"x'{}'\";", key.as_hex());
    conn.execute_batch(&statement)?;
    Ok(())
}

/// Verify the applied key by forcing a real read of page 1: with a
/// wrong key SQLCipher fails the page HMAC and SQLite reports the file
/// as "not a database" — mapped here to the clear refusal, before any
/// write of ours can touch the file.
#[cfg(feature = "encryption")]
fn verify_key(conn: &Connection, path: &Path) -> Result<()> {
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
        .map_err(|e| match e {
            // SQLITE_NOTADB: the page-1 HMAC check failed — the applied
            // key does not decrypt this file.
            rusqlite::Error::SqliteFailure(ffi, _)
                if ffi.extended_code == rusqlite::ffi::SQLITE_NOTADB =>
            {
                Error::WrongVaultKey {
                    path: path.display().to_string(),
                }
            }
            other => other.into(),
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let key = VaultKey::from_bytes([7u8; KEY_LEN]);
        assert_eq!(key.as_hex().len(), 64);
        assert_eq!(VaultKey::from_hex(&key.as_hex()).unwrap(), key);
        // Whitespace from the printable envelope's grouping is accepted.
        let spaced = key
            .as_hex()
            .chars()
            .collect::<Vec<_>>()
            .chunks(8)
            .map(|c| c.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(VaultKey::from_hex(&spaced).unwrap(), key);
        assert!(VaultKey::from_hex("00").is_err());
        assert!(VaultKey::from_hex(&"z".repeat(64)).is_err());
    }

    #[test]
    fn debug_redacts_the_key() {
        let key = VaultKey::from_bytes([9u8; KEY_LEN]);
        assert!(!format!("{key:?}").contains('9'));
    }

    #[test]
    fn envelope_names_the_stakes() {
        let key = VaultKey::from_bytes([1u8; KEY_LEN]);
        let text = printable_key_envelope(&key);
        assert!(text.contains("VAULT KEY"));
        assert!(text.contains(&key.as_hex()[..8]));
    }

    #[test]
    fn classify_reads_the_magic() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.db");
        std::fs::write(&plain, SQLITE_MAGIC).unwrap();
        assert_eq!(
            classify_vault_file(&plain).unwrap(),
            VaultFileKind::Plaintext
        );
        let enc = dir.path().join("enc.db");
        std::fs::write(&enc, [0u8; 64]).unwrap();
        assert_eq!(classify_vault_file(&enc).unwrap(), VaultFileKind::Encrypted);
        assert_eq!(
            classify_vault_file(&dir.path().join("absent.db")).unwrap(),
            VaultFileKind::Missing
        );
    }
}

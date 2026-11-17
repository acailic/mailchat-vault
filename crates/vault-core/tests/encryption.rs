// At-rest archive encryption (SQLCipher), slice 1 contracts, proven
// with raw injected keys (the headless runner has no secret service;
// the OS-keyring adapter is the app layer's concern):
//
// - with the `encryption` feature and a supplied key, a newly created
//   vault is stored encrypted and round-trips open/write/read, while
//   its file cannot be opened as a plain SQLite database;
// - a wrong key or a missing key on an encrypted vault is a clean,
//   typed refusal before any write — no read-only fallback, and the
//   file is left byte-identical (no corruption);
// - a plaintext vault opens with the feature on (key offered or not)
//   and both read models — `archive_health` and
//   `vault_location_inspect` — report `encrypted: false` honestly;
// - an encrypted vault reports `encrypted: true` in both read models;
// - backing up an encrypted vault writes an encrypted snapshot (the
//   backup is not openable as plain SQLite either), while a plaintext
//   vault's backup stays plaintext even with a key in hand.

#![cfg(feature = "encryption")]

use std::fs;
use std::path::Path;

use rusqlite::Connection;
use vault_core::backup;
use vault_core::encryption::{self, VaultFileKind, VaultKey};
use vault_core::health::archive_health;
use vault_core::message::NewMessage;
use vault_core::safe_migration::OpenOptions;
use vault_core::storage::Storage;
use vault_core::vault_move::{vault_location_inspect, ExistingContent};

fn open_keyed(path: &Path, key: Option<VaultKey>) -> vault_core::Result<Storage> {
    Storage::open_with(
        path,
        &OpenOptions {
            vault_key: key,
            ..Default::default()
        },
    )
}

fn msg(external_id: &str, body: &str) -> NewMessage {
    NewMessage::new(
        "test",
        external_id,
        "Author <a@example.org>",
        "2026-11-17T10:00:00Z",
        body,
    )
}

/// A local-only mount table (the vault_move suite's fixture shape).
fn mounts_of_local() -> impl Fn() -> std::io::Result<String> {
    let text = "rootfs / rootfs rw 0 0\n/dev/sda1 / ext4 rw,relatime 0 0\n".to_string();
    move || Ok(text.clone())
}

fn inspect(path: &Path, key: Option<&VaultKey>) -> vault_core::vault_move::LocationInspection {
    vault_location_inspect(path, None, &mounts_of_local(), key).unwrap()
}

/// A fresh key, and a second one guaranteed different from the first.
fn two_keys() -> (VaultKey, VaultKey) {
    let a = VaultKey::generate().unwrap();
    let mut other = *a.as_bytes();
    other[0] ^= 0xff;
    (a, VaultKey::from_bytes(other))
}

#[test]
fn encrypted_vault_round_trips_and_is_not_plain_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let (key, _) = two_keys();

    let s = open_keyed(&db, Some(key)).unwrap();
    assert!(s.is_encrypted());
    assert_eq!(s.vault_key(), Some(&key));
    s.insert_message(&msg("m1", "hello encrypted vault"))
        .unwrap();
    let total = s.stats().unwrap().total;
    assert_eq!(total, 1);
    drop(s);

    // The file on disk carries no plaintext SQLite header.
    assert_eq!(
        encryption::classify_vault_file(&db).unwrap(),
        VaultFileKind::Encrypted
    );
    let header = fs::read(&db).unwrap();
    assert!(&header[..16] != b"SQLite format 3\x00");

    // A plain SQLite connection cannot open it.
    let plain = Connection::open(&db).unwrap();
    let refused = plain.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()));
    assert!(
        refused.is_err(),
        "an encrypted vault must not open as plain SQLite"
    );

    // Reopening with the key reads the archived message back.
    let s = open_keyed(&db, Some(key)).unwrap();
    assert!(s.is_encrypted());
    assert_eq!(s.stats().unwrap().total, 1);
    let page = s
        .search_page(&vault_core::query::SearchQuery {
            text: "encrypted".into(),
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.hits.len(), 1);
}

#[test]
fn wrong_key_is_a_clean_refusal_with_no_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let (key, other) = two_keys();

    let s = open_keyed(&db, Some(key)).unwrap();
    s.insert_message(&msg("m1", "body")).unwrap();
    drop(s);

    let before = fs::read(&db).unwrap();
    match open_keyed(&db, Some(other)) {
        Err(vault_core::Error::WrongVaultKey { .. }) => {}
        other => panic!("expected the wrong-key refusal, got {other:?}"),
    }
    // No fallback mode and no corruption: the file is untouched.
    assert_eq!(fs::read(&db).unwrap(), before);
    // The right key still opens it afterwards.
    assert!(open_keyed(&db, Some(key)).is_ok());
}

#[test]
fn missing_key_is_a_clean_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let (key, _) = two_keys();

    let s = open_keyed(&db, Some(key)).unwrap();
    s.insert_message(&msg("m1", "body")).unwrap();
    drop(s);

    match open_keyed(&db, None) {
        Err(vault_core::Error::EncryptedVaultLocked { .. }) => {}
        other => panic!("expected the missing-key refusal, got {other:?}"),
    }
    // Storage::open (no key plumbing at all) refuses the same way —
    // never a silent plaintext or read-only fallback.
    match Storage::open(&db) {
        Err(vault_core::Error::EncryptedVaultLocked { .. }) => {}
        other => panic!("expected the missing-key refusal, got {other:?}"),
    }
}

#[test]
fn plaintext_vault_opens_with_the_feature_on_and_reports_not_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let (key, _) = two_keys();

    // Created without a key: today's plaintext behavior, feature on.
    let s = Storage::open(&db).unwrap();
    s.insert_message(&msg("m1", "plaintext body")).unwrap();
    assert!(!s.is_encrypted());
    assert_eq!(archive_health(&s).unwrap().encrypted, false);
    drop(s);

    assert_eq!(
        encryption::classify_vault_file(&db).unwrap(),
        VaultFileKind::Plaintext
    );

    // Reopens with a key *offered*: the key is ignored for a plaintext
    // vault — no silent encryption — and the honest status stays no.
    let s = open_keyed(&db, Some(key)).unwrap();
    assert!(!s.is_encrypted());
    assert!(s.vault_key().is_none());
    assert_eq!(archive_health(&s).unwrap().encrypted, false);
    assert_eq!(s.stats().unwrap().total, 1);
    drop(s);

    // The move-inspect read model reports the same honest fact.
    let inspection = inspect(dir.path(), None);
    assert_eq!(
        inspection.existing,
        ExistingContent::Vault {
            vault_id: None,
            schema_version: Some(vault_core::storage::SUPPORTED_SCHEMA_VERSION),
            directory_name: None,
            encrypted: false,
        }
    );
}

#[test]
fn encrypted_vault_reports_encrypted_in_both_read_models() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let (key, _) = two_keys();

    let s = open_keyed(&db, Some(key)).unwrap();
    s.insert_message(&msg("m1", "body")).unwrap();
    assert_eq!(archive_health(&s).unwrap().encrypted, true);
    drop(s);

    let inspection = inspect(dir.path(), Some(&key));
    assert_eq!(
        inspection.existing,
        ExistingContent::Vault {
            vault_id: None,
            schema_version: Some(vault_core::storage::SUPPORTED_SCHEMA_VERSION),
            directory_name: None,
            encrypted: true,
        }
    );

    // Without the key the inspection refuses clearly instead of
    // guessing at the destination's schema.
    match vault_location_inspect(dir.path(), None, &mounts_of_local(), None) {
        Err(vault_core::vault_move::MoveError::Vault(message)) => {
            assert!(message.contains("encrypted"), "refusal says why: {message}");
        }
        other => panic!("expected the encrypted-destination refusal, got {other:?}"),
    }
}

/// The backup snapshot of an encrypted vault is itself encrypted (the
/// back door must not open behind the locked front door), while a
/// plaintext vault with a key in hand keeps plaintext backups.
#[test]
fn backup_snapshot_inherits_the_vaults_encryption() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("vault");
    fs::create_dir_all(&data_dir).unwrap();
    let db = data_dir.join("vault.db");
    let (key, _) = two_keys();

    let s = open_keyed(&db, Some(key)).unwrap();
    s.insert_message(&msg("m1", "body with a needle to find"))
        .unwrap();
    let snapshot = dir.path().join("snapshot.db");
    let plan =
        backup::snapshot_for_backup(&data_dir, s.connection(), &snapshot, Some(&key)).unwrap();
    assert_eq!(plan.counts.messages, 1);
    drop(s);

    // The snapshot is not a plaintext SQLite file and does not open as
    // one — the backup is encrypted at rest like the vault.
    assert_eq!(
        encryption::classify_vault_file(&snapshot).unwrap(),
        VaultFileKind::Encrypted
    );
    let plain = Connection::open(&snapshot).unwrap();
    assert!(
        plain
            .query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .is_err(),
        "an encrypted backup must not open as plain SQLite"
    );

    // An encrypted vault without its key is never backed up
    // unencrypted: the snapshot refuses instead.
    let s = open_keyed(&db, Some(key)).unwrap();
    let refused = dir.path().join("refused.db");
    // The vault's key is on the connection; simulate its absence by
    // passing None explicitly.
    match backup::snapshot_for_backup(&data_dir, s.connection(), &refused, None) {
        Err(vault_core::Error::Backup(message)) => {
            assert!(message.contains("encrypted"), "refusal says why: {message}");
        }
        other => panic!("expected the no-key backup refusal, got {other:?}"),
    }
    assert!(!refused.exists());

    // A plaintext vault with a key offered keeps plaintext backups:
    // nothing is silently encrypted before the explicit conversion.
    let plain_dir = dir.path().join("plain-vault");
    fs::create_dir_all(&plain_dir).unwrap();
    let plain_db = plain_dir.join("vault.db");
    let s = Storage::open(&plain_db).unwrap();
    s.insert_message(&msg("p1", "plaintext")).unwrap();
    let plain_snapshot = dir.path().join("plain-snapshot.db");
    backup::snapshot_for_backup(&plain_dir, s.connection(), &plain_snapshot, Some(&key)).unwrap();
    assert_eq!(
        encryption::classify_vault_file(&plain_snapshot).unwrap(),
        VaultFileKind::Plaintext
    );
}

/// The user-printable key envelope is the only key-restore story: it
/// carries exactly the key, and nothing else does.
#[test]
fn printable_envelope_restores_the_key() {
    let (key, _) = two_keys();
    let envelope = encryption::printable_key_envelope(&key);
    // The envelope groups the hex with spaces; from_hex accepts that.
    let restored = VaultKey::from_hex(
        &envelope
            .lines()
            .find(|line| {
                !line.trim().is_empty() && line.chars().all(|c| c.is_ascii_hexdigit() || c == ' ')
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(restored, key);
}

/// The pool path (the app's shape) opens an encrypted vault through
/// every connection with the key from `PoolConfig`.
#[test]
fn pool_opens_an_encrypted_vault_with_the_injected_key() {
    use vault_core::jobs::{DbPool, PoolConfig};

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("vault.db");
    let (key, _) = two_keys();

    {
        let pool = DbPool::spawn(
            &db,
            PoolConfig {
                vault_key: Some(key),
                ..Default::default()
            },
            |_| {},
        )
        .unwrap();
        let n = pool
            .submit_write(move |s| {
                s.insert_message(&msg("pool-1", "written through the pool"))?;
                Ok(s.stats()?.total)
            })
            .unwrap();
        assert_eq!(n, 1);
        pool.shutdown();
    }

    assert_eq!(
        encryption::classify_vault_file(&db).unwrap(),
        VaultFileKind::Encrypted
    );

    // Without the key the pool refuses to open the encrypted vault.
    match DbPool::spawn(&db, PoolConfig::default(), |_| {}) {
        Err(vault_core::Error::EncryptedVaultLocked { .. }) => {}
        Err(other) => panic!("expected the missing-key refusal, got {other}"),
        Ok(_) => panic!("expected the missing-key refusal, got an open pool"),
    }
}

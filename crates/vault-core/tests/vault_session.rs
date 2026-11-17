//! The A4 vault-session seam: one owner of vault identity, the
//! exclusive session lock, and the derived vault paths.
//!
//! These tests cover the acceptance contract of the seam: the lock
//! refuses a second concurrent session on the same vault directory and
//! admits the next open once the first is released; a legacy directory
//! (no identity file) opens in place and gains a stable identity; an
//! identity carried into a directory with a different name refuses to
//! open, while the same name under a different parent still opens
//! (identity records the directory name, never an absolute path).

use std::fs;

use vault_core::vault_session::{SessionError, VaultSession};

/// A second open of the same vault directory fails with the distinct
/// lock error while the first session lives; releasing the first
/// allows the next open, which sees the same vault identity.
#[test]
fn second_open_locks_out_until_the_first_session_is_released() {
    let vault = tempfile::tempdir().unwrap();
    let first = VaultSession::open(vault.path()).expect("first open succeeds");
    let id = first.vault_id();
    match VaultSession::open(vault.path()) {
        Err(SessionError::Locked) => {}
        other => panic!("expected the lock refusal, got {other:?}"),
    }
    drop(first);
    let third = VaultSession::open(vault.path()).expect("open after release succeeds");
    assert_eq!(third.vault_id(), id, "identity is stable across sessions");
}

/// A legacy vault (identity file absent) opens in place with the
/// derived paths unchanged, and the first successful open writes the
/// identity file with a fresh stable UUID — a re-open reads it back.
#[test]
fn legacy_vault_opens_in_place_and_gains_identity() {
    let vault = tempfile::tempdir().unwrap();
    fs::write(vault.path().join("vault.db"), b"legacy database bytes").unwrap();
    let session = VaultSession::open(vault.path()).expect("legacy vault opens in place");
    assert_eq!(session.dir(), vault.path());
    assert_eq!(session.db_path(), vault.path().join("vault.db").as_path());
    assert_eq!(session.media_dir(), vault.path().join("media").as_path());
    assert_eq!(
        session.settings_path(),
        vault.path().join("settings.json").as_path()
    );
    let id = session.vault_id();
    drop(session);
    let identity_path = vault.path().join("vault.json");
    let identity =
        fs::read_to_string(&identity_path).expect("identity file written on first open");
    assert!(
        identity.contains(&id.to_string()),
        "identity file carries the minted UUID: {identity}"
    );
    // The legacy content is untouched and the UUID is stable on re-open.
    assert_eq!(
        fs::read(vault.path().join("vault.db")).unwrap(),
        b"legacy database bytes"
    );
    assert_eq!(
        VaultSession::open(vault.path()).unwrap().vault_id(),
        id,
        "re-open keeps the minted UUID"
    );
}

/// A vault directory copied under a different name (identity file
/// included) refuses to open: the identity's recorded directory name
/// binds it to that name. The same directory name under a different
/// parent still opens — the identity never records an absolute path.
#[test]
fn copied_vault_under_a_different_directory_name_refuses_to_open() {
    let origin = tempfile::tempdir().unwrap();
    let session = VaultSession::open(origin.path()).unwrap();
    let id = session.vault_id();
    drop(session);

    // A copy in a differently-named directory must not open in place.
    let home = tempfile::tempdir().unwrap();
    let renamed = home.path().join("renamed-copy");
    fs::create_dir(&renamed).unwrap();
    fs::copy(origin.path().join("vault.json"), renamed.join("vault.json")).unwrap();
    match VaultSession::open(&renamed) {
        Err(SessionError::DirectoryNameMismatch { .. }) => {}
        other => panic!("expected a directory-name mismatch refusal, got {other:?}"),
    }

    // The same name under a different parent opens with the same UUID.
    let moved_home = tempfile::tempdir().unwrap();
    let same_name = moved_home.path().join(origin.path().file_name().unwrap());
    fs::create_dir(&same_name).unwrap();
    fs::copy(origin.path().join("vault.json"), same_name.join("vault.json")).unwrap();
    let moved = VaultSession::open(&same_name)
        .expect("identity follows the directory name, not the absolute path");
    assert_eq!(moved.vault_id(), id);
}

/// An identity file that is not a valid identity for this build
/// (foreign format marker, unparseable UUID, wrong shape) refuses the
/// open instead of silently re-minting one.
#[test]
fn invalid_identity_file_refuses_to_open() {
    let vault = tempfile::tempdir().unwrap();
    let cases = [
        "{\"format\":\"some-other-product\",\"format_version\":1}".to_string(),
        "{\"format\":\"mailchat-vault\",\"format_version\":1,\"vault_id\":\"not-a-uuid\"}"
            .to_string(),
        "not json at all".to_string(),
    ];
    for (i, text) in cases.into_iter().enumerate() {
        fs::write(vault.path().join("vault.json"), text).unwrap();
        match VaultSession::open(vault.path()) {
            Err(SessionError::InvalidIdentity(_)) => {}
            other => panic!("case {i}: expected an identity refusal, got {other:?}"),
        }
    }
}

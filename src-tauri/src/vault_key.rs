//! The vault's at-rest encryption key: OS-keyring custody, behind the
//! app's `encryption` feature.
//!
//! The key is a 256-bit SQLCipher key (see `vault_core::encryption`).
//! Custody follows the existing keyring pattern (`secrets`): one entry
//! under the shared `mailchat-vault` service, keyed `vault:key`. On
//! first use with encryption enabled a fresh key is minted and stored;
//! from then on every app start loads it. The keyring entry is the
//! machine's copy — the user's copy is the printable envelope
//! ([`envelope_text`]), which this module formats but never stores
//! anywhere: there is no escrow, and losing both the keyring entry and
//! the envelope loses the archive, by design.
//!
//! The default build (without the feature) never mints, loads, or asks
//! for a key: this module is compiled out and the vault opens exactly
//! as before.
//!
//! Manual/CI verification note: a headless runner has no secret
//! service, so the keyed-open path is covered by `vault-core`'s
//! injected-key tests (`cargo test -p vault-core --test encryption
//! --features encryption`); this adapter needs a desktop session (or a
//! CI service with a keyring) before release.

use vault_core::encryption::VaultKey;

/// Keyring entry for the vault key, under the shared
/// [`crate::secrets::SERVICE`] namespace.
const KEY_ENTRY: &str = "vault:key";

/// Load the vault key from the OS keyring, minting and storing a fresh
/// 256-bit key when no entry exists yet (first use with encryption
/// enabled). Failing cleanly beats failing quietly: a missing or
/// unreadable keyring surfaces as an error the caller can show, never
/// as a silently plaintext vault.
pub fn load_or_create() -> Result<VaultKey, String> {
    match load()? {
        Some(key) => Ok(key),
        None => {
            let key = VaultKey::generate().map_err(|e| e.to_string())?;
            let entry = keyring::Entry::new(crate::secrets::SERVICE, KEY_ENTRY)
                .map_err(|e| e.to_string())?;
            entry
                .set_password(&key.as_hex())
                .map_err(|e| format!("cannot store the vault key in the OS keyring: {e}"))?;
            Ok(key)
        }
    }
}

/// Load the vault key when the keyring holds one; `Ok(None)` when no
/// entry exists (never mints — used by read-only callers).
pub fn load() -> Result<Option<VaultKey>, String> {
    let entry = keyring::Entry::new(crate::secrets::SERVICE, KEY_ENTRY)
        .map_err(|e| format!("cannot open the OS keyring: {e}"))?;
    match entry.get_password() {
        Ok(hex) => VaultKey::from_hex(&hex)
            .map(Some)
            .map_err(|e| format!("the stored vault key is malformed: {e}")),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!(
            "cannot read the vault key from the OS keyring: {e}"
        )),
    }
}

/// The user-printable restore envelope for the vault key — the one
/// form in which the key may leave the machine, because the user
/// printed it and keeps it. Formatted from the key; never stored.
pub fn envelope_text(key: &VaultKey) -> String {
    vault_core::encryption::printable_key_envelope(key)
}

//! IMAP account password storage: OS keyring first, memory fallback.
//!
//! Preferred store: the OS keyring — Secret Service (GNOME Keyring/KWallet)
//! on Linux via a pure-Rust zbus client, Keychain Services on macOS,
//! Credential Manager on Windows — under the service name `mailchat-vault`,
//! one entry per account (`user@host`).
//!
//! When the OS keyring is unavailable at runtime (headless session, locked
//! keyring, unsupported platform), the password is kept **only in process
//! memory for the current app session** and disappears when the app exits.
//! Passwords are never written to the vault database (see
//! `vault_core::imap` for the ingest-side guarantees) and never logged.
//! [`Secrets::save`] reports which store is active so the UI can tell the
//! user.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;

/// Keyring service name for all mailchat-vault accounts.
pub const SERVICE: &str = "mailchat-vault";

/// Which backing store a password was saved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretStore {
    /// The OS keyring (persistent across app restarts).
    OsKeyring,
    /// Process memory only (cleared when the app exits).
    Memory,
}

/// Account secret storage, managed as Tauri app state.
pub struct Secrets {
    /// Session-memory fallback, keyed `user@host`.
    memory: Mutex<HashMap<String, String>>,
}

impl Secrets {
    pub fn new() -> Self {
        Self {
            memory: Mutex::new(HashMap::new()),
        }
    }

    /// Keyring/memory key for one account.
    fn account_key(host: &str, username: &str) -> String {
        format!("{username}@{host}")
    }

    /// Remember a password for `user@host`: OS keyring when available,
    /// otherwise session memory. Returns the store that was used.
    pub fn save(&self, host: &str, username: &str, password: &str) -> SecretStore {
        let key = Self::account_key(host, username);
        let keyring =
            keyring::Entry::new(SERVICE, &key).and_then(|entry| entry.set_password(password));
        match keyring {
            Ok(()) => SecretStore::OsKeyring,
            Err(_) => {
                self.memory
                    .lock()
                    .expect("secrets memory lock")
                    .insert(key, password.to_string());
                SecretStore::Memory
            }
        }
    }

    /// Load the stored password for `user@host`: OS keyring first, then the
    /// session-memory fallback. `None` when nothing is stored (the UI
    /// should prompt for it).
    pub fn load(&self, host: &str, username: &str) -> Option<String> {
        let key = Self::account_key(host, username);
        if let Some(password) = self.load_key(&key) {
            return Some(password);
        }
        let legacy_key = Self::account_key(host, &username.to_lowercase());
        if legacy_key == key {
            return None;
        }
        let password = self.load_key(&legacy_key)?;
        self.save(host, username, &password);
        Some(password)
    }

    /// Read one keyring/memory key: OS keyring first, then session memory.
    fn load_key(&self, key: &str) -> Option<String> {
        if let Ok(entry) = keyring::Entry::new(SERVICE, key) {
            if let Ok(password) = entry.get_password() {
                return Some(password);
            }
        }
        self.memory
            .lock()
            .expect("secrets memory lock")
            .get(key)
            .cloned()
    }
}

impl Default for Secrets {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Secrets::load` must keep pre-upgrade users reachable: passwords saved
    /// by older releases were keyed `lower(username)@host`, while the new
    /// connector/exact-case lookup key is `username@host`. The exact-case
    /// key wins; the legacy key only fills the gap — and on first successful
    /// open the secret is migrated (re-stored) under the exact-case key.
    ///
    /// Account names use the reserved `.invalid` domain so the OS keyring
    /// path (when one is available) never touches real accounts.
    const HOST: &str = "imap.secrets-test.invalid";

    #[test]
    fn load_finds_legacy_lowercase_key_and_migrates_to_exact_case_key() {
        let secrets = Secrets::new();
        // A pre-upgrade release lowercased the username before saving, so
        // the stored key holds `ana@...` while the account is now loaded
        // with its exact configured casing.
        let username = "Ana@Example.invalid";
        secrets.save(HOST, &username.to_lowercase(), "legacy-app-password");

        let loaded = secrets
            .load(HOST, username)
            .expect("legacy key must resolve the stored password");
        assert_eq!(loaded, "legacy-app-password");

        // Migration: the secret was re-stored under the exact-case key, so
        // the next load resolves directly.
        let again = secrets
            .load(HOST, username)
            .expect("migrated secret must resolve directly");
        assert_eq!(again, "legacy-app-password");
    }

    #[test]
    fn load_prefers_exact_case_key_over_stale_legacy() {
        let secrets = Secrets::new();
        let username = "Ana@Example.invalid";
        // Both keys exist with different passwords: the exact-case key is
        // authoritative, the legacy entry must not shadow it.
        secrets.save(HOST, username, "fresh-app-password");
        secrets.save(HOST, &username.to_lowercase(), "stale-app-password");

        let loaded = secrets
            .load(HOST, username)
            .expect("exact-case key must resolve");
        assert_eq!(loaded, "fresh-app-password");
    }

    #[test]
    fn load_returns_none_when_nothing_is_stored() {
        let secrets = Secrets::new();
        // All-lowercase username: the legacy key equals the lookup key, so
        // there is no second place to look — the UI prompts for a password.
        assert_eq!(secrets.load(HOST, "ana@example.invalid"), None);
        // Cased username with nothing stored anywhere: also None.
        assert_eq!(secrets.load(HOST, "Ana@Example.invalid"), None);
    }
}

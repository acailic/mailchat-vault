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

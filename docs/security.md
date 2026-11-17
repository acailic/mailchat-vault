# Security: what is and is not protected

Status: **current for the encryption plumbing (slice 1)** — this page
states exactly what the at-rest encryption added in slice 1 does and
does not protect, so the privacy promise can be checked against the
code rather than inferred from marketing. The archive database is
[SQLite with SQLCipher](https://github.com/sqlcipher/sqlcipher)
(AES-256-CBC per page with an HMAC per page, the widely audited
at-rest encryption used by Signal Desktop's local-store lineage);
the bundled build is compiled from the SQLCipher amalgamation with a
vendored OpenSSL (the `bundled-sqlcipher-vendored-openssl` feature of
`rusqlite` 0.37).

## Opt-in, not default

Encryption engages only when **both** hold:

1. the build was compiled with the `encryption` cargo feature
   (vault-core; the desktop app forwards its own feature of the same
   name), and
2. a 256-bit vault key is supplied when the vault opens — the desktop
   app keeps it in the OS keyring (Secret Service / Keychain /
   Credential Manager, service `mailchat-vault`, entry `vault:key`,
   the same keyring the account credentials use).

A default build, or an open without a key, behaves exactly as before:
plaintext vaults, plaintext backups. There is no silent encryption and
no silent conversion of an existing archive — converting a plaintext
vault to an encrypted one is a separate, explicit, journaled operation
(a follow-up task; until it exists, an existing plaintext vault stays
plaintext).

## What encryption protects

With the feature on and a key supplied, a **newly created** vault file
(`vault.db`) is stored encrypted: the database pages, including FTS
indexes and media metadata, are AES-256 ciphertext with per-page HMACs.
A file browser, a stolen disk, a cloud-synced Documents folder, or a
shared backup drive sees ciphertext, not messages. Backups taken from
an encrypted vault inherit its state: the backup snapshot is written
through the keyed open, so the backup is encrypted with the same key —
locking the front door does not leave the back door open.

An encrypted vault refuses to open without its key, with a clear error
before any write — there is deliberately **no read-only fallback and no
plaintext fallback**. A wrong key is a clean refusal (`WrongVaultKey`),
not corruption.

## What encryption does not protect

- **Process memory.** While the app runs, decrypted pages, query
  results, and the key itself live in process memory; so does every
  other program's data. Memory disclosure (a crash dump, a debugger, a
  swapped-out page) is out of scope for at-rest encryption.
- **OS swap and hibernation.** Swap files written by the operating
  system can contain decrypted pages from memory. Users with high
  threat models should configure encrypted swap (FileVault,
  BitLocker, or an encrypted swap partition), which protects every
  application at once.
- **Pre-migration vaults and backups.** Every vault created before
  encryption was enabled — and every backup taken from one — is a
  plaintext SQLite file and stays that way until it is explicitly
  converted (the encrypt-in-place follow-up). The `archive_health` and
  vault-inspect read models report `encrypted: false` for them
  honestly; they never claim protection that is not there.
- **Plaintext exports.** Deliberate exports (HTML, JSONL, mbox, chat
  exports) are user-requested plaintext by design; encryption of the
  archive never encrypts what the user explicitly exported.
- **A running session's key.** While unlocked, the key is in the OS
  keyring and in process memory. Locking the screen does not lock the
  vault; closing the app does (the key is re-read from the keyring at
  every start — an encrypted vault plus a deleted keyring entry plus no
  printed envelope is unreadable, including by us).

## Key custody: no escrow, one envelope

There is no key escrow, no cloud key sync, and no passphrase-derived
fallback. The key exists in exactly two places:

1. the **OS keyring** of the machine that created the vault (entry
   `vault:key`), and
2. the **user-printable restore envelope** the user can print when
   encryption is enabled: the 64-hex-character key in grouped lines
   with restore instructions (formatted by
   `vault_core::encryption::printable_key_envelope`; the desktop
   adapter exposes it in `vault_key.rs`).

Losing both means losing the archive — permanently and by design; a
privacy-positioned app cannot also promise recovery from a key it never
holds. Restoring on a new machine needs the keyring entry re-created
from the printed envelope before the vault (or an encrypted backup)
opens. Backups of an encrypted vault are encrypted **with the same
key** as the vault, so the envelope restores both.

## Threat model summary

| Threat | Protected? |
| --- | --- |
| Stolen/disk-forensics on a closed vault file | Yes (SQLCipher at rest) |
| Backup container of an encrypted vault copied | Yes (encrypted, same key) |
| Malware running as the user, app running unlocked | No (memory access) |
| Swap/hibernation file analysis | No (OS-level; use encrypted swap) |
| Pre-conversion plaintext vaults and their backups | No (not yet encrypted) |
| User-exported plaintext files | No (by design) |
| Key loss (keyring deleted, no envelope) | Unrecoverable, by design |

The repository-level policy (reporting, supported versions, signing) is
[SECURITY.md](../SECURITY.md); the privacy boundaries of the
architecture (what leaves the machine, what never does) are
[docs/architecture.md](architecture.md).

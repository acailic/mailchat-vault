//! vault-core: storage and ingest core for mailchat-vault.
//!
//! Everything here is free of UI dependencies. The Tauri app (`src-tauri`)
//! is a thin shell over this crate.
//!
//! Layout:
//! - [`message`]: the message model (`NewMessage` in, `Message` out).
//! - [`storage`]: SQLite database with FTS5 full-text index, migrations
//!   (routed through [`safe_migration`]'s snapshot/rollback path),
//!   idempotent inserts, stable conversation keys (M1.3), search, IMAP
//!   sync cursors.
//! - [`safe_migration`]: the M1.4 safe-migration path — pre-migration
//!   snapshot with capacity preflight, single-transaction conversion,
//!   post-conversion index validation, rollback/reopen from the
//!   snapshot, and refusal of newer-schema databases.
//! - [`ingest`]: the [`ingest::Source`] whole-archive abstraction — a
//!   convenience for small parser tests; production paths stream through
//!   [`bounded`] instead.
//! - [`bounded`]: the M2.2 bounded ingest — streaming parser interfaces
//!   ([`bounded::StreamSource`]) and the batch-commit driver with
//!   cancellation, durable issue records, and restart recovery
//!   ([`bounded::BoundedRun`]).
//! - [`mail`]: shared RFC 5322 raw-mail parsing (used by mbox and IMAP),
//!   including the In-Reply-To/References reply graph.
//! - [`mbox`]: the manual/demo `.mbox` file source.
//! - [`whatsapp`]: the WhatsApp `.txt` chat export source.
//! - [`telegram`]: the Telegram `result.json` chat export source.
//! - [`signal`]: the Signal Desktop conversation JSON export source.
//! - [`imessage`]: the iMessage source — a copy-first read of the Mac
//!   Messages app's `chat.db` SQLite store.
//! - [`discord`]: the Discord Data Package chat export source.
//! - [`jsonl`]: the JSON Lines import source — rows from the app's own
//!   JSON Lines export, each keeping its original source identity.
//! - [`chat_export`]: chat export format detection plus the import flow
//!   (with media extraction) for picked files.
//! - [`media`]: content-addressed media store and the extraction pass that
//!   links export files to messages.
//! - [`object_store`]: the M4.1 SHA-256 object store behind `media/` —
//!   the stage → promote → reference lifecycle (no half-written object
//!   is ever visible, references never dangle), the import capacity
//!   budget, and the reconciliation/integrity tool.
//! - [`pdf_text`]: part 1a of attachment-content search (#171) —
//!   budget-bounded, local-only PDF text extraction with explicit
//!   corrupt/encrypted/unsupported/over-budget results; storage, jobs,
//!   and search wiring arrive with the later parts of #171.
//! - [`backup`]: the M4.4 versioned backup container and staged restore —
//!   a ZIP holding `manifest.json`, a consistent SQLite snapshot (the
//!   backup API, never a live-file copy), the settings file, and every
//!   referenced media object, each checksummed in the manifest; restore
//!   validates checksums/SQLite integrity/references in a staging vault
//!   before a journaled, recoverable switch replaces the active one.
//! - [`import_bounds`]: the M4.1 import bounds — traversal/symlink-safe
//!   path resolution, bounded directory walks, and bounded ZIP
//!   expansion (every import tree goes through this fence).
//! - [`jobs`]: the M2.1 database pool — a writer worker thread over a
//!   bounded write queue plus two read connections — and background
//!   jobs with typed states/errors and a seq-stamped event log with
//!   polling recovery.
//! - [`watch`]: automatic folder watching (background import of new or
//!   modified export files from user-chosen folders).
//! - [`imap`]: the incremental IMAP/Gmail source (app password or OAuth2
//!   XOAUTH2 login), with atomic batch commits and a durable failed-item
//!   ledger (M2.3).
//! - [`imap_coordinator`]: the M2.3 IMAP coordinator — sync rounds over
//!   every configured IMAP source (per-source exclusion, credential
//!   resolution, per-source outcomes) on the M2.1 pool.
//! - [`oauth`]: the Google OAuth2 login flow behind IMAP XOAUTH2 — PKCE
//!   loopback-redirect authorization, token exchange/refresh, SASL string.
//! - [`search`]: free-text to FTS5 MATCH query building.
//! - [`simhash`]: 64-bit SimHash near-duplicate fingerprints over
//!   k=4 word shingles (storage slice: computed at import, persisted
//!   per message; query-time folding and the backfill for existing
//!   archives are follow-up slices).
//! - [`llm`]: LLM-assisted search — natural language to an FTS5 plan, then
//!   a summary over the top results (privacy-scoped RAG).
//! - [`vector`]: local embeddings (offline embedder, SQLite blob index,
//!   incremental embedding job) and the FTS+vector hybrid ranking used by
//!   the Ask pipeline.
//! - [`settings`]: the Settings file — LLM provider profiles, default
//!   provider, and app options (no keys; those live in the OS keyring).
//! - [`query`]: paged/filtered search requests and chat-thread context.
//! - [`stats`]: archive statistics for the dashboard (totals, date range,
//!   counts by source/day/chat/author).
//! - [`whats_new`]: the "what's new since your last visit" boot digest
//!   (the M7 follow-up) — the vault-scoped last-visit record plus the
//!   derivation of new messages per source and finished/failed import
//!   and sync jobs since it.
//! - [`export`]: export search results or whole chats to CSV / JSON Lines /
//!   MBOX.
//! - [`export_html`]: the A8 readable offline HTML export bundle — static
//!   escaped pages, copied media objects, a provenance/missing-file
//!   manifest written last, staged publication, and honest
//!   reconstructed-versus-original fidelity labels.
//! - [`staged_write`]: staged file publication for exports — the export
//!   streams into a private temp sibling of the destination and only a
//!   fully written, flushed, fsynced file is renamed into place, so a
//!   failed export never replaces a previously valid user file.
//! - [`health`]: persisted archive-health evidence — the media
//!   verification summary and per-import-source last-import records the
//!   health overview reads (absent records read as not checked), plus
//!   the `archive_health` read model composing the overview's three
//!   separate committed facts (last sync, last import, last verified
//!   backup) with the ledger's unresolved failure counts.
//! - [`vault_session`]: the vault session seam (A4) — one owner of
//!   vault identity, the exclusive advisory session lock, and the
//!   derived vault paths, so every consumer resolves the vault through
//!   a single session before custom locations exist.
//! - [`vault_move`]: the A7 vault-location engine — candidate-location
//!   inspection (write access, filesystem type, free space, existing
//!   identity, schema compatibility), the phased move preview with a
//!   byte estimate, and the device-local move journal whose every
//!   phase has one defined recovery action at startup.

pub mod backup;
pub mod bounded;
pub mod chat_export;
pub mod demo;
pub mod discord;
pub mod encryption;
pub mod export;
pub mod export_html;
pub mod export_manifest;
pub mod extract;
pub mod files;
pub mod health;
pub mod imap;
pub mod imap_coordinator;
pub mod imessage;
pub mod import_bounds;
pub mod ingest;
pub mod jobs;
pub mod jsonl;
pub mod llm;
pub mod mail;
pub mod mbox;
pub mod media;
pub mod message;
pub mod messenger;
pub mod oauth;
pub mod object_store;
pub mod pdf_text;
pub mod person_timeline;
pub mod preview;
pub mod query;
pub mod rank_fusion;
pub mod reminders;
pub mod safe_migration;
pub mod scheduler;
pub mod search;
pub mod search2;
pub mod settings;
pub mod signal;
pub mod simhash;
pub mod staged_write;
pub mod stats;
pub mod storage;
pub mod telegram;
pub mod util;
pub mod vault_move;
pub mod vault_session;
pub mod vector;
pub mod watch;
pub mod whats_new;
pub mod whatsapp;

use thiserror::Error;

/// Fidelity label for a record whose original source bytes were not
/// retained (A3b): every record archived without original retention —
/// all rows from before the feature existed, and any import that did
/// not opt in — carries this label; only records with an
/// `original_references` row carry retained original bytes. The A8
/// export fidelity surfaces consume this constant so the wording stays
/// identical everywhere.
pub const ORIGINAL_NOT_RETAINED: &str = "original not retained";

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("email parse error: {0}")]
    MailParse(#[from] mailparse::MailParseError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("llm error: {0}")]
    Llm(String),
    #[error("chat import error: {0}")]
    ChatImport(String),
    #[error("import bounds exceeded: {0}")]
    ImportBounds(String),
    #[error("settings error: {0}")]
    Settings(String),
    #[error("invalid date filter: {0}")]
    InvalidDate(String),
    #[error("imap error: {0}")]
    Imap(#[from] ::imap::Error),
    #[error("oauth error: {0}")]
    OAuth(String),
    #[error("tls error: {0}")]
    Tls(#[from] rustls::Error),
    #[error("invalid tls server name: {0}")]
    InvalidServerName(#[from] rustls::pki_types::InvalidDnsNameError),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("saved search error: {0}")]
    SavedSearch(String),
    #[error("embedding error: {0}")]
    Embedding(String),
    #[error("jobs error: {0}")]
    Jobs(String),
    #[error("invalid export: {0}")]
    InvalidExport(String),
    #[error("invalid collection: {0}")]
    InvalidCollection(String),
    #[error("media store error: {0}")]
    Media(String),
    #[error("backup/restore error: {0}")]
    Backup(String),
    #[error("archive schema version {db} is newer than this build supports ({supported}); upgrade the app before opening it")]
    SchemaTooNew { db: i64, supported: i64 },
    #[error("unsafe to migrate: {0}")]
    UnsafeMigration(String),
    #[error("post-migration validation failed: {0}")]
    MigrationValidationFailed(String),
    #[error("a previous migration of this archive failed its validation ({reason}); the original was restored and is recoverable from the snapshot at {snapshot}; resolve the cause or restore the snapshot manually before opening again")]
    MigrationFailed { reason: String, snapshot: String },
    #[error("not enough free space to snapshot the archive before migration: need {needed} bytes, only {available} available")]
    InsufficientSpace { needed: u64, available: u64 },
    #[error("migration failure injected at {0}")]
    MigrationFault(&'static str),
    #[error("person not found: {0}")]
    PersonNotFound(String),
    #[error("invalid person input: {0}")]
    InvalidPersonInput(String),
    #[error("identity link conflict: {source_instance_id}:{anchor_kind}:{anchor_value} is already linked to person {owner_name} ({owner_id}); unlink it there first")]
    IdentityLinkConflict {
        source_instance_id: String,
        anchor_kind: String,
        anchor_value: String,
        owner_id: String,
        owner_name: String,
    },
    #[error("identity link not found: {0}")]
    IdentityLinkNotFound(String),
    #[error("person organization changed: the commit carried organization revision {expected} but the vault is at {current}; re-preview and confirm against the current revision")]
    PersonLinksStaleRevision { expected: u64, current: u64 },
    #[error("encryption error: {0}")]
    Encryption(String),
    #[error("the vault at {path} is encrypted and no key was supplied; supply its key — there is deliberately no read-only fallback")]
    EncryptedVaultLocked { path: String },
    #[error("the supplied vault key does not open the encrypted vault at {path}")]
    WrongVaultKey { path: String },
    #[error("the vault at {path} appears encrypted, but this build was compiled without the `encryption` feature — rebuild with it to open the vault")]
    EncryptionUnsupported { path: String },
    #[error("a vault key was supplied for {path}, but this build was compiled without the `encryption` feature — rebuild with it to create an encrypted vault")]
    KeyWithoutEncryption { path: String },
}

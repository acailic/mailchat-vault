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
//! - [`discord`]: the Discord Data Package chat export source.
//! - [`chat_export`]: chat export format detection plus the import flow
//!   (with media extraction) for picked files.
//! - [`media`]: content-addressed media store and the extraction pass that
//!   links export files to messages.
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
//! - [`export`]: export search results or whole chats to CSV / JSON Lines /
//!   MBOX.

pub mod bounded;
pub mod chat_export;
pub mod demo;
pub mod discord;
pub mod export;
pub mod imap;
pub mod imap_coordinator;
pub mod ingest;
pub mod jobs;
pub mod llm;
pub mod mail;
pub mod mbox;
pub mod media;
pub mod message;
pub mod oauth;
pub mod query;
pub mod safe_migration;
pub mod search;
pub mod settings;
pub mod signal;
pub mod stats;
pub mod storage;
pub mod telegram;
pub mod util;
pub mod vector;
pub mod watch;
pub mod whatsapp;

use thiserror::Error;

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
    #[error("embedding error: {0}")]
    Embedding(String),
    #[error("jobs error: {0}")]
    Jobs(String),
    #[error("invalid export: {0}")]
    InvalidExport(String),
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
}

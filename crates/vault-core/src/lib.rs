//! vault-core: storage and ingest core for mailchat-vault.
//!
//! Everything here is free of UI dependencies. The Tauri app (`src-tauri`)
//! is a thin shell over this crate.
//!
//! Layout:
//! - [`message`]: the message model (`NewMessage` in, `Message` out).
//! - [`storage`]: SQLite database with FTS5 full-text index, migrations,
//!   idempotent inserts, search, IMAP sync cursors.
//! - [`ingest`]: the [`ingest::Source`] abstraction and idempotent ingest.
//! - [`mail`]: shared RFC 5322 raw-mail parsing (used by mbox and IMAP).
//! - [`mbox`]: the manual/demo `.mbox` file source.
//! - [`whatsapp`]: the WhatsApp `.txt` chat export source.
//! - [`telegram`]: the Telegram `result.json` chat export source.
//! - [`signal`]: the Signal Desktop conversation JSON export source.
//! - [`chat_export`]: format detection for picked chat export files.
//! - [`imap`]: the incremental IMAP/Gmail source.
//! - [`search`]: free-text to FTS5 MATCH query building.
//! - [`llm`]: LLM-assisted search — natural language to an FTS5 plan, then
//!   a summary over the top results (privacy-scoped RAG).
//! - [`vector`]: local embeddings (offline embedder, SQLite blob index,
//!   incremental embedding job) and the FTS+vector hybrid ranking used by
//!   the Ask pipeline.
//! - [`query`]: paged/filtered search requests and chat-thread context.

pub mod chat_export;
pub mod demo;
pub mod imap;
pub mod ingest;
pub mod llm;
pub mod mail;
pub mod mbox;
pub mod message;
pub mod query;
pub mod search;
pub mod signal;
pub mod storage;
pub mod telegram;
pub mod util;
pub mod vector;
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
    #[error("invalid date filter: {0}")]
    InvalidDate(String),
    #[error("imap error: {0}")]
    Imap(#[from] ::imap::Error),
    #[error("tls error: {0}")]
    Tls(#[from] rustls::Error),
    #[error("invalid tls server name: {0}")]
    InvalidServerName(#[from] rustls::pki_types::InvalidDnsNameError),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("embedding error: {0}")]
    Embedding(String),
}

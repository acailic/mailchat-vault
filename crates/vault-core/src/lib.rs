//! vault-core: storage and ingest core for mailchat-vault.
//!
//! Everything here is free of UI dependencies. The Tauri app (`src-tauri`)
//! is a thin shell over this crate.
//!
//! Layout:
//! - [`message`]: the message model (`NewMessage` in, `Message` out).
//! - [`storage`]: SQLite database with FTS5 full-text index, migrations,
//!   idempotent inserts, stable conversation keys (M1.3), search, IMAP
//!   sync cursors.
//! - [`ingest`]: the [`ingest::Source`] abstraction and idempotent ingest.
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
//! - [`watch`]: automatic folder watching (background import of new or
//!   modified export files from user-chosen folders).
//! - [`imap`]: the incremental IMAP/Gmail source (app password or OAuth2
//!   XOAUTH2 login).
//! - [`oauth`]: the Google OAuth2 login flow behind IMAP XOAUTH2 — PKCE
//!   loopback-redirect authorization, token exchange/refresh, SASL string.
//! - [`search`]: free-text to FTS5 MATCH query building.
//! - [`llm`]: LLM-assisted search — natural language to an FTS5 plan, then
//!   a summary over the top results (privacy-scoped RAG).
//! - [`vector`]: local embeddings (offline embedder, SQLite blob index,
//!   incremental embedding job) and the FTS+vector hybrid ranking used by
//!   the Ask pipeline.
//! - [`query`]: paged/filtered search requests and chat-thread context.
//! - [`stats`]: archive statistics for the dashboard (totals, date range,
//!   counts by source/day/chat/author).
//! - [`export`]: export search results or whole chats to CSV / JSON Lines /
//!   MBOX.

pub mod chat_export;
pub mod demo;
pub mod discord;
pub mod export;
pub mod imap;
pub mod ingest;
pub mod llm;
pub mod mail;
pub mod mbox;
pub mod media;
pub mod message;
pub mod oauth;
pub mod query;
pub mod search;
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
    #[error("invalid export: {0}")]
    InvalidExport(String),
}

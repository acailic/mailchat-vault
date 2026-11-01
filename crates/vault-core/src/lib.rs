//! vault-core: storage and ingest core for mailchat-vault.
//!
//! Everything here is free of UI dependencies. The Tauri app (`src-tauri`)
//! is a thin shell over this crate.
//!
//! Layout:
//! - [`message`]: the message model (`NewMessage` in, `Message` out).
//! - [`storage`]: SQLite database with FTS5 full-text index, migrations,
//!   idempotent inserts, search.
//! - [`ingest`]: the [`ingest::Source`] abstraction and idempotent ingest.
//! - [`mbox`]: the manual/demo `.mbox` file source.
//! - [`whatsapp`]: the WhatsApp `.txt` chat export source.
//! - [`telegram`]: the Telegram `result.json` chat export source.
//! - [`chat_export`]: format detection for picked chat export files.
//! - [`search`]: free-text to FTS5 MATCH query building.

pub mod chat_export;
pub mod demo;
pub mod ingest;
pub mod mbox;
pub mod message;
pub mod search;
pub mod storage;
pub mod telegram;
pub mod util;
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
}

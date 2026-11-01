//! Message model shared by storage and ingest.

use serde::{Deserialize, Serialize};

/// A message arriving from an ingest source.
///
/// `source` + `external_id` form the dedup key: re-ingesting the same message
/// (same source and same upstream id) never creates a second row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewMessage {
    /// Ingest origin, e.g. `"mbox"` or `"demo"`.
    pub source: String,
    /// Stable id within the source (e.g. email `Message-ID`).
    pub external_id: String,
    /// Kind of channel, e.g. `"email"` or `"chat"`.
    pub channel: String,
    /// Conversation grouping within the source (thread/chat id); empty for
    /// single-mailbox imports.
    pub chat_id: String,
    /// Display author ("Name <address>" or handle).
    pub author: String,
    /// Sent time as RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`); empty when unknown.
    pub sent_at: String,
    /// Searchable text body (for email this includes the subject).
    pub body: String,
    /// Structured extras as a JSON object string (`"{}"` when none).
    pub raw_json: String,
}

impl NewMessage {
    /// Build a message with the given key fields; `channel` defaults to
    /// `"email"`, `chat_id` to `""`, and `raw_json` to `"{}"`.
    pub fn new(
        source: impl Into<String>,
        external_id: impl Into<String>,
        author: impl Into<String>,
        sent_at: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            external_id: external_id.into(),
            channel: "email".to_string(),
            chat_id: String::new(),
            author: author.into(),
            sent_at: sent_at.into(),
            body: body.into(),
            raw_json: "{}".to_string(),
        }
    }
}

/// A stored message row, as returned by queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub source: String,
    pub external_id: String,
    pub channel: String,
    pub chat_id: String,
    pub author: String,
    pub sent_at: String,
    pub body: String,
    pub raw_json: String,
}

/// Result of an idempotent insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "id", rename_all = "snake_case")]
pub enum InsertOutcome {
    /// New row stored; carries the row id.
    Inserted(i64),
    /// Dedup key (source, external_id) already present; carries the existing
    /// row id. Nothing was written.
    Duplicate(i64),
}

/// Archive contents summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    pub total: u64,
    pub by_source: Vec<SourceCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCount {
    pub source: String,
    pub count: u64,
}

/// Incremental IMAP sync cursor for one `(source, folder)`.
/// See [`crate::imap`] for the UIDVALIDITY/UID model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImapSyncState {
    /// The folder's `UIDVALIDITY` when the cursor was written.
    pub uidvalidity: u32,
    /// The highest message UID already fetched in this folder.
    pub last_uid: u32,
}

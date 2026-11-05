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
    /// Stable, provider-scoped conversation key (M1.3), e.g.
    /// `"telegram:chat:123456789"` or `"whatsapp:chat:sha256:…"`. Empty
    /// when the source carries nothing stable (legacy `chat_id` grouping
    /// applies). See [`crate::storage`] for how conversations resolve.
    #[serde(default)]
    pub conversation_key: String,
    /// Email reply-graph edges: `In-Reply-To` plus every id in
    /// `References`, root-most first, deduplicated. Empty for non-mail
    /// sources. See [`crate::mail`].
    #[serde(default)]
    pub reply_refs: Vec<String>,
    /// Verbatim source timestamp text (`""` = none recorded; callers that
    /// leave it empty get `sent_at` copied, as in M1.1).
    #[serde(default)]
    pub original_timestamp: String,
    /// The timezone the timestamp was interpreted in: an IANA name
    /// (`"Europe/Berlin"`), an offset (`"+02:00"`), or `"UTC"` for
    /// absolute stamps. `""` = not recorded (M1.1 rows).
    #[serde(default)]
    pub timezone: String,
    /// Time-confidence label: one of `explicit_offset`, `user_confirmed`,
    /// `legacy_assumed`, `unresolved`, `missing` (see the
    /// `archive_messages.time_confidence` CHECK). `""` = derive from
    /// `sent_at` the way M1.1 did.
    #[serde(default)]
    pub time_confidence: String,
    /// MIME attachments/inline parts decoded out of the raw mail by the
    /// parser (M4.2). Transient by design: the media pass
    /// ([`crate::media`]) stores the bytes into the content-addressed
    /// object store and records the `media` entries in `raw_json` before
    /// the batch commits, then the list is dropped — the persisted shape
    /// is the `raw_json` link, never the bytes. Chat sources leave it
    /// empty.
    #[serde(default)]
    pub attachments: Vec<NewAttachment>,
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
            conversation_key: String::new(),
            reply_refs: Vec::new(),
            original_timestamp: String::new(),
            timezone: String::new(),
            time_confidence: String::new(),
            attachments: Vec::new(),
        }
    }
}

/// One decoded MIME part of a mail: display name, coarse kind, and the
/// transfer-decoded bytes (base64/quoted-printable already undone).
/// See [`crate::mail`] for which parts become attachments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAttachment {
    /// Display name: the part's `filename`/`name` parameter, its
    /// `Content-ID`, or a generated `attachment-<n>` fallback.
    pub name: String,
    /// `image`, `video`, `audio`, `document`, `text` — the coarse kind
    /// for the detail view.
    pub kind: String,
    /// The decoded bytes, exactly as the sender's part carried them.
    pub bytes: Vec<u8>,
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

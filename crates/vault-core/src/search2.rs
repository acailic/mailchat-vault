//! v2 search/detail/context read paths (M3.2).
//!
//! The v2 IPC surface reads the canonical store (M1) directly and answers
//! with **summaries**, not full `Message` rows: search pages carry bounded
//! snippets plus the metadata the flat results list renders, and only
//! `message_detail_v2` / `conversation_context_v2` return full bodies.
//! Commands live in the Tauri shell; everything here is storage-level and
//! unit-tested like the rest of `vault-core`.
//!
//! Ordering contract (A07 — stable, flat, deterministic):
//!
//! - **Relevance** (text search, the default for text): FTS5 `rank`, then
//!   ascending message id — identical input and archive always paginate
//!   in the same order, and equal-rank rows have a stable tiebreak.
//! - **Newest** (browse with empty text, or the user's explicit choice):
//!   `sent_instant DESC, id DESC`, with unresolved/missing instants
//!   **last** — after every dated message, deterministically by id.
//! - Results are a **flat list**: no conversation regrouping, the page is
//!   exactly the underlying order.
//!
//! Snapshot/refresh contract: the count and the rows of one page are read
//! inside one deferred read transaction, and every page carries the
//! `archive_revision` of that snapshot. When a content-changing commit
//! lands between two pages, the revision differs and the UI offers a
//! refresh instead of silently restarting pagination (A07).
//!
//! Date bounds use an explicit IANA timezone (validated), so filters mean
//! the same local days on every machine; messages with no resolved
//! instant are excluded from any bounded search (they have no position on
//! the timeline) and sort last in unbounded browse.

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;

use crate::query::{day_instant_utc, escape_like, parse_day};
use crate::storage::Storage;
use crate::{Error, Result};

/// Upper bound for search text length (Unicode characters).
pub const MAX_TEXT_CHARS: usize = 2000;
/// Snippet bound (Unicode characters, hit markers included) — the v2
/// contract caps result snippets at 500 characters.
pub const MAX_SNIPPET_CHARS: usize = 500;
/// Browse-mode snippet bound (a body prefix, no match positions).
pub const BROWSE_SNIPPET_CHARS: usize = 240;
/// Default/maximum page size and maximum offset for v2 search.
pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 200;
pub const MAX_OFFSET: u32 = 1_000_000;
/// Context pages are smaller than search pages (reading pane, not results).
pub const MAX_CONTEXT_LIMIT: u32 = 100;
/// How many distinct import scopes the filter chip lists.
pub const IMPORT_SCOPES_LISTED: u32 = 50;

// ---------------------------------------------------------------------------
// Request / response shapes (snake_case JSON over IPC)
// ---------------------------------------------------------------------------

/// Sort order for [`SearchArchiveRequest`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchSort {
    /// FTS5 relevance, then ascending id. Only meaningful with text;
    /// an empty text browses newest regardless of the requested sort.
    #[default]
    Relevance,
    /// Resolved instant descending, then id descending, unknown last.
    Newest,
}

impl SearchSort {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "relevance" => Ok(Self::Relevance),
            "newest" => Ok(Self::Newest),
            other => Err(Error::InvalidQuery(format!(
                "unknown sort {other:?} (expected relevance or newest)"
            ))),
        }
    }
}

/// One v2 search request (see the data/API contract's
/// `SearchArchiveRequest`). All filters are conjunctive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchArchiveRequest {
    /// Free text; sanitized into an FTS5 MATCH expression when non-empty.
    /// Empty text browses newest-first under the filters.
    pub text: String,
    /// Source instance ids (canonical UUIDs); empty = all sources.
    pub source_ids: Vec<String>,
    /// Restrict to one conversation (canonical UUID).
    pub conversation_id: Option<String>,
    /// Restrict to messages first observed by one bounded import run
    /// (the stable `import_id` M2.2 keys checkpoints on).
    pub import_job_id: Option<String>,
    /// Case-insensitive (Unicode-folded) substring on the author.
    pub author: String,
    /// Inclusive lower day bound (`YYYY-MM-DD`) in `timezone`.
    pub from_date: Option<String>,
    /// Inclusive upper day bound (`YYYY-MM-DD`) in `timezone`.
    pub to_date: Option<String>,
    /// IANA timezone the day bounds are interpreted in; empty = OS default.
    pub timezone: String,
    /// `Some(true)` = only messages with attachment references,
    /// `Some(false)` = only without, `None` = no filter.
    pub has_attachment: Option<bool>,
    pub sort: SearchSort,
    /// Default 50, clamped to `1..=200`.
    pub limit: Option<u32>,
    /// Clamped to `0..=1_000_000`.
    pub offset: Option<u32>,
}

impl Default for SearchArchiveRequest {
    fn default() -> Self {
        Self {
            text: String::new(),
            source_ids: Vec::new(),
            conversation_id: None,
            import_job_id: None,
            author: String::new(),
            from_date: None,
            to_date: None,
            timezone: String::new(),
            has_attachment: None,
            sort: SearchSort::Relevance,
            limit: Some(DEFAULT_LIMIT),
            offset: Some(0),
        }
    }
}

impl SearchArchiveRequest {
    /// Clamp/normalize every field and validate the day bounds. Returns
    /// the resolved values the storage layer binds.
    pub fn normalize(&self) -> Result<NormalizedSearch> {
        if self.text.chars().count() > MAX_TEXT_CHARS {
            return Err(Error::InvalidQuery(format!(
                "text exceeds {} characters",
                MAX_TEXT_CHARS
            )));
        }
        let mut source_ids: Vec<String> = self
            .source_ids
            .iter()
            .map(|s| s.trim().to_string())
            .collect();
        source_ids.retain(|s| !s.is_empty());
        source_ids.dedup();

        let tz_label = if self.timezone.trim().is_empty() {
            iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
        } else {
            self.timezone.trim().to_string()
        };
        let tz: chrono_tz::Tz = tz_label
            .parse()
            .map_err(|_| Error::InvalidQuery(format!("unknown timezone {tz_label:?}")))?;

        let from = match &self.from_date {
            None => None,
            Some(raw) => {
                let raw = raw.trim();
                if raw.is_empty() {
                    None
                } else {
                    Some(parse_day(raw).ok_or_else(|| {
                        Error::InvalidQuery(format!("from_date must be YYYY-MM-DD, got {raw:?}"))
                    })?)
                }
            }
        };
        let to = match &self.to_date {
            None => None,
            Some(raw) => {
                let raw = raw.trim();
                if raw.is_empty() {
                    None
                } else {
                    Some(parse_day(raw).ok_or_else(|| {
                        Error::InvalidQuery(format!("to_date must be YYYY-MM-DD, got {raw:?}"))
                    })?)
                }
            }
        };
        if let (Some(from), Some(to)) = (from, to) {
            if from > to {
                return Err(Error::InvalidQuery(format!(
                    "reversed date bounds: from_date {from} is after to_date {to}"
                )));
            }
        }

        Ok(NormalizedSearch {
            text: self.text.trim().to_string(),
            source_ids,
            conversation_id: self
                .conversation_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            import_job_id: self
                .import_job_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            author: self.author.trim().to_string(),
            // Inclusive whole local days: from local midnight of the from
            // day to (exclusive) local midnight of the day after to.
            from_instant: match from {
                None => None,
                Some(day) => Some(day_instant_utc(&tz, day)?),
            },
            to_instant_exclusive: match to {
                None => None,
                Some(day) => {
                    // Keep the bound inside year 9999: instants format as
                    // fixed `%Y` so text comparison stays time order.
                    let max_day =
                        chrono::NaiveDate::from_ymd_opt(9999, 12, 31).expect("valid date");
                    let next = day
                        .checked_add_days(chrono::Days::new(1))
                        .filter(|next| *next <= max_day)
                        .ok_or_else(|| {
                            Error::InvalidDate(format!("to_date out of range: {day}"))
                        })?;
                    Some(day_instant_utc(&tz, next)?)
                }
            },
            has_attachment: self.has_attachment,
            sort: self.sort,
            limit: self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
            offset: self.offset.unwrap_or(0).min(MAX_OFFSET),
        })
    }
}

/// The clamped/validated form of a [`SearchArchiveRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedSearch {
    pub text: String,
    pub source_ids: Vec<String>,
    pub conversation_id: Option<String>,
    pub import_job_id: Option<String>,
    pub author: String,
    pub from_instant: Option<String>,
    pub to_instant_exclusive: Option<String>,
    pub has_attachment: Option<bool>,
    pub sort: SearchSort,
    pub limit: u32,
    pub offset: u32,
}

/// One flat search result row: identity, metadata, bounded snippet, and
/// the attachment/conflict signals the results list renders. No bodies
/// beyond the snippet, no raw JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageSummary {
    pub id: i64,
    pub source_id: String,
    pub source_label: String,
    pub conversation_id: Option<String>,
    pub conversation_label: Option<String>,
    pub author: String,
    pub subject: Option<String>,
    /// Resolved UTC instant only; `None` when unknown.
    pub sent_at: Option<String>,
    pub time_confidence: String,
    /// Bounded to [`MAX_SNIPPET_CHARS`] with `\u{1}`/`\u{2}` hit markers;
    /// empty in browse mode (no match positions to mark).
    pub snippet: String,
    /// Attachment references on the message (available + missing).
    pub attachment_count: u32,
    /// References with preserved bytes (extracted media). At M3.2 the
    /// canonical store records only preserved media as links, so this is
    /// the count of `media` entries; missing references stay visible as
    /// unavailable rows (M4 owns honest byte availability).
    pub available_attachment_count: u32,
    /// Another archived revision shares this message's identity.
    pub has_conflicting_revision: bool,
}

/// One v2 search page: flat rows, the exact total from the same read
/// snapshot, and the snapshot's archive revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchArchivePage {
    pub hits: Vec<MessageSummary>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
    pub archive_revision: u64,
}

/// One attachment/media reference on a message, with what the archive
/// can honestly say about it at M3.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentSummary {
    /// Display name, best the source recorded.
    pub name: String,
    /// `image`, `video`, `audio`, `document`, …
    pub kind: String,
    pub size: u64,
    /// Whether the referenced bytes are preserved in the vault's media
    /// store. `false` = reference only (placeholder / unextracted); the
    /// reading pane shows it as unavailable until M4 preserves it.
    pub available: bool,
    /// Store-relative path of preserved bytes (`media/<sha256>.<ext>`),
    /// absent for reference-only rows.
    pub path: Option<String>,
}

/// One provenance observation of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OccurrenceView {
    /// `import`, `legacy-import`, `imap:<folder>`, …
    pub scope: String,
    pub provider_item_id: String,
    pub uidvalidity: Option<u32>,
    /// The bounded import run that first observed the message (`""` for
    /// pre-M3.2 provenance and connector paths).
    pub import_id: String,
    pub observed_at: String,
}

/// A sibling archived revision of the same identity (conflicting payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionLink {
    pub message_id: i64,
    /// The identity namespace:key the revision shares.
    pub identity: String,
    pub revision_hash: String,
}

/// Full single-message reading payload (the v2 detail view).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDetail {
    pub id: i64,
    pub source_id: String,
    pub source_label: String,
    pub channel: String,
    pub chat_id: String,
    pub external_id: String,
    pub conversation_id: Option<String>,
    pub conversation_label: Option<String>,
    /// `verified` / `user_linked` / `legacy_unresolved`; `None` when the
    /// message stands alone (no conversation).
    pub conversation_confidence: Option<String>,
    pub author: String,
    pub subject: Option<String>,
    pub body: String,
    pub sent_at: Option<String>,
    /// Verbatim source timestamp text (`""` when none was recorded).
    pub original_timestamp: String,
    /// IANA label the instant was interpreted in (`""` when unknown).
    pub timezone: String,
    pub time_confidence: String,
    pub attachments: Vec<AttachmentSummary>,
    pub occurrences: Vec<OccurrenceView>,
    /// Other archived revisions sharing this message's identity.
    pub revisions: Vec<RevisionLink>,
    /// Source-specific JSON, for the "Technical details" disclosure.
    pub raw_json: String,
}

/// One context row (chronological neighbors of the anchor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMessage {
    pub id: i64,
    pub author: String,
    pub sent_at: Option<String>,
    pub time_confidence: String,
    pub body: String,
}

/// Conversation context anchored on one message (the v2 reading pane).
///
/// `before`/`after` are oldest → newest pages around the anchor.
/// A message with no conversation, or one grouped by an unverified
/// legacy grouping, returns the anchor alone: neighbors would be guesses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationContext {
    pub anchor: MessageDetail,
    pub before: Vec<ContextMessage>,
    pub after: Vec<ContextMessage>,
    /// The anchor's conversation grouping is an unresolved legacy
    /// grouping — the pane shows "Conversation could not be verified"
    /// and no neighbors.
    pub unverified_conversation: bool,
    pub has_more_before: bool,
    pub has_more_after: bool,
}

/// One import scope for the results filter chip: a bounded import run
/// that observed at least one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportScope {
    pub import_id: String,
    /// Messages whose first observation came from this run.
    pub messages: u64,
    pub first_observed_at: String,
    pub last_observed_at: String,
}

// ---------------------------------------------------------------------------
// IPC error shape
// ---------------------------------------------------------------------------

/// The structured error every v2 command returns (snake_case JSON):
/// `code` + safe `message` + optional `field`, `retryable`, `job_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub field: Option<String>,
    pub retryable: bool,
    pub job_id: Option<String>,
}

impl AppError {
    pub fn invalid_input(message: impl Into<String>, field: Option<&str>) -> Self {
        Self {
            code: "invalid_input".into(),
            message: message.into(),
            field: field.map(str::to_string),
            retryable: false,
            job_id: None,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found".into(),
            message: message.into(),
            field: None,
            retryable: false,
            job_id: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: "internal".into(),
            message: message.into(),
            field: None,
            retryable: true,
            job_id: None,
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(field) = &self.field {
            write!(f, " (field: {field})")?;
        }
        Ok(())
    }
}

impl std::error::Error for AppError {}

impl From<Error> for AppError {
    fn from(err: Error) -> Self {
        match &err {
            Error::InvalidQuery(message) | Error::InvalidExport(message) => {
                AppError::invalid_input(message.clone(), None)
            }
            Error::InvalidDate(message) => {
                AppError::invalid_input(message.clone(), Some("from_date/to_date"))
            }
            Error::ChatImport(message) | Error::Settings(message) => {
                AppError::invalid_input(message.clone(), None)
            }
            _ => AppError::internal(err.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Attachment signals from raw_json
// ---------------------------------------------------------------------------

/// The attachment references a message records in its source-specific
/// JSON (M3.2's honest read of what exists before M4's attachment
/// tables): preserved `media` links are available; WhatsApp placeholder
/// objects, Discord attachment metadata, and Telegram `photo`/`file`
/// references without a linked media entry are unavailable references.
pub fn attachment_summaries(raw_json: &str) -> Vec<AttachmentSummary> {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw_json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut media_names: Vec<String> = Vec::new();
    if let Some(Value::Array(entries)) = map.get("media") {
        for entry in entries {
            let Value::Object(entry) = entry else {
                continue;
            };
            let name = str_of(entry, "name").unwrap_or("media");
            let kind = str_of(entry, "kind").unwrap_or("file");
            let size = entry.get("size").and_then(Value::as_u64).unwrap_or(0);
            media_names.push(name.to_string());
            out.push(AttachmentSummary {
                name: name.to_string(),
                kind: kind.to_string(),
                size,
                available: true,
                path: str_of(entry, "path").map(str::to_string),
            });
        }
    }
    // WhatsApp placeholder: one reference; if the media pass linked a
    // file for it the `media` entry above already carries it.
    if let Some(Value::Object(placeholder)) = map.get("attachment") {
        let kind = str_of(placeholder, "kind").unwrap_or("attachment");
        let name = str_of(placeholder, "name").unwrap_or("attachment reference");
        if !media_names.iter().any(|m| m.as_str() == name) {
            out.push(AttachmentSummary {
                name: name.to_string(),
                kind: kind.to_string(),
                size: 0,
                available: false,
                path: None,
            });
        }
    }
    // Discord Data Package rows keep the API's `attachments` array.
    if let Some(Value::Array(entries)) = map.get("attachments") {
        for entry in entries {
            let Value::Object(entry) = entry else {
                continue;
            };
            let name = str_of(entry, "filename").unwrap_or("attachment");
            let kind = str_of(entry, "content_type").unwrap_or("file");
            let size = entry.get("size").and_then(Value::as_u64).unwrap_or(0);
            out.push(AttachmentSummary {
                name: name.to_string(),
                kind: kind.to_string(),
                size,
                available: false,
                path: None,
            });
        }
    }
    // Telegram exports keep the original objects verbatim.
    for key in ["photo", "file"] {
        if let Some(Value::Object(item)) = map.get(key) {
            let name = str_of(item, "file_name")
                .or_else(|| str_of(item, "file_name_unique"))
                .map(str::to_string)
                .unwrap_or_else(|| key.to_string());
            if !media_names.contains(&name) {
                out.push(AttachmentSummary {
                    name,
                    kind: key.to_string(),
                    size: 0,
                    available: false,
                    path: None,
                });
            }
        }
    }
    out
}

/// SQL predicate (over alias `m`) matching messages with at least one
/// attachment reference, mirroring [`attachment_summaries`] > 0.
/// `COALESCE` keeps the boolean total order two-valued: a missing JSON
/// path is `0`/absent, never SQL `NULL`, so `NOT (…)` filters correctly.
const HAS_ATTACHMENT_SQL: &str = "(COALESCE(json_array_length(m.raw_json, '$.media'), 0) > 0 \
     OR json_type(m.raw_json, '$.attachment') IS NOT NULL \
     OR COALESCE(json_array_length(m.raw_json, '$.attachments'), 0) > 0 \
     OR json_type(m.raw_json, '$.photo') IS NOT NULL \
     OR json_type(m.raw_json, '$.file') IS NOT NULL)";

fn str_of<'a>(map: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

/// Truncate to `max` Unicode characters on a char boundary.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.char_indices()
        .nth(max)
        .map(|(idx, _)| s[..idx].to_string())
        .unwrap_or_else(|| s.to_string())
}

/// The browse-mode snippet: a whitespace-collapsed body prefix, bounded
/// so summaries stay summaries even without match positions.
fn browse_snippet(body: &str) -> String {
    let collapsed: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&collapsed, BROWSE_SNIPPET_CHARS)
}

// ---------------------------------------------------------------------------
// Filter building (v2)
// ---------------------------------------------------------------------------

/// `AND …` fragments over aliases `m` (archive_messages), `si`
/// (source_instances), bound in order. Shared by the count and the page
/// statements of one search.
struct V2Filters {
    sql: String,
    values: Vec<SqlValue>,
}

impl V2Filters {
    fn build(req: &NormalizedSearch) -> Result<Self> {
        let mut sql = String::new();
        let mut values: Vec<SqlValue> = Vec::new();

        if !req.source_ids.is_empty() {
            let placeholders = vec!["?"; req.source_ids.len()].join(", ");
            sql.push_str(&format!(" AND si.id IN ({placeholders})"));
            values.extend(req.source_ids.iter().cloned().map(SqlValue::from));
        }
        if let Some(conversation) = &req.conversation_id {
            sql.push_str(" AND m.conversation_id = ?");
            values.push(SqlValue::from(conversation.clone()));
        }
        if let Some(import) = &req.import_job_id {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM message_occurrences o \
                 WHERE o.message_id = m.id AND o.import_id = ?)",
            );
            values.push(SqlValue::from(import.clone()));
        }
        if !req.author.is_empty() {
            // Same Unicode-folded substring comparison the v1 filter uses
            // (`fold` is registered on every open connection).
            sql.push_str(" AND fold(m.author) LIKE fold(?) ESCAPE '\\'");
            values.push(SqlValue::from(format!("%{}%", escape_like(&req.author))));
        }
        if let Some(from) = &req.from_instant {
            sql.push_str(" AND m.sent_instant >= ?");
            values.push(SqlValue::from(from.clone()));
        }
        if let Some(to) = &req.to_instant_exclusive {
            sql.push_str(" AND m.sent_instant < ?");
            values.push(SqlValue::from(to.clone()));
        }
        if req.from_instant.is_some() || req.to_instant_exclusive.is_some() {
            // No resolved instant → no timeline position → excluded from
            // any bounded search (both bounds agree on the same set).
            sql.push_str(" AND m.sent_instant IS NOT NULL");
        }
        if let Some(has) = req.has_attachment {
            let predicate = if has {
                format!(" AND {HAS_ATTACHMENT_SQL}")
            } else {
                format!(" AND NOT {HAS_ATTACHMENT_SQL}")
            };
            sql.push_str(&predicate);
        }
        Ok(Self { sql, values })
    }

    fn bind_values(&self) -> Vec<SqlValue> {
        self.values.clone()
    }
}

/// The summary columns both orderings share, in the order [`summary_from_row`]
/// reads them.
const SUMMARY_COLUMNS: &str = "m.id, si.id, si.label, m.conversation_id, c.label, \
     m.author, NULLIF(m.subject, ''), m.sent_instant, m.time_confidence, m.raw_json, \
     (EXISTS (SELECT 1 FROM message_identities i1 \
        JOIN message_identities i2 \
          ON i2.source_instance_id = i1.source_instance_id \
         AND i2.namespace = i1.namespace \
         AND i2.external_key = i1.external_key \
         AND i2.message_id <> i1.message_id \
      WHERE i1.message_id = m.id))";

const SUMMARY_JOINS: &str = "FROM archive_messages AS m \
     JOIN source_instances AS si ON si.id = m.source_instance_id \
     LEFT JOIN conversations AS c ON c.id = m.conversation_id";

fn summary_from_row(
    row: &rusqlite::Row<'_>,
    snippet: Option<String>,
) -> rusqlite::Result<MessageSummary> {
    let raw_json: String = row.get(9)?;
    let attachments = attachment_summaries(&raw_json);
    Ok(MessageSummary {
        id: row.get(0)?,
        source_id: row.get(1)?,
        source_label: row.get(2)?,
        conversation_id: row.get(3)?,
        conversation_label: row.get(4)?,
        author: row.get(5)?,
        subject: row.get(6)?,
        sent_at: row.get(7)?,
        time_confidence: row.get(8)?,
        snippet: snippet.unwrap_or_default(),
        attachment_count: attachments.len() as u32,
        available_attachment_count: attachments.iter().filter(|a| a.available).count() as u32,
        has_conflicting_revision: row.get(10)?,
    })
}

// ---------------------------------------------------------------------------
// Storage read paths
// ---------------------------------------------------------------------------

impl Storage {
    /// Paged, filtered v2 search over the canonical store. Count and rows
    /// are read inside one deferred transaction (one snapshot), and the
    /// page carries that snapshot's [`archive_revision`](Self::archive_revision).
    pub fn search_archive(&self, request: &SearchArchiveRequest) -> Result<SearchArchivePage> {
        let req = request.normalize()?;
        let filters = V2Filters::build(&req)?;
        // Empty text browses newest regardless of the requested sort.
        let newest_order = req.text.is_empty() || req.sort == SearchSort::Newest;

        let tx = self.conn.unchecked_transaction()?;

        let total: u64 = {
            let (sql, bind) = if req.text.is_empty() {
                (
                    format!("SELECT COUNT(*) {SUMMARY_JOINS} WHERE 1=1 {}", filters.sql),
                    filters.bind_values(),
                )
            } else {
                let mut bind = vec![SqlValue::from(crate::search::build_match_query(&req.text))];
                bind.extend(filters.bind_values());
                (
                    format!(
                        "SELECT COUNT(*) FROM archive_fts \
                         JOIN archive_messages AS m ON m.id = archive_fts.rowid \
                         JOIN source_instances AS si ON si.id = m.source_instance_id \
                         LEFT JOIN conversations AS c ON c.id = m.conversation_id \
                         WHERE archive_fts MATCH ?1 {}",
                        filters.sql
                    ),
                    bind,
                )
            };
            let mut stmt = tx.prepare(&sql)?;
            stmt.query_row(rusqlite::params_from_iter(bind), |row| row.get(0))?
        };

        let hits: Vec<MessageSummary> = if req.text.is_empty() {
            // Browse: newest first — resolved instant descending, then id
            // descending; unknown instants last, deterministically by id.
            // No match positions exist, so the row's bounded snippet is a
            // body prefix (the "two-line snippet" of the results list).
            let mut bind = filters.bind_values();
            bind.push(SqlValue::Integer(req.limit as i64));
            bind.push(SqlValue::Integer(req.offset as i64));
            let sql = format!(
                "SELECT {SUMMARY_COLUMNS}, substr(m.body, 1, 600) {SUMMARY_JOINS} \
                 WHERE 1=1 {} \
                 ORDER BY (m.sent_instant IS NULL) ASC, m.sent_instant DESC, m.id DESC \
                 LIMIT ? OFFSET ?",
                filters.sql
            );
            let mut stmt = tx.prepare(&sql)?;
            let rows: Vec<MessageSummary> = stmt
                .query_map(rusqlite::params_from_iter(bind), |row| {
                    let raw: String = row.get(11)?;
                    summary_from_row(row, Some(browse_snippet(&raw)))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        } else if newest_order {
            // Text query explicitly sorted newest: same order as browse,
            // still driven by the FTS index for the match set — and the
            // match snippet still renders (order and snippet are
            // independent choices).
            let mut bind = vec![
                SqlValue::from(crate::query::SNIPPET_OPEN_MARK.to_string()),
                SqlValue::from(crate::query::SNIPPET_CLOSE_MARK.to_string()),
                SqlValue::from(crate::search::build_match_query(&req.text)),
            ];
            bind.extend(filters.bind_values());
            bind.push(SqlValue::Integer(req.limit as i64));
            bind.push(SqlValue::Integer(req.offset as i64));
            let sql = format!(
                "SELECT {SUMMARY_COLUMNS}, \
                 snippet(archive_fts, 0, ?1, ?2, '…', 16) \
                 FROM archive_fts \
                 JOIN archive_messages AS m ON m.id = archive_fts.rowid \
                 JOIN source_instances AS si ON si.id = m.source_instance_id \
                 LEFT JOIN conversations AS c ON c.id = m.conversation_id \
                 WHERE archive_fts MATCH ?3 {} \
                 ORDER BY (m.sent_instant IS NULL) ASC, m.sent_instant DESC, m.id DESC \
                 LIMIT ? OFFSET ?",
                filters.sql
            );
            let mut stmt = tx.prepare(&sql)?;
            let rows: Vec<MessageSummary> = stmt
                .query_map(rusqlite::params_from_iter(bind), |row| {
                    let raw: String = row.get(11)?;
                    summary_from_row(row, Some(truncate_chars(&raw, MAX_SNIPPET_CHARS)))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        } else {
            // Relevance: rank, then ascending id — the stable flat order.
            // Bind order follows the SQL text: the two snippet markers,
            // the MATCH expression, the filters, then LIMIT/OFFSET.
            let mut bind = vec![
                SqlValue::from(crate::query::SNIPPET_OPEN_MARK.to_string()),
                SqlValue::from(crate::query::SNIPPET_CLOSE_MARK.to_string()),
                SqlValue::from(crate::search::build_match_query(&req.text)),
            ];
            bind.extend(filters.bind_values());
            bind.push(SqlValue::Integer(req.limit as i64));
            bind.push(SqlValue::Integer(req.offset as i64));
            let sql = format!(
                "SELECT {SUMMARY_COLUMNS}, \
                 snippet(archive_fts, 0, ?1, ?2, '…', 16) \
                 FROM archive_fts \
                 JOIN archive_messages AS m ON m.id = archive_fts.rowid \
                 JOIN source_instances AS si ON si.id = m.source_instance_id \
                 LEFT JOIN conversations AS c ON c.id = m.conversation_id \
                 WHERE archive_fts MATCH ?3 {} \
                 ORDER BY rank, m.id LIMIT ? OFFSET ?",
                filters.sql
            );
            let mut stmt = tx.prepare(&sql)?;
            let rows: Vec<MessageSummary> = stmt
                .query_map(rusqlite::params_from_iter(bind), |row| {
                    let raw: String = row.get(11)?;
                    summary_from_row(row, Some(truncate_chars(&raw, MAX_SNIPPET_CHARS)))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };

        let archive_revision = read_archive_revision(&tx)?;
        tx.commit()?;

        Ok(SearchArchivePage {
            hits,
            total,
            limit: req.limit,
            offset: req.offset,
            archive_revision,
        })
    }

    /// The full v2 reading payload for one message. `Ok(None)` when no
    /// message has that id (the command maps it to `not_found`).
    pub fn message_detail(&self, id: i64) -> Result<Option<MessageDetail>> {
        let tx = self.conn.unchecked_transaction()?;
        let Some(mut detail) = Self::message_detail_tx(&tx, id)? else {
            return Ok(None);
        };
        detail.occurrences = Self::occurrences_tx(&tx, id)?;
        detail.revisions = Self::revisions_tx(&tx, id)?;
        tx.commit()?;
        Ok(Some(detail))
    }

    fn message_detail_tx(tx: &Connection, id: i64) -> Result<Option<MessageDetail>> {
        let mut stmt = tx.prepare(
            "SELECT m.id, si.id, si.label, m.channel, m.chat_id, m.external_id, \
             m.conversation_id, c.label, c.confidence, m.author, NULLIF(m.subject, ''), \
             m.body, m.sent_instant, m.original_timestamp, m.timezone, m.time_confidence, \
             m.raw_json \
             FROM archive_messages AS m \
             JOIN source_instances AS si ON si.id = m.source_instance_id \
             LEFT JOIN conversations AS c ON c.id = m.conversation_id \
             WHERE m.id = ?1",
        )?;
        let detail = stmt
            .query_row(params![id], |row| {
                let raw_json: String = row.get(16)?;
                Ok(MessageDetail {
                    id: row.get(0)?,
                    source_id: row.get(1)?,
                    source_label: row.get(2)?,
                    channel: row.get(3)?,
                    chat_id: row.get(4)?,
                    external_id: row.get(5)?,
                    conversation_id: row.get(6)?,
                    conversation_label: row.get(7)?,
                    conversation_confidence: row.get(8)?,
                    author: row.get(9)?,
                    subject: row.get(10)?,
                    body: row.get(11)?,
                    sent_at: row.get(12)?,
                    original_timestamp: row.get(13)?,
                    timezone: row.get(14)?,
                    time_confidence: row.get(15)?,
                    attachments: attachment_summaries(&raw_json),
                    occurrences: Vec::new(),
                    revisions: Vec::new(),
                    raw_json,
                })
            })
            .optional()?;
        Ok(detail)
    }

    fn occurrences_tx(tx: &Connection, id: i64) -> Result<Vec<OccurrenceView>> {
        let mut stmt = tx.prepare(
            "SELECT scope, provider_item_id, uidvalidity, import_id, observed_at \
             FROM message_occurrences WHERE message_id = ?1 \
             ORDER BY observed_at, scope, provider_item_id",
        )?;
        let rows = stmt
            .query_map(params![id], |row| {
                Ok(OccurrenceView {
                    scope: row.get(0)?,
                    provider_item_id: row.get(1)?,
                    uidvalidity: row.get::<_, Option<i64>>(2)?.map(|v| v.max(0) as u32),
                    import_id: row.get(3)?,
                    observed_at: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Sibling archived revisions: other message rows sharing any of this
    /// message's identity tuples (same instance/namespace/external key).
    fn revisions_tx(tx: &Connection, id: i64) -> Result<Vec<RevisionLink>> {
        let mut stmt = tx.prepare(
            "SELECT i2.message_id, i1.namespace, i1.external_key, i2.revision_hash \
             FROM message_identities i1 \
             JOIN message_identities i2 \
               ON i2.source_instance_id = i1.source_instance_id \
              AND i2.namespace = i1.namespace \
              AND i2.external_key = i1.external_key \
              AND i2.message_id <> i1.message_id \
             WHERE i1.message_id = ?1 \
             ORDER BY i2.message_id",
        )?;
        let rows = stmt
            .query_map(params![id], |row| {
                Ok(RevisionLink {
                    message_id: row.get(0)?,
                    identity: format!("{}:{}", row.get::<_, String>(1)?, row.get::<_, String>(2)?),
                    revision_hash: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut seen = std::collections::HashSet::new();
        Ok(rows
            .into_iter()
            .filter(|r| seen.insert((r.message_id, r.identity.clone())))
            .collect())
    }

    /// Chronological context around one message (the v2 reading pane).
    ///
    /// `before_cursor`/`after_cursor` are message ids bounding the next
    /// page ("load earlier" passes the oldest id currently held; the
    /// first call passes `None` to page from the anchor). A message with
    /// no conversation — or grouped by an unresolved legacy grouping —
    /// returns the anchor alone: neighbors would be guesses (M1: no
    /// filename groups, no subject-only merges in verified context).
    pub fn conversation_context(
        &self,
        id: i64,
        before_cursor: Option<i64>,
        after_cursor: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Option<ConversationContext>> {
        let limit = limit.unwrap_or(20).clamp(1, MAX_CONTEXT_LIMIT) as i64;
        let tx = self.conn.unchecked_transaction()?;
        let Some(anchor) = Self::message_detail_tx(&tx, id)? else {
            return Ok(None);
        };

        let Some(conversation_id) = anchor.conversation_id.clone() else {
            tx.commit()?;
            return Ok(Some(ConversationContext {
                anchor,
                before: Vec::new(),
                after: Vec::new(),
                unverified_conversation: false,
                has_more_before: false,
                has_more_after: false,
            }));
        };
        if anchor.conversation_confidence.as_deref() == Some("legacy_unresolved") {
            tx.commit()?;
            return Ok(Some(ConversationContext {
                anchor,
                before: Vec::new(),
                after: Vec::new(),
                unverified_conversation: true,
                has_more_before: false,
                has_more_after: false,
            }));
        }

        // Anchor (or cursor) position; unknown instants order by the
        // legacy `sent_at`-text position (empty sorts oldest), matching
        // the v1 thread ordering exactly.
        let position_of = |mid: i64| -> Result<(String, i64)> {
            tx.query_row(
                "SELECT COALESCE(sent_instant, ''), id FROM archive_messages WHERE id = ?1",
                params![mid],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(Into::into)
        };
        let (anchor_sent, anchor_id) = position_of(id)?;
        let before_bound = match before_cursor {
            None => (anchor_sent.clone(), anchor_id),
            Some(cursor) => position_of(cursor)?,
        };
        let after_bound = match after_cursor {
            None => (anchor_sent.clone(), anchor_id),
            Some(cursor) => position_of(cursor)?,
        };

        let page = |older_than: &(String, i64),
                    forward: bool,
                    n: i64|
         -> Result<(Vec<ContextMessage>, bool)> {
            let (cmp, sent_order, id_order) = if forward {
                (">", "ASC", "ASC")
            } else {
                ("<", "DESC", "DESC")
            };
            let sql = format!(
                "SELECT m.id, m.author, m.sent_instant, m.time_confidence, m.body \
                 FROM archive_messages AS m \
                 WHERE m.conversation_id = ?1 \
                   AND (COALESCE(m.sent_instant, '') {cmp} ?2 \
                        OR (COALESCE(m.sent_instant, '') = ?2 AND m.id {cmp} ?3)) \
                 ORDER BY COALESCE(m.sent_instant, '') {sent_order}, m.id {id_order} \
                 LIMIT ?4",
            );
            let mut stmt = tx.prepare(&sql)?;
            let rows: Vec<ContextMessage> = stmt
                .query_map(
                    params![conversation_id, older_than.0, older_than.1, n + 1],
                    |row| {
                        Ok(ContextMessage {
                            id: row.get(0)?,
                            author: row.get(1)?,
                            sent_at: row.get(2)?,
                            time_confidence: row.get(3)?,
                            body: row.get(4)?,
                        })
                    },
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let has_more = rows.len() as i64 > n;
            let mut rows = rows;
            rows.truncate(n as usize);
            if !forward {
                // Fetched newest → oldest; return oldest → newest.
                rows.reverse();
            }
            Ok((rows, has_more))
        };

        let (before, has_more_before) = page(&before_bound, false, limit)?;
        let (after, has_more_after) = page(&after_bound, true, limit)?;
        tx.commit()?;

        Ok(Some(ConversationContext {
            anchor,
            before,
            after,
            unverified_conversation: false,
            has_more_before,
            has_more_after,
        }))
    }

    /// The import scopes seen in message occurrences, newest first —
    /// what the import-scope filter chip offers ("View imported
    /// messages" after a completed import).
    pub fn import_scopes(&self) -> Result<Vec<ImportScope>> {
        let mut stmt = self.conn.prepare(
            "SELECT import_id, COUNT(*), MIN(observed_at), MAX(observed_at) \
             FROM message_occurrences \
             WHERE import_id <> '' \
             GROUP BY import_id \
             ORDER BY MIN(observed_at) DESC, import_id \
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![IMPORT_SCOPES_LISTED as i64], |row| {
                Ok(ImportScope {
                    import_id: row.get(0)?,
                    messages: row.get::<_, i64>(1)?.max(0) as u64,
                    first_observed_at: row.get(2)?,
                    last_observed_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The full v2 details of an explicit selection of messages, in the
    /// requested order (the selection export reads exactly what the user
    /// picked). Missing ids count in `missing` and are skipped, never
    /// fabricated.
    pub fn message_details(&self, ids: &[i64]) -> Result<(Vec<MessageDetail>, u64)> {
        let tx = self.conn.unchecked_transaction()?;
        let mut details = Vec::with_capacity(ids.len());
        let mut missing = 0u64;
        for &id in ids {
            match Self::message_detail_tx(&tx, id)? {
                Some(mut detail) => {
                    detail.occurrences = Self::occurrences_tx(&tx, id)?;
                    detail.revisions = Self::revisions_tx(&tx, id)?;
                    details.push(detail);
                }
                None => missing += 1,
            }
        }
        tx.commit()?;
        Ok((details, missing))
    }
}

// ---------------------------------------------------------------------------
// Selection export (explicit ids → UTF-8 text or JSON, with provenance)
// ---------------------------------------------------------------------------

/// One selection-export format: readable UTF-8 text or JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SelectionFormat {
    Text,
    Json,
}

impl SelectionFormat {
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "text" | "txt" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(Error::InvalidExport(format!(
                "unknown format {other:?} (expected text or json)"
            ))),
        }
    }
}

/// What one selection export produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionExportSummary {
    pub format: SelectionFormat,
    /// Messages written.
    pub written: u64,
    /// Selected ids that no longer exist in the vault.
    pub missing: u64,
}

/// Render one message detail as readable UTF-8 text with provenance
/// (what “Copy message” puts on the clipboard, too).
pub fn detail_as_text(detail: &MessageDetail) -> String {
    let mut out = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(out, "=== Message {} ===", detail.id);
    let _ = writeln!(
        out,
        "Source: {} ({})",
        detail.source_label, detail.source_id
    );
    if let Some(label) = &detail.conversation_label {
        let confidence = detail.conversation_confidence.as_deref().unwrap_or("");
        let _ = writeln!(out, "Conversation: {label} ({confidence})");
    }
    let _ = writeln!(out, "From: {}", detail.author);
    if let Some(subject) = &detail.subject {
        let _ = writeln!(out, "Subject: {subject}");
    }
    match &detail.sent_at {
        Some(at) => {
            let _ = writeln!(out, "Date: {at} ({})", detail.time_confidence);
        }
        None => {
            let _ = writeln!(out, "Date: unknown ({})", detail.time_confidence);
        }
    }
    if !detail.original_timestamp.is_empty() {
        let tz = if detail.timezone.is_empty() {
            "timezone unknown"
        } else {
            &detail.timezone
        };
        let _ = writeln!(
            out,
            "Original timestamp: {} ({tz})",
            detail.original_timestamp
        );
    }
    if !detail.attachments.is_empty() {
        let _ = writeln!(out, "Attachments:");
        for a in &detail.attachments {
            let state = if a.available {
                "available"
            } else {
                "not preserved"
            };
            let _ = writeln!(
                out,
                "  - {} [{}] ({}, {} bytes)",
                a.name, a.kind, state, a.size
            );
        }
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", detail.body);
    let _ = writeln!(out);
    if !detail.revisions.is_empty() {
        let _ = writeln!(out, "Conflicting revisions of this message:");
        for r in &detail.revisions {
            let _ = writeln!(out, "  - message {} ({})", r.message_id, r.identity);
        }
    }
    if !detail.occurrences.is_empty() {
        let _ = writeln!(out, "Provenance:");
        for o in &detail.occurrences {
            let import = if o.import_id.is_empty() {
                String::new()
            } else {
                format!(", import {}", o.import_id)
            };
            let _ = writeln!(
                out,
                "  - {} (item {}, observed {}){}",
                o.scope, o.provider_item_id, o.observed_at, import
            );
        }
    }
    out
}

/// Export an explicit selection of messages as `format`, streaming through
/// `out`. Text is readable UTF-8 with provenance; JSON is one array of the
/// full detail objects (raw JSON included, as the UI's "Technical
/// details" disclosure does). The requested id order is preserved;
/// vanished ids are counted, never fabricated.
pub fn export_selection<W: Write>(
    storage: &Storage,
    ids: &[i64],
    format: SelectionFormat,
    out: W,
) -> Result<SelectionExportSummary> {
    let (details, missing) = storage.message_details(ids)?;
    let mut out = out;
    match format {
        SelectionFormat::Text => {
            for detail in &details {
                out.write_all(detail_as_text(detail).as_bytes())?;
                out.write_all(b"\n")?;
            }
        }
        SelectionFormat::Json => {
            serde_json::to_writer_pretty(&mut out, &details)?;
            out.write_all(b"\n")?;
        }
    }
    out.flush()?;
    Ok(SelectionExportSummary {
        format,
        written: details.len() as u64,
        missing,
    })
}

/// Read the archive revision inside an open transaction (one snapshot
/// with the page's rows).
fn read_archive_revision(tx: &Connection) -> Result<u64> {
    let value: Option<String> = tx
        .query_row(
            "SELECT value FROM archive_meta WHERE key = 'archive_revision'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.and_then(|v| v.parse().ok()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::NewMessage;

    fn seeded() -> Storage {
        let s = Storage::open_in_memory().unwrap();
        for (i, (sent, body)) in [
            ("2026-01-01T10:00:00Z", "old alpha delivery date"),
            ("", "no date alpha at all"),
            ("2026-03-01T10:00:00Z", "newest alpha here"),
            ("2026-01-01T10:00:00Z", "same instant alpha tie"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut msg = NewMessage::new("demo", format!("m{i}"), "Ana", sent, body);
            msg.channel = "email".into();
            s.insert_message(&msg).unwrap();
        }
        s
    }

    #[test]
    fn normalize_clamps_and_validates() {
        let req = SearchArchiveRequest {
            limit: Some(0),
            offset: Some(u32::MAX),
            ..SearchArchiveRequest::default()
        };
        let n = req.normalize().unwrap();
        assert_eq!(n.limit, 1);
        assert_eq!(n.offset, MAX_OFFSET);
        let req = SearchArchiveRequest {
            limit: Some(999_999),
            ..SearchArchiveRequest::default()
        };
        assert_eq!(req.normalize().unwrap().limit, MAX_LIMIT);

        let req = SearchArchiveRequest {
            text: "x".repeat(MAX_TEXT_CHARS + 1),
            ..SearchArchiveRequest::default()
        };
        assert!(req.normalize().is_err());

        let req = SearchArchiveRequest {
            from_date: Some("2026-02-03".into()),
            to_date: Some("2026-02-02".into()),
            ..SearchArchiveRequest::default()
        };
        let err = req.normalize().unwrap_err();
        assert!(err.to_string().contains("reversed"), "{err}");

        let req = SearchArchiveRequest {
            from_date: Some("2026-13-01".into()),
            ..SearchArchiveRequest::default()
        };
        assert!(req.normalize().is_err());

        let req = SearchArchiveRequest {
            timezone: "Mars/Olympus".into(),
            ..SearchArchiveRequest::default()
        };
        assert!(req.normalize().is_err());

        // Bound translation uses the named timezone: 2026-07-15 in
        // Europe/Zagreb starts at 2026-07-14T22:00:00Z (CEST +02:00).
        let req = SearchArchiveRequest {
            from_date: Some("2026-07-15".into()),
            timezone: "Europe/Zagreb".into(),
            ..SearchArchiveRequest::default()
        };
        let n = req.normalize().unwrap();
        assert_eq!(n.from_instant.as_deref(), Some("2026-07-14T22:00:00Z"));
    }

    #[test]
    fn browse_orders_newest_first_with_unknown_last() {
        let s = seeded();
        let page = s.search_archive(&SearchArchiveRequest::default()).unwrap();
        let ids: Vec<i64> = page.hits.iter().map(|h| h.id).collect();
        // Newest → oldest (id 3 = Mar 1); the two equal Jan-1 instants
        // tiebreak by id descending (4 before 1); the unknown-time message
        // (id 2) is last, never interleaved.
        assert_eq!(ids, vec![3, 4, 1, 2]);
        // Browse snippets are bounded body prefixes (no match markers).
        assert!(
            page.hits
                .iter()
                .all(|h| !h.snippet.is_empty()
                    && !h.snippet.contains(crate::query::SNIPPET_OPEN_MARK))
        );
        assert!(page.hits[0].snippet.starts_with("newest"));
        assert_eq!(page.hits[3].sent_at, None);
        assert_eq!(page.hits[3].time_confidence, "missing");
    }

    #[test]
    fn relevance_sorts_by_rank_then_id_and_marks_snippets() {
        let s = seeded();
        let run = || {
            s.search_archive(&SearchArchiveRequest {
                text: "alpha".into(),
                ..SearchArchiveRequest::default()
            })
            .unwrap()
        };
        let page = run();
        assert_eq!(page.total, 4);
        let ids: Vec<i64> = page.hits.iter().map(|h| h.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(sorted, vec![1, 2, 3, 4], "every match appears exactly once");
        // The flat order is deterministic: the same request paginates in
        // exactly the same order every time.
        assert_eq!(run().hits.iter().map(|h| h.id).collect::<Vec<_>>(), ids);
        // Snippets carry the hit markers the frontend highlights on and
        // stay bounded.
        assert!(page
            .hits
            .iter()
            .all(|h| h.snippet.contains(crate::query::SNIPPET_OPEN_MARK)));
    }

    #[test]
    fn text_newest_is_the_user_choice_not_the_default() {
        let s = seeded();
        let page = s
            .search_archive(&SearchArchiveRequest {
                text: "alpha".into(),
                sort: SearchSort::Newest,
                ..SearchArchiveRequest::default()
            })
            .unwrap();
        assert_eq!(
            page.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
            vec![3, 4, 1, 2]
        );
        // Empty text browses newest regardless of the requested sort.
        let page = s
            .search_archive(&SearchArchiveRequest {
                sort: SearchSort::Relevance,
                ..SearchArchiveRequest::default()
            })
            .unwrap();
        assert_eq!(
            page.hits.iter().map(|h| h.id).collect::<Vec<_>>(),
            vec![3, 4, 1, 2]
        );
    }

    #[test]
    fn pagination_is_stable_and_counted() {
        let s = Storage::open_in_memory().unwrap();
        for i in 0..7 {
            let msg = NewMessage::new(
                "demo",
                format!("p{i}"),
                "Ana",
                format!("2026-01-0{}T10:00:00Z", i + 1),
                format!("paginated body {i}"),
            );
            s.insert_message(&msg).unwrap();
        }
        let req = SearchArchiveRequest {
            limit: Some(3),
            ..SearchArchiveRequest::default()
        };
        let p1 = s.search_archive(&req).unwrap();
        let p2 = s
            .search_archive(&SearchArchiveRequest {
                offset: Some(3),
                ..req.clone()
            })
            .unwrap();
        let p3 = s
            .search_archive(&SearchArchiveRequest {
                offset: Some(6),
                ..req.clone()
            })
            .unwrap();
        assert_eq!((p1.hits.len(), p1.total, p1.offset), (3, 7, 0));
        assert_eq!((p2.hits.len(), p2.offset), (3, 3));
        assert_eq!((p3.hits.len(), p3.offset), (1, 6));
        let all: Vec<i64> = [p1, p2, p3]
            .iter()
            .flat_map(|p| p.hits.iter().map(|h| h.id))
            .collect();
        let mut unique = all.clone();
        unique.dedup();
        assert_eq!(
            all.len(),
            unique.len(),
            "pages never overlap or repeat rows"
        );
        assert_eq!(unique.len(), 7);
    }

    #[test]
    fn archive_revision_advances_on_writes_and_pages_carry_it() {
        let s = seeded();
        let first = s.search_archive(&SearchArchiveRequest::default()).unwrap();
        assert_eq!(first.archive_revision, s.archive_revision().unwrap());
        let before = first.archive_revision;
        s.insert_message(&NewMessage::new(
            "demo",
            "extra",
            "Ana",
            "2026-06-01T10:00:00Z",
            "another alpha body",
        ))
        .unwrap();
        let after = s.search_archive(&SearchArchiveRequest::default()).unwrap();
        assert_eq!(after.archive_revision, before + 1);
        // Reads alone never bump it.
        let again = s.search_archive(&SearchArchiveRequest::default()).unwrap();
        assert_eq!(again.archive_revision, after.archive_revision);
    }

    #[test]
    fn date_bounds_exclude_unknown_times_and_use_the_timezone() {
        let s = seeded();
        // Zagreb 2026-01-01 = 2025-12-31T23:00:00Z .. 2026-01-01T23:00:00Z,
        // so both 10:00Z messages on Jan 1 fall inside the local day.
        let page = s
            .search_archive(&SearchArchiveRequest {
                from_date: Some("2026-01-01".into()),
                to_date: Some("2026-01-01".into()),
                timezone: "Europe/Zagreb".into(),
                ..SearchArchiveRequest::default()
            })
            .unwrap();
        assert_eq!(
            page.total, 2,
            "both Jan-1 10:00Z messages, not the undated one"
        );
        assert!(page.hits.iter().all(|h| h.sent_at.is_some()));
    }

    #[test]
    fn filters_author_source_and_attachment() {
        let s = Storage::open_in_memory().unwrap();
        let mut with_media = NewMessage::new(
            "demo",
            "w1",
            "Ana",
            "2026-01-01T10:00:00Z",
            "media message body",
        );
        with_media.raw_json = serde_json::json!({
            "media": [{ "sha256": "h", "path": "media/h.jpg", "name": "pic.jpg", "kind": "image", "size": 10 }]
        })
        .to_string();
        s.insert_message(&with_media).unwrap();
        let mut placeholder = NewMessage::new(
            "demo",
            "w2",
            "Borna",
            "2026-01-02T10:00:00Z",
            "placeholder message",
        );
        placeholder.raw_json =
            serde_json::json!({ "attachment": { "kind": "image", "name": "lost.jpg" } })
                .to_string();
        s.insert_message(&placeholder).unwrap();
        s.insert_message(&NewMessage::new(
            "demo",
            "w3",
            "Ana",
            "2026-01-03T10:00:00Z",
            "plain message",
        ))
        .unwrap();

        let find = |req: SearchArchiveRequest| {
            s.search_archive(&req)
                .unwrap()
                .hits
                .iter()
                .map(|h| h.id)
                .collect::<Vec<i64>>()
        };

        // Unicode-folded author substring (browse is newest-first).
        assert_eq!(
            find(SearchArchiveRequest {
                author: "aNA".into(),
                ..SearchArchiveRequest::default()
            }),
            vec![3, 1]
        );
        // Attachment filter matches preserved media AND the explicit
        // missing reference (newest first).
        assert_eq!(
            find(SearchArchiveRequest {
                has_attachment: Some(true),
                ..SearchArchiveRequest::default()
            }),
            vec![2, 1]
        );
        assert_eq!(
            find(SearchArchiveRequest {
                has_attachment: Some(false),
                ..SearchArchiveRequest::default()
            }),
            vec![3]
        );
        // Summary counts: preserved = available, placeholder = not.
        let page = s
            .search_archive(&SearchArchiveRequest {
                has_attachment: Some(true),
                ..SearchArchiveRequest::default()
            })
            .unwrap();
        let by_id: std::collections::HashMap<i64, &MessageSummary> =
            page.hits.iter().map(|h| (h.id, h)).collect();
        assert_eq!(
            (
                by_id[&1].attachment_count,
                by_id[&1].available_attachment_count
            ),
            (1, 1)
        );
        assert_eq!(
            (
                by_id[&2].attachment_count,
                by_id[&2].available_attachment_count
            ),
            (1, 0)
        );
    }

    #[test]
    fn import_scope_filter_and_listing() {
        let s = Storage::open_in_memory().unwrap();
        let batch = |import_id: &str, ids: &[&str]| {
            let messages: Vec<NewMessage> = ids
                .iter()
                .map(|id| NewMessage::new("demo", id.to_string(), "Ana", "", format!("body {id}")))
                .collect();
            s.commit_import_batch(import_id, "demo", &messages, &[], 0, true)
                .unwrap();
        };
        batch("imp-early", &["a", "b"]);
        batch("imp-late", &["c"]);

        let page = s
            .search_archive(&SearchArchiveRequest {
                import_job_id: Some("imp-early".into()),
                ..SearchArchiveRequest::default()
            })
            .unwrap();
        assert_eq!(page.total, 2);
        assert!(page.hits.iter().all(|h| !h.source_id.is_empty()));

        let scopes = s.import_scopes().unwrap();
        let ids: Vec<&str> = scopes.iter().map(|s| s.import_id.as_str()).collect();
        assert!(ids.contains(&"imp-early") && ids.contains(&"imp-late"));
        let late = scopes.iter().find(|s| s.import_id == "imp-late").unwrap();
        assert_eq!(late.messages, 1);

        // The detail view exposes the stamped occurrence provenance.
        let detail = s.message_detail(1).unwrap().unwrap();
        assert!(detail
            .occurrences
            .iter()
            .any(|o| o.import_id == "imp-early"));
    }

    #[test]
    fn detail_carries_metadata_provenance_and_revisions() {
        let s = seeded();
        let detail = s.message_detail(1).unwrap().unwrap();
        assert_eq!(detail.body, "old alpha delivery date");
        assert_eq!(detail.source_label, "demo");
        assert_eq!(detail.time_confidence, "legacy_assumed");
        assert_eq!(detail.original_timestamp, "2026-01-01T10:00:00Z");
        assert!(detail.revisions.is_empty(), "no conflicts in this fixture");
        assert!(s.message_detail(9999).unwrap().is_none());

        // Conflicting revisions of one identity: both rows archived, each
        // summary/detail links the other (visible conflict marker).
        let instance = s
            .ensure_source_instance(&crate::storage::NewSourceInstance {
                kind: "imap",
                label: "Test account",
                connector_key: "imap:test.example:993:user@example.com",
                config_json: "{}",
            })
            .unwrap();
        let mail = |body: &str| {
            crate::message::NewMessage::new(
                "imap",
                "<mid-1@x>",
                "Ana",
                "2026-02-01T10:00:00Z",
                body,
            )
        };
        fn identity<'a>(rev: &'a str) -> crate::storage::ConnectorIdentity<'a> {
            crate::storage::ConnectorIdentity {
                namespace: "imap",
                external_key: "<mid-1@x>",
                revision_hash: rev,
            }
        }
        let occurrence = crate::storage::Occurrence {
            scope: "imap:INBOX",
            provider_item_id: "7",
            uidvalidity: Some(1),
        };
        let first = s
            .insert_mail_message(
                &instance,
                &mail("first revision body"),
                "Subject one",
                &identity("r1"),
                &occurrence,
            )
            .unwrap();
        let second = s
            .insert_mail_message(
                &instance,
                &mail("second revision body"),
                "Subject two",
                &identity("r2"),
                &occurrence,
            )
            .unwrap();
        let (id_a, id_b) = match (first, second) {
            (
                crate::storage::MailInsertOutcome::Inserted { id: a, .. },
                crate::storage::MailInsertOutcome::Inserted {
                    id: b,
                    conflicting_revision: true,
                },
            ) => (a, b),
            other => panic!("expected two inserted revisions, got {other:?}"),
        };
        let detail_a = s.message_detail(id_a).unwrap().unwrap();
        assert_eq!(
            detail_a
                .revisions
                .iter()
                .map(|r| r.message_id)
                .collect::<Vec<_>>(),
            vec![id_b]
        );
        // Search summaries flag the conflict too.
        let page = s
            .search_archive(&SearchArchiveRequest {
                text: "revision".into(),
                ..SearchArchiveRequest::default()
            })
            .unwrap();
        let conflicting: Vec<i64> = page
            .hits
            .iter()
            .filter(|h| h.has_conflicting_revision)
            .map(|h| h.id)
            .collect();
        assert!(conflicting.contains(&id_a) && conflicting.contains(&id_b));
    }

    #[test]
    fn context_pages_chronologically_with_cursors_and_stops_at_unverified() {
        let s = Storage::open_in_memory().unwrap();
        // A verified conversation (provider key) with five dated messages.
        for i in 0..5 {
            let mut msg = NewMessage::new(
                "demo",
                format!("t{i}"),
                "Ana",
                format!("2026-01-01T0{}:00:00Z", i),
                format!("thread body {i}"),
            );
            msg.channel = "chat".into();
            msg.chat_id = "team".into();
            msg.conversation_key = "telegram:chat:1".into();
            s.insert_message(&msg).unwrap();
        }
        let ctx = s
            .conversation_context(3, None, None, Some(2))
            .unwrap()
            .unwrap();
        assert_eq!(ctx.anchor.id, 3);
        assert_eq!(
            ctx.before.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            ctx.after.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert!(!ctx.unverified_conversation);
        // Both sides exhausted exactly at the page size: no more.
        assert!(!ctx.has_more_before && !ctx.has_more_after);

        // Smaller pages set the load-more flags on both sides.
        let small = s
            .conversation_context(3, None, None, Some(1))
            .unwrap()
            .unwrap();
        assert_eq!(
            small.before.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(
            small.after.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![4]
        );
        assert!(small.has_more_before && small.has_more_after);

        // "Load earlier/later": cursors at the outermost held ids page
        // further without losing the anchor.
        let wider = s
            .conversation_context(3, Some(2), Some(4), Some(2))
            .unwrap()
            .unwrap();
        assert_eq!(
            wider.before.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![1]
        );
        assert!(!wider.has_more_before);
        assert_eq!(
            wider.after.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![5]
        );
        assert!(!wider.has_more_after);
        assert_eq!(wider.anchor.id, 3);

        // A standalone message (no conversation) returns the anchor alone.
        let standalone = seeded();
        let ctx = standalone
            .conversation_context(1, None, None, Some(8))
            .unwrap()
            .unwrap();
        assert!(ctx.before.is_empty() && ctx.after.is_empty());
        assert!(!ctx.unverified_conversation);

        // A legacy-unresolved grouping is honest: anchor only, flagged.
        let legacy = Storage::open_in_memory().unwrap();
        for i in 0..3 {
            let mut msg = NewMessage::new(
                "demo",
                format!("l{i}"),
                "Ana",
                format!("2026-01-01T0{}:00:00Z", i),
                format!("legacy body {i}"),
            );
            msg.channel = "chat".into();
            msg.chat_id = "oldgroup".into(); // no provider key → legacy grouping
            legacy.insert_message(&msg).unwrap();
        }
        let ctx = legacy
            .conversation_context(2, None, None, Some(8))
            .unwrap()
            .unwrap();
        assert!(ctx.unverified_conversation);
        assert!(ctx.before.is_empty() && ctx.after.is_empty());
        assert!(legacy
            .conversation_context(999, None, None, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn snippet_is_bounded_to_500_chars() {
        let s = Storage::open_in_memory().unwrap();
        let body = format!("alpha {}", "word ".repeat(2000));
        s.insert_message(&NewMessage::new(
            "demo",
            "long",
            "Ana",
            "2026-01-01T10:00:00Z",
            body,
        ))
        .unwrap();
        let page = s
            .search_archive(&SearchArchiveRequest {
                text: "alpha".into(),
                ..SearchArchiveRequest::default()
            })
            .unwrap();
        assert!(page.hits[0].snippet.chars().count() <= MAX_SNIPPET_CHARS);
    }

    #[test]
    fn selection_export_renders_text_and_json_with_provenance() {
        let s = Storage::open_in_memory().unwrap();
        let messages: Vec<NewMessage> = ["a", "b"]
            .into_iter()
            .map(|id| {
                NewMessage::new(
                    "demo",
                    id.to_string(),
                    "Ana",
                    "2026-01-01T10:00:00Z",
                    format!("body of {id}"),
                )
            })
            .collect();
        s.commit_import_batch("imp-x", "demo", &messages, &[], 0, true)
            .unwrap();

        let mut text = Vec::new();
        let summary = export_selection(&s, &[2, 1, 999], SelectionFormat::Text, &mut text).unwrap();
        assert_eq!((summary.written, summary.missing), (2, 1));
        let rendered = String::from_utf8(text).unwrap();
        assert!(rendered.contains("=== Message 2 ==="), "{rendered}");
        assert!(rendered.contains("body of b"));
        assert!(rendered.contains("Source: demo"));
        assert!(rendered.contains("Date: 2026-01-01T10:00:00Z"));
        assert!(rendered.contains("import imp-x"), "provenance included");
        // Selection order is preserved (2 before 1).
        assert!(rendered.find("Message 2").unwrap() < rendered.find("Message 1").unwrap());

        let mut json = Vec::new();
        let summary = export_selection(&s, &[1], SelectionFormat::Json, &mut json).unwrap();
        assert_eq!((summary.written, summary.missing), (1, 0));
        let parsed: Vec<MessageDetail> = serde_json::from_slice(&json).unwrap();
        assert_eq!(parsed[0].body, "body of a");
        assert_eq!(parsed[0].occurrences.len(), 1);
        assert!(SelectionFormat::parse("csv").is_err());
    }
}

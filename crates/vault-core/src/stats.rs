//! Archive statistics for the dashboard: totals, the archive's date range,
//! and message counts grouped by source, day, chat, and author.
//!
//! [`Storage::archive_stats`] computes all of it from the trigger-
//! maintained counter tables (schema migration 4) in one call, so the
//! dashboard answers within its 100 ms budget on million-message archives
//! (asserted by `tests/stats_perf.rs`).

use serde::{Deserialize, Serialize};

use crate::message::SourceCount;

/// How many chats/authors the dashboard tables show by default.
pub const DEFAULT_TOP: u32 = 10;
/// Upper bound for the chat/author table size (keeps responses bounded).
pub const MAX_TOP: u32 = 100;

/// Everything the dashboard renders, in one IPC round trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveStats {
    /// Total messages in the archive.
    pub total: u64,
    /// Earliest non-empty `sent_at` (RFC 3339 UTC); `None` when nothing is
    /// dated.
    pub first_sent_at: Option<String>,
    /// Latest non-empty `sent_at`; `None` when nothing is dated.
    pub last_sent_at: Option<String>,
    /// Every source with its message count, largest first.
    pub by_source: Vec<SourceCount>,
    /// Messages per day (`YYYY-MM-DD`), day ascending, over the whole
    /// archive. Days without messages are omitted; messages with an
    /// unknown `sent_at` are excluded. The frontend downsamples to months
    /// or years when the series is too long to chart as days.
    pub by_day: Vec<DayCount>,
    /// The chats with the most messages, count descending. Messages
    /// without a `chat_id` (single-mailbox imports) are excluded — they
    /// are not conversations.
    pub top_chats: Vec<ChatCount>,
    /// The authors with the most messages, count descending.
    pub top_authors: Vec<AuthorCount>,
}

/// Messages archived on one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayCount {
    /// The UTC day as `YYYY-MM-DD` (the stored `sent_at` prefix).
    pub day: String,
    pub count: u64,
}

/// Messages archived in one chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatCount {
    pub source: String,
    pub chat_id: String,
    pub count: u64,
}

/// Messages archived from one author.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorCount {
    /// Display author; empty for messages whose author is unknown.
    pub author: String,
    pub count: u64,
}

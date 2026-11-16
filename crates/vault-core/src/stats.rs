//! Archive statistics for the dashboard: totals, the archive's date range,
//! and message counts grouped by source, day, chat, and author — plus the
//! small what's-new digest types (the M7 follow-up's boot surface; see
//! [`crate::whats_new`] for the derivation).
//!
//! [`Storage::archive_stats`] computes all of it from the trigger-
//! maintained counter tables (schema migration 6) in one call, so the
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

// ---------------------------------------------------------------------------
// What's-new boot digest (the M7 follow-up)
// ---------------------------------------------------------------------------

/// One source's newly archived messages in the what's-new boot digest:
/// the durable instance id (so the digest row keys and filters stably)
/// plus the label the source chips show, and the count of messages
/// archived since the previous visit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceNewCount {
    /// The durable `source_instances.id`.
    pub source_id: String,
    /// Display label (the same `COALESCE` the stats chips show).
    pub label: String,
    pub count: u64,
}

/// One finished or failed import or sync job in the what's-new boot
/// digest: enough for a one-line "what happened while you were away"
/// entry, never the ledger row's full serialized state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatsNewJob {
    /// The job kind (`import_mbox`, `import_chat_export`, `imap_sync`,
    /// `imap_sync_all` — the kinds [`crate::whats_new`] reports).
    pub kind: String,
    /// `"succeeded"` or `"failed"`.
    pub state: String,
    /// RFC 3339 UTC — when the job finished.
    pub finished_at: String,
    /// The one-line result summary the ledger row carries.
    pub summary: String,
}

/// What changed in the vault since the previous visit (the what's-new
/// boot digest, derived by [`crate::whats_new`]): newly archived
/// messages per source plus finished or failed imports and syncs.
/// Reading it advances the last-visit timestamp, so the next digest
/// counts only newer changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatsNewDigest {
    /// RFC 3339 UTC — the previous visit this digest covers. `None` on
    /// the first visit (or when the stored record was unreadable): the
    /// digest is deliberately empty then — a quiet first boot, never a
    /// whole-archive dump.
    pub since: Option<String>,
    /// RFC 3339 UTC — the instant this read recorded as the new
    /// last-visit (the cutoff the next digest counts from).
    pub read_at: String,
    /// Newly archived messages per source since `since`, heaviest first.
    pub new_messages: Vec<SourceNewCount>,
    /// Finished or failed import/sync jobs since `since`, newest first.
    pub jobs: Vec<WhatsNewJob>,
}

impl WhatsNewDigest {
    /// Whether anything changed — the quiet surface shows nothing at
    /// all when the digest is empty.
    pub fn is_empty(&self) -> bool {
        self.new_messages.is_empty() && self.jobs.is_empty()
    }
}

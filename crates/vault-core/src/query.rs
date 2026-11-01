//! Paged, filtered search queries and chat-thread context over the archive.
//!
//! Date filters are LOCAL-day bounds, matching what the UI displays: each
//! `YYYY-MM-DD` bound is translated into the UTC instant of local midnight
//! (of the day itself for `from_date`, of the following day for `to_date`)
//! before being compared with the stored UTC `sent_at`. Messages whose
//! `sent_at` is empty (an unparseable or missing date) are excluded whenever
//! any date bound is active: they have no defined position on the timeline,
//! so both bounds omit them consistently.

use chrono::{FixedOffset, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::message::Message;
use crate::Error;

/// Marker characters wrapped around FTS5-matched terms inside snippets.
///
/// Control characters are used because they cannot occur in normalized FTS5
/// token text and are vanishingly rare in real message bodies; the frontend
/// splits [`SearchHit::snippet`] on them to render highlights.
pub const SNIPPET_OPEN_MARK: char = '\u{1}';
pub const SNIPPET_CLOSE_MARK: char = '\u{2}';

/// One search request: free text plus optional facet filters.
///
/// All filters are conjunctive; every string filter is empty when inactive.
/// `from_date`/`to_date` are inclusive local-day `YYYY-MM-DD` bounds; each
/// is translated to the UTC instant of local midnight before being compared
/// against the RFC 3339 `sent_at` column (same fixed format, so text order
/// is time order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Free text; sanitized into an FTS5 MATCH expression when non-empty.
    /// An empty text browses the archive (newest first) under the filters.
    pub text: String,
    /// Restrict to these ingest sources (e.g. `"mbox"`, `"whatsapp"`);
    /// empty means all sources.
    pub sources: Vec<String>,
    /// Case-insensitive substring filter on the author.
    pub author: String,
    /// Inclusive lower day bound (`YYYY-MM-DD`), empty when unbounded.
    pub from_date: String,
    /// Inclusive upper day bound (`YYYY-MM-DD`), empty when unbounded.
    pub to_date: String,
    /// Page size, clamped to `1..=MAX_LIMIT`.
    pub limit: u32,
    /// Page offset, clamped to `0..`.
    pub offset: u32,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            sources: Vec::new(),
            author: String::new(),
            from_date: String::new(),
            to_date: String::new(),
            limit: DEFAULT_LIMIT,
            offset: 0,
        }
    }
}

impl SearchQuery {
    /// Clamp `limit`/`offset` into range and validate the day bounds.
    pub fn normalize(&mut self) -> Result<(), Error> {
        self.limit = self.limit.clamp(1, MAX_LIMIT);
        self.offset = self.offset.min(MAX_OFFSET);
        self.sources.retain(|s| !s.trim().is_empty());
        self.sources.dedup();
        for (label, date) in [("from_date", &self.from_date), ("to_date", &self.to_date)] {
            if !date.is_empty() && parse_day(date).is_none() {
                return Err(Error::InvalidQuery(format!(
                    "{label} must be YYYY-MM-DD, got {date:?}"
                )));
            }
        }
        Ok(())
    }
}

/// Upper bound for page size (keeps single responses small; the UI paginates).
pub const MAX_LIMIT: u32 = 200;
/// Upper bound for page offset.
pub const MAX_OFFSET: u32 = 1_000_000;
/// Default page size used by the UI.
pub const DEFAULT_LIMIT: u32 = 50;

/// A single search result: the message plus a compact body snippet.
///
/// `snippet` is `None` in browse mode (empty text), where there are no match
/// positions to mark; the frontend falls back to truncating the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub message: Message,
    /// Body fragment around the best match, with
    /// [`SNIPPET_OPEN_MARK`]/[`SNIPPET_CLOSE_MARK`] around matched terms.
    pub snippet: Option<String>,
}

/// One page of results plus the total match count for pagination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    /// Rows matching the query ignoring `limit`/`offset`.
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
}

/// A message together with its adjacent messages in the same chat thread.
///
/// `before` and `after` are ordered oldest → newest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatContext {
    pub before: Vec<Message>,
    pub message: Message,
    pub after: Vec<Message>,
}

/// Parse a strict `YYYY-MM-DD` day (no time part).
pub(crate) fn parse_day(input: &str) -> Option<NaiveDate> {
    if input.len() != 10 {
        return None;
    }
    let bytes = input.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let digits: Vec<u8> = input.bytes().filter(|b| b.is_ascii_digit()).collect();
    if digits.len() != 8 {
        return None;
    }
    NaiveDate::parse_from_str(input, "%Y-%m-%d").ok()
}

/// The UTC instant (RFC 3339 `…Z`) of local midnight starting `day` at
/// `offset` — the shared boundary the date filters compare against, so a
/// day bound selects exactly what the UI renders as that local day.
pub(crate) fn local_day_instant_utc(day: NaiveDate, offset: FixedOffset) -> String {
    let local_midnight = day.and_hms_opt(0, 0, 0).expect("00:00:00 is a valid time");
    (local_midnight - offset)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// Escape `%`, `_` and `\` for a LIKE pattern with `ESCAPE '\'`.
pub(crate) fn escape_like(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_clamps_limit_and_offset() {
        let mut q = SearchQuery {
            limit: 0,
            offset: u32::MAX,
            ..SearchQuery::default()
        };
        q.normalize().unwrap();
        assert_eq!(q.limit, 1);
        assert_eq!(q.offset, MAX_OFFSET);
        let mut q = SearchQuery {
            limit: 99_999,
            ..SearchQuery::default()
        };
        q.normalize().unwrap();
        assert_eq!(q.limit, MAX_LIMIT);
    }

    #[test]
    fn normalize_rejects_bad_dates() {
        let mut q = SearchQuery {
            from_date: "2026-13-01".into(),
            ..SearchQuery::default()
        };
        assert!(matches!(q.normalize(), Err(Error::InvalidQuery(_))));
        let mut q = SearchQuery {
            to_date: "01-02-2026".into(),
            ..SearchQuery::default()
        };
        assert!(matches!(q.normalize(), Err(Error::InvalidQuery(_))));
    }

    #[test]
    fn normalize_accepts_good_dates_and_dedups_sources() {
        let mut q = SearchQuery {
            from_date: "2026-01-31".into(),
            to_date: "2026-12-31".into(),
            sources: vec!["mbox".into(), "mbox".into(), " ".into()],
            ..SearchQuery::default()
        };
        q.normalize().unwrap();
        assert_eq!(q.sources, vec!["mbox"]);
    }

    #[test]
    fn parse_day_strict() {
        assert_eq!(
            parse_day("2026-02-29"), // not a leap year
            None
        );
        assert!(parse_day("2028-02-29").is_some());
        assert!(parse_day("2026-1-01").is_none());
        assert!(parse_day("2026-01-01T00").is_none());
        assert!(parse_day("").is_none());
    }

    #[test]
    fn local_midnight_rolls_over_month_and_year() {
        let plus_one = FixedOffset::east_opt(3600).unwrap();
        let utc = FixedOffset::east_opt(0).unwrap();
        // 2026-01-06 local midnight at UTC+1 is 2026-01-05T23:00Z.
        assert_eq!(
            local_day_instant_utc(parse_day("2026-01-06").unwrap(), plus_one),
            "2026-01-05T23:00:00Z"
        );
        // At UTC the local midnight is the plain day boundary.
        assert_eq!(
            local_day_instant_utc(parse_day("2026-01-06").unwrap(), utc),
            "2026-01-06T00:00:00Z"
        );
        // Month rollover: Feb 1 local midnight at UTC+1 lands in January.
        assert_eq!(
            local_day_instant_utc(parse_day("2026-02-01").unwrap(), plus_one),
            "2026-01-31T23:00:00Z"
        );
        // Year rollover: Jan 1 local midnight at UTC+1 lands in December.
        assert_eq!(
            local_day_instant_utc(parse_day("2026-01-01").unwrap(), plus_one),
            "2025-12-31T23:00:00Z"
        );
    }

    #[test]
    fn escape_like_escapes_wildcards() {
        assert_eq!(escape_like("50%_of\\it"), "50\\%\\_of\\\\it");
        assert_eq!(escape_like("plain"), "plain");
    }
}

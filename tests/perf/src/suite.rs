//! The benchmark query suite: the ten paged-search queries every run
//! times. Pure data — the p95 budgets live in the committed
//! [`crate::protocol::Baseline`] file, not here, so the suite cannot
//! drift out of sync with the citable artifact.

use vault_core::query::SearchQuery;

use crate::fixture::{
    word_at, AUTHOR_HANDLE_RANK, DATE_FILTER_FROM, DATE_FILTER_TO, FREQUENT_RANK, RARE_RANK,
    UBIQUITOUS_RANK, UNCOMMON_RANK,
};

/// One benchmark query: the free text exactly as typed into the app
/// (empty text = newest-first browse), optional inclusive date range,
/// page size, and page offset.
#[derive(Debug, Clone)]
pub struct QuerySpec {
    pub label: &'static str,
    pub text: String,
    pub date_from: Option<&'static str>,
    pub date_to: Option<&'static str>,
    pub limit: u32,
    pub offset: u32,
}

impl QuerySpec {
    /// The suite, in run order. Labels are stable identifiers: they key
    /// the budgets in `baseline.json` and the rows in every recorded
    /// table (see BASELINE.md and docs/benchmark.md).
    pub fn suite() -> Vec<QuerySpec> {
        vec![
            QuerySpec {
                label: "ubiquitous-term",
                text: word_at(UBIQUITOUS_RANK),
                date_from: None,
                date_to: None,
                limit: 50,
                offset: 0,
            },
            QuerySpec {
                label: "frequent-term",
                text: word_at(FREQUENT_RANK),
                date_from: None,
                date_to: None,
                limit: 50,
                offset: 0,
            },
            QuerySpec {
                label: "rare-term",
                text: word_at(RARE_RANK),
                date_from: None,
                date_to: None,
                limit: 50,
                offset: 0,
            },
            QuerySpec {
                label: "two-term-and",
                text: format!("{} {}", word_at(FREQUENT_RANK), word_at(UNCOMMON_RANK)),
                date_from: None,
                date_to: None,
                limit: 50,
                offset: 0,
            },
            QuerySpec {
                label: "author-term",
                text: word_at(AUTHOR_HANDLE_RANK),
                date_from: None,
                date_to: None,
                limit: 50,
                offset: 0,
            },
            QuerySpec {
                label: "frequent+date-filter",
                text: word_at(FREQUENT_RANK),
                date_from: Some(DATE_FILTER_FROM),
                date_to: Some(DATE_FILTER_TO),
                limit: 50,
                offset: 0,
            },
            QuerySpec {
                label: "frequent-paged-200",
                text: word_at(FREQUENT_RANK),
                date_from: None,
                date_to: None,
                limit: 200,
                offset: 0,
            },
            QuerySpec {
                label: "frequent-deep-page",
                text: word_at(FREQUENT_RANK),
                date_from: None,
                date_to: None,
                limit: 50,
                offset: 5_000,
            },
            QuerySpec {
                label: "browse-newest",
                text: String::new(),
                date_from: None,
                date_to: None,
                limit: 50,
                offset: 0,
            },
            QuerySpec {
                label: "browse-deep-page",
                text: String::new(),
                date_from: None,
                date_to: None,
                limit: 50,
                offset: 100_000,
            },
        ]
    }

    /// The exact `SearchQuery` the app's `search_messages` IPC builds.
    pub fn query(&self) -> SearchQuery {
        SearchQuery {
            text: self.text.clone(),
            sources: Vec::new(),
            author: String::new(),
            from_date: self.date_from.unwrap_or_default().to_string(),
            to_date: self.date_to.unwrap_or_default().to_string(),
            limit: self.limit,
            offset: self.offset,
            ..Default::default()
        }
    }
}

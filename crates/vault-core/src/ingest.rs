//! Ingest abstraction: a [`Source`] yields [`NewMessage`]s which are stored
//! idempotently.

use serde::{Deserialize, Serialize};

use crate::message::{InsertOutcome, NewMessage};
use crate::storage::Storage;
use crate::Result;

/// A source of archived messages.
///
/// Implementations read from anywhere (an `.mbox` file, an IMAP mailbox, a
/// chat export) and return complete messages; storage handles dedup so a
/// source may safely be re-collected.
pub trait Source {
    /// Stable source name used as the dedup namespace (e.g. `"mbox"`).
    fn name(&self) -> &str;
    /// Collect the messages currently available from this source. Sources
    /// that can encounter unparseable items count them in `Collected::failed`
    /// instead of dropping them silently.
    fn collect(&self) -> Result<Collected>;
}

/// Messages a [`Source`] encountered during one collection pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collected {
    /// Messages parsed successfully and yielded for storage.
    pub messages: Vec<NewMessage>,
    /// Messages the source encountered but could not parse.
    pub failed: u64,
}

/// Outcome of ingesting one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestSummary {
    pub source: String,
    /// Messages the source encountered: yielded messages plus parse failures
    /// (`seen == inserted + skipped + failed`).
    pub seen: u64,
    /// Messages newly stored.
    pub inserted: u64,
    /// Messages skipped because their `(source, external_id)` was already
    /// present.
    pub skipped: u64,
    /// Messages that failed to parse and were not stored.
    pub failed: u64,
}

/// Pull every message from `source` into `storage`, idempotently.
pub fn ingest(storage: &Storage, source: &dyn Source) -> Result<IngestSummary> {
    let Collected { messages, failed } = source.collect()?;
    let mut summary = IngestSummary {
        source: source.name().to_string(),
        seen: (messages.len() as u64) + failed,
        inserted: 0,
        skipped: 0,
        failed,
    };
    for msg in &messages {
        match storage.insert_message(msg)? {
            InsertOutcome::Inserted(_) => summary.inserted += 1,
            InsertOutcome::Duplicate(_) => summary.skipped += 1,
        }
    }
    Ok(summary)
}

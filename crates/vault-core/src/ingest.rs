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
    /// Collect the messages currently available from this source.
    fn collect(&self) -> Result<Vec<NewMessage>>;
}

/// Outcome of ingesting one source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestSummary {
    pub source: String,
    /// Messages yielded by the source.
    pub seen: u64,
    /// Messages newly stored.
    pub inserted: u64,
    /// Messages skipped because their `(source, external_id)` was already
    /// present.
    pub skipped: u64,
}

/// Pull every message from `source` into `storage`, idempotently.
pub fn ingest(storage: &Storage, source: &dyn Source) -> Result<IngestSummary> {
    let messages = source.collect()?;
    let mut summary = IngestSummary {
        source: source.name().to_string(),
        seen: messages.len() as u64,
        inserted: 0,
        skipped: 0,
    };
    for msg in &messages {
        match storage.insert_message(msg)? {
            InsertOutcome::Inserted(_) => summary.inserted += 1,
            InsertOutcome::Duplicate(_) => summary.skipped += 1,
        }
    }
    Ok(summary)
}

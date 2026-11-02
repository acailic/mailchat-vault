//! Ingest abstraction: a [`Source`] yields [`NewMessage`]s which are stored
//! idempotently.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::media::MediaSummary;
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
    let collected = source.collect()?;
    ingest_collected(storage, source.name(), collected, None)
}

/// Store already-collected messages, optionally backfilling media links.
///
/// With `media` present, messages whose `raw_json` carries a `media` array
/// are counted in [`MediaSummary::linked`] — newly inserted rows because
/// their links were just written, duplicate rows when merging fresh media
/// entries into the existing row's `raw_json` changed it (see
/// [`Storage::attach_media`]). Chat imports pass a summary here; every
/// other source passes `None`.
pub fn ingest_collected(
    storage: &Storage,
    source_name: &str,
    collected: Collected,
    mut media: Option<&mut MediaSummary>,
) -> Result<IngestSummary> {
    let Collected { messages, failed } = collected;
    let mut summary = IngestSummary {
        source: source_name.to_string(),
        seen: (messages.len() as u64) + failed,
        inserted: 0,
        skipped: 0,
        failed,
    };
    for msg in &messages {
        // Only chat imports pass a media summary; skip the JSON parse
        // entirely for every other source.
        let entries = if media.is_some() {
            media_entries(&msg.raw_json).unwrap_or_default()
        } else {
            Vec::new()
        };
        match storage.insert_message(msg)? {
            InsertOutcome::Inserted(_) => {
                summary.inserted += 1;
                if let Some(summary) = media.as_deref_mut() {
                    if !entries.is_empty() {
                        summary.linked += 1;
                    }
                }
            }
            InsertOutcome::Duplicate(id) => {
                summary.skipped += 1;
                if let Some(summary) = media.as_deref_mut() {
                    if !entries.is_empty() && storage.attach_media(id, &entries)? {
                        summary.linked += 1;
                    }
                }
            }
        }
    }
    Ok(summary)
}

/// The `media` array of a message's `raw_json`, if it has a non-empty one.
fn media_entries(raw_json: &str) -> Option<Vec<Value>> {
    let value: Value = serde_json::from_str(raw_json).ok()?;
    let entries = value.get("media")?.as_array()?;
    if entries.is_empty() {
        None
    } else {
        Some(entries.clone())
    }
}

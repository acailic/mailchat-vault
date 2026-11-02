//! Telegram chat export source: parses the `result.json` written by
//! Telegram Desktop's "Export chat history".
//!
//! The export archives text: every original message object is kept
//! verbatim in `raw_json`, so attachments (`photo`, `file`, …), reply
//! references, and forwarded metadata stay recorded; when the export
//! ships with its files, the media pass (see [`crate::media`]) copies
//! them into the content-addressed store and adds a `media` array to
//! `raw_json`. Timestamps prefer `date_unixtime` (UTC-exact; recorded
//! with confidence `explicit_offset`); the naive `date` string is a
//! fallback stored as UTC, exactly as the export writes it (confidence
//! `legacy_assumed`, zone `UTC`). Service entries (calls, joins,
//! pins) carry no body and are skipped.
//!
//! The conversation key (M1.3) is `telegram:chat:<export id>` — the
//! provider-guaranteed chat id — so the same chat re-exported after a
//! rename stays one conversation (the label follows the newest name),
//! while two different chats sharing a display name stay distinct.
//! Exports without an id cannot be safely keyed and keep the legacy
//! name grouping.

use std::fs;
use std::path::PathBuf;

use chrono::{NaiveDateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::Result;

/// Source name and dedup namespace for Telegram imports.
pub const TELEGRAM_SOURCE: &str = "telegram";

/// Reads one Telegram `result.json` export and yields its messages.
#[derive(Debug, Clone)]
pub struct TelegramSource {
    path: PathBuf,
}

impl TelegramSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Source for TelegramSource {
    fn name(&self) -> &str {
        TELEGRAM_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        let bytes = fs::read(&self.path)?;
        let chat_id_fallback = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        parse_telegram(&bytes, &chat_id_fallback)
    }
}

/// The export top level; every message object is kept as raw JSON so
/// nothing in it is lost.
#[derive(Debug, Deserialize)]
struct TelegramExport {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    messages: Vec<Value>,
}

/// Parse Telegram export JSON into messages.
///
/// `chat_id_fallback` is used when the export carries neither a chat name
/// nor a chat id. `external_id` is `<chat>/<message id>`: message ids
/// repeat across chats, and the dedup key is namespaced by source only,
/// so the chat key comes from the stable export `id` when present — two
/// chats sharing a display name stay distinct, and a re-export after a
/// contact rename dedups instead of duplicating. Entries that are not
/// objects or have no id count as failed; service entries are skipped
/// silently.
pub fn parse_telegram(bytes: &[u8], chat_id_fallback: &str) -> Result<Collected> {
    let export: TelegramExport = serde_json::from_slice(bytes)?;
    let chat_id = export
        .name
        .as_deref()
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .or_else(|| export.id.as_ref().map(id_to_string))
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| chat_id_fallback.to_string());
    let dedup_chat = export
        .id
        .as_ref()
        .map(id_to_string)
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| chat_id.clone());
    // The provider-guaranteed chat id keys the conversation (M1.3);
    // exports without one stay on the legacy display-name grouping.
    let conversation_key = if export
        .id
        .as_ref()
        .map(id_to_string)
        .is_some_and(|c| !c.is_empty())
    {
        format!("telegram:chat:{dedup_chat}")
    } else {
        String::new()
    };

    let mut messages = Vec::new();
    let mut failed: u64 = 0;
    for entry in &export.messages {
        let Some(obj) = entry.as_object() else {
            failed += 1;
            continue;
        };
        // Entries with an explicit non-"message" type are service
        // events (calls, joins, pins) with no body: skipped. An entry
        // with no type at all is malformed (real exports always type
        // them) and counts as failed.
        match obj.get("type").and_then(Value::as_str) {
            Some("message") => {}
            Some(_) => continue,
            None => {
                failed += 1;
                continue;
            }
        }
        let Some(id) = obj.get("id").and_then(Value::as_i64) else {
            failed += 1;
            continue;
        };

        let author = obj
            .get("from")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| obj.get("from_id").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string();
        let (sent_at, original_timestamp, timezone, confidence) = telegram_sent_at(obj);
        let body = message_text(obj.get("text"));
        let external_id = format!("{}/{}", dedup_chat, id);

        let mut msg = NewMessage::new(TELEGRAM_SOURCE, external_id, author, sent_at, body);
        msg.channel = "chat".to_string();
        msg.chat_id = chat_id.clone();
        msg.conversation_key = conversation_key.clone();
        msg.original_timestamp = original_timestamp;
        msg.timezone = timezone;
        msg.time_confidence = confidence;
        // The whole original object: attachments, reply_to, forwarded
        // metadata, … all stay recorded in raw_json.
        msg.raw_json = entry.to_string();
        messages.push(msg);
    }
    Ok(Collected { messages, failed })
}

/// Detection-only view of the export: it just has to carry a `messages`
/// array. `IgnoredAny` elements are zero-sized, so deserializing skips
/// message bodies without building a [`serde_json::Value`] tree — a huge
/// file costs no memory proportional to its size.
#[derive(Deserialize)]
#[allow(dead_code)] // the field exists to make serde enforce array-ness
struct TelegramDetect {
    messages: Vec<serde::de::IgnoredAny>,
}

/// True when the bytes are a Telegram export: JSON carrying a `messages`
/// array.
pub fn is_telegram_export(bytes: &[u8]) -> bool {
    serde_json::from_slice::<TelegramDetect>(bytes).is_ok()
}

/// Same check, streaming from a reader: the file is walked token by token
/// and never buffered whole, so detection reads of arbitrarily large files
/// stay bounded. The reader is wrapped in a [`std::io::BufReader`] so the
/// scan is I/O-bound instead of issuing one read call per byte —
/// unbuffered, a 20 MB file costs ~20M single-byte reads and stalls the
/// watch loop for seconds.
pub fn is_telegram_export_reader<R: std::io::Read>(reader: R) -> bool {
    serde_json::from_reader::<std::io::BufReader<R>, TelegramDetect>(std::io::BufReader::new(
        reader,
    ))
    .is_ok()
}

/// Timestamp: `date_unixtime` is UTC-exact and wins (recorded with
/// confidence `explicit_offset`, zone `UTC`); the naive `date` string is
/// a fallback stored as UTC, exactly as the export writes it (confidence
/// `legacy_assumed`, zone `UTC`). Returns `(sent_at, original, timezone,
/// confidence)`; an unparseable stamp stays empty (no instant, no text).
fn telegram_sent_at(obj: &serde_json::Map<String, Value>) -> (String, String, String, String) {
    const RFC3339: &str = "%Y-%m-%dT%H:%M:%SZ";
    if let Some(unix_text) = obj.get("date_unixtime").and_then(Value::as_str) {
        if let Ok(unix) = unix_text.parse::<i64>() {
            if let Some(t) = Utc.timestamp_opt(unix, 0).single() {
                return (
                    t.format(RFC3339).to_string(),
                    unix_text.to_string(),
                    "UTC".to_string(),
                    "explicit_offset".to_string(),
                );
            }
        }
    }
    if let Some(date_text) = obj.get("date").and_then(Value::as_str) {
        if let Ok(naive) = NaiveDateTime::parse_from_str(date_text, "%Y-%m-%dT%H:%M:%S") {
            let naive_text = naive.format("%Y-%m-%dT%H:%M:%S").to_string();
            return (
                format!("{naive_text}Z"),
                naive_text,
                "UTC".to_string(),
                "legacy_assumed".to_string(),
            );
        }
    }
    (String::new(), String::new(), String::new(), String::new())
}

/// Telegram `text` is either a plain string or an array of plain strings
/// and entity objects (`{ "type": …, "text": … }`); array parts
/// concatenate in order, entities contributing their `text`.
fn message_text(text: Option<&Value>) -> String {
    match text {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|p| match p {
                Value::String(s) => s.as_str(),
                Value::Object(o) => o.get("text").and_then(Value::as_str).unwrap_or(""),
                _ => "",
            })
            .collect(),
        _ => String::new(),
    }
}

/// The chat `id` may be a number or a string in different exporter
/// versions.
fn id_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

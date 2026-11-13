//! Facebook Messenger (DYI JSON) export source: parses the
//! `messages/inbox/<thread>/message_*.json` files written by
//! "Download Your Information" with format set to JSON.
//!
//! Every original message object is kept verbatim in `raw_json` (text
//! and media references alike); the conversation key comes from the
//! thread folder's numeric suffix; and messages carry synthetic
//! external ids (Facebook exports none). This reader takes one thread
//! file at a time — the folder walk that merges a thread's
//! `message_1.json` … `message_N.json` parts is the tree import's job.
//!
//! The file is read with the same two-pass streaming as the other chat
//! JSON sources (pass 1: the header; pass 2: `messages`, one entry at a
//! time). Detection ([`is_messenger_export`]) runs ahead of the
//! Telegram importer's probe, whose format test is merely "has a
//! `messages` array" — [`crate::chat_export`] pins the detection order.

use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use serde::{Deserialize, Deserializer as _};
use serde_json::Value;

use crate::bounded::{EntryOutcome, Flow, StreamItem, StreamSource};
use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::util::fnv1a_fields;
use crate::Result;

/// Source name and dedup namespace for Messenger imports.
pub const MESSENGER_SOURCE: &str = "messenger";

/// Detection-only walk of a DYI message file (streamed: only the first
/// `messages` element is materialized, `participants` elements are
/// zero-sized `IgnoredAny`s), so detecting an arbitrarily large export
/// stays bounded. True only for an object with a `participants` array
/// plus a `messages` array whose first element carries `sender_name` or
/// `timestamp_ms` — the combination no other supported format has.
pub fn is_messenger_export(bytes: &[u8]) -> bool {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    de.deserialize_map(DetectVisitor).unwrap_or(false)
}

/// [`is_messenger_export`] streamed from a reader (the caller rewinds
/// between probes, as `chat_export::detect_chat_format_reader` does).
pub fn is_messenger_export_reader<R: std::io::Read>(reader: R) -> bool {
    let mut de = serde_json::Deserializer::from_reader(BufReader::new(reader));
    de.deserialize_map(DetectVisitor).unwrap_or(false)
}

struct DetectVisitor;

impl<'de> serde::de::Visitor<'de> for DetectVisitor {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a Messenger export object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<bool, A::Error> {
        // The whole map is walked (every other member is a zero-sized
        // `IgnoredAny`): serde_json checks the container's end even
        // after an early visitor return, so a mid-map `return Ok(true)`
        // would surface as a parse error and read as `false`.
        let mut participants = false;
        let mut sender_shaped = false;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "participants" => {
                    let _ = map.next_value::<Vec<serde::de::IgnoredAny>>()?; // array-ness
                    participants = true;
                }
                "messages" => {
                    sender_shaped = map.next_value_seed(FirstElementSender)?;
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(participants && sender_shaped)
    }
}

/// Reads only the first element of the `messages` array and reports
/// whether it is a Messenger message object.
struct FirstElementSender;

impl<'de> serde::de::DeserializeSeed<'de> for FirstElementSender {
    type Value = bool;

    fn deserialize<D: serde::de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<bool, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for FirstElementSender {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a messages array")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<bool, A::Error> {
        let Some(first) = seq.next_element::<Value>()? else {
            return Ok(false);
        };
        // The rest of the array is drained token by token through
        // zero-sized `IgnoredAny`s — bounded, and required: serde_json
        // errors on a visitor that stops mid-sequence.
        while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        let Some(obj) = first.as_object() else {
            return Ok(false);
        };
        Ok(obj.contains_key("sender_name") || obj.contains_key("timestamp_ms"))
    }
}

/// Reads one Messenger DYI thread file (`message_*.json`) and yields
/// its messages.
#[derive(Debug, Clone)]
pub struct MessengerSource {
    path: PathBuf,
}

impl MessengerSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Source for MessengerSource {
    fn name(&self) -> &str {
        MESSENGER_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        let bytes = fs::read(&self.path)?;
        parse_messenger(&bytes, &parent_folder_name(&self.path))
    }
}

/// The bounded streaming path (M2.2): two passes over the file, each
/// reading streamed — pass 1 reads only the header members (`title`,
/// `thread_path`, `participants`; the `messages` array is skipped token
/// by token through zero-sized `IgnoredAny`s), pass 2 walks the
/// `messages` array one element at a time. Peak memory is one message
/// entry plus the small header and the occurrence counter, whatever the
/// file's size.
impl StreamSource for MessengerSource {
    fn name(&self) -> &str {
        MESSENGER_SOURCE
    }

    fn stream(&self, skip: u64, emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()> {
        let folder_stem = parent_folder_name(&self.path);
        // Pass 1: the header only (streams; the messages array costs no
        // memory — IgnoredAny elements are zero-sized).
        let header: MessengerHeader =
            serde_json::from_reader(BufReader::new(fs::File::open(&self.path)?))?;
        let ctx = ThreadContext::from_header(&header, &folder_stem);
        // Pass 2: the messages array, one element at a time. The
        // occurrence map lives here and is shared by the visitor: ids
        // must count repeats across the whole file, not one entry.
        let mut failure = None;
        let (stopped, result) = {
            let mut state = crate::bounded::ConsumerState {
                skip,
                emit,
                stopped: false,
                failure: &mut failure,
            };
            let mut occ: HashMap<u64, u32> = HashMap::new();
            let result = {
                let visitor = MessagesVisitor {
                    ctx: &ctx,
                    occ: &mut occ,
                    state: &mut state,
                };
                let mut de = serde_json::Deserializer::from_reader(BufReader::new(fs::File::open(
                    &self.path,
                )?));
                de.deserialize_map(visitor)
            };
            (state.stopped, result)
        };
        if let Some(e) = failure {
            return Err(e);
        }
        match result {
            Ok(()) => Ok(()),
            Err(e) if stopped && crate::bounded::ConsumerState::is_stop_sentinel(&e) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// The header view of one thread file: `title`/`thread_path`/
/// `participants` are read small; `messages` is enforced to be an array
/// and skipped element by element (zero-sized `IgnoredAny`s).
#[derive(Debug, Deserialize)]
struct MessengerHeader {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    thread_path: Option<String>,
    #[serde(default)]
    participants: Vec<ParticipantName>,
    // The field exists to make serde enforce array-ness (the same trick
    // as `TelegramHeader`); IgnoredAny elements are zero-sized, so a
    // huge messages array costs no memory proportional to its size.
    // Never read — pass 2 walks the messages streamed.
    #[allow(dead_code)]
    messages: Vec<serde::de::IgnoredAny>,
}

#[derive(Debug, Deserialize)]
struct ParticipantName {
    #[serde(default)]
    name: Option<String>,
}

/// Conversation context shared by every entry of one thread: display
/// grouping, stable dedup thread key, and the M1.3 conversation key.
struct ThreadContext {
    chat_id: String,
    thread_key: String,
    conversation_key: String,
}

impl ThreadContext {
    fn from_header(header: &MessengerHeader, folder_stem: &str) -> Self {
        let participants = header
            .participants
            .iter()
            .filter_map(|p| p.name.as_deref().filter(|n| !n.is_empty()))
            .collect::<Vec<_>>()
            .join(", ");
        // Display grouping: the export's title, else the participant
        // list, else the folder name.
        let chat_id = header
            .title
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .or_else(|| (!participants.is_empty()).then(|| participants.clone()))
            .unwrap_or_else(|| folder_stem.to_string());
        let thread_key = thread_id(header.thread_path.as_deref(), folder_stem);
        let conversation_key = if thread_key.is_empty() {
            String::new()
        } else {
            format!("messenger:thread:{thread_key}")
        };
        Self {
            chat_id,
            thread_key,
            conversation_key,
        }
    }
}

/// The stable thread id: the trailing `_<digits>` group of the thread
/// folder name, from `thread_path` (e.g. `inbox/ana_1234567890`) or the
/// folder stem — whichever carries digits. Empty when neither does
/// (a hand-trimmed export): legacy chat_id grouping applies.
fn thread_id(thread_path: Option<&str>, folder_stem: &str) -> String {
    for candidate in [
        thread_path.unwrap_or("").rsplit('/').next().unwrap_or(""),
        folder_stem,
    ] {
        if let Some(pos) = candidate.rfind('_') {
            let digits = &candidate[pos + 1..];
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                return digits.to_string();
            }
        }
    }
    String::new()
}

/// The thread folder name a single `message_*.json` file sits in
/// (`…/<thread folder>/message_1.json` → the folder's name): the
/// fallback hint for `chat_id` and the thread id when the header's
/// `thread_path` carries neither. Empty at the filesystem root.
fn parent_folder_name(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Fields whose presence makes an entry a media message even without
/// `content` (Telegram's service-entry skip, scoped to chat metadata).
const MEDIA_FIELDS: [&str; 6] = [
    "photos",
    "videos",
    "audio_files",
    "files",
    "gifs",
    "sticker",
];

/// Convert one `messages` array entry through the shared mapping (used
/// by both the whole-file parser and the streaming pass, so their
/// behavior cannot drift apart).
fn messenger_entry(
    entry: &Value,
    ctx: &ThreadContext,
    occurrence: &mut HashMap<u64, u32>,
) -> EntryOutcome {
    let Some(obj) = entry.as_object() else {
        return EntryOutcome::Failed {
            external_ref: String::new(),
            reason: "entry is not a JSON object".to_string(),
        };
    };
    let body = obj
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let has_media = MEDIA_FIELDS.iter().any(|f| {
        obj.get(*f).is_some_and(|v| match v {
            Value::Array(a) => !a.is_empty(),
            Value::Object(_) => true,
            _ => false,
        })
    });
    let sender = obj.get("sender_name").and_then(Value::as_str);
    let ts = obj.get("timestamp_ms").and_then(Value::as_i64);
    if sender.is_none() && ts.is_none() {
        return EntryOutcome::Failed {
            external_ref: String::new(),
            reason: "entry is not a Messenger message".to_string(),
        };
    }
    if body.is_empty() && !has_media {
        return EntryOutcome::Skip;
    }
    let author = sender.unwrap_or_default().to_string();
    let ts_text = ts.map(|t| t.to_string()).unwrap_or_default();
    let sent_at = ts
        .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    // FB exports carry no message ids: a content hash over the stable
    // thread key + author + timestamp + body, with an occurrence counter
    // for byte-identical repeats (the WhatsApp formula).
    let base = fnv1a_fields(&[&ctx.thread_key, &author, &ts_text, &body]);
    let seen = occurrence.entry(base).or_insert(0);
    let external_id = format!("generated-{base:016x}-{seen}");
    *seen += 1;

    let mut msg = NewMessage::new(MESSENGER_SOURCE, external_id, author, sent_at, body);
    msg.channel = "chat".to_string();
    msg.chat_id = ctx.chat_id.clone();
    msg.conversation_key = ctx.conversation_key.clone();
    // The whole original object: media references, reactions, share
    // metadata — all stay recorded verbatim.
    msg.raw_json = entry.to_string();
    msg.original_timestamp = ts_text;
    msg.timezone = "UTC".to_string();
    msg.time_confidence = if ts.is_some() {
        "explicit_offset"
    } else {
        "missing"
    }
    .to_string();
    EntryOutcome::Message(msg)
}

/// serde visitor of the streaming pass: walks the top-level object,
/// streaming the `messages` array element by element; every other member
/// is skipped. Stops reading as soon as the consumer stops or fails (the
/// deserializer is used directly, without an end-of-input check, so an
/// early return leaves the reader where it is — nothing else is read).
struct MessagesVisitor<'a, 'c> {
    ctx: &'a ThreadContext,
    occ: &'a mut HashMap<u64, u32>,
    state: &'a mut crate::bounded::ConsumerState<'c>,
}

impl<'de, 'a, 'c> serde::de::Visitor<'de> for MessagesVisitor<'a, 'c> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a Messenger thread object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "messages" {
                map.next_value_seed(MessagesSeed {
                    ctx: self.ctx,
                    occ: self.occ,
                    state: self.state,
                })?;
                // The messages were streamed; trailing members are
                // irrelevant (the header pass already read them).
                return Ok(());
            } else {
                map.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

/// Seed that streams the `messages` array: each element is parsed into a
/// `Value` on its own (one entry alive at a time) and offered to the
/// consumer.
struct MessagesSeed<'a, 'c> {
    ctx: &'a ThreadContext,
    occ: &'a mut HashMap<u64, u32>,
    state: &'a mut crate::bounded::ConsumerState<'c>,
}

impl<'de, 'a, 'c> serde::de::DeserializeSeed<'de> for MessagesSeed<'a, 'c> {
    type Value = ();

    fn deserialize<D: serde::de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de, 'a, 'c> serde::de::Visitor<'de> for MessagesSeed<'a, 'c> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a messages array")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<(), A::Error> {
        while let Some(entry) = seq.next_element::<Value>()? {
            let outcome = messenger_entry(&entry, self.ctx, self.occ);
            if !self.state.offer(outcome) {
                return Err(crate::bounded::ConsumerState::stop());
            }
        }
        Ok(())
    }
}

/// The export top level for the whole-file parser; every message object
/// is kept verbatim so nothing in it is lost. Participants stay raw
/// values (their entries are only ever read for `name`).
#[derive(Debug, Deserialize)]
struct MessengerExport {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    thread_path: Option<String>,
    #[serde(default)]
    participants: Vec<Value>,
    #[serde(default)]
    messages: Vec<Value>,
}

/// Parse one Messenger thread file into messages.
///
/// `folder_stem` (the parent directory's name) is the fallback for
/// `chat_id` and for the thread id when the header's `thread_path`
/// carries no numeric suffix. Entries that are not objects, or that
/// have neither `sender_name` nor `timestamp_ms`, count as failed;
/// entries with neither `content` nor any media field are skipped.
fn parse_messenger(bytes: &[u8], folder_stem: &str) -> Result<Collected> {
    let export: MessengerExport = serde_json::from_slice(bytes)?;
    // The same header view the streaming pass builds, so both paths
    // share one context construction and cannot drift apart.
    let header = MessengerHeader {
        title: export.title,
        thread_path: export.thread_path,
        participants: export
            .participants
            .iter()
            .map(|p| ParticipantName {
                name: p.get("name").and_then(Value::as_str).map(str::to_string),
            })
            .collect(),
        messages: Vec::new(),
    };
    let ctx = ThreadContext::from_header(&header, folder_stem);
    let mut occurrence: HashMap<u64, u32> = HashMap::new();
    let mut messages = Vec::new();
    let mut failed: u64 = 0;
    for entry in &export.messages {
        match messenger_entry(entry, &ctx, &mut occurrence) {
            EntryOutcome::Message(msg) => messages.push(msg),
            EntryOutcome::Failed { .. } => failed += 1,
            EntryOutcome::Skip => {}
        }
    }
    Ok(Collected { messages, failed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_bytes() -> Vec<u8> {
        br#"{"participants": [{"name": "Ana Peric"}], "messages": [{"sender_name": "Ana Peric", "timestamp_ms": 1762981200000, "content": "Zdravo!"}], "title": "Ana Peric", "thread_path": "inbox/anaperic_1234567890"}"#.to_vec()
    }

    #[test]
    fn detects_messenger_thread_json() {
        assert!(is_messenger_export(&thread_bytes()));
    }

    #[test]
    fn detects_thread_with_many_messages_and_mixed_key_order() {
        // A real DYI file has thousands of messages and keys around the
        // two probes care about; only the first element materializes.
        let reversed = br#"{"title": "Ana Peric", "messages": [{"timestamp_ms": 2, "sender_name": "Ana", "content": "druga"}, {"sender_name": "Ana", "timestamp_ms": 3, "content": "treca"}], "participants": [{"name": "Ana"}]}"#;
        assert!(is_messenger_export(reversed));
        let rest: Vec<String> = (2..=51)
            .map(|i| format!("{{\"sender_name\": \"Ana\", \"timestamp_ms\": {i}}}"))
            .collect();
        let many = format!(
            "{{\"participants\": [], \"messages\": [{{\"sender_name\": \"Ana\", \"timestamp_ms\": 1}}, {}]}}",
            rest.join(", ")
        );
        assert!(is_messenger_export(many.as_bytes()));
    }

    #[test]
    fn telegram_shape_is_not_messenger() {
        // Telegram: messages array but no participants array / sender_name.
        assert!(!is_messenger_export(
            br#"{"name": "x", "id": 42, "messages": [{"id": 1, "type": "message", "date": "2026-11-01T10:00:00", "text": "hi"}]}"#
        ));
        assert!(!is_messenger_export(br#"{"messages": []}"#));
    }

    #[test]
    fn broken_json_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("message_1.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert!(MessengerSource::new(&path).collect().is_err());
    }
}

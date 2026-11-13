//! Facebook Messenger (DYI JSON) export source: parses the
//! `messages/inbox/<thread>/message_*.json` files written by
//! "Download Your Information" with format set to JSON.
//!
//! Every original message object is kept verbatim in `raw_json` (text
//! and media references alike); the conversation key comes from the
//! thread folder's numeric suffix; messages carry synthetic external
//! ids (Facebook exports none); and a thread's `message_1.json` …
//! `message_N.json` parts merge in numeric order.
//!
//! The streaming implementation lands with the parser task; this
//! module currently ships format detection only, so a DYI export
//! never misroutes to the Telegram importer (whose probe is merely
//! "has a `messages` array" — [`crate::chat_export`] pins the
//! detection order).

use std::io::BufReader;
use std::path::PathBuf;

use serde::Deserializer as _;
use serde_json::Value;

use crate::bounded::{Flow, StreamItem, StreamSource};
use crate::ingest::{Collected, Source};
use crate::{Error, Result};

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

/// Reads Messenger DYI export files and yields their messages.
/// (Streaming implementation lands with the parser task; detection
/// ships first so the format never misroutes to Telegram.)
#[derive(Debug, Clone)]
pub struct MessengerSource {
    #[allow(dead_code)] // read once the parser task lands its streaming pass
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
        Err(Error::ChatImport("messenger import not wired yet".into()))
    }
}

impl StreamSource for MessengerSource {
    fn name(&self) -> &str {
        MESSENGER_SOURCE
    }

    fn stream(&self, _skip: u64, _emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()> {
        Err(Error::ChatImport("messenger import not wired yet".into()))
    }
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
}

//! Signal chat export source: parses the per-conversation JSON exports
//! written from Signal Desktop's message database — e.g.
//! `sigtop export-messages -f json`, which writes one `<Name>.json` file
//! per conversation, each a top-level JSON array of Signal Desktop's raw
//! message objects, verbatim.
//!
//! Mapping into the archive schema:
//! - `source` is `"signal"`; `external_id` is `<conversationId>/<id>` —
//!   the conversation uuid namespaces message ids the same way Telegram's
//!   export id does, so ids stay unique across chats and a re-export
//!   after a file rename dedups instead of duplicating. Messages without
//!   an `id` (only the oldest exports) fall back to a content hash, as in
//!   [`crate::whatsapp`].
//! - `chat_id` (display grouping) is the export file's name stem — sigtop
//!   names files after the conversation — then `conversationId`.
//! - `author`: outgoing messages are archived as `"You"` (the raw JSON
//!   carries no local-account name); incoming messages take `sourceName`
//!   when the export has it, else the `source` address without its
//!   `.device` suffix, else `sourceServiceId`/`sourceUuid`.
//! - `sent_at`: Signal's `sent_at`/`timestamp`/`received_at`/
//!   `serverTimestamp` are absolute Unix epoch **milliseconds**, so the
//!   stored UTC time is timezone-exact by construction (no exporter-local
//!   ambiguity like WhatsApp's). `sent_at` wins, then `timestamp`,
//!   `received_at_ms`, `received_at`, `serverTimestamp`; empty when none.
//! - Entries whose `type` is neither `incoming` nor `outgoing`
//!   (`call-history`, `keychange`, `profile-change`, `group-v2-change`, …)
//!   are service events without a body: skipped, like Telegram's.
//! - Media is not extracted in v0: attachments, quotes, reactions, and
//!   group info stay recorded verbatim in `raw_json`.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use serde_json::Value;

use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::util::fnv1a_fields;
use crate::Result;

/// Source name and dedup namespace for Signal imports.
pub const SIGNAL_SOURCE: &str = "signal";

/// Author recorded for outgoing messages: the export holds no
/// local-account name, so the archive marks self-sent messages the way
/// sigtop's text export does.
pub const OUTGOING_AUTHOR: &str = "You";

/// Message-entry types that carry a body; everything else (`group`,
/// `keychange`, `profile-change`, `call-history`, …) is a service event.
const MESSAGE_TYPES: [&str; 2] = ["incoming", "outgoing"];

/// Timestamp fields in preference order — all Unix epoch milliseconds.
const TIMESTAMP_FIELDS: [&str; 5] = [
    "sent_at",
    "timestamp",
    "received_at_ms",
    "received_at",
    "serverTimestamp",
];

/// Reads one Signal conversation `.json` export and yields its messages.
#[derive(Debug, Clone)]
pub struct SignalSource {
    path: PathBuf,
}

impl SignalSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Source for SignalSource {
    fn name(&self) -> &str {
        SIGNAL_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        let bytes = fs::read(&self.path)?;
        let chat_id_fallback = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        parse_signal(&bytes, &chat_id_fallback)
    }
}

/// Parse a Signal conversation JSON export into messages.
///
/// `chat_id_fallback` groups the messages when the export carries no
/// usable identity — it is the export file's name stem (sigtop names
/// files after the conversation). Entries that are not objects or have no
/// `type` at all count as failed; service-typed entries are skipped
/// silently. See the [module docs](self) for the field mapping.
pub fn parse_signal(bytes: &[u8], chat_id_fallback: &str) -> Result<Collected> {
    let entries: Vec<Value> = serde_json::from_slice(bytes)?;

    // The conversation uuid is the stable identity: it keys the dedup
    // namespace so a renamed export file re-dedups, while the file name
    // (the only display-name carrier) remains the chat grouping.
    let conversation_id = entries
        .iter()
        .filter_map(Value::as_object)
        .find_map(|o| string_field(o, "conversationId"))
        .unwrap_or_default();
    let chat_id = if !chat_id_fallback.is_empty() {
        chat_id_fallback.to_string()
    } else if !conversation_id.is_empty() {
        conversation_id.clone()
    } else {
        SIGNAL_SOURCE.to_string()
    };
    let dedup_chat = if conversation_id.is_empty() {
        chat_id.clone()
    } else {
        conversation_id
    };

    let mut messages = Vec::new();
    let mut failed: u64 = 0;
    let mut occurrence: HashMap<u64, u32> = HashMap::new();

    for entry in &entries {
        let Some(obj) = entry.as_object() else {
            failed += 1;
            continue;
        };
        match obj.get("type").and_then(Value::as_str) {
            Some(t) if MESSAGE_TYPES.contains(&t) => {}
            Some(_) => continue, // service event: no body to archive
            None => {
                failed += 1; // real exports always carry a type
                continue;
            }
        }

        let author = signal_author(obj);
        let sent_at = signal_sent_at(obj);
        let body = obj
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let external_id = match string_field(obj, "id") {
            Some(id) => format!("{}/{}", dedup_chat, id),
            None => {
                // Content-hash dedup id (no stable id in the oldest
                // exports); the occurrence counter keeps identical
                // back-to-back messages distinct while re-imports stay
                // idempotent.
                let base = fnv1a_fields(&[&dedup_chat, &author, &sent_at, &body]);
                let seen = occurrence.entry(base).or_insert(0);
                let external_id = format!("generated-{:016x}-{}", base, seen);
                *seen += 1;
                external_id
            }
        };

        let mut msg = NewMessage::new(SIGNAL_SOURCE, external_id, author, sent_at, body);
        msg.channel = "chat".to_string();
        msg.chat_id = chat_id.clone();
        // The whole original object: attachments, quote, reactions,
        // group membership, edit history — all stay recorded verbatim.
        msg.raw_json = entry.to_string();
        messages.push(msg);
    }
    Ok(Collected { messages, failed })
}

/// True when the bytes are a Signal conversation export: a top-level JSON
/// array holding at least one raw message object — `type` incoming or
/// outgoing plus an epoch-millisecond timestamp. The array shape cannot
/// collide with Telegram's object exports or WhatsApp's text.
pub fn is_signal_export(bytes: &[u8]) -> bool {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Array(items)) => !items.is_empty() && items.iter().any(is_signal_message),
        _ => false,
    }
}

fn is_signal_message(v: &Value) -> bool {
    let Some(obj) = v.as_object() else {
        return false;
    };
    matches!(
        obj.get("type").and_then(Value::as_str),
        Some(t) if MESSAGE_TYPES.contains(&t)
    ) && TIMESTAMP_FIELDS
        .iter()
        .any(|key| obj.get(*key).and_then(epoch_ms).is_some())
}

/// Author of one entry: outgoing messages are the local account (`"You"`),
/// incoming ones carry the sender as `sourceName` (when the export has
/// it), as the `source` address (`+385912345678.2` — device suffix
/// stripped), or as a service id.
fn signal_author(obj: &serde_json::Map<String, Value>) -> String {
    if obj.get("type").and_then(Value::as_str) == Some("outgoing") {
        return OUTGOING_AUTHOR.to_string();
    }
    if let Some(name) = string_field(obj, "sourceName") {
        return name;
    }
    if let Some(source) = string_field(obj, "source") {
        return strip_device_suffix(&source);
    }
    obj.get("sourceServiceId")
        .or_else(|| obj.get("sourceUuid"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_default()
}

/// `"+385912345678.2"` → `"+385912345678"`: Signal suffixes the sending
/// device after a dot. Only a numeric suffix is stripped, so a non-phone
/// source string can never be mangled.
fn strip_device_suffix(source: &str) -> String {
    match source.rsplit_once('.') {
        Some((left, digits))
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) =>
        {
            left.to_string()
        }
        _ => source.to_string(),
    }
}

/// Timestamp: every Signal timestamp field is absolute Unix epoch
/// milliseconds, so the UTC form is exact. The first present field in
/// preference order wins; empty when none parses (out of chrono's range
/// counts as absent, never an error).
fn signal_sent_at(obj: &serde_json::Map<String, Value>) -> String {
    const RFC3339: &str = "%Y-%m-%dT%H:%M:%SZ";
    for key in TIMESTAMP_FIELDS {
        if let Some(ms) = obj.get(key).and_then(epoch_ms) {
            if let Some(t) = Utc.timestamp_millis_opt(ms).single() {
                return t.format(RFC3339).to_string();
            }
        }
    }
    String::new()
}

/// Epoch milliseconds as a number, tolerating the string spelling some
/// export tooling writes.
fn epoch_ms(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn string_field(obj: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::{json, Value};

    fn message(obj: Value) -> Vec<u8> {
        format!("[{}]", obj).into_bytes()
    }

    #[test]
    fn epoch_milliseconds_map_to_exact_utc() {
        // 1698832800000 ms is 2023-11-01T10:00:00Z exactly — no exporter
        // timezone can shift it.
        let bytes = message(serde_json::json!({
            "id": "m1", "type": "incoming", "conversationId": "c1",
            "source": "+385912345678.2",
            "sent_at": 1698832800000u64, "timestamp": 1698832800000u64,
            "body": "hello"
        }));
        let collected = parse_signal(&bytes, "Marko").unwrap();
        assert_eq!(collected.messages.len(), 1);
        assert_eq!(collected.messages[0].sent_at, "2023-11-01T10:00:00Z");
    }

    #[test]
    fn timestamp_field_precedence() {
        let base = json!({
            "id": "m1", "type": "outgoing", "conversationId": "c1"
        });
        for (fields, expected) in [
            // sent_at wins over timestamp and received_at.
            (
                json!({"sent_at": 1000u64, "timestamp": 2000u64, "received_at": 3000u64}),
                1,
            ),
            // timestamp wins over received_at when sent_at is absent.
            (json!({"timestamp": 2000u64, "received_at": 3000u64}), 2),
            // received_at_ms (newer name) beats received_at.
            (
                json!({"received_at_ms": 3000u64, "received_at": 4000u64}),
                3,
            ),
            // received_at alone.
            (json!({"received_at": 4000u64}), 4),
            // serverTimestamp is the last resort.
            (json!({"serverTimestamp": 5000u64}), 5),
        ] {
            let mut obj = base.clone();
            if let Value::Object(extra) = fields {
                for (k, v) in extra {
                    obj[k] = v;
                }
            }
            let collected = parse_signal(&message(obj), "c").unwrap();
            let secs = collected.messages[0]
                .sent_at
                .trim_end_matches('Z')
                .rsplit_once('T')
                .unwrap()
                .1
                .to_string();
            let expect = format!("00:00:0{expected}");
            assert_eq!(secs, expect);
        }
    }

    #[test]
    fn no_timestamp_leaves_sent_at_empty() {
        let bytes = message(serde_json::json!({
            "id": "m1", "type": "incoming", "conversationId": "c1",
            "source": "+385912345678", "body": "x"
        }));
        let collected = parse_signal(&bytes, "c").unwrap();
        assert_eq!(collected.messages[0].sent_at, "");
    }

    #[test]
    fn huge_timestamp_is_absent_not_a_crash() {
        let bytes = message(serde_json::json!({
            "id": "m1", "type": "incoming", "conversationId": "c1",
            "source": "+385912345678", "sent_at": i64::MAX, "body": "x"
        }));
        let collected = parse_signal(&bytes, "c").unwrap();
        assert_eq!(collected.messages[0].sent_at, "");
    }

    #[test]
    fn author_resolution() {
        let cases = [
            // Outgoing has no source in Signal's raw JSON.
            (
                json!({"id": "1", "type": "outgoing", "conversationId": "c", "sent_at": 0, "body": ""}),
                "You",
            ),
            // source with device suffix, sourceName absent.
            (
                json!({"id": "2", "type": "incoming", "conversationId": "c", "source": "+385912345678.2", "sent_at": 0, "body": ""}),
                "+385912345678",
            ),
            // sourceName (present in some export tooling) wins.
            (
                json!({"id": "3", "type": "incoming", "conversationId": "c", "source": "+385912345678", "sourceName": "Marko", "sent_at": 0, "body": ""}),
                "Marko",
            ),
            // Older: sourceUuid.
            (
                json!({"id": "4", "type": "incoming", "conversationId": "c", "sourceUuid": "uuid-7", "sent_at": 0, "body": ""}),
                "uuid-7",
            ),
            // Newer: sourceServiceId.
            (
                json!({"id": "5", "type": "incoming", "conversationId": "c", "sourceServiceId": "svc-9", "sent_at": 0, "body": ""}),
                "svc-9",
            ),
            // Nothing at all: empty author.
            (
                json!({"id": "6", "type": "incoming", "conversationId": "c", "sent_at": 0, "body": ""}),
                "",
            ),
        ];
        for (obj, expected) in cases {
            let collected = parse_signal(&message(obj), "c").unwrap();
            assert_eq!(collected.messages[0].author, expected);
        }
    }

    #[test]
    fn device_suffix_only_strips_digits() {
        assert_eq!(strip_device_suffix("+385912345678.2"), "+385912345678");
        assert_eq!(strip_device_suffix("+385912345678"), "+385912345678");
        // Not a device suffix: uuid-ish sources stay intact.
        assert_eq!(strip_device_suffix("a.b.c"), "a.b.c");
    }

    #[test]
    fn service_entries_are_skipped_and_typeless_fail() {
        let bytes = br#"[
            {"id": "1", "type": "call-history", "conversationId": "c", "sent_at": 0},
            {"id": "2", "type": "keychange", "conversationId": "c"},
            {"id": "3", "type": "profile-change", "conversationId": "c"},
            {"id": "4", "type": "group", "conversationId": "c"},
            {"id": "5", "type": "incoming", "conversationId": "c", "sent_at": 1000, "body": "real"},
            "not an object",
            {"conversationId": "c", "body": "typeless"}
        ]"#;
        let collected = parse_signal(bytes, "chat").unwrap();
        assert_eq!(collected.messages.len(), 1);
        assert_eq!(collected.failed, 2);
        assert_eq!(collected.messages[0].body, "real");
    }

    #[test]
    fn chat_id_falls_back_to_conversation_id_then_source_name() {
        let bytes = br#"[{"id": "1", "type": "incoming", "conversationId": "conv-9", "sent_at": 0, "body": "x"}]"#;
        assert_eq!(
            parse_signal(bytes, "Marko").unwrap().messages[0].chat_id,
            "Marko"
        );
        assert_eq!(
            parse_signal(bytes, "").unwrap().messages[0].chat_id,
            "conv-9"
        );
        let no_conv = br#"[{"id": "1", "type": "incoming", "sent_at": 0, "body": "x"}]"#;
        assert_eq!(
            parse_signal(no_conv, "").unwrap().messages[0].chat_id,
            "signal"
        );
    }

    #[test]
    fn external_id_is_namespaced_by_conversation_id() {
        let bytes = br#"[{"id": "m-1", "type": "incoming", "conversationId": "conv-9", "sent_at": 0, "body": "x"}]"#;
        let collected = parse_signal(bytes, "Marko").unwrap();
        assert_eq!(collected.messages[0].external_id, "conv-9/m-1");
    }

    #[test]
    fn id_less_entries_get_stable_content_hash_ids() {
        let bytes = br#"[
            {"type": "incoming", "conversationId": "c", "source": "+385111", "sent_at": 1000, "body": "old export"},
            {"type": "incoming", "conversationId": "c", "source": "+385111", "sent_at": 1000, "body": "old export"}
        ]"#;
        let collected = parse_signal(bytes, "c").unwrap();
        assert_eq!(collected.messages.len(), 2);
        assert_ne!(
            collected.messages[0].external_id, collected.messages[1].external_id,
            "identical back-to-back messages need distinct ids"
        );
        let again = parse_signal(bytes, "c").unwrap();
        assert_eq!(collected.messages, again.messages, "must be deterministic");
    }

    #[test]
    fn numeric_or_numeric_string_timestamps_parse() {
        let as_string = message(json!({
            "id": "1", "type": "incoming", "conversationId": "c",
            "sent_at": "1698832800000", "body": "x"
        }));
        assert_eq!(
            parse_signal(&as_string, "c").unwrap().messages[0].sent_at,
            "2023-11-01T10:00:00Z"
        );
        let as_number = message(json!({
            "id": "1", "type": "incoming", "conversationId": "c",
            "sent_at": 1698832800000u64, "body": "x"
        }));
        assert_eq!(
            parse_signal(&as_number, "c").unwrap().messages[0].sent_at,
            "2023-11-01T10:00:00Z"
        );
    }

    #[test]
    fn non_array_is_an_error() {
        assert!(parse_signal(b"{}", "c").is_err());
        assert!(parse_signal(b"[", "c").is_err());
        assert!(parse_signal(b"", "c").is_err());
    }

    #[test]
    fn detection_requires_signal_array_shape() {
        let signal = br#"[{"id": "1", "type": "incoming", "conversationId": "c", "sent_at": 1000, "body": "x"}]"#;
        assert!(is_signal_export(signal));

        // Empty arrays and non-arrays never match.
        assert!(!is_signal_export(b"[]"));
        assert!(!is_signal_export(b"{}"));
        assert!(!is_signal_export(b""));
        assert!(!is_signal_export(b"[1, 2, 3]"));
        assert!(!is_signal_export(br#"[{"foo": 1}]"#));
        // Detection tolerates service entries leading the array.
        let lead = br#"[
            {"type": "call-history", "conversationId": "c"},
            {"id": "1", "type": "outgoing", "sent_at": 1, "body": "x"}
        ]"#;
        assert!(is_signal_export(lead));
        // A type without any timestamp is not enough.
        assert!(!is_signal_export(br#"[{"type": "incoming", "body": "x"}]"#));
    }
}

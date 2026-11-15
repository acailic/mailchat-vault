//! JSON Lines import source: reads back the `.jsonl` files the export
//! writes ([`crate::export`]'s `ExportFormat::Json`) — one complete
//! message object per line, the exact serde shape the app stores.
//!
//! This closes the export/import round-trip at the level of
//! machine-readable single messages: an exported subset moves between
//! vaults without a whole-vault backup container (which stays the right
//! tool for full moves — it carries media bytes and settings too).
//!
//! Mapping into the archive schema:
//! - Each row keeps its **own** `source` and `external_id` verbatim —
//!   the legacy identity [`crate::storage`] dedups on — so a file
//!   exported from one vault lands identically in another, re-imports
//!   never duplicate, and mixed-source files keep their rows under
//!   their original namespaces. The run-level name
//!   ([`JSONL_SOURCE`]) only labels the import run's summary, mirroring
//!   how `MBOX_SOURCE` names mail runs while rows dedup on their own
//!   ids.
//! - **Portable source identity.** Rows exported by the current search
//!   streams also carry a `source_key`: the source instance's
//!   label-independent identity — `instance:<kind>:<connector_key>` for
//!   connector-owned instances, the legacy namespace for legacy ones.
//!   When present, that key (never the human label) is the dedup
//!   namespace, and the row's display `source` names the created
//!   instance; two instances sharing a display label therefore
//!   round-trip as two distinct sources instead of collapsing onto one
//!   `(label, external_id)` identity. Rows without a key — every
//!   legacy export — import exactly as before. Destination instances
//!   are legacy-style with the key as their namespace, so re-exporting
//!   from the destination emits the same key and the cycle is stable;
//!   renaming an instance's display label changes only the label, never
//!   the key.
//! - The exported row's `id` (the autoincrement of the exporting vault)
//!   is ignored: a fresh vault assigns its own.
//! - Every other field (`channel`, `chat_id`, `author`, `sent_at`,
//!   `body`, `raw_json`) is copied verbatim; an absent `raw_json`
//!   defaults to `"{}"`, an absent `channel` to `"email"`, as in
//!   [`crate::message::NewMessage::new`].
//! - A malformed line — invalid JSON, a non-object line, an object
//!   without the `source`/`external_id` identity pair, or bytes that
//!   are not valid UTF-8 — becomes a durable per-item failure record
//!   (`StreamItem::Failed`, landing in `import_issues` like every
//!   other format's parse failures) instead of aborting the run or
//!   rewriting field bytes; blank lines are not items at all.
//! - Media references in a row's `raw_json` stay references: media
//!   bytes are not transferred (no media pass runs for this format,
//!   like Signal and Discord).
//!
//! Detection ([`is_jsonl_export_reader`]): the first non-empty line is
//! valid UTF-8 and parses as a JSON object carrying a string `source`
//! plus a string `external_id`. The chat-export probes (Discord,
//! Messenger, Telegram, Signal, WhatsApp) all claim whole-file shapes
//! and run first, so a one-object-per-line file can only reach this
//! probe.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use serde_json::Value;

use crate::bounded::{emit_entry, EntryOutcome, Flow, StreamItem, StreamSource};
use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::Result;

/// Run-level source name for JSON Lines imports. Labels the import run
/// (summary, checkpoint, issue records); the identity of each row is its
/// own `source` + `external_id`, preserved verbatim — see the
/// [module docs](self).
pub const JSONL_SOURCE: &str = "jsonl";

/// Reads one `.jsonl` export (one message object per line, as written by
/// [`crate::export`]) and yields its rows.
#[derive(Debug, Clone)]
pub struct JsonlSource {
    path: PathBuf,
}

impl JsonlSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

/// The bounded streaming path: one line at a time, one decoded row
/// alive at any moment — a `.jsonl` export of any size costs a fixed
/// line buffer, never the whole file.
impl StreamSource for JsonlSource {
    fn name(&self) -> &str {
        JSONL_SOURCE
    }

    fn stream(&self, skip: u64, emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()> {
        stream_jsonl(BufReader::new(fs::File::open(&self.path)?), skip, emit)
    }
}

impl Source for JsonlSource {
    fn name(&self) -> &str {
        JSONL_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        parse_jsonl(&fs::read(&self.path)?)
    }
}

/// Parse JSON Lines bytes into messages (the whole-file test
/// convenience; production streams through [`JsonlSource`]).
pub fn parse_jsonl(bytes: &[u8]) -> Result<Collected> {
    let mut messages = Vec::new();
    let mut failed: u64 = 0;
    stream_jsonl(std::io::Cursor::new(bytes), 0, &mut |item| {
        match item {
            StreamItem::Message(msg) => messages.push(msg),
            StreamItem::Failed { .. } => failed += 1,
        }
        Ok(Flow::Continue)
    })?;
    Ok(Collected { messages, failed })
}

/// The shared engine behind the streaming source and the whole-file
/// parser: read one line, decode it, offer it to the consumer (through
/// the shared skip counter, so restart recovery fast-forwards over
/// already-committed items). Lines are decoded strictly — a line that
/// is not valid UTF-8 is a per-item failure (its bytes are never
/// rewritten into replacement characters), never a run abort.
pub fn stream_jsonl<R: BufRead>(
    reader: R,
    skip: u64,
    emit: &mut dyn FnMut(StreamItem) -> Result<Flow>,
) -> Result<()> {
    let mut reader = reader;
    let mut skip = skip;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let read = reader.read_until(b'\n', &mut buf)?;
        if read == 0 {
            return Ok(());
        }
        let outcome = match std::str::from_utf8(&buf) {
            Ok(line) => jsonl_line_outcome(line),
            Err(e) => EntryOutcome::Failed {
                // Nothing trustworthy to extract from an undecodable
                // line; the position in the file is the durable locator.
                external_ref: String::new(),
                reason: format!("line is not valid UTF-8: {e}"),
            },
        };
        if let Flow::Stop = emit_entry(outcome, &mut skip, emit)? {
            return Ok(());
        }
    }
}

/// Convert one line: a well-formed row yields a message (its exported
/// `id` ignored), a malformed line a durable failure record, a blank
/// line nothing at all.
fn jsonl_line_outcome(line: &str) -> EntryOutcome {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return EntryOutcome::Skip;
    }
    let value = match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => value,
        Err(e) => {
            return EntryOutcome::Failed {
                // Nothing trustworthy to extract from an unparseable
                // line; the position in the file is the durable locator.
                external_ref: String::new(),
                reason: format!("line is not valid JSON: {e}"),
            };
        }
    };
    let Some(obj) = value.as_object() else {
        return EntryOutcome::Failed {
            external_ref: String::new(),
            reason: "line is not a JSON object".to_string(),
        };
    };
    // The identity pair is the dedup contract: a row without it cannot
    // round-trip and is recorded as a failure, exactly like an id-less
    // entry in the other chat formats.
    let (source, external_id) = match (
        string_field(obj, "source"),
        string_field(obj, "external_id"),
    ) {
        (Some(source), Some(external_id)) => (source, external_id),
        _ => {
            let external_ref = string_field(obj, "external_id").or_else(|| string_field(obj, "id"));
            return EntryOutcome::Failed {
                external_ref: external_ref.unwrap_or_default(),
                reason: "row is missing its source or external_id identity".to_string(),
            };
        }
    };
    // A portable source-identity key (see the module docs) re-namespaces
    // the row: the machine key becomes the dedup namespace — so two
    // same-labelled sources stay distinct — and the row's display
    // `source` names the destination instance instead. Rows without a
    // key (every legacy export) keep the documented `(source,
    // external_id)` contract verbatim.
    let (namespace, label) = match string_field(obj, "source_key") {
        Some(key) => (key, source),
        None => (source, String::new()),
    };
    let field = |name: &str| string_field(obj, name).unwrap_or_default();
    let mut msg = NewMessage::new(
        namespace,
        external_id,
        field("author"),
        field("sent_at"),
        field("body"),
    );
    msg.source_label = label;
    // An absent channel defaults to `"email"` (NewMessage's own
    // default); an absent raw_json to the canonical empty extras `"{}"`.
    msg.channel = field("channel");
    if msg.channel.is_empty() {
        msg.channel = "email".to_string();
    }
    msg.chat_id = field("chat_id");
    msg.raw_json = field("raw_json");
    if msg.raw_json.is_empty() {
        msg.raw_json = "{}".to_string();
    }
    EntryOutcome::Message(msg)
}

/// True when the bytes are a JSON Lines export: the first non-empty
/// line is valid UTF-8 and parses as a JSON object carrying a string
/// `source` plus a string `external_id` — the export's exact row shape.
/// A first line with invalid bytes is not this format (later lines are
/// never probed, so a bad row further in still detects). Bounded: only
/// that first line is read.
pub fn is_jsonl_export(bytes: &[u8]) -> bool {
    is_jsonl_export_reader(std::io::Cursor::new(bytes))
}

/// [`is_jsonl_export`] streamed from a reader (the real picks' shape).
pub fn is_jsonl_export_reader<R: std::io::Read>(reader: R) -> bool {
    let mut reader = BufReader::new(reader);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => return false,
            Ok(_) => {}
            Err(_) => return false,
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            return false;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(trimmed) else {
            return false;
        };
        return string_field(&obj, "source").is_some()
            && string_field(&obj, "external_id").is_some();
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

    fn row_json(id: i64, source: &str, external_id: &str, body: &str) -> String {
        format!(
            r#"{{"id":{id},"source":"{source}","external_id":"{external_id}","channel":"chat","chat_id":"ekipa","author":"Marko","sent_at":"2026-11-01T10:00:00Z","body":"{body}","raw_json":"{{}}"}}"#
        )
    }

    #[test]
    fn rows_preserve_their_own_identity_and_fields() {
        let bytes = format!(
            "{}\n{}\n",
            row_json(1, "telegram", "42/7", "zdravo"),
            row_json(2, "mbox", "a@b.c", "pozdrav")
        );
        let collected = parse_jsonl(bytes.as_bytes()).unwrap();
        assert_eq!(collected.messages.len(), 2);
        assert_eq!(collected.failed, 0);
        assert_eq!(collected.messages[0].source, "telegram");
        assert_eq!(collected.messages[0].external_id, "42/7");
        assert_eq!(collected.messages[0].channel, "chat");
        assert_eq!(collected.messages[0].chat_id, "ekipa");
        assert_eq!(collected.messages[0].author, "Marko");
        assert_eq!(collected.messages[0].sent_at, "2026-11-01T10:00:00Z");
        assert_eq!(collected.messages[0].body, "zdravo");
        assert_eq!(collected.messages[1].source, "mbox");
        assert_eq!(collected.messages[1].external_id, "a@b.c");
    }

    #[test]
    fn exported_id_is_ignored() {
        // Same row under two different vault ids: one message identity.
        let bytes = format!(
            "{}\n{}\n",
            row_json(1, "telegram", "42/7", "x"),
            row_json(999, "telegram", "42/7", "x")
        );
        let collected = parse_jsonl(bytes.as_bytes()).unwrap();
        assert_eq!(collected.messages.len(), 2);
        assert_eq!(
            collected.messages[0].external_id,
            collected.messages[1].external_id
        );
    }

    #[test]
    fn defaults_for_absent_optional_fields() {
        let bytes = br#"{"source":"mbox","external_id":"a@b.c"}"#;
        let collected = parse_jsonl(bytes).unwrap();
        let msg = &collected.messages[0];
        assert_eq!(msg.channel, "email");
        assert_eq!(msg.raw_json, "{}");
        assert_eq!(msg.body, "");
    }

    #[test]
    fn blank_lines_are_not_items() {
        let bytes = format!("\n\n{}\n   \n", row_json(1, "s", "e", "b"));
        let collected = parse_jsonl(bytes.as_bytes()).unwrap();
        assert_eq!(collected.messages.len(), 1);
        assert_eq!(collected.failed, 0);
    }

    #[test]
    fn malformed_lines_fail_without_aborting() {
        let bytes = format!(
            "{{not json}}\n{}\n\"just a string\"\n{{\"source\":\"s\"}}\n",
            row_json(1, "s", "e", "b")
        );
        let collected = parse_jsonl(bytes.as_bytes()).unwrap();
        assert_eq!(collected.messages.len(), 1);
        assert_eq!(collected.failed, 3);
    }

    #[test]
    fn invalid_utf8_in_body_fails_the_row_instead_of_rewriting_it() {
        // The reported bug's reproduction: a lone 0xff byte inside the
        // body's JSON string used to import as "a\u{fffd}b" and count
        // as success.
        let bytes = b"{\"source\":\"demo\",\"external_id\":\"badutf8\",\"body\":\"a\xffb\"}\n";
        let collected = parse_jsonl(bytes).unwrap();
        assert_eq!(collected.messages.len(), 0);
        assert_eq!(collected.failed, 1);
        assert!(!collected
            .messages
            .iter()
            .any(|m| m.body.contains('\u{fffd}')));
    }

    #[test]
    fn invalid_utf8_in_identity_fields_fails_the_row() {
        // Identity bytes must never be rewritten before dedup — the
        // row is recorded as a failure instead.
        let bad_source = b"{\"source\":\"te\xffgram\",\"external_id\":\"42/7\",\"body\":\"x\"}\n";
        let bad_external_id =
            b"{\"source\":\"telegram\",\"external_id\":\"42\xff7\",\"body\":\"x\"}\n";
        for bytes in [&bad_source[..], &bad_external_id[..]] {
            let collected = parse_jsonl(bytes).unwrap();
            assert_eq!(collected.messages.len(), 0, "{bytes:?}");
            assert_eq!(collected.failed, 1);
        }
    }

    #[test]
    fn multibyte_and_escaped_control_rows_import_verbatim() {
        // Valid UTF-8 — multibyte text plus JSON-escaped controls —
        // imports byte-for-byte; no replacement character appears.
        let bytes = b"{\"source\":\"demo\",\"external_id\":\"uni\",\"body\":\"zdravo \xf0\x9f\x91\x8b \\u0007 \\t \\u017e \\u4e2d\"}\n";
        let collected = parse_jsonl(bytes).unwrap();
        assert_eq!(collected.messages.len(), 1);
        assert_eq!(collected.failed, 0);
        assert_eq!(
            collected.messages[0].body,
            "zdravo \u{1f44b} \u{7} \t \u{17e} \u{4e2d}"
        );
    }

    #[test]
    fn detection_rejects_invalid_utf8_on_the_first_nonblank_line() {
        // A first nonblank line with invalid bytes is not this format.
        assert!(!is_jsonl_export(
            b"{\"source\":\"demo\",\"external_id\":\"1\",\"body\":\"a\xffb\"}\n"
        ));
        // A bad row further in still detects: only the first nonblank
        // line is probed.
        let mut raw: Vec<u8> = row_json(1, "demo", "1", "x").into_bytes();
        raw.push(b'\n');
        raw.extend_from_slice(b"{\"source\":\"demo\",\"external_id\":\"2\",\"body\":\"a\xffb\"}\n");
        assert!(is_jsonl_export(&raw));
    }

    #[test]
    fn source_key_renamespaces_the_row_and_the_label_names_the_instance() {
        // Two same-labelled sources, one shared external id: the machine
        // keys keep them distinct while the display label survives.
        let bytes = concat!(
            r#"{"id":1,"source":"Shared mailbox","source_key":"instance:imap:account-a","external_id":"same-message-id","channel":"email","author":"Ana","sent_at":"2026-11-01T10:00:00Z","body":"from a"}"#,
            "\n",
            r#"{"id":2,"source":"Shared mailbox","source_key":"instance:imap:account-b","external_id":"same-message-id","channel":"email","author":"Ana","sent_at":"2026-11-01T10:00:00Z","body":"from b"}"#,
            "\n",
        );
        let collected = parse_jsonl(bytes.as_bytes()).unwrap();
        assert_eq!(collected.failed, 0);
        assert_eq!(collected.messages.len(), 2);
        assert_eq!(collected.messages[0].source, "instance:imap:account-a");
        assert_eq!(collected.messages[0].source_label, "Shared mailbox");
        assert_eq!(collected.messages[1].source, "instance:imap:account-b");
        assert_eq!(collected.messages[1].source_label, "Shared mailbox");
        // The external ids are identical; only the namespaces differ, so
        // the storage dedup sees two identities, not one.
        assert_eq!(
            collected.messages[0].external_id,
            collected.messages[1].external_id
        );
        assert_ne!(collected.messages[0].source, collected.messages[1].source);
    }

    #[test]
    fn legacy_rows_without_source_key_keep_the_display_string_identity() {
        let bytes = format!("{}\n", row_json(1, "telegram", "42/7", "zdravo"));
        let collected = parse_jsonl(bytes.as_bytes()).unwrap();
        let msg = &collected.messages[0];
        assert_eq!(msg.source, "telegram");
        assert_eq!(
            msg.source_label, "",
            "no key: the namespace is its own label"
        );
    }

    #[test]
    fn blank_source_key_falls_back_to_the_legacy_contract() {
        // An explicitly empty key is absent (string_field filters
        // empties): the display source remains the identity.
        let bytes = br#"{"id":1,"source":"mbox","source_key":"","external_id":"a@b.c","body":"x"}"#;
        let collected = parse_jsonl(bytes).unwrap();
        assert_eq!(collected.messages[0].source, "mbox");
        assert_eq!(collected.messages[0].source_label, "");
    }

    #[test]
    fn detection_needs_the_identity_pair_on_the_first_line() {
        assert!(is_jsonl_export(
            row_json(1, "telegram", "42/7", "x").as_bytes()
        ));
        // Leading blank lines are tolerated.
        assert!(is_jsonl_export(
            format!("\n {}\n", row_json(1, "s", "e", "x")).as_bytes()
        ));
        // Objects without the identity pair are not JSON Lines rows.
        assert!(!is_jsonl_export(br#"{"name":"Arhiva","messages":[]}"#));
        assert!(!is_jsonl_export(br#"{"id":1,"source":"s"}"#));
        // Non-objects, arrays, garbage: no.
        assert!(!is_jsonl_export(br#"["not","jsonl"]"#));
        assert!(!is_jsonl_export(b""));
        assert!(!is_jsonl_export(b"just text\n"));
    }
}

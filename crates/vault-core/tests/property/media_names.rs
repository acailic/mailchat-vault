//! Property: the Telegram display-name rule is single-sourced.
//!
//! The M4.2 review rounds (F2 → R3F1) showed the seam: the writer
//! (`media::telegram_refs` / `extract_telegram`) names each stored media
//! entry, while the reader (`search2::attachment_summaries`) had to
//! re-derive the same candidate names to suppress unavailable "phantom"
//! rows for references that were already stored. Two encodings of one
//! rule drift; every new export shape re-opens the seam.
//!
//! The single rule is now [`vault_core::media::telegram_entry_names`],
//! consumed by both sides. This property pins the writer half of the
//! contract: **for any Telegram message object, every name
//! `extract_telegram` writes into a `media` entry is in
//! `telegram_entry_names(obj)`** — so the read side can never miss a
//! stored entry's name, and phantom rows are impossible by
//! construction.
//!
//! The writer's name choice is budget-independent (a missing entry
//! records the exact name a stored entry would carry), so the property
//! runs the pass with a pre-exhausted budget (`Capacity::new(0, 0)`)
//! against an empty export root: every reference resolves to a
//! `missing` entry carrying the writer's chosen display name.

use std::collections::HashSet;

use proptest::prelude::*;
use serde_json::{json, Map, Value};

use vault_core::media::{extract_telegram, telegram_entry_names, MediaSummary};
use vault_core::message::NewMessage;
use vault_core::object_store::Capacity;

/// A small readable vocabulary of file names (including the empty
/// string, which is never a display name).
fn any_name() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("pic.jpg"),
        Just("report.pdf"),
        Just("clip.mp4"),
        Just("Izve{0161}taj.pdf"),
        Just(""),
    ]
    .prop_map(str::to_string)
}

/// A `/`-separated relative path over a small directory vocabulary.
fn any_path() -> impl Strategy<Value = String> {
    (
        prop_oneof![
            Just(""),
            Just("photos"),
            Just("files"),
            Just("video_files"),
            Just("a/b"),
        ],
        any_name(),
    )
        .prop_map(|(dir, name)| {
            if dir.is_empty() || name.is_empty() {
                name
            } else {
                format!("{dir}/{name}")
            }
        })
}

/// One `photo`/`file` field value: absent (`null`), the classic path
/// string, or the newer object shape carrying nested path(s) and its
/// own names — plus non-ref shapes the pass must ignore.
fn any_ref_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any_path().prop_map(Value::String),
        (any_path(), any_path(), any_name(), any_name()).prop_map(
            |(file, photo, file_name, file_name_unique)| {
                json!({
                    "file": file,
                    "photo": photo,
                    "file_name": file_name,
                    "file_name_unique": file_name_unique,
                })
            },
        ),
        any::<u64>().prop_map(Value::from),
        Just(json!([])),
    ]
}

/// An arbitrary Telegram message object over the fields the naming rule
/// reads: `photo`, `file`, top-level `file_name`/`file_name_unique`,
/// and the `media_type` hint.
fn any_message_obj() -> impl Strategy<Value = Map<String, Value>> {
    (
        any_ref_value(),
        any_ref_value(),
        any_name(),
        any_name(),
        prop_oneof![
            Just(""),
            Just("voice_message"),
            Just("video_file"),
            Just("photo")
        ],
    )
        .prop_map(|(photo, file, file_name, file_name_unique, media_type)| {
            let mut obj = Map::new();
            if !photo.is_null() {
                obj.insert("photo".into(), photo);
            }
            if !file.is_null() {
                obj.insert("file".into(), file);
            }
            if !file_name.is_empty() {
                obj.insert("file_name".into(), json!(file_name));
            }
            if !file_name_unique.is_empty() {
                obj.insert("file_name_unique".into(), json!(file_name_unique));
            }
            if !media_type.is_empty() {
                obj.insert("media_type".into(), json!(media_type));
            }
            obj
        })
}

proptest! {
    #![proptest_config(crate::prop_config(256))]

    #[test]
    fn every_written_entry_name_is_in_telegram_entry_names(obj in any_message_obj()) {
        let export = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let mut msg = NewMessage::new("telegram", "1", "A", "2026-01-01T00:00:00Z", "");
        msg.raw_json = Value::Object(obj.clone()).to_string();
        let mut messages = vec![msg];

        let mut summary = MediaSummary::default();
        extract_telegram(
            &mut messages,
            export.path(),
            data.path(),
            &mut summary,
            &mut Capacity::new(0, 0),
            &mut HashSet::new(),
        )
        .unwrap();

        let expected = telegram_entry_names(&obj);
        let raw: Value = serde_json::from_str(&messages[0].raw_json).unwrap();
        let entries = raw
            .get("media")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        prop_assert_eq!(
            entries.len() as u64,
            summary.missing,
            "every reference must record exactly one missing entry"
        );
        for entry in &entries {
            let name = entry["name"].as_str().unwrap();
            prop_assert!(
                expected.iter().any(|n| n == name),
                "writer name {name:?} missing from telegram_entry_names \
                 {expected:?} for object {obj:?}"
            );
        }
    }
}

use vault_core::ingest::{ingest, IngestSummary, Source};
use vault_core::search::build_match_query;
use vault_core::signal::{parse_signal, SignalSource, SIGNAL_SOURCE};
use vault_core::storage::Storage;

// Realistic Signal conversation export, as written from Signal Desktop's
// message database (e.g. `sigtop export-messages -f json`): a top-level
// array of raw Signal Desktop message objects. Covers unicode bodies, an
// outgoing message, an attachment-only message, a quote, reactions,
// service entries (skipped), and two broken entries (failed).
const CONVERSATION_JSON: &str = r#"[
  {
    "id": "0ea0b5fd-9f2a-4c73-8f8e-2f8ce6f22a11",
    "type": "incoming",
    "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01",
    "source": "+385912345678.2",
    "sourceServiceId": "0ea0b5fd-9f2a-4c73-8f8e-2f8ce6f22a99",
    "sent_at": 1698832800000,
    "timestamp": 1698832800000,
    "received_at_ms": 1698832801400,
    "serverTimestamp": 1698832800123,
    "body": "Zdravo ekipa! 👋 Čujem da je arhiva gotova 🎉",
    "schemaVersion": 35,
    "readStatus": 1,
    "seenStatus": 1
  },
  {
    "id": "3c7b5d3a-2f1e-4a6b-8c9d-0e1f2a3b4c5d",
    "type": "outgoing",
    "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01",
    "sent_at": 1698832890000,
    "timestamp": 1698832890000,
    "body": "Jeste! Traži u arhivi 🗄️",
    "expireTimer": 0,
    "sendStateByConversationId": {}
  },
  {
    "id": "5d9c1e4f-6a7b-4c8d-9e0f-1a2b3c4d5e6f",
    "type": "incoming",
    "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01",
    "source": "+385912345678.1",
    "sent_at": 1698832950000,
    "timestamp": 1698832950000,
    "body": "",
    "hasAttachments": 1,
    "attachments": [
      {
        "fileName": "signal-2023-11-01-110050.jpg",
        "contentType": "image/jpeg",
        "size": 284119,
        "width": 1600,
        "height": 1200
      }
    ]
  },
  {
    "id": "7f1a2b3c-4d5e-4f6a-8b0c-9d1e2f3a4b5c",
    "type": "incoming",
    "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01",
    "source": "+385987654321",
    "sent_at": 1698833010000,
    "timestamp": 1698833010000,
    "body": "odgovaram na poruku o arhivi",
    "quote": {
      "id": 1698832800000,
      "author": "+385912345678",
      "text": "Zdravo ekipa! 👋 Čujem da je arhiva gotova 🎉",
      "attachments": [],
      "isViewOnce": false,
      "referencedMessageNotFound": false
    }
  },
  {
    "id": "9a8b7c6d-5e4f-4a3b-2c1d-0e9f8a7b6c5d",
    "type": "incoming",
    "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01",
    "source": "+385912345678.3",
    "sent_at": 1698833070000,
    "timestamp": 1698833070000,
    "body": "reagiram",
    "reactions": [
      {
        "emoji": "❤️",
        "fromId": "3c7b5d3a-2f1e-4a6b-8c9d-0e1f2a3b4c5d",
        "targetTimestamp": 1698832890000,
        "timestamp": 1698833071000
      }
    ]
  },
  {
    "type": "call-history",
    "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01",
    "callId": "4f6e8a2b-1111-4c8d-8e0f-9a1b2c3d4e5f",
    "sent_at": 1698833100000,
    "acceptedAt": 1698833106000
  },
  {
    "type": "keychange",
    "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01",
    "key_changed": "+385912345678",
    "sent_at": 1698833110000
  },
  {
    "type": "profile-change",
    "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01",
    "profileChange": { "from": "Marko", "to": "Marko H." },
    "sent_at": 1698833120000
  },
  { "sent_at": 1 },
  "not an object"
]"#;

#[test]
fn parses_fields_from_conversation_export() {
    let collected = parse_signal(CONVERSATION_JSON.as_bytes(), "Marko").unwrap();
    // 5 real messages; call-history/keychange/profile-change are service
    // entries (skipped); one entry has no type and one is not an object
    // (both failed).
    assert_eq!(collected.messages.len(), 5);
    assert_eq!(collected.failed, 2);

    let first = &collected.messages[0];
    assert_eq!(first.source, SIGNAL_SOURCE);
    assert_eq!(first.channel, "chat");
    // chat_id groups by the export file's name (sigtop names the file
    // after the conversation).
    assert_eq!(first.chat_id, "Marko");
    // external_id namespaces the message id by the conversation uuid.
    assert_eq!(
        first.external_id,
        "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01/0ea0b5fd-9f2a-4c73-8f8e-2f8ce6f22a11"
    );
    // The .2 device suffix is stripped from the source address.
    assert_eq!(first.author, "+385912345678");
    // sent_at is absolute epoch milliseconds: exact UTC, no exporter
    // timezone applies.
    assert_eq!(first.sent_at, "2023-11-01T10:00:00Z");
    assert_eq!(first.body, "Zdravo ekipa! 👋 Čujem da je arhiva gotova 🎉");

    // Outgoing messages carry no source in the raw JSON: archived as the
    // local account.
    let outgoing = &collected.messages[1];
    assert_eq!(outgoing.author, "You");
    assert_eq!(outgoing.sent_at, "2023-11-01T10:01:30Z");

    // Attachment-only message: empty body, attachment info verbatim in
    // raw_json.
    let photo = &collected.messages[2];
    assert_eq!(photo.body, "");
    let raw: serde_json::Value = serde_json::from_str(&photo.raw_json).unwrap();
    let att = &raw["attachments"][0];
    assert_eq!(att["fileName"], "signal-2023-11-01-110050.jpg");
    assert_eq!(att["contentType"], "image/jpeg");
    assert_eq!(att["size"], 284119);

    // Quote survives verbatim in raw_json.
    let reply = &collected.messages[3];
    assert_eq!(reply.author, "+385987654321");
    let raw: serde_json::Value = serde_json::from_str(&reply.raw_json).unwrap();
    assert_eq!(
        raw["quote"]["text"],
        "Zdravo ekipa! 👋 Čujem da je arhiva gotova 🎉"
    );
    assert_eq!(raw["quote"]["author"], "+385912345678");

    // Reactions too.
    let reacted = &collected.messages[4];
    let raw: serde_json::Value = serde_json::from_str(&reacted.raw_json).unwrap();
    assert_eq!(raw["reactions"][0]["emoji"], "❤️");
}

#[test]
fn chat_id_falls_back_to_conversation_id() {
    let collected = parse_signal(CONVERSATION_JSON.as_bytes(), "").unwrap();
    assert_eq!(
        collected.messages[0].chat_id,
        "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01"
    );
}

#[test]
fn ingest_from_file_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Marko.json");
    std::fs::write(&path, CONVERSATION_JSON).unwrap();

    let storage = Storage::open_in_memory().unwrap();
    let source = SignalSource::new(&path);
    assert_eq!(source.name(), SIGNAL_SOURCE);

    let summary = ingest(&storage, &source).unwrap();
    assert_eq!(
        summary,
        IngestSummary {
            source: SIGNAL_SOURCE.into(),
            seen: 7,
            inserted: 5,
            skipped: 0,
            failed: 2
        }
    );
    assert_eq!(storage.stats().unwrap().total, 5);

    let again = ingest(&storage, &source).unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.skipped, 5);
    assert_eq!(storage.stats().unwrap().total, 5);
}

#[test]
fn same_message_id_in_different_conversations_is_not_a_duplicate() {
    // The dedup namespace is the conversation uuid, not the display name:
    // two conversations exporting the same message id stay distinct.
    let message = |conversation: &str| {
        format!(
            r#"[{{"id": "same-id", "type": "incoming", "conversationId": "{conversation}", "source": "+3851", "sent_at": 1000, "body": "iz chata {conversation}"}}]"#
        )
    };
    let storage = Storage::open_in_memory().unwrap();
    for conversation in ["conv-aaa", "conv-bbb"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Marko.json");
        std::fs::write(&path, message(conversation)).unwrap();
        let summary = ingest(&storage, &SignalSource::new(&path)).unwrap();
        assert_eq!(summary.inserted, 1);
    }
    assert_eq!(storage.stats().unwrap().total, 2);
}

#[test]
fn renamed_reexport_dedups_instead_of_duplicating() {
    // chat_id (display) follows the file name, but the dedup key follows
    // the stable conversation uuid inside the export: importing the same
    // conversation under a new file name skips everything.
    let storage = Storage::open_in_memory().unwrap();
    for name in ["Marko.json", "Marko H..json"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, CONVERSATION_JSON).unwrap();
        ingest(&storage, &SignalSource::new(&path)).unwrap();
    }
    let stats = storage.stats().unwrap();
    assert_eq!(stats.total, 5);
    assert_eq!(stats.by_source.len(), 1);
    assert_eq!(stats.by_source[0].count, 5);
}

#[test]
fn imported_chat_is_full_text_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Marko.json");
    std::fs::write(&path, CONVERSATION_JSON).unwrap();

    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &SignalSource::new(&path)).unwrap();

    for term in ["arhiva", "reagiram", "odgovaram"] {
        let hits = storage
            .search_filtered(&build_match_query(term), None, None, 10)
            .unwrap();
        assert!(!hits.is_empty(), "no hits for {term:?}");
    }
    // Service entries are never stored, so their vocabulary is absent.
    assert!(storage
        .search_filtered(&build_match_query("call-history"), None, None, 10)
        .unwrap()
        .is_empty());
    // Unicode text is searchable verbatim.
    let hits = storage
        .search_filtered(&build_match_query("Čujem"), None, None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
}

#[test]
fn epoch_milliseconds_are_stored_as_exact_utc() {
    // 2026-11-01T09:30:00Z in a non-UTC exporter's hands: because Signal
    // timestamps are absolute epoch milliseconds, the stored time cannot
    // drift by a timezone offset (unlike WhatsApp's local-only stamps).
    let bytes = br#"[{"id": "1", "type": "incoming", "conversationId": "c", "source": "+3851", "sent_at": 1793525400000, "body": "x"}]"#;
    let collected = parse_signal(bytes, "c").unwrap();
    assert_eq!(collected.messages[0].sent_at, "2026-11-01T09:30:00Z");
}

#[test]
fn broken_json_and_unreadable_file_report_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Marko.json");
    std::fs::write(&path, b"{not json").unwrap();
    assert!(SignalSource::new(&path).collect().is_err());
    assert!(parse_signal(b"{}", "c").is_err());

    assert!(SignalSource::new("/nonexistent/Marko.json")
        .collect()
        .is_err());
}

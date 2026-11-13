use vault_core::ingest::{ingest, IngestSummary, Source};
use vault_core::messenger::{MessengerSource, MESSENGER_SOURCE};
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

// Realistic Messenger DYI thread file: a titled two-participant thread,
// text messages, a photo message, and a reactions-only entry (no body,
// no media → skipped).
const THREAD_JSON: &str = r#"{
  "participants": [{"name": "Ana Peric"}, {"name": "Marko Horvat"}],
  "title": "Ana Peric",
  "thread_path": "inbox/anaperic_1234567890",
  "is_still_participant": true,
  "messages": [
    {"sender_name": "Ana Peric", "timestamp_ms": 1762981200000, "content": "Zdravo! Kako si?"},
    {"sender_name": "Marko Horvat", "timestamp_ms": 1762981260000, "content": "Dobro, hvala."},
    {"sender_name": "Ana Peric", "timestamp_ms": 1762981320000,
     "photos": [{"uri": "messages/inbox/anaperic_1234567890/photos/ana.jpg", "creation_timestamp": 1762981320}]},
    {"sender_name": "Ana Peric", "timestamp_ms": 1762981380000, "reactions": [{"reaction": "❤"}]}
  ]
}"#;

#[test]
fn parses_single_thread_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("message_1.json");
    std::fs::write(&path, THREAD_JSON).unwrap();

    let source = MessengerSource::new(&path);
    assert_eq!(source.name(), MESSENGER_SOURCE);
    let summary = ingest(&Storage::open_in_memory().unwrap(), &source).unwrap();
    // 3 messages: the reactions-only entry has no body and no media → skipped.
    assert_eq!(
        summary,
        IngestSummary {
            source: MESSENGER_SOURCE.into(),
            seen: 3,
            inserted: 3,
            skipped: 0,
            failed: 0
        }
    );
}

#[test]
fn single_file_maps_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("message_1.json");
    std::fs::write(&path, THREAD_JSON).unwrap();

    let collected = MessengerSource::new(&path).collect().unwrap();
    let first = &collected.messages[0];
    assert_eq!(first.source, MESSENGER_SOURCE);
    assert_eq!(first.channel, "chat");
    assert_eq!(first.chat_id, "Ana Peric");
    assert_eq!(first.author, "Ana Peric");
    // 1762981200000 ms = 2025-11-12T21:00:00Z (verified with
    // `date -u -d @1762981200`; milliseconds dropped).
    assert_eq!(first.sent_at, "2025-11-12T21:00:00Z");
    assert_eq!(first.timezone, "UTC");
    assert_eq!(first.time_confidence, "explicit_offset");
    assert_eq!(first.original_timestamp, "1762981200000");
    assert_eq!(first.conversation_key, "messenger:thread:1234567890");
    assert!(first.body.contains("Zdravo"));
    // The photo message keeps its whole object in raw_json.
    let photo = &collected.messages[2];
    let raw: serde_json::Value = serde_json::from_str(&photo.raw_json).unwrap();
    assert_eq!(
        raw["photos"][0]["uri"],
        "messages/inbox/anaperic_1234567890/photos/ana.jpg"
    );
}

#[test]
fn reimport_is_idempotent_and_rename_stable() {
    let storage = Storage::open_in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("message_1.json");
    std::fs::write(&path, THREAD_JSON).unwrap();
    assert_eq!(
        ingest(&storage, &MessengerSource::new(&path))
            .unwrap()
            .inserted,
        3
    );
    let again = ingest(&storage, &MessengerSource::new(&path)).unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.skipped, 3);
    assert_eq!(storage.stats().unwrap().total, 3);
}

#[test]
fn streaming_path_matches_collect() {
    // The production path is `stream` (two-pass, one entry alive at a
    // time); its items must match the whole-file parser exactly — same
    // messages, same order, same generated ids (the occurrence map is
    // shared across the whole file, not per entry).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("message_1.json");
    std::fs::write(&path, THREAD_JSON).unwrap();

    use vault_core::bounded::{Flow, StreamItem, StreamSource};
    let collected = MessengerSource::new(&path).collect().unwrap();
    let mut streamed = Vec::new();
    MessengerSource::new(&path)
        .stream(0, &mut |item| {
            streamed.push(item);
            Ok(Flow::Continue)
        })
        .unwrap();
    assert_eq!(streamed.len(), collected.messages.len());
    for (item, msg) in streamed.iter().zip(&collected.messages) {
        assert_eq!(item, &StreamItem::Message(msg.clone()));
    }
}

#[test]
fn imported_thread_is_full_text_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("message_1.json");
    std::fs::write(&path, THREAD_JSON).unwrap();
    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &MessengerSource::new(&path)).unwrap();
    assert!(!storage
        .search_filtered(&build_match_query("zdravo"), None, None, 10)
        .unwrap()
        .is_empty());
}

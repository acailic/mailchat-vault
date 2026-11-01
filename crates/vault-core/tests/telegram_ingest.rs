use vault_core::ingest::{ingest, IngestSummary, Source};
use vault_core::search::build_match_query;
use vault_core::storage::Storage;
use vault_core::telegram::{parse_telegram, TelegramSource, TELEGRAM_SOURCE};

// Realistic Telegram Desktop export: unix timestamps (UTC) alongside naive
// local dates, string and array texts with entities, an attachment, a
// reply, a message with only from_id, a service entry, and two broken
// entries.
const RESULT_JSON: &str = r#"{
  "name": "Arhiva ekipa",
  "type": "private_group",
  "id": 123456789,
  "messages": [
    {
      "id": 100,
      "type": "message",
      "date": "2023-11-01T11:00:00",
      "date_unixtime": "1698832800",
      "from": "Marko Horvat",
      "from_id": "user111",
      "text": "Zdravo ekipa! 👋 Arhiva je gotova."
    },
    {
      "id": 101,
      "type": "message",
      "date": "2023-11-01T11:01:00",
      "date_unixtime": "1698832860",
      "from": "Jelena Babić",
      "from_id": "user222",
      "text": ["prvi red\n", "vidi link: ", {"type": "link", "text": "https://example.com/plan"}, " i kraj"]
    },
    {
      "id": 102,
      "type": "message",
      "date": "2023-11-01T11:02:00",
      "date_unixtime": "1698832920",
      "from": "Ana Šimić",
      "photo": "photos/photo_102@01-11-2023_11-02-00.jpg",
      "width": 800,
      "height": 600,
      "text": ""
    },
    {
      "id": 103,
      "type": "message",
      "date": "2023-11-01T11:03:00",
      "date_unixtime": "1698832980",
      "from": "Marko Horvat",
      "reply_to_message_id": 101,
      "text": "odgovor na link"
    },
    {
      "id": 104,
      "type": "message",
      "date": "2023-11-01T12:30:45",
      "from_id": "user333",
      "text": "bez imena, samo from_id"
    },
    {
      "type": "phone_call",
      "date": "2023-11-01T11:05:00",
      "actor": "Ana Šimić",
      "actor_id": "user333",
      "action": "phone_call"
    },
    { "text": "nema id polja" },
    "nije objekt"
  ]
}"#;

#[test]
fn parses_fields_from_result_json() {
    let collected = parse_telegram(RESULT_JSON.as_bytes(), "fallback").unwrap();
    // 5 real messages; phone_call is a service entry (skipped); one entry
    // lacks an id and one is not an object (both failed).
    assert_eq!(collected.messages.len(), 5);
    assert_eq!(collected.failed, 2);

    let first = &collected.messages[0];
    assert_eq!(first.source, TELEGRAM_SOURCE);
    assert_eq!(first.channel, "chat");
    assert_eq!(first.chat_id, "Arhiva ekipa");
    assert_eq!(first.external_id, "123456789/100");
    assert_eq!(first.author, "Marko Horvat");
    // date_unixtime is UTC and wins over the naive local date string
    // (exporter timezone here is UTC+1).
    assert_eq!(first.sent_at, "2023-11-01T10:00:00Z");
    assert!(first.body.contains("Arhiva je gotova"));

    // Array text concatenates plain parts and entity texts in order.
    let second = &collected.messages[1];
    assert_eq!(
        second.body,
        "prvi red\nvidi link: https://example.com/plan i kraj"
    );
    assert_eq!(second.sent_at, "2023-11-01T10:01:00Z");

    // Attachment: media ignored for v0, but the whole original object is
    // kept in raw_json.
    let photo = &collected.messages[2];
    assert_eq!(photo.body, "");
    let raw: serde_json::Value = serde_json::from_str(&photo.raw_json).unwrap();
    assert_eq!(raw["photo"], "photos/photo_102@01-11-2023_11-02-00.jpg");
    assert_eq!(raw["width"], 800);

    // Reply reference also survives in raw_json.
    let reply = &collected.messages[3];
    assert_eq!(reply.body, "odgovor na link");
    let raw: serde_json::Value = serde_json::from_str(&reply.raw_json).unwrap();
    assert_eq!(raw["reply_to_message_id"], 101);

    // Without "from", the author falls back to from_id; without
    // date_unixtime, the naive date is stored as UTC.
    let anon = &collected.messages[4];
    assert_eq!(anon.author, "user333");
    assert_eq!(anon.sent_at, "2023-11-01T12:30:45Z");
}

#[test]
fn chat_id_falls_back_to_id_then_hint() {
    let by_id = r#"{"id": 987654321, "messages": [
        {"id": 1, "type": "message", "date": "2023-11-01T10:00:00", "from": "A", "text": "x"}]}"#;
    let collected = parse_telegram(by_id.as_bytes(), "fallback").unwrap();
    assert_eq!(collected.messages[0].chat_id, "987654321");

    let by_hint = r#"{"messages": [
        {"id": 1, "type": "message", "date": "2023-11-01T10:00:00", "from": "A", "text": "x"}]}"#;
    let collected = parse_telegram(by_hint.as_bytes(), "result").unwrap();
    assert_eq!(collected.messages[0].chat_id, "result");
}

#[test]
fn same_message_id_in_different_chats_is_not_a_duplicate() {
    // Message ids repeat across chats; the dedup namespace comes from the
    // stable export id, so even two chats sharing a display name stay
    // distinct.
    let a = r#"{"name": "Isti naziv", "id": 111, "messages": [
        {"id": 1, "type": "message", "date": "2023-11-01T10:00:00", "from": "A", "text": "iz chata A"}]}"#;
    let b = r#"{"name": "Isti naziv", "id": 222, "messages": [
        {"id": 1, "type": "message", "date": "2023-11-01T10:00:00", "from": "B", "text": "iz chata B"}]}"#;
    let storage = Storage::open_in_memory().unwrap();
    for export in [a, b] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("result.json");
        std::fs::write(&path, export).unwrap();
        let summary = ingest(&storage, &TelegramSource::new(&path)).unwrap();
        assert_eq!(summary.inserted, 1);
    }
    assert_eq!(storage.stats().unwrap().total, 2);
}

#[test]
fn reexport_after_contact_rename_dedups_instead_of_duplicating() {
    // Same export id, different display name: the dedup key follows the
    // id, not the name.
    let before = r#"{"name": "Staro ime", "id": 333, "messages": [
        {"id": 1, "type": "message", "date": "2023-11-01T10:00:00", "from": "A", "text": "poruka"}]}"#;
    let after = r#"{"name": "Novo ime", "id": 333, "messages": [
        {"id": 1, "type": "message", "date": "2023-11-01T10:00:00", "from": "A", "text": "poruka"}]}"#;
    let storage = Storage::open_in_memory().unwrap();
    for (export, expected) in [(before, (1, 0)), (after, (0, 1))] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("result.json");
        std::fs::write(&path, export).unwrap();
        let summary = ingest(&storage, &TelegramSource::new(&path)).unwrap();
        assert_eq!(summary.inserted, expected.0);
        assert_eq!(summary.skipped, expected.1);
    }
    assert_eq!(storage.stats().unwrap().total, 1);
}

#[test]
fn ingest_from_file_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.json");
    std::fs::write(&path, RESULT_JSON).unwrap();

    let storage = Storage::open_in_memory().unwrap();
    let source = TelegramSource::new(&path);
    assert_eq!(source.name(), TELEGRAM_SOURCE);

    let summary = ingest(&storage, &source).unwrap();
    assert_eq!(
        summary,
        IngestSummary {
            source: TELEGRAM_SOURCE.into(),
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
fn imported_chat_is_full_text_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.json");
    std::fs::write(&path, RESULT_JSON).unwrap();

    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &TelegramSource::new(&path)).unwrap();

    for term in ["arhiva", "example.com", "odgovor", "jelena"] {
        let hits = storage.search(&build_match_query(term), 10).unwrap();
        assert!(!hits.is_empty(), "no hits for {term:?}");
    }
    // Service entries are not searchable (they were never stored).
    assert!(storage
        .search(&build_match_query("phone_call"), 10)
        .unwrap()
        .is_empty());
}

#[test]
fn broken_json_and_unreadable_file_report_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.json");
    std::fs::write(&path, b"{not json").unwrap();
    assert!(TelegramSource::new(&path).collect().is_err());

    assert!(TelegramSource::new("/nonexistent/result.json")
        .collect()
        .is_err());
}

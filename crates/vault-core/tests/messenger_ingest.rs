use std::path::PathBuf;

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

/// A two-thread DYI tree: root/messages/inbox/<thread>/message_N.json.
fn dyi_tree(dir: &std::path::Path) -> PathBuf {
    let ana = dir.join("messages/inbox/ana_1111");
    let grupa = dir.join("messages/inbox/grupa_2222");
    std::fs::create_dir_all(&ana).unwrap();
    std::fs::create_dir_all(&grupa).unwrap();
    std::fs::write(
        ana.join("message_1.json"),
        r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
            "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "iz prve polovine"}]}"#,
    )
    .unwrap();
    // A name sort would put message_10 before message_2; the parts must
    // merge by number.
    std::fs::write(
        ana.join("message_10.json"),
        r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
            "messages": [{"sender_name": "Ana", "timestamp_ms": 1762984800000, "content": "deset"}]}"#,
    )
    .unwrap();
    std::fs::write(
        ana.join("message_2.json"),
        r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
            "messages": [{"sender_name": "Ana", "timestamp_ms": 1762982400000, "content": "dva"}]}"#,
    )
    .unwrap();
    std::fs::write(
        grupa.join("message_1.json"),
        r#"{"participants": [{"name": "Bane"}], "title": "Grupa", "thread_path": "inbox/grupa_2222",
            "messages": [{"sender_name": "Bane", "timestamp_ms": 1762981201000, "content": "iz grupe"}]}"#,
    )
    .unwrap();
    dir.to_path_buf()
}

#[test]
fn tree_import_merges_parts_in_numeric_order_per_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    let collected = MessengerSource::new(&root).collect().unwrap();
    assert_eq!(collected.messages.len(), 4);
    // Thread "ana": numeric order — 1, 2, 10 (name sort would give 1, 10, 2).
    let ana: Vec<&vault_core::message::NewMessage> = collected
        .messages
        .iter()
        .filter(|m| m.conversation_key == "messenger:thread:1111")
        .collect();
    assert_eq!(ana.len(), 3);
    assert_eq!(ana[0].body, "iz prve polovine");
    assert_eq!(ana[1].body, "dva");
    assert_eq!(ana[2].body, "deset");
    assert_eq!(ana[0].chat_id, "Ana");
}

#[test]
fn tree_picks_resolve_from_every_level() {
    // The pick may be the DYI root, `messages/`, or `messages/inbox/`:
    // each finds both threads, in path order.
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    for pick in [
        root.clone(),
        root.join("messages"),
        root.join("messages/inbox"),
    ] {
        let collected = MessengerSource::new(&pick).collect().unwrap();
        let bodies: Vec<&str> = collected.messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(
            bodies,
            ["iz prve polovine", "dva", "deset", "iz grupe"],
            "{}",
            pick.display()
        );
    }
}

#[test]
fn thread_folder_pick_merges_its_parts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    let thread = root.join("messages/inbox/ana_1111");
    let collected = MessengerSource::new(&thread).collect().unwrap();
    assert_eq!(collected.messages.len(), 3);
    assert!(collected
        .messages
        .iter()
        .all(|m| m.conversation_key == "messenger:thread:1111"));
}

#[test]
fn tree_streaming_matches_collect_and_resumes_across_parts() {
    // The production path is `stream`: it must yield the tree's items
    // exactly as `collect` does (thread order, numeric part order, ids),
    // and `skip` must count across part and thread boundaries.
    use vault_core::bounded::{Flow, StreamItem, StreamSource};
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    let collected = MessengerSource::new(&root).collect().unwrap();
    let mut streamed = Vec::new();
    MessengerSource::new(&root)
        .stream(0, &mut |item| {
            streamed.push(item);
            Ok(Flow::Continue)
        })
        .unwrap();
    assert_eq!(streamed.len(), collected.messages.len());
    for (item, msg) in streamed.iter().zip(&collected.messages) {
        assert_eq!(item, &StreamItem::Message(msg.clone()));
    }
    // Resume past the first two items: the rest of thread "ana" (its
    // third part) and the whole of "grupa" remain.
    let mut resumed = Vec::new();
    MessengerSource::new(&root)
        .stream(2, &mut |item| {
            resumed.push(item);
            Ok(Flow::Continue)
        })
        .unwrap();
    assert_eq!(resumed.len(), 2);
    assert_eq!(
        resumed[0],
        StreamItem::Message(collected.messages[2].clone())
    );
    assert_eq!(
        resumed[1],
        StreamItem::Message(collected.messages[3].clone())
    );
    // A consumer stop ends the stream cleanly mid-tree.
    let mut seen = 0;
    MessengerSource::new(&root)
        .stream(0, &mut |_| {
            seen += 1;
            Ok(if seen == 2 {
                Flow::Stop
            } else {
                Flow::Continue
            })
        })
        .unwrap();
    assert_eq!(seen, 2);
}

#[test]
fn repeats_across_parts_of_one_thread_get_distinct_ids() {
    // The occurrence counter spans a thread's parts: a byte-identical
    // message in part 1 and part 2 must not collapse into one row.
    let tmp = tempfile::tempdir().unwrap();
    let thread = tmp.path().join("ana_1111");
    std::fs::create_dir_all(&thread).unwrap();
    let part = r#"{"participants": [{"name": "Ana"}], "title": "Ana",
        "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "ok"}]}"#;
    std::fs::write(thread.join("message_1.json"), part).unwrap();
    std::fs::write(thread.join("message_2.json"), part).unwrap();
    let collected = MessengerSource::new(&thread).collect().unwrap();
    assert_eq!(collected.messages.len(), 2);
    assert_ne!(
        collected.messages[0].external_id,
        collected.messages[1].external_id
    );
    let storage = Storage::open_in_memory().unwrap();
    assert_eq!(
        ingest(&storage, &MessengerSource::new(&thread))
            .unwrap()
            .inserted,
        2
    );
}

#[test]
fn directory_without_threads_is_refused() {
    // Hidden and macOS-junk entries never count as threads.
    let tmp = tempfile::tempdir().unwrap();
    let junk = tmp.path().join("__MACOSX/ana_1111");
    std::fs::create_dir_all(&junk).unwrap();
    std::fs::write(junk.join("message_1.json"), THREAD_JSON).unwrap();
    let hidden = tmp.path().join(".cache/ana_1111");
    std::fs::create_dir_all(&hidden).unwrap();
    std::fs::write(hidden.join("message_1.json"), THREAD_JSON).unwrap();
    let err = MessengerSource::new(tmp.path()).collect().unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("no Messenger message_*.json files found under"),
        "{text}"
    );
    assert!(text.contains(&tmp.path().display().to_string()), "{text}");
}

#[test]
fn tree_import_through_chat_export_is_dedup_idempotent() {
    use vault_core::chat_export::import_bounded;
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    // `Storage` implements `IngestSink` directly (as the other bounded
    // import tests use it).
    let mut storage = Storage::open_in_memory().unwrap();
    let first = import_bounded(
        &mut storage,
        &root,
        false,
        None,
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(first.summary.source, MESSENGER_SOURCE);
    assert_eq!(first.summary.inserted, 4);
    let second = import_bounded(
        &mut storage,
        &root,
        false,
        None,
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(second.summary.inserted, 0);
    assert_eq!(second.summary.skipped, 4);
}

/// Fetch a stored message's raw_json by its body text (Messenger ids are
/// generated content hashes, not predictable in tests), browsing the
/// archive so empty-body messages are included too.
fn raw_json_by_body(storage: &Storage, wanted: &str) -> String {
    use vault_core::query::SearchQuery;
    let page = storage
        .search_page(&SearchQuery {
            limit: 100,
            ..SearchQuery::default()
        })
        .unwrap();
    let hit = page
        .hits
        .iter()
        .find(|h| h.message.body == wanted)
        .unwrap_or_else(|| panic!("message with body {wanted:?} not stored"));
    hit.message.raw_json.clone()
}

/// Add one real photo referenced by the ana thread's first message: a
/// `photos/ana.jpg` file under the thread plus a root-relative `uri`
/// reference (the DYI uri shape), replacing the text-only message_1.
fn add_photo_reference(root: &std::path::Path) {
    let photos = root.join("messages/inbox/ana_1111/photos");
    std::fs::create_dir_all(&photos).unwrap();
    std::fs::write(photos.join("ana.jpg"), b"jpgbytes").unwrap();
    std::fs::write(
        root.join("messages/inbox/ana_1111/message_1.json"),
        r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
            "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "slika",
            "photos": [{"uri": "messages/inbox/ana_1111/photos/ana.jpg", "creation_timestamp": 1762981200}]}]}"#,
    )
    .unwrap();
}

/// dyi_tree plus the photo reference, with the thread's other two part
/// files removed for a focused count: the tree holds two part files and
/// one photo.
fn dyi_tree_with_photo(dir: &std::path::Path) -> PathBuf {
    let root = dyi_tree(dir);
    add_photo_reference(&root);
    std::fs::remove_file(root.join("messages/inbox/ana_1111/message_2.json")).ok();
    std::fs::remove_file(root.join("messages/inbox/ana_1111/message_10.json")).ok();
    root
}

#[test]
fn tree_import_extracts_referenced_media() {
    use vault_core::chat_export::import_bounded;
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree_with_photo(tmp.path());
    let data = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = import_bounded(
        &mut storage,
        &root,
        true,
        Some(data.path()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 2);
    let media = outcome.media.expect("media pass ran");
    assert_eq!(media.referenced, 1, "{media:?}");
    assert_eq!(media.stored, 1, "{media:?}");
    assert_eq!(media.linked, 1, "{media:?}");
    // The pick is the DYI root, so the unlinked sweep runs: the tree's
    // two remaining part files are its only unreferenced files (the
    // photo was consumed by its message).
    assert_eq!(media.unlinked, 2, "{media:?}");

    // The stored link rides in raw_json: query the stored message row
    // and assert its media entry.
    let raw: serde_json::Value =
        serde_json::from_str(&raw_json_by_body(&storage, "slika")).unwrap();
    let entry = &raw["media"][0];
    assert_eq!(entry["name"], "ana.jpg");
    assert_eq!(entry["kind"], "image");
    assert_eq!(entry["sha256"].as_str().unwrap().len(), 64);
    assert!(entry["path"].as_str().unwrap().starts_with("media/"));
    assert_eq!(entry["size"], 8);
    // The file itself landed under the hashed name.
    let stored = data.path().join(entry["path"].as_str().unwrap());
    assert!(stored.is_file());
}

#[test]
fn tree_reimport_with_extraction_keeps_media_links_on_skipped_rows() {
    // Import with extraction ON twice: the second run's messages are
    // dedup-skipped, and their rows must still end up with `media`
    // entries — the skip-path backfill is a no-op only because the
    // digests already match, never a loss of the links.
    use vault_core::chat_export::import_bounded;
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree_with_photo(tmp.path());
    let data = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    let first = import_bounded(
        &mut storage,
        &root,
        true,
        Some(data.path()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(first.summary.inserted, 2);
    assert_eq!(first.media.as_ref().unwrap().linked, 1);

    let second = import_bounded(
        &mut storage,
        &root,
        true,
        Some(data.path()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(second.summary.inserted, 0);
    assert_eq!(second.summary.skipped, 2);
    let media = second.media.unwrap();
    assert_eq!(media.stored, 0, "{media:?}");
    assert_eq!(media.deduped, 1, "same content is recognized, not recopied");
    assert_eq!(media.linked, 0, "rows already carry identical links");
    // The skipped photo message still carries its media entry.
    let raw: serde_json::Value =
        serde_json::from_str(&raw_json_by_body(&storage, "slika")).unwrap();
    assert_eq!(raw["media"][0]["name"], "ana.jpg");
    assert!(raw["media"][0]["sha256"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
}

#[test]
fn reimport_enriches_rows_imported_without_extraction() {
    // The interim archive shape (Task 6): rows present, references only
    // in `raw_json`, no media arrays. A re-import with extraction ON
    // must enrich those rows — the dedup-skip path backfills `media`
    // entries onto already-stored messages.
    use vault_core::chat_export::import_bounded;
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree_with_photo(tmp.path());
    let data = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    let interim = import_bounded(
        &mut storage,
        &root,
        false,
        Some(data.path()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(interim.summary.inserted, 2);
    assert!(interim.media.is_none());
    let before = raw_json_by_body(&storage, "slika");
    assert!(serde_json::from_str::<serde_json::Value>(&before)
        .unwrap()
        .get("media")
        .is_none());

    let enriched = import_bounded(
        &mut storage,
        &root,
        true,
        Some(data.path()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(enriched.summary.inserted, 0);
    assert_eq!(enriched.summary.skipped, 2);
    let media = enriched.media.expect("extraction ran");
    assert_eq!(media.referenced, 1, "{media:?}");
    assert_eq!(
        media.stored, 1,
        "the bytes enter the store for the first time"
    );
    // The skip-path backfill linked the media entry onto the stored row.
    assert_eq!(media.linked, 1, "{media:?}");
    let after: serde_json::Value =
        serde_json::from_str(&raw_json_by_body(&storage, "slika")).unwrap();
    let entry = &after["media"][0];
    assert_eq!(entry["name"], "ana.jpg");
    assert!(entry["sha256"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
fn absent_photo_uri_records_a_missing_entry_not_a_silent_drop() {
    // A trimmed export: the uri points at a file the tree does not hold.
    // The reference is recorded as an explicit missing entry, and the
    // message still counts as linked (its raw_json gained the media
    // array carrying the marker).
    use vault_core::chat_export::import_bounded;
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree_with_photo(tmp.path());
    std::fs::remove_file(root.join("messages/inbox/ana_1111/photos/ana.jpg")).unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = import_bounded(
        &mut storage,
        &root,
        true,
        Some(data.path()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    let media = outcome.media.unwrap();
    assert_eq!(media.referenced, 1, "{media:?}");
    assert_eq!(media.missing, 1, "{media:?}");
    assert_eq!(media.stored, 0, "{media:?}");
    assert_eq!(media.linked, 1, "{media:?}");
    let raw: serde_json::Value =
        serde_json::from_str(&raw_json_by_body(&storage, "slika")).unwrap();
    let entry = &raw["media"][0];
    assert_eq!(entry["name"], "ana.jpg");
    assert_eq!(entry["missing"], true);
    assert_eq!(entry["reason"], "not-in-export");
    assert!(entry.get("sha256").is_none());
}

#[test]
fn tree_pick_configures_the_media_pass_at_the_dyi_root() {
    // This fixture's messages reference no files: the message pass links
    // nothing, and the whole-root unlinked sweep stores the tree's four
    // part files (extraction of referenced files is pinned by
    // tree_import_extracts_referenced_media).
    use vault_core::chat_export::import_bounded;
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    let data = tempfile::tempdir().unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = import_bounded(
        &mut storage,
        &root,
        true,
        Some(data.path()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 4);
    let media = outcome.media.expect("the media pass is configured");
    assert_eq!(media.linked, 0, "{media:?}");
    // The pick is the DYI root: the unlinked sweep stored the tree's
    // four part files (no message references them).
    assert_eq!(media.unlinked, 4, "{media:?}");
    assert_eq!(media.stored, 0, "{media:?}");
}

#[test]
fn messenger_media_pass_scope_follows_the_pick() {
    use vault_core::chat_export::import_bounded;
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    add_photo_reference(&root);
    let data = tempfile::tempdir().unwrap();

    // A single-file pick gets no media pass at all: its references stay
    // in `raw_json` only (spec decision).
    let file = root.join("messages/inbox/ana_1111/message_1.json");
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = import_bounded(
        &mut storage,
        &file,
        true,
        Some(data.path()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 1);
    assert!(outcome.media.is_none());

    // A thread-folder pick configures the pass with the DYI root as the
    // export root, so the root-relative uri resolves and links — but the
    // pick is not the DYI root: no unlinked sweep over the tree around
    // the thread.
    let thread = root.join("messages/inbox/ana_1111");
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = import_bounded(
        &mut storage,
        &thread,
        true,
        Some(data.path()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    assert_eq!(outcome.summary.inserted, 3);
    let media = outcome.media.expect("the media pass is configured");
    assert_eq!(media.referenced, 1, "{media:?}");
    assert_eq!(media.stored, 1, "{media:?}");
    assert_eq!(media.linked, 1, "{media:?}");
    assert_eq!(media.unlinked, 0, "{media:?}");
}

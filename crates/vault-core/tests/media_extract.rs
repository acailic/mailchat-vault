// Media extraction from chat exports: real fixture files (a JPEG, a PDF,
// a WAV committed under tests/fixtures/media) are laid out as an export
// tree in a temp dir, imported, and the store + raw_json links verified —
// including hash dedup, backfill of an already-imported archive, path
// escapes, and the extraction-off option.

use std::fs;
use std::path::{Path, PathBuf};

use vault_core::chat_export::{import_bounded, ChatImportOutcome};
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

/// Real small files committed with the repo.
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/media");

fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn sha256_hex(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path).unwrap());
    format!("{:x}", hasher.finalize())
}

/// One temp workspace per test: an `export/` tree and a `data/` app-data
/// dir, so store writes never touch the repo.
struct Sandbox {
    #[allow(dead_code)]
    keeper: tempfile::TempDir,
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let keeper = tempfile::tempdir().unwrap();
        let root = keeper.path().join(tag);
        fs::create_dir_all(&root).unwrap();
        Self { keeper, root }
    }

    fn export_dir(&self) -> PathBuf {
        let dir = self.root.join("export");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    fn store_dir(&self) -> PathBuf {
        self.data_dir().join("media")
    }

    /// Copy a fixture into the export tree at `rel`.
    fn put(&self, rel: &str, fixture_name: &str) {
        let target = self.export_dir().join(rel);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(fixture(fixture_name), &target).unwrap();
    }

    fn import_with_media(&self, storage: &mut Storage) -> ChatImportOutcome {
        self.import(storage, true)
    }

    fn import(&self, storage: &mut Storage, extract_media: bool) -> ChatImportOutcome {
        self.import_picked(storage, &self.export_dir(), extract_media)
    }

    /// Import an explicitly picked path (a WhatsApp `.txt` file, like the
    /// dialog returns; the export root is its directory).
    fn import_picked(
        &self,
        storage: &mut Storage,
        picked: &Path,
        extract_media: bool,
    ) -> ChatImportOutcome {
        import_bounded(
            storage,
            picked,
            extract_media,
            Some(&self.data_dir()),
            &|| false,
            &mut |_, _, _| {},
        )
        .map(ChatImportOutcome::from)
        .unwrap()
    }
}

/// A Telegram export whose messages reference three real media files
/// (photo, document, voice) plus a plain text message.
fn write_telegram_export(sb: &Sandbox) {
    sb.put("photos/photo_102@01-11-2023_11-02-00.jpg", "photo.jpg");
    sb.put("files/plan.pdf", "plan.pdf");
    sb.put("voice_messages/audio_104@01-11-2023.ogg", "beep.wav");
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{
  "name": "Arhiva ekipa",
  "type": "private_group",
  "id": 123456789,
  "messages": [
    { "id": 100, "type": "message", "date_unixtime": "1698832800",
      "from": "Marko", "text": "zdravo ekipa" },
    { "id": 102, "type": "message", "date_unixtime": "1698832920",
      "from": "Ana", "photo": "photos/photo_102@01-11-2023_11-02-00.jpg",
      "width": 800, "height": 600, "text": "" },
    { "id": 103, "type": "message", "date_unixtime": "1698832980",
      "from": "Marko", "file": "files/plan.pdf", "text": "" },
    { "id": 104, "type": "message", "date_unixtime": "1698833040",
      "from": "Jelena", "media_type": "voice_message",
      "file": "voice_messages/audio_104@01-11-2023.ogg", "text": "" }
  ]
}"#,
    )
    .unwrap();
}

/// Fetch a stored message's raw_json by browsing the archive (empty query
/// = newest-first, so messages with empty bodies are included too).
fn raw_json_of(storage: &Storage, wanted_id: &str) -> String {
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
        .find(|h| h.message.external_id == wanted_id)
        .unwrap_or_else(|| panic!("message {wanted_id} not stored"));
    hit.message.raw_json.clone()
}

#[test]
fn telegram_media_is_extracted_hash_named_and_linked() {
    let sb = Sandbox::new("tg-extract");
    write_telegram_export(&sb);
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_with_media(&mut storage);
    assert_eq!(outcome.summary.inserted, 4);
    assert_eq!(outcome.summary.failed, 0);
    let media = outcome.media.expect("media pass ran");
    assert_eq!(media.referenced, 3);
    assert_eq!(media.stored, 3);
    assert_eq!(media.deduped, 0);
    assert_eq!(media.missing, 0);
    assert_eq!(media.linked, 3);

    // Each fixture landed under its hash name with its real extension.
    let photo_sha = sha256_hex(&fixture("photo.jpg"));
    let stored_photo = sb.store_dir().join(format!("{photo_sha}.jpg"));
    assert!(stored_photo.is_file(), "hash-named copy missing");
    assert_eq!(
        fs::read(&stored_photo).unwrap(),
        fs::read(fixture("photo.jpg")).unwrap()
    );
    assert_eq!(fs::read_dir(sb.store_dir()).unwrap().count(), 3);

    // The photo message's raw_json: verbatim export fields plus the link.
    let raw: serde_json::Value =
        serde_json::from_str(&raw_json_of(&storage, "123456789/102")).unwrap();
    assert_eq!(raw["photo"], "photos/photo_102@01-11-2023_11-02-00.jpg");
    assert_eq!(raw["width"], 800);
    let entry = &raw["media"][0];
    assert_eq!(entry["kind"], "image");
    assert_eq!(entry["name"], "photo_102@01-11-2023_11-02-00.jpg");
    assert_eq!(entry["sha256"], photo_sha);
    assert_eq!(entry["path"], format!("media/{photo_sha}.jpg"));
    assert_eq!(
        entry["size"],
        fs::metadata(fixture("photo.jpg")).unwrap().len()
    );

    // Document and voice: kinds from field/media_type.
    let doc: serde_json::Value =
        serde_json::from_str(&raw_json_of(&storage, "123456789/103")).unwrap();
    assert_eq!(doc["media"][0]["kind"], "document");
    assert_eq!(doc["media"][0]["name"], "plan.pdf");
    let voice: serde_json::Value =
        serde_json::from_str(&raw_json_of(&storage, "123456789/104")).unwrap();
    assert_eq!(voice["media_type"], "voice_message");
    assert_eq!(voice["media"][0]["kind"], "audio");
    assert_eq!(voice["media"][0]["name"], "audio_104@01-11-2023.ogg");

    // Text message untouched: no media array.
    let text: serde_json::Value =
        serde_json::from_str(&raw_json_of(&storage, "123456789/100")).unwrap();
    assert!(text.get("media").is_none());
}

#[test]
fn video_thumbnail_keeps_image_kind_and_file_takes_the_hint() {
    let sb = Sandbox::new("tg-thumb");
    fs::create_dir_all(sb.export_dir().join("photos")).unwrap();
    fs::create_dir_all(sb.export_dir().join("video_files")).unwrap();
    fs::copy(
        fixture("photo.jpg"),
        sb.export_dir().join("photos/thumb.jpg"),
    )
    .unwrap();
    fs::write(
        sb.export_dir().join("video_files/clip.mp4"),
        b"fake-video-bytes",
    )
    .unwrap();
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{"name": "d", "id": 9, "messages": [
  { "id": 1, "type": "message", "date_unixtime": "1698832800", "from": "A",
    "media_type": "video_file", "photo": "photos/thumb.jpg", "file": "video_files/clip.mp4", "text": "" }
]}"#,
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_with_media(&mut storage);
    let media = outcome.media.unwrap();
    assert_eq!(media.stored, 2);
    assert_eq!(media.deduped, 0);

    let raw: serde_json::Value = serde_json::from_str(&raw_json_of(&storage, "9/1")).unwrap();
    let thumb = &raw["media"][0];
    let clip = &raw["media"][1];
    assert_eq!(
        thumb["kind"], "image",
        "thumbnail kind from its own .jpg extension"
    );
    assert_eq!(thumb["name"], "thumb.jpg");
    assert_eq!(
        clip["kind"], "video",
        "the file ref takes the media_type hint"
    );
    assert_eq!(clip["name"], "clip.mp4");
}

#[test]
fn telegram_reimport_dedups_files_and_rows() {
    let sb = Sandbox::new("tg-reimport");
    write_telegram_export(&sb);
    let mut storage = Storage::open_in_memory().unwrap();

    sb.import_with_media(&mut storage);
    let before: Vec<String> = vec![
        raw_json_of(&storage, "123456789/102"),
        raw_json_of(&storage, "123456789/103"),
    ];

    let outcome = sb.import_with_media(&mut storage);
    assert_eq!(outcome.summary.inserted, 0);
    assert_eq!(outcome.summary.skipped, 4);
    let media = outcome.media.unwrap();
    assert_eq!(media.stored, 0);
    assert_eq!(media.deduped, 3, "same content is recognized, not recopied");
    assert_eq!(media.linked, 0, "rows already carry identical links");
    assert_eq!(media.missing, 0);
    // Still exactly three files in the store.
    assert_eq!(fs::read_dir(sb.store_dir()).unwrap().count(), 3);

    let after: Vec<String> = vec![
        raw_json_of(&storage, "123456789/102"),
        raw_json_of(&storage, "123456789/103"),
    ];
    assert_eq!(before, after, "re-import must not rewrite raw_json");
}

#[test]
fn identical_content_in_two_messages_is_deduped_by_hash() {
    let sb = Sandbox::new("tg-dedup");
    let photo = fs::read(fixture("photo.jpg")).unwrap();
    fs::create_dir_all(sb.export_dir().join("photos")).unwrap();
    fs::write(sb.export_dir().join("photos/a.jpg"), &photo).unwrap();
    fs::write(sb.export_dir().join("photos/b.jpg"), &photo).unwrap();
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{"name": "d", "id": 7, "messages": [
  { "id": 1, "type": "message", "date_unixtime": "1698832800", "from": "A", "photo": "photos/a.jpg", "text": "" },
  { "id": 2, "type": "message", "date_unixtime": "1698832860", "from": "B", "photo": "photos/b.jpg", "text": "" }
]}"#,
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_with_media(&mut storage);
    let media = outcome.media.unwrap();
    assert_eq!(media.stored, 1);
    assert_eq!(media.deduped, 1);
    assert_eq!(media.linked, 2);
    // One file on disk; both messages link to it.
    assert_eq!(fs::read_dir(sb.store_dir()).unwrap().count(), 1);
    let first: serde_json::Value = serde_json::from_str(&raw_json_of(&storage, "7/1")).unwrap();
    let second: serde_json::Value = serde_json::from_str(&raw_json_of(&storage, "7/2")).unwrap();
    assert_eq!(first["media"][0]["path"], second["media"][0]["path"]);
    assert_eq!(first["media"][0]["sha256"], second["media"][0]["sha256"]);
    assert_eq!(first["media"][0]["name"], "a.jpg");
    assert_eq!(second["media"][0]["name"], "b.jpg");
}

#[test]
fn identical_content_referenced_as_jpg_then_jpeg_is_stored_once() {
    // The store is keyed by content, not (content, extension): the same
    // bytes arriving as `a.jpg` in one export and `c.jpeg` in another are
    // stored once, and the second reference reuses the first entry's name.
    let sb = Sandbox::new("tg-cross-ext");
    let photo = fs::read(fixture("photo.jpg")).unwrap();
    fs::create_dir_all(sb.export_dir().join("photos")).unwrap();
    fs::write(sb.export_dir().join("photos/a.jpg"), &photo).unwrap();
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{"name": "d", "id": 11, "messages": [
  { "id": 1, "type": "message", "date_unixtime": "1698832800", "from": "A", "photo": "photos/a.jpg", "text": "" }
]}"#,
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    let first = sb.import_with_media(&mut storage);
    assert_eq!(first.media.unwrap().stored, 1);

    // A later export re-encodes the same photo as `c.jpeg`, imported into
    // the same app-data dir (same media store).
    let later = sb.root.join("export-later");
    fs::create_dir_all(later.join("photos")).unwrap();
    fs::write(later.join("photos/c.jpeg"), &photo).unwrap();
    fs::write(
        later.join("result.json"),
        r#"{"name": "d", "id": 12, "messages": [
  { "id": 1, "type": "message", "date_unixtime": "1698832900", "from": "A", "photo": "photos/c.jpeg", "text": "" }
]}"#,
    )
    .unwrap();
    let second = import_bounded(
        &mut storage,
        &later,
        true,
        Some(&sb.data_dir()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    let media = second.media.unwrap();
    assert_eq!(media.stored, 0, "no second copy is written");
    assert_eq!(
        media.deduped, 1,
        "recognized by content despite the extension"
    );
    assert_eq!(fs::read_dir(sb.store_dir()).unwrap().count(), 1);
    let raw: serde_json::Value = serde_json::from_str(&raw_json_of(&storage, "12/1")).unwrap();
    assert_eq!(raw["media"][0]["path"], first_stored_path(&storage, "11/1"));
}

/// Store path recorded for the first import, so the test can prove the
/// later reference reuses that exact entry rather than writing a twin.
fn first_stored_path(storage: &Storage, wanted_id: &str) -> String {
    let raw: serde_json::Value = serde_json::from_str(&raw_json_of(storage, wanted_id)).unwrap();
    raw["media"][0]["path"].as_str().unwrap().to_string()
}

#[test]
fn text_only_export_counts_missing_and_imports_anyway() {
    let sb = Sandbox::new("tg-text-only");
    // result.json referencing files that are NOT next to it.
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{"name": "d", "id": 9, "messages": [
  { "id": 1, "type": "message", "date_unixtime": "1698832800", "from": "A",
    "photo": "photos/missing.jpg", "text": "samo tekst" }
]}"#,
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_with_media(&mut storage);
    assert_eq!(outcome.summary.inserted, 1);
    let media = outcome.media.unwrap();
    assert_eq!(media.referenced, 1);
    assert_eq!(media.missing, 1);
    assert_eq!(media.stored, 0);
    // The row gained the honest missing marker (a `media` entry with
    // no digest — the reference is visible, not silently dropped).
    assert_eq!(media.linked, 1);
    let raw: serde_json::Value = serde_json::from_str(&raw_json_of(&storage, "9/1")).unwrap();
    let entry = &raw["media"][0];
    assert_eq!(entry["name"], "missing.jpg");
    assert_eq!(entry["kind"], "image");
    assert_eq!(entry["missing"], true);
    assert_eq!(entry["reason"], "not-in-export");
    assert!(entry.get("sha256").is_none());
    assert!(entry.get("path").is_none());
    // No bytes were stored — no store directory was created.
    assert!(!sb.store_dir().exists());
}

#[test]
fn reimport_with_media_backfills_an_existing_archive() {
    let sb = Sandbox::new("tg-backfill");
    write_telegram_export(&sb);
    let mut storage = Storage::open_in_memory().unwrap();

    // First import with extraction off: text-only rows, no links.
    let outcome = sb.import(&mut storage, false);
    assert_eq!(outcome.summary.inserted, 4);
    assert!(outcome.media.is_none());
    assert!(!sb.store_dir().exists());
    let plain: serde_json::Value =
        serde_json::from_str(&raw_json_of(&storage, "123456789/102")).unwrap();
    assert!(plain.get("media").is_none());

    // Same export, extraction on: all rows are duplicates but the three
    // media messages gain their links — the separate pass over the
    // existing archive.
    let outcome = sb.import_with_media(&mut storage);
    assert_eq!(outcome.summary.inserted, 0);
    assert_eq!(outcome.summary.skipped, 4);
    let media = outcome.media.unwrap();
    assert_eq!(media.stored, 3);
    assert_eq!(media.linked, 3);
    let linked: serde_json::Value =
        serde_json::from_str(&raw_json_of(&storage, "123456789/102")).unwrap();
    assert_eq!(linked["media"][0]["kind"], "image");
    // Original fields survive the backfill merge.
    assert_eq!(linked["photo"], "photos/photo_102@01-11-2023_11-02-00.jpg");

    // A third run changes nothing.
    let outcome = sb.import_with_media(&mut storage);
    assert_eq!(outcome.media.unwrap().linked, 0);
    assert_eq!(fs::read_dir(sb.store_dir()).unwrap().count(), 3);
}

#[test]
fn path_escapes_are_rejected_as_missing() {
    let sb = Sandbox::new("tg-escape");
    // A file OUTSIDE the export root that the export tries to reach.
    let outside = sb.root.join("outside.pdf");
    fs::write(&outside, fs::read(fixture("plan.pdf")).unwrap()).unwrap();
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{"name": "d", "id": 5, "messages": [
  { "id": 1, "type": "message", "date_unixtime": "1698832800", "from": "A", "file": "../outside.pdf", "text": "" },
  { "id": 2, "type": "message", "date_unixtime": "1698832860", "from": "A", "file": "/etc/passwd", "text": "" },
  { "id": 3, "type": "message", "date_unixtime": "1698832920", "from": "A", "file": "files/none.pdf", "text": "" }
]}"#,
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_with_media(&mut storage);
    assert_eq!(outcome.summary.inserted, 3);
    let media = outcome.media.unwrap();
    assert_eq!(media.missing, 3);
    assert_eq!(media.stored, 0);
    assert!(
        !sb.store_dir().exists(),
        "nothing may be copied from escapes"
    );
}

#[test]
fn whatsapp_attachments_are_matched_by_name() {
    let sb = Sandbox::new("wa-extract");
    let chat = sb.export_dir().join("WhatsApp Chat with Ana.txt");
    fs::write(
        &chat,
        "[1.11.2026., 09:00:00] Marko: Zdravo!\n\
         [1.11.2026., 09:04:00] Marko: IMG-20261101-WA0001.jpg (file attached)\n\
         [1.11.2026., 09:05:00] Ana: plan.pdf (file attached)\n\
         [1.11.2026., 09:06:00] Ana: image omitted\n\
         [1.11.2026., 09:07:00] Ana: document: izostavljen.pdf omitted\n",
    )
    .unwrap();
    // Media next to the txt, one nested a level deeper (some exports
    // group media in subfolders).
    sb.put("IMG-20261101-WA0001.jpg", "photo.jpg");
    sb.put("media_docs/plan.pdf", "plan.pdf");
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_picked(&mut storage, &chat, true);
    assert_eq!(outcome.summary.inserted, 5);
    let media = outcome.media.unwrap();
    // The two named attachments; "image omitted" has no name (not a
    // reference), and the localized "omitted" document has a name but no
    // file next to the export.
    assert_eq!(media.referenced, 3);
    assert_eq!(media.stored, 2);
    assert_eq!(media.missing, 1);
    assert_eq!(media.linked, 3, "two stored links + the missing marker");

    let photo_sha = sha256_hex(&fixture("photo.jpg"));
    assert!(sb.store_dir().join(format!("{photo_sha}.jpg")).is_file());

    // Find the messages by body through FTS.
    for (needle, kind, name) in [
        (
            "IMG-20261101-WA0001.jpg",
            "image",
            "IMG-20261101-WA0001.jpg",
        ),
        ("plan.pdf", "document", "plan.pdf"),
    ] {
        let hits = storage
            .search_filtered(&build_match_query(needle), None, None, 10)
            .unwrap();
        let raw: serde_json::Value = serde_json::from_str(&hits[0].raw_json).unwrap();
        assert_eq!(raw["media"][0]["kind"], kind, "{needle}");
        assert_eq!(raw["media"][0]["name"], name, "{needle}");
        assert_eq!(raw["attachment"]["name"], name, "placeholder kept");
    }

    // Re-import: dedup everywhere, nothing relinked.
    let outcome = sb.import_picked(&mut storage, &chat, true);
    assert_eq!(outcome.summary.skipped, 5);
    let media = outcome.media.unwrap();
    assert_eq!(media.deduped, 2);
    assert_eq!(media.linked, 0);
}

#[test]
fn whatsapp_stem_match_never_shadowed_by_foreign_extension() {
    let sb = Sandbox::new("wa-stem");
    let chat = sb.export_dir().join("WhatsApp Chat with Ana.txt");
    fs::write(
        &chat,
        "[1.11.2026., 09:00:00] Marko: doc\n\
         [1.11.2026., 09:01:00] Ana: plan.pdf (file attached)\n\
         [1.11.2026., 09:02:00] Marko: IMG-1.jpg (file attached)\n",
    )
    .unwrap();
    // A backup whose stem equals the referenced full name must not
    // answer for the missing plan.pdf…
    sb.put("media_docs/plan.pdf.bak", "plan.pdf");
    // …while the documented re-encode flavor still matches by stem.
    sb.put("IMG-1.jpeg", "photo.jpg");
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_picked(&mut storage, &chat, true);
    let media = outcome.media.unwrap();
    assert_eq!(media.referenced, 2);
    assert_eq!(
        media.missing, 1,
        "plan.pdf.bak must not answer for plan.pdf"
    );
    assert_eq!(media.stored, 1, "IMG-1.jpg matches IMG-1.jpeg by stem");
    assert_eq!(fs::read_dir(sb.store_dir()).unwrap().count(), 1);
    let hits = storage
        .search_filtered(&build_match_query("plan.pdf"), None, None, 10)
        .unwrap();
    let raw: serde_json::Value = serde_json::from_str(&hits[0].raw_json).unwrap();
    // The absent plan.pdf is a missing entry — never linked to the
    // backup file's bytes.
    let media = raw["media"].as_array().unwrap();
    assert_eq!(media.len(), 1);
    assert_eq!(media[0]["name"], "plan.pdf");
    assert_eq!(media[0]["missing"], true);
    assert!(media[0].get("sha256").is_none());
}

#[test]
fn whatsapp_export_without_files_reports_missing_named_attachments() {
    let sb = Sandbox::new("wa-text-only");
    fs::write(
        sb.export_dir().join("chat.txt"),
        "[1.11.2026., 09:00:00] Marko: vidimo se\n\
         [1.11.2026., 09:01:00] Ana: IMG-20261101-WA0001.jpg (file attached)\n",
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_picked(&mut storage, &sb.export_dir().join("chat.txt"), true);
    assert_eq!(outcome.summary.inserted, 2);
    let media = outcome.media.unwrap();
    assert_eq!(media.referenced, 1);
    assert_eq!(media.missing, 1);
    // The missing reference is recorded on the message (one marker,
    // no digest) — visible, honestly absent, re-import-stable.
    assert_eq!(media.linked, 1);
    let hits = storage
        .search_filtered(&build_match_query("vidimo se"), None, None, 10)
        .unwrap();
    assert!(!hits.is_empty());
    assert!(!sb.store_dir().exists());
}

#[test]
fn picking_the_export_directory_imports_result_json_with_media() {
    let sb = Sandbox::new("tg-pick-dir");
    write_telegram_export(&sb);
    let mut storage = Storage::open_in_memory().unwrap();
    // import() takes the directory path itself, like the file dialog does.
    let outcome = sb.import_with_media(&mut storage);
    assert_eq!(outcome.summary.source, "telegram");
    assert_eq!(outcome.media.unwrap().stored, 3);
}

#[test]
fn directory_without_result_json_and_unknown_files_error() {
    let sb = Sandbox::new("errors");
    let mut storage = Storage::open_in_memory().unwrap();

    let err = import_bounded(
        &mut storage,
        &sb.export_dir(),
        true,
        Some(&sb.data_dir()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap_err();
    assert!(
        err.to_string().contains(
            "expected a directory containing a Telegram result.json, a Discord Data Package, or a single export file"
        ),
        "{err}"
    );

    let junk = sb.root.join("junk.txt");
    fs::write(&junk, "not an export at all").unwrap();
    let err = import_bounded(
        &mut storage,
        &junk,
        true,
        Some(&sb.data_dir()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap_err();
    assert!(err.to_string().contains("unrecognized"), "{err}");
}

#[test]
fn media_files_never_match_the_export_text_or_json_by_accident() {
    // The WhatsApp walk must not link the .txt itself; Telegram skips
    // result.json because it is never referenced as media.
    let sb = Sandbox::new("wa-self");
    fs::write(
        sb.export_dir().join("chat.txt"),
        "[1.11.2026., 09:00:00] Marko: chat.txt (file attached)\n",
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = sb.import_picked(&mut storage, &sb.export_dir().join("chat.txt"), true);
    // The export names its own .txt; the file exists, is stored — this is
    // a legitimate (if odd) reference, so it links, proving name matching
    // works against real files.
    let media = outcome.media.unwrap();
    assert_eq!(media.stored, 1);
    assert_eq!(media.linked, 1);
    let sha = sha256_hex(&sb.export_dir().join("chat.txt"));
    assert!(sb.store_dir().join(format!("{sha}.txt")).is_file());
}

// ---------------------------------------------------------------------
// M4.2: missing references, re-import enrichment, unlinked files
// ---------------------------------------------------------------------

#[test]
fn missing_entries_survive_reimports_without_duplicating() {
    // AC3 for the missing state: re-importing a text-only export never
    // stacks a second marker, and neither objects nor entries duplicate.
    let sb = Sandbox::new("tg-missing-reimport");
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{"name": "d", "id": 9, "messages": [
  { "id": 1, "type": "message", "date_unixtime": "1698832800", "from": "A",
    "photo": "photos/gone.jpg", "text": "samo tekst" }
]}"#,
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    sb.import_with_media(&mut storage);
    let first = raw_json_of(&storage, "9/1");

    let outcome = sb.import_with_media(&mut storage);
    assert_eq!(outcome.summary.skipped, 1);
    assert_eq!(outcome.media.unwrap().linked, 0, "nothing new to record");
    assert_eq!(raw_json_of(&storage, "9/1"), first, "raw_json untouched");

    // A third import with the file present upgrades the marker in place.
    sb.put("photos/gone.jpg", "photo.jpg");
    let outcome = sb.import_with_media(&mut storage);
    let media = outcome.media.unwrap();
    assert_eq!(media.stored, 1);
    assert_eq!(media.missing, 0);
    assert_eq!(media.linked, 1, "the missing marker was upgraded");
    let raw: serde_json::Value = serde_json::from_str(&raw_json_of(&storage, "9/1")).unwrap();
    let entries = raw["media"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "upgrade replaces, not appends");
    let photo_sha = sha256_hex(&fixture("photo.jpg"));
    assert_eq!(entries[0]["sha256"], photo_sha);
    assert!(entries[0].get("missing").is_none());
    assert_eq!(entries[0]["name"], "gone.jpg");
    // And stays stable on the next identical re-import.
    let after_upgrade = raw_json_of(&storage, "9/1");
    let outcome = sb.import_with_media(&mut storage);
    assert_eq!(outcome.media.unwrap().linked, 0);
    assert_eq!(raw_json_of(&storage, "9/1"), after_upgrade);
}

#[test]
fn unlinked_export_files_are_stored_and_registered() {
    // A directory pick: two stray files sit next to the export's
    // referenced media; both are stored and registered, the referenced
    // files are not double-counted, and the export's own json/html are
    // left alone.
    let sb = Sandbox::new("tg-unlinked");
    write_telegram_export(&sb);
    // Stray files with content no message references (distinct bytes,
    // or the digest check would rightly treat them as linked copies).
    fs::write(
        sb.export_dir().join("photos/orphan.png"),
        b"orphan-png-bytes",
    )
    .unwrap();
    fs::write(sb.export_dir().join("files/extra.txt"), b"stray notes").unwrap();
    fs::write(
        sb.export_dir().join("export_results.html"),
        b"<html>export page</html>",
    )
    .unwrap();
    fs::write(sb.export_dir().join("users.html"), b"[]").unwrap();
    // A user's own unreferenced .html/.json files are export content,
    // not exporter sidecars: they are stored like any other stray file.
    fs::write(
        sb.export_dir().join("notes.html"),
        b"<html>my notes</html>",
    )
    .unwrap();
    fs::write(sb.export_dir().join("rules.json"), b"{}").unwrap();
    // The exporter's conversation pages (messages.html, numbered and
    // -part variants) are sidecars and stay out of the store.
    fs::write(
        sb.export_dir().join("messages.html"),
        b"<html>messages page</html>",
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_with_media(&mut storage);
    let media = outcome.media.unwrap();
    assert_eq!(media.stored, 3, "the three referenced files");
    assert_eq!(media.unlinked, 4, "the four stray files");
    assert_eq!(media.unlinked_deduped, 0);
    // Store: 3 linked + 4 unlinked; the sidecars never land.
    assert_eq!(fs::read_dir(sb.store_dir()).unwrap().count(), 7);

    // Registered: reconciliation counts them as referenced (a clean
    // import with unlinked files stays clean).
    let digests = storage.media_digests().unwrap();
    assert_eq!(digests.len(), 7);
    let objects = storage.unlinked_media_objects().unwrap();
    assert_eq!(objects.len(), 4);
    assert!(objects.iter().all(|o| o.origin == "unlinked"));
    let names: Vec<&str> = objects.iter().map(|o| o.name.as_str()).collect();
    assert!(names.contains(&"orphan.png") && names.contains(&"extra.txt"));
    assert!(names.contains(&"notes.html") && names.contains(&"rules.json"));
    let report = vault_core::object_store::ObjectStore::new(&sb.data_dir())
        .reconcile(&digests, false)
        .unwrap();
    assert!(report.is_clean(), "{report:?}");

    // AC3: re-import stores no new objects, registers no new rows,
    // relinks nothing.
    let outcome = sb.import_with_media(&mut storage);
    let media = outcome.media.unwrap();
    assert_eq!(media.stored, 0);
    assert_eq!(media.deduped, 3);
    assert_eq!(media.unlinked, 0);
    assert_eq!(media.unlinked_deduped, 4);
    assert_eq!(media.linked, 0);
    assert_eq!(fs::read_dir(sb.store_dir()).unwrap().count(), 7);
    assert_eq!(storage.unlinked_media_objects().unwrap().len(), 4);
    let digests = storage.media_digests().unwrap();
    assert_eq!(digests.len(), 7);
}

#[test]
fn bare_file_picks_never_store_the_parent_directory() {
    // The safety rule: a bare export file asserts nothing about its
    // neighbors — stray files next to a picked .txt are NOT stored.
    let sb = Sandbox::new("wa-no-unlinked");
    let chat = sb.export_dir().join("WhatsApp Chat with Ana.txt");
    fs::write(
        &chat,
        "[1.11.2026., 09:00:00] Marko: zdravo\n\
         [1.11.2026., 09:01:00] Ana: IMG-20261101-WA0001.jpg (file attached)\n",
    )
    .unwrap();
    sb.put("IMG-20261101-WA0001.jpg", "photo.jpg");
    fs::copy(fixture("beep.wav"), sb.export_dir().join("vacation.wav")).unwrap();
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_picked(&mut storage, &chat, true);
    let media = outcome.media.unwrap();
    assert_eq!(media.stored, 1, "only the referenced file");
    assert_eq!(media.unlinked + media.unlinked_deduped, 0);
    assert_eq!(fs::read_dir(sb.store_dir()).unwrap().count(), 1);
    assert_eq!(storage.unlinked_media_objects().unwrap().len(), 0);
}

#[test]
fn telegram_object_shaped_file_refs_are_extracted() {
    // Newer exporters write photo/file as objects with a nested path
    // and a file_name; both shapes are explicit references (M4.2).
    let sb = Sandbox::new("tg-object-ref");
    sb.put("photos/pic.jpg", "photo.jpg");
    sb.put("files/report.pdf", "plan.pdf");
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{"name": "d", "id": 3, "messages": [
  { "id": 1, "type": "message", "date_unixtime": "1698832800", "from": "A",
    "photo": { "photo_id": 5, "file": "photos/pic.jpg", "width": 640, "height": 480 }, "text": "" },
  { "id": 2, "type": "message", "date_unixtime": "1698832860", "from": "B",
    "file": { "file_id": 7, "file": "files/report.pdf" }, "file_name": "Izve{0161}taj.pdf", "text": "" }
]}"#,
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();

    let outcome = sb.import_with_media(&mut storage);
    let media = outcome.media.unwrap();
    assert_eq!(media.stored, 2);
    assert_eq!(media.missing, 0);

    let pic: serde_json::Value = serde_json::from_str(&raw_json_of(&storage, "3/1")).unwrap();
    assert_eq!(pic["media"][0]["name"], "pic.jpg");
    assert_eq!(pic["media"][0]["kind"], "image");
    let doc: serde_json::Value = serde_json::from_str(&raw_json_of(&storage, "3/2")).unwrap();
    assert_eq!(
        doc["media"][0]["name"], "Izve{0161}taj.pdf",
        "file_name wins"
    );
    assert_eq!(doc["media"][0]["kind"], "document");
}

#[test]
fn object_shaped_refs_with_missing_files_record_missing_entries() {
    let sb = Sandbox::new("tg-object-missing");
    fs::write(
        sb.export_dir().join("result.json"),
        r#"{"name": "d", "id": 3, "messages": [
  { "id": 1, "type": "message", "date_unixtime": "1698832800", "from": "A",
    "file": { "file_id": 7, "file": "files/lost.pdf" }, "file_name": "lost.pdf", "text": "" }
]}"#,
    )
    .unwrap();
    let mut storage = Storage::open_in_memory().unwrap();
    let outcome = sb.import_with_media(&mut storage);
    let media = outcome.media.unwrap();
    assert_eq!(media.missing, 1);
    let raw: serde_json::Value = serde_json::from_str(&raw_json_of(&storage, "3/1")).unwrap();
    assert_eq!(raw["media"][0]["name"], "lost.pdf");
    assert_eq!(raw["media"][0]["missing"], true);
}

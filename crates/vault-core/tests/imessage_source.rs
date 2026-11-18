use rusqlite::Connection;
use vault_core::bounded::{Flow, StreamItem, StreamSource as _};
use vault_core::chat_export::{detect_chat_format, detect_chat_format_reader, ChatFormat};
use vault_core::imessage::{
    apple_epoch_to_rfc3339, apple_epoch_to_unix_seconds, unix_seconds_to_apple_epoch,
    IMessageSource, IMESSAGE_SOURCE,
};
use vault_core::ingest::{ingest, Source};
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

// A synthetic chat.db with the real table shapes of the Mac Messages
// store (~/Library/Messages/chat.db): the `message`, `handle`,
// `attachment`, and `chat` tables plus the `chat_message_join` /
// `message_attachment_join` join tables. Only the columns the parser
// reads are populated; no test touches the real ~/Library path.
fn fixture_db(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("chat.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE message (
            ROWID INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT,
            service TEXT,
            is_from_me INTEGER DEFAULT 0,
            date INTEGER,
            handle_id INTEGER,
            associated_message_type INTEGER DEFAULT 0
        );
        CREATE TABLE handle (
            ROWID INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT,
            country TEXT,
            service TEXT,
            uncanonicalized_id TEXT
        );
        CREATE TABLE attachment (
            ROWID INTEGER PRIMARY KEY AUTOINCREMENT,
            guid TEXT,
            created_date INTEGER DEFAULT 0,
            start_date INTEGER DEFAULT 0,
            filename TEXT,
            mime_type TEXT,
            total_bytes INTEGER DEFAULT 0
        );
        CREATE TABLE chat (
            ROWID INTEGER PRIMARY KEY AUTOINCREMENT,
            guid TEXT,
            style INTEGER DEFAULT 45,
            state INTEGER DEFAULT 0,
            account_id TEXT,
            chat_identifier TEXT,
            service_name TEXT
        );
        CREATE TABLE chat_message_join (
            chat_id INTEGER,
            message_id INTEGER,
            message_date INTEGER
        );
        CREATE TABLE message_attachment_join (
            message_id INTEGER,
            attachment_id INTEGER
        );
        "#,
    )
    .unwrap();

    // Two handles: a phone number and an email address — the two
    // identity shapes iMessage records.
    conn.execute(
        "INSERT INTO handle (id, country, service) VALUES (?1, ?2, ?3)",
        rusqlite::params!["+15551234567", "us", "iMessage"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO handle (id, country, service) VALUES (?1, ?2, ?3)",
        rusqlite::params!["ana@example.com", "", "iMessage"],
    )
    .unwrap();

    // Two chats, identified the way Messages does: by the other
    // party's handle.
    conn.execute(
        "INSERT INTO chat (guid, chat_identifier, service_name) VALUES (?1, ?2, ?3)",
        rusqlite::params!["guid-chat-1", "+15551234567", "iMessage"],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chat (guid, chat_identifier, service_name) VALUES (?1, ?2, ?3)",
        rusqlite::params!["guid-chat-2", "ana@example.com", "iMessage"],
    )
    .unwrap();

    // 815220000 Apple-epoch seconds = 2026-11-01T10:00:00Z exactly.
    let apple = |unix: i64| unix_seconds_to_apple_epoch(unix);
    // (text, service, is_from_me, unix date, handle)
    type FixtureRow<'a> = (Option<&'a str>, &'a str, i64, i64, Option<i64>);
    let messages: &[FixtureRow<'_>] = &[
        (
            Some("Zdravo iz iMessage! 👋"),
            "iMessage",
            0,
            1793527200,
            Some(1),
        ),
        (
            Some("Jeste, arhivirano!"),
            "iMessage",
            1,
            1793527260,
            Some(1),
        ),
        (
            Some("carrier message over SMS"),
            "SMS",
            0,
            1793527320,
            Some(1),
        ),
        (
            Some("email handle poruka"),
            "iMessage",
            0,
            1793527380,
            Some(2),
        ),
        (None, "iMessage", 0, 1793527440, Some(1)),
    ];
    for (text, service, is_from_me, unix, handle) in messages {
        conn.execute(
            "INSERT INTO message (text, service, is_from_me, date, handle_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![text, service, is_from_me, apple(*unix), handle],
        )
        .unwrap();
    }

    // Message 1 carries one attachment (the classic tilde-rooted
    // Library path); message 5 is attachment-only with NULL text.
    conn.execute(
        "INSERT INTO attachment (guid, filename, mime_type)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![
            "att-1",
            "~/Library/Attachments/9a/61/IMG_0001.jpeg",
            "image/jpeg"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO attachment (guid, filename, mime_type)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![
            "att-2",
            "~/Library/Attachments/dc/40/IMG_0002.mov",
            "video/quicktime"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message_attachment_join (message_id, attachment_id) VALUES (1, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO message_attachment_join (message_id, attachment_id) VALUES (5, 2)",
        [],
    )
    .unwrap();

    // Chat membership through the standard join table.
    for (chat_id, message_id) in [(1, 1), (1, 2), (1, 3), (1, 5), (2, 4)] {
        conn.execute(
            "INSERT INTO chat_message_join (chat_id, message_id) VALUES (?1, ?2)",
            rusqlite::params![chat_id, message_id],
        )
        .unwrap();
    }
    path
}

#[test]
fn parses_text_dates_direction_handles_and_services() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture_db(dir.path());
    let collected = IMessageSource::new(&path).collect().unwrap();

    assert_eq!(collected.failed, 0);
    assert_eq!(collected.messages.len(), 5);

    let first = &collected.messages[0];
    assert_eq!(first.source, IMESSAGE_SOURCE);
    assert_eq!(first.channel, "chat");
    // external_id is the message row id: re-imports dedup on it.
    assert_eq!(first.external_id, "1");
    // Incoming: the author is the handle's phone identity.
    assert_eq!(first.author, "+15551234567");
    // Apple-epoch seconds convert to exact UTC.
    assert_eq!(first.sent_at, "2026-11-01T10:00:00Z");
    assert_eq!(first.body, "Zdravo iz iMessage! 👋");
    assert_eq!(first.timezone, "UTC");
    assert_eq!(first.time_confidence, "explicit_offset");
    assert_eq!(first.original_timestamp, "815220000");
    // Display grouping and the stable conversation key.
    assert_eq!(first.chat_id, "+15551234567");
    assert_eq!(first.conversation_key, "imessage:chat:1");
    let raw: serde_json::Value = serde_json::from_str(&first.raw_json).unwrap();
    assert_eq!(raw["service"], "iMessage");
    assert_eq!(raw["handle"], "+15551234567");
    assert_eq!(raw["is_from_me"], false);

    // Outgoing (is_from_me = 1): archived as the local account, the
    // rule every source without a local-account name follows.
    let outgoing = &collected.messages[1];
    assert_eq!(outgoing.author, "You");
    assert_eq!(outgoing.sent_at, "2026-11-01T10:01:00Z");
    let raw: serde_json::Value = serde_json::from_str(&outgoing.raw_json).unwrap();
    assert_eq!(raw["is_from_me"], true);

    // The SMS service value stays recorded verbatim.
    let sms = &collected.messages[2];
    assert_eq!(sms.body, "carrier message over SMS");
    let raw: serde_json::Value = serde_json::from_str(&sms.raw_json).unwrap();
    assert_eq!(raw["service"], "SMS");

    // An email handle identity is an author too, and its chat keys
    // under its own chat row.
    let email = &collected.messages[3];
    assert_eq!(email.author, "ana@example.com");
    assert_eq!(email.chat_id, "ana@example.com");
    assert_eq!(email.conversation_key, "imessage:chat:2");
}

#[test]
fn attachments_are_recorded_as_referenced_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture_db(dir.path());
    let collected = IMessageSource::new(&path).collect().unwrap();

    // One attachment row on message 1: recorded as a media reference
    // that is explicitly not preserved — the same missing-entry shape
    // every other source writes for unavailable media (the bytes sit
    // under ~/Library/Attachments until a user grants that folder).
    let with_attachment = &collected.messages[0];
    let raw: serde_json::Value = serde_json::from_str(&with_attachment.raw_json).unwrap();
    let entry = &raw["media"][0];
    assert_eq!(entry["name"], "IMG_0001.jpeg");
    assert_eq!(entry["kind"], "image");
    assert_eq!(entry["missing"], true);
    assert_eq!(entry["reason"], "attachments-folder-not-granted");
    assert!(entry.get("sha256").is_none());
    // The attachment metadata itself stays verbatim.
    assert_eq!(
        raw["attachments"][0]["filename"],
        "~/Library/Attachments/9a/61/IMG_0001.jpeg"
    );
    assert_eq!(raw["attachments"][0]["mime_type"], "image/jpeg");

    // Attachment-only message: empty body, the reference survives.
    let photo_only = &collected.messages[4];
    assert_eq!(photo_only.body, "");
    let raw: serde_json::Value = serde_json::from_str(&photo_only.raw_json).unwrap();
    assert_eq!(raw["media"][0]["name"], "IMG_0002.mov");
    assert_eq!(raw["media"][0]["kind"], "video");
    assert_eq!(raw["media"][0]["missing"], true);
}

#[test]
fn apple_epoch_conversions_cover_known_timestamps_both_ways() {
    // The Apple epoch is 978307200 Unix seconds after 1970.
    assert_eq!(apple_epoch_to_unix_seconds(0), 978_307_200);
    assert_eq!(unix_seconds_to_apple_epoch(978_307_200), 0);
    // Unix zero, in Apple seconds.
    assert_eq!(unix_seconds_to_apple_epoch(0), -978_307_200);
    assert_eq!(apple_epoch_to_unix_seconds(-978_307_200), 0);
    // A known wall-clock instant: 2026-11-01T10:00:00Z is Unix
    // 1793527200 = Apple 815220000.
    assert_eq!(unix_seconds_to_apple_epoch(1_793_527_200), 815_220_000);
    assert_eq!(apple_epoch_to_unix_seconds(815_220_000), 1_793_527_200);
    assert_eq!(
        apple_epoch_to_rfc3339(815_220_000).as_deref(),
        Some("2026-11-01T10:00:00Z")
    );
    // Every conversion round-trips.
    for unix in [0i64, 978_307_200, 1_793_527_200, i32::MAX as i64] {
        assert_eq!(
            apple_epoch_to_unix_seconds(unix_seconds_to_apple_epoch(unix)),
            unix
        );
    }
}

#[test]
fn copy_first_leaves_the_original_untouched_and_removes_the_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture_db(dir.path());
    let before = std::fs::read(&path).unwrap();
    let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();

    // The temp dir is pinned so the test can see the copy appear and
    // disappear; it starts empty.
    let temp_root = tempfile::tempdir().unwrap();
    let source = IMessageSource::new(&path).with_temp_dir(temp_root.path());
    assert_eq!(temp_root.path().read_dir().unwrap().count(), 0);

    let collected = source.collect().unwrap();
    assert_eq!(collected.messages.len(), 5);

    // The temporary copy is gone on the success path…
    assert_eq!(
        temp_root.path().read_dir().unwrap().count(),
        0,
        "the temporary chat.db copy must be removed after parsing"
    );
    // …and the picked file is byte-for-byte and mtime unchanged.
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        mtime_before
    );

    // The error path removes it too: a non-database SQLite-labeled
    // file fails the schema check after the copy, and the copy still
    // vanishes.
    let bogus = dir.path().join("not-a-chat.db");
    std::fs::write(&bogus, b"SQLite format 3\0garbage pages").unwrap();
    let failing = IMessageSource::new(&bogus).with_temp_dir(temp_root.path());
    assert!(failing.collect().is_err());
    assert_eq!(temp_root.path().read_dir().unwrap().count(), 0);
}

#[test]
fn stream_matches_collect_and_skip_replays_the_first_message() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture_db(dir.path());
    let collected = IMessageSource::new(&path).collect().unwrap();

    let mut streamed = Vec::new();
    IMessageSource::new(&path)
        .stream(0, &mut |item| {
            if let StreamItem::Message(msg) = item {
                streamed.push(msg);
            }
            Ok(Flow::Continue)
        })
        .unwrap();
    assert_eq!(streamed, collected.messages);

    // skip = 1 fast-forwards past the first message, exactly like a
    // resumed bounded run.
    let mut skipped = Vec::new();
    IMessageSource::new(&path)
        .stream(1, &mut |item| {
            if let StreamItem::Message(msg) = item {
                skipped.push(msg);
            }
            Ok(Flow::Continue)
        })
        .unwrap();
    assert_eq!(skipped, collected.messages[1..].to_vec());
}

#[test]
fn missing_file_reports_clear_guidance() {
    let err = IMessageSource::new("/nonexistent/chat.db")
        .collect()
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("iMessage database not found"),
        "guidance missing from: {err}"
    );
    assert!(
        err.contains("~/Library/Messages/chat.db"),
        "store path guidance missing from: {err}"
    );
}

#[test]
fn unsupported_schema_reports_the_found_schema_hash() {
    // A SQLite database without chat.db's tables: detected by the
    // byte probe, then honestly refused with the schema hash.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("other.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('hi');")
        .unwrap();
    drop(conn);

    let err = IMessageSource::new(&path)
        .collect()
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unsupported chat.db schema version"),
        "schema error missing from: {err}"
    );
    assert!(
        err.contains("schema hash"),
        "schema hash missing from: {err}"
    );
    // The hash is a concrete 16-hex-char value the user can report.
    let hash = err
        .split("schema hash ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .unwrap_or_default();
    assert_eq!(hash.len(), 16, "expected a 16-hex-char hash in: {err}");
    assert!(
        hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "hash not hex: {hash}"
    );
    // The missing pieces are named.
    assert!(err.contains("table `message`"), "{err}");

    // A chat.db missing one column (here: attachment.mime_type) is
    // the same honest refusal, naming the column.
    let partial = dir.path().join("partial.db");
    let conn = Connection::open(&partial).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE message (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT,
            service TEXT, is_from_me INTEGER, date INTEGER, handle_id INTEGER);
        CREATE TABLE handle (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT);
        CREATE TABLE attachment (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, filename TEXT);
        CREATE TABLE chat (ROWID INTEGER PRIMARY KEY AUTOINCREMENT, chat_identifier TEXT);
        CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER);
        CREATE TABLE message_attachment_join (message_id INTEGER, attachment_id INTEGER);
        "#,
    )
    .unwrap();
    drop(conn);
    let err = IMessageSource::new(&partial)
        .collect()
        .unwrap_err()
        .to_string();
    assert!(err.contains("column `attachment.mime_type`"), "{err}");
    assert!(err.contains("schema hash"), "{err}");
}

#[test]
fn detection_claims_sqlite_files_and_resolves_the_imessage_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture_db(dir.path());
    let bytes = std::fs::read(&path).unwrap();

    // A chat.db starts with the SQLite file header — the byte-level
    // probe; the JSON and text exports never match it.
    assert_eq!(detect_chat_format(&bytes), Some(ChatFormat::IMessage));
    assert_eq!(
        detect_chat_format_reader(&path).unwrap(),
        Some(ChatFormat::IMessage)
    );
    let whatsapp = b"[1.11.2026., 09:00:05] Marko: Zdravo ekipa!\n";
    assert_ne!(detect_chat_format(whatsapp), Some(ChatFormat::IMessage));
    assert_eq!(
        detect_chat_format(br#"{"name": "not a db", "messages": []}"#),
        Some(ChatFormat::Telegram)
    );
    // Plain files stay unrecognized.
    assert_eq!(detect_chat_format(b"just some text"), None);

    // The resolved stream source names the imessage dedup namespace.
    let (format, _file, source) =
        vault_core::chat_export::resolve_stream_source(&path, None).unwrap();
    assert_eq!(format, ChatFormat::IMessage);
    assert_eq!(source.name(), IMESSAGE_SOURCE);
}

#[test]
fn ingest_is_idempotent_and_full_text_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture_db(dir.path());
    let storage = Storage::open_in_memory().unwrap();
    let source = IMessageSource::new(&path);
    assert_eq!(Source::name(&source), IMESSAGE_SOURCE);

    let summary = ingest(&storage, &source).unwrap();
    assert_eq!(summary.inserted, 5);
    assert_eq!(summary.failed, 0);
    assert_eq!(storage.stats().unwrap().total, 5);

    // Re-importing the same database (or a later copy of it): every
    // message skips — the row-id external ids dedup.
    let again = ingest(&storage, &source).unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.skipped, 5);
    assert_eq!(storage.stats().unwrap().total, 5);

    for term in ["Zdravo", "arhivirano", "carrier"] {
        let hits = storage
            .search_filtered(&build_match_query(term), None, None, None, false, 10)
            .unwrap();
        assert!(!hits.is_empty(), "no hits for {term:?}");
    }
}

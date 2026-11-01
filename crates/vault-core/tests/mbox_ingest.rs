use vault_core::ingest::{ingest, IngestSummary, Source};
use vault_core::mbox::MboxSource;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

const SAMPLE_MBOX: &str = "\
From alice@example.com Sat Nov  1 10:00:00 2026
From: Alice Smith <alice@example.com>
To: Bob <bob@example.com>
Subject: Vault kickoff meeting
Date: Sat, 1 Nov 2026 10:00:00 +0100
Message-ID: <kickoff-123@example.com>

Hi Bob,

the vault kickoff is confirmed for Monday. Bring the FTS notes.

Alice
";

const SECOND_MESSAGE_NO_ID: &str = "\
From carol@example.com Sat Nov  1 11:30:00 2026
From: Carol Jones <carol@example.com>
To: Bob <bob@example.com>
Subject: Re: Vault kickoff meeting
Date: Sat, 1 Nov 2026 11:30:00 +0100

Sorry I missed it. Was the >From escaping example shown?
Also: the archive schema looked good to me.
";

fn two_message_mbox() -> String {
    format!("{SAMPLE_MBOX}\n{SECOND_MESSAGE_NO_ID}")
}

#[test]
fn parses_headers_body_and_dates() {
    let collected = vault_core::mbox::parse_mbox(two_message_mbox().as_bytes(), "inbox").unwrap();
    assert_eq!(collected.messages.len(), 2);
    assert_eq!(collected.failed, 0);

    let first = &collected.messages[0];
    assert_eq!(first.source, "mbox");
    assert_eq!(first.external_id, "kickoff-123@example.com");
    assert_eq!(first.channel, "email");
    assert_eq!(first.chat_id, "inbox");
    assert_eq!(first.author, "Alice Smith <alice@example.com>");
    // +0100 offset converted to UTC.
    assert_eq!(first.sent_at, "2026-11-01T09:00:00Z");
    // Body includes the subject plus the decoded text body.
    assert!(first.body.starts_with("Vault kickoff meeting"));
    assert!(first.body.contains("Bring the FTS notes."));
    // Structured extras kept in raw_json.
    let raw: serde_json::Value = serde_json::from_str(&first.raw_json).unwrap();
    assert_eq!(raw["subject"], "Vault kickoff meeting");
    assert_eq!(raw["message_id"], "kickoff-123@example.com");
}

#[test]
fn missing_message_id_gets_stable_generated_id() {
    let bytes = two_message_mbox().into_bytes();
    let a = vault_core::mbox::parse_mbox(&bytes, "inbox").unwrap();
    let b = vault_core::mbox::parse_mbox(&bytes, "inbox").unwrap();
    let second_a = &a.messages[1];
    let second_b = &b.messages[1];
    assert!(second_a.external_id.starts_with("generated-"));
    assert_eq!(
        second_a.external_id, second_b.external_id,
        "hash must be deterministic"
    );
    assert_eq!(second_a.sent_at, "2026-11-01T10:30:00Z");
    // Escaped >From line stays inside the body and does not split the mail.
    assert!(second_a.body.contains(">From escaping"));
}

#[test]
fn ingest_from_file_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.mbox");
    std::fs::write(&path, two_message_mbox()).unwrap();

    let storage = Storage::open_in_memory().unwrap();
    let source = MboxSource::new(&path);
    assert_eq!(source.name(), "mbox");

    let summary = ingest(&storage, &source).unwrap();
    assert_eq!(
        summary,
        IngestSummary {
            source: "mbox".into(),
            seen: 2,
            inserted: 2,
            skipped: 0,
            failed: 0
        }
    );
    assert_eq!(storage.stats().unwrap().total, 2);

    // Re-ingesting the same file: everything deduped, nothing duplicated.
    let again = ingest(&storage, &source).unwrap();
    assert_eq!(again.skipped, 2);
    assert_eq!(again.inserted, 0);
    assert_eq!(storage.stats().unwrap().total, 2);
}

#[test]
fn ingested_mail_is_full_text_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.mbox");
    std::fs::write(&path, two_message_mbox()).unwrap();

    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &MboxSource::new(&path)).unwrap();

    // Search body content, subject content, and author.
    for term in ["kickoff", "archive schema", "carol"] {
        let hits = storage.search(&build_match_query(term), 10).unwrap();
        assert!(!hits.is_empty(), "no hits for {term:?}");
    }
}

#[test]
fn unreadable_source_reports_error() {
    let source = MboxSource::new("/nonexistent/path/inbox.mbox");
    assert!(source.collect().is_err());
}

#[test]
fn multipart_base64_body_is_decoded_and_searchable() {
    let base64_body = "SGVsbG8gZnJvbSBtdWx0aXBhcnQgYmFzZTY0IHdvcmxkLg=="; // "Hello from multipart base64 world."
    let mbox = format!(
        "\
From dave@example.com Sat Nov  1 12:00:00 2026
From: Dave Multer <dave@example.com>
To: Bob <bob@example.com>
Subject: Multipart base64
Date: Sat, 1 Nov 2026 12:00:00 +0100
Message-ID: <mp-b64@example.com>
MIME-Version: 1.0
Content-Type: multipart/alternative; boundary=\"BOUNDARY\"

--BOUNDARY
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: base64

{base64_body}
--BOUNDARY
Content-Type: text/html; charset=utf-8
Content-Transfer-Encoding: quoted-printable

<p>Hello from multipart base64 world.</p>
--BOUNDARY--
"
    );
    let collected = vault_core::mbox::parse_mbox(mbox.as_bytes(), "inbox").unwrap();
    assert_eq!(collected.failed, 0);
    assert_eq!(collected.messages.len(), 1);
    let msg = &collected.messages[0];
    assert_eq!(
        msg.body,
        "Multipart base64\n\nHello from multipart base64 world."
    );

    // End-to-end: the decoded text must be full-text searchable.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("multipart.mbox");
    std::fs::write(&path, mbox).unwrap();
    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &MboxSource::new(&path)).unwrap();
    let hits = storage
        .search(&build_match_query("multipart base64 world"), 10)
        .unwrap();
    assert!(!hits.is_empty(), "decoded base64 body must be searchable");
}

#[test]
fn nonmime_8bit_body_is_stored_readable_and_searchable() {
    // No MIME-Version/Content-Type: mailparse would otherwise decode the
    // raw 8-bit body bytes as Latin-1 ("Ključ" → "KljuÄč" mojibake).
    let mbox_utf8 = "\
From kruno@example.com Sat Nov  1 15:00:00 2026
From: Kruno Test <kruno@example.com>
To: Bob <bob@example.com>
Subject: Test mail
Date: Sat, 1 Nov 2026 15:00:00 +0100
Message-ID: <nm8-utf8@example.com>

Ključ na premijeri je spreman.
";
    let collected = vault_core::mbox::parse_mbox(mbox_utf8.as_bytes(), "inbox").unwrap();
    assert_eq!(collected.failed, 0);
    assert_eq!(collected.messages.len(), 1);
    let msg = &collected.messages[0];
    assert!(
        msg.body.contains("Ključ na premijeri je spreman."),
        "UTF-8 body without declared charset must not become mojibake, got: {:?}",
        msg.body
    );

    // Legacy true-Latin-1 bytes (0xE9 = 'é') keep decoding correctly via
    // the fallback instead of turning into invalid UTF-8 replacement chars.
    let mut mbox_latin1 = mbox_utf8.as_bytes().to_vec();
    // Replace "č" (bytes c4 8d) with the single Latin-1 byte e9.
    let kljuc_pos = mbox_latin1
        .windows(2)
        .position(|w| w == [0xc4, 0x8d])
        .unwrap();
    mbox_latin1.splice(kljuc_pos..kljuc_pos + 2, [0xe9]);
    let collected = vault_core::mbox::parse_mbox(&mbox_latin1, "inbox").unwrap();
    assert_eq!(collected.messages.len(), 1);
    assert!(
        collected.messages[0]
            .body
            .contains("Kljué na premijeri je spreman."),
        "Latin-1 0xE9 must decode to 'é', got: {:?}",
        collected.messages[0].body
    );

    // End-to-end: the diacritic token must be full-text searchable.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nonmime.mbox");
    std::fs::write(&path, mbox_utf8).unwrap();
    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &MboxSource::new(&path)).unwrap();
    for term in ["premijeri", "spreman", "Ključ"] {
        let hits = storage.search(&build_match_query(term), 10).unwrap();
        assert!(!hits.is_empty(), "no hits for {term:?}");
    }
}

#[test]
fn unparseable_message_is_counted_not_dropped() {
    // mailparse rejects a header block whose first line is a bare
    // continuation line ("Header cannot start with a space"), which is what
    // a truncated or corrupted mail looks like after an mbox split.
    let mbox = format!(
        "\
{SAMPLE_MBOX}
From mallory@example.com Sat Nov  1 13:00:00 2026
   continued from a torn header line
Subject: never parses
"
    );
    let collected = vault_core::mbox::parse_mbox(mbox.as_bytes(), "inbox").unwrap();
    assert_eq!(collected.failed, 1);
    assert_eq!(collected.messages.len(), 1);
    assert_eq!(collected.messages[0].external_id, "kickoff-123@example.com");

    // Ingest reports the failure in the summary; the good mail still stores.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.mbox");
    std::fs::write(&path, mbox).unwrap();
    let storage = Storage::open_in_memory().unwrap();
    let summary = ingest(&storage, &MboxSource::new(&path)).unwrap();
    assert_eq!(
        summary,
        IngestSummary {
            source: "mbox".into(),
            seen: 2,
            inserted: 1,
            skipped: 0,
            failed: 1
        }
    );
}

#[test]
fn encoded_words_in_headers_are_decoded_for_storage_but_kept_raw_in_raw_json() {
    let mbox = "\
From ana@example.com Sat Nov  1 14:00:00 2026
From: =?UTF-8?B?QW5hIMWgaW1pYw==?= <ana@example.com>
To: Bob <bob@example.com>
Subject: =?UTF-8?Q?=C5=A0eta_kroz_arhivu?=
Date: Sat, 1 Nov 2026 14:00:00 +0100
Message-ID: <enc-1@example.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=utf-8

Ključ na premijeri.
";
    let collected = vault_core::mbox::parse_mbox(mbox.as_bytes(), "inbox").unwrap();
    assert_eq!(collected.failed, 0);
    assert_eq!(collected.messages.len(), 1);
    let msg = &collected.messages[0];
    // B-encoded display name and Q-encoded subject are decoded.
    assert_eq!(msg.author, "Ana Šimic <ana@example.com>");
    assert!(msg
        .body
        .starts_with("Šeta kroz arhivu\n\nKljuč na premijeri."));
    // raw_json keeps the same header values as storage (decoded header text).
    let raw: serde_json::Value = serde_json::from_str(&msg.raw_json).unwrap();
    assert_eq!(raw["from"], "Ana Šimic <ana@example.com>");
    assert_eq!(raw["subject"], "Šeta kroz arhivu");

    // Decoded text must be full-text searchable.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("encoded.mbox");
    std::fs::write(&path, mbox).unwrap();
    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &MboxSource::new(&path)).unwrap();
    for term in ["premijeri", "Šeta"] {
        let hits = storage.search(&build_match_query(term), 10).unwrap();
        assert!(!hits.is_empty(), "no hits for {term:?}");
    }
}

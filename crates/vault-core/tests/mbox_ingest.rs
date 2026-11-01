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
    let messages = vault_core::mbox::parse_mbox(two_message_mbox().as_bytes(), "inbox").unwrap();
    assert_eq!(messages.len(), 2);

    let first = &messages[0];
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
    let second_a = &a[1];
    let second_b = &b[1];
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
            skipped: 0
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

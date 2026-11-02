//! Integration test: the same fixture is ingested twice — once through the
//! `.mbox` file source and once through the IMAP sync path (mock server fed
//! with the fixture's raw messages) — and both archives must agree.

mod imap_mock;

use imap_mock::{MockImap, MockMailbox};
use vault_core::imap as vimap;
use vault_core::ingest::ingest;
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
";

const THIRD_MESSAGE: &str = "\
From dave@example.com Sat Nov  1 12:00:00 2026
From: Dave Multer <dave@example.com>
To: Bob <bob@example.com>
Subject: Base64 in transit
Date: Sat, 1 Nov 2026 12:00:00 +0100
Message-ID: <b64-9@example.com>
MIME-Version: 1.0
Content-Type: text/plain; charset=utf-8
Content-Transfer-Encoding: base64

U2Vjb25kIGJvZHkgdmlhIGJhc2U2NC4=
";

fn fixture() -> String {
    format!("{SAMPLE_MBOX}\n{SECOND_MESSAGE_NO_ID}\n{THIRD_MESSAGE}")
}

/// Split mbox bytes into raw messages, dropping the `From ` separator
/// lines — the inverse of what the mbox reader does before parsing, and
/// exactly what an IMAP server would hand out for the same mails.
fn raw_mails(mbox: &[u8]) -> Vec<Vec<u8>> {
    let bytes = mbox;
    let mut current: Vec<u8> = Vec::new();
    let mut mails = Vec::new();
    let mut at_line_start = true;
    let mut i = 0;
    while i < bytes.len() {
        if at_line_start && bytes[i..].starts_with(b"From ") && !current.is_empty() {
            mails.push(std::mem::take(&mut current));
        }
        at_line_start = bytes[i] == b'\n';
        current.push(bytes[i]);
        i += 1;
    }
    if !current.is_empty() {
        mails.push(current);
    }
    // Strip each mail's leading separator line (through its newline).
    mails
        .into_iter()
        .map(|mut m| {
            if let Some(nl) = m.iter().position(|&b| b == b'\n') {
                m.drain(..=nl);
            }
            m
        })
        .collect()
}

fn message_by_id(
    storage: &Storage,
    source: &str,
    external_id: &str,
) -> vault_core::message::Message {
    let all = storage
        .search_filtered(
            &build_match_query(&unique_term(storage, external_id)),
            None,
            None,
            50,
        )
        .unwrap();
    all.into_iter()
        .find(|m| m.source == source && m.external_id == external_id)
        .unwrap_or_else(|| panic!("message {external_id} not found under {source}"))
}

/// A unique search term guaranteed to hit only the fixture mails: every
/// fixture subject is unique, so use the subject instead of ids (FTS only
/// indexes body/author).
fn unique_term(_storage: &Storage, external_id: &str) -> String {
    match external_id {
        "kickoff-123@example.com" => "kickoff".to_string(),
        "b64-9@example.com" => "base64".to_string(),
        other => panic!("unexpected id {other}"),
    }
}

#[test]
fn mbox_fixture_and_imap_sync_produce_equivalent_archives() {
    let mbox = fixture();

    // Path A: .mbox file import.
    let dir = tempfile::tempdir().unwrap();
    let mbox_path = dir.path().join("inbox.mbox");
    std::fs::write(&mbox_path, &mbox).unwrap();
    let mbox_storage = Storage::open_in_memory().unwrap();
    let summary = ingest(&mbox_storage, &MboxSource::new(&mbox_path)).unwrap();
    assert_eq!(summary.source, "mbox");
    assert_eq!(summary.inserted, 3);

    // Path B: the same raw mails served over IMAP.
    let server = MockImap::start();
    server.set_mailbox("INBOX", MockMailbox::new(7, raw_mails(mbox.as_bytes())));
    let imap_storage = Storage::open_in_memory().unwrap();
    let config = vimap::ImapConfig {
        host: "mock.test".to_string(),
        port: 993,
        username: "alice@example.com".to_string(),
        password: "pw".to_string(),
        folders: Vec::new(),
    };
    let instance = vimap::ensure_source_instance(&imap_storage, &config).unwrap();
    let label = config.display_label();
    let folders = vec!["INBOX".to_string()];
    let mut s = {
        let stream = std::net::TcpStream::connect(server.addr).unwrap();
        let mut client = imap::Client::new(stream);
        client.read_greeting().unwrap();
        client
            .login("alice@example.com", "pw")
            .map_err(|(e, _)| e)
            .unwrap()
    };
    let summaries = vimap::sync_session(&mut s, &imap_storage, &instance, &folders);
    let _ = s.logout();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].inserted, 3);
    assert_eq!(summaries[0].failed, 0);

    // Same archive size.
    assert_eq!(mbox_storage.stats().unwrap().total, 3);
    assert_eq!(imap_storage.stats().unwrap().total, 3);

    // Mails with a Message-ID share the same external_id and identical
    // stored fields across both ingest paths.
    for id in ["kickoff-123@example.com", "b64-9@example.com"] {
        let a = message_by_id(&mbox_storage, "mbox", id);
        let b = message_by_id(&imap_storage, &label, id);
        assert_eq!(a.author, b.author, "{id}: author mismatch");
        assert_eq!(a.sent_at, b.sent_at, "{id}: sent_at mismatch");
        assert_eq!(
            a.body, b.body,
            "{id}: body mismatch:\n  mbox: {:?}\n  imap: {:?}",
            a.body, b.body
        );
        assert_eq!(a.external_id, b.external_id);
    }

    // The base64 transfer-encoded body decoded identically.
    let b64 = message_by_id(&imap_storage, &label, "b64-9@example.com");
    assert!(b64.body.contains("Second body via base64."));

    // The Message-ID-less mail: both paths derive a stable id from the
    // exact same raw RFC822 bytes (the mbox separator line is stripped
    // before hashing) — the mbox path keeps its legacy fnv1a id, the
    // connector path uses the contract's SHA-256 fallback key, and each
    // dedups on re-ingest within its own namespace.
    let generated = |storage: &Storage| {
        storage
            .search_filtered(&build_match_query("Sorry I missed"), None, None, 10)
            .unwrap()
            .into_iter()
            .find(|m| m.external_id != "kickoff-123@example.com")
            .expect("id-less message")
    };
    let a = generated(&mbox_storage);
    let b = generated(&imap_storage);
    assert!(
        a.external_id.starts_with("generated-"),
        "legacy path keeps the generated- fallback id"
    );
    assert_eq!(
        b.external_id.len(),
        64,
        "connector fallback key is sha256 hex"
    );
    assert_eq!(a.author, b.author);
    assert_eq!(a.body, b.body);

    // Both archives full-text-search identically for fixture terms.
    for term in ["kickoff", "FTS notes", "base64"] {
        assert!(
            !mbox_storage
                .search_filtered(&build_match_query(term), None, None, 10)
                .unwrap()
                .is_empty(),
            "mbox: no hits for {term}"
        );
        assert!(
            !imap_storage
                .search_filtered(&build_match_query(term), None, None, 10)
                .unwrap()
                .is_empty(),
            "imap: no hits for {term}"
        );
    }
}

//! Dedup guarantees, within a source and across sources. The dedup key is
//! `(source, external_id)`: re-importing the same file never duplicates, and
//! the same external id under different sources is *not* a collision — each
//! source keeps its own namespace, so the same human message arriving from
//! mbox and from WhatsApp is intentionally stored once per source.

use std::fs;
use vault_core::ingest::ingest;
use vault_core::mbox::MboxSource;
use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::storage::Storage;
use vault_core::whatsapp::WhatsappSource;

const SAMPLE_MBOX: &str = "\
From alice@example.com Sat Nov  1 10:00:00 2026
From: Alice Smith <alice@example.com>
To: Bob <bob@example.com>
Subject: Vault kickoff meeting
Date: Sat, 1 Nov 2026 10:00:00 +0100
Message-ID: <kickoff-123@example.com>

Hi Bob,

the vault kickoff is confirmed for Monday.
";

const SECOND_MAIL_NO_ID: &str = "\
From carol@example.com Sat Nov  1 11:30:00 2026
From: Carol Jones <carol@example.com>
To: Bob <bob@example.com>
Subject: Re: Vault kickoff meeting
Date: Sat, 1 Nov 2026 11:30:00 +0100

Sorry I missed it — the same words in both channels.
";

const WHATSAPP_EXPORT: &str = "\
[1.11.2026., 09:00:00] Marko Horvat: Sorry I missed it — the same words in both channels.
[1.11.2026., 09:05:00] Marko Horvat: ok
[1.11.2026., 09:05:00] Marko Horvat: ok
";

#[test]
fn reimporting_the_same_mbox_file_never_duplicates() {
    let dir = crate::util::scratch_dir();
    let path = dir.path().join("inbox.mbox");
    fs::write(&path, format!("{SAMPLE_MBOX}\n{SECOND_MAIL_NO_ID}")).unwrap();

    let s = Storage::open(dir.path().join("v.db")).unwrap();
    let first = ingest(&s, &MboxSource::new(&path)).unwrap();
    assert_eq!(first.inserted, 2);
    assert_eq!(first.skipped, 0);

    let second = ingest(&s, &MboxSource::new(&path)).unwrap();
    assert_eq!(second.inserted, 0);
    assert_eq!(second.skipped, 2);
    assert_eq!(second.seen, first.seen);

    assert_eq!(s.stats().unwrap().total, 2);
    // Stable external ids (Message-ID, or a deterministic content hash when
    // absent) are what make re-imports idempotent.
    let hits = s.search("kickoff", 10).unwrap();
    assert_eq!(hits.len(), 2);
}

#[test]
fn reimporting_the_same_whatsapp_export_never_duplicates() {
    let dir = crate::util::scratch_dir();
    let path = dir.path().join("Arhiva ekipa.txt");
    fs::write(&path, WHATSAPP_EXPORT).unwrap();

    let s = Storage::open(dir.path().join("v.db")).unwrap();
    let first = ingest(&s, &WhatsappSource::new(&path)).unwrap();
    // Three rows: the identical back-to-back "ok" lines stay distinct via
    // the occurrence counter.
    assert_eq!(first.inserted, 3);

    let second = ingest(&s, &WhatsappSource::new(&path)).unwrap();
    assert_eq!(second.inserted, 0);
    assert_eq!(second.skipped, 3);

    assert_eq!(s.stats().unwrap().total, 3);
}

#[test]
fn same_message_from_mbox_and_whatsapp_lives_once_per_source() {
    // "Sorry I missed it — the same words in both channels." arrives as a
    // mail (Carol) and as a WhatsApp line (Marko). Both are kept, one row
    // per source namespace; neither overwrites nor suppresses the other.
    let dir = crate::util::scratch_dir();
    let mbox_path = dir.path().join("inbox.mbox");
    fs::write(&mbox_path, format!("{SAMPLE_MBOX}\n{SECOND_MAIL_NO_ID}")).unwrap();
    let wa_path = dir.path().join("chat.txt");
    fs::write(&wa_path, WHATSAPP_EXPORT).unwrap();

    let s = Storage::open(dir.path().join("v.db")).unwrap();
    let first = ingest(&s, &MboxSource::new(&mbox_path)).unwrap();
    assert_eq!(first.inserted, 2);
    ingest(&s, &WhatsappSource::new(&wa_path)).unwrap();

    let stats = s.stats().unwrap();
    assert_eq!(stats.total, 5, "2 mails + 3 chat rows");
    let by_source: Vec<(&str, u64)> = stats
        .by_source
        .iter()
        .map(|c| (c.source.as_str(), c.count))
        .collect();
    assert!(by_source.contains(&("mbox", 2)));
    assert!(by_source.contains(&("whatsapp", 3)));

    // The shared sentence is searchable in both archives' rows.
    let hits = s.search("\"same words in both channels\"", 10).unwrap();
    assert_eq!(hits.len(), 2, "one hit per source namespace");
    let mut sources: Vec<&str> = hits.iter().map(|m| m.source.as_str()).collect();
    sources.sort_unstable();
    assert_eq!(sources, ["mbox", "whatsapp"]);

    // Re-importing both files again adds nothing.
    ingest(&s, &MboxSource::new(&mbox_path)).unwrap();
    ingest(&s, &WhatsappSource::new(&wa_path)).unwrap();
    assert_eq!(s.stats().unwrap().total, 5);
}

#[test]
fn identical_external_ids_across_sources_do_not_collide() {
    let s = Storage::open_in_memory().unwrap();
    let mbox_msg = NewMessage::new(
        "mbox",
        "shared-id",
        "ana",
        "2026-01-01T00:00:00Z",
        "from mbox",
    );
    let wa_msg = NewMessage::new(
        "whatsapp",
        "shared-id",
        "ana",
        "2026-01-01T00:00:00Z",
        "from whatsapp",
    );

    let id_mbox = match s.insert_message(&mbox_msg).unwrap() {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("mbox row must insert"),
    };
    let id_wa = match s.insert_message(&wa_msg).unwrap() {
        InsertOutcome::Inserted(id) => id,
        _ => panic!("whatsapp row must insert despite the shared external_id"),
    };
    assert_ne!(id_mbox, id_wa);

    // The dedup key is (source, external_id): each namespaced copy dedups
    // against itself only.
    assert!(matches!(
        s.insert_message(&mbox_msg).unwrap(),
        InsertOutcome::Duplicate(id) if id == id_mbox
    ));
    assert!(matches!(
        s.insert_message(&wa_msg).unwrap(),
        InsertOutcome::Duplicate(id) if id == id_wa
    ));

    assert_eq!(s.stats().unwrap().total, 2);
    assert_eq!(s.search("mbox", 10).unwrap().len(), 1);
    assert_eq!(s.search("whatsapp", 10).unwrap().len(), 1);
}

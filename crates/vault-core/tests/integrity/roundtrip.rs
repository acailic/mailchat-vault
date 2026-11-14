//! Export/import round-trips. The archive's dedicated export lives in
//! `vault_core::export` (JSON Lines over the same `Message` serde shape,
//! covered by its own suite); this integrity test rounds trips at the same
//! level the app itself stores rows: JSON-serialized `Message`s re-imported
//! into a fresh database through the public `insert_message` path. Every
//! field — including `raw_json` payloads and adversarial text — must come
//! back bit-identical, and the rebuilt FTS index must answer identically.

use vault_core::message::{InsertOutcome, Message, NewMessage};
use vault_core::storage::Storage;

use crate::util::{all_messages, fts_integrity_check, integrity_check};

/// A `Message` row reduced to the ingest fields of a `NewMessage`; `id` and
/// `created_at` are re-established by the import.
fn to_new(m: &Message) -> NewMessage {
    NewMessage {
        source: m.source.clone(),
        external_id: m.external_id.clone(),
        channel: m.channel.clone(),
        chat_id: m.chat_id.clone(),
        author: m.author.clone(),
        sent_at: m.sent_at.clone(),
        body: m.body.clone(),
        raw_json: m.raw_json.clone(),
        ..NewMessage::new("", "", "", "", "")
    }
}

/// Adversarial corpus: quoting, newlines, emoji, bidi marks, control
/// characters, embedded NULs, very long bodies, empty fields, and
/// structured `raw_json` that must survive verbatim.
fn corpus() -> Vec<NewMessage> {
    let mut msgs = vec![
        NewMessage::new(
            "mbox",
            "plain-1@example.com",
            "Alice Smith <alice@example.com>",
            "2026-11-01T09:00:00Z",
            "Vault kickoff meeting\n\nHi Bob,\n\nthe vault kickoff is confirmed. \"Bring\" the FTS notes.",
        ),
        NewMessage::new(
            "whatsapp",
            "generated-00000000deadbeef-0",
            "Marko Horvat",
            "2026-11-01T09:00:05Z",
            "Zdravo ekipa! 👋 Arhiva je gotova 🎉 čćžšđ \u{200e}bidi",
        ),
        NewMessage::new("mbox", "ctrl", "a", "2026-01-02T03:04:05Z", "tab\there\nCR\rreturn\u{1}unit\x00nul"),
        NewMessage::new("telegram", "empty-fields", "", "", ""),
        NewMessage::new(
            "demo",
            "long-body",
            "long author name ".repeat(50).trim_end().to_string(),
            "2026-06-30T23:59:59Z",
            format!("{} terminator", "paragraph\n\n".repeat(2_000)),
        ),
    ];
    // Structured extras must round-trip without re-encoding.
    let raw = msgs.last_mut().unwrap();
    raw.raw_json = r#"{"quoted_author":"Mom","quoted_text":"morning coffee ☕","nested":{"list":[1,2,3],"ok":true}}"#.to_string();
    raw.chat_id = "Arhiva ekipa".to_string();
    raw.channel = "chat".to_string();
    msgs
}

/// Export every row as JSON strings (the transport), import them into a
/// fresh database, and return both databases for comparison.
fn round_trip(source: &std::path::Path) -> std::path::PathBuf {
    let exported: Vec<String> = all_messages(source)
        .iter()
        .map(|m| serde_json::to_string(m).expect("serialize message"))
        .collect();

    let target = source.with_extension("imported.db");
    {
        let s = Storage::open(&target).unwrap();
        for line in &exported {
            let m: Message = serde_json::from_str(line).expect("deserialize message");
            let out = s.insert_message(&to_new(&m)).unwrap();
            assert!(
                matches!(out, InsertOutcome::Inserted(_)),
                "import must insert every exported row exactly once"
            );
        }
    }
    target
}

#[test]
fn export_import_round_trip_is_bit_identical() {
    let dir = crate::util::scratch_dir();
    let original = dir.path().join("v.db");
    {
        let s = Storage::open(&original).unwrap();
        for m in corpus() {
            s.insert_message(&m).unwrap();
        }
    }

    let imported = round_trip(&original);

    // Row-for-row identity, including ids (same insertion order) and every
    // text field, `raw_json` included.
    let before = all_messages(&original);
    let after = all_messages(&imported);
    assert_eq!(before.len(), after.len());
    assert_eq!(before, after, "imported rows must be bit-identical");

    // The imported archive is a fully working archive, not just equal data.
    assert_eq!(integrity_check(&imported).as_deref(), Some("ok"));
    assert!(fts_integrity_check(&imported).is_ok());

    // Search behavior is identical for a spread of probes.
    let a = Storage::open(&original).unwrap();
    let b = Storage::open(&imported).unwrap();
    for term in ["vault", "arhiva", "čćžšđ", "terminator", "morning"] {
        let ids_a: Vec<String> = a
            .search_filtered(term, None, None, None, false, 100)
            .unwrap()
            .into_iter()
            .map(|m| m.external_id)
            .collect();
        let ids_b: Vec<String> = b
            .search_filtered(term, None, None, None, false, 100)
            .unwrap()
            .into_iter()
            .map(|m| m.external_id)
            .collect();
        let mut sa = ids_a.clone();
        let mut sb = ids_b.clone();
        sa.sort();
        sb.sort();
        assert_eq!(
            sa, sb,
            "term {term:?} must hit the same rows in both archives"
        );
    }
}

#[test]
fn round_trip_is_idempotent_across_generations() {
    // Exporting an imported archive and importing it again changes nothing —
    // the archive is a fixed point of the round trip.
    let dir = crate::util::scratch_dir();
    let gen1 = dir.path().join("v.db");
    {
        let s = Storage::open(&gen1).unwrap();
        for m in corpus() {
            s.insert_message(&m).unwrap();
        }
    }
    let gen2 = round_trip(&gen1);
    let gen3 = round_trip(&gen2);
    assert_eq!(all_messages(&gen2), all_messages(&gen3));

    // Re-importing the *same* export into the already-populated archive is
    // a no-op thanks to dedup, not a duplication.
    let lines: Vec<String> = all_messages(&gen1)
        .iter()
        .map(|m| serde_json::to_string(m).unwrap())
        .collect();
    let s = Storage::open(&gen2).unwrap();
    for line in &lines {
        let m: Message = serde_json::from_str(line).unwrap();
        let out = s.insert_message(&to_new(&m)).unwrap();
        assert!(matches!(out, InsertOutcome::Duplicate(_)));
    }
    assert_eq!(
        all_messages(&gen2).len(),
        all_messages(&gen1).len(),
        "double import must not add rows"
    );
}

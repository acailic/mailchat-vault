use vault_core::ingest::{ingest, IngestSummary, Source};
use vault_core::search::build_match_query;
use vault_core::storage::Storage;
use vault_core::whatsapp::{parse_whatsapp, WhatsappSource, WHATSAPP_SOURCE};

// Croatian-style export: dd.mm.yyyy with a trailing year dot, 24h clock,
// unicode names and emoji, a system line, a multiline message, a quoted
// reply with the localized marker, attachment placeholders, and bidi
// marks (\u{200e}) in front of WhatsApp-generated lines.
const HR_EXPORT: &str = "\
[1.11.2026., 09:00:00] Poruke i pozivi šifrirani su presretanjem na WhatsAppu. Nitko izvan ovog chata ne može ih čitati.\n\
[1.11.2026., 09:00:05] Marko Horvat: Zdravo ekipa! 👋 Arhiva je gotova, FTS pretraga radi 🎉\n\
[1.11.2026., 09:01:00] Jelena Babić: Odlično Marko!\n\
Odmah testiram pretragu\n\
[1.11.2026., 09:02:00] Marko Horvat: \u{200e}[Citirana poruka]\n\
Jelena Babić: Odmah testiram pretragu\n\
super, hvala!\n\
[1.11.2026., 09:03:00] Jelena Babić: \u{200e}image omitted\n\
[1.11.2026., 09:04:00] Marko Horvat: IMG-20261101-WA0001.jpg (file attached)\n\
[1.11.2026., 09:05:00] Marko Horvat: ok\n\
[1.11.2026., 09:05:00] Marko Horvat: ok\n\
";

// US-style export: mm/dd/yy, 12h clock, English quoted marker echoed with
// its full header line, and "<Media omitted>".
const US_EXPORT: &str = "\
[6/28/23, 9:30:15 AM] Mom: morning coffee ☕\n\
[6/28/23, 9:31:00 AM] Dad: \u{200e}[Quoted Message]\n\
[6/28/23, 9:30:15 AM] Mom: morning coffee ☕\n\
on my way\n\
[6/28/23, 2:15:03 PM] Mom: \u{200e}<Media omitted>\n\
";

// Old Android export: no brackets, " - " separator, no seconds.
const OLD_ANDROID_EXPORT: &str = "\
25.12.26, 19:04 - Marko: sretni blagdani 🎄\n\
25.12.26, 19:05 - Jelena Babić: hvala, i tebi!\n\
25.12.26, 19:06 - Marko je dodao Anu Babić\n\
";

#[test]
fn parses_hr_export_fields() {
    let collected = parse_whatsapp(HR_EXPORT.as_bytes(), "Arhiva ekipa").unwrap();
    assert_eq!(collected.failed, 0);
    assert_eq!(collected.messages.len(), 8);

    // System line: no author, flagged in raw_json.
    let system = &collected.messages[0];
    assert_eq!(system.source, WHATSAPP_SOURCE);
    assert_eq!(system.channel, "chat");
    assert_eq!(system.chat_id, "Arhiva ekipa");
    assert_eq!(system.author, "");
    assert!(system.body.starts_with("Poruke i pozivi"));
    let raw: serde_json::Value = serde_json::from_str(&system.raw_json).unwrap();
    assert_eq!(raw["system"], true);

    // Regular message: unicode author, emoji body, UTC-normalized stamp.
    let marko = &collected.messages[1];
    assert_eq!(marko.author, "Marko Horvat");
    assert_eq!(marko.sent_at, "2026-11-01T09:00:05Z");
    assert!(marko.body.contains("FTS pretraga radi 🎉"));
    assert!(marko.external_id.starts_with("generated-"));

    // Multiline continuation lines join into one body.
    let multiline = &collected.messages[2];
    assert_eq!(multiline.body, "Odlično Marko!\nOdmah testiram pretragu");

    // Quoted reply: reply body only, reference recorded in raw_json.
    let quoted = &collected.messages[3];
    assert_eq!(quoted.body, "super, hvala!");
    let raw: serde_json::Value = serde_json::from_str(&quoted.raw_json).unwrap();
    assert_eq!(raw["quoted_author"], "Jelena Babić");
    assert_eq!(raw["quoted_text"], "Odmah testiram pretragu");

    // Attachments: placeholders kept as body, kind/name in raw_json.
    let image = &collected.messages[4];
    assert_eq!(image.body, "image omitted");
    let raw: serde_json::Value = serde_json::from_str(&image.raw_json).unwrap();
    assert_eq!(raw["attachment"]["kind"], "image");
    let file = &collected.messages[5];
    let raw: serde_json::Value = serde_json::from_str(&file.raw_json).unwrap();
    assert_eq!(raw["attachment"]["kind"], "file");
    assert_eq!(raw["attachment"]["name"], "IMG-20261101-WA0001.jpg");

    // Identical back-to-back messages stay two rows with distinct ids.
    assert_ne!(
        collected.messages[6].external_id,
        collected.messages[7].external_id
    );
}

#[test]
fn parses_us_export_month_first_12h_and_media() {
    // 28 in the month position proves mm/dd; times are 12h.
    let collected = parse_whatsapp(US_EXPORT.as_bytes(), "family").unwrap();
    assert_eq!(collected.messages.len(), 3);

    let mom = &collected.messages[0];
    assert_eq!(mom.sent_at, "2023-06-28T09:30:15Z");
    assert!(mom.body.contains("morning coffee"));

    // 2:15:03 PM is 14:15:03.
    let media = &collected.messages[2];
    assert_eq!(media.sent_at, "2023-06-28T14:15:03Z");
    let raw: serde_json::Value = serde_json::from_str(&media.raw_json).unwrap();
    assert_eq!(raw["attachment"]["kind"], "media");
}

#[test]
fn parses_old_android_hyphen_export() {
    let collected = parse_whatsapp(OLD_ANDROID_EXPORT.as_bytes(), "blagdani").unwrap();
    assert_eq!(collected.messages.len(), 3);

    let first = &collected.messages[0];
    assert_eq!(first.author, "Marko");
    assert_eq!(first.sent_at, "2026-12-25T19:04:00Z");
    assert!(first.body.contains("🎄"));

    // A hyphen-form system line (no "Author: " part) has no author.
    let system = &collected.messages[2];
    assert_eq!(system.author, "");
    assert_eq!(system.body, "Marko je dodao Anu Babić");
    let raw: serde_json::Value = serde_json::from_str(&system.raw_json).unwrap();
    assert_eq!(raw["system"], true);
}

#[test]
fn day_above_12_disambiguates_day_first() {
    // 25 cannot be a month, so this is dd/mm even though a US reader
    // might expect mm/dd.
    let export = "[25/01/26, 08:00:00] Ana: provjera\n";
    let collected = parse_whatsapp(export.as_bytes(), "c").unwrap();
    assert_eq!(collected.messages[0].sent_at, "2026-01-25T08:00:00Z");
}

#[test]
fn iso_stamp_parses() {
    let export = "[2026-11-01, 10:00] Ana: iso provjera\n";
    let collected = parse_whatsapp(export.as_bytes(), "c").unwrap();
    assert_eq!(collected.messages[0].sent_at, "2026-11-01T10:00:00Z");
}

#[test]
fn reimport_of_same_file_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Arhiva ekipa.txt");
    std::fs::write(&path, HR_EXPORT).unwrap();

    let storage = Storage::open_in_memory().unwrap();
    let source = WhatsappSource::new(&path);
    assert_eq!(source.name(), WHATSAPP_SOURCE);

    let summary = ingest(&storage, &source).unwrap();
    assert_eq!(
        summary,
        IngestSummary {
            source: WHATSAPP_SOURCE.into(),
            seen: 8,
            inserted: 8,
            skipped: 0,
            failed: 0
        }
    );
    assert_eq!(storage.stats().unwrap().total, 8);

    // Re-importing the identical file dedups everything.
    let again = ingest(&storage, &source).unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.skipped, 8);
    assert_eq!(storage.stats().unwrap().total, 8);

    // Same content under a different file name is a different chat, so it
    // stores again (chat_id is part of the content hash).
    let other = dir.path().join("Arhiva ekipa — backup.txt");
    std::fs::write(&other, HR_EXPORT).unwrap();
    let summary = ingest(&storage, &WhatsappSource::new(&other)).unwrap();
    assert_eq!(summary.inserted, 8);
    assert_eq!(storage.stats().unwrap().total, 16);
}

#[test]
fn chat_id_comes_from_file_stem() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("WhatsApp Chat with Ana.txt");
    std::fs::write(&path, "[1.11.2026., 09:00:00] Ana: pozdrav\n").unwrap();

    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &WhatsappSource::new(&path)).unwrap();
    let hits = storage.search(&build_match_query("pozdrav"), 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].chat_id, "WhatsApp Chat with Ana");
    assert_eq!(hits[0].source, WHATSAPP_SOURCE);
}

#[test]
fn imported_chat_is_full_text_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Arhiva ekipa.txt");
    std::fs::write(&path, HR_EXPORT).unwrap();

    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &WhatsappSource::new(&path)).unwrap();

    // Body content, unicode terms, and authors all hit FTS5.
    for term in ["pretragu", "šifrirani", "jelena", "arhiva"] {
        let hits = storage.search(&build_match_query(term), 10).unwrap();
        assert!(!hits.is_empty(), "no hits for {term:?}");
    }
    // The quoted reference lives in raw_json, not the body.
    assert!(storage
        .search(&build_match_query("super, hvala"), 10)
        .unwrap()
        .iter()
        .all(|m| m.body == "super, hvala!"));
}

#[test]
fn unreadable_source_reports_error() {
    let source = WhatsappSource::new("/nonexistent/path/chat.txt");
    assert!(source.collect().is_err());
}

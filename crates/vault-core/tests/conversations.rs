//! M1.3 — stable conversation keys, forward path.
//!
//! Proof over the M1.1 canonical tables: same chat names stay separated,
//! renames stay linked (one conversation, label follows, messages dedup),
//! the email reply graph groups threads safely (incomplete chains and
//! cycles never loop or mismerge), and timestamps carry explicit timezone
//! interpretation with confidence labels.

use std::path::PathBuf;

use chrono::{Datelike, FixedOffset, TimeZone};
use rusqlite::{Connection, OptionalExtension};
use vault_core::ingest::ingest;
use vault_core::mbox::MboxSource;
use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::storage::Storage;
use vault_core::telegram::TelegramSource;
use vault_core::whatsapp::{parse_whatsapp_tz, WhatsappSource};

/// Scratch database + a second raw connection for canonical-table
/// assertions (WAL allows the concurrent reader).
fn scratch(name: &str) -> (Storage, Connection, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(format!("{name}.db"));
    let storage = Storage::open(&db).unwrap();
    // Keep the tempdir alive for the test's lifetime.
    std::mem::forget(dir);
    let raw = Connection::open(&db).unwrap();
    (storage, raw, db)
}

fn write_export(_dir_name: &str, file: &str, bytes: &[u8]) -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(file);
    std::fs::write(&path, bytes).unwrap();
    std::mem::forget(dir);
    path
}

/// One conversation row: (provider_key, label, confidence).
fn conversation(raw: &Connection, provider_key: &str) -> Option<(Option<String>, String, String)> {
    raw.query_row(
        "SELECT provider_key, label, confidence FROM conversations
         WHERE provider_key = ?1",
        [provider_key],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
    .unwrap()
}

fn count_conversations(raw: &Connection) -> i64 {
    raw.query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
        .unwrap()
}

// -----------------------------------------------------------------------
// Telegram: provider-id keys
// -----------------------------------------------------------------------

const TG_BEFORE: &str = r#"{"name": "Staro ime", "id": 42, "messages": [
    {"id": 1, "type": "message", "date_unixtime": "1730432400", "from": "Ana", "text": "poruka jedan"},
    {"id": 2, "type": "message", "date_unixtime": "1730432500", "from": "Ana", "text": "poruka dva"}]}"#;
const TG_AFTER_RENAME: &str = r#"{"name": "Novo ime", "id": 42, "messages": [
    {"id": 1, "type": "message", "date_unixtime": "1730432400", "from": "Ana", "text": "poruka jedan"},
    {"id": 2, "type": "message", "date_unixtime": "1730432500", "from": "Ana", "text": "poruka dva"},
    {"id": 3, "type": "message", "date_unixtime": "1730432600", "from": "Ana", "text": "poruka tri"}]}"#;
const TG_OTHER_CHAT_SAME_NAME: &str = r#"{"name": "Staro ime", "id": 77, "messages": [
    {"id": 1, "type": "message", "date_unixtime": "1730432400", "from": "Bane", "text": "drugi chat"}]}"#;

#[test]
fn telegram_rename_stays_one_conversation_and_label_follows() {
    let (storage, raw, _) = scratch("tg-rename");
    ingest(
        &storage,
        &TelegramSource::new(write_export("a", "result.json", TG_BEFORE.as_bytes())),
    )
    .unwrap();
    ingest(
        &storage,
        &TelegramSource::new(write_export("b", "result.json", TG_AFTER_RENAME.as_bytes())),
    )
    .unwrap();

    // One conversation, keyed by the provider id, label following the
    // newest export name.
    let conv = conversation(&raw, "telegram:chat:42").expect("provider-keyed conversation");
    assert_eq!(conv.1, "Novo ime", "the label follows the rename");
    assert_eq!(conv.2, "verified");
    assert_eq!(count_conversations(&raw), 1);
    // The re-export dedups the shared messages and adds only the new one.
    assert_eq!(storage.stats().unwrap().total, 3);

    // Thread context spans the whole chat, across both imports.
    let hits = storage
        .search_filtered("\"poruka\"", None, None, None, false, 10)
        .unwrap();
    assert_eq!(hits.len(), 3);
    let ctx = storage.chat_context(hits[1].id, 8).unwrap().unwrap();
    assert_eq!(ctx.before.len() + ctx.after.len(), 2);
}

#[test]
fn telegram_same_name_different_ids_stay_separated() {
    let (storage, raw, _) = scratch("tg-samename");
    ingest(
        &storage,
        &TelegramSource::new(write_export("a", "result.json", TG_BEFORE.as_bytes())),
    )
    .unwrap();
    ingest(
        &storage,
        &TelegramSource::new(write_export(
            "b",
            "result.json",
            TG_OTHER_CHAT_SAME_NAME.as_bytes(),
        )),
    )
    .unwrap();

    // Two distinct chats share the display name: the provider keys keep
    // them two conversations.
    assert!(conversation(&raw, "telegram:chat:42").is_some());
    assert!(conversation(&raw, "telegram:chat:77").is_some());
    assert_eq!(count_conversations(&raw), 2);
    // And their messages never mix: the other chat's message has no peers.
    let hit = storage
        .search_filtered("\"drugi chat\"", None, None, None, false, 10)
        .unwrap()[0]
        .id;
    let ctx = storage.chat_context(hit, 8).unwrap().unwrap();
    assert!(ctx.before.is_empty() && ctx.after.is_empty());
}

#[test]
fn telegram_export_without_id_stays_legacy_unresolved() {
    let (storage, raw, _) = scratch("tg-noid");
    let export = r#"{"name": "Bez id", "messages": [
        {"id": 1, "type": "message", "date_unixtime": "1730432400", "from": "Ana", "text": "poruka"}]}"#;
    ingest(
        &storage,
        &TelegramSource::new(write_export("a", "result.json", export.as_bytes())),
    )
    .unwrap();
    assert_eq!(count_conversations(&raw), 1);
    let (key, _label, confidence): (Option<String>, String, String) = raw
        .query_row(
            "SELECT provider_key, label, confidence FROM conversations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(key.is_none(), "nothing in the export can be keyed safely");
    assert_eq!(confidence, "legacy_unresolved");
}

// -----------------------------------------------------------------------
// WhatsApp: content-derived keys
// -----------------------------------------------------------------------

const WA_EXPORT: &str = "\
[1.11.2026., 09:00:00] Poruke i pozivi šifrirani.\n\
[1.11.2026., 09:00:05] Ana: zdravo ekipo\n\
[1.11.2026., 09:01:00] Ana: kako ste?\n\
";
// Same chat, same participants, same first authored message — a pure
// rename of the export file.
const WA_EXPORT_LATER: &str = "\
[1.11.2026., 09:00:00] Poruke i pozivi šifrirani.\n\
[1.11.2026., 09:00:05] Ana: zdravo ekipo\n\
[1.11.2026., 09:01:00] Ana: kako ste?\n\
[1.11.2026., 09:02:00] Ana: evo mene opet\n\
";
// Same participant set, but a genuinely different chat (different first
// message): must never merge with the one above.
const WA_OTHER_CHAT: &str = "\
[1.11.2026., 09:00:00] Poruke i pozivi šifrirani.\n\
[1.11.2026., 09:00:05] Ana: ovo je sasvim drugi chat\n\
[1.11.2026., 09:01:00] Ana: drugačija prva poruka\n\
";

#[test]
fn whatsapp_rename_stays_one_conversation_and_dedups() {
    let (storage, raw, _) = scratch("wa-rename");
    ingest(
        &storage,
        &WhatsappSource::new(write_export(
            "a",
            "WhatsApp Chat with Ana.txt",
            WA_EXPORT.as_bytes(),
        )),
    )
    .unwrap();
    let second = ingest(
        &storage,
        &WhatsappSource::new(write_export(
            "b",
            "Ana Marić — preimenovano.txt",
            WA_EXPORT_LATER.as_bytes(),
        )),
    )
    .unwrap();
    // The renamed re-export dedups every shared message and adds only the
    // new one — the conversation key, not the file name, keys the ids.
    assert_eq!(second.inserted, 1);
    assert_eq!(second.skipped, 3);
    assert_eq!(storage.stats().unwrap().total, 4);

    // Exactly one conversation, provider-keyed, label following the newest
    // file stem.
    assert_eq!(count_conversations(&raw), 1);
    let (key, label, confidence): (Option<String>, String, String) = raw
        .query_row(
            "SELECT provider_key, label, confidence FROM conversations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert!(key
        .as_deref()
        .is_some_and(|k| k.starts_with("whatsapp:chat:sha256:")));
    assert_eq!(label, "Ana Marić — preimenovano");
    assert_eq!(confidence, "verified");

    // All four messages share the one conversation.
    let in_conv: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM archive_messages WHERE conversation_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(in_conv, 4);
}

#[test]
fn whatsapp_same_participants_different_chats_stay_separated() {
    let (storage, raw, _) = scratch("wa-anchor");
    ingest(
        &storage,
        &WhatsappSource::new(write_export("a", "Ana.txt", WA_EXPORT.as_bytes())),
    )
    .unwrap();
    ingest(
        &storage,
        &WhatsappSource::new(write_export("b", "Ana posao.txt", WA_OTHER_CHAT.as_bytes())),
    )
    .unwrap();

    // Identical participant set and file-ish name, but a different first
    // authored message: two keys, two conversations, no message mixing.
    assert_eq!(count_conversations(&raw), 2);
    assert_eq!(
        raw.query_row(
            "SELECT COUNT(DISTINCT provider_key) FROM conversations",
            [],
            |r| { r.get::<_, i64>(0) }
        )
        .unwrap(),
        2
    );
    assert_eq!(storage.stats().unwrap().total, 6);
}

#[test]
fn whatsapp_export_without_authors_falls_back_to_legacy_grouping() {
    let (storage, raw, _) = scratch("wa-systemonly");
    let export = "[1.11.2026., 09:00:00] Poruke i pozivi šifrirani.\n";
    let collected = {
        let tz = FixedOffset::east_opt(0).unwrap();
        parse_whatsapp_tz(export.as_bytes(), "chat", &tz, "UTC").unwrap()
    };
    assert!(collected.messages[0].conversation_key.is_empty());
    ingest_collected(&storage, collected);
    let (key, confidence): (Option<String>, String) = raw
        .query_row(
            "SELECT provider_key, confidence FROM conversations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(key.is_none(), "nothing identifies the chat safely");
    assert_eq!(confidence, "legacy_unresolved");
}

fn ingest_collected(storage: &Storage, collected: vault_core::ingest::Collected) {
    vault_core::ingest::ingest_collected(storage, "whatsapp", collected, None).unwrap();
}

// -----------------------------------------------------------------------
// WhatsApp time: explicit zone interpretation, DST, gaps, folds
// -----------------------------------------------------------------------

/// A Berlin-shaped test zone: +02:00 April–October, +01:00 otherwise, with
/// a spring-forward gap and an autumn fold on the transition Sundays.
#[derive(Clone)]
struct SummerWinter;

impl TimeZone for SummerWinter {
    type Offset = FixedOffset;

    fn from_offset(_offset: &FixedOffset) -> Self {
        SummerWinter
    }

    fn offset_from_utc_date(&self, _utc: &chrono::NaiveDate) -> FixedOffset {
        FixedOffset::east_opt(3600).unwrap()
    }

    fn offset_from_utc_datetime(&self, _utc: &chrono::NaiveDateTime) -> FixedOffset {
        FixedOffset::east_opt(3600).unwrap()
    }

    fn offset_from_local_date(
        &self,
        local: &chrono::NaiveDate,
    ) -> chrono::LocalResult<FixedOffset> {
        chrono::LocalResult::Single(
            FixedOffset::east_opt(if (4..=10).contains(&local.month()) {
                7200
            } else {
                3600
            })
            .unwrap(),
        )
    }

    fn offset_from_local_datetime(
        &self,
        local: &chrono::NaiveDateTime,
    ) -> chrono::LocalResult<FixedOffset> {
        // The 2026-03-29 02:00–03:00 wall-hour does not exist (gap);
        // 03:00–04:00 on 2026-10-25 happens twice (fold).
        if local.date() == chrono::NaiveDate::from_ymd_opt(2026, 3, 29).unwrap()
            && local.time() >= chrono::NaiveTime::from_hms_opt(2, 0, 0).unwrap()
            && local.time() < chrono::NaiveTime::from_hms_opt(3, 0, 0).unwrap()
        {
            return chrono::LocalResult::None;
        }
        if local.date() == chrono::NaiveDate::from_ymd_opt(2026, 10, 25).unwrap()
            && local.time() >= chrono::NaiveTime::from_hms_opt(3, 0, 0).unwrap()
            && local.time() < chrono::NaiveTime::from_hms_opt(4, 0, 0).unwrap()
        {
            let summer = FixedOffset::east_opt(7200).unwrap();
            return chrono::LocalResult::Ambiguous(summer, self.offset(local));
        }
        chrono::LocalResult::Single(self.offset(local))
    }
}

impl SummerWinter {
    fn offset(&self, local: &chrono::NaiveDateTime) -> FixedOffset {
        use chrono::Datelike as _;
        let summer = (4..=10).contains(&local.month());
        FixedOffset::east_opt(if summer { 7200 } else { 3600 }).unwrap()
    }
}

#[test]
fn whatsapp_stamps_interpret_dst_aware_in_explicit_zone() {
    let export = concat!(
        "[1.7.2026., 12:00:00] Ana: letnja poruka\n",
        "[1.11.2026., 12:00:00] Ana: zimska poruka\n",
        "[29.3.2026., 02:30:00] Ana: rupa u kalendaru\n",
        "[25.10.2026., 03:30:00] Ana: dupli sat\n",
    );
    let collected = parse_whatsapp_tz(
        export.as_bytes(),
        "Ana",
        &SummerWinter,
        "Europe/Bergrade-test",
    )
    .unwrap();
    let by_body = |needle: &str| {
        collected
            .messages
            .iter()
            .find(|m| m.body.contains(needle))
            .unwrap_or_else(|| panic!("no message with {needle}"))
    };

    // The same local wall-time resolves to different UTC instants across
    // the DST boundary (+02:00 summer → 10:00Z; +01:00 winter → 11:00Z),
    // each stamp with its own season's offset.
    let summer = by_body("letnja");
    assert_eq!(summer.sent_at, "2026-07-01T10:00:00Z");
    let winter = by_body("zimska");
    assert_eq!(winter.sent_at, "2026-11-01T11:00:00Z");
    for msg in [summer, winter] {
        assert_eq!(msg.timezone, "Europe/Bergrade-test");
        assert_eq!(msg.time_confidence, "legacy_assumed");
    }

    // A wall-time the spring-forward gap removed has no instant at all —
    // kept unresolved with the verbatim stamp.
    let gap = by_body("rupa");
    assert_eq!(gap.sent_at, "");
    assert_eq!(gap.original_timestamp, "2026-03-29T02:30:00");
    assert_eq!(gap.time_confidence, "unresolved");

    // A fold-repeated wall-time resolves deterministically to the first
    // (summer) occurrence.
    let fold = by_body("dupli");
    assert_eq!(fold.sent_at, "2026-10-25T01:30:00Z");
}

// -----------------------------------------------------------------------
// Email reply graph
// -----------------------------------------------------------------------

/// One raw mail with explicit headers.
fn mail(
    message_id: &str,
    subject: &str,
    date: &str,
    in_reply_to: Option<&str>,
    references: Option<&str>,
    body: &str,
) -> String {
    let mut s = format!(
        "From: sender@example.com\nTo: me@example.com\nSubject: {subject}\nDate: {date}\nMessage-ID: <{message_id}>\n"
    );
    if let Some(irt) = in_reply_to {
        s.push_str(&format!("In-Reply-To: <{irt}>\n"));
    }
    if let Some(r) = references {
        s.push_str(&format!("References: {r}\n"));
    }
    s.push_str(&format!("\n{body}\n"));
    s
}

fn mbox_bytes(mails: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    for m in mails {
        out.extend_from_slice(b"From sender@example.com Mon Nov 02 10:00:00 2026\n");
        out.extend_from_slice(m.as_bytes());
    }
    out
}

#[test]
fn email_reply_chain_groups_into_one_thread() {
    let (storage, raw, _) = scratch("email-thread");
    let mails = [
        mail(
            "root@a",
            "Plan",
            "Mon, 2 Nov 2026 10:00:00 +0100",
            None,
            None,
            "prva poruka",
        ),
        mail(
            "reply1@a",
            "Re: Plan",
            "Mon, 2 Nov 2026 11:00:00 +0100",
            Some("root@a"),
            Some("<root@a>"),
            "odgovor jedan",
        ),
        mail(
            "reply2@a",
            "Re: Plan",
            "Mon, 2 Nov 2026 12:00:00 +0100",
            Some("reply1@a"),
            Some("<root@a> <reply1@a>"),
            "odgovor dva",
        ),
    ];
    ingest(
        &storage,
        &MboxSource::new(write_export("t", "project.mbox", &mbox_bytes(&mails))),
    )
    .unwrap();

    // All three share one conversation; the thread context shows the
    // chain as before/after peers.
    assert_eq!(count_conversations(&raw), 1);
    let hit = storage
        .search_filtered("\"odgovor jedan\"", None, None, None, false, 10)
        .unwrap()[0]
        .id;
    let ctx = storage.chat_context(hit, 8).unwrap().unwrap();
    assert_eq!(ctx.before.len(), 1, "the root precedes");
    assert_eq!(ctx.after.len(), 1, "the second reply follows");
    assert!(ctx.before[0].body.contains("prva poruka"));
    assert!(ctx.after[0].body.contains("odgovor dva"));

    // Mail timestamps are exact from the header offset (M1.3 provenance).
    let (tz, confidence, original): (String, String, String) = raw
        .query_row(
            "SELECT timezone, time_confidence, original_timestamp
             FROM archive_messages WHERE external_id = 'root@a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(tz, "+01:00");
    assert_eq!(confidence, "explicit_offset");
    assert_eq!(original, "Mon, 2 Nov 2026 10:00:00 +0100");
}

#[test]
fn email_orphan_reply_unifies_when_root_and_second_reply_arrive() {
    // The reply is imported before its root: it threads under the root's
    // id; the late root stays standalone until a second reply adopts it.
    let (storage, raw, _) = scratch("email-orphan");
    let orphan = mail(
        "orphan-reply@a",
        "Re: Kasni root",
        "Mon, 2 Nov 2026 12:00:00 +0100",
        Some("late-root@a"),
        Some("<late-root@a>"),
        "odgovor pre roota",
    );
    ingest(
        &storage,
        &MboxSource::new(write_export("t", "orphan.mbox", &mbox_bytes(&[orphan]))),
    )
    .unwrap();

    // Keyed by the referenced root id — a thread of one, safely.
    let conv =
        conversation(&raw, "email:thread:late-root@a").expect("orphan threads under the root id");
    assert_eq!(conv.2, "verified");
    let orphan_conv_id: String = raw
        .query_row(
            "SELECT c.id FROM conversations c
             JOIN archive_messages m ON m.conversation_id = c.id
             WHERE m.external_id = 'orphan-reply@a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let root_now: Option<String> = raw
        .query_row(
            "SELECT conversation_id FROM archive_messages WHERE external_id = 'late-root@a'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
        .flatten();
    assert!(root_now.is_none(), "the root has not arrived yet");

    // The root arrives later, standalone (IMAP-style: no chat grouping,
    // no refs of its own): it must not fabricate a merge — the split is
    // the safe state until a later reply adopts it.
    let root = mail(
        "late-root@a",
        "Kasni root",
        "Mon, 2 Nov 2026 10:00:00 +0100",
        None,
        None,
        "root poruka",
    );
    let root_msg =
        vault_core::mail::parse_raw_mail(root.as_bytes(), "mbox", "").expect("root mail parses");
    storage.insert_message(&root_msg).unwrap();
    assert_eq!(
        count_conversations(&raw),
        1,
        "no wrong merge: split is safe"
    );

    // …and a second reply to the root adopts it into the existing thread.
    let second = mail(
        "second-reply@a",
        "Re: Kasni root",
        "Mon, 2 Nov 2026 13:00:00 +0100",
        Some("late-root@a"),
        Some("<late-root@a>"),
        "drugi odgovor",
    );
    ingest(
        &storage,
        &MboxSource::new(write_export("t", "second.mbox", &mbox_bytes(&[second]))),
    )
    .unwrap();
    let root_adopted: Option<String> = raw
        .query_row(
            "SELECT conversation_id FROM archive_messages WHERE external_id = 'late-root@a'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
        .flatten();
    assert_eq!(
        root_adopted.as_deref(),
        Some(orphan_conv_id.as_str()),
        "the root joins the thread"
    );
    let reply2_conv: String = raw
        .query_row(
            "SELECT conversation_id FROM archive_messages WHERE external_id = 'second-reply@a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(reply2_conv, orphan_conv_id);
    // One thread, three messages.
    let in_thread: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM archive_messages WHERE conversation_id = ?1",
            [&orphan_conv_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(in_thread, 3);
}

#[test]
fn email_reference_cycle_is_safe_and_unifies() {
    // A references B while B references A: resolution is single-hop
    // lookup, so this can never loop; both land in one conversation.
    let (storage, raw, _) = scratch("email-cycle");
    let mails = [
        mail(
            "cyc-a@a",
            "Ciklus",
            "Mon, 2 Nov 2026 10:00:00 +0100",
            Some("cyc-b@a"),
            Some("<cyc-b@a>"),
            "a govori",
        ),
        mail(
            "cyc-b@a",
            "Re: Ciklus",
            "Mon, 2 Nov 2026 11:00:00 +0100",
            Some("cyc-a@a"),
            Some("<cyc-a@a>"),
            "b govori",
        ),
    ];
    ingest(
        &storage,
        &MboxSource::new(write_export("t", "cycle.mbox", &mbox_bytes(&mails))),
    )
    .unwrap();
    assert_eq!(
        count_conversations(&raw),
        1,
        "the cycle closes into one thread"
    );
    let distinct: i64 = raw
        .query_row(
            "SELECT COUNT(DISTINCT conversation_id) FROM archive_messages
             WHERE external_id IN ('cyc-a@a', 'cyc-b@a')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(distinct, 1);
}

#[test]
fn email_incomplete_references_chain_is_safe() {
    // References point at an unknown root plus a known parent inside an
    // existing thread: the reply joins the parent's conversation (first
    // *stored* ref wins) instead of mis-grouping by the phantom root.
    let (storage, raw, _) = scratch("email-incomplete");
    let known = mail(
        "known-parent@a",
        "Tema",
        "Mon, 2 Nov 2026 10:00:00 +0100",
        None,
        None,
        "parent poruka",
    );
    let broken = mail(
        "broken-reply@a",
        "Re: Tema",
        "Mon, 2 Nov 2026 11:00:00 +0100",
        Some("known-parent@a"),
        Some("<phantom-root@a> <known-parent@a>"),
        "prekinuti lanac",
    );
    ingest(
        &storage,
        &MboxSource::new(write_export(
            "t",
            "broken.mbox",
            &mbox_bytes(&[known, broken]),
        )),
    )
    .unwrap();
    let distinct: i64 = raw
        .query_row(
            "SELECT COUNT(DISTINCT conversation_id) FROM archive_messages
             WHERE external_id IN ('known-parent@a', 'broken-reply@a')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(distinct, 1, "the reply stays with its stored parent");
    // No phantom thread was created for the unknown root.
    assert_eq!(count_conversations(&raw), 1);
}

#[test]
fn email_unparseable_and_missing_dates_carry_honest_labels() {
    let (storage, raw, _) = scratch("email-time");
    let mails = [
        mail(
            "bad-date@a",
            "Loš datum",
            "nije datum",
            None,
            None,
            "sadržaj",
        ),
        mail(
            "no-date@a",
            "Bez datuma",
            "",
            None,
            None,
            "bez datuma poruka",
        ),
    ];
    ingest(
        &storage,
        &MboxSource::new(write_export("t", "times.mbox", &mbox_bytes(&mails))),
    )
    .unwrap();
    let bad: (Option<String>, String, String) = raw
        .query_row(
            "SELECT sent_instant, time_confidence, original_timestamp
             FROM archive_messages WHERE external_id = 'bad-date@a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(bad.0, None, "an unparseable date yields no instant");
    assert_eq!(bad.1, "unresolved");
    assert_eq!(bad.2, "nije datum", "the verbatim evidence is kept");
    let none: (Option<String>, String, String) = raw
        .query_row(
            "SELECT sent_instant, time_confidence, original_timestamp
             FROM archive_messages WHERE external_id = 'no-date@a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(none.1, "missing");
    assert_eq!(none.2, "");
}

// -----------------------------------------------------------------------
// Direct insert path: provider conversations, validation
// -----------------------------------------------------------------------

#[test]
fn provider_conversation_renames_follow_newest_label() {
    let (storage, raw, _) = scratch("provider-label");
    let mut first = NewMessage::new("telegram", "42/1", "Ana", "2026-11-01T09:00:00Z", "poruka");
    first.channel = "chat".into();
    first.chat_id = "Staro ime".into();
    first.conversation_key = "telegram:chat:42".into();
    storage.insert_message(&first).unwrap();

    let mut renamed = NewMessage::new("telegram", "42/2", "Ana", "2026-11-01T09:01:00Z", "druga");
    renamed.channel = "chat".into();
    renamed.chat_id = "Novo ime".into();
    renamed.conversation_key = "telegram:chat:42".into();
    storage.insert_message(&renamed).unwrap();

    let conv = conversation(&raw, "telegram:chat:42").unwrap();
    assert_eq!(conv.1, "Novo ime");

    // And the duplicate path (a full re-import) also refreshes the label.
    let mut reimport = NewMessage::new("telegram", "42/1", "Ana", "2026-11-01T09:00:00Z", "poruka");
    reimport.channel = "chat".into();
    reimport.chat_id = "Sasvim novo ime".into();
    reimport.conversation_key = "telegram:chat:42".into();
    assert!(matches!(
        storage.insert_message(&reimport).unwrap(),
        InsertOutcome::Duplicate(_)
    ));
    assert_eq!(
        conversation(&raw, "telegram:chat:42").unwrap().1,
        "Sasvim novo ime"
    );
}

#[test]
fn invalid_time_confidence_is_rejected() {
    let storage = Storage::open_in_memory().unwrap();
    let mut msg = NewMessage::new("demo", "x1", "a", "2026-11-01T09:00:00Z", "poruka");
    msg.time_confidence = "probably-fine".into();
    assert!(storage.insert_message(&msg).is_err());
}

#[test]
fn signal_conversation_key_and_time_provenance() {
    let (storage, raw, _) = scratch("signal-key");
    let export = br#"[
        {"id": "m1", "type": "incoming", "conversationId": "3f9c1a2e-uuid",
         "source": "+385912345678.2", "sourceName": "Ana",
         "sent_at": 1730432400000, "body": "signal poruka"}
    ]"#;
    ingest(
        &storage,
        &vault_core::signal::SignalSource::new(write_export("s", "Ana.json", export)),
    )
    .unwrap();
    let conv = conversation(&raw, "signal:chat:3f9c1a2e-uuid").expect("uuid-keyed conversation");
    assert_eq!(conv.2, "verified");
    let (tz, confidence, original): (String, String, String) = raw
        .query_row(
            "SELECT timezone, time_confidence, original_timestamp FROM archive_messages",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (tz.as_str(), confidence.as_str()),
        ("UTC", "explicit_offset")
    );
    assert_eq!(original, "1730432400000");
}

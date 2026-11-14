//! M1.3 — stable conversation keys over legacy data (migration 7).
//!
//! A database exactly as the pre-M1.3 release left it (the frozen schema-2
//! layout, seeded through raw SQL) upgrades in place: chats re-imported
//! under a rename merge into one conversation, chats that legacy name
//! grouping wrongly merged split by their provider ids, WhatsApp
//! identities re-key so a re-import after a rename dedups, and everything
//! that cannot be safely interpreted stays legacy-unresolved.

use chrono::FixedOffset;
use rusqlite::Connection;
use vault_core::ingest::ingest;
use vault_core::storage::Storage;
use vault_core::telegram::TelegramSource;
use vault_core::whatsapp::parse_whatsapp_tz;

use crate::util::{raw, scratch_dir, TzGuard};

/// The frozen schema-2 SQL, verbatim as migrations 1 and 2 created it
/// (same fixture approach as the M1.1 canonical suite).
const V2_SCHEMA_SQL: &str = r#"
    CREATE TABLE messages (
        id           INTEGER PRIMARY KEY,
        source       TEXT    NOT NULL,
        external_id  TEXT    NOT NULL,
        channel      TEXT    NOT NULL DEFAULT '',
        chat_id      TEXT    NOT NULL DEFAULT '',
        author       TEXT    NOT NULL DEFAULT '',
        sent_at      TEXT    NOT NULL DEFAULT '',
        body         TEXT    NOT NULL DEFAULT '',
        raw_json     TEXT    NOT NULL DEFAULT '{}',
        created_at   TEXT    NOT NULL DEFAULT (datetime('now')),
        UNIQUE (source, external_id)
    );
    CREATE INDEX idx_messages_sent_at ON messages (sent_at);
    CREATE INDEX idx_messages_source_chat ON messages (source, chat_id);
    CREATE VIRTUAL TABLE messages_fts USING fts5 (
        body,
        author,
        content = 'messages',
        content_rowid = 'id'
    );
    CREATE TRIGGER messages_fts_ai AFTER INSERT ON messages BEGIN
        INSERT INTO messages_fts (rowid, body, author)
        VALUES (new.id, new.body, new.author);
    END;
    CREATE TRIGGER messages_fts_ad AFTER DELETE ON messages BEGIN
        INSERT INTO messages_fts (messages_fts, rowid, body, author)
        VALUES ('delete', old.id, old.body, old.author);
    END;
    CREATE TRIGGER messages_fts_au AFTER UPDATE ON messages BEGIN
        INSERT INTO messages_fts (messages_fts, rowid, body, author)
        VALUES ('delete', old.id, old.body, old.author);
        INSERT INTO messages_fts (rowid, body, author)
        VALUES (new.id, new.body, new.author);
    END;

    CREATE TABLE imap_sync_state (
        source      TEXT    NOT NULL,
        folder      TEXT    NOT NULL,
        uidvalidity INTEGER NOT NULL,
        last_uid    INTEGER NOT NULL DEFAULT 0,
        updated_at  TEXT    NOT NULL DEFAULT (datetime('now')),
        PRIMARY KEY (source, folder)
    );
"#;

/// Seed one legacy row into the schema-2 `messages` table.
fn legacy_row(
    conn: &Connection,
    source: &str,
    external_id: &str,
    chat_id: &str,
    author: &str,
    sent_at: &str,
    body: &str,
) {
    conn.execute(
        "INSERT INTO messages (source, external_id, channel, chat_id, author, sent_at, body, raw_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '{}')",
        rusqlite::params![
            source,
            external_id,
            if source == "mbox" || source.starts_with("imap") { "email" } else { "chat" },
            chat_id,
            author,
            sent_at,
            body
        ],
    )
    .unwrap();
}

/// Build a pre-M1.3 (schema 2) database at `path` and return it.
fn v2_db(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )
    .unwrap();
    conn.execute_batch(V2_SCHEMA_SQL).unwrap();
    conn.execute_batch(
        "INSERT INTO schema_migrations (version) VALUES (1);
         INSERT INTO schema_migrations (version) VALUES (2);",
    )
    .unwrap();
}

/// (provider_key, label, confidence) of every conversation, ordered.
fn conversations(db: &std::path::Path) -> Vec<(Option<String>, String, String)> {
    let conn = raw(db);
    let mut stmt = conn
        .prepare("SELECT provider_key, label, confidence FROM conversations ORDER BY label")
        .unwrap();
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn count(db: &std::path::Path, sql: &str) -> i64 {
    raw(db).query_row(sql, [], |r| r.get(0)).unwrap()
}

fn export_dir(_name: &str, file: &str, bytes: &[u8]) -> std::path::PathBuf {
    let dir = scratch_dir();
    let path = dir.path().join(file);
    std::fs::write(&path, bytes).unwrap();
    std::mem::forget(dir);
    path
}

#[test]
fn telegram_renamed_imports_merge_and_same_name_chats_split() {
    let dir = scratch_dir();
    let db = dir.path().join("tg-legacy.db");
    v2_db(&db);
    {
        let conn = raw(&db);
        // Chat 42 re-imported under a rename: pre-M1.3 grouping split it
        // into two conversations (display names differ) — the second
        // import's messages deduped on the stable external id, so only the
        // first name's conversation ever got rows… but a re-export under a
        // THIRD name that also picked up new messages created a second
        // conversation with only the new rows.
        legacy_row(
            &conn,
            "telegram",
            "42/100",
            "Staro ime",
            "Ana",
            "2026-11-01T09:00:00Z",
            "poruka jedan",
        );
        legacy_row(
            &conn,
            "telegram",
            "42/101",
            "Staro ime",
            "Ana",
            "2026-11-01T09:01:00Z",
            "poruka dva",
        );
        legacy_row(
            &conn,
            "telegram",
            "42/102",
            "Novo ime",
            "Ana",
            "2026-11-01T09:02:00Z",
            "poruka tri",
        );
        // Two genuinely different chats that share a display name: legacy
        // grouping merged them into ONE conversation (wrong).
        legacy_row(
            &conn,
            "telegram",
            "111/1",
            "Isti naziv",
            "Ana",
            "2026-11-02T09:00:00Z",
            "chat sto jedan",
        );
        legacy_row(
            &conn,
            "telegram",
            "222/1",
            "Isti naziv",
            "Bane",
            "2026-11-02T09:01:00Z",
            "chat dva dva",
        );
    }

    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), 15);

    // The renamed chat is one conversation under its provider key, and
    // the same-name chats split into two.
    let convs = conversations(&db);
    assert_eq!(convs.len(), 3, "{convs:?}");
    let staro = convs.iter().find(|c| c.1 == "Staro ime").unwrap();
    assert_eq!(staro.0.as_deref(), Some("telegram:chat:42"));
    assert_eq!(staro.2, "verified");
    // "Novo ime" merged away: all of chat 42's messages sit in one row set.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM archive_messages m
             JOIN conversations c ON c.id = m.conversation_id
             WHERE c.provider_key = 'telegram:chat:42'"
        ),
        3
    );
    // The same-name chats split by provider id; each keeps its own
    // messages.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM archive_messages m
             JOIN conversations c ON c.id = m.conversation_id
             WHERE c.provider_key = 'telegram:chat:111'"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM archive_messages m
             JOIN conversations c ON c.id = m.conversation_id
             WHERE c.provider_key = 'telegram:chat:222'"
        ),
        1
    );

    // A fresh export of chat 42 (post-rename) resolves into the migrated
    // conversation and dedups against the re-keyed rows.
    let export = r#"{"name": "Treće ime", "id": 42, "messages": [
        {"id": 100, "type": "message", "date_unixtime": "1793538000", "from": "Ana", "text": "poruka jedan"},
        {"id": 101, "type": "message", "date_unixtime": "1793538060", "from": "Ana", "text": "poruka dva"},
        {"id": 102, "type": "message", "date_unixtime": "1793538120", "from": "Ana", "text": "poruka tri"}]}"#;
    let summary = ingest(
        &s,
        &TelegramSource::new(export_dir("t", "result.json", export.as_bytes())),
    )
    .unwrap();
    assert_eq!(
        summary.inserted, 0,
        "re-import dedups onto the migrated rows"
    );
    assert_eq!(summary.skipped, 3);
    let label: String = raw(&db)
        .query_row(
            "SELECT label FROM conversations WHERE provider_key = 'telegram:chat:42'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(label, "Treće ime", "the label follows the newest name");
}

/// The WhatsApp export whose pre-M1.3 rows the legacy database holds: the
/// exporter-local stamps were stored blind-as-UTC by the old release.
const WA_EXPORT: &str = "\
[1.11.2026., 09:00:00] Poruke i pozivi šifrirani.\n\
[1.11.2026., 09:00:05] Ana: zdravo ekipo\n\
[1.11.2026., 09:01:00] Ana: kako ste?\n\
";

#[test]
fn whatsapp_renamed_legacy_imports_merge_and_reimport_dedups() {
    let dir = scratch_dir();
    let db = dir.path().join("wa-legacy.db");
    v2_db(&db);
    {
        let conn = raw(&db);
        // The same chat imported twice under different file stems: the old
        // id formula hashed the stem, so every message duplicated into a
        // second conversation.
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000001-0",
            "WhatsApp Chat with Ana",
            "",
            "2026-11-01T09:00:00Z",
            "Poruke i pozivi šifrirani.",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000002-0",
            "WhatsApp Chat with Ana",
            "Ana",
            "2026-11-01T09:00:05Z",
            "zdravo ekipo",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000003-0",
            "WhatsApp Chat with Ana",
            "Ana",
            "2026-11-01T09:01:00Z",
            "kako ste?",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000004-0",
            "Ana Marić",
            "",
            "2026-11-01T09:00:00Z",
            "Poruke i pozivi šifrirani.",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000005-0",
            "Ana Marić",
            "Ana",
            "2026-11-01T09:00:05Z",
            "zdravo ekipo",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000006-0",
            "Ana Marić",
            "Ana",
            "2026-11-01T09:01:00Z",
            "kako ste?",
        );
    }

    let s = Storage::open(&db).unwrap();

    // The two imports derived the same conversation key (participants +
    // first authored message) and merged: one provider-keyed conversation
    // holding every row.
    let convs = conversations(&db);
    assert_eq!(convs.len(), 1, "{convs:?}");
    assert!(convs[0]
        .0
        .as_deref()
        .is_some_and(|k| k.starts_with("whatsapp:chat:sha256:")));
    assert_eq!(convs[0].2, "verified");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM archive_messages WHERE conversation_id IS NOT NULL"
        ),
        6,
        "no rows are lost in the merge"
    );
    // Identities were re-keyed to the M1.3 formula.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM message_identities
             JOIN source_instances si ON si.id = source_instance_id
             WHERE si.legacy_namespace = 'whatsapp'
               AND external_key LIKE 'generated-%'"
        ),
        6
    );

    // The money proof: re-importing the export through the M1.3 path
    // dedups against the re-keyed identities instead of duplicating.
    let tz = FixedOffset::east_opt(0).unwrap();
    let collected = parse_whatsapp_tz(WA_EXPORT.as_bytes(), "Ana (najnovije)", &tz, "UTC").unwrap();
    let summary = vault_core::ingest::ingest_collected(
        &s,
        vault_core::whatsapp::WHATSAPP_SOURCE,
        collected,
        None,
    )
    .unwrap();
    assert_eq!(summary.inserted, 0, "re-import dedups onto re-keyed rows");
    assert_eq!(summary.skipped, 3);
    assert_eq!(s.stats().unwrap().total, 6);
    let label: String = raw(&db)
        .query_row("SELECT label FROM conversations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        label, "Ana (najnovije)",
        "the label follows the newest stem"
    );
}

#[test]
fn whatsapp_same_participant_different_chats_do_not_merge() {
    let dir = scratch_dir();
    let db = dir.path().join("wa-twomarkos.db");
    v2_db(&db);
    {
        let conn = raw(&db);
        // Two different 1:1 chats whose only participant shares a display
        // name: their first authored messages differ, so the anchor keeps
        // them apart — pre-M1.3 and post-M1.3 alike.
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000001-0",
            "Marko",
            "",
            "2026-11-01T09:00:00Z",
            "Poruke i pozivi šifrirani.",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000002-0",
            "Marko",
            "Marko",
            "2026-11-01T09:00:05Z",
            "poruka iz prvog chata",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000003-0",
            "Marko posao",
            "",
            "2026-11-01T10:00:00Z",
            "Poruke i pozivi šifrirani.",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000004-0",
            "Marko posao",
            "Marko",
            "2026-11-01T10:00:05Z",
            "sasvim druga prva poruka",
        );
    }

    Storage::open(&db).unwrap();
    let convs = conversations(&db);
    assert_eq!(convs.len(), 2, "the anchor distinguishes same-name chats");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(DISTINCT provider_key) FROM conversations"
        ),
        2
    );
}

#[test]
fn uninterpretable_legacy_rows_stay_unresolved() {
    let dir = scratch_dir();
    let db = dir.path().join("unresolved.db");
    v2_db(&db);
    {
        let conn = raw(&db);
        // Id-less Telegram export: the archived ids carry the display
        // name (`<name>/<msg id>` — the old name-fallback dedup), and a
        // display name never keys a conversation.
        legacy_row(
            &conn,
            "telegram",
            "Ime bez id/1",
            "Ime bez id",
            "Ana",
            "2026-11-01T09:00:00Z",
            "poruka",
        );
        // Id-less Signal export: `<file stem>/<message id>` — a file stem
        // is not a conversation uuid, so it never keys a conversation.
        legacy_row(
            &conn,
            "signal",
            "Signal bez id/1",
            "Signal bez id",
            "Ana",
            "2026-11-02T09:00:00Z",
            "poruka signal",
        );
        // A phone-number-shaped display name is not a chat id either:
        // real Telegram chat ids never carry a leading '+'.
        legacy_row(
            &conn,
            "telegram",
            "+3859123456/1",
            "+3859123456",
            "Ana",
            "2026-11-03T09:00:00Z",
            "poruka",
        );
        // Author-less WhatsApp export: nothing identifies the chat.
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000009-0",
            "samo sistem",
            "",
            "2026-11-01T09:00:00Z",
            "Poruke i pozivi šifrirani.",
        );
        // Legacy mbox mail: threading headers were never recorded, so no
        // reply graph can be reconstructed.
        legacy_row(
            &conn,
            "mbox",
            "old-thread@archive.local",
            "",
            "ana <ana@example.com>",
            "2026-11-01T09:00:00Z",
            "stara poruka",
        );
    }

    Storage::open(&db).unwrap();
    let convs = conversations(&db);
    // The id-less Telegram chat keeps its legacy name grouping, the
    // id-less Signal chat its legacy stem grouping, and the author-less
    // WhatsApp export its legacy stem conversation — all
    // legacy-unresolved: nothing in them can be keyed safely. The
    // legacy mbox mail (threading headers never recorded) stays
    // conversationless exactly as M1.1 left it.
    assert_eq!(convs.len(), 4, "{convs:?}");
    assert!(convs.iter().all(|c| c.0.is_none()));
    assert!(convs.iter().all(|c| c.2 == "legacy_unresolved"));
    assert!(convs.iter().any(|c| c.1 == "Ime bez id"));
    assert!(convs.iter().any(|c| c.1 == "Signal bez id"));
    assert!(convs.iter().any(|c| c.1 == "+3859123456"));
    assert!(convs.iter().any(|c| c.1 == "samo sistem"));
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM archive_messages WHERE external_id = 'old-thread@archive.local'
               AND conversation_id IS NULL"
        ),
        1
    );
    // The author-less WhatsApp messages still re-key (same formula as the
    // parser's fallback over the stem) so re-imports dedup.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM message_identities
             JOIN source_instances si ON si.id = source_instance_id
             WHERE si.legacy_namespace = 'whatsapp'
               AND external_key LIKE 'generated-%'"
        ),
        1
    );
}

#[test]
fn whatsapp_rekey_hash_matches_live_import_on_non_utc_zone() {
    // The migration re-resolves legacy blind-`Z` stamps in the machine's
    // local zone — here pinned to a non-UTC summer zone (+02:00) — so a
    // migrated row's content_hash must equal what a live import of the
    // same export hashes: the same resolution over the same wall time.
    // POSIX TZ string (Europe/Paris rules: CET +01:00, CEST +02:00, EU DST
    // transitions) parses in-memory with no /usr/share/zoneinfo, unlike an
    // IANA name which silently falls back to UTC in bare containers.
    let _tz_guard = TzGuard::pin("CET-1CEST,M3.5.0,M10.5.0/3");
    let dir = scratch_dir();
    let db = dir.path().join("wa-zone.db");
    v2_db(&db);
    {
        let conn = raw(&db);
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000001-0",
            "WhatsApp Chat with Ana",
            "",
            "2026-07-01T09:00:00Z",
            "Poruke i pozivi šifrirani.",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000002-0",
            "WhatsApp Chat with Ana",
            "Ana",
            "2026-07-01T09:00:05Z",
            "zdravo ekipo",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000003-0",
            "WhatsApp Chat with Ana",
            "Ana",
            "2026-07-01T09:01:00Z",
            "kako ste?",
        );
    }

    let _s = Storage::open(&db).unwrap();

    // The export whose stamps the legacy rows carry, re-imported through
    // the M1.3 path in the same +02:00 summer zone.
    let export = "[1.7.2026., 09:00:00] Poruke i pozivi šifrirani.\n\
                  [1.7.2026., 09:00:05] Ana: zdravo ekipo\n\
                  [1.7.2026., 09:01:00] Ana: kako ste?\n";
    let tz = FixedOffset::east_opt(2 * 3600).unwrap();
    let collected =
        parse_whatsapp_tz(export.as_bytes(), "Ana (najnovije)", &tz, "Europe/Paris").unwrap();
    assert_eq!(collected.messages.len(), 3);
    assert_eq!(
        collected.messages[1].sent_at, "2026-07-01T07:00:05Z",
        "the live path resolves 09:00:05 in +02:00 to 07:00:05Z"
    );

    // Every migrated row's content_hash equals the live formula over the
    // resolved instant: external_id, author, sent_at, body joined by
    // U+001F and SHA-256'd.
    let expected_hash = |m: &vault_core::message::NewMessage| {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            m.external_id, m.author, m.sent_at, m.body
        ));
        let mut out = String::new();
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        out
    };
    let conn = raw(&db);
    for m in &collected.messages {
        let (hash, sent_instant, timezone, confidence): (String, Option<String>, String, String) =
            conn.query_row(
                "SELECT content_hash, sent_instant, timezone, time_confidence
                 FROM archive_messages WHERE external_id = ?1",
                rusqlite::params![m.external_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(hash, expected_hash(m), "hash diverges for {}", m.body);
        assert_eq!(
            sent_instant.as_deref(),
            Some(m.sent_at.as_str()),
            "stored instant diverges for {}",
            m.body
        );
        assert_eq!(
            timezone,
            iana_time_zone::get_timezone().unwrap_or_else(|_| "local".to_string()),
            "zone label for {}",
            m.body
        );
        assert_eq!(confidence, "legacy_assumed", "confidence for {}", m.body);
    }
}

#[test]
fn signal_uuid_prefixed_legacy_rows_key_and_reimport_dedups() {
    let dir = scratch_dir();
    let db = dir.path().join("signal-keyed.db");
    v2_db(&db);
    {
        let conn = raw(&db);
        // A pre-M1.3 Signal export with the conversation uuid namespacing
        // its message ids (`<conversationId>/<id>`).
        legacy_row(
            &conn,
            "signal",
            "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01/m-1",
            "Ana",
            "Ana",
            "2026-11-01T09:00:00Z",
            "poruka signal",
        );
    }

    let s = Storage::open(&db).unwrap();
    let convs = conversations(&db);
    assert_eq!(convs.len(), 1, "{convs:?}");
    assert_eq!(
        convs[0].0.as_deref(),
        Some("signal:chat:e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01")
    );
    assert_eq!(convs[0].2, "verified");

    // Re-importing the same export (renamed file) through the M1.3 path
    // dedups onto the migrated row.
    let export = r#"[{"id": "m-1", "type": "incoming",
        "conversationId": "e7e0e0e3-1111-4a2b-9d3c-2a9d4b1e5c01",
        "source": "+3851", "sent_at": 1793538000000, "body": "poruka signal"}]"#;
    let collected = vault_core::signal::parse_signal(export.as_bytes(), "Ana (najnovije)").unwrap();
    let summary = vault_core::ingest::ingest_collected(
        &s,
        vault_core::signal::SIGNAL_SOURCE,
        collected,
        None,
    )
    .unwrap();
    assert_eq!(
        summary.inserted, 0,
        "re-import dedups onto the migrated row"
    );
    assert_eq!(summary.skipped, 1);
    assert_eq!(s.stats().unwrap().total, 1);
}

#[test]
fn migration_is_idempotent_on_reopen() {
    let dir = scratch_dir();
    let db = dir.path().join("idempotent.db");
    v2_db(&db);
    {
        let conn = raw(&db);
        legacy_row(
            &conn,
            "telegram",
            "42/100",
            "Staro ime",
            "Ana",
            "2026-11-01T09:00:00Z",
            "poruka",
        );
        legacy_row(
            &conn,
            "whatsapp",
            "generated-0000000000000001-0",
            "Ana",
            "Ana",
            "2026-11-01T09:00:05Z",
            "zdravo",
        );
    }
    let s = Storage::open(&db).unwrap();
    let after_first = conversations(&db);
    drop(s);
    // Reopening applies nothing new and changes nothing.
    let s2 = Storage::open(&db).unwrap();
    assert_eq!(s2.schema_version().unwrap(), 15);
    assert_eq!(conversations(&db), after_first);
}

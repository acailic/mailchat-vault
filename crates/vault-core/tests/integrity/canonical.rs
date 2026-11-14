//! Canonical-store migration (M1.1): a database exactly as the schema-2
//! release left it upgrades in place to the canonical store without loss,
//! and every current write reaches the canonical tables only.
//!
//! The fixture below is the frozen schema-2 layout (migrations 1+2,
//! verbatim) seeded through raw SQL, so the upgrade path exercised here is
//! the real one a user's database takes — not a synthetic near-miss.

use rusqlite::{Connection, OptionalExtension};
use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::query::SearchQuery;
use vault_core::storage::Storage;

use crate::util::{msg, raw, scratch_dir};

/// The frozen schema-2 SQL, verbatim as migrations 1 and 2 created it.
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

/// A legacy message row as the old schema stored it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LegacyRow {
    id: i64,
    source: String,
    external_id: String,
    channel: String,
    chat_id: String,
    author: String,
    sent_at: String,
    body: String,
    raw_json: String,
}

/// Build a database exactly as the schema-2 release left it: full legacy
/// schema, versions 1+2 recorded, and a representative archive — two mail
/// namespaces (one with unknown dates), a chat namespace with two chat
/// groups, plus IMAP cursors. Returns the legacy rows for comparison.
fn schema2_db_with_data(path: &std::path::Path) -> Vec<LegacyRow> {
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

    let rows = vec![
        // mbox mail: dated, no chat grouping.
        LegacyRow {
            id: 0,
            source: "mbox".into(),
            external_id: "old-1@archive.local".into(),
            channel: "email".into(),
            chat_id: String::new(),
            author: "ana <ana@example.com>".into(),
            sent_at: "2025-03-04T09:15:00Z".into(),
            body: "vintage quarterly report".into(),
            raw_json: r#"{"subject":"Q1 report"}"#.into(),
        },
        // mbox mail with an unknown date (legacy '' sentinel).
        LegacyRow {
            id: 0,
            source: "mbox".into(),
            external_id: "old-2@archive.local".into(),
            channel: "email".into(),
            chat_id: String::new(),
            author: "marko".into(),
            sent_at: String::new(),
            body: "undated zephyr note".into(),
            raw_json: "{}".into(),
        },
        // IMAP mail under a username-namespaced source.
        LegacyRow {
            id: 0,
            source: "imap:user@example.com".into(),
            external_id: "srv-1@imap.local".into(),
            channel: "email".into(),
            chat_id: String::new(),
            author: "Petra Petrović <petra@corp.example>".into(),
            sent_at: "2025-06-30T12:00:00Z".into(),
            body: "server side zephyr".into(),
            raw_json: "{}".into(),
        },
        // Chat export: two chat groups in one namespace.
        LegacyRow {
            id: 0,
            source: "whatsapp".into(),
            external_id: "wa-family-1".into(),
            channel: "chat".into(),
            chat_id: "family".into(),
            author: "mom".into(),
            sent_at: "2025-12-24T20:00:00Z".into(),
            body: "sretan božić".into(),
            raw_json: r#"{"quoted":""}"#.into(),
        },
        LegacyRow {
            id: 0,
            source: "whatsapp".into(),
            external_id: "wa-family-2".into(),
            channel: "chat".into(),
            chat_id: "family".into(),
            author: "Ivan Ivić".into(),
            sent_at: "2025-12-24T20:05:00Z".into(),
            body: "hvala mama".into(),
            raw_json: "{}".into(),
        },
        LegacyRow {
            id: 0,
            source: "whatsapp".into(),
            external_id: "wa-work-1".into(),
            channel: "chat".into(),
            chat_id: "work".into(),
            author: "nina_dev".into(),
            sent_at: "2026-01-02T08:30:00Z".into(),
            body: "standup moved to ten".into(),
            raw_json: "{}".into(),
        },
    ];
    for row in &rows {
        conn.execute(
            "INSERT INTO messages
                 (source, external_id, channel, chat_id, author, sent_at, body, raw_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                row.source,
                row.external_id,
                row.channel,
                row.chat_id,
                row.author,
                row.sent_at,
                row.body,
                row.raw_json
            ],
        )
        .unwrap();
    }
    let stored: Vec<LegacyRow> = {
        let mut stmt = conn
            .prepare(
                "SELECT id, source, external_id, channel, chat_id, author, sent_at, body, raw_json
                 FROM messages ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], legacy_from_row)
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    conn.execute_batch(
        "INSERT INTO imap_sync_state (source, folder, uidvalidity, last_uid)
         VALUES ('imap:user@example.com', 'INBOX', 7, 400);
         INSERT INTO imap_sync_state (source, folder, uidvalidity, last_uid)
         VALUES ('imap:user@example.com', 'Archive', 9, 12);",
    )
    .unwrap();
    stored
}

fn legacy_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LegacyRow> {
    Ok(LegacyRow {
        id: row.get(0)?,
        source: row.get(1)?,
        external_id: row.get(2)?,
        channel: row.get(3)?,
        chat_id: row.get(4)?,
        author: row.get(5)?,
        sent_at: row.get(6)?,
        body: row.get(7)?,
        raw_json: row.get(8)?,
    })
}

/// Read the legacy `messages` table via a raw connection.
fn legacy_snapshot(db: &std::path::Path) -> Vec<LegacyRow> {
    let conn = raw(db);
    let mut stmt = conn
        .prepare(
            "SELECT id, source, external_id, channel, chat_id, author, sent_at, body, raw_json
             FROM messages ORDER BY id",
        )
        .unwrap();
    stmt.query_map([], legacy_from_row)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// One canonical row, read via a raw connection.
#[derive(Debug)]
struct CanonicalRow {
    id: i64,
    namespace: String,
    external_id: String,
    channel: String,
    chat_id: String,
    author: String,
    sent_instant: Option<String>,
    original_timestamp: String,
    time_confidence: String,
    body: String,
    raw_json: String,
    content_hash: String,
    conversation_id: Option<String>,
}

fn canonical_rows(db: &std::path::Path) -> Vec<CanonicalRow> {
    let conn = raw(db);
    let mut stmt = conn
        .prepare(
            "SELECT m.id, si.legacy_namespace, m.external_id, m.channel, m.chat_id, m.author,
                    m.sent_instant, m.original_timestamp, m.time_confidence,
                    m.body, m.raw_json, m.content_hash, m.conversation_id
             FROM archive_messages AS m
             JOIN source_instances AS si ON si.id = m.source_instance_id
             ORDER BY m.id",
        )
        .unwrap();
    stmt.query_map([], |row| {
        Ok(CanonicalRow {
            id: row.get(0)?,
            namespace: row.get(1)?,
            external_id: row.get(2)?,
            channel: row.get(3)?,
            chat_id: row.get(4)?,
            author: row.get(5)?,
            sent_instant: row.get(6)?,
            original_timestamp: row.get(7)?,
            time_confidence: row.get(8)?,
            body: row.get(9)?,
            raw_json: row.get(10)?,
            content_hash: row.get(11)?,
            conversation_id: row.get(12)?,
        })
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

fn count(db: &std::path::Path, sql: &str) -> i64 {
    raw(db).query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn old_schema_fixture_migrates_without_loss() {
    // Migration 7 re-resolves the legacy WhatsApp stamps in the machine's
    // local zone; pin it (Europe/Paris rules: both fixture dates are winter
    // CET, +01:00) so the expectations below hold on any machine. The POSIX
    // TZ string encodes the same CET/CEST EU DST rules inline, so it parses
    // in-memory with no /usr/share/zoneinfo (an IANA name silently falls
    // back to UTC in bare containers without tzdata).
    let _tz = crate::util::TzGuard::pin("CET-1CEST,M3.5.0,M10.5.0/3");

    let dir = scratch_dir();
    let db = dir.path().join("v2.db");
    let legacy = schema2_db_with_data(&db);
    assert_eq!(legacy.len(), 6);

    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), 15);
    assert_eq!(
        raw(&db)
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    );

    // The legacy snapshot is untouched: same rows, verbatim.
    assert_eq!(legacy_snapshot(&db), legacy);

    // Row-for-row copy: ids, namespaces, and payloads preserved. The one
    // deliberate exception (M1.3, migration 7): WhatsApp external ids are
    // re-keyed onto the stable-conversation-key hash formula, so a
    // re-import after a rename dedups; the legacy snapshot above keeps
    // the original spelling verbatim either way.
    let canonical = canonical_rows(&db);
    assert_eq!(canonical.len(), legacy.len());
    for (c, l) in canonical.iter().zip(&legacy) {
        assert_eq!(c.id, l.id, "row id preserved");
        assert_eq!(c.namespace, l.source);
        if l.source == "whatsapp" {
            assert!(
                c.external_id.starts_with("generated-") && c.external_id.ends_with("-0"),
                "whatsapp ids re-key to the M1.3 formula: {}",
                c.external_id
            );
        } else {
            assert_eq!(c.external_id, l.external_id);
        }
        assert_eq!(c.channel, l.channel);
        assert_eq!(c.chat_id, l.chat_id);
        assert_eq!(c.author, l.author);
        assert_eq!(c.body, l.body);
        assert_eq!(c.raw_json, l.raw_json);
    }

    // Time provenance: known instants carry their value as both resolved
    // instant and original text with 'legacy_assumed'; unknown times are
    // NULL with 'missing' and the original text stays empty.
    for c in &canonical {
        assert_eq!(
            c.content_hash.len(),
            64,
            "every migrated row carries a sha-256 content fingerprint"
        );
        assert!(
            c.content_hash.chars().all(|ch| ch.is_ascii_hexdigit()),
            "content_hash is hex: {}",
            c.content_hash
        );
        if l_sent_at(&legacy, c.id).is_empty() {
            assert_eq!(c.sent_instant, None, "unknown legacy time stays unknown");
            assert_eq!(c.original_timestamp, "");
            assert_eq!(c.time_confidence, "missing");
        } else if c.namespace == "whatsapp" {
            // The old release stored WhatsApp stamps blind-as-UTC; the
            // migrated instant is the exporter wall time re-resolved in
            // the pinned zone (+01:00), with the verbatim stamp kept.
            let expected_instant = match (c.chat_id.as_str(), c.author.as_str()) {
                ("family", "mom") => "2025-12-24T19:00:00Z",
                ("family", "Ivan Ivić") => "2025-12-24T19:05:00Z",
                ("work", "nina_dev") => "2026-01-02T07:30:00Z",
                other => panic!("unexpected whatsapp row {other:?}"),
            };
            assert_eq!(c.sent_instant.as_deref(), Some(expected_instant));
            assert_eq!(c.original_timestamp, l_sent_at(&legacy, c.id));
            assert_eq!(c.time_confidence, "legacy_assumed");
        } else {
            assert_eq!(c.sent_instant.as_deref(), Some(l_sent_at(&legacy, c.id)));
            assert_eq!(c.original_timestamp, l_sent_at(&legacy, c.id));
            assert_eq!(c.time_confidence, "legacy_assumed");
        }
    }

    // One identity alias per legacy (source, external_id) — except
    // WhatsApp, whose aliases migration 7 deliberately re-keys onto the
    // stable-conversation-key hash formula (see above).
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM message_identities WHERE namespace = 'legacy'"
        ),
        6
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM (SELECT source, external_id FROM messages
              WHERE source != 'whatsapp'
              EXCEPT
              SELECT si.legacy_namespace, mi.external_key
              FROM message_identities mi
              JOIN source_instances si ON si.id = mi.source_instance_id
              WHERE mi.namespace = 'legacy')"
        ),
        0,
        "every non-whatsapp legacy identity has an alias"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM message_identities mi
             JOIN source_instances si ON si.id = mi.source_instance_id
             WHERE si.legacy_namespace = 'whatsapp'
               AND mi.external_key LIKE 'generated-%'"
        ),
        3,
        "whatsapp identities re-key to the generated-id formula"
    );

    // One occurrence per archived message recording the legacy import.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM message_occurrences WHERE scope = 'legacy-import'"
        ),
        6
    );

    // One unresolved source instance per namespace; three namespaces exist.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM source_instances"), 3);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM source_instances
             WHERE kind = 'legacy' AND legacy_namespace IS NOT NULL"
        ),
        3
    );

    // One conversation per non-empty chat group; migration 7 then keys
    // the WhatsApp conversations (participants + anchor derivation), so
    // both carry provider keys as 'verified' — distinct keys, so they
    // stay two conversations.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM conversations"), 2);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM conversations
             WHERE confidence = 'verified'
               AND provider_key LIKE 'whatsapp:chat:sha256:%'"
        ),
        2
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(DISTINCT provider_key) FROM conversations"
        ),
        2,
        "same-name-adjacent chats keep distinct keys"
    );
    let family = canonical.iter().find(|c| c.body == "sretan božić").unwrap();
    let family2 = canonical.iter().find(|c| c.body == "hvala mama").unwrap();
    let work = canonical
        .iter()
        .find(|c| c.body == "standup moved to ten")
        .unwrap();
    let mail = canonical
        .iter()
        .find(|c| c.external_id == "old-1@archive.local")
        .unwrap();
    assert_eq!(
        family.conversation_id, family2.conversation_id,
        "same chat group shares one conversation"
    );
    assert_ne!(family.conversation_id, work.conversation_id);
    assert!(
        mail.conversation_id.is_none(),
        "standalone mail has no conversation"
    );

    // IMAP cursors moved to canonical checkpoints, keyed per instance.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM sync_checkpoints"), 2);
    let state = s
        .get_imap_state("imap:user@example.com", "INBOX")
        .unwrap()
        .expect("migrated cursor");
    assert_eq!((state.uidvalidity, state.last_uid), (7, 400));
    let state = s
        .get_imap_state("imap:user@example.com", "Archive")
        .unwrap()
        .expect("migrated cursor");
    assert_eq!((state.uidvalidity, state.last_uid), (9, 12));

    // Baseline search works over the canonical index: bodies and authors
    // are findable, unknown-time rows are excluded by date bounds.
    assert_eq!(
        s.search_filtered("zephyr", None, None, 10).unwrap().len(),
        2
    );
    // to_date bound keeps the June 30 row and drops the undated one.
    assert_eq!(
        s.search_filtered("zephyr", None, Some("2025-12-31"), 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        s.search_filtered("zephyr", Some("2025-07-01"), None, 10)
            .unwrap()
            .len(),
        0,
        "a July 1 from-bound excludes the June 30 row and the undated row"
    );
    let page = s
        .search_page(&SearchQuery {
            sources: vec!["mbox".into()],
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(page.total, 2, "source facet filters by legacy namespace");
    let chat_hit = s
        .search_page(&SearchQuery {
            text: "božić".into(),
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(chat_hit.total, 1);
    assert!(chat_hit.hits[0].snippet.is_some(), "snippets still marked");

    // Chat context still groups thread peers within the conversation.
    let ctx = s.chat_context(family.id, 8).unwrap().unwrap();
    assert_eq!(ctx.message.id, family.id);
    assert!(ctx.after.iter().any(|m| m.id == family2.id));
    assert!(ctx.before.is_empty());

    // Stats still report per-namespace counts.
    let stats = s.stats().unwrap();
    assert_eq!(stats.total, 6);
    assert_eq!(stats.by_source.len(), 3);
}

fn l_sent_at(legacy: &[LegacyRow], id: i64) -> &str {
    legacy.iter().find(|l| l.id == id).unwrap().sent_at.as_str()
}

#[test]
fn current_writes_reach_canonical_tables_only() {
    let dir = scratch_dir();
    let db = dir.path().join("writes.db");
    let legacy = schema2_db_with_data(&db);
    let s = Storage::open(&db).unwrap();
    let max_legacy_id = legacy.iter().map(|l| l.id).max().unwrap();

    // A brand-new namespace (never seen by the legacy schema) inserts into
    // the canonical tables only.
    let mut new_msg = NewMessage::new(
        "telegram",
        "tg-77",
        "Luka L.",
        "2026-02-02T10:00:00Z",
        "fresh telegram zephyr",
    );
    new_msg.channel = "chat".into();
    new_msg.chat_id = "projekt".into();
    let out = s.insert_message(&new_msg).unwrap();
    let InsertOutcome::Inserted(new_id) = out else {
        panic!("new namespace must insert, got {out:?}")
    };
    assert!(
        new_id > max_legacy_id,
        "canonical ids continue after the migrated ids"
    );

    // A message under an existing namespace also lands canonically.
    let extra = msg("mbox", "old-3@archive.local", "second mbox body");
    let InsertOutcome::Inserted(extra_id) = s.insert_message(&extra).unwrap() else {
        panic!("existing namespace must accept new keys")
    };
    assert!(extra_id > max_legacy_id);

    // The legacy tables never changed: same rows, same count.
    assert_eq!(legacy_snapshot(&db), legacy, "legacy snapshot is untouched");
    assert_eq!(count(&db, "SELECT COUNT(*) FROM messages"), 6);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM imap_sync_state"),
        2,
        "legacy cursor table is never written either"
    );

    // The canonical store holds everything, with identities, occurrences,
    // and a conversation for the new chat.
    assert_eq!(count(&db, "SELECT COUNT(*) FROM archive_messages"), 8);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM message_identities"),
        8,
        "one identity per stored message"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM message_occurrences WHERE scope = 'import'"
        ),
        2
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM conversations"), 3);

    // Re-importing a legacy identity dedups onto the same row, unchanged.
    let repeat = NewMessage {
        body: "changed body must not be stored".into(),
        ..msg("mbox", "old-1@archive.local", "vintage quarterly report")
    };
    let out = s.insert_message(&repeat).unwrap();
    let old_id = legacy
        .iter()
        .find(|l| l.external_id == "old-1@archive.local")
        .unwrap()
        .id;
    assert!(matches!(out, InsertOutcome::Duplicate(id) if id == old_id));
    assert_eq!(count(&db, "SELECT COUNT(*) FROM archive_messages"), 8);
    assert_eq!(legacy_snapshot(&db), legacy);

    // Checkpoint writes also land in the canonical table only.
    s.set_imap_state("imap:user@example.com", "INBOX", 7, 900)
        .unwrap();
    let state = s
        .get_imap_state("imap:user@example.com", "INBOX")
        .unwrap()
        .unwrap();
    assert_eq!(state.last_uid, 900);
    assert_eq!(
        count(
            &db,
            "SELECT last_uid FROM imap_sync_state
             WHERE source = 'imap:user@example.com' AND folder = 'INBOX'"
        ),
        400,
        "legacy cursor table keeps the pre-canonical value"
    );

    // FTS stays in sync with the canonical table alone: the fresh rows are
    // searchable, and the untouched legacy index holds none of them.
    assert_eq!(s.search_filtered("fresh", None, None, 10).unwrap().len(), 1);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH 'fresh'"
        ),
        0
    );

    // Content hashes agree between the migration path and the insert path
    // for identical payloads (same canonical field join).
    let hash_insert: String = raw(&db)
        .query_row(
            "SELECT content_hash FROM archive_messages WHERE id = ?1",
            rusqlite::params![extra_id],
            |r| r.get(0),
        )
        .unwrap();
    let fresh_db = dir.path().join("hash-check.db");
    let fresh = Storage::open(&fresh_db).unwrap();
    let InsertOutcome::Inserted(fid) = fresh.insert_message(&extra).unwrap() else {
        panic!()
    };
    let hash_fresh: String = raw(&fresh_db)
        .query_row(
            "SELECT content_hash FROM archive_messages WHERE id = ?1",
            rusqlite::params![fid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hash_insert, hash_fresh);
}

#[test]
fn migration_and_insert_paths_hash_identically() {
    // Same payload through both paths: legacy fixture → migration, and the
    // current insert path. The content hashes must be identical.
    let dir = scratch_dir();

    let legacy_db = dir.path().join("hash-legacy.db");
    {
        let conn = Connection::open(&legacy_db).unwrap();
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
        conn.execute(
            "INSERT INTO messages
                 (source, external_id, channel, chat_id, author, sent_at, body, raw_json)
             VALUES ('mbox', 'hash-1', 'email', '', 'ana <ana@example.com>',
                     '2026-01-01T00:00:00Z', 'hash payload', '{}')",
            [],
        )
        .unwrap();
    }
    let s = Storage::open(&legacy_db).unwrap();
    let migrated_hash: Option<String> = raw(&legacy_db)
        .query_row(
            "SELECT content_hash FROM archive_messages WHERE external_id = 'hash-1'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
        .flatten();
    drop(s);

    let insert_db = dir.path().join("hash-insert.db");
    let s = Storage::open(&insert_db).unwrap();
    s.insert_message(&msg("mbox", "hash-1", "hash payload"))
        .unwrap();
    drop(s);
    let inserted_hash: String = raw(&insert_db)
        .query_row(
            "SELECT content_hash FROM archive_messages WHERE external_id = 'hash-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    assert_eq!(
        migrated_hash.as_deref(),
        Some(inserted_hash.as_str()),
        "migration SQL and the insert path must agree on the hash formula"
    );
}

#[test]
fn cursor_only_namespace_keeps_its_checkpoint() {
    // A namespace whose only legacy footprint is an imap_sync_state row
    // (an account that synced cursors but zero messages) must still get a
    // source instance, so its cursor migrates instead of vanishing with
    // the checkpoint copy's inner join.
    let dir = scratch_dir();
    let db = dir.path().join("cursor-only.db");
    {
        let conn = Connection::open(&db).unwrap();
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
             INSERT INTO schema_migrations (version) VALUES (2);
             INSERT INTO imap_sync_state (source, folder, uidvalidity, last_uid)
                 VALUES ('imap:quiet@example.com', 'INBOX', 11, 55);",
        )
        .unwrap();
    }

    let s = Storage::open(&db).unwrap();
    let state = s
        .get_imap_state("imap:quiet@example.com", "INBOX")
        .unwrap()
        .expect("cursor-only namespace keeps its checkpoint");
    assert_eq!((state.uidvalidity, state.last_uid), (11, 55));

    // The instance exists and the cursor keeps advancing canonically.
    s.set_imap_state("imap:quiet@example.com", "INBOX", 11, 56)
        .unwrap();
    let state = s
        .get_imap_state("imap:quiet@example.com", "INBOX")
        .unwrap()
        .unwrap();
    assert_eq!(state.last_uid, 56);
    assert_eq!(
        raw(&db).query_row(
            "SELECT COUNT(*) FROM source_instances WHERE legacy_namespace = 'imap:quiet@example.com'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap(),
        1
    );
}

#[test]
fn v3_database_upgrades_and_carries_embeddings_to_canonical() {
    // A database exactly as the schema-3 release left it (embeddings bound
    // to the legacy messages table) upgrades cleanly: the canonical store
    // is built on top, and existing vectors carry over onto the migrated
    // archive_messages rows (ids preserved) instead of being lost.
    let dir = scratch_dir();
    let db = dir.path().join("v3.db");
    {
        let conn = Connection::open(&db).unwrap();
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
             INSERT INTO schema_migrations (version) VALUES (2);
             INSERT INTO schema_migrations (version) VALUES (3);
             CREATE TABLE message_embeddings (
                 message_id  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
                 model       TEXT    NOT NULL,
                 dim         INTEGER NOT NULL,
                 embedding   BLOB    NOT NULL,
                 embedded_at TEXT    NOT NULL DEFAULT (datetime('now'))
             );
             INSERT INTO messages (source, external_id, author, sent_at, body)
                 VALUES ('mbox', 'v3-1', 'ana', '2026-01-01T00:00:00Z', 'vintage v3 body');
             INSERT INTO message_embeddings (message_id, model, dim, embedding)
                 VALUES ((SELECT id FROM messages WHERE external_id = 'v3-1'),
                         'test-model', 4, x'7f000000');",
        )
        .unwrap();
    }

    let s = Storage::open(&db).unwrap();
    assert_eq!(s.schema_version().unwrap(), 15);
    let counts = s.embedding_counts("test-model").unwrap();
    assert_eq!(counts.embedded, 1, "the v3 vector survives the migration");
    assert_eq!(counts.messages, 1, "the legacy row migrated");
    let hits = s
        .vector_scan("test-model", 4, &[1.0, 0.0, 0.0, 0.0], None, None, 10)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_id, 1, "row id preserved");
    // New canonical writes and embeddings work on the retargeted table.
    let out = s
        .insert_message(&msg("mbox", "v3-2", "fresh canonical body"))
        .unwrap();
    assert!(matches!(out, InsertOutcome::Inserted(_)));
    let summary =
        vault_core::vector::run_embedding_job(&s, &vault_core::vector::HashEmbedder::new(), None)
            .unwrap();
    assert_eq!(summary.embedded, 2, "retargeted table accepts new vectors");
}

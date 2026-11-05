//! SQLite storage: schema migrations, idempotent inserts, and FTS5 search.
//!
//! The database bundles SQLite (via `rusqlite`'s `bundled` feature), so no
//! system SQLite is required. FTS5 is enabled in the bundled build.
//!
//! Since schema version 4 the store is **canonical**: every application
//! read and write targets the `source_instances`, `conversations`,
//! `archive_messages`, `message_identities`, `message_occurrences`, and
//! `sync_checkpoints` tables plus the `archive_fts` index. The legacy
//! `messages` / `messages_fts` / `imap_sync_state` tables from versions 1–2
//! are preserved untouched as the pre-canonical snapshot — migration copies
//! every row (ids included) into the canonical tables, and nothing writes
//! the legacy tables afterwards. Removing the legacy snapshot is deferred
//! until the recovery/upgrade experience justifies a separate migration.
//!
//! Schema version 7 (M1.3) adds **stable conversation keys** over the same
//! tables: `conversations.provider_key` holds a stable, provider-scoped
//! identity (`telegram:chat:<id>`, `whatsapp:chat:sha256:…`,
//! `signal:chat:<uuid>`, `email:thread:<root message id>`), the insert
//! path resolves conversations through it (then the email reply graph,
//! then the legacy `chat_id` grouping), and the time-provenance columns
//! record the verbatim stamp, the timezone it was interpreted in, and a
//! confidence label. What cannot be safely interpreted keeps the M1.1
//! `legacy_unresolved` state.
//!
//! Since schema version 8, source instances are **durable connector
//! entities** ([`Storage::ensure_source_instance`]): a connector (the IMAP
//! sync, [`crate::imap`]) registers its canonical configuration — host,
//! port, TLS mode, and exact username for IMAP — and re-registering the
//! same configuration resolves to the same instance, while credentials
//! stay out of the stored configuration entirely. Connector mail inserts
//! ([`Storage::insert_mail_message`]) scope identities to the instance and
//! preserve conflicting revisions of one `Message-ID` as separate archived
//! rows.
//!
//! Safe migration (M1.4, [`crate::safe_migration`]): opening an existing
//! database with pending migrations first refuses anything newer than
//! this build ([`Error::SchemaTooNew`]), then preflights free space,
//! snapshots the file, converts in a single transaction, validates the
//! indexes, and — on any failure — restores the original from the
//! snapshot (kept next to the database). See [`Storage::open_with`].

use std::path::{Path, PathBuf};

use chrono::{Datelike, Days, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};

use crate::message::{InsertOutcome, Message, NewMessage, SourceCount, Stats};
use crate::query::{
    ChatContext, SearchHit, SearchPage, SearchQuery, SNIPPET_CLOSE_MARK, SNIPPET_OPEN_MARK,
};
use crate::safe_migration::{self, MigrationFailPoint, OpenOptions};
use crate::stats::{ArchiveStats, AuthorCount, ChatCount, DayCount};
use crate::vector::VectorHit;
use crate::{Error, Result};

/// The identity fingerprint of an archive at a point in time: schema
/// version, archive revision, and the row counts that describe what the
/// vault holds. The M4.4 restore flow records it when a backup is
/// previewed (staged + validated) and refuses to commit the switch when
/// the live vault's fingerprint changed in between — a restore never
/// replaces a vault that moved under it ([`crate::backup`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultFingerprint {
    pub schema_version: i64,
    pub archive_revision: u64,
    pub messages: u64,
    pub sources: u64,
    pub media_objects: u64,
}

/// The newest schema version this build knows how to produce. Databases
/// recording a higher version are refused by [`Storage::open_with`]
/// instead of being silently opened (M1.4 newer-schema refusal); backup
/// restore applies the same ceiling to archives before switching
/// anything (M4.4).
pub const SUPPORTED_SCHEMA_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].0;
const MAX_SCHEMA_VERSION: i64 = SUPPORTED_SCHEMA_VERSION;

/// Ordered schema migrations; each entry runs exactly once. Since M1.4
/// every *pending* migration commits in one single transaction on open
/// (all or nothing), guarded by [`crate::safe_migration`]'s pre-migration
/// snapshot and rollback path.
///
/// Versions 1–2 are the frozen legacy schema; version 3 adds local
/// embeddings over the legacy table; version 4 adds the canonical store and
/// copies the legacy rows into it; version 11 seeds the periodic sync
/// schedule. Never edit an applied migration — append
/// a new version instead.
const MIGRATIONS: &[(i64, &str)] = &[
    (
        1,
        r#"
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
        "#,
    ),
    (
        2,
        r#"
        CREATE TABLE imap_sync_state (
            source      TEXT    NOT NULL,
            folder      TEXT    NOT NULL,
            uidvalidity INTEGER NOT NULL,
            last_uid    INTEGER NOT NULL DEFAULT 0,
            updated_at  TEXT    NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (source, folder)
        );
        "#,
    ),
    (
        3,
        r#"
        -- One embedding per message (PK), versioned by model id so a new
        -- embedder invalidates old vectors simply by writing a different
        -- model string. Vectors are i8-quantized (one byte per component)
        -- and L2-normalized at write time (see crate::vector). Deleting a
        -- message cascades.
        CREATE TABLE message_embeddings (
            message_id  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
            model       TEXT    NOT NULL,
            dim         INTEGER NOT NULL,
            embedding   BLOB    NOT NULL,
            embedded_at TEXT    NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX idx_message_embeddings_model ON message_embeddings (model);
        "#,
    ),
    (
        4,
        // Canonical store (M1.1). Additive only: the legacy tables above
        // stay exactly as earlier versions left them. The copies below run
        // in the same transaction as the DDL, so an interrupted migration
        // leaves no half-canonical database. `vault_uuid()` and
        // `sha256_hex()` are Rust-registered SQL functions (see
        // [`register_canonical_functions`]) — SQLite has no built-ins for
        // either.
        r#"
        CREATE TABLE source_instances (
            id               TEXT PRIMARY KEY,
            -- Legacy dedup namespace this instance stands for ("mbox",
            -- "imap:user@example.com", …). NULL once connector-owned
            -- source instances arrive; UNIQUE keeps the legacy namespace
            -- → instance mapping unambiguous.
            legacy_namespace TEXT UNIQUE,
            kind             TEXT NOT NULL,
            label            TEXT NOT NULL,
            -- Connector configuration, never credentials. M1.1 stores
            -- nothing here; the legacy namespace column carries the
            -- provenance M1.1 actually has.
            config_json      TEXT NOT NULL DEFAULT '{}',
            created_at       TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE conversations (
            id                 TEXT PRIMARY KEY,
            source_instance_id TEXT NOT NULL REFERENCES source_instances (id),
            -- Pre-canonical grouping key (the legacy chat_id within the
            -- source namespace). Stable provider keys arrive with the
            -- conversation-key slice; NULL then.
            legacy_chat_key    TEXT,
            provider_key       TEXT,
            label              TEXT NOT NULL DEFAULT '',
            kind               TEXT NOT NULL DEFAULT '',
            confidence         TEXT NOT NULL DEFAULT 'legacy_unresolved'
                CHECK (confidence IN ('verified', 'user_linked', 'legacy_unresolved')),
            created_at         TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (source_instance_id, legacy_chat_key),
            UNIQUE (source_instance_id, provider_key)
        );

        CREATE TABLE archive_messages (
            id                 INTEGER PRIMARY KEY,
            source_instance_id TEXT NOT NULL REFERENCES source_instances (id),
            conversation_id    TEXT REFERENCES conversations (id),
            -- Verbatim legacy payload: external key, channel, chat
            -- grouping, author, body, and source-specific JSON are copied
            -- unchanged so row IDs and IPC shapes stay stable.
            external_id        TEXT NOT NULL DEFAULT '',
            channel            TEXT NOT NULL DEFAULT '',
            chat_id            TEXT NOT NULL DEFAULT '',
            author             TEXT NOT NULL DEFAULT '',
            subject            TEXT NOT NULL DEFAULT '',
            body               TEXT NOT NULL DEFAULT '',
            raw_json           TEXT NOT NULL DEFAULT '{}',
            -- Time provenance: the resolved UTC instant (NULL when
            -- unknown), the original timestamp text as stored, the
            -- timezone it was interpreted in ('' = not recorded), and a
            -- confidence label. Legacy timestamps keep their values and
            -- carry 'legacy_assumed'.
            sent_instant       TEXT,
            original_timestamp TEXT NOT NULL DEFAULT '',
            timezone           TEXT NOT NULL DEFAULT '',
            time_confidence    TEXT NOT NULL
                CHECK (time_confidence IN ('explicit_offset', 'user_confirmed',
                                           'legacy_assumed', 'unresolved', 'missing')),
            -- SHA-256 fingerprint over the canonical field join (see
            -- [`content_hash`]); revision comparison builds on it later.
            content_hash       TEXT NOT NULL DEFAULT '',
            created_at         TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX idx_archive_messages_sent ON archive_messages (sent_instant);
        CREATE INDEX idx_archive_messages_source ON archive_messages (source_instance_id);
        CREATE INDEX idx_archive_messages_conversation ON archive_messages (conversation_id);
        CREATE INDEX idx_archive_messages_chat ON archive_messages (source_instance_id, chat_id);

        CREATE TABLE message_identities (
            source_instance_id TEXT NOT NULL REFERENCES source_instances (id),
            -- 'legacy' for the (source, external_id) aliases migrated
            -- from v1/v2 and still created by the legacy ingest path;
            -- connector-scoped namespaces arrive with the source-instance
            -- slice.
            namespace          TEXT NOT NULL,
            external_key       TEXT NOT NULL,
            revision_hash      TEXT NOT NULL DEFAULT '',
            message_id         INTEGER NOT NULL REFERENCES archive_messages (id)
                ON DELETE CASCADE,
            created_at         TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (source_instance_id, namespace, external_key, revision_hash)
        );
        CREATE INDEX idx_message_identities_message ON message_identities (message_id);

        CREATE TABLE message_occurrences (
            message_id       INTEGER NOT NULL REFERENCES archive_messages (id)
                ON DELETE CASCADE,
            -- What observed the message: 'legacy-import' for rows carried
            -- over by this migration, 'import' for current-path inserts;
            -- remote folders arrive with the occurrence slice.
            scope            TEXT NOT NULL,
            provider_item_id TEXT NOT NULL DEFAULT '',
            uidvalidity      INTEGER,
            observed_at      TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (message_id, scope, provider_item_id)
        );
        CREATE INDEX idx_message_occurrences_message ON message_occurrences (message_id);

        CREATE TABLE sync_checkpoints (
            source_instance_id TEXT NOT NULL REFERENCES source_instances (id),
            folder             TEXT NOT NULL,
            uidvalidity        INTEGER NOT NULL,
            last_uid           INTEGER NOT NULL DEFAULT 0,
            updated_at         TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (source_instance_id, folder)
        );

        -- One unresolved legacy source instance per namespace present in
        -- either legacy table — messages or imap_sync_state (an account
        -- may hold cursors with zero synced messages). The old schema
        -- never recorded account/import provenance, and it is not guessed
        -- here (users bind real accounts explicitly later).
        INSERT INTO source_instances (id, legacy_namespace, kind, label)
        SELECT vault_uuid(), source, 'legacy', source
        FROM (SELECT source FROM messages UNION SELECT source FROM imap_sync_state);

        -- One legacy-unresolved conversation per non-empty
        -- (source, chat_id) group; MIN(channel) keeps the choice
        -- deterministic if a group ever mixed channels.
        INSERT INTO conversations (id, source_instance_id, legacy_chat_key, label, kind)
        SELECT vault_uuid(), si.id, g.chat_id, g.chat_id, g.channel
        FROM (
            SELECT source, chat_id, MIN(channel) AS channel
            FROM messages
            WHERE chat_id != ''
            GROUP BY source, chat_id
        ) g
        JOIN source_instances si ON si.legacy_namespace = g.source;

        -- Canonical message rows: same ids, verbatim payload, time
        -- provenance derived from the stored instant.
        INSERT INTO archive_messages (
            id, source_instance_id, conversation_id,
            external_id, channel, chat_id, author, subject, body, raw_json,
            sent_instant, original_timestamp, timezone, time_confidence,
            content_hash, created_at)
        SELECT
            m.id, si.id, cv.id,
            m.external_id, m.channel, m.chat_id, m.author, '', m.body, m.raw_json,
            NULLIF(m.sent_at, ''), m.sent_at, '',
            CASE WHEN m.sent_at = '' THEN 'missing' ELSE 'legacy_assumed' END,
            sha256_hex(m.external_id || char(31) || m.author || char(31) || m.sent_at
                       || char(31) || m.body),
            m.created_at
        FROM messages m
        JOIN source_instances si ON si.legacy_namespace = m.source
        LEFT JOIN conversations cv
               ON cv.source_instance_id = si.id AND cv.legacy_chat_key = m.chat_id;

        -- Alias identity for every legacy (source, external_id).
        INSERT INTO message_identities
            (source_instance_id, namespace, external_key, message_id, created_at)
        SELECT si.id, 'legacy', m.external_id, m.id, m.created_at
        FROM messages m
        JOIN source_instances si ON si.legacy_namespace = m.source;

        -- One occurrence per archived message recording the legacy import
        -- observation.
        INSERT INTO message_occurrences (message_id, scope, provider_item_id, observed_at)
        SELECT m.id, 'legacy-import', m.external_id, m.created_at
        FROM messages m;

        -- IMAP cursors move to the canonical checkpoint table, keyed by
        -- source instance; the legacy cursor table is left as a snapshot.
        INSERT INTO sync_checkpoints (source_instance_id, folder, uidvalidity, last_uid, updated_at)
        SELECT si.id, s.folder, s.uidvalidity, s.last_uid, s.updated_at
        FROM imap_sync_state s
        JOIN source_instances si ON si.legacy_namespace = s.source;

        -- Canonical FTS over body/author/subject, kept in sync by
        -- triggers; created after the copy so the one-time 'rebuild'
        -- below indexes the migrated rows in a single pass.
        CREATE VIRTUAL TABLE archive_fts USING fts5 (
            body,
            author,
            subject,
            content = 'archive_messages',
            content_rowid = 'id'
        );
        CREATE TRIGGER archive_fts_ai AFTER INSERT ON archive_messages BEGIN
            INSERT INTO archive_fts (rowid, body, author, subject)
            VALUES (new.id, new.body, new.author, new.subject);
        END;
        CREATE TRIGGER archive_fts_ad AFTER DELETE ON archive_messages BEGIN
            INSERT INTO archive_fts (archive_fts, rowid, body, author, subject)
            VALUES ('delete', old.id, old.body, old.author, old.subject);
        END;
        CREATE TRIGGER archive_fts_au AFTER UPDATE ON archive_messages BEGIN
            INSERT INTO archive_fts (archive_fts, rowid, body, author, subject)
            VALUES ('delete', old.id, old.body, old.author, old.subject);
            INSERT INTO archive_fts (rowid, body, author, subject)
            VALUES (new.id, new.body, new.author, new.subject);
        END;

        INSERT INTO archive_fts (archive_fts) VALUES ('rebuild');

        -- message_embeddings now binds to the canonical table: existing
        -- vectors from a schema-v3 database carry over verbatim (row ids
        -- are preserved by the copy above). Rebuilt in place via the
        -- create-copy-drop-rename sequence.
        CREATE TABLE message_embeddings_canonical (
            message_id  INTEGER PRIMARY KEY REFERENCES archive_messages(id) ON DELETE CASCADE,
            model       TEXT    NOT NULL,
            dim         INTEGER NOT NULL,
            embedding   BLOB    NOT NULL,
            embedded_at TEXT    NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO message_embeddings_canonical
            (message_id, model, dim, embedding, embedded_at)
        SELECT message_id, model, dim, embedding, embedded_at
        FROM message_embeddings;
        DROP TABLE message_embeddings;
        ALTER TABLE message_embeddings_canonical RENAME TO message_embeddings;
        CREATE INDEX idx_message_embeddings_model ON message_embeddings (model);
        "#,
    ),
    (
        5,
        r#"
        CREATE TABLE watched_folders (
            path     TEXT PRIMARY KEY,
            added_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        "#,
    ),
    (
        6,
        // Materialized counters for the statistics dashboard, maintained by
        // triggers on every write so reads stay O(number of groups) instead
        // of GROUP BY scans over every archive row — the dashboard answers
        // within its 100 ms budget on million-message archives (asserted by
        // tests/stats_perf.rs). The `''` day (unknown sent_instant) and `''`
        // chat_id groups are stored too and filtered on read, so the
        // counters always sum to COUNT(*).
        r#"
        CREATE TABLE stats_source (
            source TEXT PRIMARY KEY,
            n      INTEGER NOT NULL
        );
        CREATE TABLE stats_day (
            day    TEXT PRIMARY KEY,
            n      INTEGER NOT NULL
        );
        CREATE TABLE stats_chat (
            source  TEXT NOT NULL,
            chat_id TEXT NOT NULL,
            n       INTEGER NOT NULL,
            PRIMARY KEY (source, chat_id)
        );
        CREATE TABLE stats_author (
            author TEXT PRIMARY KEY,
            n      INTEGER NOT NULL
        );
        -- Top-N reads (ORDER BY n DESC ... LIMIT) stream off these.
        CREATE INDEX idx_stats_chat_n ON stats_chat (n DESC, source, chat_id);
        CREATE INDEX idx_stats_author_n ON stats_author (n DESC, author);

        CREATE TRIGGER stats_ai AFTER INSERT ON archive_messages BEGIN
            INSERT INTO stats_source (source, n) VALUES (NEW.source_instance_id, 1)
                ON CONFLICT (source) DO UPDATE SET n = n + 1;
            INSERT INTO stats_day (day, n) VALUES (COALESCE(substr(NEW.sent_instant, 1, 10), ''), 1)
                ON CONFLICT (day) DO UPDATE SET n = n + 1;
            INSERT INTO stats_chat (source, chat_id, n) VALUES (NEW.source_instance_id, NEW.chat_id, 1)
                ON CONFLICT (source, chat_id) DO UPDATE SET n = n + 1;
            INSERT INTO stats_author (author, n) VALUES (NEW.author, 1)
                ON CONFLICT (author) DO UPDATE SET n = n + 1;
        END;
        CREATE TRIGGER stats_ad AFTER DELETE ON archive_messages BEGIN
            INSERT INTO stats_source (source, n) VALUES (OLD.source_instance_id, -1)
                ON CONFLICT (source) DO UPDATE SET n = n - 1;
            INSERT INTO stats_day (day, n) VALUES (COALESCE(substr(OLD.sent_instant, 1, 10), ''), -1)
                ON CONFLICT (day) DO UPDATE SET n = n - 1;
            INSERT INTO stats_chat (source, chat_id, n) VALUES (OLD.source_instance_id, OLD.chat_id, -1)
                ON CONFLICT (source, chat_id) DO UPDATE SET n = n - 1;
            INSERT INTO stats_author (author, n) VALUES (OLD.author, -1)
                ON CONFLICT (author) DO UPDATE SET n = n - 1;
        END;
        -- The app only ever inserts archive rows, but the counters track
        -- UPDATEs too so the invariant (sums == COUNT(*)) holds under any
        -- write path. Fires only when a counter-relevant column changes.
        CREATE TRIGGER stats_au AFTER UPDATE OF source_instance_id, chat_id, author, sent_instant ON archive_messages BEGIN
            INSERT INTO stats_source (source, n) VALUES (OLD.source_instance_id, -1)
                ON CONFLICT (source) DO UPDATE SET n = n - 1;
            INSERT INTO stats_day (day, n) VALUES (COALESCE(substr(OLD.sent_instant, 1, 10), ''), -1)
                ON CONFLICT (day) DO UPDATE SET n = n - 1;
            INSERT INTO stats_chat (source, chat_id, n) VALUES (OLD.source_instance_id, OLD.chat_id, -1)
                ON CONFLICT (source, chat_id) DO UPDATE SET n = n - 1;
            INSERT INTO stats_author (author, n) VALUES (OLD.author, -1)
                ON CONFLICT (author) DO UPDATE SET n = n - 1;
            INSERT INTO stats_source (source, n) VALUES (NEW.source_instance_id, 1)
                ON CONFLICT (source) DO UPDATE SET n = n + 1;
            INSERT INTO stats_day (day, n) VALUES (COALESCE(substr(NEW.sent_instant, 1, 10), ''), 1)
                ON CONFLICT (day) DO UPDATE SET n = n + 1;
            INSERT INTO stats_chat (source, chat_id, n) VALUES (NEW.source_instance_id, NEW.chat_id, 1)
                ON CONFLICT (source, chat_id) DO UPDATE SET n = n + 1;
            INSERT INTO stats_author (author, n) VALUES (NEW.author, 1)
                ON CONFLICT (author) DO UPDATE SET n = n + 1;
        END;

        -- Backfill pre-existing archives from the canonical rows the
        -- earlier migrations just populated (fresh databases are empty).
        INSERT INTO stats_source (source, n)
            SELECT source_instance_id, COUNT(*) FROM archive_messages GROUP BY source_instance_id;
        INSERT INTO stats_day (day, n)
            SELECT COALESCE(substr(sent_instant, 1, 10), ''), COUNT(*) FROM archive_messages GROUP BY 1;
        INSERT INTO stats_chat (source, chat_id, n)
            SELECT source_instance_id, chat_id, COUNT(*) FROM archive_messages GROUP BY 1, 2;
        INSERT INTO stats_author (author, n)
            SELECT author, COUNT(*) FROM archive_messages GROUP BY author;
        "#,
    ),
    (
        7,
        // Stable conversation keys (M1.3). The schema itself is already
        // sufficient (M1.1 added `conversations.provider_key` and the
        // time-provenance columns in anticipation); this migration is a
        // pure data backfill, implemented in Rust
        // (`backfill_conversation_keys`) because it needs deterministic
        // aggregation SQL cannot express:
        //
        // - Telegram and Signal conversations derive their provider key
        //   from the archived messages' `<chat-id>/<message-id>` external
        //   ids (the provider-guaranteed chat id); conversations that
        //   collapse onto the same key (a chat re-imported under a
        //   rename) merge into one.
        // - WhatsApp conversations derive a content key (sorted
        //   participants + first authored message, the same frozen
        //   formula as the parser), merge onto it, and every WhatsApp
        //   message identity is re-keyed to the new hash formula so
        //   re-imports after a rename dedup instead of duplicating;
        //   the legacy blind-UTC stamp is re-resolved in the machine's
        //   local zone and the resolved instant, zone, and confidence
        //   stored alongside.
        // - What cannot be safely interpreted stays legacy-unresolved:
        //   exports without a provider id (id-less Telegram, uuid-less
        //   Signal), author-less WhatsApp exports, and mail archived
        //   before threading headers were recorded.
        "",
    ),
    (
        8,
        // Durable source instances (M1.2). Additive: connector-owned
        // instances gain a canonical connector identity key (unique per
        // (kind, connector_key), e.g. IMAP host/port/TLS/username), a
        // connected/disconnected state, and last attempt/success stamps.
        // Credentials are never part of the key or the stored config —
        // they live in the OS keyring (see the Tauri shell's `secrets`
        // module). Legacy instances keep `connector_key` NULL and never
        // match a connector: binding a legacy namespace to a configured
        // account is an explicit user action, never inferred here.
        r#"
        ALTER TABLE source_instances ADD COLUMN connector_key TEXT;
        ALTER TABLE source_instances ADD COLUMN state TEXT NOT NULL DEFAULT 'disconnected'
            CHECK (state IN ('connected', 'disconnected'));
        ALTER TABLE source_instances ADD COLUMN last_attempt_at TEXT;
        ALTER TABLE source_instances ADD COLUMN last_success_at TEXT;
        CREATE UNIQUE INDEX idx_source_instances_connector
            ON source_instances (kind, connector_key)
            WHERE connector_key IS NOT NULL;
        "#,
    ),
    (
        9,
        // The IMAP coordinator's durable accounting (M2.3). Additive:
        // `sync_failed_items` is the failed-item ledger — one row per
        // fetched item the vault could not store (unparseable mail, a
        // fetch that never arrived), with the reason and attempt count,
        // so "what fell and why" stays visible across restarts. Keyed by
        // the full (instance, folder, uid, uidvalidity) tuple: a
        // UIDVALIDITY reset starts a fresh namespace, and the ledger rows
        // of an abandoned validity are cleared when the checkpoint adopts
        // the new one (the rescan re-derives the truth for those UIDs).
        r#"
        CREATE TABLE sync_failed_items (
            source_instance_id TEXT NOT NULL REFERENCES source_instances (id),
            folder             TEXT NOT NULL,
            uid                INTEGER NOT NULL,
            uidvalidity        INTEGER NOT NULL,
            -- Fixed human-readable cause; never message content.
            reason             TEXT NOT NULL,
            attempts           INTEGER NOT NULL DEFAULT 1,
            first_failed_at    TEXT NOT NULL DEFAULT (datetime('now')),
            last_failed_at     TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (source_instance_id, folder, uid, uidvalidity)
        );
        ALTER TABLE source_instances ADD COLUMN excluded INTEGER NOT NULL DEFAULT 0;
        "#,
    ),
    (
        10,
        // Bounded ingest recovery (M2.2). `import_checkpoints` holds the
        // committed item position of an in-flight bounded import so an
        // interrupted run resumes from the last committed position
        // instead of re-parsing or duplicating; the row is deleted in the
        // same transaction that completes the import, so only genuinely
        // interrupted imports leave a checkpoint behind. `import_issues`
        // are the durable per-item parse failures (one row per failed
        // item, with its stream position): they outlive the checkpoint
        // because they are the record of what could not be archived,
        // queryable through `Storage::import_issues`.
        r#"
        CREATE TABLE import_checkpoints (
            import_id       TEXT PRIMARY KEY,
            source          TEXT NOT NULL,
            committed_items INTEGER NOT NULL DEFAULT 0,
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE import_issues (
            id           INTEGER PRIMARY KEY,
            import_id    TEXT NOT NULL,
            -- Ordinal of the failed item in the import's item stream
            -- (messages + failures; silently skipped entries do not
            -- count), so the record points back at the failing entry.
            position     INTEGER NOT NULL,
            external_ref TEXT NOT NULL DEFAULT '',
            reason       TEXT NOT NULL,
            recorded_at  TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE UNIQUE INDEX idx_import_issues_import ON import_issues (import_id, position);
        "#,
    ),
    (
        11,
        // The periodic sync scheduler's persisted state (M2.4). A
        // singleton row: `enabled` is the user's opt-in (off until they
        // turn it on), `interval_ms` the period between scheduled
        // coordinator rounds in milliseconds (kept inside the
        // scheduler's interval bounds by its setters; the row is seeded
        // with the defaults so reads never branch). Runtime state — the
        // single deadline and the active round's job id — is
        // process-local by design: a fresh start re-arms one interval
        // out and never inherits stale deadlines (missed time cannot
        // queue missed rounds).
        r#"
        CREATE TABLE sync_schedule (
            id           INTEGER PRIMARY KEY CHECK (id = 1),
            enabled      INTEGER NOT NULL DEFAULT 0,
            interval_ms  INTEGER NOT NULL DEFAULT 900000,
            updated_at   TEXT    NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO sync_schedule (id) VALUES (1);
        "#,
    ),
    (
        12,
        // The v2 search surface (M3.2). Two additive pieces:
        //
        // - `archive_meta` is a small key/value table for vault-level
        //   metadata that must survive restarts without a schema bump;
        //   `archive_revision` is incremented inside every
        //   content-changing transaction (message inserts, media
        //   backfills, future deletes) so `search_archive_v2` pages can
        //   report "the archive changed under your pagination" instead
        //   of silently restarting it (A07).
        // - `message_occurrences.import_id` records which bounded import
        //   run (the stable `import_id` M2.2 already keys checkpoints and
        //   issues on) first observed the message, powering the
        //   "View imported messages" import-scope filter. Pre-existing
        //   occurrences keep `''` (provenance was never recorded); the
        //   first import wins, exactly like the occurrence tuple itself.
        r#"
        CREATE TABLE archive_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        INSERT INTO archive_meta (key, value) VALUES ('archive_revision', '0');
        ALTER TABLE message_occurrences
            ADD COLUMN import_id TEXT NOT NULL DEFAULT '';
        CREATE INDEX idx_message_occurrences_import
            ON message_occurrences (import_id);
        "#,
    ),
    (
        13,
        // The media object registry (M4.2). Message-linked objects are
        // referenced by their `raw_json` `media` entries; unlinked export
        // files — stored by whole-tree imports for files that belong to
        // no message — have no message to reference them, so they get a
        // registry row instead: their digest, size, and provenance.
        // `media_digests` unions both sides, so reconciliation counts
        // unlinked objects as referenced (a clean import with unlinked
        // files stays reconciliation-clean), and the PRIMARY KEY makes
        // re-registration idempotent: re-importing the same export
        // stores no second object and no second row.
        r#"
        CREATE TABLE media_objects (
            sha256     TEXT PRIMARY KEY,
            size       INTEGER NOT NULL,
            name       TEXT    NOT NULL DEFAULT '',
            kind       TEXT    NOT NULL DEFAULT '',
            -- How the object entered the vault ('unlinked' for M4.2
            -- whole-tree imports; message-linked objects stay
            -- unregistered — their messages are the reference).
            origin     TEXT    NOT NULL DEFAULT '',
            import_id  TEXT    NOT NULL DEFAULT '',
            first_seen TEXT    NOT NULL DEFAULT (datetime('now'))
        );
        "#,
    ),
];

/// Rust halves of migrations, for backfill logic that SQL alone cannot
/// express faithfully (deterministic aggregation over archived rows).
/// Each hook runs inside the same transaction as its SQL half, right
/// after it. As with `MIGRATIONS`, never edit an applied version — add
/// a new one.
type RustMigration = fn(&Connection) -> Result<()>;
const RUST_MIGRATIONS: &[(i64, RustMigration)] = &[(7, backfill_conversation_keys)];

/// Handle to the vault database.
///
/// `rusqlite`'s `Connection` is `Send` but not `Sync`; share one handle
/// across threads behind a `Mutex`, or — what the app does since M2.1 —
/// open several connections over one file through [`crate::jobs::DbPool`]
/// (a dedicated writer thread plus read connections, WAL mode).
#[derive(Debug)]
pub struct Storage {
    /// Private to this module except the v2 query module
    /// ([`crate::search2`]), which owns the v2 IPC read paths.
    pub(crate) conn: Connection,
}

impl Storage {
    /// Open (and migrate) a database file, creating it when missing.
    ///
    /// Existing files with pending migrations go through the M1.4
    /// safe-migration path (snapshot, preflight, rollback — see
    /// [`crate::safe_migration`]); for fault-injection tests use
    /// [`Storage::open_with`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, &OpenOptions::default())
    }

    /// [`Storage::open`] with explicit migration-safety options (failure
    /// injection for the integrity suite; production callers use
    /// [`Storage::open`]).
    pub fn open_with(path: impl AsRef<Path>, opts: &OpenOptions) -> Result<Self> {
        let path: &Path = path.as_ref();
        // A durable failure marker means a previous conversion failed its
        // post-migration validation here: fail-closed must terminate, so
        // refuse immediately with that recorded reason instead of
        // re-attempting the same deterministic failure forever.
        if let Some((reason, snapshot)) = safe_migration::read_failed_marker(path)? {
            return Err(Error::MigrationFailed { reason, snapshot });
        }
        let existed = path.try_exists().map_err(Error::Io)?;
        let conn = Connection::open(path)?;
        // Version probe before anything can touch the file: a newer
        // schema must be refused with zero writes (not even pragmas).
        let current = read_schema_version(&conn)?;
        if current > MAX_SCHEMA_VERSION {
            return Err(Error::SchemaTooNew {
                db: current,
                supported: MAX_SCHEMA_VERSION,
            });
        }
        // Only an existing archive with pending conversions needs a
        // snapshot: a fresh file has nothing to lose, and an up-to-date
        // file has nothing to convert.
        let snapshot: Option<PathBuf> = if existed && current < MAX_SCHEMA_VERSION {
            Some(safe_migration::take_snapshot(&conn, path, current, opts)?)
        } else {
            None
        };
        match Self::init_with(conn, opts, existed.then_some(path), snapshot.as_deref()) {
            Ok(storage) => {
                // The conversion committed and validated; the snapshot's
                // job is done.
                if let Some(snap) = &snapshot {
                    safe_migration::remove_snapshot(snap);
                }
                Ok(storage)
            }
            Err(e) => Err(e),
        }
    }

    /// Open a private in-memory database (used by tests and demos).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_with(conn, &OpenOptions::default(), None, None)
    }

    /// Configure pragmas/functions, migrate, and validate — restoring
    /// from the pre-migration snapshot when anything fails after the
    /// snapshot was taken (M1.4 rollback path).
    fn init_with(
        conn: Connection,
        opts: &OpenOptions,
        db_path: Option<&Path>,
        snapshot: Option<&Path>,
    ) -> Result<Self> {
        let converted = Self::configure_and_migrate(&conn, opts);
        match converted {
            Ok(()) => Ok(Self { conn }),
            Err(e) => {
                // A failed post-conversion validation is deterministic —
                // the conversion committed but the converted database is
                // provably bad — so it is recorded durably and every
                // later open refuses instead of re-running it.
                let validation_failed = matches!(e, Error::MigrationValidationFailed(_));
                if let Some((db, snap)) = db_path.zip(snapshot) {
                    // Close the connection before touching its file, then
                    // put the pre-migration original back. The snapshot
                    // itself is kept as the reopen path.
                    drop(conn);
                    let restored = safe_migration::restore_from_snapshot(db, snap);
                    // Record the durable marker whether or not the restore
                    // itself succeeded.
                    if validation_failed {
                        let _ = safe_migration::write_failed_marker(db, &e.to_string(), snap);
                    }
                    if let Err(restore) = restored {
                        return Err(Error::UnsafeMigration(format!(
                            "{e}; restoring the pre-migration original also failed ({restore}) \
                             — the snapshot copy is kept at {}",
                            snap.display()
                        )));
                    }
                }
                Err(e)
            }
        }
    }

    fn configure_and_migrate(conn: &Connection, opts: &OpenOptions) -> Result<()> {
        // journal_mode returns a row (the new mode), so read-and-ignore it
        // via query_row; synchronous/foreign_keys return no rows and need
        // pragma_update.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Every connection initializes itself (M2.1: one writer + read
        // connections share the file), so each one also learns to wait
        // out the other side's brief locks instead of failing fast with
        // SQLITE_BUSY — WAL readers and the writer rarely contend, but a
        // checkpoint overlapped with a write would otherwise surface as
        // an immediate error on one of them.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        register_fold_function(conn)?;
        register_canonical_functions(conn)?;
        migrate(conn, opts)
    }

    /// Insert one message into the canonical store, idempotently on its
    /// identity alias (legacy `(source, external_id)`, namespace
    /// `'legacy'`).
    ///
    /// Inserting a message whose identity already exists is not an error:
    /// the archived row is left untouched and `InsertOutcome::Duplicate`
    /// is returned with its id. A new row lands in `archive_messages`
    /// together with its identity alias and a first `import` occurrence —
    /// the legacy `messages` table is never written (it is a read-only
    /// snapshot since schema version 4).
    ///
    /// M1.3: the conversation resolves through the message's stable
    /// [`NewMessage::conversation_key`] when present (one conversation per
    /// provider chat, label following the newest display name), through
    /// the email reply graph ([`NewMessage::reply_refs`]) for mail, and
    /// through the legacy `chat_id` grouping otherwise. Time provenance
    /// columns come from the message's explicit fields when set.
    pub fn insert_message(&self, msg: &NewMessage) -> Result<InsertOutcome> {
        let tx = self.conn.unchecked_transaction()?;
        let outcome = insert_message_in_tx(&tx, "", msg)?;
        bump_archive_revision(&tx)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Commit one bounded-import batch atomically (M2.2): message rows
    /// (with the same dedup/media-link semantics as
    /// [`Storage::insert_message`] followed by
    /// [`Storage::attach_media`]), durable issue records for the batch's
    /// failed items, and the import checkpoint advancement — all in one
    /// transaction, so an interruption leaves either the whole batch or
    /// none of it, and the checkpoint can never claim items that were
    /// not committed.
    ///
    /// `committed_after` is the number of items whose fate is durable
    /// once this batch lands (messages + recorded failures consumed so
    /// far); `completes` marks the import finished, deleting its
    /// checkpoint row in the same transaction (a completed import needs
    /// no resume point; re-running it starts fresh and dedups).
    #[allow(clippy::too_many_arguments)]
    pub fn commit_import_batch(
        &self,
        import_id: &str,
        source: &str,
        messages: &[NewMessage],
        issues: &[ImportIssueRecord<'_>],
        media_objects: &[crate::media::UnlinkedObject],
        committed_after: u64,
        completes: bool,
    ) -> Result<ImportBatchCounts> {
        let tx = self.conn.unchecked_transaction()?;
        let mut counts = ImportBatchCounts::default();
        for msg in messages {
            match insert_message_in_tx(&tx, import_id, msg)? {
                InsertOutcome::Inserted(_) => {
                    counts.inserted += 1;
                    if !media_entries(&msg.raw_json).is_empty() {
                        counts.media_linked += 1;
                    }
                }
                InsertOutcome::Duplicate(id) => {
                    counts.skipped += 1;
                    let entries = media_entries(&msg.raw_json);
                    if !entries.is_empty() && attach_media_in_tx(&tx, id, &entries)? {
                        counts.media_linked += 1;
                    }
                }
            }
        }
        for issue in issues {
            tx.execute(
                "INSERT INTO import_issues (import_id, position, external_ref, reason)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (import_id, position) DO NOTHING",
                params![import_id, issue.position, issue.external_ref, issue.reason],
            )?;
        }
        // Unlinked objects (M4.2): registered idempotently on the digest —
        // a re-import of the same tree registers nothing new, and an
        // object some message also references keeps the registry row as
        // redundant provenance (harmless: reconciliation unions sides).
        for object in media_objects {
            tx.execute(
                "INSERT OR IGNORE INTO media_objects
                     (sha256, size, name, kind, origin, import_id)
                 VALUES (?1, ?2, ?3, ?4, 'unlinked', ?5)",
                params![
                    object.sha256,
                    object.size,
                    object.name,
                    object.kind,
                    import_id
                ],
            )?;
        }
        if completes {
            tx.execute(
                "DELETE FROM import_checkpoints WHERE import_id = ?1",
                params![import_id],
            )?;
        } else {
            tx.execute(
                "INSERT INTO import_checkpoints (import_id, source, committed_items, updated_at)
                 VALUES (?1, ?2, ?3, datetime('now'))
                 ON CONFLICT(import_id) DO UPDATE SET
                    committed_items = excluded.committed_items,
                    updated_at     = excluded.updated_at",
                params![import_id, source, committed_after],
            )?;
        }
        // Any committed batch can change what a paginated search sees:
        // bump the revision in the same transaction (A07 refresh signal).
        bump_archive_revision(&tx)?;
        tx.commit()?;
        Ok(counts)
    }

    /// The committed item count of an interrupted bounded import, if one
    /// is recorded for `import_id` (see [`Storage::commit_import_batch`]).
    pub fn import_checkpoint(&self, import_id: &str) -> Result<Option<u64>> {
        let count = self
            .conn
            .query_row(
                "SELECT committed_items FROM import_checkpoints WHERE import_id = ?1",
                params![import_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(count.map(|c| c.max(0) as u64))
    }

    /// One import's durable issue records (parse failures), paged by
    /// position, oldest first.
    pub fn import_issues(
        &self,
        import_id: &str,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<ImportIssue>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, import_id, position, external_ref, reason, recorded_at
             FROM import_issues
             WHERE import_id = ?1
             ORDER BY position
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(
            params![
                import_id,
                limit.min(i64::MAX as u64) as i64,
                offset.min(i64::MAX as u64) as i64
            ],
            |row| {
                Ok(ImportIssue {
                    id: row.get(0)?,
                    import_id: row.get(1)?,
                    position: row.get::<_, i64>(2)?.max(0) as u64,
                    external_ref: row.get(3)?,
                    reason: row.get(4)?,
                    recorded_at: row.get(5)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// How many issue records one import recorded.
    pub fn import_issue_count(&self, import_id: &str) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM import_issues WHERE import_id = ?1",
            params![import_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u64)
    }

    // ------------------------------------------------------------------
    // Durable source instances (M1.2)
    // ------------------------------------------------------------------

    /// Find or create the durable source instance for a connector
    /// configuration, idempotently on `(kind, connector_key)`.
    ///
    /// The connector key is the canonical identity the connector itself
    /// derives from its configuration (IMAP: case-folded host, port, TLS
    /// mode, exact username — see [`crate::imap::ImapConfig::connector_key`]);
    /// credentials are never part of it, so reconfiguring a password
    /// resolves to the same instance. Re-registering an existing instance
    /// refreshes its label and stored configuration (non-identity fields
    /// such as the folder list) without touching its state or history.
    /// Legacy-named instances never match a connector: they keep
    /// `connector_key` NULL until a user explicitly binds them.
    pub fn ensure_source_instance(&self, spec: &NewSourceInstance<'_>) -> Result<String> {
        let tx = self.conn.unchecked_transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM source_instances
                 WHERE kind = ?1 AND connector_key = ?2",
                params![spec.kind, spec.connector_key],
                |row| row.get(0),
            )
            .optional()?;
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE source_instances SET label = ?1, config_json = ?2
                     WHERE id = ?3",
                    params![spec.label, spec.config_json, id],
                )?;
                id
            }
            None => {
                let id = new_uuid();
                tx.execute(
                    "INSERT INTO source_instances
                         (id, kind, label, connector_key, config_json)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        id,
                        spec.kind,
                        spec.label,
                        spec.connector_key,
                        spec.config_json
                    ],
                )?;
                id
            }
        };
        tx.commit()?;
        Ok(id)
    }

    /// Every source instance in the vault, oldest first: legacy-named
    /// ones (connector key `None`) and connector-owned ones. The returned
    /// configuration never contains credentials.
    pub fn list_source_instances(&self) -> Result<Vec<SourceInstance>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, label, connector_key, config_json, state,
                    excluded, last_attempt_at, last_success_at, created_at
             FROM source_instances ORDER BY created_at, id",
        )?;
        let rows = stmt
            .query_map([], source_instance_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Stamp a sync attempt on a source instance (`last_attempt_at`).
    /// Unknown ids are a no-op: the caller just ensured the instance, and
    /// attempt stamps must never fail a sync.
    pub fn record_source_attempt(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE source_instances SET last_attempt_at = datetime('now')
             WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Stamp a successful sync on a source instance and mark it connected
    /// (`last_success_at`, `state = 'connected'`).
    pub fn record_source_success(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE source_instances
             SET last_success_at = datetime('now'), state = 'connected'
             WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Mark a source instance disconnected. Archived content and cursors
    /// are kept — disconnecting an account never deletes its archive.
    pub fn disconnect_source_instance(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE source_instances SET state = 'disconnected' WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Insert one mail message through its connector, with the identity
    /// scoped to a durable source instance and the first folder/import
    /// occurrence.
    ///
    /// Dedup is on the full identity tuple
    /// `(instance, namespace, external_key, revision_hash)`: an exact
    /// repeat is [`MailInsertOutcome::Duplicate`] (only the new occurrence
    /// is recorded — the same mail seen in another folder enriches the
    /// provenance without duplicating the row). A different decoded
    /// payload sharing the same external key (e.g. two conflicting
    /// revisions of one `Message-ID`) becomes a **new archived row**: both
    /// revisions stay searchable, and the outcome flags the conflict so
    /// callers can count it. First write wins for the archived row's
    /// fields; later revisions never rewrite an earlier one.
    pub fn insert_mail_message(
        &self,
        instance_id: &str,
        msg: &NewMessage,
        subject: &str,
        identity: &ConnectorIdentity<'_>,
        occurrence: &Occurrence<'_>,
    ) -> Result<MailInsertOutcome> {
        let tx = self.conn.unchecked_transaction()?;
        let outcome =
            Self::insert_mail_message_tx(&tx, instance_id, msg, subject, identity, occurrence)?;
        bump_archive_revision(&tx)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Commit one IMAP fetch batch atomically (M2.3): the batch's mail
    /// inserts, its failed-item ledger updates, and the folder checkpoint
    /// advance all land in **one transaction**, so an interruption can
    /// never leave the cursor disagreeing with what is durably stored
    /// (the pre-M2.3 shape — per-message commits plus a separate
    /// checkpoint write — could only fall back on refetch-dedup after a
    /// crash in that window).
    ///
    /// - `mails`: the batch's parsed messages, inserted through the full
    ///   [`Storage::insert_mail_message`] contract (dedup, occurrence,
    ///   revisions) inside the batch transaction.
    /// - `failures`: items the round observed but could not store (uid →
    ///   fixed reason). Ledger rows upsert (attempt count + last time
    ///   bump on repeat); the cursor may advance past them because the
    ///   ledger — not silence — remembers them, and every later round
    ///   retries them (see [`Storage::imap_failed_uids`]).
    /// - `resolved`: previously-failed uids this batch stored fine; their
    ///   ledger rows are deleted here, in the same transaction.
    ///
    /// When `uidvalidity` differs from the folder's stored checkpoint the
    /// commit adopts the new validity and clears the folder's ledger rows
    /// from abandoned validities (the reset rescan re-derives them).
    /// `last_uid` is the checkpoint value to persist with this batch
    /// (typically the batch's highest accounted uid).
    // The flat argument list mirrors the batch's wire shape (folder,
    // validity, cursor, three payload slices) — the same call frame the
    // sync carries per fetch batch.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_imap_batch(
        &self,
        instance_id: &str,
        folder: &str,
        uidvalidity: u32,
        last_uid: u32,
        mails: &[MailBatchItem<'_>],
        failures: &[BatchFailure<'_>],
        resolved: &[u32],
    ) -> Result<MailBatchOutcome> {
        let tx = self.conn.unchecked_transaction()?;
        let outcome = Self::commit_imap_batch_tx(
            &tx,
            instance_id,
            folder,
            uidvalidity,
            last_uid,
            mails,
            failures,
            resolved,
        )?;
        bump_archive_revision(&tx)?;
        tx.commit()?;
        Ok(outcome)
    }

    /// The transaction-scoped half of [`Storage::insert_mail_message`]
    /// (and the per-item insert used by [`Storage::commit_imap_batch`]): no
    /// transaction of its own, so a caller can run several inserts inside
    /// one batch transaction. Callers commit.
    fn insert_mail_message_tx(
        tx: &Connection,
        instance_id: &str,
        msg: &NewMessage,
        subject: &str,
        identity: &ConnectorIdentity<'_>,
        occurrence: &Occurrence<'_>,
    ) -> Result<MailInsertOutcome> {
        // Exact identity tuple (same revision): the message is already
        // archived under this instance; record the observation only.
        let existing: Option<i64> = tx
            .query_row(
                "SELECT message_id FROM message_identities
                 WHERE source_instance_id = ?1 AND namespace = ?2
                   AND external_key = ?3 AND revision_hash = ?4",
                params![
                    instance_id,
                    identity.namespace,
                    identity.external_key,
                    identity.revision_hash
                ],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            tx.execute(
                "INSERT OR IGNORE INTO message_occurrences
                     (message_id, scope, provider_item_id, uidvalidity)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    id,
                    occurrence.scope,
                    occurrence.provider_item_id,
                    occurrence.uidvalidity
                ],
            )?;
            return Ok(MailInsertOutcome::Duplicate { id });
        }

        // A new revision of a known external key — or a brand new key.
        // Either way the row is archived; conflicting revisions of one
        // key coexist as separate rows.
        let sent_instant = if msg.sent_at.is_empty() {
            None
        } else {
            Some(msg.sent_at.as_str())
        };
        // Connector mail carries its date evidence in the RFC 5322 Date
        // header's explicit offset (resolved by the parser); absent dates
        // are honestly `missing`. No interpretation is layered on top
        // here — that is the time-confidence slice's job.
        let time_confidence = if msg.sent_at.is_empty() {
            "missing"
        } else {
            "explicit_offset"
        };
        let id: i64 = tx.query_row(
            r#"
            INSERT INTO archive_messages
                (source_instance_id, conversation_id, external_id, channel, chat_id,
                 author, subject, body, raw_json,
                 sent_instant, original_timestamp, timezone, time_confidence,
                 content_hash)
            VALUES (?1, NULL, ?2, ?3, '', ?4, ?5, ?6, ?7, ?8, ?9, '', ?10, ?11)
            RETURNING id
            "#,
            params![
                instance_id,
                identity.external_key,
                msg.channel,
                msg.author,
                subject,
                msg.body,
                msg.raw_json,
                sent_instant,
                msg.sent_at,
                time_confidence,
                content_hash(msg),
            ],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO message_identities
                 (source_instance_id, namespace, external_key, revision_hash, message_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                instance_id,
                identity.namespace,
                identity.external_key,
                identity.revision_hash,
                id
            ],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO message_occurrences
                 (message_id, scope, provider_item_id, uidvalidity)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id,
                occurrence.scope,
                occurrence.provider_item_id,
                occurrence.uidvalidity
            ],
        )?;
        // Visible conflict marker at the storage level: another archived
        // revision of the same external key exists under this instance.
        let revisions: i64 = tx.query_row(
            "SELECT COUNT(DISTINCT revision_hash) FROM message_identities
             WHERE source_instance_id = ?1 AND namespace = ?2 AND external_key = ?3",
            params![instance_id, identity.namespace, identity.external_key],
            |row| row.get(0),
        )?;
        Ok(MailInsertOutcome::Inserted {
            id,
            conflicting_revision: revisions > 1,
        })
    }

    /// The transaction-scoped half of [`Storage::commit_imap_batch`].
    /// Callers commit; an error rolls the whole batch back.
    #[allow(clippy::too_many_arguments)]
    fn commit_imap_batch_tx(
        tx: &Connection,
        instance_id: &str,
        folder: &str,
        uidvalidity: u32,
        last_uid: u32,
        mails: &[MailBatchItem<'_>],
        failures: &[BatchFailure<'_>],
        resolved: &[u32],
    ) -> Result<MailBatchOutcome> {
        // A validity change adopts the new namespace atomically: stale
        // ledger rows from abandoned validities cannot survive into the
        // rescan era (the rescan re-derives the truth for those UIDs).
        let saved: Option<(u32, u32)> = tx
            .query_row(
                "SELECT uidvalidity, last_uid FROM sync_checkpoints
                 WHERE source_instance_id = ?1 AND folder = ?2",
                params![instance_id, folder],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if saved.is_some_and(|(validity, _)| validity != uidvalidity) {
            tx.execute(
                "DELETE FROM sync_failed_items
                 WHERE source_instance_id = ?1 AND folder = ?2 AND uidvalidity != ?3",
                params![instance_id, folder, uidvalidity],
            )?;
        }

        let mut outcome = MailBatchOutcome::default();
        for item in mails {
            match Self::insert_mail_message_tx(
                tx,
                instance_id,
                item.message,
                item.subject,
                &item.identity,
                &item.occurrence,
            )? {
                MailInsertOutcome::Inserted {
                    conflicting_revision,
                    ..
                } => {
                    outcome.inserted += 1;
                    if conflicting_revision {
                        outcome.conflicting_revisions += 1;
                    }
                }
                MailInsertOutcome::Duplicate { .. } => outcome.skipped += 1,
            }
        }
        for failure in failures {
            tx.execute(
                "INSERT INTO sync_failed_items
                     (source_instance_id, folder, uid, uidvalidity, reason,
                      attempts, first_failed_at, last_failed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, datetime('now'), datetime('now'))
                 ON CONFLICT (source_instance_id, folder, uid, uidvalidity) DO UPDATE SET
                     reason         = excluded.reason,
                     attempts       = attempts + 1,
                     last_failed_at = excluded.last_failed_at",
                params![
                    instance_id,
                    folder,
                    failure.uid,
                    uidvalidity,
                    failure.reason
                ],
            )?;
            outcome.failed += 1;
        }
        if !resolved.is_empty() {
            let mut stmt = tx.prepare(
                "DELETE FROM sync_failed_items
                 WHERE source_instance_id = ?1 AND folder = ?2 AND uidvalidity = ?3
                   AND uid = ?4",
            )?;
            for uid in resolved {
                stmt.execute(params![instance_id, folder, uidvalidity, uid])?;
            }
        }
        tx.execute(
            "INSERT INTO sync_checkpoints
                 (source_instance_id, folder, uidvalidity, last_uid, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT (source_instance_id, folder) DO UPDATE SET
                uidvalidity = excluded.uidvalidity,
                last_uid    = excluded.last_uid,
                updated_at  = excluded.updated_at",
            params![instance_id, folder, uidvalidity, last_uid],
        )?;
        Ok(outcome)
    }

    /// The folder/import observations recorded for one message, oldest
    /// first — a mail archived once keeps one row per folder that saw it
    /// (see [`Storage::insert_mail_message`]).
    pub fn message_occurrences(&self, message_id: i64) -> Result<Vec<MessageOccurrence>> {
        let mut stmt = self.conn.prepare(
            "SELECT scope, provider_item_id, uidvalidity, observed_at
             FROM message_occurrences WHERE message_id = ?1
             ORDER BY observed_at, rowid",
        )?;
        let rows = stmt
            .query_map(params![message_id], |row| {
                Ok(MessageOccurrence {
                    scope: row.get(0)?,
                    provider_item_id: row.get(1)?,
                    uidvalidity: row.get(2)?,
                    observed_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Every archived revision sharing a message's identity external key
    /// under its source instance — the message itself included. More than
    /// one entry means conflicting revisions of one external key (e.g. two
    /// payloads behind one `Message-ID`) are preserved; every revision
    /// stays searchable, none silently wins.
    pub fn message_revisions(&self, message_id: i64) -> Result<Vec<MessageRevision>> {
        let mut stmt = self.conn.prepare(
            "SELECT mi.message_id, mi.revision_hash
             FROM message_identities AS mi
             WHERE mi.source_instance_id = (SELECT source_instance_id
                                            FROM archive_messages WHERE id = ?1)
               AND mi.namespace = (SELECT namespace FROM message_identities
                                   WHERE message_id = ?1)
               AND mi.external_key = (SELECT external_key FROM message_identities
                                      WHERE message_id = ?1)
             ORDER BY mi.revision_hash",
        )?;
        let rows = stmt
            .query_map(params![message_id], |row| {
                Ok(MessageRevision {
                    message_id: row.get(0)?,
                    revision_hash: row.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Full-text search with optional inclusive LOCAL-day sent-date bounds —
    /// the same date semantics as [`Storage::search_page`].
    ///
    /// `date_from`/`date_to` are `YYYY-MM-DD` strings; anything else is an
    /// error. `date_to` is inclusive (messages sent any time that day count).
    /// Each bound is translated to the UTC instant of local midnight before
    /// being compared with the stored UTC instant, so a day bound selects
    /// exactly what the UI renders as that local day. Messages with an
    /// unknown (NULL) instant are excluded while a bound is active — they
    /// cannot be placed in time. The MATCH expression is a bound parameter
    /// throughout, so it can never spill into SQL text.
    pub fn search_filtered(
        &self,
        match_query: &str,
        date_from: Option<&str>,
        date_to: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Message>> {
        let from = match parse_date_bound(date_from, "date_from")? {
            Some(day) => Some(crate::query::local_day_instant_utc(day)?),
            None => None,
        };
        // Inclusive date_to becomes an exclusive upper bound at local
        // midnight of the next day; RFC 3339 UTC instants compare correctly
        // as text.
        let to_exclusive = match parse_date_bound(date_to, "date_to")? {
            Some(day) => {
                let next = day
                    .checked_add_days(Days::new(1))
                    .filter(|next| (0..=9999).contains(&next.year()))
                    .ok_or_else(|| Error::InvalidDate(format!("date_to={day} out of range")))?;
                Some(crate::query::local_day_instant_utc(next)?)
            }
            None => None,
        };
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id,
                   m.channel, m.chat_id, m.author,
                   COALESCE(m.sent_instant, ''), m.body, m.raw_json
            FROM archive_fts
            JOIN archive_messages AS m ON m.id = archive_fts.rowid
            JOIN source_instances AS si ON si.id = m.source_instance_id
            WHERE archive_fts MATCH ?1
              AND (?2 IS NULL OR m.sent_instant >= ?2)
              AND (?3 IS NULL OR m.sent_instant < ?3)
              AND ((?2 IS NULL AND ?3 IS NULL) OR m.sent_instant IS NOT NULL)
            ORDER BY rank, m.id
            LIMIT ?4
            "#,
        )?;
        let rows = stmt
            .query_map(
                params![match_query, from, to_exclusive, limit],
                message_from_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Whether `expr` compiles as an FTS5 MATCH expression against the index.
    ///
    /// Used to vet LLM-generated expressions before running them: a cheap
    /// `LIMIT 1` probe whose only interesting outcome is error vs. no error
    /// (row presence is irrelevant).
    pub fn is_valid_match(&self, expr: &str) -> bool {
        let Ok(mut stmt) = self
            .conn
            .prepare("SELECT rowid FROM archive_fts WHERE archive_fts MATCH ?1 LIMIT 1")
        else {
            return false;
        };
        let Ok(mut rows) = stmt.query(params![expr]) else {
            return false;
        };
        // Stepping once forces FTS5 to compile the expression; whether a
        // row matches is irrelevant to validity.
        rows.next().is_ok()
    }

    /// Paged, filtered search over the canonical store: FTS5 relevance when
    /// `query.text` is non-empty, newest-first browse otherwise, under the
    /// same facet filters.
    ///
    /// Returns one page plus the total match count (for pagination UI). All
    /// row access stays index-backed: the FTS5 index drives text queries, the
    /// `idx_archive_messages_sent` / `idx_archive_messages_chat` indexes
    /// (plus the source-instance join when filtering by source) back the
    /// browse ordering and the filters, and `LIMIT`/`OFFSET` bound every
    /// response. The `source` facet filters by legacy namespace via the
    /// unique `source_instances.legacy_namespace` index.
    pub fn search_page(&self, query: &SearchQuery) -> Result<SearchPage> {
        let mut query = query.clone();
        query.normalize()?;
        let filters = QueryFilters::build(&query)?;

        let total: u64 = {
            let (sql, bind) = if query.text.trim().is_empty() {
                (
                    format!(
                        "SELECT COUNT(*) FROM archive_messages AS m \
                         JOIN source_instances AS si ON si.id = m.source_instance_id \
                         WHERE 1=1 {}",
                        filters.sql
                    ),
                    filters.bind(),
                )
            } else {
                let mut bind = vec![SqlValue::from(crate::search::build_match_query(
                    &query.text,
                ))];
                bind.extend(filters.bind());
                (
                    format!(
                        "SELECT COUNT(*) FROM archive_fts \
                         JOIN archive_messages AS m ON m.id = archive_fts.rowid \
                         JOIN source_instances AS si ON si.id = m.source_instance_id \
                         WHERE archive_fts MATCH ?1 {}",
                        filters.sql
                    ),
                    bind,
                )
            };
            let mut stmt = self.conn.prepare(&sql)?;
            stmt.query_row(params_from_iter(bind), |row| row.get(0))?
        };

        let hits: Vec<SearchHit> = if query.text.trim().is_empty() {
            let mut bind = filters.bind();
            bind.push(SqlValue::Integer(query.limit as i64));
            bind.push(SqlValue::Integer(query.offset as i64));
            let sql = format!(
                "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
                 m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), \
                 m.body, m.raw_json, NULL \
                 FROM archive_messages AS m \
                 JOIN source_instances AS si ON si.id = m.source_instance_id \
                 WHERE 1=1 {} \
                 ORDER BY m.sent_instant DESC, m.id DESC LIMIT ? OFFSET ?",
                filters.sql
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows: Vec<SearchHit> = stmt
                .query_map(params_from_iter(bind), hit_from_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        } else {
            // Order follows the ? placeholders in the SQL text: the two
            // snippet() markers in the SELECT, then the MATCH expression,
            // then the filter bindings, then LIMIT/OFFSET.
            let mut bind = vec![
                SqlValue::from(SNIPPET_OPEN_MARK.to_string()),
                SqlValue::from(SNIPPET_CLOSE_MARK.to_string()),
                SqlValue::from(crate::search::build_match_query(&query.text)),
            ];
            bind.extend(filters.bind());
            bind.push(SqlValue::Integer(query.limit as i64));
            bind.push(SqlValue::Integer(query.offset as i64));
            let sql = format!(
                "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
                 m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), \
                 m.body, m.raw_json, \
                 snippet(archive_fts, 0, ?1, ?2, '…', 16) \
                 FROM archive_fts \
                 JOIN archive_messages AS m ON m.id = archive_fts.rowid \
                 JOIN source_instances AS si ON si.id = m.source_instance_id \
                 WHERE archive_fts MATCH ?3 {} \
                 ORDER BY rank, m.id LIMIT ? OFFSET ?",
                filters.sql
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows: Vec<SearchHit> = stmt
                .query_map(params_from_iter(bind), hit_from_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };

        Ok(SearchPage {
            hits,
            total,
            limit: query.limit,
            offset: query.offset,
        })
    }

    /// Stream every message matching `query`'s text and filters, ignoring
    /// the pagination fields, through `sink` one row at a time. Used by
    /// export, which must cover the entire result set — rows are visited in
    /// the same order [`Storage::search_page`] paginates through (FTS5
    /// relevance for text queries, newest-first otherwise), so an export
    /// lists exactly what the UI shows across all its pages. Returns the
    /// number of rows streamed.
    pub fn search_export_stream(
        &self,
        query: &SearchQuery,
        sink: &mut dyn FnMut(&Message) -> Result<()>,
    ) -> Result<u64> {
        let mut query = query.clone();
        query.normalize()?;
        let filters = QueryFilters::build(&query)?;

        let (sql, bind): (String, Vec<SqlValue>) = if query.text.trim().is_empty() {
            (
                format!(
                    "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
                     m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), \
                     m.body, m.raw_json \
                     FROM archive_messages AS m \
                     JOIN source_instances AS si ON si.id = m.source_instance_id \
                     WHERE 1=1 {} \
                     ORDER BY m.sent_instant DESC, m.id DESC",
                    filters.sql
                ),
                filters.bind(),
            )
        } else {
            let mut bind = vec![SqlValue::from(crate::search::build_match_query(
                &query.text,
            ))];
            bind.extend(filters.bind());
            (
                format!(
                    "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
                     m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), \
                     m.body, m.raw_json \
                     FROM archive_fts \
                     JOIN archive_messages AS m ON m.id = archive_fts.rowid \
                     JOIN source_instances AS si ON si.id = m.source_instance_id \
                     WHERE archive_fts MATCH ?1 {} \
                     ORDER BY rank, m.id",
                    filters.sql
                ),
                bind,
            )
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(bind))?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            sink(&message_from_row(row)?)?;
            count += 1;
        }
        Ok(count)
    }

    /// A message plus its adjacent messages in the same conversation.
    ///
    /// `before`/`after` each hold up to `radius` messages, oldest → newest.
    /// Messages without a conversation (standalone mail, ungrouped legacy
    /// rows) have no thread peers. Ties and unknown instants order by id,
    /// matching the legacy `sent_at`-text ordering exactly (COALESCE maps
    /// the unknown NULL instant to the legacy empty-string position).
    pub fn chat_context(&self, id: i64, radius: u32) -> Result<Option<ChatContext>> {
        let radius = radius.clamp(1, crate::query::MAX_LIMIT) as i64;
        let Some(message) = self.get(id)? else {
            return Ok(None);
        };
        // Peers exist only inside a conversation: M1.3 resolved rows
        // (provider keys, email threads) group there, while messages
        // without one (empty legacy `chat_id`, standalone mail) stand
        // alone.
        let has_conversation: bool = self.conn.query_row(
            "SELECT conversation_id IS NOT NULL FROM archive_messages WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        if !has_conversation {
            return Ok(Some(ChatContext {
                before: Vec::new(),
                message,
                after: Vec::new(),
            }));
        }

        let mut before_stmt = self.conn.prepare(
            "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
             m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), m.body, m.raw_json \
             FROM archive_messages AS m \
             JOIN source_instances AS si ON si.id = m.source_instance_id \
             WHERE m.conversation_id = ( \
                 SELECT conversation_id FROM archive_messages WHERE id = ?1) \
               AND (COALESCE(m.sent_instant, '') < ?2 \
                    OR (COALESCE(m.sent_instant, '') = ?2 AND m.id < ?3)) \
             ORDER BY m.sent_instant DESC, m.id DESC LIMIT ?4",
        )?;
        let mut before: Vec<Message> = before_stmt
            .query_map(
                params![message.id, message.sent_at, message.id, radius],
                message_from_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        before.reverse(); // newest → oldest from the query; thread reads oldest → newest

        let mut after_stmt = self.conn.prepare(
            "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
             m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), m.body, m.raw_json \
             FROM archive_messages AS m \
             JOIN source_instances AS si ON si.id = m.source_instance_id \
             WHERE m.conversation_id = ( \
                 SELECT conversation_id FROM archive_messages WHERE id = ?1) \
               AND (COALESCE(m.sent_instant, '') > ?2 \
                    OR (COALESCE(m.sent_instant, '') = ?2 AND m.id > ?3)) \
             ORDER BY m.sent_instant ASC, m.id ASC LIMIT ?4",
        )?;
        let after = after_stmt
            .query_map(
                params![message.id, message.sent_at, message.id, radius],
                message_from_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(Some(ChatContext {
            before,
            message,
            after,
        }))
    }

    /// Fetch one message by row id (used to inspect a search hit).
    pub fn get(&self, id: i64) -> Result<Option<Message>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id,
                   m.channel, m.chat_id, m.author,
                   COALESCE(m.sent_instant, ''), m.body, m.raw_json
            FROM archive_messages AS m
            JOIN source_instances AS si ON si.id = m.source_instance_id
            WHERE m.id = ?1
            "#,
        )?;
        stmt.query_row(params![id], message_from_row)
            .optional()
            .map_err(Into::into)
    }

    /// Every message of one chat thread, oldest → newest (same order the
    /// detail view walks the thread in, but without the radius bound). The
    /// legacy `(source namespace, chat_id)` key resolves through the
    /// conversation rows the canonical store maintains; a message with no
    /// conversation has no thread and yields no rows. Ties and unknown
    /// instants order by id, matching the legacy ordering exactly
    /// (COALESCE maps the unknown NULL instant to the legacy empty-string
    /// position).
    pub fn chat_thread_messages(&self, source: &str, chat_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
             m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), m.body, m.raw_json \
             FROM archive_messages AS m \
             JOIN conversations AS c ON c.id = m.conversation_id \
             JOIN source_instances AS si ON si.id = c.source_instance_id \
             WHERE si.legacy_namespace = ?1 AND c.legacy_chat_key = ?2 \
             ORDER BY COALESCE(m.sent_instant, '') ASC, m.id ASC",
        )?;
        let rows = stmt
            .query_map(params![source, chat_id], message_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Archive counts: total and per source.
    ///
    /// Reads the trigger-maintained `stats_source` counters (migration 6)
    /// instead of grouping over every message row, so this stays instant
    /// on million-message archives; the numbers are identical by
    /// construction because every write path maintains the counters.
    pub fn stats(&self) -> Result<Stats> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(si.legacy_namespace, si.label, si.id) AS label, s.n \
             FROM stats_source AS s \
             JOIN source_instances AS si ON si.id = s.source \
             WHERE s.n > 0 ORDER BY s.n DESC, label",
        )?;
        let by_source = stmt
            .query_map([], |row| {
                Ok(SourceCount {
                    source: row.get(0)?,
                    count: row.get::<_, i64>(1)?.max(0) as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let total = by_source.iter().map(|s| s.count).sum();
        Ok(Stats { total, by_source })
    }

    /// Per-account overview counts for the multi-account UI (roadmap: the
    /// accounts panel): one row per durable source instance — the
    /// archived-message count and the open failed-item ledger rows — so
    /// several IMAP/chat/import accounts answer in one bounded call.
    ///
    /// The message counts read the trigger-maintained `stats_source`
    /// counter table (migration 6) — O(number of accounts), never a scan
    /// over the archive — and the failure counts group the ledger over its
    /// `(source_instance_id, …)` primary-key prefix. Accounts with zero
    /// messages are included (a connected but empty mailbox is still an
    /// account), ordered heaviest first like the source chips.
    pub fn account_counts(&self) -> Result<Vec<AccountCounts>> {
        let mut stmt = self.conn.prepare(
            "SELECT si.id, COALESCE(ss.n, 0), COALESCE(f.failed, 0)\n             FROM source_instances AS si\n             LEFT JOIN stats_source AS ss ON ss.source = si.id\n             LEFT JOIN (\n                 SELECT source_instance_id, COUNT(*) AS failed\n                 FROM sync_failed_items GROUP BY source_instance_id\n             ) AS f ON f.source_instance_id = si.id\n             ORDER BY COALESCE(ss.n, 0) DESC, si.id",
        )?;
        let counts = stmt
            .query_map([], |row| {
                Ok(AccountCounts {
                    source_id: row.get(0)?,
                    messages: row.get::<_, i64>(1)?.max(0) as u64,
                    failed_items: row.get::<_, i64>(2)?.max(0) as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(counts)
    }

    /// Full dashboard statistics (see [`crate::stats::ArchiveStats`]):
    /// total, date range, per-source counts, the per-day histogram, and
    /// the heaviest chats and authors, all in one call.
    ///
    /// The grouped counts stream straight off the trigger-maintained
    /// counter tables of migration 6 — O(number of groups), never a scan
    /// over the messages — and the date range resolves as two O(log n)
    /// index lookups, so the whole dashboard answers within its 100 ms
    /// budget on million-message archives (asserted by
    /// `tests/stats_perf.rs`). Messages with an unknown (`empty`)
    /// `sent_at` are excluded from the date range and the day histogram;
    /// messages without a `chat_id` are excluded from `top_chats`.
    pub fn archive_stats(&self, top: u32) -> Result<ArchiveStats> {
        let top = top.clamp(1, crate::stats::MAX_TOP) as i64;

        let total: u64 =
            self.conn
                .query_row("SELECT COALESCE(SUM(n), 0) FROM stats_source", [], |row| {
                    row.get::<_, i64>(0).map(|n| n.max(0) as u64)
                })?;

        // Date range: two single-aggregate lookups ride the min/max
        // optimization over idx_messages_sent_at. They must stay separate
        // statements — combining MIN and MAX in one query (or spelling the
        // filter `!= ''`) degrades to a full index scan. `> ''` skips the
        // empty-string prefix of unknown timestamps.
        let first_sent_at: Option<String> = self.conn.query_row(
            "SELECT MIN(sent_instant) FROM archive_messages WHERE sent_instant > ''",
            [],
            |row| row.get(0),
        )?;
        let last_sent_at: Option<String> = self.conn.query_row(
            "SELECT MAX(sent_instant) FROM archive_messages WHERE sent_instant > ''",
            [],
            |row| row.get(0),
        )?;

        let mut source_stmt = self.conn.prepare(
            "SELECT COALESCE(si.legacy_namespace, si.label, si.id) AS label, s.n              FROM stats_source AS s              JOIN source_instances AS si ON si.id = s.source              WHERE s.n > 0 ORDER BY s.n DESC, label",
        )?;
        let by_source = source_stmt
            .query_map([], |row| {
                Ok(SourceCount {
                    source: row.get(0)?,
                    count: row.get::<_, i64>(1)?.max(0) as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // The counter table keeps one `''` row for unknown timestamps;
        // filter it here so the histogram starts at the first dated day.
        let mut day_stmt = self
            .conn
            .prepare("SELECT day, n FROM stats_day WHERE day != '' AND n > 0 ORDER BY day")?;
        let by_day = day_stmt
            .query_map([], |row| {
                Ok(DayCount {
                    day: row.get(0)?,
                    count: row.get::<_, i64>(1)?.max(0) as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut chat_stmt = self.conn.prepare(
            "SELECT COALESCE(si.legacy_namespace, si.label, si.id) AS label, c.chat_id, c.n \
             FROM stats_chat AS c \
             JOIN source_instances AS si ON si.id = c.source \
             WHERE c.chat_id != '' AND c.n > 0 \
             ORDER BY c.n DESC, label, c.chat_id LIMIT ?1",
        )?;
        let top_chats = chat_stmt
            .query_map([top], |row| {
                Ok(ChatCount {
                    source: row.get(0)?,
                    chat_id: row.get(1)?,
                    count: row.get::<_, i64>(2)?.max(0) as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut author_stmt = self.conn.prepare(
            "SELECT author, n FROM stats_author WHERE n > 0 ORDER BY n DESC, author LIMIT ?1",
        )?;
        let top_authors = author_stmt
            .query_map([top], |row| {
                Ok(AuthorCount {
                    author: row.get(0)?,
                    count: row.get::<_, i64>(1)?.max(0) as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(ArchiveStats {
            total,
            first_sent_at,
            last_sent_at,
            by_source,
            by_day,
            top_chats,
            top_authors,
        })
    }

    /// Currently applied schema version.
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?)
    }

    /// Rebuild the canonical FTS index from scratch from the
    /// `archive_messages` table.
    ///
    /// Only needed if the index is ever corrupted; triggers keep it in sync
    /// during normal operation.
    pub fn rebuild_fts(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO archive_fts (archive_fts) VALUES ('rebuild')",
            [],
        )?;
        Ok(())
    }

    /// Test support: drop every FTS row (leaving `archive_messages` intact)
    /// so index recovery paths can be exercised. Not used by the app.
    #[doc(hidden)]
    pub fn wipe_fts_for_tests(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO archive_fts (archive_fts) VALUES ('delete-all')",
            [],
        )?;
        Ok(())
    }

    /// Load the incremental-sync cursor (UIDVALIDITY + last fetched UID)
    /// for one `(source, folder)`. See [`crate::imap`].
    pub fn get_imap_state(
        &self,
        source: &str,
        folder: &str,
    ) -> Result<Option<crate::message::ImapSyncState>> {
        let row = self
            .conn
            .query_row(
                "SELECT sc.uidvalidity, sc.last_uid FROM sync_checkpoints AS sc
                 JOIN source_instances AS si ON si.id = sc.source_instance_id
                 WHERE si.legacy_namespace = ?1 AND sc.folder = ?2",
                params![source, folder],
                |row| {
                    Ok(crate::message::ImapSyncState {
                        uidvalidity: row.get(0)?,
                        last_uid: row.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Save (upsert) the incremental-sync cursor for one `(source, folder)`
    /// in the canonical `sync_checkpoints` table (the legacy
    /// `imap_sync_state` table is never written). Stores only folder
    /// cursors — never credentials.
    pub fn set_imap_state(
        &self,
        source: &str,
        folder: &str,
        uidvalidity: u32,
        last_uid: u32,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let instance = resolve_legacy_instance(&tx, source)?;
        tx.execute(
            "INSERT INTO sync_checkpoints
                 (source_instance_id, folder, uidvalidity, last_uid, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT (source_instance_id, folder) DO UPDATE SET
                uidvalidity = excluded.uidvalidity,
                last_uid    = excluded.last_uid,
                updated_at  = excluded.updated_at",
            params![instance, folder, uidvalidity, last_uid],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Load the incremental-sync cursor for one `(source instance, folder)`
    /// — the connector-scoped variant of [`Storage::get_imap_state`], keyed
    /// by the durable instance id instead of the legacy namespace.
    pub fn get_imap_checkpoint(
        &self,
        instance_id: &str,
        folder: &str,
    ) -> Result<Option<crate::message::ImapSyncState>> {
        let row = self
            .conn
            .query_row(
                "SELECT uidvalidity, last_uid FROM sync_checkpoints
                 WHERE source_instance_id = ?1 AND folder = ?2",
                params![instance_id, folder],
                |row| {
                    Ok(crate::message::ImapSyncState {
                        uidvalidity: row.get(0)?,
                        last_uid: row.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Per-source exclusion (M2.3): an excluded source is skipped by
    /// coordinator rounds while its archive, cursors, and configuration
    /// are kept intact — exclusion pauses a source, it never deletes
    /// anything. Re-including restores it to the next round.
    pub fn set_source_excluded(&self, id: &str, excluded: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE source_instances SET excluded = ?2 WHERE id = ?1",
            params![id, excluded as i64],
        )?;
        Ok(())
    }

    /// The periodic sync schedule (M2.4): whether scheduled coordinator
    /// rounds run and at what interval. Off until the user opts in;
    /// the row is seeded with defaults by migration 11, so this always
    /// answers (defaults if the row is somehow missing).
    pub fn sync_schedule(&self) -> Result<crate::scheduler::SyncSchedule> {
        let row = self
            .conn
            .query_row(
                "SELECT enabled, interval_ms FROM sync_schedule WHERE id = 1",
                [],
                |row| {
                    Ok(crate::scheduler::SyncSchedule {
                        enabled: row.get::<_, i64>(0)? != 0,
                        interval_ms: row.get::<_, u64>(1)?,
                    })
                },
            )
            .optional()?;
        Ok(row.unwrap_or_default())
    }

    /// Persist the periodic sync schedule (M2.4). Stores what it is
    /// given — the scheduler loop (the single writer) clamps to its
    /// configured interval bounds before calling this, and normalizes
    /// on load; runtime state (deadlines, the active round) is never
    /// persisted: a fresh start re-arms one interval out.
    pub fn set_sync_schedule(
        &self,
        schedule: crate::scheduler::SyncSchedule,
    ) -> Result<crate::scheduler::SyncSchedule> {
        self.conn.execute(
            "UPDATE sync_schedule SET enabled = ?1, interval_ms = ?2, updated_at = datetime('now')
             WHERE id = 1",
            params![schedule.enabled as i64, schedule.interval_ms],
        )?;
        Ok(schedule)
    }

    /// The failed-item ledger: every item a sync observed but could not
    /// store, with the reason and attempt count — the durable
    /// "what fell and why" accounting (M2.3). `instance` filters to one
    /// source; `None` lists every source's failures, oldest first.
    pub fn list_failed_items(&self, instance: Option<&str>) -> Result<Vec<FailedItem>> {
        let sql = match instance {
            Some(_) => {
                "SELECT source_instance_id, folder, uid, uidvalidity, reason,
                        attempts, first_failed_at, last_failed_at
                 FROM sync_failed_items WHERE source_instance_id = ?1
                 ORDER BY first_failed_at, source_instance_id, folder, uid"
            }
            None => {
                "SELECT source_instance_id, folder, uid, uidvalidity, reason,
                        attempts, first_failed_at, last_failed_at
                 FROM sync_failed_items
                 ORDER BY first_failed_at, source_instance_id, folder, uid"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let map = |row: &Row<'_>| {
            Ok(FailedItem {
                source_instance_id: row.get(0)?,
                folder: row.get(1)?,
                uid: row.get(2)?,
                uidvalidity: row.get(3)?,
                reason: row.get(4)?,
                attempts: row.get(5)?,
                first_failed_at: row.get(6)?,
                last_failed_at: row.get(7)?,
            })
        };
        let rows = match instance {
            Some(instance) => stmt
                .query_map(params![instance], map)?
                .collect::<std::result::Result<Vec<_>, _>>()?,
            None => stmt
                .query_map([], map)?
                .collect::<std::result::Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }

    /// The failed uids under one `(instance, folder, validity)` — the
    /// retry set the next round re-fetches (they sit at or below the
    /// checkpoint: the cursor advanced past them only because the ledger
    /// remembered them). Ascending by uid.
    pub fn imap_failed_uids(
        &self,
        instance_id: &str,
        folder: &str,
        uidvalidity: u32,
    ) -> Result<Vec<u32>> {
        let mut stmt = self.conn.prepare(
            "SELECT uid FROM sync_failed_items
             WHERE source_instance_id = ?1 AND folder = ?2 AND uidvalidity = ?3
             ORDER BY uid",
        )?;
        let rows = stmt
            .query_map(params![instance_id, folder, uidvalidity], |row| {
                row.get::<_, u32>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // -----------------------------------------------------------------------
    // Local embeddings (crate::vector): incremental job support + cosine scan
    // -----------------------------------------------------------------------

    /// Upsert a batch of embeddings in one transaction (the embedding job's
    /// write unit). Every vector's length must equal `dim`. Vectors are
    /// stored quantized to one i8 byte per component ([`crate::vector::quantize_i8`]).
    pub fn embed_messages_batch(
        &self,
        model: &str,
        dim: u16,
        items: &[(i64, Vec<f32>)],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO message_embeddings (message_id, model, dim, embedding, embedded_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))
                 ON CONFLICT (message_id) DO UPDATE SET
                    model      = excluded.model,
                    dim        = excluded.dim,
                    embedding  = excluded.embedding,
                    embedded_at = excluded.embedded_at",
            )?;
            for (message_id, embedding) in items {
                if embedding.len() != dim as usize {
                    return Err(Error::Embedding(format!(
                        "embedding for message {message_id} has {} floats, expected {dim}",
                        embedding.len()
                    )));
                }
                let blob = crate::vector::quantize_i8(embedding);
                stmt.execute(params![message_id, model, dim, blob])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Up to `limit` messages (id + body) still lacking a `model` embedding,
    /// oldest id first: the incremental embedding job's work list. A row
    /// written for a different model counts as lacking (it gets replaced).
    pub fn pending_embeddings(&self, model: &str, limit: u64) -> Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.body FROM archive_messages AS m
             WHERE NOT EXISTS (
                 SELECT 1 FROM message_embeddings AS e
                 WHERE e.message_id = m.id AND e.model = ?1
             )
             ORDER BY m.id LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![model, limit], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Coverage counts for one model: rows embedded for it, total messages.
    pub fn embedding_counts(&self, model: &str) -> Result<EmbeddingCounts> {
        let embedded: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM message_embeddings WHERE model = ?1",
            params![model],
            |row| row.get(0),
        )?;
        let messages: u64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM archive_messages", [], |row| {
                    row.get(0)
                })?;
        Ok(EmbeddingCounts { embedded, messages })
    }

    /// Vector search: cosine of `query` against every `model` embedding,
    /// under the same LOCAL-day date bounds as [`Storage::search_filtered`]
    /// (`None` = unbounded). Returns the best `limit` hits, score-descending
    /// (ties by ascending id). Stored vectors are normalized at write time,
    /// so cosine is a plain dot product. Rows whose blob length disagrees
    /// with `dim` (a foreign model's leftovers) are skipped, not fatal.
    /// This is the manual-cosine index described in [`crate::vector`].
    pub fn vector_scan(
        &self,
        model: &str,
        dim: u16,
        query: &[f32],
        date_from: Option<&str>,
        date_to: Option<&str>,
        limit: u32,
    ) -> Result<Vec<VectorHit>> {
        if query.len() != dim as usize {
            return Err(Error::Embedding(format!(
                "query has {} floats, expected {dim}",
                query.len()
            )));
        }
        let from = match parse_date_bound(date_from, "date_from")? {
            Some(day) => Some(crate::query::local_day_instant_utc(day)?),
            None => None,
        };
        // Inclusive date_to becomes an exclusive upper bound at local
        // midnight of the next day (same semantics as search_filtered).
        let to_exclusive = match parse_date_bound(date_to, "date_to")? {
            Some(day) => {
                let next = day
                    .checked_add_days(Days::new(1))
                    .filter(|next| (0..=9999).contains(&next.year()))
                    .ok_or_else(|| Error::InvalidDate(format!("date_to={day} out of range")))?;
                Some(crate::query::local_day_instant_utc(next)?)
            }
            None => None,
        };

        // Same bound-parameter shape as search_filtered: the date bounds
        // are values, never spliced into the SQL text. Without bounds the
        // scan skips the messages join entirely (the embedding table alone
        // answers it); with bounds the join applies the same local-day
        // sent_at filter as the FTS leg.
        let (sql, has_bounds) = if from.is_some() || to_exclusive.is_some() {
            (
                "SELECT e.message_id, e.embedding
                 FROM message_embeddings AS e
                 JOIN archive_messages AS m ON m.id = e.message_id
                 WHERE e.model = ?1
                   AND (?2 IS NULL OR m.sent_instant >= ?2)
                   AND (?3 IS NULL OR m.sent_instant < ?3)
                   AND ((?2 IS NULL AND ?3 IS NULL) OR m.sent_instant IS NOT NULL)",
                true,
            )
        } else {
            (
                "SELECT message_id, embedding FROM message_embeddings WHERE model = ?1",
                false,
            )
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = if has_bounds {
            stmt.query(params![model, from, to_exclusive])?
        } else {
            stmt.query(params![model])?
        };
        let mut hits: Vec<VectorHit> = Vec::new();
        let quantized_query = crate::vector::quantize_query_i8(query);
        while let Some(row) = rows.next()? {
            let message_id = row.get::<_, i64>(0)?;
            // Zero-copy blob read: hashing bytes straight off the row
            // avoids a 512-byte allocation per scanned message.
            let blob = match row.get_ref(1)? {
                rusqlite::types::ValueRef::Blob(bytes) => bytes,
                _ => &[],
            };
            if blob.len() != dim as usize {
                continue; // a different model's row; not comparable
            }
            // Integer dot product over the i8-quantized vectors: the score
            // is the cosine up to quantization error (both sides normalized
            // and scaled by 127 at write/quantize time). Zipped slices keep
            // the per-element loop bounds-check-free.
            let mut acc: i32 = 0;
            for (&byte, &q) in blob.iter().zip(quantized_query.iter()) {
                acc += q as i32 * (byte as i8) as i32;
            }
            let score = acc as f32 / (crate::vector::I8_SCALE * crate::vector::I8_SCALE);
            if score <= crate::vector::MIN_COSINE {
                continue; // collision noise, not a similar message
            }
            hits.push(VectorHit { message_id, score });
        }
        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.message_id.cmp(&b.message_id))
        });
        hits.truncate(limit as usize);
        Ok(hits)
    }

    /// Hydrate messages by id, preserving the input order; ids without a
    /// row (deleted since ranking) are skipped.
    pub fn get_many(&self, ids: &[i64]) -> Result<Vec<Message>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut by_id: std::collections::HashMap<i64, Message> =
            std::collections::HashMap::with_capacity(ids.len());
        let mut stmt = self.conn.prepare(
            "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id,
                    m.channel, m.chat_id, m.author,
                    COALESCE(m.sent_instant, ''), m.body, m.raw_json
             FROM archive_messages AS m
             JOIN source_instances AS si ON si.id = m.source_instance_id
             WHERE m.id = ?1",
        )?;
        for id in ids {
            if let Some(message) = stmt.query_row(params![id], message_from_row).optional()? {
                by_id.insert(*id, message);
            }
        }
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// Merge media entries into an already-stored message's `raw_json`
    /// `media` array (dedup by `sha256`).
    ///
    /// This is the backfill path: re-importing an export whose files were
    /// previously missing links media into existing rows without touching
    /// any other field (the FTS triggers re-index the unchanged body
    /// harmlessly). Returns `true` when the row changed; an all-duplicates
    /// merge is a no-op, so re-imports stay idempotent.
    pub fn attach_media(&self, id: i64, entries: &[serde_json::Value]) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let changed = attach_media_in_tx(&tx, id, entries)?;
        if changed {
            // A media link changes result summaries (attachment counts),
            // so it counts as content.
            bump_archive_revision(&tx)?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// The current archive revision (M3.2): bumped inside every
    /// content-changing transaction. Search responses carry it so the UI
    /// can offer a refresh when the archive changed under a paginated
    /// result set (A07).
    pub fn archive_revision(&self) -> Result<u64> {
        archive_revision_on(&self.conn)
    }

    /// The live connection (writer when reached through the pool's write
    /// queue). Backup/maintenance operations that need the connection
    /// itself — the SQLite backup API, whole-file snapshots — use this;
    /// ordinary callers keep going through the typed methods.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// The identity fingerprint of the archive right now: schema
    /// version, archive revision, and the row counts a staged restore
    /// must not silently diverge from (M4.4's preview→commit binding —
    /// [`crate::backup`] compares the fingerprint taken when a restore
    /// was previewed with the one at commit time and refuses a switch
    /// when the active vault changed in between).
    pub fn vault_fingerprint(&self) -> Result<VaultFingerprint> {
        vault_fingerprint_on(&self.conn)
    }

    /// Every distinct media digest the archive references — the
    /// `sha256` of each `raw_json` `media` entry across all canonical
    /// messages **plus** every digest in the `media_objects` registry
    /// (M4.2: unlinked export files are registered there, since no
    /// message references them). This is the reference side of the
    /// object-store reconciliation (see
    /// [`crate::object_store::ObjectStore::reconcile`]).
    pub fn media_digests(&self) -> Result<std::collections::HashSet<String>> {
        media_digests_on(&self.conn)
    }

    /// The digests referenced by stored messages only (the
    /// message-linked side of the reference set — what the M4.2
    /// unlinked pass skips as already reachable).
    pub fn message_media_digests(&self) -> Result<std::collections::HashSet<String>> {
        let mut digests = std::collections::HashSet::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT raw_json FROM archive_messages WHERE raw_json LIKE '%\"media\"%'",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let raw: String = row.get(0)?;
                for entry in media_entries(&raw) {
                    if let Some(sha) = entry.get("sha256").and_then(|v| v.as_str()) {
                        if !sha.is_empty() {
                            digests.insert(sha.to_string());
                        }
                    }
                }
            }
        }
        Ok(digests)
    }

    /// The media objects registered as unlinked (M4.2) — export files
    /// stored without a message, with their provenance. Oldest first.
    pub fn unlinked_media_objects(&self) -> Result<Vec<RegisteredMediaObject>> {
        let mut stmt = self.conn.prepare(
            "SELECT sha256, size, name, kind, origin, import_id, first_seen
             FROM media_objects ORDER BY first_seen, sha256",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RegisteredMediaObject {
                sha256: row.get(0)?,
                size: row.get::<_, i64>(1)?.max(0) as u64,
                name: row.get(2)?,
                kind: row.get(3)?,
                origin: row.get(4)?,
                import_id: row.get(5)?,
                first_seen: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Remember a watched folder (idempotent on the canonical path).
    /// See [`crate::watch`].
    pub fn add_watched_folder(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO watched_folders (path) VALUES (?1)
             ON CONFLICT (path) DO NOTHING",
            params![path],
        )?;
        Ok(())
    }

    /// Forget a watched folder. Returns whether a row was removed.
    pub fn remove_watched_folder(&self, path: &str) -> Result<bool> {
        let removed = self
            .conn
            .execute("DELETE FROM watched_folders WHERE path = ?1", params![path])?;
        Ok(removed > 0)
    }

    /// The watched folders, ordered oldest first (same-second ties broken
    /// by path).
    pub fn watched_folders(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM watched_folders ORDER BY added_at, path")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Insert one message inside an open transaction — the shared body of
/// [`Storage::insert_message`] (one-message transaction) and
/// [`Storage::commit_import_batch`] (many-message batch transaction).
/// Identical dedup semantics in both: an existing identity records the
/// repeat occurrence and refreshes the conversation label, then reports
/// `Duplicate`; a new row lands in `archive_messages` with its identity
/// alias and first `import` occurrence. `import_id` stamps the bounded
/// import run that observed the message (M3.2 import-scope filter); the
/// single-message convenience path passes `""` (provenance unknown).
fn insert_message_in_tx(
    tx: &Transaction<'_>,
    import_id: &str,
    msg: &NewMessage,
) -> Result<InsertOutcome> {
    let instance = resolve_legacy_instance(tx, &msg.source)?;

    // Dedup on the canonical identity: first write wins forever.
    let existing: Option<i64> = tx
        .query_row(
            "SELECT message_id FROM message_identities
             WHERE source_instance_id = ?1 AND namespace = 'legacy'
               AND external_key = ?2 AND revision_hash = ''",
            params![instance, msg.external_id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        // Record the repeat observation only (idempotent on the
        // occurrence tuple); the archived row itself stays untouched.
        tx.execute(
            "INSERT OR IGNORE INTO message_occurrences
                 (message_id, scope, provider_item_id, import_id)
             VALUES (?1, 'import', ?2, ?3)",
            params![id, msg.external_id, import_id],
        )?;
        // A re-import of the same chat under a rename still refreshes
        // the conversation label (identity is stable, names follow).
        if !msg.conversation_key.is_empty() && !msg.chat_id.is_empty() {
            if let Some(conv) = find_provider_conversation(tx, &instance, &msg.conversation_key)? {
                tx.execute(
                    "UPDATE conversations SET label = ?2 WHERE id = ?1 AND label <> ?2",
                    params![conv, msg.chat_id],
                )?;
            }
        }
        return Ok(InsertOutcome::Duplicate(id));
    }

    let conversation = resolve_conversation(tx, &instance, msg)?;
    let (original_timestamp, timezone, time_confidence) = time_provenance(msg)?;
    let sent_instant = if msg.sent_at.is_empty() {
        None
    } else {
        Some(msg.sent_at.as_str())
    };
    let id: i64 = tx.query_row(
        r#"
        INSERT INTO archive_messages
            (source_instance_id, conversation_id, external_id, channel, chat_id,
             author, subject, body, raw_json,
             sent_instant, original_timestamp, timezone, time_confidence, content_hash)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        RETURNING id
        "#,
        params![
            instance,
            conversation,
            msg.external_id,
            msg.channel,
            msg.chat_id,
            msg.author,
            msg.body,
            msg.raw_json,
            sent_instant,
            original_timestamp,
            timezone,
            time_confidence,
            content_hash(msg),
        ],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO message_identities
             (source_instance_id, namespace, external_key, message_id)
         VALUES (?1, 'legacy', ?2, ?3)",
        params![instance, msg.external_id, id],
    )?;
    tx.execute(
        "INSERT INTO message_occurrences (message_id, scope, provider_item_id, import_id)
         VALUES (?1, 'import', ?2, ?3)",
        params![id, msg.external_id, import_id],
    )?;
    Ok(InsertOutcome::Inserted(id))
}

/// Increment `archive_meta.archive_revision` inside a content-changing
/// transaction (M3.2). Every committed batch that can change what a
/// paginated search returns — message inserts, occurrence enrichments,
/// media backfills, (future) deletes — bumps the counter in the same
/// transaction, so `search_archive_v2` responses can carry a revision
/// that is consistent with the rows they contain and the UI can offer a
/// refresh instead of silently restarting pagination (A07). Over-
/// reporting (bumping on a no-op batch) is safe: it only offers a
/// refresh; under-reporting would hide real changes.
fn bump_archive_revision(tx: &Connection) -> Result<()> {
    tx.execute(
        "INSERT INTO archive_meta (key, value) VALUES ('archive_revision', '1')
         ON CONFLICT(key) DO UPDATE SET
            value = CAST(CAST(value AS INTEGER) + 1 AS TEXT)
         WHERE key = 'archive_revision'",
        [],
    )?;
    Ok(())
}

/// The non-empty `media` array of a message's `raw_json`, if any — the
/// media-extraction pass writes it there ([`crate::media`]), and the
/// ingest paths link/backfill from it.
fn media_entries(raw_json: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(raw_json)
        .ok()
        .and_then(|value| {
            value
                .get("media")
                .and_then(|m| m.as_array())
                .filter(|a| !a.is_empty())
                .cloned()
        })
        .unwrap_or_default()
}

/// The current `archive_revision` counter on an arbitrary connection
/// (0 when the archive has none yet).
fn archive_revision_on(conn: &Connection) -> Result<u64> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM archive_meta WHERE key = 'archive_revision'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.and_then(|v| v.parse().ok()).unwrap_or(0))
}

/// [`Storage::media_digests`] over an arbitrary connection — every
/// digest the archive on that connection references (message media
/// entries plus the `media_objects` registry). Shared with the M4.4
/// backup module, which runs the same collection against a consistent
/// snapshot or a staged restore instead of the live writer.
pub(crate) fn media_digests_on(
    conn: &Connection,
) -> Result<std::collections::HashSet<String>> {
    let mut digests = std::collections::HashSet::new();
    if table_exists_on(conn, "archive_messages")? {
        let mut stmt =
            conn.prepare("SELECT raw_json FROM archive_messages WHERE raw_json LIKE '%\"media\"%'")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let raw: String = row.get(0)?;
            for entry in media_entries(&raw) {
                if let Some(sha) = entry.get("sha256").and_then(|v| v.as_str()) {
                    if !sha.is_empty() {
                        digests.insert(sha.to_string());
                    }
                }
            }
        }
    }
    if table_exists_on(conn, "media_objects")? {
        let mut stmt = conn.prepare("SELECT sha256 FROM media_objects WHERE sha256 != ''")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let sha: String = row.get(0)?;
            digests.insert(sha);
        }
    }
    Ok(digests)
}

/// The archive's identity fingerprint on an arbitrary connection —
/// schema version, archive revision, and the row counts that describe
/// what the vault holds (see [`Storage::vault_fingerprint`]). Backup
/// staging records it for the archive it validated; committing a staged
/// restore compares it with the live vault's fingerprint and refuses
/// when anything changed since the preview.
pub(crate) fn vault_fingerprint_on(conn: &Connection) -> Result<VaultFingerprint> {
    let count = |table: &str| -> Result<u64> {
        if !table_exists_on(conn, table)? {
            return Ok(0); // a legacy (pre-canonical) database holds none
        }
        Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
            r.get::<_, i64>(0)
        })?
        .max(0) as u64)
    };
    Ok(VaultFingerprint {
        schema_version: read_schema_version(conn)?,
        archive_revision: archive_revision_on(conn)?,
        messages: count("archive_messages")?,
        sources: count("source_instances")?,
        media_objects: count("media_objects")?,
    })
}

fn table_exists_on(conn: &Connection, name: &str) -> Result<bool> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )?;
    Ok(exists)
}

/// Merge `entries` into a message's existing `media` array inside an open
/// transaction (the shared body of [`Storage::attach_media`] and the
/// duplicate path of [`Storage::commit_import_batch`]). Returns whether
/// anything changed.
///
/// M4.2 merge rules — stored entries and missing entries coexist:
/// - an incoming stored entry (with a digest) is skipped when its digest
///   is already present;
/// - an incoming stored entry **replaces** an existing missing entry of
///   the same name (the re-import found the file; the reference is
///   upgraded in place);
/// - an incoming missing entry is skipped when any entry — stored or
///   missing — already carries its name (a text-only re-import must not
///   stack a second missing marker for the same file);
/// - anything else appends.
///
/// An all-duplicates merge is a no-op, so re-imports stay idempotent.
fn attach_media_in_tx(
    tx: &Transaction<'_>,
    id: i64,
    entries: &[serde_json::Value],
) -> Result<bool> {
    if entries.is_empty() {
        return Ok(false);
    }
    let existing_raw: Option<String> = tx
        .query_row(
            "SELECT raw_json FROM archive_messages WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(existing_raw) = existing_raw else {
        return Ok(false);
    };
    let mut raw: serde_json::Value = serde_json::from_str(&existing_raw)
        .unwrap_or_else(|_| serde_json::Value::Object(Default::default()));
    if !raw.is_object() {
        raw = serde_json::Value::Object(Default::default());
    }
    let obj = raw.as_object_mut().expect("just wrapped into an object");
    let media = obj
        .entry("media".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if !media.is_array() {
        *media = serde_json::Value::Array(Vec::new());
    }
    let array = media.as_array_mut().expect("just coerced to array");
    let mut known: std::collections::HashSet<String> = array
        .iter()
        .filter_map(|e| {
            e.get("sha256")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .collect();
    let known_names: std::collections::HashSet<String> = array
        .iter()
        .filter_map(|e| {
            e.get("name")
                .and_then(serde_json::Value::as_str)
                .filter(|n| !n.is_empty())
                .map(str::to_string)
        })
        .collect();
    let mut changed = false;
    for entry in entries {
        let sha = entry.get("sha256").and_then(serde_json::Value::as_str);
        let name = entry
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if let Some(sha) = sha.filter(|s| !s.is_empty()) {
            if known.contains(sha) {
                continue;
            }
            // Upgrade: this name was recorded missing, and the bytes
            // exist now — replace the missing marker with the real link.
            array.retain(|e| {
                !(e.get("missing").and_then(serde_json::Value::as_bool) == Some(true)
                    && e.get("name").and_then(serde_json::Value::as_str) == Some(name))
            });
            array.push(entry.clone());
            known.insert(sha.to_string());
            changed = true;
        } else {
            // A missing entry: one marker per name is enough.
            if known_names.contains(name) {
                continue;
            }
            array.push(entry.clone());
            changed = true;
        }
    }
    if changed {
        tx.execute(
            "UPDATE archive_messages SET raw_json = ?1 WHERE id = ?2",
            params![raw.to_string(), id],
        )?;
    }
    Ok(changed)
}

/// Find the legacy source instance for `namespace`, creating an unresolved
/// one when the namespace is new (kind/label record what is actually known:
/// a legacy-namespaced source with no connector provenance).
fn resolve_legacy_instance(conn: &Connection, namespace: &str) -> Result<String> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM source_instances WHERE legacy_namespace = ?1",
            params![namespace],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    let id = new_uuid();
    conn.execute(
        "INSERT INTO source_instances (id, legacy_namespace, kind, label)
         VALUES (?1, ?2, 'legacy', ?2)",
        params![id, namespace],
    )?;
    Ok(id)
}

/// Find the legacy conversation for `msg.chat_id` within `instance`,
/// creating a legacy-unresolved one when the chat is new. Used when a
/// message carries no stable key and no reply refs — sources that do
/// carry one resolve through [`resolve_provider_conversation`] or
/// [`resolve_email_thread`] instead.
fn resolve_legacy_conversation(
    conn: &Connection,
    instance: &str,
    msg: &NewMessage,
) -> Result<String> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM conversations
             WHERE source_instance_id = ?1 AND legacy_chat_key = ?2",
            params![instance, msg.chat_id],
            |row| row.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    let id = new_uuid();
    conn.execute(
        "INSERT INTO conversations
             (id, source_instance_id, legacy_chat_key, label, kind, confidence)
         VALUES (?1, ?2, ?3, ?3, ?4, 'legacy_unresolved')",
        params![id, instance, msg.chat_id, msg.channel],
    )?;
    Ok(id)
}

/// Resolve the conversation for a message per M1.3: a stable provider
/// key wins (identity from the export itself), then the email reply
/// graph, then the legacy `chat_id` grouping; `None` stands alone.
fn resolve_conversation(
    conn: &Connection,
    instance: &str,
    msg: &NewMessage,
) -> Result<Option<String>> {
    if !msg.conversation_key.is_empty() {
        return Ok(Some(resolve_provider_conversation(conn, instance, msg)?));
    }
    if !msg.reply_refs.is_empty() {
        return Ok(Some(resolve_email_thread(conn, instance, msg)?));
    }
    if !msg.chat_id.is_empty() {
        return Ok(Some(resolve_legacy_conversation(conn, instance, msg)?));
    }
    Ok(None)
}

/// The conversation carrying `key` in `instance`, if any.
fn find_provider_conversation(
    conn: &Connection,
    instance: &str,
    key: &str,
) -> Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM conversations
         WHERE source_instance_id = ?1 AND provider_key = ?2",
        params![instance, key],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Find or create the conversation for a message carrying a stable
/// provider key (`telegram:chat:<id>`, `whatsapp:chat:sha256:…`, …).
/// The key is the identity: re-imports after a rename resolve here and
/// only the label follows the newest display name.
fn resolve_provider_conversation(
    conn: &Connection,
    instance: &str,
    msg: &NewMessage,
) -> Result<String> {
    if let Some(id) = find_provider_conversation(conn, instance, &msg.conversation_key)? {
        if !msg.chat_id.is_empty() {
            conn.execute(
                "UPDATE conversations SET label = ?2 WHERE id = ?1 AND label <> ?2",
                params![id, msg.chat_id],
            )?;
        }
        return Ok(id);
    }
    let id = new_uuid();
    let label = if msg.chat_id.is_empty() {
        msg.conversation_key.clone()
    } else {
        msg.chat_id.clone()
    };
    conn.execute(
        "INSERT INTO conversations
             (id, source_instance_id, legacy_chat_key, provider_key, label, kind, confidence)
         VALUES (?1, ?2, NULL, ?3, ?4, ?5, 'verified')",
        params![id, instance, msg.conversation_key, label, msg.channel],
    )?;
    Ok(id)
}

/// Resolve the conversation for an email carrying reply-graph refs
/// (`In-Reply-To` + `References`, root-most first — see [`crate::mail`]).
///
/// The reply graph is resolved one hop at a time, never traversed, so
/// incomplete chains and reference cycles can never loop:
///
/// 1. The first ref that matches an already-stored identity with a
///    conversation joins that conversation — whichever it is, so a
///    reply that cross-links two threads lands in the root-most one
///    seen first.
/// 2. Otherwise the conversation is keyed `email:thread:<root-most ref>`:
///    the thread root's own message id. The root itself carries no refs,
///    so when it arrives after its replies it stays standalone until the
///    next reply in the thread derives the key and adopts it (and any
///    other conversationless message the refs reach).
///
/// Both paths also *adopt*: refs whose stored messages never got a
/// conversation (a parent imported before M1.3, or a root that arrived
/// alone) are attached to the resolved thread.
fn resolve_email_thread(conn: &Connection, instance: &str, msg: &NewMessage) -> Result<String> {
    for reference in &msg.reply_refs {
        let joined: Option<String> = conn
            .query_row(
                "SELECT m.conversation_id
                 FROM message_identities AS mi
                 JOIN archive_messages AS m ON m.id = mi.message_id
                 WHERE mi.source_instance_id = ?1 AND mi.namespace = 'legacy'
                   AND mi.revision_hash = '' AND mi.external_key = ?2
                   AND m.conversation_id IS NOT NULL
                 LIMIT 1",
                params![instance, reference],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(conversation) = joined {
            adopt_refs(conn, instance, &msg.reply_refs, &conversation)?;
            return Ok(conversation);
        }
    }

    let key = format!("email:thread:{}", msg.reply_refs[0]);
    if let Some(id) = find_provider_conversation(conn, instance, &key)? {
        adopt_refs(conn, instance, &msg.reply_refs, &id)?;
        return Ok(id);
    }
    let id = new_uuid();
    // Display label: the mail's subject (the first body line for mail
    // bodies, see [`crate::mail`]) when present, else the key itself.
    let label = msg
        .body
        .split('\n')
        .next()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| key.clone());
    conn.execute(
        "INSERT INTO conversations
             (id, source_instance_id, legacy_chat_key, provider_key, label, kind, confidence)
         VALUES (?1, ?2, NULL, ?3, ?4, 'email', 'verified')",
        params![id, instance, key, label],
    )?;
    adopt_refs(conn, instance, &msg.reply_refs, &id)?;
    Ok(id)
}

/// Attach already-stored refs of one source instance that have no
/// conversation yet to `conversation` (see [`resolve_email_thread`]).
fn adopt_refs(
    conn: &Connection,
    instance: &str,
    refs: &[String],
    conversation: &str,
) -> Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; refs.len()].join(", ");
    let sql = format!(
        "UPDATE archive_messages SET conversation_id = ?1
         WHERE conversation_id IS NULL AND id IN (
             SELECT mi.message_id FROM message_identities AS mi
             WHERE mi.source_instance_id = ?2 AND mi.namespace = 'legacy'
               AND mi.revision_hash = ''
               AND mi.external_key IN ({placeholders}))"
    );
    let mut bind: Vec<SqlValue> =
        vec![conversation.to_string().into(), instance.to_string().into()];
    bind.extend(refs.iter().cloned().map(SqlValue::from));
    conn.execute(&sql, params_from_iter(bind))?;
    Ok(())
}

/// The time-provenance triple `(original_timestamp, timezone,
/// time_confidence)` for an insert.
///
/// Messages that carry no explicit provenance fields keep the M1.1
/// behavior: the resolved instant doubles as the original text, no zone
/// is recorded, and the confidence is derived from `sent_at`
/// (`missing`/`legacy_assumed`). An explicit confidence label must be one
/// of the canonical-store CHECK values.
fn time_provenance(msg: &NewMessage) -> Result<(&str, &str, &str)> {
    let derived = if msg.sent_at.is_empty() {
        "missing"
    } else {
        "legacy_assumed"
    };
    if msg.time_confidence.is_empty()
        && msg.timezone.is_empty()
        && msg.original_timestamp.is_empty()
    {
        return Ok((msg.sent_at.as_str(), "", derived));
    }
    let confidence = if msg.time_confidence.is_empty() {
        derived
    } else if TIME_CONFIDENCE.contains(&msg.time_confidence.as_str()) {
        msg.time_confidence.as_str()
    } else {
        return Err(Error::ChatImport(format!(
            "invalid time_confidence {:?} (expected one of {TIME_CONFIDENCE:?})",
            msg.time_confidence
        )));
    };
    let original = if msg.original_timestamp.is_empty() {
        msg.sent_at.as_str()
    } else {
        msg.original_timestamp.as_str()
    };
    Ok((original, msg.timezone.as_str(), confidence))
}

/// The canonical time-confidence labels (the `archive_messages
/// .time_confidence` CHECK values).
const TIME_CONFIDENCE: [&str; 5] = [
    "explicit_offset",
    "user_confirmed",
    "legacy_assumed",
    "unresolved",
    "missing",
];

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
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

/// A connector's registration of a durable source instance
/// (see [`Storage::ensure_source_instance`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSourceInstance<'a> {
    /// Connector kind, e.g. `"imap"`. Kinds are connector-controlled
    /// enums — only a connector constructs its own kind.
    pub kind: &'a str,
    /// Human-readable label; display only, never identity.
    pub label: &'a str,
    /// Canonical connector identity key (connector-defined; for IMAP:
    /// case-folded host, port, TLS mode, exact username). Two
    /// configurations producing the same key are the same instance.
    pub connector_key: &'a str,
    /// Non-secret connector configuration as JSON (validated by the
    /// connector). Credentials never appear here.
    pub config_json: &'a str,
}

/// A durable source instance as stored (see
/// [`Storage::list_source_instances`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInstance {
    pub id: String,
    /// Connector kind (`"imap"`, …) or `"legacy"` for pre-canonical
    /// namespaces.
    pub kind: String,
    pub label: String,
    /// Canonical connector identity key; `None` for legacy-named
    /// instances that no connector has claimed.
    pub connector_key: Option<String>,
    /// Non-secret configuration JSON.
    pub config_json: String,
    /// `"connected"` after a successful sync, `"disconnected"` otherwise.
    pub state: String,
    /// Per-source exclusion (M2.3): coordinator rounds skip excluded
    /// sources while keeping their archive and cursors.
    pub excluded: bool,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub created_at: String,
}

fn source_instance_from_row(row: &Row<'_>) -> rusqlite::Result<SourceInstance> {
    Ok(SourceInstance {
        id: row.get(0)?,
        kind: row.get(1)?,
        label: row.get(2)?,
        connector_key: row.get(3)?,
        config_json: row.get(4)?,
        state: row.get(5)?,
        excluded: row.get::<_, i64>(6)? != 0,
        last_attempt_at: row.get(7)?,
        last_success_at: row.get(8)?,
        created_at: row.get(9)?,
    })
}

/// Connector-scoped message identity for [`Storage::insert_mail_message`]:
/// the namespace a connector owns (e.g. `"imap"`), the external key within
/// it (normalized `Message-ID`, or the SHA-256 of the raw message bytes
/// when the mail has none — see [`crate::mail::parse_raw_mail_canonical`]),
/// and the semantic revision hash over decoded content and stable headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorIdentity<'a> {
    pub namespace: &'a str,
    pub external_key: &'a str,
    pub revision_hash: &'a str,
}

/// One observation of a message by a connector: the remote folder (or
/// import scope), the provider's item id within it (the IMAP UID), and
/// the folder's `UIDVALIDITY` where applicable. The unique occurrence
/// tuple `(message, scope, provider_item_id)` keeps repeat observations
/// from adding provenance rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence<'a> {
    pub scope: &'a str,
    pub provider_item_id: &'a str,
    pub uidvalidity: Option<u32>,
}

/// Outcome of a connector mail insert
/// (see [`Storage::insert_mail_message`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailInsertOutcome {
    /// New archived row. `conflicting_revision` is `true` when another
    /// revision of the same external key was already archived under this
    /// instance — both revisions are preserved and searchable.
    Inserted { id: i64, conflicting_revision: bool },
    /// The exact identity (same external key *and* revision) was already
    /// archived; only the occurrence was recorded.
    Duplicate { id: i64 },
}

/// One parsed mail of an IMAP fetch batch, ready for
/// [`Storage::commit_imap_batch`]: the message (folder fields already
/// applied), its subject, and the identity + occurrence it archives under.
#[derive(Debug, Clone)]
pub struct MailBatchItem<'a> {
    pub message: &'a NewMessage,
    pub subject: &'a str,
    pub identity: ConnectorIdentity<'a>,
    pub occurrence: Occurrence<'a>,
}

/// One failed item of an IMAP fetch batch (M2.3): the uid the round
/// accounted but could not store, with a fixed human-readable reason —
/// never message content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchFailure<'a> {
    pub uid: u32,
    pub reason: &'a str,
}

/// Counts of one atomic batch commit
/// (see [`Storage::commit_imap_batch`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MailBatchOutcome {
    /// Messages newly stored in the batch.
    pub inserted: u64,
    /// Messages the batch skipped as already archived (dedup).
    pub skipped: u64,
    /// Items the batch led to the failed-item ledger.
    pub failed: u64,
    /// Newly stored rows that are a **conflicting revision** of an
    /// already-archived external key.
    pub conflicting_revisions: u64,
}

/// One row of the failed-item ledger
/// (see [`Storage::list_failed_items`]): what fell, where, why, how many
/// times the vault has retried it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedItem {
    pub source_instance_id: String,
    pub folder: String,
    pub uid: u32,
    pub uidvalidity: u32,
    /// Fixed human-readable cause; never message content.
    pub reason: String,
    /// How many rounds have observed the failure (1 on first record).
    pub attempts: u32,
    pub first_failed_at: String,
    pub last_failed_at: String,
}

/// One account's overview counts for the multi-account UI (roadmap: the
/// accounts panel): the archived-message count of one durable source
/// instance plus its open rows in the failed-item ledger (see
/// [`Storage::account_counts`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCounts {
    /// The durable source instance id (same ids as
    /// [`Storage::list_source_instances`]).
    pub source_id: String,
    /// Archived messages of the account (trigger-maintained counter).
    pub messages: u64,
    /// Open rows in the IMAP failed-item ledger (0 for import accounts).
    pub failed_items: u64,
}

/// A recorded observation of one archived message
/// (see [`Storage::message_occurrences`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageOccurrence {
    /// Remote folder name or import scope.
    pub scope: String,
    /// Provider item id within the scope (e.g. the IMAP UID).
    pub provider_item_id: String,
    pub uidvalidity: Option<u32>,
    pub observed_at: String,
}

/// One archived revision behind an identity external key
/// (see [`Storage::message_revisions`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRevision {
    pub message_id: i64,
    pub revision_hash: String,
}

/// Coverage counts for one embedding model (see [`Storage::embedding_counts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingCounts {
    /// Messages with a current-model embedding row.
    pub embedded: u64,
    /// Total messages in the archive.
    pub messages: u64,
}

/// Parse an optional `YYYY-MM-DD` bound into a `NaiveDate`. Years outside
/// `0000..=9999` (chrono also accepts signed 5+ digit spellings) are
/// rejected: they cannot be ordered as text against the stored RFC 3339
/// instants.
fn parse_date_bound(value: Option<&str>, field: &str) -> Result<Option<NaiveDate>> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => NaiveDate::parse_from_str(v, "%Y-%m-%d")
            .ok()
            .filter(|d| (0..=9999).contains(&d.year()))
            .map(Some)
            .ok_or_else(|| Error::InvalidDate(format!("{field}={v}"))),
    }
}

fn hit_from_row(row: &Row<'_>) -> rusqlite::Result<SearchHit> {
    Ok(SearchHit {
        message: message_from_row(row)?,
        snippet: row.get(9)?,
    })
}

/// One durable parse-failure record handed to
/// [`Storage::commit_import_batch`]: `position` is the failed item's
/// ordinal in the import's item stream, `external_ref` the best-effort
/// upstream reference (often the broken entry's id, empty when the entry
/// carried none), `reason` a short parser-level explanation (never raw
/// file bytes).
#[derive(Debug, Clone)]
pub struct ImportIssueRecord<'a> {
    pub position: u64,
    pub external_ref: &'a str,
    pub reason: &'a str,
}

/// A stored issue record, as returned by [`Storage::import_issues`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportIssue {
    pub id: i64,
    pub import_id: String,
    pub position: u64,
    pub external_ref: String,
    pub reason: String,
    pub recorded_at: String,
}

/// Per-batch counters returned by [`Storage::commit_import_batch`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportBatchCounts {
    /// Messages newly stored by this batch.
    pub inserted: u64,
    /// Messages skipped because their identity already existed.
    pub skipped: u64,
    /// Messages whose media links were written or backfilled (inserts
    /// carrying a `media` array plus duplicates that gained entries).
    pub media_linked: u64,
}

/// One row of the `media_objects` registry (M4.2): an object stored
/// without a message link — an export file that belonged to no message —
/// with its provenance. Message-linked objects stay unregistered; their
/// messages are their reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredMediaObject {
    pub sha256: String,
    pub size: u64,
    pub name: String,
    pub kind: String,
    /// How the object entered the vault (`"unlinked"`).
    pub origin: String,
    pub import_id: String,
    pub first_seen: String,
}

/// Filter SQL fragments (`AND ...` clauses referencing table aliases `m`
/// and `si`) plus the bound values for them, shared by the page and count
/// statements of [`Storage::search_page`]. Every statement joins
/// `source_instances` (alias `si`) — `Message.source` is read through its
/// unique `legacy_namespace` index and the source facet filters on it.
struct QueryFilters {
    sql: String,
    values: Vec<SqlValue>,
}

impl QueryFilters {
    fn build(query: &SearchQuery) -> Result<Self> {
        let mut sql = String::new();
        let mut values: Vec<SqlValue> = Vec::new();

        if !query.sources.is_empty() {
            let placeholders = vec!["?"; query.sources.len()].join(", ");
            // Match the same source string the API reports (`Message.source`:
            // the legacy namespace, or the connector label for connector-owned
            // sources — never the raw instance id) so a facet value taken from
            // stats/search filters the rows it came from.
            sql.push_str(&format!(
                " AND COALESCE(si.legacy_namespace, si.label, si.id) IN ({placeholders})"
            ));
            values.extend(query.sources.iter().cloned().map(SqlValue::from));
        }
        let author = query.author.trim();
        if !author.is_empty() {
            sql.push_str(" AND fold(m.author) LIKE fold(?) ESCAPE '\\'");
            values.push(SqlValue::from(format!(
                "%{}%",
                crate::query::escape_like(author)
            )));
        }
        if !query.from_date.is_empty() {
            // Local-day semantics: include everything from local midnight of
            // the from day on (the UI renders the same local day). The
            // midnight is resolved per date, so historical DST periods use
            // their own offset.
            if let Some(day) = crate::query::parse_day(&query.from_date) {
                sql.push_str(" AND m.sent_instant >= ?");
                values.push(SqlValue::from(crate::query::local_day_instant_utc(day)?));
            }
        }
        if !query.to_date.is_empty() {
            // Exclusive bound: strictly before local midnight of the day
            // after, so everything later on to_date itself still matches.
            if let Some(day) = crate::query::parse_day(&query.to_date) {
                let next = day
                    .checked_add_days(Days::new(1))
                    .filter(|next| (0..=9999).contains(&next.year()))
                    .ok_or_else(|| {
                        Error::InvalidDate(format!("date_to={} out of range", query.to_date))
                    })?;
                sql.push_str(" AND m.sent_instant < ?");
                values.push(SqlValue::from(crate::query::local_day_instant_utc(next)?));
            }
        }
        if !query.from_date.is_empty() || !query.to_date.is_empty() {
            // A message without a resolved instant has no defined position
            // on the timeline; exclude it whenever a date filter is active
            // so both bounds agree on the same set.
            sql.push_str(" AND m.sent_instant IS NOT NULL");
        }
        Ok(Self { sql, values })
    }

    fn bind(&self) -> Vec<SqlValue> {
        self.values.clone()
    }
}

/// Fold a string for the author filter's case-insensitive comparison:
/// Unicode-lowercased, with whitespace runs collapsed to single spaces.
fn fold_for_search(s: &str) -> String {
    s.split_whitespace()
        .map(|w| w.chars().flat_map(char::to_lowercase).collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Register `fold` as a deterministic scalar SQL function on `conn`, so the
/// author filter can compare Unicode-cased substrings (SQLite's built-in
/// `lower()` and its LIKE case-folding are ASCII-only).
fn register_fold_function(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        "fold",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let value = ctx.get_raw(0);
            match value {
                rusqlite::types::ValueRef::Text(bytes) => {
                    let s = std::str::from_utf8(bytes).map_err(|_| {
                        rusqlite::Error::InvalidColumnType(
                            0,
                            "author".into(),
                            rusqlite::types::Type::Text,
                        )
                    })?;
                    Ok(Some(fold_for_search(s)))
                }
                _ => Ok(None),
            }
        },
    )?;
    Ok(())
}

/// A fresh random UUID string (canonical source/conversation ids).
fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// SHA-256 fingerprint of a message's stable payload fields, joined by
/// U+001F unit separators: `external_id`, `author`, `sent_at`, `body`.
///
/// The exact same join is computed by the migration's SQL
/// (`sha256_hex(... || char(31) || ...)`), so a migrated row and a row
/// inserted through the current path hash identically. The richer semantic
/// revision hash over decoded content arrives with the revisions slice;
/// this fingerprint is the foundation it builds on.
fn content_hash(msg: &NewMessage) -> String {
    sha256_hex(&format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}",
        msg.external_id, msg.author, msg.sent_at, msg.body
    ))
}

fn sha256_hex(input: &str) -> String {
    crate::util::sha256_hex(input.as_bytes())
}

/// The UTC instant a fresh import on this machine derives from a legacy
/// blind-`Z` stamp (`2026-07-01T09:00:05Z`): the exporter-local wall time
/// interpreted in the machine's local zone, formatted exactly like the
/// live parser's `sent_at`. A wall-time a DST gap removes (or any
/// unparseable stamp) has no instant and resolves to the empty string,
/// exactly like the parser's unresolved marker.
fn resolve_blind_z_utc(text: &str) -> String {
    const RFC3339: &str = "%Y-%m-%dT%H:%M:%SZ";
    let Some(naive_text) = text.strip_suffix('Z') else {
        return String::new();
    };
    let Ok(naive) = NaiveDateTime::parse_from_str(naive_text, "%Y-%m-%dT%H:%M:%S") else {
        return String::new();
    };
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc).format(RFC3339).to_string(),
        // A DST fold repeats the wall-time: the first occurrence, the
        // same choice the parser makes everywhere else.
        chrono::LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc).format(RFC3339).to_string(),
        // A DST gap removed this wall-time from the calendar: no instant
        // exists to resolve to.
        chrono::LocalResult::None => String::new(),
    }
}

/// Register the canonical-store SQL functions used by migration 4 (and any
/// caller of the same formulas): `vault_uuid()` and `sha256_hex(text)`.
/// SQLite bundles neither.
fn register_canonical_functions(conn: &Connection) -> Result<()> {
    conn.create_scalar_function(
        "vault_uuid",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        |_| Ok(Some(new_uuid())),
    )?;
    conn.create_scalar_function(
        "sha256_hex",
        1,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8
            | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let value = ctx.get_raw(0);
            match value {
                rusqlite::types::ValueRef::Text(bytes) => {
                    let s = std::str::from_utf8(bytes).map_err(|_| {
                        rusqlite::Error::InvalidColumnType(
                            0,
                            "sha256_hex".into(),
                            rusqlite::types::Type::Text,
                        )
                    })?;
                    Ok(Some(sha256_hex(s)))
                }
                _ => Ok(None),
            }
        },
    )?;
    Ok(())
}

/// The schema version recorded in the database, or 0 when the
/// bookkeeping table does not exist yet. Read-only: safe to call before
/// any pragma or migration has touched the file.
pub(crate) fn read_schema_version(conn: &Connection) -> Result<i64> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(0);
    }
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?)
}

/// Apply pending migrations, exactly once each, in one single
/// transaction (M1.4: the conversion is all-or-nothing), then validate
/// the converted indexes. Any failure rolls the database back to its
/// pre-conversion state — first logically (transaction rollback), then,
/// when a snapshot exists, physically ([`crate::safe_migration`]).
fn migrate(conn: &Connection, opts: &OpenOptions) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )?;
    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    let pending: Vec<&(i64, &str)> = MIGRATIONS.iter().filter(|(v, _)| *v > current).collect();
    if pending.is_empty() {
        return Ok(());
    }
    {
        let tx = conn.unchecked_transaction()?;
        for &&(version, sql) in &pending {
            tx.execute_batch(sql)?;
            if let Some(&(_, hook)) = RUST_MIGRATIONS.iter().find(|(v, _)| *v == version) {
                hook(&tx)?;
            }
            tx.execute(
                "INSERT INTO schema_migrations (version) VALUES (?1)",
                params![version],
            )?;
        }
        safe_migration::inject(opts, MigrationFailPoint::DuringMigration)?;
        tx.commit()?;
    }
    // Post-conversion validation (M1.4): the committed database must
    // pass whole-file integrity and have every full-text index in
    // agreement with its content table. A failure here triggers the
    // snapshot rollback like any other migration failure.
    safe_migration::inject(opts, MigrationFailPoint::AfterMigration)?;
    safe_migration::validate_post_migration(conn)
        .map_err(|e| Error::MigrationValidationFailed(e.to_string()))?;
    Ok(())
}

// -----------------------------------------------------------------------
// Migration 7 (M1.3): stable conversation keys over the M1.1 tables
// -----------------------------------------------------------------------

/// Backfill stable conversation keys (migration 7). See the migration's
/// comment for the design; everything here is conservative — a
/// conversation whose key cannot be derived safely is left exactly as
/// M1.1 left it (`legacy_unresolved`).
fn backfill_conversation_keys(conn: &Connection) -> Result<()> {
    backfill_provider_id_conversations(conn)?;
    backfill_whatsapp_conversations(conn)?;
    Ok(())
}

/// Telegram/Signal half: the provider-guaranteed chat id is recoverable
/// from the archived external ids (`<chat-id>/<message-id>`), so every
/// message re-groups onto the conversation keyed by its own prefix —
/// merging chats that pre-M1.3 grouping split across two names (a
/// re-import under a rename) and splitting chats that legacy name
/// grouping wrongly merged (two different chats sharing a display name).
/// Messages without the `<chat-id>/` shape stay in their legacy
/// conversation, unresolved.
fn backfill_provider_id_conversations(conn: &Connection) -> Result<()> {
    // (conversation, instance, namespace) for every unkeyed provider-id
    // source conversation.
    let conversations: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT c.id, si.id, si.legacy_namespace
             FROM conversations AS c
             JOIN source_instances AS si ON si.id = c.source_instance_id
             WHERE si.legacy_namespace IN (?1, ?2)
               AND c.provider_key IS NULL",
        )?;
        let rows = stmt
            .query_map(
                params![
                    crate::telegram::TELEGRAM_SOURCE,
                    crate::signal::SIGNAL_SOURCE
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?
            .collect::<std::result::Result<_, _>>()?;
        rows
    };

    // Determinism is data-driven: process conversations by their earliest
    // archived message, so when several legacy conversations collapse onto
    // one provider key the first import is keyed in place and its label
    // survives — independent of row order in the table.
    let mut conversations = conversations;
    {
        let mut keyed: Vec<(i64, (String, String, String))> = Vec::new();
        for entry in conversations.drain(..) {
            let earliest: i64 = conn.query_row(
                "SELECT MIN(id) FROM archive_messages WHERE conversation_id = ?1",
                params![entry.0],
                |row| row.get(0),
            )?;
            keyed.push((earliest, entry));
        }
        keyed.sort_by_key(|(earliest, _)| *earliest);
        conversations = keyed.into_iter().map(|(_, entry)| entry).collect();
    }

    for (conversation, instance, namespace) in conversations {
        // Group this conversation's messages by external-id prefix.
        let groups: Vec<(String, Vec<i64>)> = {
            let mut stmt = conn.prepare(
                "SELECT CASE WHEN m.external_id LIKE '%/%'
                         THEN substr(m.external_id, 1, instr(m.external_id, '/') - 1)
                         ELSE '' END AS prefix,
                        m.id
                 FROM archive_messages AS m
                 WHERE m.conversation_id = ?1
                 ORDER BY m.id",
            )?;
            let mut by_prefix: std::collections::BTreeMap<String, Vec<i64>> =
                std::collections::BTreeMap::new();
            for row in stmt.query_map(params![conversation], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (prefix, id) = row?;
                by_prefix.entry(prefix).or_default().push(id);
            }
            by_prefix.into_iter().collect()
        };

        for (prefix, ids) in &groups {
            if !prefix_is_provider_id(&namespace, prefix) {
                continue; // not safely interpretable; stays legacy-unresolved
            }
            let key = format!("{namespace}:chat:{prefix}");
            // Find or create the keyed conversation for this prefix.
            let target = match find_provider_conversation(conn, &instance, &key)? {
                Some(id) => id,
                None => {
                    // If this conversation holds only this prefix, key it
                    // in place; otherwise spawn a dedicated conversation.
                    let sole_group = groups.len() == 1;
                    if sole_group {
                        conn.execute(
                            "UPDATE conversations SET provider_key = ?2,
                                 confidence = 'verified' WHERE id = ?1",
                            params![conversation, key],
                        )?;
                        conversation.clone()
                    } else {
                        let id = new_uuid();
                        let (label, kind): (String, String) = conn.query_row(
                            "SELECT label, kind FROM conversations WHERE id = ?1",
                            params![conversation],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )?;
                        conn.execute(
                            "INSERT INTO conversations
                                 (id, source_instance_id, legacy_chat_key,
                                  provider_key, label, kind, confidence)
                             VALUES (?1, ?2, NULL, ?3, ?4, ?5, 'verified')",
                            params![id, instance, key, label, kind],
                        )?;
                        id
                    }
                }
            };
            for message_id in ids {
                conn.execute(
                    "UPDATE archive_messages SET conversation_id = ?1 WHERE id = ?2",
                    params![target, message_id],
                )?;
            }
        }

        // Drop the legacy conversation when nothing is left in it.
        let remaining: i64 = conn.query_row(
            "SELECT COUNT(*) FROM archive_messages WHERE conversation_id = ?1",
            params![conversation],
            |row| row.get(0),
        )?;
        if remaining == 0 {
            conn.execute(
                "DELETE FROM conversations WHERE id = ?1 AND provider_key IS NULL",
                params![conversation],
            )?;
        }
    }
    Ok(())
}

/// Whether an external-id prefix can only be a provider chat id of the
/// namespace and never a display name: Telegram chat ids are integers
/// without a leading `+` (a display name is neither — phone-number-shaped
/// names carry the `+`), Signal conversation ids are UUIDs (a file stem is
/// not). Anything else is not safely interpretable and stays
/// legacy-unresolved, matching the live parsers, which key nothing for
/// id-less exports.
fn prefix_is_provider_id(namespace: &str, prefix: &str) -> bool {
    if namespace == crate::telegram::TELEGRAM_SOURCE {
        !prefix.starts_with('+') && prefix.parse::<i64>().is_ok()
    } else {
        uuid::Uuid::parse_str(prefix).is_ok()
    }
}

/// WhatsApp half: derive the content key — sorted participants plus the
/// first authored message, the same frozen formula as the parser — merge
/// renamed duplicates onto it, then re-key every WhatsApp message
/// identity to the M1.3 hash formula (conversation key, author, naive
/// timestamp, body) so re-imports after a rename dedup instead of
/// duplicating. Author-less exports cannot be keyed; they keep the
/// legacy stem grouping but their identities still re-key to the new
/// formula over the stem.
fn backfill_whatsapp_conversations(conn: &Connection) -> Result<()> {
    // (conversation, instance, legacy stem) for every WhatsApp conversation.
    let conversations: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT c.id, si.id, COALESCE(c.legacy_chat_key, c.label)
             FROM conversations AS c
             JOIN source_instances AS si ON si.id = c.source_instance_id
             WHERE si.legacy_namespace = ?1",
        )?;
        let rows = stmt
            .query_map(params![crate::whatsapp::WHATSAPP_SOURCE], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        rows
    };

    let mut candidates: Vec<(String, String, String)> = Vec::new();
    for (conversation, instance, _) in &conversations {
        let authors: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT author FROM archive_messages
                 WHERE conversation_id = ?1 AND author != ''",
            )?;
            let mut rows: Vec<String> = stmt
                .query_map(params![conversation], |row| row.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            rows.sort();
            rows
        };
        if authors.is_empty() {
            continue; // nothing in the export identifies the chat safely
        }
        // The anchor: the first authored message by insertion order
        // (imports follow the export's file order), with the naive
        // timestamp (legacy rows carry the exporter-local stamp with a
        // blind `Z`, which the shared formula strips).
        let anchor: Option<(String, String, String)> = conn
            .query_row(
                "SELECT author, original_timestamp, body
                 FROM archive_messages
                 WHERE conversation_id = ?1 AND author != ''
                 ORDER BY id LIMIT 1",
                params![conversation],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let key = crate::whatsapp::whatsapp_conversation_key(
            authors.iter().map(String::as_str),
            anchor.as_ref().map(|(author, stamp, body)| {
                (
                    author.as_str(),
                    stamp.strip_suffix('Z').unwrap_or(stamp.as_str()),
                    body.as_str(),
                )
            }),
        );
        if !key.is_empty() {
            candidates.push((conversation.clone(), instance.clone(), key));
        }
    }
    merge_conversations_onto_keys(conn, candidates)?;

    // Re-key every WhatsApp message to the M1.3 formula. The hash chat is
    // the derived key when present, else the legacy stem — matching what
    // the parser hashes for key-less exports.
    let mut resolved: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    for (conversation, _, _) in &conversations {
        let key: Option<String> = conn
            .query_row(
                "SELECT provider_key FROM conversations WHERE id = ?1",
                params![conversation],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        resolved.insert(conversation.clone(), key);
    }
    let zone_label = iana_time_zone::get_timezone().unwrap_or_else(|_| "local".to_string());
    for (conversation, _, stem) in &conversations {
        let hash_chat = resolved
            .get(conversation)
            .and_then(|k| k.as_ref().filter(|k| !k.is_empty()).cloned())
            .unwrap_or_else(|| stem.clone());
        let rows: Vec<(i64, String, String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT id, author, original_timestamp, body
                 FROM archive_messages WHERE conversation_id = ?1
                 ORDER BY id",
            )?;
            let out = stmt
                .query_map(params![conversation], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<std::result::Result<_, _>>()?;
            out
        };
        let mut occurrence: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
        for (id, author, original_timestamp, body) in rows {
            let naive = original_timestamp
                .strip_suffix('Z')
                .unwrap_or(&original_timestamp);
            let base = crate::util::fnv1a_fields(&[&hash_chat, &author, naive, &body]);
            let seen = occurrence.entry(base).or_insert(0);
            let external_id = format!("generated-{:016x}-{}", base, seen);
            *seen += 1;
            let resolved = resolve_blind_z_utc(&original_timestamp);
            let sent_instant: Option<&str> = if resolved.is_empty() {
                None
            } else {
                Some(resolved.as_str())
            };
            let confidence = if original_timestamp.is_empty() {
                "missing"
            } else if resolved.is_empty() {
                "unresolved"
            } else {
                "legacy_assumed"
            };
            let content_hash = sha256_hex(&format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                external_id, author, resolved, body
            ));
            conn.execute(
                "UPDATE archive_messages SET external_id = ?2, sent_instant = ?3,
                     timezone = ?4, time_confidence = ?5, content_hash = ?6
                 WHERE id = ?1",
                params![
                    id,
                    external_id,
                    sent_instant,
                    zone_label,
                    confidence,
                    content_hash
                ],
            )?;
            conn.execute(
                "UPDATE message_identities SET external_key = ?2
                 WHERE message_id = ?1 AND namespace = 'legacy'",
                params![id, external_id],
            )?;
            conn.execute(
                "UPDATE message_occurrences SET provider_item_id = ?2 WHERE message_id = ?1",
                params![id, external_id],
            )?;
        }
    }
    Ok(())
}

/// Merge candidate conversations that derived the same provider key: the
/// conversation holding the earliest message survives (deterministic —
/// it is the first import), the others' messages re-point to it and the
/// losers are deleted; the survivor takes the key with confidence
/// `verified`.
fn merge_conversations_onto_keys(
    conn: &Connection,
    candidates: Vec<(String, String, String)>,
) -> Result<()> {
    let mut groups: std::collections::BTreeMap<(String, String), Vec<String>> =
        std::collections::BTreeMap::new();
    for (conversation, instance, key) in candidates {
        groups
            .entry((instance, key))
            .or_default()
            .push(conversation);
    }
    for ((_instance, key), conversations) in groups {
        let mut by_age: Vec<(i64, &String)> = Vec::with_capacity(conversations.len());
        for conversation in &conversations {
            let earliest: i64 = conn.query_row(
                "SELECT MIN(id) FROM archive_messages WHERE conversation_id = ?1",
                params![conversation],
                |row| row.get(0),
            )?;
            by_age.push((earliest, conversation));
        }
        by_age.sort();
        let survivor = by_age[0].1.clone();
        for (_, loser) in &by_age[1..] {
            conn.execute(
                "UPDATE archive_messages SET conversation_id = ?1
                 WHERE conversation_id = ?2",
                params![survivor, loser],
            )?;
            conn.execute("DELETE FROM conversations WHERE id = ?1", params![loser])?;
        }
        conn.execute(
            "UPDATE conversations SET provider_key = ?2, confidence = 'verified'
             WHERE id = ?1",
            params![survivor, key],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the M1.4 post-conversion validation itself: it
    //! must *detect* a drifted or corrupt full-text index, not just run.
    //! (The injected-failure lifecycle lives in the integration suite,
    //! `tests/integrity/safe_migration.rs`.)

    use super::*;

    fn seeded() -> Storage {
        let s = Storage::open_in_memory().unwrap();
        for i in 0..5 {
            s.insert_message(&crate::message::NewMessage::new(
                "test",
                format!("m{i}"),
                "ana",
                "2026-01-01T00:00:00Z",
                format!("indexed body number {i}"),
            ))
            .unwrap();
        }
        s
    }

    #[test]
    fn validation_passes_on_a_healthy_database() {
        let s = seeded();
        safe_migration::validate_post_migration(&s.conn).unwrap();
    }

    #[test]
    fn validation_catches_a_drifted_fts_row_count() {
        let s = seeded();
        // Simulate an index that lost entries (e.g. a crash between the
        // content write and the trigger, or a torn rebuild): the docsize
        // parity check must catch what a plain open would silently accept
        // (`COUNT(*)` on the FTS table reads the content table and hides
        // the drift).
        s.conn
            .execute("DELETE FROM archive_fts WHERE rowid <= 2", [])
            .unwrap();
        let err = safe_migration::validate_post_migration(&s.conn)
            .expect_err("drifted index must fail validation");
        assert!(
            err.to_string().contains("archive_fts"),
            "error must name the drifted index: {err}"
        );
    }

    #[test]
    fn validation_catches_corrupt_fts_segments() {
        let s = seeded();
        // Structurally corrupt the index segments (same document count):
        // the FTS5 integrity-check command must flag the malformed index
        // even though every document is still accounted for.
        s.conn
            .execute("DELETE FROM archive_fts_data WHERE id % 7 = 3", [])
            .unwrap();
        let err = safe_migration::validate_post_migration(&s.conn)
            .expect_err("corrupt index must fail validation");
        assert!(
            err.to_string().contains("archive_fts"),
            "error must name the corrupt index: {err}"
        );
    }
}

#[cfg(test)]
mod media_merge_tests {
    //! M4.2 unit coverage for the stored/missing media merge rules and
    //! the `media_objects` reference union (integration coverage of the
    //! import flows lives in `tests/media_extract.rs`).

    use super::*;

    fn msg_with_media(s: &Storage, id: &str, entries: serde_json::Value) -> i64 {
        let mut m =
            crate::message::NewMessage::new("telegram", id, "a", "2026-01-01T00:00:00Z", "b");
        let mut raw = serde_json::json!({});
        raw["media"] = entries;
        m.raw_json = raw.to_string();
        match s.insert_message(&m).unwrap() {
            crate::message::InsertOutcome::Inserted(i) => i,
            _ => panic!("insert failed"),
        }
    }

    #[test]
    fn stored_entry_replaces_missing_marker_of_the_same_name() {
        let s = Storage::open_in_memory().unwrap();
        {
            let id = msg_with_media(
                &s,
                "1",
                serde_json::json!([{ "name": "pic.jpg", "kind": "image", "missing": true, "reason": "not-in-export" }]),
            );
            let changed = s
                .attach_media(
                    id,
                    &[serde_json::json!({
                        "sha256": "a".repeat(64), "path": format!("media/{}.jpg", "a".repeat(64)),
                        "name": "pic.jpg", "kind": "image", "size": 10,
                    })],
                )
                .unwrap();
            assert!(changed);
            let raw: serde_json::Value = serde_json::from_str(&raw_json(&s, id)).unwrap();
            let media = raw["media"].as_array().unwrap();
            assert_eq!(media.len(), 1, "upgrade replaces, not appends");
            assert_eq!(media[0]["sha256"], "a".repeat(64));
            assert!(media[0].get("missing").is_none());
        }
    }

    #[test]
    fn missing_entry_is_not_duplicated_per_name() {
        let s = Storage::open_in_memory().unwrap();
        {
            let marker = |name: &str, reason: &str| serde_json::json!({ "name": name, "kind": "document", "missing": true, "reason": reason });
            let id = msg_with_media(
                &s,
                "2",
                serde_json::json!([marker("gone.pdf", "not-in-export")]),
            );
            assert!(
                !s.attach_media(id, &[marker("gone.pdf", "not-in-export")])
                    .unwrap(),
                "same marker again: no-op"
            );
            // A different reason for the same name still does not stack
            // markers: one missing entry per name.
            assert!(
                !s.attach_media(id, &[marker("gone.pdf", "unreadable")])
                    .unwrap(),
                "one marker per name"
            );
            let second = marker("gone2.pdf", "not-in-export");
            assert!(s.attach_media(id, &[second]).unwrap(), "a new name appends");
            let raw: serde_json::Value = serde_json::from_str(&raw_json(&s, id)).unwrap();
            assert_eq!(raw["media"].as_array().unwrap().len(), 2);
        }
    }

    #[test]
    fn media_digests_union_message_links_and_registry() {
        let s = Storage::open_in_memory().unwrap();
        {
            let id = msg_with_media(
                &s,
                "3",
                serde_json::json!([{ "sha256": "b".repeat(64), "path": "media/x.jpg", "name": "x.jpg", "kind": "image", "size": 1 }]),
            );
            let _ = id;
            let registered = vec![crate::media::UnlinkedObject {
                sha256: "c".repeat(64),
                size: 2,
                name: "stray.png".into(),
                kind: "image".into(),
            }];
            s.commit_import_batch("imp", "telegram", &[], &[], &registered, 0, true)
                .unwrap();
            let digests = s.media_digests().unwrap();
            assert!(digests.contains(&"b".repeat(64)), "message-linked");
            assert!(digests.contains(&"c".repeat(64)), "registry row");
            let message_only = s.message_media_digests().unwrap();
            assert!(!message_only.contains(&"c".repeat(64)));
            let objects = s.unlinked_media_objects().unwrap();
            assert_eq!(objects.len(), 1);
            assert_eq!(objects[0].origin, "unlinked");
            // Re-registering the same digest is idempotent (AC3).
            s.commit_import_batch("imp", "telegram", &[], &[], &registered, 0, true)
                .unwrap();
            assert_eq!(s.unlinked_media_objects().unwrap().len(), 1);
        }
    }

    fn raw_json(s: &Storage, id: i64) -> String {
        s.conn
            .query_row(
                "SELECT raw_json FROM archive_messages WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
    }
}

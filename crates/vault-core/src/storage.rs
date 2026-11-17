//! SQLite storage: schema migrations, idempotent inserts, and FTS5 search.
//!
//! The database bundles SQLite with SQLCipher enabled (via `rusqlite`'s
//! `bundled-sqlcipher-vendored-openssl` feature), so no system SQLite is
//! required. FTS5 is enabled in the bundled build. At-rest encryption is
//! opt-in: it engages only when a key is supplied on open (see
//! [`crate::encryption`]); plaintext files open exactly as before.
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
//!
//! Since schema version 20 the vault also holds **people and
//! user-confirmed identity links** ([`Storage::create_person`],
//! [`Storage::link_person_identity`]): an explicit, reversible
//! navigation layer over the archive. Links anchor to a source
//! instance plus a stable author key or a source-scoped author name —
//! never a display name matched across sources — and no linking,
//! unlinking, renaming, or deleting ever rewrites an archived author,
//! message, or conversation (see the identity contract in
//! docs/data-and-api-contracts.md).

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Days, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, Transaction};
use serde::{Deserialize, Serialize};

use crate::message::{InsertOutcome, Message, NewMessage, SourceCount, Stats};
use crate::query::{
    ChatContext, SearchHit, SearchPage, SearchQuery, SNIPPET_CLOSE_MARK, SNIPPET_OPEN_MARK,
};
use crate::safe_migration::{self, MigrationFailPoint, OpenOptions};
use crate::search2::SearchArchiveRequest;
use crate::stats::{ArchiveStats, AuthorCount, ChatCount, DayCount, SourceNewCount};
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
    (
        14,
        // Account-level sync error state (cold review §2.2/#5: the
        // first-sync failure loop). Additive: `source_instances` gains
        // the record of the last failed sync — a stable failure class
        // token (see `imap::SyncFailure::as_str`), the user-facing
        // message, and when it happened — so the accounts panel can show
        // *what fell and why* after a restart, with the entered
        // configuration intact. `record_source_success` clears all three;
        // the row's `state` stays within its existing CHECK domain
        // ('disconnected' while an error is recorded, 'connected' after
        // a success), so no CHECK rebuild is needed.
        r#"
        ALTER TABLE source_instances ADD COLUMN last_error_class TEXT;
        ALTER TABLE source_instances ADD COLUMN last_error TEXT;
        ALTER TABLE source_instances ADD COLUMN last_error_at TEXT;
        "#,
    ),
    (
        15,
        // Covering index for the per-source mail-vs-chat taxonomy (the
        // source icons). `archive_messages.channel` records what each
        // parser actually stored (`"chat"` for a chat-export import,
        // `"email"` for a mail-file import; IMAP rows carry their folder
        // name as the channel, so an IMAP source's medium is decided by
        // its kind), so channel presence per source is the honest
        // signal — but probing it through
        // `idx_archive_messages_source` would walk every row of a
        // million-message import. This index turns each
        // `EXISTS(… WHERE source_instance_id = ? AND channel = ?)` into
        // a pure seek (see [`Storage::account_channel_flags`]); it
        // shares its leading column with `idx_archive_messages_source`,
        // so it also serves any future per-source channel grouping.
        r#"
        CREATE INDEX idx_archive_messages_source_channel
            ON archive_messages (source_instance_id, channel);
        "#,
    ),
    (
        16,
        // The finished-job ledger (critique 2, P1: "Activity forgets
        // finished jobs after restart"). The M2.1 job registry is
        // deliberately in-memory (job ids, seq numbers, and cancel flags
        // are process-local), so a restart left Activity empty over a
        // vault that had just imported thousands of messages. This table
        // is the modest durable half: every terminal job transition
        // (succeeded/failed/cancelled) appends one row — kind, times,
        // state, a one-line result summary, and the serialized terminal
        // state so a restored row renders through the same describeJobState
        // path as a live one. Rows are append-only history, never queried
        // by id and never resurrected as runnable jobs; a write prunes to
        // the newest [`JOB_LEDGER_KEEP`] rows, so the ledger stays a
        // bounded "recent activity" tail, not a second registry.
        // `job_id` is the finished run's process-local id, kept for
        // provenance only — restored rows surface under synthetic ids
        // (see jobs.rs) precisely because these ids are not stable.
        r#"
        CREATE TABLE job_ledger (
            id           INTEGER PRIMARY KEY,
            job_id       INTEGER NOT NULL,
            kind         TEXT    NOT NULL,
            submitted_at TEXT    NOT NULL,
            finished_at  TEXT    NOT NULL,
            state        TEXT    NOT NULL CHECK (state IN ('succeeded', 'failed', 'cancelled')),
            summary      TEXT    NOT NULL DEFAULT '',
            payload      TEXT    NOT NULL DEFAULT ''
        );
        "#,
    ),
    (
        17,
        // Drop `idx_archive_messages_source` (created by migration 4):
        // since migration 15 the composite
        // `idx_archive_messages_source_channel
        // (source_instance_id, channel)` covers its leading column, so
        // the single-column index serves no query the composite cannot,
        // and only costs write amplification on every insert. No query
        // references it by name; the drop is its own append-only
        // migration rather than an edit of migration 4.
        r#"
        DROP INDEX IF EXISTS idx_archive_messages_source;
        "#,
    ),
    (
        18,
        // Saved searches (the Archive workspace's recurring searches).
        // One row per named definition: `request_json` holds the serde
        // JSON of a v2 `SearchArchiveRequest` — the *dynamic* query and
        // its complete filter semantics (canonical source ids, author,
        // explicit day bounds plus timezone, attachment/honesty filters,
        // import scope, sort), with `limit`/`offset` stripped before
        // persisting because pagination is per-execution, not part of
        // the definition. Source references stay canonical source ids
        // inside the stored request, so display-label renames or
        // duplicate labels never redirect a saved search. Rename and
        // delete are single-row operations on this table and never
        // touch archived messages.
        r#"
        CREATE TABLE saved_searches (
            id           TEXT PRIMARY KEY,
            name         TEXT NOT NULL UNIQUE,
            request_json TEXT NOT NULL,
            created_at   TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
        );
        "#,
    ),
    (
        19,
        // Vault-local collections (the organization layer, part 1 of
        // the collections slice): user-named sets referencing canonical
        // messages across source instances. Additive, metadata-only —
        // nothing here ever touches archived content, provenance, or
        // identity. `collection_members.message_id` deliberately
        // carries **no** foreign key: the membership is a soft
        // reference to the stable archive rowid, so a canonical row
        // that is absent (a vault restored from an older snapshot, a
        // row lost to corruption) surfaces in the collection detail as
        // an *unavailable* row instead of the membership vanishing
        // (honest-reference semantics, decided in the slice spec).
        // Deleting a collection cascades its memberships only.
        r#"
        CREATE TABLE collections (
            id         TEXT PRIMARY KEY,
            name       TEXT    NOT NULL,
            created_at TEXT    NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT    NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE collection_members (
            collection_id TEXT    NOT NULL REFERENCES collections (id)
                ON DELETE CASCADE,
            message_id    INTEGER NOT NULL,
            added_at      TEXT    NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (collection_id, message_id)
        );
        CREATE INDEX idx_collection_members_message ON collection_members (message_id);
        "#,
    ),
    (
        20,
        // People and user-confirmed identity links (the unified person
        // view, storage slice). Additive DDL only: no archived row is
        // read or changed. Numbering: 18 (saved searches) and 19
        // (collections) are in the ledger above; the what's-new index
        // follows as 21.
        //
        // The anchoring model (see the identity contract in
        // docs/data-and-api-contracts.md): every link anchors to a
        // canonical `source_instance_id` plus one of
        // - `stable_key`: a stable, source-scoped author key (an email
        //   address, a provider user id) where the source provides one,
        //   case-folded at link time;
        // - `author_name`: the verbatim author string where the source
        //   provides nothing stable — inherently ambiguous (two humans
        //   can share a display name), therefore scoped to its source
        //   instance exactly to bound that ambiguity.
        // The UNIQUE constraint is the "one identity, one person" rule:
        // an anchor belongs to at most one person, and reassigning it
        // is always an explicit unlink-then-link — the storage API
        // refuses conflicting assignments with an explanation instead
        // of silently moving the link. Deleting a person cascades its
        // links and nothing else; messages, authors, and conversations
        // are never rewritten, merged, or re-attributed.
        r#"
        CREATE TABLE people (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE person_identity_links (
            id                 TEXT PRIMARY KEY,
            person_id          TEXT NOT NULL REFERENCES people (id) ON DELETE CASCADE,
            source_instance_id TEXT NOT NULL,
            -- 'stable_key' (stable author key, case-folded) or
            -- 'author_name' (verbatim display name, ambiguous by
            -- nature and source-scoped).
            anchor_kind        TEXT NOT NULL CHECK (anchor_kind IN ('stable_key', 'author_name')),
            anchor_value       TEXT NOT NULL,
            -- Rendered label for the link UI; display only, never part
            -- of the anchor.
            display_label      TEXT NOT NULL DEFAULT '',
            linked_at          TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE (source_instance_id, anchor_kind, anchor_value)
        );
        CREATE INDEX idx_person_identity_links_person
            ON person_identity_links (person_id);
        "#,
    ),
    (
        21,
        // The what's-new boot digest's message half (M7 follow-up;
        // renumbered 20 -> 21 for the merge: the people slice was
        // first in the queue and keeps 20)
        // counts newly archived messages per source since the last
        // visit (`Storage::messages_archived_since`). Without an index
        // whose leading column is `created_at`, that filter's query
        // plan is a full table scan — on a million-message vault the
        // scan would blow the boot budget every time the window shows.
        // The composite carries `source_instance_id` second so the
        // count query is covered by the index alone (the per-row table
        // lookup is what the bare-column index would still pay).
        r#"
        CREATE INDEX idx_archive_messages_created_source
            ON archive_messages (created_at, source_instance_id);
        "#,
    ),
    (
        22,
        // The attachment projection (A6): one row per attachment
        // reference a message records in its `raw_json` `media` array,
        // plus the registry side (`media_objects`) at read time. Linked
        // attachments previously lived only inside each message's raw
        // JSON; this index spans them without mutating any raw record
        // (the backfill in `crate::files` is the single writer). Like
        // `collection_members`, `message_id` deliberately carries no
        // foreign key: the projection is derived from raw rows, and a
        // dangling row (a vault restored from an older snapshot)
        // surfaces as an unusable reference instead of silently
        // vanishing — re-running the backfill repairs it. `UNIQUE
        // (message_id, ref_index)` makes upserts idempotent, so the
        // resumable backfill can stop and restart on any boundary.
        r#"
        CREATE TABLE attachment_refs (
            id                 INTEGER PRIMARY KEY,
            message_id         INTEGER NOT NULL,
            -- Index of the entry inside the message's media array.
            ref_index          INTEGER NOT NULL,
            source_instance_id TEXT NOT NULL DEFAULT '',
            -- Bounded import run that first observed the message (its
            -- earliest occurrence's import_id; '' when unrecorded).
            import_id          TEXT NOT NULL DEFAULT '',
            name               TEXT NOT NULL DEFAULT '',
            kind               TEXT NOT NULL DEFAULT '',
            size               INTEGER NOT NULL DEFAULT 0,
            -- NULL when the reference's bytes were never preserved (the
            -- raw entry carries `missing` plus a reason instead).
            sha256             TEXT,
            -- 'preserved' = preserved-by-reference (a digest is
            -- recorded; verifying the bytes on disk stays with the
            -- existing reconcile surface). 'missing' = the raw entry
            -- records an unpreserved reference. The CHECK keeps the
            -- two representations exactly in step: a digest exists if
            -- and only if the reference is preserved.
            availability       TEXT NOT NULL
                CHECK (availability IN ('preserved', 'missing')),
            reason             TEXT NOT NULL DEFAULT '',
            UNIQUE (message_id, ref_index),
            CHECK ((sha256 IS NULL) = (availability = 'missing'))
        );
        CREATE INDEX idx_attachment_refs_sha256 ON attachment_refs (sha256);
        CREATE INDEX idx_attachment_refs_message ON attachment_refs (message_id);
        "#,
    ),
    (
        23,
        // The person link history ledger (A5a): one row per link or
        // unlink a `person_links_commit` actually applied, tagged with
        // the organization revision the commit produced. Minimal
        // undo/audit record of confirmed association changes — no
        // message payload is ever rewritten. `person_id` deliberately
        // carries no foreign key: deleting a person removes
        // organization only, and the audit ledger must neither block
        // that deletion nor vanish with it.
        r#"
        CREATE TABLE person_link_history (
            id                 INTEGER PRIMARY KEY,
            person_id          TEXT NOT NULL,
            source_instance_id TEXT NOT NULL,
            -- 'stable_key' / 'author_name', exactly the anchor kinds of
            -- person_identity_links.
            anchor_kind        TEXT NOT NULL
                CHECK (anchor_kind IN ('stable_key', 'author_name')),
            anchor_value       TEXT NOT NULL,
            -- 'link' or 'unlink' — what the commit applied.
            operation          TEXT NOT NULL CHECK (operation IN ('link', 'unlink')),
            -- Organization revision the commit recorded the change at
            -- (the counter lives in archive_meta, key
            -- 'organization_revision').
            organization_revision INTEGER NOT NULL,
            recorded_at        TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX idx_person_link_history_person
            ON person_link_history (person_id);
        "#,
    ),
    (
        24,
        // Durable source receipts (the archive-experience plan's §6
        // `source_receipts` row): one appended row per *completed*
        // import or sync, so long-lived source health rests on
        // committed facts rather than the bounded `job_ledger` tail
        // (migration 16 keeps only the newest terminal jobs). The row
        // records what the run observed and what it did — counts and
        // file outcomes, an observed date range as *separate nullable
        // bounds* (an observed range is evidence of what the run saw,
        // never a claim of complete provider coverage between the
        // endpoints), and a `facts_json` scope object. No message
        // bodies. Rows are append-only facts; nothing prunes or
        // resurrects them. The added `import_checkpoints` columns make
        // the completing batch's new/present counts run totals across
        // an interrupted-and-resumed import: every intermediate batch
        // accumulates into the checkpoint row, and the receipt written
        // by the completing transaction reads them there before the
        // checkpoint is deleted (a single-batch import has no
        // checkpoint row and contributes its counts directly).
        r#"
        CREATE TABLE source_receipts (
            id                 INTEGER PRIMARY KEY,
            source_instance_id TEXT    NOT NULL,
            -- 'import' (a completed bounded import) or 'sync' (a
            -- completed account sync).
            kind               TEXT    NOT NULL CHECK (kind IN ('import', 'sync')),
            completed_at       TEXT    NOT NULL DEFAULT (datetime('now')),
            -- Observed sent-time bounds of the messages this run
            -- actually saw; NULL when the run observed none. Evidence,
            -- not a completeness claim.
            observed_min       TEXT,
            observed_max       TEXT,
            new_records        INTEGER NOT NULL DEFAULT 0,
            present_records    INTEGER NOT NULL DEFAULT 0,
            failed_records     INTEGER NOT NULL DEFAULT 0,
            files_preserved    INTEGER NOT NULL DEFAULT 0,
            files_missing      INTEGER NOT NULL DEFAULT 0,
            -- Scope facts (import id, run scope, …); never message
            -- content.
            facts_json         TEXT    NOT NULL DEFAULT '{}'
        );
        CREATE INDEX idx_source_receipts_source_completed
            ON source_receipts (source_instance_id, completed_at);

        ALTER TABLE import_checkpoints ADD COLUMN inserted_total
            INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE import_checkpoints ADD COLUMN skipped_total
            INTEGER NOT NULL DEFAULT 0;
        "#,
    ),
    (
        25,
        // The manual-export reminder preferences (A9, plan §5.4): one
        // row per source the user opted in — device-local scheduling
        // facts only: the interval, the single next-due deadline (RFC
        // 3339 UTC), and the snooze state. No row means no reminders
        // (the opt-in default). Content-free by construction: nothing
        // but these scheduling facts is ever stored. Dismissal is a
        // transition, not a column — it advances `next_due_at` and
        // clears `snoozed_until`. And nothing here promises OS
        // background execution after Quit: a deadline that passes while
        // the app is closed is simply due at the next launch — one
        // reminder, never a burst, because a single instant cannot
        // queue missed ones.
        r#"
        CREATE TABLE reminder_preferences (
            source_instance_id TEXT    PRIMARY KEY,
            enabled            INTEGER NOT NULL DEFAULT 0,
            -- Period between reminders, milliseconds (default monthly;
            -- kept inside the reminder core's bounds by its setters).
            interval_ms        INTEGER NOT NULL DEFAULT 2592000000,
            -- RFC 3339 UTC instant the next reminder may be presented
            -- at; NULL only on a row that never armed.
            next_due_at        TEXT,
            -- While set and still in the future: the user snoozed the
            -- due reminder; present again at/after this instant without
            -- moving `next_due_at`.
            snoozed_until      TEXT,
            updated_at         TEXT    NOT NULL DEFAULT (datetime('now'))
        );
        "#,
    ),
    (
        26,
        r#"
        CREATE TABLE original_objects (
            digest       TEXT PRIMARY KEY,
            size         INTEGER NOT NULL,
            retained_at  TEXT    NOT NULL DEFAULT (datetime('now')),
            kind         TEXT    NOT NULL DEFAULT 'rfc5322'
        );

        CREATE TABLE original_references (
            message_id INTEGER PRIMARY KEY,
            digest     TEXT NOT NULL
        );
        CREATE INDEX idx_original_references_digest ON original_references (digest);
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
    /// The honest at-rest fact established at open: whether the
    /// underlying file is a SQLCipher-encrypted database. Surfaced
    /// through [`Storage::is_encrypted`] for the health/inspect read
    /// models.
    encrypted: bool,
    /// The key this open used, present only when the vault is encrypted
    /// (a plaintext vault keeps `None` even when a key was offered — it
    /// was not used). Held so connection-creating flows that share this
    /// open's identity (the backup snapshot) inherit encryption instead
    /// of writing a plaintext side door.
    key: Option<crate::encryption::VaultKey>,
}

/// BM25F column weights every FTS ranking site orders by
/// ([`Storage::search_filtered`], [`Storage::search_page`],
/// [`Storage::search_export_stream`], and the v2 relevance page
/// [`Storage::search_archive`] in [`crate::search2`]).
///
/// Term evidence is field-dependent: a term hitting the subject line says
/// more about a message than the same term buried in a long body, and an
/// author hit sits in between (BM25F; Robertson & Zaragoza 2009, "The
/// Probabilistic Relevance Framework: BM25 and Beyond"). FTS5's `bm25()`
/// auxiliary function takes one weight per column in declared order —
/// `(body, author, subject)` here — and returns negative scores where
/// more negative is more relevant, so ordering by it ascending keeps the
/// exact direction of the old `ORDER BY rank`. The weights are formatted
/// into the SQL text as literals: they are compile-time constants, never
/// user input, and FTS5 auxiliary-function arguments cannot be bound
/// parameters. Tuned against `tests/retrieval_eval.rs`; body stays the
/// 1.0 anchor.
///
/// `pub(crate)`: shared with the v2 query module ([`crate::search2`]),
/// which ranks the IPC search path with the same weights.
pub(crate) const BM25_WEIGHT_BODY: f32 = 1.0;
pub(crate) const BM25_WEIGHT_AUTHOR: f32 = 1.0;
pub(crate) const BM25_WEIGHT_SUBJECT: f32 = 3.0;

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
        // The keyed open: classifies the file's first page, applies
        // `PRAGMA key` before anything else can touch the file, refuses
        // an encrypted vault without its key, and reports whether the
        // file is encrypted (see `crate::encryption`).
        let opened = crate::encryption::open_connection(
            path,
            rusqlite::OpenFlags::default(),
            opts.vault_key.as_ref(),
        )?;
        let conn = opened.conn;
        let key = if opened.encrypted {
            opts.vault_key
        } else {
            None
        };
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
        match Self::init_with(
            conn,
            opts,
            existed.then_some(path),
            snapshot.as_deref(),
            opened.encrypted,
            key,
        ) {
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
        Self::init_with(conn, &OpenOptions::default(), None, None, false, None)
    }

    /// Configure pragmas/functions, migrate, and validate — restoring
    /// from the pre-migration snapshot when anything fails after the
    /// snapshot was taken (M1.4 rollback path).
    fn init_with(
        conn: Connection,
        opts: &OpenOptions,
        db_path: Option<&Path>,
        snapshot: Option<&Path>,
        encrypted: bool,
        key: Option<crate::encryption::VaultKey>,
    ) -> Result<Self> {
        let converted = Self::configure_and_migrate(&conn, opts);
        match converted {
            Ok(()) => Ok(Self {
                conn,
                encrypted,
                key,
            }),
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
                InsertOutcome::Inserted(id) => {
                    counts.inserted += 1;
                    if !media_entries(&msg.raw_json).is_empty() {
                        counts.media_linked += 1;
                    }
                    if let Some(object) = &msg.original_ref {
                        record_original_in_tx(&tx, id, object)?;
                    }
                }
                InsertOutcome::Duplicate(id) => {
                    counts.skipped += 1;
                    let entries = media_entries(&msg.raw_json);
                    if !entries.is_empty() && attach_media_in_tx(&tx, id, &entries)? {
                        counts.media_linked += 1;
                    }
                    // A re-import that retained the bytes still links the
                    // original to the existing row: the reference says
                    // "these exact bytes are stored", never anything
                    // about which run first saw the message.
                    if let Some(object) = &msg.original_ref {
                        record_original_in_tx(&tx, id, object)?;
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
            // Run-total new/present counts (migration 22): intermediate
            // batches accumulated into the checkpoint row; a single-
            // batch import has none (zero baseline).
            let (mut new_records, mut present_records): (u64, u64) = (0, 0);
            if let Some((stored_new, stored_present)) = tx
                .query_row(
                    "SELECT inserted_total, skipped_total
                     FROM import_checkpoints WHERE import_id = ?1",
                    params![import_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?.max(0) as u64,
                            row.get::<_, i64>(1)?.max(0) as u64,
                        ))
                    },
                )
                .optional()?
            {
                new_records += stored_new;
                present_records += stored_present;
            }
            new_records += counts.inserted;
            present_records += counts.skipped;
            tx.execute(
                "DELETE FROM import_checkpoints WHERE import_id = ?1",
                params![import_id],
            )?;
            // The completing commit appends the import's receipt
            // (migration 22) in the same transaction — except the M4.2
            // registry commit (empty batch carrying only unlinked
            // objects, after the real final batch already completed
            // the import): it is a registry transaction of an already
            // completed run, not a second completed import.
            let registry_only =
                messages.is_empty() && issues.is_empty() && !media_objects.is_empty();
            if !registry_only {
                counts.receipt = Some(record_import_receipt_in_tx(
                    &tx,
                    import_id,
                    source,
                    messages,
                    new_records,
                    present_records,
                )?);
            }
        } else {
            tx.execute(
                "INSERT INTO import_checkpoints
                     (import_id, source, committed_items,
                      inserted_total, skipped_total, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
                 ON CONFLICT(import_id) DO UPDATE SET
                    committed_items = excluded.committed_items,
                    inserted_total  = import_checkpoints.inserted_total
                                     + excluded.inserted_total,
                    skipped_total   = import_checkpoints.skipped_total
                                     + excluded.skipped_total,
                    updated_at      = excluded.updated_at",
                params![
                    import_id,
                    source,
                    committed_after,
                    counts.inserted.min(i64::MAX as u64) as i64,
                    counts.skipped.min(i64::MAX as u64) as i64,
                ],
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
    // Finished-jobs ledger (migration 16)
    // ------------------------------------------------------------------

    /// Append one terminal job to the ledger and prune to the newest
    /// [`JOB_LEDGER_KEEP`] rows — both inside one transaction, so the
    /// ledger never grows past its bound on disk.
    pub fn record_job_ledger(&self, entry: &JobLedgerEntry) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO job_ledger (job_id, kind, submitted_at, finished_at, state, summary, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.job_id,
                entry.kind,
                entry.submitted_at,
                entry.finished_at,
                entry.state,
                entry.summary,
                entry.payload,
            ],
        )?;
        tx.execute(
            "DELETE FROM job_ledger WHERE id NOT IN \
             (SELECT id FROM job_ledger ORDER BY id DESC LIMIT ?1)",
            params![JOB_LEDGER_KEEP as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The newest finished-job ledger rows, newest first (at most
    /// `limit`). Boot recovery turns these into the read-only historical
    /// records the Activity panel lists under this session's live jobs.
    pub fn job_ledger_recent(&self, limit: u32) -> Result<Vec<JobLedgerRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT job_id, kind, submitted_at, finished_at, state, summary, payload
             FROM job_ledger
             ORDER BY id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(JobLedgerRow {
                job_id: row.get::<_, i64>(0)?.max(0) as u64,
                kind: row.get(1)?,
                submitted_at: row.get(2)?,
                finished_at: row.get(3)?,
                state: row.get(4)?,
                summary: row.get(5)?,
                payload: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Terminal ledger rows whose `finished_at` is strictly newer than
    /// `since` (an RFC 3339 UTC instant in the same form the ledger
    /// stamps carry, so the comparison is a plain string compare),
    /// newest first, at most `limit`. The what's-new boot digest's job
    /// half reads this and filters to its own kinds and outcomes (see
    /// [`crate::whats_new`]); the ledger itself stays bounded at
    /// [`JOB_LEDGER_KEEP`] rows, so this is never a large read.
    pub fn job_ledger_since(&self, since: &str, limit: u32) -> Result<Vec<JobLedgerRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT job_id, kind, submitted_at, finished_at, state, summary, payload
             FROM job_ledger
             WHERE finished_at > ?1
             ORDER BY id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since, limit as i64], |row| {
            Ok(JobLedgerRow {
                job_id: row.get::<_, i64>(0)?.max(0) as u64,
                kind: row.get(1)?,
                submitted_at: row.get(2)?,
                finished_at: row.get(3)?,
                state: row.get(4)?,
                summary: row.get(5)?,
                payload: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ------------------------------------------------------------------
    // Source receipts (migration 22)
    // ------------------------------------------------------------------

    /// Append one completed-run receipt (migration 22). The import and
    /// sync completion seams call this (directly or through
    /// [`insert_source_receipt_in_tx`]) so every completed run leaves a
    /// durable, restart-safe fact row; callers that complete runs
    /// outside those seams can record their own.
    pub fn record_source_receipt(&self, receipt: &SourceReceiptRecord<'_>) -> Result<()> {
        insert_source_receipt_in_tx(&self.conn, receipt)
    }

    /// One source instance's receipt rows, newest first (at most
    /// `limit`; 0 yields none). Receipts are append-only facts — a
    /// listing never prunes or reorders history.
    pub fn list_source_receipts(&self, source_id: &str, limit: u32) -> Result<Vec<SourceReceipt>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, source_instance_id, kind, completed_at,
                    observed_min, observed_max,
                    new_records, present_records, failed_records,
                    files_preserved, files_missing, facts_json
             FROM source_receipts
             WHERE source_instance_id = ?1
             ORDER BY completed_at DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![source_id, limit as i64], source_receipt_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
                    excluded, last_attempt_at, last_success_at, created_at,
                    last_error_class, last_error, last_error_at
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
    /// (`last_success_at`, `state = 'connected'`). A previously recorded
    /// failure is cleared — the error record is the state of the *last*
    /// finished attempt.
    ///
    /// The same transaction appends a `sync` receipt (migration 22) so
    /// the completed run leaves a durable fact. This seam does not
    /// thread the run's counters through, so the receipt's counts are
    /// honestly zero and its observed bounds `NULL`; a later slice can
    /// widen the facts without changing this row's shape. Unknown ids
    /// remain a no-op — no stamp, no receipt.
    pub fn record_source_success(&self, id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let updated = tx.execute(
            "UPDATE source_instances
              SET last_success_at = datetime('now'), state = 'connected',
                  last_error_class = NULL, last_error = NULL, last_error_at = NULL
             WHERE id = ?1",
            params![id],
        )?;
        if updated > 0 {
            insert_source_receipt_in_tx(
                &tx,
                &SourceReceiptRecord {
                    source_instance_id: id,
                    kind: SourceReceiptKind::Sync,
                    observed_min: None,
                    observed_max: None,
                    new_records: 0,
                    present_records: 0,
                    failed_records: 0,
                    files_preserved: 0,
                    files_missing: 0,
                    facts_json: "{}",
                },
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Record a failed sync on a source instance: `state =
    /// 'disconnected'`, the stable failure class token (see
    /// [`crate::imap::SyncFailure::as_str`]), the user-facing message, and
    /// the time — the account-level error state that survives restarts
    /// (the accounts panel shows what fell and why, with the entered
    /// configuration intact). A later success clears it (see
    /// [`Storage::record_source_success`]). Unknown ids are a no-op,
    /// exactly like attempt stamps: recording must never fail a sync.
    pub fn record_source_failure(&self, id: &str, class: &str, message: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE source_instances
             SET state = 'disconnected',
                 last_error_class = ?2, last_error = ?3,
                 last_error_at = datetime('now')
             WHERE id = ?1",
            params![id, class, message],
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
    ///
    /// `from_filter` keeps only messages whose author contains the value
    /// (case-insensitive for ASCII, as SQLite `LIKE` is); `require_link`
    /// keeps only bodies containing an `http…://` URL. Both are bound
    /// parameters applied before `LIMIT`. `%`/`_` inside `from_filter` pass
    /// through as LIKE wildcards.
    pub fn search_filtered(
        &self,
        match_query: &str,
        date_from: Option<&str>,
        date_to: Option<&str>,
        from_filter: Option<&str>,
        require_link: bool,
        limit: u32,
    ) -> Result<Vec<Message>> {
        self.search_filtered_scoped(
            match_query,
            date_from,
            date_to,
            from_filter,
            require_link,
            None,
            limit,
        )
    }

    /// [`Storage::search_filtered`] plus an optional validated Ask
    /// workspace scope ([`crate::search2::ResolvedAskScope`]).
    ///
    /// The scope can only narrow: its date bounds are intersected with
    /// the plan's (later lower bound, earlier upper bound — an empty
    /// intersection returns no rows, never an unscoped fallback), its
    /// author filter applies conjunctively with `from_filter`, and its
    /// source/import/attachment/honesty filters reuse the v2 search
    /// predicate SQL so plain search and Ask filter identically.
    #[allow(clippy::too_many_arguments)] // mirrors `search_filtered`'s parameter list + the scope
    pub fn search_filtered_scoped(
        &self,
        match_query: &str,
        date_from: Option<&str>,
        date_to: Option<&str>,
        from_filter: Option<&str>,
        require_link: bool,
        scope: Option<&crate::search2::ResolvedAskScope>,
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
        let Some((from, to_exclusive)) = intersected_bounds(from, to_exclusive, scope) else {
            return Ok(Vec::new());
        };
        // The scope predicates reference the same `m`/`si` aliases the
        // base query binds (plus `c` for the grouping filter, joined only
        // under a scope).
        let (extra_sql, extra_values) = scope_filter_sql(scope);
        let scope_join = if scope.is_some() {
            "LEFT JOIN conversations AS c ON c.id = m.conversation_id"
        } else {
            ""
        };
        let mut bind: Vec<rusqlite::types::Value> = vec![
            SqlValue::Text(match_query.to_string()),
            from.into(),
            to_exclusive.into(),
            from_filter.map(str::to_string).into(),
            (require_link as i64).into(),
        ];
        bind.extend(extra_values);
        bind.push((limit as i64).into());
        let mut stmt = self.conn.prepare(&format!(
            r#"
            SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id,
                   m.channel, m.chat_id, m.author,
                   COALESCE(m.sent_instant, ''), m.body, m.raw_json
            FROM archive_fts
            JOIN archive_messages AS m ON m.id = archive_fts.rowid
            JOIN source_instances AS si ON si.id = m.source_instance_id
            {scope_join}
            WHERE archive_fts MATCH ?1
              AND (?2 IS NULL OR m.sent_instant >= ?2)
              AND (?3 IS NULL OR m.sent_instant < ?3)
              AND ((?2 IS NULL AND ?3 IS NULL) OR m.sent_instant IS NOT NULL)
              AND (?4 IS NULL OR m.author LIKE '%' || ?4 || '%')
              AND (?5 = 0 OR m.body LIKE '%http%://%')
              {extra_sql}
            ORDER BY bm25(archive_fts, {BM25_WEIGHT_BODY}, {BM25_WEIGHT_AUTHOR}, {BM25_WEIGHT_SUBJECT}), m.id
            LIMIT ?
            "#
        ))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bind), message_from_row)?
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
    ///
    /// Pages start at `offset`, or — when the query carries a keyset
    /// cursor from a previous page's `next_cursor` — strictly after the
    /// cursor row's sort position (the cursor wins), so deep pages cost
    /// the same as the first page. The cursor follows the v2 search
    /// contract ([`crate::search2::SearchCursor`]): bound to the order
    /// mode it was issued under (browse for empty text, relevance
    /// otherwise), rejected with `InvalidQuery` when malformed or from
    /// another mode.
    pub fn search_page(&self, query: &SearchQuery) -> Result<SearchPage> {
        let mut query = query.clone();
        query.normalize()?;
        let filters = QueryFilters::build(&query)?;

        // Cursor validation mirrors search2: the wire form and the
        // order-mode binding are shared; v1 has no explicit sort, so
        // empty text is browse and any text query is relevance.
        let order_mode = if query.text.trim().is_empty() {
            crate::search2::CursorOrderMode::Browse
        } else {
            crate::search2::CursorOrderMode::Relevance
        };
        let cursor = match &query.cursor {
            None => None,
            Some(raw) => {
                let decoded = crate::search2::decode_search_cursor(raw)?;
                if decoded.mode() != order_mode {
                    return Err(Error::InvalidQuery(format!(
                        "cursor was issued for {} order, not {} order",
                        decoded.mode().as_str(),
                        order_mode.as_str()
                    )));
                }
                Some(decoded)
            }
        };
        // Keyset fragment (over alias `m`): rows strictly after the
        // cursor's sort position. Browse orders `sent_instant DESC,
        // id DESC` with unknown instants last (SQLite NULLs sort last
        // in DESC); relevance orders by the bm25 rank then id. Same
        // compound-predicate shapes as the v2 page.
        let (keyset_sql, keyset_bind): (String, Vec<SqlValue>) = match &cursor {
            None => (String::new(), Vec::new()),
            Some(crate::search2::SearchCursor::Browse { sent, id }) => match sent {
                Some(instant) => (
                    " AND (m.sent_instant IS NULL \
                       OR m.sent_instant < ? \
                       OR (m.sent_instant = ? AND m.id < ?))"
                        .to_string(),
                    vec![
                        SqlValue::from(instant.clone()),
                        SqlValue::from(instant.clone()),
                        SqlValue::Integer(*id),
                    ],
                ),
                None => (
                    " AND (m.sent_instant IS NULL AND m.id < ?)".to_string(),
                    vec![SqlValue::Integer(*id)],
                ),
            },
            Some(crate::search2::SearchCursor::Relevance { rank_bits, id }) => {
                let rank = f64::from_bits(*rank_bits);
                let expr = crate::search2::bm25_rank_expr();
                (
                    format!(" AND ({expr} > ? OR ({expr} = ? AND m.id > ?))"),
                    vec![
                        SqlValue::Real(rank),
                        SqlValue::Real(rank),
                        SqlValue::Integer(*id),
                    ],
                )
            }
            // A "newest"-mode cursor cannot be issued by the v1 page;
            // the mode check above already rejected it.
            Some(other @ crate::search2::SearchCursor::Newest { .. }) => {
                return Err(Error::InvalidQuery(format!(
                    "cursor was issued for {} order, not {} order",
                    other.mode().as_str(),
                    order_mode.as_str()
                )));
            }
        };
        // One row past the page detects a following page (the probe row
        // never reaches the returned page).
        let fetch = query.limit as i64 + 1;
        let offset_paging = cursor.is_none();
        let has_more;
        let mut last_rank: Option<f64> = None;

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
            bind.extend(keyset_bind);
            bind.push(SqlValue::Integer(fetch));
            if offset_paging {
                bind.push(SqlValue::Integer(query.offset as i64));
            }
            let sql = format!(
                "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
                 m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), \
                 m.body, m.raw_json, NULL \
                 FROM archive_messages AS m \
                 JOIN source_instances AS si ON si.id = m.source_instance_id \
                 WHERE 1=1 {} {} \
                 ORDER BY m.sent_instant DESC, m.id DESC {}",
                filters.sql,
                keyset_sql,
                if offset_paging {
                    "LIMIT ? OFFSET ?"
                } else {
                    "LIMIT ?"
                }
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows: Vec<SearchHit> = stmt
                .query_map(params_from_iter(bind), hit_from_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            has_more = rows.len() > query.limit as usize;
            rows.truncate(query.limit as usize);
            rows
        } else {
            // Order follows the ? placeholders in the SQL text: the two
            // snippet() markers in the SELECT, then the MATCH expression,
            // then the filter bindings, the keyset values, then the page
            // tail. The trailing column is the row's bm25 rank — the
            // sort position `next_cursor` must carry.
            let mut bind = vec![
                SqlValue::from(SNIPPET_OPEN_MARK.to_string()),
                SqlValue::from(SNIPPET_CLOSE_MARK.to_string()),
                SqlValue::from(crate::search::build_match_query(&query.text)),
            ];
            bind.extend(filters.bind());
            bind.extend(keyset_bind);
            bind.push(SqlValue::Integer(fetch));
            if offset_paging {
                bind.push(SqlValue::Integer(query.offset as i64));
            }
            let sql = format!(
                "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
                 m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), \
                 m.body, m.raw_json, \
                 snippet(archive_fts, 0, ?1, ?2, '…', 16), \
                 {} \
                 FROM archive_fts \
                 JOIN archive_messages AS m ON m.id = archive_fts.rowid \
                 JOIN source_instances AS si ON si.id = m.source_instance_id \
                 WHERE archive_fts MATCH ?3 {} {} \
                 ORDER BY bm25(archive_fts, {BM25_WEIGHT_BODY}, {BM25_WEIGHT_AUTHOR}, \
                               {BM25_WEIGHT_SUBJECT}), m.id {}",
                crate::search2::bm25_rank_expr(),
                filters.sql,
                keyset_sql,
                if offset_paging {
                    "LIMIT ? OFFSET ?"
                } else {
                    "LIMIT ?"
                }
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows: Vec<(SearchHit, f64)> = stmt
                .query_map(params_from_iter(bind), |row| {
                    let hit = hit_from_row(row)?;
                    let rank: f64 = row.get(10)?;
                    Ok((hit, rank))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            has_more = rows.len() > query.limit as usize;
            if has_more {
                last_rank = Some(rows[query.limit as usize - 1].1);
            }
            rows.truncate(query.limit as usize);
            rows.into_iter().map(|(hit, _)| hit).collect()
        };

        // next_cursor continues the list at the cursor row's position
        // whenever the probe row said more rows follow. The v1 message
        // keeps unknown instants as "" — the cursor keeps the explicit
        // None flag so the NULL boundary stays exact.
        let next_cursor = if has_more {
            let last = hits
                .last()
                .expect("a following page implies a full page was read");
            let cursor = match order_mode {
                crate::search2::CursorOrderMode::Browse => crate::search2::SearchCursor::Browse {
                    sent: if last.message.sent_at.is_empty() {
                        None
                    } else {
                        Some(last.message.sent_at.clone())
                    },
                    id: last.message.id,
                },
                crate::search2::CursorOrderMode::Relevance => {
                    crate::search2::SearchCursor::Relevance {
                        rank_bits: last_rank
                            .expect("a following relevance page carries its rank")
                            .to_bits(),
                        id: last.message.id,
                    }
                }
                crate::search2::CursorOrderMode::Newest => {
                    return Err(Error::InvalidQuery(
                        "v1 search has no text-newest order mode".into(),
                    ));
                }
            };
            Some(crate::search2::encode_search_cursor(&cursor)?)
        } else {
            None
        };

        Ok(SearchPage {
            hits,
            total,
            limit: query.limit,
            // A cursor page has no meaningful offset; the continuation
            // is the cursor itself.
            offset: if offset_paging { query.offset } else { 0 },
            next_cursor,
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
                    "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), \
                     m.external_id, \
                      m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), \
                      m.body, m.raw_json, {SOURCE_KEY_COLUMN} \
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
                    "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), \
                     m.external_id, \
                      m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), \
                      m.body, m.raw_json, {SOURCE_KEY_COLUMN} \
                      FROM archive_fts \
                      JOIN archive_messages AS m ON m.id = archive_fts.rowid \
                      JOIN source_instances AS si ON si.id = m.source_instance_id \
                      WHERE archive_fts MATCH ?1 {} \
                      ORDER BY bm25(archive_fts, {BM25_WEIGHT_BODY}, {BM25_WEIGHT_AUTHOR}, \
                                    {BM25_WEIGHT_SUBJECT}), m.id",
                    filters.sql
                ),
                bind,
            )
        };

        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(bind))?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            sink(&export_message_from_row(row)?)?;
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

    /// Stream every message of the conversation containing message `id`,
    /// oldest → newest, through `sink` one row at a time (used by
    /// export, which must cover the whole thread without collecting it
    /// into memory). The anchor's canonical conversation — the same
    /// identity [`Storage::chat_context`] walks — defines the set:
    /// provider-key conversations and email threads group there exactly
    /// like legacy `(source, chat_id)` groups, so renames and chats
    /// sharing a display name never change what streams. A message with
    /// no conversation (standalone mail, ungrouped legacy rows) stands
    /// alone and streams as its own single row. Ties and unknown
    /// instants order by id, matching the legacy ordering exactly
    /// (COALESCE maps the unknown NULL instant to the legacy
    /// empty-string position).
    ///
    /// Every row carries the portable source identity
    /// ([`SOURCE_KEY_COLUMN`]), exactly like the search export streams,
    /// so a JSON Lines archive transfers between vaults without
    /// collapsing same-labelled sources.
    pub fn conversation_thread_stream(
        &self,
        id: i64,
        sink: &mut dyn FnMut(&Message) -> Result<()>,
    ) -> Result<()> {
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
            return match self.get_for_export(id)? {
                Some(m) => sink(&m),
                None => Ok(()),
            };
        }

        let mut stmt = self.conn.prepare(&format!(
            "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
             m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), m.body, m.raw_json, \
             {SOURCE_KEY_COLUMN} \
             FROM archive_messages AS m \
             JOIN source_instances AS si ON si.id = m.source_instance_id \
             WHERE m.conversation_id = ( \
                 SELECT conversation_id FROM archive_messages WHERE id = ?1) \
             ORDER BY COALESCE(m.sent_instant, '') ASC, m.id ASC",
        ))?;
        let mut rows = stmt.query(params![id])?;
        while let Some(row) = rows.next()? {
            sink(&export_message_from_row(row)?)?;
        }
        Ok(())
    }

    /// One message by row id on the export shape: the nine
    /// [`message_from_row`] columns plus [`SOURCE_KEY_COLUMN`], so the
    /// whole-conversation export stream's standalone rows carry the same
    /// portable source identity as its thread rows. Private — the
    /// public [`Storage::get`] keeps its 9-column contract.
    fn get_for_export(&self, id: i64) -> Result<Option<Message>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT m.id, COALESCE(si.legacy_namespace, si.label, si.id), m.external_id, \
             m.channel, m.chat_id, m.author, COALESCE(m.sent_instant, ''), m.body, m.raw_json, \
             {SOURCE_KEY_COLUMN} \
             FROM archive_messages AS m \
             JOIN source_instances AS si ON si.id = m.source_instance_id \
             WHERE m.id = ?1",
        ))?;
        stmt.query_row(params![id], export_message_from_row)
            .optional()
            .map_err(Into::into)
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

    /// Newly archived messages per source instance since an instant
    /// (the what's-new boot digest's message half). `since` is an
    /// instant in the `archive_messages.created_at` column's own format
    /// (`YYYY-MM-DD HH:MM:SS` UTC — [`crate::whats_new::sqlite_instant`]
    /// converts the RFC 3339 record form); rows strictly newer count, so
    /// a message that re-imports and dedups keeps its original
    /// `created_at` and never counts twice — the numbers mean newly
    /// archived messages, not re-sightings. Grouped by source instance
    /// and joined to the same label the stats chips show, heaviest
    /// first. Rides `idx_archive_messages_created_source` (migration
    /// 21 on the merged ledger; originally numbered 20): the range
    /// above `since` is a covered index seek, never a scan, so the
    /// digest answers within the boot budget on million-message
    /// archives. The seek is pinned with INDEXED BY: without it the
    /// planner may prefer migration 15's source/channel index for the
    /// GROUP BY order and fall back to a full index scan — exactly
    /// what the boot budget cannot afford (caught by the EXPLAIN plan
    /// test in tests/whats_new.rs, first executed on the tier B host).
    pub fn messages_archived_since(&self, since: &str) -> Result<Vec<SourceNewCount>> {
        let mut stmt = self.conn.prepare(
            "SELECT si.id, COALESCE(si.legacy_namespace, si.label, si.id), COUNT(*)
             FROM archive_messages AS m INDEXED BY idx_archive_messages_created_source
             JOIN source_instances AS si ON si.id = m.source_instance_id
             WHERE m.created_at > ?1
             GROUP BY m.source_instance_id
             ORDER BY COUNT(*) DESC, si.id",
        )?;
        let counts = stmt
            .query_map([since], |row| {
                Ok(SourceNewCount {
                    source_id: row.get(0)?,
                    label: row.get(1)?,
                    count: row.get::<_, i64>(2)?.max(0) as u64,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(counts)
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

    /// Per-source channel presence for the accounts overview (the
    /// mail-vs-chat taxonomy behind the source icons): whether each
    /// source's archived messages include any `"chat"`-channel row (a
    /// chat-export import) and any `"email"`-channel row (a mail-file
    /// import; IMAP rows carry their folder name as the channel, so an
    /// IMAP source's medium is decided by its kind). Two seeks per
    /// source over the covering
    /// `idx_archive_messages_source_channel` (migration 15) —
    /// O(number of accounts), never a scan of the archive, matching
    /// [`Storage::account_counts`]'s budget.
    pub fn account_channel_flags(&self) -> Result<Vec<AccountChannels>> {
        let mut stmt = self.conn.prepare(
            "SELECT si.id,
                    EXISTS(SELECT 1 FROM archive_messages AS am
                           WHERE am.source_instance_id = si.id AND am.channel = 'chat'),
                    EXISTS(SELECT 1 FROM archive_messages AS am
                           WHERE am.source_instance_id = si.id AND am.channel = 'email')
             FROM source_instances AS si ORDER BY si.id",
        )?;
        let flags = stmt
            .query_map([], |row| {
                Ok(AccountChannels {
                    source_id: row.get(0)?,
                    has_chat: row.get(1)?,
                    has_email: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(flags)
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
        let instance = resolve_legacy_instance(&tx, source, "")?;
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

    /// The manual-export reminder preference for one source (A9, plan
    /// §5.4): whether the device-local, content-free nudge is on, its
    /// interval, and the single deadline plus snooze state. `None`
    /// until the user opts that source in — absence is the opt-out
    /// default, exactly like a fresh vault schedules nothing.
    pub fn reminder_preference(
        &self,
        source_instance_id: &str,
    ) -> Result<Option<crate::reminders::ReminderPreference>> {
        let row = self
            .conn
            .query_row(
                "SELECT enabled, interval_ms, next_due_at, snoozed_until
                 FROM reminder_preferences WHERE source_instance_id = ?1",
                params![source_instance_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? != 0,
                        row.get::<_, u64>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let (enabled, interval_ms, next_due_at, snoozed_until) = match row {
            Some(r) => r,
            None => return Ok(None),
        };
        Ok(Some(crate::reminders::ReminderPreference {
            source_instance_id: source_instance_id.to_string(),
            enabled,
            interval_ms,
            next_due_at: reminder_instant(next_due_at)?,
            snoozed_until: reminder_instant(snoozed_until)?,
        }))
    }

    /// Every reminder preference (A9): what a launch evaluates to find
    /// due reminders. Ordering is stable (by source) so callers can
    /// present them deterministically.
    pub fn reminder_preferences(&self) -> Result<Vec<crate::reminders::ReminderPreference>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_instance_id, enabled, interval_ms, next_due_at, snoozed_until
             FROM reminder_preferences ORDER BY source_instance_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(crate::reminders::ReminderPreference {
                    source_instance_id: row.get(0)?,
                    enabled: row.get::<_, i64>(1)? != 0,
                    interval_ms: row.get(2)?,
                    next_due_at: reminder_instant(row.get::<_, Option<String>>(3)?)?,
                    snoozed_until: reminder_instant(row.get::<_, Option<String>>(4)?)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Upsert one source's reminder preference (A9). Stores what it is
    /// given — the reminder core (the single writer) clamps intervals
    /// to its configured bounds before calling this. `next_due_at` /
    /// `snoozed_until` are persisted as RFC 3339 UTC so a deadline that
    /// passes while the app is closed survives restart and is simply
    /// due at the next launch.
    pub fn set_reminder_preference(
        &self,
        pref: crate::reminders::ReminderPreference,
    ) -> Result<crate::reminders::ReminderPreference> {
        let fmt = |t: Option<DateTime<Utc>>| {
            t.map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        };
        self.conn.execute(
            "INSERT INTO reminder_preferences
                 (source_instance_id, enabled, interval_ms, next_due_at, snoozed_until, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))
             ON CONFLICT(source_instance_id) DO UPDATE SET
                 enabled = excluded.enabled,
                 interval_ms = excluded.interval_ms,
                 next_due_at = excluded.next_due_at,
                 snoozed_until = excluded.snoozed_until,
                 updated_at = excluded.updated_at",
            params![
                pref.source_instance_id,
                pref.enabled as i64,
                pref.interval_ms as i64,
                fmt(pref.next_due_at),
                fmt(pref.snoozed_until)
            ],
        )?;
        Ok(pref)
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
    /// (`None` = unbounded) and the same `from_filter` / `require_link`
    /// filters, applied in SQL before the scan is ranked and truncated.
    /// Returns the best `limit` hits, score-descending
    /// (ties by ascending id). Stored vectors are normalized at write time,
    /// so cosine is a plain dot product. Rows whose blob length disagrees
    /// with `dim` (a foreign model's leftovers) are skipped, not fatal.
    /// This is the manual-cosine index described in [`crate::vector`].
    #[allow(clippy::too_many_arguments)]
    pub fn vector_scan(
        &self,
        model: &str,
        dim: u16,
        query: &[f32],
        date_from: Option<&str>,
        date_to: Option<&str>,
        from_filter: Option<&str>,
        require_link: bool,
        limit: u32,
    ) -> Result<Vec<VectorHit>> {
        self.vector_scan_scoped(
            model,
            dim,
            query,
            date_from,
            date_to,
            from_filter,
            require_link,
            None,
            limit,
        )
    }

    /// [`Storage::vector_scan`] plus an optional validated Ask workspace
    /// scope, applying exactly the filters the FTS leg applies (including
    /// the date-bound intersection with the plan — see
    /// [`Storage::search_filtered_scoped`]), so both hybrid legs always
    /// filter identically.
    #[allow(clippy::too_many_arguments)]
    pub fn vector_scan_scoped(
        &self,
        model: &str,
        dim: u16,
        query: &[f32],
        date_from: Option<&str>,
        date_to: Option<&str>,
        from_filter: Option<&str>,
        require_link: bool,
        scope: Option<&crate::search2::ResolvedAskScope>,
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
        let Some((from, to_exclusive)) = intersected_bounds(from, to_exclusive, scope) else {
            return Ok(Vec::new());
        };

        // Same bound-parameter shape as search_filtered: the date bounds
        // and sender/link filters are values, never spliced into the SQL
        // text. Without any filter the scan skips the messages join
        // entirely (the embedding table alone answers it); with one the
        // join applies the same filters as the FTS leg — a scope adds the
        // same v2 predicates (over `m`/`si`, plus the conversations join
        // the grouping filter needs).
        let has_filters = from.is_some()
            || to_exclusive.is_some()
            || from_filter.is_some()
            || require_link
            || scope.is_some();
        let (sql, bind): (String, Vec<rusqlite::types::Value>) = if has_filters {
            let (extra_sql, mut bind) = scope_filter_sql(scope);
            bind.insert(0, SqlValue::Text(model.to_string()));
            bind.insert(1, from.into());
            bind.insert(2, to_exclusive.into());
            bind.insert(3, from_filter.map(str::to_string).into());
            bind.insert(4, (require_link as i64).into());
            let scope_joins = if scope.is_some() {
                "JOIN source_instances AS si ON si.id = m.source_instance_id \
                 LEFT JOIN conversations AS c ON c.id = m.conversation_id"
            } else {
                ""
            };
            (
                format!(
                    "SELECT e.message_id, e.embedding
                 FROM message_embeddings AS e
                 JOIN archive_messages AS m ON m.id = e.message_id
                 {scope_joins}
                 WHERE e.model = ?1
                   AND (?2 IS NULL OR m.sent_instant >= ?2)
                   AND (?3 IS NULL OR m.sent_instant < ?3)
                   AND ((?2 IS NULL AND ?3 IS NULL) OR m.sent_instant IS NOT NULL)
                   AND (?4 IS NULL OR m.author LIKE '%' || ?4 || '%')
                   AND (?5 = 0 OR m.body LIKE '%http%://%')
                   {extra_sql}"
                ),
                bind,
            )
        } else {
            (
                "SELECT message_id, embedding FROM message_embeddings WHERE model = ?1".into(),
                vec![SqlValue::Text(model.to_string())],
            )
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(bind))?;
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

    /// Read one `archive_meta` key's value (M3.2's table — vault-level
    /// metadata that survives restarts without a schema bump).
    /// `None` when the key is absent.
    pub fn archive_meta(&self, key: &str) -> Result<Option<String>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM archive_meta WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value)
    }

    /// Write one `archive_meta` key's value (idempotent upsert).
    /// Not a content change: the archive revision is *not* bumped —
    /// meta keys describe the vault, they never change what a
    /// paginated search returns.
    pub fn set_archive_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO archive_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    /// The live connection (writer when reached through the pool's write
    /// queue). Backup/maintenance operations that need the connection
    /// itself — the SQLite backup API, whole-file snapshots — use this;
    /// ordinary callers keep going through the typed methods.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Whether this open's file is an encrypted (SQLCipher) database —
    /// the honest at-rest fact for the health/inspect read models.
    /// Derived from the database file header at open; a plaintext vault
    /// opened in an encryption-capable build still reports `false`.
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    /// The key this open used (`None` for a plaintext vault, including
    /// one opened with a key offered but not used). Connection-creating
    /// flows that share this open's identity — the backup snapshot, the
    /// verify opens — pass this on so an encrypted vault never grows a
    /// plaintext side door.
    pub fn vault_key(&self) -> Option<&crate::encryption::VaultKey> {
        self.key.as_ref()
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

    /// The retained original of one archived message (A3b): the digest
    /// of its raw RFC 5322 bytes as stored in the object store, or
    /// `None` when the record has no retained original — every row
    /// archived without retention, whose export fidelity label is then
    /// [`crate::ORIGINAL_NOT_RETAINED`]. The bytes themselves are
    /// opened through the object store
    /// ([`crate::object_store::ObjectStore`]) by digest.
    pub fn original_for_message(&self, message_id: i64) -> Result<Option<OriginalObject>> {
        let value = self
            .conn
            .query_row(
                "SELECT o.digest, o.size, o.kind, o.retained_at
                 FROM original_references r
                 JOIN original_objects o ON o.digest = r.digest
                 WHERE r.message_id = ?1",
                params![message_id],
                |row| {
                    Ok(OriginalObject {
                        digest: row.get(0)?,
                        size: row.get::<_, i64>(1)?.max(0) as u64,
                        kind: row.get(2)?,
                        retained_at: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(value)
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

    /// Resolve one media digest to the archived messages that reference
    /// it — the affected-message side of a reconciliation finding: when
    /// [`crate::object_store::ObjectStore::reconcile`] names a digest
    /// missing or corrupt, this is the route from the damaged object
    /// back to every message it affects. The same scan
    /// [`Storage::message_media_digests`] walks, keeping message
    /// identity and conversation/date context instead of discarding it.
    ///
    /// One row per message (oldest message id first, the first matching
    /// entry's display name), returned in bounded pages: `limit` is
    /// clamped like the search surface ([`crate::query::MAX_LIMIT`])
    /// and `total` carries the full affected count for pagination. A
    /// digest referenced by no message (a registered unlinked file —
    /// see [`Storage::unlinked_media_objects`] for its provenance)
    /// resolves to an empty page with `total == 0`. Read-only: a pure
    /// SELECT over stored rows.
    pub fn media_digest_references(
        &self,
        sha256: &str,
        offset: u32,
        limit: u32,
    ) -> Result<MediaDigestReferences> {
        let mut all = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT m.id, COALESCE(c.label, ''), m.chat_id, m.author,
                        COALESCE(m.sent_instant, m.original_timestamp, ''), m.raw_json
                 FROM archive_messages m
                 LEFT JOIN conversations c ON c.id = m.conversation_id
                 WHERE m.raw_json LIKE '%\"media\"%'
                 ORDER BY m.id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let id: i64 = row.get(0)?;
                let raw: String = row.get(5)?;
                // One row per message: the first matching entry names it.
                let matches = media_entries(&raw)
                    .into_iter()
                    .find(|entry| entry.get("sha256").and_then(|v| v.as_str()) == Some(sha256));
                if let Some(entry) = matches {
                    all.push(MediaMessageReference {
                        message_id: id,
                        conversation: row.get(1)?,
                        chat_id: row.get(2)?,
                        author: row.get(3)?,
                        date: row.get(4)?,
                        name: entry
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    });
                }
            }
        }
        let total = all.len() as u64;
        let limit = limit.clamp(1, crate::query::MAX_LIMIT);
        let offset = offset.min(crate::query::MAX_OFFSET);
        let references = all
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        Ok(MediaDigestReferences {
            sha256: sha256.to_string(),
            total,
            offset,
            limit,
            references,
        })
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

    /// Paged, searchable read of the unlinked media-object registry
    /// (M4.2) — the backend of the Files-not-linked-to-a-message view.
    /// The total, the rows, and the archive revision are read inside one
    /// transaction (one snapshot), so a page can never disagree with its
    /// own count; `limit` defaults to [`UNLINKED_DEFAULT_LIMIT`] and
    /// clamps to `1..=UNLINKED_MAX_LIMIT`, `offset` to
    /// `0..=UNLINKED_MAX_OFFSET` (the search2 paging convention — see
    /// [`crate::search2`]). The caller holds one page; the registry is
    /// never read in full.
    ///
    /// Rows report the registry's recorded provenance only: a digest
    /// shared across imports keeps its first-seen registration (nothing
    /// invents a second source), and nothing here ever assigns a file to
    /// a message — linkage stays what ingest recorded. Availability is
    /// disk state, not registry state, so the SQL read leaves it
    /// unstamped; run [`UnlinkedMediaPage::stamp_availability`] against
    /// the object store afterwards.
    pub fn unlinked_media_page(
        &self,
        request: &UnlinkedMediaPageRequest,
    ) -> Result<UnlinkedMediaPage> {
        let limit = request
            .limit
            .unwrap_or(UNLINKED_DEFAULT_LIMIT)
            .clamp(1, UNLINKED_MAX_LIMIT);
        let offset = request.offset.unwrap_or(0).min(UNLINKED_MAX_OFFSET);
        let search = request.search.trim();
        let import_id = request
            .import_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());

        // The shared filter (AND ... clauses), bound by the same values
        // in the count and the page statements — the shape search2's
        // page/count pairs use.
        let mut filter = String::new();
        let mut values: Vec<SqlValue> = Vec::new();
        if !search.is_empty() {
            filter.push_str(
                " AND (fold(name) LIKE fold(?) ESCAPE '\\'
                   OR fold(kind) LIKE fold(?) ESCAPE '\\')",
            );
            let pattern = format!("%{}%", crate::query::escape_like(search));
            values.push(SqlValue::from(pattern.clone()));
            values.push(SqlValue::from(pattern));
        }
        if let Some(import) = import_id {
            filter.push_str(" AND import_id = ?");
            values.push(SqlValue::from(import.to_string()));
        }
        let order = match request.sort {
            UnlinkedMediaSort::FirstSeen => "first_seen, sha256",
            UnlinkedMediaSort::Digest => "sha256",
        };

        let tx = self.conn.unchecked_transaction()?;
        let total: u64 = {
            let sql = format!("SELECT COUNT(*) FROM media_objects WHERE 1=1 {filter}");
            let mut stmt = tx.prepare(&sql)?;
            stmt.query_row(params_from_iter(values.iter()), |row| row.get(0))?
        };
        let objects = {
            let mut page_values = values.clone();
            page_values.push(SqlValue::Integer(limit as i64));
            page_values.push(SqlValue::Integer(offset as i64));
            let sql = format!(
                "SELECT sha256, size, name, kind, origin, import_id, first_seen
                 FROM media_objects WHERE 1=1 {filter}
                 ORDER BY {order} LIMIT ? OFFSET ?"
            );
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(page_values), |row| {
                Ok(UnlinkedMediaRow {
                    sha256: row.get(0)?,
                    size: row.get::<_, i64>(1)?.max(0) as u64,
                    name: row.get(2)?,
                    kind: row.get(3)?,
                    origin: row.get(4)?,
                    import_id: row.get(5)?,
                    first_seen: row.get(6)?,
                    available: false,
                    path: None,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let archive_revision = archive_revision_on(&tx)?;
        tx.commit()?;
        Ok(UnlinkedMediaPage {
            objects,
            total,
            limit,
            offset,
            archive_revision,
        })
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

    // ------------------------------------------------------------------
    // People and user-confirmed identity links (migration 20)
    // ------------------------------------------------------------------

    /// Create a person record. Purely a navigation layer: nothing about
    /// archived messages changes (see the identity contract in
    /// docs/data-and-api-contracts.md — a person exists only to hold
    /// user-confirmed identity links).
    pub fn create_person(&self, name: &str) -> Result<Person> {
        let name = normalize_person_name(name)?;
        let id = new_uuid();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO people (id, name) VALUES (?1, ?2)",
            params![id, name],
        )?;
        let person = tx
            .query_row(
                "SELECT id, name, created_at, updated_at FROM people WHERE id = ?1",
                params![id],
                person_from_row,
            )
            .optional()?;
        tx.commit()?;
        person.ok_or_else(|| Error::Sqlite(rusqlite::Error::QueryReturnedNoRows))
    }

    /// Rename a person (display only — a person's name is never identity
    /// evidence). Unknown ids are refused with [`Error::PersonNotFound`].
    pub fn rename_person(&self, id: &str, name: &str) -> Result<()> {
        let name = normalize_person_name(name)?;
        let updated = self.conn.execute(
            "UPDATE people SET name = ?2, updated_at = datetime('now')
             WHERE id = ?1",
            params![id, name],
        )?;
        if updated == 0 {
            return Err(Error::PersonNotFound(id.to_string()));
        }
        Ok(())
    }

    /// Delete a person and — via the schema's `ON DELETE CASCADE` —
    /// exactly their identity-link rows. Archived content is untouched:
    /// no message, author, or conversation row changes.
    pub fn delete_person(&self, id: &str) -> Result<()> {
        let deleted = self
            .conn
            .execute("DELETE FROM people WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(Error::PersonNotFound(id.to_string()));
        }
        Ok(())
    }

    /// Every person, oldest first (deterministic: creation time, then
    /// id as the tiebreaker).
    pub fn list_people(&self) -> Result<Vec<Person>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, updated_at
             FROM people ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], person_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Explicitly link one author identity to a person.
    ///
    /// The anchor is a canonical source instance plus a stable author
    /// key ([`PersonAnchorKind::StableKey`], case-folded) or a
    /// source-scoped author display string
    /// ([`PersonAnchorKind::AuthorName`], verbatim — ambiguous by
    /// nature, which is why it never escapes its source instance).
    /// Linking is the *only* way an identity joins a person; nothing is
    /// ever inferred.
    ///
    /// Re-linking the exact anchor to the same person is an idempotent
    /// no-op. Linking an anchor that already belongs to another person
    /// is refused with [`Error::IdentityLinkConflict`] naming the
    /// current owner — reassignment is always an explicit
    /// unlink-then-link, never a silent move.
    pub fn link_person_identity(
        &self,
        person_id: &str,
        link: &NewPersonIdentityLink<'_>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        apply_person_link(&tx, person_id, link)?;
        tx.commit()?;
        Ok(())
    }

    /// Remove one identity link (by its row id). Only the association
    /// row is deleted — the person, the source instance, and every
    /// archived message are untouched, and the anchor is immediately
    /// linkable again (reversibility). Unknown link ids — and links
    /// belonging to a different person — are refused with
    /// [`Error::IdentityLinkNotFound`] / [`Error::PersonNotFound`].
    pub fn unlink_person_identity(&self, person_id: &str, link_id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        apply_person_unlink(&tx, person_id, link_id)?;
        tx.commit()?;
        Ok(())
    }

    /// The identity links of one person, oldest first (deterministic:
    /// link time, then id).
    pub fn person_identity_links(&self, person_id: &str) -> Result<Vec<PersonIdentityLink>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, person_id, source_instance_id, anchor_kind,
                    anchor_value, display_label, linked_at
             FROM person_identity_links
             WHERE person_id = ?1
             ORDER BY linked_at, id",
        )?;
        let rows = stmt.query_map(params![person_id], person_link_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Resolve an identity anchor to its person, if the user has linked
    /// one. This is the lookup the person view rides: a message's
    /// (source instance, author anchor) resolves through it, so new
    /// imports under an already-linked stable key join the person's
    /// message set automatically, while the archived rows keep their
    /// original author strings. `anchor_value` is normalized exactly as
    /// [`Storage::link_person_identity`] normalizes it.
    pub fn person_for_anchor(
        &self,
        source_instance_id: &str,
        anchor_kind: PersonAnchorKind,
        anchor_value: &str,
    ) -> Result<Option<Person>> {
        let anchor_value = normalize_anchor_value(anchor_kind, anchor_value)?;
        let person = self
            .conn
            .query_row(
                "SELECT p.id, p.name, p.created_at, p.updated_at
                 FROM person_identity_links AS l JOIN people AS p ON p.id = l.person_id
                 WHERE l.source_instance_id = ?1
                   AND l.anchor_kind = ?2
                   AND l.anchor_value = ?3",
                params![source_instance_id, anchor_kind.as_str(), anchor_value],
                person_from_row,
            )
            .optional()?;
        Ok(person)
    }

    /// The organization revision the person view currently sits at
    /// (A5a): every [`Storage::person_links_commit`] that changes
    /// anything bumps it inside the same transaction, so a preview or
    /// a paginated person view can detect that the organization moved
    /// underneath it instead of silently showing a changed person.
    /// Starts at 0; lives in `archive_meta` next to the content
    /// revision, so no migration is needed for the counter itself.
    pub fn organization_revision(&self) -> Result<u64> {
        organization_revision_on(&self.conn)
    }

    /// Preview a set of link/unlink operations (A5a): per-identity
    /// evidence — the anchor exactly as it would be stored, how many
    /// archived messages it matches in its source instance, up to
    /// three representative messages, and for link ops the person
    /// already holding a conflicting anchor. Nothing is written.
    ///
    /// The result carries the organization revision it was read at;
    /// passing that revision to [`Storage::person_links_commit`] makes
    /// the confirm/re-preview cycle explicit. Same-label identities
    /// stay distinct entries: one per operation, each scoped to its
    /// own source instance — nothing is merged or auto-linked.
    ///
    /// Validation mirrors the commit: unknown persons, empty anchors,
    /// and unknown or foreign link ids are refused here, so a clean
    /// preview means a confirmable set (bar a concurrent change, which
    /// the commit's revision check catches).
    pub fn person_link_preview(&self, ops: &[PersonLinkOp<'_>]) -> Result<PersonLinkPreview> {
        const REPRESENTATIVES: usize = 3;
        let mut entries = Vec::with_capacity(ops.len());
        for op in ops {
            // Resolve the op to (person, anchor descriptor, owner forecast).
            let (person_id, source_instance_id, anchor_kind, anchor_value, display_label, owner) =
                match op {
                    PersonLinkOp::Link { person_id, link } => {
                        let anchor_value =
                            normalize_anchor_value(link.anchor_kind, link.anchor_value)?;
                        let exists: Option<String> = self
                            .conn
                            .query_row(
                                "SELECT id FROM people WHERE id = ?1",
                                params![person_id],
                                |row| row.get(0),
                            )
                            .optional()?;
                        if exists.is_none() {
                            return Err(Error::PersonNotFound(person_id.to_string()));
                        }
                        let owner: Option<Person> = self
                            .conn
                            .query_row(
                                "SELECT p.id, p.name, p.created_at, p.updated_at
                                 FROM person_identity_links AS l
                                 JOIN people AS p ON p.id = l.person_id
                                 WHERE l.source_instance_id = ?1
                                   AND l.anchor_kind = ?2
                                   AND l.anchor_value = ?3",
                                params![
                                    link.source_instance_id,
                                    link.anchor_kind.as_str(),
                                    anchor_value
                                ],
                                person_from_row,
                            )
                            .optional()?;
                        (
                            person_id.to_string(),
                            link.source_instance_id.to_string(),
                            link.anchor_kind,
                            anchor_value,
                            link.display_label.to_string(),
                            owner.filter(|p| p.id != *person_id),
                        )
                    }
                    PersonLinkOp::Unlink { person_id, link_id } => {
                        // Same refusal semantics as the unlink itself:
                        // the row must exist and belong to this person.
                        let anchor: Option<(String, String, String, String, String)> = self
                            .conn
                            .query_row(
                                "SELECT person_id, source_instance_id, anchor_kind,
                                        anchor_value, display_label
                                 FROM person_identity_links WHERE id = ?1",
                                params![link_id],
                                |row| {
                                    Ok((
                                        row.get(0)?,
                                        row.get(1)?,
                                        row.get(2)?,
                                        row.get(3)?,
                                        row.get(4)?,
                                    ))
                                },
                            )
                            .optional()?;
                        let (owner_id, source_instance_id, kind_raw, anchor_value, display_label) =
                            match anchor {
                                Some(columns) => columns,
                                None => {
                                    return Err(Error::IdentityLinkNotFound(link_id.to_string()))
                                }
                            };
                        if owner_id != *person_id {
                            return Err(Error::IdentityLinkNotFound(format!(
                                "link {link_id} belongs to person {owner_id}, not {person_id}"
                            )));
                        }
                        let anchor_kind =
                            PersonAnchorKind::from_str_opt(&kind_raw).ok_or_else(|| {
                                Error::Sqlite(rusqlite::Error::FromSqlConversionFailure(
                                    2,
                                    rusqlite::types::Type::Text,
                                    format!("invalid anchor_kind: {kind_raw}").into(),
                                ))
                            })?;
                        (
                            person_id.to_string(),
                            source_instance_id,
                            anchor_kind,
                            anchor_value,
                            display_label,
                            None,
                        )
                    }
                };
            let (matched_messages, representative_messages) = anchor_message_evidence(
                &self.conn,
                &source_instance_id,
                anchor_kind,
                &anchor_value,
                REPRESENTATIVES,
            )?;
            entries.push(PersonLinkPreviewEntry {
                person_id,
                source_instance_id,
                anchor_kind,
                anchor_value,
                display_label,
                matched_messages,
                representative_messages,
                current_owner: owner,
            });
        }
        Ok(PersonLinkPreview {
            organization_revision: self.organization_revision()?,
            entries,
        })
    }

    /// Apply a set of link/unlink operations in **one transaction**
    /// (A5a), guarded by the organization revision the caller
    /// previewed at:
    ///
    /// - a commit whose `expected_organization_revision` is not the
    ///   current one is refused with [`Error::PersonLinksStaleRevision`]
    ///   carrying the current revision — the caller re-previews and
    ///   confirms against the fresh evidence, never commits onto a
    ///   silently changed organization;
    /// - each op follows exactly the single-link semantics
    ///   ([`Storage::link_person_identity`] /
    ///   [`Storage::unlink_person_identity`]): idempotent re-links are
    ///   no-ops, conflicting anchors are refused with
    ///   [`Error::IdentityLinkConflict`], and a refusal rolls the
    ///   whole set back;
    /// - every op that actually changes the organization writes a
    ///   `person_link_history` row (migration 23) and bumps the
    ///   organization revision by exactly one for the whole set —
    ///   the revision the result (and the history rows) carry.
    ///
    /// Archived rows are never touched: only association rows, the
    /// history ledger, and the counter change.
    pub fn person_links_commit(
        &self,
        expected_organization_revision: u64,
        ops: &[PersonLinkOp<'_>],
    ) -> Result<PersonLinksCommit> {
        let tx = self.conn.unchecked_transaction()?;
        let current = organization_revision_on(&tx)?;
        if current != expected_organization_revision {
            return Err(Error::PersonLinksStaleRevision {
                expected: expected_organization_revision,
                current,
            });
        }
        let mut changed = false;
        for op in ops {
            match op {
                PersonLinkOp::Link { person_id, link } => {
                    if apply_person_link(&tx, person_id, link)? {
                        changed = true;
                        record_person_link_history(
                            &tx,
                            person_id,
                            link.source_instance_id,
                            link.anchor_kind.as_str(),
                            &normalize_anchor_value(link.anchor_kind, link.anchor_value)?,
                            PersonLinkOperation::Link,
                            current + 1,
                        )?;
                    }
                }
                PersonLinkOp::Unlink { person_id, link_id } => {
                    let anchor = apply_person_unlink(&tx, person_id, link_id)?;
                    changed = true;
                    record_person_link_history(
                        &tx,
                        person_id,
                        &anchor.source_instance_id,
                        &anchor.anchor_kind,
                        &anchor.anchor_value,
                        PersonLinkOperation::Unlink,
                        current + 1,
                    )?;
                }
            }
        }
        let organization_revision = if changed {
            bump_organization_revision(&tx)?;
            current + 1
        } else {
            current
        };
        tx.commit()?;
        Ok(PersonLinksCommit {
            organization_revision,
        })
    }

    /// One person's applied association changes from the history
    /// ledger (migration 23), oldest first — the undo/audit record
    /// behind every preview/commit cycle. Survives person deletion:
    /// deleting a person removes organization only, and the ledger is
    /// the record of what was organized, not a live association.
    pub fn person_link_history(&self, person_id: &str) -> Result<Vec<PersonLinkHistoryRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, person_id, source_instance_id, anchor_kind, anchor_value,
                    operation, organization_revision, recorded_at
             FROM person_link_history
             WHERE person_id = ?1
             ORDER BY recorded_at, id",
        )?;
        let rows = stmt.query_map(params![person_id], |row| {
            let kind_raw: String = row.get(3)?;
            let anchor_kind = PersonAnchorKind::from_str_opt(&kind_raw).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    format!("invalid anchor_kind: {kind_raw}").into(),
                )
            })?;
            let op_raw: String = row.get(5)?;
            let operation = match op_raw.as_str() {
                "link" => PersonLinkOperation::Link,
                "unlink" => PersonLinkOperation::Unlink,
                _ => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        format!("invalid operation: {op_raw}").into(),
                    ))
                }
            };
            Ok(PersonLinkHistoryRecord {
                id: row.get(0)?,
                person_id: row.get(1)?,
                source_instance_id: row.get(2)?,
                anchor_kind,
                anchor_value: row.get(4)?,
                operation,
                organization_revision: row.get::<_, i64>(6)?.max(0) as u64,
                recorded_at: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ------------------------------------------------------------------
    // Saved searches (migration 18)
    // ------------------------------------------------------------------

    /// The saved-search definitions, oldest first (same-second ties
    /// broken by name) — the order the Archive workspace lists them in.
    /// Each definition is a dynamic v2 request: executing it re-runs the
    /// search, so newly imported matching messages appear, and source
    /// references are canonical source ids, so label renames and
    /// duplicate labels never redirect a saved search.
    pub fn saved_searches(&self) -> Result<Vec<SavedSearch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, request_json, created_at, updated_at
             FROM saved_searches ORDER BY created_at, name",
        )?;
        let rows = stmt.query_map([], saved_search_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Create or update one saved-search definition:
    ///
    /// - `id` `None` creates a new definition under `name` — a name
    ///   another definition already holds is refused, not silently
    ///   overwritten;
    /// - `id` `Some(id)` updates that definition in place: a new name
    ///   (rename), a new request (update from the current search), or
    ///   both. Renaming onto a name another definition holds is
    ///   refused; an unknown `id` is refused.
    ///
    /// The request is validated by normalizing it (bad text length, day
    /// bounds, or timezone refuse the save), then persisted as serde
    /// JSON with `limit`/`offset` stripped — pagination is
    /// per-execution, not part of the definition. Only the
    /// `saved_searches` row is touched; archived messages never are.
    pub fn upsert_saved_search(
        &self,
        id: Option<&str>,
        name: &str,
        request: &SearchArchiveRequest,
    ) -> Result<SavedSearch> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::SavedSearch(
                "a saved search needs a name".to_string(),
            ));
        }
        if name.chars().count() > SAVED_SEARCH_MAX_NAME_CHARS {
            return Err(Error::SavedSearch(format!(
                "saved-search name exceeds {SAVED_SEARCH_MAX_NAME_CHARS} characters"
            )));
        }
        // Refuse to store a request that could not execute: normalize
        // (validate) it and drop the resolved form.
        request.normalize()?;

        // Pagination never persists: strip it before serializing.
        let mut stored = request.clone();
        stored.limit = None;
        stored.offset = None;
        let request_json = serde_json::to_string(&stored)?;

        let tx = self.conn.unchecked_transaction()?;
        let taken: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM saved_searches
                 WHERE name = ?1 AND (?2 IS NULL OR id != ?2)
             )",
            params![name, id],
            |row| row.get(0),
        )?;
        if taken {
            return Err(Error::SavedSearch(format!(
                "a saved search named {name:?} already exists"
            )));
        }
        let row_id = match id {
            None => {
                let new_id = new_uuid();
                tx.execute(
                    "INSERT INTO saved_searches (id, name, request_json)
                     VALUES (?1, ?2, ?3)",
                    params![new_id, name, request_json],
                )?;
                new_id
            }
            Some(id) => {
                let updated = tx.execute(
                    "UPDATE saved_searches
                     SET name = ?2, request_json = ?3,
                         updated_at = datetime('now')
                     WHERE id = ?1",
                    params![id, name, request_json],
                )?;
                if updated == 0 {
                    return Err(Error::SavedSearch(format!("no saved search with id {id}")));
                }
                id.to_string()
            }
        };
        let stored = tx.query_row(
            "SELECT id, name, request_json, created_at, updated_at
             FROM saved_searches WHERE id = ?1",
            params![row_id],
            saved_search_from_row,
        )?;
        tx.commit()?;
        Ok(stored)
    }

    /// Delete one saved-search definition. Returns whether a row was
    /// removed (deleting an unknown id is not an error). Only the
    /// definition goes — archived messages, sources, and every other
    /// definition are untouched.
    pub fn delete_saved_search(&self, id: &str) -> Result<bool> {
        let removed = self
            .conn
            .execute("DELETE FROM saved_searches WHERE id = ?1", params![id])?;
        Ok(removed > 0)
    }

    // ------------------------------------------------------------------
    // Collections (user organization, migration 19)
    // ------------------------------------------------------------------

    /// Create a collection with the given name (trimmed; empty after
    /// trimming is refused). Names are not unique — the returned id is
    /// the stable handle. Organization is vault-local metadata: nothing
    /// here reads or writes archived content.
    pub fn create_collection(&self, name: &str) -> Result<Collection> {
        let name = validate_collection_name(name)?;
        let tx = self.conn.unchecked_transaction()?;
        let id = new_uuid();
        tx.execute(
            "INSERT INTO collections (id, name) VALUES (?1, ?2)",
            params![id, name],
        )?;
        let collection =
            collection_row(&tx, &id)?.expect("just-inserted collection must read back");
        tx.commit()?;
        Ok(collection)
    }

    /// Rename a collection (`false` when no collection has `id`). The
    /// name is trimmed and must be non-empty, like
    /// [`Storage::create_collection`].
    pub fn rename_collection(&self, id: &str, name: &str) -> Result<bool> {
        let name = validate_collection_name(name)?;
        let tx = self.conn.unchecked_transaction()?;
        let renamed = tx.execute(
            "UPDATE collections SET name = ?2, updated_at = datetime('now')
             WHERE id = ?1",
            params![id, name],
        )?;
        tx.commit()?;
        Ok(renamed > 0)
    }

    /// Delete a collection. Memberships cascade; the underlying archive
    /// is never touched (`false` when no collection has `id`).
    pub fn delete_collection(&self, id: &str) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        // Delete the memberships explicitly so the archive-untouched
        // guarantee holds even under a connection with foreign keys
        // disabled (the cascade alone would still be correct, but the
        // deletion contract should not depend on the connection's pragma).
        tx.execute(
            "DELETE FROM collection_members WHERE collection_id = ?1",
            params![id],
        )?;
        let deleted = tx.execute("DELETE FROM collections WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(deleted > 0)
    }

    /// Every collection, oldest first (creation order — `rowid`, since
    /// the second-resolution `created_at` ties), each with its
    /// membership count (memberships whose referenced message is absent
    /// count too — they are surfaced, not hidden).
    pub fn list_collections(&self) -> Result<Vec<Collection>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.name, c.created_at, c.updated_at, COUNT(cm.message_id)
             FROM collections AS c
             LEFT JOIN collection_members AS cm ON cm.collection_id = c.id
             GROUP BY c.id
             ORDER BY c.rowid",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Collection {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
                message_count: row.get::<_, i64>(4)?.max(0) as u64,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// One collection's detail: the collection row plus its member
    /// rows, in add order (`rowid` — the second-resolution `added_at`
    /// ties), each joined with the message provenance the v2 detail
    /// path exposes (source instance, conversation). A membership whose
    /// canonical message is absent surfaces as an unavailable row — no
    /// provenance is invented, the membership does not vanish.
    /// `Ok(None)` when no collection has `id`.
    pub fn collection_detail(&self, id: &str) -> Result<Option<CollectionDetail>> {
        let tx = self.conn.unchecked_transaction()?;
        let Some(collection) = collection_row(&tx, id)? else {
            return Ok(None);
        };
        let members = {
            let mut stmt = tx.prepare(
                "SELECT cm.message_id, cm.added_at, m.id,
                        si.id, si.label, m.conversation_id, c.label,
                        m.author, NULLIF(m.subject, ''), m.sent_instant
                 FROM collection_members AS cm
                 LEFT JOIN archive_messages AS m ON m.id = cm.message_id
                 LEFT JOIN source_instances AS si ON si.id = m.source_instance_id
                 LEFT JOIN conversations AS c ON c.id = m.conversation_id
                 WHERE cm.collection_id = ?1
                 ORDER BY cm.rowid",
            )?;
            let members = stmt
                .query_map(params![id], |row| {
                    let available: Option<i64> = row.get(2)?;
                    if available.is_some() {
                        Ok(CollectionMember {
                            message_id: row.get(0)?,
                            available: true,
                            added_at: row.get(1)?,
                            source_id: Some(row.get(3)?),
                            source_label: Some(row.get(4)?),
                            conversation_id: row.get(5)?,
                            conversation_label: row.get(6)?,
                            author: Some(row.get(7)?),
                            subject: row.get(8)?,
                            sent_at: row.get(9)?,
                        })
                    } else {
                        Ok(CollectionMember {
                            message_id: row.get(0)?,
                            available: false,
                            added_at: row.get(1)?,
                            source_id: None,
                            source_label: None,
                            conversation_id: None,
                            conversation_label: None,
                            author: None,
                            subject: None,
                            sent_at: None,
                        })
                    }
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            members
        };
        tx.commit()?;
        Ok(Some(CollectionDetail {
            collection,
            members,
        }))
    }

    /// Add canonical messages (by stable `archive_messages` rowid) to
    /// a collection, idempotently: re-adding an existing membership
    /// changes nothing. Every id must reference an archived message,
    /// and the collection must exist — a reference that cannot be
    /// resolved now is refused instead of stored as unavailable (add
    /// what exists; absence surfaces only after the fact, through
    /// [`Storage::collection_detail`]). Returns how many memberships
    /// were newly added.
    pub fn add_collection_members(&self, id: &str, message_ids: &[i64]) -> Result<u64> {
        let tx = self.conn.unchecked_transaction()?;
        ensure_collection_exists(&tx, id)?;
        for &message_id in message_ids {
            let present: Option<i64> = tx
                .query_row(
                    "SELECT id FROM archive_messages WHERE id = ?1",
                    params![message_id],
                    |row| row.get(0),
                )
                .optional()?;
            if present.is_none() {
                return Err(Error::InvalidCollection(format!(
                    "message {message_id} is not in the archive"
                )));
            }
        }
        let mut added = 0u64;
        for &message_id in message_ids {
            added += tx.execute(
                "INSERT INTO collection_members (collection_id, message_id)
                     VALUES (?1, ?2)
                     ON CONFLICT (collection_id, message_id) DO NOTHING",
                params![id, message_id],
            )? as u64;
        }
        if added > 0 {
            tx.execute(
                "UPDATE collections SET updated_at = datetime('now') WHERE id = ?1",
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(added)
    }

    /// Remove one message from a collection (`false` when the
    /// membership does not exist). The archived message is never
    /// touched.
    pub fn remove_collection_member(&self, id: &str, message_id: i64) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;
        let removed = tx.execute(
            "DELETE FROM collection_members
             WHERE collection_id = ?1 AND message_id = ?2",
            params![id, message_id],
        )?;
        if removed > 0 {
            tx.execute(
                "UPDATE collections SET updated_at = datetime('now') WHERE id = ?1",
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(removed > 0)
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
    let instance = resolve_legacy_instance(tx, &msg.source, &msg.source_label)?;

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

/// Build and append the completed import's receipt row (migration 22)
/// inside the completing batch's transaction — the durable fact of what
/// the run observed, recorded, and failed. Every count is a run total
/// read from committed rows (new/present accumulated through the
/// checkpoint by [`commit_import_batch`]'s intermediate commits,
/// failures from `import_issues`, observed bounds and file outcomes
/// over the messages this import's occurrences point at), so an
/// interrupted-and-resumed run reports the same facts a clean run
/// would. No message bodies. Returns the stored row so the completing
/// commit's counts can carry it to the caller (A2b).
fn record_import_receipt_in_tx(
    tx: &Transaction<'_>,
    import_id: &str,
    source: &str,
    messages: &[NewMessage],
    new_records: u64,
    present_records: u64,
) -> Result<SourceReceipt> {
    // The instance the run stored under: the batch's first message
    // names it (a bounded import is one source); an empty completing
    // batch falls back to the checkpoint's namespace.
    let instance = match messages.first() {
        Some(msg) => resolve_legacy_instance(tx, &msg.source, "")?,
        None => resolve_legacy_instance(tx, source, "")?,
    };
    // Observed sent-time bounds over every message this import
    // touched, across all its batches. NULL bounds mean the run
    // observed no dated messages — the pair is evidence, never a
    // completeness claim between the endpoints.
    let (observed_min, observed_max): (Option<String>, Option<String>) = tx.query_row(
        "SELECT MIN(m.sent_instant), MAX(m.sent_instant)
         FROM archive_messages m
         JOIN message_occurrences o ON o.message_id = m.id
         WHERE o.import_id = ?1 AND m.sent_instant IS NOT NULL",
        params![import_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let failed_records: u64 = tx.query_row(
        "SELECT COUNT(*) FROM import_issues WHERE import_id = ?1",
        params![import_id],
        |row| Ok(row.get::<_, i64>(0)?.max(0) as u64),
    )?;
    // File outcomes from the run's committed media entries: an entry
    // carrying a digest is a preserved file; a `missing` entry is a
    // reference the run could not preserve. Unlinked registry objects
    // belong to no message entry and are outside these counts.
    let (mut files_preserved, mut files_missing) = (0u64, 0u64);
    {
        let mut stmt = tx.prepare(
            "SELECT m.raw_json
             FROM archive_messages m
             JOIN message_occurrences o ON o.message_id = m.id
             WHERE o.import_id = ?1 AND m.raw_json LIKE '%\"media\"%'",
        )?;
        let mut rows = stmt.query(params![import_id])?;
        while let Some(row) = rows.next()? {
            let raw: String = row.get(0)?;
            for entry in media_entries(&raw) {
                if entry
                    .get("missing")
                    .and_then(|m| m.as_bool())
                    .unwrap_or(false)
                {
                    files_missing += 1;
                } else if entry
                    .get("sha256")
                    .and_then(|s| s.as_str())
                    .is_some_and(|s| !s.is_empty())
                {
                    files_preserved += 1;
                }
            }
        }
    }
    let facts_json = serde_json::json!({ "import_id": import_id }).to_string();
    insert_source_receipt_in_tx(
        tx,
        &SourceReceiptRecord {
            source_instance_id: &instance,
            kind: SourceReceiptKind::Import,
            observed_min: observed_min.as_deref(),
            observed_max: observed_max.as_deref(),
            new_records,
            present_records,
            failed_records,
            files_preserved,
            files_missing,
            facts_json: &facts_json,
        },
    )?;
    // Read the row back for the columns the INSERT generates (`id`,
    // `completed_at`), so what the caller receives is exactly what the
    // listing serves.
    tx.query_row(
        "SELECT id, source_instance_id, kind, completed_at,
                observed_min, observed_max,
                new_records, present_records, failed_records,
                files_preserved, files_missing, facts_json
         FROM source_receipts WHERE id = last_insert_rowid()",
        [],
        source_receipt_from_row,
    )
    .map_err(Into::into)
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

/// The person-organization counterpart of [`bump_archive_revision`]
/// (A5a): bumped inside every `person_links_commit` that changes
/// anything, so person views and pending confirmations can detect a
/// changed organization instead of silently acting on a stale one.
/// Same key/value shape in `archive_meta`; separate counter, because
/// content and organization change independently.
fn bump_organization_revision(tx: &Connection) -> Result<()> {
    tx.execute(
        "INSERT INTO archive_meta (key, value) VALUES ('organization_revision', '1')
         ON CONFLICT(key) DO UPDATE SET
            value = CAST(CAST(value AS INTEGER) + 1 AS TEXT)
         WHERE key = 'organization_revision'",
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

/// The current `organization_revision` counter on an arbitrary
/// connection (0 before the first person-links commit — see
/// [`Storage::organization_revision`]).
pub(crate) fn organization_revision_on(conn: &Connection) -> Result<u64> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM archive_meta WHERE key = 'organization_revision'",
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
pub(crate) fn media_digests_on(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut digests = std::collections::HashSet::new();
    if table_exists_on(conn, "archive_messages")? {
        let mut stmt = conn
            .prepare("SELECT raw_json FROM archive_messages WHERE raw_json LIKE '%\"media\"%'")?;
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
    // Retained originals (A3b): the raw RFC 5322 bytes are objects in
    // the same store, referenced by the `original_objects` registry —
    // backup carries them and reconciliation covers them exactly like
    // media objects.
    if table_exists_on(conn, "original_objects")? {
        let mut stmt = conn.prepare("SELECT digest FROM original_objects")?;
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
        Ok(conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| {
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

/// Record one retained original for `message_id` inside an open ingest
/// transaction (A3b): the object row is idempotent on the digest (the
/// retention pass already stored/deduped the bytes through the object
/// store before this transaction — reference rows land *after* promote,
/// mirroring the media-reference order), and the reference row is
/// idempotent on the pair, so re-imports write nothing new.
fn record_original_in_tx(
    tx: &Transaction<'_>,
    message_id: i64,
    object: &crate::object_store::ObjectRef,
) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO original_objects (digest, size, kind)
         VALUES (?1, ?2, 'rfc5322')",
        params![object.sha256, object.size],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO original_references (message_id, digest)
         VALUES (?1, ?2)",
        params![message_id, object.sha256],
    )?;
    Ok(())
}

/// Find the legacy source instance for `namespace`, creating an
/// unresolved one when the namespace is new (kind/label record what is
/// actually known: a legacy-namespaced source with no connector
/// provenance). `label` is the instance's display name: the JSON Lines
/// importer passes a portable identity key as the namespace together
/// with the exporting vault's human label, so the destination instance
/// is named what the source vault named it; empty (every other caller)
/// means the namespace is its own label. Names follow identity — a
/// label only ever renames the resolved instance, never selects a
/// different one.
fn resolve_legacy_instance(conn: &Connection, namespace: &str, label: &str) -> Result<String> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM source_instances WHERE legacy_namespace = ?1",
            params![namespace],
            |row| row.get(0),
        )
        .optional()?
    {
        if !label.is_empty() {
            conn.execute(
                "UPDATE source_instances SET label = ?1 WHERE id = ?2 AND label <> ?1",
                params![label, id],
            )?;
        }
        return Ok(id);
    }
    let id = new_uuid();
    conn.execute(
        "INSERT INTO source_instances (id, legacy_namespace, kind, label)
         VALUES (?1, ?2, 'legacy', ?3)",
        params![
            id,
            namespace,
            if label.is_empty() { namespace } else { label }
        ],
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

pub(crate) fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        source: row.get(1)?,
        source_key: String::new(),
        external_id: row.get(2)?,
        channel: row.get(3)?,
        chat_id: row.get(4)?,
        author: row.get(5)?,
        sent_at: row.get(6)?,
        body: row.get(7)?,
        raw_json: row.get(8)?,
    })
}

/// The portable source identity of a row's source instance, as one SQL
/// column projected next to the [`message_from_row`] columns: connector-
/// owned instances compose `instance:<kind>:<connector_key>` (both parts
/// are connector-controlled, never human-edited labels, and the
/// `instance:` prefix keeps the key from aliasing any display string),
/// legacy instances reuse their legacy namespace (whose export already
/// carried it as `source`, so behavior is unchanged). An instance with
/// neither — the pre-canonical degenerate case — projects an empty key
/// and the legacy display-string contract applies.
pub(crate) const SOURCE_KEY_COLUMN: &str = "CASE WHEN si.connector_key IS NOT NULL \
     THEN 'instance:' || si.kind || ':' || si.connector_key \
     ELSE COALESCE(si.legacy_namespace, '') END";

/// Build an export row from a 10-column selection: the nine
/// [`message_from_row`] columns plus [`SOURCE_KEY_COLUMN`]. Only the JSON
/// export streams — search and whole-conversation alike — select that
/// extra column; every other query keeps the 9-column shape and an empty
/// key.
pub(crate) fn export_message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    let mut message = message_from_row(row)?;
    message.source_key = row.get(9)?;
    Ok(message)
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

/// What a user-confirmed identity link anchors to (migration 20). See
/// the identity contract in docs/data-and-api-contracts.md: the anchor
/// is always a source instance plus one of these, never a bare display
/// name matched across sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonAnchorKind {
    /// A stable, source-scoped author key where the source provides one
    /// (an email address for mail sources, a provider user id for chat
    /// exports that carry one). Case-folded at link time.
    StableKey,
    /// The verbatim author display string where the source provides
    /// nothing stable. Inherently ambiguous (two humans can share a
    /// display name), hence scoped to its source instance.
    AuthorName,
}

impl PersonAnchorKind {
    /// The stored `anchor_kind` token.
    pub fn as_str(self) -> &'static str {
        match self {
            PersonAnchorKind::StableKey => "stable_key",
            PersonAnchorKind::AuthorName => "author_name",
        }
    }

    /// Parse a stored `anchor_kind` token; `None` for anything else.
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "stable_key" => Some(PersonAnchorKind::StableKey),
            "author_name" => Some(PersonAnchorKind::AuthorName),
            _ => None,
        }
    }
}

/// A user-created person record (migration 20). Purely a navigation
/// layer over the archive: nothing about archived messages changes when
/// a person is created, renamed, or deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    /// Display name; never identity evidence.
    pub name: String,
    pub created_at: String,
    pub updated_at: String,
}

/// The identity-to-person association a caller asks to create (see
/// [`Storage::link_person_identity`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPersonIdentityLink<'a> {
    /// The canonical source instance the identity lives in.
    pub source_instance_id: &'a str,
    /// What `anchor_value` holds — see [`PersonAnchorKind`].
    pub anchor_kind: PersonAnchorKind,
    /// The stable author key (case-folded on storage) or the verbatim
    /// author display string (trimmed).
    pub anchor_value: &'a str,
    /// Rendered label for the link UI (e.g. the address as the user
    /// saw it); display only, never part of the anchor.
    pub display_label: &'a str,
}

/// One stored user-confirmed identity link (see
/// [`Storage::person_identity_links`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonIdentityLink {
    pub id: String,
    pub person_id: String,
    pub source_instance_id: String,
    pub anchor_kind: PersonAnchorKind,
    pub anchor_value: String,
    pub display_label: String,
    pub linked_at: String,
}

/// Validate and normalize a person name: trimmed and non-empty.
fn normalize_person_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::InvalidPersonInput(
            "person name must not be empty".into(),
        ));
    }
    Ok(name.to_string())
}

/// Normalize an anchor value for storage and lookup: stable keys are
/// trimmed and case-folded (email convention — addresses and provider
/// keys are case-insensitive in practice); author-name anchors keep
/// their verbatim case (display strings are matched exactly) and are
/// only trimmed. Empty anchors never identify anyone.
fn normalize_anchor_value(kind: PersonAnchorKind, value: &str) -> Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidPersonInput(
            "anchor value must not be empty".into(),
        ));
    }
    Ok(match kind {
        PersonAnchorKind::StableKey => trimmed.to_lowercase(),
        PersonAnchorKind::AuthorName => trimmed.to_string(),
    })
}

fn person_from_row(row: &Row<'_>) -> rusqlite::Result<Person> {
    Ok(Person {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
    })
}

fn person_link_from_row(row: &Row<'_>) -> rusqlite::Result<PersonIdentityLink> {
    let kind_raw: String = row.get(3)?;
    let anchor_kind = PersonAnchorKind::from_str_opt(&kind_raw).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            format!("invalid anchor_kind: {kind_raw}").into(),
        )
    })?;
    Ok(PersonIdentityLink {
        id: row.get(0)?,
        person_id: row.get(1)?,
        source_instance_id: row.get(2)?,
        anchor_kind,
        anchor_value: row.get(4)?,
        display_label: row.get(5)?,
        linked_at: row.get(6)?,
    })
}

/// One operation inside a preview/commit set (A5a). Every op names the
/// person it acts on; a `Link` carries the full anchor descriptor,
/// an `Unlink` names the stored link row to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonLinkOp<'a> {
    Link {
        person_id: &'a str,
        link: NewPersonIdentityLink<'a>,
    },
    Unlink {
        person_id: &'a str,
        link_id: &'a str,
    },
}

/// What a history row (and an op) did to an anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonLinkOperation {
    Link,
    Unlink,
}

impl PersonLinkOperation {
    /// The stored `operation` token.
    pub fn as_str(self) -> &'static str {
        match self {
            PersonLinkOperation::Link => "link",
            PersonLinkOperation::Unlink => "unlink",
        }
    }
}

/// A representative message of one identity, for the preview's "these
/// are the messages you would organize" evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonLinkPreviewMessage {
    pub id: i64,
    pub author: String,
    pub sent_at: String,
    /// Short body excerpt (bounded, display only).
    pub snippet: String,
}

/// Per-identity preview evidence (A5a): the anchor exactly as it would
/// be stored, how many archived messages it matches in its source
/// instance, and up to three representative messages. Same-label
/// identities stay distinct entries — one entry per operation, each
/// anchored to its own source instance, never merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonLinkPreviewEntry {
    pub person_id: String,
    pub source_instance_id: String,
    pub anchor_kind: PersonAnchorKind,
    pub anchor_value: String,
    pub display_label: String,
    /// Archived messages in this source instance the anchor matches.
    pub matched_messages: u64,
    /// Up to three representative matches, oldest first.
    pub representative_messages: Vec<PersonLinkPreviewMessage>,
    /// For a `Link` op: the person already holding the anchor, when it
    /// is someone else — the commit would refuse with
    /// [`Error::IdentityLinkConflict`]; `None` when the anchor is free
    /// (or already this person's). Unlink ops carry `None`.
    pub current_owner: Option<Person>,
}

/// The [`Storage::person_link_preview`] result: the organization
/// revision the evidence was read at (the value a subsequent
/// [`Storage::person_links_commit`] must carry), one entry per
/// operation, in operation order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonLinkPreview {
    pub organization_revision: u64,
    pub entries: Vec<PersonLinkPreviewEntry>,
}

/// The [`Storage::person_links_commit`] result: the organization
/// revision the vault now sits at (the caller's new preview base).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonLinksCommit {
    pub organization_revision: u64,
}

/// One applied association change from the audit ledger (migration
/// 23). Pure record: nothing about archived messages is derivable or
/// mutated from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonLinkHistoryRecord {
    pub id: i64,
    pub person_id: String,
    pub source_instance_id: String,
    pub anchor_kind: PersonAnchorKind,
    pub anchor_value: String,
    pub operation: PersonLinkOperation,
    /// Organization revision the commit that applied this change
    /// produced.
    pub organization_revision: u64,
    pub recorded_at: String,
}

/// Extract the case-folded address a stable-key anchor matches from a
/// verbatim author string: the `<...>` segment of a `Name <address>`
/// author, or the whole trimmed string when the author is a bare
/// address (contains `@`). Authors that carry neither form — chat
/// display names — have *no* stable key; they surface as ambiguous,
/// source-scoped author-name identities and never match a stable-key
/// anchor.
pub(crate) fn author_stable_key(author: &str) -> Option<String> {
    let trimmed = author.trim();
    if let (Some(open), Some(close)) = (trimmed.rfind('<'), trimmed.rfind('>')) {
        if open < close {
            let inner = trimmed[open + 1..close].trim();
            if !inner.is_empty() {
                return Some(inner.to_lowercase());
            }
        }
    }
    if trimmed.contains('@') {
        return Some(trimmed.to_lowercase());
    }
    None
}

/// A short, char-boundary-safe body excerpt for preview evidence.
fn preview_snippet(body: &str) -> String {
    const MAX_CHARS: usize = 120;
    if body.chars().count() <= MAX_CHARS {
        return body.to_string();
    }
    body.chars().take(MAX_CHARS).collect()
}

/// The anchor columns of one `person_identity_links` row, as the
/// unlink path needs them (explanation errors and history records).
struct LinkAnchor {
    source_instance_id: String,
    anchor_kind: String,
    anchor_value: String,
}

/// The shared body of [`Storage::link_person_identity`] and the commit
/// path: validate, conflict-check, and insert one link row on `conn`
/// (inside the caller's transaction). Returns whether a row was
/// inserted — re-linking an anchor this person already holds is an
/// idempotent no-op, not a change.
fn apply_person_link(
    conn: &Connection,
    person_id: &str,
    link: &NewPersonIdentityLink<'_>,
) -> Result<bool> {
    let anchor_value = normalize_anchor_value(link.anchor_kind, link.anchor_value)?;
    let person: Option<String> = conn
        .query_row(
            "SELECT id FROM people WHERE id = ?1",
            params![person_id],
            |row| row.get(0),
        )
        .optional()?;
    if person.is_none() {
        return Err(Error::PersonNotFound(person_id.to_string()));
    }
    let owner: Option<(String, String)> = conn
        .query_row(
            "SELECT p.id, p.name
             FROM person_identity_links AS l JOIN people AS p ON p.id = l.person_id
             WHERE l.source_instance_id = ?1
               AND l.anchor_kind = ?2
               AND l.anchor_value = ?3",
            params![
                link.source_instance_id,
                link.anchor_kind.as_str(),
                anchor_value
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match owner {
        Some((owner_id, owner_name)) if owner_id != person_id => {
            return Err(Error::IdentityLinkConflict {
                source_instance_id: link.source_instance_id.to_string(),
                anchor_kind: link.anchor_kind.as_str().to_string(),
                anchor_value,
                owner_id,
                owner_name,
            });
        }
        // Already linked to this person: idempotent no-op.
        Some(_) => Ok(false),
        None => {
            conn.execute(
                "INSERT INTO person_identity_links
                     (id, person_id, source_instance_id, anchor_kind,
                      anchor_value, display_label)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    new_uuid(),
                    person_id,
                    link.source_instance_id,
                    link.anchor_kind.as_str(),
                    anchor_value,
                    link.display_label
                ],
            )?;
            Ok(true)
        }
    }
}

/// The shared body of [`Storage::unlink_person_identity`] and the
/// commit path: locate one link row, refuse with the same
/// explanations when it is missing or someone else's, and delete it
/// inside the caller's transaction. Returns the removed row's anchor
/// columns for the history record.
fn apply_person_unlink(conn: &Connection, person_id: &str, link_id: &str) -> Result<LinkAnchor> {
    let anchor: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT person_id, source_instance_id, anchor_kind, anchor_value
             FROM person_identity_links WHERE id = ?1",
            params![link_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let (owner, source_instance_id, anchor_kind, anchor_value) = match anchor {
        Some(columns) => columns,
        None => return Err(Error::IdentityLinkNotFound(link_id.to_string())),
    };
    if owner != person_id {
        return Err(Error::IdentityLinkNotFound(format!(
            "link {link_id} belongs to person {owner}, not {person_id}"
        )));
    }
    conn.execute(
        "DELETE FROM person_identity_links WHERE id = ?1 AND person_id = ?2",
        params![link_id, person_id],
    )?;
    Ok(LinkAnchor {
        source_instance_id,
        anchor_kind,
        anchor_value,
    })
}

/// Record one applied association change in the history ledger
/// (migration 23) at the revision the committing transaction produced.
fn record_person_link_history(
    conn: &Connection,
    person_id: &str,
    source_instance_id: &str,
    anchor_kind: &str,
    anchor_value: &str,
    operation: PersonLinkOperation,
    organization_revision: u64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO person_link_history
             (person_id, source_instance_id, anchor_kind, anchor_value,
              operation, organization_revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            person_id,
            source_instance_id,
            anchor_kind,
            anchor_value,
            operation.as_str(),
            organization_revision as i64
        ],
    )?;
    Ok(())
}

/// Message evidence of one anchor inside its source instance: how many
/// archived rows match, plus up to `limit` representative rows, oldest
/// first. Author-name anchors match the verbatim author string;
/// stable-key anchors match the case-folded address extracted from the
/// author string (`Name <address>` and bare-address forms).
fn anchor_message_evidence(
    conn: &Connection,
    source_instance_id: &str,
    anchor_kind: PersonAnchorKind,
    anchor_value: &str,
    limit: usize,
) -> Result<(u64, Vec<PersonLinkPreviewMessage>)> {
    const REPRESENTATIVE_SQL: &str = "SELECT id, author, COALESCE(sent_instant, ''), body
             FROM archive_messages
             WHERE source_instance_id = ?1";
    match anchor_kind {
        PersonAnchorKind::AuthorName => {
            let matched: i64 = conn.query_row(
                "SELECT COUNT(*) FROM archive_messages
                 WHERE source_instance_id = ?1 AND author = ?2",
                params![source_instance_id, anchor_value],
                |row| row.get(0),
            )?;
            let mut stmt = conn.prepare(&format!(
                "{REPRESENTATIVE_SQL} AND author = ?2
                 ORDER BY COALESCE(sent_instant, ''), id LIMIT {limit}"
            ))?;
            let rows = stmt.query_map(params![source_instance_id, anchor_value], |row| {
                Ok(PersonLinkPreviewMessage {
                    id: row.get(0)?,
                    author: row.get(1)?,
                    sent_at: row.get(2)?,
                    snippet: preview_snippet(&row.get::<_, String>(3)?),
                })
            })?;
            let representative = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((matched.max(0) as u64, representative))
        }
        PersonAnchorKind::StableKey => {
            // The `instr` prefilter (case-folded both sides — stable
            // keys are always stored lowercased) keeps the scan bounded
            // to authors that even mention the address; the exact check
            // extracts and case-folds each author's address in Rust.
            let mut stmt = conn.prepare(&format!(
                "{REPRESENTATIVE_SQL} AND instr(lower(author), ?2) > 0
                 ORDER BY COALESCE(sent_instant, ''), id"
            ))?;
            let rows = stmt.query_map(params![source_instance_id, anchor_value], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            let mut matched = 0u64;
            let mut representative = Vec::new();
            for row in rows {
                let (id, author, sent_at, body) = row?;
                if author_stable_key(&author).as_deref() != Some(anchor_value) {
                    continue;
                }
                matched += 1;
                if representative.len() < limit {
                    representative.push(PersonLinkPreviewMessage {
                        id,
                        author,
                        sent_at,
                        snippet: preview_snippet(&body),
                    });
                }
            }
            Ok((matched, representative))
        }
    }
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
    /// `"connected"` after a successful sync, `"disconnected"`
    /// otherwise (including while a failure is recorded).
    pub state: String,
    /// Per-source exclusion (M2.3): coordinator rounds skip excluded
    /// sources while keeping their archive and cursors.
    pub excluded: bool,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    /// The last failed sync's stable class token (see
    /// [`crate::imap::SyncFailure::as_str`]); `None` after a success.
    pub last_error_class: Option<String>,
    /// The last failed sync's user-facing message; `None` after a success.
    pub last_error: Option<String>,
    /// When the last failed sync was recorded.
    pub last_error_at: Option<String>,
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
        last_error_class: row.get(10)?,
        last_error: row.get(11)?,
        last_error_at: row.get(12)?,
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

/// One source's channel presence for the accounts overview (see
/// [`Storage::account_channel_flags`]): whether the source's archived
/// messages include any `"chat"`-channel row (a chat-export import) and
/// any `"email"`-channel row (a mail-file import; IMAP rows carry their
/// folder name as the channel, so an IMAP source's medium is decided by
/// its kind).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountChannels {
    /// The durable source instance id (same ids as
    /// [`Storage::list_source_instances`]).
    pub source_id: String,
    /// At least one archived message carries channel `"chat"`.
    pub has_chat: bool,
    /// At least one archived message carries channel `"email"` — a
    /// mail-file import. (IMAP rows carry folder names as their channel;
    /// an IMAP source's medium is decided by kind.)
    pub has_email: bool,
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

/// Intersect plan-level sent-date bounds (already resolved to UTC
/// instant strings) with an optional validated Ask scope's bounds: the
/// later lower bound and the earlier upper bound win, so the plan can
/// only narrow the user's scope — never broaden it. `None` means the
/// intersection is empty (no message can satisfy both); callers must
/// then return zero results, never fall back to unscoped retrieval.
///
/// Both sides are RFC 3339 UTC strings in the fixed `%Y` format, so
/// lexicographic comparison is chronological.
fn intersected_bounds(
    from: Option<String>,
    to_exclusive: Option<String>,
    scope: Option<&crate::search2::ResolvedAskScope>,
) -> Option<(Option<String>, Option<String>)> {
    let Some(scope) = scope else {
        return Some((from, to_exclusive));
    };
    let from = match (from, scope.from_instant.clone()) {
        (Some(plan), Some(scope)) => Some(plan.max(scope)),
        (plan, scope) => plan.or(scope),
    };
    let to_exclusive = match (to_exclusive, scope.to_instant_exclusive.clone()) {
        (Some(plan), Some(scope)) => Some(plan.min(scope)),
        (plan, scope) => plan.or(scope),
    };
    match (&from, &to_exclusive) {
        (Some(from), Some(to)) if from >= to => None,
        _ => Some((from, to_exclusive)),
    }
}

/// The v2 predicate SQL and bind values of an optional Ask scope, ready
/// to append to a retrieval statement that binds the `m`/`si` aliases
/// (plus `c`, which the caller joins under a scope). Reuses
/// [`Storage::search_archive`]'s filter SQL verbatim.
fn scope_filter_sql(
    scope: Option<&crate::search2::ResolvedAskScope>,
) -> (String, Vec<rusqlite::types::Value>) {
    match scope {
        None => (String::new(), Vec::new()),
        Some(scope) => {
            let crate::search2::V2Filters { sql, values } = scope.v2_filters();
            (sql, values)
        }
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportBatchCounts {
    /// Messages newly stored by this batch.
    pub inserted: u64,
    /// Messages skipped because their identity already existed.
    pub skipped: u64,
    /// Messages whose media links were written or backfilled (inserts
    /// carrying a `media` array plus duplicates that gained entries).
    pub media_linked: u64,
    /// The completed import's receipt, written by the completing commit
    /// in the same transaction as the batch it summarizes (run-total
    /// new/present/failed counts, file outcomes, observed bounds).
    /// `None` on intermediate batches and on the registry-only commit
    /// that follows an already-completed import.
    pub receipt: Option<SourceReceipt>,
}

/// How many finished jobs the ledger keeps (migration 16). A write prunes
/// to this many newest rows; the panel renders at most its top 20 anyway.
pub const JOB_LEDGER_KEEP: usize = 50;

/// Upper bound for a saved-search name (Unicode characters), mirroring
/// the bounded text fields of the v2 request it stores.
pub const SAVED_SEARCH_MAX_NAME_CHARS: usize = 200;

/// One stored saved-search definition (migration 18): a name plus the
/// dynamic v2 request it re-executes. See [`Storage::upsert_saved_search`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSearch {
    /// Canonical row id (a UUID); stable across renames.
    pub id: String,
    pub name: String,
    /// The stored request, `limit`/`offset` stripped (pagination is
    /// per-execution, not part of the definition).
    pub request: SearchArchiveRequest,
    pub created_at: String,
    pub updated_at: String,
}

/// Map one `saved_searches` row to its [`SavedSearch`]. A row whose
/// request JSON cannot deserialize into the current request model
/// surfaces as an error rather than being silently dropped or widened.
fn saved_search_from_row(row: &Row<'_>) -> rusqlite::Result<SavedSearch> {
    let request_json: String = row.get(2)?;
    let request = serde_json::from_str(&request_json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(SavedSearch {
        id: row.get(0)?,
        name: row.get(1)?,
        request,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

/// One terminal job to append to the finished-jobs ledger (see
/// [`Storage::record_job_ledger`]). Owned: the entry crosses thread
/// boundaries into the write queue, away from the registry it came from.
#[derive(Debug, Clone)]
pub struct JobLedgerEntry {
    /// The finished run's process-local job id, provenance only.
    pub job_id: u64,
    pub kind: String,
    pub submitted_at: String,
    pub finished_at: String,
    /// `"succeeded"`, `"failed"`, or `"cancelled"`.
    pub state: String,
    /// One-line human summary of the result (the same result part the
    /// UI's describeJobState renders).
    pub summary: String,
    /// The serialized terminal [`crate::jobs::JobState`] JSON, so a
    /// restored row reconstructs the exact state it finished in.
    pub payload: String,
}

/// One stored ledger row, as returned by [`Storage::job_ledger_recent`]
/// (newest first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobLedgerRow {
    pub job_id: u64,
    pub kind: String,
    pub submitted_at: String,
    pub finished_at: String,
    /// `"succeeded"`, `"failed"`, or `"cancelled"`.
    pub state: String,
    pub summary: String,
    pub payload: String,
}

/// Which kind of completed run a [`SourceReceipt`] documents. The DB
/// CHECK constraint stores the lowercase spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceReceiptKind {
    /// A completed bounded import (a file/export import).
    Import,
    /// A completed account sync (e.g. one IMAP run).
    Sync,
}

impl SourceReceiptKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Sync => "sync",
        }
    }
}

/// One completed-run receipt to append to `source_receipts` (migration
/// 22), handed to [`Storage::record_source_receipt`]. The import and
/// sync completion seams build this from what their transaction
/// durably knows; counts that a seam cannot know stay zero (never
/// guessed), and the observed bounds stay `None` when the run observed
/// no dated messages.
#[derive(Debug, Clone)]
pub struct SourceReceiptRecord<'a> {
    pub source_instance_id: &'a str,
    pub kind: SourceReceiptKind,
    pub observed_min: Option<&'a str>,
    pub observed_max: Option<&'a str>,
    pub new_records: u64,
    pub present_records: u64,
    pub failed_records: u64,
    pub files_preserved: u64,
    pub files_missing: u64,
    /// Scope facts as a JSON object (`import_id`, run scope, …). Never
    /// message content.
    pub facts_json: &'a str,
}

/// One stored receipt row, as returned by
/// [`Storage::list_source_receipts`] (newest first). The exposed
/// observed range is a pair of independent nullable bounds — an
/// observed range is evidence of what the run saw, and this type has
/// no field that could claim complete provider coverage between them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReceipt {
    pub id: i64,
    pub source_instance_id: String,
    pub kind: SourceReceiptKind,
    pub completed_at: String,
    pub observed_min: Option<String>,
    pub observed_max: Option<String>,
    pub new_records: u64,
    pub present_records: u64,
    pub failed_records: u64,
    pub files_preserved: u64,
    pub files_missing: u64,
    pub facts_json: String,
}

/// Append one receipt row inside an open transaction — the shared body
/// of [`Storage::record_source_receipt`] and the completing-batch path
/// of [`Storage::commit_import_batch`], which writes its import receipt
/// in the same transaction as the batch it summarizes.
fn insert_source_receipt_in_tx(tx: &Connection, receipt: &SourceReceiptRecord<'_>) -> Result<()> {
    tx.execute(
        "INSERT INTO source_receipts
             (source_instance_id, kind, observed_min, observed_max,
              new_records, present_records, failed_records,
              files_preserved, files_missing, facts_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            receipt.source_instance_id,
            receipt.kind.as_str(),
            receipt.observed_min,
            receipt.observed_max,
            receipt.new_records.min(i64::MAX as u64) as i64,
            receipt.present_records.min(i64::MAX as u64) as i64,
            receipt.failed_records.min(i64::MAX as u64) as i64,
            receipt.files_preserved.min(i64::MAX as u64) as i64,
            receipt.files_missing.min(i64::MAX as u64) as i64,
            receipt.facts_json,
        ],
    )?;
    Ok(())
}

/// Map one `source_receipts` row to its [`SourceReceipt`] (column order:
/// `id, source_instance_id, kind, completed_at, observed_min,
/// observed_max, new_records, present_records, failed_records,
/// files_preserved, files_missing, facts_json`). A `kind` outside the
/// enum (written by some other tool) surfaces as an error rather than
/// being silently re-labeled.
fn source_receipt_from_row(row: &Row<'_>) -> rusqlite::Result<SourceReceipt> {
    let kind: String = row.get(2)?;
    let kind = match kind.as_str() {
        "import" => SourceReceiptKind::Import,
        "sync" => SourceReceiptKind::Sync,
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                format!("unknown source receipt kind: {other}").into(),
            ))
        }
    };
    Ok(SourceReceipt {
        id: row.get(0)?,
        source_instance_id: row.get(1)?,
        kind,
        completed_at: row.get(3)?,
        observed_min: row.get(4)?,
        observed_max: row.get(5)?,
        new_records: row.get::<_, i64>(6)?.max(0) as u64,
        present_records: row.get::<_, i64>(7)?.max(0) as u64,
        failed_records: row.get::<_, i64>(8)?.max(0) as u64,
        files_preserved: row.get::<_, i64>(9)?.max(0) as u64,
        files_missing: row.get::<_, i64>(10)?.max(0) as u64,
        facts_json: row.get(11)?,
    })
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

/// One retained original object (A3b, `original_objects`): the raw
/// RFC 5322 bytes of a message, stored by digest in the object store.
/// See [`Storage::original_for_message`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginalObject {
    /// SHA-256 of the retained bytes — the object-store digest.
    pub digest: String,
    /// Size of the retained bytes.
    pub size: u64,
    /// What the bytes are (`"rfc5322"`).
    pub kind: String,
    /// When the bytes were first retained.
    pub retained_at: String,
}

/// One archived message that references a media digest (the resolution
/// of an integrity finding to the messages it affects, see
/// [`Storage::media_digest_references`]): the id every reading surface
/// (`message_detail_v2`, conversation links) keys on, plus the
/// conversation/date context to show beside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaMessageReference {
    /// `archive_messages.id` — stable, and the key the detail and
    /// context IPC surfaces accept.
    pub message_id: i64,
    /// The conversation's display label ('' when the row never resolved
    /// to one).
    pub conversation: String,
    /// The message's legacy chat grouping as stored.
    pub chat_id: String,
    /// The author as stored ('' when unknown).
    pub author: String,
    /// The resolved UTC instant when known, else the original timestamp
    /// text; '' when neither exists.
    pub date: String,
    /// The media entry's original display name (e.g. `photo.jpg`).
    pub name: String,
}

/// The affected-message page for one digest (see
/// [`Storage::media_digest_references`]): every archived message whose
/// `raw_json` media entries reference the digest, in bounded pages with
/// the full count for pagination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDigestReferences {
    pub sha256: String,
    /// Total messages referencing the digest, ignoring the page window.
    pub total: u64,
    pub offset: u32,
    pub limit: u32,
    /// The page, oldest message id first (one row per message; a digest
    /// a single message references twice names it once).
    pub references: Vec<MediaMessageReference>,
}

/// Default/maximum page size and maximum offset for the paged unlinked
/// read (mirrors the search2 paging bounds).
pub const UNLINKED_DEFAULT_LIMIT: u32 = 50;
pub const UNLINKED_MAX_LIMIT: u32 = 200;
pub const UNLINKED_MAX_OFFSET: u32 = 1_000_000;

/// Sort order for [`UnlinkedMediaPageRequest`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnlinkedMediaSort {
    /// `first_seen, sha256` — oldest first, the order
    /// [`Storage::unlinked_media_objects`] returns; same-second ties
    /// break deterministically by digest.
    #[default]
    FirstSeen,
    /// `sha256` ascending — a stable order independent of import time.
    Digest,
}

/// One paged request over the unlinked media-object registry (see
/// [`Storage::unlinked_media_page`]). All filters are conjunctive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlinkedMediaPageRequest {
    /// Case-insensitive (Unicode-folded) substring over the original
    /// name and the kind; empty = no filter.
    pub search: String,
    /// Restrict to objects first registered by one bounded import run
    /// (the stable `import_id` the registry records); empty/`None` = all
    /// imports.
    pub import_id: Option<String>,
    pub sort: UnlinkedMediaSort,
    /// Default 50, clamped to `1..=200`.
    pub limit: Option<u32>,
    /// Clamped to `0..=1_000_000`.
    pub offset: Option<u32>,
}

/// One row of the paged unlinked-files view: a registry row's provenance
/// plus what the object store says about the bytes right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlinkedMediaRow {
    pub sha256: String,
    pub size: u64,
    pub name: String,
    pub kind: String,
    /// How the object entered the vault (`"unlinked"`).
    pub origin: String,
    /// The bounded import run that first registered the digest — the
    /// recorded first-seen provenance, never a guess: a digest shared
    /// across imports keeps the registration the first import wrote.
    pub import_id: String,
    pub first_seen: String,
    /// Whether the digest's bytes are present in the media store.
    /// `false` until [`UnlinkedMediaPage::stamp_availability`] runs —
    /// the SQL read never touches disk.
    pub available: bool,
    /// Store-relative object path (`media/<sha256>.<ext>`) when the
    /// bytes are present — the reference `media_export` /
    /// `media_open_external` act on. `None` = unstamped or missing.
    pub path: Option<String>,
}

/// One page of the unlinked-files view: bounded rows, the exact total
/// from the same read snapshot, and that snapshot's `archive_revision`
/// (the A07 staleness signal, like [`crate::search2::SearchArchivePage`]
/// — an import that lands between two pages bumps it, and the UI offers
/// a refresh instead of silently restarting pagination).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlinkedMediaPage {
    pub objects: Vec<UnlinkedMediaRow>,
    pub total: u64,
    pub limit: u32,
    pub offset: u32,
    pub archive_revision: u64,
}

impl UnlinkedMediaPage {
    /// Stamp each row's availability from the object store's presence
    /// check: one store enumeration answers every digest on the page
    /// (bounded by the page size — the store is not re-scanned per row,
    /// and the full registry is never loaded). Present bytes set
    /// `available` and the store-relative `path`; missing bytes leave
    /// `available: false` with no path — an honest unavailable state,
    /// never a guessed file name.
    pub fn stamp_availability(&mut self, store: &crate::object_store::ObjectStore) {
        let present: std::collections::HashMap<String, String> = store
            .enumerate()
            .into_iter()
            .map(|object| (object.sha256, object.path))
            .collect();
        for row in &mut self.objects {
            match present.get(&row.sha256) {
                Some(path) => {
                    row.available = true;
                    row.path = Some(path.clone());
                }
                None => {
                    row.available = false;
                    row.path = None;
                }
            }
        }
    }
}

/// A vault-local collection (migration 19): a user-named set of
/// references to canonical messages. Organization metadata only — the
/// name, timestamps, and membership count; source content, provenance,
/// and identity live on the referenced rows and are never copied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: String,
    pub name: String,
    pub created_at: String,
    /// Last create/rename/membership change.
    pub updated_at: String,
    /// Membership rows, including any whose referenced message is
    /// absent (they surface as unavailable, never disappear).
    pub message_count: u64,
}

/// One collection's full read ([`Storage::collection_detail`]): the
/// collection row plus its members in add order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionDetail {
    pub collection: Collection,
    pub members: Vec<CollectionMember>,
}

/// One membership row of a collection detail: what the vault can
/// honestly say about the referenced canonical message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionMember {
    /// The stable `archive_messages` rowid the membership references —
    /// the handle the UI deep-links back to the original message and
    /// its conversation.
    pub message_id: i64,
    /// Whether the referenced canonical message still exists. An
    /// unavailable row carries no provenance and previews no content:
    /// the reference itself is shown, never fabricated.
    pub available: bool,
    pub added_at: String,
    /// Provenance joins, present only when `available` (the same joins
    /// the v2 message detail performs).
    pub source_id: Option<String>,
    pub source_label: Option<String>,
    pub conversation_id: Option<String>,
    pub conversation_label: Option<String>,
    pub author: Option<String>,
    /// Subject line (`None` when the message has none — chat sources).
    pub subject: Option<String>,
    /// Resolved UTC instant (`None` when unknown).
    pub sent_at: Option<String>,
}

/// A collection name, trimmed; empty after trimming is refused.
fn validate_collection_name(name: &str) -> Result<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidCollection(
            "collection name must not be empty".into(),
        ));
    }
    Ok(trimmed)
}

/// One collection row with its live membership count — shared by
/// [`Storage::create_collection`] and [`Storage::collection_detail`].
fn collection_row(conn: &Connection, id: &str) -> Result<Option<Collection>> {
    let row = conn
        .query_row(
            "SELECT id, name, created_at, updated_at,
                    (SELECT COUNT(*) FROM collection_members
                     WHERE collection_id = collections.id)
             FROM collections WHERE id = ?1",
            params![id],
            |row| {
                Ok(Collection {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                    message_count: row.get::<_, i64>(4)?.max(0) as u64,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Refuse an unknown collection id inside a write transaction.
fn ensure_collection_exists(conn: &Connection, id: &str) -> Result<()> {
    let present: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM collections WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?;
    if present.is_none() {
        return Err(Error::InvalidCollection(format!("no collection {id}")));
    }
    Ok(())
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

/// Decode one reminder timestamp column (migration 25): RFC 3339,
/// NULL for "not set". A stored instant this build cannot parse fails
/// loudly instead of silently disarming a reminder — the scheduling
/// state is honest or it is nothing.
fn reminder_instant(
    text: Option<String>,
) -> std::result::Result<Option<DateTime<Utc>>, rusqlite::Error> {
    match text {
        None => Ok(None),
        Some(t) => DateTime::parse_from_rfc3339(&t)
            .map(|d| Some(d.with_timezone(&Utc)))
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            }),
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

    fn seeded_links() -> Storage {
        let s = Storage::open_in_memory().unwrap();
        let rows = [
            ("ana@example.com", "see https://example.com/a now"),
            ("Ana Example <ana@example.com>", "no links here, just words"),
            (
                "bo@example.com",
                "docs at https://example.com/b and HTTP://example.com/c",
            ),
            ("carol@example.com", "nothing to see"),
        ];
        for (i, (author, body)) in rows.iter().enumerate() {
            s.insert_message(&crate::message::NewMessage::new(
                "test",
                format!("m{i}"),
                *author,
                "2026-01-01T00:00:00Z",
                *body,
            ))
            .unwrap();
        }
        s
    }

    #[test]
    fn search_filtered_from_is_case_insensitive_substring() {
        let s = seeded_links();
        let out = s
            .search_filtered("example", None, None, Some("ANA@"), false, 50)
            .unwrap();
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out.iter().all(|m| m.author.to_lowercase().contains("ana@")));
    }

    #[test]
    fn search_filtered_require_link_keeps_only_url_bodies() {
        let s = seeded_links();
        let out = s
            .search_filtered("example", None, None, None, true, 50)
            .unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|m| m.body.contains("://")));
    }

    #[test]
    fn search_filtered_combines_from_and_require_link() {
        let s = seeded_links();
        let out = s
            .search_filtered("example", None, None, Some("bo@"), true, 50)
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].author, "bo@example.com");
    }

    #[test]
    fn vector_scan_applies_the_same_filters() {
        let s = seeded_links();
        // Embed two messages (ids 1 and 4) with identical normalized vectors:
        // id 1 is ana's (has a URL), id 4 is carol's (no URL, not ana).
        let model = "test-embed";
        s.embed_messages_batch(model, 2, &[(1, vec![1.0, 0.0]), (4, vec![1.0, 0.0])])
            .unwrap();
        // Unfiltered: both ids are candidates.
        let all = s
            .vector_scan(model, 2, &[1.0, 0.0], None, None, None, false, 10)
            .unwrap();
        assert_eq!(all.len(), 2, "{all:?}");
        // Sender filter: only message 1 (ana) survives.
        let filtered = s
            .vector_scan(model, 2, &[1.0, 0.0], None, None, Some("ana"), false, 10)
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].message_id, 1);
        // Link filter: only message 1 (has a URL) survives.
        let linked = s
            .vector_scan(model, 2, &[1.0, 0.0], None, None, None, true, 10)
            .unwrap();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].message_id, 1);
    }

    #[test]
    fn scoped_legs_apply_import_and_honesty_filters_identically() {
        use crate::search2::{validate_ask_scope, AskScope};
        use crate::vector::Embedder as _;
        use std::collections::HashSet;

        let s = Storage::open_in_memory().unwrap();
        // imp-1: one message with a stored media reference, one plain;
        // imp-2: one plain message plus one undated message whose media
        // reference is a missing marker (assumed time + missing bytes).
        let mut with_media = crate::message::NewMessage::new(
            "legacy-scope",
            "m1",
            "ana",
            "2026-05-01T00:00:00Z",
            "alpha media body",
        );
        with_media.raw_json = r#"{"media":[{"sha256":"a","path":"media/a.jpg","name":"a.jpg","kind":"image","size":1}]}"#.to_string();
        let plain = crate::message::NewMessage::new(
            "legacy-scope",
            "m2",
            "ana",
            "2026-05-02T00:00:00Z",
            "alpha plain body",
        );
        let other = crate::message::NewMessage::new(
            "legacy-scope",
            "m3",
            "bo",
            "2026-05-03T00:00:00Z",
            "alpha other body",
        );
        let mut undated_missing =
            crate::message::NewMessage::new("legacy-scope", "m4", "bo", "", "alpha undated body");
        undated_missing.raw_json =
            r#"{"media":[{"name":"gone.pdf","kind":"document","missing":true,"reason":"not-in-export"}]}"#
                .to_string();
        s.commit_import_batch(
            "imp-1",
            "legacy-scope",
            &[with_media, plain],
            &[],
            &[],
            2,
            true,
        )
        .unwrap();
        s.commit_import_batch(
            "imp-2",
            "legacy-scope",
            &[other, undated_missing],
            &[],
            &[],
            2,
            true,
        )
        .unwrap();

        // Embed every message with the query text so cosine is maximal
        // and vector-leg membership reflects only the filters.
        let embedder = crate::vector::HashEmbedder;
        let query_text = "alpha shared body";
        let ids: Vec<(i64, Vec<f32>)> = s
            .connection()
            .prepare("SELECT id FROM archive_messages ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .map(|id| (id, embedder.embed(query_text)))
            .collect();
        s.embed_messages_batch(embedder.id(), embedder.dim(), &ids)
            .unwrap();

        let scopes = [
            (
                "import",
                AskScope {
                    import_job_id: Some("imp-1".into()),
                    ..AskScope::default()
                },
            ),
            (
                "attachments",
                AskScope {
                    has_attachment: Some(true),
                    ..AskScope::default()
                },
            ),
            (
                "no attachments",
                AskScope {
                    has_attachment: Some(false),
                    ..AskScope::default()
                },
            ),
            (
                "assumed times",
                AskScope {
                    time_assumed: Some(true),
                    ..AskScope::default()
                },
            ),
            (
                "exact times",
                AskScope {
                    time_assumed: Some(false),
                    ..AskScope::default()
                },
            ),
            (
                "files missing",
                AskScope {
                    files_missing: Some(true),
                    ..AskScope::default()
                },
            ),
            (
                "import ∩ attachments",
                AskScope {
                    import_job_id: Some("imp-1".into()),
                    has_attachment: Some(true),
                    ..AskScope::default()
                },
            ),
        ];
        for (name, scope) in scopes {
            let resolved = validate_ask_scope(&s, &scope).unwrap();
            let fts: HashSet<i64> = s
                .search_filtered_scoped("alpha", None, None, None, false, Some(&resolved), 50)
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect();
            let vector: HashSet<i64> = s
                .vector_scan_scoped(
                    embedder.id(),
                    embedder.dim(),
                    &embedder.embed(query_text),
                    None,
                    None,
                    None,
                    false,
                    Some(&resolved),
                    50,
                )
                .unwrap()
                .into_iter()
                .map(|hit| hit.message_id)
                .collect();
            // Ground truth: plain v2 search with the same filters.
            let request = crate::search2::SearchArchiveRequest {
                text: "alpha".into(),
                import_job_id: scope.import_job_id.clone(),
                has_attachment: scope.has_attachment,
                time_assumed: scope.time_assumed,
                grouping_unverified: scope.grouping_unverified,
                files_missing: scope.files_missing,
                ..Default::default()
            };
            let archive: HashSet<i64> = s
                .search_archive(&request)
                .unwrap()
                .hits
                .into_iter()
                .map(|hit| hit.id)
                .collect();
            // The legacy import path records every fixture stamp as
            // assumed, so the "exact times" scope legitimately matches
            // nothing — the legs must still agree on that (empty == empty).
            let may_be_empty = name == "exact times";
            assert!(
                may_be_empty || !archive.is_empty(),
                "fixture must match for {name}"
            );
            assert_eq!(fts, archive, "FTS leg diverged for {name}");
            assert_eq!(vector, archive, "vector leg diverged for {name}");
        }
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

    /// Register one unlinked object through the real batch-commit path
    /// (the `INSERT OR IGNORE` semantics the paged query must not
    /// change), optionally forcing its `first_seen` for ordering tests.
    fn register_unlinked(
        s: &Storage,
        import_id: &str,
        sha: &str,
        name: &str,
        kind: &str,
        size: u64,
        first_seen: Option<&str>,
    ) {
        let objects = vec![crate::media::UnlinkedObject {
            sha256: sha.to_string(),
            size,
            name: name.into(),
            kind: kind.into(),
        }];
        s.commit_import_batch(import_id, "telegram", &[], &[], &objects, 0, true)
            .unwrap();
        if let Some(seen) = first_seen {
            s.conn
                .execute(
                    "UPDATE media_objects SET first_seen = ?1 WHERE sha256 = ?2",
                    params![seen, sha],
                )
                .unwrap();
        }
    }

    #[test]
    fn unlinked_media_page_clamps_limits_and_offsets() {
        let s = Storage::open_in_memory().unwrap();
        for (i, (sha, name)) in [
            ("a", "orphan.png"),
            ("b", "extra.txt"),
            ("c", "notes.html"),
            ("d", "beep.wav"),
            ("e", "rules.json"),
        ]
        .into_iter()
        .enumerate()
        {
            register_unlinked(
                &s,
                "imp-a",
                &sha.repeat(64),
                name,
                "document",
                10 + i as u64,
                None,
            );
        }

        // Defaults: limit 50, offset 0, one page under the total.
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest::default())
            .unwrap();
        assert_eq!(
            (page.objects.len(), page.total, page.limit, page.offset),
            (5, 5, 50, 0)
        );
        assert_eq!(page.archive_revision, s.archive_revision().unwrap());

        // A zero/absurd limit clamps to the 1..=200 window, and the
        // clamped values are reported back (the caller paginates on
        // what ran, not what was asked).
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest {
                limit: Some(0),
                ..UnlinkedMediaPageRequest::default()
            })
            .unwrap();
        assert_eq!((page.objects.len(), page.limit), (1, 1));
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest {
                limit: Some(10_000),
                ..UnlinkedMediaPageRequest::default()
            })
            .unwrap();
        assert_eq!((page.objects.len(), page.limit), (5, 200));

        // Offsets walk the same order; past the end the page is empty
        // but the total still reports the whole registry.
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest {
                limit: Some(2),
                offset: Some(4),
                ..UnlinkedMediaPageRequest::default()
            })
            .unwrap();
        assert_eq!((page.objects.len(), page.total, page.offset), (1, 5, 4));
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest {
                offset: Some(500),
                ..UnlinkedMediaPageRequest::default()
            })
            .unwrap();
        assert_eq!((page.objects.len(), page.total, page.offset), (0, 5, 500));
    }

    #[test]
    fn unlinked_media_page_searches_name_and_kind_folded() {
        let s = Storage::open_in_memory().unwrap();
        register_unlinked(&s, "imp-a", &"a".repeat(64), "Orphan.PNG", "image", 1, None);
        register_unlinked(
            &s,
            "imp-a",
            &"b".repeat(64),
            "plan.pdf",
            "document",
            2,
            None,
        );
        register_unlinked(
            &s,
            "imp-a",
            &"c".repeat(64),
            "50%_off.txt",
            "document",
            3,
            None,
        );
        register_unlinked(&s, "imp-a", &"d".repeat(64), "beep.wav", "audio", 4, None);

        let page = |search: &str| {
            s.unlinked_media_page(&UnlinkedMediaPageRequest {
                search: search.to_string(),
                ..UnlinkedMediaPageRequest::default()
            })
            .unwrap()
        };

        // Name and kind, case-insensitively (ASCII folds, Unicode via
        // the shared fold() SQL function).
        let hit = page("orphan");
        assert_eq!((hit.objects.len(), hit.total), (1, 1));
        assert_eq!(hit.objects[0].name, "Orphan.PNG");
        let docs = page("document");
        assert_eq!(docs.total, 2, "kind is searchable");
        let aud = page("AUDIO");
        assert_eq!(aud.total, 1);
        // LIKE wildcards in the search stay literal: "%" matches the
        // one name that contains it, not everything.
        let pct = page("%");
        assert_eq!(pct.total, 1);
        assert_eq!(pct.objects[0].name, "50%_off.txt");
        // No match is an empty page with an honest zero total.
        let none = page("no-such-file");
        assert_eq!((none.objects.len(), none.total), (0, 0));
    }

    #[test]
    fn unlinked_media_page_filters_by_import() {
        let s = Storage::open_in_memory().unwrap();
        register_unlinked(&s, "imp-a", &"a".repeat(64), "orphan.png", "image", 1, None);
        register_unlinked(
            &s,
            "imp-a",
            &"b".repeat(64),
            "extra.txt",
            "document",
            2,
            None,
        );
        register_unlinked(
            &s,
            "imp-b",
            &"c".repeat(64),
            "notes.html",
            "document",
            3,
            None,
        );

        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest {
                import_id: Some("imp-a".into()),
                ..UnlinkedMediaPageRequest::default()
            })
            .unwrap();
        assert_eq!(page.total, 2);
        assert!(page.objects.iter().all(|row| row.import_id == "imp-a"));

        // Conjunctive with search, and an unknown import is empty (an
        // honest zero, never a fallback to every import).
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest {
                import_id: Some("imp-a".into()),
                search: "orphan".into(),
                ..UnlinkedMediaPageRequest::default()
            })
            .unwrap();
        assert_eq!((page.objects.len(), page.total), (1, 1));
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest {
                import_id: Some("imp-missing".into()),
                ..UnlinkedMediaPageRequest::default()
            })
            .unwrap();
        assert_eq!((page.objects.len(), page.total), (0, 0));
    }

    #[test]
    fn unlinked_media_page_orders_by_first_seen_then_digest_or_by_digest() {
        let s = Storage::open_in_memory().unwrap();
        // Digest order (a < b < c) is the inverse of first-seen order,
        // so each sort is proven by disagreeing with the other.
        register_unlinked(
            &s,
            "imp-a",
            &"c".repeat(64),
            "first.png",
            "image",
            1,
            Some("2026-01-01 00:00:00"),
        );
        register_unlinked(
            &s,
            "imp-a",
            &"b".repeat(64),
            "second.png",
            "image",
            2,
            Some("2026-01-02 00:00:00"),
        );
        register_unlinked(
            &s,
            "imp-a",
            &"a".repeat(64),
            "third.png",
            "image",
            3,
            Some("2026-01-03 00:00:00"),
        );

        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest::default())
            .unwrap();
        let by_seen: Vec<&str> = page.objects.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(by_seen, ["first.png", "second.png", "third.png"]);

        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest {
                sort: UnlinkedMediaSort::Digest,
                ..UnlinkedMediaPageRequest::default()
            })
            .unwrap();
        let by_digest: Vec<&str> = page.objects.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(by_digest, ["third.png", "second.png", "first.png"]);

        // Same-second first_seen ties break deterministically by digest
        // (the full-registry read's rule).
        s.conn
            .execute(
                "UPDATE media_objects SET first_seen = '2026-01-03 00:00:00'",
                [],
            )
            .unwrap();
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest::default())
            .unwrap();
        let tied: Vec<String> = page.objects.iter().map(|r| r.sha256.clone()).collect();
        let mut sorted = tied.clone();
        sorted.sort();
        assert_eq!(tied, sorted, "digest is the tiebreak");
    }

    #[test]
    fn unlinked_media_page_reports_empty_registry_and_reimport_rows_once() {
        let s = Storage::open_in_memory().unwrap();
        // Empty registry: an empty page, zero total, current revision.
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest::default())
            .unwrap();
        assert_eq!((page.objects.len(), page.total), (0, 0));
        assert_eq!(page.archive_revision, s.archive_revision().unwrap());

        // Re-registering the same export adds no row the query can see:
        // the registry's INSERT OR IGNORE stays the only write path.
        register_unlinked(&s, "imp-a", &"a".repeat(64), "orphan.png", "image", 7, None);
        register_unlinked(&s, "imp-a", &"a".repeat(64), "orphan.png", "image", 7, None);
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest::default())
            .unwrap();
        assert_eq!((page.objects.len(), page.total), (1, 1));
        // A second import registering the same digest keeps the first
        // registration's provenance — first-seen is never rewritten.
        register_unlinked(&s, "imp-b", &"a".repeat(64), "orphan.png", "image", 7, None);
        let page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest {
                import_id: Some("imp-b".into()),
                ..UnlinkedMediaPageRequest::default()
            })
            .unwrap();
        assert_eq!(page.total, 0, "shared digest keeps its first-seen import");
    }

    #[test]
    fn unlinked_media_page_availability_follows_the_store_presence_check() {
        let s = Storage::open_in_memory().unwrap();

        // No store at all: every row is honestly unavailable, with no
        // path to act on.
        register_unlinked(&s, "imp-a", &"a".repeat(64), "orphan.png", "image", 3, None);
        let mut page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest::default())
            .unwrap();
        assert!(page
            .objects
            .iter()
            .all(|row| !row.available && row.path.is_none()));
        page.stamp_availability(&crate::object_store::ObjectStore::new(
            std::path::Path::new("/nonexistent-mcv-unlinked"),
        ));
        assert!(page
            .objects
            .iter()
            .all(|row| !row.available && row.path.is_none()));

        // A store holding one of the registered digests: that row turns
        // available with its store-relative path, the other stays
        // honestly unavailable.
        let dir = tempfile::tempdir().unwrap();
        let store = crate::object_store::ObjectStore::new(dir.path());
        let mut budget = crate::object_store::Capacity::default();
        let src = dir.path().join("bytes.png");
        std::fs::write(&src, b"orphan-bytes").unwrap();
        let staged = store.stage(&src, "png", &mut budget).unwrap();
        let present_sha = staged.sha256.clone();
        store.promote(staged, &mut budget).unwrap();
        let stored = store.enumerate();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].sha256, present_sha);

        register_unlinked(&s, "imp-b", &present_sha, "present.png", "image", 12, None);
        let mut page = s
            .unlinked_media_page(&UnlinkedMediaPageRequest::default())
            .unwrap();
        assert_eq!(page.total, 2);
        page.stamp_availability(&store);
        for row in &page.objects {
            if row.sha256 == present_sha {
                assert!(row.available, "present digest is available");
                assert_eq!(row.path.as_deref(), Some(stored[0].path.as_str()));
            } else {
                assert!(!row.available, "absent digest stays unavailable");
                assert_eq!(row.path, None);
            }
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

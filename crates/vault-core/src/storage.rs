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

use std::path::Path;

use chrono::{Datelike, Days, NaiveDate};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::message::{InsertOutcome, Message, NewMessage, SourceCount, Stats};
use crate::query::{
    ChatContext, SearchHit, SearchPage, SearchQuery, SNIPPET_CLOSE_MARK, SNIPPET_OPEN_MARK,
};
use crate::stats::{ArchiveStats, AuthorCount, ChatCount, DayCount};
use crate::vector::VectorHit;
use crate::{Error, Result};

/// Ordered schema migrations; each entry runs once, inside a transaction.
///
/// Versions 1–2 are the frozen legacy schema; version 3 adds local
/// embeddings over the legacy table; version 4 adds the canonical store and
/// copies the legacy rows into it. Never edit an applied migration — append
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
];

/// Handle to the vault database.
///
/// `rusqlite`'s `Connection` is `Send` but not `Sync`; wrap it in a `Mutex`
/// when sharing across threads (the Tauri app does exactly that).
#[derive(Debug)]
pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Open (and migrate) a database file, creating it when missing.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Open a private in-memory database (used by tests and demos).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        // journal_mode returns a row (the new mode), so read-and-ignore it
        // via query_row; synchronous/foreign_keys return no rows and need
        // pragma_update.
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        register_fold_function(&conn)?;
        register_canonical_functions(&conn)?;
        migrate(&conn)?;
        Ok(Self { conn })
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
    pub fn insert_message(&self, msg: &NewMessage) -> Result<InsertOutcome> {
        let tx = self.conn.unchecked_transaction()?;
        let instance = resolve_legacy_instance(&tx, &msg.source)?;

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
                     (message_id, scope, provider_item_id)
                 VALUES (?1, 'import', ?2)",
                params![id, msg.external_id],
            )?;
            tx.commit()?;
            return Ok(InsertOutcome::Duplicate(id));
        }

        let conversation = if msg.chat_id.is_empty() {
            None
        } else {
            Some(resolve_legacy_conversation(&tx, &instance, msg)?)
        };
        let sent_instant = if msg.sent_at.is_empty() {
            None
        } else {
            Some(msg.sent_at.as_str())
        };
        let confidence = if msg.sent_at.is_empty() {
            "missing"
        } else {
            "legacy_assumed"
        };
        let id: i64 = tx.query_row(
            r#"
            INSERT INTO archive_messages
                (source_instance_id, conversation_id, external_id, channel, chat_id,
                 author, subject, body, raw_json,
                 sent_instant, original_timestamp, timezone, time_confidence, content_hash)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', ?7, ?8, ?9, ?10, '', ?11, ?12)
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
                msg.sent_at,
                confidence,
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
            "INSERT INTO message_occurrences (message_id, scope, provider_item_id)
             VALUES (?1, 'import', ?2)",
            params![id, msg.external_id],
        )?;
        tx.commit()?;
        Ok(InsertOutcome::Inserted(id))
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
            SELECT m.id, COALESCE(si.legacy_namespace, si.id), m.external_id,
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
                "SELECT m.id, COALESCE(si.legacy_namespace, si.id), m.external_id, \
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
                "SELECT m.id, COALESCE(si.legacy_namespace, si.id), m.external_id, \
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

    /// A message plus its adjacent messages in the same conversation.
    ///
    /// `before`/`after` each hold up to `radius` messages, oldest → newest.
    /// Messages without a conversation (empty legacy `chat_id`) have no
    /// thread peers. Ties and unknown instants order by id, matching the
    /// legacy `sent_at`-text ordering exactly (COALESCE maps the unknown
    /// NULL instant to the legacy empty-string position).
    pub fn chat_context(&self, id: i64, radius: u32) -> Result<Option<ChatContext>> {
        let radius = radius.clamp(1, crate::query::MAX_LIMIT) as i64;
        let Some(message) = self.get(id)? else {
            return Ok(None);
        };
        if message.chat_id.is_empty() {
            return Ok(Some(ChatContext {
                before: Vec::new(),
                message,
                after: Vec::new(),
            }));
        }

        let mut before_stmt = self.conn.prepare(
            "SELECT m.id, COALESCE(si.legacy_namespace, si.id), m.external_id, \
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
            "SELECT m.id, COALESCE(si.legacy_namespace, si.id), m.external_id, \
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
            SELECT m.id, COALESCE(si.legacy_namespace, si.id), m.external_id,
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

    /// Archive counts: total and per source.
    ///
    /// Reads the trigger-maintained `stats_source` counters (migration 6)
    /// instead of grouping over every message row, so this stays instant
    /// on million-message archives; the numbers are identical by
    /// construction because every write path maintains the counters.
    pub fn stats(&self) -> Result<Stats> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(si.legacy_namespace, si.id) AS label, s.n \
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

        let mut source_stmt = self
            .conn
            .prepare("SELECT source, n FROM stats_source WHERE n > 0 ORDER BY n DESC, source")?;
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
            "SELECT COALESCE(si.legacy_namespace, si.id) AS label, c.chat_id, c.n \
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
            "SELECT m.id, COALESCE(si.legacy_namespace, si.id), m.external_id,
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
        if entries.is_empty() {
            return Ok(false);
        }
        let existing_raw: Option<String> = self
            .conn
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
        let known: std::collections::HashSet<String> = array
            .iter()
            .filter_map(|e| {
                e.get("sha256")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let mut changed = false;
        for entry in entries {
            let sha = entry.get("sha256").and_then(serde_json::Value::as_str);
            if sha.is_none_or(str::is_empty) || sha.is_some_and(|s| known.contains(s)) {
                continue;
            }
            array.push(entry.clone());
            changed = true;
        }
        if changed {
            self.conn.execute(
                "UPDATE archive_messages SET raw_json = ?1 WHERE id = ?2",
                params![raw.to_string(), id],
            )?;
        }
        Ok(changed)
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
/// creating a legacy-unresolved one when the chat is new. The stable
/// provider keys and email reply graph arrive with the conversation-key
/// slice; the legacy chat key is the grouping M1.1 actually has.
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
            sql.push_str(&format!(" AND si.legacy_namespace IN ({placeholders})"));
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
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
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

/// Apply pending migrations, each in its own transaction, exactly once.
fn migrate(conn: &Connection) -> Result<()> {
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
    for &(version, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > current) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version) VALUES (?1)",
            params![version],
        )?;
        tx.commit()?;
    }
    Ok(())
}

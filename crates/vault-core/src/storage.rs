//! SQLite storage: schema migrations, idempotent inserts, and FTS5 search.
//!
//! The database bundles SQLite (via `rusqlite`'s `bundled` feature), so no
//! system SQLite is required. FTS5 is enabled in the bundled build.

use std::path::Path;

use chrono::{Duration, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::message::{InsertOutcome, Message, NewMessage, SourceCount, Stats};
use crate::{Error, Result};

/// Ordered schema migrations; each entry runs once, inside a transaction.
const MIGRATIONS: &[(i64, &str)] = &[(
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
)];

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
        migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Insert one message, idempotently on `(source, external_id)`.
    ///
    /// Inserting a message whose dedup key already exists is not an error:
    /// the existing row is left untouched and `InsertOutcome::Duplicate` is
    /// returned with its id.
    pub fn insert_message(&self, msg: &NewMessage) -> Result<InsertOutcome> {
        let inserted_id = self
            .conn
            .query_row(
                r#"
                INSERT INTO messages
                    (source, external_id, channel, chat_id, author, sent_at, body, raw_json)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT (source, external_id) DO NOTHING
                RETURNING id
                "#,
                params![
                    msg.source,
                    msg.external_id,
                    msg.channel,
                    msg.chat_id,
                    msg.author,
                    msg.sent_at,
                    msg.body,
                    msg.raw_json,
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        match inserted_id {
            Some(id) => Ok(InsertOutcome::Inserted(id)),
            None => {
                let existing = self.conn.query_row(
                    "SELECT id FROM messages WHERE source = ?1 AND external_id = ?2",
                    params![msg.source, msg.external_id],
                    |row| row.get::<_, i64>(0),
                )?;
                Ok(InsertOutcome::Duplicate(existing))
            }
        }
    }

    /// Full-text search over `body` and `author` using FTS5.
    ///
    /// `match_query` is a raw FTS5 MATCH expression (see
    /// [`crate::search::build_match_query`] for building one safely from free
    /// text). Results are ordered by FTS5 relevance (`rank`).
    pub fn search(&self, match_query: &str, limit: u32) -> Result<Vec<Message>> {
        self.search_filtered(match_query, None, None, limit)
    }

    /// Full-text search with optional inclusive sent-date bounds.
    ///
    /// `date_from`/`date_to` are `YYYY-MM-DD` strings; anything else is an
    /// error. `date_to` is inclusive (messages sent any time that day count).
    /// Messages with an unknown (`empty`) `sent_at` are excluded while a
    /// bound is active — they cannot be placed in time. The MATCH expression
    /// is a bound parameter throughout, so it can never spill into SQL text.
    pub fn search_filtered(
        &self,
        match_query: &str,
        date_from: Option<&str>,
        date_to: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Message>> {
        let from = parse_date_bound(date_from, "date_from")?.map(|d| d.to_string());
        // Inclusive date_to becomes an exclusive upper bound at midnight of
        // the next day; RFC 3339 UTC strings compare correctly as text.
        let to_exclusive =
            parse_date_bound(date_to, "date_to")?.map(|day| (day + Duration::days(1)).to_string());
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.id, m.source, m.external_id, m.channel, m.chat_id,
                   m.author, m.sent_at, m.body, m.raw_json
            FROM messages_fts
            JOIN messages AS m ON m.id = messages_fts.rowid
            WHERE messages_fts MATCH ?1
              AND (?2 IS NULL OR m.sent_at >= ?2)
              AND (?3 IS NULL OR m.sent_at < ?3)
            ORDER BY rank
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
            .prepare("SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1 LIMIT 1")
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

    /// Fetch one message by row id (used to inspect a search hit).
    pub fn get(&self, id: i64) -> Result<Option<Message>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT id, source, external_id, channel, chat_id,
                   author, sent_at, body, raw_json
            FROM messages WHERE id = ?1
            "#,
        )?;
        stmt.query_row(params![id], message_from_row)
            .optional()
            .map_err(Into::into)
    }

    /// Archive counts: total and per source.
    pub fn stats(&self) -> Result<Stats> {
        let total: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?;
        let mut stmt = self.conn.prepare(
            "SELECT source, COUNT(*) FROM messages GROUP BY source ORDER BY COUNT(*) DESC, source",
        )?;
        let by_source = stmt
            .query_map([], |row| {
                Ok(SourceCount {
                    source: row.get(0)?,
                    count: row.get(1)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Stats { total, by_source })
    }

    /// Rebuild the FTS index from scratch from the `messages` table.
    ///
    /// Only needed if the index is ever corrupted; triggers keep it in sync
    /// during normal operation.
    pub fn rebuild_fts(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages_fts (messages_fts) VALUES ('rebuild')",
            [],
        )?;
        Ok(())
    }

    /// Currently applied schema version.
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?)
    }

    /// Test support: drop every FTS row (leaving `messages` intact) so index
    /// recovery paths can be exercised. Not used by the app.
    #[doc(hidden)]
    pub fn wipe_fts_for_tests(&self) -> Result<()> {
        self.conn.execute(
            "INSERT INTO messages_fts (messages_fts) VALUES ('delete-all')",
            [],
        )?;
        Ok(())
    }
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

/// Parse an optional `YYYY-MM-DD` bound into a `NaiveDate`.
fn parse_date_bound(value: Option<&str>, field: &str) -> Result<Option<NaiveDate>> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => NaiveDate::parse_from_str(v, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| Error::InvalidDate(format!("{field}={v}"))),
    }
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

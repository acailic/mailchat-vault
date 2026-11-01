//! SQLite storage: schema migrations, idempotent inserts, and FTS5 search.
//!
//! The database bundles SQLite (via `rusqlite`'s `bundled` feature), so no
//! system SQLite is required. FTS5 is enabled in the bundled build.

use std::path::Path;

use chrono::{Datelike, Days, NaiveDate};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::message::{InsertOutcome, Message, NewMessage, SourceCount, Stats};
use crate::query::{
    ChatContext, SearchHit, SearchPage, SearchQuery, SNIPPET_CLOSE_MARK, SNIPPET_OPEN_MARK,
};
use crate::{Error, Result};

/// Ordered schema migrations; each entry runs once, inside a transaction.
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

    /// Full-text search with optional inclusive LOCAL-day sent-date bounds —
    /// the same date semantics as [`Storage::search_page`].
    ///
    /// `date_from`/`date_to` are `YYYY-MM-DD` strings; anything else is an
    /// error. `date_to` is inclusive (messages sent any time that day count).
    /// Each bound is translated to the UTC instant of local midnight before
    /// being compared with the stored UTC `sent_at`, so a day bound selects
    /// exactly what the UI renders as that local day. Messages with an
    /// unknown (`empty`) `sent_at` are excluded while a bound is active —
    /// they cannot be placed in time. The MATCH expression is a bound
    /// parameter throughout, so it can never spill into SQL text.
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
            SELECT m.id, m.source, m.external_id, m.channel, m.chat_id,
                   m.author, m.sent_at, m.body, m.raw_json
            FROM messages_fts
            JOIN messages AS m ON m.id = messages_fts.rowid
            WHERE messages_fts MATCH ?1
              AND (?2 IS NULL OR m.sent_at >= ?2)
              AND (?3 IS NULL OR m.sent_at < ?3)
              AND ((?2 IS NULL AND ?3 IS NULL) OR m.sent_at != '')
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

    /// Paged, filtered search: FTS5 relevance when `query.text` is non-empty,
    /// newest-first browse otherwise, under the same facet filters.
    ///
    /// Returns one page plus the total match count (for pagination UI). All
    /// row access stays index-backed: the FTS5 index drives text queries, the
    /// `idx_messages_sent_at` / `idx_messages_source_chat` indexes back the
    /// browse ordering and the source/date filters, and `LIMIT`/`OFFSET` bound
    /// every response.
    pub fn search_page(&self, query: &SearchQuery) -> Result<SearchPage> {
        let mut query = query.clone();
        query.normalize()?;
        let filters = QueryFilters::build(&query)?;

        let total: u64 = {
            let (sql, bind) = if query.text.trim().is_empty() {
                (
                    format!(
                        "SELECT COUNT(*) FROM messages AS m WHERE 1=1 {}",
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
                        "SELECT COUNT(*) FROM messages_fts \
                         JOIN messages AS m ON m.id = messages_fts.rowid \
                         WHERE messages_fts MATCH ?1 {}",
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
                "SELECT m.id, m.source, m.external_id, m.channel, m.chat_id, \
                 m.author, m.sent_at, m.body, m.raw_json, NULL \
                 FROM messages AS m WHERE 1=1 {} \
                 ORDER BY m.sent_at DESC, m.id DESC LIMIT ? OFFSET ?",
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
                "SELECT m.id, m.source, m.external_id, m.channel, m.chat_id, \
                 m.author, m.sent_at, m.body, m.raw_json, \
                 snippet(messages_fts, 0, ?1, ?2, '…', 16) \
                 FROM messages_fts \
                 JOIN messages AS m ON m.id = messages_fts.rowid \
                 WHERE messages_fts MATCH ?3 {} \
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

    /// A message plus its adjacent messages in the same chat thread.
    ///
    /// `before`/`after` each hold up to `radius` messages, oldest → newest.
    /// Messages without a `chat_id` have no thread peers.
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
            "SELECT id, source, external_id, channel, chat_id, author, sent_at, body, raw_json \
             FROM messages \
             WHERE source = ?1 AND chat_id = ?2 \
               AND (sent_at < ?3 OR (sent_at = ?3 AND id < ?4)) \
             ORDER BY sent_at DESC, id DESC LIMIT ?5",
        )?;
        let mut before: Vec<Message> = before_stmt
            .query_map(
                params![
                    message.source,
                    message.chat_id,
                    message.sent_at,
                    message.id,
                    radius
                ],
                message_from_row,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        before.reverse(); // newest → oldest from the query; thread reads oldest → newest

        let mut after_stmt = self.conn.prepare(
            "SELECT id, source, external_id, channel, chat_id, author, sent_at, body, raw_json \
             FROM messages \
             WHERE source = ?1 AND chat_id = ?2 \
               AND (sent_at > ?3 OR (sent_at = ?3 AND id > ?4)) \
             ORDER BY sent_at ASC, id ASC LIMIT ?5",
        )?;
        let after = after_stmt
            .query_map(
                params![
                    message.source,
                    message.chat_id,
                    message.sent_at,
                    message.id,
                    radius
                ],
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

    /// Currently applied schema version.
    pub fn schema_version(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?)
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
                "SELECT uidvalidity, last_uid FROM imap_sync_state
                 WHERE source = ?1 AND folder = ?2",
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

    /// Save (upsert) the incremental-sync cursor for one `(source, folder)`.
    /// Stores only folder cursors — never credentials.
    pub fn set_imap_state(
        &self,
        source: &str,
        folder: &str,
        uidvalidity: u32,
        last_uid: u32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO imap_sync_state (source, folder, uidvalidity, last_uid, updated_at)
             VALUES (?1, ?2, ?3, ?4, datetime('now'))
             ON CONFLICT (source, folder) DO UPDATE SET
                uidvalidity = excluded.uidvalidity,
                last_uid    = excluded.last_uid,
                updated_at  = excluded.updated_at",
            params![source, folder, uidvalidity, last_uid],
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

/// Parse an optional `YYYY-MM-DD` bound into a `NaiveDate`. Years outside
/// `0000..=9999` (chrono also accepts signed 5+ digit spellings) are
/// rejected: they cannot be ordered as text against the stored RFC 3339
/// `sent_at` strings.
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

/// Filter SQL fragments (`AND ...` clauses referencing table alias `m`) plus
/// the bound values for them, shared by the page and count statements of
/// [`Storage::search_page`].
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
            sql.push_str(&format!(" AND m.source IN ({placeholders})"));
            values.extend(query.sources.iter().cloned().map(SqlValue::from));
        }
        let author = query.author.trim();
        if !author.is_empty() {
            sql.push_str(" AND m.author LIKE ? ESCAPE '\\'");
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
                sql.push_str(" AND m.sent_at >= ?");
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
                sql.push_str(" AND m.sent_at < ?");
                values.push(SqlValue::from(crate::query::local_day_instant_utc(next)?));
            }
        }
        if !query.from_date.is_empty() || !query.to_date.is_empty() {
            // An unparseable date stored as empty sent_at has no defined
            // position on the timeline; exclude it whenever a date filter is
            // active so both bounds agree on the same set.
            sql.push_str(" AND m.sent_at != ''");
        }
        Ok(Self { sql, values })
    }

    fn bind(&self) -> Vec<SqlValue> {
        self.values.clone()
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

//! SQLite storage: schema migrations, idempotent inserts, and FTS5 search.
//!
//! The database bundles SQLite (via `rusqlite`'s `bundled` feature), so no
//! system SQLite is required. FTS5 is enabled in the bundled build.

use std::path::Path;

use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::message::{InsertOutcome, Message, NewMessage, SourceCount, Stats};
use crate::query::{
    ChatContext, SearchHit, SearchPage, SearchQuery, SNIPPET_CLOSE_MARK, SNIPPET_OPEN_MARK,
};
use crate::Result;

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
        let filters = QueryFilters::build(&query);

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
    fn build(query: &SearchQuery) -> Self {
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
            sql.push_str(" AND m.sent_at >= ?");
            values.push(SqlValue::from(query.from_date.clone()));
        }
        if !query.to_date.is_empty() {
            // Inclusive day bound: sent_at < (to_date + 1 day) textually
            // includes every timestamp later on to_date itself.
            if let Some(bound) = crate::query::day_after_exclusive(&query.to_date) {
                sql.push_str(" AND m.sent_at < ?");
                values.push(SqlValue::from(bound));
            }
        }
        if !query.from_date.is_empty() || !query.to_date.is_empty() {
            // An unparseable date stored as empty sent_at has no defined
            // position on the timeline; exclude it whenever a date filter is
            // active so both bounds agree on the same set.
            sql.push_str(" AND m.sent_at != ''");
        }
        Self { sql, values }
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

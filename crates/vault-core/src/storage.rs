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
use crate::vector::VectorHit;
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
    (
        3,
        r#"
        -- One embedding per message (PK), versioned by model id so a new
        -- embedder invalidates old vectors simply by writing a different
        -- model string. Vectors are i8-quantized (one byte per component,
        -- see crate::vector), L2-normalized
        -- at write time (see crate::vector). Deleting a message cascades.
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
            "SELECT m.id, m.body FROM messages AS m
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
        let messages: u64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?;
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
                 JOIN messages AS m ON m.id = e.message_id
                 WHERE e.model = ?1
                   AND (?2 IS NULL OR m.sent_at >= ?2)
                   AND (?3 IS NULL OR m.sent_at < ?3)
                   AND ((?2 IS NULL AND ?3 IS NULL) OR m.sent_at != '')",
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
            "SELECT id, source, external_id, channel, chat_id,
                    author, sent_at, body, raw_json
             FROM messages WHERE id = ?1",
        )?;
        for id in ids {
            if let Some(message) = stmt.query_row(params![id], message_from_row).optional()? {
                by_id.insert(*id, message);
            }
        }
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
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

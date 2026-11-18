//! Test support only — derives migration rollback SQL from the migration
//! ledger. Doc-hidden like `Storage::wipe_fts_for_tests`: nothing in the
//! app consumes this module; the collections `version17_db` fixture and
//! the property guard test do.
//!
//! The deriver walks [`MIGRATIONS`] newest-first, strips `--` comments,
//! splits each migration into statements, and inverts the DDL:
//!
//! - `CREATE TABLE` / `CREATE VIRTUAL TABLE` → `DROP TABLE IF EXISTS`
//! - `CREATE [UNIQUE] INDEX` → `DROP INDEX IF EXISTS`
//! - `CREATE TRIGGER` → `DROP TRIGGER IF EXISTS`
//! - `ALTER TABLE t ADD COLUMN c` → best-effort
//!   `ALTER TABLE t DROP COLUMN c` (SQLite has no `IF EXISTS` for
//!   columns; callers tolerate errors on these, exactly like the
//!   guarded column-drop loop the collections fixture has always used)
//!
//! Statements within one migration are inverted in reverse order, so
//! child tables drop before their parents. Non-DDL statements (INSERT
//! backfills, FTS rebuilds) carry no schema and are skipped. Migrations
//! that are not a plain CREATE / ADD-COLUMN shape live in the frozen
//! escape-hatch list below, each with its reason. The guard test
//! `derived_rollback_to_every_version_reopens_at_head`
//! (tests/property/migrations.rs) rolls a fresh database back to every
//! historical version and reopens it at head, so a future migration the
//! deriver cannot handle fails at its own addition — not a week later
//! in some fixture as "table already exists".

use std::mem;

use crate::storage::MIGRATIONS;

/// Escape hatches: migrations whose SQL the statement scanner cannot
/// invert, each entry with the reason and the hand-written inverse,
/// emitted (newest-first) where that migration sits in the walk.
///
/// This is a frozen legacy set — the append-only CREATE / ADD-COLUMN
/// forms used from migration 18 onward need no hatch, so a new migration
/// never extends this list. Migration 7 is data-only (its Rust backfill
/// over an empty database is a no-op) and needs no inverse.
const HATCHES: &[(i64, &[&str])] = &[
    // Migration 4 rebuilds `message_embeddings` in place
    // (create-copy-drop-rename onto the canonical table). The scanner
    // sees only `CREATE TABLE message_embeddings_canonical` — already
    // renamed away, so its drop is a no-op — and leaves the rebuilt
    // table behind. The hatch restores the migration-3 table shape so
    // migration 4 re-applies exactly as it did over a real schema-v3
    // database (and migration 3's own undo, which runs after it in the
    // newest-first walk, removes the hatch's work again whenever
    // migration 3 is being undone too).
    (
        4,
        &[
            "DROP TABLE IF EXISTS message_embeddings",
            "CREATE TABLE message_embeddings (
                 message_id  INTEGER PRIMARY KEY REFERENCES messages(id)
                     ON DELETE CASCADE,
                 model       TEXT    NOT NULL,
                 dim         INTEGER NOT NULL,
                 embedding   BLOB    NOT NULL,
                 embedded_at TEXT    NOT NULL DEFAULT (datetime('now'))
             )",
            "CREATE INDEX idx_message_embeddings_model
                 ON message_embeddings (model)",
        ],
    ),
    // Migration 17 is itself only a `DROP INDEX` (of
    // `idx_archive_messages_source`, created by migration 4); a drop has
    // no inverse the scanner can derive. The hatch recreates the index
    // from its migration-4 definition, so a rolled-back database below
    // version 17 matches the shape migration 17 then removes again.
    (
        17,
        &["CREATE INDEX IF NOT EXISTS idx_archive_messages_source
                 ON archive_messages (source_instance_id)"],
    ),
    // Migration 28 rebuilds `archive_fts` in place (drop the triggers
    // and the external-content table, recreate both with the fourth
    // `transcript` column, reindex). Like migration 4, the scanner sees
    // only the new CREATEs — their drops leave nothing behind — and the
    // pre-28 FTS shape lives in a migration at or below the rollback
    // floor, so no walk ever recreates it. The hatch restores the
    // three-column `archive_fts` and its triggers from their original
    // definitions and reindexes, so a database rolled back below 28
    // re-applies migration 28 exactly as it did over a real schema-v27
    // database.
    (
        28,
        &[
            "CREATE VIRTUAL TABLE archive_fts USING fts5 (
                 body, author, subject,
                 content = 'archive_messages',
                 content_rowid = 'id'
             )",
            "CREATE TRIGGER archive_fts_ai AFTER INSERT ON archive_messages BEGIN
                 INSERT INTO archive_fts (rowid, body, author, subject)
                 VALUES (new.id, new.body, new.author, new.subject);
             END",
            "CREATE TRIGGER archive_fts_ad AFTER DELETE ON archive_messages BEGIN
                 INSERT INTO archive_fts (archive_fts, rowid, body, author, subject)
                 VALUES ('delete', old.id, old.body, old.author, old.subject);
             END",
            "CREATE TRIGGER archive_fts_au AFTER UPDATE ON archive_messages BEGIN
                 INSERT INTO archive_fts (archive_fts, rowid, body, author, subject)
                 VALUES ('delete', old.id, old.body, old.author, old.subject);
                 INSERT INTO archive_fts (rowid, body, author, subject)
                 VALUES (new.id, new.body, new.author, new.subject);
             END",
            "INSERT INTO archive_fts (archive_fts) VALUES ('rebuild')",
        ],
    ),
];

/// Test support (doc-hidden): SQL statements that undo the schema DDL of
/// every migration newer than `floor` — newest migration first, the
/// statements of one migration in reverse order (children before
/// parents), escape hatches included. Every statement is guarded
/// (`IF EXISTS`) and can run as one batch. Complete the rollback with
/// [`rollback_column_drops_after`] and the bookkeeping delete
/// (`DELETE FROM schema_migrations WHERE version > floor`).
#[doc(hidden)]
pub fn rollback_drops_after(floor: i64) -> Vec<String> {
    let mut out = Vec::new();
    for &(version, sql) in MIGRATIONS.iter().rev() {
        if version <= floor {
            continue;
        }
        out.extend(inverse_creates(sql));
        if let Some((_, hatch)) = HATCHES.iter().find(|(v, _)| *v == version) {
            out.extend(hatch.iter().map(|stmt| (*stmt).to_string()));
        }
    }
    out
}

/// Test support (doc-hidden): best-effort `ALTER TABLE … DROP COLUMN`
/// inverses of every `ADD COLUMN` newer than `floor`, newest migration
/// first. SQLite has no `IF EXISTS` for columns (and the table may
/// already have been dropped), so callers execute each statement
/// tolerating errors.
#[doc(hidden)]
pub fn rollback_column_drops_after(floor: i64) -> Vec<String> {
    let mut out = Vec::new();
    for &(version, sql) in MIGRATIONS.iter().rev() {
        if version <= floor {
            continue;
        }
        out.extend(inverse_add_columns(sql));
    }
    out
}

/// The inverse drops for one migration's CREATE statements, in reverse
/// statement order.
fn inverse_creates(sql: &str) -> Vec<String> {
    statements(sql)
        .iter()
        .rev()
        .filter_map(|stmt| inverse_create(stmt))
        .collect()
}

/// The best-effort column drops for one migration's ADD COLUMN
/// statements, in reverse statement order.
fn inverse_add_columns(sql: &str) -> Vec<String> {
    statements(sql)
        .iter()
        .rev()
        .filter_map(|stmt| inverse_add_column(stmt))
        .collect()
}

/// Inverse of one `CREATE TABLE` / `CREATE VIRTUAL TABLE` /
/// `CREATE [UNIQUE] INDEX` / `CREATE TRIGGER` statement; `None` for any
/// other statement (data backfills, rebuilds, drops).
fn inverse_create(stmt: &str) -> Option<String> {
    let toks: Vec<&str> = stmt.split_whitespace().collect();
    if !toks
        .first()
        .is_some_and(|t| t.eq_ignore_ascii_case("CREATE"))
    {
        return None;
    }
    let rest = &toks[1..];
    let (object, name_at) = if keyword_at(rest, 0, "VIRTUAL") && keyword_at(rest, 1, "TABLE") {
        ("TABLE", 2)
    } else if keyword_at(rest, 0, "UNIQUE") && keyword_at(rest, 1, "INDEX") {
        ("INDEX", 2)
    } else if keyword_at(rest, 0, "TABLE") {
        ("TABLE", 1)
    } else if keyword_at(rest, 0, "INDEX") {
        ("INDEX", 1)
    } else if keyword_at(rest, 0, "TRIGGER") {
        ("TRIGGER", 1)
    } else {
        return None;
    };
    let name = object_name(rest, name_at)?;
    Some(format!("DROP {object} IF EXISTS {name}"))
}

/// Inverse of one `ALTER TABLE t ADD [COLUMN] c …` statement; `None` for
/// any other statement (including other ALTER forms — migration 4's
/// rename is covered by the escape hatch).
fn inverse_add_column(stmt: &str) -> Option<String> {
    let toks: Vec<&str> = stmt.split_whitespace().collect();
    if !keyword_at(&toks, 0, "ALTER") || !keyword_at(&toks, 1, "TABLE") {
        return None;
    }
    let table = toks.get(2)?;
    let mut i = 3;
    if !keyword_at(&toks, i, "ADD") {
        return None;
    }
    i += 1;
    if keyword_at(&toks, i, "COLUMN") {
        i += 1;
    }
    let column = toks.get(i)?;
    Some(format!("ALTER TABLE {table} DROP COLUMN {column}"))
}

/// The created object's name, skipping an optional `IF NOT EXISTS`.
fn object_name<'a>(rest: &'a [&str], after: usize) -> Option<&'a str> {
    let mut i = after;
    if keyword_at(rest, i, "IF") {
        if !keyword_at(rest, i + 1, "NOT") || !keyword_at(rest, i + 2, "EXISTS") {
            return None;
        }
        i += 3;
    }
    rest.get(i).copied()
}

/// Split migration SQL into single statements. Semicolons inside string
/// literals and inside `CREATE TRIGGER … BEGIN … END;` bodies do not end
/// a statement (trigger bodies are sequences of their own
/// semicolon-terminated statements); the body's closing `END;` does.
fn statements(sql: &str) -> Vec<String> {
    let chars: Vec<char> = strip_comments(sql).chars().collect();
    let mut stmts = Vec::new();
    let mut cur = String::new();
    let mut in_string = false;
    let mut in_trigger_body = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            cur.push(c);
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    cur.push('\'');
                    i += 1;
                } else {
                    in_string = false;
                }
            }
        } else if c == '\'' {
            in_string = true;
            cur.push(c);
        } else if c == ';' {
            cur.push(c);
            if !in_trigger_body {
                stmts.push(mem::take(&mut cur));
            }
        } else {
            if !in_trigger_body && word_at(&chars, i, "BEGIN") && starts_with_create_trigger(&cur) {
                in_trigger_body = true;
            } else if in_trigger_body
                && word_at(&chars, i, "END")
                && statement_end_follows(&chars, i + 3)
            {
                in_trigger_body = false;
            }
            cur.push(c);
        }
        i += 1;
    }
    if !cur.trim().is_empty() {
        stmts.push(cur);
    }
    stmts
}

/// Remove `--` line comments while leaving string literals (and escaped
/// `''` quotes) untouched. Line breaks are kept: they can separate a
/// comment from the tokens that follow it.
fn strip_comments(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = String::with_capacity(sql.len());
    let mut in_string = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\'' {
                if chars.get(i + 1) == Some(&'\'') {
                    out.push('\'');
                    i += 1;
                } else {
                    in_string = false;
                }
            }
        } else if c == '\'' {
            in_string = true;
            out.push(c);
        } else if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            if i < chars.len() {
                out.push('\n');
                i += 1;
            }
            continue;
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Whether the accumulated statement opens a trigger whose `BEGIN`
/// would open a body (`CREATE TRIGGER …`).
fn starts_with_create_trigger(acc: &str) -> bool {
    let mut toks = acc.split_whitespace();
    toks.next()
        .is_some_and(|t| t.eq_ignore_ascii_case("CREATE"))
        && toks
            .next()
            .is_some_and(|t| t.eq_ignore_ascii_case("TRIGGER"))
}

/// Whether `chars[i..]` starts with `word` as a standalone SQL token
/// (ASCII case-insensitive, identifier-character boundaries).
fn word_at(chars: &[char], i: usize, word: &str) -> bool {
    let w: Vec<char> = word.chars().collect();
    if i + w.len() > chars.len()
        || !chars[i..i + w.len()]
            .iter()
            .zip(&w)
            .all(|(c, w)| c.eq_ignore_ascii_case(w))
    {
        return false;
    }
    let boundary = |c: char| c.is_ascii_alphanumeric() || c == '_';
    (i == 0 || !boundary(chars[i - 1]))
        && (i + w.len() == chars.len() || !boundary(chars[i + w.len()]))
}

/// Whether the first non-space character after an `END` token closes the
/// trigger statement (the body's terminator is `END;`).
fn statement_end_follows(chars: &[char], from: usize) -> bool {
    let mut j = from;
    while j < chars.len() && chars[j].is_whitespace() {
        j += 1;
    }
    j < chars.len() && chars[j] == ';'
}

/// Whether `toks[i]` equals the keyword (ASCII case-insensitive).
fn keyword_at<S: AsRef<str>>(toks: &[S], i: usize, keyword: &str) -> bool {
    toks.get(i)
        .is_some_and(|t| t.as_ref().eq_ignore_ascii_case(keyword))
}

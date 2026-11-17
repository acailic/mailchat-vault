//! iMessage chat source: parses the Mac Messages app's store — the
//! `chat.db` SQLite database under `~/Library/Messages` — after copying
//! it to a temporary file first.
//!
//! Copy-first is a hard rule: Messages keeps `chat.db` open and WAL-active
//! while it runs, and reading the live file risks a torn snapshot. The
//! source never opens the picked path as a database — [`IMessageSource`]
//! copies it to an OS-temp file, opens the copy read-only, and deletes
//! the copy on every exit path including errors (the
//! [`tempfile::NamedTempFile`] guard). Nothing under `~/Library` is ever
//! written.
//!
//! Mapping into the archive schema:
//! - `source` is `"imessage"`; `external_id` is the `message` table's
//!   row id — unique across the whole database, so re-importing the
//!   same chat.db (or a later copy of it) dedups instead of
//!   duplicating.
//! - `chat_id` (display grouping) is the joined `chat` row's
//!   `chat_identifier` (Messages keeps the other party's phone or
//!   email there), then the handle, then the source name.
//! - `conversation_key` is `imessage:chat:<chat row id>` — the
//!   provider-stable chat row ids survive database copies, so chats
//!   re-key identically on every import.
//! - `author`: outgoing messages (`is_from_me = 1`) are archived as
//!   `"You"` (chat.db carries no local-account name, the Signal rule);
//!   incoming messages take the `handle.id` phone or email address.
//! - `sent_at`: `message.date` counts from the **Apple epoch**
//!   (2001-01-01T00:00:00Z) — absolute, so the stored UTC time is
//!   exact by construction ([`apple_epoch_to_rfc3339`]). Modern
//!   macOS writes some rows in nanoseconds; magnitudes past
//!   [`APPLE_NANOSECOND_THRESHOLD`] are scaled down.
//! - attachments: the joined `attachment` rows are recorded as
//!   **referenced only** — missing `media` entries in `raw_json` with
//!   reason `attachments-folder-not-granted` — because the bytes live
//!   under `~/Library/Attachments` and are linked only when the user
//!   grants that folder (a follow-up slice). Recording follows the
//!   same shape every other source uses for unpreserved media, so
//!   the files view lists them as missing references, never drops
//!   them silently.
//!
//! Honesty rules: a missing table or column reports an unsupported
//! schema version together with a hash of the found schema (so a
//! future macOS layout change is diagnosable from the error alone);
//! an absent or unreadable picked file reports clear guidance
//! (pick `~/Library/Messages/chat.db`, grant Full Disk Access, or
//! copy the file with Finder and pick the copy).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use rusqlite::{Connection, OpenFlags};
use tempfile::NamedTempFile;

use crate::bounded::{EntryOutcome, Flow, StreamItem, StreamSource};
use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::{Error, Result};

/// Source name and dedup namespace for iMessage imports.
pub const IMESSAGE_SOURCE: &str = "imessage";

/// Author recorded for outgoing messages: chat.db carries no
/// local-account name, so self-sent messages are marked the way the
/// other account-less chat sources are.
pub const OUTGOING_AUTHOR: &str = "You";

/// Seconds between the Apple epoch (2001-01-01T00:00:00Z, the zero
/// point of `message.date`) and the Unix epoch.
pub const APPLE_EPOCH_OFFSET_SECONDS: i64 = 978_307_200;

/// `message.date` magnitudes at or above this are nanoseconds, not
/// seconds: Apple-epoch seconds stay under ~1.6e10 for the next few
/// centuries, while nanosecond stamps for any post-2001 date start
/// at 1e9 and grow from there. (macOS ~10.15+ writes new rows in
/// nanoseconds; older rows stay seconds.)
const APPLE_NANOSECOND_THRESHOLD: u64 = 10_000_000_000;

/// Why a referenced attachment's bytes are not preserved: they live
/// under `~/Library/Attachments`, which only a user folder grant
/// makes readable (the same honest-reference shape every other
/// source's media pass records for absent files).
const ATTACHMENTS_REASON: &str = "attachments-folder-not-granted";

/// The 16-byte magic every SQLite database file starts with — the
/// whole byte-level probe: format detection never opens a database.
pub const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// Convert Apple-epoch seconds to Unix-epoch seconds.
pub fn apple_epoch_to_unix_seconds(apple: i64) -> i64 {
    apple.saturating_add(APPLE_EPOCH_OFFSET_SECONDS)
}

/// Convert Unix-epoch seconds to Apple-epoch seconds (the inverse of
/// [`apple_epoch_to_unix_seconds`]).
pub fn unix_seconds_to_apple_epoch(unix: i64) -> i64 {
    unix.saturating_sub(APPLE_EPOCH_OFFSET_SECONDS)
}

/// Normalize a raw `message.date` value to Apple-epoch seconds,
/// scaling the nanosecond spelling modern macOS writes down by 1e9.
fn apple_epoch_seconds(value: i64) -> i64 {
    if value.unsigned_abs() >= APPLE_NANOSECOND_THRESHOLD {
        value / 1_000_000_000
    } else {
        value
    }
}

/// Apple-epoch `message.date` → the RFC 3339 UTC string the message
/// model stores. `None` when the value falls outside chrono's range
/// (recorded as an unknown time, exactly like an absent timestamp —
/// never an import failure).
pub fn apple_epoch_to_rfc3339(apple: i64) -> Option<String> {
    let unix = apple_epoch_to_unix_seconds(apple_epoch_seconds(apple));
    Utc.timestamp_opt(unix, 0)
        .single()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// True when the bytes start with the SQLite file header — the
/// is-this-format probe over picked bytes. Byte-level only: whether a
/// database is a real chat.db is decided by the schema check after
/// the copy, with an honest error when it is not.
pub fn is_imessage_chat_db(bytes: &[u8]) -> bool {
    bytes.len() >= SQLITE_HEADER.len() && &bytes[..SQLITE_HEADER.len()] == SQLITE_HEADER
}

/// [`is_imessage_chat_db`] over a reader: reads the 16-byte header
/// and nothing more — no database is opened for detection.
pub fn is_imessage_chat_db_reader<R: Read>(mut reader: R) -> Result<bool> {
    let mut header = [0u8; 16];
    match reader.read_exact(&mut header) {
        Ok(()) => Ok(&header == SQLITE_HEADER),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(Error::from(e)),
    }
}

/// The tables and columns the parser joins over — chat.db's stable
/// core since ~OS X 10.8 (`message`, `handle`, `attachment`, the two
/// join tables, `chat`). A missing entry means an unknown schema
/// version, reported with a hash of whatever schema was found.
const REQUIRED_SCHEMA: &[(&str, &[&str])] = &[
    (
        "message",
        &["text", "service", "is_from_me", "date", "handle_id"],
    ),
    ("handle", &["id"]),
    ("attachment", &["filename", "mime_type"]),
    ("chat", &["chat_identifier"]),
    ("chat_message_join", &["chat_id", "message_id"]),
    ("message_attachment_join", &["message_id", "attachment_id"]),
];

/// One message row of the parse query, with its joined handle and one
/// attachment (the query yields one row per message-attachment pair;
/// consecutive rows with the same row id fold into one message).
struct RawRow {
    row_id: i64,
    text: Option<String>,
    service: Option<String>,
    is_from_me: i64,
    date: i64,
    handle: Option<String>,
    filename: Option<String>,
    mime_type: Option<String>,
    chat_name: Option<String>,
    chat_row_id: Option<i64>,
}

impl RawRow {
    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            row_id: row.get(0)?,
            text: row.get(1)?,
            service: row.get(2)?,
            is_from_me: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            date: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
            handle: row.get(5)?,
            filename: row.get(6)?,
            mime_type: row.get(7)?,
            chat_name: row.get(8)?,
            chat_row_id: row.get(9)?,
        })
    }
}

/// The message-joining parse query: handle via `message.handle_id`,
/// attachments through `message_attachment_join`, and the first
/// joined chat through `chat_message_join` (a message forwarded to
/// several chats groups under its original chat, deterministically
/// the lowest chat row id). One row per message-attachment pair.
const PARSE_SQL: &str = r#"
SELECT m.ROWID, m.text, m.service, m.is_from_me, m.date,
       h.id,
       a.filename, a.mime_type,
       (SELECT c.chat_identifier FROM chat_message_join cmj
          JOIN chat c ON c.ROWID = cmj.chat_id
         WHERE cmj.message_id = m.ROWID
         ORDER BY cmj.chat_id LIMIT 1),
       (SELECT cmj.chat_id FROM chat_message_join cmj
         WHERE cmj.message_id = m.ROWID
         ORDER BY cmj.chat_id LIMIT 1)
FROM message m
LEFT JOIN handle h ON h.ROWID = m.handle_id
LEFT JOIN message_attachment_join maj ON maj.message_id = m.ROWID
LEFT JOIN attachment a ON a.ROWID = maj.attachment_id
ORDER BY m.ROWID
"#;

/// A message under construction: the first row of its group plus the
/// folded attachment references.
struct MessageGroup {
    head: RawRow,
    attachments: Vec<(Option<String>, Option<String>)>,
}

impl MessageGroup {
    fn new(head: RawRow) -> Self {
        Self {
            head,
            attachments: Vec::new(),
        }
    }

    /// Convert the group through the shared mapping (used by both the
    /// whole-file collector and the streaming pass, so their behavior
    /// cannot drift apart).
    fn finish(self) -> EntryOutcome {
        let Self {
            head:
                RawRow {
                    row_id,
                    text,
                    service,
                    is_from_me,
                    date,
                    handle,
                    filename: _,
                    mime_type: _,
                    chat_name,
                    chat_row_id,
                },
            attachments,
        } = self;
        let external_id = row_id.to_string();
        let author = if is_from_me != 0 {
            OUTGOING_AUTHOR.to_string()
        } else {
            handle.clone().unwrap_or_default()
        };
        let sent_at = apple_epoch_to_rfc3339(date).unwrap_or_default();
        let body = text.clone().unwrap_or_default();
        let has_time = !sent_at.is_empty();

        let mut msg = NewMessage::new(IMESSAGE_SOURCE, external_id, author, sent_at.clone(), body);
        msg.channel = "chat".to_string();
        msg.chat_id = chat_name
            .clone()
            .or(handle.clone())
            .unwrap_or_else(|| IMESSAGE_SOURCE.to_string());
        msg.conversation_key = chat_row_id
            .map(|id| format!("imessage:chat:{id}"))
            .unwrap_or_default();
        // Apple-epoch stamps are absolute: UTC-exact by construction,
        // recorded as explicitly offset (the Signal rule).
        if has_time {
            msg.original_timestamp = date.to_string();
            msg.timezone = "UTC".to_string();
            msg.time_confidence = "explicit_offset".to_string();
        }
        msg.raw_json = serde_json::json!({
            "service": service,
            "handle": handle,
            "is_from_me": is_from_me != 0,
            "date": date,
            // Attachment metadata verbatim (the query's rows), plus
            // the referenced-only media entries — the missing-entry
            // shape every other source records for unpreserved media.
            "attachments": attachments
                .iter()
                .map(|(filename, mime_type)| {
                    serde_json::json!({ "filename": filename, "mime_type": mime_type })
                })
                .collect::<Vec<_>>(),
            "media": attachments
                .iter()
                .filter_map(|(filename, mime_type)| {
                    let name = attachment_display_name(filename.as_deref())?;
                    let kind = attachment_kind(mime_type.as_deref(), &name);
                    Some(serde_json::json!({
                        "name": name,
                        "kind": kind,
                        "missing": true,
                        "reason": ATTACHMENTS_REASON,
                    }))
                })
                .collect::<Vec<_>>(),
        })
        .to_string();
        EntryOutcome::Message(msg)
    }
}

/// Coarse media kind of an attachment row: the MIME type's family
/// when it names one of the coarse kinds (the Signal contentType
/// rule), else the file-extension table via
/// [`crate::media::media_kind`].
fn attachment_kind(mime_type: Option<&str>, name: &str) -> String {
    let coarse = mime_type
        .and_then(|ct| ct.split('/').next())
        .filter(|first| matches!(*first, "image" | "video" | "audio"));
    crate::media::media_kind(coarse, name)
}

/// Display name of an attachment row: chat.db records `filename` as a
/// tilde-rooted path into `~/Library/Attachments` — the display name
/// is its final path segment.
fn attachment_display_name(filename: Option<&str>) -> Option<String> {
    let filename = filename?.trim();
    if filename.is_empty() {
        return None;
    }
    filename
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// An opened, schema-checked read-only connection over the temporary
/// copy. The copy is deleted when the guard drops — every exit path
/// of every caller, errors included.
struct ChatDbCopy {
    /// Declared before `conn` so it drops after: the connection must
    /// be closed before the temp file is removed.
    _copy: NamedTempFile,
    conn: Connection,
}

/// Map a copy failure of the picked path to honest guidance: absent,
/// locked/permission-denied (macOS TCC / Messages holding the file),
/// or unreadable for another reason.
fn copy_error(path: &Path, err: std::io::Error) -> Error {
    match err.kind() {
        std::io::ErrorKind::NotFound => Error::ChatImport(format!(
            "iMessage database not found at {}: the Messages store lives at \
             ~/Library/Messages/chat.db — pick that file or a manual copy of it",
            path.display()
        )),
        std::io::ErrorKind::PermissionDenied => Error::ChatImport(format!(
            "could not read {} ({}): Messages keeps its database locked and macOS \
             guards it with Full Disk Access — grant the app Full Disk Access in \
             System Settings, or copy chat.db to another folder (e.g. the Desktop) \
             with Finder and pick the copy",
            path.display(),
            err
        )),
        _ => Error::ChatImport(format!(
            "could not read {} ({}): copy chat.db to another folder with Finder \
             and pick the copy",
            path.display(),
            err
        )),
    }
}

/// Verify the found schema against [`REQUIRED_SCHEMA`]: a missing
/// table or column is an unsupported schema version, reported with a
/// short SHA-256 hash over the found `table:column` set so an unknown
/// layout is diagnosable from the error alone.
fn verify_schema(conn: &Connection) -> Result<()> {
    let mut tables: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<_, _>>()?;
    for name in names {
        // The name came from sqlite_master itself; quoting keeps any
        // exotic identifier safe. Columns come from table_info.
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        let mut cols = conn.prepare(&format!("PRAGMA table_info({quoted})"))?;
        let column_names = cols
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        tables.insert(name, column_names);
    }
    let mut missing = Vec::new();
    for (table, columns) in REQUIRED_SCHEMA {
        match tables.get(*table) {
            None => missing.push(format!("table `{table}`")),
            Some(found) => {
                for column in *columns {
                    if !found.iter().any(|c| c.eq_ignore_ascii_case(column)) {
                        missing.push(format!("column `{table}.{column}`"));
                    }
                }
            }
        }
    }
    if !missing.is_empty() {
        return Err(Error::ChatImport(format!(
            "unsupported chat.db schema version (found schema hash {}): missing {} — \
             this Messages database layout is not the one this build reads; report \
             the hash",
            schema_hash(&tables),
            missing.join(", ")
        )));
    }
    Ok(())
}

/// Short hash of a found schema: SHA-256 over the sorted
/// `table:sorted-columns` lines, first 16 hex chars — enough to pin a
/// layout in a bug report without dumping anyone's schema.
fn schema_hash(tables: &BTreeMap<String, Vec<String>>) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for (table, columns) in tables {
        let mut columns = columns.clone();
        columns.sort();
        hasher.update(table.as_bytes());
        hasher.update(b":");
        hasher.update(columns.join(",").as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    digest[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

/// The iMessage chat source: reads the picked `chat.db` (or a manual
/// copy of it) through a temporary copy — never the live file.
#[derive(Debug, Clone)]
pub struct IMessageSource {
    path: PathBuf,
    /// Where the temporary copy is created (tests pin it to a fixture
    /// directory to prove the copy-first discipline; production leaves
    /// it `None` for the OS temp directory).
    temp_dir: Option<PathBuf>,
}

impl IMessageSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            temp_dir: None,
        }
    }

    /// Pin where the temporary copy is created (the copy-first test
    /// discipline: the fixture's temp dir must be empty again after
    /// the run).
    pub fn with_temp_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.temp_dir = Some(dir.into());
        self
    }

    /// Copy the picked file, open the copy read-only, verify the
    /// schema. The copy is owned by the returned guard and deleted on
    /// its drop, whichever way parsing ends.
    fn open_copy(&self) -> Result<ChatDbCopy> {
        let copy = match &self.temp_dir {
            Some(dir) => NamedTempFile::new_in(dir).map_err(Error::from)?,
            None => NamedTempFile::new().map_err(Error::from)?,
        };
        std::fs::copy(&self.path, copy.path()).map_err(|e| copy_error(&self.path, e))?;
        let conn = Connection::open_with_flags(
            copy.path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| {
            Error::ChatImport(format!(
                "the copied database could not be opened as SQLite ({e}) — the file \
                 may have been copied mid-write; quit Messages and copy chat.db again"
            ))
        })?;
        verify_schema(&conn)?;
        Ok(ChatDbCopy { _copy: copy, conn })
    }
}

/// Fold consecutive same-row-id query rows into message groups (the
/// query yields one row per message-attachment pair): `push` returns
/// the previous message's group when the row id changes; `take`
/// finishes the tail.
struct GroupFolder {
    group: Option<MessageGroup>,
}

impl GroupFolder {
    fn new() -> Self {
        Self { group: None }
    }

    fn push(&mut self, raw: RawRow) -> Option<MessageGroup> {
        let row_id = raw.row_id;
        let attachment = (raw.filename.clone(), raw.mime_type.clone());
        match self.group.take() {
            Some(open) if open.head.row_id == row_id => {
                let mut open = open;
                open.attachments.push(attachment);
                self.group = Some(open);
                None
            }
            done => {
                let mut open = MessageGroup::new(raw);
                open.attachments.push(attachment);
                self.group = Some(open);
                done
            }
        }
    }

    fn finish(&mut self) -> Option<MessageGroup> {
        self.group.take()
    }
}

impl Source for IMessageSource {
    fn name(&self) -> &str {
        IMESSAGE_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        let db = self.open_copy()?;
        let mut stmt = db.conn.prepare(PARSE_SQL)?;
        let mut rows = stmt.query([])?;
        let mut messages = Vec::new();
        let mut failed: u64 = 0;
        let mut folder = GroupFolder::new();
        while let Some(row) = rows.next().map_err(Error::from)? {
            let raw = match RawRow::read(row) {
                Ok(raw) => raw,
                Err(_) => {
                    // An unreadable row is one failed item, counted
                    // instead of dropped silently.
                    failed += 1;
                    continue;
                }
            };
            if let Some(done) = folder.push(raw) {
                match done.finish() {
                    EntryOutcome::Message(msg) => messages.push(msg),
                    EntryOutcome::Failed { .. } => failed += 1,
                    EntryOutcome::Skip => {}
                }
            }
        }
        if let Some(done) = folder.finish() {
            match done.finish() {
                EntryOutcome::Message(msg) => messages.push(msg),
                EntryOutcome::Failed { .. } => failed += 1,
                EntryOutcome::Skip => {}
            }
        }
        Ok(Collected { messages, failed })
    }
}

/// The bounded streaming path: same copy-first copy, same query, but
/// row by row — each finished message group is offered to the
/// consumer before the next row is read, so peak memory is one
/// message group whatever the database's size, and `skip` replays a
/// committed run's fate exactly (the shared [`EntryOutcome`] path).
impl StreamSource for IMessageSource {
    fn name(&self) -> &str {
        IMESSAGE_SOURCE
    }

    fn stream(&self, skip: u64, emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()> {
        let db = self.open_copy()?;
        let mut stmt = db.conn.prepare(PARSE_SQL)?;
        let mut rows = stmt.query([])?;
        let mut failure = None;
        let (stopped, result) = {
            let mut state = crate::bounded::ConsumerState {
                skip,
                emit,
                stopped: false,
                failure: &mut failure,
            };
            let mut folder = GroupFolder::new();
            let mut result = Ok(());
            loop {
                let row = match rows.next() {
                    Ok(Some(row)) => row,
                    Ok(None) => break,
                    Err(e) => {
                        result = Err(Error::from(e));
                        break;
                    }
                };
                let raw = match RawRow::read(row) {
                    Ok(raw) => raw,
                    Err(_) => {
                        // An unreadable row is one failed item, not
                        // the end of the stream.
                        if !state.offer(EntryOutcome::Failed {
                            external_ref: String::new(),
                            reason: "row could not be read from chat.db".to_string(),
                        }) {
                            break;
                        }
                        continue;
                    }
                };
                if let Some(done) = folder.push(raw) {
                    if !state.offer(done.finish()) {
                        break;
                    }
                }
            }
            let tail = folder.finish();
            let stopped = state.stopped;
            if !stopped && state.failure.is_none() {
                if let Some(done) = tail {
                    state.offer(done.finish());
                }
            }
            (stopped, result)
        };
        if let Some(e) = failure {
            return Err(e);
        }
        let _ = stopped;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_epoch_conversions_are_exact_both_ways() {
        // The offset itself: Apple epoch zero is 2001-01-01T00:00:00Z.
        assert_eq!(APPLE_EPOCH_OFFSET_SECONDS, 978_307_200);
        assert_eq!(unix_seconds_to_apple_epoch(0), -978_307_200);
        assert_eq!(apple_epoch_to_unix_seconds(-978_307_200), 0);
        // Apple epoch zero is the Unix timestamp of 2001-01-01.
        assert_eq!(apple_epoch_to_unix_seconds(0), 978_307_200);
        assert_eq!(unix_seconds_to_apple_epoch(978_307_200), 0);
        // A known wall-clock pair: 2023-11-01T10:00:00Z is Unix
        // 1698832800 = Apple 720525600.
        assert_eq!(unix_seconds_to_apple_epoch(1698832800), 720525600);
        assert_eq!(apple_epoch_to_unix_seconds(720525600), 1698832800);
    }

    #[test]
    fn apple_epoch_formats_as_exact_utc() {
        assert_eq!(
            apple_epoch_to_rfc3339(720525600).as_deref(),
            Some("2023-11-01T10:00:00Z")
        );
        // Unix epoch instant, expressed in Apple seconds.
        assert_eq!(
            apple_epoch_to_rfc3339(-978_307_200).as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        // Extremes never panic — the nanosecond scaling keeps them
        // inside chrono's range, and the Option stays the API's honest
        // "could not format" channel.
        assert!(apple_epoch_to_rfc3339(i64::MAX).is_some());
        assert!(apple_epoch_to_rfc3339(i64::MIN).is_some());
    }

    #[test]
    fn nanosecond_dates_scale_to_seconds() {
        // Modern macOS writes some rows in nanoseconds: the same
        // 2023-11-01T10:00:00Z instant as ns must format identically
        // to the seconds spelling.
        let ns = 720_525_600_000_000_000i64;
        assert_eq!(apple_epoch_seconds(ns), 720_525_600);
        assert_eq!(
            apple_epoch_to_rfc3339(ns).as_deref(),
            apple_epoch_to_rfc3339(720_525_600).as_deref()
        );
        // Seconds-magnitude values pass through untouched (also for
        // pre-2001 negative stamps).
        assert_eq!(apple_epoch_seconds(720_525_600), 720_525_600);
        assert_eq!(apple_epoch_seconds(-100), -100);
    }

    #[test]
    fn sqlite_header_probe_is_bytes_only() {
        assert!(is_imessage_chat_db(b"SQLite format 3\0and then pages"));
        assert!(!is_imessage_chat_db(b"SQLite format 4\0"));
        assert!(!is_imessage_chat_db(b"SQLite format 3")); // no NUL yet
        assert!(!is_imessage_chat_db(b"[{\"json\": 1}]"));
        assert!(!is_imessage_chat_db(b""));
        // The reader variant reads only the header bytes.
        assert!(is_imessage_chat_db_reader(&b"SQLite format 3\0xx"[..]).unwrap());
        assert!(!is_imessage_chat_db_reader(&b"short"[..]).unwrap());
        assert!(!is_imessage_chat_db_reader(&b""[..]).unwrap());
    }

    #[test]
    fn attachment_names_take_the_last_path_segment() {
        assert_eq!(
            attachment_display_name(Some("~/Library/Attachments/9a/9b/IMG_0001.jpeg")).as_deref(),
            Some("IMG_0001.jpeg")
        );
        assert_eq!(
            attachment_display_name(Some("plain.jpg")).as_deref(),
            Some("plain.jpg")
        );
        assert_eq!(attachment_display_name(Some("")), None);
        assert_eq!(attachment_display_name(Some("   ")), None);
        assert_eq!(attachment_display_name(None), None);
        // A trailing separator never yields an empty name.
        assert_eq!(attachment_display_name(Some("dir/")), None);
    }
}

//! Shared helpers for the integrity suite: database seeding, raw SQLite
//! inspection, and file-level manipulation used to simulate corruption and
//! crashes.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use vault_core::message::{Message, NewMessage};
use vault_core::storage::Storage;

/// A temp dir on the fastest available scratch filesystem. File-based
/// integrity tests create and re-open many databases; on machines whose
/// root filesystem fsyncs slowly that dominates the runtime, so prefer
/// tmpfs (`/dev/shm`) wherever it exists and fall back to the default temp
/// dir otherwise.
pub fn scratch_dir() -> tempfile::TempDir {
    if Path::new("/dev/shm").is_dir() {
        if let Ok(dir) = tempfile::Builder::new()
            .prefix("vault-integrity-")
            .tempdir_in("/dev/shm")
        {
            return dir;
        }
    }
    tempfile::tempdir().expect("create scratch dir")
}

/// SQLite page size the bundled build uses by default; WAL frames are
/// `page_size + 24` bytes on top of the 32-byte WAL header.
pub const PAGE_SIZE: u64 = 4096;

/// Pins the process `TZ` variable for the guard's lifetime: the prior
/// value is restored on drop (even on a failing assertion), and a shared
/// mutex is held so two parallel zone-pinned tests in this binary never
/// pin — and restore — the zone under each other.
pub struct TzGuard {
    prior: Option<String>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl TzGuard {
    pub fn pin(zone: &str) -> Self {
        static TZ_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        let lock = TZ_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prior = std::env::var("TZ").ok();
        std::env::set_var("TZ", zone);
        Self { prior, _lock: lock }
    }
}

impl Drop for TzGuard {
    fn drop(&mut self) {
        match self.prior.take() {
            Some(tz) => std::env::set_var("TZ", tz),
            None => std::env::remove_var("TZ"),
        }
    }
}

/// A message with a known author and date; `source` namespaces the dedup key.
pub fn msg(source: &str, external_id: &str, body: &str) -> NewMessage {
    NewMessage::new(
        source,
        external_id,
        "ana <ana@example.com>",
        "2026-01-01T00:00:00Z",
        body,
    )
}

/// Create a database file with `n` uniquely-tokenized messages, close it
/// cleanly (so the WAL is checkpointed away), and return its path.
pub fn seeded_file(dir: &Path, name: &str, n: u32) -> PathBuf {
    let db = dir.join(name);
    {
        let s = Storage::open(&db).expect("seed: open");
        for i in 0..n {
            s.insert_message(&msg(
                "integrity",
                &format!("m{i}"),
                &format!("tok{i} common filler text to give rows some weight"),
            ))
            .expect("seed: insert");
        }
    } // drop: clean close checkpoints the WAL into the main file
    db
}

/// Open a raw SQLite connection on the file, independent of `Storage`.
pub fn raw(db: &Path) -> Connection {
    Connection::open(db).expect("raw open")
}

/// `PRAGMA integrity_check` verdict, or `None` when even SQLite cannot open
/// the file (itself a form of detection).
pub fn integrity_check(db: &Path) -> Option<String> {
    let conn = Connection::open(db).ok()?;
    let mut stmt = conn.prepare("PRAGMA integrity_check").ok()?;
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .ok()?
        .collect::<Result<_, _>>()
        .ok()?;
    Some(rows.join("; "))
}

/// FTS5's own consistency oracle: the special `integrity-check` command
/// verifies that the canonical full-text index agrees with the external
/// `archive_messages` content table (it catches corrupt or inconsistent
/// index entries, e.g. after recovery or VACUUM). Note it validates
/// existing index entries, so an entirely *missing* index (see the wipe
/// scenario) is not flagged by it — that case is covered behaviorally by
/// search going blind.
pub fn fts_integrity_check(db: &Path) -> rusqlite::Result<()> {
    let conn = Connection::open(db)?;
    conn.execute(
        "INSERT INTO archive_fts (archive_fts) VALUES ('integrity-check')",
        [],
    )
    .map(|_| ())
}

/// Read every stored message as a `Message`, in row-id order (from the
/// canonical `archive_messages` table; the legacy `messages` snapshot is
/// never written and never read by the app).
pub fn all_messages(db: &Path) -> Vec<Message> {
    let conn = raw(db);
    let mut stmt = conn
        .prepare(
            "SELECT m.id, si.legacy_namespace, m.external_id, m.channel, m.chat_id,
                    m.author, COALESCE(m.sent_instant, ''), m.body, m.raw_json
             FROM archive_messages AS m
             JOIN source_instances AS si ON si.id = m.source_instance_id
             ORDER BY m.id",
        )
        .expect("prepare select");
    stmt.query_map([], message_from_row)
        .expect("query archive_messages")
        .collect::<Result<Vec<_>, _>>()
        .expect("read archive_messages")
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
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

/// Truncate the file to `keep` bytes.
pub fn truncate_file(path: &Path, keep: u64) {
    let f = fs::File::options()
        .write(true)
        .open(path)
        .expect("open for truncate");
    f.set_len(keep).expect("truncate");
}

/// Overwrite `len` bytes at `offset` with 0xFF (simulates garbage written
/// over live pages, e.g. by a failing disk or a partial restore).
pub fn corrupt_bytes(path: &Path, offset: u64, len: usize) {
    let mut f = fs::File::options()
        .write(true)
        .open(path)
        .expect("open for corrupt");
    f.seek(SeekFrom::Start(offset)).expect("seek");
    f.write_all(&vec![0xFF; len]).expect("corrupt bytes");
}

/// Copy the database file and, when present, its WAL — but never the
/// `-shm` index (SQLite rebuilds it on recovery). This captures the exact
/// on-disk state of a process that was killed mid-flight: committed frames
/// may live in the WAL, un-checkpointed.
pub fn copy_db_and_wal(from: &Path, to: &Path) {
    fs::copy(from, to).expect("copy db");
    let wal = wal_path(from);
    if wal.exists() {
        fs::copy(&wal, wal_path(to)).expect("copy wal");
    }
}

/// `x.db` → `x.db-wal`.
pub fn wal_path(db: &Path) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push("-wal");
    PathBuf::from(s)
}

pub fn file_len(path: &Path) -> u64 {
    fs::metadata(path).expect("metadata").len()
}

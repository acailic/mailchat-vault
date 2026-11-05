//! Read-only digest of a vault database, for smoke tests.
//!
//! Prints one JSON object with the counts that define "the archive is
//! intact" so a caller can compare a digest taken *before* an upgrade
//! with one taken *after*: content counters (`messages`, `body_bytes`,
//! `fts_docs`, `fts_vault_hits`) must be identical, while
//! `schema_version` is allowed to advance (that is the migration doing
//! its job). Used by the Windows installer smoke test
//! (`scripts/windows-smoke.ps1`, driven by
//! `.github/workflows/windows-smoke.yml`).
//!
//! Opens the file read-only by default and never migrates, so it is
//! safe to point at an archive produced by an older build. If the
//! read-only open or first read fails because the WAL left behind by a
//! force-killed writer needs recovery (`SQLITE_READONLY_RECOVERY` and
//! friends), the tool reopens read-write once to let SQLite recover and
//! `PRAGMA wal_checkpoint(TRUNCATE)` the log — recovery and
//! checkpointing never change logical content, so before/after digests
//! stay comparable — then reads read-only again.
//!
//! ```sh
//! cargo run --release -p vault-core --example smoke_inspect -- <vault.db>
//! ```

use std::collections::BTreeMap;

use rusqlite::OpenFlags;

fn count(conn: &rusqlite::Connection, sql: &str) -> rusqlite::Result<i64> {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0))
}

fn open_read_only(path: &str) -> rusqlite::Result<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
}

fn needs_wal_recovery(e: &rusqlite::Error) -> bool {
    match e {
        rusqlite::Error::SqliteFailure(ffi, _) => matches!(
            ffi.extended_code,
            rusqlite::ffi::SQLITE_READONLY_RECOVERY
                | rusqlite::ffi::SQLITE_READONLY_CANTINIT
                | rusqlite::ffi::SQLITE_READONLY_ROLLBACK
        ),
        _ => false,
    }
}

fn recover_wal(path: &str) -> rusqlite::Result<()> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

fn recover_then_reopen(path: &str) -> rusqlite::Connection {
    recover_wal(path).unwrap_or_else(|e| die(&format!("WAL recovery on {path} failed: {e}")));
    open_read_only(path).unwrap_or_else(|e| {
        die(&format!(
            "cannot reopen {path} read-only after recovery: {e}"
        ))
    })
}

fn take_digest(conn: &rusqlite::Connection) -> rusqlite::Result<BTreeMap<String, i64>> {
    let mut digest: BTreeMap<String, i64> = BTreeMap::new();
    // The schema version lives in `schema_migrations` (MAX(version)),
    // not `PRAGMA user_version` — see `read_schema_version` in storage.rs.
    let version_sql = "SELECT COALESCE(MAX(version), 0) FROM schema_migrations";
    digest.insert("schema_version".into(), count(conn, version_sql)?);
    for (key, sql) in [
        ("messages", "SELECT COUNT(*) FROM archive_messages"),
        ("identities", "SELECT COUNT(*) FROM message_identities"),
        ("occurrences", "SELECT COUNT(*) FROM message_occurrences"),
        ("conversations", "SELECT COUNT(*) FROM conversations"),
        ("source_instances", "SELECT COUNT(*) FROM source_instances"),
        (
            "body_bytes",
            "SELECT COALESCE(SUM(LENGTH(body)), 0) FROM archive_messages",
        ),
        ("fts_docs", "SELECT COUNT(*) FROM archive_fts"),
        (
            "fts_vault_hits",
            "SELECT COUNT(*) FROM archive_fts WHERE archive_fts MATCH 'vault'",
        ),
    ] {
        digest.insert(key.into(), count(conn, sql)?);
    }
    Ok(digest)
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| usage("missing <vault.db> argument"));
    if path == "--help" || path == "-h" {
        usage("");
    }

    let conn = match open_read_only(&path) {
        Ok(conn) => conn,
        Err(ref e) if needs_wal_recovery(e) => recover_then_reopen(&path),
        Err(e) => die(&format!("cannot open {path}: {e}")),
    };

    let digest = match take_digest(&conn) {
        Ok(d) => d,
        Err(ref e) if needs_wal_recovery(e) => {
            drop(conn);
            let conn = recover_then_reopen(&path);
            take_digest(&conn)
                .unwrap_or_else(|e| die(&format!("digest after recovery failed: {e}")))
        }
        Err(e) => die(&format!("digest failed: {e}")),
    };
    println!("{}", serde_json::to_string_pretty(&digest).unwrap());
}

fn usage(msg: &str) -> ! {
    eprintln!("usage: smoke_inspect <vault.db>  ({msg})");
    std::process::exit(2);
}

fn die(msg: &str) -> ! {
    eprintln!("smoke_inspect: {msg}");
    std::process::exit(1);
}

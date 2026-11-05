//! Pins the frozen Windows-upgrade fixture
//! (`tests/windows-smoke/fixtures/vault-v0.1.0.db`) so regeneration from
//! newer code — or content drift of any kind — is caught in CI instead
//! of discovered when the Windows smoke run's old app refuses the
//! database. See `examples/make_smoke_fixture.rs` for what the fixture
//! is and how to regenerate it (only from the v0.1.0 tag).

use std::path::Path;

use rusqlite::OpenFlags;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/windows-smoke/fixtures/vault-v0.1.0.db"
);

fn open() -> rusqlite::Connection {
    assert!(
        Path::new(FIXTURE).exists(),
        "fixture {FIXTURE} is missing — see make_smoke_fixture.rs"
    );
    // Sidecars are never committed; sweep any left by earlier tooling so
    // this test stays hygienic, then open immutable so *this* open
    // cannot recreate them (the fixture is quiescent — checkpointed at
    // generation — so immutable reads cannot miss WAL content).
    for sidecar in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{FIXTURE}{sidecar}"));
    }
    rusqlite::Connection::open_with_flags(
        format!("file:{FIXTURE}?immutable=1"),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .expect("open fixture read-only")
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn fixture_is_pinned_at_the_v0_1_0_shape() {
    let conn = open();
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );

    // The v0.1.0 schema: newer would make the old app refuse the file,
    // older would not exercise the upgrade migration.
    assert_eq!(
        count(&conn, "SELECT MAX(version) FROM schema_migrations"),
        11
    );

    // The demo corpus the upgrade must carry across untouched.
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM archive_messages"), 6);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM message_identities"), 6);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM message_occurrences"), 6);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM conversations"), 2);
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM source_instances"), 1);
    assert_eq!(
        count(
            &conn,
            "SELECT COALESCE(SUM(LENGTH(body)), 0) FROM archive_messages"
        ),
        618
    );
    assert_eq!(count(&conn, "SELECT COUNT(*) FROM archive_fts"), 6);
    assert_eq!(
        count(
            &conn,
            "SELECT COUNT(*) FROM archive_fts WHERE archive_fts MATCH 'vault'"
        ),
        3
    );
}

#[test]
fn fixture_carries_the_expected_demo_content() {
    let conn = open();
    // Spot-check the corpus rather than only counts, so a same-sized
    // regeneration with different content still fails.
    let welcome: String = conn
        .query_row(
            "SELECT body FROM archive_messages WHERE external_id = 'demo-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        welcome.starts_with("Welcome to your local mail and chat archive"),
        "demo-1 body drifted: {welcome:?}"
    );
    let ids: Vec<String> = conn
        .prepare("SELECT external_id FROM archive_messages ORDER BY external_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        ids,
        ["demo-1", "demo-2", "demo-3", "demo-4", "demo-5", "demo-6"]
    );
}

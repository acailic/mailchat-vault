//! Scenario: credential no-persist — after syncing (including failed
//! rounds), the password appears nowhere on disk: not in the database
//! file, not in the WAL/SHM sidecars, not in any other file under the
//! storage directory, and not in any row of any table. `ImapConfig`'s
//! `Debug` output is redacted too.

use std::path::Path;

use rusqlite::OpenFlags;

use crate::imap_mock::MockMailbox;
use crate::{mails, Account, PASSWORD, USER};
use vault_core::imap as vimap;
use vault_core::storage::Storage;

/// Recursively collect every file under `dir` (absolute paths).
fn all_files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

/// Assert the password's byte sequence appears in none of the files under
/// `dir`, naming the offender clearly.
fn assert_no_password_in_any_file(dir: &Path) {
    for path in all_files_under(dir) {
        let Ok(bytes) = std::fs::read(&path) else {
            continue; // unreadable sidecars hold no copyable bytes
        };
        assert!(
            !bytes
                .windows(PASSWORD.len())
                .any(|w| w == PASSWORD.as_bytes()),
            "password leaked into {}",
            path.display()
        );
    }
}

/// A full, messy account lifecycle — successful sync, incremental sync,
/// and a failing folder — against a file-backed vault. Afterwards the
/// storage directory (live WAL included, and again after close/checkpoint)
/// contains no byte sequence of the password.
#[test]
fn no_trace_of_the_password_in_any_file_after_a_full_sync_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("vault.db");
    let storage = Storage::open(&db_path).unwrap();

    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(3, "pw")));
    account.set_mailbox("Broken", MockMailbox::new(4, mails(60, "pwbroken")));
    account.server.fail_uid_fetch_once(Some(2));

    // Round 1: healthy folder syncs, the other breaks mid-scan — both
    // paths write to the database.
    let summaries = account.sync_round(&storage, &["INBOX", "Broken"]);
    assert_eq!(summaries.len(), 2);
    assert!(summaries.iter().any(|s| s.error.is_some()));

    // Round 2: incremental growth on the healthy folder.
    account.add_message(
        "INBOX",
        crate::mail(Some("pw-4@scenario.test"), "pw mail 4", "pwbody4"),
    );
    account.server.fail_uid_fetch_once(None); // heal the server
    let summaries = account.sync_round(&storage, &["INBOX", "Broken"]);
    assert!(summaries.iter().all(|s| s.error.is_none()));
    assert_eq!(storage.stats().unwrap().total, 64);

    // While the connection is open the WAL sidecar holds the newest
    // writes; scan the live directory first...
    assert!(db_path.exists(), "the vault db must exist");
    assert_no_password_in_any_file(dir.path());

    // ...and again after close, when SQLite checkpoints and removes the
    // sidecars (anything surviving into the main db file is still caught).
    drop(storage);
    assert_no_password_in_any_file(dir.path());
}

/// The only thing IMAP sync persists per folder is a cursor: the canonical
/// `sync_checkpoints` table's columns are exactly the cursor fields (keyed
/// by source instance), its rows are exactly one per synced folder under
/// the account's durable source instance, and no cell anywhere in the database
/// holds the password.
#[test]
fn the_sync_state_table_persists_only_folder_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("vault.db");
    let storage = Storage::open(&db_path).unwrap();

    let account = Account::start(USER);
    account.set_mailbox("INBOX", MockMailbox::new(7, mails(1, "cursor")));
    account.set_mailbox("Archive", MockMailbox::new(3, mails(1, "cursorar")));
    account.sync_round(&storage, &["INBOX", "Archive"]);
    drop(storage); // checkpoint so the read-only connection sees everything

    let conn =
        rusqlite::Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();

    // The sync-state schema is cursors and nothing else.
    let columns: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('sync_checkpoints') ORDER BY cid")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(
        columns,
        vec![
            "source_instance_id",
            "folder",
            "uidvalidity",
            "last_uid",
            "updated_at"
        ],
        "the sync-checkpoint table must persist cursors only — no credentials"
    );

    // Exactly one cursor row per synced folder, keyed by the account's
    // durable source instance and holding the folder names (the connector
    // configuration — host/port/TLS/username, never credentials — lives
    // on the instance row itself).
    let rows: Vec<(String, String, i64, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT si.connector_key, sc.folder, sc.uidvalidity, sc.last_uid \
                 FROM sync_checkpoints AS sc \
                 JOIN source_instances AS si ON si.id = sc.source_instance_id \
                 ORDER BY sc.folder",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    };
    assert_eq!(
        rows,
        vec![
            (
                format!("imap|{}|993|implicit-tls|{USER}", crate::MOCK_HOST),
                "Archive".to_string(),
                3,
                1
            ),
            (
                format!("imap|{}|993|implicit-tls|{USER}", crate::MOCK_HOST),
                "INBOX".to_string(),
                7,
                1
            ),
        ]
    );

    // The instance row stores the non-secret connector configuration —
    // and no password anywhere in it.
    let config_json: String = conn
        .query_row(
            "SELECT config_json FROM source_instances WHERE kind = 'imap'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let config: serde_json::Value = serde_json::from_str(&config_json).unwrap();
    assert_eq!(config["username"], USER);
    assert_eq!(config["host"], crate::MOCK_HOST);
    assert_eq!(config["port"], 993);
    assert_eq!(config["tls_mode"], "implicit-tls");
    assert!(!config.to_string().contains(PASSWORD));

    // Belt and braces: no table in the database contains the password in
    // any text cell. (The byte-level file scan lives in the sibling test;
    // this catches structured storage of a transformed-but-plain copy.)
    let mut tables: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    tables.sort();
    for table in &tables {
        // FTS shadow tables have no text of their own worth scanning;
        // they mirror the message tables anyway.
        if table.starts_with("messages_fts_") || table.starts_with("archive_fts_") {
            continue;
        }
        let quoted = table.replace('\"', "\"\"");
        let sql = format!("SELECT * FROM \"{quoted}\"");
        let mut stmt = match conn.prepare(&sql) {
            Ok(stmt) => stmt,
            Err(_) => continue,
        };
        let col_count = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for col in 0..col_count {
                if let Ok(Some(text)) = row.get::<_, Option<String>>(col) {
                    assert!(
                        !text.contains(PASSWORD),
                        "password leaked into {table} column {col}"
                    );
                }
            }
        }
    }
}

/// `ImapConfig`'s `Debug` output must never print the password — logs and
/// error reports routinely debug-print configs.
#[test]
fn imap_config_debug_output_is_redacted() {
    let config = vimap::ImapConfig::gmail(USER, PASSWORD);
    let printed = format!("{config:?}");
    assert!(
        !printed.contains(PASSWORD),
        "Debug leaked the password: {printed}"
    );
    assert!(
        printed.contains("<redacted>"),
        "redaction must be visible: {printed}"
    );
    assert!(
        printed.contains(USER),
        "non-secret fields stay readable: {printed}"
    );
    assert!(printed.contains("imap.gmail.com"));

    // The redaction also survives nested Debug contexts ({:?} of a
    // containing struct would go through the same impl).
    let nested = format!("{:?}", (config, 42));
    assert!(!nested.contains(PASSWORD));
}

/// The same redaction contract covers the OAuth access token: an
/// `Xoauth2` config's `Debug` output never prints the token, and the auth
/// mode stays readable.
#[test]
fn imap_config_debug_output_redacts_the_oauth_token() {
    let config = vimap::ImapConfig {
        host: "imap.gmail.com".to_string(),
        port: 993,
        username: USER.to_string(),
        auth: vimap::ImapAuth::Xoauth2 {
            access_token: "access-1".to_string(),
        },
        folders: Vec::new(),
    };
    let printed = format!("{config:?}");
    assert!(
        !printed.contains("access-1"),
        "Debug leaked the token: {printed}"
    );
    assert!(
        printed.contains("xoauth2"),
        "the auth mode stays readable: {printed}"
    );
}

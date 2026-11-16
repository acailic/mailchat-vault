//! Saved-search persistence contracts (schema 18): the vault stores
//! named definitions as dynamic v2 search requests, not frozen result
//! lists — so a reopened search re-executes and picks up newly imported
//! matching messages. Proven here at the storage layer the IPC commands
//! sit on:
//!
//! - CRUD round-trip: create/list/update-request/rename/delete, with
//!   pagination (`limit`/`offset`) stripped from the stored request and
//!   name conflicts, empty names, unknown ids, and unexecutable requests
//!   refused;
//! - restart persistence: a storage reopen (a new process opening the
//!   same `vault.db`) serves the exact definition, and executing it
//!   after importing another matching message includes it;
//! - source references are canonical source ids: display-label renames
//!   and duplicate labels never redirect a saved search;
//! - rename/update/delete touch the definition row only — never
//!   archived messages, never the other definitions;
//! - backup round-trip: definitions ride inside the `vault.db` snapshot
//!   of the backup container and come back with the restore;
//! - personal-pool isolation: the sample archive (an in-memory storage,
//!   exactly what the shell's demo session is) never serves or absorbs
//!   definitions — writes go through the personal pool's write queue,
//!   the same path the IPC commands use.

use vault_core::backup::{self, RestoreOptions};
use vault_core::jobs::{DbPool, PoolConfig};
use vault_core::message::NewMessage;
use vault_core::search2::{SearchArchiveRequest, SearchSort};
use vault_core::storage::{SavedSearch, Storage, SAVED_SEARCH_MAX_NAME_CHARS};

fn request(text: &str, author: &str) -> SearchArchiveRequest {
    SearchArchiveRequest {
        text: text.into(),
        author: author.into(),
        ..SearchArchiveRequest::default()
    }
}

/// The form a definition stores: the request with pagination (limit /
/// offset) stripped — it is per-execution, never part of the definition.
fn stripped(mut req: SearchArchiveRequest) -> SearchArchiveRequest {
    req.limit = None;
    req.offset = None;
    req
}

fn msg(
    source: &str,
    source_label: &str,
    external_id: &str,
    author: &str,
    body: &str,
) -> NewMessage {
    let mut m = NewMessage::new(source, external_id, author, "2026-11-01T10:00:00Z", body);
    m.source_label = source_label.into();
    m
}

fn assert_saved_search_error(err: vault_core::Error, needle: &str) {
    let text = err.to_string();
    assert!(
        text.contains(needle),
        "error must mention {needle:?}, got: {text}"
    );
}

/// One temp workspace: a `vault/` data dir driven through the real pool
/// (the app's shape: WAL writer + read connections).
struct Sandbox {
    #[allow(dead_code)]
    keeper: tempfile::TempDir,
    root: std::path::PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let keeper = tempfile::tempdir().unwrap();
        let root = keeper.path().join(tag);
        std::fs::create_dir_all(&root).unwrap();
        Self { keeper, root }
    }

    fn data_dir(&self) -> std::path::PathBuf {
        self.root.join("vault")
    }

    fn db(&self) -> std::path::PathBuf {
        self.data_dir().join("vault.db")
    }

    fn spawn_pool(&self) -> std::sync::Arc<DbPool> {
        std::fs::create_dir_all(self.data_dir()).unwrap();
        DbPool::spawn(self.db(), PoolConfig::default(), |_| {}).unwrap()
    }

    fn with_pool<R: Send + 'static>(&self, op: impl FnOnce(&Storage) -> R + Send + 'static) -> R {
        let pool = self.spawn_pool();
        let result = pool
            .submit_write(move |s| vault_core::Result::Ok(op(s)))
            .unwrap();
        pool.shutdown();
        result
    }
}

fn no_progress(_: backup::ProgressNote) {}

#[test]
fn crud_round_trip_strips_pagination_and_refuses_conflicts() {
    let s = Storage::open_in_memory().unwrap();

    // Empty vault: the list exists and is empty (the UI's empty state).
    assert!(s.saved_searches().unwrap().is_empty());

    // Create with the full filter surface: text, canonical source ids,
    // date bounds with an explicit timezone, the honesty filters, import
    // scope, and sort order all round-trip verbatim.
    let req = SearchArchiveRequest {
        text: "invoice".into(),
        source_ids: vec!["11111111-1111-1111-1111-111111111111".into()],
        conversation_id: Some("22222222-2222-2222-2222-222222222222".into()),
        import_job_id: Some("import-7".into()),
        author: "marko".into(),
        from_date: Some("2024-01-01".into()),
        to_date: Some("2024-12-31".into()),
        timezone: "Europe/Zagreb".into(),
        has_attachment: Some(true),
        time_assumed: Some(false),
        grouping_unverified: Some(true),
        files_missing: Some(false),
        sort: SearchSort::Newest,
        limit: Some(7),
        offset: Some(40),
    };
    let saved = s.upsert_saved_search(None, "Invoices 2024", &req).unwrap();
    assert_eq!(saved.name, "Invoices 2024");
    assert_ne!(saved.id, "");
    let mut expected = req.clone();
    // Pagination is per-execution, never part of the definition.
    expected.limit = None;
    expected.offset = None;
    assert_eq!(saved.request, expected);

    // List serves the stored definition.
    let list = s.saved_searches().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0], saved);

    // A duplicate name is refused, never a silent overwrite.
    let err = s
        .upsert_saved_search(None, "Invoices 2024", &request("other", ""))
        .unwrap_err();
    assert_saved_search_error(err, "already exists");

    // Empty/blank names and over-long names are refused.
    for bad_name in ["", "   "] {
        let err = s
            .upsert_saved_search(None, bad_name, &request("x", ""))
            .unwrap_err();
        assert_saved_search_error(err, "needs a name");
    }
    let long = "n".repeat(SAVED_SEARCH_MAX_NAME_CHARS + 1);
    let err = s
        .upsert_saved_search(None, &long, &request("x", ""))
        .unwrap_err();
    assert_saved_search_error(err, "exceeds");

    // A request that cannot execute refuses the save (reversed bounds,
    // bad day format, unknown timezone).
    for bad in [
        SearchArchiveRequest {
            from_date: Some("2024-12-31".into()),
            to_date: Some("2024-01-01".into()),
            ..request("x", "")
        },
        SearchArchiveRequest {
            from_date: Some("31.12.2024".into()),
            ..request("x", "")
        },
        SearchArchiveRequest {
            timezone: "Not/AZone".into(),
            ..request("x", "")
        },
    ] {
        assert!(
            s.upsert_saved_search(None, "bad request", &bad).is_err(),
            "unexecutable request must refuse the save: {bad:?}"
        );
    }
    assert_eq!(s.saved_searches().unwrap().len(), 1, "no partial writes");

    // Update the request in place (same id): a new dynamic definition,
    // same name.
    let updated = s
        .upsert_saved_search(Some(&saved.id), "Invoices 2024", &request("receipt", "ana"))
        .unwrap();
    assert_eq!(updated.id, saved.id);
    assert_eq!(updated.request, stripped(request("receipt", "ana")));

    // Rename in place: new name, same request, same id.
    let renamed = s
        .upsert_saved_search(Some(&saved.id), "Receipts", &request("receipt", "ana"))
        .unwrap();
    assert_eq!(renamed.id, saved.id);
    assert_eq!(renamed.name, "Receipts");
    assert_eq!(renamed.request, stripped(request("receipt", "ana")));
    // Rename frees the old name for a new definition.
    let second = s
        .upsert_saved_search(None, "Invoices 2024", &request("invoice", ""))
        .unwrap();

    // Renaming onto another definition's name is refused.
    let err = s
        .upsert_saved_search(Some(&second.id), "Receipts", &request("invoice", ""))
        .unwrap_err();
    assert_saved_search_error(err, "already exists");

    // An unknown id is refused, not a silent no-op.
    let err = s
        .upsert_saved_search(Some("no-such-id"), "Whatever", &request("x", ""))
        .unwrap_err();
    assert_saved_search_error(err, "no saved search with id");

    // A name is trimmed before it is stored.
    let trimmed = s
        .upsert_saved_search(None, "  Padded  ", &request("x", ""))
        .unwrap();
    assert_eq!(trimmed.name, "Padded");

    // Delete removes exactly one row; deleting an unknown id is not an
    // error and removes nothing.
    assert!(s.delete_saved_search(&saved.id).unwrap());
    let names: Vec<String> = s
        .saved_searches()
        .unwrap()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(
        names,
        vec!["Invoices 2024".to_string(), "Padded".to_string()]
    );
    assert!(!s.delete_saved_search(&saved.id).unwrap());
    assert!(!s.delete_saved_search("no-such-id").unwrap());
}

#[test]
fn definitions_survive_reopen_and_stay_dynamic() {
    let keeper = tempfile::tempdir().unwrap();
    let db = keeper.path().join("vault.db");

    // Two sources that share one display label, so the search is pinned
    // by canonical id, and one import-scoped message set.
    let s = Storage::open(&db).unwrap();
    s.insert_message(&msg(
        "imap:a@example.com",
        "Work mail",
        "a-1",
        "marko",
        "the invoice for november",
    ))
    .unwrap();
    s.insert_message(&msg(
        "imap:b@example.com",
        "Work mail",
        "b-1",
        "marko",
        "the invoice for december",
    ))
    .unwrap();
    let sources = s.list_source_instances().unwrap();
    assert_eq!(sources.len(), 2, "two distinct source instances");
    assert_eq!(
        sources[0].label, sources[1].label,
        "both instances carry the same display label"
    );
    // Resolve the canonical id of the first source through the archive
    // itself (its one uniquely-identifiable message).
    let a_id = s
        .search_archive(&request("november", ""))
        .unwrap()
        .hits
        .pop()
        .expect("the november message is archived")
        .source_id;

    let req = SearchArchiveRequest {
        text: "invoice".into(),
        source_ids: vec![a_id.clone()],
        from_date: Some("2024-01-01".into()),
        to_date: Some("2026-12-31".into()),
        timezone: "Europe/Zagreb".into(),
        sort: SearchSort::Newest,
        ..SearchArchiveRequest::default()
    };
    let saved = s
        .upsert_saved_search(None, "invoices from A", &req)
        .unwrap();

    // Restart semantics: drop every handle, reopen the same file — the
    // definition serves with its filters intact and still executes.
    drop(s);
    let s = Storage::open(&db).unwrap();
    let list = s.saved_searches().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, saved.id);
    assert_eq!(list[0].name, "invoices from A");
    assert_eq!(list[0].request, stripped(req.clone()));

    // Dynamic, not a frozen id list: a message imported *after* the
    // definition was saved appears when the stored request re-executes —
    // and only from the referenced source, whose id still pins the
    // search while a *third* source steals the same display label.
    s.insert_message(&msg(
        "imap:a@example.com",
        "Work mail",
        "a-2",
        "marko",
        "another invoice arrived",
    ))
    .unwrap();
    s.insert_message(&msg(
        "imap:c@example.com",
        "Work mail",
        "c-1",
        "marko",
        "invoice from the label twin",
    ))
    .unwrap();
    let page = s.search_archive(&list[0].request).unwrap();
    let hit_ids: Vec<i64> = page.hits.iter().map(|h| h.id).collect();
    assert_eq!(
        hit_ids,
        vec![3, 1],
        "the stored request re-executes dynamically: the message imported \
         after the save (id 3) joins the original (id 1), while b-1 (id 2, \
         other source) and c-1 (id 4, label twin) stay out — the canonical \
         source id pins the search despite three identical display labels"
    );
}

#[test]
fn rename_update_delete_touch_the_definition_only() {
    let s = Storage::open_in_memory().unwrap();
    s.insert_message(&msg("src", "Source", "m-1", "ana", "body one"))
        .unwrap();
    s.insert_message(&msg("src", "Source", "m-2", "marko", "body two"))
        .unwrap();
    let before_stats = s.stats().unwrap();
    let before_sources = s.list_source_instances().unwrap();

    let first = s
        .upsert_saved_search(None, "agreements", &request("agreement", ""))
        .unwrap();
    let second = s
        .upsert_saved_search(None, "marko mail", &request("", "marko"))
        .unwrap();

    // Rename, update, and delete each leave the archive and the other
    // definition untouched.
    s.upsert_saved_search(Some(&first.id), "contracts", &request("contract", ""))
        .unwrap();
    s.delete_saved_search(&second.id).unwrap();

    let remaining = s.saved_searches().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].name, "contracts");
    assert_eq!(remaining[0].request, stripped(request("contract", "")));

    assert_eq!(s.stats().unwrap().total, before_stats.total);
    assert_eq!(
        s.list_source_instances().unwrap().len(),
        before_sources.len()
    );
    assert_eq!(
        s.search_archive(&request("body", "")).unwrap().total,
        2,
        "archived messages are intact after every definition operation"
    );

    // Deleting the last definition never touches the archive either.
    assert!(s.delete_saved_search(&first.id).unwrap());
    assert!(s.saved_searches().unwrap().is_empty());
    assert_eq!(s.stats().unwrap().total, before_stats.total);
}

#[test]
fn definitions_ride_inside_the_backup_container() {
    let sb = Sandbox::new("saved-search-backup");
    let req = request("invoice", "marko");
    {
        let pool = sb.spawn_pool();
        pool.submit_write(|s| {
            s.insert_message(&msg(
                "mbox",
                "Old exports",
                "m-1",
                "marko",
                "an invoice body",
            ))
            .map(|_| ())
        })
        .unwrap();
        let req_for_write = req.clone();
        pool.submit_write(move |s| {
            s.upsert_saved_search(None, "invoices", &req_for_write)
                .map(|_| ())
        })
        .unwrap();
        let fingerprint = pool.with_read(|s| s.vault_fingerprint().unwrap());
        // The backup snapshots the live vault.db through a connection —
        // the definition lives inside that snapshot.
        let data_dir = sb.data_dir();
        let dest = sb.root.join("vault.mcbackup");
        pool.submit_write(move |s| {
            backup::create_backup(
                &data_dir,
                s.connection(),
                &dest,
                &no_progress,
                &|| false,
                &backup::BackupOptions::default(),
            )
            .map(|_| ())
        })
        .unwrap();
        // Mutate after the backup: a definition added now must not
        // appear in the restored snapshot.
        pool.submit_write(|s| {
            s.upsert_saved_search(None, "after backup", &request("later", ""))
                .map(|_| ())
        })
        .unwrap();
        pool.shutdown();

        // The staged, journaled restore replaces the vault with the
        // backup's state — the definition comes back with it.
        backup::restore_backup(
            &sb.data_dir(),
            &sb.root.join("vault.mcbackup"),
            &fingerprint,
            &no_progress,
            &RestoreOptions::default(),
            &|_| false,
        )
        .unwrap();
    }

    let restored: Vec<SavedSearch> = sb.with_pool(|s| s.saved_searches().unwrap());
    assert_eq!(restored.len(), 1, "exactly the backed-up definition");
    assert_eq!(restored[0].name, "invoices");
    assert_eq!(restored[0].request, stripped(req.clone()));
    // The restored vault still answers the definition's search.
    let restored_req = restored[0].request.clone();
    let hits = sb.with_pool(move |s| s.search_archive(&restored_req).unwrap().total);
    assert_eq!(hits, 1, "the restored archive still matches the definition");
}

#[test]
fn the_sample_archive_never_serves_or_absorbs_definitions() {
    let sb = Sandbox::new("saved-search-demo");
    let req = request("invoice", "");

    // The sample archive is a private in-memory vault seeded with the
    // demo corpus — exactly what the shell's demo session builds. The
    // IPC write path (personal pool's write queue, guarded by the
    // demo-refusal check in the shell) stores definitions only there.
    let demo = Storage::open_in_memory().unwrap();
    for m in vault_core::demo::messages() {
        demo.insert_message(&m).unwrap();
    }
    // Definitions written into the demo storage itself (it is a plain
    // storage) exist only there.
    demo.upsert_saved_search(None, "demo-only", &req).unwrap();

    let saved = sb.with_pool(move |s| s.upsert_saved_search(None, "personal", &req).unwrap());

    // Neither side sees the other's definitions: the demo never serves
    // personal definitions and never absorbs them.
    let demo_names: Vec<String> = demo
        .saved_searches()
        .unwrap()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(demo_names, vec!["demo-only".to_string()]);
    let personal: Vec<String> = sb
        .with_pool(|s| s.saved_searches().unwrap())
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert_eq!(personal, vec!["personal".to_string()]);

    // Dropping the demo session (demo_leave) discards its definitions
    // with it; the personal vault is untouched underneath.
    drop(demo);
    let after: Vec<SavedSearch> = sb.with_pool(|s| s.saved_searches().unwrap());
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, saved.id);

    // The demo corpus itself ships no definitions on a fresh open.
    let fresh_demo = Storage::open_in_memory().unwrap();
    assert!(
        fresh_demo.saved_searches().unwrap().is_empty(),
        "a fresh sample archive starts with no saved searches"
    );
}

//! Integration tests for folder watching (real `notify` backend, real temp
//! folders): new/modified export files are imported in the background after
//! the settle window, re-imports never duplicate (dedup contract), skips
//! and errors are surfaced as events, and the watcher never dies.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vault_core::jobs::{DbPool, PoolConfig};
use vault_core::watch::{FolderWatcher, WatchConfig, WatchEvent, WatchSnapshot};

/// Fast enough for tests, slow enough to coalesce write bursts.
fn test_config() -> WatchConfig {
    WatchConfig {
        settle: Duration::from_millis(150),
        tick: Duration::from_millis(40),
        max_events: 20,
    }
}

/// A pool over a fresh file DB in `tmp` (M2.1: the watcher works through
/// the pool's bounded write queue, so tests exercise the real path).
fn pool(tmp: &tempfile::TempDir) -> Arc<DbPool> {
    DbPool::spawn(tmp.path().join("vault.db"), PoolConfig::default(), |_| {}).unwrap()
}

fn spawn_watcher(pool: &Arc<DbPool>) -> FolderWatcher {
    FolderWatcher::spawn(
        Arc::clone(pool),
        test_config(),
        |_snapshot: &WatchSnapshot| {},
    )
}

/// Poll until `cond` holds or `timeout` elapses (returns whether it held).
fn eventually(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    cond()
}

fn total(pool: &Arc<DbPool>) -> u64 {
    pool.with_read(|s| s.stats().unwrap().total)
}

fn latest_events(watcher: &FolderWatcher) -> Vec<WatchEvent> {
    watcher.snapshot().events
}

fn has_event_with(events: &[WatchEvent], path_fragment: &str) -> bool {
    events.iter().any(|e| e.path().contains(path_fragment))
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn mail(id: &str, subject: &str) -> String {
    format!(
        "From alice@example.com Sat Nov  1 10:00:00 2026\n\
         From: Alice Smith <alice@example.com>\n\
         To: Bob <bob@example.com>\n\
         Subject: {subject}\n\
         Date: Sat, 1 Nov 2026 10:00:00 +0100\n\
         Message-ID: <{id}@example.com>\n\
         \n\
         body of {subject}\n"
    )
}

fn mbox_bytes(ids: &[(&str, &str)]) -> Vec<u8> {
    let mut s = String::new();
    for (id, subject) in ids {
        s.push_str(&mail(id, subject));
    }
    s.into_bytes()
}

const WHATSAPP_EXPORT: &str =
    "[1.11.2026., 09:00:05] Marko: Zdravo ekipa!\n[1.11.2026., 09:01:00] Ana: Vidimo se u pet!\n";

fn telegram_export() -> Vec<u8> {
    br#"{"name": "Arhiva", "type": "private_group", "id": 42, "messages": [
        {"id": 1, "type": "message", "date": "2026-11-01T10:00:00", "from": "Marko", "text": "hi"},
        {"id": 2, "type": "message", "date": "2026-11-01T10:01:00", "from": "Ana", "text": "zdravo"}
    ]}"#
    .to_vec()
}

fn signal_export() -> Vec<u8> {
    br#"[
        {"id": "1", "type": "incoming", "conversationId": "c", "sent_at": 1000, "body": "hi"},
        {"id": "2", "type": "outgoing", "conversationId": "c", "sent_at": 2000, "body": "zdravo"}
    ]"#
    .to_vec()
}

/// A bare RFC 5322 mail (no mbox `From ` separator) — the .eml shape.
fn eml_bytes(id: &str, subject: &str) -> Vec<u8> {
    format!(
        "From: Alice Smith <alice@example.com>\n\
         To: Bob <bob@example.com>\n\
         Subject: {subject}\n\
         Date: Sat, 1 Nov 2026 10:00:00 +0100\n\
         Message-ID: <{id}@example.com>\n\
         \n\
         body of {subject}\n"
    )
    .into_bytes()
}

/// One Messenger DYI thread part: `participants` + `messages` with
/// `sender_name`/`timestamp_ms`/`content` (the minimal parser shape).
fn messenger_part(messages: &[(&str, i64, &str)]) -> Vec<u8> {
    let entries: Vec<String> = messages
        .iter()
        .map(|(sender, ts, content)| {
            format!(
                r#"{{"sender_name": "{sender}", "timestamp_ms": {ts}, "content": "{content}"}}"#
            )
        })
        .collect();
    format!(
        r#"{{"participants": [{{"name": "Ana Peric"}}], "title": "Ana Peric", "thread_path": "inbox/anaperic_1234567890", "messages": [{}]}}"#,
        entries.join(", ")
    )
    .into_bytes()
}

/// The `Imported` summary of the newest event naming `path_fragment`.
fn imported_summary(
    watcher: &FolderWatcher,
    path_fragment: &str,
) -> Option<vault_core::ingest::IngestSummary> {
    watcher.snapshot().events.iter().find_map(|e| match e {
        WatchEvent::Imported { path, summary } if path.contains(path_fragment) => {
            Some(summary.clone())
        }
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn initial_scan_imports_existing_export_files() {
    let tmp = tempfile::tempdir().unwrap();
    let watched = tmp.path().join("exports");
    std::fs::create_dir_all(watched.join("telegram")).unwrap();
    std::fs::create_dir_all(watched.join("signal")).unwrap();
    std::fs::write(watched.join("mail.mbox"), mbox_bytes(&[("a-1", "One")])).unwrap();
    std::fs::write(watched.join("wa.txt"), WHATSAPP_EXPORT).unwrap();
    std::fs::write(
        watched.join("telegram").join("result.json"),
        telegram_export(),
    )
    .unwrap();
    std::fs::write(
        watched.join("signal").join("conversation.json"),
        signal_export(),
    )
    .unwrap();
    // Not an export: must be skipped, not imported and not fatal.
    std::fs::write(watched.join("notes.txt"), "shopping list\n").unwrap();
    std::fs::write(watched.join("logo.png"), b"not text").unwrap();

    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);
    watcher.add_folder(&watched).unwrap();

    // mbox(1) + whatsapp(2) + telegram(2) + signal(2) = 7 messages; png is
    // not even a candidate; notes.txt is skipped.
    assert!(
        eventually(Duration::from_secs(10), || total(&storage) == 7),
        "expected 7 messages, got {} (events: {:?})",
        total(&storage),
        latest_events(&watcher)
    );
    let snap = watcher.snapshot();
    assert_eq!(snap.folders.len(), 1);
    assert!(has_event_with(&snap.events, "notes.txt"));
    assert!(
        snap.events
            .iter()
            .any(|e| matches!(e, WatchEvent::Skipped { .. })),
        "notes.txt must be a skip event: {:?}",
        snap.events
    );
    watcher.shutdown();
    storage.shutdown();
}

#[test]
fn new_and_modified_files_import_after_settle() {
    let tmp = tempfile::tempdir().unwrap();
    let watched = tmp.path().join("exports");
    std::fs::create_dir_all(&watched).unwrap();

    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);
    watcher.add_folder(&watched).unwrap();

    // New file appears after the folder is armed.
    let mbox = watched.join("later.mbox");
    std::fs::write(&mbox, mbox_bytes(&[("b-1", "First"), ("b-2", "Second")])).unwrap();
    assert!(
        eventually(Duration::from_secs(10), || total(&storage) == 2),
        "expected 2 after first write, got {} (events: {:?})",
        total(&storage),
        latest_events(&watcher)
    );

    // The file is rewritten with one more message: only the new one is
    // stored (dedup contract).
    let mut grown = mbox_bytes(&[("b-1", "First"), ("b-2", "Second"), ("b-3", "Third")]);
    grown.extend_from_slice(b"garbage that is not a mail\n");
    std::fs::write(&mbox, grown).unwrap();
    assert!(
        eventually(Duration::from_secs(10), || total(&storage) == 3),
        "expected 3 after modify, got {} (events: {:?})",
        total(&storage),
        latest_events(&watcher)
    );

    // Re-firing settle passes must not duplicate anything: counts stay.
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(total(&storage), 3);
    let snap = watcher.snapshot();
    assert!(snap.running);
    assert!(snap
        .events
        .iter()
        .any(|e| matches!(e, WatchEvent::Imported { .. })));
    watcher.shutdown();
    storage.shutdown();
}

#[test]
fn write_bursts_debounce_into_one_import_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let watched = tmp.path().join("exports");
    std::fs::create_dir_all(&watched).unwrap();

    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);
    watcher.add_folder(&watched).unwrap();

    // Simulate a file being slowly written: many small appends.
    let path = watched.join("slow.mbox");
    let mut fh = std::fs::File::create(&path).unwrap();
    use std::io::Write;
    for chunk in mail("c-1", "Slow").split_inclusive('\n') {
        std::thread::sleep(Duration::from_millis(20));
        fh.write_all(chunk.as_bytes()).unwrap();
    }
    fh.sync_all().unwrap();
    drop(fh);

    assert!(
        eventually(Duration::from_secs(10), || total(&storage) == 1),
        "slow write not imported (events: {:?})",
        latest_events(&watcher)
    );
    std::thread::sleep(Duration::from_millis(500));
    // The burst coalesced into exactly one import pass for the file: the
    // event ring keeps one Imported record per import, so a second settle
    // pass would show up as a second record.
    let imports_of_file = watcher
        .snapshot()
        .events
        .iter()
        .filter(|e| matches!(e, WatchEvent::Imported { path, .. } if path.ends_with("slow.mbox")))
        .count();
    assert_eq!(imports_of_file, 1, "events: {:?}", latest_events(&watcher));
    assert_eq!(total(&storage), 1);
    watcher.shutdown();
    storage.shutdown();
}

#[test]
fn removing_a_folder_stops_its_imports() {
    let tmp = tempfile::tempdir().unwrap();
    let watched = tmp.path().join("exports");
    std::fs::create_dir_all(&watched).unwrap();

    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);
    watcher.add_folder(&watched).unwrap();
    std::fs::write(watched.join("a.mbox"), mbox_bytes(&[("d-1", "One")])).unwrap();
    assert!(eventually(Duration::from_secs(10), || total(&storage) == 1));

    watcher.remove_folder(&watched).unwrap();
    let snap = watcher.snapshot();
    assert!(snap.folders.is_empty());
    assert!(snap
        .events
        .iter()
        .any(|e| matches!(e, WatchEvent::FolderRemoved { .. })));

    // Removing again is a silent no-op: no second FolderRemoved event.
    let events_before = watcher.snapshot().events;
    watcher.remove_folder(&watched).unwrap();
    assert_eq!(watcher.snapshot().events, events_before);

    // Files appearing after removal are not imported.
    std::fs::write(watched.join("b.mbox"), mbox_bytes(&[("d-2", "Two")])).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(total(&storage), 1, "file imported after removal");
    // Storage no longer lists the folder either.
    assert!(storage.with_read(|s| s.watched_folders().unwrap().is_empty()));
    watcher.shutdown();
    storage.shutdown();
}

#[test]
fn folders_persist_across_watcher_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let watched = tmp.path().join("exports");
    std::fs::create_dir_all(&watched).unwrap();
    std::fs::write(watched.join("a.mbox"), mbox_bytes(&[("e-1", "One")])).unwrap();

    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);
    watcher.add_folder(&watched).unwrap();
    assert!(eventually(Duration::from_secs(10), || total(&storage) == 1));
    watcher.shutdown();

    // A fresh watcher over the same database re-arms the folder and its
    // initial scan re-imports nothing new (dedup).
    let watcher2 = spawn_watcher(&storage);
    assert!(
        eventually(Duration::from_secs(10), || {
            let s = watcher2.snapshot();
            s.running && s.folders.len() == 1
        }),
        "folder not re-armed: {:?}",
        watcher2.snapshot()
    );
    assert!(eventually(Duration::from_secs(10), || total(&storage) == 1));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(total(&storage), 1, "restart duplicated messages");

    // And it is still live for new files.
    std::fs::write(watched.join("b.mbox"), mbox_bytes(&[("e-2", "Two")])).unwrap();
    assert!(eventually(Duration::from_secs(10), || total(&storage) == 2));
    watcher2.shutdown();
    storage.shutdown();
}

#[test]
fn add_folder_validates_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);

    let missing = tmp.path().join("does-not-exist");
    let err = watcher.add_folder(&missing).unwrap_err();
    assert!(err.contains("does-not-exist"), "got: {err}");

    let file = tmp.path().join("file.txt");
    std::fs::write(&file, b"x").unwrap();
    let err = watcher.add_folder(&file).unwrap_err();
    assert!(err.contains("not a folder"), "got: {err}");

    // A valid add twice is idempotent.
    std::fs::create_dir_all(tmp.path().join("ok")).unwrap();
    watcher.add_folder(tmp.path().join("ok")).unwrap();
    watcher.add_folder(tmp.path().join("ok")).unwrap();
    assert_eq!(watcher.snapshot().folders.len(), 1);
    assert!(watcher.snapshot().running);
    watcher.shutdown();
    storage.shutdown();
}

#[test]
fn vanished_folder_is_reported_and_watcher_survives() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);

    // Folder that disappears between restarts: recorded, not fatal, and
    // still removable afterwards.
    let gone = tmp.path().join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    let gone_key = gone.to_string_lossy().into_owned();
    storage
        .submit_write(move |s| s.add_watched_folder(&gone_key))
        .unwrap();
    std::fs::remove_dir_all(&gone).unwrap();
    let watcher2 = spawn_watcher(&storage);
    assert!(
        eventually(Duration::from_secs(5), || {
            watcher2
                .snapshot()
                .events
                .iter()
                .any(|e| matches!(e, WatchEvent::Failed { .. }))
        }),
        "vanished folder not reported: {:?}",
        watcher2.snapshot().events
    );
    assert!(watcher2.snapshot().running);
    // The still-missing folder can be forgotten without an error.
    watcher2.remove_folder(&gone).unwrap();
    assert!(watcher2.snapshot().folders.is_empty());
    // And the watcher keeps working: another folder can be added.
    let fresh = tmp.path().join("fresh");
    std::fs::create_dir_all(&fresh).unwrap();
    watcher2.add_folder(&fresh).unwrap();
    assert_eq!(watcher2.snapshot().folders.len(), 1);
    watcher2.shutdown();
    storage.shutdown();
    watcher.shutdown();
    storage.shutdown();
}

#[test]
fn eml_dropped_into_watched_folder_imports_one_mail() {
    let tmp = tempfile::tempdir().unwrap();
    let watched = tmp.path().join("exports");
    std::fs::create_dir_all(&watched).unwrap();

    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);
    watcher.add_folder(&watched).unwrap();

    let eml = watched.join("dragged-out.eml");
    std::fs::write(&eml, eml_bytes("eml-1", "Single mail")).unwrap();
    assert!(
        eventually(Duration::from_secs(10), || total(&storage) == 1),
        "expected 1 mail, got {} (events: {:?})",
        total(&storage),
        latest_events(&watcher)
    );
    let summary =
        imported_summary(&watcher, "dragged-out.eml").expect("an Imported event for the .eml");
    assert_eq!(summary.source, "mbox");
    assert_eq!(summary.inserted, 1);
    // Re-settling must not duplicate.
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(total(&storage), 1);
    watcher.shutdown();
    storage.shutdown();
}

#[test]
fn eml_and_mbox_with_same_message_id_share_one_row() {
    let tmp = tempfile::tempdir().unwrap();
    let watched = tmp.path().join("exports");
    std::fs::create_dir_all(&watched).unwrap();

    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);
    watcher.add_folder(&watched).unwrap();

    // The same mail as a loose .eml first, then inside an .mbox: one
    // row (the shared mbox namespace dedups across both spellings).
    std::fs::write(watched.join("one.eml"), eml_bytes("dup-1", "Same mail")).unwrap();
    assert!(eventually(Duration::from_secs(10), || total(&storage) == 1));
    std::fs::write(
        watched.join("two.mbox"),
        mbox_bytes(&[("dup-1", "Same mail")]),
    )
    .unwrap();
    assert!(
        eventually(Duration::from_secs(10), || {
            imported_summary(&watcher, "two.mbox").is_some()
        }),
        "mbox import event missing (events: {:?})",
        latest_events(&watcher)
    );
    assert_eq!(total(&storage), 1, "same Message-ID stored twice");
    let summary = imported_summary(&watcher, "two.mbox").unwrap();
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.inserted, 0);
    watcher.shutdown();
    storage.shutdown();
}

#[test]
fn messenger_part_arriving_after_arm_imports_the_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let watched = tmp.path().join("exports");
    // The DYI tree's folders exist before arming (dropping a whole
    // nested tree in one burst races the backend's per-directory watch
    // registration — a pre-existing notify limitation, not this
    // feature); the part file itself lands after the folder is armed.
    let thread = watched.join("facebook-ana/messages/inbox/anaperic_1234567890");
    std::fs::create_dir_all(&thread).unwrap();

    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);
    watcher.add_folder(&watched).unwrap();

    // The part file settles and imports the whole thread.
    std::fs::write(
        thread.join("message_1.json"),
        messenger_part(&[
            ("Ana Peric", 1762981200000, "zdravo"),
            ("Ana Peric", 1762981260000, "kako si?"),
        ]),
    )
    .unwrap();
    assert!(
        eventually(Duration::from_secs(10), || total(&storage) == 2),
        "expected 2 thread messages, got {} (events: {:?})",
        total(&storage),
        latest_events(&watcher)
    );
    let summary =
        imported_summary(&watcher, "message_1.json").expect("an Imported event for the part file");
    assert_eq!(summary.source, "messenger");
    assert_eq!(summary.inserted, 2);
    watcher.shutdown();
    storage.shutdown();
}

#[test]
fn messenger_part_landing_later_reimports_thread_without_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let watched = tmp.path().join("exports");
    // Thread folders pre-exist the arm (see the note in the test
    // above); the parts themselves arrive through the watcher.
    let thread = watched.join("facebook-ana/messages/inbox/anaperic_1234567890");
    std::fs::create_dir_all(&thread).unwrap();

    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);
    watcher.add_folder(&watched).unwrap();

    // Part 1 arrives: the thread so far is two messages.
    std::fs::write(
        thread.join("message_1.json"),
        messenger_part(&[
            ("Ana Peric", 1762981200000, "zdravo"),
            ("Ana Peric", 1762981260000, "kako si?"),
        ]),
    )
    .unwrap();
    assert!(eventually(Duration::from_secs(10), || total(&storage) == 2));

    // Part 2 lands later: a part arriving means the *thread* changed,
    // so the whole thread re-imports — the two known messages dedup,
    // the three new ones store, and the total is the sum.
    std::fs::write(
        thread.join("message_2.json"),
        messenger_part(&[
            ("Ana Peric", 1762981320000, "dobro"),
            ("Ana Peric", 1762981380000, "a ti?"),
            ("Ana Peric", 1762981440000, "odlicno"),
        ]),
    )
    .unwrap();
    assert!(
        eventually(Duration::from_secs(10), || total(&storage) == 5),
        "expected 5 thread messages after part 2, got {} (events: {:?})",
        total(&storage),
        latest_events(&watcher)
    );
    let summary =
        imported_summary(&watcher, "message_2.json").expect("an Imported event for part 2");
    assert_eq!(summary.source, "messenger");
    assert_eq!(summary.seen, 5);
    assert_eq!(summary.inserted, 3);
    assert_eq!(summary.skipped, 2);
    // No settle pass ever duplicates a row.
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(total(&storage), 5);
    watcher.shutdown();
    storage.shutdown();
}

#[test]
fn initial_scan_of_populated_messenger_tree_imports_once() {
    let tmp = tempfile::tempdir().unwrap();
    let watched = tmp.path().join("exports");
    let thread = watched.join("messages/inbox/ana_1234567890");
    std::fs::create_dir_all(&thread).unwrap();
    std::fs::write(
        thread.join("message_1.json"),
        messenger_part(&[
            ("Ana", 1762981200000, "prva"),
            ("Ana", 1762981260000, "druga"),
        ]),
    )
    .unwrap();

    let storage = pool(&tmp);
    let watcher = spawn_watcher(&storage);
    watcher.add_folder(&watched).unwrap();

    // The initial scan imports the tree once; the settle passes that
    // follow never duplicate it.
    assert!(
        eventually(Duration::from_secs(10), || total(&storage) == 2),
        "expected 2 messages, got {} (events: {:?})",
        total(&storage),
        latest_events(&watcher)
    );
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(total(&storage), 2, "initial scan duplicated messages");
    let summary = imported_summary(&watcher, "message_1.json").unwrap();
    assert_eq!(summary.source, "messenger");
    watcher.shutdown();
    storage.shutdown();
}

//! Folder watching: automatic background import of export files from
//! user-chosen folders.
//!
//! [`FolderWatcher`] arms a `notify` (inotify/FSEvents/ReadDirectoryChangesW)
//! watch on each folder the user picked — recursively, persisted in the
//! vault database, so watched folders survive restarts. New or modified
//! `.mbox`, WhatsApp `.txt`, and Telegram `.json` export files are imported
//! in the background once they settle:
//!
//! - **Debounce/settle**: file writes arrive in bursts (a file being copied
//!   or written fires many events). A file becomes an import candidate only
//!   after it has stayed quiet for [`WatchConfig::settle`]; further events
//!   push the deadline back. A truncated read therefore self-heals: the
//!   next modification re-imports the file and the dedup contract keeps it
//!   consistent (only messages actually present get stored, exactly once).
//! - **Dedup by contract**: imports go through the ordinary [`Source`]
//!   pipeline, so `(source, external_id)` uniqueness makes re-importing a
//!   file — initial scan, settle pass, or modification — always safe.
//! - **Never crashes the process**: every failure (unreadable file,
//!   unparseable export, storage error, vanished folder) is recorded as a
//!   [`WatchEvent`] visible in the UI; the watch loop never panics and
//!   keeps running.
//! - **Status**: [`WatchSnapshot`] (watched folders, pending files, recent
//!   events) is published on every change for the UI to render.
//!
//! This module has no UI dependencies; the Tauri shell maps snapshots to
//! window events and the commands to `watch_*` IPC commands.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use notify::event::{DataChange, ModifyKind};
use notify::{recommended_watcher, Event, EventKind, RecursiveMode, Watcher};
use serde::Serialize;

use crate::chat_export::{detect_chat_format, ChatFormat};
use crate::ingest::{ingest, IngestSummary, Source};
use crate::mbox::MboxSource;
use crate::storage::Storage;
use crate::telegram::TelegramSource;
use crate::whatsapp::WhatsappSource;

/// File extensions that are import candidates when they appear or change.
const WATCHED_EXTENSIONS: [&str; 3] = ["mbox", "txt", "json"];

/// How deep the initial folder scan descends (guards against symlink
/// cycles).
const SCAN_DEPTH_LIMIT: usize = 64;

/// Tuning knobs of the watch loop. Tests use a short `settle`.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Quiet period after the last filesystem event on a file before it is
    /// considered complete and imported (the debounce window).
    pub settle: Duration,
    /// How often the loop wakes up to check for settled files and commands.
    pub tick: Duration,
    /// How many recent [`WatchEvent`]s the snapshot keeps for the UI.
    pub max_events: usize,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            settle: Duration::from_millis(1500),
            tick: Duration::from_millis(200),
            max_events: 50,
        }
    }
}

/// One thing that happened in the watcher, newest first in the snapshot.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WatchEvent {
    /// A folder was armed (its existing files are being imported).
    FolderAdded { path: String },
    /// A folder was forgotten.
    FolderRemoved { path: String },
    /// A file settled and was imported (dedup contract applies).
    Imported {
        path: String,
        summary: IngestSummary,
    },
    /// A file had a watched extension but is not a known export format;
    /// nothing was imported.
    Skipped { path: String, reason: String },
    /// A file or folder operation failed; the watcher keeps running.
    Failed { path: String, error: String },
}

impl WatchEvent {
    /// The folder/file the event is about.
    pub fn path(&self) -> &str {
        match self {
            WatchEvent::FolderAdded { path }
            | WatchEvent::FolderRemoved { path }
            | WatchEvent::Imported { path, .. }
            | WatchEvent::Skipped { path, .. }
            | WatchEvent::Failed { path, .. } => path,
        }
    }
}

/// A point-in-time view of the watcher for the UI.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WatchSnapshot {
    /// Whether the watch loop thread is alive.
    pub running: bool,
    /// Canonical paths of the watched folders.
    pub folders: Vec<String>,
    /// Import candidates waiting for their settle deadline.
    pub pending: u64,
    /// Recent events, newest first (bounded by [`WatchConfig::max_events`]).
    pub events: Vec<WatchEvent>,
}

/// Control messages to the watch loop.
enum Command {
    AddFolder {
        path: PathBuf,
        ack: Sender<Result<(), String>>,
    },
    RemoveFolder {
        path: PathBuf,
        ack: Sender<Result<(), String>>,
    },
    Shutdown,
}

/// Handle to the background watch loop.
///
/// `snapshot()` reads shared state directly; folder add/remove round-trip
/// to the loop (which owns the `notify` watcher and the folder list) and
/// block for its acknowledgement.
pub struct FolderWatcher {
    control: Sender<Command>,
    snapshot: Arc<Mutex<WatchSnapshot>>,
    thread: Option<JoinHandle<()>>,
}

impl FolderWatcher {
    /// Start the watch loop over `storage`'s watched folders (re-arming
    /// folders persisted by a previous run) and call `on_change` with the
    /// new snapshot whenever it changes.
    ///
    /// If the thread cannot spawn at all, the handle exists with
    /// `running == false` and folder commands return errors — the process
    /// never panics from the watcher.
    pub fn spawn(
        storage: Arc<Mutex<Storage>>,
        config: WatchConfig,
        on_change: impl Fn(&WatchSnapshot) + Send + Sync + 'static,
    ) -> Self {
        let (control_tx, control_rx) = std::sync::mpsc::channel::<Command>();
        let snapshot = Arc::new(Mutex::new(WatchSnapshot::default()));
        let snapshot_for_thread = Arc::clone(&snapshot);
        let thread = std::thread::Builder::new()
            .name("folder-watch".into())
            .spawn(move || {
                run_loop(storage, config, on_change, control_rx, snapshot_for_thread);
            })
            .ok();
        if thread.is_none() {
            // Thread could not start (resource exhaustion). Record it in
            // the shared snapshot instead of panicking.
            if let Ok(mut snap) = snapshot.lock() {
                snap.running = false;
            }
        }
        Self {
            control: control_tx,
            snapshot,
            thread,
        }
    }

    /// Arm a folder: persisted, watched recursively, and its existing
    /// candidate files enqueued for import.
    ///
    /// Errors when the path does not exist, is not a folder, or cannot be
    /// watched — the watcher state is unchanged then.
    pub fn add_folder(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let (ack, ack_rx) = std::sync::mpsc::channel();
        self.control
            .send(Command::AddFolder {
                path: path.as_ref().to_path_buf(),
                ack,
            })
            .map_err(|_| "watch loop is not running".to_string())?;
        ack_rx
            .recv()
            .map_err(|_| "watch loop stopped while adding the folder".to_string())?
    }

    /// Forget a folder and stop watching it. Removing a path that is not
    /// watched is a successful no-op.
    pub fn remove_folder(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let (ack, ack_rx) = std::sync::mpsc::channel();
        self.control
            .send(Command::RemoveFolder {
                path: path.as_ref().to_path_buf(),
                ack,
            })
            .map_err(|_| "watch loop is not running".to_string())?;
        ack_rx
            .recv()
            .map_err(|_| "watch loop stopped while removing the folder".to_string())?
    }

    /// The current watcher status.
    pub fn snapshot(&self) -> WatchSnapshot {
        self.snapshot.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Stop the loop and wait for it (used by tests). Ordinary drops send
    /// `Shutdown` without joining, so app teardown never blocks on an
    /// import that is mid-file.
    pub fn shutdown(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.control.send(Command::Shutdown);
            let _ = thread.join();
        }
    }
}

impl Drop for FolderWatcher {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.control.send(Command::Shutdown);
            // Detach: the process is exiting; do not block teardown.
            std::thread::spawn(move || {
                let _ = thread.join();
            });
        }
    }
}

/// Lock the snapshot without ever panicking on poisoning (a panic in an
/// importer must not take the loop down with it).
fn lock_snap(snapshot: &Arc<Mutex<WatchSnapshot>>) -> MutexGuard<'_, WatchSnapshot> {
    snapshot.lock().unwrap_or_else(|p| p.into_inner())
}

fn lock_storage(storage: &Arc<Mutex<Storage>>) -> Result<MutexGuard<'_, Storage>, String> {
    storage
        .lock()
        .map_err(|e| format!("storage lock poisoned: {e}"))
}

/// Set the pending count, publish the snapshot, and notify `on_change`
/// after releasing the lock.
fn publish(
    snapshot: &Arc<Mutex<WatchSnapshot>>,
    pending: &HashMap<PathBuf, Instant>,
    on_change: &impl Fn(&WatchSnapshot),
) {
    let changed = {
        let mut snap = lock_snap(snapshot);
        snap.pending = pending.len() as u64;
        snap.clone()
    };
    on_change(&changed);
}

/// Append an event (newest first) and trim the ring.
fn push_event(snap: &mut WatchSnapshot, event: WatchEvent, max_events: usize) {
    snap.events.insert(0, event);
    if snap.events.len() > max_events {
        snap.events.truncate(max_events);
    }
}

/// Whether a filesystem event kind should (re)arm the settle timer for its
/// paths (file creation/writes/metadata/rename-into; not reads/removals).
fn is_activity(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Modify(
                ModifyKind::Any
                    | ModifyKind::Data(DataChange::Any)
                    | ModifyKind::Name(_)
                    | ModifyKind::Other
                    | ModifyKind::Metadata(_)
            )
    )
}

/// A file path with a watched extension (case-insensitive).
fn is_import_candidate(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| WATCHED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Record one filesystem event: activity (re)arms the settle timer for its
/// file paths; removals disarm theirs.
fn handle_fs_event(event: &Event, pending: &mut HashMap<PathBuf, Instant>, settle: Duration) {
    match &event.kind {
        EventKind::Remove(_) => {
            for path in &event.paths {
                pending.remove(path);
            }
        }
        kind if is_activity(kind) => {
            for path in &event.paths {
                if path.is_dir() {
                    continue; // only files are import candidates
                }
                if is_import_candidate(path) {
                    pending.insert(path.clone(), Instant::now() + settle);
                }
            }
        }
        _ => {}
    }
}

/// Arm a folder with the notify backend.
fn arm_folder(watcher: &mut notify::RecommendedWatcher, dir: &Path) -> Result<(), String> {
    watcher
        .watch(dir, RecursiveMode::Recursive)
        .map_err(|e| e.to_string())
}

/// Enqueue every candidate file under `dir` as immediately due: existing
/// files are by definition settled, and re-importing is free (dedup
/// contract).
fn enqueue_initial_scan(dir: &Path, pending: &mut HashMap<PathBuf, Instant>) {
    let mut files = Vec::new();
    scan_dir(dir, 0, &mut files);
    for file in files {
        pending.insert(file, Instant::now());
    }
}

fn scan_dir(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > SCAN_DEPTH_LIMIT {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            scan_dir(&path, depth + 1, out);
        } else if is_import_candidate(&path) {
            out.push(path);
        }
    }
}

/// The watch loop: filesystem events + commands in, imports out.
fn run_loop(
    storage: Arc<Mutex<Storage>>,
    config: WatchConfig,
    on_change: impl Fn(&WatchSnapshot) + Send + Sync + 'static,
    control: Receiver<Command>,
    snapshot: Arc<Mutex<WatchSnapshot>>,
) {
    // notify delivers through a callback; bridge into a std channel.
    let (fs_tx, fs_rx) = std::sync::mpsc::channel::<Result<Event, notify::Error>>();
    let mut watcher = match recommended_watcher(move |res| {
        let _ = fs_tx.send(res);
    }) {
        Ok(watcher) => watcher,
        Err(e) => {
            // Cannot watch anything at all (backend unavailable): record it
            // and stop the loop cleanly — the process itself is fine.
            let mut snap = lock_snap(&snapshot);
            snap.running = false;
            push_event(
                &mut snap,
                WatchEvent::Failed {
                    path: "(watcher backend)".into(),
                    error: format!("cannot start folder watching: {e}"),
                },
                config.max_events,
            );
            let changed = snap.clone();
            drop(snap);
            on_change(&changed);
            return;
        }
    };

    // Import candidates: file path → earliest import time (last event +
    // settle). One entry per file debounces any event burst.
    let mut pending: HashMap<PathBuf, Instant> = HashMap::new();

    // Re-arm folders persisted by earlier runs. Errors here (missing dir,
    // backend limit) are recorded, not fatal: the folder stays configured,
    // so it re-arms on the next start once present again.
    {
        let stored: Vec<String> = lock_storage(&storage)
            .and_then(|s| s.watched_folders().map_err(|e| e.to_string()))
            .unwrap_or_default();
        {
            let mut snap = lock_snap(&snapshot);
            snap.running = true;
            for path in stored {
                let dir = PathBuf::from(&path);
                match arm_folder(&mut watcher, &dir) {
                    Ok(()) => {
                        snap.folders.push(path.clone());
                        enqueue_initial_scan(&dir, &mut pending);
                    }
                    Err(e) => push_event(
                        &mut snap,
                        WatchEvent::Failed {
                            path,
                            error: format!("cannot watch on startup: {e}"),
                        },
                        config.max_events,
                    ),
                }
            }
            snap.folders.sort();
        }
        publish(&snapshot, &pending, &on_change);
    }

    loop {
        // Drain filesystem events until the next tick boundary.
        let deadline = Instant::now() + config.tick;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match fs_rx.recv_timeout(remaining) {
                Ok(Ok(event)) => handle_fs_event(&event, &mut pending, config.settle),
                Ok(Err(e)) => {
                    // A single backend error (e.g. an evicted watch) is
                    // surfaced but never fatal.
                    let mut snap = lock_snap(&snapshot);
                    push_event(
                        &mut snap,
                        WatchEvent::Failed {
                            path: "(watcher backend)".into(),
                            error: e.to_string(),
                        },
                        config.max_events,
                    );
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }

        // Commands (add/remove folders, shutdown).
        while let Ok(cmd) = control.try_recv() {
            match cmd {
                Command::Shutdown => return,
                Command::AddFolder { path, ack } => {
                    let result = {
                        let mut snap = lock_snap(&snapshot);
                        add_folder(
                            &mut watcher,
                            &storage,
                            &mut pending,
                            &mut snap,
                            &config,
                            &path,
                        )
                    };
                    publish(&snapshot, &pending, &on_change);
                    let _ = ack.send(result);
                }
                Command::RemoveFolder { path, ack } => {
                    let result = {
                        let mut snap = lock_snap(&snapshot);
                        remove_folder(
                            &mut watcher,
                            &storage,
                            &mut pending,
                            &mut snap,
                            &config,
                            &path,
                        )
                    };
                    publish(&snapshot, &pending, &on_change);
                    let _ = ack.send(result);
                }
            }
        }

        // Import files whose quiet window elapsed.
        let now = Instant::now();
        let due: Vec<PathBuf> = pending
            .iter()
            .filter(|(_, due)| **due <= now)
            .map(|(path, _)| path.clone())
            .collect();
        if !due.is_empty() {
            for path in due {
                pending.remove(&path);
                import_file(&storage, &snapshot, &config, &on_change, &path);
            }
            publish(&snapshot, &pending, &on_change);
        }
    }
}

/// Add a folder (command handler): validate, persist, arm, initial scan.
fn add_folder(
    watcher: &mut notify::RecommendedWatcher,
    storage: &Arc<Mutex<Storage>>,
    pending: &mut HashMap<PathBuf, Instant>,
    snap: &mut WatchSnapshot,
    config: &WatchConfig,
    path: &Path,
) -> Result<(), String> {
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("{} is not a folder", canonical.display()));
    }
    let key = canonical.to_string_lossy().into_owned();
    if snap.folders.contains(&key) {
        return Ok(()); // already watched — idempotent
    }
    arm_folder(watcher, &canonical)?;
    if let Err(e) =
        lock_storage(storage).and_then(|s| s.add_watched_folder(&key).map_err(|e| e.to_string()))
    {
        // Persist failed: undo the arm so the watcher matches its records.
        let _ = watcher.unwatch(&canonical);
        return Err(e);
    }
    snap.folders.push(key.clone());
    snap.folders.sort();
    enqueue_initial_scan(&canonical, pending);
    push_event(
        snap,
        WatchEvent::FolderAdded { path: key },
        config.max_events,
    );
    Ok(())
}

/// Remove a folder (command handler): unwatch (best-effort — the folder may
/// already be gone), drop its pending candidates, forget in storage.
/// Removing a path that is not watched is a successful no-op.
fn remove_folder(
    watcher: &mut notify::RecommendedWatcher,
    storage: &Arc<Mutex<Storage>>,
    pending: &mut HashMap<PathBuf, Instant>,
    snap: &mut WatchSnapshot,
    config: &WatchConfig,
    path: &Path,
) -> Result<(), String> {
    // Canonicalize when possible; a folder deleted from disk must still be
    // removable by its stored path, so fall back to the given spelling.
    let mut keys: Vec<PathBuf> = vec![path.to_path_buf()];
    if let Ok(canonical) = path.canonicalize() {
        keys.insert(0, canonical);
    }
    let mut removed_key = keys
        .first()
        .map(|k| k.to_string_lossy().into_owned())
        .unwrap_or_default();
    for key in &keys {
        let ks = key.to_string_lossy().into_owned();
        let _ = watcher.unwatch(key);
        pending.retain(|p, _| !p.starts_with(key));
        if snap.folders.contains(&ks) {
            removed_key = ks.clone();
        }
        snap.folders.retain(|f| f != &ks);
        if let Ok(s) = lock_storage(storage) {
            let _ = s.remove_watched_folder(&ks);
        }
    }
    push_event(
        snap,
        WatchEvent::FolderRemoved { path: removed_key },
        config.max_events,
    );
    Ok(())
}

/// Import one settled file through the normal source pipeline (dedup
/// contract applies). All failures become events, never panics.
fn import_file(
    storage: &Arc<Mutex<Storage>>,
    snapshot: &Arc<Mutex<WatchSnapshot>>,
    config: &WatchConfig,
    on_change: &impl Fn(&WatchSnapshot),
    path: &Path,
) {
    let display = path.display().to_string();
    let event = match import_source_for(path) {
        Ok(Some(source)) => match lock_storage(storage)
            .and_then(|s| ingest(&s, source.as_ref()).map_err(|e| e.to_string()))
        {
            Ok(summary) => WatchEvent::Imported {
                path: display,
                summary,
            },
            Err(e) => WatchEvent::Failed {
                path: display,
                error: e,
            },
        },
        Ok(None) => WatchEvent::Skipped {
            path: display,
            reason: "not a recognized export (.mbox, WhatsApp .txt, or Telegram result.json)"
                .into(),
        },
        Err(e) => WatchEvent::Failed {
            path: display,
            error: e,
        },
    };
    let changed = {
        let mut snap = lock_snap(snapshot);
        push_event(&mut snap, event, config.max_events);
        snap.clone()
    };
    on_change(&changed);
}

/// Pick the ingest source for an import candidate by extension + contents.
/// `Ok(None)` = watched extension but unknown contents (a skip, not an
/// error — folders legitimately hold unrelated .txt/.json files).
fn import_source_for(path: &Path) -> Result<Option<Box<dyn Source>>, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "mbox" {
        return Ok(Some(Box::new(MboxSource::new(path))));
    }
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match detect_chat_format(&bytes) {
        Some(ChatFormat::Telegram) => Ok(Some(Box::new(TelegramSource::new(path)))),
        Some(ChatFormat::WhatsApp) => Ok(Some(Box::new(WhatsappSource::new(path)))),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, RemoveKind};

    #[test]
    fn extensions_filter_is_case_insensitive() {
        assert!(is_import_candidate(Path::new("/x/a.mbox")));
        assert!(is_import_candidate(Path::new("/x/A.MBOX")));
        assert!(is_import_candidate(Path::new("/x/chat.TXT")));
        assert!(is_import_candidate(Path::new("/x/result.json")));
        assert!(!is_import_candidate(Path::new("/x/photo.png")));
        assert!(!is_import_candidate(Path::new("/x/noext")));
        assert!(!is_import_candidate(Path::new("/x/report.pdf")));
    }

    #[test]
    fn activity_kinds_arm_and_removals_disarm() {
        assert!(is_activity(&EventKind::Create(CreateKind::File)));
        assert!(is_activity(&EventKind::Create(CreateKind::Folder)));
        assert!(is_activity(&EventKind::Modify(ModifyKind::Data(
            DataChange::Any
        ))));
        assert!(is_activity(&EventKind::Modify(ModifyKind::Metadata(
            notify::event::MetadataKind::Any
        ))));
        assert!(!is_activity(&EventKind::Remove(RemoveKind::File)));
        assert!(!is_activity(&EventKind::Access(
            notify::event::AccessKind::Any
        )));

        let settle = Duration::from_millis(10);
        let mut pending = HashMap::new();
        let file = PathBuf::from("/definitely-not-here-watch-test/a.mbox");
        handle_fs_event(
            &Event::new(EventKind::Create(CreateKind::File)).add_path(file.clone()),
            &mut pending,
            settle,
        );
        assert!(pending.contains_key(&file));
        handle_fs_event(
            &Event::new(EventKind::Remove(RemoveKind::File)).add_path(file.clone()),
            &mut pending,
            settle,
        );
        assert!(!pending.contains_key(&file));
    }

    #[test]
    fn event_ring_is_bounded_and_newest_first() {
        let mut snap = WatchSnapshot::default();
        for i in 0..5 {
            push_event(
                &mut snap,
                WatchEvent::FolderAdded {
                    path: format!("/f{i}"),
                },
                3,
            );
        }
        assert_eq!(snap.events.len(), 3);
        assert_eq!(snap.events[0].path(), "/f4");
        assert_eq!(snap.events[2].path(), "/f2");
    }
}

//! Mailchat Vault desktop app: a thin Tauri shell over `vault-core`.
//!
//! Commands exposed to the frontend:
//! - `search_messages` — full-text search hitting SQLite FTS5.
//! - `import_mbox` — manual `.mbox` import (the demo ingest source).
//! - `seed_demo` — insert the built-in sample messages (idempotent).
//! - `stats` — archive counts per source.

use std::sync::{Mutex, MutexGuard};

use tauri::{Manager, State};
use vault_core::ingest::{ingest, IngestSummary, Source};
use vault_core::mbox::MboxSource;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;
use vault_core::{demo, message, Error, Result as CoreResult};

/// Shared app state: one open storage handle guarded by a mutex.
struct AppState {
    storage: Mutex<Storage>,
}

fn lock(state: &State<AppState>) -> Result<MutexGuard<'_, Storage>, String> {
    state.storage.lock().map_err(|e| e.to_string())
}

/// Full-text search over the archive. Free text is sanitized into an FTS5
/// MATCH expression, so arbitrary input can never be a query syntax error.
#[tauri::command]
fn search_messages(
    state: State<AppState>,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<message::Message>, String> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let limit = limit.unwrap_or(50).clamp(1, 200);
    lock(&state)?
        .search(&build_match_query(&query), limit)
        .map_err(|e| e.to_string())
}

/// Import every message of an `.mbox` file (idempotent; re-importing skips
/// already-stored messages).
#[tauri::command]
fn import_mbox(state: State<AppState>, path: String) -> Result<IngestSummary, String> {
    let source = MboxSource::new(path);
    let storage = lock(&state)?;
    ingest(&storage, &source).map_err(|e: Error| e.to_string())
}

/// Insert the built-in demo messages so a fresh install has searchable data.
#[tauri::command]
fn seed_demo(state: State<AppState>) -> Result<IngestSummary, String> {
    let storage = lock(&state)?;
    ingest(&storage, &DemoSource).map_err(|e| e.to_string())
}

/// Archive counts: total and per source.
#[tauri::command]
fn stats(state: State<AppState>) -> Result<message::Stats, String> {
    lock(&state)?.stats().map_err(|e| e.to_string())
}

/// The built-in demo messages as an ingest source.
struct DemoSource;

impl Source for DemoSource {
    fn name(&self) -> &str {
        demo::DEMO_SOURCE
    }

    fn collect(&self) -> CoreResult<Vec<message::NewMessage>> {
        Ok(demo::messages())
    }
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
            let storage = Storage::open(data_dir.join("vault.db")).map_err(|e| e.to_string())?;
            app.manage(AppState {
                storage: Mutex::new(storage),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search_messages,
            import_mbox,
            seed_demo,
            stats
        ])
        .run(tauri::generate_context!())
        .expect("error while running mailchat vault");
}

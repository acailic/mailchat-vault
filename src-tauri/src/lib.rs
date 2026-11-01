//! Mailchat Vault desktop app: a thin Tauri shell over `vault-core`.
//!
//! Commands exposed to the frontend:
//! - `search_messages` — paged, filtered full-text search hitting SQLite FTS5
//!   (relevance-ranked with match snippets, or newest-first browse).
//! - `chat_context` — a message plus its adjacent messages in the same chat.
//! - `llm_ask` — natural-language search: LLM-translated FTS5 plan +
//!   optional date range + hybrid FTS/vector ranking + RAG answer over the
//!   top results.
//! - `llm_status` — LLM availability for the UI (never the key).
//! - `embed_messages` — incremental local embedding job (new messages only).
//! - `embed_status` — embedding coverage for the UI (model, dim, pending).
//! - `import_mbox` — manual `.mbox` import (the demo ingest source).
//! - `import_chat_export` — WhatsApp `.txt` / Telegram `result.json` /
//!   Signal `.json` import with format auto-detection (the frontend picks
//!   the file via the dialog plugin).
//! - `sync_imap` — incremental IMAP/Gmail sync (first live mail source).
//! - `seed_demo` — insert the built-in sample messages (idempotent).
//! - `stats` — archive counts per source (drives the source filter chips).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use serde::Serialize;
use tauri::{Manager, State};
use vault_core::chat_export::{detect_chat_format, ChatFormat};
use vault_core::imap::{self, FolderSyncSummary, ImapConfig};
use vault_core::ingest::{ingest, Collected, IngestSummary, Source};
use vault_core::llm::{
    self, AnthropicLlm, AskOutcome, LlmConfig, LlmStatus, PlanSource, PlannedQuery, QueryPlan,
};
use vault_core::mbox::MboxSource;
use vault_core::query::{ChatContext, SearchPage, SearchQuery};
use vault_core::signal::SignalSource;
use vault_core::storage::Storage;
use vault_core::telegram::TelegramSource;
use vault_core::vector::{self, EmbedJobSummary, EmbeddingStatus, HashEmbedder};
use vault_core::whatsapp::WhatsappSource;
use vault_core::{demo, message, Error, Result as CoreResult};

mod secrets;

/// Shared app state: one open storage handle guarded by a mutex, the
/// optional LLM backend (`None` when no API key is configured), and the
/// local embedder behind hybrid ranking (offline, stateless).
struct AppState {
    storage: Arc<Mutex<Storage>>,
    llm: Option<AnthropicLlm>,
    embedder: HashEmbedder,
}

/// Result of an IMAP sync: per-folder summaries plus where an entered
/// password was kept (the OS keyring or session memory).
#[derive(Debug, Serialize)]
struct ImapSyncOutcome {
    folders: Vec<FolderSyncSummary>,
    /// `None` when a previously stored password was reused; `Some(store)`
    /// when the user just entered one and it was saved to `store`.
    saved_to: Option<secrets::SecretStore>,
}

fn lock<'a>(state: &'a State<'_, AppState>) -> Result<MutexGuard<'a, Storage>, String> {
    state.storage.lock().map_err(|e| e.to_string())
}

/// Paged, filtered full-text search over the archive.
///
/// `query.text` is sanitized into an FTS5 MATCH expression, so arbitrary
/// input can never be a query syntax error; an empty text browses newest
/// first under the same filters. Every response is bounded by
/// `limit`/`offset`; `total` carries the match count for pagination.
#[tauri::command]
fn search_messages(state: State<AppState>, query: SearchQuery) -> Result<SearchPage, String> {
    lock(&state)?.search_page(&query).map_err(|e| e.to_string())
}

/// One message plus up to `radius` adjacent messages of the same chat
/// thread (used by the detail view to link to neighbors).
#[tauri::command]
fn chat_context(
    state: State<AppState>,
    id: i64,
    radius: Option<u32>,
) -> Result<Option<ChatContext>, String> {
    lock(&state)?
        .chat_context(id, radius.unwrap_or(8))
        .map_err(|e| e.to_string())
}

/// Natural-language search: translate the question into an FTS5 plan
/// (MATCH expression + optional date range), run it locally, fuse the FTS
/// ranking with the local vector leg when embeddings exist, then summarize
/// the top results (RAG). Degrades to the plain term search whenever the LLM
/// is unavailable or misbehaves — search results never depend on the LLM
/// succeeding, and ranking never depends on embeddings existing. The
/// storage lock is never held across an LLM round trip.
#[tauri::command]
fn llm_ask(
    state: State<AppState>,
    question: String,
    limit: Option<u32>,
) -> Result<AskOutcome, String> {
    let question = question.trim();
    if question.is_empty() {
        return Ok(AskOutcome::empty());
    }
    if question.chars().count() > 2000 {
        return Err("question is too long (max 2000 characters)".into());
    }
    let limit = limit.unwrap_or(50).clamp(1, 200);
    let today = Utc::now().date_naive();

    // Same steps as [`vault_core::llm::ask`], composed so that each storage
    // lock is short-lived: translate (network) → validate (lock) → run +
    // hybrid-rank (lock, local CPU) → summarize (network).
    let planned = match &state.llm {
        None => PlannedQuery {
            plan: QueryPlan::plain(question),
            source: PlanSource::NoLlm,
            notes: Vec::new(),
        },
        Some(backend) => {
            let candidate = llm::translate(backend, question, today);
            let storage = lock(&state)?;
            llm::validate(&storage, question, candidate)
        }
    };
    let (results, ranking) = {
        let storage = lock(&state)?;
        let fts_results = llm::run_plan(&storage, &planned, limit).map_err(|e| e.to_string())?;
        let hybrid = llm::rank_hybrid(
            &storage,
            &state.embedder,
            question,
            &planned,
            &fts_results,
            limit,
        )
        .map_err(|e| e.to_string())?;
        (hybrid.results, hybrid.ranking)
    };
    let (answer, summary_notes) = match &state.llm {
        Some(backend) => {
            let summarized =
                llm::summarize_results(backend, question, &results, llm::DEFAULT_SUMMARIZE_TOP);
            (summarized.answer, summarized.notes)
        }
        None => (None, Vec::new()),
    };
    let mut notes = planned.notes;
    notes.extend(summary_notes);
    Ok(AskOutcome {
        plan: planned.plan,
        plan_source: planned.source,
        ranking,
        results,
        answer,
        notes,
    })
}

/// LLM availability for the UI: enabled flag, model, endpoint host, and how
/// many top results feed the summary. Never exposes the API key.
#[tauri::command]
fn llm_status(state: State<AppState>) -> LlmStatus {
    LlmStatus::from_config(state.llm.as_ref().map(AnthropicLlm::config))
}

/// Run the incremental local embedding job: embeds only messages still
/// lacking an embedding for the current model, in batches. Completely
/// offline and CPU-bound; large first runs can take a moment, so the work
/// runs off the UI thread.
#[tauri::command]
async fn embed_messages(
    state: State<'_, AppState>,
    limit: Option<u64>,
) -> Result<EmbedJobSummary, String> {
    let storage = Arc::clone(&state.storage);
    let embedder = state.embedder;
    tauri::async_runtime::spawn_blocking(move || {
        let storage = storage
            .lock()
            .map_err(|e| format!("storage lock poisoned: {e}"))?;
        vector::run_embedding_job(&storage, &embedder, limit).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("embedding task failed: {e}"))?
}

/// Local embedding coverage for the UI: model id, dimensionality, how many
/// messages are embedded, and how many are still pending.
#[tauri::command]
fn embed_status(state: State<AppState>) -> Result<EmbeddingStatus, String> {
    let storage = lock(&state)?;
    vector::embedding_status(&storage, &state.embedder).map_err(|e| e.to_string())
}

/// Import every message of an `.mbox` file (idempotent; re-importing skips
/// already-stored messages).
#[tauri::command]
fn import_mbox(state: State<AppState>, path: String) -> Result<IngestSummary, String> {
    let source = MboxSource::new(path);
    let storage = lock(&state)?;
    ingest(&storage, &source).map_err(|e: Error| e.to_string())
}

/// Import a chat export: a WhatsApp `.txt` export, a Telegram
/// `result.json`, or a Signal conversation `.json` (e.g. from
/// `sigtop export-messages -f json`, one file per conversation). The
/// format is detected from the file contents. Re-importing skips
/// already-stored messages.
#[tauri::command]
fn import_chat_export(state: State<AppState>, path: String) -> Result<IngestSummary, String> {
    let picked = PathBuf::from(&path);
    let file = if picked.is_dir() {
        let result_json = picked.join("result.json");
        if !result_json.is_file() {
            return Err(format!("no result.json found in {}", picked.display()));
        }
        result_json
    } else {
        picked
    };

    let bytes = std::fs::read(&file).map_err(|e| e.to_string())?;
    let source: Box<dyn Source> = match detect_chat_format(&bytes) {
        Some(ChatFormat::Telegram) => Box::new(TelegramSource::new(&file)),
        Some(ChatFormat::Signal) => Box::new(SignalSource::new(&file)),
        Some(ChatFormat::WhatsApp) => Box::new(WhatsappSource::new(&file)),
        None => {
            return Err(format!(
                "unrecognized chat export format: {} (expected a WhatsApp .txt export, a Telegram result.json, or a Signal conversation .json)",
                file.display()
            ))
        }
    };
    let storage = lock(&state)?;
    ingest(&storage, source.as_ref()).map_err(|e: Error| e.to_string())
}

/// Incrementally sync an IMAP account (Gmail via app password, or any
/// host/port/user/password server). Only messages new since the last sync
/// are fetched; re-syncing never duplicates.
///
/// Password handling: an entered password is used for this sync and kept in
/// the OS keyring when available, otherwise in session memory only — never
/// in the vault database. An empty password field reuses the stored one.
#[tauri::command]
async fn sync_imap(
    state: State<'_, AppState>,
    secrets: State<'_, secrets::Secrets>,
    host: String,
    port: Option<u16>,
    username: String,
    password: String,
    folders: Option<Vec<String>>,
) -> Result<ImapSyncOutcome, String> {
    // Email addresses and hostnames are case-insensitive, so one account
    // spelled with different casing must map to one dedup namespace
    // (`imap:<username>`) and one keyring entry (`user@host`).
    let host = host.trim().to_lowercase();
    let username = username.trim().to_lowercase();
    if host.is_empty() {
        return Err("host is required (e.g. imap.gmail.com)".into());
    }
    if username.is_empty() {
        return Err("username is required".into());
    }

    // Resolve the password: entered now (and remembered) or stored before.
    let (password, saved_to) = if password.is_empty() {
        let stored = secrets
            .load(&host, &username)
            .ok_or_else(|| "no password stored for this account — enter one".to_string())?;
        (stored, None)
    } else {
        let store = secrets.save(&host, &username, &password);
        (password, Some(store))
    };

    let config = ImapConfig {
        host,
        port: port.unwrap_or(imap::GMAIL_IMAP_PORT),
        username,
        password,
        folders: folders.unwrap_or_default(),
    };
    let storage = Arc::clone(&state.storage);
    // Network sync can take minutes; keep it off the UI thread.
    let folders = tauri::async_runtime::spawn_blocking(move || {
        let storage = storage
            .lock()
            .map_err(|e| format!("storage lock poisoned: {e}"))?;
        imap::sync(&config, &storage).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("sync task failed: {e}"))??;

    Ok(ImapSyncOutcome { folders, saved_to })
}

/// Insert the built-in demo messages so a fresh install has searchable data.
#[tauri::command]
fn seed_demo(state: State<AppState>) -> Result<IngestSummary, String> {
    let storage = lock(&state)?;
    ingest(&storage, &DemoSource).map_err(|e: Error| e.to_string())
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

    fn collect(&self) -> CoreResult<Collected> {
        Ok(Collected {
            messages: demo::messages(),
            failed: 0,
        })
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
            let storage = Storage::open(data_dir.join("vault.db")).map_err(|e| e.to_string())?;
            let llm = match LlmConfig::from_env() {
                Ok(config) => config.map(AnthropicLlm::new),
                Err(e) => {
                    eprintln!("LLM disabled: {e}");
                    None
                }
            };
            app.manage(AppState {
                storage: Arc::new(Mutex::new(storage)),
                llm,
                embedder: HashEmbedder::new(),
            });
            app.manage(secrets::Secrets::new());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search_messages,
            chat_context,
            llm_ask,
            llm_status,
            embed_messages,
            embed_status,
            import_mbox,
            import_chat_export,
            sync_imap,
            seed_demo,
            stats
        ])
        .run(tauri::generate_context!())
        .expect("error while running mailchat vault");
}

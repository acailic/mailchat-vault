//! Mailchat Vault desktop app: a thin Tauri shell over `vault-core`.
//!
//! Commands exposed to the frontend:
//! - `search_messages` — full-text search hitting SQLite FTS5.
//! - `llm_ask` — natural-language search: LLM-translated FTS5 plan +
//!   optional date range + RAG answer over the top results.
//! - `llm_status` — LLM availability for the UI (never the key).
//! - `import_mbox` — manual `.mbox` import (the demo ingest source).
//! - `import_chat_export` — WhatsApp `.txt` / Telegram `result.json`
//!   import with format auto-detection (the frontend picks the file via
//!   the dialog plugin).
//! - `seed_demo` — insert the built-in sample messages (idempotent).
//! - `stats` — archive counts per source.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use chrono::Utc;
use tauri::{Manager, State};
use vault_core::chat_export::{detect_chat_format, ChatFormat};
use vault_core::ingest::{ingest, Collected, IngestSummary, Source};
use vault_core::llm::{
    self, AnthropicLlm, AskOutcome, LlmConfig, LlmStatus, PlanSource, PlannedQuery, QueryPlan,
};
use vault_core::mbox::MboxSource;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;
use vault_core::telegram::TelegramSource;
use vault_core::whatsapp::WhatsappSource;
use vault_core::{demo, message, Error, Result as CoreResult};

/// Shared app state: one open storage handle guarded by a mutex, plus the
/// optional LLM backend (`None` when no API key is configured).
struct AppState {
    storage: Mutex<Storage>,
    llm: Option<AnthropicLlm>,
}

fn lock<'a>(state: &'a State<'_, AppState>) -> Result<MutexGuard<'a, Storage>, String> {
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

/// Natural-language search: translate the question into an FTS5 plan
/// (MATCH expression + optional date range), run it locally, then summarize
/// the top results (RAG). Degrades to the plain term search whenever the LLM
/// is unavailable or misbehaves — search results never depend on the LLM
/// succeeding. The storage lock is never held across an LLM round trip.
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
    // lock is short-lived: translate (network) → validate (lock) → run
    // (lock) → summarize (network).
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
    let results = {
        let storage = lock(&state)?;
        llm::run_plan(&storage, &planned, limit).map_err(|e| e.to_string())?
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

/// Import every message of an `.mbox` file (idempotent; re-importing skips
/// already-stored messages).
#[tauri::command]
fn import_mbox(state: State<AppState>, path: String) -> Result<IngestSummary, String> {
    let source = MboxSource::new(path);
    let storage = lock(&state)?;
    ingest(&storage, &source).map_err(|e: Error| e.to_string())
}

/// Import a chat export: a WhatsApp `.txt` export or a Telegram
/// `result.json` (a directory containing `result.json` works too). The
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
        Some(ChatFormat::WhatsApp) => Box::new(WhatsappSource::new(&file)),
        None => {
            return Err(format!(
                "unrecognized chat export format: {} (expected a WhatsApp .txt export or a Telegram result.json)",
                file.display()
            ))
        }
    };
    let storage = lock(&state)?;
    ingest(&storage, source.as_ref()).map_err(|e: Error| e.to_string())
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
                storage: Mutex::new(storage),
                llm,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search_messages,
            llm_ask,
            llm_status,
            import_mbox,
            import_chat_export,
            seed_demo,
            stats
        ])
        .run(tauri::generate_context!())
        .expect("error while running mailchat vault");
}

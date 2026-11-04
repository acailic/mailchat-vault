//! Mailchat Vault desktop app: a thin Tauri shell over `vault-core`.
//!
//! Since M2.1 the app opens the vault through [`vault_core::jobs::DbPool`]
//! (a dedicated writer worker thread over a bounded write queue plus two
//! read connections in WAL mode): every read command runs on an idle read
//! connection, every write is marshaled through the queue, and long work
//! (imports, IMAP sync, embedding) also exists as **v2 background jobs**
//! — `job_submit` starts one, `job_get`/`job_list` report their typed
//! states, `job_events_since` recovers missed `job-event` window events
//! by polling a monotonic cursor, and `job_cancel` stops one
//! cooperatively. No command ever performs network I/O while holding
//! storage: an IMAP sync fetches on its job thread and only its database
//! effects round-trip through the write queue.
//!
//! Commands exposed to the frontend:
//! - `search_messages` — paged, filtered full-text search hitting SQLite FTS5
//!   (relevance-ranked with match snippets, or newest-first browse).
//! - `chat_context` — a message plus its adjacent messages in the same chat.
//! - `llm_ask` — natural-language search: LLM-translated FTS5 plan +
//!   optional date range + hybrid FTS/vector ranking + RAG answer over the
//!   top results. The optional `provider` argument picks the LLM provider
//!   from the Ask menu (a settings profile, `env`, or `local` for
//!   local-only mode).
//! - `llm_status` — LLM availability for the UI: the default selection
//!   plus the full provider menu (never the key).
//! - `embed_messages` — incremental local embedding job (new messages only).
//! - `embed_status` — embedding coverage for the UI (model, dim, pending),
//!   behind hybrid ranking.
//! - `settings_get` / `settings_save` / `llm_key_clear` — the Settings UI:
//!   provider profiles, default provider, and app options live in
//!   `settings.json` in the app data dir; API keys live in the OS keyring
//!   (session-memory fallback) and never in the file or the database.
//! - `import_mbox` — manual `.mbox` import (the demo ingest source).
//! - `import_chat_export` — WhatsApp `.txt` / Telegram `result.json` /
//!   Signal `.json` / Discord Data Package import with format
//!   auto-detection and optional media extraction into the app data
//!   `media/` store (the frontend picks the file via the dialog plugin).
//! - `sync_imap` — incremental IMAP/Gmail sync (first live mail source):
//!   app password, or Google sign-in (OAuth2 → XOAUTH2) when `auth` says
//!   so.
//! - `sources_list` / `source_set_excluded` / `source_failures` — the
//!   M2.3 source surface: configured sources with their exclusion flag,
//!   per-source exclusion (skipped by coordinator rounds, archive kept),
//!   and the durable failed-item ledger (what fell, why, how often
//!   retried — visible, never silent).
//! - `watch_add_folder` / `watch_remove_folder` / `watch_status` —
//!   automatic folder watching: new/modified export files in user-chosen
//!   folders are imported in the background; the UI is kept current via
//!   `watch-status` window events.
//! - `oauth_gmail_login` — run the Gmail Google sign-in (browser OAuth2
//!   flow with PKCE) and store the refresh token in the OS keyring.
//! - `oauth_status` — Gmail sign-in availability for the UI (configured?
//!   signed in?), never any secret.
//! - `job_submit` / `job_get` / `job_list` / `job_events_since` /
//!   `job_cancel` — the v2 background-job protocol (see above); progress
//!   and state changes stream to the UI as `job-event` window events.
//!   Kinds: `seed_demo`, `import_mbox`, `import_chat_export`,
//!   `embed_messages`, `imap_sync` (one named account), and
//!   `imap_sync_all` (the M2.3 coordinator round over every configured,
//!   non-excluded IMAP source, credentials resolved per source from the
//!   keyring).
//! - `seed_demo` — insert the built-in sample messages (idempotent).
//! - `stats` — archive counts per source (drives the source filter chips).
//! - `archive_stats` — dashboard aggregates: totals, date range, counts by
//!   source/day, top chats and top authors.
//! - `export_search` — write the current search results (all pages, under
//!   the active filters) to a user-chosen file as CSV, JSON Lines or MBOX.
//! - `export_chat` — write the whole chat thread of one message to a
//!   user-chosen file in the same formats.

use std::collections::HashSet;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::Utc;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use vault_core::bounded::{import_id_for, BoundedRun, Flow, StreamItem, StreamSource};
use vault_core::chat_export::{self, ChatImportOutcome};
use vault_core::export::{self, ExportFormat, ExportSummary};
use vault_core::imap::{self, FolderSyncSummary, ImapAuth, ImapConfig};
use vault_core::imap_coordinator::{self, RoundSummary, StoredImapConfig};
use vault_core::ingest::IngestSummary;
use vault_core::jobs::{
    DbPool, JobContext, JobError, JobEventsPage, JobId, JobRecord, PoolConfig, PoolSink, PoolSync,
};
use vault_core::llm::{
    self, AnthropicLlm, AskOutcome, LlmConfig, LlmStatus, PlanSource, PlannedQuery, QueryPlan,
};
use vault_core::mbox::MboxSource;
use vault_core::oauth::{self, OAuthConfig};
use vault_core::object_store::{ObjectStore, ReconcileReport};
use vault_core::query::{ChatContext, SearchPage, SearchQuery};
use vault_core::scheduler::{SyncSchedule as CoreSyncSchedule, SyncScheduler};
use vault_core::search2::{
    self, AppError, ConversationContext, ImportScope, MessageDetail, SearchArchivePage,
    SearchArchiveRequest, SelectionExportSummary, SelectionFormat,
};
use vault_core::settings::{self, Options, Settings, SETTINGS_VERSION};
use vault_core::stats::{ArchiveStats, DEFAULT_TOP};
use vault_core::storage::{FailedItem, SourceInstance};
use vault_core::vector::{
    self, EmbedJobSummary, Embedder, EmbeddingStatus, HashEmbedder, EMBED_BODY_PREFIX_CHARS,
};
use vault_core::watch::{FolderWatcher, WatchSnapshot};
use vault_core::{demo, message, Error, Result as CoreResult};

mod registry;
mod secrets;

use registry::{LlmRegistry, SaveSettingsPayload, Selection, SettingsView};

/// Batch size of the v2 embedding job (mirrors `vector`'s internal batch;
/// progress is reported once per batch).
const EMBED_JOB_BATCH: u64 = 500;

/// Shared app state: the database pool (M2.1: writer worker + two read
/// connections), the LLM provider registry (settings profiles + keyring
/// keys + env fallback), the local offline embedder behind hybrid ranking
/// (stateless), the background folder watcher, and the opt-in periodic
/// sync scheduler (M2.4).
struct AppState {
    pool: Arc<DbPool>,
    registry: Arc<Mutex<LlmRegistry>>,
    /// Where `settings_save` writes the settings file.
    settings_path: PathBuf,
    embedder: HashEmbedder,
    watcher: FolderWatcher,
    scheduler: Arc<SyncScheduler>,
}

/// Result of an IMAP sync: per-folder summaries plus where an entered
/// password was kept (the OS keyring or session memory).
#[derive(Debug, Serialize)]
struct ImapSyncOutcome {
    folders: Vec<FolderSyncSummary>,
    /// `None` when a previously stored password was reused or the sync
    /// ran over OAuth2; `Some(store)` when the user just entered a
    /// password and it was saved to `store`.
    saved_to: Option<secrets::SecretStore>,
}

/// Result of a completed Gmail Google sign-in.
#[derive(Debug, Serialize)]
struct OAuthLoginOutcome {
    /// The account that signed in (normalized to lowercase).
    username: String,
    /// Where the refresh token was kept.
    saved_to: secrets::SecretStore,
}

/// Gmail sign-in availability for the UI; never includes a secret.
#[derive(Debug, Serialize)]
struct OAuthStatus {
    /// Whether the OAuth client credentials are configured (env vars).
    configured: bool,
    /// Whether a refresh token is stored for the account.
    signed_in: bool,
}

/// The ticket `job_submit` returns.
#[derive(Debug, Serialize)]
struct JobHandle {
    id: JobId,
}

/// Paged, filtered full-text search over the archive.
///
/// `query.text` is sanitized into an FTS5 MATCH expression, so arbitrary
/// input can never be a query syntax error; an empty text browses newest
/// first under the same filters. Every response is bounded by
/// `limit`/`offset`; `total` carries the match count for pagination.
#[tauri::command]
fn search_messages(state: State<AppState>, query: SearchQuery) -> Result<SearchPage, String> {
    state
        .pool
        .with_read(|s| s.search_page(&query))
        .map_err(|e| e.to_string())
}

/// One message plus up to `radius` adjacent messages of the same chat
/// thread (used by the detail view to link to neighbors).
#[tauri::command]
fn chat_context(
    state: State<AppState>,
    id: i64,
    radius: Option<u32>,
) -> Result<Option<ChatContext>, String> {
    state
        .pool
        .with_read(|s| s.chat_context(id, radius.unwrap_or(8)))
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// v2 search/detail/context (M3.2)
// ---------------------------------------------------------------------------
//
// The v2 IPC surface reads the canonical store through `search2` and
// returns summaries (bounded snippets, no full bodies) plus the archive
// revision of the read snapshot; errors are structured `AppError`s
// (code/message/field/retryable), not strings. The v1 commands above
// stay as adapters until every caller migrates.

/// Paged, filtered v2 search: flat stable ranking (relevance = rank then
/// id; newest = instant desc, id desc, unknown last), bounded snippets,
/// exact total from one read snapshot, and that snapshot's archive
/// revision so the UI can offer a refresh instead of silently restarting
/// pagination when the archive changes (A07).
#[tauri::command]
fn search_archive_v2(
    state: State<AppState>,
    request: SearchArchiveRequest,
) -> Result<SearchArchivePage, AppError> {
    state
        .pool
        .with_read(|s| s.search_archive(&request))
        .map_err(AppError::from)
}

/// The full v2 reading payload for one message: body, metadata, time
/// provenance, attachments, occurrences, and conflicting-revision links.
#[tauri::command]
fn message_detail_v2(state: State<AppState>, id: i64) -> Result<MessageDetail, AppError> {
    state
        .pool
        .with_read(|s| s.message_detail(id))
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found(format!("no message with id {id}")))
}

/// Chronological context around one message, paged by message-id cursors
/// ("load earlier/later" without losing the anchor). Unverified legacy
/// groupings return the anchor alone with `unverified_conversation` set.
#[tauri::command]
fn conversation_context_v2(
    state: State<AppState>,
    id: i64,
    before_cursor: Option<i64>,
    after_cursor: Option<i64>,
    limit: Option<u32>,
) -> Result<ConversationContext, AppError> {
    state
        .pool
        .with_read(|s| s.conversation_context(id, before_cursor, after_cursor, limit))
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found(format!("no message with id {id}")))
}

/// The import scopes available as a results filter ("View imported
/// messages" after an import completes), newest first.
#[tauri::command]
fn list_import_scopes_v2(state: State<AppState>) -> Result<Vec<ImportScope>, AppError> {
    state
        .pool
        .with_read(|s| s.import_scopes())
        .map_err(AppError::from)
}

/// Reconcile the media object store against the archive (M4.1): every
/// `raw_json` media reference must have its SHA-256 object in the app
/// data `media/` store, and every object is checked against those
/// references. The report names missing references (external damage —
/// the stage → promote → reference lifecycle cannot produce them),
/// unreferenced orphans, corrupt objects (content hash ≠ name digest,
/// re-hashed with `verify`), staging litter from interrupted writes,
/// and foreign files. Read-only: repairing litter is a separate
/// explicit sweep (`ObjectStore::sweep_staging`), corruption recovery
/// stays manual by design.
#[tauri::command]
async fn media_reconcile(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReconcileReport, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let digests = state
        .pool
        .with_read(|s| s.media_digests())
        .map_err(|e| e.to_string())?;
    let verify = true; // maintenance call: pay the re-hash for certainty
    tauri::async_runtime::spawn_blocking(move || {
        ObjectStore::new(&data_dir).reconcile(&digests, verify)
    })
    .await
    .map_err(|e| format!("reconcile task failed: {e}"))?
    .map_err(|e| e.to_string())
}

/// Export an explicit selection of messages (the rows the user checked
/// in the results list, in that order) as UTF-8 text or JSON with
/// provenance, to a natively-picked destination. Attachments are never
/// implicitly included — they are a separate explicit export (M4).
#[tauri::command]
async fn export_messages_v2(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    format: String,
    path: String,
) -> Result<SelectionExportSummary, AppError> {
    let format = SelectionFormat::parse(&format).map_err(AppError::from)?;
    if ids.is_empty() {
        return Err(AppError::invalid_input(
            "selection export needs at least one message id",
            Some("ids"),
        ));
    }
    if ids.len() > 10_000 {
        return Err(AppError::invalid_input(
            "selection export is bounded to 10,000 messages per file",
            Some("ids"),
        ));
    }
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        pool.with_read(|s| -> CoreResult<SelectionExportSummary> {
            let file = std::fs::File::create(&path).map_err(Error::from)?;
            let mut out = BufWriter::new(file);
            search2::export_selection(s, &ids, format, &mut out)
        })
        .map_err(AppError::from)
    })
    .await
    .map_err(|e| AppError::internal(format!("export task failed: {e}")))?
}

/// Natural-language search: translate the question into an FTS5 plan
/// (MATCH expression + optional date range), run it locally, fuse the FTS
/// ranking with the local vector leg when embeddings exist, then summarize
/// the top results (RAG). Degrades to the plain term search whenever the LLM
/// is unavailable or misbehaves — search results never depend on the LLM
/// succeeding, and ranking never depends on embeddings existing. Reads run
/// on the pool's read connections; the LLM round trips hold no storage.
///
/// `provider` optionally picks the backend for this ask: a settings
/// profile id, `env` (the environment fallback), or `local` (plain local
/// search, nothing sent anywhere). `None` uses the default selection
/// (settings default → env fallback → local-only).
#[tauri::command]
fn llm_ask(
    state: State<AppState>,
    question: String,
    limit: Option<u32>,
    provider: Option<String>,
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

    // Resolve the backend without holding the registry lock across any
    // network round trip: the selection clones its LlmConfig out.
    let backend: Option<AnthropicLlm>;
    let mut local_source: Option<PlanSource> = None;
    let mut local_note: Option<String> = None;
    let summarize_top: usize;
    {
        let registry = state
            .registry
            .lock()
            .map_err(|e| format!("registry lock poisoned: {e}"))?;
        summarize_top = registry.settings().options.summarize_top as usize;
        match registry.select(provider.as_deref()) {
            Selection::Provider { config, .. } => backend = Some(AnthropicLlm::new(config)),
            Selection::Local { reason } => {
                backend = None;
                // An explicitly requested (but unusable) provider is local
                // mode with a note; no selection at all with nothing
                // configured stays the classic no-LLM mode.
                local_source = Some(if provider.is_some() {
                    PlanSource::Local
                } else {
                    PlanSource::NoLlm
                });
                if !reason.is_empty() {
                    local_note = Some(reason);
                }
            }
        }
    }

    // Same steps as [`vault_core::llm::ask`], composed so that each read
    // is short-lived: translate (network, no storage) → validate (read) →
    // run + hybrid-rank (read, local CPU) → summarize (network).
    let planned = match &backend {
        None => PlannedQuery {
            plan: QueryPlan::plain(question),
            source: local_source.expect("local selection always sets a source"),
            notes: local_note.into_iter().collect(),
        },
        Some(backend) => {
            let candidate = llm::translate(backend, question, today);
            state
                .pool
                .with_read(|s| llm::validate(s, question, candidate))
        }
    };
    let (results, ranking) = {
        let fts_results = state
            .pool
            .with_read(|s| llm::run_plan(s, &planned, limit))
            .map_err(|e| e.to_string())?;
        let hybrid = state
            .pool
            .with_read(|s| {
                llm::rank_hybrid(s, &state.embedder, question, &planned, &fts_results, limit)
            })
            .map_err(|e| e.to_string())?;
        (hybrid.results, hybrid.ranking)
    };
    let (answer, summary_notes) = match &backend {
        Some(backend) => {
            let summarized = llm::summarize_results(backend, question, &results, summarize_top);
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

/// LLM availability for the UI: the default selection (enabled flag,
/// model, endpoint host) plus the full provider menu and how many top
/// results feed the summary. Never exposes any API key.
#[tauri::command]
fn llm_status(state: State<AppState>) -> Result<LlmStatus, String> {
    let registry = state
        .registry
        .lock()
        .map_err(|e| format!("registry lock poisoned: {e}"))?;
    Ok(registry.status())
}

/// Everything the Settings UI shows: provider profiles (with a `has_key`
/// flag — never the keys), the default selection, options, and the
/// environment-fallback diagnostics.
#[tauri::command]
fn settings_get(state: State<AppState>) -> Result<SettingsView, String> {
    let registry = state
        .registry
        .lock()
        .map_err(|e| format!("registry lock poisoned: {e}"))?;
    Ok(SettingsView::from(&*registry))
}

/// Save the settings file and apply API-key updates. Order matters:
/// validate first (an invalid configuration is refused with the pairing
/// error and nothing is written), then the file, then the keyring — so a
/// failed save never leaves keys orphaned in the keyring or an invalid
/// file on disk. Returns the fresh view the UI re-renders from.
#[tauri::command]
fn settings_save(
    state: State<'_, AppState>,
    secrets: State<'_, Arc<secrets::Secrets>>,
    payload: SaveSettingsPayload,
) -> Result<SettingsView, String> {
    let mut settings = Settings {
        version: SETTINGS_VERSION,
        providers: payload.providers,
        default_provider: payload.default_provider,
        options: Options {
            theme: payload.theme,
            summarize_top: payload.summarize_top,
        },
    };
    settings.normalize();
    settings.validate().map_err(|e| e.to_string())?;

    // Key updates must reference configured providers (ids are opaque, so
    // a stale id from a removed row is a client bug we refuse, not ignore).
    let ids: HashSet<&str> = settings.providers.iter().map(|p| p.id.as_str()).collect();
    for update in &payload.api_keys {
        let id = update.id.trim();
        if !ids.contains(id) {
            return Err(format!("API key update for unknown provider “{id}”"));
        }
    }

    settings
        .save(&state.settings_path)
        .map_err(|e| e.to_string())?;

    // Keys only change once the file is safely written.
    let mut key_store = None;
    for update in &payload.api_keys {
        let id = update.id.trim();
        let key = update.key.trim();
        if key.is_empty() {
            secrets.clear_llm_key(id);
        } else {
            key_store = Some(secrets.save_llm_key(id, key));
        }
    }
    // Forget the keys of providers that no longer exist (keyring hygiene).
    let old_ids: Vec<String> = {
        let registry = state
            .registry
            .lock()
            .map_err(|e| format!("registry lock poisoned: {e}"))?;
        registry
            .settings()
            .providers
            .iter()
            .map(|p| p.id.clone())
            .collect()
    };
    for id in old_ids {
        if !ids.contains(id.as_str()) {
            secrets.clear_llm_key(&id);
        }
    }

    let mut registry = state
        .registry
        .lock()
        .map_err(|e| format!("registry lock poisoned: {e}"))?;
    registry.update(settings, key_store, |id| secrets.load_llm_key(id));
    Ok(SettingsView::from(&*registry))
}

/// Remove one provider's API key from the keyring (and session memory).
/// The settings file is untouched.
#[tauri::command]
fn llm_key_clear(
    state: State<'_, AppState>,
    secrets: State<'_, Arc<secrets::Secrets>>,
    id: String,
) -> Result<SettingsView, String> {
    let id = id.trim();
    secrets.clear_llm_key(id);
    let mut registry = state
        .registry
        .lock()
        .map_err(|e| format!("registry lock poisoned: {e}"))?;
    registry.drop_key(id);
    Ok(SettingsView::from(&*registry))
}

/// Run the incremental local embedding job: embeds only messages still
/// lacking an embedding for the current model, in batches. Completely
/// offline and CPU-bound; runs on the writer through the bounded queue.
#[tauri::command]
async fn embed_messages(state: State<'_, AppState>) -> Result<EmbedJobSummary, String> {
    let pool = Arc::clone(&state.pool);
    let embedder = state.embedder;
    tauri::async_runtime::spawn_blocking(move || {
        pool.submit_write(move |s| vector::run_embedding_job(s, &embedder, None))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("embedding task failed: {e}"))?
}

/// Local embedding coverage for the UI: model id, dimensionality, how many
/// messages are embedded, and how many are still pending.
#[tauri::command]
fn embed_status(state: State<AppState>) -> Result<EmbeddingStatus, String> {
    state
        .pool
        .with_read(|s| vector::embedding_status(s, &state.embedder))
        .map_err(|e| e.to_string())
}

/// Import every message of an `.mbox` file (idempotent; re-importing
/// skips already-stored messages). M2.2: the import streams — the file is
/// parsed one message at a time on this blocking task, and batches commit
/// atomically through the pool's write queue with checkpoint advancement,
/// so arbitrarily large files import under a bounded memory budget.
#[tauri::command]
async fn import_mbox(state: State<'_, AppState>, path: String) -> Result<IngestSummary, String> {
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        let source = MboxSource::new(&path);
        let run = BoundedRun::new(import_id_for("mbox-import", Path::new(&path)));
        let mut sink = PoolSink(&pool);
        let mut no_progress = |_: u64, _: u64, _: &str| {};
        run.run(&source, &mut sink, &|| false, &mut no_progress)
            .map(|outcome| outcome.summary)
            .map_err(|e: Error| e.to_string())
    })
    .await
    .map_err(|e| format!("import task failed: {e}"))?
}

/// Import a chat export: a WhatsApp `.txt` export, a Telegram `result.json`
/// (a directory containing it works too), a Signal conversation `.json`
/// (e.g. from `sigtop export-messages -f json`, one file per conversation),
/// or a Discord Data Package messages export — a single channel
/// `messages.json`, the package's `messages/` directory, or the package
/// root. The format is detected from the file contents. Re-importing skips
/// already-stored messages.
///
/// With `extract_media` (default `true`), media files referenced by the
/// export are copied into the app data `media/` store (hash-named,
/// content-deduplicated) and linked from each message; messages already
/// in the archive get their links backfilled, so re-importing an export
/// with media enriches it.
///
/// M2.2: the import streams — detection reads bounded views, the parser
/// never holds the archive, and batches commit atomically through the
/// pool's write queue with checkpoint advancement (an interrupted import
/// resumes from its committed position when re-invoked on the same file).
#[tauri::command]
async fn import_chat_export(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    extract_media: Option<bool>,
) -> Result<ChatImportOutcome, String> {
    let extract_media = extract_media.unwrap_or(true);
    let data_dir = if extract_media {
        Some(app.path().app_data_dir().map_err(|e| e.to_string())?)
    } else {
        None
    };
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        let mut sink = PoolSink(&pool);
        let mut no_progress = |_: u64, _: u64, _: &str| {};
        chat_export::import_bounded(
            &mut sink,
            Path::new(&path),
            extract_media,
            data_dir.as_deref(),
            &|| false,
            &mut no_progress,
        )
        .map(ChatImportOutcome::from)
        .map_err(|e: Error| e.to_string())
    })
    .await
    .map_err(|e| format!("import task failed: {e}"))?
}

/// Incrementally sync an IMAP account. Two auth modes:
///
/// - `"password"` (default): host/port/user + app password (Gmail) or
///   account password. An entered password is used for this sync and kept
///   in the OS keyring when available, otherwise in session memory only —
///   never in the vault database. An empty password field reuses the
///   stored one.
/// - `"oauth"`: Gmail Google sign-in — the stored OAuth2 refresh token
///   (from [`oauth_gmail_login`], OS keyring) is exchanged for a fresh
///   access token and the sync authenticates via SASL XOAUTH2. Requires
///   the `MAILCHAT_GMAIL_CLIENT_ID`/`_SECRET` env vars to be set.
///
/// Only messages new since the last sync are fetched; re-syncing never
/// duplicates. Password handling: an entered password is used for this
/// sync and kept in the OS keyring when available, otherwise in session
/// memory only — never in the vault database. An empty password field
/// reuses the stored one.
///
/// M2.1: the sync's network waits happen on this blocking task; every
/// database effect (instance, cursors, inserts) round-trips through the
/// pool's bounded write queue ([`PoolSync`]) — searches keep working
/// during a sync, and no transaction ever spans a network read.
///
/// The flat argument list mirrors the sync form's fields, like every other
/// Tauri command here.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn sync_imap(
    state: State<'_, AppState>,
    secrets: State<'_, Arc<secrets::Secrets>>,
    host: String,
    port: Option<u16>,
    username: String,
    password: String,
    folders: Option<Vec<String>>,
    auth: Option<String>,
) -> Result<ImapSyncOutcome, String> {
    // Hostnames are case-insensitive, so one server spelled with different
    // casing must map to one durable source instance. Usernames stay
    // exactly as configured: the identity contract case-folds hosts, not
    // usernames (see ImapConfig::connector_key), so two casings of a
    // username are distinct accounts until the server says otherwise.
    let host = host.trim().to_lowercase();
    let username = username.trim().to_string();
    if username.is_empty() {
        return Err("username is required".into());
    }
    let folders = folders.unwrap_or_default();
    let port = port.unwrap_or(imap::GMAIL_IMAP_PORT);

    let pool = Arc::clone(&state.pool);
    let secrets = Arc::clone(secrets.inner());
    // Secret resolution (keyring), token refresh (blocking HTTP), and the
    // network sync itself all block; keep them off the UI thread.
    let (folders, saved_to) = tauri::async_runtime::spawn_blocking(move || {
        let auth = auth.as_deref().unwrap_or("password");
        // Only the Gmail sign-in flow has a sensible default host; every
        // other server must be named explicitly.
        if host.is_empty() && auth != "oauth" {
            return Err("host is required (e.g. imap.gmail.com)".to_string());
        }
        let host = if host.is_empty() {
            imap::GMAIL_IMAP_HOST.to_string()
        } else {
            host
        };
        let (imap_auth, saved_to) = match auth {
            "password" => {
                // Resolve the password: entered now (and remembered) or
                // stored before.
                let (password, saved_to) = if password.is_empty() {
                    let stored = secrets.load(&host, &username).ok_or_else(|| {
                        "no password stored for this account — enter one".to_string()
                    })?;
                    (stored, None)
                } else {
                    let store = secrets.save(&host, &username, &password);
                    (password, Some(store))
                };
                (ImapAuth::Password(password), saved_to)
            }
            "oauth" => {
                // Refresh-token sign-in: mint a fresh access token and use
                // SASL XOAUTH2. The refresh token itself never leaves the
                // secret store (and never enters the vault database).
                let config = oauth_config()?;
                let refresh = secrets.load_refresh(&host, &username).ok_or_else(|| {
                    "no Google sign-in stored for this account — use “Sign in with Google” first"
                        .to_string()
                })?;
                let tokens =
                    oauth::refresh_access_token(&config, &refresh).map_err(|e| e.to_string())?;
                (
                    ImapAuth::Xoauth2 {
                        access_token: tokens.access_token,
                    },
                    None,
                )
            }
            other => {
                return Err(format!(
                    "unknown auth mode {other:?} (expected \"password\" or \"oauth\")"
                ))
            }
        };

        let config = ImapConfig {
            host,
            port,
            username,
            auth: imap_auth,
            folders,
        };
        imap::sync_with(&config, &PoolSync(&pool))
            .map_err(|e| e.to_string())
            .map(|folders| (folders, saved_to))
    })
    .await
    .map_err(|e| format!("sync task failed: {e}"))??;

    Ok(ImapSyncOutcome { folders, saved_to })
}

/// The OAuth client credentials from the environment, or a clear error
/// telling the user what to register and set (see README → IMAP sync).
fn oauth_config() -> Result<OAuthConfig, String> {
    oauth::OAuthConfig::from_env()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "Gmail sign-in is not configured: set MAILCHAT_GMAIL_CLIENT_ID and \
             MAILCHAT_GMAIL_CLIENT_SECRET to your own Google OAuth client (see README → \
             IMAP sync → Gmail (Google sign-in))"
                .to_string()
        })
}

/// Run the Gmail Google sign-in: open the system browser at Google's
/// consent page (PKCE + loopback redirect), wait for the redirect,
/// exchange the code, and store the refresh token in the OS keyring
/// (session memory fallback). Blocks until the sign-in completes or
/// times out (~5 min). The short-lived access token is not kept — every
/// sync mints a fresh one from the refresh token.
#[tauri::command]
async fn oauth_gmail_login(
    secrets: State<'_, Arc<secrets::Secrets>>,
    host: String,
    username: String,
) -> Result<OAuthLoginOutcome, String> {
    let host = host.trim().to_lowercase();
    let host = if host.is_empty() {
        imap::GMAIL_IMAP_HOST.to_string()
    } else {
        host
    };
    let username = username.trim().to_lowercase();
    if username.is_empty() {
        return Err("username (your Gmail address) is required".into());
    }
    let config = oauth_config()?;
    let hint = username.clone();
    let secrets = Arc::clone(secrets.inner());
    // Browser flow, token exchange, and the keyring write all block; keep
    // them off the UI thread.
    let outcome = tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
        let tokens = oauth::run_login(
            &config,
            Some(&hint),
            |url| webbrowser::open(url).map_err(|e| e.to_string()),
            oauth::LOGIN_TIMEOUT,
        )
        .map_err(|e| e.to_string())?;
        let refresh = tokens.refresh_token.ok_or_else(|| {
            "Google returned no refresh token — consent must be granted; retry the sign-in"
                .to_string()
        })?;
        let saved_to = secrets.save_refresh(&host, &username, &refresh);
        Ok(OAuthLoginOutcome { username, saved_to })
    })
    .await
    .map_err(|e| format!("sign-in task failed: {e}"))??;

    Ok(outcome)
}

/// Gmail sign-in availability for the UI: whether the OAuth client is
/// configured (env vars set) and whether the account already has a
/// stored sign-in. Never exposes any secret.
#[tauri::command]
async fn oauth_status(
    secrets: State<'_, Arc<secrets::Secrets>>,
    host: Option<String>,
    username: Option<String>,
) -> Result<OAuthStatus, String> {
    let secrets = Arc::clone(secrets.inner());
    // The keyring read can block on the OS; keep it off the UI thread.
    tauri::async_runtime::spawn_blocking(move || {
        let configured = OAuthConfig::from_env()
            .map(|c| c.is_some())
            .unwrap_or(false);
        let signed_in = match username {
            Some(username) if !username.trim().is_empty() => {
                let host = host
                    .map(|h| h.trim().to_lowercase())
                    .filter(|h| !h.is_empty())
                    .unwrap_or_else(|| imap::GMAIL_IMAP_HOST.to_string());
                let username = username.trim().to_lowercase();
                secrets.load_refresh(&host, &username).is_some()
            }
            _ => false,
        };
        Ok(OAuthStatus {
            configured,
            signed_in,
        })
    })
    .await
    .map_err(|e| format!("status task failed: {e}"))?
    .map_err(|e: String| format!("status check failed: {e}"))
}

/// Insert the built-in demo messages so a fresh install has searchable
/// data (streamed through the bounded pipeline like every other import).
#[tauri::command]
async fn seed_demo(state: State<'_, AppState>) -> Result<IngestSummary, String> {
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        let run = BoundedRun::new("demo-seed");
        let mut sink = PoolSink(&pool);
        let mut no_progress = |_: u64, _: u64, _: &str| {};
        run.run(&DemoSource, &mut sink, &|| false, &mut no_progress)
            .map(|outcome| outcome.summary)
            .map_err(|e: Error| e.to_string())
    })
    .await
    .map_err(|e| format!("seed task failed: {e}"))?
}

/// Arm a watched folder: existing and new/modified export files
/// (`.mbox`, WhatsApp `.txt`, Telegram `.json`, Signal conversation
/// `.json`) under it are imported in the background; re-imports never
/// duplicate. Returns the fresh status.
#[tauri::command]
fn watch_add_folder(state: State<AppState>, path: String) -> Result<WatchSnapshot, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("pick a folder to watch first".into());
    }
    state.watcher.add_folder(path)?;
    Ok(state.watcher.snapshot())
}

/// Forget a watched folder (removing one that no longer exists is fine).
#[tauri::command]
fn watch_remove_folder(state: State<AppState>, path: String) -> Result<WatchSnapshot, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("path is required".into());
    }
    state.watcher.remove_folder(path)?;
    Ok(state.watcher.snapshot())
}

/// Current watcher status: watched folders, pending files, recent events.
#[tauri::command]
fn watch_status(state: State<AppState>) -> WatchSnapshot {
    state.watcher.snapshot()
}

// ---------------------------------------------------------------------------
// v2 background jobs
// ---------------------------------------------------------------------------

/// What one `job_submit` starts (validated `kind` + parameters). Kept as a
/// pure value so parameter validation is testable without app state.
#[derive(Debug, PartialEq)]
enum JobSpec {
    SeedDemo,
    ImportMbox {
        path: String,
    },
    ImportChatExport {
        path: String,
        extract_media: bool,
    },
    EmbedMessages,
    ImapSync {
        host: String,
        port: u16,
        username: String,
        password: String,
        folders: Vec<String>,
    },
    ImapSyncAll,
}

impl JobSpec {
    /// The `kind` string reported in job records and events.
    fn kind(&self) -> &'static str {
        match self {
            JobSpec::SeedDemo => "seed_demo",
            JobSpec::ImportMbox { .. } => "import_mbox",
            JobSpec::ImportChatExport { .. } => "import_chat_export",
            JobSpec::EmbedMessages => "embed_messages",
            JobSpec::ImapSync { .. } => "imap_sync",
            JobSpec::ImapSyncAll => "imap_sync_all",
        }
    }

    /// Parse and validate a `{kind, params}` request. Field errors name
    /// the field; unknown kinds list the supported set.
    fn parse(kind: &str, params: &Value) -> Result<Self, String> {
        let string_field = |name: &str| -> Result<String, String> {
            params
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| format!("params.{name} is required"))
        };
        match kind {
            "seed_demo" | "embed_messages" => Ok(match kind {
                "seed_demo" => JobSpec::SeedDemo,
                _ => JobSpec::EmbedMessages,
            }),
            "import_mbox" => Ok(JobSpec::ImportMbox {
                path: string_field("path")?,
            }),
            "import_chat_export" => {
                let extract_media = params
                    .get("extract_media")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                Ok(JobSpec::ImportChatExport {
                    path: string_field("path")?,
                    extract_media,
                })
            }
            "imap_sync" => {
                let port = params
                    .get("port")
                    .and_then(Value::as_u64)
                    .unwrap_or(imap::GMAIL_IMAP_PORT as u64);
                if !(1..=65535).contains(&port) {
                    return Err("params.port must be a valid port".into());
                }
                let folders = params
                    .get("folders")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(JobSpec::ImapSync {
                    host: string_field("host")?.trim().to_lowercase(),
                    port: port as u16,
                    username: string_field("username")?.trim().to_string(),
                    // Parity with the v1 sync command: an empty (or
                    // absent) password means "resolve from the stored
                    // secret" — resolved in `job_submit` before the job
                    // body runs.
                    password: params
                        .get("password")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    folders,
                })
            }
            // The coordinator round: no parameters — it syncs every
            // configured, non-excluded IMAP source (M2.3).
            "imap_sync_all" => Ok(JobSpec::ImapSyncAll),
            other => Err(format!(
                "unknown job kind “{other}” (supported: seed_demo, import_mbox, \
                 import_chat_export, embed_messages, imap_sync, imap_sync_all)"
            )),
        }
    }
}

/// Two-way password parity with the v1 sync command: an entered password
/// is remembered (keyring) and used for the run; an empty one resolves
/// the stored secret, erroring when none is stored.
fn resolve_imap_password(
    secrets: &secrets::Secrets,
    host: &str,
    username: &str,
    password: &str,
) -> Result<String, String> {
    if password.is_empty() {
        secrets
            .load(host, username)
            .ok_or_else(|| "no password stored for this account — enter one".to_string())
    } else {
        secrets.save(host, username, password);
        Ok(password.to_string())
    }
}

/// The coordinator round's credential resolution, per source: a stored
/// app password first; otherwise a stored Google sign-in refreshed into
/// a fresh access token (SASL XOAUTH2). `Ok(None)` — the source is
/// reported as [`vault_core::imap_coordinator::SourceSyncStatus::NoCredentials`] —
/// strictly means nothing stored; an `Err` means stored credentials
/// exist but could not be used (no OAuth client configuration, or the
/// token refresh failed). Secrets stay in the keyring/session memory;
/// only the short-lived token exists for the round, in memory.
fn resolve_imap_auth(
    secrets: &secrets::Secrets,
    oauth: Option<&OAuthConfig>,
    stored: &StoredImapConfig,
) -> Result<Option<ImapAuth>, String> {
    if let Some(password) = secrets.load(&stored.host, &stored.username) {
        return Ok(Some(ImapAuth::Password(password)));
    }
    let Some(refresh) = secrets.load_refresh(&stored.host, &stored.username) else {
        return Ok(None);
    };
    let Some(config) = oauth else {
        return Err("Google sign-in configured but no OAuth client credentials found".to_string());
    };
    let tokens = oauth::refresh_access_token(config, &refresh).map_err(|e| e.to_string())?;
    Ok(Some(ImapAuth::Xoauth2 {
        access_token: tokens.access_token,
    }))
}

/// Submit a background job (the v2 protocol). Returns immediately with a
/// [`JobHandle`]; follow the job through `job-event` window events and
/// recover missed ones with `job_events_since`.
#[tauri::command]
fn job_submit(
    app: AppHandle,
    state: State<AppState>,
    secrets: State<'_, Arc<secrets::Secrets>>,
    kind: String,
    params: Value,
) -> Result<JobHandle, String> {
    let spec = JobSpec::parse(&kind, &params)?;
    let pool = Arc::clone(&state.pool);
    let embedder = state.embedder;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    // Parity with the v1 sync command: an entered password is kept in the
    // keyring; an empty one resolves the stored secret. Resolved here (on
    // the command thread) so the job body carries a ready config.
    let spec = match spec {
        JobSpec::ImapSync {
            host,
            port,
            username,
            password,
            folders,
        } => {
            let password = resolve_imap_password(&secrets, &host, &username, &password)?;
            JobSpec::ImapSync {
                host,
                port,
                username,
                password,
                folders,
            }
        }
        other => other,
    };
    let job_pool = Arc::clone(&pool);
    let secrets = Arc::clone(secrets.inner());
    pool.try_submit_job(spec.kind(), move |ctx: &JobContext| {
        run_job_spec(ctx, &spec, &job_pool, embedder, &data_dir, &secrets)
    })
    .map(|id| JobHandle { id })
    .map_err(|e| e.to_string())
}

/// Execute one validated [`JobSpec`] on the job runner thread. All storage
/// access goes through the pool; every kind reports progress and a JSON
/// result summary. (Unit-tested through `JobSpec::parse`; execution is
/// covered end-to-end by vault-core's jobs and coordinator tests.)
fn run_job_spec(
    ctx: &JobContext,
    spec: &JobSpec,
    pool: &DbPool,
    embedder: HashEmbedder,
    data_dir: &Path,
    secrets: &Arc<secrets::Secrets>,
) -> std::result::Result<Value, JobError> {
    match spec {
        JobSpec::SeedDemo => {
            if ctx.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            let source = DemoSource;
            let outcome = run_bounded(ctx, pool, "demo-seed", &source, None, "demo")?;
            Ok(serde_json::to_value(&outcome.summary).unwrap_or(Value::Null))
        }
        JobSpec::ImportMbox { path } => {
            if ctx.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            let source = MboxSource::new(path);
            let import_id = import_id_for("mbox-import", Path::new(path));
            let outcome = run_bounded(ctx, pool, import_id, &source, None, path)?;
            Ok(serde_json::to_value(&outcome.summary).unwrap_or(Value::Null))
        }
        JobSpec::ImportChatExport {
            path,
            extract_media,
        } => {
            if ctx.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            let extract = *extract_media;
            let media_dir = if extract {
                Some(data_dir.to_path_buf())
            } else {
                None
            };
            let moved_path = path.clone();
            let mut sink = PoolSink(pool);
            let cancelled = || ctx.is_cancelled();
            let progress =
                |done: u64, total: u64, message: &str| ctx.progress(done, total, message);
            let mut progress = progress;
            let outcome = chat_export::import_bounded(
                &mut sink,
                Path::new(&moved_path),
                extract,
                media_dir.as_deref(),
                &cancelled,
                &mut progress,
            )?;
            if outcome.cancelled {
                return Err(JobError::Cancelled);
            }
            Ok(serde_json::to_value(ChatImportOutcome::from(outcome)).unwrap_or(Value::Null))
        }
        JobSpec::EmbedMessages => run_embed_job(ctx, embedder),
        JobSpec::ImapSync {
            host,
            port,
            username,
            password,
            folders,
        } => {
            let config = ImapConfig {
                host: host.clone(),
                port: *port,
                username: username.clone(),
                auth: ImapAuth::Password(password.clone()),
                folders: folders.clone(),
            };
            // Network on this thread; each storage call round-trips the
            // bounded write queue via the pool adapter.
            let sink = PoolSync(pool);
            let summaries = imap::sync_with(&config, &sink)?;
            Ok(serde_json::to_value(&summaries).unwrap_or(Value::Null))
        }
        JobSpec::ImapSyncAll => run_sync_all_round(ctx, pool, secrets),
    }
}

/// One coordinator round over every configured IMAP source (M2.3) —
/// the body of the manual `imap_sync_all` job *and* of every scheduled
/// round the M2.4 scheduler submits: excluded sources are skipped,
/// credentials resolve per source from the keyring (password, then
/// Google sign-in), a source without credentials is reported rather
/// than errored, and each folder's batches commit atomically with their
/// checkpoint + failed-item ledger updates.
fn run_sync_all_round(
    ctx: &JobContext,
    pool: &DbPool,
    secrets: &Arc<secrets::Secrets>,
) -> std::result::Result<Value, JobError> {
    let oauth = OAuthConfig::from_env().ok().flatten();
    let secrets = Arc::clone(secrets);
    ctx.progress(0, 1, "syncing sources");
    let round: RoundSummary = imap_coordinator::sync_round(
        pool,
        |instance| match StoredImapConfig::of(instance) {
            Some(stored) => resolve_imap_auth(&secrets, oauth.as_ref(), &stored),
            None => Ok(None),
        },
        || ctx.is_cancelled(),
    );
    ctx.progress(1, 1, "sources synced");
    Ok(serde_json::to_value(&round).unwrap_or(Value::Null))
}

/// Drive one bounded streaming import as a job: parsing happens on this
/// (the runner) thread, each batch commits through the pool's write
/// queue, cancellation is checked per item (the current atomic batch is
/// finished, the partial one discarded — committed data and counts stay
/// exact), and progress reports committed items after every batch.
fn run_bounded(
    ctx: &JobContext,
    pool: &DbPool,
    import_id: impl Into<String>,
    source: &dyn StreamSource,
    media: Option<(vault_core::bounded::MediaKind, &Path, &Path)>,
    _label: &str,
) -> std::result::Result<vault_core::bounded::BoundedOutcome, JobError> {
    let run = BoundedRun {
        import_id: import_id.into(),
        limits: Default::default(),
        media,
    };
    let mut sink = PoolSink(pool);
    let cancelled = || ctx.is_cancelled();
    let progress = |done: u64, total: u64, message: &str| ctx.progress(done, total, message);
    let mut progress = progress;
    let outcome = run
        .run(source, &mut sink, &cancelled, &mut progress)
        .map_err(JobError::from)?;
    if outcome.cancelled {
        return Err(JobError::Cancelled);
    }
    Ok(outcome)
}

/// The incremental embedding job with per-batch progress: pending list
/// (read) → embed (CPU, offline) → batch write, repeat. Reports progress
/// after every batch and checks cancellation between batches.
fn run_embed_job(ctx: &JobContext, embedder: HashEmbedder) -> std::result::Result<Value, JobError> {
    let model = embedder.id().to_string();
    let dim = embedder.dim();
    let start = Instant::now();
    let counts = ctx
        .read(|s| s.embedding_counts(&model))
        .map_err(JobError::from)?;
    let total = counts.messages.saturating_sub(counts.embedded);
    ctx.progress(0, total, "embedding");
    let mut embedded: u64 = 0;
    loop {
        if ctx.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        let model_for_read = model.clone();
        let pending = ctx
            .read(|s| s.pending_embeddings(&model_for_read, EMBED_JOB_BATCH))
            .map_err(JobError::from)?;
        if pending.is_empty() {
            break;
        }
        let items: Vec<(i64, Vec<f32>)> = pending
            .iter()
            .map(|(id, body)| {
                let prefix: String = body.chars().take(EMBED_BODY_PREFIX_CHARS).collect();
                (*id, embedder.embed(&prefix))
            })
            .collect();
        let wrote = items.len() as u64;
        let model_for_write = model.clone();
        ctx.write(move |s| s.embed_messages_batch(&model_for_write, dim, &items))?;
        embedded += wrote;
        ctx.progress(embedded, total, "embedding");
        if wrote < EMBED_JOB_BATCH {
            break;
        }
    }
    let counts = ctx
        .read(|s| s.embedding_counts(&model))
        .map_err(JobError::from)?;
    let remaining = counts.messages.saturating_sub(counts.embedded);
    Ok(json!({
        "model": model,
        "dim": dim,
        "embedded": embedded,
        "remaining": remaining,
        "elapsed_ms": start.elapsed().as_millis() as u64,
    }))
}

/// Every durable source instance in the vault (M2.3): kind, label,
/// connector key, connected state, per-source `excluded` flag, and the
/// attempt/success stamps. Never credentials — those live in the OS
/// keyring (see `secrets`), never in the database.
#[tauri::command]
fn sources_list(state: State<AppState>) -> Result<Vec<SourceInstance>, String> {
    state
        .pool
        .with_read(|s| s.list_source_instances().map_err(|e| e.to_string()))
}

/// Per-source exclusion (M2.3): an excluded source is skipped by every
/// coordinator round (`imap_sync_all`) while its archive, cursors, and
/// configuration are kept intact. An explicit single-source `sync_imap`
/// still runs for an excluded source — manual override stays manual.
#[tauri::command]
fn source_set_excluded(
    state: State<AppState>,
    source_id: String,
    excluded: bool,
) -> Result<(), String> {
    let pool = Arc::clone(&state.pool);
    pool.submit_write(move |s| s.set_source_excluded(&source_id, excluded))
        .map_err(|e| e.to_string())
}

/// The failed-item ledger (M2.3): what fell, where, why, and how many
/// times the vault has retried it — the durable accounting behind every
/// IMAP sync. `source_id` filters to one source; `None` lists all.
#[tauri::command]
fn source_failures(
    state: State<AppState>,
    source_id: Option<String>,
) -> Result<Vec<FailedItem>, String> {
    state.pool.with_read(|s| {
        s.list_failed_items(source_id.as_deref())
            .map_err(|e| e.to_string())
    })
}

/// The periodic-sync schedule for the UI (M2.4): the persisted choice
/// plus the live scheduler status in one round trip.
#[derive(Debug, Clone, Serialize)]
struct SyncScheduleView {
    enabled: bool,
    /// The configured interval, in whole minutes.
    interval_minutes: u64,
    /// The scheduled round currently queued or running.
    active_job: Option<u64>,
    /// Approximate seconds until the next scheduled round (rounded up;
    /// refreshed on status transitions, not every second).
    next_due_in_secs: Option<u64>,
    /// Scheduled rounds submitted since the app started.
    rounds_submitted: u64,
}

impl SyncScheduleView {
    fn of(status: &vault_core::scheduler::SyncSchedulerStatus) -> Self {
        Self {
            enabled: status.enabled,
            interval_minutes: status.interval_ms / 60_000,
            active_job: status.active_job,
            next_due_in_secs: status.next_due_in_secs,
            rounds_submitted: status.rounds_submitted,
        }
    }
}

/// The opt-in periodic sync schedule and its live status (M2.4).
#[tauri::command]
fn sync_schedule_get(state: State<AppState>) -> SyncScheduleView {
    SyncScheduleView::of(&state.scheduler.status())
}

/// Opt in to / opt out of periodic sync, or change the interval (M2.4).
/// `enabled` and `interval_minutes` are each optional; what is passed is
/// applied, the rest keeps its current value. The interval is clamped to
/// whole minutes within 1..=1440; enabling arms one full interval out
/// (never an immediate round), and disabling cancels an active round.
#[tauri::command]
fn sync_schedule_set(
    state: State<AppState>,
    enabled: Option<bool>,
    interval_minutes: Option<u64>,
) -> Result<SyncScheduleView, String> {
    let schedule = resolve_schedule_params(&state.scheduler.status(), enabled, interval_minutes)?;
    let status = state.scheduler.set(schedule)?;
    Ok(SyncScheduleView::of(&status))
}

/// Merge a `sync_schedule_set` request into the current schedule,
/// validating the interval (whole minutes, 1..=1440). Pure — unit
/// tested below.
fn resolve_schedule_params(
    current: &vault_core::scheduler::SyncSchedulerStatus,
    enabled: Option<bool>,
    interval_minutes: Option<u64>,
) -> Result<CoreSyncSchedule, String> {
    let interval_minutes = match interval_minutes {
        Some(minutes) => {
            if !(1..=1440).contains(&minutes) {
                return Err(format!("interval_minutes must be 1..=1440, got {minutes}"));
            }
            minutes
        }
        None => (current.interval_ms / 60_000).max(1),
    };
    Ok(CoreSyncSchedule {
        enabled: enabled.unwrap_or(current.enabled),
        interval_ms: interval_minutes * 60_000,
    })
}

/// One job's current record (`None` for an unknown or already-evicted id).
#[tauri::command]
fn job_get(state: State<AppState>, id: JobId) -> Option<JobRecord> {
    state.pool.job(id)
}

/// All current records: active plus recently finished jobs.
#[tauri::command]
fn job_list(state: State<AppState>) -> Vec<JobRecord> {
    state.pool.jobs()
}

/// Polling recovery: every event after `seq`, plus the current job
/// snapshot. `trimmed` warns that events older than `oldest_kept` were
/// dropped — reconcile from `jobs` then. `job_events_since(0)` fetches
/// everything retained (boot recovery after a webview reload).
#[tauri::command]
fn job_events_since(state: State<AppState>, seq: u64) -> JobEventsPage {
    state.pool.events_since(seq)
}

/// Ask a job to stop (cooperative; the job finishes at its next safe
/// point). Returns whether the job was still cancellable.
#[tauri::command]
fn job_cancel(state: State<AppState>, id: JobId) -> bool {
    state.pool.cancel_job(id)
}

/// Export the current search results (every row matching the query's text
/// and filters, ignoring pagination, in the order the UI shows) to `path`.
///
/// `format` is `"csv"`, `"json"` (JSON Lines) or `"mbox"`; MBOX carries
/// only email-channel rows and reports the rest as `skipped`. Rows stream
/// through a `BufWriter` (never all in memory) off the UI thread; since
/// M2.1 the stream reads from a read connection, so imports and syncs
/// keep committing while a large export writes.
#[tauri::command]
async fn export_search(
    state: State<'_, AppState>,
    query: SearchQuery,
    format: String,
    path: String,
) -> Result<ExportSummary, String> {
    let format = ExportFormat::parse(&format).map_err(|e| e.to_string())?;
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        pool.with_read(|s| -> CoreResult<ExportSummary> {
            let file = std::fs::File::create(&path).map_err(Error::from)?;
            let mut out = BufWriter::new(file);
            let summary = export::export_search(s, &query, format, &mut out)
                .and_then(|s| out.flush().map(|_| s).map_err(Error::from))?;
            Ok(summary)
        })
        .map_err(|e: Error| e.to_string())
    })
    .await
    .map_err(|e| format!("export task failed: {e}"))?
}

/// Export the whole chat thread containing message `id` (oldest → newest;
/// a threadless message exports as one row) to `path`. `None` when no
/// message has that id.
#[tauri::command]
async fn export_chat(
    state: State<'_, AppState>,
    id: i64,
    format: String,
    path: String,
) -> Result<Option<ExportSummary>, String> {
    let format = ExportFormat::parse(&format).map_err(|e| e.to_string())?;
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        pool.with_read(|s| -> CoreResult<Option<ExportSummary>> {
            // Existence is checked before the destination is touched: an
            // unknown id (message deleted between list render and export
            // click) must leave no file at the user-chosen path.
            if s.get(id)?.is_none() {
                return Ok(None);
            }
            let file = std::fs::File::create(&path).map_err(Error::from)?;
            let mut out = BufWriter::new(file);
            export::export_chat(s, id, format, &mut out)
                .and_then(|s| out.flush().map(|_| s).map_err(Error::from))
        })
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("export task failed: {e}"))?
}

/// Archive counts: total and per source.
#[tauri::command]
fn stats(state: State<AppState>) -> Result<message::Stats, String> {
    state
        .pool
        .with_read(|s| s.stats())
        .map_err(|e| e.to_string())
}

/// Dashboard aggregates in one round trip: total messages, the archive's
/// date range, per-source and per-day counts, and the heaviest chats and
/// authors (bounded by `top`, default 10, clamped to 100).
#[tauri::command]
fn archive_stats(state: State<AppState>, top: Option<u32>) -> Result<ArchiveStats, String> {
    state
        .pool
        .with_read(|s| s.archive_stats(top.unwrap_or(DEFAULT_TOP)))
        .map_err(|e| e.to_string())
}

/// The built-in demo messages as an ingest source: the tiny
/// whole-archive helper stays for tests; the job path streams (the demo
/// set is six messages, so both paths are equally bounded in practice).
struct DemoSource;

impl StreamSource for DemoSource {
    fn name(&self) -> &str {
        demo::DEMO_SOURCE
    }

    fn stream(
        &self,
        mut skip: u64,
        emit: &mut dyn FnMut(StreamItem) -> CoreResult<Flow>,
    ) -> CoreResult<()> {
        for msg in demo::messages() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            if emit(StreamItem::Message(msg))? == Flow::Stop {
                return Ok(());
            }
        }
        Ok(())
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
            // M2.1: one pool over the vault file — writer worker + two
            // read connections. Job events stream to the UI; the same
            // events are recoverable by polling `job_events_since`.
            let emit_handle = app.handle().clone();
            let pool = DbPool::spawn(
                data_dir.join("vault.db"),
                PoolConfig::default(),
                move |event| {
                    let _ = emit_handle.emit("job-event", event);
                },
            )
            .map_err(|e| e.to_string())?;
            // Settings come from the file first; the environment keeps
            // working as a read-only fallback provider exactly as before
            // the Settings UI existed.
            let secrets = Arc::new(secrets::Secrets::new());
            let settings_path = data_dir.join(settings::SETTINGS_FILE);
            let settings = Settings::load(&settings_path);
            if let Err(e) = &settings {
                eprintln!("Settings ignored: {e}");
            }
            let env = LlmConfig::from_env();
            if let Err(e) = &env {
                eprintln!("LLM env config ignored: {e}");
            }
            let registry = LlmRegistry::load(settings, env, |id| secrets.load_llm_key(id));
            // Folder watcher: imports new/modified export files from the
            // user's chosen folders through the pool's write queue and
            // pushes every status change to the UI as a `watch-status`
            // event.
            let emit_handle = app.handle().clone();
            let watcher = FolderWatcher::spawn(
                Arc::clone(&pool),
                vault_core::watch::WatchConfig::default(),
                move |snapshot| {
                    let _ = emit_handle.emit("watch-status", snapshot);
                },
            );
            // Periodic sync scheduler (M2.4): opt-in coordinator rounds,
            // submitted through the same job system as the manual round.
            // Its rounds resolve credentials exactly like the manual
            // `imap_sync_all` job; every status change reaches the UI as
            // a `scheduler-status` event.
            let emit_handle = app.handle().clone();
            let scheduler = Arc::new(SyncScheduler::spawn(
                Arc::clone(&pool),
                vault_core::scheduler::SchedulerConfig::default(),
                {
                    let secrets = Arc::clone(&secrets);
                    Arc::new(move |ctx: &JobContext| run_sync_all_round(ctx, ctx.pool(), &secrets))
                },
                move |status| {
                    let _ = emit_handle.emit("scheduler-status", status);
                },
            ));
            app.manage(AppState {
                pool,
                registry: Arc::new(Mutex::new(registry)),
                settings_path,
                embedder: HashEmbedder::new(),
                watcher,
                scheduler,
            });
            app.manage(Arc::clone(&secrets));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            search_messages,
            chat_context,
            llm_ask,
            llm_status,
            embed_messages,
            embed_status,
            settings_get,
            settings_save,
            llm_key_clear,
            import_mbox,
            import_chat_export,
            sync_imap,
            sources_list,
            source_set_excluded,
            source_failures,
            sync_schedule_get,
            sync_schedule_set,
            watch_add_folder,
            watch_remove_folder,
            watch_status,
            oauth_gmail_login,
            oauth_status,
            job_submit,
            job_get,
            job_list,
            job_events_since,
            job_cancel,
            seed_demo,
            stats,
            archive_stats,
            export_search,
            export_chat,
            search_archive_v2,
            message_detail_v2,
            conversation_context_v2,
            list_import_scopes_v2,
            export_messages_v2,
            media_reconcile
        ])
        .run(tauri::generate_context!())
        .expect("error while running mailchat vault");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(map: Value) -> Value {
        map
    }

    #[test]
    fn schedule_params_merge_and_validate() {
        let current = vault_core::scheduler::SyncSchedulerStatus {
            running: true,
            enabled: false,
            interval_ms: 15 * 60 * 1000,
            active_job: None,
            next_due_in_secs: None,
            rounds_submitted: 0,
        };
        // Enable keeps the current interval.
        assert_eq!(
            resolve_schedule_params(&current, Some(true), None).unwrap(),
            CoreSyncSchedule {
                enabled: true,
                interval_ms: 900_000
            }
        );
        // Interval-only keeps the current enabled state (still off here).
        assert_eq!(
            resolve_schedule_params(&current, None, Some(30)).unwrap(),
            CoreSyncSchedule {
                enabled: false,
                interval_ms: 1_800_000
            }
        );
        // Both at once.
        assert_eq!(
            resolve_schedule_params(&current, Some(true), Some(1440)).unwrap(),
            CoreSyncSchedule {
                enabled: true,
                interval_ms: 86_400_000
            }
        );
        // Range errors name the field; 0 and 1441 are both refused.
        assert!(resolve_schedule_params(&current, None, Some(0))
            .unwrap_err()
            .contains("interval_minutes must be 1..=1440"));
        assert!(resolve_schedule_params(&current, None, Some(1441))
            .unwrap_err()
            .contains("interval_minutes must be 1..=1440"));
    }

    #[test]
    fn schedule_view_maps_minutes_and_status() {
        let status = vault_core::scheduler::SyncSchedulerStatus {
            running: true,
            enabled: true,
            interval_ms: 1_800_000,
            active_job: Some(7),
            next_due_in_secs: Some(1800),
            rounds_submitted: 3,
        };
        let view = SyncScheduleView::of(&status);
        assert!(view.enabled);
        assert_eq!(view.interval_minutes, 30);
        assert_eq!(view.active_job, Some(7));
        assert_eq!(view.next_due_in_secs, Some(1800));
        assert_eq!(view.rounds_submitted, 3);
    }

    #[test]
    fn job_spec_parses_and_validates_each_kind() {
        assert_eq!(
            JobSpec::parse("seed_demo", &params(json!({}))).unwrap(),
            JobSpec::SeedDemo
        );
        assert_eq!(
            JobSpec::parse("embed_messages", &params(json!({}))).unwrap(),
            JobSpec::EmbedMessages
        );
        assert_eq!(
            JobSpec::parse("import_mbox", &params(json!({"path": "/a/b.mbox"}))).unwrap(),
            JobSpec::ImportMbox {
                path: "/a/b.mbox".into()
            }
        );
        // extract_media defaults to true, false round-trips.
        assert_eq!(
            JobSpec::parse("import_chat_export", &params(json!({"path": "/c.txt"}))).unwrap(),
            JobSpec::ImportChatExport {
                path: "/c.txt".into(),
                extract_media: true
            }
        );
        assert_eq!(
            JobSpec::parse(
                "import_chat_export",
                &params(json!({"path": "/c.txt", "extract_media": false}))
            )
            .unwrap(),
            JobSpec::ImportChatExport {
                path: "/c.txt".into(),
                extract_media: false
            }
        );
        assert_eq!(
            JobSpec::parse(
                "imap_sync",
                &params(json!({
                    "host": "IMAP.Gmail.COM",
                    "username": "  a@b.c  ",
                    "password": "pw",
                }))
            )
            .unwrap(),
            JobSpec::ImapSync {
                host: "imap.gmail.com".into(),
                port: imap::GMAIL_IMAP_PORT,
                username: "a@b.c".into(),
                password: "pw".into(),
                folders: Vec::new(),
            }
        );

        // Parity with the v1 sync command: an empty (or absent) password
        // is accepted — job_submit resolves it from the stored secret.
        assert_eq!(
            JobSpec::parse(
                "imap_sync",
                &params(json!({
                    "host": "imap.gmail.com",
                    "username": "a@b.c",
                    "password": "",
                }))
            )
            .unwrap(),
            JobSpec::ImapSync {
                host: "imap.gmail.com".into(),
                port: imap::GMAIL_IMAP_PORT,
                username: "a@b.c".into(),
                password: String::new(),
                folders: Vec::new(),
            }
        );
        assert_eq!(
            JobSpec::parse(
                "imap_sync",
                &params(json!({"host": "imap.gmail.com", "username": "a@b.c"}))
            )
            .unwrap(),
            JobSpec::ImapSync {
                host: "imap.gmail.com".into(),
                port: imap::GMAIL_IMAP_PORT,
                username: "a@b.c".into(),
                password: String::new(),
                folders: Vec::new(),
            }
        );
        // Required-field and range errors name the field.
        let err = JobSpec::parse("import_mbox", &params(json!({}))).unwrap_err();
        assert!(err.contains("params.path"), "{err}");
        let err = JobSpec::parse(
            "imap_sync",
            &params(json!({"host": "h", "username": "u", "password": "p", "port": 99999})),
        )
        .unwrap_err();
        assert!(err.contains("params.port"), "{err}");
        // Unknown kinds list the supported set.
        let err = JobSpec::parse("make_coffee", &params(json!({}))).unwrap_err();
        assert!(err.contains("unknown job kind"), "{err}");
        assert!(err.contains("imap_sync"), "{err}");
    }

    #[test]
    fn job_submit_imap_password_resolution_is_two_way() {
        let secrets = secrets::Secrets::new();
        let host = "imap.password-test.invalid";
        let username = "TwoWay@Test.invalid";
        // Entered: used for the run and remembered in the keyring.
        assert_eq!(
            resolve_imap_password(&secrets, host, username, "entered-pw").unwrap(),
            "entered-pw"
        );
        // A later empty submit resolves the stored credential — no re-entry.
        assert_eq!(
            resolve_imap_password(&secrets, host, username, "").unwrap(),
            "entered-pw"
        );
        // An account with nothing stored still requires entry.
        assert_eq!(
            resolve_imap_password(&secrets, host, "None@Test.invalid", "").unwrap_err(),
            "no password stored for this account — enter one"
        );
    }

    #[test]
    fn job_spec_kinds_round_trip() {
        assert_eq!(JobSpec::SeedDemo.kind(), "seed_demo");
        assert_eq!(JobSpec::EmbedMessages.kind(), "embed_messages");
        assert_eq!(
            JobSpec::ImportMbox {
                path: String::new()
            }
            .kind(),
            "import_mbox"
        );
        assert_eq!(
            JobSpec::ImportChatExport {
                path: String::new(),
                extract_media: true
            }
            .kind(),
            "import_chat_export"
        );
        assert_eq!(
            JobSpec::ImapSync {
                host: String::new(),
                port: 993,
                username: String::new(),
                password: String::new(),
                folders: Vec::new()
            }
            .kind(),
            "imap_sync"
        );
        assert_eq!(JobSpec::ImapSyncAll.kind(), "imap_sync_all");
        assert_eq!(
            JobSpec::parse("imap_sync_all", &serde_json::json!({})).unwrap(),
            JobSpec::ImapSyncAll
        );
        // Extra parameters are ignored for the coordinator round: it
        // takes its configuration from the vault's source instances.
        assert_eq!(
            JobSpec::parse("imap_sync_all", &serde_json::json!({ "host": "x" })).unwrap(),
            JobSpec::ImapSyncAll
        );
    }
}

//! Mailchat Vault desktop app: a thin Tauri shell over `vault-core`.
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
//! - `watch_add_folder` / `watch_remove_folder` / `watch_status` —
//!   automatic folder watching: new/modified export files in user-chosen
//!   folders are imported in the background; the UI is kept current via
//!   `watch-status` window events.
//! - `oauth_gmail_login` — run the Gmail Google sign-in (browser OAuth2
//!   flow with PKCE) and store the refresh token in the OS keyring.
//! - `oauth_status` — Gmail sign-in availability for the UI (configured?
//!   signed in?), never any secret.
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
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use vault_core::chat_export::{self, ChatImportOutcome};
use vault_core::export::{self, ExportFormat, ExportSummary};
use vault_core::imap::{self, FolderSyncSummary, ImapAuth, ImapConfig};
use vault_core::ingest::{ingest, Collected, IngestSummary, Source};
use vault_core::llm::{
    self, AnthropicLlm, AskOutcome, LlmConfig, LlmStatus, PlanSource, PlannedQuery, QueryPlan,
};
use vault_core::mbox::MboxSource;
use vault_core::query::{ChatContext, SearchPage, SearchQuery};
use vault_core::settings::{self, Options, Settings, SETTINGS_VERSION};
use vault_core::stats::{ArchiveStats, DEFAULT_TOP};
use vault_core::storage::Storage;
use vault_core::vector::{self, EmbedJobSummary, EmbeddingStatus, HashEmbedder};
use vault_core::watch::{FolderWatcher, WatchSnapshot};
use vault_core::{demo, message, Error, Result as CoreResult};

mod registry;
mod secrets;

use registry::{LlmRegistry, SaveSettingsPayload, Selection, SettingsView};

/// Shared app state: one open storage handle guarded by a mutex, plus the
/// LLM provider registry (settings profiles + keyring keys + env fallback),
/// the local offline embedder behind hybrid ranking (stateless), and the
/// background folder watcher.
struct AppState {
    storage: Arc<Mutex<Storage>>,
    registry: Arc<Mutex<LlmRegistry>>,
    /// Where `settings_save` writes the settings file.
    settings_path: PathBuf,
    embedder: HashEmbedder,
    watcher: FolderWatcher,
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

    // Same steps as [`vault_core::llm::ask`], composed so that each storage
    // lock is short-lived: translate (network) → validate (lock) → run +
    // hybrid-rank (lock, local CPU) → summarize (network).
    let planned = match &backend {
        None => PlannedQuery {
            plan: QueryPlan::plain(question),
            source: local_source.expect("local selection always sets a source"),
            notes: local_note.into_iter().collect(),
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
    secrets: State<'_, secrets::Secrets>,
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
    secrets: State<'_, secrets::Secrets>,
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
/// offline and CPU-bound; large first runs can take a moment, so the work
/// runs off the UI thread.
#[tauri::command]
async fn embed_messages(state: State<'_, AppState>) -> Result<EmbedJobSummary, String> {
    let storage = Arc::clone(&state.storage);
    let embedder = state.embedder;
    tauri::async_runtime::spawn_blocking(move || {
        let storage = storage
            .lock()
            .map_err(|e| format!("storage lock poisoned: {e}"))?;
        vector::run_embedding_job(&storage, &embedder, None).map_err(|e| e.to_string())
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
/// `result.json` (a directory containing it works too), a Signal
/// conversation `.json` (e.g. from `sigtop export-messages -f json`, one
/// file per conversation), or a Discord Data Package messages export —
/// a single channel `messages.json`, the package's `messages/` directory,
/// or the package root. The format is detected from the file contents.
/// Re-importing skips already-stored messages.
///
/// With `extract_media` (default `true`), media files referenced by the
/// export are copied into the app data `media/` store (hash-named,
/// content-deduplicated) and linked from each message; messages already
/// in the archive get their links backfilled, so re-importing an export
/// with media enriches it.
#[tauri::command]
fn import_chat_export(
    app: AppHandle,
    state: State<AppState>,
    path: String,
    extract_media: Option<bool>,
) -> Result<ChatImportOutcome, String> {
    let extract_media = extract_media.unwrap_or(true);
    let data_dir = if extract_media {
        Some(app.path().app_data_dir().map_err(|e| e.to_string())?)
    } else {
        None
    };
    let storage = lock(&state)?;
    chat_export::import(
        &storage,
        Path::new(&path),
        extract_media,
        data_dir.as_deref(),
    )
    .map_err(|e: Error| e.to_string())
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
/// duplicates.
///
/// The flat argument list mirrors the sync form's fields, like every other
/// Tauri command here.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn sync_imap(
    state: State<'_, AppState>,
    secrets: State<'_, secrets::Secrets>,
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
    if host.is_empty() {
        return Err("host is required (e.g. imap.gmail.com)".into());
    }
    if username.is_empty() {
        return Err("username is required".into());
    }
    let folders = folders.unwrap_or_default();
    let port = port.unwrap_or(imap::GMAIL_IMAP_PORT);

    let (imap_auth, saved_to) = match auth.as_deref().unwrap_or("password") {
        "password" => {
            // Resolve the password: entered now (and remembered) or stored
            // before.
            let (password, saved_to) = if password.is_empty() {
                let stored = secrets
                    .load(&host, &username)
                    .ok_or_else(|| "no password stored for this account — enter one".to_string())?;
                (stored, None)
            } else {
                let store = secrets.save(&host, &username, &password);
                (password, Some(store))
            };
            (ImapAuth::Password(password), saved_to)
        }
        "oauth" => {
            // Refresh-token sign-in: mint a fresh access token and use SASL
            // XOAUTH2. The refresh token itself never leaves the secret
            // store (and never enters the vault database).
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
    secrets: State<'_, secrets::Secrets>,
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
    // Browser flow + token exchange are blocking; keep them off the UI
    // thread.
    let tokens = tauri::async_runtime::spawn_blocking(move || {
        oauth::run_login(
            &config,
            Some(&hint),
            |url| webbrowser::open(url).map_err(|e| e.to_string()),
            oauth::LOGIN_TIMEOUT,
        )
    })
    .await
    .map_err(|e| format!("sign-in task failed: {e}"))?
    .map_err(|e| e.to_string())?;

    let refresh = tokens.refresh_token.ok_or_else(|| {
        "Google returned no refresh token — consent must be granted; retry the sign-in".to_string()
    })?;
    let saved_to = secrets.save_refresh(&host, &username, &refresh);
    Ok(OAuthLoginOutcome { username, saved_to })
}

/// Gmail sign-in availability for the UI: whether the OAuth client is
/// configured (env vars set) and whether the account already has a
/// stored sign-in. Never exposes any secret.
#[tauri::command]
fn oauth_status(
    secrets: State<'_, secrets::Secrets>,
    host: Option<String>,
    username: Option<String>,
) -> OAuthStatus {
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
    OAuthStatus {
        configured,
        signed_in,
    }
}

/// Insert the built-in demo messages so a fresh install has searchable data.
#[tauri::command]
fn seed_demo(state: State<AppState>) -> Result<IngestSummary, String> {
    let storage = lock(&state)?;
    ingest(&storage, &DemoSource).map_err(|e: Error| e.to_string())
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

/// Export the current search results (every row matching the query's text
/// and filters, ignoring pagination, in the order the UI shows) to `path`.
///
/// `format` is `"csv"`, `"json"` (JSON Lines) or `"mbox"`; MBOX carries
/// only email-channel rows and reports the rest as `skipped`. Rows stream
/// through a `BufWriter` (never all in memory); like `sync_imap`, the
/// storage lock is held for the whole write, so searches wait for large
/// exports instead of racing the streaming read.
#[tauri::command]
async fn export_search(
    state: State<'_, AppState>,
    query: SearchQuery,
    format: String,
    path: String,
) -> Result<ExportSummary, String> {
    let format = ExportFormat::parse(&format).map_err(|e| e.to_string())?;
    let storage = Arc::clone(&state.storage);
    tauri::async_runtime::spawn_blocking(move || {
        let storage = storage
            .lock()
            .map_err(|e| format!("storage lock poisoned: {e}"))?;
        let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        let mut out = BufWriter::new(file);
        let summary = export::export_search(&storage, &query, format, &mut out)
            .and_then(|s| Ok(out.flush().map(|_| s)?))
            .map_err(|e| e.to_string())?;
        Ok(summary)
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
    let storage = Arc::clone(&state.storage);
    tauri::async_runtime::spawn_blocking(move || {
        let storage = storage
            .lock()
            .map_err(|e| format!("storage lock poisoned: {e}"))?;
        // Existence is checked before the destination is touched: an
        // unknown id (message deleted between list render and export
        // click) must leave no file at the user-chosen path.
        if storage.get(id).map_err(|e| e.to_string())?.is_none() {
            return Ok(None);
        }
        let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        let mut out = BufWriter::new(file);
        export::export_chat(&storage, id, format, &mut out)
            .and_then(|s| Ok(out.flush().map(|_| s)?))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("export task failed: {e}"))?
}

/// Archive counts: total and per source.
#[tauri::command]
fn stats(state: State<AppState>) -> Result<message::Stats, String> {
    lock(&state)?.stats().map_err(|e| e.to_string())
}

/// Dashboard aggregates in one round trip: total messages, the archive's
/// date range, per-source and per-day counts, and the heaviest chats and
/// authors (bounded by `top`, default 10, clamped to 100).
#[tauri::command]
fn archive_stats(state: State<AppState>, top: Option<u32>) -> Result<ArchiveStats, String> {
    lock(&state)?
        .archive_stats(top.unwrap_or(DEFAULT_TOP))
        .map_err(|e| e.to_string())
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
            // Settings come from the file first; the environment keeps
            // working as a read-only fallback provider exactly as before
            // the Settings UI existed.
            let secrets = secrets::Secrets::new();
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
            let storage = Arc::new(Mutex::new(storage));
            // Folder watcher: imports new/modified export files from the
            // user's chosen folders in the background and pushes every
            // status change to the UI as a `watch-status` event.
            let emit_handle = app.handle().clone();
            let watcher = FolderWatcher::spawn(
                Arc::clone(&storage),
                vault_core::watch::WatchConfig::default(),
                move |snapshot| {
                    let _ = emit_handle.emit("watch-status", snapshot);
                },
            );
            app.manage(AppState {
                storage,
                registry: Arc::new(Mutex::new(registry)),
                settings_path,
                embedder: HashEmbedder::new(),
                watcher,
            });
            app.manage(secrets);
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
            watch_add_folder,
            watch_remove_folder,
            watch_status,
            oauth_gmail_login,
            oauth_status,
            seed_demo,
            stats,
            archive_stats,
            export_search,
            export_chat
        ])
        .run(tauri::generate_context!())
        .expect("error while running mailchat vault");
}

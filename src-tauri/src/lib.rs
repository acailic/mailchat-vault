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
//!   Signal `.json` / Discord Data Package / Messenger / JSON Lines
//!   `.jsonl` import with format auto-detection and optional media
//!   extraction into the app data `media/` store (the frontend picks
//!   the file via the dialog plugin).
//! - `sync_imap` — incremental IMAP/Gmail sync (first live mail source):
//!   app password, or Google sign-in (OAuth2 → XOAUTH2) when `auth` says
//!   so.
//! - `sources_list` / `source_set_excluded` / `source_failures` — the
//!   M2.3 source surface: configured sources with their exclusion flag,
//!   per-source exclusion (skipped by coordinator rounds, archive kept),
//!   and the durable failed-item ledger (what fell, why, how often
//!   retried — visible, never silent).
//! - `accounts_list` / `account_forget_credentials` — the multi-account
//!   UI surface (roadmap: the accounts panel): every account in one
//!   overview — IMAP accounts (host/user identity, never credentials)
//!   and import accounts — with per-account message counts, open
//!   failed-item rows, last-sync stamps, pause (exclusion) state, and
//!   which credential kinds are stored; and the removal action that
//!   forgets one account's keyring secrets while keeping its archive.
//! - `watch_add_folder` / `watch_remove_folder` / `watch_retry` /
//!   `watch_status` — automatic folder watching: new/modified export
//!   files in user-chosen folders are imported in the background, a
//!   failed import can be retried once its cause is resolved (the
//!   file must still be under a watched folder; already stored rows
//!   stay single); the UI is kept current via `watch-status` window
//!   events (including per-unit import states for the retry affordance).
//! - `oauth_gmail_login` — run the Gmail Google sign-in (browser OAuth2
//!   flow with PKCE) and store the refresh token in the OS keyring.
//! - `oauth_status` — Gmail sign-in availability for the UI (configured?
//!   signed in?), never any secret.
//! - `job_submit` / `job_get` / `job_list` / `job_events_since` /
//!   `job_cancel` — the v2 background-job protocol (see above); progress
//!   and state changes stream to the UI as `job-event` window events.
//!   Kinds: `seed_demo`, `import_mbox`, `import_chat_export`,
//!   `embed_messages`, `imap_sync` (one named account), `imap_sync_all`
//!   (the M2.3 coordinator round over every configured, non-excluded
//!   IMAP source, credentials resolved per source from the keyring),
//!   `backup_create` and `restore_preview` (M4.4: the versioned backup
//!   container writer and the staged, journaled restore validation), and
//!   `prepare_ask_v2` / `generate_answer_v2` (M5.2 part 1: the job-backed
//!   ask flow — prepare runs the local search leg under the active
//!   workspace scope and returns the exact bounded payload plus its
//!   SHA-256 digest for approval, with zero network; generate re-checks
//!   the approved digest — stale on question/filters/endpoint/result
//!   changes — and sends only that approved snapshot; both are
//!   cancellable between pipeline steps with progress events, and both
//!   keep working under the demo routing).
//! - `seed_demo` — insert the built-in sample messages (idempotent).
//! - `demo_enter` / `demo_leave` / `demo_status` — the isolated sample
//!   archive (U02): `demo_enter` opens a private in-memory vault seeded
//!   with the demo corpus; reads route there while it is open, every
//!   personal-vault write refuses, and `demo_leave` drops it without
//!   the personal `vault.db` ever having been touched.
//! - `preview_import` — bounded import preview (U03): detect the picked
//!   file/folder/`.zip`, sample messages, bounded counts, media
//!   presence, input size, and free disk space — nothing is stored.
//! - `stats` — archive counts per source (drives the source filter chips).
//! - `archive_stats` — dashboard aggregates: totals, date range, counts by
//!   source/day, top chats and top authors.
//! - `whats_new_digest` — the "what's new since your last visit" boot
//!   digest (M7 follow-up): new messages per source plus finished or
//!   failed imports and syncs since the previous read; one call derives
//!   the digest and advances the vault-scoped last-visit cutoff exactly
//!   once (first visit: a quiet, empty digest).
//! - `backup_inspect` / `restore_commit` / `recovery_status` /
//!   `recovery_forget` / `return_to_previous_vault` — the M4.4
//!   backup/restore surface: quick container inspection, the commit
//!   step of a staged validated restore (stops jobs, closes the pool,
//!   switches through the recoverable journal — a restart is required
//!   afterwards), and the previous-vault recovery surface. Startup
//!   resolves an interrupted switch before anything opens the vault.
//! - `last_media_verification` / `last_imports` — the persisted
//!   archive-health evidence (read-only): when the media store was last
//!   reconciled with its coverage counts, and every import source's
//!   last attempt / last success records. Absence is an explicit
//!   not-checked, never a healthy default.
//! - `export_search_v2` — write the current v2 search results (all pages, under
//!   the active filters) to a user-chosen file as CSV, JSON Lines or MBOX.
//! - `export_chat` — write the whole chat thread of one message to a
//!   user-chosen file in the same formats.
//! - `saved_searches_list` / `saved_searches_save` /
//!   `saved_searches_delete` — the Archive workspace's saved searches:
//!   named dynamic v2 search requests (query, canonical source ids,
//!   date bounds with explicit timezone, attachment/honesty filters,
//!   import scope, sort — pagination stripped). Saving creates a
//!   definition or updates one in place (rename and/or new request);
//!   deleting removes the definition only, never archived messages.
//!   Definitions are personal-vault state: reads and writes target the
//!   personal pool and writes refuse while the sample archive is open.
//!
//! Desktop notifications for new mail (the Tauri notification plugin,
//! registered here, drives macOS/Windows/Linux natively) live in the
//! frontend: `src/notify.ts` derives a "new mail" digest from succeeded
//! sync jobs, v1 sync outcomes, and watch snapshots, and stays silent
//! while the window is focused, when the Settings option is off, or when
//! the OS denies permission — see that module for the contract.
//!
//! Background keep-alive (M7, the D10 amendment of 2026-11-11) makes
//! that whole background surface span window closes, strictly opt-in:
//! two default-off Settings options (keep running in the background /
//! start automatically at login), a tray (Show / Sync now / Quit) that
//! exists exactly while keep-alive is on, a single-instance guard that
//! focuses this window on a second launch, and a close handler that
//! hides instead of exits only when the option is on AND a tray stack
//! is usable (otherwise it degrades to the pre-M7 close-exit so a
//! hidden unreachable process can never exist). See `src/tray.rs`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use vault_core::backup::{self, BackupCounts, BackupManifest, RecoveryStatus, RestoreOutcome};
use vault_core::bounded::{BoundedRun, Flow, StreamItem, StreamSource};
use vault_core::chat_export::{self, ChatImportOutcome};
use vault_core::export::{self, ExportFormat, ExportSummary};
use vault_core::health::{
    self, ImportAttempt, ImportCounts, MediaVerificationSummary, SourceImportRecord,
};
use vault_core::imap::{self, FolderSyncSummary, ImapAuth, ImapConfig};
use vault_core::imap_coordinator::{self, RoundSummary, StoredImapConfig};
use vault_core::ingest::IngestSummary;
use vault_core::jobs::{
    DbPool, JobContext, JobError, JobEventsPage, JobId, JobRecord, PoolConfig, PoolSink, PoolSync,
};
use vault_core::llm::{
    self, AnthropicLlm, AskOutcome, AskV2Error, LlmStatus, PlanSource, PlannedQuery,
    PreparedAskRegistry, PreparedEndpoint, QueryPlan,
};
use vault_core::oauth::{self, OAuthConfig};
use vault_core::object_store::{MediaUsage, ObjectStore, ReconcileReport};
use vault_core::query::{ChatContext, SearchPage, SearchQuery};
use vault_core::scheduler::{SyncSchedule as CoreSyncSchedule, SyncScheduler};
use vault_core::search2::{
    self, AppError, ConversationContext, ImportScope, MessageDetail, SearchArchivePage,
    SearchArchiveRequest, SelectionExportSummary, SelectionFormat,
};
use vault_core::settings::{self, Options, Settings, SETTINGS_VERSION};
use vault_core::staged_write::{staged_write, staged_write_opt};
use vault_core::stats::{ArchiveStats, WhatsNewDigest, DEFAULT_TOP};
use vault_core::storage::{
    FailedItem, MediaMessageReference, RegisteredMediaObject, SavedSearch, SourceInstance, Storage,
    UnlinkedMediaPage, UnlinkedMediaPageRequest,
};
use vault_core::vector::{
    self, EmbedJobSummary, Embedder, EmbeddingStatus, HashEmbedder, EMBED_BODY_PREFIX_CHARS,
};
use vault_core::watch::{FolderWatcher, WatchSnapshot};
use vault_core::whats_new;
use vault_core::{demo, message, Error, Result as CoreResult};

mod media_proto;
mod registry;
mod secrets;
mod tray;

use registry::{LlmRegistry, LlmTestPayload, SaveSettingsPayload, Selection, SettingsView};

/// Batch size of the v2 embedding job (mirrors `vector`'s internal batch;
/// progress is reported once per batch).
const EMBED_JOB_BATCH: u64 = 500;

/// Shared app state: the database pool (M2.1: writer worker + two read
/// connections), the LLM provider registry (settings profiles + keyring
/// keys + env fallback), the local offline embedder behind hybrid ranking
/// (stateless), the background folder watcher, and the opt-in periodic
/// sync scheduler (M2.4).
/// Mutual exclusion for the two vault-switch entry points
/// (`restore_commit`, `return_to_previous_vault`): only one switch may
/// run its precheck→drain→switch sequence at a time. A second attempt
/// while one is in flight refuses fast, with the app fully usable.
#[derive(Default)]
struct SwitchGate {
    busy: std::sync::atomic::AtomicBool,
}

/// One held switch slot; releases the gate on drop (a panic or early
/// return can never wedge it closed).
struct SwitchLease<'a> {
    busy: &'a std::sync::atomic::AtomicBool,
}

impl SwitchGate {
    /// Claim the switch slot. `Err` carries the user-facing refusal
    /// (backup and staging jobs hold the same gate, so the wording
    /// covers both).
    fn try_begin(&self) -> Result<SwitchLease<'_>, String> {
        if self.busy.swap(true, std::sync::atomic::Ordering::AcqRel) {
            Err(
                "a vault switch or backup is already in progress — wait for it to finish"
                    .to_string(),
            )
        } else {
            Ok(SwitchLease { busy: &self.busy })
        }
    }
}

impl Drop for SwitchLease<'_> {
    fn drop(&mut self) {
        self.busy.store(false, std::sync::atomic::Ordering::Release);
    }
}

struct AppState {
    pool: Arc<DbPool>,
    registry: Arc<Mutex<LlmRegistry>>,
    /// Where `settings_save` writes the settings file.
    settings_path: PathBuf,
    embedder: HashEmbedder,
    watcher: FolderWatcher,
    scheduler: Arc<SyncScheduler>,
    /// The vault root (app data dir): `vault.db`, `media/`,
    /// `settings.json` — what the M4.4 backup containers capture.
    data_dir: PathBuf,
    /// Serializes `restore_commit` / `return_to_previous_vault` /
    /// `recovery_forget` and the `backup_create` / `restore_preview`
    /// jobs (see [`SwitchGate`]).
    switch_gate: Arc<SwitchGate>,
    /// What startup recovery did to an interrupted switch this session
    /// (U05 restart UX), scoped to the journal it resolved: `RolledBack`/
    /// `RolledForward` mean the recorded journal was resolved here, not
    /// before the restart — and only while that same journal is still
    /// the recorded one (a later in-session switch overwrites the
    /// journal; the old outcome must not leak onto the new switch).
    startup_recovery: StartupRecovery,
    /// U02 (first-source journey): the isolated sample archive.
    /// `Some` while the user browses the built-in demo corpus inside a
    /// private in-memory vault — reads route there, personal-vault
    /// writes refuse, and the personal `vault.db` is never touched.
    /// Dropping the handle on `demo_leave` discards the whole demo.
    demo: Arc<Mutex<Option<Arc<Mutex<Storage>>>>>,
    /// M5.2: the session's single pending prepared ask (preview →
    /// approve → generate). In-memory only, mirroring the session-scoped
    /// consent decision: an approval never survives a restart.
    prepared_asks: Arc<PreparedAskRegistry>,
}

/// The vault read commands run against: the personal pool, or the
/// isolated in-memory sample archive while it is open (U02). Every
/// read-only command goes through [`ActiveVault::with_read`] so a
/// browsing session sees the demo corpus — searches, detail, context,
/// stats — while the personal archive stays untouched underneath.
enum ActiveVault {
    Pool(Arc<DbPool>),
    Demo(Arc<Mutex<Storage>>),
}

impl ActiveVault {
    /// The vault this command should read right now.
    fn from(state: &AppState) -> Self {
        match state.demo_vault() {
            Some(demo) => ActiveVault::Demo(demo),
            None => ActiveVault::Pool(Arc::clone(&state.pool)),
        }
    }

    /// Run one read against the active vault. Cheap and short-lived:
    /// a pool read takes an idle read connection (WAL — it never waits
    /// behind writes); a demo read takes the demo storage's mutex.
    fn with_read<R>(&self, op: impl FnOnce(&Storage) -> R) -> R {
        match self {
            ActiveVault::Pool(pool) => pool.with_read(op),
            ActiveVault::Demo(demo) => {
                let storage = demo.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                op(&storage)
            }
        }
    }
}

impl AppState {
    /// The open sample archive's storage handle, cloned out of its
    /// slot (safe to move into a blocking task).
    fn demo_vault(&self) -> Option<Arc<Mutex<Storage>>> {
        self.demo
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(Arc::clone)
    }

    /// Refuse personal-vault mutations while the sample archive is
    /// open — the demo must never mix into the personal vault, and a
    /// user who believes they are browsing a sample must not silently
    /// change their real archive underneath.
    fn refuse_in_demo(&self) -> Result<(), String> {
        if self.demo_vault().is_some() {
            Err(
                "you are browsing the isolated sample archive — leave it before changing your vault"
                    .to_string(),
            )
        } else {
            Ok(())
        }
    }
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
    ActiveVault::from(&state)
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
    ActiveVault::from(&state)
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
    ActiveVault::from(&state)
        .with_read(|s| s.search_archive(&request))
        .map_err(AppError::from)
}

/// The full v2 reading payload for one message: body, metadata, time
/// provenance, attachments, occurrences, and conflicting-revision links.
#[tauri::command]
fn message_detail_v2(state: State<AppState>, id: i64) -> Result<MessageDetail, AppError> {
    ActiveVault::from(&state)
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
    ActiveVault::from(&state)
        .with_read(|s| s.conversation_context(id, before_cursor, after_cursor, limit))
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found(format!("no message with id {id}")))
}

/// The import scopes available as a results filter ("View imported
/// messages" after an import completes), newest first.
#[tauri::command]
fn list_import_scopes_v2(state: State<AppState>) -> Result<Vec<ImportScope>, AppError> {
    ActiveVault::from(&state)
        .with_read(|s| s.import_scopes())
        .map_err(AppError::from)
}

// ---------------------------------------------------------------------------
// Saved searches (schema 18)
// ---------------------------------------------------------------------------

/// What one `saved_searches_save` writes: `id` `None` creates a new
/// definition under `name`; `Some(id)` updates that definition in place
/// — a new name (rename), a new request (update from the current
/// search), or both.
#[derive(Debug, Deserialize)]
struct SavedSearchSave {
    id: Option<String>,
    name: String,
    request: SearchArchiveRequest,
}

/// The vault's saved-search definitions, oldest first. Definitions are
/// personal-vault state: this always reads the personal pool, even
/// while the isolated sample archive is open — the demo never serves
/// (or absorbs) definitions.
#[tauri::command]
fn saved_searches_list(state: State<AppState>) -> Result<Vec<SavedSearch>, AppError> {
    state
        .pool
        .with_read(|s| s.saved_searches())
        .map_err(AppError::from)
}

/// Save the current search under a name (create), or update an existing
/// definition (rename and/or replace its request). The stored request is
/// the dynamic v2 request minus pagination, so reopening re-executes the
/// search and newly imported matching messages appear. Writes touch the
/// definition row only, never archived messages, and refuse while the
/// sample archive is open (personal-vault mutation guard).
#[tauri::command]
fn saved_searches_save(
    state: State<AppState>,
    definition: SavedSearchSave,
) -> Result<SavedSearch, AppError> {
    state
        .refuse_in_demo()
        .map_err(|e| AppError::invalid_input(e, None))?;
    let pool = Arc::clone(&state.pool);
    pool.submit_write(move |s| {
        s.upsert_saved_search(
            definition.id.as_deref(),
            &definition.name,
            &definition.request,
        )
    })
    .map_err(AppError::from)
}

/// Delete one saved-search definition (idempotent: deleting an unknown
/// id succeeds). Only the definition goes — archived messages are never
/// touched. Refuses while the sample archive is open.
#[tauri::command]
fn saved_searches_delete(state: State<AppState>, id: String) -> Result<(), AppError> {
    state
        .refuse_in_demo()
        .map_err(|e| AppError::invalid_input(e, None))?;
    let pool = Arc::clone(&state.pool);
    pool.submit_write(move |s| s.delete_saved_search(&id).map(|_| ()))
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
/// stays manual by design. `media_reconcile_detail` resolves the
/// missing/corrupt findings to the affected messages and known imports.
///
/// The check itself stays live and on demand; once it finishes, its
/// timestamp and coverage counts are persisted as the media
/// verification summary (archive health, `vault_core::health`) so the
/// health overview can say *when* the store was last verified and what
/// that pass saw — an explicit not-checked before the first pass.
#[tauri::command]
async fn media_reconcile(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReconcileReport, String> {
    // The report compares the store on disk against the *personal*
    // vault's references; against the demo's empty reference set every
    // real object would read as an orphan, so it refuses instead.
    state.refuse_in_demo()?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let digests = ActiveVault::from(&state)
        .with_read(|s| s.media_digests())
        .map_err(|e| e.to_string())?;
    let verify = true; // maintenance call: pay the re-hash for certainty
    let report = tauri::async_runtime::spawn_blocking(move || {
        ObjectStore::new(&data_dir).reconcile(&digests, verify)
    })
    .await
    .map_err(|e| format!("reconcile task failed: {e}"))?
    .map_err(|e| e.to_string())?;
    let summary = MediaVerificationSummary::of_report(
        &report,
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        pool.submit_write(move |s| health::record_media_verification(s, &summary))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("media verification record task failed: {e}"))??;
    Ok(report)
}

/// Disk usage of the media object store (M4.3): object count, total
/// bytes, the referenced/orphaned split, a per-extension breakdown,
/// and the largest objects — the "what is eating my disk" report over
/// the same enumeration `media_reconcile` sees.
#[tauri::command]
async fn media_usage(app: AppHandle, state: State<'_, AppState>) -> Result<MediaUsage, String> {
    // Same reasoning as `media_reconcile`: the referenced/orphaned
    // split is only meaningful against the personal vault.
    state.refuse_in_demo()?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let digests = ActiveVault::from(&state)
        .with_read(|s| s.media_digests())
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || Ok(ObjectStore::new(&data_dir).usage(&digests)))
        .await
        .map_err(|e| format!("usage task failed: {e}"))?
}

/// One damaged digest resolved for recovery: every archived message it
/// affects (a bounded page plus the total) and, when the digest is a
/// registered unlinked file, its known import provenance — so a
/// finding says what to open and what to re-import, not just that a
/// hash is gone.
#[derive(Debug, Clone, Serialize)]
struct IntegrityFinding {
    sha256: String,
    /// `"missing"` (no object in the store) or `"corrupt"` (object
    /// present, content hash ≠ name digest).
    category: String,
    /// The raw reconcile note for corrupt objects (e.g. what the file
    /// hashes to); empty for missing ones.
    detail: String,
    /// Import provenance when the digest sits in the `media_objects`
    /// unlinked-file registry (the known import that stored it);
    /// `None` for message-referenced objects.
    registered: Option<RegisteredMediaObject>,
    /// Total archived messages referencing this digest, ignoring the
    /// page window.
    total_references: u64,
    /// The affected-message page (a digest shared by many messages
    /// resolves page by page; `offset`/`limit` echo the request).
    references: Vec<MediaMessageReference>,
    offset: u32,
    limit: u32,
}

/// One failure category's resolved findings: the total count of damaged
/// digests plus a bounded list (the report stays small however damaged
/// the store is; `media_reconcile` remains the full enumeration).
#[derive(Debug, Clone, Serialize)]
struct IntegrityFindings {
    total: u64,
    findings: Vec<IntegrityFinding>,
}

/// A bounded tail of names for the categories that need no resolution
/// (orphans, staging litter, foreign files): what fits plus how much
/// there is.
#[derive(Debug, Clone, Serialize)]
struct IntegrityTail {
    total: u64,
    entries: Vec<String>,
}

/// The detailed media integrity report: `media_reconcile`'s findings
/// resolved into what a user can act on — affected messages to open,
/// known imports to re-import — grouped by failure category, bounded,
/// and read-only (recovery stays manual: restore a known-good backup
/// or re-import the original export).
#[derive(Debug, Clone, Serialize)]
struct MediaReconcileDetail {
    missing: IntegrityFindings,
    corrupt: IntegrityFindings,
    orphans: IntegrityTail,
    stale_staging: IntegrityTail,
    foreign: IntegrityTail,
}

/// Most resolved findings returned per category (the report is a
/// bounded diagnostic view, not an enumeration; `media_reconcile`
/// lists everything).
const MAX_FINDINGS_PER_CATEGORY: usize = 200;
/// Most names per resolution-free category tail.
const MAX_TAIL_ENTRIES: usize = 200;
/// Default affected-message page size for one finding.
const DEFAULT_REFERENCES_LIMIT: u32 = 20;

/// The digest half of a corrupt entry (`"<sha256>.<ext> (hashes to …)"`
/// — the object's name stem) and its note, or `None` for an unexpected
/// shape (defensive: the strings come from our own `reconcile`).
fn corrupt_entry_digest(entry: &str) -> (Option<String>, &str) {
    let (name, note) = entry.split_once(" (").unwrap_or((entry, ""));
    let digest = name
        .rsplit_once('.')
        .filter(|(stem, _)| stem.len() == 64 && stem.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|(stem, _)| stem.to_ascii_lowercase());
    (digest, note)
}

/// Resolve one category's damaged digests into findings through the
/// vault's stored references and registry: `damaged` carries
/// `(digest, note)` pairs (the note is the corrupt entry's remark —
/// what the file hashes to — or empty for missing objects). Pure read;
/// errors propagate.
fn resolve_findings(
    storage: &Storage,
    damaged: &[(String, String)],
    category: &str,
    only: Option<&str>,
    registered: &HashMap<String, RegisteredMediaObject>,
    offset: u32,
    limit: u32,
) -> CoreResult<IntegrityFindings> {
    let selected: Vec<&(String, String)> = damaged
        .iter()
        .filter(|(digest, _)| only.is_none_or(|only| only == digest.as_str()))
        .collect();
    let total = selected.len() as u64;
    let mut findings = Vec::new();
    for (digest, note) in selected.into_iter().take(MAX_FINDINGS_PER_CATEGORY) {
        let page = storage.media_digest_references(digest, offset, limit)?;
        findings.push(IntegrityFinding {
            sha256: digest.clone(),
            category: category.to_string(),
            detail: note.clone(),
            registered: registered.get(digest).cloned(),
            total_references: page.total,
            references: page.references,
            offset: page.offset,
            limit: page.limit,
        });
    }
    Ok(IntegrityFindings { total, findings })
}

/// The resolved media integrity report: `media_reconcile` with every
/// missing/corrupt finding traced back to the affected archived
/// messages (bounded pages plus totals) and to registered unlinked
/// files' known import provenance. Read-only and diagnostic — nothing
/// is written, deleted, or rewritten; recovery is the user's explicit
/// action (restore a known-good backup, re-import the export).
///
/// `digest` narrows the resolved findings to one damaged object, so a
/// large reference set pages through `offset`/`limit` per finding;
/// without it the report shows every finding's first page.
#[tauri::command]
async fn media_reconcile_detail(
    app: AppHandle,
    state: State<'_, AppState>,
    digest: Option<String>,
    offset: Option<u32>,
    limit: Option<u32>,
) -> Result<MediaReconcileDetail, String> {
    // Same reasoning as `media_reconcile`: findings resolve against the
    // personal vault's messages and imports, never the demo's.
    state.refuse_in_demo()?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let reference_offset = offset.unwrap_or(0);
    let reference_limit = limit.unwrap_or(DEFAULT_REFERENCES_LIMIT);
    let digests = ActiveVault::from(&state)
        .with_read(|s| s.media_digests())
        .map_err(|e| e.to_string())?;
    let verify = true; // maintenance call: pay the re-hash for certainty
    let report = tauri::async_runtime::spawn_blocking(move || {
        ObjectStore::new(&data_dir).reconcile(&digests, verify)
    })
    .await
    .map_err(|e| format!("reconcile task failed: {e}"))?
    .map_err(|e| e.to_string())?;

    // Resolution reads stored rows only: affected-message pages plus
    // the unlinked-file registry's provenance. (The closure's return
    // type is annotated like every other `with_read` call in this file:
    // the `?` operators inside only bound the error type, they do not
    // pin it, and the outer `map_err` needs it known.)
    ActiveVault::from(&state)
        .with_read(|s| -> CoreResult<MediaReconcileDetail> {
            let registered: HashMap<String, RegisteredMediaObject> = s
                .unlinked_media_objects()?
                .into_iter()
                .map(|row| (row.sha256.clone(), row))
                .collect();
            // Corrupt entries are `"<sha256>.<ext> (note)"` strings from
            // the report; recover the digest so the finding resolves.
            let corrupt: Vec<(String, String)> = report
                .corrupt
                .iter()
                .map(|entry| {
                    let (digest, note) = corrupt_entry_digest(entry);
                    (digest.unwrap_or_default(), note.to_string())
                })
                .collect();
            let missing: Vec<(String, String)> = report
                .missing
                .iter()
                .map(|digest| (digest.clone(), String::new()))
                .collect();
            let resolve = |damaged: &[(String, String)], category: &str| {
                resolve_findings(
                    s,
                    damaged,
                    category,
                    digest.as_deref(),
                    &registered,
                    reference_offset,
                    reference_limit,
                )
            };
            let tail = |entries: &[String]| IntegrityTail {
                total: entries.len() as u64,
                entries: entries.iter().take(MAX_TAIL_ENTRIES).cloned().collect(),
            };
            Ok(MediaReconcileDetail {
                missing: resolve(&missing, "missing")?,
                corrupt: resolve(&corrupt, "corrupt")?,
                orphans: tail(&report.orphans),
                stale_staging: tail(&report.stale_staging),
                foreign: tail(&report.foreign),
            })
        })
        .map_err(|e| e.to_string())
}

/// Paged, searchable read of the unlinked media-object registry (M4.2):
/// export files the vault stores without a message link, with their
/// recorded provenance (original name, kind, size, the import that first
/// registered them, first-seen time) and per-digest byte availability.
/// Read-only: the count, rows, and archive revision come from one read
/// snapshot, availability is stamped from the object store's presence
/// check on the blocking pool, and the caller holds one bounded page —
/// the registry is never loaded in full. Rows never assign a file to a
/// message; a future Files view reuses `media_export` /
/// `media_open_external` on the returned `path`.
#[tauri::command]
async fn media_unlinked_page(
    app: AppHandle,
    state: State<'_, AppState>,
    request: UnlinkedMediaPageRequest,
) -> Result<UnlinkedMediaPage, String> {
    // Same reasoning as `media_usage`: the registry's provenance belongs
    // to the personal vault, never the demo corpus.
    state.refuse_in_demo()?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let mut page = ActiveVault::from(&state)
        .with_read(|s| s.unlinked_media_page(&request))
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        page.stamp_availability(&ObjectStore::new(&data_dir));
        page
    })
    .await
    .map_err(|e| format!("unlinked files task failed: {e}"))
}

/// Summary of one explicit media export (M4.3): a byte-exact copy of
/// the store object at the user-picked path.
#[derive(Debug, Clone, Serialize)]
struct MediaExportSummary {
    path: String,
    size: u64,
}

/// Summary of one explicit external open (M4.3): the disposable temp
/// copy handed to the OS handler, and its size.
#[derive(Debug, Clone, Serialize)]
struct MediaOpenSummary {
    path: String,
    size: u64,
}

/// Validate an object name the frontend claims came from a
/// `media/<sha256>.<ext>` path: the same strict single-component shape
/// the `mcmedia://` handler serves, so no command can be steered
/// outside the store by a crafted `raw_json`.
fn validated_object_name(name: &str) -> Result<String, String> {
    media_proto::object_name_of(&format!("/{name}"))
        .ok_or_else(|| format!("not a media object name (expected <sha256>.<ext>): {name}"))
}

/// Export one preserved media object to a user-picked path (M4.3). The
/// action is always explicit (the frontend only offers it from a
/// button); the copy is byte-exact and the store object itself is
/// never moved, linked, or mutated.
#[tauri::command]
async fn media_export(
    app: AppHandle,
    name: String,
    path: String,
) -> Result<MediaExportSummary, String> {
    let name = validated_object_name(&name)?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let src = media_proto::store_path(&data_dir, &name);
    tauri::async_runtime::spawn_blocking(move || {
        let meta =
            std::fs::metadata(&src).map_err(|_| format!("media object not found: {name}"))?;
        if !meta.is_file() {
            return Err(format!("media object not found: {name}"));
        }
        let size = std::fs::copy(&src, &path).map_err(|e| format!("export copy failed: {e}"))?;
        Ok(MediaExportSummary { path, size })
    })
    .await
    .map_err(|e| format!("export task failed: {e}"))?
}

/// The per-digest preview tree under the system temp directory that
/// `media_open_external` writes into and purges.
fn media_open_root() -> PathBuf {
    std::env::temp_dir().join("mailchat-media-open")
}

/// Serializes `media_open_external` invocations so the
/// keep-only-current-copy purge can never destroy an in-flight open.
static MEDIA_OPEN_LOCK: Mutex<()> = Mutex::new(());

/// Best-effort removal of every prior `media_open_external` preview
/// copy, so only the most recent open's copy ever survives. Also runs
/// at app start to clear copies left behind by a previous session. An
/// entry that is not the app's own directory — a symlink or a foreign
/// file planted at the root path — is removed as the entry itself, so
/// the copy never lands outside the app-owned tree.
fn purge_media_open_copies() {
    let root = media_open_root();
    if let Ok(meta) = std::fs::symlink_metadata(&root) {
        if !meta.is_dir() {
            let _ = std::fs::remove_file(&root);
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Restrict a freshly created preview directory (0o700) or file
/// (0o600) to the current user on Unix, verifying the mode actually
/// landed on a regular (non-symlink) entry; a no-op elsewhere.
#[cfg(unix)]
fn restrict_preview_permissions(path: &Path, dir: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if dir { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    let meta = std::fs::symlink_metadata(path)?;
    let right_kind = if dir { meta.is_dir() } else { meta.is_file() };
    if !right_kind || meta.permissions().mode() & 0o777 != mode {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "preview entry is not a private regular directory or file",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_preview_permissions(_path: &Path, _dir: bool) -> std::io::Result<()> {
    Ok(())
}

/// Open one preserved media object with the OS default application
/// (M4.3) — always an explicit user action. The object is first copied
/// into a disposable per-digest temp preview file, so an external
/// viewer that writes back can never touch the content-addressed
/// store (integrity stays a `media_reconcile` call away). Every prior
/// preview is purged before the new copy is written, and the preview
/// tree is private to the user (0700 dirs / 0600 files).
#[tauri::command]
async fn media_open_external(
    app: AppHandle,
    name: String,
    display_name: Option<String>,
) -> Result<MediaOpenSummary, String> {
    let name = validated_object_name(&name)?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let src = media_proto::store_path(&data_dir, &name);
    let display = display_name.unwrap_or_else(|| name.clone());
    tauri::async_runtime::spawn_blocking(move || -> Result<MediaOpenSummary, String> {
        let meta =
            std::fs::metadata(&src).map_err(|_| format!("media object not found: {name}"))?;
        if !meta.is_file() {
            return Err(format!("media object not found: {name}"));
        }
        let _guard = MEDIA_OPEN_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Per-digest temp directory: two objects sharing a display
        // name never clobber each other.
        let digest = name.split('.').next().unwrap_or("object").to_string();
        let preview_root = media_open_root();
        let tmp_dir = preview_root.join(digest);
        purge_media_open_copies();
        std::fs::create_dir_all(&preview_root)
            .map_err(|e| format!("cannot prepare preview dir: {e}"))?;
        restrict_preview_permissions(&preview_root, true)
            .map_err(|e| format!("cannot enforce preview permissions: {e}"))?;
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| format!("cannot prepare preview dir: {e}"))?;
        restrict_preview_permissions(&tmp_dir, true)
            .map_err(|e| format!("cannot enforce preview permissions: {e}"))?;
        let file_name = media_proto::safe_display_name(&display, &name);
        let dest = tmp_dir.join(&file_name);
        let size = std::fs::copy(&src, &dest).map_err(|e| format!("preview copy failed: {e}"))?;
        restrict_preview_permissions(&dest, false)
            .map_err(|e| format!("cannot enforce preview permissions: {e}"))?;
        let url = tauri::Url::from_file_path(&dest)
            .map_err(|_| "preview path is not a valid file URL".to_string())?;
        webbrowser::open(url.as_str()).map_err(|e| format!("cannot open externally: {e}"))?;
        Ok(MediaOpenSummary {
            path: dest.to_string_lossy().into_owned(),
            size,
        })
    })
    .await
    .map_err(|e| format!("open task failed: {e}"))?
}

// ---------------------------------------------------------------------------
// Backup & restore (M4.4)
// ---------------------------------------------------------------------------
//
// The long halves run as jobs (`backup_create`, `restore_preview` —
// progress streams through the job-event protocol); these commands are
// the quick, synchronous surfaces: inspecting a container, committing
// a validated restore (which stops the app's own access and requires a
// restart), and the previous-vault recovery surface.

/// Read a container's manifest without hashing its entries — the
/// pre-restore summary (versions, counts, created-at) the UI shows
/// before validating.
#[tauri::command]
async fn backup_inspect(archive: String) -> Result<BackupManifest, String> {
    tauri::async_runtime::spawn_blocking(move || backup::read_manifest(Path::new(&archive)))
        .await
        .map_err(|e| format!("inspect task failed: {e}"))?
        .map_err(|e| e.to_string())
}

/// Commit a staged, validated restore. Every refusal the commit can
/// raise is checked read-only (`verify_committable`) while the backend
/// is still live, so a refused commit never leaves a drained pool
/// behind a usable UI; only then does this stop the scheduler, watcher,
/// and the database pool — the switch moves `vault.db` itself — and
/// replaces the vault payload through the recoverable switch journal.
/// On success the app must be restarted to reopen the vault.
/// `sources_needing_reauth` counts the restored IMAP accounts whose
/// `user@host` credentials are missing from this machine's keyring: a
/// same-machine restore with live keyring entries reports 0
/// (credentials are never part of a backup).
#[tauri::command]
async fn restore_commit(
    state: State<'_, AppState>,
    secrets: State<'_, Arc<secrets::Secrets>>,
    preview_id: String,
) -> Result<RestoreOutcome, String> {
    // A switch drains the personal pool — never under the demo vault.
    state.refuse_in_demo()?;
    // One switch at a time: a concurrent commit/return refuses fast
    // while this one runs its precheck→drain→switch sequence.
    let _switch_lease = state.switch_gate.try_begin()?;
    let scheduler = Arc::clone(&state.scheduler);
    let pool = Arc::clone(&state.pool);
    let data_dir = state.data_dir.clone();
    let secrets = Arc::clone(secrets.inner());
    tauri::async_runtime::spawn_blocking(move || -> Result<RestoreOutcome, String> {
        // The active vault must be exactly what the preview validated.
        let fingerprint = pool
            .with_read(|s| s.vault_fingerprint())
            .map_err(|e| e.to_string())?;
        // Refusals surface before anything stops: the app stays usable.
        backup::verify_committable(&data_dir, &preview_id, &fingerprint, &|_| {})
            .map_err(|e| e.to_string())?;
        // Stop scheduled rounds and watched-folder imports before the
        // switch (their writes would fail on the drained pool anyway,
        // but disabling is the honest stop).
        let _ = scheduler.set(vault_core::scheduler::SyncSchedule {
            enabled: false,
            interval_ms: 0,
        });
        pool.shutdown();
        let outcome = backup::commit_restore(
            &data_dir,
            &preview_id,
            &fingerprint,
            &|_| {},
            &backup::RestoreOptions::default(),
            &needs_reauth_after_restore(&secrets),
        )
        .map_err(|e| {
            // The pool is drained now and the journal may hold a
            // mid-switch state that resolves on restart. The exact
            // `switch_stopped:` token is the stable frontend contract
            // (see SWITCH_STOPPED_MARKER in src/backupview.ts) — keep
            // the human message after it, never inside a prose match.
            format!("switch_stopped: restart required — the switch stopped and recovers on restart: {e}")
        })?;
        Ok(outcome)
    })
    .await
    .map_err(|e| format!("restore task failed: {e}"))?
}

/// The per-source reauth check the restore outcome counts with: an
/// IMAP account whose `user@host` entry is missing from this machine's
/// keyring (non-IMAP sources never hold credentials, so they never
/// count).
fn needs_reauth_after_restore(
    secrets: &secrets::Secrets,
) -> impl Fn(&vault_core::storage::SourceInstance) -> bool + '_ {
    move |instance| {
        vault_core::imap_coordinator::StoredImapConfig::of(instance)
            .map(|stored| !secrets.has_account(&stored.host, &stored.username))
            .unwrap_or(false)
    }
}

/// The recovery state the Settings panel renders, plus the two
/// facts only the app layer knows: the vault's data dir (the
/// "where does my archive live" answer) and what this session's
/// startup recovery did to the recorded switch (the journal alone
/// cannot distinguish a clean restart-after-switch from a rollback
/// that resolved an interrupted one).
#[derive(Debug, Clone, PartialEq, Serialize)]
struct RecoveryStatusView {
    #[serde(flatten)]
    status: RecoveryStatus,
    /// The vault root (app data dir).
    data_dir: PathBuf,
    /// How the active vault came to be, when a switch ever ran — the
    /// origin from the journal, resolved by the startup outcome.
    last_switch: Option<LastSwitchView>,
}

/// One line of "what is active now, since when, from where": the
/// journal's switch origin plus this session's recovery outcome.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct LastSwitchView {
    /// `"restored"` (from a backup container) or `"returned"` (to a
    /// recorded previous-vault slot).
    kind: &'static str,
    /// The container path for a restore (`null` for a return).
    archive: Option<PathBuf>,
    /// RFC 3339 UTC — when the switch ran.
    since: String,
    /// What happened to this switch at this session's startup:
    /// `"none"` it completed before the restart; `"rolled_back"` /
    /// `"rolled_forward"` an interrupted switch was resolved here.
    recovery: &'static str,
}

/// Merge the journal's switch origin with this session's startup
/// recovery outcome: a rolled-back switch means the *pre-switch*
/// vault is active, so the origin line must say so instead of claiming
/// the restored one.
fn last_switch_view(origin: &backup::LastSwitchOrigin, recovery: &'static str) -> LastSwitchView {
    LastSwitchView {
        kind: match origin.kind {
            backup::SwitchOriginKind::Restored => "restored",
            backup::SwitchOriginKind::Returned => "returned",
        },
        archive: origin.archive.clone(),
        since: origin.created_at.clone(),
        recovery,
    }
}

/// What this session's startup recovery did, and to which journal (see
/// [`AppState::startup_recovery`]): the resolved journal's `created_at`
/// scopes the outcome — after a later in-session switch overwrites the
/// journal, the stale outcome stops applying.
#[derive(Debug, Clone)]
struct StartupRecovery {
    outcome: backup::SwitchRecovery,
}

/// The recovery word for the origin line: `"rolled_back"` /
/// `"rolled_forward"` only while the recorded journal is still the one
/// this session's startup recovery resolved; otherwise `"none"`.
fn startup_recovery_word(
    startup: &StartupRecovery,
    journal_created_at: Option<&str>,
) -> &'static str {
    let resolved_journal = match &startup.outcome {
        backup::SwitchRecovery::RolledBack {
            journal_created_at, ..
        }
        | backup::SwitchRecovery::RolledForward {
            journal_created_at, ..
        } => Some(journal_created_at.as_str()),
        _ => None,
    };
    let scoped = matches!(
        (journal_created_at, resolved_journal),
        (Some(recorded), Some(resolved)) if recorded == resolved,
    );
    if scoped {
        match startup.outcome {
            backup::SwitchRecovery::RolledBack { .. } => return "rolled_back",
            backup::SwitchRecovery::RolledForward { .. } => return "rolled_forward",
            _ => {}
        }
    }
    "none"
}

/// The recovery state: the switch journal, a staged (validated,
/// uncommitted) restore, and the recorded previous-vault slots —
/// plus the vault's data dir and the resolved switch origin.
#[tauri::command]
async fn recovery_status(state: State<'_, AppState>) -> Result<RecoveryStatusView, String> {
    let data_dir = state.data_dir.clone();
    let mut status =
        tauri::async_runtime::spawn_blocking(move || backup::recovery_status(&data_dir))
            .await
            .map_err(|e| format!("status task failed: {e}"))?
            .map_err(|e| e.to_string())?;
    let last_switch = status.last_switch.as_ref().map(|origin| {
        last_switch_view(
            origin,
            startup_recovery_word(
                &state.startup_recovery,
                status.journal_created_at.as_deref(),
            ),
        )
    });
    // The flattened `status` carries its own `last_switch` under the
    // same JSON key as the view's resolved one; drop it so exactly one
    // shape — the app-merged view — reaches the frontend.
    status.last_switch = None;
    Ok(RecoveryStatusView {
        data_dir: state.data_dir.clone(),
        status,
        last_switch,
    })
}

/// The persisted "last verified backup" of the personal vault (U05):
/// written into `archive_meta` when a `backup_create` job's container
/// finished verifying, read here at boot so the safety state survives
/// restarts. `still_present` honestly flags a container the user moved
/// or deleted since — "verified" describes creation time, and the
/// panel must not promise a file that is no longer there.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct LastBackupView {
    /// RFC 3339 UTC — when the container finished verifying.
    created_at: String,
    /// The destination the container was written to.
    path: PathBuf,
    /// The container's size in bytes.
    size: u64,
    /// The counts of the vault the snapshot captured.
    counts: BackupCounts,
    /// Whether the container still sits at the recorded path.
    still_present: bool,
}

#[tauri::command]
async fn last_backup(state: State<'_, AppState>) -> Result<Option<LastBackupView>, String> {
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        let record = pool
            .with_read(backup::last_backup_record)
            .map_err(|e| e.to_string())?;
        Ok(record.map(|record| LastBackupView {
            created_at: record.created_at,
            path: record.path.clone(),
            size: record.size,
            counts: record.counts,
            still_present: record.path.is_file(),
        }))
    })
    .await
    .map_err(|e| format!("last-backup task failed: {e}"))?
}

/// The persisted media verification summary of the personal vault
/// (archive health): when the media store was last reconciled and the
/// coverage counts that pass saw — missing (external damage) kept
/// distinct from corrupt, orphans, and litter. `None` is the explicit
/// not-checked answer: no `media_reconcile` pass has finished (or the
/// stored record is unreadable). The historical summary never claims
/// the store is healthy *now*; only when it was last checked.
#[tauri::command]
async fn last_media_verification(
    state: State<'_, AppState>,
) -> Result<Option<MediaVerificationSummary>, String> {
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        pool.with_read(health::media_verification)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("media verification task failed: {e}"))?
}

/// The per-import-source last-import evidence of the personal vault
/// (archive health): every import source's last attempt (outcome and
/// counts, or the failure) kept distinct from its last success, so a
/// failed or cancelled recent attempt never reads as a successful
/// refresh nor overwrites the recorded success time. Empty when no
/// import has finished — sources never imported read as absent, not
/// failed.
#[tauri::command]
async fn last_imports(state: State<'_, AppState>) -> Result<Vec<SourceImportRecord>, String> {
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        pool.with_read(health::import_source_records)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("last-imports task failed: {e}"))?
}

/// Explicitly forget one previous-vault slot (releases its disk).
/// Serialized with the switch entry points: a Forget racing a return's
/// pre-journal window would delete the very slot the switch is about
/// to consume, so it takes the same gate and refuses while one runs.
#[tauri::command]
async fn recovery_forget(state: State<'_, AppState>, slot_id: String) -> Result<(), String> {
    state.refuse_in_demo()?;
    let _switch_lease = state.switch_gate.try_begin()?;
    let data_dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || backup::recovery_forget(&data_dir, &slot_id))
        .await
        .map_err(|e| format!("forget task failed: {e}"))?
        .map_err(|e| e.to_string())
}

/// Switch the active vault back to a recorded previous vault through
/// the same validated switch process. Like `restore_commit` this
/// raises every refusal (unknown slot, unresolved switch, failed slot
/// validation) before stopping the app's access, so a refused return
/// leaves the app usable; a committed one requires a restart.
#[tauri::command]
async fn return_to_previous_vault(
    state: State<'_, AppState>,
    secrets: State<'_, Arc<secrets::Secrets>>,
    slot_id: String,
) -> Result<RestoreOutcome, String> {
    // A switch drains the personal pool — never under the demo vault.
    state.refuse_in_demo()?;
    // One switch at a time: a concurrent commit/return refuses fast
    // while this one runs its precheck→drain→switch sequence.
    let _switch_lease = state.switch_gate.try_begin()?;
    let scheduler = Arc::clone(&state.scheduler);
    let pool = Arc::clone(&state.pool);
    let data_dir = state.data_dir.clone();
    let secrets = Arc::clone(secrets.inner());
    tauri::async_runtime::spawn_blocking(move || -> Result<RestoreOutcome, String> {
        // Refusals surface before anything stops: the app stays usable.
        backup::verify_returnable(&data_dir, &slot_id).map_err(|e| e.to_string())?;
        let _ = scheduler.set(vault_core::scheduler::SyncSchedule {
            enabled: false,
            interval_ms: 0,
        });
        pool.shutdown();
        backup::return_to_previous_vault(
            &data_dir,
            &slot_id,
            &|_| {},
            &needs_reauth_after_restore(&secrets),
        )
        .map_err(|e| {
            format!("switch_stopped: restart required — the switch stopped and recovers on restart: {e}")
        })
    })
    .await
    .map_err(|e| format!("return task failed: {e}"))?
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
    let vault = ActiveVault::from(&state);
    tauri::async_runtime::spawn_blocking(move || {
        vault
            .with_read(|s| -> CoreResult<SelectionExportSummary> {
                staged_write(Path::new(&path), |out| {
                    search2::export_selection(s, &ids, format, out)
                })
            })
            .map_err(AppError::from)
    })
    .await
    .map_err(|e| AppError::internal(format!("export task failed: {e}")))?
}

/// Write a UTF-8 text file to an absolute path the frontend picked with
/// its save dialog (used by the collected-links export; no dialog logic
/// lives here).
#[tauri::command]
fn save_text(path: String, contents: String) -> Result<(), String> {
    std::fs::write(&path, contents.as_bytes()).map_err(|e| format!("write {path}: {e}"))
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
///
/// `consent` is the M5 **AI-consent gate**: an ask may contact a network
/// provider only when it is explicitly `true`. Without consent (the
/// default, `None`/`false`) a network selection is downgraded to a fully
/// local ask before any LLM object exists — see [`run_ask`].
///
/// `scope` optionally narrows the ask to the active workspace filters
/// (canonical source ids, author, local date bounds with timezone,
/// attachment state, import id, honesty filters). It is validated
/// against the vault first; an invalid scope fails the command before
/// any retrieval. `None` (or absent) asks over the entire archive, so
/// existing callers keep working.
#[tauri::command]
fn llm_ask(
    state: State<AppState>,
    question: String,
    limit: Option<u32>,
    provider: Option<String>,
    consent: Option<bool>,
    scope: Option<search2::AskScope>,
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
    let summarize_top: usize;
    let selection: Selection;
    {
        let registry = state
            .registry
            .lock()
            .map_err(|e| format!("registry lock poisoned: {e}"))?;
        summarize_top = registry.settings().options.summarize_top as usize;
        selection = registry.select(provider.as_deref());
    }
    run_ask(
        &ActiveVault::from(&state),
        &state.embedder,
        selection,
        provider.is_some(),
        question,
        scope,
        limit,
        summarize_top,
        today,
        consent.unwrap_or(false),
    )
}

/// The ask pipeline after provider selection: the M5 **AI-consent gate**,
/// then translate → validate → run + hybrid-rank → summarize.
///
/// The gate is absolute and sits *before* anything network-shaped exists:
/// a [`Selection::Provider`] without explicit consent (`consent != true`)
/// is downgraded to a fully local ask with a visible note — the network
/// backend is never constructed, so no translate or summarize round trip
/// can go out and nothing leaves the machine without the user's agreement.
/// A free function (not a command) so tests can drive it against a mock
/// endpoint and prove the no-consent case makes zero network calls.
///
/// An optional workspace `scope` is validated *before* the gate — an
/// invalid scope fails the ask with a validation error before any
/// retrieval or network round trip — and then constrains both retrieval
/// legs: the plan can only narrow it (intersected date bounds), never
/// broaden it, and an empty date intersection yields no results without
/// ever falling back to unscoped retrieval.
#[allow(clippy::too_many_arguments)] // mirrors vault_core::llm::ask's step list
fn run_ask(
    vault: &ActiveVault,
    embedder: &HashEmbedder,
    selection: Selection,
    provider_requested: bool,
    question: &str,
    scope: Option<search2::AskScope>,
    limit: u32,
    summarize_top: usize,
    today: chrono::NaiveDate,
    consent: bool,
) -> Result<AskOutcome, String> {
    // Scope validation first: a malformed or unknown-referencing scope
    // fails the command before anything else happens (local read only).
    let scope = match scope {
        Some(raw) => Some(
            vault
                .with_read(|s| search2::validate_ask_scope(s, &raw))
                .map_err(|e| e.to_string())?,
        ),
        None => None,
    };

    // AI-consent gate: without explicit consent a network selection
    // becomes local-only. (A local selection was already local; its note
    // explains why, unchanged from pre-M5 behavior.)
    let (backend, local_source, local_notes): (Option<AnthropicLlm>, PlanSource, Vec<String>) =
        match selection {
            Selection::Provider { .. } if !consent => (
                None,
                PlanSource::Local,
                vec![
                    "AI consent not granted — this ask ran fully locally; nothing was sent to any provider"
                        .into(),
                ],
            ),
            Selection::Provider { config, .. } => {
                (Some(AnthropicLlm::new(config)), PlanSource::Llm, Vec::new())
            }
            Selection::Local { reason } => (
                None,
                if provider_requested {
                    PlanSource::Local
                } else {
                    PlanSource::NoLlm
                },
                if reason.is_empty() { Vec::new() } else { vec![reason] },
            ),
        };

    // Same steps as [`vault_core::llm::ask`], composed so that each read
    // is short-lived: translate (network, no storage) → validate (read) →
    // run + hybrid-rank (read, local CPU) → summarize (network).
    let planned = match &backend {
        None => PlannedQuery {
            plan: QueryPlan::plain(question),
            source: local_source,
            notes: local_notes,
        },
        Some(backend) => {
            let candidate = llm::translate(backend, question, today);
            vault.with_read(|s| llm::validate(s, question, candidate))
        }
    };
    let (results, ranking) = {
        let fts_results = vault
            .with_read(|s| llm::run_plan_scoped(s, &planned, scope.as_ref(), limit))
            .map_err(|e| e.to_string())?;
        let hybrid = vault
            .with_read(|s| {
                llm::rank_hybrid_scoped(
                    s,
                    embedder,
                    question,
                    &planned,
                    &fts_results,
                    scope.as_ref(),
                    limit,
                )
            })
            .map_err(|e| e.to_string())?;
        (hybrid.results, hybrid.ranking)
    };
    let collected = if planned.plan.extract_links {
        vault_core::extract::extract_links(&results)
    } else {
        Vec::new()
    };
    let (answer, summary_notes) = match &backend {
        Some(backend) => {
            let summarized =
                llm::summarize_results(backend, question, &results, summarize_top, &collected);
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
        collected,
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

/// `LlmTestResult` — the outcome of one "Test connection" click.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LlmTestResult {
    pub ok: bool,
    pub latency_ms: u64,
    /// What happened: "{host} replied on model {model}" or the
    /// classified failure message.
    pub detail: String,
}

/// One "Test connection" round trip against a provider profile or the
/// environment fallback (`env`). The explicit button click is the
/// consent: the request is a fixed "ping" with `max_tokens: 1` and sends
/// nothing from the vault, so it stands outside the Ask consent gate.
///
/// The probe is blocking I/O bounded only by the request timeout (30s),
/// so it runs on the blocking pool: a slow or unreachable endpoint never
/// freezes the UI or stalls other commands.
#[tauri::command]
async fn llm_test(
    state: State<'_, AppState>,
    payload: LlmTestPayload,
) -> Result<LlmTestResult, String> {
    // Resolve the config under the registry lock, and release it before
    // the network round trip (same pattern as llm_ask): the config is
    // cloned out, so the probe below owns everything it needs.
    let config = {
        let registry = state
            .registry
            .lock()
            .map_err(|e| format!("registry lock poisoned: {e}"))?;
        registry.test_config(&payload)?
    };
    tauri::async_runtime::spawn_blocking(move || {
        let start = std::time::Instant::now();
        let outcome = config.test_connection();
        let latency_ms = start.elapsed().as_millis() as u64;
        let detail = match &outcome {
            Ok(()) => format!("{} replied on model {}", config.host(), config.model),
            Err(e) => e.to_string(),
        };
        LlmTestResult {
            ok: outcome.is_ok(),
            latency_ms,
            detail,
        }
    })
    .await
    .map_err(|e| format!("connection test task failed: {e}"))
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
/// error and nothing is written), then the file, then the keyring, then
/// the registry — and only after the save has succeeded end to end do
/// the OS-level applications run (the M7 tray reconcile and the OS
/// login-item registration); every failure up to those OS calls leaves
/// no keys orphaned in the keyring and no invalid file on disk, and a
/// failure in one of the OS calls surfaces as a save error the user can
/// retry. Returns the fresh view the UI re-renders from.
#[tauri::command]
fn settings_save(
    app: AppHandle,
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
            notifications: payload.notifications,
            keep_running_in_background: payload.keep_running_in_background,
            start_at_login: payload.start_at_login,
        },
        env_overrides: vault_core::settings::EnvOverrides {
            anthropic_base_url: payload.env_overrides.base_url.clone(),
            llm_model: payload.env_overrides.model.clone(),
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

    // Env API-key updates name variables, not provider ids — refuse
    // anything outside the two the environment fallback reads.
    const ENV_KEY_VARS: [&str; 2] = ["ZAI_API_KEY", "ANTHROPIC_API_KEY"];
    for update in &payload.env_api_keys {
        let var = update.id.trim();
        if !ENV_KEY_VARS.contains(&var) {
            return Err(format!("env API key update for unknown variable “{var}”"));
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
    // Env keys ride the same rule under keyring ids `env:<VAR>`.
    for update in &payload.env_api_keys {
        let id = format!("env:{}", update.id.trim());
        let key = update.key.trim();
        if key.is_empty() {
            secrets.clear_llm_key(&id);
        } else {
            key_store = Some(secrets.save_llm_key(&id, key));
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

    let keep_alive = settings.options.keep_running_in_background;
    let start_at_login = settings.options.start_at_login;
    {
        let mut registry = state
            .registry
            .lock()
            .map_err(|e| format!("registry lock poisoned: {e}"))?;
        registry.update(settings, key_store, |id| secrets.load_llm_key(id));
    }
    // M7: the tray exists exactly while keep-alive is on — reconcile to
    // what was just saved. A failed build only degrades (the close
    // handler hides only when the tray icon is actually up).
    if keep_alive {
        if let Err(e) = tray::ensure(&app) {
            eprintln!("tray not available: {e}");
        }
    } else {
        tray::remove(&app);
    }
    // M7: apply the OS login-item registration only after the save has
    // succeeded end to end — never re-armed behind the user's back by a
    // boot. Every failure up to this call leaves the OS autostart
    // consistent with the settings file; a genuine enable/disable failure
    // here surfaces as a save error the user can retry (on the disable
    // path that can leave the OS armed while the file says off).
    let autostart = app.autolaunch();
    if start_at_login {
        autostart
            .enable()
            .map_err(|e| format!("could not set up start-at-login: {e}"))?;
    } else {
        autostart
            .disable()
            .map_err(|e| format!("could not remove start-at-login: {e}"))?;
    }
    let registry = state
        .registry
        .lock()
        .map_err(|e| format!("registry lock poisoned: {e}"))?;
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
    state.refuse_in_demo()?;
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
    let embedder = state.embedder;
    ActiveVault::from(&state)
        .with_read(|s| vector::embedding_status(s, &embedder))
        .map_err(|e| e.to_string())
}

/// Import every message of an `.mbox` file (idempotent; re-importing
/// skips already-stored messages). An `.eml` folder pick — or a `.zip`
/// of `.eml` files, expanded through the shared import fence — imports
/// one mail per file. M2.2: the import streams — the file is
/// parsed one message at a time on this blocking task, and batches commit
/// atomically through the pool's write queue with checkpoint advancement,
/// so arbitrarily large files import under a bounded memory budget.
/// M4.2: each mail's MIME attachments/inline parts are decoded by the
/// parser and staged into the app data `media/` store, linked from the
/// message's `media` entries — re-importing skips stored messages
/// without re-storing their bytes. The outcome mirrors the chat import:
/// ingest summary plus the media pass's counters; the run's import_id
/// stays the value the import-scope filter keys on, so the UI can offer
/// "View imported messages" scoped to exactly this run.
#[tauri::command]
async fn import_mbox(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<ChatImportOutcome, String> {
    state.refuse_in_demo()?;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        // The shared bounded body resolves the pick (directory → `.eml`
        // folder, `.zip` → fenced temp tree, else a single mbox/.eml
        // file) and keeps any temp tree alive for the whole run.
        let mut sink = PoolSink(&pool);
        let mut no_progress = |_: u64, _: u64, _: &str| {};
        vault_core::mbox::import_pick_bounded(
            &mut sink,
            Path::new(&path),
            &data_dir,
            &|| false,
            &mut no_progress,
        )
        .map(ChatImportOutcome::from)
        .map_err(|e: Error| e.to_string())
    })
    .await
    .map_err(|e| format!("import task failed: {e}"))?
}

/// Import a chat export: a WhatsApp `.txt` export, a Telegram `result.json`
/// (a directory containing it works too), a Signal conversation `.json`
/// (e.g. from `sigtop export-messages -f json`, one file per conversation),
/// a Discord Data Package messages export — a single channel
/// `messages.json`, the package's `messages/` directory, or the package
/// root — or a JSON Lines `.jsonl` export (one stored message object per
/// line; each row keeps its original source and external id, so exports
/// move between vaults and re-imports never duplicate). The format is
/// detected from the file contents. Re-importing skips already-stored
/// messages.
///
/// With `extract_media` (default `true`), media files referenced by the
/// export are copied into the app data `media/` store (hash-named,
/// content-deduplicated) and linked from each message; messages already
/// in the archive get their links backfilled, so re-importing an export
/// with media enriches it. JSON Lines rows carry media as `raw_json`
/// references only — media bytes are not transferred (the backup
/// container is the whole-vault tool).
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
    confirmed_timezone: Option<String>,
    source: Option<String>,
) -> Result<ChatImportOutcome, String> {
    state.refuse_in_demo()?;
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
        chat_export::import_bounded_with(
            &mut sink,
            Path::new(&path),
            extract_media,
            data_dir.as_deref(),
            confirmed_timezone.as_deref(),
            source.as_deref(),
            &|| false,
            &mut no_progress,
        )
        .map(ChatImportOutcome::from)
        .map_err(|e: Error| e.to_string())
    })
    .await
    .map_err(|e| format!("import task failed: {e}"))?
}

/// The rejection shape of a failed [`sync_imap`]: the actionable message
/// (classified — never a raw errno string; see `vault_core::imap::
/// SyncFailure`), the stable failure class token, and — the honest
/// credential rule the cold review asked for (§2.2/#5) — where an entered
/// password was already kept when the attempt failed. A password is saved
/// *before* the connection is attempted, so a failed sync can still have
/// stored one; the UI must tell the user it is there (and that a
/// session-memory store loses it on restart).
#[derive(Debug, Clone, Serialize)]
struct SyncFailureView {
    /// Stable class token (`"connection_refused"`, `"auth_failed"`, …).
    class: String,
    /// The actionable, user-facing message.
    message: String,
    /// Where an entered password was saved before the failure (`None`
    /// when no password was entered or nothing was saved).
    saved_to: Option<secrets::SecretStore>,
}

impl SyncFailureView {
    /// A classified connect/sync failure, with where the password went.
    fn classified(e: &vault_core::Error, saved_to: Option<secrets::SecretStore>) -> Self {
        let failure = imap::SyncFailure::classify(e);
        Self {
            class: failure.as_str().to_string(),
            message: failure.describe(e),
            saved_to,
        }
    }

    /// A pre-flight failure (missing input, unconfigured OAuth) whose
    /// message is already the actionable text — kept verbatim.
    fn config(message: impl Into<String>) -> Self {
        Self {
            class: "config".to_string(),
            message: message.into(),
            saved_to: None,
        }
    }
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
/// M4.2: each fetched mail's MIME attachments/inline parts are stored
/// into the app data `media/` object store as the sync parses them, on
/// the sync thread between fetch and commit.
///
/// The flat argument list mirrors the sync form's fields, like every other
/// Tauri command here.
///
/// A failed sync rejects with [`SyncFailureView`]: the classified,
/// actionable message (never a raw errno), the stable failure class, and
/// where an entered password was already kept — see that type's docs for
/// the honest-credential rule.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn sync_imap(
    app: AppHandle,
    state: State<'_, AppState>,
    secrets: State<'_, Arc<secrets::Secrets>>,
    host: String,
    port: Option<u16>,
    username: String,
    password: String,
    folders: Option<Vec<String>>,
    auth: Option<String>,
) -> Result<ImapSyncOutcome, SyncFailureView> {
    state.refuse_in_demo().map_err(SyncFailureView::config)?;
    // Hostnames are case-insensitive, so one server spelled with different
    // casing must map to one durable source instance. Usernames stay
    // exactly as configured: the identity contract case-folds hosts, not
    // usernames (see ImapConfig::connector_key), so two casings of a
    // username are distinct accounts until the server says otherwise.
    let host = host.trim().to_lowercase();
    let username = username.trim().to_string();
    if username.is_empty() {
        return Err(SyncFailureView::config("username is required"));
    }
    let folders = folders.unwrap_or_default();
    let port = port.unwrap_or(imap::GMAIL_IMAP_PORT);

    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| SyncFailureView::config(e.to_string()))?;
    let pool = Arc::clone(&state.pool);
    let secrets = Arc::clone(secrets.inner());
    // Secret resolution (keyring), token refresh (blocking HTTP), and the
    // network sync itself all block; keep them off the UI thread. The
    // outcome carries the saved-password store even on failure (a password
    // is saved before the connection is attempted), so the UI can state
    // where the credential lives — the honest-credential rule.
    tauri::async_runtime::spawn_blocking(move || -> Result<ImapSyncOutcome, SyncFailureView> {
        let auth = auth.as_deref().unwrap_or("password");
        // Only the Gmail sign-in flow has a sensible default host; every
        // other server must be named explicitly.
        if host.is_empty() && auth != "oauth" {
            return Err(SyncFailureView::config(
                "host is required (e.g. imap.gmail.com)",
            ));
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
                        SyncFailureView::config("no password stored for this account — enter one")
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
                let config = oauth_config().map_err(SyncFailureView::config)?;
                let refresh = secrets.load_refresh(&host, &username).ok_or_else(|| {
                    SyncFailureView::config(
                        "no Google sign-in stored for this account — use \u{201c}Sign in with \
                         Google\u{201d} first",
                    )
                })?;
                let tokens = oauth::refresh_access_token(&config, &refresh)
                    .map_err(|e| SyncFailureView::classified(&e, None))?;
                (
                    ImapAuth::Xoauth2 {
                        access_token: tokens.access_token,
                    },
                    None,
                )
            }
            other => {
                return Err(SyncFailureView::config(format!(
                    "unknown auth mode {other:?} (expected \"password\" or \"oauth\")"
                )))
            }
        };

        let config = ImapConfig {
            host,
            port,
            username,
            auth: imap_auth,
            folders,
        };
        // The vault-core sync records the failure on the account row
        // (class + message) so the accounts panel shows what fell, why,
        // and when — surviving restarts.
        imap::sync_with(&config, &PoolSync(&pool), Some(&data_dir))
            .map_err(|e| SyncFailureView::classified(&e, saved_to))
            .map(|folders| ImapSyncOutcome { folders, saved_to })
    })
    .await
    .map_err(|e| SyncFailureView::config(format!("sync task failed: {e}")))?
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
    state.refuse_in_demo()?; // the sample archive replaces personal seeding
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

// ---------------------------------------------------------------------------
// Isolated sample archive (U02, first-source journey)
// ---------------------------------------------------------------------------

/// Whether the isolated sample archive is open. The demo lives in a
/// private in-memory vault seeded with the built-in demo corpus; reads
/// route there while it is open and personal writes refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct DemoMode {
    active: bool,
}

/// Open the sample archive: a fresh in-memory canonical database,
/// seeded through the same bounded pipeline every import uses. Demo
/// flows never read or write the personal `vault.db` — leaving the
/// demo drops the in-memory copy entirely, so demo rows can never mix
/// into the personal vault (the M3.4 boundary). Re-entering replaces
/// any earlier demo.
#[tauri::command]
async fn demo_enter(state: State<'_, AppState>) -> Result<DemoMode, String> {
    let slot = Arc::clone(&state.demo);
    tauri::async_runtime::spawn_blocking(move || {
        let mut storage = Storage::open_in_memory()
            .map_err(|e| format!("cannot open the sample archive: {e}"))?;
        let run = BoundedRun::new("demo-seed");
        let mut no_progress = |_: u64, _: u64, _: &str| {};
        run.run(&DemoSource, &mut storage, &|| false, &mut no_progress)
            .map_err(|e: Error| format!("cannot seed the sample archive: {e}"))?;
        *slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(Arc::new(Mutex::new(storage)));
        Ok(DemoMode { active: true })
    })
    .await
    .map_err(|e| format!("sample-archive task failed: {e}"))?
}

/// Leave the sample archive: drops the in-memory demo vault and returns
/// to the personal one, which was never modified.
#[tauri::command]
async fn demo_leave(state: State<'_, AppState>) -> Result<DemoMode, String> {
    *state
        .demo
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    Ok(DemoMode { active: false })
}

/// Whether the sample archive is open (boot recovery for a webview
/// reload while the demo is open — the demo lives in the app process,
/// not the page).
#[tauri::command]
fn demo_status(state: State<AppState>) -> DemoMode {
    DemoMode {
        active: state.demo_vault().is_some(),
    }
}

// ---------------------------------------------------------------------------
// Bounded import preview (U03, first-source journey)
// ---------------------------------------------------------------------------

/// The wire shape of `preview_import`: the bounded preview plus the
/// free space on the vault's data dir, so the confirm step can warn
/// when the archive likely does not fit (M3.3 capacity guidance).
#[derive(Debug, Clone, Serialize)]
struct PreviewImportView {
    #[serde(flatten)]
    preview: vault_core::preview::ImportPreview,
    /// Free bytes on the vault's data dir (None when unavailable).
    free_bytes: Option<u64>,
}

/// Bounded preview of a picked import input (file, directory, or
/// `.zip`): detected format, sample messages, bounded counts, media
/// presence, and input size — nothing is stored. The confirm step then
/// dispatches to `import_mbox` / `import_chat_export` on the same path.
#[tauri::command]
async fn preview_import(app: AppHandle, path: String) -> Result<PreviewImportView, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let preview =
            vault_core::preview::preview_import(Path::new(&path)).map_err(|e| e.to_string())?;
        let free_bytes = fs4::available_space(&data_dir).ok();
        Ok(PreviewImportView {
            preview,
            free_bytes,
        })
    })
    .await
    .map_err(|e| format!("preview task failed: {e}"))?
}

/// Arm a watched folder: existing and new/modified export files
/// (`.mbox`, WhatsApp `.txt`, Telegram `.json`, Signal conversation
/// `.json`) under it are imported in the background; re-imports never
/// duplicate. Returns the fresh status.
#[tauri::command]
fn watch_add_folder(state: State<AppState>, path: String) -> Result<WatchSnapshot, String> {
    state.refuse_in_demo()?;
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
    state.refuse_in_demo()?;
    let path = path.trim();
    if path.is_empty() {
        return Err("path is required".into());
    }
    state.watcher.remove_folder(path)?;
    Ok(state.watcher.snapshot())
}

/// Re-run a failed watched-folder import: the file must still sit under
/// an enabled watched folder; it is re-enqueued through the watcher's
/// settle window under its unchanged import identity, so already stored
/// messages stay single. Returns the fresh status (its per-unit states
/// tell the UI which units are retryable). A unit that is pending or
/// running is left alone — repeated calls never duplicate work.
#[tauri::command]
fn watch_retry(state: State<AppState>, path: String) -> Result<WatchSnapshot, String> {
    state.refuse_in_demo()?;
    let path = path.trim();
    if path.is_empty() {
        return Err("path is required".into());
    }
    state.watcher.retry(path)?;
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
        /// GP03: the user-confirmed IANA zone for naive timestamps
        /// (None → the documented default assumption).
        confirmed_timezone: Option<String>,
        /// GP03: the explicitly chosen destination source — the run's
        /// dedup namespace (None → the derived name).
        source: Option<String>,
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
    /// M4.4: write a versioned backup container of the whole vault to
    /// a native-picked destination (long-running: reports progress).
    BackupCreate {
        dest: String,
    },
    /// M4.4: validate a picked backup container and stage it for a
    /// journaled restore (long-running: reports progress). Committing
    /// is the separate `restore_commit` command.
    RestorePreview {
        archive: String,
    },
    /// M5.2: prepare one ask — run the fully local search leg under the
    /// active workspace scope and register the exact bounded payload
    /// plus its digest for approval (zero network; cancellable with
    /// progress).
    PrepareAsk {
        question: String,
        scope: Option<search2::AskScope>,
        limit: u32,
        provider: Option<String>,
    },
    /// M5.2: generate the answer for one approved prepared ask —
    /// re-checks the approved digest (stale on question, filters,
    /// endpoint/config, or result-snapshot changes) and sends only the
    /// approved snapshot.
    GenerateAnswer {
        prepared_id: u64,
        digest: String,
        /// The same provider selection prepare used (profile id, `env`,
        /// or `local`); re-resolved and compared against the prepared
        /// endpoint, so a mismatch refuses instead of sending.
        provider: Option<String>,
    },
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
            JobSpec::BackupCreate { .. } => "backup_create",
            JobSpec::RestorePreview { .. } => "restore_preview",
            JobSpec::PrepareAsk { .. } => "prepare_ask_v2",
            JobSpec::GenerateAnswer { .. } => "generate_answer_v2",
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
                // GP03: both optional — absent means the documented
                // defaults (the derived source, the assumed zone).
                let optional_field = |name: &str| -> Option<String> {
                    params
                        .get(name)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                };
                Ok(JobSpec::ImportChatExport {
                    path: string_field("path")?,
                    extract_media,
                    confirmed_timezone: optional_field("confirmed_timezone"),
                    source: optional_field("source"),
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
            "backup_create" => Ok(JobSpec::BackupCreate {
                dest: string_field("dest")?,
            }),
            "restore_preview" => Ok(JobSpec::RestorePreview {
                archive: string_field("archive")?,
            }),
            // M5.2: the v2 job-backed ask flow. `question` follows the
            // `llm_ask` contract (non-blank, ≤ 2000 characters); `scope`
            // is the optional workspace-filter snapshot; `limit` mirrors
            // `llm_ask`'s clamp; `provider` picks the same menu.
            "prepare_ask_v2" => {
                let question = string_field("question")?.trim().to_string();
                if question.is_empty() {
                    return Err("params.question must not be blank".into());
                }
                if question.chars().count() > 2000 {
                    return Err("params.question is too long (max 2000 characters)".into());
                }
                let scope = match params.get("scope") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(
                        serde_json::from_value::<search2::AskScope>(value.clone())
                            .map_err(|e| format!("params.scope is invalid: {e}"))?,
                    ),
                };
                let limit = params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(50)
                    .clamp(1, 200) as u32;
                let provider = params
                    .get("provider")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                Ok(JobSpec::PrepareAsk {
                    question,
                    scope,
                    limit,
                    provider,
                })
            }
            "generate_answer_v2" => {
                let prepared_id = params
                    .get("prepared_id")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "params.prepared_id is required".to_string())?;
                let digest = string_field("digest")?;
                let digest = digest.to_ascii_lowercase();
                if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err("params.digest must be a 64-character SHA-256 hex string".into());
                }
                let provider = params
                    .get("provider")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                Ok(JobSpec::GenerateAnswer {
                    prepared_id,
                    digest,
                    provider,
                })
            }
            other => Err(format!(
                "unknown job kind “{other}” (supported: seed_demo, import_mbox, \
                 import_chat_export, embed_messages, imap_sync, imap_sync_all, \
                 backup_create, restore_preview, prepare_ask_v2, generate_answer_v2)"
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
    submit_job_spec(&app, &state, secrets.inner(), spec)
}

/// What the M5.2 ask job kinds need beyond the shared pool: demo-aware
/// read routing (asks keep answering from the open sample archive — they
/// are read-only, so they may run under the demo), the provider registry
/// (the selection is re-resolved inside the job body so no registry lock
/// is ever held across a network round trip), and the session's pending
/// prepared ask.
struct AskJobDeps {
    vault: ActiveVault,
    registry: Arc<Mutex<LlmRegistry>>,
    prepared: Arc<PreparedAskRegistry>,
}

/// Submit one validated [`JobSpec`] through the shared job engine —
/// the single submission path behind `job_submit` and the tray's
/// *Sync now* (M7), so both take the same demo lockout, queue, and
/// job-record semantics. `Err` is the user-facing refusal (demo
/// lockout, queue full).
fn submit_job_spec(
    app: &AppHandle,
    state: &AppState,
    secrets: &Arc<secrets::Secrets>,
    spec: JobSpec,
) -> Result<JobHandle, String> {
    // Every job kind besides the asks writes the personal vault
    // (imports, syncs, seeds, embeddings, backups, restores) — none of
    // those may run under the demo. The M5.2 ask jobs are read-only
    // over the active vault, so they stay available while the demo is
    // open and read from it.
    if !matches!(
        spec,
        JobSpec::PrepareAsk { .. } | JobSpec::GenerateAnswer { .. }
    ) {
        state.refuse_in_demo()?;
    }
    let pool = Arc::clone(&state.pool);
    let embedder = state.embedder;
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let job_pool = Arc::clone(&pool);
    let secrets = Arc::clone(secrets);
    let switch_gate = Arc::clone(&state.switch_gate);
    let asks = AskJobDeps {
        vault: ActiveVault::from(state),
        registry: Arc::clone(&state.registry),
        prepared: Arc::clone(&state.prepared_asks),
    };
    pool.try_submit_job(spec.kind(), move |ctx: &JobContext| {
        run_job_spec(
            ctx,
            &spec,
            &job_pool,
            embedder,
            &data_dir,
            &secrets,
            &switch_gate,
            &asks,
        )
    })
    .map(|id| JobHandle { id })
    .map_err(|e| e.to_string())
}

/// The tray's *Sync now* (M7): an ordinary `imap_sync_all` round —
/// the same submission path as the jobs panel's manual round, so the
/// tray sync is visible in the jobs panel like any manual one.
fn submit_sync_now(app: &AppHandle) -> Result<JobHandle, String> {
    let state = app.state::<AppState>();
    let secrets = app.state::<Arc<secrets::Secrets>>();
    submit_job_spec(app, &state, &secrets, JobSpec::ImapSyncAll)
}

/// Execute one validated [`JobSpec`] on the job runner thread. All storage
/// access goes through the pool; every kind reports progress and a JSON
/// result summary. (Unit-tested through `JobSpec::parse`; execution is
/// covered end-to-end by vault-core's jobs and coordinator tests.)
#[allow(clippy::too_many_arguments)] // the job kinds need the app's shared deps
fn run_job_spec(
    ctx: &JobContext,
    spec: &JobSpec,
    pool: &DbPool,
    embedder: HashEmbedder,
    data_dir: &Path,
    secrets: &Arc<secrets::Secrets>,
    switch_gate: &SwitchGate,
    asks: &AskJobDeps,
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
            // Resolve the pick exactly like the direct `import_mbox`
            // command — through the shared bounded body (`import_mbox`
            // job-path regression: the pick is a directory of `.eml`
            // files or a `.zip` of them just as often as a single mbox
            // file). The body keeps the zip's temp-tree guard alive for
            // the whole run; dropping it early would un-squash the zip
            // mid-import.
            let mut sink = PoolSink(pool);
            let cancelled = || ctx.is_cancelled();
            let mut progress =
                |done: u64, total: u64, message: &str| ctx.progress(done, total, message);
            let outcome = vault_core::mbox::import_pick_bounded(
                &mut sink,
                Path::new(path),
                data_dir,
                &cancelled,
                &mut progress,
            )
            .map_err(JobError::from);
            let outcome =
                record_import_evidence(ctx, &import_fallback_source(None, path), outcome)?;
            if outcome.cancelled {
                return Err(JobError::Cancelled);
            }
            // The outcome carries the media pass's counters too — the
            // same shape as `import_mbox` and the chat-export import.
            Ok(serde_json::to_value(ChatImportOutcome::from(outcome)).unwrap_or(Value::Null))
        }
        JobSpec::ImportChatExport {
            path,
            extract_media,
            confirmed_timezone,
            source,
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
            let outcome = chat_export::import_bounded_with(
                &mut sink,
                Path::new(&moved_path),
                extract,
                media_dir.as_deref(),
                confirmed_timezone.as_deref(),
                source.as_deref(),
                &cancelled,
                &mut progress,
            );
            let fallback = import_fallback_source(source.as_deref(), path);
            let outcome = record_import_evidence(ctx, &fallback, outcome)?;
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
            let summaries = imap::sync_with(&config, &sink, Some(data_dir))?;
            Ok(serde_json::to_value(&summaries).unwrap_or(Value::Null))
        }
        JobSpec::ImapSyncAll => run_sync_all_round(ctx, pool, secrets, data_dir),
        JobSpec::BackupCreate { dest } => {
            run_backup_create(ctx, switch_gate, data_dir, Path::new(dest))
        }
        JobSpec::RestorePreview { archive } => {
            run_restore_preview(ctx, switch_gate, data_dir, Path::new(archive))
        }
        JobSpec::PrepareAsk {
            question,
            scope,
            limit,
            provider,
        } => run_prepare_ask(
            ctx,
            asks,
            embedder,
            question,
            scope.as_ref(),
            *limit,
            provider.as_deref(),
        ),
        JobSpec::GenerateAnswer {
            prepared_id,
            digest,
            provider,
        } => run_generate_answer(ctx, asks, *prepared_id, digest, provider.as_deref()),
    }
}

// ---------------------------------------------------------------------------
// M5.2: the job-backed ask flow (prepare → approve → generate)
// ---------------------------------------------------------------------------

/// The endpoint fingerprint of one resolved provider selection: what the
/// prepared digest binds (a local selection has no endpoint — nothing
/// would ever be sent). The API key is deliberately absent: rotating a
/// key must not invalidate an approved payload.
fn prepared_endpoint(selection: &Selection) -> PreparedEndpoint {
    match selection {
        Selection::Provider { config, .. } => PreparedEndpoint {
            base_url: config.base_url.clone(),
            model: config.model.clone(),
        },
        Selection::Local { .. } => PreparedEndpoint::local(),
    }
}

/// Map a v2 ask-flow refusal to its job error kind: cancellations are
/// cancellations, approval refusals are invalid input (the client must
/// prepare again), scope/storage faults are storage, provider faults are
/// network.
fn ask_v2_job_error(error: AskV2Error) -> JobError {
    match error {
        AskV2Error::Cancelled => JobError::Cancelled,
        AskV2Error::NoPendingPreparedAsk | AskV2Error::StaleDigest | AskV2Error::ArchiveChanged => {
            JobError::Invalid {
                message: error.to_string(),
            }
        }
        AskV2Error::Vault(message) => JobError::Storage { message },
        AskV2Error::Provider(message) => JobError::Network { message },
    }
}

/// The `prepare_ask_v2` job body: resolve the provider selection (its
/// endpoint fingerprint is what the digest binds), then run the fully
/// local prepare leg — scope validation, scoped retrieval, hybrid
/// ranking, exact payload build, digest — with cooperative cancellation
/// and progress between the pipeline steps. Zero network by
/// construction: no provider client is ever built here, so nothing can
/// leave the machine before the user approves the payload.
fn run_prepare_ask(
    ctx: &JobContext,
    asks: &AskJobDeps,
    embedder: HashEmbedder,
    question: &str,
    scope: Option<&search2::AskScope>,
    limit: u32,
    provider: Option<&str>,
) -> std::result::Result<Value, JobError> {
    if ctx.is_cancelled() {
        return Err(JobError::Cancelled);
    }
    // Resolve the endpoint and the summarize window under the registry
    // lock, and release it before anything else runs (the config is
    // cloned out — same pattern as `llm_ask`).
    let (endpoint, summarize_top): (PreparedEndpoint, usize) = {
        let registry = asks.registry.lock().map_err(|e| JobError::Internal {
            message: format!("registry lock poisoned: {e}"),
        })?;
        (
            prepared_endpoint(&registry.select(provider)),
            registry.settings().options.summarize_top as usize,
        )
    };
    let cancel = || ctx.is_cancelled();
    let mut progress = |done: u64, total: u64, message: &str| ctx.progress(done, total, message);
    let outcome = asks.vault.with_read(|s| {
        llm::prepare_scoped_ask(
            s,
            &embedder,
            &asks.prepared,
            &endpoint,
            question,
            scope,
            limit,
            summarize_top,
            &cancel,
            &mut progress,
        )
    });
    match outcome {
        Ok(outcome) => Ok(serde_json::to_value(&outcome).unwrap_or(Value::Null)),
        Err(e) => Err(ask_v2_job_error(e)),
    }
}

/// The `generate_answer_v2` job body: re-resolve the provider selection
/// (a changed endpoint/config refuses the stale approval inside the
/// generate leg), consume the digest-bound approval under a short-lived
/// read, then send only the approved payload — the network round trip
/// runs on the job thread with no storage held (the pool's rule), with
/// cooperative cancellation and progress between the steps. The digest
/// approval itself is the explicit consent for this one send — no
/// session-consent flag is involved.
fn run_generate_answer(
    ctx: &JobContext,
    asks: &AskJobDeps,
    prepared_id: u64,
    digest: &str,
    provider: Option<&str>,
) -> std::result::Result<Value, JobError> {
    if ctx.is_cancelled() {
        return Err(JobError::Cancelled);
    }
    let selection = {
        let registry = asks.registry.lock().map_err(|e| JobError::Internal {
            message: format!("registry lock poisoned: {e}"),
        })?;
        registry.select(provider)
    };
    let endpoint = prepared_endpoint(&selection);
    let backend = match &selection {
        Selection::Provider { config, .. } => Some(AnthropicLlm::new(config.clone())),
        Selection::Local { .. } => None,
    };
    let cancel = || ctx.is_cancelled();
    let mut progress = |done: u64, total: u64, message: &str| ctx.progress(done, total, message);
    // Local phase (approval consumption + staleness re-checks) under a
    // short-lived read; nothing network-shaped happens inside it.
    let prepared = asks
        .vault
        .with_read(|s| {
            llm::take_approved_ask(
                s,
                &asks.prepared,
                &endpoint,
                prepared_id,
                digest,
                &cancel,
                &mut progress,
            )
        })
        .map_err(ask_v2_job_error)?;
    // Cooperative checkpoint between the re-check and the send.
    if ctx.is_cancelled() {
        return Err(JobError::Cancelled);
    }
    // Network phase on the job thread: only the approved snapshot.
    let answer = llm::send_approved_payload(
        &prepared,
        backend.as_ref().map(|b| b as &dyn llm::Llm),
        &cancel,
        &mut progress,
    );
    match answer {
        Ok(answer) => Ok(serde_json::to_value(&answer).unwrap_or(Value::Null)),
        Err(e) => Err(ask_v2_job_error(e)),
    }
}

/// The evidence key for an import that failed before its source name
/// resolved: the explicitly chosen destination source when the request
/// carried one (GP03), else the picked file's own name — the health
/// overview can still say which pick failed, and when.
fn import_fallback_source(destination: Option<&str>, path: &str) -> String {
    destination
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string())
        })
}

/// Record one finished import attempt's per-source evidence (archive
/// health): a completed or cancelled run attributes to the run's own
/// dedup namespace (`outcome.summary.source` — always resolved by the
/// bounded driver), a failed run to `fallback_source` (it never got far
/// enough to name one). Succeeded and cancelled runs carry their
/// counts; failures carry the error. Best effort by design: the
/// evidence is a side record and must never mask the import's own
/// outcome — its write failure cannot fail (or "un-fail") the import.
fn record_import_evidence<E: std::fmt::Display>(
    ctx: &JobContext,
    fallback_source: &str,
    result: std::result::Result<vault_core::bounded::BoundedOutcome, E>,
) -> std::result::Result<vault_core::bounded::BoundedOutcome, E> {
    let at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    match result {
        Ok(outcome) => {
            let source = outcome.summary.source.clone();
            let attempt = if outcome.cancelled {
                ImportAttempt::Cancelled {
                    counts: ImportCounts::of_summary(&outcome.summary),
                }
            } else {
                ImportAttempt::Succeeded {
                    counts: ImportCounts::of_summary(&outcome.summary),
                }
            };
            let _ = ctx.write(move |s| health::record_import_attempt(s, &source, &at, &attempt));
            Ok(outcome)
        }
        Err(error) => {
            let source = fallback_source.to_string();
            let attempt = ImportAttempt::Failed {
                error: error.to_string(),
            };
            let _ = ctx.write(move |s| health::record_import_attempt(s, &source, &at, &attempt));
            Err(error)
        }
    }
}

/// The `backup_create` job body: a versioned container of the whole
/// vault (SQLite snapshot + settings + referenced objects) written to
/// a native-picked destination, verified before it is published.
/// The snapshot runs under the write queue (new writes pause behind it
/// — "backup pauses new writes after current batches finish"); the
/// long container copy then runs on the job runner with progress and
/// cooperative cancellation while search keeps serving.
fn run_backup_create(
    ctx: &JobContext,
    switch_gate: &SwitchGate,
    data_dir: &Path,
    dest: &Path,
) -> std::result::Result<Value, JobError> {
    if ctx.is_cancelled() {
        return Err(JobError::Cancelled);
    }
    // Serialized with the switch entry points: a snapshot racing a
    // restore's drain/switch would run against a half-switched vault
    // (and its write queue dies with the drained pool).
    let _switch_lease = switch_gate
        .try_begin()
        .map_err(|message| JobError::Storage { message })?;
    ctx.progress(0, 0, "taking a consistent database snapshot");
    let dest_parent = dest.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "backup".to_string());
    let snapshot_tmp = dest_parent.join(format!(".{stem}-snapshot-{}", std::process::id()));
    let summary = (|| -> std::result::Result<backup::BackupSummary, JobError> {
        let plan = ctx
            .write({
                let data_dir = data_dir.to_path_buf();
                let snapshot_tmp = snapshot_tmp.clone();
                move |s| backup::snapshot_for_backup(&data_dir, s.connection(), &snapshot_tmp)
            })
            .map_err(backup_job_error)?;
        backup::write_backup_container(
            data_dir,
            &plan,
            dest,
            &|note| {
                ctx.progress(note.done, note.total, note.phase);
            },
            &|| ctx.is_cancelled(),
            &backup::BackupOptions::default(),
        )
        .map_err(backup_job_error)
    })();
    // The snapshot temp never outlives the job: a failed snapshot or a
    // failed/cancelled container write leaves nothing beside the
    // user's picked destination (success consumed it already — this
    // is a no-op then).
    backup::discard_snapshot(&snapshot_tmp);
    let summary = summary?;
    ctx.progress(1, 1, "backup verified");
    // U05: persist the finished (verified) backup in the vault's own
    // `archive_meta` so the Storage & backup panel can say "last
    // verified backup …" after a restart, not only in this session.
    // The record rides `vault.db`, so a restore replaces it with the
    // restored vault's own history — never the replaced vault's.
    ctx.write({
        let summary = summary.clone();
        move |s| backup::record_last_backup(s, &summary)
    })
    .map_err(backup_job_error)?;
    Ok(serde_json::to_value(&summary).unwrap_or(Value::Null))
}

/// The `restore_preview` job body: validate the picked container and
/// stage it under the recovery directory. Nothing in the active vault
/// changes; the returned preview id commits through `restore_commit`.
fn run_restore_preview(
    ctx: &JobContext,
    switch_gate: &SwitchGate,
    data_dir: &Path,
    archive: &Path,
) -> std::result::Result<Value, JobError> {
    if ctx.is_cancelled() {
        return Err(JobError::Cancelled);
    }
    // Serialized with the switch entry points: staging wipes and
    // repopulates the recovery staging directory — exactly the tree a
    // commit's switch phase B moves items out of.
    let _switch_lease = switch_gate
        .try_begin()
        .map_err(|message| JobError::Storage { message })?;
    let fingerprint = ctx
        .read(|s| s.vault_fingerprint())
        .map_err(backup_job_error)?;
    let preview = backup::stage_restore(
        data_dir,
        archive,
        &fingerprint,
        &|note| {
            ctx.progress(note.done, note.total, note.phase);
        },
        &backup::RestoreOptions::default(),
    )
    .map_err(backup_job_error)?;
    ctx.progress(1, 1, "restore validated — review it in Settings");
    Ok(serde_json::to_value(&preview).unwrap_or(Value::Null))
}

/// Map core backup errors into job errors (a cooperative cancellation
/// surfaces as `JobError::Cancelled`, everything else as storage).
fn backup_job_error(e: Error) -> JobError {
    if let Error::Backup(message) = &e {
        if message == "cancelled" {
            return JobError::Cancelled;
        }
    }
    JobError::from(e)
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
    data_dir: &Path,
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
        Some(data_dir),
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
    media: Option<vault_core::bounded::MediaPass<'_>>,
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
    ActiveVault::from(&state).with_read(|s| s.list_source_instances().map_err(|e| e.to_string()))
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
    state.refuse_in_demo()?;
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
    ActiveVault::from(&state).with_read(|s| {
        s.list_failed_items(source_id.as_deref())
            .map_err(|e| e.to_string())
    })
}

// ---------------------------------------------------------------------------
// Multi-account surface (roadmap: the accounts panel)
// ---------------------------------------------------------------------------

/// One account row of the multi-account overview: what the accounts
/// panel renders per account. Never credentials — `imap` carries the
/// connection identity plus which credential *kinds* are stored.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct AccountView {
    /// The durable source instance id (the same id `sources_list`
    /// reports; pause uses it, the search chips filter by it).
    source_id: String,
    /// `"imap"` for live email accounts, `"import"` for file imports
    /// (mbox/chat exports) and legacy namespaces.
    kind: String,
    /// Human-readable label (`imap:host:port:username` or the import's
    /// source name).
    label: String,
    /// Medium of the account's archived messages — the mail-vs-chat
    /// taxonomy behind the source icons: `"mail"` (a live IMAP account
    /// or an email-file import), `"chat"` (a chat-export import),
    /// `"mixed"` when both channels are present, `"unknown"` when
    /// nothing is archived yet. Derived from the messages' own
    /// `channel` column (what the parser actually stored), never the
    /// label.
    medium: String,
    /// IMAP connection identity (never present for imports).
    imap: Option<AccountImap>,
    /// Archived messages of the account (trigger-maintained counters).
    message_count: u64,
    /// Open rows in the failed-item ledger (the account's sync backlog).
    failed_items: u64,
    /// `"connected"` after a successful sync, `"disconnected"`
    /// otherwise.
    state: String,
    /// Paused: excluded from every coordinator round; the archive and
    /// cursors are kept (M2.3 exclusion re-used as the account pause).
    excluded: bool,
    last_attempt_at: Option<String>,
    last_success_at: Option<String>,
    /// The last failed sync's user-facing message — the account-level
    /// error state that survives restarts (cold review §2.2/#5); `None`
    /// after a success.
    last_error: Option<String>,
    /// When the last failed sync was recorded.
    last_error_at: Option<String>,
}

/// The non-secret IMAP identity of one account, parsed from the stored
/// source configuration (see [`StoredImapConfig`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
struct AccountImap {
    host: String,
    port: u16,
    username: String,
    /// Configured folders (empty = the connector's default set).
    folders: Vec<String>,
    /// Whether the secret store holds an app password for the account.
    has_password: bool,
    /// Whether the secret store holds a Google sign-in refresh token.
    has_oauth: bool,
}

/// Assemble the accounts overview from the stored source instances, the
/// per-account counts, the per-source channel flags (the mail-vs-chat
/// taxonomy), and a credential probe (`(password?, oauth?)` per stored
/// IMAP config — the keyring in the app, plain closures in tests).
/// Pure: unit-tested below, command-thread free.
fn assemble_accounts(
    instances: &[SourceInstance],
    counts: &[vault_core::storage::AccountCounts],
    channels: &[vault_core::storage::AccountChannels],
    credentials: impl Fn(&StoredImapConfig) -> (bool, bool),
) -> Vec<AccountView> {
    let counts: std::collections::HashMap<&str, &vault_core::storage::AccountCounts> =
        counts.iter().map(|c| (c.source_id.as_str(), c)).collect();
    let channels: std::collections::HashMap<&str, &vault_core::storage::AccountChannels> =
        channels.iter().map(|c| (c.source_id.as_str(), c)).collect();
    instances
        .iter()
        .map(|instance| {
            let zero = vault_core::storage::AccountCounts {
                source_id: String::new(),
                messages: 0,
                failed_items: 0,
            };
            let no_channels = vault_core::storage::AccountChannels {
                source_id: String::new(),
                has_chat: false,
                has_email: false,
            };
            let count = counts.get(instance.id.as_str()).copied().unwrap_or(&zero);
            let channel = channels
                .get(instance.id.as_str())
                .copied()
                .unwrap_or(&no_channels);
            let imap = StoredImapConfig::of(instance).map(|stored| {
                let (has_password, has_oauth) = credentials(&stored);
                AccountImap {
                    host: stored.host,
                    port: stored.port,
                    username: stored.username,
                    folders: stored.folders,
                    has_password,
                    has_oauth,
                }
            });
            AccountView {
                source_id: instance.id.clone(),
                kind: if instance.kind == "imap" {
                    "imap".into()
                } else {
                    "import".into()
                },
                label: instance.label.clone(),
                // IMAP is mail by construction (kind decides). Imports
                // follow what their parsers actually stored — the
                // channel flags — with "unknown" when nothing is
                // archived yet and "mixed" if one namespace ever held
                // both channels.
                medium: if instance.kind == "imap" {
                    "mail".into()
                } else {
                    match (channel.has_chat, channel.has_email) {
                        (true, false) => "chat".into(),
                        (true, true) => "mixed".into(),
                        (false, true) => "mail".into(),
                        (false, false) => "unknown".into(),
                    }
                },
                imap,
                message_count: count.messages,
                failed_items: count.failed_items,
                state: instance.state.clone(),
                excluded: instance.excluded,
                last_attempt_at: instance.last_attempt_at.clone(),
                last_success_at: instance.last_success_at.clone(),
                last_error: instance.last_error.clone(),
                last_error_at: instance.last_error_at.clone(),
            }
        })
        .collect()
}

/// Every account in one overview (the accounts panel): IMAP accounts and
/// import accounts with per-account message counts, failed-item backlog,
/// last-sync stamps, pause state, and which credential kinds are stored.
/// One bounded read (instances + counter rows) plus one keyring probe per
/// IMAP account — keyring access happens off the UI thread.
#[tauri::command]
async fn accounts_list(
    state: State<'_, AppState>,
    secrets: State<'_, Arc<secrets::Secrets>>,
) -> Result<Vec<AccountView>, String> {
    let vault = ActiveVault::from(&state);
    let secrets = Arc::clone(secrets.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let (instances, counts, channels) = vault
            .with_read(|s| -> vault_core::Result<_> {
                Ok((
                    s.list_source_instances()?,
                    s.account_counts()?,
                    s.account_channel_flags()?,
                ))
            })
            .map_err(|e: vault_core::Error| e.to_string())?;
        Ok(assemble_accounts(
            &instances,
            &counts,
            &channels,
            |stored| {
                (
                    secrets.load(&stored.host, &stored.username).is_some(),
                    secrets
                        .load_refresh(&stored.host, &stored.username)
                        .is_some(),
                )
            },
        ))
    })
    .await
    .map_err(|e| format!("accounts task failed: {e}"))?
}

/// Forget one IMAP account's credentials (the accounts panel's removal
/// action): the app password and the Google sign-in refresh token leave
/// the OS keyring / session memory. The account's archive, cursors, and
/// configuration stay — only the secrets go. Import accounts have no
/// stored credentials; an unknown id is an error, never a silent no-op.
#[tauri::command]
async fn account_forget_credentials(
    state: State<'_, AppState>,
    secrets: State<'_, Arc<secrets::Secrets>>,
    source_id: String,
) -> Result<bool, String> {
    state.refuse_in_demo()?;
    let vault = ActiveVault::from(&state);
    let secrets = Arc::clone(secrets.inner());
    tauri::async_runtime::spawn_blocking(move || {
        let instance = vault
            .with_read(|s| s.list_source_instances())
            .map_err(|e: vault_core::Error| e.to_string())?
            .into_iter()
            .find(|i| i.id == source_id)
            .ok_or_else(|| format!("unknown account {source_id:?}"))?;
        let stored = StoredImapConfig::of(&instance).ok_or_else(|| {
            format!(
                "{} is an import account — it stores no credentials",
                instance.label
            )
        })?;
        Ok(secrets.clear_account(&stored.host, &stored.username))
    })
    .await
    .map_err(|e| format!("forget-credentials task failed: {e}"))?
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
/// keep committing while a large export writes. The output is staged in
/// a temp sibling of `path` and renamed into place only once complete —
/// a failed export never truncates a previously valid file.
#[tauri::command]
async fn export_search(
    state: State<'_, AppState>,
    query: SearchQuery,
    format: String,
    path: String,
) -> Result<ExportSummary, String> {
    let format = ExportFormat::parse(&format).map_err(|e| e.to_string())?;
    let vault = ActiveVault::from(&state);
    tauri::async_runtime::spawn_blocking(move || {
        vault
            .with_read(|s| -> CoreResult<ExportSummary> {
                staged_write(Path::new(&path), |out| {
                    export::export_search(s, &query, format, out)
                })
            })
            .map_err(|e: Error| e.to_string())
    })
    .await
    .map_err(|e| format!("export task failed: {e}"))?
}

/// Export all matches of the executed v2 search, retaining instance ids,
/// attachment/import/conversation filters, timezone bounds, and ordering.
/// The legacy `export_search` command remains available to older callers.
#[tauri::command]
async fn export_search_v2(
    state: State<'_, AppState>,
    request: SearchArchiveRequest,
    format: String,
    path: String,
) -> Result<ExportSummary, AppError> {
    let format = ExportFormat::parse(&format).map_err(AppError::from)?;
    // Reject invalid queries before creating/truncating the destination.
    request.normalize().map_err(AppError::from)?;
    let vault = ActiveVault::from(&state);
    tauri::async_runtime::spawn_blocking(move || {
        vault
            .with_read(|s| -> CoreResult<ExportSummary> {
                staged_write(Path::new(&path), |out| {
                    export::export_search_v2(s, &request, format, out)
                })
            })
            .map_err(AppError::from)
    })
    .await
    .map_err(|e| AppError::internal(format!("export task failed: {e}")))?
}

/// Export the whole conversation containing message `id` (oldest →
/// newest; a message with no conversation exports as one row) to
/// `path`. `None` when no message has that id.
#[tauri::command]
async fn export_chat(
    state: State<'_, AppState>,
    id: i64,
    format: String,
    path: String,
) -> Result<Option<ExportSummary>, String> {
    let format = ExportFormat::parse(&format).map_err(|e| e.to_string())?;
    let vault = ActiveVault::from(&state);
    tauri::async_runtime::spawn_blocking(move || {
        vault
            .with_read(|s| -> CoreResult<Option<ExportSummary>> {
                // Existence is checked before the destination is touched: an
                // unknown id (message deleted between list render and export
                // click) must leave no file at the user-chosen path. The
                // staged write re-checks through `export_chat` itself and
                // declines to publish if the anchor vanished meanwhile.
                if s.get(id)?.is_none() {
                    return Ok(None);
                }
                staged_write_opt(Path::new(&path), |out| {
                    export::export_chat(s, id, format, out)
                })
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("export task failed: {e}"))?
}

/// Archive counts: total and per source.
#[tauri::command]
fn stats(state: State<AppState>) -> Result<message::Stats, String> {
    ActiveVault::from(&state)
        .with_read(|s| s.stats())
        .map_err(|e| e.to_string())
}

/// Dashboard aggregates in one round trip: total messages, the archive's
/// date range, per-source and per-day counts, and the heaviest chats and
/// authors (bounded by `top`, default 10, clamped to 100).
#[tauri::command]
fn archive_stats(state: State<AppState>, top: Option<u32>) -> Result<ArchiveStats, String> {
    ActiveVault::from(&state)
        .with_read(|s| s.archive_stats(top.unwrap_or(DEFAULT_TOP)))
        .map_err(|e| e.to_string())
}

/// The "what's new since your last visit" boot digest (the M7
/// follow-up): new messages per source plus finished or failed imports
/// and syncs since the previous read — what changed while the app was
/// closed or hidden in the tray. One call is one visit: the digest is
/// derived through a pool read connection (WAL — it never queues
/// behind writes), and the new last-visit timestamp is recorded on the
/// write queue, so each read advances the cutoff exactly once and the
/// next call counts only newer changes. The first visit (no stored
/// record) is a quiet, deliberately empty digest. The last-visit
/// record lives in the personal vault's own `archive_meta`, so it is
/// vault-scoped — switching or restoring a vault never crosses digests
/// (which is also why this reads the personal pool directly, not the
/// demo-routed active vault). Message counts mean newly archived
/// messages: re-imports that dedup keep their original `created_at`
/// and never count twice. If the advance write fails the call errors
/// and the old cutoff stays — the next call would repeat the same
/// digest, never lose one.
#[tauri::command]
async fn whats_new_digest(state: State<'_, AppState>) -> Result<WhatsNewDigest, String> {
    let pool = Arc::clone(&state.pool);
    tauri::async_runtime::spawn_blocking(move || {
        let now = Utc::now().to_rfc3339();
        let digest = pool
            .with_read(|s| whats_new::read_digest(s, &now))
            .map_err(|e| e.to_string())?;
        pool.submit_write(move |s| whats_new::record_last_visit(s, &now))
            .map_err(|e| e.to_string())?;
        Ok(digest)
    })
    .await
    .map_err(|e| format!("what's-new task failed: {e}"))?
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

// ---------------------------------------------------------------------------
// mcmedia:// read-only media protocol (M4.3)
// ---------------------------------------------------------------------------

/// Serve one media object request on the blocking pool: an exact-range
/// slice (`206`), the whole object (`200`), `416` for unsatisfiable
/// ranges, `404` for anything that is not a store object. Every
/// response — error or data — carries the hard media CSP and
/// `nosniff`, and no HTML content type is ever served (see
/// [`media_proto::content_type_for`]).
fn serve_media_object(
    path: &Path,
    name: &str,
    range: Option<&str>,
) -> tauri::http::Response<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let not_found = |status: u16, message: &str| {
        tauri::http::Response::builder()
            .status(status)
            .header("Content-Type", "text/plain; charset=utf-8")
            .header("Content-Security-Policy", media_proto::MEDIA_RESPONSE_CSP)
            .header("X-Content-Type-Options", "nosniff")
            .body(message.as_bytes().to_vec())
            .expect("static media error response")
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return not_found(404, "no such media object");
    };
    if !meta.is_file() {
        return not_found(404, "no such media object");
    }
    let len = meta.len();
    let builder = tauri::http::Response::builder()
        .header("Accept-Ranges", "bytes")
        .header("Content-Type", media_proto::content_type_for(name))
        .header("Content-Security-Policy", media_proto::MEDIA_RESPONSE_CSP)
        .header("X-Content-Type-Options", "nosniff")
        // Read-only local data: any same-page fetch (and the e2e range
        // checks) may read it, including the headers that prove the
        // serving contract (CSP, ranges, content type).
        .header("Access-Control-Allow-Origin", "*")
        .header(
            "Access-Control-Expose-Headers",
            "Accept-Ranges, Content-Length, Content-Type, Content-Range, \
             Content-Security-Policy, X-Content-Type-Options",
        );
    let read_exact =
        |file: &mut std::fs::File, start: u64, count: u64| -> std::io::Result<Vec<u8>> {
            let mut buf = vec![0u8; count as usize];
            file.seek(SeekFrom::Start(start))?;
            let mut filled = 0usize;
            while filled < buf.len() {
                let n = file.read(&mut buf[filled..])?;
                if n == 0 {
                    break; // object shrank mid-read: serve what is there
                }
                filled += n;
            }
            buf.truncate(filled);
            Ok(buf)
        };
    match media_proto::parse_range(range, len) {
        media_proto::ByteRange::Unsatisfiable => builder
            .status(416)
            .header("Content-Range", format!("bytes */{len}"))
            .body(Vec::new())
            .unwrap_or_else(|_| not_found(500, "bad response")),
        media_proto::ByteRange::Partial { start, end } => {
            let Ok(mut file) = std::fs::File::open(path) else {
                return not_found(404, "no such media object");
            };
            let count = end - start + 1;
            match read_exact(&mut file, start, count) {
                Ok(buf) => builder
                    .status(206)
                    .header("Content-Range", format!("bytes {start}-{end}/{len}"))
                    .body(buf)
                    .unwrap_or_else(|_| not_found(500, "bad response")),
                Err(_) => not_found(500, "media read failed"),
            }
        }
        media_proto::ByteRange::Full => {
            let Ok(mut file) = std::fs::File::open(path) else {
                return not_found(404, "no such media object");
            };
            match read_exact(&mut file, 0, len) {
                Ok(buf) => builder
                    .status(200)
                    .body(buf)
                    .unwrap_or_else(|_| not_found(500, "bad response")),
                Err(_) => not_found(500, "media read failed"),
            }
        }
    }
}

/// Response text for a recognized headless CLI flag, printed by [`run`]
/// before the GUI starts.
///
/// `--version`/`-V` and `--help`/`-h` exist so packaged builds can be
/// smoke-tested headlessly without a window server (see
/// `scripts/dmg-smoke-test.sh`): they print to stdout and exit 0 instead
/// of opening the vault window. Anything else — including no arguments —
/// returns `None` and the GUI launches as usual.
fn cli_flag_response(args: &[String]) -> Option<String> {
    for arg in args {
        match arg.as_str() {
            "--version" | "-V" => {
                return Some(format!("mailchat-vault {}\n", env!("CARGO_PKG_VERSION")));
            }
            "--help" | "-h" => {
                return Some(
                    concat!(
                        "Mailchat Vault — local email & chat archive desktop app.\n",
                        "\n",
                        "Usage: mailchat-vault [--version | --help]\n",
                        "\n",
                        "Without flags the vault window opens. Flags:\n",
                        "  -V, --version  print the version and exit\n",
                        "  -h, --help     print this help and exit\n",
                    )
                    .to_string(),
                );
            }
            _ => {}
        }
    }
    None
}

/// What a window-close request should do (M7 keep-alive). Pure so the
/// decision is unit-testable; the `on_window_event` handler applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseAction {
    /// Default (pre-M7, unchanged) behavior: the window closes, and the
    /// app exits when its last window is gone.
    Close,
    /// Keep-alive: prevent the close and hide the window; the tray keeps
    /// the process reachable between opens.
    HideToTray,
}

/// The close decision: hide **only** when the user opted into
/// keep-alive AND the tray icon is actually up — a hidden process
/// without a way back must never exist (the D10 amendment's degradation
/// rule).
fn close_action(keep_running_in_background: bool, tray_up: bool) -> CloseAction {
    if keep_running_in_background && tray_up {
        CloseAction::HideToTray
    } else {
        CloseAction::Close
    }
}

pub fn run() {
    // Headless CLI flags: handle them before any GUI/window-server work
    // so the packaged binary answers `--version`/`--help` anywhere,
    // including a CI runner with no display.
    if let Some(text) = cli_flag_response(
        &std::env::args_os()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
    ) {
        print!("{text}");
        return;
    }
    tauri::Builder::default()
        // M7 single-instance guard — registered first, before any other
        // plugin could race a second launch. A second launch focuses
        // this instance's window instead of opening a second pool over
        // the same vault; the guard is scoped per user session and app
        // identity (on Linux, the session bus name), which for the
        // shipped app equals per-vault (one app data dir per user).
        // Where no desktop IPC channel exists (a bus-less CI display),
        // the guard is inert and launching behaves exactly as before.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main_window(app);
        }))
        // M7: the OS login-item registration surface (Rust-side only —
        // `settings_save` applies it; no frontend commands exposed).
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        // M4.3: the read-only media protocol. Registered asynchronously
        // and served on the blocking pool so no media read — full or
        // ranged — can stall the UI thread; range requests answer with
        // an exact slice, so arbitrarily large objects stream on demand.
        .register_asynchronous_uri_scheme_protocol("mcmedia", |ctx, request, responder| {
            let data_dir = ctx.app_handle().path().app_data_dir().ok();
            let range = request
                .headers()
                .get("Range")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let name = media_proto::object_name_of(request.uri().path());
            let (Some(data_dir), Some(name)) = (data_dir, name) else {
                responder.respond(
                    tauri::http::Response::builder()
                        .status(404)
                        .header("Content-Type", "text/plain; charset=utf-8")
                        .header("Content-Security-Policy", media_proto::MEDIA_RESPONSE_CSP)
                        .header("X-Content-Type-Options", "nosniff")
                        .body(b"no such media object".to_vec())
                        .expect("static media 404 response"),
                );
                return;
            };
            let path = media_proto::store_path(&data_dir, &name);
            tauri::async_runtime::spawn_blocking(move || {
                let response = serve_media_object(&path, &name, range.as_deref());
                responder.respond(response);
            });
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
            std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
            // Copies left behind by a previous session's external opens
            // never outlive this one's first open.
            purge_media_open_copies();
            // M4.4: resolve an interrupted restore/return switch before
            // anything opens the vault — `switching` rolls back to the
            // intact previous vault, `staged` rolls forward to the new
            // one; either way this app start opens one verified vault.
            // A failed recovery fails CLOSED: opening anyway could spawn
            // the pool over a missing vault.db (a fresh empty database)
            // whose new session writes the next restart's rollback would
            // then silently discard. The outcome is kept for the U05
            // restart UX: the recovery panel says which vault ended up
            // active when a restart resolved an interrupted switch. A
            // resolved (rolled) outcome carries the `created_at` of the
            // journal it resolved, so a later in-session switch that
            // overwrites the journal never inherits the stale word.
            let startup_recovery = match backup::recover_interrupted_switch(&data_dir) {
                Ok(recovery) => StartupRecovery { outcome: recovery },
                Err(e) => {
                    return Err(format!(
                        "an interrupted vault switch could not be resolved: {e} — \
                         the vault will not be opened in this state; check the \
                         recovery directory or restore from a healthy backup"
                    )
                    .into());
                }
            };
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
            let registry =
                LlmRegistry::load(settings, Box::new(|name| std::env::var(name).ok()), |id| {
                    secrets.load_llm_key(id)
                });
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
                    let data_dir = data_dir.clone();
                    Arc::new(move |ctx: &JobContext| {
                        run_sync_all_round(ctx, ctx.pool(), &secrets, &data_dir)
                    })
                },
                move |status| {
                    let _ = emit_handle.emit("scheduler-status", SyncScheduleView::of(status));
                },
            ));
            app.manage(AppState {
                pool,
                registry: Arc::new(Mutex::new(registry)),
                settings_path,
                embedder: HashEmbedder::new(),
                watcher,
                scheduler,
                data_dir,
                switch_gate: Arc::new(SwitchGate::default()),
                startup_recovery,
                demo: Arc::new(Mutex::new(None)),
                prepared_asks: Arc::new(PreparedAskRegistry::new()),
            });
            app.manage(Arc::clone(&secrets));
            // M7: with keep-alive saved on, the tray must exist from the
            // first moment the window could close — build it at boot. A
            // failed build only degrades (close exits); the close
            // handler hides only when the tray icon is actually up.
            let keep_alive = app
                .state::<AppState>()
                .registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .settings()
                .options
                .keep_running_in_background;
            if keep_alive {
                if let Err(e) = tray::ensure(app.handle()) {
                    eprintln!("tray not available: {e}");
                }
            }
            Ok(())
        })
        // M7 keep-alive (D10 amendment): with the opt-in option on AND a
        // tray icon actually up, a window close hides to the tray instead
        // of exiting — the process (and with it the opt-in scheduler,
        // folder watching, and notifications) spans window closes. Any
        // other combination keeps the exact pre-M7 behavior.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let keep_running = window
                    .app_handle()
                    .try_state::<AppState>()
                    .map(|state| {
                        state
                            .registry
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .settings()
                            .options
                            .keep_running_in_background
                    })
                    .unwrap_or(false);
                let tray_up = tray::exists(window.app_handle());
                if keep_running && !tray_up {
                    // Loud degradation warning: keep-alive is on but there
                    // is no tray to hide to — closing exits.
                    eprintln!(
                        "keep-alive on but the tray icon is not up: closing the window exits"
                    );
                }
                if close_action(keep_running, tray_up) == CloseAction::HideToTray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
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
            llm_test,
            import_mbox,
            import_chat_export,
            sync_imap,
            sources_list,
            source_set_excluded,
            source_failures,
            accounts_list,
            account_forget_credentials,
            sync_schedule_get,
            sync_schedule_set,
            watch_add_folder,
            watch_remove_folder,
            watch_retry,
            watch_status,
            oauth_gmail_login,
            oauth_status,
            job_submit,
            job_get,
            job_list,
            job_events_since,
            job_cancel,
            seed_demo,
            demo_enter,
            demo_leave,
            demo_status,
            preview_import,
            stats,
            archive_stats,
            whats_new_digest,
            export_search,
            export_search_v2,
            export_chat,
            search_archive_v2,
            message_detail_v2,
            conversation_context_v2,
            list_import_scopes_v2,
            saved_searches_list,
            saved_searches_save,
            saved_searches_delete,
            export_messages_v2,
            save_text,
            media_reconcile,
            media_reconcile_detail,
            media_usage,
            media_unlinked_page,
            media_export,
            media_open_external,
            backup_inspect,
            restore_commit,
            recovery_status,
            last_backup,
            last_media_verification,
            last_imports,
            recovery_forget,
            return_to_previous_vault
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
    fn close_action_hides_only_with_opt_in_and_a_usable_tray() {
        // Pre-M7 default (and the toggle off): close exits, everywhere.
        assert_eq!(close_action(false, true), CloseAction::Close);
        assert_eq!(close_action(false, false), CloseAction::Close);
        // Opt-in with the tray icon up: hide to tray.
        assert_eq!(close_action(true, true), CloseAction::HideToTray);
        // Opt-in but the tray icon never came up: never a hidden
        // unreachable process — closing degrades to the pre-M7 exit.
        assert_eq!(close_action(true, false), CloseAction::Close);
    }

    #[test]
    fn import_fallback_source_prefers_destination_then_the_picked_name() {
        // Archive health: a failed import that never resolved its source
        // name attributes to the explicitly chosen destination source
        // (trimmed), else the picked file's own name — the overview can
        // still say which pick failed, and when.
        assert_eq!(
            import_fallback_source(Some("  Work chats "), "/anywhere/chat.zip"),
            "Work chats"
        );
        // A blank destination is as good as none: fall back to the name.
        assert_eq!(
            import_fallback_source(Some("   "), "/home/you/Downloads/chat.zip"),
            "chat.zip"
        );
        assert_eq!(import_fallback_source(None, "export.txt"), "export.txt");
        // A bare file name (no directory component) is its own name.
        assert_eq!(import_fallback_source(None, "chat.zip"), "chat.zip");
    }

    #[test]
    fn last_switch_view_merges_origin_with_startup_recovery() {
        // U05 restart UX: the journal says where the switch came from;
        // only this session's startup recovery knows whether a restart
        // resolved it — the merged line must never claim the restored
        // vault is active after a rollback.
        let restored = backup::LastSwitchOrigin {
            kind: backup::SwitchOriginKind::Restored,
            archive: Some(PathBuf::from("/media/vault.mcbackup")),
            created_at: "2026-11-27T15:30:45Z".to_string(),
        };
        let rolled = StartupRecovery {
            outcome: backup::SwitchRecovery::RolledBack {
                previous_slot: "previous-20261127153045-abc123".to_string(),
                journal_created_at: "2026-11-27T15:30:45Z".to_string(),
            },
        };
        let clean = last_switch_view(
            &restored,
            startup_recovery_word(
                &StartupRecovery {
                    outcome: backup::SwitchRecovery::AlreadyCompleted {
                        previous_slot: None,
                    },
                },
                Some("2026-11-27T15:30:45Z"),
            ),
        );
        assert_eq!(clean.kind, "restored");
        assert_eq!(
            clean.archive.as_deref(),
            Some(Path::new("/media/vault.mcbackup"))
        );
        assert_eq!(clean.recovery, "none");

        let rolled_back = last_switch_view(
            &restored,
            startup_recovery_word(&rolled, Some("2026-11-27T15:30:45Z")),
        );
        assert_eq!(rolled_back.recovery, "rolled_back");

        let rolled_forward = last_switch_view(
            &restored,
            startup_recovery_word(
                &StartupRecovery {
                    outcome: backup::SwitchRecovery::RolledForward {
                        previous_slot: "previous-20261127153045-abc123".to_string(),
                        journal_created_at: "2026-11-27T15:30:45Z".to_string(),
                    },
                },
                Some("2026-11-27T15:30:45Z"),
            ),
        );
        assert_eq!(rolled_forward.recovery, "rolled_forward");

        // A later in-session switch overwrites the journal: the stale
        // rolled-back outcome must not leak onto the new switch's
        // origin — it completed cleanly in this session.
        let newer = last_switch_view(
            &restored,
            startup_recovery_word(&rolled, Some("2026-11-27T18:00:00Z")),
        );
        assert_eq!(newer.recovery, "none");

        let returned = backup::LastSwitchOrigin {
            kind: backup::SwitchOriginKind::Returned,
            archive: None,
            created_at: "2026-11-27T16:00:00Z".to_string(),
        };
        let view = last_switch_view(
            &returned,
            startup_recovery_word(
                &StartupRecovery {
                    outcome: backup::SwitchRecovery::None,
                },
                Some("2026-11-27T16:00:00Z"),
            ),
        );
        assert_eq!(view.kind, "returned");
        assert_eq!(view.archive, None);
        assert_eq!(view.since, "2026-11-27T16:00:00Z");
    }

    #[test]
    fn switch_gate_serializes_concurrent_switches() {
        // The serialization the two switch commands rely on: a second
        // attempt while one lease is held refuses fast with the
        // user-facing message; releasing (drop) makes the slot available
        // again, and a panic-path drop cannot wedge the gate.
        let gate = SwitchGate::default();
        let lease = gate.try_begin().expect("first switch claims the gate");
        let second = gate.try_begin();
        assert!(
            matches!(&second, Err(m) if m == "a vault switch or backup is already in progress — wait for it to finish"),
            "a concurrent switch must refuse with the user-facing message"
        );
        drop(lease);
        gate.try_begin()
            .expect("the gate reopens after the switch ends");
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
                extract_media: true,
                confirmed_timezone: None,
                source: None
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
                extract_media: false,
                confirmed_timezone: None,
                source: None
            }
        );
        // GP03: the confirmed zone and destination source round-trip.
        assert_eq!(
            JobSpec::parse(
                "import_chat_export",
                &params(json!({
                    "path": "/c.txt",
                    "confirmed_timezone": "Europe/Berlin",
                    "source": "Family chats"
                }))
            )
            .unwrap(),
            JobSpec::ImportChatExport {
                path: "/c.txt".into(),
                extract_media: true,
                confirmed_timezone: Some("Europe/Berlin".into()),
                source: Some("Family chats".into())
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
    fn job_spec_parses_the_v2_ask_kinds() {
        // Prepare: minimal form (no scope, no provider, default limit).
        assert_eq!(
            JobSpec::parse("prepare_ask_v2", &params(json!({"question": "  vault?  "}))).unwrap(),
            JobSpec::PrepareAsk {
                question: "vault?".into(),
                scope: None,
                limit: 50,
                provider: None,
            }
        );
        // Full form: scope snapshot, explicit limit, provider pick.
        assert_eq!(
            JobSpec::parse(
                "prepare_ask_v2",
                &params(json!({
                    "question": "vault?",
                    "scope": { "author": "ana", "from_date": "2026-10-01", "timezone": "UTC" },
                    "limit": 8,
                    "provider": "env"
                }))
            )
            .unwrap(),
            JobSpec::PrepareAsk {
                question: "vault?".into(),
                scope: Some(search2::AskScope {
                    author: Some("ana".into()),
                    from_date: Some("2026-10-01".into()),
                    timezone: Some("UTC".into()),
                    ..search2::AskScope::default()
                }),
                limit: 8,
                provider: Some("env".into()),
            }
        );
        // The limit mirrors llm_ask's clamp, not a refusal.
        assert_eq!(
            JobSpec::parse(
                "prepare_ask_v2",
                &params(json!({"question": "q", "limit": 99999}))
            )
            .unwrap(),
            JobSpec::PrepareAsk {
                question: "q".into(),
                scope: None,
                limit: 200,
                provider: None,
            }
        );
        // A null scope is the explicit "entire archive".
        assert!(matches!(
            JobSpec::parse(
                "prepare_ask_v2",
                &params(json!({"question": "q", "scope": null}))
            )
            .unwrap(),
            JobSpec::PrepareAsk { scope: None, .. }
        ));
        // Question validation follows the llm_ask contract.
        let err = JobSpec::parse("prepare_ask_v2", &params(json!({}))).unwrap_err();
        assert!(err.contains("params.question"), "{err}");
        let err =
            JobSpec::parse("prepare_ask_v2", &params(json!({"question": "   "}))).unwrap_err();
        assert!(err.contains("params.question"), "{err}");
        let err = JobSpec::parse(
            "prepare_ask_v2",
            &params(json!({"question": "x".repeat(2001)})),
        )
        .unwrap_err();
        assert!(err.contains("2000"), "{err}");
        let err = JobSpec::parse(
            "prepare_ask_v2",
            &params(json!({"question": "q", "scope": { "author": 5 }})),
        )
        .unwrap_err();
        assert!(err.contains("params.scope"), "{err}");

        // Generate: id + approved digest, optional provider echo.
        const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(
            JobSpec::parse(
                "generate_answer_v2",
                &params(json!({"prepared_id": 3, "digest": DIGEST}))
            )
            .unwrap(),
            JobSpec::GenerateAnswer {
                prepared_id: 3,
                digest: DIGEST.into(),
                provider: None,
            }
        );
        // Uppercase hex is normalized to the registry's lowercase form.
        let spec = JobSpec::parse(
            "generate_answer_v2",
            &params(json!({"prepared_id": 3, "digest": DIGEST.to_ascii_uppercase()})),
        )
        .unwrap();
        assert_eq!(
            spec,
            JobSpec::GenerateAnswer {
                prepared_id: 3,
                digest: DIGEST.into(),
                provider: None,
            }
        );
        // The digest must be a 64-char hex string; prepared_id is required.
        for bad in ["", "abc", &DIGEST[..63], &format!("{DIGEST}zz")] {
            let err = JobSpec::parse(
                "generate_answer_v2",
                &params(json!({"prepared_id": 3, "digest": bad})),
            )
            .unwrap_err();
            assert!(err.contains("params.digest"), "{bad}: {err}");
        }
        let err =
            JobSpec::parse("generate_answer_v2", &params(json!({"digest": DIGEST}))).unwrap_err();
        assert!(err.contains("params.prepared_id"), "{err}");
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
                extract_media: true,
                confirmed_timezone: None,
                source: None
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

    #[test]
    fn llm_test_result_serializes_flat() {
        let result = LlmTestResult {
            ok: false,
            latency_ms: 12,
            detail: "invalid API key (HTTP 401)".into(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains(r#""ok":false"#));
        assert!(json.contains(r#""latency_ms":12"#));
        assert!(json.contains("invalid API key"));
    }

    // -----------------------------------------------------------------------
    // M5: AI consent gate (evidence: a local TCP recorder plays the
    // provider endpoint — "mock network" — so the tests can prove which
    // asks produced network traffic)
    // -----------------------------------------------------------------------

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use vault_core::jobs::PoolConfig;
    use vault_core::llm::LlmConfig;
    use vault_core::vector::HashEmbedder;

    /// Read one HTTP/1.1 request (headers + Content-Length body) and return
    /// its body (what would leave the machine).
    fn read_recorded_body(stream: &mut std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            let n = stream.read(&mut chunk).expect("read request");
            assert!(n > 0, "connection closed before headers ended");
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let content_length: usize = head
            .to_ascii_lowercase()
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .map(|v| v.trim().parse().expect("content-length"))
            .unwrap_or(0);
        while buf.len() < header_end + content_length {
            let n = stream.read(&mut chunk).expect("read body");
            assert!(n > 0, "connection closed before body ended");
            buf.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&buf[header_end..header_end + content_length]).to_string()
    }

    /// A local recorder standing in for the LLM provider endpoint: every
    /// accepted connection is answered Anthropic-style with the next entry
    /// of `replies`, and the accept loop stops when `stop` is set — so a
    /// test can prove that *no* request ever arrived by setting `stop`
    /// after the ask returned and collecting an empty request list.
    fn spawn_llm_recorder(
        replies: Vec<String>,
        stop: Arc<AtomicBool>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local LLM recorder");
        let addr = listener.local_addr().unwrap();
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let handle = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).expect("blocking stream");
                        let body = read_recorded_body(&mut stream);
                        let reply = replies
                            .get(bodies.len())
                            .cloned()
                            .unwrap_or_else(|| "{}".into());
                        let payload = serde_json::json!({
                            "content": [{ "type": "text", "text": reply }]
                        })
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            payload.len(),
                            payload
                        );
                        stream.write_all(response.as_bytes()).expect("write reply");
                        bodies.push(body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
            bodies
        });
        (format!("http://{addr}/"), handle)
    }

    /// A pool over a throwaway vault seeded with `rows` matching messages.
    fn ask_pool(rows: usize) -> (tempfile::TempDir, Arc<DbPool>) {
        let tmp = tempfile::tempdir().unwrap();
        let pool =
            DbPool::spawn(tmp.path().join("vault.db"), PoolConfig::default(), |_| {}).unwrap();
        pool.submit_write(move |s| {
            for i in 0..rows {
                s.insert_message(&vault_core::message::NewMessage::new(
                    "test",
                    format!("m{i}"),
                    format!("Author {i:02}"),
                    format!("2026-10-{:02}T12:00:00Z", i % 28 + 1),
                    format!("consent marker {i:02} about vault topics"),
                ))?;
            }
            Ok(())
        })
        .unwrap();
        (tmp, pool)
    }

    fn recorder_selection(base_url: String) -> Selection {
        Selection::Provider {
            id: "recorder".into(),
            name: "Recorder".into(),
            config: LlmConfig {
                base_url,
                api_key: "consent-test-key".into(),
                model: "consent-test-model".into(),
            },
        }
    }

    fn ask_today() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
    }

    #[test]
    fn ask_without_consent_makes_zero_network_calls() {
        // AC 1 of the M5 consent contract: with a fully usable network
        // provider selected but no explicit consent, not a single request
        // may leave the machine — the ask still answers, purely locally.
        let (_tmp, pool) = ask_pool(6);
        let stop = Arc::new(AtomicBool::new(false));
        let (base_url, server) = spawn_llm_recorder(
            vec![
                // Translate + summarize replies the pipeline must never use.
                "{\"match\": \"\\\"consent\\\"\", \"date_from\": null, \"date_to\": null}".into(),
                "the summarize reply no consent-less ask may fetch".into(),
            ],
            Arc::clone(&stop),
        );

        let outcome = run_ask(
            &ActiveVault::Pool(Arc::clone(&pool)),
            &HashEmbedder::new(),
            recorder_selection(base_url),
            true,
            "consent marker vault?",
            None,
            50,
            8,
            ask_today(),
            false,
        )
        .unwrap();

        // The ask degraded to fully local mode with the visible note.
        assert_eq!(outcome.plan_source, PlanSource::Local);
        assert!(
            outcome.answer.is_none(),
            "no answer without a network round trip"
        );
        assert!(outcome
            .notes
            .iter()
            .any(|n| n.contains("AI consent not granted")));
        assert_eq!(outcome.results.len(), 6, "the local search still answers");

        // Stop the recorder and prove nothing ever arrived (AC 1).
        stop.store(true, Ordering::SeqCst);
        let requests = server.join().expect("recorder thread");
        assert!(
            requests.is_empty(),
            "without consent no network call may go out, got {}",
            requests.len()
        );
        pool.shutdown();
    }

    #[test]
    fn ask_with_consent_uses_the_provider_end_to_end() {
        // With explicit consent the same selection goes out: exactly the
        // translate + summarize round trips reach the provider.
        let (_tmp, pool) = ask_pool(6);
        let stop = Arc::new(AtomicBool::new(false));
        let (base_url, server) = spawn_llm_recorder(
            vec![
                "{\"match\": \"\\\"consent\\\"\", \"date_from\": null, \"date_to\": null}".into(),
                "served consent summary [1].".into(),
            ],
            Arc::clone(&stop),
        );

        let outcome = run_ask(
            &ActiveVault::Pool(Arc::clone(&pool)),
            &HashEmbedder::new(),
            recorder_selection(base_url),
            true,
            "consent marker vault?",
            None,
            50,
            8,
            ask_today(),
            true,
        )
        .unwrap();

        assert_eq!(outcome.plan_source, PlanSource::Llm);
        assert_eq!(
            outcome.answer.as_deref(),
            Some("served consent summary [1].")
        );
        assert_eq!(outcome.results.len(), 6);

        stop.store(true, Ordering::SeqCst);
        let requests = server.join().expect("recorder thread");
        assert_eq!(requests.len(), 2, "translate + summarize round trips");
        // The results are source-linked evidence: every message carries its
        // archive row id, so the UI can open each source in thread context.
        assert!(outcome.results.iter().all(|m| m.id > 0));
        pool.shutdown();
    }

    #[test]
    fn run_ask_collects_links_when_the_plan_extracts() {
        let (_tmp, pool) = ask_pool(2);
        // One message must carry a URL for the extractor to find; the plan's
        // has_link filter keeps only that row.
        pool.submit_write(move |s| {
            s.insert_message(&vault_core::message::NewMessage::new(
                "test",
                String::from("links"),
                "Author 00",
                "2026-10-01T12:00:00Z",
                "consent marker 42 about vault topics and https://example.com/a".to_string(),
            ))?;
            Ok(())
        })
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (base_url, server) = spawn_llm_recorder(
            vec![
                "{\"match\": \"consent\", \"has_link\": true, \"extract_links\": true}".into(),
                "collected one link [1].".into(),
            ],
            Arc::clone(&stop),
        );

        let outcome = run_ask(
            &ActiveVault::Pool(Arc::clone(&pool)),
            &HashEmbedder::new(),
            recorder_selection(base_url),
            true,
            "collect all consent links",
            None,
            50,
            8,
            ask_today(),
            true,
        )
        .unwrap();

        assert!(outcome.plan.extract_links);
        assert_eq!(outcome.collected.len(), 1, "{:?}", outcome.collected);
        assert_eq!(outcome.collected[0].url, "https://example.com/a");
        assert_eq!(outcome.collected[0].hits[0].author, "Author 00");

        stop.store(true, Ordering::SeqCst);
        let bodies = server.join().expect("recorder thread");
        assert_eq!(bodies.len(), 2, "translate + summarize round trips");
        // The summarize round trip carries the collected-links section.
        assert!(bodies[1].contains("Collected links"), "{}", bodies[1]);
        pool.shutdown();
    }

    #[test]
    fn ask_without_a_key_falls_back_to_local_exactly_like_before() {
        // AC 4: consent changes nothing about the no-key fallback — a
        // selection without a usable provider (no key stored) still
        // answers locally, whatever consent says.
        let (_tmp, pool) = ask_pool(4);
        let selection = Selection::Local {
            reason: "provider “z.ai GLM” has no API key stored — set one in Settings".into(),
        };
        let outcome = run_ask(
            &ActiveVault::Pool(Arc::clone(&pool)),
            &HashEmbedder::new(),
            selection,
            true,
            "consent marker vault",
            None,
            50,
            8,
            ask_today(),
            true, // consent granted — but there is nothing to send to
        )
        .unwrap();
        assert_eq!(outcome.plan_source, PlanSource::Local);
        assert_eq!(outcome.answer, None);
        assert_eq!(outcome.results.len(), 4);
        assert!(outcome.notes[0].contains("no API key"));
        pool.shutdown();
    }

    #[test]
    fn cli_flag_version() {
        for flag in ["--version", "-V"] {
            let out = cli_flag_response(&[flag.to_string()]).unwrap();
            assert_eq!(
                out,
                format!("mailchat-vault {}\n", env!("CARGO_PKG_VERSION")),
                "flag {flag}"
            );
        }
    }

    #[test]
    fn cli_flag_help() {
        for flag in ["--help", "-h"] {
            let out = cli_flag_response(&[flag.to_string()]).unwrap();
            assert!(out.contains("Usage: mailchat-vault"), "flag {flag}");
            assert!(out.contains("--version"), "flag {flag}");
            assert!(out.ends_with('\n'), "flag {flag}");
        }
    }

    #[test]
    fn cli_flag_none_for_gui_and_unknown_args() {
        assert!(cli_flag_response(&[]).is_none());
        assert!(cli_flag_response(&["some-export-file.mbox".to_string()]).is_none());
        // The first recognized flag wins among mixed arguments.
        let out = cli_flag_response(&["ignored.mbox".to_string(), "-V".to_string()]).unwrap();
        assert!(out.starts_with("mailchat-vault "));
    }

    // ------------------------------------------------------------------
    // Multi-account overview assembly (roadmap: the accounts panel)
    // ------------------------------------------------------------------

    fn imap_instance(
        id: &str,
        host: &str,
        port: u16,
        username: &str,
        folders: &[&str],
    ) -> SourceInstance {
        SourceInstance {
            id: id.into(),
            kind: "imap".into(),
            label: format!("imap:{host}:{port}:{username}"),
            connector_key: Some(format!("imap|{host}|{port}|implicit-tls|{username}")),
            config_json: serde_json::json!({
                "host": host,
                "port": port,
                "tls_mode": "implicit-tls",
                "username": username,
                "folders": folders,
            })
            .to_string(),
            state: "connected".into(),
            excluded: false,
            last_attempt_at: Some("2026-01-02 10:00:00".into()),
            last_success_at: Some("2026-01-02 10:00:05".into()),
            last_error_class: None,
            last_error: None,
            last_error_at: None,
            created_at: "2026-01-01 00:00:00".into(),
        }
    }

    fn import_instance(id: &str, label: &str) -> SourceInstance {
        SourceInstance {
            id: id.into(),
            kind: "legacy".into(),
            label: label.into(),
            connector_key: None,
            config_json: "{}".into(),
            state: "connected".into(),
            excluded: false,
            last_attempt_at: None,
            last_success_at: None,
            last_error_class: None,
            last_error: None,
            last_error_at: None,
            created_at: "2026-01-01 00:00:00".into(),
        }
    }

    #[test]
    fn accounts_assembly_maps_every_account_kind() {
        let instances = vec![
            imap_instance(
                "inst-imap",
                "imap.gmail.com",
                993,
                "ana@gmail.com",
                &["INBOX"],
            ),
            import_instance("inst-mbox", "mbox:archive.mbox"),
        ];
        let counts = vec![
            vault_core::storage::AccountCounts {
                source_id: "inst-imap".into(),
                messages: 12,
                failed_items: 1,
            },
            vault_core::storage::AccountCounts {
                source_id: "inst-mbox".into(),
                messages: 34,
                failed_items: 0,
            },
        ];
        let channels = vec![vault_core::storage::AccountChannels {
            source_id: "inst-mbox".into(),
            has_chat: false,
            has_email: true,
        }];

        let views = assemble_accounts(&instances, &counts, &channels, |stored| {
            (stored.username == "ana@gmail.com", false)
        });

        assert_eq!(views.len(), 2);
        let imap = &views[0];
        assert_eq!(imap.kind, "imap");
        assert_eq!(imap.source_id, "inst-imap");
        assert_eq!(imap.label, "imap:imap.gmail.com:993:ana@gmail.com");
        assert_eq!(imap.message_count, 12);
        assert_eq!(imap.failed_items, 1);
        assert!(!imap.excluded);
        let identity = imap.imap.as_ref().expect("imap identity");
        assert_eq!(identity.host, "imap.gmail.com");
        assert_eq!(identity.port, 993);
        assert_eq!(identity.username, "ana@gmail.com");
        assert_eq!(identity.folders, vec!["INBOX".to_string()]);
        assert!(identity.has_password);
        assert!(!identity.has_oauth);

        let import = &views[1];
        assert_eq!(import.kind, "import");
        assert_eq!(import.imap, None, "imports carry no IMAP identity");
        assert_eq!(import.message_count, 34);
    }

    #[test]
    fn accounts_assembly_derives_the_medium_from_stored_channels() {
        // The mail-vs-chat taxonomy reaches the panel from the messages'
        // own channel column: IMAP is mail by construction, a chat
        // export is chat, a mail-file import is mail, an import with
        // nothing archived yet is unknown, and a namespace that somehow
        // holds both channels is mixed — never a guess from the label.
        let imap_acct = imap_instance("inst-imap", "imap.gmail.com", 993, "ana@gmail.com", &[]);
        let views = assemble_accounts(&[imap_acct], &[], &[], |_: &StoredImapConfig| {
            (false, false)
        });
        assert_eq!(views[0].medium, "mail", "IMAP is mail by kind");

        let medium_of = |has_chat: bool, has_email: bool| {
            let channels = vec![vault_core::storage::AccountChannels {
                source_id: "inst-x".into(),
                has_chat,
                has_email,
            }];
            let views = assemble_accounts(
                &[import_instance("inst-x", "whatsapp")],
                &[],
                &channels,
                |_: &StoredImapConfig| (false, false),
            );
            views[0].medium.clone()
        };
        assert_eq!(medium_of(true, false), "chat", "chat export");
        assert_eq!(medium_of(false, true), "mail", "mbox/.eml import");
        assert_eq!(medium_of(true, true), "mixed", "both channels stored");
        assert_eq!(medium_of(false, false), "unknown", "nothing archived yet");
        // A missing channel row (the flags query missed the instance)
        // reads as unknown, never as a fabricated mail/chat claim.
        let views = assemble_accounts(
            &[import_instance("inst-y", "telegram")],
            &[],
            &[],
            |_: &StoredImapConfig| (false, false),
        );
        assert_eq!(views[0].medium, "unknown");
    }

    #[test]
    fn accounts_assembly_defaults_missing_counts_to_zero_and_marks_pause() {
        // An instance the counts query somehow did not cover still lists
        // (a connected-but-empty account), and the exclusion flag rides
        // through as the account's paused state.
        let mut paused = imap_instance("inst-paused", "imap.b.test", 993, "bob@b.test", &[]);
        paused.excluded = true;
        let views = assemble_accounts(&[paused], &[], &[], |_: &StoredImapConfig| (false, true));
        assert_eq!(views.len(), 1);
        assert!(views[0].excluded, "exclusion is the pause state");
        assert_eq!(views[0].message_count, 0);
        assert_eq!(views[0].failed_items, 0);
        let identity = views[0].imap.as_ref().unwrap();
        assert!(identity.has_oauth, "credential kinds come from the probe");
        assert!(!identity.has_password);
    }

    #[test]
    fn accounts_assembly_carries_the_recorded_failure_for_the_panel() {
        // The account-level error state (cold review §2.2/#5): what fell
        // and why survives restarts in the vault database and must reach
        // the panel row — the entered identity stays on the same row.
        let mut failed = imap_instance("inst-failed", "imap.c.test", 993, "cin@c.test", &["INBOX"]);
        failed.state = "disconnected".into();
        failed.last_error_class = Some("connection_refused".into());
        failed.last_error = Some(
            "nothing answered at that host and port — check the server address and port (993 for IMAP over TLS)"
                .into(),
        );
        failed.last_error_at = Some("2026-11-09 23:00:00".into());
        let views = assemble_accounts(&[failed], &[], &[], |_: &StoredImapConfig| (false, false));
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!(view.state, "disconnected");
        assert_eq!(
            view.last_error.as_deref(),
            Some("nothing answered at that host and port — check the server address and port (993 for IMAP over TLS)")
        );
        assert_eq!(view.last_error_at.as_deref(), Some("2026-11-09 23:00:00"));
        // A healthy account carries the empty error state, not a stale one.
        let healthy = imap_instance("inst-ok", "imap.d.test", 993, "dee@d.test", &[]);
        let views = assemble_accounts(&[healthy], &[], &[], |_: &StoredImapConfig| (true, false));
        assert_eq!(views[0].last_error, None);
        assert_eq!(views[0].last_error_at, None);
    }
}

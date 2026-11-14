// Typed IPC boundary (M3.1): every Tauri command and event the frontend
// uses crosses here exactly once, with its wire types next to it. UI code
// never calls `invoke` directly, so command names and payload shapes have a
// single source of truth.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  BackupManifest,
  RecoveryStatusView,
  RestoreOutcomeView,
} from "./backupview";
import type { MediaUsageView } from "./media";
import type {
  AccountView,
  ArchiveStats,
  ChatContext,
  ChatImportOutcome,
  DemoMode,
  EmbeddingStatus,
  ExportSummary,
  ImportPreview,
  JobEventsPage,
  SearchPage,
  SourceInstance,
  Stats,
  SyncScheduleView,
  WatchSnapshot,
} from "./types";
import type { AskOutcome, LlmStatus } from "./llmview";
import type { SaveSettingsPayload, SettingsView } from "./settings";
import type {
  ConversationContext,
  ImportScope,
  MessageDetail,
  SearchArchivePage,
  SearchArchiveRequest,
  SelectionExportSummary,
} from "./v2types";

// ---------------------------------------------------------------------------
// Wire types that only the IPC layer needs
// ---------------------------------------------------------------------------

// `FolderSyncSummary` (the per-folder sync outcome) is mirrored once,
// fully, in types.ts — this boundary re-exports it so `imap_sync`'s
// wire shape and the coordinator's share one definition.
export type { FolderSyncSummary } from "./types";
import type { FolderSyncSummary } from "./types";

export interface ImapSyncOutcome {
  folders: FolderSyncSummary[];
  /** null when a stored password was reused or the sync ran over OAuth2;
   * "os_keyring" | "memory" when the entered password was just saved
   * there. */
  saved_to: "os_keyring" | "memory" | null;
}

/** The rejection shape of a failed `sync_imap`: the classified,
 * actionable message (never a raw errno string), the stable failure
 * class token, and — when a password was entered — where it was already
 * saved before the connection was attempted (the honest-credential
 * rule: a failed sync can still have stored one). Older/degraded paths
 * may reject with a plain string; use `syncFailureText` to normalize. */
export interface SyncFailure {
  class: string;
  message: string;
  saved_to: "os_keyring" | "memory" | null;
}

export interface OAuthLoginOutcome {
  username: string;
  saved_to: "os_keyring" | "memory";
}

export interface OAuthStatus {
  configured: boolean;
  signed_in: boolean;
}

/** Which credentials an IMAP sync uses, mirroring the backend's auth modes. */
export type ImapAuthMode = "password" | "oauth";

/** The search/query surface shared by search and export. */
export interface SearchQueryInput {
  text: string;
  sources: string[];
  author: string;
  from_date: string;
  to_date: string;
  limit: number;
  offset: number;
}

// ---------------------------------------------------------------------------
// v2 search/detail/context (M3.2) — the canonical-store reading surface
// ---------------------------------------------------------------------------

export function searchArchiveV2(request: SearchArchiveRequest): Promise<SearchArchivePage> {
  return invoke<SearchArchivePage>("search_archive_v2", { request });
}

export function messageDetailV2(id: number): Promise<MessageDetail> {
  return invoke<MessageDetail>("message_detail_v2", { id });
}

export function conversationContextV2(args: {
  id: number;
  beforeCursor?: number | null;
  afterCursor?: number | null;
  limit?: number;
}): Promise<ConversationContext> {
  return invoke<ConversationContext>("conversation_context_v2", {
    id: args.id,
    beforeCursor: args.beforeCursor ?? null,
    afterCursor: args.afterCursor ?? null,
    limit: args.limit ?? null,
  });
}

export function listImportScopesV2(): Promise<ImportScope[]> {
  return invoke<ImportScope[]>("list_import_scopes_v2");
}

export function sourcesList(): Promise<SourceInstance[]> {
  return invoke<SourceInstance[]>("sources_list");
}

// ---------------------------------------------------------------------------
// Multi-account surface (roadmap: the accounts panel)
// ---------------------------------------------------------------------------

export function accountsList(): Promise<AccountView[]> {
  return invoke<AccountView[]>("accounts_list");
}

/** Forget one IMAP account's credentials (keyring + session memory).
 * Returns whether anything was stored; the archive stays untouched. */
export function accountForgetCredentials(sourceId: string): Promise<boolean> {
  return invoke<boolean>("account_forget_credentials", { sourceId });
}

/** Pause/resume one source (per-source exclusion: coordinator rounds skip
 * excluded sources while keeping archive and cursors). */
export function sourceSetExcluded(sourceId: string, excluded: boolean): Promise<void> {
  return invoke<void>("source_set_excluded", { sourceId, excluded });
}


export function exportMessagesV2(args: {
  ids: number[];
  format: string;
  path: string;
}): Promise<SelectionExportSummary> {
  return invoke<SelectionExportSummary>("export_messages_v2", args);
}

// ---------------------------------------------------------------------------
// Media objects (M4.3): read-only display protocol commands
// ---------------------------------------------------------------------------

export function mediaUsage(): Promise<MediaUsageView> {
  return invoke<MediaUsageView>("media_usage");
}

export function mediaExport(args: {
  name: string;
  path: string;
}): Promise<{ path: string; size: number }> {
  return invoke<{ path: string; size: number }>("media_export", args);
}

export function mediaOpenExternal(args: {
  name: string;
  displayName: string | null;
}): Promise<{ path: string; size: number }> {
  return invoke<{ path: string; size: number }>("media_open_external", {
    name: args.name,
    displayName: args.displayName,
  });
}

export interface ReconcileReportView {
  objects: number;
  referenced: number;
  missing: string[];
  orphans: string[];
  corrupt: string[];
  stale_staging: string[];
  foreign: string[];
}

export function mediaReconcile(): Promise<ReconcileReportView> {
  return invoke<ReconcileReportView>("media_reconcile");
}

// ---------------------------------------------------------------------------
// Backup & restore (M4.4)
// ---------------------------------------------------------------------------

export type {
  BackupManifest,
  BackupSummaryView,
  LastBackupView,
  RecoveryStatusView,
  RestoreOutcomeView,
  RestorePreviewView,
} from "./backupview";
import type { LastBackupView } from "./backupview";

/** Quick manifest read of a picked container (pre-restore summary). */
export function backupInspect(archive: string): Promise<BackupManifest> {
  return invoke<BackupManifest>("backup_inspect", { archive });
}

/** Commit a staged, validated restore. Stops the app's vault access;
 * the UI must ask for a restart on success. */
export function restoreCommit(previewId: string): Promise<RestoreOutcomeView> {
  return invoke<RestoreOutcomeView>("restore_commit", { previewId });
}

/** The recovery state: journal, staged preview, previous-vault slots. */
export function recoveryStatus(): Promise<RecoveryStatusView> {
  return invoke<RecoveryStatusView>("recovery_status");
}

/** The persisted "last verified backup" of the personal vault
 *  (read from the vault's own metadata; survives restarts). */
export function lastBackup(): Promise<LastBackupView | null> {
  return invoke<LastBackupView | null>("last_backup");
}

/** Explicitly forget one previous-vault slot. */
export function recoveryForget(slotId: string): Promise<void> {
  return invoke<void>("recovery_forget", { slotId });
}

/** Switch back to a previous vault (validated switch; restart after). */
export function returnToPreviousVault(slotId: string): Promise<RestoreOutcomeView> {
  return invoke<RestoreOutcomeView>("return_to_previous_vault", { slotId });
}

// ---------------------------------------------------------------------------
// Search, reading, stats
// ---------------------------------------------------------------------------

export function searchMessages(query: SearchQueryInput): Promise<SearchPage> {
  return invoke<SearchPage>("search_messages", { query });
}

export function chatContext(id: number, radius = 8): Promise<ChatContext | null> {
  return invoke<ChatContext | null>("chat_context", { id, radius });
}

export function stats(): Promise<Stats> {
  return invoke<Stats>("stats");
}

export function archiveStats(top = 10): Promise<ArchiveStats> {
  return invoke<ArchiveStats>("archive_stats", { top });
}

// ---------------------------------------------------------------------------
// Import / sync / watch
// ---------------------------------------------------------------------------

// M4.2+: the mbox import runs the mail media pass (MIME attachments are
// stored and linked), so its outcome mirrors the chat import's: the
// ingest summary, the run's import_id (the import-scope filter value),
// plus how the media pass went.
export function importMbox(path: string): Promise<ChatImportOutcome> {
  return invoke<ChatImportOutcome>("import_mbox", { path });
}

export function importChatExport(
  path: string,
  extractMedia: boolean,
): Promise<ChatImportOutcome> {
  return invoke<ChatImportOutcome>("import_chat_export", { path, extractMedia });
}

/** Bounded preview of a picked import input (U03): nothing is stored;
 * the confirm step dispatches to importMbox/importChatExport. */
export function previewImport(path: string): Promise<ImportPreview> {
  return invoke<ImportPreview>("preview_import", { path });
}

// ---------------------------------------------------------------------------
// Isolated sample archive (U02)
// ---------------------------------------------------------------------------

export function demoEnter(): Promise<DemoMode> {
  return invoke<DemoMode>("demo_enter");
}

export function demoLeave(): Promise<DemoMode> {
  return invoke<DemoMode>("demo_leave");
}

export function demoStatus(): Promise<DemoMode> {
  return invoke<DemoMode>("demo_status");
}

export function syncImap(args: {
  host: string;
  port: number | null;
  username: string;
  password: string;
  folders: string[] | null;
  auth: ImapAuthMode;
}): Promise<ImapSyncOutcome> {
  return invoke<ImapSyncOutcome>("sync_imap", args);
}

export function oauthStatus(args: { host: string; username: string }): Promise<OAuthStatus> {
  return invoke<OAuthStatus>("oauth_status", args);
}

export function oauthGmailLogin(args: { host: string; username: string }): Promise<OAuthLoginOutcome> {
  return invoke<OAuthLoginOutcome>("oauth_gmail_login", args);
}

export function watchStatus(): Promise<WatchSnapshot> {
  return invoke<WatchSnapshot>("watch_status");
}

export function watchAddFolder(path: string): Promise<WatchSnapshot> {
  return invoke<WatchSnapshot>("watch_add_folder", { path });
}

export function watchRemoveFolder(path: string): Promise<WatchSnapshot> {
  return invoke<WatchSnapshot>("watch_remove_folder", { path });
}

// ---------------------------------------------------------------------------
// Jobs, scheduler, embeddings
// ---------------------------------------------------------------------------

export function jobSubmit(kind: string, params: Record<string, unknown>): Promise<{ id: number }> {
  return invoke<{ id: number }>("job_submit", { kind, params });
}

export function jobEventsSince(seq: number): Promise<JobEventsPage> {
  return invoke<JobEventsPage>("job_events_since", { seq });
}

export function embedStatus(): Promise<EmbeddingStatus> {
  return invoke<EmbeddingStatus>("embed_status");
}

export function syncScheduleGet(): Promise<SyncScheduleView> {
  return invoke<SyncScheduleView>("sync_schedule_get");
}

export function syncScheduleSet(
  enabled: boolean | null,
  intervalMinutes: number | null,
): Promise<SyncScheduleView> {
  return invoke<SyncScheduleView>("sync_schedule_set", { enabled, intervalMinutes });
}

// ---------------------------------------------------------------------------
// Ask / LLM
// ---------------------------------------------------------------------------

export function llmStatus(): Promise<LlmStatus> {
  return invoke<LlmStatus>("llm_status");
}

export function llmAsk(args: {
  question: string;
  limit: number;
  provider: string | null;
  /** M5 AI-consent gate: `true` is the only way any network round trip
   * happens; without it the backend runs the ask fully locally. */
  consent: boolean;
}): Promise<AskOutcome> {
  return invoke<AskOutcome>("llm_ask", args);
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

export function exportSearch(args: {
  query: SearchQueryInput;
  format: string;
  path: string;
}): Promise<ExportSummary> {
  return invoke<ExportSummary>("export_search", args);
}

/** Export the complete executed v2 search, including instance-id scope. */
export function exportSearchV2(args: {
  request: SearchArchiveRequest;
  format: string;
  path: string;
}): Promise<ExportSummary> {
  return invoke<ExportSummary>("export_search_v2", args);
}

export function exportChat(args: {
  id: number;
  format: string;
  path: string;
}): Promise<ExportSummary | null> {
  return invoke<ExportSummary | null>("export_chat", args);
}

/** Write a UTF-8 text file to a path picked with the save dialog. */
export function saveText(args: { path: string; contents: string }): Promise<null> {
  return invoke<null>("save_text", args);
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

export function settingsGet(): Promise<SettingsView> {
  return invoke<SettingsView>("settings_get");
}

export function settingsSave(payload: SaveSettingsPayload): Promise<SettingsView> {
  return invoke<SettingsView>("settings_save", { payload });
}

export function llmKeyClear(id: string): Promise<SettingsView> {
  return invoke<SettingsView>("llm_key_clear", { id });
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/** Subscribe to a backend event; returns the unsubscribe handle. */
export function onEvent<T>(name: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(name, (event) => handler(event.payload));
}

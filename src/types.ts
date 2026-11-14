/** Types mirroring the Rust/serde shapes sent over the Tauri IPC bridge. */

export interface Message {
  id: number;
  source: string;
  external_id: string;
  channel: string;
  chat_id: string;
  author: string;
  sent_at: string;
  body: string;
  raw_json: string;
}

export interface SourceCount {
  source: string;
  count: number;
}

export interface Stats {
  total: number;
  by_source: SourceCount[];
}

export interface DayCount {
  /** UTC day as "YYYY-MM-DD". */
  day: string;
  count: number;
}

export interface ChatCount {
  source: string;
  chat_id: string;
  count: number;
}

export interface AuthorCount {
  /** Empty for messages whose author is unknown. */
  author: string;
  count: number;
}

/** Dashboard aggregates: one `archive_stats` round trip. */
export interface ArchiveStats {
  total: number;
  /** RFC 3339 UTC; null when nothing in the archive is dated. */
  first_sent_at: string | null;
  last_sent_at: string | null;
  by_source: SourceCount[];
  by_day: DayCount[];
  top_chats: ChatCount[];
  top_authors: AuthorCount[];
}

export interface IngestSummary {
  source: string;
  seen: number;
  inserted: number;
  skipped: number;
  failed: number;
}

/** How a chat import's media extraction pass went. */
export interface MediaSummary {
  /** Messages that referenced at least one media file. */
  referenced: number;
  /** Files newly written into the media store. */
  stored: number;
  /** Files already in the store by content hash. */
  deduped: number;
  /** Referenced files missing from the export (text-only exports). */
  missing: number;
  /** Messages whose media links were written or backfilled. */
  linked: number;
  /** Export-tree files no message references, newly stored. */
  unlinked: number;
  /** Unlinked files whose content was already in the store. */
  unlinked_deduped: number;
  /** Unlinked files skipped because the media budget ran out. */
  unlinked_dropped: number;
  /** 1 when the unlinked pass's tree walk failed (counts understate). */
  unlinked_walk_failed: number;
}

/** Result of an import that can run a media pass: ingest + media.
 * Mbox imports return it too — the mail pass stores MIME attachments,
 * and the run's import_id stays the import-scope filter value. */
export interface ChatImportOutcome {
  summary: IngestSummary;
  /** The import run's id (an import-scope filter value). */
  import_id: string;
  /** Null when media extraction was disabled. */
  media: MediaSummary | null;
}

/** One sample message of a bounded import preview (U03). */
export interface ImportPreviewSample {
  author: string;
  subject: string;
  sent_at: string | null;
  snippet: string;
}

/** Bounded preview of a picked import input (U03): detected format,
 * samples, bounded counts, media presence, input size. */
export interface ImportPreview {
  kind: string;
  source: string;
  input_bytes: number;
  messages_seen: number;
  scan_complete: boolean;
  parse_failures: number;
  samples: ImportPreviewSample[];
  media_referenced: number;
  media_missing: number;
  warnings: string[];
  /** Added by the `preview_import` command: free bytes on the vault's
   * data dir (null when unavailable). */
  free_bytes: number | null;
}

/** Whether the isolated sample archive is open (U02). */
export interface DemoMode {
  active: boolean;
}

/** Mirrors `vault_core::watch::WatchEvent` (serde tagged, snake_case). */
export type WatchEvent =
  | { kind: "folder_added"; path: string }
  | { kind: "folder_removed"; path: string }
  | { kind: "imported"; path: string; summary: IngestSummary }
  | { kind: "skipped"; path: string; reason: string }
  | { kind: "failed"; path: string; error: string };

/** Mirrors `vault_core::watch::WatchSnapshot` (serde field names). */
export interface WatchSnapshot {
  running: boolean;
  folders: string[];
  pending: number;
  events: WatchEvent[];
}

/** Search request: free text plus conjunctive facet filters. */
export interface SearchQuery {
  text: string;
  sources: string[];
  author: string;
  from_date: string;
  to_date: string;
  limit: number;
  offset: number;
}

export interface SearchHit {
  message: Message;
  /** Body snippet with \x01/\x02 around matched terms; null in browse mode. */
  snippet: string | null;
}

export interface SearchPage {
  hits: SearchHit[];
  total: number;
  limit: number;
  offset: number;
}

export interface ChatContext {
  /** Oldest → newest. */
  before: Message[];
  message: Message;
  /** Oldest → newest. */
  after: Message[];
}

/** Local embedding coverage (mirrors `vault_core::vector::EmbeddingStatus`). */
export interface EmbeddingStatus {
  model: string;
  dim: number;
  embedded: number;
  messages: number;
  pending: number;
}

/** What one embedding-job run did (mirrors `vault_core::vector::EmbedJobSummary`). */
export interface EmbedJobSummary {
  model: string;
  dim: number;
  embedded: number;
  remaining: number;
  elapsed_ms: number;
}

/** Outcome of one export run. */
export interface ExportSummary {
  format: "csv" | "json" | "mbox";
  /** Rows written to the file. */
  written: number;
  /** Rows matching the selection but not representable in the format
   * (non-email rows in an MBOX export). Always 0 for CSV/JSON. */
  skipped: number;

}

// ---------------------------------------------------------------------------
// v2 background jobs (mirrors `vault_core::jobs`; serde tagged, snake_case)
// ---------------------------------------------------------------------------

/** Typed job failure (tagged by `kind`). */
export type JobError =
  | { kind: "storage"; message: string }
  | { kind: "io"; message: string }
  | { kind: "import"; message: string }
  | { kind: "network"; message: string }
  | { kind: "cancelled" }
  | { kind: "queue_full"; queue: string }
  | { kind: "invalid"; message: string }
  | { kind: "internal"; message: string };

/** Cooperative progress last reported by a running job. */
export interface JobProgress {
  done: number;
  total: number;
  message: string;
}

/** A job's state machine (tagged by `state`). */
export type JobState =
  | { state: "queued" }
  | { state: "running" }
  | { state: "succeeded"; result: unknown }
  | { state: "failed"; error: JobError }
  | { state: "cancelled" };

/** A job's current record — the authoritative state. */
export interface JobRecord {
  id: number;
  kind: string;
  state: JobState;
  submitted_at: string;
  started_at: string | null;
  finished_at: string | null;
  progress: JobProgress | null;
  /** True only on read-only historical rows restored from the finished-
   * jobs ledger after a restart: listed for context, never cancellable,
   * never counted among this session's jobs. Absent on live records. */
  restored?: boolean;
}

/** One job state change, stamped with a global monotonic `seq`. */
export type JobEvent =
  | { seq: number; at: string; job: number; type: "submitted"; kind: string }
  | { seq: number; at: string; job: number; type: "started" }
  | { seq: number; at: string; job: number; type: "progress"; done: number; total: number; message: string }
  | { seq: number; at: string; job: number; type: "succeeded"; result: unknown }
  | { seq: number; at: string; job: number; type: "failed"; error: JobError }
  | { seq: number; at: string; job: number; type: "cancelled" };

/** The `job_events_since` page: missed events plus a fresh snapshot. */
export interface JobEventsPage {
  events: JobEvent[];
  latest_seq: number;
  oldest_kept: number;
  trimmed: boolean;
  jobs: JobRecord[];
}

// ---------------------------------------------------------------------------
// M2.3: IMAP sources, exclusion, and the failed-item ledger
// (mirrors `vault_core::storage` / `vault_core::imap_coordinator`)
// ---------------------------------------------------------------------------

/** A durable source instance (never credentials). */
export interface SourceInstance {
  id: string;
  kind: string;
  label: string;
  /** `null` for legacy-named instances no connector has claimed. */
  connector_key: string | null;
  config_json: string;
  state: "connected" | "disconnected" | string;
  /** Per-source exclusion: coordinator rounds skip excluded sources. */
  excluded: boolean;
  last_attempt_at: string | null;
  last_success_at: string | null;
  created_at: string;
}

// ---------------------------------------------------------------------------
// Multi-account UI (roadmap: the accounts panel) — mirrors the Tauri
// `AccountView` / `AccountImap` wire types (never credentials)
// ---------------------------------------------------------------------------

/** The non-secret IMAP identity of one account. */
export interface AccountImap {
  host: string;
  port: number;
  username: string;
  /** Configured folders (empty = the connector's default set). */
  folders: string[];
  /** Whether the secret store holds an app password. */
  has_password: boolean;
  /** Whether the secret store holds a Google sign-in refresh token. */
  has_oauth: boolean;
}

/** One account row of the accounts overview. */
export interface AccountView {
  /** The durable source instance id (pause uses it; chips filter by it). */
  source_id: string;
  /** `"imap"` for live email accounts, `"import"` for file imports. */
  kind: "imap" | "import" | string;
  label: string;
  /** Medium of the account's archived messages — the mail-vs-chat
   *  taxonomy behind the source icons: `"mail"` (a live IMAP account
   *  or an email-file import), `"chat"` (a chat-export import),
   *  `"mixed"` when both channels are present, `"unknown"` when
   *  nothing is archived yet. The backend derives it from the
   *  messages' own `channel` column, never the label. */
  medium: "mail" | "chat" | "mixed" | "unknown" | string;
  /** IMAP connection identity; `null` for imports. */
  imap: AccountImap | null;
  /** Archived messages of the account. */
  message_count: number;
  /** Open rows in the failed-item ledger (the sync backlog). */
  failed_items: number;
  /** `"connected"` after a successful sync, `"disconnected"` otherwise. */
  state: "connected" | "disconnected" | string;
  /** Paused: excluded from coordinator rounds (archive and cursors kept). */
  excluded: boolean;
  last_attempt_at: string | null;
  last_success_at: string | null;
  /** The last failed sync's user-facing message — the account-level
   * error state that survives restarts; `null` after a success. */
  last_error: string | null;
  /** When the last failed sync was recorded. */
  last_error_at: string | null;
}


/** One row of the failed-item ledger: what fell, where, why, how often retried. */
export interface FailedItem {
  source_instance_id: string;
  folder: string;
  uid: number;
  uidvalidity: number;
  /** Fixed human-readable cause; never message content. */
  reason: string;
  attempts: number;
  first_failed_at: string;
  last_failed_at: string;
}

/** Media-pass counters of one folder's sync round (mirrors
 *  `vault_core::imap::FolderMediaSummary`): what the shared mail
 *  media handle stored, deduped, and failed to stage for this
 *  folder's fetches. */
export interface FolderMediaSummary {
  stored: number;
  deduped: number;
  missing: number;
}

/** Per-folder outcome of one IMAP sync (mirrors `vault_core::imap::FolderSyncSummary`). */
export interface FolderSyncSummary {
  folder: string;
  uidvalidity: number;
  reset: boolean;
  seen: number;
  inserted: number;
  skipped: number;
  failed: number;
  /** Previously-failed items this round finally stored (ledger cleared). */
  resolved_failures: number;
  conflicting_revisions: number;
  error: string | null;
  /** Media-pass counters for this folder's round; null when the sync
   *  ran without a media pass (no media data dir). */
  media: FolderMediaSummary | null;
}

/** What happened to one source in a coordinator round (tagged by `status`). */
export type SourceSyncStatus =
  | { status: "synced"; folders: FolderSyncSummary[] }
  | { status: "no_credentials" }
  | { status: "failed"; error: string };

/** One source's outcome in a coordinator round. */
export interface SourceSyncOutcome {
  instance_id: string;
  label: string;
  status: SourceSyncStatus;
}

/** A whole `imap_sync_all` round: per-source outcomes plus excluded labels. */
export interface RoundSummary {
  outcomes: SourceSyncOutcome[];
  excluded: string[];
}

/** The periodic-sync schedule + live scheduler status (M2.4), mirrors
 * the Tauri `SyncScheduleView`. */
export interface SyncScheduleView {
  enabled: boolean;
  /** Whole minutes between scheduled rounds (default 15). */
  interval_minutes: number;
  /** The scheduled round currently queued or running, else null. */
  active_job: number | null;
  /** Approximate seconds until the next round (rounded up), else null. */
  next_due_in_secs: number | null;
  /** Scheduled rounds submitted since the app started. */
  rounds_submitted: number;
}

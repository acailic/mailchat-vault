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
}

/** Result of a chat export import: ingest summary + media pass. */
export interface ChatImportOutcome {
  summary: IngestSummary;
  /** Null when media extraction was disabled. */
  media: MediaSummary | null;
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

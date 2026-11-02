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

/** v2 IPC wire shapes (M3.2) — mirrors `vault_core::search2` serde types. */

/** Sort order for a v2 search request. */
export type SearchSort = "relevance" | "newest";

/** One v2 search request (see the data/API contract's SearchArchiveRequest). */
export interface SearchArchiveRequest {
  text: string;
  /** Source instance ids (canonical UUIDs); empty = all sources. */
  source_ids: string[];
  conversation_id?: string | null;
  /** The bounded import run to scope results to ("View imported messages"). */
  import_job_id?: string | null;
  author: string;
  from_date?: string | null;
  to_date?: string | null;
  /** IANA timezone the day bounds are interpreted in; "" = OS default. */
  timezone: string;
  has_attachment?: boolean | null;
  /** Tri-state honesty filters; null = filter off (NEWWO-21). */
  time_assumed?: boolean | null;
  grouping_unverified?: boolean | null;
  files_missing?: boolean | null;
  sort: SearchSort;
  limit?: number;
  offset?: number;
  /** Opaque keyset cursor from a previous page's next_cursor. Wins over
   *  offset when both are set; deep pages cost the same as the first. */
  cursor?: string | null;
}

/** The time-confidence labels the canonical store records. */
export type TimeConfidence =
  | "explicit_offset"
  | "user_confirmed"
  | "legacy_assumed"
  | "unresolved"
  | "missing";

/** One flat search result row: metadata + bounded snippet, no full body. */
export interface MessageSummary {
  id: number;
  source_id: string;
  source_label: string;
  conversation_id: string | null;
  conversation_label: string | null;
  author: string;
  subject: string | null;
  /** Resolved UTC instant only; null when unknown. */
  sent_at: string | null;
  time_confidence: TimeConfidence | string;
  /** \x01/\x02 hit markers; empty in browse mode. */
  snippet: string;
  attachment_count: number;
  available_attachment_count: number;
  has_conflicting_revision: boolean;
  /** Near-duplicate copies folded into this row (NEWWO-101): how many
   *  copies the representative hides, excluding itself. Absent = 0 —
   *  the row renders exactly as before the fold existed. */
  cluster_count?: number;
  /** Ids of the folded near-duplicate copies (matching cluster_count);
   *  absent = none. Ids only — the detail payload carries the rest. */
  member_ids?: number[];
}

/** One v2 search page from a single read snapshot. */
export interface SearchArchivePage {
  hits: MessageSummary[];
  total: number;
  limit: number;
  offset: number;
  /** Bumped by every content-changing commit; differs ⇒ offer refresh. */
  archive_revision: number;
  /** Keyset cursor to the next page (the last row's sort position);
   *  null when this page reaches the end. Pass back as the request's
   *  cursor to continue at first-page cost regardless of depth. */
  next_cursor: string | null;
}

/** One attachment/media reference on a message. */
export interface AttachmentSummary {
  name: string;
  kind: string;
  size: number;
  /** Preserved bytes (M3.2: extracted media) vs reference-only. */
  available: boolean;
  /** Store-relative path of preserved bytes; absent for references. */
  path?: string | null;
}

/** One provenance observation of a message. */
export interface OccurrenceView {
  scope: string;
  provider_item_id: string;
  uidvalidity: number | null;
  import_id: string;
  observed_at: string;
}

/** A sibling archived revision sharing the message's identity. */
export interface RevisionLink {
  message_id: number;
  identity: string;
  revision_hash: string;
}

/** Full single-message reading payload (the v2 detail view). */
export interface MessageDetail {
  id: number;
  source_id: string;
  source_label: string;
  channel: string;
  chat_id: string;
  external_id: string;
  conversation_id: string | null;
  conversation_label: string | null;
  conversation_confidence: string | null;
  author: string;
  subject: string | null;
  body: string;
  sent_at: string | null;
  original_timestamp: string;
  timezone: string;
  time_confidence: string;
  attachments: AttachmentSummary[];
  occurrences: OccurrenceView[];
  revisions: RevisionLink[];
  raw_json: string;
}

/** One chronological neighbor of the anchor. */
export interface ContextMessage {
  id: number;
  author: string;
  sent_at: string | null;
  time_confidence: string;
  body: string;
}

/** Conversation context anchored on one message (the v2 reading pane). */
export interface ConversationContext {
  anchor: MessageDetail;
  /** Oldest → newest. */
  before: ContextMessage[];
  after: ContextMessage[];
  /** The grouping is an unresolved legacy grouping: anchor only. */
  unverified_conversation: boolean;
  has_more_before: boolean;
  has_more_after: boolean;
}

/** One import scope for the results filter chip. */
export interface ImportScope {
  import_id: string;
  messages: number;
  first_observed_at: string;
  last_observed_at: string;
}

/** Structured v2 command error. */
export interface AppError {
  code: string;
  message: string;
  field?: string | null;
  retryable: boolean;
  job_id?: string | null;
}

/** What one selection export produced. */
export interface SelectionExportSummary {
  format: "text" | "json";
  written: number;
  missing: number;
}

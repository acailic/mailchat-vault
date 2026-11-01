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

export interface IngestSummary {
  source: string;
  seen: number;
  inserted: number;
  skipped: number;
  failed: number;
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

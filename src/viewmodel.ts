/// Pure view-model logic for the search UI. No DOM, no Tauri calls — every
/// function here is unit- and snapshot-tested in viewmodel.test.ts.

import type { ChatContext, SearchHit } from "./types";

/** Marker characters the backend wraps around FTS5-matched terms. */
const SNIPPET_OPEN = "\x01";
const SNIPPET_CLOSE = "\x02";

/** A run of text, optionally a search-term match to be highlighted. */
export interface Segment {
  text: string;
  hit: boolean;
}

/** Split free-text input into search terms (whitespace-separated). */
export function splitTerms(text: string): string[] {
  return text.trim().length === 0 ? [] : text.trim().split(/\s+/);
}

/**
 * Split a backend snippet on its match markers into highlight segments.
 * Stray markers (control chars inside a real message body) degrade
 * gracefully: they just produce extra segments.
 */
export function parseSnippet(snippet: string): Segment[] {
  const segments: Segment[] = [];
  let hit = false;
  let start = 0;
  for (let i = 0; i < snippet.length; i++) {
    const ch = snippet[i];
    if (ch === SNIPPET_OPEN || ch === SNIPPET_CLOSE) {
      if (i > start) segments.push({ text: snippet.slice(start, i), hit });
      start = i + 1;
      hit = ch === SNIPPET_OPEN;
    }
  }
  if (start < snippet.length) segments.push({ text: snippet.slice(start), hit });
  return segments;
}

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * Highlight whole-word, case-insensitive occurrences of `terms` in arbitrary
 * text (used for full bodies and thread neighbors in the detail view, where
 * the backend supplies no snippet).
 */
export function highlightSegments(text: string, terms: string[]): Segment[] {
  const needles = terms.filter((t) => t.length > 0);
  if (needles.length === 0) return text.length ? [{ text, hit: false }] : [];
  const pattern = needles
    .map(escapeRegExp)
    .map((t) => `(?<![\\p{L}\\p{N}])${t}(?![\\p{L}\\p{N}])`)
    .join("|");
  const re = new RegExp(pattern, "giu");
  const segments: Segment[] = [];
  let start = 0;
  for (const match of text.matchAll(re)) {
    const at = match.index ?? 0;
    if (at > start) segments.push({ text: text.slice(start, at), hit: false });
    segments.push({ text: match[0], hit: true });
    start = at + match[0].length;
  }
  if (start < text.length) segments.push({ text: text.slice(start), hit: false });
  return segments;
}

/** One extracted media file linked from a message's raw_json. */
export interface MessageMedia {
  sha256: string;
  /** Store-relative path, `media/<sha256>.<ext>`. */
  path: string;
  /** Original file name from the export. */
  name: string;
  /** `image`, `video`, `audio`, `sticker`, `document`, … */
  kind: string;
  /** File size in bytes. */
  size: number;
}

/** An attachment placeholder recorded without an extracted file. */
export interface MessageAttachment {
  kind: string;
  name?: string;
}

function parseRawJson(rawJson: string): Record<string, unknown> {
  try {
    const value: unknown = JSON.parse(rawJson);
    return value && typeof value === "object" ? (value as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

/** The media links of a message (invalid raw_json → empty list). */
export function mediaOf(rawJson: string): MessageMedia[] {
  const raw = parseRawJson(rawJson);
  const media = raw.media;
  if (!Array.isArray(media)) return [];
  return media.filter(
    (m): m is MessageMedia =>
      !!m && typeof m === "object" && typeof (m as MessageMedia).path === "string",
  );
}

/** The attachment placeholder of a message, if any (WhatsApp exports). */
export function attachmentOf(rawJson: string): MessageAttachment | null {
  const raw = parseRawJson(rawJson);
  const attachment = raw.attachment;
  if (!attachment || typeof attachment !== "object") return null;
  const { kind, name } = attachment as Record<string, unknown>;
  if (typeof kind !== "string") return null;
  return typeof name === "string" ? { kind, name } : { kind };
}

/** One chat/thread group of consecutive search hits. */
export interface HitGroup {
  /** `${source}∕${chat_id}` — unique group key. */
  key: string;
  source: string;
  chatId: string;
  /** Human label, e.g. `mbox · thread-a` or `demo · no thread`. */
  label: string;
  hits: SearchHit[];
}

/** Group hits by (source, chat_id), preserving search order. */
export function groupHits(hits: SearchHit[]): HitGroup[] {
  const groups: HitGroup[] = [];
  const index = new Map<string, HitGroup>();
  for (const hit of hits) {
    const { source, chat_id } = hit.message;
    const key = `${source}/${chat_id}`;
    let group = index.get(key);
    if (!group) {
      group = {
        key,
        source,
        chatId: chat_id,
        label: chat_id ? `${source} · ${chat_id}` : `${source} · no thread`,
        hits: [],
      };
      index.set(key, group);
      groups.push(group);
    }
    group.hits.push(hit);
  }
  return groups;
}

/** Pagination state for the status line and prev/next buttons. */
export interface PageModel {
  total: number;
  from: number;
  to: number;
  page: number;
  pageCount: number;
  hasPrev: boolean;
  hasNext: boolean;
}

export function pageModel(total: number, offset: number, limit: number): PageModel {
  const page = Math.floor(offset / Math.max(1, limit)) + 1;
  const pageCount = Math.ceil(total / Math.max(1, limit));
  return {
    total,
    from: total === 0 ? 0 : offset + 1,
    to: Math.min(offset + limit, total),
    page,
    pageCount,
    hasPrev: offset > 0,
    hasNext: offset + limit < total,
  };
}

/** Clamp-and-step helper for the active result index (keyboard navigation). */
export function moveIndex(current: number, length: number, delta: number): number {
  if (length === 0) return -1;
  const next = current + delta;
  return Math.max(0, Math.min(length - 1, next));
}

/** One flattened thread row for the detail view. */
export interface ThreadEntry {
  message: import("./types").Message;
  current: boolean;
}

/** Flatten a ChatContext into a single oldest → newest thread list. */
export function threadEntries(ctx: ChatContext): ThreadEntry[] {
  return [
    ...ctx.before.map((message) => ({ message, current: false })),
    { message: ctx.message, current: true },
    ...ctx.after.map((message) => ({ message, current: false })),
  ];
}

/** The neighbor id to load when moving up/down inside the detail thread. */
export function neighborId(
  ctx: ChatContext,
  direction: -1 | 1,
): number | null {
  const entries = threadEntries(ctx);
  const at = entries.findIndex((e) => e.current);
  const target = entries[at + direction];
  return target ? target.message.id : null;
}

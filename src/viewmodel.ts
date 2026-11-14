/// Pure view-model logic for the search UI. No DOM, no Tauri calls — every
/// function here is unit- and snapshot-tested in viewmodel.test.ts.

import type { ArchiveStats, ChatContext, DayCount, SearchHit } from "./types";

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
 * How many matched runs one text may mark before the rest renders plain:
 * term-dense bodies stay readable instead of becoming a wall of amber.
 */
export const HIGHLIGHT_MAX_MARKS = 50;

/**
 * Highlight whole-word, case-insensitive occurrences of `terms` in arbitrary
 * text (used for full bodies and thread neighbors in the detail view, where
 * the backend supplies no snippet). At most `maxMarks` matched runs become
 * hit segments; any remaining text (later matches included) is one plain
 * tail, so the combined text content never changes.
 */
export function highlightSegments(
  text: string,
  terms: string[],
  maxMarks: number = HIGHLIGHT_MAX_MARKS,
): Segment[] {
  const needles = terms.filter((t) => t.length > 0);
  if (needles.length === 0) return text.length ? [{ text, hit: false }] : [];
  const pattern = needles
    .map(escapeRegExp)
    .map((t) => `(?<![\\p{L}\\p{N}])${t}(?![\\p{L}\\p{N}])`)
    .join("|");
  const re = new RegExp(pattern, "giu");
  const segments: Segment[] = [];
  let start = 0;
  let marks = 0;
  for (const match of text.matchAll(re)) {
    if (marks >= maxMarks) break;
    const at = match.index ?? 0;
    if (at > start) segments.push({ text: text.slice(start, at), hit: false });
    segments.push({ text: match[0], hit: true });
    start = at + match[0].length;
    marks++;
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

/** The stored rendered-HTML body of an email message, when present
 * (invalid raw_json → null; empty or non-string → null). */
export function htmlOf(rawJson: string): string | null {
  const html = parseRawJson(rawJson).html;
  return typeof html === "string" && html.length > 0 ? html : null;
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

// ---------------------------------------------------------------------------
// Statistics dashboard
// ---------------------------------------------------------------------------

/** One bar of the day histogram (already bucketed). */
export interface DayBar {
  /** Bucket label: a day ("2026-11-01"), month ("2026-11"), or year ("2026"). */
  label: string;
  count: number;
}

/** Sum `days` into consecutive buckets keyed by `keyOf`, order preserved. */
function bucketSum(days: DayCount[], keyOf: (d: DayCount) => string): DayBar[] {
  const bars: DayBar[] = [];
  const index = new Map<string, DayBar>();
  for (const d of days) {
    const key = keyOf(d);
    let bar = index.get(key);
    if (!bar) {
      bar = { label: key, count: 0 };
      index.set(key, bar);
      bars.push(bar);
    }
    bar.count += d.count;
  }
  return bars;
}

/**
 * Downsample a per-day series to a chartable number of bars: days are used
 * while they fit; beyond that consecutive days merge into month buckets,
 * then into year buckets (the coarsest fallback, so even decade-long
 * archives stay chartable). Buckets stay in ascending order and empty
 * buckets never appear (the backend series omits empty days).
 */
export function dayBars(days: DayCount[], maxBars: number): DayBar[] {
  if (days.length <= maxBars) {
    return days.map((d) => ({ label: d.day, count: d.count }));
  }
  const months = bucketSum(days, (d) => d.day.slice(0, 7));
  if (months.length <= maxBars) return months;
  return bucketSum(days, (d) => d.day.slice(0, 4));
}

/** Bar height as a percentage of the tallest bar (1..100; 0 only for 0). */
export function barHeightPct(count: number, max: number): number {
  if (max <= 0 || count <= 0) return 0;
  return Math.max(1, Math.round((count / max) * 100));
}

/** Thousands-grouped integer: 1234567 → "1,234,567". */
export function formatCount(n: number): string {
  return String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

/** One-line summary over the dashboard aggregates (shown above the chart). */
export function dashboardSummary(stats: ArchiveStats): string {
  if (stats.total === 0) return "empty archive";
  const first = stats.first_sent_at?.slice(0, 10) ?? null;
  const last = stats.last_sent_at?.slice(0, 10) ?? null;
  const range =
    first && last
      ? `${first} → ${last}`
      : (first ?? last ?? "no dates");
  const sources = stats.by_source.length;
  return (
    `${formatCount(stats.total)} messages · ${sources} source${sources === 1 ? "" : "s"} · ${range}`
  );
}

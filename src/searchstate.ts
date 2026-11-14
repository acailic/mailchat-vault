/// Pure state logic for the v2 search UI (M3.2). No DOM, no Tauri calls —
/// everything here is unit-tested in searchstate.test.ts. The DOM glue
/// lives in render2.ts / main.ts.
///
/// Owns: the filter model and its removable chips, the era-chip timeline
/// over the archive's day distribution, the flat results
/// selection model, results selection/scroll restoration, deep links
/// (`mailchat://message/<id>`), archive-revision refresh detection, and
/// the readable copy-text rendering (kept format-compatible with the
/// Rust `detail_as_text` used for file exports).

import type { DayCount } from "./types";
import type {
  ConversationContext,
  MessageDetail,
  MessageSummary,
  SearchArchiveRequest,
  SearchSort,
} from "./v2types";
import {
  conversationConfidenceLabel,
  timeConfidenceLabel,
} from "./format";

// ---------------------------------------------------------------------------
// Filter model + chips
// ---------------------------------------------------------------------------

/** The active v2 search filters (the chip bar's source of truth). */
export interface SearchFilters {
  text: string;
  /** Instance ids; labels resolved by the caller. */
  sourceIds: string[];
  author: string;
  fromDate: string;
  toDate: string;
  timezone: string;
  /** null = filter off. */
  hasAttachment: boolean | null;
  /** null/absent = filter off; true = assumed/missing dates, false = confirmed. */
  timeAssumed?: boolean | null;
  /** null/absent = filter off; true = unresolved grouping, false = verified. */
  groupingUnverified?: boolean | null;
  /** null/absent = filter off; true = references without preserved bytes. */
  filesMissing?: boolean | null;
  /** null = filter off. */
  importJobId: string | null;
  sort: SearchSort;
}

export const DEFAULT_FILTERS: SearchFilters = {
  text: "",
  sourceIds: [],
  author: "",
  fromDate: "",
  toDate: "",
  timezone: "",
  hasAttachment: null,
  timeAssumed: null,
  groupingUnverified: null,
  filesMissing: null,
  importJobId: null,
  sort: "relevance",
};

/** True when any filter would narrow a browse (text alone does not). */
export function hasActiveFilters(f: SearchFilters): boolean {
  return (
    f.sourceIds.length > 0 ||
    f.author.trim().length > 0 ||
    f.fromDate.length > 0 ||
    f.toDate.length > 0 ||
    f.hasAttachment !== null ||
    f.timeAssumed != null ||
    f.groupingUnverified != null ||
    f.filesMissing != null ||
    f.importJobId !== null
  );
}

/** One removable chip over the active filters. */
export interface FilterChip {
  /** Which filter the chip removes. */
  kind:
    | "source"
    | "author"
    | "from-date"
    | "to-date"
    | "attachment"
    | "time-assumed"
    | "grouping-unverified"
    | "files-missing"
    | "import-scope";
  /** Chip text ("author: Ana", "from 2026-01-01", "has attachment", …). */
  label: string;
  /** The value to remove (source id / import id); unused for toggles. */
  value: string;
}

/**
 * The removable chips for the applied filters (source, person, date,
 * attachment, import scope). Source labels come from the caller's id→label
 * map; import labels from the caller's id→label map.
 */
export function filterChips(
  f: SearchFilters,
  labels: { source: (id: string) => string; import: (id: string) => string },
): FilterChip[] {
  const chips: FilterChip[] = [];
  for (const id of f.sourceIds) {
    chips.push({ kind: "source", label: labels.source(id), value: id });
  }
  if (f.author.trim()) {
    chips.push({ kind: "author", label: `author: ${f.author.trim()}`, value: "" });
  }
  if (f.fromDate) {
    chips.push({ kind: "from-date", label: `from ${f.fromDate}`, value: "" });
  }
  if (f.toDate) {
    chips.push({ kind: "to-date", label: `to ${f.toDate}`, value: "" });
  }
  if (f.hasAttachment !== null) {
    chips.push({
      kind: "attachment",
      label: f.hasAttachment ? "has attachment" : "no attachment",
      value: "",
    });
  }
  if (f.timeAssumed != null) {
    chips.push({
      kind: "time-assumed",
      label: f.timeAssumed ? "dates assumed" : "dates confirmed",
      value: "",
    });
  }
  if (f.groupingUnverified != null) {
    chips.push({
      kind: "grouping-unverified",
      label: f.groupingUnverified ? "grouping unverified" : "grouping verified",
      value: "",
    });
  }
  if (f.filesMissing != null) {
    chips.push({
      kind: "files-missing",
      label: f.filesMissing ? "files missing" : "files preserved",
      value: "",
    });
  }
  if (f.importJobId !== null) {
    chips.push({
      kind: "import-scope",
      label: `import: ${labels.import(f.importJobId)}`,
      value: f.importJobId,
    });
  }
  return chips;
}

/** Remove one chip's filter from a filters object (pure). */
export function withoutChip(f: SearchFilters, chip: FilterChip): SearchFilters {
  const next: SearchFilters = { ...f };
  switch (chip.kind) {
    case "source":
      next.sourceIds = f.sourceIds.filter((id) => id !== chip.value);
      break;
    case "author":
      next.author = "";
      break;
    case "from-date":
      next.fromDate = "";
      break;
    case "to-date":
      next.toDate = "";
      break;
    case "attachment":
      next.hasAttachment = null;
      break;
    case "time-assumed":
      next.timeAssumed = null;
      break;
    case "grouping-unverified":
      next.groupingUnverified = null;
      break;
    case "files-missing":
      next.filesMissing = null;
      break;
    case "import-scope":
      next.importJobId = null;
      break;
  }
  return next;
}

/** Reject reversed day bounds before any invocation (UX contract). */
export function dateBoundsValid(fromDate: string, toDate: string): boolean {
  if (!fromDate || !toDate) return true;
  return fromDate <= toDate;
}

// ---------------------------------------------------------------------------
// Era chips (the archive's timeline as one-click date filters)
// ---------------------------------------------------------------------------

/** One era chip: a timeline bucket over the archive's dated messages. */
export interface EraBucket {
  /** Chip label: a year ("2019") or an ISO month ("2026-10"). */
  label: string;
  /** Inclusive day bounds — the exact values the from/to inputs take. */
  from: string;
  to: string;
}

/** Last calendar day of an ISO month "YYYY-MM" ("2024-02" → "2024-02-29"). */
function monthEnd(month: string): string {
  const year = Number(month.slice(0, 4));
  const mm = Number(month.slice(5, 7));
  const lastDay = new Date(Date.UTC(year, mm, 0)).getUTCDate();
  return `${month}-${String(lastDay).padStart(2, "0")}`;
}

/**
 * Derive the era chips from the archive's day distribution (the same
 * `by_day` series the Activity dashboard charts). Years when the archive
 * spans multiple calendar years, months when it all sits inside one; a
 * single month of history (or nothing dated) has no timeline to strip, so
 * it renders nothing. Over-cap months coarsen to the year (the same
 * months→years step `dayBars` takes); over-cap years keep the most recent
 * `maxChips` — nothing coarser exists, and the date inputs remain the
 * full-fidelity path. Buckets appear only for periods with messages (the
 * backend series omits empty days) and stay in ascending order.
 */
export function eraBuckets(days: DayCount[], maxChips = 8): EraBucket[] {
  if (days.length === 0) return [];
  const years = [...new Set(days.map((d) => d.day.slice(0, 4)))].sort();
  if (years.length >= 2) {
    const buckets = years.map((year) => ({
      label: year,
      from: `${year}-01-01`,
      to: `${year}-12-31`,
    }));
    return buckets.length > maxChips ? buckets.slice(-maxChips) : buckets;
  }
  const months = [...new Set(days.map((d) => d.day.slice(0, 7)))].sort();
  if (months.length < 2) return [];
  if (months.length > maxChips) {
    const year = years[0];
    return [{ label: year, from: `${year}-01-01`, to: `${year}-12-31` }];
  }
  return months.map((month) => ({ label: month, from: `${month}-01`, to: monthEnd(month) }));
}

/**
 * The bucket the filters currently hold, by exact bounds: a chip lights up
 * only when fromDate/toDate are precisely its bucket's bounds — a hand-
 * typed range inside 2019 must not read as "the 2019 chip is on". -1 when
 * no bucket matches.
 */
export function activeEraIndex(buckets: EraBucket[], f: SearchFilters): number {
  return buckets.findIndex((b) => b.from === f.fromDate && b.to === f.toDate);
}

/** Build the v2 IPC request from the filter model. */
export function buildRequest(f: SearchFilters, limit: number, offset: number): SearchArchiveRequest {
  return {
    text: f.text,
    source_ids: [...f.sourceIds],
    author: f.author.trim(),
    from_date: f.fromDate || null,
    to_date: f.toDate || null,
    timezone: f.timezone,
    has_attachment: f.hasAttachment,
    time_assumed: f.timeAssumed ?? null,
    grouping_unverified: f.groupingUnverified ?? null,
    files_missing: f.filesMissing ?? null,
    import_job_id: f.importJobId,
    // Empty text browses newest regardless of the requested sort; the
    // backend enforces it too — send the user's choice as-is.
    sort: f.sort,
    limit,
    offset,
  };
}

// ---------------------------------------------------------------------------
// Stale-response guard
// ---------------------------------------------------------------------------

/**
 * Monotonic response token: a response is current only if its token is
 * the newest issued. Rapid-fire queries cannot overwrite each other's
 * results (A07).
 */
export class Sequence {
  #current = 0;

  /** Issue the next token (call when starting a request). */
  next(): number {
    return ++this.#current;
  }

  /** Invalidate outstanding tokens (call on close/supersede). */
  bump(): void {
    this.#current++;
  }

  /** Is `token` still the newest? */
  isCurrent(token: number): boolean {
    return token === this.#current;
  }
}

// ---------------------------------------------------------------------------
// Selection + scroll restoration
// ---------------------------------------------------------------------------

/** What "Back to results" restores: the selected row and its scroll. */
export interface ResultsSnapshot {
  /** The message id the user opened (re-selected on return). */
  selectedId: number | null;
  /** The keyboard-active row index at open time. */
  activeIndex: number;
  scrollTop: number;
}

/** Capture the results list state before leaving it (opening a message). */
export function captureResults(
  list: { scrollTop: number },
  activeIndex: number,
  selectedId: number | null,
): ResultsSnapshot {
  return { selectedId, activeIndex, scrollTop: list.scrollTop };
}

/** The pure half of [`restoreResults`]: the indices to restore. */
export function restoreIndices(
  hits: MessageSummary[],
  snapshot: ResultsSnapshot | null,
): { activeIndex: number; selectedId: number | null } {
  if (!snapshot) return { activeIndex: -1, selectedId: null };
  let activeIndex = snapshot.activeIndex;
  if (snapshot.selectedId !== null) {
    const at = hits.findIndex((h) => h.id === snapshot.selectedId);
    if (at >= 0) activeIndex = at;
  }
  if (hits.length === 0) activeIndex = -1;
  else activeIndex = Math.min(Math.max(activeIndex, 0), hits.length - 1);
  return { activeIndex, selectedId: snapshot.selectedId };
}

/**
 * Restore after returning: re-select the row for `snapshot.selectedId`
 * (or clamp the old index) and put the scroll back where it was.
 */
export function restoreResults(
  list: { scrollTop: number },
  hits: MessageSummary[],
  snapshot: ResultsSnapshot | null,
): { activeIndex: number; selectedId: number | null } {
  const restored = restoreIndices(hits, snapshot);
  if (snapshot) list.scrollTop = snapshot.scrollTop;
  return restored;
}

// ---------------------------------------------------------------------------
// Archive-revision refresh detection
// ---------------------------------------------------------------------------

/**
 * Refresh policy (A07): a page whose revision differs from the revision
 * the visible results were loaded at means the archive changed under the
 * pagination — mark the results as having new data; never silently move
 * rows. The first page ever loaded establishes the baseline.
 */
export function revisionChanged(
  visibleRevision: number | null,
  pageRevision: number,
): boolean {
  return visibleRevision !== null && visibleRevision !== pageRevision;
}

// ---------------------------------------------------------------------------
// Deep links
// ---------------------------------------------------------------------------

/** The internal, vault-local message locator (not a public URL). */
export function messageLink(id: number): string {
  return `mailchat://message/${id}`;
}

const MESSAGE_LINK = /^mailchat:\/\/message\/(\d{1,16})$/;

/**
 * Parse a `mailchat://message/<id>` locator (typed or pasted into the
 * search box, or handed to the app by a future OS registration). Returns
 * the message id, or null when the text is not a message link.
 */
export function parseMessageLink(text: string): number | null {
  const match = MESSAGE_LINK.exec(text.trim());
  return match ? Number(match[1]) : null;
}

// ---------------------------------------------------------------------------
// Selection export model
// ---------------------------------------------------------------------------

/**
 * The explicit selection for export/copy: ids in results order (what the
 * user sees is what exports, top to bottom).
 */
export function selectionIds(hits: MessageSummary[], selected: Set<number>): number[] {
  return hits.filter((h) => selected.has(h.id)).map((h) => h.id);
}

// ---------------------------------------------------------------------------
// Copy text (format-compatible with Rust `detail_as_text`)
// ---------------------------------------------------------------------------

/** Readable text + provenance for one message (Copy message / exports). */
export function messageToText(detail: MessageDetail): string {
  const lines: string[] = [];
  lines.push(`=== Message ${detail.id} ===`);
  lines.push(`Source: ${detail.source_label} (${detail.source_id})`);
  if (detail.conversation_label) {
    const qualifier = conversationConfidenceLabel(detail.conversation_confidence);
    lines.push(
      qualifier
        ? `Conversation: ${detail.conversation_label} (${qualifier})`
        : `Conversation: ${detail.conversation_label}`,
    );
  }
  lines.push(`From: ${detail.author}`);
  if (detail.subject) lines.push(`Subject: ${detail.subject}`);
  lines.push(
    detail.sent_at
      ? `Date: ${detail.sent_at} (${timeConfidenceLabel(detail.time_confidence)})`
      : `Date: unknown (${timeConfidenceLabel(detail.time_confidence)})`,
  );
  if (detail.original_timestamp) {
    const tz = detail.timezone || "timezone unknown";
    lines.push(`Original timestamp: ${detail.original_timestamp} (${tz})`);
  }
  if (detail.attachments.length > 0) {
    lines.push("Attachments:");
    for (const a of detail.attachments) {
      const state = a.available ? "available" : "not preserved";
      lines.push(`  - ${a.name} [${a.kind}] (${state}, ${a.size} bytes)`);
    }
  }
  lines.push("");
  lines.push(detail.body);
  lines.push("");
  if (detail.revisions.length > 0) {
    lines.push("Conflicting revisions of this message:");
    for (const r of detail.revisions) {
      lines.push(`  - message ${r.message_id} (${r.identity})`);
    }
  }
  if (detail.occurrences.length > 0) {
    lines.push("Provenance:");
    for (const o of detail.occurrences) {
      const importPart = o.import_id ? `, import ${o.import_id}` : "";
      lines.push(
        `  - ${o.scope} (item ${o.provider_item_id}, observed ${o.observed_at})${importPart}`,
      );
    }
  }
  return lines.join("\n") + "\n";
}

// ---------------------------------------------------------------------------
// Reading-pane context model
// ---------------------------------------------------------------------------

/** One flattened context row for the reading pane's thread list. */
export interface ContextEntry {
  id: number;
  author: string;
  sentAt: string | null;
  body: string;
  current: boolean;
}

/** Flatten a ConversationContext into one oldest → newest list. */
export function contextEntries(ctx: ConversationContext): ContextEntry[] {
  return [
    ...ctx.before.map((m) => ({
      id: m.id,
      author: m.author,
      sentAt: m.sent_at,
      body: m.body,
      current: false,
    })),
    {
      id: ctx.anchor.id,
      author: ctx.anchor.author,
      sentAt: ctx.anchor.sent_at,
      body: ctx.anchor.body,
      current: true,
    },
    ...ctx.after.map((m) => ({
      id: m.id,
      author: m.author,
      sentAt: m.sent_at,
      body: m.body,
      current: false,
    })),
  ];
}

/** The cursors for "load earlier/later" (the outermost ids held). */
export function contextCursors(entries: ContextEntry[]): {
  beforeCursor: number | null;
  afterCursor: number | null;
} {
  if (entries.length === 0) return { beforeCursor: null, afterCursor: null };
  return {
    beforeCursor: entries[0].id,
    afterCursor: entries[entries.length - 1].id,
  };
}

/** Merge a newly loaded page into the held entries (pure). */
export function mergeContextPages(
  entries: ContextEntry[],
  page: ConversationContext,
): ContextEntry[] {
  const merged = [...entries];
  for (const m of [...page.before].reverse()) {
    if (!merged.some((e) => e.id === m.id)) {
      merged.unshift({
        id: m.id,
        author: m.author,
        sentAt: m.sent_at,
        body: m.body,
        current: false,
      });
    }
  }
  for (const m of page.after) {
    if (!merged.some((e) => e.id === m.id)) {
      merged.push({
        id: m.id,
        author: m.author,
        sentAt: m.sent_at,
        body: m.body,
        current: false,
      });
    }
  }
  return merged;
}

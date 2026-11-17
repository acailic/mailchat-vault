/// Pure parser for Gmail-style operator queries typed into the search
/// box (P1·05). No DOM, no Tauri calls — everything here is
/// unit-tested in queryparser.test.ts; the DOM glue lives in main.ts.
///
/// A query is whitespace-separated tokens; `field:value` tokens map
/// onto the existing v2 filter model (`SearchFilters` — the same
/// fields the UI filter controls drive), every other token is a plain
/// full-text term. Operators override the matching filter fields;
/// untouched filters survive. Every unsupported, unknown, or malformed
/// token produces an honest hint while the rest of the query still
/// runs — nothing is ever silently ignored.

import type { SearchFilters } from "./searchstate";

/** One source a `source:` value can resolve to (instance id + label). */
export interface SourceLabel {
  id: string;
  label: string;
}

/** A parsed query: the updated filters plus one hint line per token
 * that could not apply (empty when the query is clean). */
export interface AppliedQuery {
  filters: SearchFilters;
  hints: string[];
}

/** Hint copy, kept as data so tests assert it without the DOM. */
export const HINTS = {
  unknownOperator: (field: string): string => `unknown operator ${field}:`,
  unknownValue: (op: string, value: string): string => `unknown ${op} value "${value}"`,
  missingValue: (op: string): string => `${op} needs a value`,
  invalidDate: (op: string, value: string): string =>
    `invalid date "${value}" for ${op} — expected YYYY-MM-DD`,
  unsupported: (op: string): string => `${op} is not supported yet`,
  unreadNotTracked: "read state is not tracked (is:unread)",
  oneAuthor: (kept: string): string => `only one author filter exists — kept "${kept}"`,
  oneDate: (op: string, kept: string): string => `only one ${op} date applies — kept ${kept}`,
  reversedDates: (kept: string, dropped: string): string =>
    `reversed date range — kept ${kept}, dropped ${dropped}`,
  unknownSource: (value: string, available: string[]): string =>
    `unknown source "${value}" — available: ${available.join(", ")}`,
  ambiguousSource: (value: string, matches: string[]): string =>
    `ambiguous source "${value}" — matches: ${matches.join(", ")}`,
};

/** Operator tokens: a letter-led field name, a colon, a value (which
 * may be double-quoted and contain spaces or further colons). Tokens
 * whose field does not start with a letter (`12:30`, `"a:b"`) stay
 * plain terms. */
const OPERATOR = /^([A-Za-z][A-Za-z0-9_-]*):(.*)$/;

const ISO_DATE = /^(\d{4})-(\d{2})-(\d{2})$/;

/** Split box text into tokens; double quotes guard inner whitespace. */
function tokenize(text: string): string[] {
  const tokens: string[] = [];
  let current = "";
  let quoted = false;
  const push = () => {
    if (current) {
      tokens.push(current);
      current = "";
    }
  };
  for (const ch of text) {
    if (ch === '"') {
      quoted = !quoted;
      current += ch;
    } else if (!quoted && /\s/.test(ch)) {
      push();
    } else {
      current += ch;
    }
  }
  push();
  return tokens;
}

/** Strip one pair of surrounding double quotes and trim (operator
 * values only — plain terms keep their quotes as FTS5 phrase syntax). */
function unquote(value: string): string {
  const v = value.trim();
  if (v.length >= 2 && v.startsWith('"') && v.endsWith('"')) {
    return v.slice(1, -1).trim();
  }
  return v;
}

/** A real calendar day in the strict `YYYY-MM-DD` shape. */
function isIsoDate(value: string): boolean {
  const m = ISO_DATE.exec(value);
  if (!m) return false;
  const year = Number(m[1]);
  const month = Number(m[2]);
  const day = Number(m[3]);
  if (month < 1 || month > 12) return false;
  const lastDay = new Date(Date.UTC(year, month, 0)).getUTCDate();
  return day >= 1 && day <= lastDay;
}

/** The calendar day before an ISO day, via UTC arithmetic (the same
 * approach as `monthEnd` in searchstate.ts — never local time). */
function dayBefore(day: string): string {
  const year = Number(day.slice(0, 4));
  const month = Number(day.slice(5, 7));
  const date = Number(day.slice(8, 10));
  return new Date(Date.UTC(year, month - 1, date - 1)).toISOString().slice(0, 10);
}

/** How one `source:` value resolved against the label list. */
type SourceResolution =
  | { kind: "ok"; id: string }
  | { kind: "ambiguous"; matches: string[] }
  | { kind: "unknown"; available: string[] };

/** Resolve a `source:` value: exact case-insensitive label match
 * first, then a unique label prefix. Ambiguous or unknown values never
 * guess — they return the candidates for an honest hint. */
function resolveSource(value: string, sources: SourceLabel[]): SourceResolution {
  const needle = value.toLowerCase();
  const withId = (s: SourceLabel) => `${s.label} (${s.id})`;
  const exact = sources.filter((s) => s.label.toLowerCase() === needle);
  if (exact.length === 1) return { kind: "ok", id: exact[0].id };
  if (exact.length > 1) return { kind: "ambiguous", matches: exact.map(withId) };
  const prefix = sources.filter((s) => s.label.toLowerCase().startsWith(needle));
  if (prefix.length === 1) return { kind: "ok", id: prefix[0].id };
  if (prefix.length > 1) return { kind: "ambiguous", matches: prefix.map(withId) };
  return { kind: "unknown", available: sources.map((s) => s.label) };
}

/**
 * Apply the operator tokens in `text` to the current filters. Operator
 * fields override their UI counterparts (`from:`/`to:` the author,
 * `after:`/`before:` the date bounds, `source:` the source selection,
 * `has:attachment` the attachment tri-state); filters no operator
 * touches survive untouched, and plain terms become the FTS text.
 */
export function applyQuery(
  filters: SearchFilters,
  text: string,
  sources: SourceLabel[],
): AppliedQuery {
  const hints: string[] = [];
  const terms: string[] = [];
  let author: string | null = null;
  let fromDate: string | null = null; // after: (inclusive) — the value as typed
  let toDate: string | null = null; // before: (exclusive) — the day before
  let afterRaw: string | null = null; // the typed after: value (for hint copy)
  let beforeRaw: string | null = null; // the typed before: value
  const dateOrder: ("after" | "before")[] = []; // which bound was typed first
  const sourceIds: string[] = [];
  let hasAttachment: boolean | null = null;

  for (const token of tokenize(text)) {
    const m = OPERATOR.exec(token);
    if (!m) {
      terms.push(token);
      continue;
    }
    const field = m[1].toLowerCase();
    const value = unquote(m[2]);
    switch (field) {
      case "from":
      case "to": {
        if (!value) {
          hints.push(HINTS.missingValue(`${field}:`));
          break;
        }
        if (author === null) author = value;
        else if (author !== value) hints.push(HINTS.oneAuthor(author));
        break;
      }
      case "subject":
        hints.push(HINTS.unsupported("subject:"));
        break;
      case "after":
      case "before": {
        const op = `${field}:`;
        if (!value) {
          hints.push(HINTS.missingValue(op));
          break;
        }
        if (!isIsoDate(value)) {
          hints.push(HINTS.invalidDate(op, value));
          break;
        }
        if (field === "after") {
          if (afterRaw !== null) {
            if (afterRaw !== value) hints.push(HINTS.oneDate(op, afterRaw));
          } else {
            fromDate = value;
            afterRaw = value;
            dateOrder.push("after");
          }
        } else if (beforeRaw !== null) {
          if (beforeRaw !== value) hints.push(HINTS.oneDate(op, beforeRaw));
        } else {
          toDate = dayBefore(value);
          beforeRaw = value;
          dateOrder.push("before");
        }
        break;
      }
      case "source": {
        if (!value) {
          hints.push(HINTS.missingValue("source:"));
          break;
        }
        const resolved = resolveSource(value, sources);
        if (resolved.kind === "ok") {
          if (!sourceIds.includes(resolved.id)) sourceIds.push(resolved.id);
        } else if (resolved.kind === "ambiguous") {
          hints.push(HINTS.ambiguousSource(value, resolved.matches));
        } else {
          hints.push(HINTS.unknownSource(value, resolved.available));
        }
        break;
      }
      case "has": {
        const v = value.toLowerCase();
        if (!v) hints.push(HINTS.missingValue("has:"));
        else if (v === "attachment") hasAttachment = true;
        else if (v === "voice" || v === "original") hints.push(HINTS.unsupported(`has:${v}`));
        else hints.push(HINTS.unknownValue("has:", value));
        break;
      }
      case "is": {
        const v = value.toLowerCase();
        if (!v) hints.push(HINTS.missingValue("is:"));
        else if (v === "unread" || v === "read") hints.push(HINTS.unreadNotTracked);
        else hints.push(HINTS.unknownValue("is:", value));
        break;
      }
      default:
        // A `scheme://…` shape is a URL someone pasted, not an operator
        // attempt — it searches as text (the near-link deep-link text
        // `mailchat://message/12x` rides the same rule).
        if (m[2].startsWith("//")) {
          terms.push(token);
          break;
        }
        hints.push(HINTS.unknownOperator(field));
        break;
    }
  }

  // Query-internal reversed bounds: keep the first bound the user
  // typed, drop the conflicting one, hint — the rest still runs. (A
  // reversal against a surviving UI bound is left to the existing
  // reversed-bounds guard in runSearch.)
  if (fromDate !== null && toDate !== null && fromDate > toDate) {
    if (dateOrder[0] === "after") {
      toDate = null;
      hints.push(HINTS.reversedDates(`after:${afterRaw}`, `before:${beforeRaw}`));
    } else {
      fromDate = null;
      hints.push(HINTS.reversedDates(`before:${beforeRaw}`, `after:${afterRaw}`));
    }
  }

  const next: SearchFilters = { ...filters };
  next.text = terms.join(" ");
  if (author !== null) next.author = author;
  if (fromDate !== null) next.fromDate = fromDate;
  if (toDate !== null) next.toDate = toDate;
  if (sourceIds.length > 0) next.sourceIds = sourceIds;
  if (hasAttachment !== null) next.hasAttachment = hasAttachment;
  return { filters: next, hints };
}

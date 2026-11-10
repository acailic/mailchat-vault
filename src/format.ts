/// Display helpers for search results (pure functions, unit tested).

import type { FolderMediaSummary, FolderSyncSummary } from "./types";

/** Render an RFC 3339 timestamp as a compact local string. */
export function formatSentAt(iso: string): string {
  if (!iso) return "unknown time";
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
    `${pad(date.getHours())}:${pad(date.getMinutes())}`
  );
}

/** The user-facing label of one time-confidence token: a short word
 *  (or two) that says how much to trust the shown time — the token
 *  itself never reaches the reading pane, the copy text, or exports
 *  (the same classification discipline as the sync error classes: the
 *  stable token stays in the store/wire, the human phrase in every
 *  surface a user reads). An unknown future token surfaces verbatim —
 *  never blanked and never guessed. */
export function timeConfidenceLabel(confidence: string): string {
  switch (confidence) {
    case "explicit_offset":
      return "exact";
    case "user_confirmed":
      return "confirmed";
    case "legacy_assumed":
      return "assumed";
    case "unresolved":
      return "unresolved";
    case "missing":
      return "no time recorded";
    default:
      return confidence;
  }
}

/** The one-sentence explanation of one time-confidence token (the
 *  tooltip depth of the reading pane's date row): what the archive
 *  actually knows about the time. Empty when there is nothing useful
 *  to say about an unrecognized token. */
export function timeConfidenceDetail(confidence: string): string {
  switch (confidence) {
    case "explicit_offset":
      return "The time was read from the message itself, with its own timezone.";
    case "user_confirmed":
      return "You confirmed this time yourself.";
    case "legacy_assumed":
      return "The time was assumed during import and may be off — the source did not state it directly.";
    case "unresolved":
      return "The message's time could not be read; no instant was fabricated.";
    case "missing":
      return "The message carried no time at all.";
    default:
      return "";
  }
}

/** The user-facing label of one conversation-confidence token: how the
 *  archive knows these messages form one conversation. Empty for an
 *  absent token (no qualifier — an empty "()" fragment must never
 *  print). Unknown tokens surface verbatim, like time confidence. */
export function conversationConfidenceLabel(
  confidence: string | null | undefined,
): string {
  switch (confidence ?? "") {
    case "verified":
      return "verified";
    case "user_linked":
      return "user-linked";
    case "legacy_unresolved":
      return "unverified grouping";
    case "":
      return "";
    default:
      return confidence ?? "";
  }
}

/** Collapse a message body into a single-line snippet of at most `max` chars. */
export function snippet(body: string, max = 200): string {
  const collapsed = body.replace(/\s+/g, " ").trim();
  if (collapsed.length <= max) return collapsed;
  return collapsed.slice(0, max - 1).trimEnd() + "…";
}

/** Human-readable byte size: `12 B`, `3.4 KB`, `2.1 MB`, … */
export function humanSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "—";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  const rounded = value >= 100 ? Math.round(value) : Math.round(value * 10) / 10;
  return `${rounded} ${units[unit]}`;
}

/** One folder's sync outcome as one line, shared by the sync status
 *  and the accounts panel: the fixed insert/skip parts, then only the
 *  non-zero extras — parse failures, full rescans, folder errors, and
 *  the media-pass counters (S2's only-non-zero style: a clean folder
 *  with no attachments says nothing about media). */
export function folderSummaryLine(f: FolderSyncSummary): string {
  let line = `${f.folder}: ${f.inserted} new, ${f.skipped} already present`;
  if (f.failed > 0) line += `, ${f.failed} failed to parse`;
  const media = mediaSummaryPart(f.media);
  if (media) line += `, media: ${media}`;
  if (f.reset) line += " (full rescan)";
  if (f.error) line += ` — failed: ${f.error}`;
  return line;
}

/** The non-zero media-pass parts of one folder's sync ("2 stored,
 *  1 deduped, 1 missing"); empty when the pass stored nothing or did
 *  not run. */
export function mediaSummaryPart(
  media: FolderMediaSummary | null | undefined,
): string {
  if (!media) return "";
  const parts: string[] = [];
  const add = (n: number, label: string) => {
    if (n > 0) parts.push(`${n} ${label}`);
  };
  add(media.stored, "stored");
  add(media.deduped, "deduped");
  add(media.missing, "missing");
  return parts.join(", ");
}

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

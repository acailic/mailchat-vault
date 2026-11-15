// Pure display helpers for the folder-watching UI (unit tested, no DOM
// beyond the builder functions below; every dynamic string goes through
// textContent, so paths and errors can never inject markup).

import type { WatchEvent, WatchSnapshot } from "./types";

/** One-line status over the watcher snapshot. */
export function watchStatusText(s: WatchSnapshot): string {
  if (!s.running) {
    return "Folder watching is not running — restart the app.";
  }
  const n = s.folders.length;
  if (n === 0) {
    return "No folders watched yet — pick one to import exports automatically.";
  }
  const parts = [`Watching ${n} folder${n === 1 ? "" : "s"}`];
  if (s.pending > 0) parts.push(`${s.pending} file${s.pending === 1 ? "" : "s"} settling`);
  return `${parts.join(" · ")} — new/modified .mbox / .eml mail files, WhatsApp .txt, Telegram and Signal .json exports, and Messenger message_*.json thread parts import in the background.`;
}

/** One-line description of a watcher event. */
export function describeWatchEvent(e: WatchEvent): string {
  switch (e.kind) {
    case "folder_added":
      return `now watching ${e.path}`;
    case "folder_removed":
      return `stopped watching ${e.path}`;
    case "imported": {
      const { summary } = e;
      const parts = [`${summary.inserted} new`];
      if (summary.skipped > 0) parts.push(`${summary.skipped} already present`);
      if (summary.failed > 0) parts.push(`${summary.failed} failed to parse`);
      return `imported ${e.path}: ${parts.join(", ")}`;
    }
    case "skipped":
      return `skipped ${e.path} (${e.reason})`;
    case "failed":
      return `failed ${e.path} — ${e.error}`;
  }
}

export interface WatchFoldersOptions {
  onRemove: (path: string) => void;
}

/** Render the watched-folder list with a Remove button per folder
 * (replaces children). */
export function renderWatchFolders(
  el: HTMLElement,
  snapshot: Pick<WatchSnapshot, "folders">,
  opts: WatchFoldersOptions,
): void {
  const items = snapshot.folders.map((path) => {
    const li = document.createElement("li");
    li.className = "watch-folder";
    const label = document.createElement("span");
    label.className = "path";
    label.textContent = path;
    const remove = document.createElement("button");
    remove.className = "watch-remove";
    remove.type = "button";
    remove.textContent = "Remove";
    remove.title = `Stop watching ${path}`;
    remove.addEventListener("click", () => opts.onRemove(path));
    li.append(label, remove);
    return li;
  });
  el.replaceChildren(...items);
}

/** Render recent watcher events (newest first) as one line each. */
export function renderWatchEvents(el: HTMLElement, events: WatchEvent[]): void {
  const items = events.slice(0, 20).map((event) => {
    const li = document.createElement("li");
    li.className = `watch-event ${event.kind}`;
    li.textContent = describeWatchEvent(event);
    return li;
  });
  el.replaceChildren(...items);
}

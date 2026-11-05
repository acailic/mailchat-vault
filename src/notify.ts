// Desktop notifications for new mail (roadmap: "nova pošta nakon
// sync/watch"). When a sync round (manual, coordinator, or scheduled) or
// a folder-watch import stores new messages, the app shows one native OS
// notification — cross-platform through the Tauri notification plugin
// (macOS, Windows, Linux) — with the count and where the mail landed.
//
// Rules (the roadmap contract):
// - only cycles that actually stored something new notify (`inserted > 0`
//   summed over the round; failed/skipped folders never count);
// - silent while the app window is focused: a focused window means the
//   user is already reading in the app, so a notification would only
//   duplicate what they can see;
// - silent when the Settings option is off;
// - never throws: a notification is advisory, so every failure (missing
//   permission, unavailable OS notifier) degrades to "no notification"
//   without touching the UI flow it rode in on.
//
// Pure decision logic (digest extraction, titles/bodies, the focus and
// enabled gates) is separated from the plugin adapter so the module is
// unit-testable with a fake channel; the wiring lives in `main.ts`.

import type { JobEvent, WatchEvent } from "./types";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";

/** What one notification is about: how many new messages arrived and
 * where from. */
export interface NewMailDigest {
  /** Messages newly stored by the cycle. Always > 0. */
  count: number;
  /** IMAP folders that received new mail (sync rounds); empty for
   * watched imports. */
  folders: string[];
  /** The watched file that was imported (watch cycles); null for sync. */
  file: string | null;
}

/** The minimal folder summary shape both sync paths report (see
 * `FolderSyncSummary` in ipc.ts / vault-core). */
interface FolderInserts {
  folder: string;
  inserted: number;
}

/** Sum new messages over folder summaries. `null` when nothing new was
 * stored — a cycle that only skipped, failed, or resolved ledger items
 * never notifies. */
export function digestFromSyncFolders(folders: FolderInserts[]): NewMailDigest | null {
  let count = 0;
  const fresh: string[] = [];
  for (const f of folders) {
    if (typeof f?.inserted === "number" && f.inserted > 0) {
      count += f.inserted;
      if (typeof f.folder === "string") fresh.push(f.folder);
    }
  }
  return count > 0 ? { count, folders: fresh, file: null } : null;
}

/** Extract a digest from a succeeded job's result value. Understands the
 * two sync kinds — `imap_sync` (a `FolderSyncSummary[]`) and
 * `imap_sync_all` (a coordinator `RoundSummary`) — and returns `null`
 * for every other kind or unexpected shape. Defensive by design: the
 * result is untyped JSON crossing IPC. */
export function digestFromJobResult(kind: string | undefined, result: unknown): NewMailDigest | null {
  if (kind === "imap_sync") {
    return Array.isArray(result) ? digestFromSyncFolders(result as FolderInserts[]) : null;
  }
  if (kind === "imap_sync_all") {
    const outcomes = (result as { outcomes?: unknown } | null)?.outcomes;
    if (!Array.isArray(outcomes)) return null;
    const folders: FolderInserts[] = [];
    for (const outcome of outcomes) {
      const status = (outcome as { status?: unknown } | null)?.status;
      const synced = status as { status?: unknown; folders?: unknown } | null;
      if (synced?.status === "synced" && Array.isArray(synced.folders)) {
        folders.push(...(synced.folders as FolderInserts[]));
      }
    }
    return digestFromSyncFolders(folders);
  }
  return null;
}

/** Extract a digest from a watch event: only completed imports with new
 * messages notify. */
export function digestFromWatchEvent(event: WatchEvent | undefined): NewMailDigest | null {
  if (!event || event.kind !== "imported") return null;
  const inserted = event.summary?.inserted ?? 0;
  if (inserted <= 0) return null;
  // Just the file name — the full path is noise in a notification body.
  const file = event.path.split(/[\\/]/).filter(Boolean).pop() ?? event.path;
  return { count: inserted, folders: [], file };
}

/** The notification title: how much new mail arrived. */
export function notificationTitle(digest: NewMailDigest): string {
  return digest.count === 1 ? "1 new message" : `${digest.count} new messages`;
}

/** How many folders the body lists before folding the rest away. */
const MAX_LISTED_FOLDERS = 3;

/** The notification body: where the mail landed. Sync rounds name the
 * folders that received mail; watched imports name the file. */
export function notificationBody(digest: NewMailDigest): string {
  if (digest.file !== null) {
    return `imported from ${digest.file}`;
  }
  const listed = digest.folders.slice(0, MAX_LISTED_FOLDERS);
  const rest = digest.folders.length - listed.length;
  const names = listed.join(", ");
  return rest > 0 ? `${names} +${rest} more` : names;
}

/** The OS notification surface the notifier talks to (the plugin adapter
 * below in production; a fake in tests). Permission answers may be sync
 * or async — every consumer awaits them. */
export interface NotifyChannel {
  isPermissionGranted(): boolean | Promise<boolean>;
  requestPermission(): boolean | Promise<boolean>;
  send(title: string, body: string): void;
}

/** Adapter over the Tauri notification plugin (macOS/Windows/Linux). */
export function pluginNotifyChannel(): NotifyChannel {
  return {
    isPermissionGranted: () => isPermissionGranted(),
    requestPermission: async () => (await requestPermission()) === "granted",
    send: (title, body) => sendNotification({ title, body }),
  };
}

/** Everything the notifier needs, injected so tests stay hermetic. */
export interface NewMailNotifierDeps {
  channel: NotifyChannel;
  /** Whether the app window currently has focus (notifications stay
   * silent while it does). */
  isFocused(): boolean;
  /** Whether desktop notifications are enabled in Settings. */
  isEnabled(): boolean;
}

export interface NewMailNotifier {
  /** A live `job-event` landed (only succeeded sync rounds matter).
   * `kind` is the job's kind from the jobs tracker. */
  onJobEvent(event: JobEvent, kind: string | undefined): void;
  /** A live `watch-status` snapshot landed (only the newest event
   * matters, and each distinct event notifies at most once). */
  onWatchSnapshot(events: WatchEvent[]): void;
  /** A finished v1 sync command (the single-account Sync button)
   * reported its folders. */
  onSyncOutcome(folders: FolderInserts[]): void;
  /** The user toggled the Settings checkbox — a user gesture, so this is
   * the moment to ask the OS for notification permission. */
  onSettingToggled(on: boolean): void;
}

/** Build a notifier. All notification attempts are fire-and-forget and
 * swallow their own errors: the UI flow that produced the event must
 * never fail because a notification did. */
export function createNewMailNotifier(deps: NewMailNotifierDeps): NewMailNotifier {
  let lastJobSeq = 0;
  let lastWatchKey: string | null = null;

  const deliver = (digest: NewMailDigest | null): void => {
    if (digest === null) return;
    if (!deps.isEnabled()) return;
    if (deps.isFocused()) return; // the user is already reading in the app
    void (async () => {
      try {
        let granted = await deps.channel.isPermissionGranted();
        if (!granted) granted = await deps.channel.requestPermission();
        if (granted) deps.channel.send(notificationTitle(digest), notificationBody(digest));
      } catch {
        // No OS notifier, DBus missing, permission refused mid-flight —
        // stay silent; the data itself is already safely in the archive.
      }
    })();
  };

  return {
    onJobEvent(event, kind) {
      if (event.seq <= lastJobSeq) return; // stale duplicate / replay
      lastJobSeq = event.seq;
      if (event.type !== "succeeded") return;
      deliver(digestFromJobResult(kind, event.result));
    },
    onWatchSnapshot(events) {
      const newest = events[0];
      const digest = digestFromWatchEvent(newest);
      if (digest === null) return;
      // The same Imported event stays `events[0]` in every later
      // snapshot until something newer happens — notify it once.
      const key = JSON.stringify(newest);
      if (key === lastWatchKey) return;
      lastWatchKey = key;
      deliver(digest);
    },
    onSyncOutcome(folders) {
      deliver(digestFromSyncFolders(folders));
    },
    onSettingToggled(on) {
      if (!on) return;
      void (async () => {
        try {
          if (!(await deps.channel.isPermissionGranted()))
            await deps.channel.requestPermission();
        } catch {
          // Permission prompts are best-effort; deliver() retries anyway.
        }
      })();
    },
  };
}

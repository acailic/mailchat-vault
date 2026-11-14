// Pure display helpers for the periodic-sync (auto-sync) UI (M2.4) —
// unit tested, no DOM; every dynamic string goes through textContent in
// the callers, so nothing here can inject markup.

import type { SyncScheduleView } from "./types";

/** Format the countdown of a `next_due_in_secs` value approximately. */
export function formatCountdown(secs: number): string {
  if (secs <= 90) return `~${secs}s`;
  const minutes = Math.round(secs / 60);
  if (minutes < 90) return `~${minutes} min`;
  const hours = Math.round(minutes / 60);
  return `~${hours} h`;
}

/** One-line status over the scheduler view. */
export function autoSyncStatusText(view: SyncScheduleView): string {
  if (!view.enabled) {
    return "Auto-sync off: every IMAP source syncs when you press Sync (or run imap_sync_all).";
  }
  if (view.active_job !== null && view.active_job !== undefined) {
    return `Auto-sync round running (job ${view.active_job}): never overlapping, missed time is never queued.`;
  }
  const countdown =
    view.next_due_in_secs !== null && view.next_due_in_secs !== undefined
      ? `next round ${formatCountdown(view.next_due_in_secs)}`
      : "next round scheduled";
  return `Auto-sync on: ${countdown}.`;
}

/** The interval choices the UI offers (minutes); 15 is the default. */
export const INTERVAL_CHOICES = [5, 15, 30, 60] as const;

/** Whether a given interval is one of the offered choices. */
export function isIntervalChoice(minutes: number): boolean {
  return (INTERVAL_CHOICES as readonly number[]).includes(minutes);
}

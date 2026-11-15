// Pure view-model + display helpers for the v2 background-jobs panel
// (unit tested, no DOM beyond the builder functions below; every dynamic
// string goes through textContent, so errors can never inject markup).
//
// The client keeps a cursor (the largest event `seq` it has applied) and a
// map of job records. Live updates arrive as `job-event` window events;
// a gap in `seq` (or a webview reload) is healed by polling
// `job_events_since(cursor)` — the page carries the missed events plus a
// fresh records snapshot, and its `trimmed` flag tells the client the
// event ring no longer holds part of the gap, so it reconciles from the
// snapshot instead of the timeline.

import type { JobEvent, JobEventsPage, JobProgress, JobRecord } from "./types";
import { iconEl, type IconName } from "./icons";

/** Protocol types (see crates/vault-core/src/jobs.rs). */

export interface JobsTrackerState {
  /** Largest applied event seq (0 = nothing applied yet). */
  cursor: number;
  /** Newest failure observed through either live events or recovery. */
  latestFailureSeq: number;
  /** Records by id, insertion-ordered (newest submitted last). */
  records: Map<number, JobRecord>;
  /** Seq of the newest detail applied per job id. A polling page is a
   * snapshot taken at its `latest_seq`; a record whose applied seq is
   * beyond that horizon knows more than the page and must survive the
   * merge (a late response must not revert a finished job). */
  appliedSeq: Map<number, number>;
  /** True when an event revealed a gap the tracker cannot close alone —
   * the caller should run a `job_events_since` poll. */
  needsPoll: boolean;
}

export function newJobsTracker(): JobsTrackerState {
  return {
    cursor: 0,
    latestFailureSeq: 0,
    records: new Map(),
    appliedSeq: new Map(),
    needsPoll: true,
  };
}

/** Apply one live event (in arrival order). Sets `needsPoll` when the
 * event's seq leaves a gap — evidence of missed window events. */
export function applyJobEvent(tracker: JobsTrackerState, event: JobEvent): void {
  if (event.seq <= tracker.cursor) return; // stale duplicate
  if (event.seq > tracker.cursor + 1) tracker.needsPoll = true;
  tracker.cursor = event.seq;
  applyDetail(tracker, event);
}

/** Apply a polling page. With `trimmed`, timeline reconstruction is
 * impossible — the snapshot replaces the record map wholesale. The merge
 * is monotonic in the page's `latest_seq` (the snapshot's knowledge
 * horizon): a record the tracker updated past that horizon — a live
 * event that landed while the request was in flight — keeps its newer
 * state, so a late response can never revert a finished job. */
export function applyJobsPage(tracker: JobsTrackerState, page: JobEventsPage): void {
  // Jobs whose applied knowledge outruns the snapshot's horizon. Decided
  // before any mutation; page events cannot join this set (their seq is
  // at most `latest_seq`).
  const outrunsPage = new Set(
    [...tracker.appliedSeq]
      .filter(([, seq]) => seq > page.latest_seq)
      .map(([id]) => id),
  );
  // A gap can put the cursor beyond a missed failure; a trimmed ring can
  // omit its event entirely. The snapshot still tells us it is newly
  // failed — but only where the merge actually accepts the row.
  const wasFailed = new Map(
    [...tracker.records].map(([id, r]) => [id, r.state.state === "failed"]),
  );
  const recoversFailure = (record: JobRecord): boolean =>
    record.state.state === "failed" &&
    !outrunsPage.has(record.id) &&
    !wasFailed.get(record.id);

  let recoveredFailure = false;
  if (page.trimmed) {
    // Timeline reconstruction is impossible — the snapshot replaces the
    // record map wholesale (records absent from it still drop) — then the
    // records that outrun the snapshot's horizon are put back over it,
    // so in-flight terminal states survive a stale trimmed page.
    const newer = [...tracker.records.entries()]
      .filter(([id]) => outrunsPage.has(id))
      .map(([id, record]) => [id, record, tracker.appliedSeq.get(id)!] as const);
    tracker.records = new Map(page.jobs.map((r) => [r.id, r]));
    tracker.appliedSeq = new Map(page.jobs.map((r) => [r.id, page.latest_seq]));
    for (const [id, record, seq] of newer) {
      tracker.records.set(id, record);
      tracker.appliedSeq.set(id, seq);
    }
    recoveredFailure = page.jobs.some(recoversFailure);
  } else {
    for (const event of page.events) {
      if (event.seq <= tracker.cursor) continue;
      applyDetail(tracker, event);
    }
    for (const record of page.jobs) {
      if (outrunsPage.has(record.id)) continue; // keep the newer live record
      tracker.records.set(record.id, record);
      tracker.appliedSeq.set(record.id, page.latest_seq);
    }
    recoveredFailure = page.jobs.some(recoversFailure);
  }
  tracker.cursor = Math.max(tracker.cursor, page.latest_seq);
  if (recoveredFailure) {
    tracker.latestFailureSeq = Math.max(tracker.latestFailureSeq, page.latest_seq);
  }
  // A page older than the cursor (a response that raced newer events)
  // cannot have covered the gap — keep the flag so the caller re-polls
  // instead of stranding it.
  tracker.needsPoll = page.latest_seq < tracker.cursor;
}

function applyDetail(tracker: JobsTrackerState, event: JobEvent): void {
  const existing = tracker.records.get(event.job);
  const base: JobRecord =
    existing ??
    ({
      id: event.job,
      kind: "unknown",
      state: { state: "queued" },
      submitted_at: event.at,
      started_at: null,
      finished_at: null,
      progress: null,
    } as JobRecord);
  switch (event.type) {
    case "submitted":
      base.state = { state: "queued" };
      base.kind = event.kind;
      base.submitted_at = event.at;
      break;
    case "started":
      base.state = { state: "running" };
      base.started_at = event.at;
      break;
    case "progress":
      base.state = { state: "running" };
      base.progress = { done: event.done, total: event.total, message: event.message };
      break;
    case "succeeded":
      base.state = { state: "succeeded", result: event.result };
      base.finished_at = event.at;
      break;
    case "failed":
      base.state = { state: "failed", error: event.error };
      base.finished_at = event.at;
      tracker.latestFailureSeq = Math.max(tracker.latestFailureSeq, event.seq);
      break;
    case "cancelled":
      base.state = { state: "cancelled" };
      base.finished_at = event.at;
      break;
  }
  tracker.records.set(event.job, base);
  tracker.appliedSeq.set(event.job, event.seq);
}

/** Records ordered newest-first (most recently submitted first). */
export function orderedRecords(tracker: JobsTrackerState): JobRecord[] {
  return [...tracker.records.values()].sort((a, b) => b.id - a.id);
}

/** Whether any job of the given kind is queued or running. */
export function hasActiveJob(tracker: JobsTrackerState, kind: string): boolean {
  return [...tracker.records.values()].some(
    (r) => r.kind === kind && (r.state.state === "queued" || r.state.state === "running"),
  );
}

/** Number of queued/running jobs — the navigation Activity badge. */
export function activeJobCount(tracker: JobsTrackerState): number {
  return [...tracker.records.values()].filter(
    (r) => r.state.state === "queued" || r.state.state === "running",
  ).length;
}

/** One-line status over the tracker for the panel header. Restored
 * ledger rows count honestly but separately: "2 finished · 12 earlier"
 * never claims this session ran what a previous one did. */
export function jobsStatusText(tracker: JobsTrackerState): string {
  const records = orderedRecords(tracker);
  const active = records.filter(
    (r) => r.state.state === "queued" || r.state.state === "running",
  );
  if (records.length === 0) {
    return "No background jobs yet. Imports, IMAP syncs and building the local search index run here.";
  }
  if (active.length === 0) {
    const finished = records.filter((r) => !r.restored).length;
    const earlier = records.length - finished;
    const parts: string[] = [];
    if (finished > 0) parts.push(`${finished} finished`);
    if (earlier > 0) parts.push(`${earlier} earlier`);
    return `No active jobs (${parts.join(" · ")}).`;
  }
  const running = active.filter((r) => r.state.state === "running");
  const queued = active.length - running.length;
  const parts: string[] = [];
  if (running.length > 0) {
    parts.push(`${running.length} running`);
  }
  if (queued > 0) {
    parts.push(`${queued} queued`);
  }
  return `${parts.join(" · ")} — searches stay live while jobs work.`;
}

/** Human label for a job kind. */
export function jobKindLabel(kind: string): string {
  switch (kind) {
    case "seed_demo":
      return "demo seed";
    case "import_mbox":
      return ".mbox import";
    case "import_chat_export":
      return "chat import";
    case "embed_messages":
      return "local search index";
    case "imap_sync":
      return "IMAP sync";
    case "imap_sync_all":
      return "IMAP sync (all sources)";
    default:
      return kind;
  }
}

/** The `done/total (pct%) · hint` line for a running job, shared by the
 *  Activity row and the import flow's inline card. Null when the total is
 *  unknown — the caller falls back to its own indeterminate wording.
 *  Progress hints arrive from the backend as stable worker tokens; the
 *  one plain-language rewrite ("embedding" → "indexing") keeps the
 *  Activity row in user terms without touching the wire. */
export function jobProgressText(progress: JobProgress): string | null {
  if (progress.total <= 0) return null;
  const pct = jobProgressPercent(progress)!;
  const hint = progress.message
    ? ` · ${progress.message === "embedding" ? "indexing" : progress.message}`
    : "";
  return `${progress.done}/${progress.total} (${pct}%)${hint}`;
}

/** Whole-number percent (0–100, clamped) for a determinate progress bar;
 * null when the total is unknown — render an indeterminate bar instead. */
export function jobProgressPercent(progress: JobProgress): number | null {
  if (progress.total <= 0) return null;
  return Math.min(100, Math.round((progress.done / progress.total) * 100));
}

/** One-line description of a job's current state (with progress). */
export function describeJobState(record: JobRecord): string {
  const label = jobKindLabel(record.kind);
  switch (record.state.state) {
    case "queued":
      return `${label} — queued`;
    case "running": {
      const progress = record.progress;
      const counts = progress ? jobProgressText(progress) : null;
      return counts ? `${label} — ${counts}` : `${label} — running`;
    }
    case "succeeded":
      return `${label} — done${describeResult(record.state.result)}`;
    case "failed":
      return `${label} — failed: ${describeJobError(record.state.error)}`;
    case "cancelled":
      return `${label} — cancelled`;
  }
}

function describeResult(result: unknown): string {
  if (typeof result !== "object" || result === null) return "";
  const r = result as Record<string, unknown>;
  // Imports that run a media pass return { summary, media } — the ingest
  // counts live one level down; flat results (embed jobs) read directly.
  const counts =
    r.summary && typeof r.summary === "object"
      ? (r.summary as Record<string, unknown>)
      : r;
  const bits: string[] = [];
  if (typeof counts.inserted === "number") {
    bits.push(`${counts.inserted} new`);
    if (typeof counts.skipped === "number" && counts.skipped > 0)
      bits.push(`${counts.skipped} present`);
  }
  if (typeof r.embedded === "number" && r.embedded > 0) bits.push(`${r.embedded} indexed`);
  return bits.length > 0 ? ` (${bits.join(", ")})` : "";
}

/** Render a typed job error without leaking internal detail. */
export function describeJobError(error: { kind: string; message?: string }): string {
  switch (error.kind) {
    case "cancelled":
      return "cancelled";
    case "queue_full":
      return "the job queue is full — try again in a moment";
    case "storage":
    case "io":
    case "import":
    case "network":
    case "invalid":
    case "internal":
      return error.message ?? error.kind;
    default:
      return error.message ?? error.kind;
  }
}

/** Decorative state glyph for a job row; null when plain text reads best. */
function jobStateIcon(state: string): IconName | null {
  if (state === "running") return "spinner";
  if (state === "succeeded") return "check";
  if (state === "failed") return "error";
  return null;
}

/** Render the jobs list (newest first, capped). */
export function renderJobsList(el: HTMLElement, tracker: JobsTrackerState): void {
  const items = orderedRecords(tracker)
    .slice(0, 20)
    .map((record) => {
      const li = document.createElement("li");
      li.className = `job ${record.state.state}`;
      const glyph = jobStateIcon(record.state.state);
      if (glyph) li.append(iconEl(glyph, glyph === "spinner" ? "icon-spin" : ""));
      li.append(document.createTextNode(describeJobState(record)));
      return li;
    });
  el.replaceChildren(...items);
}

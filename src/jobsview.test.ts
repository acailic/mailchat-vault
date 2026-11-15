// @vitest-environment happy-dom
// Unit tests for the v2 background-jobs view helpers: the cursor-tracking
// client (live event application, gap detection → polling, trimmed-page
// reconciliation), status wording, and the DOM list builder.
import { describe, expect, it } from "vitest";
import type { JobError, JobEvent, JobEventsPage, JobRecord } from "./types";
import {
  activeJobCount,
  applyJobEvent,
  applyJobsPage,
  describeJobState,
  hasActiveJob,
  jobsStatusText,
  newJobsTracker,
  orderedRecords,
  renderJobsList,
} from "./jobsview";

const at = "2026-11-01T10:00:00Z";

const submitted = (seq: number, job: number): JobEvent => ({
  seq,
  at,
  job,
  type: "submitted",
  kind: "import_mbox",
});
const started = (seq: number, job: number): JobEvent => ({ seq, at, job, type: "started" });
const progress = (seq: number, job: number, done: number, total: number): JobEvent => ({
  seq,
  at,
  job,
  type: "progress",
  done,
  total,
  message: "embedding",
});
const succeeded = (seq: number, job: number, result: unknown): JobEvent => ({
  seq,
  at,
  job,
  type: "succeeded",
  result,
});
const failed = (seq: number, job: number, error: JobError): JobEvent => ({
  seq,
  at,
  job,
  type: "failed",
  error,
});
const cancelled = (seq: number, job: number): JobEvent => ({
  seq,
  at,
  job,
  type: "cancelled",
});
const record = (over: Partial<JobRecord>): JobRecord => ({
  id: 1,
  kind: "import_mbox",
  state: { state: "queued" },
  submitted_at: at,
  started_at: null,
  finished_at: null,
  progress: null,
  ...over,
});

const page = (over: Partial<JobEventsPage>): JobEventsPage => ({
  events: [],
  latest_seq: 0,
  oldest_kept: 1,
  trimmed: false,
  jobs: [],
  ...over,
});

describe("jobs tracker (v2 protocol client)", () => {
  it("recovers a missed failure from the snapshot without flagging it again on later polls", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, submitted(1, 1));
    applyJobEvent(tracker, submitted(4, 2)); // missed job 1's failure
    const failed = record({
      id: 1, state: { state: "failed", error: { kind: "network", message: "down" } },
    });
    applyJobsPage(tracker, page({ latest_seq: 4, jobs: [failed] }));
    expect(tracker.latestFailureSeq).toBe(4);
    applyJobsPage(tracker, page({ latest_seq: 5, jobs: [failed] }));
    expect(tracker.latestFailureSeq).toBe(4);
  });

  it("recovers failures when the event ring has been trimmed", () => {
    const tracker = newJobsTracker();
    applyJobsPage(tracker, page({
      trimmed: true, oldest_kept: 41, latest_seq: 60,
      jobs: [record({ state: { state: "failed", error: { kind: "io", message: "full" } } })],
    }));
    expect(tracker.latestFailureSeq).toBe(60);
  });

  it("applies a contiguous event stream and tracks the cursor", () => {
    const tracker = newJobsTracker();
    // A fresh tracker wants one boot poll; an empty page satisfies it.
    expect(tracker.needsPoll).toBe(true);
    applyJobsPage(tracker, page({ latest_seq: 0 }));
    expect(tracker.needsPoll).toBe(false);

    applyJobEvent(tracker, submitted(1, 7));
    applyJobEvent(tracker, started(2, 7));
    applyJobEvent(tracker, progress(3, 7, 5, 10));
    expect(tracker.cursor).toBe(3);
    expect(tracker.needsPoll).toBe(false);
    const rec = tracker.records.get(7)!;
    expect(rec.state.state).toBe("running");
    expect(rec.progress).toEqual({ done: 5, total: 10, message: "embedding" });

    applyJobEvent(tracker, succeeded(4, 7, { inserted: 3, skipped: 1 }));
    expect(tracker.records.get(7)!.state).toEqual({
      state: "succeeded",
      result: { inserted: 3, skipped: 1 },
    });
    expect(tracker.records.get(7)!.finished_at).toBe(at);
  });

  it("ignores stale duplicates and detects seq gaps as needing a poll", () => {
    const tracker = newJobsTracker();
    applyJobsPage(tracker, page({ latest_seq: 0 }));
    applyJobEvent(tracker, submitted(1, 1));
    applyJobEvent(tracker, submitted(1, 1)); // stale duplicate: no-op
    expect(tracker.cursor).toBe(1);
    expect(tracker.needsPoll).toBe(false);

    // A jump from 1 to 3 means seq 2 was missed (a dropped window event).
    applyJobEvent(tracker, started(3, 1));
    expect(tracker.cursor).toBe(3);
    expect(tracker.needsPoll).toBe(true);
  });

  it("heals a gap through a polling page", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, submitted(1, 1));
    applyJobEvent(tracker, succeeded(4, 1, null)); // gap: 2..3 missed
    expect(tracker.needsPoll).toBe(true);

    applyJobsPage(
      tracker,
      page({
        events: [started(2, 1), progress(3, 1, 1, 2)],
        latest_seq: 4,
        jobs: [record({ id: 1, state: { state: "succeeded", result: null } })],
      }),
    );
    expect(tracker.needsPoll).toBe(false);
    expect(tracker.cursor).toBe(4);
    // The page's authoritative snapshot wins over the reconstructed one.
    expect(tracker.records.get(1)!.state.state).toBe("succeeded");
  });

  it("reconciles wholesale from the snapshot when the ring was trimmed", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, submitted(1, 1));
    // The client missed far more than the ring keeps.
    applyJobsPage(
      tracker,
      page({
        trimmed: true,
        oldest_kept: 41,
        latest_seq: 60,
        events: [],
        jobs: [
          record({ id: 1, kind: "imap_sync", state: { state: "failed", error: { kind: "network", message: "timeout" } } }),
          record({ id: 2, kind: "embed_messages", state: { state: "succeeded", result: { embedded: 4 } } }),
        ],
      }),
    );
    expect(tracker.needsPoll).toBe(false);
    expect(tracker.cursor).toBe(60);
    expect(orderedRecords(tracker).map((r) => r.id)).toEqual([2, 1]);
    expect(tracker.records.get(1)!.state.state).toBe("failed");
  });

  it("boot recovery: an empty tracker polls from zero", () => {
    const tracker = newJobsTracker();
    expect(tracker.cursor).toBe(0);
    expect(tracker.needsPoll).toBe(true);
    applyJobsPage(
      tracker,
      page({
        events: [submitted(1, 1), started(2, 1), succeeded(3, 1, { ok: true })],
        latest_seq: 3,
        jobs: [record({ id: 1, state: { state: "succeeded", result: { ok: true } } })],
      }),
    );
    expect(tracker.records.get(1)!.state.state).toBe("succeeded");
    expect(orderedRecords(tracker)).toHaveLength(1);
  });
});

describe("late poll responses (event/page interleaving)", () => {
  it("a late poll response never reverts a finished job to running", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, submitted(1, 1));
    applyJobEvent(tracker, started(2, 1));
    // The response snapshot was taken at seq 2 (job still running); the
    // terminal event lands before the response does.
    const stale = page({
      latest_seq: 2,
      jobs: [record({ id: 1, state: { state: "running" }, started_at: at })],
    });
    applyJobEvent(tracker, succeeded(3, 1, { inserted: 3 }));
    applyJobsPage(tracker, stale);
    expect(tracker.cursor).toBe(3);
    expect(tracker.records.get(1)!.state).toEqual({
      state: "succeeded",
      result: { inserted: 3 },
    });
    expect(activeJobCount(tracker)).toBe(0);
    expect(hasActiveJob(tracker, "import_mbox")).toBe(false);
  });

  it("failed and cancelled jobs equally survive their own stale pages", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, submitted(1, 1));
    applyJobEvent(tracker, started(2, 1));
    applyJobEvent(tracker, submitted(3, 2));
    applyJobEvent(tracker, failed(4, 1, { kind: "network", message: "down" }));
    applyJobEvent(tracker, cancelled(5, 2));
    // Snapshot served at seq 3: job 1 running, job 2 queued.
    applyJobsPage(
      tracker,
      page({
        latest_seq: 3,
        jobs: [
          record({ id: 1, state: { state: "running" }, started_at: at }),
          record({ id: 2, state: { state: "queued" } }),
        ],
      }),
    );
    expect(tracker.records.get(1)!.state.state).toBe("failed");
    expect(tracker.records.get(2)!.state.state).toBe("cancelled");
    expect(activeJobCount(tracker)).toBe(0);
    expect(tracker.cursor).toBe(5);
  });

  it("a stale trimmed page keeps newer records over the wholesale rebuild", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, submitted(1, 1));
    applyJobEvent(tracker, started(2, 1));
    applyJobEvent(tracker, succeeded(3, 1, null));
    // Job 2 was submitted while the request was in flight — the snapshot
    // never saw it at all.
    applyJobEvent(tracker, submitted(4, 2));
    applyJobsPage(
      tracker,
      page({
        trimmed: true,
        oldest_kept: 1,
        latest_seq: 2,
        jobs: [record({ id: 1, state: { state: "running" }, started_at: at })],
      }),
    );
    expect(tracker.records.get(1)!.state.state).toBe("succeeded");
    expect(tracker.records.get(2)!.state.state).toBe("queued");
    expect(activeJobCount(tracker)).toBe(1);
    expect(tracker.cursor).toBe(4);
  });

  it("overlapping poll responses applied out of order cannot revert anything", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, submitted(1, 1));
    applyJobEvent(tracker, submitted(2, 2));
    applyJobEvent(tracker, started(3, 1));
    applyJobEvent(tracker, succeeded(4, 1, { embedded: 4 }));
    // The newer response (served at seq 5) lands first: it carries job
    // 2's start, which was missed live.
    applyJobsPage(
      tracker,
      page({
        events: [started(5, 2)],
        latest_seq: 5,
        jobs: [
          record({
            id: 1,
            state: { state: "succeeded", result: { embedded: 4 } },
            finished_at: at,
          }),
          record({ id: 2, kind: "imap_sync", state: { state: "running" }, started_at: at }),
        ],
      }),
    );
    // ...then the older one (served at seq 3), which believes job 1 is
    // still running and job 2 still queued.
    applyJobsPage(
      tracker,
      page({
        latest_seq: 3,
        jobs: [
          record({ id: 1, state: { state: "running" }, started_at: at }),
          record({ id: 2, kind: "imap_sync", state: { state: "queued" } }),
        ],
      }),
    );
    expect(tracker.cursor).toBe(5);
    expect(tracker.records.get(1)!.state.state).toBe("succeeded");
    expect(tracker.records.get(2)!.state.state).toBe("running");
    expect(activeJobCount(tracker)).toBe(1);
  });

  it("live progress outruns a snapshot row taken before it", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, submitted(1, 1));
    applyJobEvent(tracker, started(2, 1));
    applyJobEvent(tracker, progress(3, 1, 7, 10));
    applyJobsPage(
      tracker,
      page({
        latest_seq: 2,
        jobs: [
          record({
            id: 1,
            state: { state: "running" },
            started_at: at,
            progress: { done: 2, total: 10, message: "embedding" },
          }),
        ],
      }),
    );
    const rec = tracker.records.get(1)!;
    expect(rec.state.state).toBe("running");
    expect(rec.progress).toEqual({ done: 7, total: 10, message: "embedding" });
    expect(activeJobCount(tracker)).toBe(1);
  });

  it("a gap stays flagged until a page new enough to cover it lands", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, submitted(1, 1));
    applyJobEvent(tracker, succeeded(4, 1, null)); // gap: 2..3 missed
    expect(tracker.needsPoll).toBe(true);
    // A response racing the gap (older than the cursor) cannot heal it…
    applyJobsPage(
      tracker,
      page({
        latest_seq: 3,
        jobs: [record({ id: 1, state: { state: "running" }, started_at: at })],
      }),
    );
    expect(tracker.needsPoll).toBe(true);
    expect(tracker.cursor).toBe(4);
    expect(tracker.records.get(1)!.state.state).toBe("succeeded");
    // …a page at the cursor does.
    applyJobsPage(
      tracker,
      page({
        events: [started(2, 1), progress(3, 1, 1, 2)],
        latest_seq: 4,
        jobs: [
          record({ id: 1, state: { state: "succeeded", result: null }, finished_at: at }),
        ],
      }),
    );
    expect(tracker.needsPoll).toBe(false);
    expect(tracker.cursor).toBe(4);
  });

  it("a stale page still delivers rows the tracker has no newer knowledge for", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, submitted(1, 1));
    // Job 2 lived entirely inside the missed gap 2..3.
    applyJobEvent(tracker, succeeded(4, 1, null));
    // A poll response served at seq 3 lands late: its snapshot saw job 2
    // running but not job 1 finishing. Job 1 keeps its terminal state;
    // job 2 — otherwise unknown — is still merged in.
    applyJobsPage(
      tracker,
      page({
        latest_seq: 3,
        events: [], // older than the cursor — nothing to replay
        jobs: [
          record({ id: 1, state: { state: "running" }, started_at: at }),
          record({ id: 2, kind: "imap_sync", state: { state: "running" }, started_at: at }),
        ],
      }),
    );
    expect(tracker.records.get(1)!.state.state).toBe("succeeded");
    expect(tracker.records.get(2)!.state.state).toBe("running");
    expect(activeJobCount(tracker)).toBe(1);
    // The page predates the cursor, so the gap flag survives it.
    expect(tracker.needsPoll).toBe(true);
  });
});

describe("jobsStatusText", () => {
  it("describes the empty state", () => {
    expect(jobsStatusText(newJobsTracker())).toContain("No background jobs yet");
  });

  it("counts running and queued jobs", () => {
    const tracker = newJobsTracker();
    applyJobsPage(
      tracker,
      page({
        latest_seq: 6,
        jobs: [
          record({ id: 3, kind: "imap_sync", state: { state: "running" } }),
          record({ id: 2, kind: "embed_messages", state: { state: "queued" } }),
          record({ id: 1, state: { state: "succeeded", result: null } }),
        ],
      }),
    );
    expect(jobsStatusText(tracker)).toBe(
      "1 running · 1 queued — searches stay live while jobs work.",
    );
  });

  it("reports only-finished archives", () => {
    const tracker = newJobsTracker();
    applyJobsPage(
      tracker,
      page({ latest_seq: 2, jobs: [record({ id: 1, state: { state: "cancelled" } })] }),
    );
    expect(jobsStatusText(tracker)).toBe("No active jobs (1 finished).");
  });

  it("counts restored ledger rows as 'earlier', never as this session's", () => {
    // Boot after a restart: the snapshot carries two restored rows and
    // one job this session finished.
    const tracker = newJobsTracker();
    applyJobsPage(
      tracker,
      page({
        latest_seq: 3,
        jobs: [
          record({ id: 42, state: { state: "succeeded", result: { inserted: 5505 } } }),
          record({
            id: 999_999_999_998,
            kind: "import_mbox",
            state: { state: "succeeded", result: { inserted: 5505, skipped: 3 } },
            restored: true,
          }),
          record({
            id: 999_999_999_999,
            kind: "imap_sync",
            state: { state: "cancelled" },
            restored: true,
          }),
        ],
      }),
    );
    expect(jobsStatusText(tracker)).toBe("No active jobs (1 finished · 2 earlier).");

    // Restored-only history does not claim session work either.
    const fresh = newJobsTracker();
    applyJobsPage(
      fresh,
      page({
        latest_seq: 0,
        jobs: [
          record({
            id: 999_999_999_999,
            kind: "import_mbox",
            state: { state: "succeeded", result: { inserted: 5505 } },
            restored: true,
          }),
        ],
      }),
    );
    expect(jobsStatusText(fresh)).toBe("No active jobs (1 earlier).");
  });
});

describe("hasActiveJob", () => {
  it("is true only for queued or running jobs of the kind", () => {
    const tracker = newJobsTracker();
    applyJobsPage(
      tracker,
      page({
        latest_seq: 3,
        jobs: [
          record({ id: 2, kind: "embed_messages", state: { state: "queued" } }),
          record({ id: 1, kind: "embed_messages", state: { state: "succeeded", result: null } }),
        ],
      }),
    );
    expect(hasActiveJob(tracker, "embed_messages")).toBe(true);
    expect(hasActiveJob(tracker, "imap_sync")).toBe(false);
  });
});

describe("activeJobCount", () => {
  it("activeJobCount counts queued and running jobs only", () => {
    const tracker = newJobsTracker();
    applyJobEvent(tracker, { seq: 1, type: "submitted", job: 1, kind: "import_mbox", at: "t" });
    applyJobEvent(tracker, { seq: 2, type: "started", job: 1, at: "t" });
    applyJobEvent(tracker, { seq: 3, type: "submitted", job: 2, kind: "imap_sync_all", at: "t" });
    applyJobEvent(tracker, { seq: 4, type: "succeeded", job: 2, at: "t", result: null });
    expect(activeJobCount(tracker)).toBe(1);
  });
});

describe("describeJobState", () => {
  it("renders progress with a percentage", () => {
    const rec = record({
      kind: "embed_messages",
      state: { state: "running" },
      progress: { done: 500, total: 1200, message: "embedding" },
    });
    // Plain words on the Activity row: the kind reads "local search
    // index" and the backend's "embedding" worker token renders as
    // "indexing" (the wire token itself stays unchanged).
    expect(describeJobState(rec)).toBe("local search index — 500/1200 (42%) · indexing");
  });

  it("renders terminal states with summaries and typed errors", () => {
    expect(
      describeJobState(
        record({ state: { state: "succeeded", result: { inserted: 4, skipped: 2 } } }),
      ),
    ).toBe(".mbox import — done (4 new, 2 present)");
    expect(
      describeJobState(record({ state: { state: "failed", error: { kind: "import", message: "bad export" } } })),
    ).toBe(".mbox import — failed: bad export");
    expect(describeJobState(record({ state: { state: "cancelled" } }))).toBe(
      ".mbox import — cancelled",
    );
    expect(describeJobState(record({ state: { state: "queued" } }))).toBe(
      ".mbox import — queued",
    );
  });
});

describe("renderJobsList", () => {
  it("renders newest first, capped, with state classes", () => {
    document.body.innerHTML = '<ul id="jobs"></ul>';
    const el = document.querySelector<HTMLElement>("#jobs")!;
    const tracker = newJobsTracker();
    const jobs = Array.from({ length: 25 }, (_, i) =>
      record({
        id: i + 1,
        kind: "imap_sync",
        state: { state: "succeeded", result: null },
      }),
    );
    applyJobsPage(tracker, page({ latest_seq: 75, jobs }));
    renderJobsList(el, tracker);
    const items = [...el.querySelectorAll("li")];
    expect(items).toHaveLength(20);
    expect(items[0].textContent).toContain("IMAP sync — done");
    expect(items[0].className).toBe("job succeeded");
    // Newest (highest id) first.
    expect(items.map((li) => li.textContent!.startsWith("IMAP sync")).every(Boolean)).toBe(true);
  });

  it("renders restored ledger rows below this session's, through the same line", () => {
    document.body.innerHTML = '<ul id="jobs"></ul>';
    const el = document.querySelector<HTMLElement>("#jobs")!;
    const tracker = newJobsTracker();
    // Live ids sit above the restored band, exactly like the wire.
    applyJobsPage(
      tracker,
      page({
        latest_seq: 1,
        jobs: [
          record({
            id: 1_000_000_000_001,
            kind: "embed_messages",
            state: { state: "succeeded", result: { embedded: 8 } },
          }),
          record({
            id: 999_999_999_998,
            kind: "import_mbox",
            state: { state: "succeeded", result: { summary: { inserted: 5505, skipped: 3 } } },
            restored: true,
          }),
          record({
            id: 999_999_999_999,
            kind: "imap_sync",
            state: {
              state: "failed",
              error: { kind: "network", message: "connection refused" },
            },
            restored: true,
          }),
        ],
      }),
    );
    renderJobsList(el, tracker);
    const items = [...el.querySelectorAll("li")];
    expect(items).toHaveLength(3);
    // Live record first (its id sits above the restored band), then the
    // restored rows newest-first (the newest carries the higher id) — all
    // through describeJobState, so a restored import reads exactly like
    // one this session ran.
    expect(items[0].textContent).toBe("local search index — done (8 indexed)");
    expect(items[0].className).toBe("job succeeded");
    expect(items[1].textContent).toBe("IMAP sync — failed: connection refused");
    expect(items[1].className).toBe("job failed");
    expect(items[2].textContent).toBe(".mbox import — done (5505 new, 3 present)");
    expect(items[2].className).toBe("job succeeded");
  });
});

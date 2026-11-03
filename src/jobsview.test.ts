// @vitest-environment happy-dom
// Unit tests for the v2 background-jobs view helpers: the cursor-tracking
// client (live event application, gap detection → polling, trimmed-page
// reconciliation), status wording, and the DOM list builder.
import { describe, expect, it } from "vitest";
import type { JobEvent, JobEventsPage, JobRecord } from "./types";
import {
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

describe("describeJobState", () => {
  it("renders progress with a percentage", () => {
    const rec = record({
      kind: "embed_messages",
      state: { state: "running" },
      progress: { done: 500, total: 1200, message: "embedding" },
    });
    expect(describeJobState(rec)).toBe("local embeddings — 500/1200 (42%) · embedding");
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
});

// Unit tests for the drop-to-search onboarding model (src/onboarding.ts):
// the drop → detect → import → completion state machine driven with
// fixture payload shapes (a bounded-preview result and a job outcome —
// the same shapes the faked IPC layer delivers; no real archives), the
// honest serial-queue copy, the completion view-model's no-receipt
// rule, the top-author seed, and the rolling-rate ETA estimator's
// honesty rules (monotonic, never NaN, "estimating…" while unknown).
import { describe, expect, it } from "vitest";
import {
  ESTIMATING_TEXT,
  RollingEta,
  completionOf,
  etaText,
  initialOnboardingState,
  queueWaitLine,
  reduceOnboarding,
  topAuthorSeed,
} from "./onboarding";
import type { ArchiveStats, ChatImportOutcome, ImportPreview } from "./types";

/** A bounded-preview result as the mbox (Takeout) path delivers it: the
 * scan completed, so messages_seen is the known total. */
const MBOX_PREVIEW: ImportPreview = {
  kind: "mbox",
  source: "mbox",
  input_bytes: 4096,
  messages_seen: 120,
  scan_complete: true,
  parse_failures: 0,
  samples: [{ author: "Ana Sender <ana@example.com>", subject: "Welcome", sent_at: "2026-01-01T10:00:00Z", snippet: "hello" }],
  media_referenced: 0,
  media_missing: 0,
  warnings: [],
  naive_timestamps: 0,
  assumed_zone: null,
  free_bytes: 1024 * 1024 * 1024,
};

/** A WhatsApp export's preview: detected as "whatsapp" by the bounded
 * scan, media referenced. */
const WHATSAPP_PREVIEW: ImportPreview = {
  ...MBOX_PREVIEW,
  kind: "whatsapp",
  source: "whatsapp:chat",
  messages_seen: 40,
  media_referenced: 12,
  media_missing: 0,
};

function outcome(source: string, seen: number, inserted: number, skipped: number, failed: number): ChatImportOutcome {
  return { summary: { source, seen, inserted, skipped, failed }, import_id: "run-1", media: null };
}

describe("onboarding state machine (drop → detect → import → completion)", () => {
  it("an mbox drop detects through the preview and completes with the run's real counts", () => {
    let s = initialOnboardingState();
    s = reduceOnboarding(s, { type: "drop", paths: ["/takeout/mail.mbox"] });
    // The queue only holds paths not yet started: the drop moved the
    // path straight to detection in the wiring's pump, modelled here by
    // detect-started leaving the queue to the caller.
    expect(s.queue).toEqual(["/takeout/mail.mbox"]);

    s = reduceOnboarding(s, { type: "detect-started", path: "/takeout/mail.mbox" });
    expect(s.phase).toBe("detecting");
    s = reduceOnboarding(s, { type: "detected", path: "/takeout/mail.mbox", preview: MBOX_PREVIEW });
    expect(s.phase).toBe("awaiting-confirm");
    expect(s.preview?.kind).toBe("mbox"); // auto-detect, no mode picker

    s = reduceOnboarding(s, { type: "import-started", path: "/takeout/mail.mbox" });
    expect(s.phase).toBe("importing");
    s = reduceOnboarding(s, {
      type: "import-succeeded",
      path: "/takeout/mail.mbox",
      outcome: outcome("mbox", 120, 118, 2, 0),
    });
    expect(s.phase).toBe("completed");
    expect(s.completion).toEqual({ source: "mbox", seen: 120, inserted: 118, skipped: 2, failed: 0 });
  });

  it("a WhatsApp export drop drives the same machine with its own detection", () => {
    let s = initialOnboardingState();
    s = reduceOnboarding(s, { type: "drop", paths: ["/whatsapp/chat.zip"] });
    s = reduceOnboarding(s, { type: "detect-started", path: "/whatsapp/chat.zip" });
    s = reduceOnboarding(s, { type: "detected", path: "/whatsapp/chat.zip", preview: WHATSAPP_PREVIEW });
    expect(s.preview?.kind).toBe("whatsapp");
    s = reduceOnboarding(s, { type: "import-started", path: "/whatsapp/chat.zip" });
    s = reduceOnboarding(s, {
      type: "import-succeeded",
      path: "/whatsapp/chat.zip",
      outcome: outcome("whatsapp:chat", 40, 40, 0, 1),
    });
    expect(s.phase).toBe("completed");
    expect(s.completion).toEqual({ source: "whatsapp:chat", seen: 40, inserted: 40, skipped: 0, failed: 1 });
  });

  it("the completion view-model never carries a receipt id", () => {
    // The serialized outcome carries {summary, import_id, media} with no
    // receipt; completionOf keeps only the summary counts — even though
    // the outcome here carries an import_id, it must not leak through.
    const completion = completionOf(outcome("mbox", 3, 2, 1, 0));
    expect(Object.keys(completion).sort()).toEqual(["failed", "inserted", "seen", "skipped", "source"]);
    expect(JSON.stringify(completion)).not.toContain("receipt");
    expect(JSON.stringify(completion)).not.toContain("import_id");
  });

  it("multi-file drops queue behind the flow; extra drops never jump the queue", () => {
    let s = initialOnboardingState();
    s = reduceOnboarding(s, { type: "drop", paths: ["/a.mbox", "/b.zip"] });
    s = reduceOnboarding(s, { type: "detect-started", path: "/a.mbox" });
    // A drop mid-detect joins the back; the active path keeps the slot.
    s = reduceOnboarding(s, { type: "drop", paths: ["/c.txt"] });
    expect(s.queue).toEqual(["/a.mbox", "/b.zip", "/c.txt"]);
    // Stale events for a path that is not active cannot hijack the flow.
    s = reduceOnboarding(s, { type: "detected", path: "/b.zip", preview: WHATSAPP_PREVIEW });
    expect(s.phase).toBe("detecting");
  });

  it("a cancelled import returns to the start state and keeps the waiting queue", () => {
    let s = initialOnboardingState();
    s = reduceOnboarding(s, { type: "drop", paths: ["/a.mbox", "/b.zip"] });
    s = reduceOnboarding(s, { type: "detect-started", path: "/a.mbox" });
    s = reduceOnboarding(s, { type: "detected", path: "/a.mbox", preview: MBOX_PREVIEW });
    s = reduceOnboarding(s, { type: "import-started", path: "/a.mbox" });
    s = reduceOnboarding(s, { type: "import-cancelled", path: "/a.mbox" });
    expect(s.phase).toBe("idle");
    expect(s.completion).toBeNull();
    expect(s.queue).toEqual(["/a.mbox", "/b.zip"]);
  });

  it("a failed detect or import records the plain-words message", () => {
    let s = initialOnboardingState();
    s = reduceOnboarding(s, { type: "drop", paths: ["/a.mbox"] });
    s = reduceOnboarding(s, { type: "detect-started", path: "/a.mbox" });
    s = reduceOnboarding(s, { type: "detect-failed", path: "/a.mbox", message: "Could not read /a.mbox" });
    expect(s.phase).toBe("failed");
    expect(s.error).toBe("Could not read /a.mbox");

    s = reduceOnboarding(s, { type: "reset" });
    expect(s.phase).toBe("idle");
    expect(s.error).toBeNull();
  });

  it("empty drops change nothing", () => {
    const s = initialOnboardingState();
    expect(reduceOnboarding(s, { type: "drop", paths: [] })).toBe(s);
    expect(reduceOnboarding(s, { type: "drop", paths: [""] })).toBe(s);
  });
});

describe("queueWaitLine (the honest serial queue)", () => {
  it("says imports run one at a time, singular and plural", () => {
    expect(queueWaitLine(0)).toBe("");
    expect(queueWaitLine(1)).toBe("1 more archive waiting — imports run one at a time.");
    expect(queueWaitLine(3)).toBe("3 more archives waiting — imports run one at a time.");
  });
});

describe("topAuthorSeed (the completion search's example query)", () => {
  const stats = (authors: Array<{ author: string; count: number }>): ArchiveStats => ({
    total: 10,
    first_sent_at: null,
    last_sent_at: null,
    by_source: [],
    by_day: [],
    top_chats: [],
    top_authors: authors,
  });

  it("picks the most frequent sender", () => {
    expect(
      topAuthorSeed(stats([
        { author: "ana", count: 4 },
        { author: "marko", count: 9 },
        { author: "jelena", count: 2 },
      ])),
    ).toBe("marko");
  });

  it("never invents a seed: empty authors or zero counts stay null", () => {
    expect(topAuthorSeed(stats([]))).toBeNull();
    expect(topAuthorSeed(stats([{ author: "", count: 5 }]))).toBeNull();
    expect(topAuthorSeed(stats([{ author: "ana", count: 0 }]))).toBeNull();
  });
});

describe("RollingEta (the rolling-rate ETA estimator)", () => {
  it("stays 'estimating…' while the total is unknown or no rate exists yet", () => {
    const eta = new RollingEta();
    expect(eta.etaMs(0)).toBeNull(); // unknown/zero total
    eta.sample(10, 1_000);
    expect(eta.etaMs(100)).toBeNull(); // one sample: no rate yet
    expect(etaText(null)).toBe(ESTIMATING_TEXT);
  });

  it("estimates from the rolling rate and never returns NaN", () => {
    const eta = new RollingEta();
    eta.sample(0, 0);
    eta.sample(50, 1_000); // 50 units per second
    const ms = eta.etaMs(100);
    expect(ms).not.toBeNull();
    expect(Number.isNaN(ms as number)).toBe(false);
    expect(ms).toBeCloseTo(1_000, -1); // 50 remaining at 50/s = ~1s
  });

  it("is monotonic: the shown ETA never increases within a run", () => {
    const eta = new RollingEta();
    eta.sample(0, 0);
    eta.sample(40, 1_000);
    const first = eta.etaMs(100);
    expect(first).not.toBeNull();
    // The rate halves (a slow patch): the raw estimate would double,
    // but the estimate shown may only tighten.
    eta.sample(50, 3_000);
    const second = eta.etaMs(100);
    expect(second).not.toBeNull();
    expect(second!).toBeLessThanOrEqual(first!);
    // And a speed-up still gives a smaller, honest estimate.
    eta.sample(90, 3_500);
    const third = eta.etaMs(100);
    expect(third!).toBeLessThanOrEqual(second!);
  });

  it("finishing work reports zero remaining, never a negative or NaN", () => {
    const eta = new RollingEta();
    eta.sample(0, 0);
    eta.sample(100, 1_000);
    expect(eta.etaMs(100)).toBe(0);
  });

  it("ignores rewound or duplicated counters instead of breaking the rate", () => {
    const eta = new RollingEta();
    eta.sample(10, 1_000);
    eta.sample(5, 2_000); // rewound
    eta.sample(10, 1_500); // time rewound
    eta.sample(10, 1_000); // exact duplicate
    expect(eta.etaMs(100)).toBeNull(); // still effectively one sample
  });

  it("a new run starts fresh: history and the monotonic clamp reset", () => {
    const eta = new RollingEta();
    eta.sample(0, 0);
    eta.sample(50, 1_000);
    expect(eta.etaMs(100)).toBeCloseTo(1_000, -1);
    eta.startRun();
    expect(eta.etaMs(100)).toBeNull(); // no samples survive the reset
    eta.sample(0, 10_000);
    eta.sample(100, 11_000); // 100/s this time
    expect(eta.etaMs(200)).toBeCloseTo(1_000, -1);
  });

  it("etaText renders honest buckets and never NaN", () => {
    expect(etaText(null)).toBe(ESTIMATING_TEXT);
    expect(etaText(Number.NaN)).toBe(ESTIMATING_TEXT);
    expect(etaText(-5)).toBe(ESTIMATING_TEXT);
    expect(etaText(4_000)).toBe("under 10 seconds");
    expect(etaText(30_000)).toBe("~30 seconds");
    expect(etaText(90_000)).toBe("~2 minutes");
    expect(etaText(3_600_000)).toBe("~1 hour");
  });
});

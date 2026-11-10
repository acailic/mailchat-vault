import { describe, expect, it } from "vitest";
import {
  folderSummaryLine,
  formatSentAt,
  humanSize,
  mediaSummaryPart,
  snippet,
} from "./format";
import type { FolderSyncSummary } from "./types";

describe("formatSentAt", () => {
  it("renders a compact local timestamp", () => {
    // Keep the expectation timezone-independent by parsing what we format.
    const out = formatSentAt("2026-11-01T09:05:00Z");
    expect(out).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
    expect(new Date(out).getFullYear()).toBe(2026);
  });

  it("handles empty and invalid values", () => {
    expect(formatSentAt("")).toBe("unknown time");
    expect(formatSentAt("not-a-date")).toBe("not-a-date");
  });

  it("renders the local calendar day, including near-midnight instants", () => {
    // Near-midnight fixture: two instants 40 minutes apart straddling UTC
    // midnight. Expected values are derived from the same local Date getters
    // the implementation uses, so the test stays timezone-independent on any
    // host while still pinning local-day rendering: switching the formatter
    // to UTC getters (getUTCHours/…) fails on every non-UTC machine, and a
    // near-midnight instant is exactly where UTC and local days diverge.
    const expectLocal = (iso: string) => {
      const d = new Date(iso);
      const pad = (n: number) => String(n).padStart(2, "0");
      return (
        `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
        `${pad(d.getHours())}:${pad(d.getMinutes())}`
      );
    };
    const justBeforeMidnightUtc = "2026-03-08T23:40:00Z";
    const justAfterMidnightUtc = "2026-03-09T00:20:00Z";
    expect(formatSentAt(justBeforeMidnightUtc)).toBe(expectLocal(justBeforeMidnightUtc));
    expect(formatSentAt(justAfterMidnightUtc)).toBe(expectLocal(justAfterMidnightUtc));
  });
});

describe("humanSize", () => {
  it("renders bytes through terabytes", () => {
    expect(humanSize(0)).toBe("0 B");
    expect(humanSize(12)).toBe("12 B");
    expect(humanSize(1024)).toBe("1 KB");
    expect(humanSize(2048)).toBe("2 KB");
    expect(humanSize(3.5 * 1024 * 1024)).toBe("3.5 MB");
    expect(humanSize(5 * 1024 * 1024 * 1024)).toBe("5 GB");
    expect(humanSize(2.5 * 1024 ** 4)).toBe("2.5 TB");
  });

  it("rounds large mantissas and rejects nonsense", () => {
    expect(humanSize(102.4 * 1024)).toBe("102 KB");
    expect(humanSize(999 * 1024 * 1024)).toBe("999 MB");
    expect(humanSize(-5)).toBe("—");
    expect(humanSize(Number.NaN)).toBe("—");
    expect(humanSize(Number.POSITIVE_INFINITY)).toBe("—");
  });
});

describe("snippet", () => {
  it("collapses whitespace", () => {
    expect(snippet("Subject\n\nbody   text\nwith\nlines")).toBe("Subject body text with lines");
  });

  it("truncates long bodies with an ellipsis", () => {
    const out = snippet("x".repeat(300), 200);
    expect(out.length).toBe(200);
    expect(out.endsWith("…")).toBe(true);
  });

  it("keeps short bodies untouched", () => {
    expect(snippet("short body", 200)).toBe("short body");
  });
});

describe("folderSummaryLine (sync outcome lines, shared)", () => {
  const base: FolderSyncSummary = {
    folder: "INBOX",
    uidvalidity: 7,
    reset: false,
    seen: 3,
    inserted: 1,
    skipped: 2,
    failed: 0,
    resolved_failures: 0,
    conflicting_revisions: 0,
    error: null,
    media: null,
  };

  it("keeps the insert/skip line for a clean folder", () => {
    expect(folderSummaryLine(base)).toBe("INBOX: 1 new, 2 already present");
  });

  it("appends only the non-zero extras in order (failed, media, rescan, error)", () => {
    const line = folderSummaryLine({
      ...base,
      failed: 2,
      reset: true,
      error: "io error",
      media: { stored: 3, deduped: 1, missing: 0 },
    });
    expect(line).toBe(
      "INBOX: 1 new, 2 already present, 2 failed to parse, media: 3 stored, 1 deduped (full rescan) — failed: io error",
    );
  });

  it("a zero media pass or a pass that did not run says nothing", () => {
    expect(folderSummaryLine({ ...base, media: { stored: 0, deduped: 0, missing: 0 } })).toBe(
      "INBOX: 1 new, 2 already present",
    );
    expect(folderSummaryLine({ ...base, media: null })).toBe("INBOX: 1 new, 2 already present");
  });
});

describe("mediaSummaryPart (only-non-zero, S2 style)", () => {
  it("lists every non-zero counter in order", () => {
    expect(mediaSummaryPart({ stored: 2, deduped: 1, missing: 1 })).toBe(
      "2 stored, 1 deduped, 1 missing",
    );
    expect(mediaSummaryPart({ stored: 0, deduped: 0, missing: 3 })).toBe("3 missing");
  });

  it("empty for null/undefined/all-zero", () => {
    expect(mediaSummaryPart(null)).toBe("");
    expect(mediaSummaryPart(undefined)).toBe("");
    expect(mediaSummaryPart({ stored: 0, deduped: 0, missing: 0 })).toBe("");
  });
});

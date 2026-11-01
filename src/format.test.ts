import { describe, expect, it } from "vitest";
import { formatSentAt, snippet } from "./format";

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

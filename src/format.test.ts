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

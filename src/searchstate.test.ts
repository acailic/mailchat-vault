// Unit tests for the v2 search state logic (M3.2): filter chips, era
// buckets, request building, stale-response tokens, selection/scroll
// restoration, archive-revision refresh detection, deep links, copy text,
// and the reading-pane context paging model.

import { describe, expect, it } from "vitest";
import type { MessageSummary } from "./v2types";
import {
  activeEraIndex,
  buildRequest,
  captureResults,
  contextCursors,
  contextEntries,
  dateBoundsValid,
  DEFAULT_FILTERS,
  eraBuckets,
  filterChips,
  hasActiveFilters,
  mergeContextPages,
  messageLink,
  messageToText,
  parseMessageLink,
  restoreResults,
  revisionChanged,
  selectionIds,
  Sequence,
  withoutChip,
  type SearchFilters,
} from "./searchstate";

const labels = {
  source: (id: string) => `Source ${id}`,
  import: (id: string) => `import-${id}`,
};

function filters(over: Partial<SearchFilters>): SearchFilters {
  return { ...DEFAULT_FILTERS, ...over };
}

/** One `by_day` series entry (the era strip's input). */
const day = (d: string, count = 1) => ({ day: d, count });

describe("filter model", () => {
  it("empty filters are inactive; any filter activates browse", () => {
    expect(hasActiveFilters(DEFAULT_FILTERS)).toBe(false);
    expect(hasActiveFilters(filters({ author: "ana" }))).toBe(true);
    expect(hasActiveFilters(filters({ text: "hello" }))).toBe(false);
    expect(hasActiveFilters(filters({ hasAttachment: false }))).toBe(true);
  });

  it("chips cover every filter kind with removable labels", () => {
    const f = filters({
      sourceIds: ["s1", "s2"],
      author: "Ana",
      fromDate: "2026-01-01",
      toDate: "2026-02-01",
      hasAttachment: true,
      importJobId: "imp-9",
    });
    const chips = filterChips(f, labels);
    expect(chips.map((c) => c.kind)).toEqual([
      "source",
      "source",
      "author",
      "from-date",
      "to-date",
      "attachment",
      "import-scope",
    ]);
    expect(chips[0].label).toBe("Source s1");
    expect(chips[2].label).toBe("author: Ana");
    expect(chips[3].label).toBe("from 2026-01-01");
    expect(chips[5].label).toBe("has attachment");
    expect(chips[6].label).toBe("import: import-imp-9");
  });

  it("a negative attachment chip reads as no attachment", () => {
    const chips = filterChips(filters({ hasAttachment: false }), labels);
    expect(chips[0].label).toBe("no attachment");
  });

  it("removing a chip clears exactly that filter", () => {
    const f = filters({ sourceIds: ["s1", "s2"], author: "Ana", hasAttachment: true });
    const chips = filterChips(f, labels);
    let next = withoutChip(f, chips[0]); // remove source s1
    expect(next.sourceIds).toEqual(["s2"]);
    expect(next.author).toBe("Ana");
    next = withoutChip(next, chips[2]); // remove author
    expect(next.author).toBe("");
    expect(next.sourceIds).toEqual(["s2"]);
    next = withoutChip(next, filterChips(next, labels)[1]); // remove attachment
    expect(next.hasAttachment).toBeNull();
  });

  it("reversed date bounds are rejected before invocation", () => {
    expect(dateBoundsValid("2026-02-02", "2026-02-01")).toBe(false);
    expect(dateBoundsValid("2026-02-01", "2026-02-01")).toBe(true);
    expect(dateBoundsValid("", "2026-02-01")).toBe(true);
    expect(dateBoundsValid("2026-02-01", "")).toBe(true);
  });

  it("buildRequest maps the filter model onto the wire shape", () => {
    const req = buildRequest(
      filters({
        text: "vault",
        sourceIds: ["s1"],
        author: "Ana",
        fromDate: "2026-01-01",
        importJobId: "imp-1",
        hasAttachment: true,
        sort: "newest",
      }),
      50,
      100,
    );
    expect(req).toEqual({
      text: "vault",
      source_ids: ["s1"],
      author: "Ana",
      from_date: "2026-01-01",
      to_date: null,
      timezone: "",
      has_attachment: true,
      import_job_id: "imp-1",
      sort: "newest",
      limit: 50,
      offset: 100,
    });
    // The user's sort choice survives for text searches.
    expect(buildRequest(filters({ text: "x", sort: "newest" }), 50, 0).sort).toBe("newest");
  });
});

describe("era buckets", () => {
  it("a multi-year archive becomes one year chip per year, ascending", () => {
    const buckets = eraBuckets([
      day("2021-03-04"),
      day("2019-01-01", 5),
      day("2020-12-31", 2),
    ]);
    expect(buckets).toEqual([
      { label: "2019", from: "2019-01-01", to: "2019-12-31" },
      { label: "2020", from: "2020-01-01", to: "2020-12-31" },
      { label: "2021", from: "2021-01-01", to: "2021-12-31" },
    ]);
  });

  it("an archive inside one year becomes month chips with true month ends", () => {
    const buckets = eraBuckets([
      day("2024-01-31"),
      day("2024-02-29"), // leap February: the 29th, not the 28th
      day("2024-03-01"),
    ]);
    expect(buckets).toEqual([
      { label: "2024-01", from: "2024-01-01", to: "2024-01-31" },
      { label: "2024-02", from: "2024-02-01", to: "2024-02-29" },
      { label: "2024-03", from: "2024-03-01", to: "2024-03-31" },
    ]);
  });

  it("months that straddle a year boundary still take year chips", () => {
    expect(eraBuckets([day("2026-12-30"), day("2027-01-02")]).map((b) => b.label)).toEqual([
      "2026",
      "2027",
    ]);
  });

  it("more than eight years keeps the most recent eight", () => {
    const days = Array.from({ length: 12 }, (_, y) => day(`${2014 + y}-06-15`));
    expect(eraBuckets(days).map((b) => b.label)).toEqual([
      "2018",
      "2019",
      "2020",
      "2021",
      "2022",
      "2023",
      "2024",
      "2025",
    ]);
  });

  it("more than eight months in one year coarsen to that single year", () => {
    const days = Array.from({ length: 10 }, (_, m) => day(`2026-${String(m + 1).padStart(2, "0")}-05`));
    expect(eraBuckets(days)).toEqual([{ label: "2026", from: "2026-01-01", to: "2026-12-31" }]);
  });

  it("a tiny or undated archive renders nothing", () => {
    expect(eraBuckets([])).toEqual([]); // undated (empty day series)
    expect(eraBuckets([day("2026-01-10", 4)])).toEqual([]); // one month of history
  });
});

describe("era chip highlight", () => {
  const buckets = eraBuckets([day("2019-05-05"), day("2020-05-05")]);

  it("lights the bucket whose exact bounds the filters hold", () => {
    expect(activeEraIndex(buckets, filters({ fromDate: "2020-01-01", toDate: "2020-12-31" }))).toBe(1);
  });

  it("a hand-typed range inside a year is not that year's chip", () => {
    expect(activeEraIndex(buckets, filters({ fromDate: "2020-03-01", toDate: "2020-12-31" }))).toBe(-1);
  });

  it("one-sided or empty bounds never match", () => {
    expect(activeEraIndex(buckets, filters({ fromDate: "2020-01-01" }))).toBe(-1);
    expect(activeEraIndex(buckets, filters({ toDate: "2020-12-31" }))).toBe(-1);
    expect(activeEraIndex(buckets, DEFAULT_FILTERS)).toBe(-1);
  });
});

describe("stale-response guard", () => {
  it("older responses are never current after a newer token", () => {
    const seq = new Sequence();
    const a = seq.next();
    const b = seq.next();
    expect(seq.isCurrent(a)).toBe(false);
    expect(seq.isCurrent(b)).toBe(true);
  });

  it("bump invalidates every outstanding token", () => {
    const seq = new Sequence();
    const a = seq.next();
    seq.bump();
    expect(seq.isCurrent(a)).toBe(false);
    const b = seq.next();
    expect(seq.isCurrent(b)).toBe(true);
  });
});

describe("selection + scroll restoration", () => {
  const hits: MessageSummary[] = [1, 2, 3].map((id) => ({
    id,
    source_id: `s${id}`,
    source_label: `Source ${id}`,
    conversation_id: null,
    conversation_label: null,
    author: "Ana",
    subject: null,
    sent_at: "2026-01-01T10:00:00Z",
    time_confidence: "legacy_assumed",
    snippet: "",
    attachment_count: 0,
    available_attachment_count: 0,
    has_conflicting_revision: false,
  }));

  it("capture records the selected row and scroll position", () => {
    const snap = captureResults({ scrollTop: 321 }, 1, 2);
    expect(snap).toEqual({ selectedId: 2, activeIndex: 1, scrollTop: 321 });
  });

  it("restore re-selects the opened row and puts scroll back", () => {
    const list = { scrollTop: 0 };
    const snap = captureResults({ scrollTop: 321 }, 1, 2);
    const restored = restoreResults(list, hits, snap);
    expect(restored.activeIndex).toBe(1);
    expect(restored.selectedId).toBe(2);
    expect(list.scrollTop).toBe(321);
  });

  it("a vanished selected row clamps instead of vanishing", () => {
    const list = { scrollTop: 0 };
    const restored = restoreResults(list, hits, { selectedId: 999, activeIndex: 5, scrollTop: 9 });
    expect(restored.activeIndex).toBe(2);
  });

  it("no snapshot means no selection (fresh search)", () => {
    const list = { scrollTop: 42 };
    expect(restoreResults(list, hits, null)).toEqual({ activeIndex: -1, selectedId: null });
  });
});

describe("archive-revision refresh", () => {
  it("the first page establishes the baseline (no notice)", () => {
    expect(revisionChanged(null, 7)).toBe(false);
  });
  it("a later page at a different revision offers refresh", () => {
    expect(revisionChanged(7, 8)).toBe(true);
    expect(revisionChanged(7, 7)).toBe(false);
  });
});

describe("deep links", () => {
  it("round-trips message ids through the internal locator", () => {
    expect(messageLink(42)).toBe("mailchat://message/42");
    expect(parseMessageLink("mailchat://message/42")).toBe(42);
    expect(parseMessageLink("  mailchat://message/12345  ")).toBe(12345);
  });
  it("rejects non-links, other hosts, and malformed ids", () => {
    expect(parseMessageLink("hello vault")).toBeNull();
    expect(parseMessageLink("mailto://message/1")).toBeNull();
    expect(parseMessageLink("mailchat://conversation/1")).toBeNull();
    expect(parseMessageLink("mailchat://message/12a")).toBeNull();
    expect(parseMessageLink("mailchat://message/")).toBeNull();
  });
});

describe("selection export model", () => {
  const hit = (id: number): MessageSummary => ({
    id,
    source_id: `s${id}`,
    source_label: `Source ${id}`,
    conversation_id: null,
    conversation_label: null,
    author: "Ana",
    subject: null,
    sent_at: "2026-01-01T10:00:00Z",
    time_confidence: "legacy_assumed",
    snippet: "",
    attachment_count: 0,
    available_attachment_count: 0,
    has_conflicting_revision: false,
  });

  it("exports checked rows in results order, not click order", () => {
    const hits = [10, 11, 12].map(hit);
    // selection clicked in reverse order: still exports top-to-bottom
    const selected = new Set([12, 10]);
    expect(selectionIds(hits, selected)).toEqual([10, 12]);
    expect(selectionIds(hits, new Set())).toEqual([]);
  });
});

describe("copy text", () => {
  const detail = {
    id: 7,
    source_id: "uuid-1",
    source_label: "mbox",
    channel: "email",
    chat_id: "thread-a",
    external_id: "e2e-m-1",
    conversation_id: "uuid-c",
    conversation_label: "thread-a",
    conversation_confidence: "legacy_unresolved",
    author: "Ana <ana@example.com>",
    subject: "Delivery date",
    body: "The vault arrives Friday.",
    sent_at: "2026-01-02T10:00:00Z",
    original_timestamp: "Fri, 02 Jan 2026 11:00:00 +0100",
    timezone: "Europe/Zagreb",
    time_confidence: "legacy_assumed",
    attachments: [
      { name: "spec.pdf", kind: "document", size: 99, available: true },
      { name: "lost.jpg", kind: "image", size: 0, available: false },
    ],
    occurrences: [
      {
        scope: "import",
        provider_item_id: "e2e-m-1",
        uidvalidity: null,
        import_id: "imp-42",
        observed_at: "2026-11-01 12:00:00",
      },
    ],
    revisions: [{ message_id: 9, identity: "legacy:e2e-m-1", revision_hash: "h2" }],
    raw_json: "{}",
  };

  it("renders readable text with metadata and provenance", () => {
    const text = messageToText(detail);
    expect(text).toContain("=== Message 7 ===");
    expect(text).toContain("Source: mbox (uuid-1)");
    expect(text).toContain("From: Ana <ana@example.com>");
    expect(text).toContain("Subject: Delivery date");
    // Confidence reaches the reader as words, never the store's raw
    // tokens (F08): `legacy_assumed` → "assumed", `legacy_unresolved`
    // → "unverified grouping".
    expect(text).toContain("Date: 2026-01-02T10:00:00Z (assumed)");
    expect(text).toContain("Conversation: thread-a (unverified grouping)");
    expect(text).not.toContain("legacy_assumed");
    expect(text).not.toContain("legacy_unresolved");
    expect(text).toContain("Original timestamp: Fri, 02 Jan 2026 11:00:00 +0100 (Europe/Zagreb)");
    expect(text).toContain("The vault arrives Friday.");
    expect(text).toContain("spec.pdf [document] (available, 99 bytes)");
    expect(text).toContain("lost.jpg [image] (not preserved, 0 bytes)");
    expect(text).toContain("import imp-42");
    expect(text).toContain("message 9 (legacy:e2e-m-1)");
  });

  it("unknown dates read as unknown, never fabricated", () => {
    const text = messageToText({
      ...detail,
      sent_at: null,
      time_confidence: "missing",
      original_timestamp: "",
      attachments: [],
      occurrences: [],
      revisions: [],
    });
    expect(text).toContain("Date: unknown (no time recorded)");
    expect(text).not.toContain("Original timestamp:");
    expect(text).not.toContain("Attachments:");
  });

  it("an exact time and a verified thread read plainly, an absent qualifier prints no empty fragment", () => {
    const exact = messageToText({
      ...detail,
      time_confidence: "explicit_offset",
      conversation_confidence: "verified",
    });
    expect(exact).toContain("Date: 2026-01-02T10:00:00Z (exact)");
    expect(exact).toContain("Conversation: thread-a (verified)");
    expect(exact).not.toContain("explicit_offset");

    const bare = messageToText({ ...detail, conversation_confidence: null });
    expect(bare).toContain("Conversation: thread-a\n");
    expect(bare).not.toContain("()");

    // An unrecognized future token surfaces verbatim — never blanked.
    const future = messageToText({ ...detail, time_confidence: "some_future_token" });
    expect(future).toContain("(some_future_token)");
  });
});

describe("reading-pane context model", () => {
  const ctx = {
    anchor: {
      id: 3,
      author: "Ana",
      body: "anchor",
      sent_at: "2026-01-02T10:00:00Z",
    },
    before: [
      { id: 1, author: "Ana", sent_at: "2026-01-01T10:00:00Z", time_confidence: "explicit_offset", body: "one" },
      { id: 2, author: "Bo", sent_at: "2026-01-02T09:00:00Z", time_confidence: "explicit_offset", body: "two" },
    ],
    after: [
      { id: 4, author: "Bo", sent_at: "2026-01-03T10:00:00Z", time_confidence: "explicit_offset", body: "four" },
    ],
    unverified_conversation: false,
    has_more_before: true,
    has_more_after: false,
  };

  it("flattens oldest → newest with the anchor marked current", () => {
    const entries = contextEntries(ctx as never);
    expect(entries.map((e) => e.id)).toEqual([1, 2, 3, 4]);
    expect(entries.filter((e) => e.current).map((e) => e.id)).toEqual([3]);
  });

  it("cursors point at the outermost held rows", () => {
    const cursors = contextCursors(contextEntries(ctx as never));
    expect(cursors).toEqual({ beforeCursor: 1, afterCursor: 4 });
  });

  it("merging a page prepends/appends without duplicates or losing the anchor", () => {
    const entries = contextEntries(ctx as never);
    const earlier = {
      ...ctx,
      before: [
        { id: 0, author: "Ana", sent_at: "2025-12-31T10:00:00Z", time_confidence: "explicit_offset", body: "zero" },
        { id: 1, author: "Ana", sent_at: "2026-01-01T10:00:00Z", time_confidence: "explicit_offset", body: "one" },
      ],
      after: [],
    };
    const merged = mergeContextPages(entries, earlier as never);
    expect(merged.map((e) => e.id)).toEqual([0, 1, 2, 3, 4]);
    expect(merged.filter((e) => e.current).map((e) => e.id)).toEqual([3]);
  });

  it("a multi-row earlier page stays oldest → newest at the front", () => {
    const entries = contextEntries(ctx as never);
    const earlier = {
      ...ctx,
      before: [
        { id: -2, author: "Ana", sent_at: "2025-12-30T10:00:00Z", time_confidence: "explicit_offset", body: "minus two" },
        { id: -1, author: "Ana", sent_at: "2025-12-31T10:00:00Z", time_confidence: "explicit_offset", body: "minus one" },
        { id: 0, author: "Ana", sent_at: "2026-01-01T10:00:00Z", time_confidence: "explicit_offset", body: "zero" },
        { id: 1, author: "Ana", sent_at: "2026-01-01T10:00:00Z", time_confidence: "explicit_offset", body: "one" },
      ],
      after: [],
    };
    const merged = mergeContextPages(entries, earlier as never);
    expect(merged.map((e) => e.id)).toEqual([-2, -1, 0, 1, 2, 3, 4]);
    expect(merged.filter((e) => e.current).map((e) => e.id)).toEqual([3]);
  });

  it("a multi-row later page stays oldest → newest at the back", () => {
    const entries = contextEntries(ctx as never);
    const later = {
      ...ctx,
      before: [],
      after: [
        { id: 4, author: "Bo", sent_at: "2026-01-03T10:00:00Z", time_confidence: "explicit_offset", body: "four" },
        { id: 5, author: "Bo", sent_at: "2026-01-04T10:00:00Z", time_confidence: "explicit_offset", body: "five" },
      ],
    };
    const merged = mergeContextPages(entries, later as never);
    expect(merged.map((e) => e.id)).toEqual([1, 2, 3, 4, 5]);
    expect(merged.filter((e) => e.current).map((e) => e.id)).toEqual([3]);
  });
});

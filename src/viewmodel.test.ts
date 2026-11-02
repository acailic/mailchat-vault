import { describe, expect, it } from "vitest";
import type { ArchiveStats, ChatContext, Message, SearchHit } from "./types";
import {
  attachmentOf,
  barHeightPct,
  dashboardSummary,
  dayBars,
  formatCount,
  groupHits,
  highlightSegments,
  mediaOf,
  moveIndex,
  neighborId,
  pageModel,
  parseSnippet,
  splitTerms,
  threadEntries,
} from "./viewmodel";

function hit(id: number, source: string, chatId: string, body = "x"): SearchHit {
  const m: Message = {
    id,
    source,
    external_id: `e${id}`,
    channel: "chat",
    chat_id: chatId,
    author: "a",
    sent_at: "2026-01-01T00:00:00Z",
    body,
    raw_json: "{}",
  };
  return { message: m, snippet: null };
}

describe("splitTerms", () => {
  it("splits on whitespace and drops empties", () => {
    expect(splitTerms("  quarterly   vault\treport ")).toEqual([
      "quarterly",
      "vault",
      "report",
    ]);
  });

  it("empty input yields no terms", () => {
    expect(splitTerms("")).toEqual([]);
    expect(splitTerms("   ")).toEqual([]);
  });
});

describe("parseSnippet", () => {
  it("splits backend markers into hit and plain segments", () => {
    expect(parseSnippet("pre \x01vault\x02 post")).toEqual([
      { text: "pre ", hit: false },
      { text: "vault", hit: true },
      { text: " post", hit: false },
    ]);
  });

  it("handles multiple and adjacent matches", () => {
    expect(parseSnippet("\x01a\x02 mid \x01b\x02")).toEqual([
      { text: "a", hit: true },
      { text: " mid ", hit: false },
      { text: "b", hit: true },
    ]);
  });

  it("handles marker-free text and stray close markers", () => {
    expect(parseSnippet("plain")).toEqual([{ text: "plain", hit: false }]);
    expect(parseSnippet("weird\x02close")).toEqual([
      { text: "weird", hit: false },
      { text: "close", hit: false },
    ]);
  });
});

describe("highlightSegments", () => {
  it("highlights whole words case-insensitively", () => {
    expect(highlightSegments("The Vault and VAULT, vaults", ["vault"])).toEqual([
      { text: "The ", hit: false },
      { text: "Vault", hit: true },
      { text: " and ", hit: false },
      { text: "VAULT", hit: true },
      { text: ", vaults", hit: false },
    ]);
  });

  it("highlights multiple terms and leaves non-matching text alone", () => {
    const out = highlightSegments("quarterly vault report", ["vault", "report"]);
    expect(out).toEqual([
      { text: "quarterly ", hit: false },
      { text: "vault", hit: true },
      { text: " ", hit: false },
      { text: "report", hit: true },
    ]);
  });

  it("escapes regex metacharacters in terms", () => {
    expect(highlightSegments("a.b (x) *star*", ["a.b"])).toEqual([
      { text: "a.b", hit: true },
      { text: " (x) *star*", hit: false },
    ]);
  });

  it("no terms → single plain segment", () => {
    expect(highlightSegments("body", [])).toEqual([{ text: "body", hit: false }]);
  });

  it("unicode words respect boundaries", () => {
    expect(highlightSegments("krv i krvarenje", ["krv"])).toEqual([
      { text: "krv", hit: true },
      { text: " i krvarenje", hit: false },
    ]);
  });
});

describe("groupHits", () => {
  it("merges all hits of a thread into one group, ordered by first hit", () => {
    const groups = groupHits([
      hit(1, "mbox", "thread-a"),
      hit(2, "mbox", "thread-a"),
      hit(3, "whatsapp", "family"),
      hit(4, "mbox", "thread-a"),
    ]);
    expect(groups.map((g) => [g.key, g.hits.length])).toEqual([
      ["mbox/thread-a", 3],
      ["whatsapp/family", 1],
    ]);
    expect(groups[0].hits.map((h) => h.message.id)).toEqual([1, 2, 4]);
  });

  it("labels chats without a thread id", () => {
    const groups = groupHits([hit(1, "demo", "")]);
    expect(groups[0].label).toBe("demo · no thread");
    expect(groupHits([hit(1, "mbox", "t")])[0].label).toBe("mbox · t");
  });

  it("keeps first-seen group order when chats interleave", () => {
    const groups = groupHits([
      hit(1, "telegram", "devs"),
      hit(2, "mbox", "a"),
      hit(3, "telegram", "devs"),
      hit(4, "whatsapp", ""),
      hit(5, "mbox", "a"),
      hit(6, "telegram", "devs"),
    ]);
    expect(groups.map((g) => g.key)).toEqual(["telegram/devs", "mbox/a", "whatsapp/"]);
    expect(groups.map((g) => g.hits.length)).toEqual([3, 2, 1]);
    expect(groups[0].hits.map((h) => h.message.id)).toEqual([1, 3, 6]);
  });
});

describe("pageModel", () => {
  it("computes range, page numbers and button states", () => {
    expect(pageModel(1234, 0, 50)).toEqual({
      total: 1234,
      from: 1,
      to: 50,
      page: 1,
      pageCount: 25,
      hasPrev: false,
      hasNext: true,
    });
    expect(pageModel(1234, 50, 50)).toMatchObject({ from: 51, to: 100, page: 2, hasPrev: true, hasNext: true });
    expect(pageModel(1234, 1200, 50)).toMatchObject({ from: 1201, to: 1234, page: 25, hasNext: false });
  });

  it("empty totals", () => {
    expect(pageModel(0, 0, 50)).toMatchObject({ from: 0, to: 0, page: 1, pageCount: 0, hasPrev: false, hasNext: false });
  });
});

describe("moveIndex", () => {
  it("clamps within the list", () => {
    expect(moveIndex(-1, 5, 1)).toBe(0);
    expect(moveIndex(0, 5, -1)).toBe(0);
    expect(moveIndex(4, 5, 1)).toBe(4);
    expect(moveIndex(2, 5, -1)).toBe(1);
  });

  it("empty list stays -1", () => {
    expect(moveIndex(3, 0, 1)).toBe(-1);
  });

  it("stepping backwards from no selection lands on the first row", () => {
    expect(moveIndex(-1, 3, -1)).toBe(0);
  });
});

describe("threadEntries / detail model", () => {
  const ctx: ChatContext = {
    before: [hit(1, "m", "c").message, hit(2, "m", "c").message],
    message: hit(3, "m", "c").message,
    after: [hit(4, "m", "c").message],
  };

  it("flattens oldest → newest with the current row marked", () => {
    const entries = threadEntries(ctx);
    expect(entries.map((e) => [e.message.id, e.current])).toEqual([
      [1, false],
      [2, false],
      [3, true],
      [4, false],
    ]);
  });

  it("handles a solo message with no neighbors", () => {
    const solo: ChatContext = {
      before: [],
      message: hit(9, "m", "c").message,
      after: [],
    };
    expect(threadEntries(solo)).toEqual([{ message: solo.message, current: true }]);
  });
});

describe("neighborId", () => {
  const ctx: ChatContext = {
    before: [hit(1, "m", "c").message, hit(2, "m", "c").message],
    message: hit(3, "m", "c").message,
    after: [hit(4, "m", "c").message, hit(5, "m", "c").message],
  };

  it("steps to the previous and next thread neighbor", () => {
    expect(neighborId(ctx, -1)).toBe(2);
    expect(neighborId(ctx, 1)).toBe(4);
  });

  it("returns null at the thread edges", () => {
    const topEdge: ChatContext = {
      before: [],
      message: hit(9, "m", "c").message,
      after: [hit(10, "m", "c").message],
    };
    const bottomEdge: ChatContext = {
      before: [hit(8, "m", "c").message],
      message: hit(9, "m", "c").message,
      after: [],
    };
    expect(neighborId(topEdge, -1)).toBeNull();
    expect(neighborId(bottomEdge, 1)).toBeNull();
    // Edges still allow stepping back inward.
    expect(neighborId(topEdge, 1)).toBe(10);
    expect(neighborId(bottomEdge, -1)).toBe(8);
  });
});

// ---------------------------------------------------------------------------
// Media links & attachment placeholders (raw_json conventions)
// ---------------------------------------------------------------------------

describe("mediaOf / attachmentOf", () => {
  const rawJson = JSON.stringify({
    attachment: { kind: "file", name: "IMG-1.jpg" },
    media: [
      { sha256: "a".repeat(64), path: `media/${"a".repeat(64)}.jpg`, name: "IMG-1.jpg", kind: "image", size: 12 },
    ],
  });

  it("mediaOf returns the linked files", () => {
    const media = mediaOf(rawJson);
    expect(media.length).toBe(1);
    expect(media[0]).toEqual({
      sha256: "a".repeat(64),
      path: `media/${"a".repeat(64)}.jpg`,
      name: "IMG-1.jpg",
      kind: "image",
      size: 12,
    });
  });

  it("attachmentOf returns kind and optional name", () => {
    expect(attachmentOf(rawJson)).toEqual({ kind: "file", name: "IMG-1.jpg" });
    expect(attachmentOf(JSON.stringify({ attachment: { kind: "media" } }))).toEqual({ kind: "media" });
    expect(attachmentOf("{}")).toBeNull();
  });

  it("degrades on missing media, non-arrays, and broken JSON", () => {
    expect(mediaOf("{}")).toEqual([]);
    expect(mediaOf(JSON.stringify({ media: "nope" }))).toEqual([]);
    expect(mediaOf("{broken")).toEqual([]);
    expect(attachmentOf("{broken")).toBeNull();
    // Telegram raw_json keeps the whole original message object.
    const tg = JSON.stringify({ photo: "photos/p.jpg", text: "" });
    expect(mediaOf(tg)).toEqual([]);
    expect(attachmentOf(tg)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Statistics dashboard
// ---------------------------------------------------------------------------

function day(dayStr: string, count: number) {
  return { day: dayStr, count };
}

describe("dayBars", () => {
  it("keeps daily bars while they fit", () => {
    const days = [day("2026-01-01", 3), day("2026-01-02", 4)];
    expect(dayBars(days, 10)).toEqual([
      { label: "2026-01-01", count: 3 },
      { label: "2026-01-02", count: 4 },
    ]);
  });

  it("merges into month buckets when days exceed the bar budget", () => {
    const days = [
      day("2026-01-01", 1),
      day("2026-01-31", 2),
      day("2026-02-01", 3),
      day("2026-02-28", 4),
    ];
    expect(dayBars(days, 2)).toEqual([
      { label: "2026-01", count: 3 },
      { label: "2026-02", count: 7 },
    ]);
  });

  it("falls back to year buckets when months still exceed the budget", () => {
    const days: Array<{ day: string; count: number }> = [];
    for (let m = 1; m <= 12; m++) {
      for (let d = 1; d <= 28; d++) {
        days.push(day(`2025-${String(m).padStart(2, "0")}-${String(d).padStart(2, "0")}`, 1));
      }
      days.push(day(`2026-${String(m).padStart(2, "0")}-15`, 2));
    }
    const bars = dayBars(days, 6);
    expect(bars).toEqual([
      { label: "2025", count: 336 },
      { label: "2026", count: 24 },
    ]);
  });

  it("preserves ascending order and never emits empty buckets", () => {
    const days = [day("2024-12-30", 1), day("2024-12-31", 2), day("2025-01-01", 5)];
    const bars = dayBars(days, 2);
    expect(bars).toEqual([
      { label: "2024-12", count: 3 },
      { label: "2025-01", count: 5 },
    ]);
  });

  it("falls through to years when even months overflow the budget", () => {
    const days = [day("2024-12-31", 1), day("2026-01-01", 5)];
    expect(dayBars(days, 1).map((b) => b.label)).toEqual(["2024", "2026"]);
  });
});

describe("barHeightPct", () => {
  it("scales counts against the maximum", () => {
    expect(barHeightPct(50, 100)).toBe(50);
    expect(barHeightPct(100, 100)).toBe(100);
    expect(barHeightPct(0, 100)).toBe(0);
  });

  it("never renders an invisible bar for a nonzero count", () => {
    expect(barHeightPct(1, 1_000_000)).toBe(1);
  });

  it("degrades to zero without a positive maximum", () => {
    expect(barHeightPct(5, 0)).toBe(0);
  });
});

describe("formatCount", () => {
  it("groups thousands deterministically", () => {
    expect(formatCount(0)).toBe("0");
    expect(formatCount(999)).toBe("999");
    expect(formatCount(1234567)).toBe("1,234,567");
  });
});

const DASH: ArchiveStats = {
  total: 1_234_567,
  first_sent_at: "2018-01-08T08:00:00Z",
  last_sent_at: "2026-11-01T23:59:59Z",
  by_source: [
    { source: "mbox", count: 700_000 },
    { source: "whatsapp", count: 534_567 },
  ],
  by_day: [day("2018-01-08", 12), day("2026-11-01", 30)],
  top_chats: [{ source: "whatsapp", chat_id: "family", count: 100_000 }],
  top_authors: [{ author: "ana", count: 90_000 }],
};

describe("dashboardSummary", () => {
  it("summarizes total, sources, and the date range", () => {
    expect(dashboardSummary(DASH)).toBe(
      "1,234,567 messages · 2 sources · 2018-01-08 → 2026-11-01",
    );
  });

  it("an empty archive is a single word", () => {
    expect(dashboardSummary({ ...DASH, total: 0 })).toBe("empty archive");
  });

  it("handles undated archives and singular sources", () => {
    expect(
      dashboardSummary({
        ...DASH,
        total: 5,
        first_sent_at: null,
        last_sent_at: null,
        by_source: [{ source: "mbox", count: 5 }],
      }),
    ).toBe("5 messages · 1 source · no dates");
  });
});

// ---------------------------------------------------------------------------
// UI-state snapshots (view models rendered to plain JSON)
// ---------------------------------------------------------------------------

describe("UI state snapshots", () => {
  it("grouped results page", () => {
    const hits = [
      hit(1, "mbox", "thread-a", "quarterly vault"),
      hit(2, "mbox", "thread-a", "vault again"),
      hit(3, "whatsapp", "family", "vault keys"),
    ];
    const model = {
      groups: groupHits(hits).map((g) => ({ label: g.label, count: g.hits.length })),
      page: pageModel(1234, 0, 50),
    };
    expect(model).toMatchSnapshot();
  });

  it("highlighted detail body segments", () => {
    const segments = highlightSegments(
      "The quarterly vault report mentions the vault budget.",
      ["vault"],
    );
    expect(segments).toMatchSnapshot();
  });
});

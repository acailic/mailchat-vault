import { describe, expect, it } from "vitest";
import type { ChatContext, Message, SearchHit } from "./types";
import {
  groupHits,
  highlightSegments,
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

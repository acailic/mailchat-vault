// @vitest-environment happy-dom
// DOM snapshot tests of UI state: the rendered results list, pagination,
// source chips, and detail panel (built via render.ts, which only uses
// textContent — reading innerHTML back is safe for snapshots).
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  renderDashboard,
  renderDetail,
  renderPagination,
  renderResults,
  renderSourceChips,
} from "./render";
import type { ArchiveStats, ChatContext, Message, SearchHit, SourceCount } from "./types";
import { groupHits, pageModel } from "./viewmodel";

// Snapshots must not depend on the machine timezone: render the raw
// timestamp instead of the local-time formatter (formatSentAt itself is
// covered by format.test.ts).
vi.mock("./format", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./format")>();
  return {
    ...actual,
    formatSentAt: (iso: string) => iso || "unknown time",
  };
});

function message(id: number, source: string, chatId: string, body: string, author = "ana"): Message {
  return {
    id,
    source,
    external_id: `e${id}`,
    channel: source === "mbox" ? "email" : "chat",
    chat_id: chatId,
    author,
    sent_at: `2026-01-0${id}T10:00:00Z`,
    body,
    raw_json: "{}",
  };
}

function hit(m: Message, snippet: string | null): SearchHit {
  return { message: m, snippet };
}

beforeEach(() => {
  document.replaceChildren(document.createElement("main"));
});

describe("results list", () => {
  it("renders grouped hits with highlighted snippets and active row", () => {
    const hits = [
      hit(message(1, "mbox", "thread-a", "full body one"), "pre \x01vault\x02 post"),
      hit(message(2, "mbox", "thread-a", "full body two"), "\x01vault\x02 only"),
      hit(message(3, "whatsapp", "", "browse body"), null),
    ];
    const list = document.createElement("ul");
    renderResults(list, {
      groups: groupHits(hits),
      activeIndex: 1,
      terms: ["vault"],
      onOpen: () => {},
    });
    expect(list.innerHTML).toMatchSnapshot();
    // Active row is marked with aria-current.
    expect(list.querySelector(".hit-item.active")?.getAttribute("aria-current")).toBe("true");
    expect(list.querySelectorAll(".hit-item").length).toBe(3);
    // Snippet markers become <mark class="hit"> runs.
    expect(list.querySelectorAll("mark.hit").length).toBe(2);
  });

  it("highlights query terms in browse mode bodies", () => {
    const hits = [hit(message(1, "demo", "chat", "the Vault body text"), null)];
    const list = document.createElement("ul");
    renderResults(list, {
      groups: groupHits(hits),
      activeIndex: -1,
      terms: ["vault"],
      onOpen: () => {},
    });
    expect(list.querySelector("mark.hit")?.textContent).toBe("Vault");
  });

  it("never interpolates message content as HTML", () => {
    const evil = message(1, "mbox", "<script>x</script>", '<img src=x onerror="alert(1)">');
    const list = document.createElement("ul");
    renderResults(list, {
      groups: groupHits([hit(evil, "ok \x01body\x02")]),
      activeIndex: -1,
      terms: [],
      onOpen: () => {},
    });
    expect(list.querySelectorAll("script,img").length).toBe(0);
    expect(list.innerHTML.includes("<img")).toBe(false);
  });

  it("opens a hit on click", () => {
    let opened = -1;
    const hits = [hit(message(7, "mbox", "t", "b"), null)];
    const list = document.createElement("ul");
    renderResults(list, {
      groups: groupHits(hits),
      activeIndex: -1,
      terms: [],
      onOpen: (id) => (opened = id),
    });
    (list.querySelector(".hit-item") as HTMLElement).click();
    expect(opened).toBe(7);
  });

  it("renders an empty state with no groups or rows", () => {
    const list = document.createElement("ul");
    renderResults(list, {
      groups: [],
      activeIndex: -1,
      terms: [],
      onOpen: () => {},
    });
    expect(list.innerHTML).toBe("");
    expect(list.querySelectorAll(".group,.hit-item").length).toBe(0);
  });

  it("replaces previously rendered rows on re-render (no stale rows)", () => {
    const list = document.createElement("ul");
    const first = [hit(message(1, "mbox", "t", "one"), null)];
    renderResults(list, {
      groups: groupHits(first),
      activeIndex: -1,
      terms: [],
      onOpen: () => {},
    });
    const second = [hit(message(2, "mbox", "t", "two"), null)];
    renderResults(list, {
      groups: groupHits(second),
      activeIndex: -1,
      terms: [],
      onOpen: () => {},
    });
    expect(list.querySelectorAll(".hit-item").length).toBe(1);
    expect(list.querySelector<HTMLElement>(".hit-item")?.dataset.id).toBe("2");
  });

  it("marks every hit with a stable flat index across groups", () => {
    const hits = [
      hit(message(1, "mbox", "a", "x"), null),
      hit(message(2, "mbox", "a", "y"), null),
      hit(message(3, "demo", "", "z"), null),
    ];
    const list = document.createElement("ul");
    renderResults(list, {
      groups: groupHits(hits),
      activeIndex: -1,
      terms: [],
      onOpen: () => {},
    });
    expect([...list.querySelectorAll<HTMLElement>(".hit-item")].map((li) => li.dataset.index)).toEqual([
      "0",
      "1",
      "2",
    ]);
  });
});

describe("pagination", () => {
  it("renders range info and enables the right buttons", () => {
    const el = document.createElement("div");
    renderPagination(el, pageModel(1234, 50, 50), { onPrev: () => {}, onNext: () => {} });
    expect(el.innerHTML).toMatchSnapshot();
    expect((el.querySelector(".page-btn") as HTMLButtonElement).disabled).toBe(false);
    expect((el.querySelectorAll(".page-btn")[1] as HTMLButtonElement).disabled).toBe(false);
  });

  it("empty results disable both buttons", () => {
    const el = document.createElement("div");
    renderPagination(el, pageModel(0, 0, 50), { onPrev: () => {}, onNext: () => {} });
    expect(el.textContent).toContain("0 results");
    expect(el.querySelectorAll(".page-btn:disabled").length).toBe(2);
  });

  it("last page keeps prev enabled and disables next (snapshot)", () => {
    const el = document.createElement("div");
    renderPagination(el, pageModel(120, 100, 50), { onPrev: () => {}, onNext: () => {} });
    expect(el.innerHTML).toMatchSnapshot();
    const [prev, next] = [...el.querySelectorAll(".page-btn")] as HTMLButtonElement[];
    expect(prev.disabled).toBe(false);
    expect(next.disabled).toBe(true);
  });

  it("fires prev/next handlers", () => {
    const fired: string[] = [];
    const el = document.createElement("div");
    renderPagination(el, pageModel(200, 50, 50), {
      onPrev: () => fired.push("prev"),
      onNext: () => fired.push("next"),
    });
    (el.querySelectorAll(".page-btn")[0] as HTMLButtonElement).click();
    (el.querySelectorAll(".page-btn")[1] as HTMLButtonElement).click();
    expect(fired).toEqual(["prev", "next"]);
  });
});

describe("source chips", () => {
  it("renders toggles with counts and pressed state", () => {
    const available: SourceCount[] = [
      { source: "mbox", count: 900 },
      { source: "whatsapp", count: 80 },
    ];
    const el = document.createElement("div");
    renderSourceChips(el, { available, selected: ["whatsapp"], onToggle: () => {} });
    expect(el.innerHTML).toMatchSnapshot();
    expect(el.querySelector('[data-source="whatsapp"]')?.getAttribute("aria-pressed")).toBe("true");
  });

  it("toggling fires with the source name", () => {
    let toggled = "";
    const el = document.createElement("div");
    renderSourceChips(el, {
      available: [{ source: "mbox", count: 1 }],
      selected: [],
      onToggle: (s) => (toggled = s),
    });
    (el.querySelector(".chip") as HTMLElement).click();
    expect(toggled).toBe("mbox");
  });

  it("renders all chips unselected by default (snapshot)", () => {
    const el = document.createElement("div");
    renderSourceChips(el, {
      available: [
        { source: "mbox", count: 900 },
        { source: "telegram", count: 80 },
      ],
      selected: [],
      onToggle: () => {},
    });
    expect(el.innerHTML).toMatchSnapshot();
    expect(el.querySelectorAll(".chip.on").length).toBe(0);
  });
});

describe("detail panel", () => {
  const ctx: ChatContext = {
    before: [message(1, "telegram", "devs", "earlier message about the vault")],
    message: message(2, "telegram", "devs", "The Vault body with Vault twice."),
    after: [message(3, "telegram", "devs", "later message")],
  };

  it("renders meta, full highlighted body and clickable neighbors", () => {
    const el = document.createElement("aside");
    renderDetail(el, {
      ctx,
      queryText: "vault",
      onOpenMessage: () => {},
      onClose: () => {},
    });
    expect(el.innerHTML).toMatchSnapshot();
    expect(el.querySelectorAll(".thread-row").length).toBe(3);
    expect((el.querySelector(".thread-row.current") as HTMLElement).dataset.id).toBe("2");
    // Current row body keeps every occurrence highlighted.
    expect(el.querySelectorAll(".thread-row.current mark.hit").length).toBe(2);
    // Neighbors are links; current is not.
    expect(el.querySelectorAll(".thread-row.link").length).toBe(2);
  });

  it("renders media links and unresolved attachment placeholders", () => {
    const rawJson = JSON.stringify({
      attachment: { kind: "file", name: "IMG-20261101-WA0001.jpg" },
      media: [
        {
          sha256: "a".repeat(64),
          path: `media/${"a".repeat(64)}.jpg`,
          name: "IMG-20261101-WA0001.jpg",
          kind: "image",
          size: 2048,
        },
        {
          sha256: "b".repeat(64),
          path: `media/${"b".repeat(64)}.pdf`,
          name: "plan.pdf",
          kind: "document",
          size: 5 * 1024 * 1024,
        },
      ],
    });
    const withMedia: ChatContext = {
      before: [],
      message: { ...message(4, "whatsapp", "family", "IMG-20261101-WA0001.jpg"), raw_json: rawJson },
      after: [],
    };
    const el = document.createElement("aside");
    renderDetail(el, {
      ctx: withMedia,
      queryText: "",
      onOpenMessage: () => {},
      onClose: () => {},
    });
    expect(el.innerHTML).toMatchSnapshot();
    const items = [...el.querySelectorAll(".media-item")];
    expect(items.length).toBe(2);
    expect(items[0].querySelector(".media-kind")!.textContent).toBe("image");
    expect(items[0].querySelector(".media-name")!.textContent).toBe("IMG-20261101-WA0001.jpg");
    expect(items[0].textContent).toContain("2 KB");
    expect(items[0].textContent).toContain(`media/${"a".repeat(64)}.jpg`);
    expect(items[1].textContent).toContain("5 MB");

    // An attachment with a name that was never extracted shows as
    // unresolved; a matching media entry suppresses the placeholder.
    const unresolved: ChatContext = {
      before: [],
      message: {
        ...message(5, "whatsapp", "family", "video omitted"),
        raw_json: JSON.stringify({ attachment: { kind: "video", name: "VID-1.mp4" } }),
      },
      after: [],
    };
    const el2 = document.createElement("aside");
    renderDetail(el2, {
      ctx: unresolved,
      queryText: "",
      onOpenMessage: () => {},
      onClose: () => {},
    });
    const unresolvedItems = el2.querySelectorAll(".media-item.unresolved");
    expect(unresolvedItems.length).toBe(1);
    expect(unresolvedItems[0].textContent).toContain("VID-1.mp4");
    expect(unresolvedItems[0].textContent).toContain("not in export");
  });

  it("renders raw_json-free meta and never executes message content", () => {
    const evil: ChatContext = {
      before: [],
      message: message(1, "mbox", "x", '<img src=x onerror="alert(1)">', '<b>"evil"</b>'),
      after: [],
    };
    const el = document.createElement("aside");
    renderDetail(el, {
      ctx: evil,
      queryText: "",
      onOpenMessage: () => {},
      onClose: () => {},
    });
    expect(el.querySelectorAll("img,b").length).toBe(0);
  });

  it("close button and neighbor clicks fire callbacks", () => {
    let closed = false;
    let opened = -1;
    const el = document.createElement("aside");
    renderDetail(el, {
      ctx,
      queryText: "",
      onOpenMessage: (id) => (opened = id),
      onClose: () => (closed = true),
    });
    (el.querySelector(".close-btn") as HTMLElement).click();
    expect(closed).toBe(true);
    (el.querySelector('.thread-row[data-id="3"]') as HTMLElement).click();
    expect(opened).toBe(3);
  });

  it("renders a solo message with no neighbors (snapshot)", () => {
    const solo: ChatContext = {
      before: [],
      message: message(9, "telegram", "devs", "only message in thread"),
      after: [],
    };
    const el = document.createElement("aside");
    renderDetail(el, {
      ctx: solo,
      queryText: "message",
      onOpenMessage: () => {},
      onClose: () => {},
    });
    expect(el.innerHTML).toMatchSnapshot();
    expect(el.querySelectorAll(".thread-row").length).toBe(1);
    expect(el.querySelector(".thread-row.current")).not.toBeNull();
    expect(el.querySelectorAll(".thread-row.link").length).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// Statistics dashboard
// ---------------------------------------------------------------------------

function archiveStats(over: Partial<ArchiveStats> = {}): ArchiveStats {
  return {
    total: 1234,
    first_sent_at: "2026-01-01T08:00:00Z",
    last_sent_at: "2026-03-31T22:00:00Z",
    by_source: [
      { source: "mbox", count: 900 },
      { source: "whatsapp", count: 334 },
    ],
    by_day: [
      { day: "2026-01-01", count: 10 },
      { day: "2026-02-01", count: 20 },
      { day: "2026-03-31", count: 5 },
    ],
    top_chats: [
      { source: "whatsapp", chat_id: "family", count: 300 },
      { source: "mbox", chat_id: "thread-a", count: 90 },
    ],
    top_authors: [
      { author: "Ana <ana@example.com>", count: 500 },
      { author: "", count: 12 },
    ],
    ...over,
  };
}

describe("dashboard", () => {
  it("renders summary, day bars, and count tables (snapshot)", () => {
    const el = document.createElement("div");
    renderDashboard(el, { stats: archiveStats() });
    expect(el.innerHTML).toMatchSnapshot();
    expect(el.querySelector("p.meta")?.textContent).toBe(
      "1,234 messages · 2 sources · 2026-01-01 → 2026-03-31",
    );
    expect(el.querySelectorAll(".day-bar").length).toBe(3);
    // Bar heights scale against the tallest day (20).
    expect((el.querySelectorAll(".day-bar")[0] as HTMLElement).style.height).toBe("50%");
    expect((el.querySelectorAll(".day-bar")[1] as HTMLElement).style.height).toBe("100%");
    expect((el.querySelectorAll(".day-bar")[2] as HTMLElement).style.height).toBe("25%");
    expect(el.querySelectorAll(".stat-table").length).toBe(3);
    expect(el.querySelectorAll(".stat-table tbody tr").length).toBe(6);
    // The unknown author gets a readable label.
    const authorRows = [...el.querySelectorAll(".stat-table")[2].querySelectorAll("td.label")];
    expect(authorRows.map((td) => td.textContent)).toEqual([
      "Ana <ana@example.com>",
      "(unknown)",
    ]);
  });

  it("an empty archive renders the summary only", () => {
    const el = document.createElement("div");
    renderDashboard(el, {
      stats: archiveStats({
        total: 0,
        first_sent_at: null,
        last_sent_at: null,
        by_source: [],
        by_day: [],
        top_chats: [],
        top_authors: [],
      }),
    });
    expect(el.textContent).toBe("empty archive");
    expect(el.querySelectorAll(".day-chart,.stat-table").length).toBe(0);
  });

  it("never interpolates archive content as HTML", () => {
    const el = document.createElement("div");
    renderDashboard(el, {
      stats: archiveStats({
        by_source: [{ source: '<script>alert(1)</script>', count: 1 }],
        top_chats: [
          { source: "wa", chat_id: '<img src=x onerror="alert(1)">', count: 1 },
        ],
        top_authors: [{ author: "<b>bold</b>", count: 1 }],
      }),
    });
    expect(el.querySelectorAll("script,img,b").length).toBe(0);
    // The hostile strings survive only as escaped text/attribute content,
    // never as elements (title attributes may legitimately contain the
    // literal characters — they are not markup).
    expect(el.querySelector("td.label")?.textContent).toBe("<script>alert(1)</script>");
  });

  it("downsamples long day series into month buckets", () => {
    const by_day = Array.from({ length: 200 }, (_, i) => ({
      day: `2026-${String((i % 12) + 1).padStart(2, "0")}-15`,
      count: 1,
    }));
    const el = document.createElement("div");
    renderDashboard(el, { stats: archiveStats({ by_day }) });
    expect(el.querySelectorAll(".day-bar").length).toBe(12); // month buckets
  });
});

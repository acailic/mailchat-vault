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
import type { SourceChipRow } from "./accountsview";
import type { ArchiveStats, ChatContext, Message, SearchHit } from "./types";
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
    const opened: Array<[number, number]> = [];
    const hits = [hit(message(7, "mbox", "t", "b"), null)];
    const list = document.createElement("ul");
    renderResults(list, {
      groups: groupHits(hits),
      activeIndex: -1,
      terms: [],
      onOpen: (index, id) => opened.push([index, id]),
    });
    (list.querySelector(".hit-item") as HTMLElement).click();
    // The row passes its flat index with the id: main.ts sets activeIndex
    // from it so Back/close can restore focus to the initiating row.
    expect(opened).toEqual([[0, 7]]);
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
    const rows: SourceChipRow[] = [
      { id: "inst-mbox", label: "mbox", count: 900, icon: "file" },
      { id: "inst-whatsapp", label: "whatsapp", count: 80, icon: "chat" },
    ];
    const el = document.createElement("div");
    renderSourceChips(el, { rows, selected: ["inst-whatsapp"], onToggle: () => {} });
    expect(el.innerHTML).toMatchSnapshot();
    expect(el.querySelector('[data-source="inst-whatsapp"]')?.getAttribute("aria-pressed")).toBe("true");
  });

  it("leads each chip with its taxonomy icon without touching the text", () => {
    // The mail-vs-chat axis is visible on the chip itself; the icon is
    // decorative (aria-hidden svg), so the chip's accessible text — and
    // every text-based assertion — is unchanged.
    const el = document.createElement("div");
    renderSourceChips(el, {
      rows: [
        { id: "inst-imap", label: "imap.gmail.com-ana", count: 12, icon: "mail" },
        { id: "inst-whatsapp", label: "whatsapp", count: 80, icon: "chat" },
      ],
      selected: [],
      onToggle: () => {},
    });
    const chips = [...el.querySelectorAll(".chip")];
    expect(chips.map((c) => c.querySelector("use")?.getAttribute("href"))).toEqual([
      "#i-mail",
      "#i-chat",
    ]);
    expect(chips.map((c) => c.querySelector("svg")?.getAttribute("aria-hidden"))).toEqual([
      "true",
      "true",
    ]);
    expect(chips.map((c) => c.textContent)).toEqual([
      "imap.gmail.com-ana (12)",
      "whatsapp (80)",
    ]);
  });

  it("toggling fires with the source instance id", () => {
    let toggled = "";
    const el = document.createElement("div");
    renderSourceChips(el, {
      rows: [{ id: "inst-mbox", label: "mbox", count: 1, icon: "file" }],
      selected: [],
      onToggle: (id) => (toggled = id),
    });
    (el.querySelector(".chip") as HTMLElement).click();
    expect(toggled).toBe("inst-mbox");
  });

  it("renders all chips unselected by default (snapshot)", () => {
    const el = document.createElement("div");
    renderSourceChips(el, {
      rows: [
        { id: "inst-mbox", label: "mbox", count: 900, icon: "file" },
        { id: "inst-telegram", label: "telegram", count: 80, icon: "chat" },
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
      onExportChat: () => {},
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
      onExportChat: () => {},
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
      onExportChat: () => {},
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
      onExportChat: () => {},
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
      onExportChat: () => {},
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
      onExportChat: () => {},
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
    // Bar heights scale against the tallest day (20)…
    expect((el.querySelectorAll(".day-bar")[0] as HTMLElement).style.height).toBe("50%");
    expect((el.querySelectorAll(".day-bar")[1] as HTMLElement).style.height).toBe("100%");
    expect((el.querySelectorAll(".day-bar")[2] as HTMLElement).style.height).toBe("25%");
    // …and the same share feeds the color ramp via --bar-heat.
    expect(
      (el.querySelectorAll(".day-bar")[0] as HTMLElement).style.getPropertyValue("--bar-heat"),
    ).toBe("50");
    expect(
      (el.querySelectorAll(".day-bar")[1] as HTMLElement).style.getPropertyValue("--bar-heat"),
    ).toBe("100");
    // Time axis under the histogram: first, middle, and last bucket.
    expect(
      [...el.querySelectorAll(".day-chart-axis span")].map((s) => s.textContent),
    ).toEqual(["2026-01-01", "2026-02-01", "2026-03-31"]);
    expect(el.querySelectorAll(".stat-table").length).toBe(3);
    expect(el.querySelectorAll(".stat-table tbody tr").length).toBe(6);
    // The author shows its display name; the full address stays in the
    // tooltip; the unknown author gets a readable label.
    const authorRows = [...el.querySelectorAll(".stat-table")[2].querySelectorAll("td.label")];
    expect(authorRows.map((td) => td.textContent)).toEqual([
      "Ana",
      "(unknown)",
    ]);
    expect(authorRows[0].getAttribute("title")).toBe("Ana <ana@example.com>");
  });

  it("labels every day bar with its count for eyes and machines", () => {
    const el = document.createElement("div");
    renderDashboard(el, { stats: archiveStats() });
    const bars = [...el.querySelectorAll(".day-bar")];
    // Machine-readable label (aria-label) and tooltip per bar.
    expect(bars.map((b) => b.getAttribute("aria-label"))).toEqual([
      "2026-01-01: 10 messages",
      "2026-02-01: 20 messages",
      "2026-03-31: 5 messages",
    ]);
    expect(bars[0].getAttribute("title")).toBe("2026-01-01: 10 messages");
    // Few enough bars: the count is visible text too.
    expect(bars.map((b) => b.querySelector(".day-bar-count")?.textContent)).toEqual([
      "10",
      "20",
      "5",
    ]);
  });

  it("a single day renders its count as plain text, not one full-width bar", () => {
    const el = document.createElement("div");
    renderDashboard(el, {
      stats: archiveStats({
        by_day: [{ day: "2026-11-01", count: 6 }],
      }),
    });
    expect(el.querySelectorAll(".day-bar").length).toBe(0);
    const chart = el.querySelector(".day-chart") as HTMLElement;
    expect(chart.classList.contains("single")).toBe(true);
    const note = chart.querySelector(".day-single");
    expect(note?.textContent).toBe("2026-11-01: 6 messages");
    expect(chart.getAttribute("aria-label")).toBe(
      "Messages per day — 2026-11-01: 6 messages",
    );
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

  it("merges quoted and unquoted spellings of one author into one row", () => {
    const el = document.createElement("div");
    renderDashboard(el, {
      stats: archiveStats({
        top_authors: [
          { author: '"Aleksandar Ilic" <a@b.c>', count: 759 },
          { author: "Aleksandar Ilic <a@b.c>", count: 272 },
          { author: "Ana <ana@example.com>", count: 500 },
        ],
      }),
    });
    const rows = [...el.querySelectorAll(".stat-table")[2].querySelectorAll("tbody tr")];
    // One person, one row, summed count — re-sorted so the merged row leads.
    expect(rows.map((tr) => tr.querySelector("td.label")?.textContent)).toEqual([
      "Aleksandar Ilic",
      "Ana",
    ]);
    expect(rows[0].querySelector("td.num")?.textContent).toBe("1,031");
    // The tooltip keeps both full identities of record.
    expect(rows[0].querySelector("td.label")?.getAttribute("title")).toBe(
      '"Aleksandar Ilic" <a@b.c>; Aleksandar Ilic <a@b.c>',
    );
    expect(rows[1].querySelector("td.label")?.getAttribute("title")).toBe(
      "Ana <ana@example.com>",
    );
  });

  it("sentence-cases mail folder names in Top chats and leaves chat names verbatim", () => {
    const el = document.createElement("div");
    renderDashboard(el, {
      stats: archiveStats({
        top_chats: [
          { source: "mbox", chat_id: "All mail Including Spam and Trash", count: 90 },
          { source: "whatsapp", chat_id: "Family Group", count: 300 },
        ],
      }),
    });
    const labels = [...el.querySelectorAll(".stat-table")[1].querySelectorAll("td.label")];
    // The mail folder label reads in sentence case; the user-chosen chat
    // name keeps its own casing. (Rows keep the backend's count order.)
    expect(labels.map((td) => td.textContent)).toEqual([
      "mbox · All mail including spam and trash",
      "whatsapp · Family Group",
    ]);
    // The raw stored value stays in the tooltip.
    expect(labels[0].getAttribute("title")).toBe(
      "mbox · All mail Including Spam and Trash",
    );
    expect(labels[1].getAttribute("title")).toBe("whatsapp · Family Group");
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
    // The axis follows the bucketing: month labels, no duplicates.
    expect(
      [...el.querySelectorAll(".day-chart-axis span")].map((s) => s.textContent),
    ).toEqual(["2026-01", "2026-06", "2026-12"]);
  });

  it("dense charts keep every bar machine-readable but drop the visible counts", () => {
    // Days spanning two years: 24 month buckets, too many for readable
    // visible labels (every count is 6 to keep the check uniform).
    const by_day = Array.from({ length: 700 }, (_, i) => ({
      day: `${i % 24 < 12 ? 2025 : 2026}-${String((i % 12) + 1).padStart(2, "0")}-15`,
      count: 6,
    }));
    const el = document.createElement("div");
    renderDashboard(el, { stats: archiveStats({ by_day }) });
    const bars = [...el.querySelectorAll(".day-bar")];
    expect(bars.length).toBe(24);
    expect(el.querySelectorAll(".day-bar-count").length).toBe(0);
    // Every bar still names its bucket and its summed count for machines
    // and tooltips ("2026-04: 30 messages").
    expect(
      bars.every((b) => /^\d{4}-\d{2}: [\d,]+ messages$/.test(b.getAttribute("aria-label") ?? "")),
    ).toBe(true);
    expect(bars.every((b) => b.getAttribute("title"))).toBe(true);
  });
});

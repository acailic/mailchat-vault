// @vitest-environment happy-dom
// DOM snapshot tests of UI state: the rendered results list, pagination,
// source chips, and detail panel (built via render.ts, which only uses
// textContent — reading innerHTML back is safe for snapshots).
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  renderDetail,
  renderPagination,
  renderResults,
  renderSourceChips,
} from "./render";
import type { ChatContext, Message, SearchHit, SourceCount } from "./types";
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
      hits,
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
      hits,
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
      hits: [hit(evil, "ok \x01body\x02")],
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
      hits,
      groups: groupHits(hits),
      activeIndex: -1,
      terms: [],
      onOpen: (id) => (opened = id),
    });
    (list.querySelector(".hit-item") as HTMLElement).click();
    expect(opened).toBe(7);
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
});

// @vitest-environment happy-dom
// Integration tests for the UI wiring in src/main.ts: the real index.html
// markup is booted against a mocked Tauri IPC bridge and driven with real
// DOM events (key presses, clicks, changes). Covers search triggering and
// the empty-query guard, loading/empty/error states, filter interactions,
// result grouping, pagination (including the stale-response guard), the
// detail view (open/close/thread walking), and keyboard navigation.
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { Window } from "happy-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatContext, Message, SearchHit, SearchPage, Stats } from "./types";

// Timestamps must render timezone-independently here (same approach as
// render.test.ts); formatSentAt itself is covered by format.test.ts.
vi.mock("./format", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./format")>();
  return {
    ...actual,
    formatSentAt: (iso: string) => iso || "unknown time",
  };
});

const tauri = vi.hoisted(() => ({
  calls: [] as Array<{ cmd: string; args: unknown }>,
  handlers: new Map<string, (args: unknown) => unknown>(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown): unknown => {
    tauri.calls.push({ cmd, args });
    const handler = tauri.handlers.get(cmd);
    if (!handler) return Promise.reject(new Error(`no mock handler for "${cmd}"`));
    return handler(args);
  },
}));

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const STATS: Stats = {
  total: 9,
  by_source: [
    { source: "mbox", count: 6 },
    { source: "demo", count: 3 },
  ],
};

function message(id: number, source: string, chatId: string, body: string, author = "ana"): Message {
  return {
    id,
    source,
    external_id: `e${id}`,
    channel: source === "mbox" ? "email" : "chat",
    chat_id: chatId,
    author,
    sent_at: `2026-01-${String(id).padStart(2, "0")}T10:00:00Z`,
    body,
    raw_json: "{}",
  };
}

function hitOf(m: Message, snippet: string | null = null): SearchHit {
  return { message: m, snippet };
}

function pageOf(hits: SearchHit[], total: number, offset = 0): SearchPage {
  return { hits, total, limit: 50, offset };
}

const HITS: SearchHit[] = [
  hitOf(message(1, "mbox", "thread-a", "quarterly vault report")),
  hitOf(message(2, "mbox", "thread-a", "vault again", "bruno")),
  hitOf(message(3, "demo", "", "vault keys")),
];

const CTX: ChatContext = {
  before: [message(10, "mbox", "thread-a", "earlier"), message(11, "mbox", "thread-a", "before current")],
  message: message(1, "mbox", "thread-a", "quarterly vault report"),
  after: [message(12, "mbox", "thread-a", "after current"), message(13, "mbox", "thread-a", "much later")],
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

// ---------------------------------------------------------------------------
// Harness: fresh window + real markup + module re-import per test
// ---------------------------------------------------------------------------

let win: Window;

/** Anything we can dispatch a happy-dom event on (element or document). */
interface DispatchTarget {
  dispatchEvent(event: object): boolean;
}

function $<T extends HTMLElement = HTMLElement>(sel: string): T {
  const el = win.document.querySelector(sel) as unknown as T | null;
  if (!el) throw new Error(`missing element ${sel}`);
  return el;
}

function input(sel: string): HTMLInputElement {
  return $<HTMLInputElement>(sel);
}

function pageButtons(): HTMLButtonElement[] {
  return [...win.document.querySelectorAll(".page-btn")] as unknown as HTMLButtonElement[];
}

function pressKey(target: DispatchTarget, key: string): void {
  target.dispatchEvent(
    new win.KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }),
  );
}

function fireChange(target: DispatchTarget): void {
  target.dispatchEvent(new win.Event("change", { bubbles: true }));
}

function searchCalls(): Array<{ cmd: string; args: unknown }> {
  return tauri.calls.filter((c) => c.cmd === "search_messages");
}

function ctxCalls(): Array<{ cmd: string; args: unknown }> {
  return tauri.calls.filter((c) => c.cmd === "chat_context");
}

function searchRespondsWith(respond: () => unknown): void {
  tauri.handlers.set("search_messages", respond);
}

function queueResponses(responses: unknown[]): void {
  const pending = [...responses];
  searchRespondsWith(() => {
    const next = pending.shift();
    if (next === undefined) throw new Error("unexpected extra search_messages call");
    return next;
  });
}

async function runQuery(text: string): Promise<void> {
  input("#search").value = text;
  pressKey(input("#search"), "Enter");
  await vi.waitFor(() => expect(searchCalls().length).toBeGreaterThan(0));
}

async function boot(): Promise<void> {
  win = new Window();
  vi.stubGlobal("window", win);
  vi.stubGlobal("document", win.document);
  // Note: the happy-dom environment replaces the global URL class, so the
  // index.html path is resolved through node:url instead of new URL().
  const indexHtml = join(dirname(fileURLToPath(import.meta.url)), "..", "index.html");
  const html = readFileSync(indexHtml, "utf8");
  const start = html.indexOf("<body>") + "<body>".length;
  const end = html.lastIndexOf("</body>");
  win.document.body.innerHTML = html
    .slice(start, end)
    .replace(/<script[\s\S]*?<\/script>/g, "");
  await import("./main");
  // Wait until the initial stats refresh settled (label replaced or cleared).
  await vi.waitFor(() => expect($("#stats").textContent).not.toBe("…"));
}

async function bootSearched(): Promise<void> {
  await boot();
  searchRespondsWith(() => pageOf(HITS, 3));
  tauri.handlers.set("chat_context", () => CTX);
  await runQuery("vault");
  await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
}

beforeEach(() => {
  vi.resetModules();
  tauri.calls.length = 0;
  tauri.handlers.clear();
  tauri.handlers.set("stats", () => STATS);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

// ---------------------------------------------------------------------------
// Search box
// ---------------------------------------------------------------------------

describe("search box", () => {
  it("boots with stats, chips, empty results and the empty-query prompt", async () => {
    await boot();
    expect($("#stats").textContent).toBe("9 messages (mbox: 6, demo: 3)");
    expect($("#status").textContent).toBe("Type something to search, or set a filter to browse.");
    expect($("#status").classList.contains("error")).toBe(false);
    expect([...win.document.querySelectorAll(".chip")].map((c) => c.textContent)).toEqual([
      "mbox (6)",
      "demo (3)",
    ]);
    expect($("#results").children.length).toBe(0);
    expect($(".page-info").textContent).toBe("0 results");
  });

  it("an empty or whitespace-only query triggers no search at all", async () => {
    await boot();
    input("#search").value = "   ";
    pressKey(input("#search"), "Enter");
    await flush();
    expect(searchCalls()).toHaveLength(0);
    expect($("#status").textContent).toBe("Type something to search, or set a filter to browse.");
    expect($("#results").children.length).toBe(0);
  });

  it("Enter runs a search with the trimmed text and default filters", async () => {
    await bootSearched();
    expect(searchCalls()).toHaveLength(1);
    expect(searchCalls()[0].args).toEqual({
      query: {
        text: "vault",
        sources: [],
        author: "",
        from_date: "",
        to_date: "",
        limit: 50,
        offset: 0,
      },
    });
    expect($("#status").textContent).toBe("1–3 of 3 · 2 chat groups for “vault”");
    expect($("#status").classList.contains("error")).toBe(false);
    // Results are grouped by chat with a count per group.
    const headers = [...win.document.querySelectorAll("#results .group h3")].map(
      (h) => h.textContent,
    );
    expect(headers).toEqual(["mbox · thread-a (2)", "demo · no thread (1)"]);
    expect(win.document.querySelectorAll("#results .hit-item").length).toBe(3);
    // Full grouped-results snapshot (timezone-independent via mocked format).
    expect($("#results").innerHTML).toMatchSnapshot();
  });

  it("the Search button triggers the same run", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    input("#search").value = "vault";
    $("#search-btn").click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
  });

  it("shows the loading state until the response lands", async () => {
    await boot();
    const slow = deferred<SearchPage>();
    searchRespondsWith(() => slow.promise);
    input("#search").value = "vault";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect($("#status").textContent).toBe("Searching…"));
    slow.resolve(pageOf(HITS, 3));
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–3 of 3"));
  });

  it("a failing search marks the status line as an error and keeps old results", async () => {
    await bootSearched();
    searchRespondsWith(() => Promise.reject("backend exploded"));
    await runQuery("boom");
    await vi.waitFor(() => expect($("#status").textContent).toBe("backend exploded"));
    expect($("#status").classList.contains("error")).toBe(true);
    // Previous results stay on screen.
    expect(win.document.querySelectorAll("#results .hit-item").length).toBe(3);
  });

  it("a zero-hit search renders the empty state", async () => {
    await boot();
    searchRespondsWith(() => pageOf([], 0));
    await runQuery("nothing-matches");
    await vi.waitFor(() => expect($("#status").textContent).toBe("0 results for “nothing-matches”"));
    expect($(".page-info").textContent).toBe("0 results");
    const buttons = pageButtons();
    expect(buttons.map((b) => b.disabled)).toEqual([true, true]);
    expect(win.document.querySelectorAll("#results .group, #results .hit-item").length).toBe(0);
  });
});

// ---------------------------------------------------------------------------
// Stale-response guard (rapid re-searches / pagination races)
// ---------------------------------------------------------------------------

describe("stale-response guard", () => {
  it("a late response from an earlier query never overwrites the newest results", async () => {
    await boot();
    const slowOld = deferred<SearchPage>();
    const oldHits = [hitOf(message(5, "old", "old-thread", "stale result"))];
    queueResponses([slowOld.promise, Promise.resolve(pageOf(HITS, 3))]);

    await runQuery("old");
    await vi.waitFor(() => expect($("#status").textContent).toBe("Searching…"));
    await runQuery("new");
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–3 of 3"));
    expect($("#status").textContent).toContain("for “new”");

    // The slow first query finally resolves — it must be discarded.
    slowOld.resolve(pageOf(oldHits, 99));
    await flush();
    expect($("#status").textContent).toBe("1–3 of 3 · 2 chat groups for “new”");
    expect(win.document.querySelectorAll("#results .hit-item").length).toBe(3);
    expect(win.document.querySelector('#results .hit-item[data-id="5"]')).toBeNull();
  });

  it("an error from a superseded query is ignored", async () => {
    await boot();
    const slowOld = deferred<SearchPage>();
    queueResponses([slowOld.promise, Promise.resolve(pageOf(HITS, 3))]);
    await runQuery("old");
    await runQuery("new");
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–3 of 3"));

    slowOld.reject("too late");
    await flush();
    expect($("#status").textContent).toBe("1–3 of 3 · 2 chat groups for “new”");
    expect($("#status").classList.contains("error")).toBe(false);
  });

  it("a late pagination response does not overwrite a newer query's results", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 2), 120));
    await runQuery("vault");
    await vi.waitFor(() => expect($(".page-info").textContent).toBe("1–50 of 120 · page 1/3"));

    // Next page request is slow; meanwhile a new query races past it.
    const slowPage = deferred<SearchPage>();
    queueResponses([slowPage.promise, Promise.resolve(pageOf(HITS, 3, 0))]);
    pageButtons()[1].click(); // Next → offset 50, slow
    input("#search").value = "other";
    pressKey(input("#search"), "Enter"); // newer query → offset 0, fast
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–3 of 3"));

    slowPage.resolve(pageOf(HITS.slice(0, 2), 120, 50));
    await flush();
    expect($("#status").textContent).toBe("1–3 of 3 · 2 chat groups for “other”");
    expect($(".page-info").textContent).toBe("1–3 of 3 · page 1/1");
    expect(
      searchCalls().map((c) => (c.args as { query: { offset: number } }).query.offset),
    ).toEqual([0, 50, 0]);
  });
});

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

describe("filters", () => {
  it("a source chip triggers a filtered browse and reflects its pressed state", async () => {
    await boot();
    await vi.waitFor(() => expect(win.document.querySelectorAll(".chip").length).toBe(2));
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 6));
    ($('[data-source="mbox"]') as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({
      query: { text: "", sources: ["mbox"] },
    });
    // Browse mode: no query echo in the status line.
    expect($("#status").textContent).toBe("1–6 of 6 · 1 chat group");
    const chip = $('[data-source="mbox"]');
    expect(chip.classList.contains("on")).toBe(true);
    expect(chip.getAttribute("aria-pressed")).toBe("true");

    // Toggling the only filter off returns to the empty state via the
    // empty-query guard — without another IPC round-trip.
    searchRespondsWith(() => pageOf(HITS, 9));
    ($('[data-source="mbox"]') as HTMLElement).click();
    await flush();
    expect(searchCalls()).toHaveLength(1); // only the initial filtered browse
    expect($('[data-source="mbox"]')).not.toBeNull();
    expect($('[data-source="mbox"]').classList.contains("on")).toBe(false);
    expect($("#status").textContent).toBe("Type something to search, or set a filter to browse.");
    expect(win.document.querySelectorAll("#results .hit-item").length).toBe(0);
  });

  it("the author filter sends the trimmed substring on Enter", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(1, 2), 1));
    input("#author").value = "  bruno ";
    pressKey(input("#author"), "Enter");
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({ query: { text: "", author: "bruno" } });
    expect($("#status").textContent).toBe("1–1 of 1 · 1 chat group");
  });

  it("date filters apply on change", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    input("#from-date").value = "2026-01-01";
    fireChange(input("#from-date"));
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({ query: { from_date: "2026-01-01" } });

    input("#to-date").value = "2026-01-31";
    fireChange(input("#to-date"));
    await vi.waitFor(() => expect(searchCalls().length).toBe(2));
    expect(searchCalls()[1].args).toMatchObject({
      query: { from_date: "2026-01-01", to_date: "2026-01-31" },
    });
  });

  it("Clear resets every filter without issuing another query", async () => {
    await boot();
    await vi.waitFor(() => expect(win.document.querySelectorAll(".chip").length).toBe(2));
    searchRespondsWith(() => pageOf(HITS, 9));
    ($('[data-source="mbox"]') as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls().length).toBe(1));
    input("#author").value = "bruno";
    input("#from-date").value = "2026-01-01";
    input("#to-date").value = "2026-01-31";

    $("#clear-filters").click();
    await flush();
    expect(input("#author").value).toBe("");
    expect(input("#from-date").value).toBe("");
    expect(input("#to-date").value).toBe("");
    expect($('[data-source="mbox"]').classList.contains("on")).toBe(false);
    expect($('[data-source="mbox"]').getAttribute("aria-pressed")).toBe("false");
    // All filters empty → the empty-query guard fires instead of a browse.
    expect(searchCalls()).toHaveLength(1);
    expect($("#status").textContent).toBe("Type something to search, or set a filter to browse.");
  });
});

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

describe("pagination", () => {
  it("steps through pages via Next/Prev with the right offsets", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 2), 120, 0));
    await runQuery("vault");
    await vi.waitFor(() => expect($(".page-info").textContent).toBe("1–50 of 120 · page 1/3"));
    const [prev, next] = pageButtons();
    expect(prev.disabled).toBe(true);
    expect(next.disabled).toBe(false);

    searchRespondsWith(() => pageOf(HITS.slice(0, 2), 120, 50));
    next.click();
    await vi.waitFor(() => expect($(".page-info").textContent).toBe("51–100 of 120 · page 2/3"));
    expect(pageButtons()[0].disabled).toBe(false);

    searchRespondsWith(() => pageOf(HITS.slice(0, 2), 120, 0));
    pageButtons()[0].click();
    await vi.waitFor(() => expect($(".page-info").textContent).toBe("1–50 of 120 · page 1/3"));
    expect(searchCalls().map((c) => (c.args as { query: { offset: number } }).query.offset)).toEqual([
      0, 50, 0,
    ]);
  });
});

// ---------------------------------------------------------------------------
// Detail view
// ---------------------------------------------------------------------------

describe("detail view", () => {
  it("opens on click, fetches the thread with radius 8, and snapshots the panel", async () => {
    await bootSearched();
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    expect(ctxCalls().map((c) => c.args)).toEqual([{ id: 1, radius: 8 }]);
    expect(win.document.querySelectorAll("#detail .thread-row").length).toBe(5);
    expect(($(".thread-row.current") as HTMLElement).dataset.id).toBe("1");
    expect(win.document.querySelectorAll("#detail .thread-row.link").length).toBe(4);
    expect($("#detail").innerHTML).toMatchSnapshot();
  });

  it("Escape and the close button close the panel", async () => {
    await bootSearched();
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));

    pressKey(win.document, "Escape");
    expect($("#detail").classList.contains("open")).toBe(false);
    expect($("#detail").children.length).toBe(0);

    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    ($(".close-btn") as HTMLElement).click();
    expect($("#detail").classList.contains("open")).toBe(false);
  });

  it("arrow keys walk the thread by loading the neighbor message", async () => {
    await bootSearched();
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect(ctxCalls()).toHaveLength(1));

    pressKey(win.document, "ArrowUp");
    await vi.waitFor(() => expect(ctxCalls()).toHaveLength(2));
    expect(ctxCalls()[1].args).toEqual({ id: 11, radius: 8 });

    pressKey(win.document, "ArrowDown");
    await vi.waitFor(() => expect(ctxCalls()).toHaveLength(3));
    expect(ctxCalls()[2].args).toEqual({ id: 12, radius: 8 });
  });

  it("an arrow at the thread edge loads nothing", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    const topEdge: ChatContext = {
      before: [],
      message: message(1, "mbox", "thread-a", "quarterly vault report"),
      after: [message(2, "mbox", "thread-a", "next message")],
    };
    tauri.handlers.set("chat_context", () => topEdge);
    await runQuery("vault");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));

    pressKey(win.document, "ArrowUp");
    await flush();
    expect(ctxCalls()).toHaveLength(1); // only the initial open
  });

  it("a vanished message (null context) leaves the panel closed", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    tauri.handlers.set("chat_context", () => null);
    await runQuery("vault");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await flush();
    expect($("#detail").classList.contains("open")).toBe(false);
    expect($("#status").classList.contains("error")).toBe(false);
  });

  it("clicking a thread neighbor opens that message", async () => {
    await bootSearched();
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect(ctxCalls()).toHaveLength(1));
    ($('.thread-row[data-id="12"]') as HTMLElement).click();
    await vi.waitFor(() => expect(ctxCalls()).toHaveLength(2));
    expect(ctxCalls()[1].args).toEqual({ id: 12, radius: 8 });
  });
});

// ---------------------------------------------------------------------------
// Keyboard navigation through results
// ---------------------------------------------------------------------------

describe("keyboard navigation", () => {
  it("/ focuses the search box", async () => {
    await bootSearched();
    pressKey(win.document.body, "/");
    expect(win.document.activeElement).toBe($("#search"));
  });

  it("arrows move the active row, clamp at the ends, Escape deselects", async () => {
    await bootSearched();
    pressKey(win.document.body, "ArrowDown");
    expect($('[data-index="0"]').classList.contains("active")).toBe(true);
    expect($('[data-index="0"]').getAttribute("aria-current")).toBe("true");

    pressKey(win.document.body, "ArrowDown");
    expect($('[data-index="1"]').classList.contains("active")).toBe(true);
    expect($('[data-index="0"]').classList.contains("active")).toBe(false);

    pressKey(win.document.body, "ArrowUp");
    expect($('[data-index="0"]').classList.contains("active")).toBe(true);

    for (let i = 0; i < 5; i++) pressKey(win.document.body, "ArrowDown");
    expect($('[data-index="2"]').classList.contains("active")).toBe(true);
    expect(win.document.querySelectorAll(".hit-item.active").length).toBe(1);

    pressKey(win.document.body, "Escape");
    expect(win.document.querySelector(".hit-item.active")).toBeNull();
    expect(win.document.querySelector('[aria-current="true"]')).toBeNull();
  });

  it("Enter opens the detail view for the active hit", async () => {
    await bootSearched();
    pressKey(win.document.body, "ArrowDown");
    pressKey(win.document.body, "Enter");
    await vi.waitFor(() => expect(ctxCalls()).toHaveLength(1));
    expect(ctxCalls()[0].args).toEqual({ id: 1, radius: 8 });
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
  });

  it("Enter with no active row opens nothing", async () => {
    await bootSearched();
    pressKey(win.document.body, "Enter");
    await flush();
    expect(ctxCalls()).toHaveLength(0);
    expect($("#detail").classList.contains("open")).toBe(false);
  });

  it("arrow keys typed into the search box do not move the selection", async () => {
    await bootSearched();
    pressKey(win.document.body, "ArrowDown");
    expect($('[data-index="0"]').classList.contains("active")).toBe(true);

    input("#search").focus();
    pressKey(input("#search"), "ArrowDown");
    await flush();
    expect($('[data-index="0"]').classList.contains("active")).toBe(true);
    expect(win.document.querySelectorAll(".hit-item.active").length).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// Stats & chips
// ---------------------------------------------------------------------------

describe("stats and chips", () => {
  it("shows 'empty archive' and no chips when nothing is stored", async () => {
    tauri.handlers.set("stats", () => ({ total: 0, by_source: [] }));
    await boot();
    await vi.waitFor(() => expect($("#stats").textContent).toBe("empty archive"));
    expect(win.document.querySelectorAll(".chip").length).toBe(0);
  });

  it("a stats failure clears the label but leaves search usable", async () => {
    tauri.handlers.set("stats", () => Promise.reject("db locked"));
    await boot();
    await vi.waitFor(() => expect($("#stats").textContent).toBe(""));
    expect(win.document.querySelectorAll(".chip").length).toBe(0);

    searchRespondsWith(() => pageOf(HITS, 3));
    await runQuery("vault");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
  });
});

// ---------------------------------------------------------------------------
// Ask UI (natural-language search) + local embedding controls
// ---------------------------------------------------------------------------

import type { AskOutcome } from "./llmview";
import type { EmbeddingStatus, EmbedJobSummary } from "./types";

const EMBED_STATUS: EmbeddingStatus = {
  model: "hash-ngram-v1",
  dim: 512,
  embedded: 3,
  messages: 4,
  pending: 1,
};

const ASK_OUTCOME: AskOutcome = {
  plan: { match_expr: '"vault"', date_from: "2026-10-01", date_to: null },
  plan_source: "llm",
  ranking: "hybrid_fts_vector",
  results: [
    message(21, "mbox", "thread-a", "vault sync planning went fine"),
    message(22, "demo", "", "the syncing of the vault had problems"),
  ],
  answer: "October covered sync [1] and problems [2].",
  notes: [],
};

describe("ask and local embeddings", () => {
  it("boots with the embedding coverage line", async () => {
    tauri.handlers.set("embed_status", () => EMBED_STATUS);
    await boot();
    await vi.waitFor(() => expect($("#embed-status").textContent).toContain("3/4"));
    expect($("#embed-status").textContent).toContain("1 pending");
    expect($("#embed-status").textContent).toContain("hash-ngram-v1");
  });

  it("an embed_status failure leaves the line empty, not broken", async () => {
    tauri.handlers.set("embed_status", () => Promise.reject("backend exploded"));
    await boot();
    await vi.waitFor(() => expect($("#embed-status").textContent).toBe(""));
  });

  it("Update local embeddings runs the incremental job and reports", async () => {
    tauri.handlers.set("embed_status", () => EMBED_STATUS);
    await boot();
    const summary: EmbedJobSummary = {
      model: "hash-ngram-v1",
      dim: 512,
      embedded: 4,
      remaining: 0,
      elapsed_ms: 12,
    };
    tauri.handlers.set("embed_messages", () => summary);
    $("#embed-btn").click();
    await vi.waitFor(() =>
      expect($("#embed-status").textContent).toBe(
        "Embedded 4 new · 0 pending (12 ms, hash-ngram-v1)",
      ),
    );
    const call = tauri.calls.find((c) => c.cmd === "embed_messages");
    expect(call?.args).toEqual({});
    expect($<HTMLButtonElement>("#embed-btn").disabled).toBe(false);
  });

  it("an embedding job failure is reported inline", async () => {
    tauri.handlers.set("embed_status", () => EMBED_STATUS);
    await boot();
    tauri.handlers.set("embed_messages", () => Promise.reject("disk full"));
    $("#embed-btn").click();
    await vi.waitFor(() => expect($("#embed-status").textContent).toBe("Embedding failed: disk full"));
  });

  it("Ask renders the ranking label, results, and the answer", async () => {
    await boot();
    tauri.handlers.set("llm_ask", () => ASK_OUTCOME);
    input("#ask").value = "vault sync?";
    pressKey(input("#ask"), "Enter");
    await vi.waitFor(() =>
      expect($("#ask-status").textContent).toContain("ranked: hybrid FTS + local vectors"),
    );
    expect($("#ask-status").textContent).toContain("planned by LLM");
    expect($("#ask-results").querySelectorAll("li").length).toBe(2);
    expect($("#answer").textContent).toBe(ASK_OUTCOME.answer);
    expect($("#answer").hidden).toBe(false);
    expect($("#ask-notes").hidden).toBe(true);
  });

  it("an empty question asks nothing", async () => {
    await boot();
    input("#ask").value = "   ";
    pressKey(input("#ask"), "Enter");
    await flush();
    expect(tauri.calls.filter((c) => c.cmd === "llm_ask")).toHaveLength(0);
    expect($("#ask-status").textContent).toBe("Ask a question first.");
  });
});

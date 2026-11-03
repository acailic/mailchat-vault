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
import type {
  ArchiveStats,
  Message,
  Stats,
  SyncScheduleView,
  WatchSnapshot,
} from "./types";
import type {
  ConversationContext,
  MessageSummary,
  SearchArchivePage,
} from "./v2types";

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

// The chat-export import, the folder-watching panel, and the export
// controls use the OS file/folder/save pickers; stub them per test.
const dialog = vi.hoisted(() => ({ open: vi.fn(), savedPath: "__unset__" as string | null }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: dialog.open,
  save: () => Promise.resolve(dialog.savedPath),
}));

const tauriEvents = vi.hoisted(() => ({
  listeners: new Map<string, (e: { payload: unknown }) => void>(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (e: { payload: unknown }) => void) => {
    tauriEvents.listeners.set(name, handler);
    return Promise.resolve(() => tauriEvents.listeners.delete(name));
  },}));

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

const ARCHIVE_STATS: ArchiveStats = {
  total: 9,
  first_sent_at: "2026-01-01T10:00:00Z",
  last_sent_at: "2026-01-03T10:00:00Z",
  by_source: STATS.by_source,
  by_day: [
    { day: "2026-01-01", count: 3 },
    { day: "2026-01-02", count: 4 },
    { day: "2026-01-03", count: 2 },
  ],
  top_chats: [{ source: "demo", chat_id: "project-chat", count: 4 }],
  top_authors: [
    { author: "marko", count: 2 },
    { author: "jelena", count: 2 },
    { author: "ana", count: 1 },
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

function summary(
  id: number,
  source: string,
  conversation: string | null,
  author: string,
  snippetText = "vault …",
): MessageSummary {
  return {
    id,
    source_id: `inst-${source}`,
    source_label: source,
    conversation_id: conversation ? `conv-${conversation}` : null,
    conversation_label: conversation,
    author,
    subject: null,
    sent_at: `2026-01-${String(id).padStart(2, "0")}T10:00:00Z`,
    time_confidence: "legacy_assumed",
    snippet: snippetText,
    attachment_count: 0,
    available_attachment_count: 0,
    has_conflicting_revision: false,
  };
}

function pageOf(hits: MessageSummary[], total: number, offset = 0, revision = 4): SearchArchivePage {
  return { hits, total, limit: 50, offset, archive_revision: revision };
}

const HITS: MessageSummary[] = [
  summary(1, "mbox", "thread-a", "ana"),
  summary(2, "mbox", "thread-a", "bruno"),
  summary(3, "demo", null, "ana"),
];

const CTX: ConversationContext = {
  anchor: {
    id: 1,
    source_id: "inst-mbox",
    source_label: "mbox",
    channel: "email",
    chat_id: "thread-a",
    external_id: "e1",
    conversation_id: "conv-thread-a",
    conversation_label: "thread-a",
    conversation_confidence: "verified",
    author: "ana",
    subject: null,
    body: "quarterly vault report",
    sent_at: "2026-01-01T10:00:00Z",
    original_timestamp: "2026-01-01T10:00:00Z",
    timezone: "",
    time_confidence: "legacy_assumed",
    attachments: [],
    occurrences: [{ scope: "import", provider_item_id: "e1", uidvalidity: null, import_id: "imp-1", observed_at: "2026-01-01 00:00:00" }],
    revisions: [],
    raw_json: "{}",
  },
  before: [
    { id: 10, author: "ana", sent_at: "2026-01-01T09:00:00Z", time_confidence: "legacy_assumed", body: "earlier" },
    { id: 11, author: "ana", sent_at: "2026-01-01T09:30:00Z", time_confidence: "legacy_assumed", body: "before current" },
  ],
  after: [
    { id: 12, author: "ana", sent_at: "2026-01-01T11:00:00Z", time_confidence: "legacy_assumed", body: "after current" },
    { id: 13, author: "ana", sent_at: "2026-01-01T12:00:00Z", time_confidence: "legacy_assumed", body: "much later" },
  ],
  unverified_conversation: false,
  has_more_before: false,
  has_more_after: false,
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
  return tauri.calls.filter((c) => c.cmd === "search_archive_v2");
}

function ctxCalls(): Array<{ cmd: string; args: unknown }> {
  return tauri.calls.filter((c) => c.cmd === "conversation_context_v2");
}

function searchRespondsWith(respond: () => unknown): void {
  tauri.handlers.set("search_archive_v2", respond);
}

function queueResponses(responses: unknown[]): void {
  const pending = [...responses];
  searchRespondsWith(() => {
    const next = pending.shift();
    if (next === undefined) throw new Error("unexpected extra search_archive_v2 call");
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
  vi.stubGlobal("navigator", win.navigator);
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
  tauri.handlers.set("conversation_context_v2", () => CTX);
  await runQuery("vault");
  await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
}

beforeEach(() => {
  vi.resetModules();
  tauri.calls.length = 0;
  tauri.handlers.clear();
  tauri.handlers.set("stats", () => STATS);
  // Canonical source instances (id ↔ display label) for the v2 source
  // filter; import scopes for the import-scope chip.
  tauri.handlers.set("sources_list", () => [
    {
      id: "inst-mbox",
      kind: "legacy",
      label: "mbox",
      connector_key: "mbox",
      config_json: "{}",
      state: "connected",
      excluded: false,
      last_attempt_at: null,
      last_success_at: null,
      created_at: "2026-01-01 00:00:00",
    },
    {
      id: "inst-demo",
      kind: "legacy",
      label: "demo",
      connector_key: "demo",
      config_json: "{}",
      state: "connected",
      excluded: false,
      last_attempt_at: null,
      last_success_at: null,
      created_at: "2026-01-01 00:00:00",
    },
  ]);
  tauri.handlers.set("list_import_scopes_v2", () => [
    {
      import_id: "imp-1",
      messages: 2,
      first_observed_at: "2026-01-01 00:00:00",
      last_observed_at: "2026-01-01 00:00:00",
    },
  ]);
  tauriEvents.listeners.clear();
  dialog.open.mockReset();
  dialog.savedPath = null;});

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
      request: {
        text: "vault",
        source_ids: [],
        author: "",
        from_date: null,
        to_date: null,
        timezone: "",
        has_attachment: null,
        import_job_id: null,
        sort: "relevance",
        limit: 50,
        offset: 0,
      },
    });
    expect($("#status").textContent).toBe("1–3 of 3 for “vault”");
    expect($("#status").classList.contains("error")).toBe(false);
    // Results are a flat list: one row per hit, backend order, no
    // conversation regrouping headers.
    expect(win.document.querySelectorAll("#results .group, #results .group h3").length).toBe(0);
    const rows = [...win.document.querySelectorAll("#results .hit-item")];
    expect(rows.map((r) => (r as unknown as HTMLElement).dataset.id)).toEqual(["1", "2", "3"]);
    // Full flat-results snapshot (timezone-independent via mocked format).
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
    const slow = deferred<SearchArchivePage>();
    searchRespondsWith(() => slow.promise);
    input("#search").value = "vault";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect($("#status").textContent).toBe("Searching…"));
    slow.resolve(pageOf(HITS, 3));
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–3 of 3"));
  });

  it("a failing search marks the status line as an error and keeps old results", async () => {
    await bootSearched();
    searchRespondsWith(() =>
      Promise.reject({ code: "internal", message: "backend exploded", retryable: true }),
    );
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
    const slowOld = deferred<SearchArchivePage>();
    const oldHits = [summary(5, "old", "old-thread", "ana", "stale result")];
    queueResponses([slowOld.promise, Promise.resolve(pageOf(HITS, 3))]);

    await runQuery("old");
    await vi.waitFor(() => expect($("#status").textContent).toBe("Searching…"));
    await runQuery("new");
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–3 of 3"));
    expect($("#status").textContent).toContain("for “new”");

    // The slow first query finally resolves — it must be discarded.
    slowOld.resolve(pageOf(oldHits, 99));
    await flush();
    expect($("#status").textContent).toBe("1–3 of 3 for “new”");
    expect(win.document.querySelectorAll("#results .hit-item").length).toBe(3);
    expect(win.document.querySelector('#results .hit-item[data-id="5"]')).toBeNull();
  });

  it("an error from a superseded query is ignored", async () => {
    await boot();
    const slowOld = deferred<SearchArchivePage>();
    queueResponses([slowOld.promise, Promise.resolve(pageOf(HITS, 3))]);
    await runQuery("old");
    await runQuery("new");
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–3 of 3"));

    slowOld.reject("too late");
    await flush();
    expect($("#status").textContent).toBe("1–3 of 3 for “new”");
    expect($("#status").classList.contains("error")).toBe(false);
  });

  it("a late pagination response does not overwrite a newer query's results", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 2), 120));
    await runQuery("vault");
    await vi.waitFor(() => expect($(".page-info").textContent).toBe("1–50 of 120 · page 1/3"));

    // Next page request is slow; meanwhile a new query races past it.
    const slowPage = deferred<SearchArchivePage>();
    queueResponses([slowPage.promise, Promise.resolve(pageOf(HITS, 3, 0))]);
    pageButtons()[1].click(); // Next → offset 50, slow
    input("#search").value = "other";
    pressKey(input("#search"), "Enter"); // newer query → offset 0, fast
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–3 of 3"));

    slowPage.resolve(pageOf(HITS.slice(0, 2), 120, 50));
    await flush();
    expect($("#status").textContent).toBe("1–3 of 3 for “other”");
    expect($(".page-info").textContent).toBe("1–3 of 3 · page 1/1");
    expect(
      searchCalls().map((c) => (c.args as { request: { offset: number } }).request.offset),
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
      request: { text: "", source_ids: ["inst-mbox"] },
    });
    // Browse mode: no query echo in the status line.
    expect($("#status").textContent).toBe("1–6 of 6");
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

  it("a shared display label selects every instance carrying that label", async () => {
    tauri.handlers.set("sources_list", () => [
      {
        id: "inst-mbox",
        kind: "legacy",
        label: "mbox",
        connector_key: "mbox",
        config_json: "{}",
        state: "connected",
        excluded: false,
        last_attempt_at: null,
        last_success_at: null,
        created_at: "2026-01-01 00:00:00",
      },
      {
        id: "inst-mbox2",
        kind: "imap",
        label: "mbox",
        connector_key: "imap",
        config_json: "{}",
        state: "connected",
        excluded: false,
        last_attempt_at: null,
        last_success_at: null,
        created_at: "2026-01-01 00:00:00",
      },
      {
        id: "inst-demo",
        kind: "legacy",
        label: "demo",
        connector_key: "demo",
        config_json: "{}",
        state: "connected",
        excluded: false,
        last_attempt_at: null,
        last_success_at: null,
        created_at: "2026-01-01 00:00:00",
      },
    ]);
    await boot();
    await vi.waitFor(() => expect(win.document.querySelectorAll(".chip").length).toBe(2));
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 6));
    ($('[data-source="mbox"]') as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({
      request: { text: "", source_ids: ["inst-mbox", "inst-mbox2"] },
    });
  });

  it("removing a colliding-label source chip clears every instance it expanded to", async () => {
    tauri.handlers.set("sources_list", () => [
      {
        id: "inst-mbox",
        kind: "legacy",
        label: "mbox",
        connector_key: "mbox",
        config_json: "{}",
        state: "connected",
        excluded: false,
        last_attempt_at: null,
        last_success_at: null,
        created_at: "2026-01-01 00:00:00",
      },
      {
        id: "inst-mbox2",
        kind: "imap",
        label: "mbox",
        connector_key: "imap",
        config_json: "{}",
        state: "connected",
        excluded: false,
        last_attempt_at: null,
        last_success_at: null,
        created_at: "2026-01-01 00:00:00",
      },
      {
        id: "inst-demo",
        kind: "legacy",
        label: "demo",
        connector_key: "demo",
        config_json: "{}",
        state: "connected",
        excluded: false,
        last_attempt_at: null,
        last_success_at: null,
        created_at: "2026-01-01 00:00:00",
      },
    ]);
    await boot();
    await vi.waitFor(() => expect(win.document.querySelectorAll(".chip").length).toBe(2));
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 6));
    ($('[data-source="mbox"]') as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({
      request: { source_ids: ["inst-mbox", "inst-mbox2"] },
    });
    ($("#active-filter-chips .filter-chip") as HTMLElement).click();
    await flush();
    expect(searchCalls()).toHaveLength(1);
    expect(win.document.querySelectorAll("#active-filter-chips .filter-chip").length).toBe(0);
    expect($("#status").textContent).toBe("Type something to search, or set a filter to browse.");
  });

  it("the author filter sends the trimmed substring on Enter", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(1, 2), 1));
    input("#author").value = "  bruno ";
    pressKey(input("#author"), "Enter");
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({ request: { text: "", author: "bruno" } });
    expect($("#status").textContent).toBe("1–1 of 1");
  });

  it("date filters apply on change", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    input("#from-date").value = "2026-01-01";
    fireChange(input("#from-date"));
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({ request: { from_date: "2026-01-01" } });

    input("#to-date").value = "2026-01-31";
    fireChange(input("#to-date"));
    await vi.waitFor(() => expect(searchCalls().length).toBe(2));
    expect(searchCalls()[1].args).toMatchObject({
      request: { from_date: "2026-01-01", to_date: "2026-01-31" },
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
    expect(
      searchCalls().map((c) => (c.args as { request: { offset: number } }).request.offset),
    ).toEqual([0, 50, 0]);
  });
});

// ---------------------------------------------------------------------------
// Detail view
// ---------------------------------------------------------------------------

describe("detail view", () => {
  it("opens on click, fetches the context with paged cursors, and snapshots the panel", async () => {
    await bootSearched();
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    expect(ctxCalls().map((c) => c.args)).toEqual([
      { id: 1, beforeCursor: null, afterCursor: null, limit: 8 },
    ]);
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
    expect(ctxCalls()[1].args).toEqual({ id: 11, beforeCursor: null, afterCursor: null, limit: 8 });

    pressKey(win.document, "ArrowDown");
    await vi.waitFor(() => expect(ctxCalls()).toHaveLength(3));
    expect(ctxCalls()[2].args).toEqual({ id: 12, beforeCursor: null, afterCursor: null, limit: 8 });
  });

  it("an arrow at the thread edge loads nothing", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    const topEdge: ConversationContext = {
      anchor: CTX.anchor,
      before: [],
      after: [
        { id: 2, author: "ana", sent_at: "2026-01-02T10:00:00Z", time_confidence: "legacy_assumed", body: "next message" },
      ],
      unverified_conversation: false,
      has_more_before: false,
      has_more_after: false,
    };
    tauri.handlers.set("conversation_context_v2", () => topEdge);
    await runQuery("vault");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));

    pressKey(win.document, "ArrowUp");
    await flush();
    expect(ctxCalls()).toHaveLength(1); // only the initial open
  });

  it("a vanished message is a clean not-found, panel stays closed", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    tauri.handlers.set("conversation_context_v2", () =>
      Promise.reject({ code: "not_found", message: "no message with id 1" }),
    );
    await runQuery("vault");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() =>
      expect($("#status").textContent).toBe("Message is no longer in this vault."),
    );
    expect($("#detail").classList.contains("open")).toBe(false);
    expect($("#status").classList.contains("error")).toBe(true);
  });

  it("clicking a thread neighbor opens that message", async () => {
    await bootSearched();
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect(ctxCalls()).toHaveLength(1));
    ($('.thread-row[data-id="12"]') as HTMLElement).click();
    await vi.waitFor(() => expect(ctxCalls()).toHaveLength(2));
    expect(ctxCalls()[1].args).toEqual({ id: 12, beforeCursor: null, afterCursor: null, limit: 8 });
  });
});

// ---------------------------------------------------------------------------
// v2 search features (M3.2): chips, attachment/import filters, deep links,
// restorable selection/scroll, refresh notice, selection export, copy
// ---------------------------------------------------------------------------

describe("v2 filters", () => {
  it("the attachment filter sends has_attachment on change", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 1));
    const select = $<HTMLSelectElement>("#has-attachment");
    select.value = "with";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({ request: { has_attachment: true } });

    select.value = "without";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls().length).toBe(2));
    expect(searchCalls()[1].args).toMatchObject({ request: { has_attachment: false } });
  });

  it("the import-scope filter lists scopes and filters by the chosen run", async () => {
    await boot();
    await vi.waitFor(() => {
      const select = $<HTMLSelectElement>("#import-scope");
      expect([...select.querySelectorAll("option")].map((o) => o.value)).toEqual([
        "",
        "imp-1",
      ]);
    });
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 2));
    const select = $<HTMLSelectElement>("#import-scope");
    select.value = "imp-1";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({ request: { import_job_id: "imp-1" } });
  });

  it("the sort choice is sent for text searches", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    input("#search").value = "vault";
    const select = $<HTMLSelectElement>("#sort-order");
    select.value = "newest";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({ request: { sort: "newest", text: "vault" } });
  });

  it("applied filters render as removable chips; removal re-runs the search", async () => {
    await boot();
    await vi.waitFor(() => expect(win.document.querySelectorAll(".chip").length).toBe(2));
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 1));
    ($('[data-source="demo"]') as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    input("#author").value = "ana";
    pressKey(input("#author"), "Enter");
    await vi.waitFor(() => expect(searchCalls().length).toBe(2));

    const chips = [...win.document.querySelectorAll("#active-filter-chips .filter-chip")];
    expect(chips.map((c) => c.textContent?.trim())).toEqual(["demo ×", "author: ana ×"]);

    // Removing the author chip keeps the source filter.
    (chips[1] as unknown as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls().length).toBe(3));
    expect(searchCalls()[2].args).toMatchObject({
      request: { source_ids: ["inst-demo"], author: "" },
    });
    expect(win.document.querySelectorAll("#active-filter-chips .filter-chip").length).toBe(1);
  });

  it("reversed date bounds are rejected inline before any invocation", async () => {
    await boot();
    input("#from-date").value = "2026-02-02";
    input("#to-date").value = "2026-02-01";
    input("#search").value = "vault";
    pressKey(input("#search"), "Enter");
    await flush();
    expect(searchCalls()).toHaveLength(0);
    expect($("#status").textContent).toContain("Reversed date range");
    expect($("#status").classList.contains("error")).toBe(true);
  });
});

describe("deep links", () => {
  it("a pasted mailchat://message/<id> locator opens that message directly", async () => {
    await boot();
    tauri.handlers.set("conversation_context_v2", () => CTX);
    input("#search").value = "mailchat://message/7";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect(ctxCalls()).toHaveLength(1));
    expect(ctxCalls()[0].args).toMatchObject({ id: 7 });
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    // No search ran: the locator is navigation, not a text query.
    expect(searchCalls()).toHaveLength(0);
  });

  it("non-link text still searches normally", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    input("#search").value = "mailchat://message/12x";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect((searchCalls()[0].args as { request: { text: string } }).request.text).toBe(
      "mailchat://message/12x",
    );
  });
});

describe("restorable selection and scroll", () => {
  it("Back to results re-selects the opened row and restores the scroll", async () => {
    await bootSearched();
    // Move the keyboard selection to row 2 (id 2) and scroll the list.
    pressKey(win.document.body, "ArrowDown");
    pressKey(win.document.body, "ArrowDown");
    const results = $("#results");
    results.scrollTop = 240;

    ($('.hit-item[data-id="2"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    expect((win.document.querySelector('.hit-item[data-id="2"]') as unknown as HTMLElement).classList.contains("selected")).toBe(true);

    // Opening replaced the rows (renderAll) — scroll and selection must
    // come back on Back, not immediately.
    ($(".close-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect((win.document.querySelector('.hit-item[data-id="2"]') as unknown as HTMLElement).classList.contains("selected")).toBe(true),
    );
    expect(($(".hit-item.active") as HTMLElement).dataset.id).toBe("2");
    expect($("#results").scrollTop).toBe(240);
    expect($("#detail").classList.contains("open")).toBe(false);
  });

  it("a fresh search resets the selection and snapshot", async () => {
    await bootSearched();
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    ($(".close-btn") as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(false));

    searchRespondsWith(() => pageOf(HITS.slice(1), 1));
    await runQuery("next");
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–1 of 1"));
    expect(win.document.querySelector(".hit-item.selected")).toBeNull();
    expect(win.document.querySelector(".hit-item.active")).toBeNull();
  });
});

describe("archive revision refresh (A07)", () => {
  it("a page response at a different revision offers Refresh instead of silently restarting", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 2), 120, 0, 5));
    await runQuery("vault");
    await vi.waitFor(() => expect($(".page-info").textContent).toBe("1–50 of 120 · page 1/3"));
    expect($("#refresh-notice").hidden).toBe(true);

    // The archive changed while paginating: the next page carries a new
    // revision — a refresh offer appears, rows are not silently reset.
    searchRespondsWith(() => pageOf(HITS.slice(0, 2), 121, 50, 6));
    pageButtons()[1].click();
    await vi.waitFor(() => expect($("#refresh-notice").hidden).toBe(false));
    expect($("#refresh-notice").textContent).toContain("archive changed");
    expect($("#status").textContent).toContain("new data available");

    // Refresh re-runs the fresh submission and clears the notice.
    searchRespondsWith(() => pageOf(HITS.slice(0, 2), 121, 0, 6));
    ($("#refresh-notice button") as HTMLElement).click();
    await vi.waitFor(() => expect($("#refresh-notice").hidden).toBe(true));
    expect($("#status").textContent).not.toContain("new data available");
  });

  it("a succeeded job while results are visible offers Refresh", async () => {
    await bootSearched();
    expect($("#refresh-notice").hidden).toBe(true);
    tauriEvents.listeners.get("job-event")?.({ payload: { seq: 1, at: "", job: 1, type: "succeeded" } });
    await vi.waitFor(() => expect($("#refresh-notice").hidden).toBe(false));
  });
});

describe("selection export and copy actions", () => {
  it("checking rows enables Export selected and exports the checked ids in order", async () => {
    await bootSearched();
    const boxes = [...win.document.querySelectorAll("#results input.row-select")] as unknown as HTMLInputElement[];
    expect(($("#export-selection-btn") as HTMLButtonElement).disabled).toBe(true);
    boxes[2].checked = true;
    fireChange(boxes[2]);
    boxes[0].checked = true;
    fireChange(boxes[0]);
    await vi.waitFor(() =>
      expect(($("#export-selection-btn") as HTMLButtonElement).disabled).toBe(false),
    );
    expect($("#export-selection-btn").textContent).toBe("Export selected (2)…");

    dialog.savedPath = "/tmp/selection.txt";
    tauri.handlers.set("export_messages_v2", () => ({ format: "text", written: 2, missing: 0 }));
    ($("#export-selection-btn") as HTMLElement).click();
    await vi.waitFor(() => expect($("#selection-status").textContent).toBe("Exported 2 messages."));
    const call = tauri.calls.find((c) => c.cmd === "export_messages_v2");
    expect(call?.args).toEqual({ ids: [3, 1], format: "text", path: "/tmp/selection.txt" });
  });

  it("exporting a selection built across pages exports every checked id", async () => {
    await bootSearched();
    // Page 1 of a 51-result archive, then the Next page (offset 50).
    queueResponses([pageOf(HITS, 51), pageOf([summary(51, "mbox", null, "ana")], 51, 50)]);
    await runQuery("vault");
    const boxesOn = () =>
      [...win.document.querySelectorAll("#results input.row-select")] as unknown as HTMLInputElement[];
    boxesOn()[0].checked = true;
    fireChange(boxesOn()[0]);
    pageButtons()[1].click();
    await vi.waitFor(() =>
      expect(win.document.querySelector("#results .hit-item")?.getAttribute("data-id")).toBe("51"),
    );
    boxesOn()[0].checked = true;
    fireChange(boxesOn()[0]);
    await vi.waitFor(() => expect($("#export-selection-btn").textContent).toBe("Export selected (2)…"));
    dialog.savedPath = "/tmp/selection.txt";
    tauri.handlers.set("export_messages_v2", () => ({ format: "text", written: 2, missing: 0 }));
    ($("#export-selection-btn") as HTMLElement).click();
    await vi.waitFor(() => expect($("#selection-status").textContent).toBe("Exported 2 messages."));
    const call = tauri.calls.find((c) => c.cmd === "export_messages_v2");
    expect(call?.args).toEqual({ ids: [51, 1], format: "text", path: "/tmp/selection.txt" });
  });

  it("an export with vanished ids reports the skipped count honestly", async () => {
    await bootSearched();
    const boxes = [...win.document.querySelectorAll("#results input.row-select")] as unknown as HTMLInputElement[];
    boxes[0].checked = true;
    fireChange(boxes[0]);
    dialog.savedPath = "/tmp/selection.json";
    $<HTMLSelectElement>("#selection-format").value = "json";
    tauri.handlers.set("export_messages_v2", () => ({ format: "json", written: 0, missing: 1 }));
    ($("#export-selection-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect($("#selection-status").textContent).toBe(
        "Exported 0 messages (1 no longer in the vault and skipped).",
      ),
    );
  });

  it("Copy message and Copy link write provenance text and the locator", async () => {
    await bootSearched();
    const clipboard = vi.fn((_text: string) => Promise.resolve());
    Object.defineProperty(win.navigator, "clipboard", {
      value: { writeText: clipboard },
      configurable: true,
    });
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));

    ($(".copy-btn") as HTMLElement).click();
    await vi.waitFor(() => expect(clipboard).toHaveBeenCalledTimes(1));
    const copiedText = clipboard.mock.calls[0][0];
    expect(copiedText).toContain("quarterly vault report");
    expect(copiedText).toContain("import imp-1");
    await vi.waitFor(() => expect($("#status").textContent).toContain("Message copied"));

    ($(".copy-link-btn") as HTMLElement).click();
    await vi.waitFor(() => expect(clipboard).toHaveBeenCalledTimes(2));
    expect(clipboard.mock.calls[1][0]).toBe("mailchat://message/1");
  });

  it("the pane's Export… exports exactly the open message", async () => {
    await bootSearched();
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    dialog.savedPath = "/tmp/one.txt";
    tauri.handlers.set("export_messages_v2", () => ({ format: "text", written: 1, missing: 0 }));
    ($(".export-selection-btn") as HTMLElement).click();
    await vi.waitFor(() => expect($("#selection-status").textContent).toBe("Exported 1 message."));
    const call = tauri.calls.find((c) => c.cmd === "export_messages_v2");
    expect(call?.args).toMatchObject({ ids: [1], format: "text" });
  });
});

describe("context paging (load earlier/later)", () => {
  it("Load earlier pages beyond the held rows with the outermost cursor", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    const more: ConversationContext = {
      ...CTX,
      has_more_before: true,
      has_more_after: false,
    };
    tauri.handlers.set("conversation_context_v2", () => more);
    await runQuery("vault");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    expect(($(".load-earlier-btn") as HTMLButtonElement).disabled).toBe(false);
    expect(win.document.querySelectorAll("#detail .thread-row").length).toBe(5);

    const earlierPage: ConversationContext = {
      anchor: CTX.anchor,
      before: [
        { id: 8, author: "ana", sent_at: "2026-01-01T08:00:00Z", time_confidence: "legacy_assumed", body: "way earlier" },
      ],
      after: [],
      unverified_conversation: false,
      has_more_before: false,
      has_more_after: false,
    };
    tauri.handlers.set("conversation_context_v2", () => earlierPage);
    ($(".load-earlier-btn") as HTMLElement).click();
    await vi.waitFor(() => expect(ctxCalls().length).toBe(2));
    expect(ctxCalls()[1].args).toMatchObject({
      id: 1,
      beforeCursor: 10,
      afterCursor: null,
      limit: 8,
    });
    await vi.waitFor(() => expect(win.document.querySelectorAll("#detail .thread-row").length).toBe(6));
    // The anchor stays; Load earlier is exhausted now.
    expect(($(".thread-row.current") as HTMLElement).dataset.id).toBe("1");
    expect(($(".load-earlier-btn") as HTMLButtonElement).disabled).toBe(true);
  });

  it("an unverified conversation shows the notice and no neighbors", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS, 3));
    tauri.handlers.set("conversation_context_v2", () => ({
      ...CTX,
      before: [],
      after: [],
      unverified_conversation: true,
    }));
    await runQuery("vault");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    expect($("#detail").textContent).toContain("Conversation could not be verified");
    expect(win.document.querySelectorAll("#detail .thread-row").length).toBe(1);
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
    expect(ctxCalls()[0].args).toEqual({ id: 1, beforeCursor: null, afterCursor: null, limit: 8 });
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
// Chat export import (media extraction wiring)
// ---------------------------------------------------------------------------

describe("chat export import", () => {
  const OUTCOME = {
    summary: { source: "telegram", seen: 4, inserted: 4, skipped: 0, failed: 0 },
    media: { referenced: 3, stored: 3, deduped: 1, missing: 0, linked: 3 },
  };

  function importCalls(): Array<{ cmd: string; args: unknown }> {
    return tauri.calls.filter((c) => c.cmd === "import_chat_export");
  }

  it("passes the picked path and the checked extract-media toggle, then reports media counts", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/tg-export/result.json");
    tauri.handlers.set("import_chat_export", () => OUTCOME);
    $("#chat-import-btn").click();
    await vi.waitFor(() => expect(importCalls()).toHaveLength(1));
    expect(importCalls()[0].args).toEqual({
      path: "/tmp/tg-export/result.json",
      extractMedia: true,
    });
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Imported 4 new, 0 already present (of 4 in telegram). Media: 3 stored, 1 deduped, 3 linked.",
      ),
    );
  });

  it("unchecking the toggle disables extraction and omits the media part", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/chat.txt");
    input("#chat-media").checked = false;
    tauri.handlers.set("import_chat_export", () => ({
      summary: { source: "whatsapp", seen: 2, inserted: 2, skipped: 0, failed: 0 },
      media: null,
    }));
    $("#chat-import-btn").click();
    await vi.waitFor(() => expect(importCalls()).toHaveLength(1));
    expect(importCalls()[0].args).toEqual({ path: "/tmp/chat.txt", extractMedia: false });
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Imported 2 new, 0 already present (of 2 in whatsapp).",
      ),
    );
  });

  it("a cancelled picker imports nothing; a backend error is shown", async () => {
    await boot();
    dialog.open.mockResolvedValue(null);
    $("#chat-import-btn").click();
    await vi.waitFor(() => expect(dialog.open).toHaveBeenCalled());
    await flush();
    expect(importCalls()).toHaveLength(0);

    dialog.open.mockResolvedValue("/tmp/chat.txt");
    tauri.handlers.set("import_chat_export", () => Promise.reject("bad export"));
    $("#chat-import-btn").click();
    await vi.waitFor(() => expect($("#import-status").textContent).toBe("Import failed: bad export"));
  });
});

// Export
// ---------------------------------------------------------------------------

describe("export", () => {
  it("exports the executed search with all its filters, not just the page", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 2), 120));
    input("#author").value = "bruno";
    pressKey(input("#author"), "Enter");
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–50 of 120"));

    dialog.savedPath = "/tmp/out.csv";
    tauri.handlers.set("export_search", () => ({ format: "csv", written: 120, skipped: 0 }));
    $("#export-btn").click();
    await vi.waitFor(() =>
      expect($("#export-status").textContent).toBe("Exported 120 messages to /tmp/out.csv."),
    );
    const exportCalls = tauri.calls.filter((c) => c.cmd === "export_search");
    expect(exportCalls).toHaveLength(1);
    expect(exportCalls[0].args).toEqual({
      query: {
        text: "",
        sources: [],
        author: "bruno",
        from_date: "",
        to_date: "",
        limit: 50,
        offset: 0,
      },
      format: "csv",
      path: "/tmp/out.csv",
    });
  });

  it("passes the chosen format and notes skipped non-email rows for mbox", async () => {
    await bootSearched();
    dialog.savedPath = "/tmp/archive.mbox";
    tauri.handlers.set("export_search", () => ({
      format: "mbox",
      written: 40,
      skipped: 9,
    }));
    $<HTMLSelectElement>("#export-format").value = "mbox";
    $("#export-btn").click();
    await vi.waitFor(() =>
      expect($("#export-status").textContent).toBe(
        "Exported 40 messages (9 non-email skipped — MBOX carries email rows) to /tmp/archive.mbox.",
      ),
    );
    expect(tauri.calls.find((c) => c.cmd === "export_search")?.args).toMatchObject({
      format: "mbox",
      path: "/tmp/archive.mbox",
    });
  });

  it("a cancelled save dialog exports nothing", async () => {
    await bootSearched();
    dialog.savedPath = null; // cancelled
    $("#export-btn").click();
    await flush();
    expect(tauri.calls.filter((c) => c.cmd === "export_search")).toHaveLength(0);
    expect($("#export-status").textContent).toBe("");
  });

  it("with no executed search there is nothing to export and no dialog", async () => {
    await boot();
    dialog.savedPath = "/tmp/never.csv";
    $("#export-btn").click();
    await flush();
    expect(tauri.calls.filter((c) => c.cmd === "export_search")).toHaveLength(0);
    expect($("#export-status").textContent).toBe(
      "Nothing to export — run a search (or set filters) first.",
    );
  });

  it("a failing export surfaces the error", async () => {
    await bootSearched();
    dialog.savedPath = "/root/forbidden.csv";
    tauri.handlers.set("export_search", () => Promise.reject("disk full"));
    $("#export-btn").click();
    await vi.waitFor(() =>
      expect($("#export-status").textContent).toBe("Export failed: disk full"),
    );
    expect($<HTMLButtonElement>("#export-btn").disabled).toBe(false);
  });

  it("exports the whole chat from the detail view", async () => {
    await bootSearched();
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));

    dialog.savedPath = "/tmp/thread.jsonl";
    tauri.handlers.set("export_chat", () => ({ format: "json", written: 5, skipped: 0 }));
    $<HTMLSelectElement>("#export-format").value = "json";
    ($(".export-chat-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect($("#export-status").textContent).toBe("Exported 5 messages to /tmp/thread.jsonl."),
    );
    expect(tauri.calls.filter((c) => c.cmd === "export_chat")[0].args).toEqual({
      id: 1,
      format: "json",
      path: "/tmp/thread.jsonl",
    });
  });

  it("a vanished message in the chat export reports clearly", async () => {
    await bootSearched();
    ($('.hit-item[data-id="1"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));

    dialog.savedPath = "/tmp/thread.csv";
    tauri.handlers.set("export_chat", () => null);
    ($(".export-chat-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect($("#export-status").textContent).toBe(
        "Export failed: message no longer exists.",
      ),
    );

  });
});

// ---------------------------------------------------------------------------
// Statistics dashboard
// ---------------------------------------------------------------------------

describe("statistics dashboard", () => {
  it("renders summary, day bars, and tables on boot", async () => {
    tauri.handlers.set("archive_stats", () => ARCHIVE_STATS);
    await boot();
    await vi.waitFor(() => expect($("#dashboard p.meta")).toBeTruthy());
    expect($("#dashboard p.meta").textContent).toBe("9 messages · 2 sources · 2026-01-01 → 2026-01-03");
    expect(win.document.querySelectorAll("#dashboard .day-bar").length).toBe(3);
    expect(win.document.querySelectorAll("#dashboard .stat-table").length).toBe(3);
    expect(win.document.querySelectorAll("#dashboard .stat-table tbody tr").length).toBe(6);
    expect($("#dashboard-status").textContent).toBe("");
  });

  it("an empty archive renders the summary only", async () => {
    tauri.handlers.set("archive_stats", () => ({
      total: 0,
      first_sent_at: null,
      last_sent_at: null,
      by_source: [],
      by_day: [],
      top_chats: [],
      top_authors: [],
    }));
    await boot();
    await vi.waitFor(() => expect($("#dashboard").textContent).toBe("empty archive"));
  });

  it("a failure shows the error but leaves search usable", async () => {
    tauri.handlers.set("archive_stats", () => Promise.reject("db locked"));
    await boot();
    await vi.waitFor(() => expect($("#dashboard-status").textContent).toContain("db locked"));
    expect(win.document.querySelectorAll("#dashboard .day-bar").length).toBe(0);

    searchRespondsWith(() => pageOf(HITS, 3));
    await runQuery("vault");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
  });

  it("the Refresh button re-fetches the dashboard", async () => {
    let calls = 0;
    tauri.handlers.set("archive_stats", () => {
      calls += 1;
      return ARCHIVE_STATS;
    });
    await boot();
    await vi.waitFor(() => expect($("#dashboard p.meta")).toBeTruthy());
    const before = tauri.calls.filter((c) => c.cmd === "archive_stats").length;
    $("#stats-refresh").click();
    await vi.waitFor(() =>
      expect(tauri.calls.filter((c) => c.cmd === "archive_stats").length).toBe(before + 1),
    );
    expect(calls).toBeGreaterThanOrEqual(2);
  });

  it("a late response from an earlier refresh never overwrites the newest", async () => {
    const slow = deferred<ArchiveStats>();
    let n = 0;
    tauri.handlers.set("archive_stats", () => {
      n += 1;
      return n === 1 ? slow.promise : ARCHIVE_STATS;
    });
    await boot(); // first (slow) dashboard fetch in flight
    $("#stats-refresh").click(); // newer refresh wins
    await vi.waitFor(() => expect($("#dashboard p.meta")).toBeTruthy());

    const stale: ArchiveStats = {
      ...ARCHIVE_STATS,
      total: 999999,
    };
    slow.resolve(stale);
    await flush();
    expect($("#dashboard p.meta").textContent).toContain("9 messages");
    expect($("#dashboard p.meta").textContent).not.toContain("999,999");
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
import type { EmbeddingStatus } from "./types";

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

  it("Update local embeddings submits a v2 background job and follows its events", async () => {
    tauri.handlers.set("embed_status", () => EMBED_STATUS);
    await boot();
    tauri.handlers.set(
      "job_events_since",
      () => ({ events: [], latest_seq: 0, oldest_kept: 1, trimmed: false, jobs: [] }),
    );
    tauri.handlers.set("job_submit", () => ({ id: 9 }));
    $("#embed-btn").click();
    await vi.waitFor(() =>
      expect(
        tauri.calls.some(
          (c) =>
            c.cmd === "job_submit" &&
            (c.args as { kind?: string } | undefined)?.kind === "embed_messages",
        ),
      ).toBe(true),
    );
    // The button is busy while the job runs (no v1 result awaited).
    expect($<HTMLButtonElement>("#embed-btn").disabled).toBe(true);
    expect($("#embed-status").textContent).toContain("Embedding…");

    // The jobs panel renders the job lifecycle from its events.
    const emit = (payload: unknown) =>
      tauriEvents.listeners.get("job-event")!({ payload });
    emit({ seq: 1, at: "t", job: 9, type: "submitted", kind: "embed_messages" });
    emit({ seq: 2, at: "t", job: 9, type: "started" });
    emit({
      seq: 3,
      at: "t",
      job: 9,
      type: "progress",
      done: 2,
      total: 4,
      message: "embedding",
    });
    expect($("#jobs-list").textContent).toContain("local embeddings — 2/4 (50%)");
    emit({ seq: 4, at: "t", job: 9, type: "succeeded", result: { embedded: 4 } });
    await vi.waitFor(() =>
      expect($("#jobs-list").textContent).toContain("local embeddings — done (4 embedded)"),
    );
    // Finished: the button re-enables.
    expect($<HTMLButtonElement>("#embed-btn").disabled).toBe(false);
  });

  it("an embedding job that fails to start is reported inline", async () => {
    tauri.handlers.set("embed_status", () => EMBED_STATUS);
    await boot();
    tauri.handlers.set("job_submit", () => Promise.reject("queue is full"));
    $("#embed-btn").click();
    await vi.waitFor(() =>
      expect($("#embed-status").textContent).toBe("Embedding job failed to start: queue is full"),
    );
    expect($<HTMLButtonElement>("#embed-btn").disabled).toBe(false);
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

// Folder watching
// ---------------------------------------------------------------------------

const EMPTY_WATCH: WatchSnapshot = {
  running: true,
  folders: [],
  pending: 0,
  events: [],
};

const ARMED_WATCH: WatchSnapshot = {
  running: true,
  folders: ["/home/me/exports"],
  pending: 1,
  events: [
    {
      kind: "imported",
      path: "/home/me/exports/mail.mbox",
      summary: { source: "mbox", seen: 3, inserted: 2, skipped: 1, failed: 0 },
    },
    { kind: "folder_added", path: "/home/me/exports" },
  ],
};

function watchCalls(): Array<{ cmd: string; args: unknown }> {
  return tauri.calls.filter((c) => c.cmd.startsWith("watch_"));
}

function emitWatch(snapshot: WatchSnapshot): void {
  const handler = tauriEvents.listeners.get("watch-status");
  if (!handler) throw new Error("no watch-status listener registered");
  handler({ payload: snapshot });
}

describe("folder watching", () => {
  it("boots with the empty status and a registered watch-status listener", async () => {
    tauri.handlers.set("watch_status", () => EMPTY_WATCH);
    await boot();
    await vi.waitFor(() =>
      expect($("#watch-status").textContent).toContain("No folders watched yet"),
    );
    expect(tauriEvents.listeners.has("watch-status")).toBe(true);
    expect(watchCalls()).toHaveLength(1); // only the initial watch_status
  });

  it("renders folders, pending count, and recent events from the snapshot", async () => {
    tauri.handlers.set("watch_status", () => ARMED_WATCH);
    await boot();
    await vi.waitFor(() =>
      expect($("#watch-status").textContent).toContain("Watching 1 folder"),
    );
    expect($("#watch-status").textContent).toContain("1 file settling");
    const folders = [...win.document.querySelectorAll("#watch-folders .watch-folder .path")];
    expect(folders.map((f) => f.textContent)).toEqual(["/home/me/exports"]);
    const events = [...win.document.querySelectorAll("#watch-events .watch-event")];
    expect(events.map((e) => e.textContent)).toEqual([
      "imported /home/me/exports/mail.mbox: 2 new, 1 already present",
      "now watching /home/me/exports",
    ]);
  });

  it("Add folder… picks a directory, invokes watch_add_folder, renders the result, refreshes stats", async () => {
    dialog.open.mockResolvedValue("/home/me/exports");
    tauri.handlers.set("watch_status", () => EMPTY_WATCH);
    tauri.handlers.set("watch_add_folder", () => ARMED_WATCH);
    await boot();

    const statsBefore = tauri.calls.filter((c) => c.cmd === "stats").length;
    $("#watch-add-btn").click();
    await vi.waitFor(() => expect(watchCalls().some((c) => c.cmd === "watch_add_folder")).toBe(true));

    expect(dialog.open).toHaveBeenCalledWith(
      expect.objectContaining({ directory: true, multiple: false }),
    );
    const add = watchCalls().find((c) => c.cmd === "watch_add_folder")!;
    expect(add.args).toEqual({ path: "/home/me/exports" });
    await vi.waitFor(() =>
      expect($("#watch-status").textContent).toContain("Watching 1 folder"),
    );
    // A new folder may have imported files: stats are refreshed.
    await vi.waitFor(() =>
      expect(tauri.calls.filter((c) => c.cmd === "stats").length).toBeGreaterThan(statsBefore),
    );
  });

  it("a cancelled folder pick invokes nothing", async () => {
    dialog.open.mockResolvedValue(null);
    tauri.handlers.set("watch_status", () => EMPTY_WATCH);
    await boot();
    $("#watch-add-btn").click();
    await vi.waitFor(() => expect(dialog.open).toHaveBeenCalled());
    await flush();
    expect(watchCalls().some((c) => c.cmd === "watch_add_folder")).toBe(false);
  });

  it("Watch with a typed path invokes watch_add_folder and clears the input", async () => {
    tauri.handlers.set("watch_status", () => EMPTY_WATCH);
    tauri.handlers.set("watch_add_folder", () => ARMED_WATCH);
    await boot();

    input("#watch-path").value = "/home/me/exports";
    pressKey(input("#watch-path"), "Enter");
    await vi.waitFor(() =>
      expect(watchCalls().some((c) => c.cmd === "watch_add_folder")).toBe(true),
    );
    const add = watchCalls().find((c) => c.cmd === "watch_add_folder")!;
    expect(add.args).toEqual({ path: "/home/me/exports" });
    await vi.waitFor(() => expect(input("#watch-path").value).toBe(""));
    await vi.waitFor(() =>
      expect($("#watch-status").textContent).toContain("Watching 1 folder"),
    );
  });

  it("Watch with an empty typed path shows a hint and invokes nothing", async () => {
    tauri.handlers.set("watch_status", () => EMPTY_WATCH);
    await boot();
    pressKey(input("#watch-path"), "Enter");
    await vi.waitFor(() =>
      expect($("#watch-status").textContent).toContain("Type a folder path first"),
    );
    expect(watchCalls().some((c) => c.cmd === "watch_add_folder")).toBe(false);
  });

  it("an add failure is shown as an error, not thrown", async () => {
    dialog.open.mockResolvedValue("/nope");
    tauri.handlers.set("watch_status", () => EMPTY_WATCH);
    tauri.handlers.set("watch_add_folder", () => Promise.reject("not a folder"));
    await boot();
    $("#watch-add-btn").click();
    await vi.waitFor(() =>
      expect($("#watch-status").textContent).toContain("Could not watch folder: not a folder"),
    );
    expect($<HTMLButtonElement>("#watch-add-btn").disabled).toBe(false);
  });

  it("a failed typed-path add keeps the typed path for correction", async () => {
    tauri.handlers.set("watch_status", () => EMPTY_WATCH);
    tauri.handlers.set("watch_add_folder", () => Promise.reject("not a folder"));
    await boot();

    input("#watch-path").value = "/home/me/exportz";
    pressKey(input("#watch-path"), "Enter");
    await vi.waitFor(() =>
      expect($("#watch-status").textContent).toContain("Could not watch folder"),
    );
    expect(input("#watch-path").value).toBe("/home/me/exportz");

    tauri.handlers.set("watch_add_folder", () => ARMED_WATCH);
    pressKey(input("#watch-path"), "Enter");
    await vi.waitFor(() => expect(input("#watch-path").value).toBe(""));
  });

  it("watch-status events re-render and an imported newest event refreshes stats", async () => {
    tauri.handlers.set("watch_status", () => EMPTY_WATCH);
    await boot();
    await vi.waitFor(() => expect($("#watch-status").textContent).toContain("No folders"));

    const statsBefore = tauri.calls.filter((c) => c.cmd === "stats").length;
    emitWatch(ARMED_WATCH);
    await vi.waitFor(() =>
      expect($("#watch-folders .watch-folder .path")!.textContent).toBe("/home/me/exports"),
    );
    // Newest event is "imported" → chips/stats refresh fires.
    await vi.waitFor(() =>
      expect(tauri.calls.filter((c) => c.cmd === "stats").length).toBe(statsBefore + 1),
    );
  });

  it("watch-status events with no import do not refresh stats", async () => {
    tauri.handlers.set("watch_status", () => EMPTY_WATCH);
    await boot();
    const statsBefore = tauri.calls.filter((c) => c.cmd === "stats").length;
    // A pending-only update (folder armed, newest event is not an import).
    emitWatch({
      running: true,
      folders: ["/home/me/exports"],
      pending: 2,
      events: [{ kind: "folder_added", path: "/home/me/exports" }],
    });
    await vi.waitFor(() => expect($("#watch-status").textContent).toContain("2 files settling"));
    expect(tauri.calls.filter((c) => c.cmd === "stats").length).toBe(statsBefore);
  });

  it("Remove invokes watch_remove_folder with the folder path", async () => {
    tauri.handlers.set("watch_status", () => ARMED_WATCH);
    tauri.handlers.set("watch_remove_folder", () => EMPTY_WATCH);
    await boot();
    await vi.waitFor(() => expect($("#watch-folders .watch-folder")).toBeTruthy());

    $("#watch-folders .watch-remove").click();
    await vi.waitFor(() =>
      expect(watchCalls().some((c) => c.cmd === "watch_remove_folder")).toBe(true),
    );
    const remove = watchCalls().find((c) => c.cmd === "watch_remove_folder")!;
    expect(remove.args).toEqual({ path: "/home/me/exports" });
    await vi.waitFor(() =>
      expect($("#watch-status").textContent).toContain("No folders watched yet"),
    );
  });
});

// IMAP sync auth mode (app password vs Google sign-in)
// ---------------------------------------------------------------------------

const SYNC_OUTCOME = {
  folders: [
    {
      folder: "INBOX",
      uidvalidity: 7,
      reset: true,
      seen: 2,
      inserted: 2,
      skipped: 0,
      failed: 0,
      error: null,
    },
  ],
  saved_to: null,
};

function imapCalls(): Array<{ cmd: string; args: unknown }> {
  return tauri.calls.filter((c) => c.cmd === "sync_imap");
}

function select(sel: string, value: string): void {
  const el = $<HTMLSelectElement>(sel);
  el.value = value;
  fireChange(el);
}

async function clickSync(): Promise<void> {
  ($<HTMLButtonElement>("#imap-sync-btn") as HTMLButtonElement).click();
  await vi.waitFor(() => expect(imapCalls().length).toBeGreaterThan(0));
}

describe("IMAP sync auth mode", () => {
  it("defaults to password mode: password field visible, sign-in hidden, auth sent as password", async () => {
    await boot();
    expect(input("#imap-pass").hidden).toBe(false);
    expect($<HTMLButtonElement>("#imap-oauth-btn").hidden).toBe(true);

    tauri.handlers.set("sync_imap", () => SYNC_OUTCOME);
    input("#imap-host").value = "imap.gmail.com";
    input("#imap-user").value = "you@gmail.com";
    input("#imap-pass").value = "abcd-efgh-ijkl-mnop";
    await clickSync();

    const args = imapCalls()[0].args as Record<string, unknown>;
    expect(args.auth).toBe("password");
    expect(args.password).toBe("abcd-efgh-ijkl-mnop");
    await vi.waitFor(() =>
      expect($("#imap-status").textContent).toContain("INBOX: 2 new"),
    );
  });

  it("switching to Google sign-in hides the password field, shows the sign-in button and a hint", async () => {
    await boot();
    tauri.handlers.set("oauth_status", () => ({ configured: true, signed_in: false }));
    input("#imap-host").value = "imap.gmail.com";
    input("#imap-user").value = "you@gmail.com";
    select("#imap-auth", "oauth");
    await flush();

    expect(input("#imap-pass").hidden).toBe(true);
    expect($<HTMLButtonElement>("#imap-oauth-btn").hidden).toBe(false);
    const calls = tauri.calls.filter((c) => c.cmd === "oauth_status");
    expect(calls.length).toBe(1);
    expect(calls[0].args).toEqual({ host: "imap.gmail.com", username: "you@gmail.com" });
    await vi.waitFor(() =>
      expect($("#imap-oauth-hint").textContent).toContain("Not signed in yet"),
    );
  });

  it("the sign-in button runs the browser flow and reports where the token was kept", async () => {
    await boot();
    tauri.handlers.set("oauth_status", () => ({ configured: true, signed_in: false }));
    input("#imap-user").value = "you@gmail.com";
    select("#imap-auth", "oauth");
    await flush();

    tauri.handlers.set("oauth_gmail_login", () => ({
      username: "you@gmail.com",
      saved_to: "os_keyring",
    }));
    ($<HTMLButtonElement>("#imap-oauth-btn") as HTMLButtonElement).click();
    await vi.waitFor(() =>
      expect($("#imap-status").textContent).toContain("Signed in as you@gmail.com"),
    );
    expect($("#imap-status").textContent).toContain("OS keyring");
    // The status hint refreshes to the signed-in wording.
    tauri.handlers.set("oauth_status", () => ({ configured: true, signed_in: true }));
    ($<HTMLButtonElement>("#imap-oauth-btn") as HTMLButtonElement).click();
    await vi.waitFor(() => expect($("#imap-oauth-hint").textContent).toContain("Signed in as"));
  });

  it("a failed sign-in surfaces the error and re-enables the button", async () => {
    await boot();
    tauri.handlers.set("oauth_status", () => ({ configured: true, signed_in: false }));
    input("#imap-user").value = "you@gmail.com";
    select("#imap-auth", "oauth");
    await flush();

    tauri.handlers.set("oauth_gmail_login", () => {
      throw new Error("sign-in timed out waiting for the browser redirect");
    });
    ($<HTMLButtonElement>("#imap-oauth-btn") as HTMLButtonElement).click();
    await vi.waitFor(() => expect($("#imap-status").textContent).toContain("Sign-in failed"));
    expect($<HTMLButtonElement>("#imap-oauth-btn").disabled).toBe(false);
  });

  it("an OAuth sync sends auth oauth and never the (hidden) password field value", async () => {
    await boot();
    tauri.handlers.set("oauth_status", () => ({ configured: true, signed_in: true }));
    input("#imap-host").value = "imap.gmail.com";
    input("#imap-user").value = "you@gmail.com";
    // A stale password left in the (now hidden) field must not be sent.
    input("#imap-pass").value = "stale-app-password";
    select("#imap-auth", "oauth");
    await flush();

    tauri.handlers.set("sync_imap", () => SYNC_OUTCOME);
    await clickSync();

    const args = imapCalls()[0].args as Record<string, unknown>;
    expect(args.auth).toBe("oauth");
    expect(args.password).toBe("");
  });

  it("an OAuth sync with an empty host field still reaches the backend (it defaults to Gmail)", async () => {
    await boot();
    tauri.handlers.set("oauth_status", () => ({ configured: true, signed_in: true }));
    input("#imap-host").value = "";
    input("#imap-user").value = "you@gmail.com";
    select("#imap-auth", "oauth");
    await flush();

    tauri.handlers.set("sync_imap", () => SYNC_OUTCOME);
    await clickSync();

    const args = imapCalls()[0].args as Record<string, unknown>;
    expect(args.host).toBe("");
    expect(args.auth).toBe("oauth");
    await vi.waitFor(() =>
      expect($("#imap-status").textContent).toContain("INBOX: 2 new"),
    );
  });

  it("switching back to password mode restores the password field and hides the button", async () => {
    await boot();
    tauri.handlers.set("oauth_status", () => ({ configured: true, signed_in: false }));
    select("#imap-auth", "oauth");
    await flush();
    select("#imap-auth", "password");
    await flush();

    expect(input("#imap-pass").hidden).toBe(false);
    expect($<HTMLButtonElement>("#imap-oauth-btn").hidden).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Ask provider menu (multi-provider)
// ---------------------------------------------------------------------------

const LOCAL_ITEM = {
  id: "local",
  name: "Local full-text",
  kind: "local",
  model: "",
  host: "",
  available: true,
};

const ZAI_ITEM = {
  id: "zai",
  name: "z.ai GLM",
  kind: "zai",
  model: "glm-5",
  host: "api.z.ai",
  available: true,
};

const KEYLESS_ITEM = {
  id: "mine",
  name: "Custom",
  kind: "custom",
  model: "m",
  host: "127.0.0.1",
  available: false,
};

function menuOptions(): Array<{ value: string; text: string; disabled: boolean }> {
  const select = $<HTMLSelectElement>("#ask-provider");
  return [...select.options].map((o) => ({
    value: o.value,
    text: o.textContent ?? "",
    disabled: o.disabled,
  }));
}

function emptyOutcome() {
  return {
    plan: { match_expr: "x", date_from: null, date_to: null },
    plan_source: "no_llm",
    ranking: "fts_only",
    results: [],
    answer: null,
    notes: [],
  };
}

describe("ask provider menu", () => {
  it("boots with the menu built from llm_status, default preselected", async () => {
    tauri.handlers.set("llm_status", () => ({
      enabled: true,
      model: "glm-5",
      host: "api.z.ai",
      summarize_top: 8,
      providers: [LOCAL_ITEM, ZAI_ITEM, KEYLESS_ITEM],
      default_provider: "zai",
    }));
    await boot();
    await vi.waitFor(() => expect(menuOptions().length).toBe(3));
    expect(menuOptions()).toEqual([
      { value: "local", text: "Local full-text (no LLM)", disabled: false },
      { value: "zai", text: "z.ai GLM (glm-5 · api.z.ai)", disabled: false },
      { value: "mine", text: "Custom (m · 127.0.0.1) — no API key", disabled: true },
    ]);
    expect(input("#ask-provider").value).toBe("zai");
    expect($("#llm-note").textContent).toContain("LLM assist is on (glm-5 via api.z.ai)");
    expect($("#llm-note").textContent).toContain("top 8 result messages");
  });

  it("preselects local-only when nothing is configured", async () => {
    tauri.handlers.set("llm_status", () => ({
      enabled: false,
      model: "",
      host: "",
      summarize_top: 8,
      providers: [LOCAL_ITEM],
      default_provider: null,
    }));
    await boot();
    await vi.waitFor(() => expect(input("#ask-provider").value).toBe("local"));
    expect($("#llm-note").textContent).toContain("LLM assist is off");
    expect($("#llm-note").textContent).toContain("nothing leaves this machine");
  });

  it("an ask sends the selected provider id and the note follows selection", async () => {
    tauri.handlers.set("llm_status", () => ({
      enabled: true,
      model: "glm-5",
      host: "api.z.ai",
      summarize_top: 8,
      providers: [LOCAL_ITEM, ZAI_ITEM],
      default_provider: "zai",
    }));
    tauri.handlers.set("llm_ask", () => emptyOutcome());
    await boot();
    await vi.waitFor(() => expect(input("#ask-provider").value).toBe("zai"));

    // Switch to local mode: the privacy note switches before asking.
    input("#ask-provider").value = "local";
    fireChange(input("#ask-provider"));
    expect($("#llm-note").textContent).toContain("LLM assist is off");

    input("#ask").value = "vault?";
    pressKey(input("#ask"), "Enter");
    await vi.waitFor(() =>
      expect(tauri.calls.find((c) => c.cmd === "llm_ask")).toBeTruthy(),
    );
    expect(tauri.calls.find((c) => c.cmd === "llm_ask")!.args).toEqual({
      question: "vault?",
      limit: 50,
      provider: "local",
    });

    // And back to the provider for the next ask.
    input("#ask-provider").value = "zai";
    fireChange(input("#ask-provider"));
    expect($("#llm-note").textContent).toContain("LLM assist is on");
    input("#ask").value = "again";
    pressKey(input("#ask"), "Enter");
    await vi.waitFor(() => expect(tauri.calls.filter((c) => c.cmd === "llm_ask").length).toBe(2));
    expect(tauri.calls.filter((c) => c.cmd === "llm_ask")[1].args).toEqual({
      question: "again",
      limit: 50,
      provider: "zai",
    });
  });

  it("a menu rebuild keeps a still-valid selection, else falls back to the default", async () => {
    let status = {
      enabled: true,
      model: "glm-5",
      host: "api.z.ai",
      summarize_top: 8,
      providers: [LOCAL_ITEM, ZAI_ITEM, KEYLESS_ITEM],
      default_provider: "zai",
    };
    tauri.handlers.set("llm_status", () => status);
    tauri.handlers.set("settings_save", (args: unknown) => {
      void (args as { payload: unknown }).payload;
      // Simulate: the keyless provider got a key, zai was removed.
      status = {
        enabled: true,
        model: "m",
        host: "127.0.0.1",
        summarize_top: 8,
        providers: [LOCAL_ITEM, { ...KEYLESS_ITEM, available: true }],
        default_provider: "mine",
      };
      return viewWith((v) => {
        v.default_provider = "mine";
        v.providers = [customProvider({ has_key: true })];
      });
    });
    await boot();
    await vi.waitFor(() => expect(input("#ask-provider").value).toBe("zai"));

    // User picks mine; it is disabled but selectable state is only guarded
    // on rebuild, so first make it available, then keep it across a save.
    status = { ...status, providers: [LOCAL_ITEM, ZAI_ITEM, { ...KEYLESS_ITEM, available: true }] };
    // Save triggers a menu rebuild: selection "zai" no longer exists → the
    // new default "mine" is preselected.
    $("#settings-save-btn").click();
    await vi.waitFor(() => expect(input("#ask-provider").value).toBe("mine"));
    expect(menuOptions().find((o) => o.value === "zai")).toBeUndefined();
    expect($("#llm-note").textContent).toContain("LLM assist is on (m via 127.0.0.1)");
  });

  it("a save that does not change the default keeps the user's selection", async () => {
    let status = {
      enabled: true,
      model: "glm-5",
      host: "api.z.ai",
      summarize_top: 8,
      providers: [LOCAL_ITEM, ZAI_ITEM],
      default_provider: "zai",
    };
    tauri.handlers.set("llm_status", () => status);
    tauri.handlers.set("settings_save", () => viewWith()); // same default "zai"
    await boot();
    await vi.waitFor(() => expect(input("#ask-provider").value).toBe("zai"));

    // The user deliberately switches to local, then saves an unrelated
    // change (theme): their selection survives the rebuild.
    input("#ask-provider").value = "local";
    fireChange(input("#ask-provider"));
    input("#theme").value = "dark";
    fireChange(input("#theme"));
    $("#settings-save-btn").click();
    await vi.waitFor(() => expect($("#settings-note").textContent).toContain("Saved."));
    await flush();
    expect(input("#ask-provider").value).toBe("local");
    expect($("#llm-note").textContent).toContain("LLM assist is off");
  });
});

// ---------------------------------------------------------------------------
// Settings screen
// ---------------------------------------------------------------------------

function zaiProvider(extra: Partial<Record<string, unknown>> = {}) {
  return {
    id: "zai",
    name: "",
    kind: "zai",
    base_url: "",
    model: "",
    has_key: true,
    ...extra,
  };
}

function customProvider(extra: Partial<Record<string, unknown>> = {}) {
  return {
    id: "mine",
    name: "Mine",
    kind: "custom",
    base_url: "http://127.0.0.1:9/api",
    model: "m",
    has_key: false,
    ...extra,
  };
}

function viewWith(mutate: (v: Record<string, unknown>) => void = () => {}): Record<string, unknown> {
  const view = {
    providers: [zaiProvider()],
    default_provider: "zai",
    theme: "system",
    summarize_top: 8,
    env: null,
    env_error: null,
    settings_error: null,
    key_store: null,
  } as Record<string, unknown>;
  mutate(view);
  return view;
}

function rowIds(): string[] {
  return [...win.document.querySelectorAll(".provider-row")].map(
    (r) => (r as unknown as HTMLElement).dataset.id!,
  );
}

function rowEl(id: string): HTMLElement {
  return $<HTMLElement>(`.provider-row[data-id="${id}"]`);
}

function rowInput(id: string, cls: string): HTMLInputElement {
  return rowEl(id).querySelector(`.${cls}`) as unknown as HTMLInputElement;
}

describe("settings screen", () => {
  it("renders providers, the default radio, options, and env diagnostics", async () => {
    tauri.handlers.set("settings_get", () =>
      viewWith((v) => {
        v.providers = [zaiProvider(), customProvider()];
        v.default_provider = "mine";
        v.theme = "dark";
        v.summarize_top = 12;
        v.env = { model: "env-model", host: "127.0.0.1" };
        v.env_error = null;
        v.key_store = "os_keyring";
      }),
    );
    await boot();
    await vi.waitFor(() => expect(rowIds()).toEqual(["zai", "mine"]));
    // Row contents: kind label, values, key placeholder for a stored key.
    expect($<HTMLElement>('.provider-row[data-id="zai"] .kind-label').textContent)
      .toBe("z.ai GLM");
    expect(rowInput("zai", "prov-name").value).toBe("");
    expect(rowInput("zai", "prov-url").value).toBe("");
    expect(rowInput("zai", "prov-url").placeholder).toContain("api.z.ai");
    expect(rowInput("zai", "prov-key").placeholder).toContain("stored");
    expect(rowInput("mine", "prov-url").value).toBe("http://127.0.0.1:9/api");
    // Default radio on the right row, theme and option applied.
    const checked = $<HTMLInputElement>('input[name="default-provider"]:checked');
    expect(checked?.value).toBe("mine");
    expect(input("#theme").value).toBe("dark");
    expect(input("#summarize-top").value).toBe("12");
    expect(document.documentElement.style.colorScheme).toBe("dark");
    // Key-store note and the environment fallback line.
    expect($("#settings-note").textContent).toContain("OS keyring");
    expect($("#env-info").textContent).toContain("Environment fallback: env-model via 127.0.0.1");
  });

  it("surfaces env pairing errors and settings-file problems", async () => {
    tauri.handlers.set("settings_get", () =>
      viewWith((v) => {
        v.env_error = "ANTHROPIC_API_KEY is set but ANTHROPIC_BASE_URL is not";
        v.settings_error = "settings error: invalid settings.json";
      }),
    );
    await boot();
    await vi.waitFor(() => expect($("#env-info").textContent).toContain("ANTHROPIC_BASE_URL"));
    expect($("#env-info").textContent).toContain("Settings file problem");
  });

  it("adds provider rows with unique ids and per-kind prefills", async () => {
    await boot();
    input("#add-kind").value = "anthropic";
    $("#add-provider-btn").click();
    input("#add-kind").value = "custom";
    $("#add-provider-btn").click();
    $("#add-provider-btn").click();
    await vi.waitFor(() => expect(rowIds()).toEqual(["anthropic", "custom", "custom-2"]));
    expect(rowInput("anthropic", "prov-url").value).toBe("https://api.anthropic.com");
    expect(rowInput("anthropic", "prov-model").value).not.toBe("");
    expect(rowInput("custom", "prov-url").value).toBe("");
  });

  it("saving sends the collected form plus non-empty key updates, and re-renders", async () => {
    let saved: { cmd: string; args: unknown } | undefined;
    tauri.handlers.set("settings_get", () => viewWith());
    tauri.handlers.set("settings_save", (args: unknown) => {
      saved = { cmd: "settings_save", args: (args as { payload: unknown }).payload };
      return viewWith((v) => {
        v.theme = "light";
        v.key_store = "memory";
      });
    });
    tauri.handlers.set("llm_status", () => ({
      enabled: true,
      model: "glm-5",
      host: "api.z.ai",
      summarize_top: 8,
      providers: [LOCAL_ITEM, ZAI_ITEM],
      default_provider: "zai",
    }));
    await boot();

    // Edit the zai row: name + a new key; add a custom provider with a key.
    rowInput("zai", "prov-name").value = "Work GLM";
    rowInput("zai", "prov-key").value = "  secret-1  ";
    input("#add-kind").value = "custom";
    $("#add-provider-btn").click();
    await vi.waitFor(() => expect(rowIds()).toEqual(["zai", "custom"]));
    rowInput("custom", "prov-url").value = "http://127.0.0.1:9/api";
    rowInput("custom", "prov-model").value = "m";
    rowInput("custom", "prov-key").value = "secret-2";
    input("#summarize-top").value = "20";
    input("#theme").value = "light";

    $("#settings-save-btn").click();
    await vi.waitFor(() => expect($("#settings-note").textContent).toContain("Saved."));
    expect(saved!.args).toEqual({
      providers: [
        { id: "zai", name: "Work GLM", kind: "zai", base_url: "", model: "" },
        { id: "custom", name: "", kind: "custom", base_url: "http://127.0.0.1:9/api", model: "m" },
      ],
      default_provider: "zai",
      theme: "light",
      summarize_top: 20,
      api_keys: [
        { id: "zai", key: "secret-1" }, // trimmed
        { id: "custom", key: "secret-2" },
      ],
    });
    // The response re-renders: key inputs reset, theme applied, note names
    // the fallback store.
    expect(rowInput("zai", "prov-key").value).toBe("");
    expect(document.documentElement.style.colorScheme).toBe("light");
    expect($("#settings-note").textContent).toContain("session memory");
  });

  it("a validation refusal shows the backend error and saves nothing", async () => {
    tauri.handlers.set("settings_get", () => viewWith());
    tauri.handlers.set(
      "settings_save",
      () => Promise.reject("settings error: provider “Mine”: a custom provider needs a base URL"),
    );
    await boot();
    $("#settings-save-btn").click();
    await vi.waitFor(() => expect($("#settings-note").textContent).toContain("Not saved"));
    expect($("#settings-note").textContent).toContain("needs a base URL");
    expect($("#settings-note").classList.contains("error")).toBe(true);
    // The form keeps the user's edits (no re-render on failure).
    expect(rowIds()).toEqual(["zai"]);
  });

  it("remove drops a row from the form; clear-key asks the backend and re-renders", async () => {
    tauri.handlers.set("settings_get", () =>
      viewWith((v) => {
        v.providers = [zaiProvider(), customProvider({ has_key: true })];
      }),
    );
    tauri.handlers.set("llm_key_clear", (args: unknown) => {
      const { id } = args as { id: string };
      expect(id).toBe("mine");
      return viewWith((v) => {
        v.providers = [zaiProvider(), customProvider({ has_key: false })];
      });
    });
    await boot();
    await vi.waitFor(() => expect(rowIds()).toEqual(["zai", "mine"]));

    // Clear key: one command with the row id.
    ($<HTMLElement>('.provider-row[data-id="mine"] .clear-key-btn')).click();
    await vi.waitFor(() =>
      expect(tauri.calls.find((c) => c.cmd === "llm_key_clear")).toBeTruthy(),
    );
    await vi.waitFor(() =>
      expect($<HTMLElement>('.provider-row[data-id="mine"] .clear-key-btn').hidden).toBe(true),
    );

    // Remove: the row disappears client-side (picked up by the next save).
    ($<HTMLElement>('.provider-row[data-id="mine"] .remove-prov-btn')).click();
    await vi.waitFor(() => expect(rowIds()).toEqual(["zai"]));
  });

  it("theme changes apply immediately, not only after save", async () => {
    tauri.handlers.set("settings_get", () => viewWith((v) => (v.theme = "system")));
    await boot();
    await vi.waitFor(() => expect(input("#theme").value).toBe("system"));
    input("#theme").value = "dark";
    fireChange(input("#theme"));
    expect(document.documentElement.style.colorScheme).toBe("dark");
  });
});

// ---------------------------------------------------------------------------
// Auto-sync (the opt-in periodic scheduler, M2.4)
// ---------------------------------------------------------------------------

const SCHEDULE_OFF: SyncScheduleView = {
  enabled: false,
  interval_minutes: 15,
  active_job: null,
  next_due_in_secs: null,
  rounds_submitted: 0,
};

describe("auto-sync", () => {
  it("boots with the schedule off and renders the off state", async () => {
    tauri.handlers.set("sync_schedule_get", () => SCHEDULE_OFF);
    await boot();
    await vi.waitFor(() =>
      expect(tauri.calls.some((c) => c.cmd === "sync_schedule_get")).toBe(true),
    );
    expect(input("#auto-sync-toggle").checked).toBe(false);
    expect($<HTMLSelectElement>("#auto-sync-interval").value).toBe("15");
    expect($("#auto-sync-status").textContent).toContain("Auto-sync off");
  });

  it("checking the toggle enables the schedule with the chosen interval", async () => {
    tauri.handlers.set("sync_schedule_get", () => SCHEDULE_OFF);
    tauri.handlers.set("sync_schedule_set", (args) => {
      expect(args).toEqual({ enabled: true, intervalMinutes: 30 });
      return {
        enabled: true,
        interval_minutes: 30,
        active_job: null,
        next_due_in_secs: 1800,
        rounds_submitted: 0,
      };
    });
    await boot();
    input("#auto-sync-interval").value = "30";
    fireChange(input("#auto-sync-interval"));
    input("#auto-sync-toggle").checked = true;
    fireChange(input("#auto-sync-toggle"));
    await vi.waitFor(() =>
      expect(tauri.calls.some((c) => c.cmd === "sync_schedule_set")).toBe(true),
    );
    expect($("#auto-sync-status").textContent).toContain("Auto-sync on");
    expect($("#auto-sync-status").textContent).toContain("~30 min");
  });

  it("scheduler-status events re-render the panel (active round)", async () => {
    tauri.handlers.set("sync_schedule_get", () => SCHEDULE_OFF);
    await boot();
    const listener = tauriEvents.listeners.get("scheduler-status");
    expect(listener).toBeTruthy();
    listener!({ payload: { ...SCHEDULE_OFF, enabled: true, active_job: 7 } });
    expect(input("#auto-sync-toggle").checked).toBe(true);
    expect($("#auto-sync-status").textContent).toContain("job 7");
  });

  it("a failed change surfaces the error instead of guessing state", async () => {
    tauri.handlers.set("sync_schedule_get", () => SCHEDULE_OFF);
    tauri.handlers.set("sync_schedule_set", () => Promise.reject("vault busy"));
    await boot();
    input("#auto-sync-toggle").checked = true;
    fireChange(input("#auto-sync-toggle"));
    await vi.waitFor(() =>
      expect($("#auto-sync-status").textContent).toContain("Auto-sync change failed"),
    );
  });

  it("changing the interval while off remembers the choice without a command", async () => {
    tauri.handlers.set("sync_schedule_get", () => SCHEDULE_OFF);
    await boot();
    input("#auto-sync-interval").value = "60";
    fireChange(input("#auto-sync-interval"));
    await flush();
    expect(tauri.calls.some((c) => c.cmd === "sync_schedule_set")).toBe(false);
    expect(input("#auto-sync-interval").value).toBe("60");
  });
});

// ---------------------------------------------------------------------------
// Workspace shell (M3.1): layout states, navigation, reading pane
// ---------------------------------------------------------------------------

describe("workspace shell", () => {
  function app(): HTMLElement {
    return $("#app");
  }

  function setViewportWidth(width: number): void {
    win.innerWidth = width;
    win.dispatchEvent(new win.Event("resize"));
  }

  function openDetailViaResult(): Promise<void> {
    searchRespondsWith(() => pageOf(HITS, 3));
    tauri.handlers.set("conversation_context_v2", () => CTX);
    return (async () => {
      input("#search").value = "vault";
      pressKey(input("#search"), "Enter");
      await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
      ($("#results .hit-item") as HTMLElement).click();
      await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    })();
  }

  it("ships the semantic landmarks, labels, and live regions", async () => {
    await boot();
    expect(win.document.querySelector('nav#nav[aria-label="Workspace"]')).toBeTruthy();
    expect(win.document.querySelector("main#main")).toBeTruthy();
    expect(win.document.querySelector('aside#reading[aria-label="Reading"]')).toBeTruthy();
    expect(win.document.querySelector("#status")?.getAttribute("role")).toBe("status");
    expect(win.document.querySelector("#stats")?.getAttribute("role")).toBe("status");
    // The search box is labelled (visually-hidden label).
    expect(win.document.querySelector('label[for="search"]')).toBeTruthy();
    // The reading pane's back action is a real, labelled button.
    const back = $("#back-to-results") as HTMLButtonElement;
    expect(back.textContent).toContain("Back to results");
  });

  it("navigation switches views with aria-current, results stay mounted", async () => {
    await boot();
    await openDetailViaResult();
    const settingsBtn = $(".nav-view-btn[data-view-target='settings']");
    settingsBtn.click();
    expect(app().dataset.view).toBe("settings");
    expect(settingsBtn.getAttribute("aria-current")).toBe("page");
    // Leaving and returning preserves the open reading pane and results.
    $(".nav-view-btn[data-view-target='workspace']").click();
    expect(app().dataset.view).toBe("workspace");
    expect($("#detail").classList.contains("open")).toBe(true);
    expect(app().dataset.reading).toBe("open");
  });

  it("selecting a source chip from another view returns to the workspace", async () => {
    await boot();
    $(".nav-view-btn[data-view-target='settings']").click();
    expect(app().dataset.view).toBe("settings");
    searchRespondsWith(() => pageOf(HITS, 3));
    $('[data-source="mbox"]').click();
    await flush();
    expect(app().dataset.view).toBe("workspace");
    expect(tauri.calls.some((c) => c.cmd === "search_archive_v2")).toBe(true);
  });

  it("resize drives the layout state through wide/medium/compact", async () => {
    await boot();
    setViewportWidth(1280);
    expect(app().dataset.layout).toBe("wide");
    setViewportWidth(900);
    expect(app().dataset.layout).toBe("medium");
    setViewportWidth(640);
    expect(app().dataset.layout).toBe("compact");
    // 200% zoom on a 1280px window is the compact state, not a clipped wide.
    setViewportWidth(640);
    expect(app().dataset.layout).toBe("compact");
  });

  it("compact: the navigation drawer opens, announces, and Escape closes it first", async () => {
    await boot();
    setViewportWidth(640);
    const toggle = $("#nav-toggle") as HTMLButtonElement;
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    toggle.click();
    expect(app().dataset.nav).toBe("open");
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    pressKey(win.document, "Escape");
    expect(app().dataset.nav).toBe("closed");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
  });

  it("Escape closes the drawer before the reading pane", async () => {
    await boot();
    setViewportWidth(640);
    await openDetailViaResult();
    $("#nav-toggle").click();
    expect(app().dataset.nav).toBe("open");
    pressKey(win.document, "Escape");
    expect(app().dataset.nav).toBe("closed");
    expect($("#detail").classList.contains("open")).toBe(true); // reading survives
    pressKey(win.document, "Escape");
    expect($("#detail").classList.contains("open")).toBe(false);
  });

  it("opening a message focuses its heading in swapped layouts, Back restores the clicked row", async () => {
    await boot();
    setViewportWidth(900); // medium: reading replaces the results area
    searchRespondsWith(() => pageOf(HITS, 3));
    tauri.handlers.set("conversation_context_v2", () => CTX);
    input("#search").value = "vault";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    const row = $("#results .hit-item") as HTMLElement;
    row.focus();
    row.click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    const heading = $("#detail .detail-card h2");
    expect(win.document.activeElement).toBe(heading);
    $("#back-to-results").click();
    expect($("#detail").classList.contains("open")).toBe(false);
    expect(app().dataset.reading).toBe("closed");
    // The initiating result row receives focus again.
    const active = win.document.activeElement as unknown as HTMLElement | null;
    expect(active?.classList.contains("hit-item")).toBe(true);
    expect(active?.getAttribute("aria-current")).toBe("true");
  });

  it("Enter on a focused result row opens that row's message", async () => {
    await bootSearched();
    const rows = win.document.querySelectorAll("#results .hit-item") as unknown as HTMLElement[];
    const row = rows[2];
    expect(row.getAttribute("role")).toBe("button");
    row.focus();
    pressKey(row, "Enter");
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    expect(ctxCalls().at(-1)?.args).toMatchObject({ id: Number(row.dataset.id) });
  });

  it("opening a message in wide layout keeps focus on the current thread row", async () => {
    await boot();
    setViewportWidth(1280);
    searchRespondsWith(() => pageOf(HITS, 3));
    tauri.handlers.set("conversation_context_v2", () => CTX);
    input("#search").value = "vault";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    const row = $("#results .hit-item") as HTMLElement;
    row.focus();
    row.click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    expect(win.document.activeElement).toBe($<HTMLElement>("#detail .thread-row.current"));
  });

  it("Space on a focused thread row opens that message", async () => {
    await bootSearched();
    setViewportWidth(1280);
    await openDetailViaResult();
    const threadRow = $<HTMLElement>("#detail .thread-row:not(.current)");
    threadRow.focus();
    pressKey(threadRow, " ");
    await vi.waitFor(() => expect(ctxCalls().length).toBeGreaterThan(1));
    expect(ctxCalls().at(-1)?.args).toMatchObject({ id: Number(threadRow.dataset.id) });
  });

  it("arrow thread navigation keeps focus on the current thread row", async () => {
    await bootSearched();
    setViewportWidth(1280);
    const all = [...CTX.before, CTX.anchor, ...CTX.after];
    const ctxFor = (id: number): ConversationContext => {
      const at = all.findIndex((m) => m.id === id);
      return {
        anchor: { ...CTX.anchor, id },
        before: all.slice(0, at).filter((m) => m.id !== id),
        after: all.slice(at + 1),
        unverified_conversation: false,
        has_more_before: false,
        has_more_after: false,
      };
    };
    tauri.handlers.set("conversation_context_v2", (args: unknown) =>
      ctxFor((args as { id: number }).id),
    );
    input("#search").value = "vault";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    ($("#results .hit-item") as HTMLElement).click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    const threadRow = $<HTMLElement>("#detail .thread-row:not(.current)");
    threadRow.focus();
    pressKey(threadRow, "ArrowDown");
    await vi.waitFor(() =>
      expect($("#detail .thread-row.current")?.getAttribute("data-id")).toBe("12"),
    );
    const current = $<HTMLElement>("#detail .thread-row.current");
    expect(win.document.activeElement).toBe(current);
    expect(current.getAttribute("tabindex")).toBe("-1");
  });

  it("ArrowDown from a focused row keeps focus on the new active row", async () => {
    await bootSearched();
    const row = $("#results .hit-item") as HTMLElement;
    row.focus();
    pressKey(win.document, "ArrowDown");
    const active = win.document.activeElement as unknown as HTMLElement | null;
    expect(active?.classList.contains("hit-item")).toBe(true);
    expect(active?.getAttribute("aria-current")).toBe("true");
    const activeRow = $<HTMLElement>("#results .hit-item.active");
    expect(activeRow.dataset.index).toBe("0");
  });

  it("arrows and Enter on a focused select do not hijack result navigation", async () => {
    await bootSearched();
    pressKey(win.document, "ArrowDown"); // select row 0 in the results list
    expect($<HTMLElement>("#results .hit-item.active").dataset.index).toBe("0");
    const select = $("#export-format");
    select.focus();
    pressKey(select, "ArrowDown");
    expect($<HTMLElement>("#results .hit-item.active").dataset.index).toBe("0");
    const callsBefore = ctxCalls().length;
    pressKey(select, "Enter");
    expect(ctxCalls().length).toBe(callsBefore);
  });

  it("in wide layout opening a message does not steal focus", async () => {
    await boot();
    setViewportWidth(1280);
    $("h1").focus();
    await openDetailViaResult();
    expect(win.document.activeElement?.tagName).toBe("H1");
    expect(app().dataset.reading).toBe("open");
    // Back is a no-op visually in wide, but still closes nothing.
    $("#back-to-results").click();
    expect($("#detail").classList.contains("open")).toBe(false);
  });

  it("closing the reading pane in wide layout restores focus to the active row", async () => {
    await boot();
    setViewportWidth(1280);
    searchRespondsWith(() => pageOf(HITS, 3));
    tauri.handlers.set("conversation_context_v2", () => CTX);
    input("#search").value = "vault";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    const row = $("#results .hit-item") as HTMLElement;
    row.focus();
    row.click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    const closeBtn = $<HTMLElement>(".close-btn");
    closeBtn.focus();
    closeBtn.click();
    expect($("#detail").classList.contains("open")).toBe(false);
    const active = win.document.activeElement as unknown as HTMLElement | null;
    expect(active?.classList.contains("hit-item")).toBe(true);
    expect(active?.getAttribute("aria-current")).toBe("true");
  });

  it("Ctrl/Cmd+K focuses the search box from anywhere", async () => {
    await boot();
    $("h1").focus();
    win.document.dispatchEvent(
      new win.KeyboardEvent("keydown", { key: "k", ctrlKey: true, bubbles: true, cancelable: true }),
    );
    expect(win.document.activeElement?.id).toBe("search");
    win.document.dispatchEvent(
      new win.KeyboardEvent("keydown", { key: "k", metaKey: true, bubbles: true, cancelable: true }),
    );
    expect(win.document.activeElement?.id).toBe("search");
  });

  it("'/' is ignored while typing in a field", async () => {
    await boot();
    const author = input("#author");
    author.focus();
    pressKey(author, "/");
    expect(win.document.activeElement).toBe(author);
  });
});

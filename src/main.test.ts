// @vitest-environment happy-dom
// Integration tests for the UI wiring in src/main.ts: the real index.html
// markup is booted against a mocked Tauri IPC bridge and driven with real
// DOM events (key presses, clicks, changes). Covers search triggering and
// the empty-query guard, loading/empty/error states, filter interactions,
// flat results, pagination (including the stale-response guard), the
// detail view (open/close/context walking), and keyboard navigation.
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

// Whole-window drag-drop import: the Tauri webview API is faked the same
// way — the drag-drop handler is captured so tests can emit the
// enter/over/drop/leave payloads the native events deliver (an OS drag
// itself cannot be synthesized in happy-dom, the same stance as the
// pickers).
const tauriWebview = vi.hoisted(() => ({
  dragHandlers: [] as Array<(e: { payload: unknown }) => void>,
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: (handler: (e: { payload: unknown }) => void) => {
      tauriWebview.dragHandlers.push(handler);
      return Promise.resolve(() => {});
    },
  }),
}));

// Desktop notifications: the plugin surface is faked per test (sent
// titles/bodies land in `notifications.sent`); permission defaults to
// granted so tests exercise the decision layer, not the OS.
const notifications = vi.hoisted(() => ({
  granted: true,
  requested: 0,
  requestAnswer: true as boolean,
  sent: [] as Array<{ title: string; body: string }>,
}));
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: () => notifications.granted,
  requestPermission: () => {
    notifications.requested += 1;
    return Promise.resolve(notifications.requestAnswer ? "granted" : "denied");
  },
  sendNotification: (options: { title: string; body?: string }) => {
    notifications.sent.push({ title: options.title, body: options.body ?? "" });
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
  // Wait until the initial stats refresh settled (label replaced or
  // cleared) AND the initial newest-first browse (U04) settled, then
  // splice the boot-time browse out of the call log so per-test
  // call-count assertions stay about the actions the test itself drives
  // (the boot browse has its own tests below).
  await vi.waitFor(() => expect($("#stats").textContent).not.toBe("…"));
  await vi.waitFor(() => expect($("#status").textContent).not.toBe("Loading…"));
  tauri.calls = tauri.calls.filter((c) => c.cmd !== "search_archive_v2");
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
  // The boot-time newest-first browse and the demo-status probe (U02/U04)
  // need defaults; individual tests override.
  tauri.handlers.set("search_archive_v2", () => pageOf([], 0));
  tauri.handlers.set("demo_status", () => Promise.resolve({ active: false }));
  // Notifications boot fail-closed (silent until settings load), so every
  // notify test needs the settings round-trip to resolve.
  tauri.handlers.set("settings_get", () => viewWith());
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
  tauriWebview.dragHandlers.length = 0;
  dialog.open.mockReset();
  dialog.savedPath = null;
  notifications.granted = true;
  notifications.requested = 0;
  notifications.requestAnswer = true;
  notifications.sent.length = 0;
  // The multi-account overview boots with the empty state by default
  // (individual tests override the handler with fixtures).
  tauri.handlers.set("accounts_list", () => []);
});

afterEach(() => {
  vi.unstubAllGlobals();
});

const flush = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

// ---------------------------------------------------------------------------
// Search box
// ---------------------------------------------------------------------------

describe("search box", () => {
  it("boots with stats, chips, and the initial newest-first browse", async () => {
    await boot();
    expect($("#stats").textContent).toBe("9 messages (Mail import: 6, demo: 3)");
    // U04: boot ends in a browse, not a dead prompt.
    expect($("#status").textContent).toBe("0 results");
    expect($("#status").classList.contains("error")).toBe(false);
    expect([...win.document.querySelectorAll(".chip")].map((c) => c.textContent)).toEqual([
      "Mail import (6)",
      "demo (3)",
    ]);
    expect($("#results").children.length).toBe(0);
    expect($(".page-info").textContent).toBe("0 results");
    expect($("#first-run").getAttribute("hidden")).not.toBeNull();
  });

  it("an empty or whitespace-only query browses instead of refusing", async () => {
    await boot();
    input("#search").value = "   ";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect((searchCalls()[0].args as { request: { text: string } }).request.text).toBe("");
    await vi.waitFor(() => expect($("#status").textContent).toBe("0 results"));
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
        time_assumed: null,
        grouping_unverified: null,
        files_missing: null,
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
    ($('[data-source="inst-mbox"]') as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({
      request: { text: "", source_ids: ["inst-mbox"] },
    });
    // Browse mode: no query echo in the status line.
    expect($("#status").textContent).toBe("1–6 of 6");
    const chip = $('[data-source="inst-mbox"]');
    expect(chip.classList.contains("on")).toBe(true);
    expect(chip.getAttribute("aria-pressed")).toBe("true");

    // Toggling the only filter off returns to the unfiltered browse
    // (U04: empty text browses — the second IPC round-trip).
    searchRespondsWith(() => pageOf(HITS, 9));
    ($('[data-source="inst-mbox"]') as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(2));
    expect(searchCalls()[1].args).toMatchObject({
      request: { text: "", source_ids: [] },
    });
    expect($('[data-source="inst-mbox"]')).not.toBeNull();
    expect($('[data-source="inst-mbox"]').classList.contains("on")).toBe(false);
    expect($("#status").textContent).toBe("1–9 of 9");
    expect(win.document.querySelectorAll("#results .hit-item").length).toBe(3);
  });

  it("two same-named sources stay independent chips keyed by id", async () => {
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
    await vi.waitFor(() => expect(win.document.querySelectorAll(".chip").length).toBe(3));
    const chips = [...win.document.querySelectorAll(".chip")] as unknown as HTMLElement[];
    expect(chips.map((c) => c.dataset.source)).toEqual([
      "inst-mbox",
      "inst-mbox2",
      "inst-demo",
    ]);
    // The shared label is disambiguated and loses the (ambiguous) count;
    // the unique label keeps its count.
    expect(chips.map((c) => c.textContent)).toEqual([
      "Mail import · import",
      "Mail import",
      "demo (3)",
    ]);
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 6));
    chips[0].click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({
      request: { text: "", source_ids: ["inst-mbox"] },
    });
  });

  it("removing one same-named source chip keeps the other instance selected", async () => {
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
    await vi.waitFor(() => expect(win.document.querySelectorAll(".chip").length).toBe(3));
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 6));
    ($('[data-source="inst-mbox2"]') as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    ($('[data-source="inst-mbox"]') as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(2));
    expect(searchCalls()[1].args).toMatchObject({
      request: { source_ids: ["inst-mbox2", "inst-mbox"] },
    });
    // One applied filter chip per instance id, labeled distinctly.
    const chips = [...win.document.querySelectorAll("#active-filter-chips .filter-chip")];
    expect(chips.map((c) => c.textContent?.trim())).toEqual([
      "Mail import ×",
      "Mail import · import ×",
    ]);
    // Removing one leaves the other instance's filter in place.
    (chips[0] as unknown as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(3));
    expect(searchCalls()[2].args).toMatchObject({
      request: { source_ids: ["inst-mbox"] },
    });
    expect(win.document.querySelectorAll("#active-filter-chips .filter-chip").length).toBe(1);
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

  it("Clear resets every filter and re-browses unfiltered", async () => {
    await boot();
    await vi.waitFor(() => expect(win.document.querySelectorAll(".chip").length).toBe(2));
    searchRespondsWith(() => pageOf(HITS, 9));
    ($('[data-source="inst-mbox"]') as HTMLElement).click();
    await vi.waitFor(() => expect(searchCalls().length).toBe(1));
    input("#author").value = "bruno";
    input("#from-date").value = "2026-01-01";
    input("#to-date").value = "2026-01-31";

    $("#clear-filters").click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(2)); // clear re-browses (U04)
    expect(input("#author").value).toBe("");
    expect(input("#from-date").value).toBe("");
    expect(input("#to-date").value).toBe("");
    expect($('[data-source="inst-mbox"]').classList.contains("on")).toBe(false);
    expect($('[data-source="inst-mbox"]').getAttribute("aria-pressed")).toBe("false");
    expect($("#status").textContent).toBe("1–9 of 9");
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

  it("the honesty date filter sends time_assumed on change (true, false, off)", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 1));
    const select = $<HTMLSelectElement>("#time-assumed");
    select.value = "assumed";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({ request: { time_assumed: true } });

    select.value = "confirmed";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls().length).toBe(2));
    expect(searchCalls()[1].args).toMatchObject({ request: { time_assumed: false } });

    select.value = "";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls().length).toBe(3));
    expect(searchCalls()[2].args).toMatchObject({ request: { time_assumed: null } });
  });

  it("the grouping filter sends grouping_unverified on change (true, false, off)", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 1));
    const select = $<HTMLSelectElement>("#grouping-unverified");
    select.value = "unverified";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({ request: { grouping_unverified: true } });

    select.value = "verified";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls().length).toBe(2));
    expect(searchCalls()[1].args).toMatchObject({ request: { grouping_unverified: false } });

    select.value = "";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls().length).toBe(3));
    expect(searchCalls()[2].args).toMatchObject({ request: { grouping_unverified: null } });
  });

  it("the files filter sends files_missing on change (true, false, off)", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 1));
    const select = $<HTMLSelectElement>("#files-missing");
    select.value = "missing";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({ request: { files_missing: true } });

    select.value = "preserved";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls().length).toBe(2));
    expect(searchCalls()[1].args).toMatchObject({ request: { files_missing: false } });

    select.value = "";
    fireChange(select);
    await vi.waitFor(() => expect(searchCalls().length).toBe(3));
    expect(searchCalls()[2].args).toMatchObject({ request: { files_missing: null } });
  });

  it("applied honesty filters render as chips; Clear resets the selects and the fold count", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 1));
    const time = $<HTMLSelectElement>("#time-assumed");
    time.value = "assumed";
    fireChange(time);
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    const grouping = $<HTMLSelectElement>("#grouping-unverified");
    grouping.value = "verified";
    fireChange(grouping);
    await vi.waitFor(() => expect(searchCalls().length).toBe(2));
    const files = $<HTMLSelectElement>("#files-missing");
    files.value = "missing";
    fireChange(files);
    await vi.waitFor(() => expect(searchCalls().length).toBe(3));

    const chips = [...win.document.querySelectorAll("#active-filter-chips .filter-chip")];
    expect(chips.map((c) => c.textContent?.trim())).toEqual([
      "dates assumed ×",
      "grouping verified ×",
      "files missing ×",
    ]);
    expect($("#filters-summary").textContent).toBe("Filters · 3 on");

    $("#clear-filters").click();
    await vi.waitFor(() => expect($("#filters-summary").textContent).toBe("Filters"));
    expect(time.value).toBe("");
    expect(grouping.value).toBe("");
    expect(files.value).toBe("");
    expect(win.document.querySelectorAll("#active-filter-chips .filter-chip").length).toBe(0);
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
    ($('[data-source="inst-demo"]') as HTMLElement).click();
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

  it("the Filters fold's summary counts the applied filters", async () => {
    await boot();
    // No filters: the fold is the plain "Filters" label, not pressed.
    expect($("#filters-summary").textContent).toBe("Filters");
    expect($("#filters-summary").classList.contains("on")).toBe(false);
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 1));
    input("#author").value = "ana";
    pressKey(input("#author"), "Enter");
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    const attachment = $<HTMLSelectElement>("#has-attachment");
    attachment.value = "with";
    fireChange(attachment);
    await vi.waitFor(() => expect(searchCalls().length).toBe(2));
    expect($("#filters-summary").textContent).toBe("Filters · 2 on");
    expect($("#filters-summary").classList.contains("on")).toBe(true);
    // Clear resets the filters and the count together.
    $("#clear-filters").click();
    await vi.waitFor(() => expect($("#filters-summary").textContent).toBe("Filters"));
    expect($("#filters-summary").classList.contains("on")).toBe(false);
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

// ---------------------------------------------------------------------------
// Era chips: the archive's timeline as one-click date filters. The from/to
// inputs stay the single source of truth — the pressed chip is derived
// from them on every render, never stored.
// ---------------------------------------------------------------------------

describe("era chips", () => {
  /** Three years of history → year chips 2019, 2020, 2021. */
  const ERA_STATS: ArchiveStats = {
    ...ARCHIVE_STATS,
    first_sent_at: "2019-05-01T10:00:00Z",
    last_sent_at: "2021-07-04T10:00:00Z",
    by_day: [
      { day: "2019-05-01", count: 2 },
      { day: "2020-06-02", count: 3 },
      { day: "2021-07-04", count: 4 },
    ],
  };

  function eraChips(): HTMLElement[] {
    return [...win.document.querySelectorAll("#era-chips .era-chip")] as unknown as HTMLElement[];
  }

  it("renders the archive's years; a click sets the from/to inputs and filters", async () => {
    tauri.handlers.set("archive_stats", () => ERA_STATS);
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 1));
    await boot();
    await vi.waitFor(() => expect(eraChips().length).toBe(3));
    expect(eraChips().map((c) => c.textContent)).toEqual(["2019", "2020", "2021"]);
    expect(eraChips().every((c) => c.getAttribute("aria-pressed") === "false")).toBe(true);

    eraChips()[1].click();
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(input("#from-date").value).toBe("2020-01-01");
    expect(input("#to-date").value).toBe("2020-12-31");
    expect(searchCalls()[0].args).toMatchObject({
      request: { from_date: "2020-01-01", to_date: "2020-12-31" },
    });
    // Pressed only because the inputs hold exactly this bucket's bounds.
    await vi.waitFor(() => expect(eraChips()[1].getAttribute("aria-pressed")).toBe("true"));
    // The existing applied-filter system shows the era as removable chips.
    const applied = [...win.document.querySelectorAll("#active-filter-chips .filter-chip")];
    expect(applied.map((c) => c.textContent?.trim())).toEqual([
      "from 2020-01-01 ×",
      "to 2020-12-31 ×",
    ]);
  });

  it("clicking the active era removes the bound; Clear clears it end to end", async () => {
    tauri.handlers.set("archive_stats", () => ERA_STATS);
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 1));
    await boot();
    await vi.waitFor(() => expect(eraChips().length).toBe(3));
    eraChips()[2].click();
    await vi.waitFor(() => expect(eraChips()[2].getAttribute("aria-pressed")).toBe("true"));

    // Toggling the pressed chip off clears the inputs and the request.
    eraChips()[2].click();
    await vi.waitFor(() => expect(input("#from-date").value).toBe(""));
    expect(input("#to-date").value).toBe("");
    expect(eraChips().every((c) => c.getAttribute("aria-pressed") === "false")).toBe(true);

    // Re-applied through the chip, the toolbar's Clear must clear it too.
    eraChips()[0].click();
    await vi.waitFor(() => expect(eraChips()[0].getAttribute("aria-pressed")).toBe("true"));
    $("#clear-filters").click();
    await vi.waitFor(() => expect(input("#from-date").value).toBe(""));
    expect(input("#to-date").value).toBe("");
    expect(eraChips().every((c) => c.getAttribute("aria-pressed") === "false")).toBe(true);
  });

  it("hand-typed bounds inside an era never light its chip", async () => {
    tauri.handlers.set("archive_stats", () => ERA_STATS);
    searchRespondsWith(() => pageOf(HITS, 3));
    await boot();
    await vi.waitFor(() => expect(eraChips().length).toBe(3));
    input("#from-date").value = "2020-03-01";
    input("#to-date").value = "2020-12-31";
    fireChange(input("#to-date"));
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    expect(searchCalls()[0].args).toMatchObject({
      request: { from_date: "2020-03-01", to_date: "2020-12-31" },
    });
    await vi.waitFor(() =>
      expect(eraChips().every((c) => c.getAttribute("aria-pressed") === "false")).toBe(true),
    );
  });

  it("a one-month timeline renders no strip (no dead chrome)", async () => {
    tauri.handlers.set("archive_stats", () => ARCHIVE_STATS);
    await boot();
    await vi.waitFor(() => expect($("#dashboard p.meta")).toBeTruthy());
    expect(win.document.querySelectorAll("#era-chips .era-chip").length).toBe(0);
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
    expect(call?.args).toEqual({ ids: [1, 3], format: "text", path: "/tmp/selection.txt" });
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

describe("import picker → preview → confirm (U03)", () => {
  const TELEGRAM_PREVIEW = {
    kind: "telegram",
    source: "telegram",
    input_bytes: 4096,
    messages_seen: 4,
    scan_complete: true,
    parse_failures: 0,
    samples: [
      {
        author: "Marko",
        subject: "hi",
        sent_at: "2026-11-01T10:00:00Z",
        snippet: "hi",
      },
    ],
    media_referenced: 1,
    media_missing: 0,
    warnings: [],
    free_bytes: 1024 * 1024 * 1024,
  };

  function importCalls(cmd: string): Array<{ cmd: string; args: unknown }> {
    return tauri.calls.filter((c) => c.cmd === cmd);
  }

  function jobSubmits(): Array<{ cmd: string; args: unknown }> {
    return tauri.calls.filter((c) => c.cmd === "job_submit");
  }

  /** Emit one job-event exactly as the backend stream delivers it. */
  const emitJob = (payload: unknown) =>
    tauriEvents.listeners.get("job-event")!({ payload });

  /** A succeeded import job's result — the same outcome shape the
   * direct import commands return. */
  const jobResult = (summary: unknown, importId: string, media: unknown = null) => ({
    summary,
    import_id: importId,
    media,
  });

  it("previews the pick, then follows its import job inline and offers the follow-up", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/tg-export/result.json");
    tauri.handlers.set("preview_import", () => Promise.resolve(TELEGRAM_PREVIEW));
    tauri.handlers.set("job_submit", () => ({ id: 31 }));
    tauri.handlers.set("list_import_scopes_v2", () => []);
    $("#import-pick-btn").click();

    // The preview card renders before anything is stored.
    await vi.waitFor(() => expect($("#import-preview").getAttribute("hidden")).toBeNull());
    expect(importCalls("preview_import")).toHaveLength(1);
    expect(jobSubmits()).toHaveLength(0);
    expect($(".preview-title").textContent).toBe("Telegram export");
    expect($(".preview-path").textContent).toBe("/tmp/tg-export/result.json");
    expect($("#import-preview").textContent).toContain("4 messages found");

    // Confirm submits the import job with the checked media toggle and
    // the resolved destination source (the derived name, GP03).
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect(jobSubmits()).toHaveLength(1));
    expect(jobSubmits()[0].args).toEqual({
      kind: "import_chat_export",
      params: {
        path: "/tmp/tg-export/result.json",
        extract_media: true,
        source: "telegram",
      },
    });

    // The inline card owns the run: queued before the first event, with
    // Cancel right there — no detour to Activity.
    await vi.waitFor(() => expect($("#import-progress").getAttribute("hidden")).toBeNull());
    expect($(".import-progress-line").textContent).toContain("Queued");
    expect($(".preview-path").textContent).toBe("/tmp/tg-export/result.json");
    expect($("#import-cancel-btn").textContent).toBe("Cancel");

    // The job's own events drive the card: counts, %, determinate bar.
    emitJob({ seq: 1, at: "t", job: 31, type: "submitted", kind: "import_chat_export" });
    emitJob({ seq: 2, at: "t", job: 31, type: "started" });
    emitJob({
      seq: 3,
      at: "t",
      job: 31,
      type: "progress",
      done: 2,
      total: 4,
      message: "reading messages",
    });
    expect($(".import-progress-line").textContent).toBe("2/4 (50%) · reading messages");
    expect(($(".progress-fill") as HTMLElement).style.width).toBe("50%");

    // Success hands off: the summary line lands, the card steps aside.
    emitJob({
      seq: 4,
      at: "t",
      job: 31,
      type: "succeeded",
      result: jobResult(
        { source: "telegram", seen: 4, inserted: 4, skipped: 0, failed: 0 },
        "chat-import-abc123",
        { referenced: 3, stored: 3, deduped: 1, missing: 0, linked: 3 },
      ),
    });
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Imported 4 new, 0 already present (of 4 in telegram). Media: 3 stored, 1 deduped, 3 linked.",
      ),
    );
    expect($("#import-progress").getAttribute("hidden")).not.toBeNull();
    // The follow-up jumps to this run's messages via the import scope.
    expect($("#import-followup").getAttribute("hidden")).toBeNull();
    // The post-import browse settles first; the follow-up's click runs a
    // fresh search scoped to exactly this run.
    await vi.waitFor(() => expect(searchCalls().length).toBeGreaterThanOrEqual(1));
    searchRespondsWith(() => pageOf(HITS.slice(0, 1), 1));
    const beforeFollowup = searchCalls().length;
    $("#view-imported-btn").click();
    await vi.waitFor(() => expect(searchCalls().length).toBe(beforeFollowup + 1));
    expect(searchCalls().at(-1)!.args).toMatchObject({
      request: { import_job_id: "chat-import-abc123" },
    });
  });

  it("an mbox preview submits an import_mbox job", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/archive.mbox");
    tauri.handlers.set("preview_import", () =>
      Promise.resolve({
        ...TELEGRAM_PREVIEW,
        kind: "mbox",
        source: "mbox",
        media_referenced: 0,
        media_missing: 0,
      }),
    );
    tauri.handlers.set("job_submit", () => ({ id: 32 }));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($(".preview-title").textContent).toBe("Email archive (.mbox)"));
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect(jobSubmits()).toHaveLength(1));
    expect(jobSubmits()[0].args).toEqual({
      kind: "import_mbox",
      params: { path: "/tmp/archive.mbox" },
    });
    // The card follows this flow's job (queued before any event lands).
    expect($("#import-progress").getAttribute("hidden")).toBeNull();
    expect($(".import-progress-line").textContent).toContain("Queued");
    emitJob({ seq: 1, at: "t", job: 32, type: "submitted", kind: "import_mbox" });
    emitJob({
      seq: 2,
      at: "t",
      job: 32,
      type: "succeeded",
      result: jobResult(
        { source: "mbox", seen: 2, inserted: 2, skipped: 0, failed: 0 },
        "mbox-import-def456",
      ),
    });
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Imported 2 new, 0 already present (of 2 in mbox).",
      ),
    );
  });

  it("an eml preview submits import_mbox, not the chat-export path", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/mail.eml");
    tauri.handlers.set("preview_import", () =>
      Promise.resolve({
        ...TELEGRAM_PREVIEW,
        kind: "eml",
        source: "mbox",
        messages_seen: 1,
        media_referenced: 0,
        media_missing: 0,
      }),
    );
    tauri.handlers.set("job_submit", () => ({ id: 33 }));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($(".preview-title").textContent).toBe("Email messages (.eml)"));
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect(jobSubmits()).toHaveLength(1));
    expect(jobSubmits()[0].args).toEqual({
      kind: "import_mbox",
      params: { path: "/tmp/mail.eml" },
    });
    expect(
      jobSubmits().some((c) => (c.args as { kind?: string }).kind === "import_chat_export"),
    ).toBe(false);
    emitJob({ seq: 1, at: "t", job: 33, type: "submitted", kind: "import_mbox" });
    emitJob({
      seq: 2,
      at: "t",
      job: 33,
      type: "succeeded",
      result: jobResult(
        { source: "mbox", seen: 1, inserted: 1, skipped: 0, failed: 0 },
        "mbox-import-eml789",
      ),
    });
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Imported 1 new, 0 already present (of 1 in mbox).",
      ),
    );
  });

  it("a cancelled picker previews nothing; cancelling the preview stores nothing", async () => {
    await boot();
    dialog.open.mockResolvedValue(null);
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect(dialog.open).toHaveBeenCalled());
    await flush();
    expect(importCalls("preview_import")).toHaveLength(0);

    dialog.open.mockResolvedValue("/tmp/chat.txt");
    tauri.handlers.set("preview_import", () => Promise.resolve(TELEGRAM_PREVIEW));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-cancel-btn")).toBeTruthy());
    $("#preview-cancel-btn").click();
    await flush();
    expect(jobSubmits()).toHaveLength(0);
    expect($("#import-preview").getAttribute("hidden")).not.toBeNull();
    expect($("#import-status").textContent).toBe("");
  });

  it("preview and import failures are friendly, never raw errno", async () => {
    await boot();
    dialog.open.mockResolvedValue("/moved.txt");
    tauri.handlers.set("preview_import", () =>
      Promise.reject("io error: No such file or directory (os error 2)"),
    );
    $("#import-pick-btn").click();
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toContain("Could not read /moved.txt"),
    );

    // A submission refused before any job existed: no card, the friendly
    // line says what happened.
    dialog.open.mockResolvedValue("/tmp/chat.txt");
    tauri.handlers.set("preview_import", () => Promise.resolve(TELEGRAM_PREVIEW));
    tauri.handlers.set("job_submit", () =>
      Promise.reject("io error: No such file or directory (os error 2)"),
    );
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    $("#preview-import-btn").click();
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Could not read /tmp/chat.txt — the file may have moved or been deleted. Pick it again.",
      ),
    );
    expect($("#import-progress").getAttribute("hidden")).not.toBeNull();
  });

  it("a failed import keeps its plain-language message in the card until dismissed", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/chat.txt");
    tauri.handlers.set("preview_import", () => Promise.resolve(TELEGRAM_PREVIEW));
    tauri.handlers.set("job_submit", () => ({ id: 34 }));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect($("#import-progress").getAttribute("hidden")).toBeNull());
    emitJob({ seq: 1, at: "t", job: 34, type: "submitted", kind: "import_chat_export" });
    emitJob({ seq: 2, at: "t", job: 34, type: "started" });
    emitJob({
      seq: 3,
      at: "t",
      job: 34,
      type: "failed",
      error: { kind: "import", message: "bad export" },
    });
    // The card carries the message (typed job error, described plainly)
    // and the recovery note; the status line stays out of it.
    expect($(".import-progress-line").textContent).toBe("Import failed — bad export.");
    expect($("#import-progress").textContent).toContain("nothing is duplicated");
    expect($("#import-status").textContent).toBe("");
    expect($("#import-progress").getAttribute("hidden")).toBeNull();

    // Dismiss returns the section to its start state.
    $("#import-progress-dismiss").click();
    expect($("#import-progress").getAttribute("hidden")).not.toBeNull();
    expect($("#import-progress").children.length).toBe(0);
  });

  it("confirming a naive-timestamp preview carries the corrected zone and chosen source (GP03)", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/chat.txt");
    tauri.handlers.set("preview_import", () =>
      Promise.resolve({
        ...TELEGRAM_PREVIEW,
        naive_timestamps: 4,
        assumed_zone: "Europe/Belgrade",
      }),
    );
    tauri.handlers.set("job_submit", () => ({ id: 41 }));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-zone-input")).toBeTruthy());
    // The card states the assumption and its DST honesty up front.
    expect($("#import-preview").textContent).toContain(
      "4 messages carry clock times without a timezone",
    );
    expect($("#import-preview").textContent).toContain("assumed Europe/Belgrade");
    // Correct the zone once and rename the destination source.
    input("#preview-zone-input").value = "Europe/Berlin";
    input("#preview-source-input").value = "Family chats";
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect(jobSubmits()).toHaveLength(1));
    expect(jobSubmits()[0].args).toEqual({
      kind: "import_chat_export",
      params: {
        path: "/tmp/chat.txt",
        extract_media: true,
        source: "Family chats",
        confirmed_timezone: "Europe/Berlin",
      },
    });
  });

  it("a failed import's Retry re-submits the same path and options (GP03)", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/chat.txt");
    tauri.handlers.set("preview_import", () =>
      Promise.resolve({
        ...TELEGRAM_PREVIEW,
        naive_timestamps: 2,
        assumed_zone: "Europe/Belgrade",
      }),
    );
    let nextId = 42;
    tauri.handlers.set("job_submit", () => ({ id: nextId++ }));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-zone-input")).toBeTruthy());
    input("#preview-zone-input").value = "Europe/Berlin";
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect(jobSubmits()).toHaveLength(1));
    emitJob({ seq: 1, at: "t", job: 42, type: "submitted", kind: "import_chat_export" });
    emitJob({ seq: 2, at: "t", job: 42, type: "failed", error: { kind: "import", message: "x" } });

    // The failed card offers the in-place retry.
    await vi.waitFor(() => expect($("#import-retry-btn")).toBeTruthy());
    $("#import-retry-btn").click();
    await vi.waitFor(() => expect(jobSubmits()).toHaveLength(2));
    // Verbatim re-submission: same path and options, nothing re-asked.
    expect(jobSubmits()[1].args).toEqual(jobSubmits()[0].args);
    // The new run's card replaces the failure (follows the fresh job id).
    await vi.waitFor(() =>
      expect(win.document.querySelector(".import-progress-card.failed")).toBeNull(),
    );
    expect(win.document.querySelector(".import-progress-card.queued")).toBeTruthy();
  });

  it("a completed import links its non-zero counts to recovery actions (GP03)", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/chat.txt");
    tauri.handlers.set("preview_import", () => Promise.resolve(TELEGRAM_PREVIEW));
    tauri.handlers.set("job_submit", () => ({ id: 45 }));
    tauri.handlers.set("list_import_scopes_v2", () => []);
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    $("#preview-import-btn").click();
    emitJob({ seq: 1, at: "t", job: 45, type: "submitted", kind: "import_chat_export" });
    emitJob({
      seq: 2,
      at: "t",
      job: 45,
      type: "succeeded",
      result: jobResult(
        { source: "telegram", seen: 4, inserted: 3, skipped: 0, failed: 1 },
        "chat-import-abc",
        { referenced: 2, stored: 1, deduped: 0, missing: 1, linked: 1, unlinked: 0, unlinked_deduped: 0 },
      ),
    });
    await vi.waitFor(() => expect($("#import-status").textContent).toContain("1 failed to parse"));
    // Each non-zero count gets its recovery action: the parse failure
    // opens Activity's explanation; the missing media file names the
    // re-export-with-media path.
    const failures = $("#import-failures-btn");
    expect(failures.textContent).toContain("failed to parse");
    expect($("#import-missing-media").textContent).toContain(
      "1 media file missing from the export",
    );
    failures.click();
    await vi.waitFor(() => expect($("#app").dataset.view).toBe("activity"));
  });

  it("Cancel stops this flow's job with no confirm dialog, then the section resets", async () => {
    await boot();
    // happy-dom ships no window.confirm; provide the spy the assertion
    // needs (Cancel must never open one).
    const confirmSpy = vi.fn(() => false);
    (win as unknown as { confirm: unknown }).confirm = confirmSpy;
    dialog.open.mockResolvedValue("/tmp/chat.txt");
    tauri.handlers.set("preview_import", () => Promise.resolve(TELEGRAM_PREVIEW));
    let nextJobId = 35;
    tauri.handlers.set("job_submit", () => ({ id: nextJobId++ }));
    tauri.handlers.set("job_cancel", () => true);
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect($("#import-cancel-btn")).toBeTruthy());

    // Confirm-free: cancellation is recoverable (re-import never
    // duplicates), so one click asks the engine to stop.
    $("#import-cancel-btn").click();
    await vi.waitFor(() =>
      expect(tauri.calls.some((c) => c.cmd === "job_cancel")).toBe(true),
    );
    expect(tauri.calls.find((c) => c.cmd === "job_cancel")!.args).toEqual({ id: 35 });
    expect(confirmSpy).not.toHaveBeenCalled();
    expect(($("#import-cancel-btn") as HTMLButtonElement).disabled).toBe(true);
    expect($("#import-cancel-btn").textContent).toBe("Cancelling…");

    // A progress event after the cancel request re-renders the card but
    // must not re-enable the button: the engine stops cooperatively, so
    // "Cancelling…" stands until the terminal `cancelled` event.
    emitJob({ seq: 1, at: "t", job: 35, type: "submitted", kind: "import_chat_export" });
    emitJob({
      seq: 2,
      at: "t",
      job: 35,
      type: "progress",
      done: 2,
      total: 4,
      message: "messages",
    });
    expect(($("#import-cancel-btn") as HTMLButtonElement).disabled).toBe(true);
    expect($("#import-cancel-btn").textContent).toBe("Cancelling…");
    expect($(".import-progress-line").textContent).toBe("2/4 (50%) · messages");

    // Cooperative: the stop is requested, the engine confirms by event.
    emitJob({ seq: 3, at: "t", job: 35, type: "cancelled" });
    await vi.waitFor(() => expect($("#import-progress").getAttribute("hidden")).not.toBeNull());
    expect($("#import-status").textContent).toBe("");
    // A new import can start right away — and its Cancel button is live
    // again (the request died with the job it was aimed at).
    dialog.open.mockResolvedValue("/tmp/other.txt");
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect(importCalls("preview_import")).toHaveLength(2));
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect($("#import-cancel-btn")).toBeTruthy());
    expect(($("#import-cancel-btn") as HTMLButtonElement).disabled).toBe(false);
    expect($("#import-cancel-btn").textContent).toBe("Cancel");
  });

  it("an unrecognized succeeded result still says the import finished", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/chat.txt");
    tauri.handlers.set("preview_import", () => Promise.resolve(TELEGRAM_PREVIEW));
    tauri.handlers.set("job_submit", () => ({ id: 37 }));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect($("#import-progress").getAttribute("hidden")).toBeNull());
    emitJob({ seq: 1, at: "t", job: 37, type: "submitted", kind: "import_chat_export" });
    // A result the outcome reader does not recognize: no invented counts,
    // no follow-up to scope — but the section still reports that the run
    // finished instead of silently hiding the card.
    emitJob({ seq: 2, at: "t", job: 37, type: "succeeded", result: { nonsense: true } });
    await vi.waitFor(() => expect($("#import-progress").getAttribute("hidden")).not.toBeNull());
    expect($("#import-status").textContent).toBe("Import finished.");
    expect($("#import-followup").getAttribute("hidden")).not.toBeNull();
    expect(win.document.querySelector("#view-imported-btn")).toBeNull();
  });

  it("a failed stats fetch at import completion never marks the archive view live", async () => {
    // Empty archive at boot: the first-run state leaves the initial
    // browse armed, so the import's completion follow-up is what would
    // mark the view live — and must not when the stats fetch fails.
    tauri.handlers.set("stats", () => ({ total: 0, by_source: [] }));
    await boot();
    expect($("#status").textContent).toBe("Your archive is empty — add a source to get started.");
    dialog.open.mockResolvedValue("/tmp/chat.txt");
    tauri.handlers.set("preview_import", () => Promise.resolve(TELEGRAM_PREVIEW));
    tauri.handlers.set("job_submit", () => ({ id: 38 }));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect($("#import-progress").getAttribute("hidden")).toBeNull());

    // The stats fetch at completion fails: no browse may fire on the
    // stale total, while the summary line and follow-up still render.
    tauri.handlers.set("stats", () => Promise.reject("stats backend down"));
    emitJob({ seq: 1, at: "t", job: 38, type: "submitted", kind: "import_chat_export" });
    emitJob({
      seq: 2,
      at: "t",
      job: 38,
      type: "succeeded",
      result: jobResult(
        { source: "telegram", seen: 4, inserted: 4, skipped: 0, failed: 0 },
        "chat-import-noStats",
      ),
    });
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Imported 4 new, 0 already present (of 4 in telegram).",
      ),
    );
    expect($("#import-followup").getAttribute("hidden")).toBeNull();
    expect(win.document.querySelector("#view-imported-btn")).toBeTruthy();
    // The honest status line from the catch stands in for the counts…
    expect($("#status").textContent).toBe("Type something to search, or set a filter to browse.");
    // …and no archive browse ran: the search IPC stayed uncalled.
    expect(searchCalls()).toHaveLength(0);

    // The next successful stats refresh (here: another job's terminal
    // event) drives the browse — proving the failed pass never armed
    // nor consumed the initial-browse flag.
    tauri.handlers.set("stats", () => ({
      total: 4,
      by_source: [{ source: "telegram", count: 4 }],
    }));
    emitJob({ seq: 3, at: "t", job: 39, type: "submitted", kind: "imap_sync" });
    emitJob({ seq: 4, at: "t", job: 39, type: "cancelled", kind: "imap_sync" });
    await vi.waitFor(() => expect(searchCalls()).toHaveLength(1));
    await vi.waitFor(() => expect($("#status").textContent).toBe("0 results"));
  });

  it("the confirm button disables when free space cannot fit the input", async () => {
    await boot();
    dialog.open.mockResolvedValue("/tmp/big.zip");
    tauri.handlers.set("preview_import", () =>
      Promise.resolve({
        ...TELEGRAM_PREVIEW,
        input_bytes: 10 * 1024 * 1024 * 1024,
        free_bytes: 1024,
      }),
    );
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    expect(($("#preview-import-btn") as HTMLButtonElement).disabled).toBe(true);
    expect($("#import-preview").textContent).toContain("free — the archive may not fit");
  });

  it("surfaces fully-deduped strays when no stray was stored", async () => {
    // Re-importing an export directory: every stray dedupes, so the
    // unlinked counter stays 0 — the deduped strays still render.
    await boot();
    dialog.open.mockResolvedValue("/tmp/tg-export/result.json");
    tauri.handlers.set("preview_import", () => Promise.resolve(TELEGRAM_PREVIEW));
    tauri.handlers.set("list_import_scopes_v2", () => []);
    tauri.handlers.set("job_submit", () => ({ id: 36 }));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect($("#import-progress").getAttribute("hidden")).toBeNull());
    emitJob({ seq: 1, at: "t", job: 36, type: "submitted", kind: "import_chat_export" });
    emitJob({
      seq: 2,
      at: "t",
      job: 36,
      type: "succeeded",
      result: jobResult(
        { source: "telegram", seen: 2, inserted: 0, skipped: 2, failed: 0 },
        "chat-import-dedup",
        {
          referenced: 1,
          stored: 0,
          deduped: 1,
          missing: 0,
          linked: 1,
          unlinked: 0,
          unlinked_deduped: 2,
          unlinked_dropped: 0,
          unlinked_walk_failed: 0,
        },
      ),
    });
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Imported 0 new, 2 already present (of 2 in telegram)." +
          " Media: 1 deduped, 1 linked, 2 unlinked (deduped).",
      ),
    );
  });

  it("an mbox import's status line renders its media counters", async () => {
    // M4.2: the import_mbox job result carries { summary, media } — the
    // mail pass's stored MIME attachments surface in the status line
    // (they used to be dropped on the floor).
    await boot();
    dialog.open.mockResolvedValue("/tmp/archive.mbox");
    tauri.handlers.set("preview_import", () =>
      Promise.resolve({
        ...TELEGRAM_PREVIEW,
        kind: "mbox",
        source: "mbox",
        media_referenced: 2,
        media_missing: 0,
      }),
    );
    tauri.handlers.set("job_submit", () => ({ id: 37 }));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($(".preview-title").textContent).toBe("Email archive (.mbox)"));
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect($("#import-progress").getAttribute("hidden")).toBeNull());
    emitJob({ seq: 1, at: "t", job: 37, type: "submitted", kind: "import_mbox" });
    emitJob({
      seq: 2,
      at: "t",
      job: 37,
      type: "succeeded",
      result: jobResult(
        { source: "mbox", seen: 2, inserted: 2, skipped: 0, failed: 0 },
        "mbox-import-media",
        {
          referenced: 2,
          stored: 2,
          deduped: 0,
          missing: 0,
          linked: 2,
          unlinked: 0,
          unlinked_deduped: 0,
          unlinked_dropped: 0,
          unlinked_walk_failed: 0,
        },
      ),
    });
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Imported 2 new, 0 already present (of 2 in mbox). Media: 2 stored, 2 linked.",
      ),
    );
  });

  it("budget drops and a failed walk render in the status line", async () => {
    // The honesty counters are only-non-zero: a budget-refused stray
    // ("N dropped (budget)") and a failed tree walk ("walk failed")
    // surface instead of pretending the pass was clean.
    await boot();
    dialog.open.mockResolvedValue("/tmp/tg-export/result.json");
    tauri.handlers.set("preview_import", () => Promise.resolve(TELEGRAM_PREVIEW));
    tauri.handlers.set("list_import_scopes_v2", () => []);
    tauri.handlers.set("job_submit", () => ({ id: 38 }));
    $("#import-pick-btn").click();
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    $("#preview-import-btn").click();
    await vi.waitFor(() => expect($("#import-progress").getAttribute("hidden")).toBeNull());
    emitJob({ seq: 1, at: "t", job: 38, type: "submitted", kind: "import_chat_export" });
    emitJob({
      seq: 2,
      at: "t",
      job: 38,
      type: "succeeded",
      result: jobResult(
        { source: "telegram", seen: 1, inserted: 1, skipped: 0, failed: 0 },
        "chat-import-honesty",
        {
          referenced: 0,
          stored: 0,
          deduped: 0,
          missing: 0,
          linked: 0,
          unlinked: 0,
          unlinked_deduped: 3,
          unlinked_dropped: 2,
          unlinked_walk_failed: 1,
        },
      ),
    });
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Imported 1 new, 0 already present (of 1 in telegram)." +
          " Media: 3 unlinked (deduped), 2 dropped (budget), walk failed.",
      ),
    );
  });

  it("an import still running from a previous session shows its card on boot", async () => {
    // Recovery: the boot poll's records snapshot carries an import job
    // that outlived a restart (or a webview reload) — the import section
    // adopts it and finishes the journey inline.
    tauri.handlers.set("job_events_since", () => ({
      events: [],
      latest_seq: 3,
      oldest_kept: 1,
      trimmed: false,
      jobs: [
        {
          id: 5,
          kind: "import_mbox",
          state: { state: "running" },
          submitted_at: "t",
          started_at: "t",
          finished_at: null,
          progress: { done: 10, total: 40, message: "reading messages" },
        },
      ],
    }));
    await boot();
    await vi.waitFor(() => expect($("#import-progress").getAttribute("hidden")).toBeNull());
    expect($(".import-progress-line").textContent).toBe("10/40 (25%) · reading messages");
    expect(($(".progress-fill") as HTMLElement).style.width).toBe("25%");
    expect($("#import-cancel-btn")).toBeTruthy();
    // While it runs, this flow will not start a second import.
    dialog.open.mockResolvedValue("/tmp/other.mbox");
    $("#import-pick-btn").click();
    await flush();
    expect(importCalls("preview_import")).toHaveLength(0);

    // Its success lands the same follow-up as a fresh import.
    emitJob({
      seq: 4,
      at: "t",
      job: 5,
      type: "succeeded",
      result: jobResult(
        { source: "mbox", seen: 40, inserted: 40, skipped: 0, failed: 0 },
        "mbox-import-recovered",
      ),
    });
    await vi.waitFor(() =>
      expect($("#import-status").textContent).toBe(
        "Imported 40 new, 0 already present (of 40 in mbox).",
      ),
    );
    expect($("#import-followup").getAttribute("hidden")).toBeNull();
    expect($("#import-progress").getAttribute("hidden")).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Whole-window drag-drop import (Tauri drag-drop events → the same flow)
// ---------------------------------------------------------------------------

describe("drag-drop import", () => {
  const PREVIEW = {
    kind: "telegram",
    source: "telegram",
    input_bytes: 4096,
    messages_seen: 4,
    scan_complete: true,
    parse_failures: 0,
    samples: [
      { author: "Marko", subject: "hi", sent_at: "2026-11-01T10:00:00Z", snippet: "hi" },
    ],
    media_referenced: 0,
    media_missing: 0,
    warnings: [],
    free_bytes: 1024 * 1024 * 1024,
  };

  function importCalls(cmd: string): Array<{ cmd: string; args: unknown }> {
    return tauri.calls.filter((c) => c.cmd === cmd);
  }

  function previewedPaths(): string[] {
    return importCalls("preview_import").map((c) => (c.args as { path: string }).path);
  }

  /** Emit one Tauri drag-drop event exactly as the webview API delivers
   * it (the wiring ignores positions). */
  const emitDrag = (payload: unknown) => {
    for (const handler of tauriWebview.dragHandlers) handler({ payload });
  };
  const dragOver = () => emitDrag({ type: "over", position: { x: 2, y: 2 } });
  const dragLeave = () => emitDrag({ type: "leave" });
  const dropPaths = (paths: string[]) =>
    emitDrag({ type: "drop", paths, position: { x: 1, y: 1 } });
  const emitJob = (payload: unknown) =>
    tauriEvents.listeners.get("job-event")!({ payload });

  it("both drop zones render with the advertised copy", async () => {
    await boot();
    const zones = [...win.document.querySelectorAll(".drop-zone")] as unknown as HTMLElement[];
    expect(zones.map((z) => z.id)).toEqual(["first-run-drop", "import-drop"]);
    for (const zone of zones) {
      expect(zone.textContent?.trim()).toBe(
        "Drop archives here — .mbox · .eml · .json · .jsonl · .txt",
      );
    }
  });

  it("a dropped file routes through the same bounded preview as the picker", async () => {
    await boot();
    tauri.handlers.set("preview_import", () => Promise.resolve(PREVIEW));
    dropPaths(["/tmp/archive.mbox"]);
    await vi.waitFor(() => expect($("#import-preview").getAttribute("hidden")).toBeNull());
    expect(previewedPaths()).toEqual(["/tmp/archive.mbox"]);
    expect($(".preview-path").textContent).toBe("/tmp/archive.mbox");
    // Nothing is stored without the confirm — the preview card waits.
    expect(tauri.calls.filter((c) => c.cmd === "job_submit")).toHaveLength(0);
    expect($("#preview-import-btn")).toBeTruthy();
  });

  it("a dropped folder path routes the same way", async () => {
    await boot();
    tauri.handlers.set("preview_import", () => Promise.resolve(PREVIEW));
    dropPaths(["/tmp/tg-export"]);
    await vi.waitFor(() => expect($(".preview-path").textContent).toBe("/tmp/tg-export"));
    expect(previewedPaths()).toEqual(["/tmp/tg-export"]);
    expect(tauri.calls.filter((c) => c.cmd === "job_submit")).toHaveLength(0);
  });

  it("a multi-path drop previews one at a time, in drop order", async () => {
    await boot();
    tauri.handlers.set("preview_import", () => Promise.resolve(PREVIEW));
    dropPaths(["/tmp/a.mbox", "/tmp/b.json", "/tmp/c.txt"]);
    // The first path previews immediately; the rest queue behind it.
    await vi.waitFor(() => expect($(".preview-path").textContent).toBe("/tmp/a.mbox"));
    expect(previewedPaths()).toEqual(["/tmp/a.mbox"]);
    expect($("#import-status").textContent).toBe("2 dropped archives waiting.");

    // Cancelling the first preview hands the flow to the second.
    $("#preview-cancel-btn").click();
    await vi.waitFor(() => expect($(".preview-path").textContent).toBe("/tmp/b.json"));
    expect(previewedPaths()).toEqual(["/tmp/a.mbox", "/tmp/b.json"]);
    expect($("#import-status").textContent).toBe("1 dropped archive waiting.");

    // The third previews the same way; the drained queue goes quiet.
    $("#preview-cancel-btn").click();
    await vi.waitFor(() => expect($(".preview-path").textContent).toBe("/tmp/c.txt"));
    expect($("#import-status").textContent).toBe("");
    $("#preview-cancel-btn").click();
    await flush();
    expect(previewedPaths()).toEqual(["/tmp/a.mbox", "/tmp/b.json", "/tmp/c.txt"]);
    expect(tauri.calls.filter((c) => c.cmd === "job_submit")).toHaveLength(0);
  });

  it("a drop mid-import waits, then previews at the job's terminal event", async () => {
    await boot();
    tauri.handlers.set("preview_import", () => Promise.resolve(PREVIEW));
    tauri.handlers.set("job_submit", () => ({ id: 71 }));
    tauri.handlers.set("list_import_scopes_v2", () => []);
    dropPaths(["/tmp/a.mbox"]);
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    $("#preview-import-btn").click();
    await vi.waitFor(() =>
      expect(tauri.calls.filter((c) => c.cmd === "job_submit")).toHaveLength(1),
    );

    // The drop lands while the job runs: it waits, nothing previews.
    dropPaths(["/tmp/b.mbox"]);
    await flush();
    expect(previewedPaths()).toEqual(["/tmp/a.mbox"]);
    expect($("#import-status").textContent).toBe("1 dropped archive waiting.");

    // The terminal event frees the flow — the next path previews.
    emitJob({ seq: 1, at: "t", job: 71, type: "submitted", kind: "import_chat_export" });
    emitJob({
      seq: 2,
      at: "t",
      job: 71,
      type: "succeeded",
      result: {
        summary: { source: "telegram", seen: 4, inserted: 4, skipped: 0, failed: 0 },
        import_id: "chat-import-drop",
        media: null,
      },
    });
    await vi.waitFor(() => expect(previewedPaths()).toEqual(["/tmp/a.mbox", "/tmp/b.mbox"]));
    await vi.waitFor(() => expect($(".preview-path").textContent).toBe("/tmp/b.mbox"));
  });

  it("a refused submission frees the flow for the next dropped path", async () => {
    await boot();
    tauri.handlers.set("preview_import", () => Promise.resolve(PREVIEW));
    tauri.handlers.set("job_submit", () => Promise.reject("queue is full"));
    dropPaths(["/tmp/a.mbox", "/tmp/b.mbox"]);
    await vi.waitFor(() => expect($("#preview-import-btn")).toBeTruthy());
    $("#preview-import-btn").click();
    // Refused before any job existed: the queue advances to the next
    // path instead of stalling behind a flow that never ran.
    await vi.waitFor(() => expect(previewedPaths()).toEqual(["/tmp/a.mbox", "/tmp/b.mbox"]));
    await vi.waitFor(() => expect($(".preview-path").textContent).toBe("/tmp/b.mbox"));
  });

  it("drops are ignored while the sample archive is open", async () => {
    tauri.handlers.set("stats", () => ({ total: 0, by_source: [] }));
    tauri.handlers.set("demo_status", () => Promise.resolve({ active: true }));
    await boot();
    await vi.waitFor(() => expect($("#demo-banner").getAttribute("hidden")).toBeNull());
    dropPaths(["/tmp/a.mbox"]);
    await flush();
    expect(importCalls("preview_import")).toHaveLength(0);
    expect($("#import-status").textContent).toBe("");
  });

  it("drag enter/over highlight both drop zones; leave and drop clear it", async () => {
    await boot();
    tauri.handlers.set("preview_import", () => Promise.resolve(PREVIEW));
    const active = () =>
      ([...win.document.querySelectorAll(".drop-zone")] as unknown as HTMLElement[]).map(
        (z) => z.classList.contains("drop-active"),
      );
    emitDrag({ type: "enter", paths: ["/tmp/a.mbox"], position: { x: 1, y: 1 } });
    expect(active()).toEqual([true, true]);
    dragOver();
    expect(active()).toEqual([true, true]);
    dragLeave();
    expect(active()).toEqual([false, false]);
    // A re-enter highlights again; the drop itself clears it (and an
    // empty drop enqueues nothing).
    emitDrag({ type: "enter", paths: [], position: { x: 1, y: 1 } });
    expect(active()).toEqual([true, true]);
    dropPaths([]);
    expect(active()).toEqual([false, false]);
    expect(importCalls("preview_import")).toHaveLength(0);
  });
});

describe("first-run state and the isolated sample archive (U02)", () => {
  it("an empty archive presents the first-source card instead of a search prompt", async () => {
    tauri.handlers.set("stats", () => ({ total: 0, by_source: [] }));
    await boot();
    await vi.waitFor(() => expect($("#first-run").getAttribute("hidden")).toBeNull());
    expect($("#demo-banner").getAttribute("hidden")).not.toBeNull();
    expect($("#status").textContent).toBe(
      "Your archive is empty — add a source to get started.",
    );
    // Searching the empty archive stays honest, not an error.
    searchRespondsWith(() => pageOf([], 0));
    input("#search").value = "vault";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect($("#status").textContent).toBe("0 results for “vault”"));
    // The card survives a manual empty browse attempt.
    expect($("#first-run").getAttribute("hidden")).toBeNull();
  });

  it("entering the sample archives browses it, leaving restores the personal vault", async () => {
    await boot();
    // Demo vault: its own stats/sources/search responses.
    const demoCalls: string[] = [];
    tauri.handlers.set("demo_enter", () => {
      demoCalls.push("enter");
      return Promise.resolve({ active: true });
    });
    tauri.handlers.set("demo_leave", () => {
      demoCalls.push("leave");
      return Promise.resolve({ active: false });
    });
    const demoStats = { total: 6, by_source: [{ source: "demo", count: 6 }] };
    let inDemo = false;
    tauri.handlers.set("demo_enter", () => {
      demoCalls.push("enter");
      inDemo = true;
      return Promise.resolve({ active: true });
    });
    tauri.handlers.set("demo_leave", () => {
      demoCalls.push("leave");
      inDemo = false;
      return Promise.resolve({ active: false });
    });
    tauri.handlers.set("stats", () => (inDemo ? demoStats : STATS));
    searchRespondsWith(() =>
      pageOf(
        inDemo
          ? [
              {
                ...HITS[0],
                id: 101,
                source_id: "inst-demo",
                source_label: "demo",
              },
            ]
          : HITS,
        inDemo ? 6 : 9,
      ),
    );

    $("#first-run-demo-btn").click();
    await vi.waitFor(() => expect(demoCalls).toEqual(["enter"]));
    await vi.waitFor(() =>
      expect($("#stats").textContent).toBe("6 messages (demo: 6)"),
    );
    await vi.waitFor(() => expect($("#demo-banner").getAttribute("hidden")).toBeNull());
    // The demo archive browses immediately (U04 under the demo).
    await vi.waitFor(() => expect($("#status").textContent).toBe("1–6 of 6"));
    // Source controls are locked while the demo is open (the backend
    // refuses writes regardless).
    expect(($("#import-pick-btn") as HTMLButtonElement).disabled).toBe(true);
    expect(($("#imap-sync-btn") as HTMLButtonElement).disabled).toBe(true);

    // Leaving restores the personal vault: banner gone, its stats back.
    $("#demo-leave-btn").click();
    await vi.waitFor(() => expect(demoCalls).toEqual(["enter", "leave"]));
    await vi.waitFor(() =>
      expect($("#stats").textContent).toBe("9 messages (Mail import: 6, demo: 3)"),
    );
    await vi.waitFor(() => expect($("#demo-banner").getAttribute("hidden")).not.toBeNull());
    await vi.waitFor(() => expect($("#status").textContent).toBe("1–9 of 9"));
    expect(($("#import-pick-btn") as HTMLButtonElement).disabled).toBe(false);
  });

  it("a webview reload while the demo is open restores the banner (demo_status)", async () => {
    tauri.handlers.set("stats", () => ({ total: 0, by_source: [] }));
    tauri.handlers.set("demo_status", () => Promise.resolve({ active: true }));
    tauri.handlers.set("demo_enter", () => Promise.resolve({ active: true }));
    await boot();
    await vi.waitFor(() => expect($("#demo-banner").getAttribute("hidden")).toBeNull());
    // The demo vault's stats browse, not the empty personal vault.
    await vi.waitFor(() => expect($("#first-run").getAttribute("hidden")).not.toBeNull());
  });
});

// Export
// ---------------------------------------------------------------------------

describe("export", () => {
  it("one Export… picker holds the format and both what-to-export choices, folded by default", async () => {
    await bootSearched();
    const menu = $("#export-menu") as HTMLDetailsElement;
    expect(menu.open).toBe(false);
    expect($("#export-summary").textContent).toBe("Export…");
    // Everything the old always-visible strip offered now lives inside
    // the one disclosure — nothing export-related sits beside it.
    for (const id of ["export-format", "export-btn", "selection-format", "export-selection-btn"]) {
      expect($(`#${id}`)!.closest("#export-menu")).toBe(menu);
    }
    // The outcome lines stay outside the fold so a finished export is
    // still reported after the picker closes.
    expect($("#export-status")!.closest("#export-menu")).toBeNull();
    expect($("#selection-status")!.closest("#export-menu")).toBeNull();
    // The disabled-until-selection rule survives the move.
    expect($<HTMLButtonElement>("#export-selection-btn").disabled).toBe(true);
    ($(".hit-item[data-id=\"1\"] .row-select") as HTMLElement).click();
    await vi.waitFor(() =>
      expect($<HTMLButtonElement>("#export-selection-btn").disabled).toBe(false),
    );
  });

  it("the picker hides with the first-run card: no export offered on an empty archive", async () => {
    tauri.handlers.set("stats", () => ({ total: 0, by_source: [] }));
    await boot();
    await vi.waitFor(() => expect($("#first-run").getAttribute("hidden")).toBeNull());
    // The summary, the picker, and its status lines disappear together.
    expect($("#export-controls").getAttribute("hidden")).not.toBeNull();
    expect($("#export-menu")!.closest("#export-controls")).toBeTruthy();
  });

  it("exports the executed search with all its filters, not just the page", async () => {
    await boot();
    searchRespondsWith(() => pageOf(HITS.slice(0, 2), 120));
    input("#author").value = "bruno";
    input("#from-date").value = "2026-01-01";
    input("#to-date").value = "2026-12-31";
    select("#has-attachment", "with");
    select("#import-scope", "imp-1");
    select("#sort-order", "newest");
    $('#source-chips .chip[data-source="inst-mbox"]').click();
    await runQuery("vault");
    await vi.waitFor(() => expect($("#status").textContent).toContain("1–50 of 120"));

    dialog.savedPath = "/tmp/out.csv";
    tauri.handlers.set("export_search_v2", () => ({ format: "csv", written: 120, skipped: 0 }));
    $("#export-btn").click();
    await vi.waitFor(() =>
      expect($("#export-status").textContent).toBe("Exported 120 messages to /tmp/out.csv."),
    );
    const exportCalls = tauri.calls.filter((c) => c.cmd === "export_search_v2");
    expect(exportCalls).toHaveLength(1);
    expect(exportCalls[0].args).toEqual({
      request: {
        text: "vault",
        source_ids: ["inst-mbox"],
        author: "bruno",
        from_date: "2026-01-01",
        to_date: "2026-12-31",
        timezone: "",
        has_attachment: true,
        time_assumed: null,
        grouping_unverified: null,
        files_missing: null,
        import_job_id: "imp-1",
        sort: "newest",
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
    tauri.handlers.set("export_search_v2", () => ({
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
    expect(tauri.calls.find((c) => c.cmd === "export_search_v2")?.args).toMatchObject({
      format: "mbox",
      path: "/tmp/archive.mbox",
    });
  });

  it("a cancelled save dialog exports nothing", async () => {
    await bootSearched();
    dialog.savedPath = null; // cancelled
    $("#export-btn").click();
    await flush();
    expect(tauri.calls.filter((c) => c.cmd === "export_search_v2")).toHaveLength(0);
    expect($("#export-status").textContent).toBe("");
  });

  it("with no executed search there is nothing to export and no dialog", async () => {
    await boot();
    dialog.savedPath = "/tmp/never.csv";
    $("#export-btn").click();
    await flush();
    expect(tauri.calls.filter((c) => c.cmd === "export_search_v2")).toHaveLength(0);
    expect($("#export-status").textContent).toBe(
      "Nothing to export — run a search (or set filters) first.",
    );
  });

  it("a failing export surfaces the error", async () => {
    await bootSearched();
    dialog.savedPath = "/root/forbidden.csv";
    tauri.handlers.set("export_search_v2", () => Promise.reject({
      code: "internal", message: "disk full", retryable: true,
    }));
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
    // The chat format select lives in the reading header's Export picker.
    $<HTMLSelectElement>("#chat-format").value = "json";
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

  it("groups thousands in the nav line like the chips beside it", async () => {
    tauri.handlers.set("stats", () => ({
      total: 5505,
      by_source: [{ source: "mbox", count: 5505 }],
    }));
    await boot();
    await vi.waitFor(() =>
      expect($("#stats").textContent).toBe("5,505 messages (Mail import: 5,505)"),
    );
    // The same count renders the same in the chip just below the line.
    expect([...win.document.querySelectorAll(".chip")].map((c) => c.textContent)).toEqual([
      "Mail import (5,505)",
    ]);
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

  it("chips filter by stable id; two same-named sources stay independent", async () => {
    // Override the boot fixtures BEFORE boot so the first stats/sources
    // refresh renders the duplicate-label pair.
    tauri.handlers.set("sources_list", () => [
      { id: "id-1", kind: "legacy", label: "chat", connector_key: "mbox", config_json: "{}",
        state: "connected", excluded: false, last_attempt_at: null, last_success_at: null,
        created_at: "2026-01-01 00:00:00" },
      { id: "id-2", kind: "legacy", label: "chat", connector_key: "mbox", config_json: "{}",
        state: "connected", excluded: false, last_attempt_at: null, last_success_at: null,
        created_at: "2026-01-01 00:00:00" },
    ]);
    tauri.handlers.set("accounts_list", () => []);
    tauri.handlers.set("stats", () => ({ total: 5, by_source: [{ source: "chat", count: 5 }] }));
    await boot();
    await vi.waitFor(() =>
      expect([...win.document.querySelectorAll("#source-chips .chip")].length).toBe(2),
    );
    const chips = [...win.document.querySelectorAll("#source-chips .chip")] as unknown as HTMLElement[];
    expect(chips.map((c) => c.dataset.source)).toEqual(["id-1", "id-2"]);
    chips[0].click();
    await vi.waitFor(() => expect(searchCalls().length).toBeGreaterThan(0));
    const args = searchCalls().at(-1)!.args as { request: { source_ids: string[] } };
    expect(args.request.source_ids).toEqual(["id-1"]);
    // The other same-named chip is still unpressed and independent.
    expect(
      ([...win.document.querySelectorAll("#source-chips .chip")][1] as unknown as HTMLElement).classList.contains("on"),
    ).toBe(false);
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
  plan: {
    match_expr: '"vault"',
    date_from: "2026-10-01",
    date_to: null,
    from: null,
    has_link: null,
    extract_links: false,
  },
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
    await vi.waitFor(() => expect($("#embed-status").textContent).toContain("3 of 4"));
    expect($("#embed-status").textContent).toContain("1 pending");
    expect($("#embed-status").textContent).toContain("hash-ngram-v1");
  });

  it("Ask stays collapsed behind its toggle; embeddings live on the AI settings page", async () => {
    tauri.handlers.set("embed_status", () => EMBED_STATUS);
    await boot();
    // Progressive disclosure: nothing retrieval-irrelevant is spread under
    // the results until the user folds it open.
    expect(($("#ask-fold") as HTMLDetailsElement).open).toBe(false);
    expect($("#ask-summary").textContent).toBe("Ask the archive");
    // The local-embeddings controls moved out of the workspace into
    // Settings → AI assistance, next to the rest of the AI configuration.
    expect($("#embed-btn")!.closest("#settings-ai")).toBeTruthy();
    expect($("#embed-status")!.closest("#settings-ai")).toBeTruthy();
    expect($("#embed-btn")!.closest("#ask-section")).toBeNull();
    expect($("#embed-btn")!.closest("#results-col")).toBeNull();
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
    expect($("#embed-status").textContent).toContain("Building the local search index");

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
    expect($("#jobs-list").textContent).toContain("local search index — 2/4 (50%)");
    emit({ seq: 4, at: "t", job: 9, type: "succeeded", result: { embedded: 4 } });
    await vi.waitFor(() =>
      expect($("#jobs-list").textContent).toContain("local search index — done (4 indexed)"),
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
      expect($("#embed-status").textContent).toBe(
        "Could not start building the local search index: queue is full",
      ),
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

  it("renders collected links with cited hits and exports them", async () => {
    await boot();
    tauri.handlers.set("llm_ask", () => ({
      plan: { match_expr: "example", date_from: null, date_to: null, from: "ana@", has_link: true, extract_links: true },
      plan_source: "llm",
      ranking: "fts_only",
      results: [],
      answer: "here is what I found",
      notes: [],
      collected: [
        {
          url: "https://example.com/a",
          hits: [
            { message_id: 7, author: "ana", sent_at: "2026-10-01T00:00:00Z", context: "see the link" },
          ],
        },
      ],
    }));
    tauri.handlers.set("conversation_context_v2", () => ({}));
    input("#ask").value = "collect links from ana";
    pressKey(input("#ask"), "Enter");
    await vi.waitFor(() => expect($("#ask-collected").hidden).toBe(false));
    expect($("#ask-collected-count").textContent).toBe("1 link from 1 message");
    expect($("#ask-collected-list summary").textContent).toContain("https://example.com/a");
    // A hit opens its citing message in thread context.
    ($("#ask-collected-list .ask-source") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(tauri.calls.some((c) => c.cmd === "conversation_context_v2" && (c.args as { id: number }).id === 7)).toBe(true),
    );
    // Export goes through the save dialog and save_text.
    dialog.savedPath = "/tmp/links.txt";
    ($("#ask-collected-export") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(tauri.calls.some((c) => c.cmd === "save_text")).toBe(true),
    );
    const call = tauri.calls.find((c) => c.cmd === "save_text")!;
    expect((call.args as { contents: string }).contents).toContain("https://example.com/a");
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

describe("activity badge", () => {
  const emit = (payload: unknown) =>
    tauriEvents.listeners.get("job-event")!({ payload });

  it("shows the active-job count and clears it when jobs finish", async () => {
    await boot();
    emit({ seq: 1, at: "t", job: 7, type: "submitted", kind: "import_mbox" });
    emit({ seq: 2, at: "t", job: 7, type: "started" });
    await vi.waitFor(() => expect(($("#activity-count") as HTMLElement).hidden).toBe(false));
    expect($("#activity-count").textContent).toBe("1");
    emit({ seq: 3, at: "t", job: 7, type: "succeeded", result: null });
    await vi.waitFor(() => expect(($("#activity-count") as HTMLElement).hidden).toBe(true));
  });

  it("a failed job raises the attention flag until Activity is opened", async () => {
    await boot();
    emit({ seq: 1, at: "t", job: 9, type: "submitted", kind: "imap_sync_all" });
    emit({
      seq: 2,
      at: "t",
      job: 9,
      type: "failed",
      error: { kind: "network", message: "down" },
    });
    await vi.waitFor(() => expect(($("#activity-attention") as HTMLElement).hidden).toBe(false));
    expect($("#activity-attention").textContent).toContain("needs attention");
    ($('.nav-view-btn[data-view-target="activity"]') as HTMLElement).click();
    await vi.waitFor(() => expect(($("#activity-attention") as HTMLElement).hidden).toBe(true));
  });

  it("does not reflag an acknowledged failure when its event is replayed", async () => {
    await boot();
    emit({ seq: 1, at: "t", job: 9, type: "submitted", kind: "imap_sync_all" });
    const failure = {
      seq: 2, at: "t", job: 9, type: "failed",
      error: { kind: "network", message: "down" },
    };
    emit(failure);
    expect($("#activity-attention").hidden).toBe(false);
    $('.nav-view-btn[data-view-target="activity"]').click();
    expect($("#activity-attention").hidden).toBe(true);
    $('.nav-view-btn[data-view-target="workspace"]').click();
    emit(failure);
    expect($("#activity-attention").hidden).toBe(true);
  });

  it("flags failures recovered by polling and acknowledges them while Activity is open", async () => {
    tauri.handlers.set("job_events_since", () => ({
      events: [
        { seq: 1, at: "t", job: 9, type: "submitted", kind: "imap_sync_all" },
        { seq: 2, at: "t", job: 9, type: "failed", error: { kind: "network", message: "down" } },
      ],
      latest_seq: 2, oldest_kept: 1, trimmed: false, jobs: [],
    }));
    await boot();
    await vi.waitFor(() => expect($("#jobs-list").textContent).toContain("failed"));
    expect($("#activity-attention").hidden).toBe(false);
    $('.nav-view-btn[data-view-target="activity"]').click();
    expect($("#activity-attention").hidden).toBe(true);
    emit({ seq: 3, at: "t", job: 10, type: "submitted", kind: "imap_sync_all" });
    emit({ seq: 4, at: "t", job: 10, type: "failed", error: { kind: "network", message: "down again" } });
    $('.nav-view-btn[data-view-target="workspace"]').click();
    expect($("#activity-attention").hidden).toBe(true);
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
      expect($("#watch-status").textContent).toContain("Pick a folder above"),
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

  it("a failed first sync shows the classified message, the credential note, and refreshes the accounts panel", async () => {
    await boot();
    // The failed account as the panel lists it after the failure: the
    // durable error state recorded by the backend (survives restarts).
    const FAILED_FIRST_SYNC = {
      source_id: "inst-imap-failed",
      kind: "imap",
      label: "imap:127.0.0.1:59993:first@h.invalid",
      medium: "mail",
      imap: {
        host: "127.0.0.1",
        port: 59993,
        username: "first@h.invalid",
        folders: [],
        has_password: true,
        has_oauth: false,
      },
      message_count: 0,
      failed_items: 0,
      state: "disconnected",
      excluded: false,
      last_attempt_at: "2026-11-09 23:00:00",
      last_success_at: null,
      last_error:
        "nothing answered at that host and port — check the server address and port (993 for IMAP over TLS)",
      last_error_at: "2026-11-09 23:00:01",
    };
    let syncCalled = false;
    tauri.handlers.set("accounts_list", () => (syncCalled ? [FAILED_FIRST_SYNC] : []));
    tauri.handlers.set("sync_imap", () => {
      syncCalled = true;
      return Promise.reject({
        class: "connection_refused",
        message:
          "nothing answered at that host and port — check the server address and port (993 for IMAP over TLS)",
        saved_to: "memory",
      });
    });
    input("#imap-host").value = "127.0.0.1";
    input("#imap-port").value = "59993";
    input("#imap-user").value = "first@h.invalid";
    input("#imap-pass").value = "not-a-real-password";
    await clickSync();

    await vi.waitFor(() =>
      expect($("#imap-status").textContent).toContain("Sync failed: nothing answered"),
    );
    // No raw errno ever reaches the user.
    expect($("#imap-status").textContent).not.toContain("os error");
    // The honest credential rule: the password was saved (session memory)
    // before the connection was attempted, and a restart clears it.
    expect($("#imap-status").textContent).toContain(
      "kept in memory only for this session",
    );
    // The panel refreshes after the failure: the account row is there with
    // its durable error state — not a stale "No accounts yet".
    await vi.waitFor(() =>
      expect(
        $("#sources-list .account-row[data-source-id='inst-imap-failed'] .account-sync")
          .textContent,
      ).toContain("sync failed at 2026-11-09 23:00:01"),
    );
    expect(($<HTMLButtonElement>("#imap-sync-btn") as HTMLButtonElement).disabled).toBe(false);
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
      consent: false,
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
      consent: false, // no consent granted in this test: the ask stays local
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
    $("#settings-save-ai").click();
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
    $("#settings-save-ai").click();
    await vi.waitFor(() => expect($("#settings-note").textContent).toContain("Saved."));
    await flush();
    expect(input("#ask-provider").value).toBe("local");
    expect($("#llm-note").textContent).toContain("LLM assist is off");
  });
});

// ---------------------------------------------------------------------------
// M5: explicit AI consent + source-linked answers (evidence navigation)
// ---------------------------------------------------------------------------

/** What `llm_ask` returns for a consent-less network ask: fully local,
 * with the visible backend note (mirrors the backend's downgrade). */
const LOCAL_ASK_OUTCOME: AskOutcome = {
  plan: { match_expr: '"vault"', date_from: null, date_to: null, from: null, has_link: null, extract_links: false },
  plan_source: "local",
  ranking: "fts_only",
  results: [
    message(21, "mbox", "thread-a", "vault sync planning went fine"),
    message(22, "demo", "", "the syncing of the vault had problems"),
  ],
  answer: null,
  notes: [
    "AI consent not granted — this ask ran fully locally; nothing was sent to any provider",
  ],
};

/** A thread-context fixture anchored at the requested message, so the
 * detail pane renders the *original* source plus its conversation. */
function ctxFor(id: number): ConversationContext {
  const source = (ASK_OUTCOME.results as Message[]).find((m) => m.id === id);
  return {
    ...CTX,
    anchor: {
      ...CTX.anchor,
      id,
      body: source ? source.body : CTX.anchor.body,
    },
  };
}

function askCalls(): Array<{ cmd: string; args: unknown }> {
  return tauri.calls.filter((c) => c.cmd === "llm_ask");
}

/** Boot with a usable network provider preselected; the ask handler
 * answers like the real backend: local without consent, full RAG with. */
async function bootConsentable(): Promise<void> {
  tauri.handlers.set("llm_status", () => ({
    enabled: true,
    model: "glm-5",
    host: "api.z.ai",
    summarize_top: 8,
    providers: [LOCAL_ITEM, ZAI_ITEM],
    default_provider: "zai",
  }));
  tauri.handlers.set("llm_ask", (args: unknown) =>
    (args as { consent: boolean }).consent ? ASK_OUTCOME : LOCAL_ASK_OUTCOME,
  );
  tauri.handlers.set(
    "conversation_context_v2",
    (args: unknown) => ctxFor((args as { id: number }).id),
  );
  await boot();
  await vi.waitFor(() => expect(input("#ask-provider").value).toBe("zai"));
}

async function askFromInput(question: string): Promise<void> {
  input("#ask").value = question;
  pressKey(input("#ask"), "Enter");
  await vi.waitFor(() => expect(askCalls().length).toBeGreaterThan(0));
}

describe("ask AI consent + evidence navigation", () => {
  it("boots with consent not granted, visibly, and no prompt", async () => {
    await bootConsentable();
    await vi.waitFor(() =>
      expect($("#consent-status").textContent).toContain("AI consent: not granted"),
    );
    expect($("#consent-status").textContent).toContain("fully locally");
    expect($<HTMLButtonElement>("#consent-revoke-btn").hidden).toBe(true);
    expect($("#consent-prompt").hidden).toBe(true);
  });

  it("a network ask without consent runs locally and shows the per-question prompt", async () => {
    await bootConsentable();
    await askFromInput("vault?");
    expect(askCalls()[0].args).toMatchObject({ provider: "zai", consent: false });
    // The prompt states exactly what would leave the machine.
    expect($("#consent-prompt").hidden).toBe(false);
    expect($("#consent-text").textContent).toContain("glm-5 via api.z.ai");
    expect($("#consent-text").textContent).toContain("top 8 matching messages");
    // The local outcome is visible: local plan label + the backend note.
    await vi.waitFor(() =>
      expect($("#ask-status").textContent).toContain("local full-text"),
    );
    expect($("#ask-notes").textContent).toContain("AI consent not granted");
    expect($("#answer").hidden).toBe(true);
  });

  it("an open prompt re-renders when the provider selection changes to another network provider", async () => {
    tauri.handlers.set("llm_status", () => ({
      enabled: true,
      model: "glm-5",
      host: "api.z.ai",
      summarize_top: 8,
      providers: [LOCAL_ITEM, ZAI_ITEM, { ...KEYLESS_ITEM, available: true }],
      default_provider: "zai",
    }));
    tauri.handlers.set("llm_ask", () => LOCAL_ASK_OUTCOME);
    await boot();
    await vi.waitFor(() => expect(input("#ask-provider").value).toBe("zai"));
    await askFromInput("vault?");
    expect($("#consent-prompt").hidden).toBe(false);
    expect($("#consent-text").textContent).toContain("glm-5 via api.z.ai");
    // Switch to the second network provider while the prompt is open:
    // the sentence must now name the destination a consent click would
    // actually use.
    input("#ask-provider").value = "mine";
    fireChange(input("#ask-provider"));
    expect($("#consent-prompt").hidden).toBe(false);
    expect($("#consent-text").textContent).toContain("m via 127.0.0.1");
    expect($("#consent-text").textContent).not.toContain("api.z.ai");
  });

  it("a late consent-less outcome never overwrites the consented ask's answer", async () => {
    await bootConsentable();
    let releaseLocal!: () => void;
    const localGate = new Promise<void>((r) => (releaseLocal = r));
    tauri.handlers.set("llm_ask", (args: unknown) => {
      const { consent } = args as { consent: boolean };
      return consent
        ? Promise.resolve(ASK_OUTCOME)
        : localGate.then(() => LOCAL_ASK_OUTCOME);
    });
    await askFromInput("vault?");
    expect($("#consent-prompt").hidden).toBe(false);
    // Send & ask fires the consented ask while the consent-less one is
    // still in flight; the consent-less response then lands last.
    $("#consent-allow-btn").click();
    await vi.waitFor(() => expect(askCalls().length).toBe(2));
    await vi.waitFor(() => expect($("#answer").hidden).toBe(false));
    releaseLocal();
    await new Promise((r) => setTimeout(r, 0));
    // The consented answer and its plan line survive the stale response.
    expect($("#answer").textContent).toBe(ASK_OUTCOME.answer);
    expect($("#ask-status").textContent).toContain("planned by LLM");
    expect($("#ask-notes").textContent).not.toContain("AI consent not granted");
  });

  it("Send & ask grants consent, re-asks with consent:true, and remembers it", async () => {
    await bootConsentable();
    await askFromInput("vault?");
    $("#consent-allow-btn").click();
    await vi.waitFor(() => expect(askCalls().length).toBe(2));
    expect(askCalls()[1].args).toMatchObject({
      question: "vault?",
      provider: "zai",
      consent: true,
    });
    // State visible; prompt gone.
    expect($("#consent-prompt").hidden).toBe(true);
    await vi.waitFor(() =>
      expect($("#consent-status").textContent).toContain("granted for this session"),
    );
    expect($<HTMLButtonElement>("#consent-revoke-btn").hidden).toBe(false);
    // Remembered within the session: the next ask goes straight out.
    await askFromInput("again?");
    expect(askCalls().length).toBe(3);
    expect(askCalls()[2].args).toMatchObject({ question: "again?", consent: true });
    expect($("#consent-prompt").hidden).toBe(true);
  });

  it("Keep asking locally dismisses the prompt; asks stay consent:false", async () => {
    await bootConsentable();
    await askFromInput("vault?");
    $("#consent-local-btn").click();
    expect($("#consent-prompt").hidden).toBe(true);
    expect($("#consent-status").textContent).toContain("not granted");
    await askFromInput("once more?");
    expect(askCalls().length).toBe(2);
    expect(askCalls()[1].args).toMatchObject({ consent: false });
    // The next network ask re-offers the prompt.
    expect($("#consent-prompt").hidden).toBe(false);
  });

  it("Revoke resets a granted consent mid-session", async () => {
    await bootConsentable();
    expect($("#ask-summary").textContent).toBe("Ask the archive");
    await askFromInput("vault?");
    $("#consent-allow-btn").click();
    await vi.waitFor(() =>
      expect($("#consent-status").textContent).toContain("granted for this session"),
    );
    // A live grant stays visible on the fold's summary even when Ask is
    // collapsed again (the Filters convention for applied state).
    expect($("#ask-summary").textContent).toBe("Ask the archive · AI consent on");
    expect($("#ask-summary").classList.contains("on")).toBe(true);
    $("#consent-revoke-btn").click();
    await vi.waitFor(() =>
      expect($("#consent-status").textContent).toContain("not granted"),
    );
    expect($("#ask-summary").textContent).toBe("Ask the archive");
    expect($("#ask-summary").classList.contains("on")).toBe(false);
    expect($<HTMLButtonElement>("#consent-revoke-btn").hidden).toBe(true);
    await askFromInput("after revoke?");
    expect(askCalls().at(-1)!.args).toMatchObject({ consent: false });
    expect($("#consent-prompt").hidden).toBe(false);
  });

  it("consent is session-scoped: a fresh app session starts ungranted", async () => {
    await bootConsentable();
    await askFromInput("vault?");
    $("#consent-allow-btn").click();
    await vi.waitFor(() =>
      expect($("#consent-status").textContent).toContain("granted for this session"),
    );
    // Simulate an app restart: a brand-new module session against a fresh
    // DOM. The consent lives in memory only (never localStorage or any
    // file), so the new session must start ungranted and ask again.
    tauri.calls.length = 0;
    vi.resetModules();
    await boot();
    await vi.waitFor(() =>
      expect($("#consent-status").textContent).toContain("AI consent: not granted"),
    );
    expect(win.localStorage.getItem("aiConsentGranted")).toBe(null);
  });

  it("the local provider never prompts and never sends", async () => {
    await bootConsentable();
    input("#ask-provider").value = "local";
    fireChange(input("#ask-provider"));
    await askFromInput("vault?");
    expect(askCalls()[0].args).toMatchObject({ provider: "local", consent: false });
    expect($("#consent-prompt").hidden).toBe(true);
  });

  it("answer citations are clickable and open the source in thread context", async () => {
    await bootConsentable();
    await askFromInput("vault?");
    $("#consent-allow-btn").click(); // consented ask → answer with [1]/[2]
    await vi.waitFor(() => expect($("#answer").hidden).toBe(false));
    const cites = [...win.document.querySelectorAll("#answer .citation")] as unknown as HTMLButtonElement[];
    expect(cites.map((c) => c.dataset.cite)).toEqual(["1", "2"]);
    // The answer text survives intact (buttons restate the markers).
    expect($("#answer").textContent).toBe(ASK_OUTCOME.answer);
    // Citation [2] → the second result row, the original archived message.
    cites[1].click();
    await vi.waitFor(() => expect(ctxCalls().length).toBeGreaterThan(0));
    expect((ctxCalls()[0].args as { id: number }).id).toBe(22);
    // ...opened in the reading pane with its conversation context.
    await vi.waitFor(() =>
      expect($("#detail").textContent).toContain("the syncing of the vault had problems"),
    );
    expect($("#detail").textContent).toContain("before current");
  });

  it("out-of-range citations stay plain text, never dead links", async () => {
    tauri.handlers.set("llm_status", () => ({
      enabled: true,
      model: "glm-5",
      host: "api.z.ai",
      summarize_top: 8,
      providers: [LOCAL_ITEM, ZAI_ITEM],
      default_provider: "local",
    }));
    const hallucinated: AskOutcome = {
      ...LOCAL_ASK_OUTCOME,
      answer: "answered from [1] and [3].",
    };
    tauri.handlers.set("llm_ask", () => hallucinated);
    await boot();
    await askFromInput("vault?");
    await vi.waitFor(() => expect($("#answer").hidden).toBe(false));
    const cites = win.document.querySelectorAll("#answer .citation");
    expect(
      [...cites].map((c) => (c as unknown as HTMLElement).dataset.cite),
    ).toEqual(["1"]); // [3] is not a link
    expect($("#answer").textContent).toBe("answered from [1] and [3].");
  });

  it("citations beyond the summary window stay plain text, never dead links", async () => {
    tauri.handlers.set("llm_status", () => ({
      enabled: true,
      model: "glm-5",
      host: "api.z.ai",
      summarize_top: 1,
      providers: [LOCAL_ITEM, ZAI_ITEM],
      default_provider: "local",
    }));
    const beyondWindow: AskOutcome = {
      ...LOCAL_ASK_OUTCOME,
      answer: "answered from [1] and [2].",
    };
    tauri.handlers.set("llm_ask", () => beyondWindow);
    await boot();
    await askFromInput("vault?");
    await vi.waitFor(() => expect($("#answer").hidden).toBe(false));
    const cites = win.document.querySelectorAll("#answer .citation");
    expect(
      [...cites].map((c) => (c as unknown as HTMLElement).dataset.cite),
    ).toEqual(["1"]); // [2] sits past summarize_top=1 → not a link
    expect($("#answer").textContent).toBe("answered from [1] and [2].");
  });

  it("answer source rows are numbered like citations and open in thread context", async () => {
    await bootConsentable();
    await askFromInput("vault?"); // local outcome still lists the sources
    await vi.waitFor(() =>
      expect(win.document.querySelectorAll("#ask-results .ask-source").length).toBe(2),
    );
    const rows = [...win.document.querySelectorAll("#ask-results .ask-source")] as unknown as HTMLButtonElement[];
    const metaOf = (row: HTMLButtonElement): string =>
      (row.querySelector(".meta") as HTMLElement | null)?.textContent ?? "";
    expect(metaOf(rows[0]).startsWith("[1]")).toBe(true);
    expect(metaOf(rows[1]).startsWith("[2]")).toBe(true);
    rows[0].click();
    await vi.waitFor(() => expect(ctxCalls().length).toBeGreaterThan(0));
    expect((ctxCalls()[0].args as { id: number }).id).toBe(21);
    await vi.waitFor(() =>
      expect($("#detail").textContent).toContain("vault sync planning went fine"),
    );
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
    notifications: true,
    keep_running_in_background: false,
    start_at_login: false,
    env: null,
    env_overrides: { base_url: "", model: "", zai_key: false, anthropic_key: false },
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
    tauri.handlers.set("settings_get", () =>
      viewWith((v) => {
        v.providers = [];
        v.default_provider = null;
      }),
    );
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

    $("#settings-save-general").click();
    await vi.waitFor(() => expect($("#settings-note").textContent).toContain("Saved."));
    expect(saved!.args).toEqual({
      providers: [
        { id: "zai", name: "Work GLM", kind: "zai", base_url: "", model: "" },
        { id: "custom", name: "", kind: "custom", base_url: "http://127.0.0.1:9/api", model: "m" },
      ],
      default_provider: "zai",
      theme: "light",
      summarize_top: 20,
      notifications: true,
      keep_running_in_background: false,
      start_at_login: false,
      api_keys: [
        { id: "zai", key: "secret-1" }, // trimmed
        { id: "custom", key: "secret-2" },
      ],
      env_overrides: { base_url: "", model: "" },
      env_api_keys: [],
    });
    // The response re-renders: key inputs reset, theme applied, note names
    // the fallback store.
    expect(rowInput("zai", "prov-key").value).toBe("");
    expect(document.documentElement.style.colorScheme).toBe("light");
    expect($("#settings-note").textContent).toContain("session memory");
  });

  it("renders the M7 keep-alive toggles from the view and saves them back", async () => {
    // Boot with both options on: the checkboxes must render the saved
    // state (the HTML ships them unchecked, so a checked box proves the
    // settings round-trip rendered it).
    tauri.handlers.set("settings_get", () =>
      viewWith((v) => {
        v.keep_running_in_background = true;
        v.start_at_login = true;
      }),
    );
    let saved: unknown;
    tauri.handlers.set("settings_save", (args: unknown) => {
      saved = (args as { payload: unknown }).payload;
      return viewWith((v) => {
        v.keep_running_in_background = false;
        v.start_at_login = false;
      });
    });
    await boot();
    await vi.waitFor(() => expect($<HTMLInputElement>("#keepalive-toggle").checked).toBe(true));
    expect($<HTMLInputElement>("#autostart-toggle").checked).toBe(true);

    // Opt back out of both and save: the payload carries the explicit
    // opt-out (never a silent drop), and the response re-renders them
    // unchecked — the same contract as the notifications toggle.
    $("#keepalive-toggle").click();
    $("#autostart-toggle").click();
    $("#settings-save-general").click();
    await vi.waitFor(() => expect($("#settings-note").textContent).toContain("Saved."));
    expect(saved).toMatchObject({
      keep_running_in_background: false,
      start_at_login: false,
    });
    expect($<HTMLInputElement>("#keepalive-toggle").checked).toBe(false);
    expect($<HTMLInputElement>("#autostart-toggle").checked).toBe(false);
  });

  it("a validation refusal shows the backend error and saves nothing", async () => {
    tauri.handlers.set("settings_get", () => viewWith());
    tauri.handlers.set(
      "settings_save",
      () => Promise.reject("settings error: provider “Mine”: a custom provider needs a base URL"),
    );
    await boot();
    $("#settings-save-general").click();
    await vi.waitFor(() => expect($("#settings-note").textContent).toContain("Not saved"));
    expect($("#settings-note").textContent).toContain("needs a base URL");
    expect($("#settings-note").classList.contains("error")).toBe(true);
    // The form keeps the user's edits (no re-render on failure).
    expect(rowIds()).toEqual(["zai"]);
  });

  it("a Save clicked before the settings render lands is ignored, then works after", async () => {
    let releaseSettings: ((view: Record<string, unknown>) => void) | null = null;
    tauri.handlers.set(
      "settings_get",
      () =>
        new Promise((resolve) => {
          releaseSettings = resolve;
        }),
    );
    await boot();
    tauri.handlers.set("settings_save", () => viewWith((v) => (v.notifications = true)));
    input("#notify-toggle").checked = true; // HTML default, not persisted state
    $("#settings-save-general").click();
    await flush();
    expect(tauri.calls.some((c) => c.cmd === "settings_save")).toBe(false);

    releaseSettings!(
      viewWith((v) => {
        v.notifications = false;
      }),
    );
    await vi.waitFor(() => expect(input("#notify-toggle").checked).toBe(false));
    input("#notify-toggle").checked = true;
    $("#settings-save-general").click();
    await vi.waitFor(() => expect($("#settings-note").textContent).toContain("Saved."));
    const save = tauri.calls.find((c) => c.cmd === "settings_save");
    expect((save!.args as { payload: { notifications: boolean } }).payload.notifications).toBe(
      true,
    );
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

  it("preserves a saved interval outside the standard choices when toggling", async () => {
    tauri.handlers.set("sync_schedule_get", () => ({
      ...SCHEDULE_OFF,
      enabled: true,
      interval_minutes: 7,
    }));
    tauri.handlers.set("sync_schedule_set", () => ({ ...SCHEDULE_OFF, interval_minutes: 7 }));
    await boot();
    const select = $<HTMLSelectElement>("#auto-sync-interval");
    expect(select.value).toBe("7");
    input("#auto-sync-toggle").checked = false;
    fireChange(input("#auto-sync-toggle"));
    await vi.waitFor(() =>
      expect($("#auto-sync-status").textContent).toContain("Auto-sync off"),
    );
    expect(tauri.calls.find((c) => c.cmd === "sync_schedule_set")?.args)
      .toEqual({ enabled: false, intervalMinutes: 7 });
    expect(select.value).toBe("7");
    expect(Array.from(select.options).filter((o) => o.value === "7")).toHaveLength(1);
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
// Desktop notifications for new mail (sync/watch cycles)
// ---------------------------------------------------------------------------

describe("desktop notifications", () => {
  const emitJob = (payload: unknown) =>
    tauriEvents.listeners.get("job-event")!({ payload });

  /** happy-dom windows report focus by default; notifications only fire
   * for background arrivals, so tests that expect a notification unhook
   * focus first. */
  let focusSpy: ReturnType<typeof vi.spyOn> | null = null;
  function blur(): void {
    focusSpy = vi.spyOn(win.document, "hasFocus").mockReturnValue(false);
  }

  afterEach(() => {
    focusSpy?.mockRestore();
    focusSpy = null;
  });

  function bootWithJobs(): Promise<void> {
    tauri.handlers.set(
      "job_events_since",
      () => ({ events: [], latest_seq: 0, oldest_kept: 1, trimmed: false, jobs: [] }),
    );
    return boot();
  }

  it("notifies a succeeded sync round that stored new mail while unfocused", async () => {
    await bootWithJobs();
    blur();
    emitJob({ seq: 1, at: "t", job: 5, type: "submitted", kind: "imap_sync_all" });
    emitJob({ seq: 2, at: "t", job: 5, type: "started" });
    emitJob({
      seq: 3,
      at: "t",
      job: 5,
      type: "succeeded",
      result: {
        outcomes: [
          {
            instance_id: "i1",
            label: "imap:mail.example.com:993:me@example.com",
            status: {
              status: "synced",
              folders: [
                { folder: "INBOX", inserted: 2, skipped: 1, failed: 0 },
                { folder: "Sent", inserted: 0, skipped: 3, failed: 0 },
              ],
            },
          },
        ],
        excluded: [],
      },
    });
    await vi.waitFor(() => expect(notifications.sent.length).toBe(1));
    expect(notifications.sent[0]).toEqual({
      title: "2 new messages",
      body: "INBOX",
    });
  });

  it("stays silent while the app window is focused (the user is reading)", async () => {
    await bootWithJobs();
    // happy-dom's default hasFocus() is true — no spy needed.
    emitJob({ seq: 1, at: "t", job: 5, type: "submitted", kind: "imap_sync" });
    emitJob({
      seq: 2,
      at: "t",
      job: 5,
      type: "succeeded",
      result: [{ folder: "INBOX", inserted: 3 }],
    });
    emitWatch({
      ...EMPTY_WATCH,
      events: [
        {
          kind: "imported",
          path: "/home/me/exports/mail.mbox",
          summary: { source: "mbox", seen: 3, inserted: 2, skipped: 1, failed: 0 },
        },
      ],
    });
    await flush();
    expect(notifications.sent).toEqual([]);
  });

  it("notifies a watched import once, without re-notifying replayed snapshots", async () => {
    await bootWithJobs();
    blur();
    const snapshot: WatchSnapshot = {
      ...EMPTY_WATCH,
      events: [
        {
          kind: "imported",
          path: "/home/me/exports/chat.txt",
          summary: { source: "whatsapp", seen: 4, inserted: 4, skipped: 0, failed: 0 },
        },
      ],
    };
    emitWatch(snapshot);
    await vi.waitFor(() => expect(notifications.sent.length).toBe(1));
    expect(notifications.sent[0]).toEqual({
      title: "4 new messages",
      body: "imported from chat.txt",
    });
    // The watcher pushes a fresh snapshot per change; the same newest
    // event keeps arriving — one notification, not a stream.
    emitWatch(snapshot);
    emitWatch(snapshot);
    await flush();
    expect(notifications.sent.length).toBe(1);
  });

  it("stays silent for cycles with no new mail and for failed jobs", async () => {
    await bootWithJobs();
    blur();
    emitJob({ seq: 1, at: "t", job: 5, type: "submitted", kind: "imap_sync_all" });
    emitJob({
      seq: 2,
      at: "t",
      job: 5,
      type: "succeeded",
      result: { outcomes: [], excluded: [] },
    });
    emitJob({ seq: 3, at: "t", job: 6, type: "submitted", kind: "imap_sync" });
    emitJob({
      seq: 4,
      at: "t",
      job: 6,
      type: "failed",
      error: { kind: "network", message: "unreachable" },
    });
    await flush();
    expect(notifications.sent).toEqual([]);
  });

  it("the Settings option turns notifications off and back on", async () => {
    tauri.handlers.set("settings_get", () => viewWith((v) => (v.notifications = false)));
    await bootWithJobs();
    blur();
    expect(input("#notify-toggle").checked).toBe(false);
    emitJob({ seq: 1, at: "t", job: 5, type: "submitted", kind: "imap_sync" });
    emitJob({
      seq: 2,
      at: "t",
      job: 5,
      type: "succeeded",
      result: [{ folder: "INBOX", inserted: 3 }],
    });
    await flush();
    expect(notifications.sent).toEqual([]);

    // Turning the option back on re-enables notifications for the next
    // cycle (and the toggle itself asks the OS for permission — a user
    // gesture).
    notifications.granted = false; // ask on the gesture
    let savedView = viewWith();
    tauri.handlers.set("settings_save", (args: unknown) => {
      const payload = (args as { payload: Record<string, unknown> }).payload;
      savedView = viewWith((v) => (v.notifications = payload.notifications === true));
      return savedView;
    });
    input("#notify-toggle").checked = true;
    fireChange(input("#notify-toggle"));
    await vi.waitFor(() => expect(notifications.requested).toBe(1));
    $("#settings-save-general").click();
    await vi.waitFor(() => expect($("#settings-note").textContent).toContain("Saved."));
    const save = tauri.calls.find((c) => c.cmd === "settings_save");
    expect((save!.args as { payload: { notifications: boolean } }).payload.notifications).toBe(
      true,
    );
    emitJob({ seq: 3, at: "t", job: 7, type: "submitted", kind: "imap_sync" });
    emitJob({
      seq: 4,
      at: "t",
      job: 7,
      type: "succeeded",
      result: [{ folder: "INBOX", inserted: 1 }],
    });
    await vi.waitFor(() => expect(notifications.sent.length).toBe(1));
  });

  it("asks for permission when not yet granted, and stays silent when denied", async () => {
    await bootWithJobs();
    blur();
    notifications.granted = false;
    notifications.requestAnswer = false; // the OS prompt is refused
    emitJob({ seq: 1, at: "t", job: 5, type: "submitted", kind: "imap_sync" });
    emitJob({
      seq: 2,
      at: "t",
      job: 5,
      type: "succeeded",
      result: [{ folder: "INBOX", inserted: 3 }],
    });
    await vi.waitFor(() => expect(notifications.requested).toBe(1));
    await flush();
    expect(notifications.sent).toEqual([]);
  });

  it("stays silent when a sync succeeds before the settings load resolves", async () => {
    let releaseSettings: ((view: Record<string, unknown>) => void) | null = null;
    tauri.handlers.set(
      "settings_get",
      () =>
        new Promise((resolve) => {
          releaseSettings = resolve;
        }),
    );
    await bootWithJobs();
    blur();
    emitJob({ seq: 1, at: "t", job: 5, type: "submitted", kind: "imap_sync" });
    emitJob({
      seq: 2,
      at: "t",
      job: 5,
      type: "succeeded",
      result: [{ folder: "INBOX", inserted: 3 }],
    });
    await flush();
    expect(notifications.sent).toEqual([]);
    // Once settings prove the option is on, later cycles notify again.
    releaseSettings!(viewWith());
    await flush();
    emitJob({ seq: 3, at: "t", job: 6, type: "submitted", kind: "imap_sync" });
    emitJob({
      seq: 4,
      at: "t",
      job: 6,
      type: "succeeded",
      result: [{ folder: "INBOX", inserted: 1 }],
    });
    await vi.waitFor(() => expect(notifications.sent.length).toBe(1));
  });

  it("the manual Sync button's result also feeds the notification layer", async () => {
    await bootWithJobs();
    blur();
    tauri.handlers.set("sync_imap", () => ({
      folders: [
        {
          folder: "INBOX",
          uidvalidity: 1,
          reset: false,
          seen: 5,
          inserted: 2,
          skipped: 3,
          failed: 0,
          resolved_failures: 0,
          conflicting_revisions: 0,
          error: null,
        },
      ],
      saved_to: null,
    }));
    input("#imap-host").value = "mail.example.com";
    input("#imap-user").value = "me@example.com";
    input("#imap-pass").value = "secret";
    $("#imap-sync-btn").click();
    await vi.waitFor(() => expect(notifications.sent.length).toBe(1));
    expect(notifications.sent[0]).toEqual({
      title: "2 new messages",
      body: "INBOX",
    });
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

  it("navigation switches views with aria-current; results stay mounted", async () => {
    await boot();
    const buttons = [...document.querySelectorAll(".nav-view-btn")].map(
      (b) => (b as HTMLElement).dataset.viewTarget,
    );
    expect(buttons).toEqual(["workspace", "sources", "settings", "activity"]);
    const app = $("#app") as HTMLElement;
    const archiveBtn = $('.nav-view-btn[data-view-target="workspace"]') as HTMLElement;
    expect(archiveBtn.getAttribute("aria-current")).toBe("page");
    ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
    await vi.waitFor(() => expect(app.dataset.view).toBe("sources"));
    expect(archiveBtn.getAttribute("aria-current")).toBe(null);
    expect(($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).getAttribute("aria-current")).toBe("page");
    expect($("#results")).toBeTruthy(); // results stay mounted
    expect($("#topbar-title").textContent).toBe("Sources");
  });

  it("leaving and returning to the workspace preserves the open reading pane", async () => {
    await boot();
    await openDetailViaResult();
    expect($("#detail").classList.contains("open")).toBe(true);
    expect($("#detail .detail-card")).toBeTruthy();
    const settingsBtn = $(".nav-view-btn[data-view-target='settings']");
    settingsBtn.click();
    expect(app().dataset.view).toBe("settings");
    expect(settingsBtn.getAttribute("aria-current")).toBe("page");
    // Leaving and returning preserves the open reading pane and results.
    $(".nav-view-btn[data-view-target='workspace']").click();
    expect(app().dataset.view).toBe("workspace");
    expect($("#detail").classList.contains("open")).toBe(true);
    expect(app().dataset.reading).toBe("open");
    // The pane still shows the opened message, not a blank slate.
    expect($("#detail .detail-card")).toBeTruthy();
  });

  it("selecting a source chip from another view returns to the workspace", async () => {
    await boot();
    $(".nav-view-btn[data-view-target='settings']").click();
    expect(app().dataset.view).toBe("settings");
    searchRespondsWith(() => pageOf(HITS, 3));
    $('[data-source="inst-mbox"]').click();
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
    // The row's button (role=button .hit-main) is the focus target; the
    // li itself is a plain list item.
    const row = $("#results .hit-item .hit-main") as HTMLElement;
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
    expect(active?.classList.contains("hit-main")).toBe(true);
    expect(active?.closest(".hit-item")?.getAttribute("aria-current")).toBe("true");
  });

  it("Enter on a focused result row opens that row's message", async () => {
    await bootSearched();
    const rows = win.document.querySelectorAll(
      "#results .hit-item .hit-main",
    ) as unknown as HTMLElement[];
    const row = rows[2];
    expect(row.getAttribute("role")).toBe("button");
    row.focus();
    pressKey(row, "Enter");
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    expect(ctxCalls().at(-1)?.args).toMatchObject({
      id: Number((row.closest(".hit-item") as HTMLElement)?.dataset.id),
    });
  });

  it("opening a message in wide layout keeps focus on the current thread row", async () => {
    await boot();
    setViewportWidth(1280);
    searchRespondsWith(() => pageOf(HITS, 3));
    tauri.handlers.set("conversation_context_v2", () => CTX);
    input("#search").value = "vault";
    pressKey(input("#search"), "Enter");
    await vi.waitFor(() => expect($("#results .hit-item")).toBeTruthy());
    const row = $("#results .hit-item .hit-main") as HTMLElement;
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
    const row = $("#results .hit-item .hit-main") as HTMLElement;
    row.focus();
    pressKey(win.document, "ArrowDown");
    const active = win.document.activeElement as unknown as HTMLElement | null;
    expect(active?.classList.contains("hit-main")).toBe(true);
    expect(active?.closest(".hit-item")?.getAttribute("aria-current")).toBe("true");
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
    const row = $("#results .hit-item .hit-main") as HTMLElement;
    row.focus();
    row.click();
    await vi.waitFor(() => expect($("#detail").classList.contains("open")).toBe(true));
    const closeBtn = $<HTMLElement>(".close-btn");
    closeBtn.focus();
    closeBtn.click();
    expect($("#detail").classList.contains("open")).toBe(false);
    const active = win.document.activeElement as unknown as HTMLElement | null;
    expect(active?.classList.contains("hit-main")).toBe(true);
    expect(active?.closest(".hit-item")?.getAttribute("aria-current")).toBe("true");
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

describe("workspace tabs (Search / Ask) and the hideable sidebar", () => {
  function app(): HTMLElement {
    return $("#app");
  }

  function setViewportWidth(width: number): void {
    win.innerWidth = width;
    win.dispatchEvent(new win.Event("resize"));
  }

  it("switches panels via the tab strip; both panels stay mounted", async () => {
    await boot();
    expect(app().dataset.workspaceSection).toBe("search");
    const searchTab = $("#tab-search") as HTMLElement;
    const askTab = $("#tab-ask") as HTMLElement;
    expect(askTab.getAttribute("aria-selected")).toBe("false");
    askTab.click();
    expect(app().dataset.workspaceSection).toBe("ask");
    expect(askTab.getAttribute("aria-selected")).toBe("true");
    expect(searchTab.getAttribute("aria-selected")).toBe("false");
    // The tabpanels point at each other and both stay in the DOM.
    expect(askTab.getAttribute("aria-controls")).toBe("ask-section");
    expect($("#search-section")).toBeTruthy();
    expect($("#ask-section")).toBeTruthy();
    searchTab.click();
    expect(app().dataset.workspaceSection).toBe("search");
  });

  it("leaving and returning keeps the active tab; Ctrl+K lands on Search", async () => {
    await boot();
    ($("#tab-ask") as HTMLElement).click();
    expect(app().dataset.workspaceSection).toBe("ask");
    $(".nav-view-btn[data-view-target='settings']").click();
    expect(app().dataset.view).toBe("settings");
    $(".nav-view-btn[data-view-target='workspace']").click();
    expect(app().dataset.workspaceSection).toBe("ask"); // preserved
    win.document.dispatchEvent(
      new win.KeyboardEvent("keydown", { key: "k", ctrlKey: true, bubbles: true, cancelable: true }),
    );
    expect(app().dataset.workspaceSection).toBe("search");
    expect(win.document.activeElement?.id).toBe("search");
  });

  it("desktop: the hamburger collapses the nav column and persists the choice", async () => {
    await boot();
    setViewportWidth(1280);
    const toggle = $("#nav-toggle") as HTMLButtonElement;
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    toggle.click();
    expect(app().dataset.navCollapsed).toBe("true");
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(win.localStorage.getItem("mcv.navCollapsed")).toBe("1");
    toggle.click();
    expect(app().dataset.navCollapsed).toBe("false");
    expect(win.localStorage.getItem("mcv.navCollapsed")).toBe("0");
  });

  it("compact: the hamburger keeps working the drawer, not the desktop collapse", async () => {
    await boot();
    setViewportWidth(640);
    const toggle = $("#nav-toggle") as HTMLButtonElement;
    toggle.click();
    expect(app().dataset.nav).toBe("open");
    expect(app().dataset.navCollapsed).toBe("false"); // untouched
    toggle.click();
    expect(app().dataset.nav).toBe("closed");
  });
});

describe("sources view sub-sections", () => {
  it("renders five sub-sections and defaults to home", async () => {
    await boot();
    const app = $("#app") as HTMLElement;
    expect(app.dataset.sourcesSection).toBe("home");
    for (const id of ["#sources-home", "#sources-add", "#sources-import", "#sources-email", "#sources-automatic"]) {
      expect($(id), id).toBeTruthy();
    }
  });

  it("the Add source chooser navigates to import / email and Back returns home", async () => {
    await boot();
    ($("#sources-add-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("add"),
    );
    ($("#add-choice-import") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("import"),
    );
    ($("#import-back-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
    ($("#add-choice-email") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("email"),
    );
    ($("#email-back-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
  });

  it("the Automatic imports link opens the automatic section and Back returns", async () => {
    await boot();
    ($("#sources-automatic-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("automatic"),
    );
    // The relocated scheduler and watch controls are inside that section.
    expect($("#sources-automatic")!.contains($("#auto-sync-toggle"))).toBe(true);
    expect($("#sources-automatic")!.contains($("#watch-folders"))).toBe(true);
    ($("#automatic-back-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
  });

  it("Add source and Automatic imports move keyboard focus into the opened section", async () => {
    await boot();
    ($("#sources-add-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("add"),
    );
    // The section switch hides the trigger; focus must follow into the chooser.
    expect(win.document.activeElement).toBe($("#add-choice-import"));
    ($("#sources-automatic-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("automatic"),
    );
    // The heading carries tabindex="-1" (the reading-pane h2 pattern).
    expect(win.document.activeElement).toBe($("#sources-automatic-heading"));
  });

  it("Back buttons restore focus to the trigger that opened the section", async () => {
    await boot();
    // From the chooser itself.
    ($("#sources-add-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("add"),
    );
    ($("#add-back-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
    expect(win.document.activeElement).toBe($("#sources-add-btn"));
    // From the import flow.
    ($("#sources-add-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("add"),
    );
    ($("#add-choice-import") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("import"),
    );
    ($("#import-back-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
    expect(win.document.activeElement).toBe($("#sources-add-btn"));
    // From the email flow.
    ($("#sources-add-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("add"),
    );
    ($("#add-choice-email") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("email"),
    );
    ($("#email-back-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
    expect(win.document.activeElement).toBe($("#sources-add-btn"));
    // From the automatic-imports section back to its own link.
    ($("#sources-automatic-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("automatic"),
    );
    ($("#automatic-back-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
    expect(win.document.activeElement).toBe($("#sources-automatic-btn"));
  });

  it("nav to sources resets to the home section from a deep section", async () => {
    await boot();
    ($("#sources-add-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("add"),
    );
    ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
  });

  it("the email form keeps ids, hides the password row in oauth mode, and shows server/port under Advanced", async () => {
    await boot();
    ($("#sources-add-btn") as HTMLElement).click();
    ($("#add-choice-email") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("email"),
    );
    expect($("#imap-user")).toBeTruthy();
    expect($("#imap-pass")).toBeTruthy();
    expect($("#imap-advanced")).toBeTruthy();
    // Default: app-password mode shows the password row.
    expect(($("#imap-pass-label") as HTMLElement).hidden).toBe(false);
    input("#imap-auth").value = "oauth";
    fireChange(input("#imap-auth"));
    expect(($("#imap-pass") as HTMLElement).hidden).toBe(true);
    expect(($("#imap-pass-label") as HTMLElement).hidden).toBe(true);
  });

  it("first-run buttons deep-link into the import / email sections", async () => {
    tauri.handlers.set("stats", () => ({ total: 0, by_source: [] }));
    await boot();
    ($("#first-run-import-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("import"),
    );
    ($("#first-run-account-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("email"),
    );
  });
});

// ---------------------------------------------------------------------------
// Accounts panel / Sources list (multi-account UI): listing, View messages,
// sync-now params, the Details fold (pause, update credentials, forget,
// disconnect), empty state — the acceptance-critical wiring.
// ---------------------------------------------------------------------------

const IMAP_ACCOUNT = {
  source_id: "inst-imap-1",
  kind: "imap",
  label: "imap:imap.gmail.com:993:ana@gmail.com",
  medium: "mail",
  imap: {
    host: "imap.gmail.com",
    port: 993,
    username: "ana@gmail.com",
    folders: ["INBOX", "Archive"],
    has_password: true,
    has_oauth: false,
  },
  message_count: 1234,
  failed_items: 2,
  state: "connected",
  excluded: false,
  last_attempt_at: "2026-01-02 10:00:00",
  last_success_at: "2026-01-02 10:00:05",
  last_error: null,
  last_error_at: null,
} as const;

const IMPORT_ACCOUNT = {
  source_id: "inst-import-1",
  kind: "import",
  label: "mbox:archive.mbox",
  medium: "mail",
  imap: null,
  message_count: 6,
  failed_items: 0,
  state: "connected",
  excluded: false,
  last_attempt_at: null,
  last_success_at: null,
  last_error: null,
  last_error_at: null,
} as const;

const OAUTH_ACCOUNT = {
  ...IMAP_ACCOUNT,
  source_id: "inst-imap-2",
  label: "imap:imap.gmail.com:993:bob@gmail.com",
  imap: {
    host: "imap.gmail.com",
    port: 993,
    username: "bob@gmail.com",
    folders: [],
    has_password: false,
    has_oauth: true,
  },
} as const;

// The UI fixtures: source ids matching the boot `sources_list` fixtures
// (inst-mbox / inst-demo) so View messages and the nav chips agree.
const IMAP_ACCOUNT_UI = {
  ...IMAP_ACCOUNT,
  source_id: "inst-demo",
  label: "imap:imap.gmail.com:993:ana@gmail.com",
  imap: {
    host: "imap.gmail.com",
    port: 993,
    username: "ana@gmail.com",
    folders: ["INBOX"],
    has_password: true,
    has_oauth: false,
  },
} as const;

const IMPORT_ACCOUNT_UI = {
  ...IMPORT_ACCOUNT,
  source_id: "inst-mbox",
} as const;

describe("accounts panel", () => {
  it("lists accounts with stats, sync state, and per-kind actions", async () => {
    tauri.handlers.set("accounts_list", () => [IMAP_ACCOUNT, IMPORT_ACCOUNT]);
    await boot();
    await vi.waitFor(() =>
      expect($("#sources-list .account-row[data-source-id='inst-imap-1']")).toBeTruthy(),
    );
    const imapRow = $("#sources-list .account-row[data-source-id='inst-imap-1']");
    expect(imapRow.querySelector(".account-label")!.textContent).toBe(
      "ana@gmail.com · imap.gmail.com",
    );
    expect((imapRow.querySelector(".account-label") as HTMLElement).title).toBe(
      "Stored source: imap:imap.gmail.com:993:ana@gmail.com",
    );
    expect(imapRow.querySelector(".account-kind")!.textContent).toBe("Email (IMAP)");
    expect(imapRow.querySelector(".account-stats")!.textContent).toBe(
      "1,234 messages · 2 failing",
    );
    expect(imapRow.querySelector(".account-sync")!.textContent).toContain(
      "last synced 2026-01-02 10:00:05",
    );
    expect(imapRow.querySelector(".account-sync")!.textContent).toContain(
      "credentials: password",
    );
    expect(
      [...imapRow.querySelectorAll("button")].map((b) => b.textContent),
    ).toEqual([
      "View messages",
      "Sync now",
      "Pause",
      "Update credentials…",
      "Forget credentials",
      "Disconnect…",
    ]);
    // The management actions sit inside the Details fold; View messages
    // and Sync now stay primary (outside it), each with an
    // account-scoped accessible name.
    expect(imapRow.querySelector("details.source-details .account-pause-btn")).toBeTruthy();
    expect(
      imapRow.querySelector("details.source-details .source-disconnect-btn"),
    ).toBeTruthy();
    for (const button of imapRow.querySelectorAll("button")) {
      expect(button.getAttribute("aria-label")).toContain("ana@gmail.com");
    }
    // The import account renders with stats and View messages but no
    // management fold and no sync action.
    const importRow = $("#sources-list .account-row[data-source-id='inst-import-1']");
    expect(importRow.querySelector(".account-kind")!.textContent).toBe("Import");
    expect(importRow.querySelector(".source-view-btn")).toBeTruthy();
    expect(importRow.querySelector("details.source-details")).toBe(null);
    expect(importRow.querySelector(".account-sync-btn")).toBe(null);
    expect($("#sources-status").textContent).toContain("2 accounts");
  });

  it("shows the empty state when no account exists", async () => {
    await boot();
    await vi.waitFor(() => expect($("#sources-status").textContent).toBe(""));
    expect($("#sources-list .account-row.empty")).toBeTruthy();
    expect($("#sources-list .account-row.empty").textContent).toContain("No sources yet");
  });

  it("sync now triggers sync_imap with the account's stored parameters", async () => {
    tauri.handlers.set("accounts_list", () => [IMAP_ACCOUNT]);
    let syncArgs: unknown = null;
    tauri.handlers.set("sync_imap", (args) => {
      syncArgs = args;
      return {
        folders: [
          {
            folder: "INBOX",
            uidvalidity: 7,
            reset: false,
            seen: 3,
            inserted: 2,
            skipped: 1,
            failed: 0,
            resolved_failures: 0,
            conflicting_revisions: 0,
            error: null,
          },
        ],
        saved_to: null,
      };
    });
    await boot();
    await vi.waitFor(() => expect($("#sources-list .account-sync-btn")).toBeTruthy());
    ($("#sources-list .account-sync-btn") as HTMLButtonElement).click();
    await vi.waitFor(() => expect(syncArgs).not.toBeNull());
    // The acceptance-critical assertion: the stored account maps to the
    // right sync_imap parameters — identity from the vault, an empty
    // password (the backend resolves the keyring secret), stored folders.
    expect(syncArgs).toEqual({
      host: "imap.gmail.com",
      port: 993,
      username: "ana@gmail.com",
      password: "",
      folders: ["INBOX", "Archive"],
      auth: "password",
    });
    await vi.waitFor(() =>
      expect($("#sources-status").textContent).toContain(
        "Synced ana@gmail.com · imap.gmail.com: INBOX: 2 new, 1 already present",
      ),
    );
  });

  it("an OAuth-only account syncs through Google sign-in with default folders", async () => {
    tauri.handlers.set("accounts_list", () => [OAUTH_ACCOUNT]);
    let syncArgs: unknown = null;
    tauri.handlers.set("sync_imap", (args) => {
      syncArgs = args;
      return { folders: [], saved_to: null };
    });
    await boot();
    await vi.waitFor(() => expect($("#sources-list .account-sync-btn")).toBeTruthy());
    ($("#sources-list .account-sync-btn") as HTMLButtonElement).click();
    await vi.waitFor(() => expect(syncArgs).not.toBeNull());
    expect(syncArgs).toEqual({
      host: "imap.gmail.com",
      port: 993,
      username: "bob@gmail.com",
      password: "",
      folders: null,
      auth: "oauth",
    });
  });

  it("a failed sync reports the error in the panel status", async () => {
    tauri.handlers.set("accounts_list", () => [IMAP_ACCOUNT]);
    tauri.handlers.set("sync_imap", () => {
      throw new Error("no password stored for this account — enter one");
    });
    await boot();
    await vi.waitFor(() => expect($("#sources-list .account-sync-btn")).toBeTruthy());
    ($("#sources-list .account-sync-btn") as HTMLButtonElement).click();
    await vi.waitFor(() =>
      expect($("#sources-status").textContent).toContain(
        "Sync failed for ana@gmail.com · imap.gmail.com",
      ),
    );
    expect($("#sources-status").textContent).toContain("no password stored");
  });

  it("pause triggers source_set_excluded and re-renders the row", async () => {
    let excluded = false;
    tauri.handlers.set("accounts_list", () => [
      { ...IMAP_ACCOUNT, excluded },
    ]);
    tauri.handlers.set("source_set_excluded", (args) => {
      excluded = (args as { excluded: boolean }).excluded;
      return null;
    });
    await boot();
    await vi.waitFor(() => expect($("#sources-list .account-pause-btn")).toBeTruthy());
    // The management actions live in the Details fold: open it first
    // (happy-dom needs `open` set; no summary click required).
    ($("#sources-list details.source-details") as HTMLDetailsElement).open = true;
    ($("#sources-list .account-pause-btn") as HTMLButtonElement).click();
    await vi.waitFor(() =>
      expect(
        tauri.calls.find(
          (c) =>
            c.cmd === "source_set_excluded" &&
            (c.args as { sourceId: string; excluded: boolean }).sourceId === "inst-imap-1" &&
            (c.args as { sourceId: string; excluded: boolean }).excluded === true,
        ),
      ).toBeTruthy(),
    );
    await vi.waitFor(() =>
      expect($("#sources-list .account-pause-btn").textContent).toBe("Resume"),
    );
    expect($("#sources-status").textContent).toContain("paused");
  });

  it("forget credentials triggers the removal command and re-lists", async () => {
    tauri.handlers.set("accounts_list", () => [IMAP_ACCOUNT]);
    tauri.handlers.set("account_forget_credentials", () => true);
    await boot();
    await vi.waitFor(() => expect($("#sources-list .account-forget-btn")).toBeTruthy());
    ($("#sources-list details.source-details") as HTMLDetailsElement).open = true;
    ($("#sources-list .account-forget-btn") as HTMLButtonElement).click();
    await vi.waitFor(() =>
      expect(
        tauri.calls.find(
          (c) =>
            c.cmd === "account_forget_credentials" &&
            (c.args as { sourceId: string }).sourceId === "inst-imap-1",
        ),
      ).toBeTruthy(),
    );
    await vi.waitFor(() =>
      expect($("#sources-status").textContent).toContain("Forgot stored credentials"),
    );
  });

  it("the sources home lists accounts; no Accounts destination exists", async () => {
    // The handler is installed before boot: the panel renders from the
    // boot-time refresh (boot refresh unchanged this task).
    tauri.handlers.set("accounts_list", () => [
      {
        source_id: "inst-mail",
        kind: "imap",
        label: "imap:imap.gmail.com:993:ana@gmail.com",
        medium: "mail",
        imap: {
          host: "imap.gmail.com", port: 993, username: "ana@gmail.com",
          folders: ["INBOX"], has_password: true, has_oauth: false,
        },
        message_count: 10, failed_items: 0, state: "connected", excluded: false,
        last_attempt_at: null, last_success_at: "2026-01-02 10:00:00",
        last_error: null, last_error_at: null,
      },
    ]);
    await boot();
    ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#sources-list .account-row").textContent).toContain("ana@gmail.com"));
    expect(win.document.querySelector('.nav-view-btn[data-view-target="accounts"]')).toBe(null);
    expect(win.document.querySelector("#view-accounts")).toBe(null);
  });

  it("Refresh re-lists the accounts", async () => {
    let listed = 0;
    tauri.handlers.set("accounts_list", () => {
      listed += 1;
      return [];
    });
    await boot();
    const before = listed;
    ($("#sources-refresh") as HTMLButtonElement).click();
    await vi.waitFor(() => expect(listed).toBeGreaterThan(before));
  });

  it("View messages scopes the archive to that source id and returns to results", async () => {
    // The handler must be installed before boot: the panel renders from
    // the boot-time accounts refresh.
    tauri.handlers.set("accounts_list", () => [IMPORT_ACCOUNT_UI]); // id "inst-mbox"
    await bootSearched();
    ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#sources-list .source-view-btn")).toBeTruthy());
    ($("#sources-list .source-view-btn") as HTMLElement).click();
    await vi.waitFor(() => expect(($("#app") as HTMLElement).dataset.view).toBe("workspace"));
    const args = searchCalls().at(-1)!.args as { request: { source_ids: string[] } };
    expect(args.request.source_ids).toEqual(["inst-mbox"]);
    // The focus contract: landing on the results view puts focus in the
    // search box, so the next keystroke types a query.
    expect(win.document.activeElement).toBe($("#search"));
  });

  it("Update credentials prefills the email form from the account", async () => {
    tauri.handlers.set("accounts_list", () => [IMAP_ACCOUNT_UI]); // username ana@gmail.com, host imap.gmail.com, folders ["INBOX"]
    await boot();
    ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#sources-list .source-update-credentials-btn")).toBeTruthy());
    ($("#sources-list .source-update-credentials-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("email"),
    );
    expect((input("#imap-user") as HTMLInputElement).value).toBe("ana@gmail.com");
    expect((input("#imap-host") as HTMLInputElement).value).toBe("imap.gmail.com");
    expect((input("#imap-folders") as HTMLInputElement).value).toBe("INBOX");
    // The stored password decides the form's auth mode: password mode,
    // password row visible, focus on the password input.
    expect(($("#imap-auth") as HTMLSelectElement).value).toBe("password");
    expect(($("#imap-pass") as HTMLElement).hidden).toBe(false);
    expect(win.document.activeElement).toBe($("#imap-pass"));
    expect($("#imap-status").textContent).toBe(
      "Re-enter the password for ana@gmail.com · imap.gmail.com, then Connect and sync.",
    );
  });

  it("Update credentials arms OAuth mode for an OAuth-only account", async () => {
    // bob@gmail.com has only a Google sign-in stored: the form must open
    // in oauth mode — password row hidden, Google button shown and
    // focused — with the sign-in-again status line.
    tauri.handlers.set("accounts_list", () => [OAUTH_ACCOUNT]);
    await boot();
    ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#sources-list .source-update-credentials-btn")).toBeTruthy());
    ($("#sources-list .source-update-credentials-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("email"),
    );
    expect(($("#imap-auth") as HTMLSelectElement).value).toBe("oauth");
    expect(($("#imap-pass") as HTMLElement).hidden).toBe(true);
    expect(($("#imap-oauth-btn") as HTMLElement).hidden).toBe(false);
    expect(win.document.activeElement).toBe($("#imap-oauth-btn"));
    expect($("#imap-status").textContent).toBe(
      "Sign in with Google again for bob@gmail.com · imap.gmail.com, then Connect and sync.",
    );
  });

  it("Disconnect pauses the source, forgets credentials, and says the archive stays", async () => {
    tauri.handlers.set("accounts_list", () => [IMAP_ACCOUNT_UI]);
    tauri.handlers.set("source_set_excluded", () => null);
    tauri.handlers.set("account_forget_credentials", () => true);
    await boot();
    ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
    await vi.waitFor(() => expect($("#sources-list .source-disconnect-btn")).toBeTruthy());
    ($("#sources-list details.source-details") as HTMLDetailsElement).open = true;
    const btn = $("#sources-list .source-disconnect-btn") as HTMLButtonElement;
    btn.click();
    btn.click();
    await vi.waitFor(() =>
      expect(
        tauri.calls.some(
          (c) =>
            c.cmd === "source_set_excluded" &&
            (c.args as { excluded: boolean }).excluded === true,
        ),
      ).toBe(true),
    );
    await vi.waitFor(() =>
      expect(tauri.calls.some((c) => c.cmd === "account_forget_credentials")).toBe(true),
    );
    await vi.waitFor(() =>
      expect($("#sources-status").textContent).toContain("archive stays"),
    );
  });
});

// Storage & backup panel (U05: persisted last verified backup + the
// restart-after-switch origin line + the section's first-class spot)
// ---------------------------------------------------------------------------

describe("settings pages", () => {
  it("renders three pages with a switcher; default is General", async () => {
    await boot();
    expect(($("#app") as HTMLElement).dataset.settingsSection).toBe("general");
    const targets = [...document.querySelectorAll(".settings-nav-btn")].map(
      (b) => (b as HTMLElement).dataset.settingsTarget,
    );
    expect(targets).toEqual(["general", "storage", "ai"]);
    // General holds the theme/notifications/background controls; the
    // backup group lives on the storage page; providers on the AI page.
    expect($("#settings-general")!.contains($("#theme"))).toBe(true);
    expect($("#settings-general")!.contains($("#keepalive-toggle"))).toBe(true);
    expect($("#settings-storage")!.contains($("#backup"))).toBe(true);
    expect($("#settings-storage")!.contains($("#media-store"))).toBe(true);
    expect($("#settings-ai")!.contains($("#provider-rows"))).toBe(true);
  });

  it("switching pages moves aria-current and loads storage data on the storage page", async () => {
    await boot();
    tauri.handlers.set("last_backup", () => null);
    ($('.nav-view-btn[data-view-target="settings"]') as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.view).toBe("settings"),
    );
    expect(tauri.calls.some((c) => c.cmd === "media_usage")).toBe(false);
    ($('.settings-nav-btn[data-settings-target="storage"]') as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.settingsSection).toBe("storage"),
    );
    await vi.waitFor(() =>
      expect(tauri.calls.some((c) => c.cmd === "media_usage")).toBe(true),
    );
    expect(
      ($('.settings-nav-btn[data-settings-target="storage"]') as HTMLElement).getAttribute("aria-current"),
    ).toBe("page");
    expect(
      ($('.settings-nav-btn[data-settings-target="general"]') as HTMLElement).getAttribute("aria-current"),
    ).toBe(null);
  });

  it("each page's Save posts the same full settings payload", async () => {
    await boot();
    ($('.nav-view-btn[data-view-target="settings"]') as HTMLElement).click();
    ($("#settings-save-general") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(tauri.calls.some((c) => c.cmd === "settings_save")).toBe(true),
    );
  });

  it("the settings note sits under the switcher, visible from every page", async () => {
    await boot();
    const note = $("#settings-note")!;
    // Inside the settings view (so it renders there) but never inside one
    // of the pages — a page-scoped note is hidden with its page, and the
    // save feedback ("Saving…/Saved./Not saved") must be reachable from
    // whichever page's Save was pressed.
    expect($("#view-settings")!.contains(note)).toBe(true);
    expect($("#settings-general")!.contains(note)).toBe(false);
    expect($("#settings-storage")!.contains(note)).toBe(false);
    expect($("#settings-ai")!.contains(note)).toBe(false);
    expect($("#settings-general")!.previousElementSibling).toBe(note);
  });
});

describe("storage & backup panel (U05)", () => {
  it("the storage page leads with Storage & backup, then the media store", async () => {
    await boot();
    const storage = $("#settings-storage")!;
    const kids = [...storage.querySelectorAll(":scope > section")].map((s) => s.id);
    expect(kids).toEqual(["backup", "media-store"]);
  });

  it("shows the persisted last verified backup when the storage page opens", async () => {
    tauri.handlers.set("last_backup", () => ({
      created_at: "2026-11-27T15:30:45Z",
      path: "/media/vault-2026-11-27.mcbackup",
      size: 2048,
      counts: { messages: 1234, sources: 3, media_objects: 57 },
      still_present: true,
    }));
    await boot();
    $(".nav-view-btn[data-view-target='settings']").click();
    $('.settings-nav-btn[data-settings-target="storage"]').click();
    await vi.waitFor(() =>
      expect($("#backup-status").textContent).toContain("Last verified backup: 2026-11-27T15:30:45Z"),
    );
    expect($("#backup-status").textContent).toContain("/media/vault-2026-11-27.mcbackup");
  });

  it("the empty state explains what the last-backup line means", async () => {
    tauri.handlers.set("last_backup", () => null);
    await boot();
    $(".nav-view-btn[data-view-target='settings']").click();
    $('.settings-nav-btn[data-settings-target="storage"]').click();
    await vi.waitFor(() =>
      expect($("#backup-status").textContent).toContain("No verified backup yet"),
    );
  });

  it("a flag is raised when the recorded container is no longer on disk", async () => {
    tauri.handlers.set("last_backup", () => ({
      created_at: "2026-11-27T15:30:45Z",
      path: "/media/moved-away.mcbackup",
      size: 2048,
      counts: { messages: 1, sources: 1, media_objects: 0 },
      still_present: false,
    }));
    await boot();
    $(".nav-view-btn[data-view-target='settings']").click();
    $('.settings-nav-btn[data-settings-target="storage"]').click();
    await vi.waitFor(() =>
      expect($("#backup-status").textContent).toContain("the file is no longer at that path"),
    );
  });

  it("says what the active vault is, since when, from where, and where it lives", async () => {
    tauri.handlers.set("recovery_status", () => ({
      journal_state: "completed",
      journal_created_at: "2026-11-27T15:30:45Z",
      staged_preview: false,
      previous: [],
      last_switch: {
        kind: "restored",
        archive: "/media/vault-2026-11-27.mcbackup",
        since: "2026-11-27T15:30:45Z",
        recovery: "none",
      },
      data_dir: "/home/me/.local/share/mailchat-vault",
    }));
    await boot();
    $(".nav-view-btn[data-view-target='settings']").click();
    $('.settings-nav-btn[data-settings-target="storage"]').click();
    await vi.waitFor(() =>
      expect($("#vault-origin").textContent).toBe(
        "Active vault: switched in from the backup /media/vault-2026-11-27.mcbackup at 2026-11-27T15:30:45Z.",
      ),
    );
    expect($("#vault-location").textContent).toBe(
      "Archive location: /home/me/.local/share/mailchat-vault",
    );
  });

  it("a never-switched vault shows no origin line, just the location", async () => {
    tauri.handlers.set("recovery_status", () => ({
      journal_state: null,
      journal_created_at: null,
      staged_preview: false,
      previous: [],
      last_switch: null,
      data_dir: "/home/me/.local/share/mailchat-vault",
    }));
    await boot();
    $(".nav-view-btn[data-view-target='settings']").click();
    $('.settings-nav-btn[data-settings-target="storage"]').click();
    await vi.waitFor(() =>
      expect($("#vault-location").textContent).toContain("Archive location:"),
    );
    expect($("#vault-origin").textContent).toBe("");
  });

  it("an interrupted switch resolved by startup recovery is said out loud", async () => {
    tauri.handlers.set("recovery_status", () => ({
      journal_state: "completed",
      journal_created_at: "2026-11-27T15:30:45Z",
      staged_preview: false,
      previous: [],
      last_switch: {
        kind: "restored",
        archive: "/media/vault-2026-11-27.mcbackup",
        since: "2026-11-27T15:30:45Z",
        recovery: "rolled_back",
      },
      data_dir: "/home/me/.local/share/mailchat-vault",
    }));
    await boot();
    $(".nav-view-btn[data-view-target='settings']").click();
    $('.settings-nav-btn[data-settings-target="storage"]').click();
    await vi.waitFor(() =>
      expect($("#vault-origin").textContent).toContain("interrupted and rolled back at startup"),
    );
  });

  it("a sync with attachments renders the folder's media counters", async () => {
    await boot();
    tauri.handlers.set("sync_imap", () => ({
      folders: [
        {
          folder: "INBOX",
          uidvalidity: 7,
          reset: false,
          seen: 2,
          inserted: 2,
          skipped: 0,
          failed: 0,
          resolved_failures: 0,
          conflicting_revisions: 0,
          error: null,
          media: { stored: 2, deduped: 1, missing: 0 },
        },
      ],
      saved_to: null,
    }));
    input("#imap-host").value = "imap.example.com";
    input("#imap-user").value = "a@b.c";
    input("#imap-pass").value = "pw";
    await clickSync();
    await vi.waitFor(() =>
      expect($("#imap-status").textContent).toContain(
        "INBOX: 2 new, 0 already present, media: 2 stored, 1 deduped",
      ),
    );
  });
});

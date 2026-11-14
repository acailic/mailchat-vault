// @vitest-environment happy-dom
// DOM tests for the v2 rendering (M3.2): flat results rows with badges
// and selection checkboxes, era chips, applied filter chips, import-scope
// picker, refresh notice, and the reading pane (detail + context +
// actions).

import { describe, expect, it, vi } from "vitest";
import {
  fillImportScopes,
  renderDetail2,
  renderEraChips,
  renderFilterChips,
  renderFlatResults,
  renderRefreshNotice,
  timeLabel,
} from "./render2";
import { contextEntries } from "./searchstate";
import type { ConversationContext, MessageSummary } from "./v2types";

// Timestamps render timezone-independently (formatSentAt is covered by
// format.test.ts); the local clock only pads fixed fields here.
vi.mock("./format", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./format")>();
  return {
    ...actual,
    formatSentAt: (iso: string) => iso || "unknown time",
  };
});

function hit(over: Partial<MessageSummary> = {}): MessageSummary {
  return {
    id: 1,
    source_id: "s1",
    source_label: "mbox",
    conversation_id: "c1",
    conversation_label: "thread-a",
    author: "Ana",
    subject: "Delivery date",
    sent_at: "2026-01-02T10:00:00Z",
    time_confidence: "legacy_assumed",
    snippet: "",
    attachment_count: 0,
    available_attachment_count: 0,
    has_conflicting_revision: false,
    ...over,
  };
}

describe("flat results", () => {
  it("renders one flat row per hit in the backend order", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [hit({ id: 1 }), hit({ id: 2, author: "Bo" })],
      activeIndex: 1,
      selectedId: null,
      selection: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
    });
    const rows = el.querySelectorAll("li.hit-item");
    expect(rows.length).toBe(2);
    expect(rows[1].classList.contains("active")).toBe(true);
    expect(rows[0].textContent).toContain("Ana");
    expect(rows[1].textContent).toContain("Bo");
    // Flat: no group headers between rows.
    expect(el.querySelector(".group")).toBeNull();
  });

  it("the row's button is the main area, never the list item around the checkbox", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [hit({ author: "Mailchat Vault <demo@mailchat-vault.local>" })],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
    });
    // The list item is a plain list item: the checkbox inside it is a
    // sibling interactive control, not a control nested in a button
    // (no double announcement, no Space-key ambiguity).
    const li = el.querySelector("li.hit-item") as HTMLElement;
    expect(li.getAttribute("role")).toBeNull();
    expect(li.getAttribute("tabindex")).toBeNull();
    const main = li.querySelector(".hit-main") as HTMLElement;
    expect(main.getAttribute("role")).toBe("button");
    expect(main.getAttribute("tabindex")).toBe("0");
    expect(main.getAttribute("title")).toBe("Open this message");
    const check = li.querySelector("input.row-select") as HTMLInputElement;
    expect(check.getAttribute("aria-label")).toBe("Select message 1 for export");
    // The meta line shows the display name; the full address stays in
    // the title attribute.
    const meta = main.querySelector(".meta") as HTMLElement;
    expect(meta.textContent).toContain("Mailchat Vault · ");
    expect(meta.textContent).not.toContain("demo@mailchat-vault.local");
    expect(meta.getAttribute("title")).toBe(
      "Mailchat Vault <demo@mailchat-vault.local>",
    );
  });

  it("marks the hit markers as <mark> runs", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [hit({ snippet: "the \x01vault\x02 arrives" })],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
    });
    const mark = el.querySelector("mark.hit");
    expect(mark?.textContent).toBe("vault");
  });

  it("attachment and conflict badges render honestly", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [
        hit({ attachment_count: 2, available_attachment_count: 1 }),
        hit({ has_conflicting_revision: true }),
      ],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
    });
    const badges = el.querySelectorAll(".badge.attachments");
    expect(badges[0].textContent).toBe("1/2");
    expect(badges[0].getAttribute("aria-label")).toBe("attachments: 1 of 2");
    expect(badges[0].innerHTML).toContain("#i-paperclip");
    expect(el.querySelector(".badge.conflict")?.textContent).toBe("revisions");
  });

  it("unknown time never renders a fabricated timestamp", () => {
    expect(timeLabel(hit({ sent_at: null, time_confidence: "missing" }))).toBe("unknown time");
    expect(timeLabel(hit({ time_confidence: "legacy_assumed" }))).toContain("(assumed)");
    expect(timeLabel(hit({ time_confidence: "explicit_offset" }))).not.toContain("(");
  });

  it("row selection checkboxes reflect and drive the selection", () => {
    const el = document.createElement("div");
    const toggled: number[] = [];
    renderFlatResults(el, {
      hits: [hit({ id: 5 }), hit({ id: 6 })],
      activeIndex: -1,
      selectedId: null,
      selection: new Set([6]),
      terms: [],
      onOpen: () => {},
      onToggleSelect: (id) => toggled.push(id),
    });
    const boxes = el.querySelectorAll("input.row-select");
    expect(boxes.length).toBe(2);
    expect((boxes[0] as HTMLInputElement).checked).toBe(false);
    expect((boxes[1] as HTMLInputElement).checked).toBe(true);
    boxes[0].dispatchEvent(new Event("change"));
    expect(toggled).toEqual([5]);
  });

  it("clicking a row opens the message", () => {
    const el = document.createElement("div");
    const opened: number[] = [];
    renderFlatResults(el, {
      hits: [hit({ id: 9 })],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      terms: [],
      onOpen: (id) => opened.push(id),
      onToggleSelect: () => {},
    });
    (el.querySelector("li.hit-item") as HTMLElement).click();
    expect(opened).toEqual([9]);
  });

  it("Enter and Space activate the row's button; the checkbox stays its own control", () => {
    const el = document.createElement("div");
    const opened: number[] = [];
    renderFlatResults(el, {
      hits: [hit({ id: 9 })],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      terms: [],
      onOpen: (id) => opened.push(id),
      onToggleSelect: () => {},
    });
    const main = el.querySelector(".hit-main") as HTMLElement;
    main.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
    );
    main.dispatchEvent(
      new KeyboardEvent("keydown", { key: " ", bubbles: true, cancelable: true }),
    );
    expect(opened).toEqual([9, 9]);
    // The checkbox is a sibling control: its clicks never open the row.
    (el.querySelector("input.row-select") as HTMLElement).click();
    expect(opened).toEqual([9, 9]);
  });
});

describe("filter chips", () => {
  it("renders removable chips and clears all", () => {
    const el = document.createElement("div");
    const removed: string[] = [];
    let cleared = 0;
    renderFilterChips(el, {
      chips: [
        { kind: "source", label: "Personal mail", value: "s1" },
        { kind: "attachment", label: "has attachment", value: "" },
      ],
      onRemove: (chip) => removed.push(chip.kind),
      onClear: () => cleared++,
    });
    const chips = el.querySelectorAll("button.filter-chip");
    expect(chips.length).toBe(2);
    expect(chips[0].textContent).toContain("Personal mail");
    (chips[0] as HTMLElement).click();
    expect(removed).toEqual(["source"]);
    (el.querySelector("button.clear-chips") as HTMLElement).click();
    expect(cleared).toBe(1);
  });

  it("no active filters render no chips", () => {
    const el = document.createElement("div");
    renderFilterChips(el, { chips: [], onRemove: () => {}, onClear: () => {} });
    expect(el.children.length).toBe(0);
  });
});

describe("era chips", () => {
  const buckets = [
    { label: "2019", from: "2019-01-01", to: "2019-12-31" },
    { label: "2020", from: "2020-01-01", to: "2020-12-31" },
  ];

  it("renders one pressed toggle per bucket; clicks report the index", () => {
    const el = document.createElement("div");
    const toggled: number[] = [];
    renderEraChips(el, { buckets, activeIndex: 1, onToggle: (i) => toggled.push(i) });
    const chips = [...el.querySelectorAll("button.era-chip")] as HTMLElement[];
    expect(chips.map((c) => c.textContent)).toEqual(["2019", "2020"]);
    expect(chips.map((c) => c.getAttribute("aria-pressed"))).toEqual(["false", "true"]);
    expect(chips[1].classList.contains("on")).toBe(true);
    // Controls name their action; the active chip names removal.
    expect(chips[0].title).toContain("Filter to 2019");
    expect(chips[1].title).toContain("Remove the 2020 date range");
    chips[0].click();
    chips[0].click();
    expect(toggled).toEqual([0, 0]);
  });

  it("fewer than two buckets render nothing (no dead chrome)", () => {
    const el = document.createElement("div");
    renderEraChips(el, { buckets: [], activeIndex: -1, onToggle: () => {} });
    expect(el.children.length).toBe(0);
    renderEraChips(el, {
      buckets: [{ label: "2026", from: "2026-01-01", to: "2026-12-31" }],
      activeIndex: 0,
      onToggle: () => {},
    });
    expect(el.children.length).toBe(0);
  });
});

describe("import scope picker", () => {
  it("lists scopes with human labels and keeps the selection", () => {
    const select = document.createElement("select");
    fillImportScopes(
      select,
      [
        {
          import_id: "imp-a",
          messages: 12,
          // Local noon: the same calendar day in every timezone.
          first_observed_at: "2026-11-01T12:00:00",
          last_observed_at: "2026-11-02T12:00:00",
        },
        { import_id: "imp-b", messages: 3, first_observed_at: "", last_observed_at: "" },
      ],
      "imp-b",
    );
    const options = [...select.querySelectorAll("option")];
    expect(options.map((o) => o.value)).toEqual(["", "imp-a", "imp-b"]);
    expect(options[0].textContent).toBe("any import");
    // The raw import id never leads the label; it stays one hover away.
    expect(options[1].textContent).toBe("Imported Nov 1, 2026 — 12 messages");
    expect(options[1].title).toBe("imp-a");
    expect(options[2].textContent).toBe("3 messages");
    expect(options[2].title).toBe("imp-b");
    expect(select.value).toBe("imp-b");
  });

  it("an unknown selection falls back to any", () => {
    const select = document.createElement("select");
    fillImportScopes(select, [], "gone");
    expect(select.value).toBe("");
  });
});

describe("refresh notice", () => {
  it("appears with a Refresh button only when visible", () => {
    const el = document.createElement("div");
    let refreshed = 0;
    renderRefreshNotice(el, false, () => refreshed++);
    expect(el.hidden).toBe(true);
    expect(el.children.length).toBe(0);
    renderRefreshNotice(el, true, () => refreshed++);
    expect(el.hidden).toBe(false);
    expect(el.textContent).toContain("archive changed");
    (el.querySelector("button") as HTMLElement).click();
    expect(refreshed).toBe(1);
  });
});

describe("reading pane", () => {
  const ctx: ConversationContext = {
    anchor: {
      id: 3,
      source_id: "s1",
      source_label: "mbox",
      channel: "email",
      chat_id: "thread-a",
      external_id: "e2e-m-3",
      conversation_id: "c1",
      conversation_label: "thread-a",
      conversation_confidence: "verified",
      author: "Ana",
      subject: "Delivery date",
      body: "The \x01vault\x21 arrives".replace("\x21", "\x02"),
      sent_at: "2026-01-02T10:00:00Z",
      original_timestamp: "Fri, 02 Jan 2026 11:00:00 +0100",
      timezone: "Europe/Zagreb",
      time_confidence: "legacy_assumed",
      attachments: [
        { name: "spec.pdf", kind: "document", size: 2048, available: true, path: null },
        { name: "lost.jpg", kind: "image", size: 0, available: false },
      ],
      occurrences: [
        {
          scope: "import",
          provider_item_id: "e2e-m-3",
          uidvalidity: null,
          import_id: "imp-a",
          observed_at: "2026-11-01 12:00:00",
        },
      ],
      revisions: [{ message_id: 7, identity: "legacy:e2e-m-3", revision_hash: "h2" }],
      raw_json: "{\"keep\": 1}",
    },
    before: [
      {
        id: 1,
        author: "Ana",
        sent_at: "2026-01-01T10:00:00Z",
        time_confidence: "explicit_offset",
        body: "earlier message",
      },
    ],
    after: [
      {
        id: 4,
        author: "Bo",
        sent_at: "2026-01-03T10:00:00Z",
        time_confidence: "explicit_offset",
        body: "later message",
      },
    ],
    unverified_conversation: false,
    has_more_before: true,
    has_more_after: true,
  };

  function render(over: Partial<Parameters<typeof renderDetail2>[1]> = {}) {
    const el = document.createElement("aside");
    renderDetail2(el, {
      ctx,
      entries: contextEntries(ctx),
      queryText: "vault",
      hasMoreBefore: true,
      hasMoreAfter: true,
      onOpenMessage: () => {},
      onLoadEarlier: () => {},
      onLoadLater: () => {},
      onCopy: () => {},
      onCopyLink: () => {},
      onExport: () => {},
      onExportChat: () => {},
      onExportMedia: () => {},
      onOpenExternalMedia: () => {},
      onClose: () => {},
      ...over,
    });
    return el;
  }

  it("renders the detail card: author title, meta table, highlighted body", () => {
    const el = render();
    const card = el.querySelector(".detail-card") as HTMLElement;
    expect(card.querySelector("h2")?.textContent).toBe("Ana");
    const table = card.querySelector(".meta-table")?.textContent ?? "";
    expect(table).toContain("mbox");
    expect(table).toContain("e2e-m-3");
    expect(table).toContain("Delivery date");
    // F08: confidence reads as a word, never the raw store token, and
    // the tooltip carries the one-sentence explanation.
    expect(table).toContain("(assumed)");
    expect(table).not.toContain("legacy_assumed");
    expect(table).toContain("thread-a (verified)");
    const dateCell = [...card.querySelectorAll(".meta-table dd")].find((dd) =>
      dd.textContent?.includes("(assumed)"),
    ) as HTMLElement;
    expect(dateCell.title).toContain("Time certainty: assumed — ");
    expect(card.querySelectorAll("ol.thread li.thread-row").length).toBe(3);
    expect(card.querySelectorAll("li.thread-row.current").length).toBe(1);
  });

  it("attachments list separates preserved from not preserved", () => {
    const el = render();
    const items = [...el.querySelectorAll("li.media-item")];
    expect(items.length).toBe(2);
    expect(items[0].textContent).toContain("spec.pdf");
    expect(items[0].textContent).toContain("2 KB");
    expect(items[1].classList.contains("unresolved")).toBe(true);
    expect(items[1].textContent).toContain("not in export");
  });

  it("load earlier/later buttons reflect the has-more flags", () => {
    const el = render({ hasMoreBefore: false, hasMoreAfter: true });
    const earlier = el.querySelector("button.load-earlier-btn") as HTMLButtonElement;
    const later = el.querySelector("button.load-later-btn") as HTMLButtonElement;
    expect(earlier.disabled).toBe(true);
    expect(later.disabled).toBe(false);
  });

  it("the unverified grouping shows the notice and no neighbors", () => {
    const unverified = { ...ctx, unverified_conversation: true, before: [], after: [] };
    const el = render({ ctx: unverified, entries: contextEntries(unverified) });
    expect(el.textContent).toContain("Conversation could not be verified");
    // The conversation row itself says why in user terms.
    expect(el.textContent).toContain("thread-a (unverified grouping)");
    expect(el.querySelectorAll("li.thread-row").length).toBe(1);
  });

  it("actions are wired: copy, copy link, export, export chat, close, open neighbor", () => {
    const calls: string[] = [];
    const el = render({
      onCopy: () => calls.push("copy"),
      onCopyLink: () => calls.push("link"),
      onExport: () => calls.push("export"),
      onExportChat: () => calls.push("export-chat"),
      onClose: () => calls.push("close"),
      onOpenMessage: (id) => calls.push(`open:${id}`),
    });
    (el.querySelector("button.copy-btn") as HTMLElement).click();
    (el.querySelector("button.copy-link-btn") as HTMLElement).click();
    (el.querySelector("button.export-selection-btn") as HTMLElement).click();
    (el.querySelector("button.export-chat-btn") as HTMLElement).click();
    (el.querySelector("button.close-btn") as HTMLElement).click();
    (el.querySelector("li.thread-row.link") as HTMLElement).click();
    expect(calls).toEqual(["copy", "link", "export", "export-chat", "close", "open:1"]);
  });

  it("header utilities are icon-only buttons named by aria-label", () => {
    const el = render();
    const actions = el.querySelector(".header-actions") as HTMLElement;
    const named = (selector: string) => {
      const button = actions.querySelector(selector) as HTMLElement;
      expect(button.textContent).toBe(""); // icon only, no text label
      expect(button.getAttribute("aria-label")).toBeTruthy();
      expect(button.title).toBeTruthy();
      return button;
    };
    named("button.copy-btn");
    named("button.copy-link-btn");
    named("button.close-btn");
    // Back to results is gone from the header: the swap layouts own it in
    // the reading toolbar, and the wide pane gets a plain Close instead.
    expect(actions.textContent).not.toContain("Back to results");
    // Three icon utilities + the Export… disclosure: nothing else.
    expect([...actions.children].map((c) => c.tagName)).toEqual([
      "BUTTON",
      "BUTTON",
      "DETAILS",
      "BUTTON",
    ]);
  });

  it("Export… opens a picker with this message and the conversation", () => {
    const el = render();
    const picker = el.querySelector("details.reading-export") as HTMLDetailsElement;
    expect(picker.open).toBe(false);
    const summary = picker.querySelector("summary") as HTMLElement;
    expect(summary.getAttribute("aria-label")).toBe("Export");
    const rows = [...picker.querySelectorAll(".export-row")];
    expect(rows.length).toBe(2);
    expect(rows[0].textContent).toContain("This message");
    const messageFormat = rows[0].querySelector("select") as HTMLSelectElement;
    expect([...messageFormat.options].map((o) => o.value)).toEqual(["text", "json"]);
    expect(rows[1].textContent).toContain("Conversation");
    const chatFormat = rows[1].querySelector("select") as HTMLSelectElement;
    expect([...chatFormat.options].map((o) => o.value)).toEqual(["csv", "json", "mbox"]);
  });

  it("a re-render keeps the chosen formats and the picker's open state", () => {
    const el = render();
    const picker = el.querySelector("details.reading-export") as HTMLDetailsElement;
    picker.open = true;
    (el.querySelector("#message-format") as HTMLSelectElement).value = "json";
    (el.querySelector("#chat-format") as HTMLSelectElement).value = "mbox";
    // Same message (a context page load / background refresh):
    renderDetail2(el, {
      ctx,
      entries: contextEntries(ctx),
      queryText: "vault",
      hasMoreBefore: true,
      hasMoreAfter: true,
      onOpenMessage: () => {},
      onLoadEarlier: () => {},
      onLoadLater: () => {},
      onCopy: () => {},
      onCopyLink: () => {},
      onExport: () => {},
      onExportChat: () => {},
      onExportMedia: () => {},
      onOpenExternalMedia: () => {},
      onClose: () => {},
    });
    expect((el.querySelector("details.reading-export") as HTMLDetailsElement).open).toBe(true);
    expect((el.querySelector("#message-format") as HTMLSelectElement).value).toBe("json");
    expect((el.querySelector("#chat-format") as HTMLSelectElement).value).toBe("mbox");
  });

  it("raw JSON stays collapsed behind Technical details", () => {
    const el = render();
    const details = el.querySelector("details.detail-technical") as HTMLDetailsElement;
    expect(details.open).toBe(false);
    expect(details.querySelector("pre.raw-json")?.textContent).toContain('"keep": 1');
  });
});

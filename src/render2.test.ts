// @vitest-environment happy-dom
// DOM tests for the v2 rendering (M3.2): flat results rows with badges
// and selection checkboxes, era chips, applied filter chips, import-scope
// picker, refresh notice, the reading pane (detail + context + actions),
// pagination, source chips, and the statistics dashboard.

import { describe, expect, it, vi } from "vitest";
import type { SourceChipRow } from "./accountsview";
import {
  fillImportScopes,
  renderDashboard,
  renderDetail2,
  renderEraChips,
  renderFilterChips,
  renderFlatResults,
  renderPagination,
  renderRefreshNotice,
  renderSourceChips,
  timeLabel,
} from "./render2";
import { contextEntries } from "./searchstate";
import type { ArchiveStats } from "./types";
import { HIGHLIGHT_MAX_MARKS } from "./viewmodel";
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
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    const rows = el.querySelectorAll("li.hit-item");
    expect(rows.length).toBe(2);
    expect(rows[1].classList.contains("active")).toBe(true);
    expect(rows[0].textContent).toContain("Ana");
    expect(rows[1].textContent).toContain("Bo");
    // The raw `mbox` source label reads as plain words on the row.
    expect(rows[0].textContent).toContain("Mail import");
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
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    // The list item is the grid row — NOT the button: the checkbox
    // inside it stays a sibling interactive control, not a control
    // nested in a button (no double announcement, no Space-key
    // ambiguity), and the row itself carries the selection state.
    const li = el.querySelector("li.hit-item") as HTMLElement;
    expect(li.getAttribute("role")).toBe("row");
    expect(li.getAttribute("aria-selected")).toBe("false");
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
    expect(meta.querySelector(".hit-author")?.textContent).toBe("Mailchat Vault");
    expect(meta.textContent).toContain("Mailchat Vault ");
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
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    const mark = el.querySelector("mark.hit");
    expect(mark?.textContent).toBe("vault");
  });

  it("never interpolates message content as HTML", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [
        hit({
          author: '<b>"evil"</b>',
          subject: '<img src=x onerror="alert(1)">',
          snippet: "ok \x01body\x02",
        }),
      ],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    expect(el.querySelectorAll("script,img,b").length).toBe(0);
    expect(el.innerHTML.includes("<img")).toBe(false);
  });

  it("a long snippet renders whole under the row's CSS two-line clamp", () => {
    // The visual clamp lives in app.css (.hit-item .body,
    // -webkit-line-clamp: 2) — this guards the DOM contract it hangs on:
    // the snippet text arrives whole on the .body element (the backend
    // already bounds it), with mark runs intact, and no JS truncation
    // trims it before the clamp can do its quiet work.
    const long =
      "quarterly vault report attached please review before the board " +
      "meeting on friday because the numbers changed after the export " +
      "and the summary no longer matches what finance signed off on";
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [hit({ snippet: `report \x01numbers\x02 changed ${long}` })],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    const body = el.querySelector("li.hit-item .body") as HTMLElement;
    expect(body.className).toBe("body");
    expect(body.textContent).toBe(`report numbers changed ${long}`);
    expect(el.querySelector("mark.hit")?.textContent).toBe("numbers");
  });

  it("attachment and conflict badges render honestly", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [
        hit({ attachment_count: 2, available_attachment_count: 1 }),
        hit({
          attachment_count: 1,
          available_attachment_count: 0,
        }),
        hit({ attachment_count: 2, available_attachment_count: 2 }),
        hit({ has_conflicting_revision: true }),
      ],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    const badges = el.querySelectorAll(".badge.attachments");
    // The visible words carry their units ("files kept"); the exact
    // counts and the references-vs-preserved split stay in title/aria.
    expect(badges[0].textContent).toBe("1 of 2 files kept");
    expect(badges[0].getAttribute("aria-label")).toBe("attachments: 1 of 2");
    expect(badges[0].innerHTML).toContain("#i-paperclip");
    expect(badges[1].textContent).toBe("0 of 1 files kept");
    expect(badges[1].getAttribute("aria-label")).toBe("attachments: 0 of 1");
    expect(badges[2].textContent).toBe("2 files");
    expect(el.querySelector(".badge.conflict")?.textContent).toBe("revisions");
  });

  it("Signal media badges read the same counts as email attachments", () => {
    // The badge is source-agnostic: a Signal row whose raw_json media
    // pass stored one of two attachments, one whose array the pass did
    // not account for (one honest unavailable row), and one fully
    // preserved render exactly the email/WhatsApp badges (M4.3 parity).
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [
        hit({
          source_label: "signal",
          attachment_count: 2,
          available_attachment_count: 1,
        }),
        hit({
          source_label: "signal",
          attachment_count: 1,
          available_attachment_count: 0,
        }),
        hit({
          source_label: "signal",
          attachment_count: 3,
          available_attachment_count: 3,
        }),
      ],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    const badges = [...el.querySelectorAll(".badge.attachments")] as HTMLElement[];
    expect(badges.length).toBe(3);
    expect(badges[0].textContent).toBe("1 of 2 files kept");
    expect(badges[0].getAttribute("aria-label")).toBe("attachments: 1 of 2");
    expect(badges[0].title).toBe("2 attachment references, 1 preserved");
    expect(badges[1].textContent).toBe("0 of 1 files kept");
    expect(badges[2].textContent).toBe("3 files");
    expect(badges[2].title).toBe("3 attachments");
  });

  it("unknown time never renders a fabricated timestamp", () => {
    expect(timeLabel(hit({ sent_at: null, time_confidence: "missing" }))).toBe("unknown time");
    expect(timeLabel(hit({ time_confidence: "legacy_assumed" }))).toContain("(assumed)");
    expect(timeLabel(hit({ time_confidence: "explicit_offset" }))).not.toContain("(");
  });

  it("a marked-uncertain time explains itself on the row, not just in the detail pane", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [
        hit(),
        hit({ time_confidence: "explicit_offset" }),
        hit({ time_confidence: "unresolved" }),
      ],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    const times = [...el.querySelectorAll(".hit-main .meta span.time")] as HTMLElement[];
    expect(times.length).toBe(3);
    // The meta line still reads the same (the span changes no text).
    expect(times[0].textContent).toContain("(assumed)");
    // Assumed: the hover title is the detail pane's own explanation, so
    // both surfaces say the same thing.
    expect(times[0].title).toBe(
      "The time was assumed during import and may be off — the source did not state it directly.",
    );
    expect(times[2].title).toBe(
      "The message's time could not be read; no instant was fabricated.",
    );
    // Exact times carry no marker, so they carry no tooltip noise either.
    expect(times[1].title).toBe("");
  });

  it("row selection checkboxes reflect and drive the selection", () => {
    const el = document.createElement("div");
    const toggled: number[] = [];
    renderFlatResults(el, {
      hits: [hit({ id: 5 }), hit({ id: 6 })],
      activeIndex: -1,
      selectedId: null,
      selection: new Set([6]),
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: (id) => toggled.push(id),
      onToggleFold: () => {},
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
      expandedFolds: new Set(),
      terms: [],
      onOpen: (id) => opened.push(id),
      onToggleSelect: () => {},
      onToggleFold: () => {},
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
      expandedFolds: new Set(),
      terms: [],
      onOpen: (id) => opened.push(id),
      onToggleSelect: () => {},
      onToggleFold: () => {},
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

  it("a folded hit renders a collapsed 'N similar copies' expander as a sibling control", () => {
    const el = document.createElement("div");
    const toggled: number[] = [];
    const opened: number[] = [];
    renderFlatResults(el, {
      hits: [hit({ id: 4, cluster_count: 2, member_ids: [11, 12] })],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      onOpen: (id) => opened.push(id),
      onToggleSelect: () => {},
      onToggleFold: (id) => toggled.push(id),
    });
    const li = el.querySelector("li.hit-item") as HTMLElement;
    const main = li.querySelector(".hit-main") as HTMLElement;
    const toggle = li.querySelector("button.fold-toggle") as HTMLButtonElement;
    expect(toggle).toBeTruthy();
    // Collapsed by default, and the count label reads as words.
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(toggle.textContent).toBe("2 similar copies");
    expect(toggle.getAttribute("aria-controls")).toBe("fold-members-4");
    // A sibling of the row's main area, never nested inside its
    // role=button (same a11y rule as the selection checkbox).
    expect(main.contains(toggle)).toBe(false);
    expect(li.querySelector("ul.fold-members")).toBeNull();
    // Toggling reports the representative and never opens the row.
    toggle.click();
    expect(toggled).toEqual([4]);
    expect(opened).toEqual([]);
  });

  it("the expander label carries its singular and plural forms", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [
        hit({ id: 1, cluster_count: 1, member_ids: [21] }),
        hit({ id: 2, cluster_count: 3, member_ids: [22, 23, 24] }),
      ],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    const labels = [...el.querySelectorAll("button.fold-toggle")].map((b) => b.textContent);
    expect(labels).toEqual(["1 similar copy", "3 similar copies"]);
  });

  it("an expanded fold lists its copies by message id; each opens through the shared path", () => {
    const el = document.createElement("div");
    const opened: number[] = [];
    const toggled: number[] = [];
    renderFlatResults(el, {
      hits: [hit({ id: 4, cluster_count: 2, member_ids: [11, 12] })],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set([4]),
      terms: [],
      onOpen: (id) => opened.push(id),
      onToggleSelect: () => {},
      onToggleFold: (id) => toggled.push(id),
    });
    const toggle = el.querySelector("button.fold-toggle") as HTMLButtonElement;
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const members = [...el.querySelectorAll("ul.fold-members li.fold-member button")];
    expect(members.map((b) => b.textContent)).toEqual(["Open copy 11", "Open copy 12"]);
    // Opening a copy opens the copy's id, not the representative's, and
    // does not double-fire the row's own open.
    (members[1] as HTMLElement).click();
    expect(opened).toEqual([12]);
    // The expander itself still toggles the representative.
    toggle.click();
    expect(toggled).toEqual([4]);
    expect(opened).toEqual([12]);
  });

  it("count-0 and absent-count hits render exactly the row of today", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [hit({ id: 1, cluster_count: 0, member_ids: [] }), hit({ id: 2 })],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set([1, 2]),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    // No expander, no member list — even for rows sitting in the
    // expanded set (nothing to expand).
    expect(el.querySelector(".fold-toggle")).toBeNull();
    expect(el.querySelector(".fold-members")).toBeNull();
    for (const li of [...el.querySelectorAll("li.hit-item")] as HTMLElement[]) {
      expect(li.children.length).toBe(2);
      expect(li.children[0].className).toBe("row-select");
      expect(li.children[1].className).toBe("hit-main mv-press");
    }
  });

  it("applies the density mode as data-density on the list container", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [hit()],
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      density: "compact",
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    expect(el.dataset.density).toBe("compact");
  });

  it("defaults to comfortable and an ARIA grid with selected rows", () => {
    const el = document.createElement("div");
    renderFlatResults(el, {
      hits: [hit({ id: 1 }), hit({ id: 2 })],
      activeIndex: 1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    expect(el.dataset.density).toBe("comfortable");
    expect(el.getAttribute("role")).toBe("grid");
    const rows = [...el.querySelectorAll("li.hit-item")] as HTMLElement[];
    expect(rows.map((r) => r.getAttribute("aria-selected"))).toEqual(["false", "true"]);
  });

  it("never renders more DOM rows than the cap", () => {
    const el = document.createElement("div");
    const many = Array.from({ length: 250 }, (_, i) => hit({ id: i + 1 }));
    renderFlatResults(el, {
      hits: many,
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    expect(el.querySelectorAll("li.hit-item").length).toBe(200);
    // The cap is the default; an explicit higher cap renders more.
    renderFlatResults(el, {
      hits: many,
      activeIndex: -1,
      selectedId: null,
      selection: new Set(),
      expandedFolds: new Set(),
      terms: [],
      renderCap: 220,
      onOpen: () => {},
      onToggleSelect: () => {},
      onToggleFold: () => {},
    });
    expect(el.querySelectorAll("li.hit-item").length).toBe(220);
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

  /** eraBuckets caps at 8 — the longest strip the app derives. */
  const eightBuckets = Array.from({ length: 8 }, (_, i) => {
    const year = 2019 + i;
    return { label: String(year), from: `${year}-01-01`, to: `${year}-12-31` };
  });

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

  it("carries the quiet visible 'Time' label", () => {
    const el = document.createElement("div");
    renderEraChips(el, { buckets, activeIndex: -1, onToggle: () => {} });
    const label = el.querySelector(".era-chip-label") as HTMLElement;
    expect(label.textContent).toBe("Time");
    // A label, not a control.
    expect(el.querySelectorAll("button").length).toBe(2);
  });

  it("caps the strip at the 5 most recent eras; Older… reveals the rest", () => {
    const el = document.createElement("div");
    const toggled: number[] = [];
    renderEraChips(el, { buckets: eightBuckets, activeIndex: -1, onToggle: (i) => toggled.push(i) });
    const chips = () => [...el.querySelectorAll("button.era-chip")] as HTMLElement[];
    // Collapsed: the five most recent years, oldest of them first.
    expect(chips().map((c) => c.textContent)).toEqual(["2022", "2023", "2024", "2025", "2026"]);
    const older = el.querySelector("button.era-more") as HTMLElement;
    expect(older.textContent).toBe("Older…");
    expect(older.getAttribute("aria-expanded")).toBe("false");
    expect(older.title).toContain("3 older time periods");

    // Clicking a visible chip reports its FULL bucket index (2022 is
    // bucket 3 of 8), never its position among the visible ones.
    chips()[0].click();
    expect(toggled).toEqual([3]);

    // Older… expands; Fewer collapses; the state rides the strip element.
    older.click();
    expect(chips().map((c) => c.textContent)).toEqual(
      eightBuckets.map((b) => b.label),
    );
    const fewer = el.querySelector("button.era-more") as HTMLElement;
    expect(fewer.textContent).toBe("Fewer");
    expect(fewer.getAttribute("aria-expanded")).toBe("true");
    fewer.click();
    expect(chips()).toHaveLength(5);
    expect((el.querySelector("button.era-more") as HTMLElement).textContent).toBe("Older…");

    // A re-render (searches re-render constantly) keeps the collapsed
    // state — and by default it is collapsed until asked.
    renderEraChips(el, { buckets: eightBuckets, activeIndex: -1, onToggle: () => {} });
    expect(chips()).toHaveLength(5);
  });

  it("never hides the active era behind the cap", () => {
    const el = document.createElement("div");
    // 2019 (index 0) is applied but older than the cap: it stays visible,
    // ahead of the five most recent.
    renderEraChips(el, { buckets: eightBuckets, activeIndex: 0, onToggle: () => {} });
    const chips = [...el.querySelectorAll("button.era-chip")] as HTMLElement[];
    expect(chips.map((c) => c.textContent)).toEqual([
      "2019",
      "2022",
      "2023",
      "2024",
      "2025",
      "2026",
    ]);
    expect(chips[0].classList.contains("on")).toBe(true);
    expect(chips[0].getAttribute("aria-pressed")).toBe("true");
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

  it("shrinking to the cap resets expansion; a regrow starts collapsed", () => {
    const el = document.createElement("div");
    renderEraChips(el, { buckets: eightBuckets, activeIndex: -1, onToggle: () => {} });
    (el.querySelector("button.era-more") as HTMLElement).click(); // expand
    expect(el.querySelectorAll("button.era-chip").length).toBe(8);

    // The timeline shrinks to the cap (a re-import with tighter bounds):
    // nothing to hide, and the persisted expanded state must not linger.
    const fiveBuckets = eightBuckets.slice(3);
    renderEraChips(el, { buckets: fiveBuckets, activeIndex: -1, onToggle: () => {} });
    expect(el.querySelectorAll("button.era-chip").length).toBe(5);
    expect(el.querySelector("button.era-more")).toBeNull();

    // Regrow past the cap: the strip starts collapsed again — five chips
    // and an Older… toggle, not the eight the stale flag would show.
    renderEraChips(el, { buckets: eightBuckets, activeIndex: -1, onToggle: () => {} });
    expect(el.querySelectorAll("button.era-chip").length).toBe(5);
    const older = el.querySelector("button.era-more") as HTMLElement;
    expect(older.textContent).toBe("Older…");
    expect(older.getAttribute("aria-expanded")).toBe("false");
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
    // The raw `mbox` label reads as "Mail import"; the stored label
    // stays one hover away on the source row.
    expect(table).toContain("Mail import");
    const sourceCell = [...card.querySelectorAll(".meta-table dd")].find((dd) =>
      dd.textContent === "Mail import",
    ) as HTMLElement;
    expect(sourceCell.title).toBe("Stored source: mbox");
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

  it("an addressed author headlines as the display name, full form one hover away", () => {
    // The rows show the display name (wave-1 discipline); the pane's
    // headline follows it instead of the raw "Name <address>" form.
    const raw = ctx.anchor.author;
    ctx.anchor.author = "Ana Sender <ana@example.com>";
    try {
      const el = render();
      const h2 = el.querySelector(".detail-card h2") as HTMLElement;
      expect(h2.textContent).toBe("Ana Sender");
      expect(h2.title).toBe("Ana Sender <ana@example.com>");
      // The exact address stays in the meta table.
      expect(el.querySelector(".detail-card .meta-table")?.textContent).toContain(
        "ana@example.com",
      );
    } finally {
      ctx.anchor.author = raw;
    }
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

  it("conversation-context neighbor rows mark the query terms", () => {
    const neighborCtx: ConversationContext = {
      ...ctx,
      before: [{ ...ctx.before[0], body: "earlier vault message" }],
      after: [{ ...ctx.after[0], body: "later vault message" }],
    };
    const el = render({ ctx: neighborCtx, entries: contextEntries(neighborCtx) });
    const beforeRow = el.querySelector('li.thread-row[data-id="1"]') as HTMLElement;
    const afterRow = el.querySelector('li.thread-row[data-id="4"]') as HTMLElement;
    expect(beforeRow.querySelector("mark.hit")?.textContent).toBe("vault");
    expect(afterRow.querySelectorAll("mark.hit").length).toBe(1);
    // Marks never change the rendered text.
    expect(beforeRow.querySelector(".body")?.textContent).toBe("earlier vault message");
  });

  it("browse mode (no query text) renders no marks anywhere in the pane", () => {
    const neighborCtx: ConversationContext = {
      ...ctx,
      before: [{ ...ctx.before[0], body: "earlier vault message" }],
      after: [{ ...ctx.after[0], body: "later vault message" }],
    };
    const el = render({
      ctx: neighborCtx,
      entries: contextEntries(neighborCtx),
      queryText: "",
    });
    expect(el.querySelectorAll("mark.hit").length).toBe(0);
  });

  it("the anchor body marks at most the shared cap, text unchanged", () => {
    const body = `${"see vault ".repeat(HIGHLIGHT_MAX_MARKS + 5)}end`;
    const cappedCtx: ConversationContext = {
      ...ctx,
      anchor: { ...ctx.anchor, body },
    };
    const el = render({ ctx: cappedCtx, entries: contextEntries(cappedCtx) });
    const bodyEl = el.querySelector(".detail-body .body") as HTMLElement;
    expect(bodyEl.querySelectorAll("mark.hit").length).toBe(HIGHLIGHT_MAX_MARKS);
    expect(bodyEl.textContent).toBe(body);
  });

  it("never executes message content on the raw body path", () => {
    const evil: ConversationContext = {
      ...ctx,
      before: [],
      after: [],
      anchor: {
        ...ctx.anchor,
        author: '<b>"evil"</b>',
        body: '<img src=x onerror="alert(1)">',
        attachments: [],
        raw_json: "{}",
      },
    };
    const el = render({ ctx: evil, entries: contextEntries(evil), queryText: "" });
    expect(el.querySelectorAll("img,b,script").length).toBe(0);
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

  const htmlCtx: ConversationContext = {
    ...ctx,
    anchor: {
      ...ctx.anchor,
      raw_json: JSON.stringify({ html: "<p>telo pisma</p>" }),
    },
  };

  it("email HTML renders inside a sandboxed iframe behind a Rendered/Raw toggle", () => {
    const el = render({ ctx: htmlCtx, entries: contextEntries(htmlCtx) });
    const buttons = [...el.querySelectorAll("button.mode-btn")] as HTMLButtonElement[];
    expect(buttons.map((b) => b.textContent)).toEqual(["Rendered", "Raw"]);
    // Default mode is Rendered: sandboxed iframe, CSP meta, the HTML
    // itself only ever inside the srcdoc attribute.
    const frame = el.querySelector("iframe.rendered-frame") as HTMLIFrameElement;
    expect(frame).not.toBeNull();
    expect(frame.getAttribute("sandbox")).toBe("");
    expect(frame.getAttribute("srcdoc")).toContain("default-src 'none'");
    // Links are retargeted to new windows the sandbox never opens, so
    // clicks cannot navigate the frame itself.
    expect(frame.getAttribute("srcdoc")).toContain('<base target="_blank">');
    expect(frame.getAttribute("srcdoc")).toContain("<p>telo pisma</p>");
    expect(el.querySelector(".detail-card .body")?.innerHTML).not.toContain(
      "<p>telo pisma</p>",
    );
    expect(buttons[0].getAttribute("aria-pressed")).toBe("true");
    // Raw restores today's highlighted plain-text body and drops the frame.
    buttons[1].click();
    expect(el.querySelector("iframe.rendered-frame")).toBeNull();
    expect(buttons[0].getAttribute("aria-pressed")).toBe("false");
    expect(buttons[1].getAttribute("aria-pressed")).toBe("true");
    expect(el.querySelector(".detail-card .body")?.textContent).toContain("arrives");
    // Rendered comes back.
    buttons[0].click();
    expect(el.querySelector("iframe.rendered-frame")).not.toBeNull();
  });

  it("no stored HTML: no toggle, plain body stays", () => {
    const el = render(); // fixture raw_json is {"keep": 1}
    expect(el.querySelector("button.mode-btn")).toBeNull();
    expect(el.querySelector("iframe.rendered-frame")).toBeNull();
    expect(el.querySelector(".detail-card .body")?.textContent).toContain("arrives");
  });
});

describe("results footer (count line, density, sentinel)", () => {
  it("renders the accumulated count line and the density toggle", () => {
    const el = document.createElement("div");
    renderPagination(el, {
      loaded: 60,
      total: 120,
      density: "comfortable",
      renderCap: 200,
      onLoadMore: () => {},
      onToggleDensity: () => {},
    });
    expect(el.querySelector(".page-info")?.textContent).toBe("1–60 of 120");
    const toggle = el.querySelector(".density-toggle") as HTMLButtonElement;
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
    expect(el.querySelector(".load-sentinel")).toBeTruthy();
    expect(el.querySelector(".load-more-btn")).toBeNull();
  });

  it("empty results render the count line and toggle only", () => {
    const el = document.createElement("div");
    renderPagination(el, {
      loaded: 0,
      total: 0,
      density: "compact",
      renderCap: 200,
      onLoadMore: () => {},
      onToggleDensity: () => {},
    });
    expect(el.textContent).toContain("0 results");
    expect(
      (el.querySelector(".density-toggle") as HTMLButtonElement).getAttribute("aria-pressed"),
    ).toBe("true");
    expect(el.querySelector(".load-sentinel")).toBeNull();
    expect(el.querySelector(".load-more-btn")).toBeNull();
  });

  it("beyond the render cap the sentinel becomes an explicit Load more", () => {
    const el = document.createElement("div");
    renderPagination(el, {
      loaded: 200,
      total: 320,
      density: "comfortable",
      renderCap: 200,
      onLoadMore: () => {},
      onToggleDensity: () => {},
    });
    expect(el.querySelector(".load-sentinel")).toBeNull();
    expect((el.querySelector(".load-more-btn") as HTMLButtonElement).textContent).toBe("Load more");
  });

  it("with everything loaded there is no growth control", () => {
    const el = document.createElement("div");
    renderPagination(el, {
      loaded: 120,
      total: 120,
      density: "comfortable",
      renderCap: 200,
      onLoadMore: () => {},
      onToggleDensity: () => {},
    });
    expect(el.querySelector(".page-info")?.textContent).toBe("1–120 of 120");
    expect(el.querySelector(".load-sentinel")).toBeNull();
    expect(el.querySelector(".load-more-btn")).toBeNull();
  });

  it("fires the load-more and density handlers", () => {
    const fired: string[] = [];
    const el = document.createElement("div");
    renderPagination(el, {
      loaded: 200,
      total: 320,
      density: "compact",
      renderCap: 200,
      onLoadMore: () => fired.push("more"),
      onToggleDensity: () => fired.push("density"),
    });
    (el.querySelector(".load-more-btn") as HTMLButtonElement).click();
    (el.querySelector(".density-toggle") as HTMLButtonElement).click();
    expect(fired).toEqual(["more", "density"]);
  });
});

describe("source chips", () => {
  it("renders toggles with counts and pressed state", () => {
    const rows: SourceChipRow[] = [
      { id: "inst-mbox", label: "mbox", count: 900, icon: "file", status: "" },
      { id: "inst-whatsapp", label: "whatsapp", count: 80, icon: "chat", status: "" },
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
        { id: "inst-imap", label: "imap.gmail.com-ana", count: 12, icon: "mail", status: "" },
        { id: "inst-whatsapp", label: "whatsapp", count: 80, icon: "chat", status: "" },
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
      rows: [{ id: "inst-mbox", label: "mbox", count: 1, icon: "file", status: "" }],
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
        { id: "inst-mbox", label: "mbox", count: 900, icon: "file", status: "" },
        { id: "inst-telegram", label: "telegram", count: 80, icon: "chat", status: "" },
      ],
      selected: [],
      onToggle: () => {},
    });
    expect(el.innerHTML).toMatchSnapshot();
    expect(el.querySelectorAll(".chip.on").length).toBe(0);
  });

  it("appends the per-source status after the label, leaving the label-and-count text intact", () => {
    // The e2e specs assert each chip's label and count by substring;
    // the status adds to the chip's accessible name as its own span
    // and never rewrites the label text it follows.
    const el = document.createElement("div");
    renderSourceChips(el, {
      rows: [
        { id: "inst-imap", label: "imap.gmail.com-ana", count: 12, icon: "mail", status: "Syncing…" },
        { id: "inst-mbox", label: "mbox", count: 900, icon: "file", status: "" },
      ],
      selected: ["inst-imap"],
      onToggle: () => {},
    });
    const chips = [...el.querySelectorAll(".chip")];
    expect(chips[0].querySelector(".chip-status")?.textContent).toBe("Syncing…");
    expect(chips[0].querySelector("span:not(.chip-status)")?.textContent).toBe(
      "imap.gmail.com-ana (12)",
    );
    expect(chips[0].textContent).toBe("imap.gmail.com-ana (12)Syncing…");
    // A row without a status renders no span — the chip is unchanged.
    expect(chips[1].querySelector(".chip-status")).toBeNull();
    expect(chips[1].textContent).toBe("mbox (900)");
    // The pressed state and the decorative icon are untouched by status.
    expect(chips[0].getAttribute("aria-pressed")).toBe("true");
    expect(chips[0].querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
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
    // Plain language: the folder table reads "Busiest folders", and the
    // raw `mbox` label reads "Mail import" (the stored label stays one
    // hover away in the row's tooltip).
    expect([...el.querySelectorAll(".stat-table h3")].map((h) => h.textContent)).toEqual([
      "By source",
      "Busiest folders",
      "Top authors",
    ]);
    const sourceRows = [...el.querySelectorAll(".stat-table")[0].querySelectorAll("td.label")];
    expect(sourceRows.map((td) => td.textContent)).toEqual(["Mail import", "whatsapp"]);
    expect(sourceRows[0].getAttribute("title")).toBe("mbox");
    const folderRows = [...el.querySelectorAll(".stat-table")[1].querySelectorAll("td.label")];
    expect(folderRows.map((td) => td.textContent)).toEqual([
      "whatsapp · family",
      "Mail import · Thread-a",
    ]);
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
      "Mail import · All mail including spam and trash",
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

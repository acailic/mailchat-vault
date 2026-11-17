// @vitest-environment happy-dom
// The signature surface's spec: every visible match is marked in the
// results rows. Operator-derived terms (the from:/to: author value the
// query parser applied) and free terms mark the subject-fallback rows
// and the author meta run; snippet rows keep their backend marks; the
// combined text content never changes — only <mark class="hit"> runs
// differ. Follows render2.test.ts's builder-driven pattern.
import { describe, expect, it } from "vitest";
import { renderFlatResults } from "./render2";
import { applyQuery } from "./queryparser";
import { DEFAULT_FILTERS } from "./searchstate";
import { highlightTerms } from "./viewmodel";
import type { MessageSummary } from "./v2types";

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

function renderRows(hits: MessageSummary[], terms: string[]): HTMLElement {
  const el = document.createElement("div");
  renderFlatResults(el, {
    hits,
    activeIndex: -1,
    selectedId: null,
    selection: new Set(),
    expandedFolds: new Set(),
    terms,
    onOpen: () => {},
    onToggleSelect: () => {},
    onToggleFold: () => {},
  });
  return el;
}

function markTexts(el: HTMLElement): string[] {
  return [...el.querySelectorAll("mark.hit")].map((m) => m.textContent);
}

describe("results highlight (signature surface)", () => {
  it("marks free terms in the subject fallback", () => {
    const el = renderRows([hit({ subject: "quarterly delivery date" })], ["delivery"]);
    expect(markTexts(el)).toEqual(["delivery"]);
  });

  it("marks multiple whole-word occurrences, bounded", () => {
    const el = renderRows(
      [hit({ subject: "vault report on the vault export" })],
      ["vault"],
    );
    expect(markTexts(el)).toEqual(["vault", "vault"]);
  });

  it("operator-derived terms: from:/to:'s author value marks the meta run", () => {
    const applied = applyQuery(DEFAULT_FILTERS, "from:ana delivery", []);
    expect(applied.filters.author).toBe("ana");
    const el = renderRows(
      [hit({ author: "ana", subject: "delivery date" })],
      highlightTerms(applied.filters),
    );
    const marks = markTexts(el);
    expect(marks).toContain("ana");
    expect(marks).toContain("delivery");
    // The author's mark sits in the meta line, the term's in the body.
    expect(el.querySelector(".meta mark.hit")?.textContent).toBe("ana");
    expect(el.querySelector(".body mark.hit")?.textContent).toBe("delivery");
  });

  it("a quoted to: value marks the display-name meta run", () => {
    const applied = applyQuery(DEFAULT_FILTERS, 'to:"Ana Garcia"', []);
    const el = renderRows(
      [hit({ author: "Ana Garcia <ana@x.y>", subject: "hello" })],
      highlightTerms(applied.filters),
    );
    expect(el.querySelector(".meta mark.hit")?.textContent).toBe("Ana Garcia");
    // Full address stays one hover away, unmarked text intact.
    expect(el.querySelector(".meta")?.getAttribute("title")).toBe(
      "Ana Garcia <ana@x.y>",
    );
  });

  it("snippet rows keep their backend marks and add none client-side", () => {
    const el = renderRows(
      [hit({ snippet: "the \x01vault\x02 arrives", subject: "vault report" })],
      [],
    );
    expect(markTexts(el)).toEqual(["vault"]);
  });

  it("snippet rows keep their backend marks even with terms applied", () => {
    const el = renderRows(
      [hit({ snippet: "the \x01vault\x02 arrives", subject: "vault report" })],
      ["arrives"],
    );
    // The snippet's own marker runs are the rendered marks.
    expect(markTexts(el)).toEqual(["vault"]);
  });

  it("no-snippet, no-subject rows render an empty body without marks", () => {
    const el = renderRows([hit({ subject: null, snippet: "" })], ["vault"]);
    expect(markTexts(el)).toEqual([]);
    expect(el.querySelector(".body")?.textContent).toBe("");
  });

  it("marking never changes the combined text content", () => {
    const subject = "delivery date for the vault";
    const withTerms = renderRows([hit({ subject })], ["vault", "delivery"]);
    const without = renderRows([hit({ subject })], []);
    expect(withTerms.querySelector(".body")?.textContent).toBe(
      without.querySelector(".body")?.textContent,
    );
    expect(withTerms.querySelector(".meta")?.textContent).toBe(
      without.querySelector(".meta")?.textContent,
    );
  });

  it("the author meta run stays plain without an author term", () => {
    const el = renderRows([hit({ author: "ana", subject: "vault" })], ["vault"]);
    expect(el.querySelector(".meta mark.hit")).toBeNull();
    expect(el.querySelector(".body mark.hit")?.textContent).toBe("vault");
  });
});

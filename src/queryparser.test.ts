// Unit tests for the search-box operator parser (P1·05): tokenizing,
// every supported operator's filter mapping, honest hints for every
// unsupported / unknown / malformed token, source-label resolution,
// and request equivalence with the equivalent UI filters.

import { describe, expect, it } from "vitest";
import { applyQuery, HINTS, type SourceLabel } from "./queryparser";
import { buildRequest, DEFAULT_FILTERS, type SearchFilters } from "./searchstate";

function filters(over: Partial<SearchFilters>): SearchFilters {
  return { ...DEFAULT_FILTERS, ...over };
}

const SOURCES: SourceLabel[] = [
  { id: "inst-mbox", label: "mbox" },
  { id: "inst-whatsapp", label: "whatsapp" },
  { id: "inst-telegram", label: "telegram" },
];

/** Parse `text` over default filters with the standard source list. */
function parse(text: string, sources: SourceLabel[] = SOURCES) {
  return applyQuery(DEFAULT_FILTERS, text, sources);
}

describe("tokenizing", () => {
  it("plain terms become the FTS text, whitespace-normalized", () => {
    const { filters: f, hints } = parse("vault   report");
    expect(f.text).toBe("vault report");
    expect(hints).toEqual([]);
  });

  it("quoted phrases keep their quotes as FTS5 phrase syntax", () => {
    expect(parse('"vault report"').filters.text).toBe('"vault report"');
  });

  it("digit-led fields are not operators (times stay plain terms)", () => {
    const { filters: f, hints } = parse("12:30 meeting");
    expect(f.text).toBe("12:30 meeting");
    expect(hints).toEqual([]);
  });

  it("field names are case-insensitive", () => {
    expect(parse("FROM:ana").filters.author).toBe("ana");
  });
});

describe("author operators (from:/to:)", () => {
  it("from: sets the author substring filter", () => {
    const { filters: f, hints } = parse("from:ana");
    expect(f.author).toBe("ana");
    expect(f.text).toBe("");
    expect(hints).toEqual([]);
  });

  it("to: maps onto the same author filter", () => {
    expect(parse("to:ana").filters.author).toBe("ana");
  });

  it("quoted values with spaces parse", () => {
    expect(parse('from:"ana garcia"').filters.author).toBe("ana garcia");
  });

  it("Unicode addresses parse as values", () => {
    expect(parse("from:ana.çelik@österreich.example").filters.author).toBe(
      "ana.çelik@österreich.example",
    );
  });

  it("from: and to: with the same value are one idempotent filter", () => {
    const { filters: f, hints } = parse("from:ana to:ana");
    expect(f.author).toBe("ana");
    expect(hints).toEqual([]);
  });

  it("from: and to: with different values keep the first and hint", () => {
    const { filters: f, hints } = parse("from:ana to:bruno");
    expect(f.author).toBe("ana");
    expect(hints).toEqual([HINTS.oneAuthor("ana")]);
  });

  it("an empty value hints and leaves the author untouched", () => {
    const { filters: f, hints } = parse("from:");
    expect(f.author).toBe("");
    expect(hints).toEqual([HINTS.missingValue("from:")]);
  });

  it("the query author overrides the UI author; other UI filters survive", () => {
    const ui = filters({ author: "bruno", timeAssumed: true, sort: "newest" });
    const { filters: f } = applyQuery(ui, "from:ana", SOURCES);
    expect(f.author).toBe("ana");
    expect(f.timeAssumed).toBe(true);
    expect(f.sort).toBe("newest");
  });

  it("with no author operator the UI author survives", () => {
    const { filters: f } = applyQuery(filters({ author: "bruno" }), "vault", SOURCES);
    expect(f.author).toBe("bruno");
  });
});

describe("date operators (before:/after:)", () => {
  it("after: is inclusive — from_date becomes the given day", () => {
    const { filters: f, hints } = parse("after:2024-01-01");
    expect(f.fromDate).toBe("2024-01-01");
    expect(f.toDate).toBe("");
    expect(hints).toEqual([]);
  });

  it("before: is exclusive — to_date becomes the day before", () => {
    expect(parse("before:2024-01-02").filters.toDate).toBe("2024-01-01");
  });

  it("before: crosses month and year boundaries via UTC arithmetic", () => {
    expect(parse("before:2024-03-01").filters.toDate).toBe("2024-02-29"); // leap year
    expect(parse("before:2024-01-01").filters.toDate).toBe("2023-12-31");
  });

  it("malformed dates hint and never reach the filters", () => {
    for (const bad of ["2024-2-1", "2024-02-30", "2024-13-01", "not-a-date", "24-01-01"]) {
      const { filters: f, hints } = parse(`before:${bad}`);
      expect(f.toDate).toBe("");
      expect(hints).toEqual([HINTS.invalidDate("before:", bad)]);
    }
  });

  it("an empty date value hints as missing", () => {
    expect(parse("after:").hints).toEqual([HINTS.missingValue("after:")]);
  });

  it("a repeated bound with a different value keeps the first and hints", () => {
    const { filters: f, hints } = parse("after:2024-01-01 after:2024-06-01");
    expect(f.fromDate).toBe("2024-01-01");
    expect(hints).toEqual([HINTS.oneDate("after:", "2024-01-01")]);
  });

  it("a repeated bound with the same value is idempotent", () => {
    expect(parse("after:2024-01-01 after:2024-01-01").hints).toEqual([]);
  });

  it("reversed query bounds keep the first-typed bound, drop the other, and hint", () => {
    const { filters: f, hints } = parse("after:2024-06-01 before:2024-01-01");
    expect(f.fromDate).toBe("2024-06-01");
    expect(f.toDate).toBe("");
    expect(hints).toEqual([HINTS.reversedDates("after:2024-06-01", "before:2024-01-01")]);
  });

  it("reversed bounds typed the other way keep the before bound", () => {
    const { filters: f, hints } = parse("before:2024-01-01 after:2024-06-01");
    expect(f.toDate).toBe("2023-12-31");
    expect(f.fromDate).toBe("");
    expect(hints).toEqual([HINTS.reversedDates("before:2024-01-01", "after:2024-06-01")]);
  });

  it("a valid after+before window sets both bounds", () => {
    const { filters: f, hints } = parse("after:2024-01-01 before:2024-02-01");
    expect(f.fromDate).toBe("2024-01-01");
    expect(f.toDate).toBe("2024-01-31");
    expect(hints).toEqual([]);
  });

  it("the query overrides only the bounds it sets; the other UI bound survives", () => {
    const ui = filters({ fromDate: "2020-01-01", toDate: "2030-01-01" });
    expect(applyQuery(ui, "after:2024-01-01", SOURCES).filters).toMatchObject({
      fromDate: "2024-01-01",
      toDate: "2030-01-01",
    });
    expect(applyQuery(ui, "before:2024-01-01", SOURCES).filters).toMatchObject({
      fromDate: "2020-01-01",
      toDate: "2023-12-31",
    });
  });
});

describe("source operator", () => {
  it("an exact label resolves to its instance id", () => {
    const { filters: f, hints } = parse("source:whatsapp");
    expect(f.sourceIds).toEqual(["inst-whatsapp"]);
    expect(hints).toEqual([]);
  });

  it("exact matching is case-insensitive", () => {
    expect(parse("source:WhatsApp").filters.sourceIds).toEqual(["inst-whatsapp"]);
  });

  it("an exact match wins over a would-be prefix match", () => {
    const sources = [
      { id: "inst-wa", label: "wa" },
      { id: "inst-washington", label: "washington" },
    ];
    expect(parse("source:wa", sources).filters.sourceIds).toEqual(["inst-wa"]);
  });

  it("a unique label prefix resolves", () => {
    expect(parse("source:tele").filters.sourceIds).toEqual(["inst-telegram"]);
  });

  it("an ambiguous prefix never guesses — it hints with the matches", () => {
    const sources = [
      { id: "inst-1", label: "whatsapp" },
      { id: "inst-2", label: "whatsapp-archive" },
    ];
    const { filters: f, hints } = parse("source:whats", sources);
    expect(f.sourceIds).toEqual([]);
    expect(hints).toEqual([HINTS.ambiguousSource("whats", ["whatsapp (inst-1)", "whatsapp-archive (inst-2)"])]);
  });

  it("duplicate labels are ambiguous even on an exact match", () => {
    const sources = [
      { id: "inst-1", label: "whatsapp" },
      { id: "inst-2", label: "whatsapp" },
    ];
    const { filters: f, hints } = parse("source:whatsapp", sources);
    expect(f.sourceIds).toEqual([]);
    expect(hints).toEqual([HINTS.ambiguousSource("whatsapp", ["whatsapp (inst-1)", "whatsapp (inst-2)"])]);
  });

  it("an unknown label hints with the available labels", () => {
    const { filters: f, hints } = parse("source:signal");
    expect(f.sourceIds).toEqual([]);
    expect(hints).toEqual([HINTS.unknownSource("signal", ["mbox", "whatsapp", "telegram"])]);
  });

  it("multiple source: tokens union (deduplicated)", () => {
    const { filters: f, hints } = parse("source:mbox source:telegram source:mbox");
    expect(f.sourceIds).toEqual(["inst-mbox", "inst-telegram"]);
    expect(hints).toEqual([]);
  });

  it("resolved sources replace the UI selection; a failed resolution leaves it alone", () => {
    const ui = filters({ sourceIds: ["inst-mbox"] });
    expect(applyQuery(ui, "source:telegram", SOURCES).filters.sourceIds).toEqual([
      "inst-telegram",
    ]);
    expect(applyQuery(ui, "source:signal", SOURCES).filters.sourceIds).toEqual(["inst-mbox"]);
  });

  it("an empty value hints as missing", () => {
    expect(parse("source:").hints).toEqual([HINTS.missingValue("source:")]);
  });
});

describe("has: operator", () => {
  it("has:attachment turns the tri-state on", () => {
    const { filters: f, hints } = parse("has:attachment");
    expect(f.hasAttachment).toBe(true);
    expect(hints).toEqual([]);
  });

  it("has:attachment overrides a UI attachment filter", () => {
    const ui = filters({ hasAttachment: false });
    expect(applyQuery(ui, "has:attachment", SOURCES).filters.hasAttachment).toBe(true);
  });

  it("with no has: token the UI tri-state survives", () => {
    expect(applyQuery(filters({ hasAttachment: false }), "vault", SOURCES).filters.hasAttachment).toBe(false);
  });

  it("has:voice and has:original read not-supported-yet", () => {
    for (const op of ["has:voice", "has:original"]) {
      const { filters: f, hints } = parse(op);
      expect(f.hasAttachment).toBeNull();
      expect(hints).toEqual([HINTS.unsupported(op)]);
    }
  });

  it("an unrecognized has: value hints", () => {
    expect(parse("has:starred").hints).toEqual([HINTS.unknownValue("has:", "starred")]);
  });
});

describe("known-but-absent operators", () => {
  it("subject: reads not supported yet", () => {
    const { filters: f, hints } = parse("subject:vault");
    expect(f.text).toBe("");
    expect(hints).toEqual([HINTS.unsupported("subject:")]);
  });

  it("is:unread reads not tracked", () => {
    expect(parse("is:unread").hints).toEqual([HINTS.unreadNotTracked]);
    expect(parse("is:read").hints).toEqual([HINTS.unreadNotTracked]);
  });

  it("an unrecognized is: value hints as unknown", () => {
    expect(parse("is:starred").hints).toEqual([HINTS.unknownValue("is:", "starred")]);
  });

  it("an unknown operator hints as unknown — never a silent ignore", () => {
    const { filters: f, hints } = parse("frobnicate:x");
    expect(f).toEqual(DEFAULT_FILTERS);
    expect(hints).toEqual([HINTS.unknownOperator("frobnicate")]);
  });

  it("scheme://… shapes stay searchable text, not operator attempts", () => {
    const { filters: f, hints } = parse("https://example.com/report");
    expect(f.text).toBe("https://example.com/report");
    expect(hints).toEqual([]);
    // A near-link deep link that fails locator parsing still searches.
    expect(parse("mailchat://message/12x").filters.text).toBe("mailchat://message/12x");
  });
});

describe("hints never stop the rest of the query", () => {
  it("a fully mixed query applies every good token and hints every bad one", () => {
    const { filters: f, hints } = parse(
      'from:"ana garcia" subject:report source:signal has:voice after:2024-13-01 frobnicate:x quarterly before:2024-02-01',
    );
    expect(f.author).toBe("ana garcia");
    expect(f.sourceIds).toEqual([]);
    expect(f.hasAttachment).toBeNull();
    expect(f.fromDate).toBe("");
    expect(f.toDate).toBe("2024-01-31");
    expect(f.text).toBe("quarterly");
    expect(hints).toEqual([
      HINTS.unsupported("subject:"),
      HINTS.unknownSource("signal", ["mbox", "whatsapp", "telegram"]),
      HINTS.unsupported("has:voice"),
      HINTS.invalidDate("after:", "2024-13-01"),
      HINTS.unknownOperator("frobnicate"),
    ]);
  });

  it("an operator-only query browses with empty text", () => {
    const { filters: f } = parse("from:ana has:attachment");
    expect(f.text).toBe("");
    expect(f.author).toBe("ana");
    expect(f.hasAttachment).toBe(true);
  });
});

describe("request equivalence with the UI filters", () => {
  it("from:x has:attachment before:2024-01-01 builds the same request as the UI filters", () => {
    const { filters: f, hints } = parse("from:x has:attachment before:2024-01-01");
    expect(hints).toEqual([]);
    const viaQuery = buildRequest(f, 50, 0);
    const viaUi = buildRequest(
      filters({ author: "x", hasAttachment: true, toDate: "2023-12-31" }),
      50,
      0,
    );
    expect(viaQuery).toEqual(viaUi);
  });

  it("a compound query with sources and both bounds matches its UI equivalent", () => {
    const { filters: f } = parse("source:whatsapp after:2024-01-01 before:2024-03-01 report");
    expect(buildRequest(f, 50, 0)).toEqual(
      buildRequest(
        filters({
          text: "report",
          sourceIds: ["inst-whatsapp"],
          fromDate: "2024-01-01",
          toDate: "2024-02-29",
        }),
        50,
        0,
      ),
    );
  });
});

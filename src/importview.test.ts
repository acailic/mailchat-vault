// @vitest-environment happy-dom
// Unit tests for the import preview card builder (U03) and the inline
// import progress card (wave 3): what renders, what the counts line says
// for exact vs capped scans, when the confirm button disables (space,
// nothing found, busy), and how the progress card follows its job
// (queued/running counts and %, determinate vs indeterminate bar,
// failure message, cancel/dismiss wiring).
import { Window } from "happy-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  buildImportCompletion,
  formatHint,
  formatLabel,
  importActionLabel,
  isImportJobKind,
  renderImportPreview,
  renderImportProgress,
} from "./importview";
import type { ImportPreview, JobRecord } from "./types";

const PREVIEW: ImportPreview = {
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
      snippet: "hello there",
    },
  ],
  media_referenced: 1,
  media_missing: 0,
  warnings: [],
  naive_timestamps: 0,
  assumed_zone: null,
  free_bytes: 1024 * 1024 * 1024,
};

let el: HTMLElement;

function render(preview: ImportPreview, freeBytes: number | null = 1 << 30): void {
  renderImportPreview(el as HTMLElement, preview, {
    path: "/tmp/result.json",
    freeBytes,
    busy: false,
    existingSources: [],
    onImport: () => {},
    onCancel: () => {},
  });
}

beforeEach(() => {
  const win = new Window();
  el = win.document.createElement("div") as unknown as HTMLElement;
});

describe("formatLabel / formatHint", () => {
  it("names every known format and hints where its export comes from", () => {
    expect(formatLabel("mbox")).toBe("Email archive (.mbox)");
    expect(formatLabel("eml")).toBe("Email messages (.eml)");
    expect(formatHint("eml")).toContain("Outlook");
    expect(formatLabel("whatsapp")).toBe("WhatsApp export");
    expect(formatHint("telegram")).toContain("Telegram Desktop");
    expect(formatLabel("messenger")).toBe("Messenger (Facebook) export");
    expect(formatHint("messenger")).toContain("Download profile information");
    expect(formatLabel("jsonl")).toBe("JSON Lines export (.jsonl)");
    expect(formatHint("jsonl")).toContain("JSON Lines");
    expect(formatHint("nope")).toBeNull();
  });
});

describe("importActionLabel", () => {
  it("states the exact count for a complete scan and a lower bound otherwise", () => {
    expect(importActionLabel({ ...PREVIEW, messages_seen: 1 })).toBe("Import 1 message");
    expect(importActionLabel(PREVIEW)).toBe("Import 4 messages");
    expect(importActionLabel({ ...PREVIEW, scan_complete: false, messages_seen: 500 })).toBe(
      "Import the first 500 messages",
    );
  });
});

describe("renderImportPreview", () => {
  it("renders format, path, counts, media line, and enabled confirm", () => {
    render(PREVIEW);
    expect(el.hidden).toBe(false);
    expect(el.querySelector(".preview-title")!.textContent).toBe("Telegram export");
    expect(el.querySelector(".preview-path")!.textContent).toBe("/tmp/result.json");
    expect(el.textContent).toContain("4 messages found");
    expect(el.textContent).toContain("1 messages reference media files");
    const confirm = el.querySelector<HTMLButtonElement>("#preview-import-btn")!;
    expect(confirm.disabled).toBe(false);
    expect(confirm.textContent).toBe("Import 4 messages");
    expect(el.querySelector("#preview-cancel-btn")!.textContent).toBe("Choose another file");
  });

  it("a capped scan reads as a lower bound and carries its warning", () => {
    render({
      ...PREVIEW,
      scan_complete: false,
      messages_seen: 500,
      parse_failures: 2,
      warnings: [
        "the preview scanned the first 500 items — every count is a lower bound, not an exact total",
      ],
    });
    expect(el.textContent).toContain("at least 500 messages");
    expect(el.textContent).toContain("2 entries will be reported as issues");
    expect(el.querySelector(".preview-warning")!.textContent).toContain("lower bound");
  });

  it("missing media is stated, not hidden", () => {
    render({ ...PREVIEW, media_missing: 1 });
    expect(el.textContent).toContain("1 file missing from the export");
  });

  it("the confirm disables when the input likely does not fit or nothing was found", () => {
    render(PREVIEW, 1024); // far less than 2 × 4 KiB? 1024 < 8192 → no fit
    expect(el.querySelector<HTMLButtonElement>("#preview-import-btn")!.disabled).toBe(true);
    expect(el.textContent).toContain("free — the archive may not fit");

    render({ ...PREVIEW, messages_seen: 0, media_referenced: 0 }, 1 << 30);
    expect(el.querySelector<HTMLButtonElement>("#preview-import-btn")!.disabled).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// GP03: the confirmation card — timezone confirmation for naive stamps
// and the destination-source chooser.
// ---------------------------------------------------------------------------

describe("renderImportPreview confirmation card (GP03)", () => {
  it("states the naive-stamp count, preselects the assumed zone, and hands the confirmed zone to Import", () => {
    let confirmed: { zone: string | null; source: string } | null = null;
    renderImportPreview(el, { ...PREVIEW, naive_timestamps: 3, assumed_zone: "Europe/Belgrade" }, {
      path: "/tmp/chat.txt",
      freeBytes: 1 << 30,
      busy: false,
      existingSources: [],
      onImport: (c) => (confirmed = c),
      onCancel: () => {},
    });
    expect(el.textContent).toContain(
      "3 messages carry clock times without a timezone — assumed Europe/Belgrade",
    );
    expect(el.textContent).toContain("first occurrence");
    expect(el.textContent).toContain("stays unresolved");
    const zone = el.querySelector<HTMLInputElement>("#preview-zone-input")!;
    expect(zone.value).toBe("Europe/Belgrade");
    zone.value = "Europe/Berlin"; // corrected once, before confirming
    el.querySelector<HTMLButtonElement>("#preview-import-btn")!.click();
    expect(confirmed).toEqual({ zone: "Europe/Berlin", source: "telegram" });
  });

  it("renders no zone block when the preview saw no naive timestamps", () => {
    render(PREVIEW);
    expect(el.querySelector("#preview-zone-input")).toBeNull();
    let confirmed: { zone: string | null; source: string } | null = null;
    renderImportPreview(el, PREVIEW, {
      path: "/tmp/result.json",
      freeBytes: 1 << 30,
      busy: false,
      existingSources: [],
      onImport: (c) => (confirmed = c),
      onCancel: () => {},
    });
    el.querySelector<HTMLButtonElement>("#preview-import-btn")!.click();
    expect(confirmed!.zone).toBeNull(); // nothing was assumed, nothing to confirm
  });

  it("names the derived source, preselects it, and offers existing import sources", () => {
    renderImportPreview(el, PREVIEW, {
      path: "/tmp/result.json",
      freeBytes: 1 << 30,
      busy: false,
      existingSources: ["telegram", "whatsapp"],
      onImport: () => {},
      onCancel: () => {},
    });
    expect(el.textContent).toContain('Import into source "telegram"');
    const source = el.querySelector<HTMLInputElement>("#preview-source-input")!;
    expect(source.value).toBe("telegram"); // the derived default — no required step
    const options = [...el.querySelectorAll("#preview-source-list option")].map((o) =>
      (o as HTMLOptionElement).value,
    );
    expect(options).toEqual(["whatsapp"]); // the derived name is not repeated
    let confirmed: { zone: string | null; source: string } | null = null;
    renderImportPreview(el, PREVIEW, {
      path: "/tmp/result.json",
      freeBytes: 1 << 30,
      busy: false,
      existingSources: [],
      onImport: (c) => (confirmed = c),
      onCancel: () => {},
    });
    const input = el.querySelector<HTMLInputElement>("#preview-source-input")!;
    input.value = "Family chats"; // a new name
    el.querySelector<HTMLButtonElement>("#preview-import-btn")!.click();
    expect(confirmed!.source).toBe("Family chats");
  });
});

// ---------------------------------------------------------------------------
// Inline progress card (wave 3)
// ---------------------------------------------------------------------------

const job = (over: Partial<JobRecord> & { state: JobRecord["state"] }): JobRecord => ({
  id: 12,
  kind: "import_chat_export",
  submitted_at: "2026-11-01T10:00:00Z",
  started_at: "2026-11-01T10:00:01Z",
  finished_at: null,
  progress: null,
  ...over,
});

describe("isImportJobKind", () => {
  it("names exactly the two kinds the import flow submits", () => {
    expect(isImportJobKind("import_mbox")).toBe(true);
    expect(isImportJobKind("import_chat_export")).toBe(true);
    expect(isImportJobKind("imap_sync")).toBe(false);
    expect(isImportJobKind("embed_messages")).toBe(false);
  });
});

describe("renderImportProgress", () => {
  const renderJob = (record: JobRecord, path?: string) => {
    const onCancel = vi.fn();
    const onDismiss = vi.fn();
    renderImportProgress(el, record, { path, onCancel, onDismiss });
    return { onCancel, onDismiss };
  };

  it("a queued job renders the job label, its path, a queued line, and Cancel", () => {
    const { onCancel } = renderJob(
      job({ kind: "import_mbox", state: { state: "queued" } }),
      "/tmp/archive.mbox",
    );
    expect(el.hidden).toBe(false);
    expect(el.querySelector(".preview-title")!.textContent).toContain(".mbox import");
    expect(el.querySelector(".preview-path")!.textContent).toBe("/tmp/archive.mbox");
    expect(el.querySelector(".import-progress-line")!.textContent).toContain("Queued");
    expect(el.getAttribute("role")).toBeNull(); // the line is the live region, not the card
    expect(el.querySelector(".import-progress-line")!.getAttribute("role")).toBe("status");
    const cancel = el.querySelector<HTMLButtonElement>("#import-cancel-btn")!;
    expect(cancel.disabled).toBe(false);
    cancel.click();
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("a running job with a known total shows shared counts wording and a determinate bar", () => {
    renderJob(
      job({
        state: { state: "running" },
        progress: { done: 291, total: 582, message: "reading messages" },
      }),
    );
    // The same line the Activity row shows (describeJobState's counts).
    expect(el.querySelector(".import-progress-line")!.textContent).toBe(
      "291/582 (50%) · reading messages",
    );
    const fill = el.querySelector<HTMLElement>(".progress-fill")!;
    expect(fill.style.width).toBe("50%");
    const track = el.querySelector(".progress-track")!;
    expect(track.classList.contains("indeterminate")).toBe(false);
    expect(track.getAttribute("aria-hidden")).toBe("true"); // visual; the line states the numbers
    expect(el.querySelector("#import-cancel-btn")).toBeTruthy();
  });

  it("percent clamps at 100 when done passes the reported total", () => {
    renderJob(
      job({
        state: { state: "running" },
        progress: { done: 70, total: 60, message: "" },
      }),
    );
    expect(el.querySelector(".import-progress-line")!.textContent).toBe("70/60 (100%)");
    expect(el.querySelector<HTMLElement>(".progress-fill")!.style.width).toBe("100%");
  });

  it("an unknown total renders the message and an indeterminate bar, never a fake fill", () => {
    renderJob(
      job({
        state: { state: "running" },
        progress: { done: 5, total: 0, message: "counting messages" },
      }),
    );
    expect(el.querySelector(".import-progress-line")!.textContent).toBe("counting messages");
    const track = el.querySelector(".progress-track")!;
    expect(track.classList.contains("indeterminate")).toBe(true);
    expect(track.getAttribute("aria-hidden")).toBe("true"); // decorative movement
    expect(el.querySelector(".progress-fill")).toBeNull();
  });

  it("a running job with neither total nor message says it plainly", () => {
    renderJob(job({ state: { state: "running" }, progress: null }));
    expect(el.querySelector(".import-progress-line")!.textContent).toBe("Importing…");
  });

  it("a failed job keeps its message in the card with Close, not Cancel", () => {
    const { onDismiss, onCancel } = renderJob(
      job({ state: { state: "failed", error: { kind: "import", message: "bad export" } } }),
    );
    const line = el.querySelector(".import-progress-line")!;
    expect(line.textContent).toBe("Import failed — bad export.");
    expect(line.classList.contains("error")).toBe(true);
    expect(el.textContent).toContain("nothing is duplicated");
    expect(el.querySelector("#import-cancel-btn")).toBeNull();
    el.querySelector<HTMLButtonElement>("#import-progress-dismiss")!.click();
    expect(onDismiss).toHaveBeenCalledTimes(1);
    expect(onCancel).not.toHaveBeenCalled();
  });

  it("a failed job with a Retry affordance re-submits through onRetry (GP03)", () => {
    const onRetry = vi.fn();
    renderImportProgress(
      el,
      job({ state: { state: "failed", error: { kind: "import", message: "interrupted" } } }),
      { onCancel: () => {}, onDismiss: () => {}, onRetry },
    );
    const retry = el.querySelector<HTMLButtonElement>("#import-retry-btn")!;
    expect(retry.textContent).toBe("Retry import");
    retry.click();
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("a failed job without a retry callback renders no dead Retry button", () => {
    renderJob(job({ state: { state: "failed", error: { kind: "import", message: "x" } } }));
    expect(el.querySelector("#import-retry-btn")).toBeNull();
  });

  it("succeeded and cancelled render nothing — their hand-offs live elsewhere", () => {
    renderJob(job({ state: { state: "succeeded", result: null } }));
    expect(el.hidden).toBe(true);
    expect(el.children.length).toBe(0);
    renderJob(job({ state: { state: "cancelled" } }));
    expect(el.hidden).toBe(true);
    expect(el.children.length).toBe(0);
  });

  it("a running job with counts and an ETA appends the ETA to the line", () => {
    renderImportProgress(
      el,
      job({
        state: { state: "running" },
        progress: { done: 2, total: 4, message: "reading messages" },
      }),
      { onCancel: () => {}, onDismiss: () => {}, eta: "~2 minutes" },
    );
    expect(el.querySelector(".import-progress-line")!.textContent).toBe(
      "2/4 (50%) · reading messages · ~2 minutes",
    );
  });

  it("a running job whose ETA is honest 'estimating…' says so rather than hiding", () => {
    renderImportProgress(
      el,
      job({
        state: { state: "running" },
        progress: { done: 1, total: 4, message: "reading messages" },
      }),
      { onCancel: () => {}, onDismiss: () => {}, eta: "estimating…" },
    );
    expect(el.querySelector(".import-progress-line")!.textContent).toBe(
      "1/4 (25%) · reading messages · estimating…",
    );
  });

  it("an unknown-total line never gains an ETA suffix (nothing to estimate against)", () => {
    renderImportProgress(
      el,
      job({
        state: { state: "running" },
        progress: { done: 5, total: 0, message: "counting messages" },
      }),
      { onCancel: () => {}, onDismiss: () => {}, eta: "estimating…" },
    );
    expect(el.querySelector(".import-progress-line")!.textContent).toBe("counting messages");
  });
});

// ---------------------------------------------------------------------------
// Completion card (P0·02): real counts + the pre-focused search hand-off
// ---------------------------------------------------------------------------

describe("buildImportCompletion", () => {
  const COMPLETION = {
    source: "mbox",
    seen: 120,
    inserted: 118,
    skipped: 2,
    failed: 0,
  };

  function build(seed: string | null) {
    const onSearch = vi.fn();
    const card = buildImportCompletion(COMPLETION, { seed, onSearch });
    el.append(card);
    return { onSearch, card };
  }

  it("renders the run's inserted/skipped counts from the outcome summary", () => {
    build(null);
    expect(el.querySelector(".preview-title")!.textContent).toBe("Import complete");
    expect(el.querySelectorAll(".meta")[0]!.textContent).toBe(
      "Imported 118 new, 2 already present (of 120 in Mail import).",
    );
  });

  it("names parse failures when the run had them", () => {
    const card = buildImportCompletion(
      { ...COMPLETION, failed: 3 },
      { seed: null, onSearch: () => {} },
    );
    el.append(card);
    expect(el.querySelectorAll(".meta")[0]!.textContent).toBe(
      "Imported 118 new, 2 already present, 3 failed to parse (of 120 in Mail import).",
    );
  });

  it("carries no receipt id — the outcome shape has none and none is invented", () => {
    build("ana@example.com");
    expect(el.textContent).not.toContain("receipt");
    expect(el.querySelector("[data-receipt-id]")).toBeNull();
  });

  it("seeds the search box with the archive's most frequent sender", () => {
    build("Ana Sender <ana@example.com>");
    const input = el.querySelector<HTMLInputElement>("#import-done-search")!;
    expect(input.value).toBe("Ana Sender <ana@example.com>");
    expect(input.getAttribute("aria-label")).toBe("Search your archive");
  });

  it("an unseeded box renders empty and says search is ready anyway", () => {
    build(null);
    const input = el.querySelector<HTMLInputElement>("#import-done-search")!;
    expect(input.value).toBe("");
    expect(el.textContent).toContain("type anything and press Enter");
  });

  it("Enter in the search box hands the query to onSearch; empty never fires", () => {
    const { onSearch } = build("marko");
    const input = el.querySelector<HTMLInputElement>("#import-done-search")!;
    input.value = "marko";
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(onSearch).toHaveBeenCalledWith("marko");
    input.value = "   ";
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(onSearch).toHaveBeenCalledTimes(1);
  });

  it("the Search button submits the same hand-off for pointer users", () => {
    const { onSearch } = build("jelena");
    const input = el.querySelector<HTMLInputElement>("#import-done-search")!;
    input.value = "jelena";
    el.querySelector<HTMLButtonElement>("#import-done-search-btn")!.click();
    expect(onSearch).toHaveBeenCalledWith("jelena");
  });
});

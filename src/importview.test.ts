// @vitest-environment happy-dom
// Unit tests for the import preview card builder (U03): what renders,
// what the counts line says for exact vs capped scans, and when the
// confirm button disables (space, nothing found, busy).
import { Window } from "happy-dom";
import { beforeEach, describe, expect, it } from "vitest";
import { formatHint, formatLabel, importActionLabel, renderImportPreview } from "./importview";
import type { ImportPreview } from "./types";

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
  free_bytes: 1024 * 1024 * 1024,
};

let el: HTMLElement;

function render(preview: ImportPreview, freeBytes: number | null = 1 << 30): void {
  renderImportPreview(el as HTMLElement, preview, {
    path: "/tmp/result.json",
    freeBytes,
    busy: false,
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
    expect(formatLabel("whatsapp")).toBe("WhatsApp export");
    expect(formatHint("telegram")).toContain("Telegram Desktop");
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

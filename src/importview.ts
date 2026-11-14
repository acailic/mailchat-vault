// Import preview card and first-run guidance (U02/U03, first-source
// journey): pure DOM builders, every dynamic string through textContent.
// The preview shows what the bounded backend scan found — detected
// format, sample messages, bounded counts, media presence, size and
// free-space guidance — and the confirm/cancel actions the wiring in
// main.ts attaches. Unit-tested via happy-dom in importview.test.ts.

import { humanSize } from "./format";
import type { ImportPreview } from "./types";

/** How the confirm button labels itself for each detected format. */
export function importActionLabel(preview: ImportPreview): string {
  const count = preview.scan_complete
    ? String(preview.messages_seen)
    : `the first ${preview.messages_seen}`;
  return `Import ${count} message${preview.messages_seen === 1 ? "" : "s"}`;
}

/** Human names + where the export comes from, per format. */
export function formatLabel(kind: string): string {
  switch (kind) {
    case "mbox":
      return "Email archive (.mbox)";
    case "eml":
      return "Email messages (.eml)";
    case "whatsapp":
      return "WhatsApp export";
    case "telegram":
      return "Telegram export";
    case "signal":
      return "Signal conversation export";
    case "discord":
      return "Discord Data Package";
    case "messenger":
      return "Messenger (Facebook) export";
    default:
      return kind;
  }
}

/** One line per format: how to produce this export on the phone/desktop. */
export function formatHint(kind: string): string | null {
  switch (kind) {
    case "mbox":
      return "Any mail app can export folders as .mbox (Thunderbird: ImportExportTools NG).";
    case "eml":
      return "Drag messages out of Outlook (or any mail app) as .eml files; a folder of them imports together.";
    case "whatsapp":
      return "WhatsApp → chat → ⋮ → More → Export chat (with or without media).";
    case "telegram":
      return "Telegram Desktop → chat settings → Export chat history (JSON, machine-readable).";
    case "signal":
      return "sigtop export-messages -f json (Signal Desktop keeps no built-in JSON export).";
    case "discord":
      return "Discord → Settings → Privacy → Request Data (the package arrives by email).";
    case "messenger":
      return "Facebook → Settings → Your Facebook information → Download profile information — set Format to JSON; import messages/ or the whole export.";
    default:
      return null;
  }
}

export interface PreviewOptions {
  /** The path the picker returned (shown so the user can verify it). */
  path: string;
  /** Free bytes on the vault disk (null = unknown). */
  freeBytes: number | null;
  /** Import is disabled while another import runs. */
  busy: boolean;
  onImport: () => void;
  onCancel: () => void;
}

/**
 * Render the preview card. The confirm button is disabled when the
 * free-space guidance says the archive likely does not fit (twice the
 * input size must be free: text lands in the database and media files
 * are copied), when the preview saw no messages, or while busy.
 */
export function renderImportPreview(el: HTMLElement, preview: ImportPreview, opts: PreviewOptions): void {
  el.replaceChildren();
  el.hidden = false;

  const card = document.createElement("div");
  card.className = "preview-card";

  const head = document.createElement("div");
  head.className = "row";
  const title = document.createElement("h3");
  title.className = "preview-title";
  title.textContent = formatLabel(preview.kind);
  const path = document.createElement("span");
  path.className = "meta preview-path";
  path.title = opts.path;
  path.textContent = opts.path;
  head.append(title, path);
  card.append(head);

  const counts = document.createElement("div");
  counts.className = "meta";
  const countText = preview.scan_complete
    ? `${preview.messages_seen} message${preview.messages_seen === 1 ? "" : "s"} found`
    : `at least ${preview.messages_seen} messages (preview scanned the first ${preview.messages_seen})`;
  const parts = [countText];
  if (preview.parse_failures > 0) {
    parts.push(`${preview.parse_failures} entries will be reported as issues`);
  }
  parts.push(`${humanSize(preview.input_bytes)} input`);
  if (preview.media_referenced > 0) {
    parts.push(
      preview.media_missing > 0
        ? `${preview.media_referenced} messages reference media, ${preview.media_missing} file${preview.media_missing === 1 ? "" : "s"} missing from the export`
        : `${preview.media_referenced} messages reference media files`,
    );
  }
  counts.textContent = parts.join(" · ");
  card.append(counts);

  if (opts.freeBytes !== null) {
    const space = document.createElement("div");
    space.className = "meta";
    const fits = opts.freeBytes >= preview.input_bytes * 2;
    space.textContent = fits
      ? `${humanSize(opts.freeBytes)} free on this machine`
      : `only ${humanSize(opts.freeBytes)} free — the archive may not fit`;
    if (!fits) space.classList.add("error");
    card.append(space);
  }

  const hint = formatHint(preview.kind);
  if (hint) {
    const hintEl = document.createElement("p");
    hintEl.className = "meta preview-hint";
    hintEl.textContent = hint;
    card.append(hintEl);
  }

  if (preview.samples.length > 0) {
    const list = document.createElement("ul");
    list.className = "preview-samples";
    for (const sample of preview.samples) {
      const li = document.createElement("li");
      const meta = document.createElement("div");
      meta.className = "meta";
      const who = sample.author || "unknown";
      const when = sample.sent_at ?? "no date";
      const what = sample.subject || "(no text)";
      meta.textContent = `${who} · ${when} · ${what}`;
      const body = document.createElement("div");
      body.className = "body";
      body.textContent = sample.snippet;
      li.append(meta, body);
      list.append(li);
    }
    card.append(list);
  }

  for (const warning of preview.warnings) {
    const warn = document.createElement("p");
    warn.className = "meta preview-warning";
    warn.textContent = warning;
    card.append(warn);
  }

  const actions = document.createElement("div");
  actions.className = "row";
  const confirm = document.createElement("button");
  confirm.type = "button";
  confirm.id = "preview-import-btn";
  confirm.className = "primary";
  confirm.textContent = importActionLabel(preview);
  const noSpace = opts.freeBytes !== null && opts.freeBytes < preview.input_bytes * 2;
  const nothing = preview.messages_seen === 0 && preview.parse_failures === 0;
  confirm.disabled = opts.busy || noSpace || nothing;
  if (noSpace) {
    confirm.title = "Not enough free space for this archive";
  } else if (nothing) {
    confirm.title = "Nothing importable was found in this file";
  }
  confirm.addEventListener("click", opts.onImport);
  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.id = "preview-cancel-btn";
  cancel.textContent = "Choose another file";
  cancel.addEventListener("click", opts.onCancel);
  actions.append(confirm, cancel);
  card.append(actions);

  el.append(card);
}

/** Hide the preview card (cancel / after the import finished). */
export function hideImportPreview(el: HTMLElement): void {
  el.hidden = true;
  el.replaceChildren();
}

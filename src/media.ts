/// Media display (M4.3): how preserved attachment objects render in the
/// reading pane, and the media-store usage report for Settings.
///
/// Preserved objects are served read-only by the app's `mcmedia://`
/// protocol handler (strict name validation, byte ranges, hard response
/// CSP — see `src-tauri/src/media_proto.rs`); this module only builds
/// URLs and DOM. Playback is always user-started: no `autoplay`
/// attribute is ever set, and preload is `"none"` (audio) /
/// `"metadata"` (video) so nothing is fetched until the user acts.
/// Documents — and anything not provably passive media — render as a
/// row with explicit actions only, never inline: HTML in an attachment
/// is data to export, never content to execute.

import { type IconName, iconEl } from "./icons";
import { humanSize } from "./format";
import type { AttachmentSummary } from "./v2types";

/** The URL of one store object, or null when the path is not a
 * `media/<sha256>.<ext>` store path (never build a URL from anything
 * else — the protocol handler would reject it anyway). */
export function mediaUrl(path: string | null | undefined): string | null {
  const name = objectNameOf(path);
  return name === null ? null : `mcmedia://local/${name}`;
}

/** The validated `<64-hex>.<ext>` object name of a store-relative path,
 * or null for anything else (mirrors the backend's validation so a bad
 * `raw_json` never produces a fetchable-looking URL). */
export function objectNameOf(path: string | null | undefined): string | null {
  if (typeof path !== "string") return null;
  const name = path.split("/").pop() ?? "";
  if (!/^\/?media\/[^/]+$/.test(path.startsWith("/") ? path : `/${path}`)) return null;
  const match = /^([0-9a-f]{64})\.([a-z0-9]{1,16})$/.exec(name);
  return match === null ? null : name;
}

/** How an available attachment renders. `null` = row only (documents,
 * unknown kinds): nothing inline. */
export function inlineKind(attachment: AttachmentSummary): "image" | "audio" | "video" | null {
  const name = objectNameOf(attachment.path);
  const ext = name === null ? "" : (name.split(".").pop() ?? "");
  switch (attachment.kind) {
    case "image":
    case "photo":
    case "gif":
    case "animation":
      return "image";
    // Stickers: renderable only in a raster format (webp animations,
      // static stickers); `.tgs` (Lottie JSON) and `.webm` video
      // stickers stay rows.
    case "sticker":
      return ["webp", "jpg", "jpeg", "png", "gif", "bmp"].includes(ext) ? "image" : null;
    case "audio":
    case "voice_message":
    case "audio_file":
      return "audio";
    case "video":
    case "video_file":
    case "video_message":
      return "video";
    default:
      return null;
  }
}

export interface MediaItemHandlers {
  /** Save a byte-exact copy of the object (native save dialog; M4.3). */
  onExport: (attachment: AttachmentSummary) => void;
  /** Open the object with the OS default app (explicit; M4.3). */
  onOpenExternal: (attachment: AttachmentSummary) => void;
}

// ---------------------------------------------------------------------------
// Attachment chips (the media section's compact overview)
// ---------------------------------------------------------------------------

/** Sprite icon for one attachment kind (chips lead with it; decorative,
 * aria-hidden — the chip's text is the accessible name). */
export function mediaChipIcon(kind: string): IconName {
  switch (kind) {
    case "image":
    case "photo":
    case "gif":
    case "animation":
    case "sticker":
      return "image";
    case "audio":
    case "voice_message":
    case "audio_file":
      return "audio";
    case "video":
    case "video_file":
    case "video_message":
      return "video";
    default:
      return "file";
  }
}

/** How many attachment chips stay on the collapsed row before the
 * "+N more" expander — a fixed count like the era-chip cap
 * (ERA_CHIP_VISIBLE), never a layout measurement. */
export const MEDIA_CHIP_VISIBLE = 4;

/** Build one chip <li>: icon by kind, name, humanSize (.tnum). An
 * available attachment is a button whose one action opens it with the
 * OS default app (Enter included); an unpreserved one renders as a
 * quiet non-interactive chip that says "not in export". */
function mediaChipItem(
  attachment: AttachmentSummary,
  handlers: MediaItemHandlers,
): HTMLLIElement {
  const li = document.createElement("li");
  const name = document.createElement("span");
  name.className = "media-chip-name";
  name.textContent = attachment.name;
  const size = document.createElement("span");
  size.className = "media-chip-size tnum";
  if (!attachment.available) {
    // A reference the export did not preserve: an honest quiet chip,
    // never an actionable one.
    const chip = document.createElement("span");
    chip.className = "chip media-chip unresolved";
    chip.title = "This attachment is a reference — the file was not preserved in the export";
    size.textContent = "not in export";
    chip.append(iconEl(mediaChipIcon(attachment.kind)), name, size);
    li.append(chip);
    return li;
  }
  const chip = document.createElement("button");
  chip.type = "button";
  chip.className = "chip media-chip";
  chip.title = `Open ${attachment.name} with your system's default application`;
  size.textContent = humanSize(attachment.size);
  chip.append(iconEl(mediaChipIcon(attachment.kind)), name, size);
  const open = () => handlers.onOpenExternal(attachment);
  chip.addEventListener("click", open);
  // Buttons activate on Enter/Space natively; handling the keys here
  // with preventDefault keeps activation explicit (and testable)
  // without double-firing the native click.
  chip.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      open();
    }
  });
  li.append(chip);
  return li;
}

/** Fill one chips row (the era-chip pattern): the first
 * [`MEDIA_CHIP_VISIBLE`] chips plus a "+N more" expander whose click
 * flips the row's own data-expanded state and re-renders in place, so
 * the expansion survives re-renders without any measurement. */
export function renderMediaChips(
  el: HTMLUListElement,
  attachments: AttachmentSummary[],
  handlers: MediaItemHandlers,
): void {
  const expanded = el.dataset.expanded === "true";
  const hidden = Math.max(0, attachments.length - MEDIA_CHIP_VISIBLE);
  if (hidden === 0) {
    // At or below the cap a stale expanded flag renders identically,
    // but resets now so a regrow starts collapsed (era-chip discipline).
    delete el.dataset.expanded;
  }
  const visible =
    expanded || hidden === 0 ? attachments : attachments.slice(0, MEDIA_CHIP_VISIBLE);
  el.replaceChildren(...visible.map((a) => mediaChipItem(a, handlers)));
  if (hidden > 0) {
    const li = document.createElement("li");
    li.className = "media-chip-more-item";
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "chip media-chip-more";
    toggle.textContent = expanded ? "Fewer" : `+${hidden} more`;
    toggle.setAttribute("aria-expanded", String(expanded));
    toggle.title = expanded
      ? "Show only the first attachments"
      : `Show ${hidden} more attachment${hidden === 1 ? "" : "s"}`;
    toggle.addEventListener("click", () => {
      el.dataset.expanded = expanded ? "false" : "true";
      renderMediaChips(el, attachments, handlers);
    });
    li.append(toggle);
    el.append(li);
  }
}

/** Build the media section's compact chips row. `initiallyExpanded`
 * carries the previous render's expansion for the same message (the
 * caller reads it off the old card, the exportPicker pattern). */
export function buildMediaChips(
  attachments: AttachmentSummary[],
  handlers: MediaItemHandlers,
  initiallyExpanded = false,
): HTMLUListElement {
  const list = document.createElement("ul");
  list.className = "media-chips";
  if (initiallyExpanded) list.dataset.expanded = "true";
  renderMediaChips(list, attachments, handlers);
  return list;
}

/** Build one attachment row of the reading pane's media section. */
export function buildMediaItem(
  attachment: AttachmentSummary,
  handlers: MediaItemHandlers,
): HTMLLIElement {
  const item = document.createElement("li");
  item.className = "media-item" + (attachment.available ? "" : " unresolved");

  const head = document.createElement("div");
  head.className = "media-head";
  const kind = document.createElement("span");
  kind.className = "media-kind";
  kind.textContent = attachment.kind;
  const name = document.createElement("span");
  name.className = "media-name";
  name.textContent = attachment.name;
  const size = document.createElement("span");
  size.className = "meta";
  size.textContent = attachment.available ? humanSize(attachment.size) : "not in export";
  head.append(kind, name, size);
  item.append(head);

  // Everything below needs a preserved, well-formed store object.
  const url = mediaUrl(attachment.path);
  if (url === null) return item;

  // Explicit, user-started actions for every preserved object.
  const actions = document.createElement("div");
  actions.className = "media-actions row";
  const saveButton = document.createElement("button");
  saveButton.type = "button";
  saveButton.className = "media-save-btn";
  saveButton.textContent = "Save a copy…";
  saveButton.title = "Save a byte-exact copy of this file";
  saveButton.addEventListener("click", () => handlers.onExport(attachment));
  const openButton = document.createElement("button");
  openButton.type = "button";
  openButton.className = "media-open-btn";
  openButton.textContent = "Open externally…";
  openButton.title = "Open this file with your system's default application";
  openButton.addEventListener("click", () => handlers.onOpenExternal(attachment));
  actions.append(saveButton, openButton);
  item.append(actions);

  // Inline rendering for passive media kinds only. Playback starts when
  // the user presses play: no autoplay attribute, minimal preload.
  const display = inlineKind(attachment);
  if (display === "image") {
    const frame = document.createElement("div");
    frame.className = "media-frame";
    const img = document.createElement("img");
    img.src = url;
    img.alt = attachment.name;
    img.loading = "lazy";
    img.decoding = "async";
    frame.append(img);
    item.append(frame);
  } else if (display === "audio") {
    const audio = document.createElement("audio");
    audio.className = "media-audio";
    audio.controls = true;
    audio.preload = "none";
    audio.src = url;
    item.append(audio);
  } else if (display === "video") {
    const frame = document.createElement("div");
    frame.className = "media-frame";
    const video = document.createElement("video");
    video.className = "media-video";
    video.controls = true;
    video.preload = "metadata";
    // Duration/size only — playback never starts on its own.
    video.src = url;
    frame.append(video);
    item.append(frame);
  }
  return item;
}

// ---------------------------------------------------------------------------
// Media-store usage report (Settings)
// ---------------------------------------------------------------------------

/** Wire shape of `media_usage` (mirrors vault-core's `MediaUsage`). */
export interface MediaUsageView {
  objects: number;
  total_bytes: number;
  referenced: number;
  referenced_bytes: number;
  orphan_objects: number;
  orphan_bytes: number;
  by_extension: { ext: string; objects: number; bytes: number }[];
  largest: { sha256: string; path: string; size: number }[];
}

/** Render the media-store disk usage report: totals, the
 * referenced/orphaned split, a per-extension table with inline bars,
 * and the largest objects. */
export function renderMediaUsage(el: HTMLElement, usage: MediaUsageView | null): void {
  el.replaceChildren();
  if (usage === null) {
    const empty = document.createElement("p");
    empty.className = "meta";
    empty.textContent = "No usage report yet.";
    el.append(empty);
    return;
  }
  const summary = document.createElement("p");
  summary.className = "meta";
  summary.textContent =
    `${usage.objects.toLocaleString()} objects · ${humanSize(usage.total_bytes)} total · ` +
    `${usage.referenced.toLocaleString()} referenced (${humanSize(usage.referenced_bytes)}) · ` +
    `${usage.orphan_objects.toLocaleString()} unreferenced (${humanSize(usage.orphan_bytes)})`;
  el.append(summary);

  if (usage.by_extension.length > 0) {
    const extTable = document.createElement("table");
    extTable.className = "media-usage-table";
    const max = Math.max(...usage.by_extension.map((e) => e.bytes), 1);
    for (const row of usage.by_extension) {
      const tr = document.createElement("tr");
      const ext = document.createElement("td");
      ext.textContent = `.${row.ext}`;
      const barCell = document.createElement("td");
      const bar = document.createElement("span");
      bar.className = "usage-bar";
      bar.style.width = `${Math.max((row.bytes / max) * 100, 1)}%`;
      barCell.append(bar);
      const count = document.createElement("td");
      count.textContent = `${row.objects.toLocaleString()} ×`;
      const size = document.createElement("td");
      size.textContent = humanSize(row.bytes);
      tr.append(ext, barCell, count, size);
      extTable.append(tr);
    }
    el.append(extTable);
  }

  if (usage.largest.length > 0) {
    const list = document.createElement("ul");
    list.className = "media-usage-largest";
    for (const object of usage.largest) {
      const li = document.createElement("li");
      li.className = "meta";
      li.textContent = `${humanSize(object.size)} · ${object.path}`;
      list.append(li);
    }
    const heading = document.createElement("p");
    heading.className = "meta";
    heading.textContent = "Largest objects:";
    el.append(heading, list);
  }
}

/** Icon names — one entry per `<symbol id="i-…">` in the index.html sprite.
 * The `empty-*` names are the empty-state illustrations: larger 48-grid
 * line-art scenes (not UI glyphs) under the same decorative, aria-hidden
 * contract — the headline and copy beside them carry the meaning. */
export type IconName =
  | "archive"
  | "sources"
  | "settings"
  | "activity"
  | "menu"
  | "search"
  | "close"
  | "export"
  | "paperclip"
  | "copy"
  | "link"
  | "check"
  | "warning"
  | "error"
  | "spinner"
  | "mail"
  | "chat"
  | "file"
  | "server"
  | "empty-search"
  | "empty-sources"
  | "empty-import"
  | "empty-backup";

/** Inline icon markup referencing the static sprite in index.html.
 * Decorative by contract: aria-hidden, always accompanied by a text label. */
export function icon(name: IconName, cls = ""): string {
  const classes = cls ? `icon ${cls}` : "icon";
  return `<svg class="${classes}" aria-hidden="true" focusable="false"><use href="#i-${name}"></use></svg>`;
}

/** DOM element variant of `icon` for programmatic rendering. */
export function iconEl(name: IconName, cls = ""): SVGSVGElement {
  const tpl = document.createElement("template");
  tpl.innerHTML = icon(name, cls).trim();
  return tpl.content.firstElementChild as SVGSVGElement;
}

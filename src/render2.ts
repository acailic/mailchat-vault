/// DOM rendering for the v2 search UI (M3.2): flat results rows, era
/// chips, applied filter chips, and the v2 reading pane (detail +
/// chronological context).
/// Pure builders like render.ts — every dynamic string goes through
/// textContent, tested via happy-dom in render2.test.ts.

import {
  authorDisplayName,
  conversationConfidenceLabel,
  formatSentAt,
  friendlySourceLabel,
  importScopeLabel,
  snippet as truncateBody,
  timeConfidenceDetail,
  timeConfidenceLabel,
} from "./format";
import { type IconName, icon, iconEl } from "./icons";
import {
  appendSegments,
} from "./render";
import {
  type ContextEntry,
  type EraBucket,
  type FilterChip,
  messageLink,
} from "./searchstate";
import {
  highlightSegments,
  htmlOf,
  parseSnippet,
  splitTerms,
} from "./viewmodel";
import { buildMediaItem } from "./media";
import type {
  AttachmentSummary,
  ConversationContext,
  ImportScope,
  MessageSummary,
} from "./v2types";

// ---------------------------------------------------------------------------
// Flat results
// ---------------------------------------------------------------------------

export interface FlatResultsOptions {
  hits: MessageSummary[];
  /** Keyboard-active row (-1 = none). */
  activeIndex: number;
  /** Row the user opened (Back restored it); -1 = none. */
  selectedId: number | null;
  /** Ids checked for selection export. */
  selection: Set<number>;
  /** Query terms for client-side highlighting in browse mode. */
  terms: string[];
  onOpen: (id: number) => void;
  onToggleSelect: (id: number) => void;
}

/** The time-uncertainty marker shown instead of a fabricated timestamp. */
export function timeLabel(hit: MessageSummary): string {
  const base = formatSentAt(hit.sent_at ?? "");
  if (!hit.sent_at) return base;
  // Legacy-assumed dates stay visible but labeled (A04 honesty).
  return hit.time_confidence === "legacy_assumed"
    ? `${base} (assumed)`
    : hit.time_confidence === "unresolved"
      ? `${base} (unresolved)`
      : base;
}

function resultRow(hit: MessageSummary, opts: FlatResultsOptions, at: number): HTMLLIElement {
  const li = document.createElement("li");
  li.className =
    "hit-item" +
    (at === opts.activeIndex ? " active" : "") +
    (opts.selectedId === hit.id ? " selected" : "");
  li.dataset.id = String(hit.id);
  li.dataset.index = String(at);
  if (at === opts.activeIndex) li.setAttribute("aria-current", "true");

  // Selection checkbox for the explicit selection export — a sibling of
  // the row's button, never a control nested inside one (no double
  // announcement, no Space-key ambiguity).
  const check = document.createElement("input");
  check.type = "checkbox";
  check.className = "row-select";
  check.checked = opts.selection.has(hit.id);
  check.setAttribute("aria-label", `Select message ${hit.id} for export`);
  check.addEventListener("click", (event) => event.stopPropagation());
  check.addEventListener("change", () => opts.onToggleSelect(hit.id));

  // The row's activatable area: role=button + tabindex live here, on an
  // element that contains only text. The li stays a plain list item
  // (class/data/aria-current remain on it for styling and keyboard nav).
  const main = document.createElement("div");
  main.className = "hit-main";
  main.setAttribute("role", "button");
  main.setAttribute("tabindex", "0");
  main.title = "Open this message";
  const meta = document.createElement("div");
  meta.className = "meta";
  const displayAuthor = authorDisplayName(hit.author) || "unknown";
  // The time sits in its own span so a marked-uncertain time can
  // explain itself on hover: "(assumed)" no longer waits for the
  // detail pane to say what it means (A04 honesty). The wording is
  // the detail pane's own one-sentence explanation, so both surfaces
  // say the same thing.
  const time = document.createElement("span");
  time.className = "time";
  time.textContent = timeLabel(hit);
  if (
    hit.time_confidence === "legacy_assumed" ||
    hit.time_confidence === "unresolved"
  ) {
    time.title = timeConfidenceDetail(hit.time_confidence);
  }
  meta.append(displayAuthor, " · ", time, " · ", friendlySourceLabel(hit.source_label));
  if (hit.conversation_label) meta.append(" · ", hit.conversation_label);
  // The full "Name <address>" form stays one hover away.
  if (displayAuthor !== (hit.author || "unknown")) meta.title = hit.author;
  if (hit.attachment_count > 0) {
    const files = document.createElement("span");
    files.className = "badge attachments";
    files.title =
      hit.available_attachment_count < hit.attachment_count
        ? `${hit.attachment_count} attachment references, ${hit.available_attachment_count} preserved`
        : `${hit.attachment_count} attachments`;
    files.setAttribute(
      "aria-label",
      `attachments: ${hit.available_attachment_count} of ${hit.attachment_count}`,
    );
    // The visible words say what the numbers mean — a bare "0/1" reads
    // as a fraction with no units; the title/aria keep the exact counts.
    files.innerHTML =
      icon("paperclip") +
      (hit.available_attachment_count < hit.attachment_count
        ? `${hit.available_attachment_count} of ${hit.attachment_count} files kept`
        : `${hit.attachment_count} ${hit.attachment_count === 1 ? "file" : "files"}`);
    meta.append(" ", files);
  }
  if (hit.has_conflicting_revision) {
    const conflict = document.createElement("span");
    conflict.className = "badge conflict";
    conflict.title = "Another revision of this message exists in the archive";
    conflict.textContent = "revisions";
    meta.append(" ", conflict);
  }

  const body = document.createElement("div");
  body.className = "body";
  if (hit.snippet) {
    appendSegments(body, parseSnippet(hit.snippet));
  } else if (hit.subject) {
    body.textContent = hit.subject;
  } else {
    // Browse without a subject: nothing bounded to show — the metadata
    // row is the honest summary (bodies are a detail-view payload).
    body.textContent = "";
  }

  main.append(meta, body);
  li.append(check, main);
  const open = () => opts.onOpen(hit.id);
  // Clicks anywhere in the row except the checkbox open the message:
  // the li listener is plain delegation (the li carries no button
  // semantics), and the checkbox stops its own clicks from bubbling.
  li.addEventListener("click", open);
  // Keyboard activation belongs to the button element alone.
  main.addEventListener("keydown", (event) => {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      event.stopPropagation();
      open();
    }
  });
  return li;
}

/** Render the flat, stable results list (no conversation regrouping). */
export function renderFlatResults(el: HTMLElement, opts: FlatResultsOptions): void {
  el.replaceChildren(...opts.hits.map((hit, at) => resultRow(hit, opts, at)));
}

// ---------------------------------------------------------------------------
// Applied filter chips
// ---------------------------------------------------------------------------

export interface FilterChipsOptions {
  chips: FilterChip[];
  onRemove: (chip: FilterChip) => void;
  onClear: () => void;
}

/** Removable chips for the applied filters + a Clear all button. */
export function renderFilterChips(el: HTMLElement, opts: FilterChipsOptions): void {
  const chips = opts.chips.map((chip) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "chip filter-chip";
    button.dataset.kind = chip.kind;
    button.title = `Remove filter: ${chip.label}`;
    const label = document.createElement("span");
    label.textContent = chip.label;
    const x = document.createElement("span");
    x.className = "chip-x";
    x.textContent = "×";
    x.setAttribute("aria-hidden", "true");
    button.append(label, " ", x);
    button.addEventListener("click", () => opts.onRemove(chip));
    return button;
  });
  if (chips.length > 0) {
    const clear = document.createElement("button");
    clear.type = "button";
    clear.className = "chip clear-chips";
    clear.textContent = "Clear filters";
    clear.addEventListener("click", opts.onClear);
    el.replaceChildren(...chips, clear);
  } else {
    el.replaceChildren();
  }
}

// ---------------------------------------------------------------------------
// Era chips (timeline filters)
// ---------------------------------------------------------------------------

export interface EraChipsOptions {
  buckets: EraBucket[];
  /** The bucket the filters currently hold (-1 = none). */
  activeIndex: number;
  onToggle: (index: number) => void;
}

/** How many era chips stay visible before the "Older…" toggle: the most
 * recent buckets, so a long timeline leads with the eras near today. */
export const ERA_CHIP_VISIBLE = 5;

/**
 * The era strip: one quiet row of timeline toggles under the toolbar,
 * labelled "Time". Each chip applies its bucket's exact from/to bounds
 * through the normal date-input machinery (main.ts sets the inputs and
 * re-runs the search); the active chip is a pressed toggle whose click
 * removes the bound. Longer timelines cap the visible chips at the
 * [`ERA_CHIP_VISIBLE`] most recent, with an "Older…" toggle for the rest
 * (the expansion state lives on the strip's own data attribute, so it
 * survives re-renders). The active era never hides behind the cap —
 * hiding the pressed chip would hide the applied filter's own
 * affordance. Fewer than two buckets render nothing — no dead chrome
 * over a timeline too short to strip.
 */
export function renderEraChips(el: HTMLElement, opts: EraChipsOptions): void {
  if (opts.buckets.length < 2) {
    delete el.dataset.expanded;
    el.replaceChildren();
    return;
  }
  const expanded = el.dataset.expanded === "true";
  const hidden = Math.max(0, opts.buckets.length - ERA_CHIP_VISIBLE);
  // Buckets ascend oldest → newest; the cap keeps the most recent, and
  // `onToggle` always reports the index into the FULL bucket array.
  let visible: number[];
  if (expanded || hidden === 0) {
    visible = opts.buckets.map((_, index) => index);
  } else {
    visible = opts.buckets.slice(hidden).map((_, i) => hidden + i);
    if (opts.activeIndex >= 0 && opts.activeIndex < hidden) {
      visible.unshift(opts.activeIndex); // stays ascending: it is older
    }
  }

  const label = document.createElement("span");
  label.className = "era-chip-label";
  label.textContent = "Time";

  const chips = visible.map((index) => {
    const bucket = opts.buckets[index];
    const active = index === opts.activeIndex;
    const button = document.createElement("button");
    button.type = "button";
    button.className = "chip era-chip" + (active ? " on" : "");
    button.dataset.era = bucket.label;
    button.setAttribute("aria-pressed", active ? "true" : "false");
    button.title = active
      ? `Remove the ${bucket.label} date range (${bucket.from} → ${bucket.to})`
      : `Filter to ${bucket.label} (${bucket.from} → ${bucket.to})`;
    button.textContent = bucket.label;
    button.addEventListener("click", () => opts.onToggle(index));
    return button;
  });

  const more =
    hidden > 0
      ? (() => {
          const toggle = document.createElement("button");
          toggle.type = "button";
          toggle.className = "chip era-more";
          toggle.textContent = expanded ? "Fewer" : "Older…";
          toggle.setAttribute("aria-expanded", String(expanded));
          toggle.title = expanded
            ? "Show only the most recent time periods"
            : `Show ${hidden} older time period${hidden === 1 ? "" : "s"}`;
          toggle.addEventListener("click", () => {
            // Expansion is the strip's own state: it rides the element's
            // data attribute through every re-render.
            el.dataset.expanded = expanded ? "false" : "true";
            renderEraChips(el, opts);
          });
          return [toggle];
        })()
      : [];

  el.replaceChildren(label, ...chips, ...more);
}

// ---------------------------------------------------------------------------
// Import-scope picker
// ---------------------------------------------------------------------------

/** Fill the import-scope <select> from the listed scopes. Each option is
 * labeled by the run's first-observed day and message count (see
 * importScopeLabel); the raw import id stays one hover away in the
 * option's title. */
export function fillImportScopes(
  select: HTMLSelectElement,
  scopes: ImportScope[],
  selected: string | null,
): void {
  const options: HTMLOptionElement[] = [];
  const any = document.createElement("option");
  any.value = "";
  any.textContent = "any import";
  options.push(any);
  for (const scope of scopes) {
    const option = document.createElement("option");
    option.value = scope.import_id;
    option.textContent = importScopeLabel(scope);
    option.title = scope.import_id;
    options.push(option);
  }
  select.replaceChildren(...options);
  select.value = selected ?? "";
  if (select.value !== (selected ?? "")) select.value = "";
}

// ---------------------------------------------------------------------------
// Refresh notice
// ---------------------------------------------------------------------------

/** Show or hide the "archive changed under your results" refresh offer. */
export function renderRefreshNotice(
  el: HTMLElement,
  visible: boolean,
  onRefresh: () => void,
): void {
  el.replaceChildren();
  el.hidden = !visible;
  if (!visible) return;
  const note = document.createElement("span");
  note.className = "meta";
  note.textContent = "The archive changed — results may be out of date.";
  const button = document.createElement("button");
  button.type = "button";
  button.id = "refresh-results-btn";
  button.textContent = "Refresh";
  button.addEventListener("click", onRefresh);
  el.append(note, " ", button);
}

// ---------------------------------------------------------------------------
// v2 reading pane
// ---------------------------------------------------------------------------

export interface Detail2Options {
  ctx: ConversationContext;
  /** Held context rows (merged pages), for thread rendering. */
  entries: ContextEntry[];
  /** Query text for whole-word highlighting in the anchor body. */
  queryText: string;
  hasMoreBefore: boolean;
  hasMoreAfter: boolean;
  onOpenMessage: (id: number) => void;
  onLoadEarlier: () => void;
  onLoadLater: () => void;
  onCopy: () => void;
  onCopyLink: () => void;
  onExport: () => void;
  /** Export the whole verified conversation thread (v1 export_chat). */
  onExportChat: () => void;
  /** Save a byte-exact copy of one preserved media object (M4.3). */
  onExportMedia: (attachment: AttachmentSummary) => void;
  /** Open one preserved media object externally (M4.3). */
  onOpenExternalMedia: (attachment: AttachmentSummary) => void;
  onClose: () => void;
}

function contextRow(
  entry: ContextEntry,
  terms: string[],
  onOpen: (id: number) => void,
): HTMLLIElement {
  const row = document.createElement("li");
  row.className = "thread-row" + (entry.current ? " current" : "");
  row.dataset.id = String(entry.id);
  const meta = document.createElement("div");
  meta.className = "meta";
  meta.textContent = `${entry.author || "unknown"} · ${formatSentAt(entry.sentAt ?? "")}`;
  const body = document.createElement("div");
  body.className = "body";
  if (entry.current) {
    appendSegments(body, highlightSegments(entry.body, terms));
  } else {
    body.textContent = truncateBody(entry.body, 140);
  }
  row.append(meta, body);
  if (!entry.current) {
    row.classList.add("link");
    row.setAttribute("role", "button");
    row.setAttribute("tabindex", "0");
    row.title = "Open this message";
    const open = () => onOpen(entry.id);
    row.addEventListener("click", open);
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        event.stopPropagation();
        open();
      }
    });
  }
  return row;
}

/** A quiet icon-only action for the reading header. The icon is
 * decorative; the accessible name is the explicit aria-label and the
 * tooltip (title) carries the longer explanation. */
function iconButton(
  name: IconName,
  cls: string,
  label: string,
  title: string,
  onClick: () => void,
): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = `icon-btn ${cls}`;
  button.setAttribute("aria-label", label);
  button.title = title;
  button.append(iconEl(name));
  button.addEventListener("click", onClick);
  return button;
}

/** One format <select> for the reading header's Export picker. */
function formatSelect(
  id: string,
  label: string,
  options: Array<[value: string, text: string]>,
  selected: string | null,
): HTMLSelectElement {
  const select = document.createElement("select");
  select.id = id;
  select.setAttribute("aria-label", label);
  select.title = label;
  for (const [value, text] of options) {
    const option = document.createElement("option");
    option.value = value;
    option.textContent = text;
    select.append(option);
  }
  // A re-render (opening a neighbour, loading more context) keeps the
  // format the user already chose instead of snapping back to the default.
  if (selected !== null && options.some(([value]) => value === selected)) {
    select.value = selected;
  }
  return select;
}

/** The reading header's single Export… action: a native disclosure whose
 * picker offers this message (text/JSON) and the whole conversation
 * (CSV/JSON Lines/MBOX). Chat export lives here as the picker's second
 * section rather than in a separate overflow menu. */
function exportPicker(el: HTMLElement, opts: Detail2Options): HTMLDetailsElement {
  const previous = (id: string) =>
    el.querySelector<HTMLSelectElement>(`#${id}`)?.value ?? null;
  // The picker stays open across a re-render of the SAME message (a
  // background refresh must not snap it shut mid-choice); opening another
  // message starts with it closed.
  const previousCard = el.querySelector<HTMLElement>("article.detail-card");
  const wasOpen =
    previousCard?.dataset.messageId === String(opts.ctx.anchor.id) &&
    (previousCard.querySelector<HTMLDetailsElement>("details.reading-export")?.open ?? false);

  const picker = document.createElement("details");
  picker.className = "reading-export";
  picker.open = wasOpen;
  const summary = document.createElement("summary");
  summary.className = "icon-btn";
  summary.setAttribute("aria-label", "Export");
  summary.title = "Export this message or the whole conversation";
  summary.append(iconEl("export"));

  const row = (caption: string, select: HTMLSelectElement, action: HTMLButtonElement) => {
    const line = document.createElement("div");
    line.className = "export-row";
    const label = document.createElement("span");
    label.className = "meta";
    label.textContent = caption;
    line.append(label, select, action);
    return line;
  };

  const exportButton = document.createElement("button");
  exportButton.type = "button";
  exportButton.className = "export-selection-btn";
  exportButton.textContent = "Export message…";
  exportButton.title = "Export this message as text or JSON";
  exportButton.addEventListener("click", opts.onExport);

  const exportChatButton = document.createElement("button");
  exportChatButton.type = "button";
  exportChatButton.className = "export-chat-btn";
  exportChatButton.textContent = "Export chat…";
  exportChatButton.title = "Export the whole conversation thread";
  exportChatButton.addEventListener("click", opts.onExportChat);

  const body = document.createElement("div");
  body.className = "export-popover";
  body.append(
    row(
      "This message",
      formatSelect(
        "message-format",
        "Message export format",
        [
          ["text", "Text"],
          ["json", "JSON"],
        ],
        previous("message-format"),
      ),
      exportButton,
    ),
    row(
      "Conversation",
      formatSelect(
        "chat-format",
        "Chat export format",
        [
          ["csv", "CSV"],
          ["json", "JSON Lines"],
          ["mbox", "MBOX (email rows)"],
        ],
        previous("chat-format"),
      ),
      exportChatButton,
    ),
  );
  picker.append(summary, body);
  return picker;
}

/** Render the v2 reading pane: detail card + context thread + actions. */
export function renderDetail2(el: HTMLElement, opts: Detail2Options): void {
  const { ctx, entries } = opts;
  const detail = ctx.anchor;
  const terms = splitTerms(opts.queryText);

  const card = document.createElement("article");
  card.className = "detail-card";
  card.dataset.messageId = String(detail.id);

  const header = document.createElement("header");
  const title = document.createElement("h2");
  // The headline follows the rows' discipline: the display name, with
  // the full "Name <address>" form one hover away. The exact address
  // stays in the meta table below either way.
  const displayAuthor = authorDisplayName(detail.author) || "unknown";
  title.textContent = displayAuthor;
  if (displayAuthor !== (detail.author || "unknown")) title.title = detail.author;
  const actions = document.createElement("div");
  actions.className = "header-actions";
  // Utility actions are icon-only and equal in weight; Export… opens the
  // picker. "Back to results" is not here: the swap layouts
  // (medium/compact) own it in the reading toolbar, and the wide layout —
  // where the results sit beside the pane — gets a plain Close instead
  // (hidden by CSS whenever the toolbar's Back is showing).
  actions.append(
    iconButton(
      "copy",
      "copy-btn",
      "Copy message",
      "Copy the message text with provenance",
      opts.onCopy,
    ),
    iconButton(
      "link",
      "copy-link-btn",
      "Copy link",
      `Copy the internal locator (${messageLink(detail.id)})`,
      opts.onCopyLink,
    ),
    exportPicker(el, opts),
    iconButton("close", "close-btn", "Close", "Close the reading pane (Esc)", opts.onClose),
  );
  header.append(title, actions);

  const meta = document.createElement("dl");
  meta.className = "meta-table";
  const metaRow = (label: string, value: string, title?: string) => {
    const dt = document.createElement("dt");
    dt.textContent = label;
    const dd = document.createElement("dd");
    dd.textContent = value;
    if (title) dd.title = title;
    meta.append(dt, dd);
  };
  metaRow(
    "source",
    friendlySourceLabel(detail.source_label),
    // The stored label stays one hover away (the same discipline as the
    // account rows' "Stored source" title).
    detail.source_label === friendlySourceLabel(detail.source_label)
      ? undefined
      : `Stored source: ${detail.source_label}`,
  );
  metaRow("from", detail.author || "unknown");
  if (detail.subject) metaRow("subject", detail.subject);
  if (detail.conversation_label) {
    const unverified = ctx.unverified_conversation;
    const qualifier = unverified
      ? "unverified grouping"
      : conversationConfidenceLabel(detail.conversation_confidence);
    metaRow(
      "conversation",
      qualifier ? `${detail.conversation_label} (${qualifier})` : detail.conversation_label,
      unverified
        ? "The source did not confirm these messages belong to one thread — the reading pane shows the selected message only."
        : qualifier
          ? "How the archive knows these messages form one conversation."
          : undefined,
    );
  }
  const timeLabelText = timeConfidenceLabel(detail.time_confidence);
  const timeDetailText = timeConfidenceDetail(detail.time_confidence);
  metaRow(
    "date",
    detail.sent_at
      ? `${formatSentAt(detail.sent_at)} (${timeLabelText})`
      : `unknown (${timeLabelText})`,
    timeDetailText
      ? `Time certainty: ${timeLabelText} — ${timeDetailText}`
      : `Time certainty: ${timeLabelText}`,
  );
  if (detail.original_timestamp) {
    metaRow("original timestamp", detail.original_timestamp);
  }
  metaRow("message id", detail.external_id || String(detail.id));

  const body = document.createElement("div");
  body.className = "detail-body";
  const bodyHeading = document.createElement("h3");
  bodyHeading.textContent = "Message";
  const bodyText = document.createElement("div");
  bodyText.className = "body";
  const html = htmlOf(detail.raw_json);
  if (html === null) {
    appendSegments(bodyText, highlightSegments(detail.body, terms));
    body.append(bodyHeading, bodyText);
  } else {
    // Rendered/Raw toggle. The stored HTML is presentation from an
    // untrusted sender: it is only ever placed into a sandboxed
    // iframe's srcdoc (no scripts, unique origin, no navigation, and
    // a CSP that blocks every remote subresource — tracking pixels
    // included). A sandboxed frame can still navigate itself, so the
    // srcdoc also carries <base target="_blank">: every link targets
    // a new window, which the popup-less sandbox then refuses to open
    // — link clicks stay inert. Raw is today's highlighted plain text.
    const toggle = document.createElement("div");
    toggle.className = "mode-toggle";
    toggle.setAttribute("role", "group");
    toggle.setAttribute("aria-label", "Message view mode");
    const renderedBtn = document.createElement("button");
    renderedBtn.type = "button";
    renderedBtn.className = "mode-btn";
    renderedBtn.textContent = "Rendered";
    const rawBtn = document.createElement("button");
    rawBtn.type = "button";
    rawBtn.className = "mode-btn";
    rawBtn.textContent = "Raw";
    const show = (asRendered: boolean) => {
      bodyText.replaceChildren();
      body.querySelector("iframe.rendered-frame")?.remove();
      if (asRendered) {
        const frame = document.createElement("iframe");
        frame.className = "rendered-frame";
        frame.title = "Rendered message body";
        frame.setAttribute("sandbox", "");
        frame.setAttribute(
          "srcdoc",
          '<!doctype html><meta http-equiv="Content-Security-Policy" ' +
            'content="default-src \'none\'; style-src \'unsafe-inline\'; ' +
            'img-src data:"><base target="_blank">' +
            html,
        );
        // Sibling of .body, never inside it: the serialized markup of
        // .detail-card .body stays free of sender HTML entirely.
        body.append(frame);
      } else {
        appendSegments(bodyText, highlightSegments(detail.body, terms));
      }
      renderedBtn.setAttribute("aria-pressed", String(asRendered));
      rawBtn.setAttribute("aria-pressed", String(!asRendered));
    };
    renderedBtn.addEventListener("click", () => show(true));
    rawBtn.addEventListener("click", () => show(false));
    toggle.append(renderedBtn, rawBtn);
    body.append(bodyHeading, toggle, bodyText);
    show(true);
  }

  const sections: HTMLElement[] = [header, meta, body];

  if (ctx.unverified_conversation) {
    const note = document.createElement("p");
    note.className = "meta unverified-note";
    note.textContent = "Conversation could not be verified — showing the selected message only.";
    sections.push(note);
  }

  if (detail.attachments.length > 0) {
    const media = document.createElement("section");
    media.className = "detail-media";
    const heading = document.createElement("h3");
    heading.textContent = "Attachments";
    const list = document.createElement("ul");
    list.className = "media-list";
    for (const attachment of detail.attachments) {
      list.append(
        buildMediaItem(attachment, {
          onExport: opts.onExportMedia,
          onOpenExternal: opts.onOpenExternalMedia,
        }),
      );
    }
    media.append(heading, list);
    sections.push(media);
  }

  const threadSection = document.createElement("section");
  threadSection.className = "detail-thread";
  const threadHeading = document.createElement("h3");
  threadHeading.textContent = "Conversation context";
  const loadEarlier = document.createElement("button");
  loadEarlier.type = "button";
  loadEarlier.className = "load-earlier-btn";
  loadEarlier.textContent = "Load earlier";
  loadEarlier.disabled = !opts.hasMoreBefore;
  loadEarlier.addEventListener("click", opts.onLoadEarlier);
  const loadLater = document.createElement("button");
  loadLater.type = "button";
  loadLater.className = "load-later-btn";
  loadLater.textContent = "Load later";
  loadLater.disabled = !opts.hasMoreAfter;
  loadLater.addEventListener("click", opts.onLoadLater);
  const threadBar = document.createElement("div");
  threadBar.className = "row";
  threadBar.append(loadEarlier, loadLater);
  const thread = document.createElement("ol");
  thread.className = "thread";
  thread.append(
    ...entries.map((entry) => contextRow(entry, terms, opts.onOpenMessage)),
  );
  threadSection.append(threadHeading, threadBar, thread);
  sections.push(threadSection);

  const provenance = document.createElement("details");
  provenance.className = "detail-provenance";
  const provenanceSummary = document.createElement("summary");
  provenanceSummary.textContent = "Provenance";
  const provenanceBody = document.createElement("div");
  if (detail.occurrences.length > 0) {
    const occurrences = document.createElement("ul");
    occurrences.className = "provenance-list";
    for (const occurrence of detail.occurrences) {
      const item = document.createElement("li");
      const importPart = occurrence.import_id ? ` · import ${occurrence.import_id}` : "";
      item.textContent = `${occurrence.scope} · item ${occurrence.provider_item_id} · observed ${occurrence.observed_at}${importPart}`;
      occurrences.append(item);
    }
    provenanceBody.append(occurrences);
  } else {
    const none = document.createElement("p");
    none.className = "meta";
    none.textContent = "No recorded observations.";
    provenanceBody.append(none);
  }
  if (detail.revisions.length > 0) {
    const revisions = document.createElement("ul");
    revisions.className = "provenance-list";
    for (const revision of detail.revisions) {
      const item = document.createElement("li");
      const open = document.createElement("button");
      open.type = "button";
      open.textContent = `Open revision ${revision.message_id}`;
      open.addEventListener("click", () => opts.onOpenMessage(revision.message_id));
      item.append(open, ` · ${revision.identity}`);
      revisions.append(item);
    }
    provenanceBody.append(revisions);
  }
  provenance.append(provenanceSummary, provenanceBody);
  sections.push(provenance);

  const technical = document.createElement("details");
  technical.className = "detail-technical";
  const technicalSummary = document.createElement("summary");
  technicalSummary.textContent = "Technical details";
  const technicalBody = document.createElement("pre");
  technicalBody.className = "raw-json";
  try {
    technicalBody.textContent = JSON.stringify(JSON.parse(detail.raw_json), null, 2);
  } catch {
    technicalBody.textContent = detail.raw_json;
  }
  technical.append(technicalSummary, technicalBody);
  sections.push(technical);

  card.append(...sections);
  el.replaceChildren(card);
}

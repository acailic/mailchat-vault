/// DOM rendering for the v2 search UI (M3.2): flat results rows, era
/// chips, applied filter chips, the v2 reading pane (detail +
/// chronological context), pagination, source chips, and the statistics
/// dashboard.
/// Pure builders — every dynamic string goes through textContent, tested
/// via happy-dom in render2.test.ts.

import type { SourceChipRow } from "./accountsview";
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
  type ContextEntry,
  type EraBucket,
  type FilterChip,
  foldCount,
  messageLink,
  similarCopiesLabel,
} from "./searchstate";
import type { ArchiveStats } from "./types";
import {
  type DayBar,
  type Segment,
  barHeightPct,
  dashboardSummary,
  dayBars,
  formatCount,
  highlightSegments,
  htmlOf,
  parseSnippet,
  splitTerms,
} from "./viewmodel";
import { buildMediaChips, buildMediaItem } from "./media";
import type {
  AttachmentSummary,
  ConversationContext,
  ImportScope,
  MessageSummary,
} from "./v2types";

// ---------------------------------------------------------------------------
// Highlight segments
// ---------------------------------------------------------------------------

/** Append segments as text nodes plus `<mark class="hit">` runs. */
export function appendSegments(el: HTMLElement, segments: Segment[]): void {
  for (const seg of segments) {
    if (seg.hit) {
      const mark = document.createElement("mark");
      mark.className = "hit";
      mark.textContent = seg.text;
      el.append(mark);
    } else {
      el.append(document.createTextNode(seg.text));
    }
  }
}

// ---------------------------------------------------------------------------
// Flat results
// ---------------------------------------------------------------------------

/** The two results-list density modes (persisted choice, see main.ts). */
export type DensityMode = "comfortable" | "compact";

/** DOM-row ceiling: renderFlatResults never renders more rows than this
 * (sentinel loading appends pages up to it, then an explicit Load more
 * control takes over — the list stays scroll-smooth without unbounded
 * DOM growth). */
export const MAX_RENDERED_ROWS = 200;

export interface FlatResultsOptions {
  hits: MessageSummary[];
  /** Keyboard-active row (-1 = none). */
  activeIndex: number;
  /** Row the user opened (Back restored it); -1 = none. */
  selectedId: number | null;
  /** Ids checked for selection export. */
  selection: Set<number>;
  /** Representative rows whose folded copies are expanded inline. */
  expandedFolds: Set<number>;
  /** Query terms for client-side highlighting in browse mode. */
  terms: string[];
  /** Row density applied to the list container (data-density). */
  density?: DensityMode;
  /** Rendered-row ceiling (defaults to MAX_RENDERED_ROWS). */
  renderCap?: number;
  onOpen: (id: number) => void;
  onToggleSelect: (id: number) => void;
  onToggleFold: (id: number) => void;
}

/** The time-uncertainty marker shown instead of a fabricated timestamp. */
export function timeLabel(hit: Pick<MessageSummary, "sent_at" | "time_confidence">): string {
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

  // Grid semantics for keyboard-first navigation: the list is the grid,
  // each row an aria-row carrying the selection state. The activation
  // discipline is unchanged — role=button + tabindex stay on the main
  // area below, never on the row itself, and the checkbox stays its
  // sibling (no control nested inside a control).
  li.setAttribute("role", "row");
  li.setAttribute("aria-selected", at === opts.activeIndex ? "true" : "false");

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
  // element that contains only text. The li stays a plain grid row
  // (class/data/aria-current remain on it for styling and keyboard nav).
  // mv-press gives the shared press feedback on the motion tokens.
  const main = document.createElement("div");
  main.className = "hit-main mv-press";
  main.setAttribute("role", "button");
  main.setAttribute("tabindex", "0");
  main.title = "Open this message";
  const meta = document.createElement("div");
  meta.className = "meta hit-heading";
  const displayAuthor = authorDisplayName(hit.author) || "unknown";
  const author = document.createElement("span");
  author.className = "hit-author";
  author.textContent = displayAuthor;
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
  // The author run is marked like the body when an author filter (or a
  // from:/to: operator) is in effect — the row shows why it matched
  // (text content is unchanged; only <mark> runs differ): replace the
  // plain-text author span content with the marked segments.
  author.textContent = "";
  appendSegments(author, highlightSegments(displayAuthor, opts.terms));
  meta.append(author, " ", time);
  // Provenance gets its own subdued line under the author/time row
  // (UI professionalization: scanning weight off the primary meta).
  const provenance = document.createElement("div");
  provenance.className = "meta hit-provenance";
  const source = document.createElement("span");
  source.textContent = friendlySourceLabel(hit.source_label);
  provenance.append(source);
  if (hit.conversation_label) {
    const conversation = document.createElement("span");
    conversation.className = "hit-conversation";
    conversation.textContent = hit.conversation_label;
    provenance.append(" ", conversation);
  }
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
    provenance.append(" ", files);
  }
  if (hit.has_conflicting_revision) {
    const conflict = document.createElement("span");
    conflict.className = "badge conflict";
    conflict.title = "Another revision of this message exists in the archive";
    conflict.textContent = "revisions";
    provenance.append(" ", conflict);
  }

  const body = document.createElement("div");
  body.className = "body";
  if (hit.snippet) {
    appendSegments(body, parseSnippet(hit.snippet));
  } else if (hit.subject) {
    // No backend snippet: the subject is the preview line, and it gets
    // the same bounded client-side highlighting the detail pane uses.
    appendSegments(body, highlightSegments(hit.subject, opts.terms));
  } else {
    // Browse without a subject: nothing bounded to show — the metadata
    // row is the honest summary (bodies are a detail-view payload).
    body.textContent = "";
  }

  main.append(meta, body, provenance);
  li.append(check, main);
  // Near-duplicate fold (NEWWO-101): a row that folds copies carries an
  // expander under the main area — a sibling control like the selection
  // checkbox, never nested inside the row's role=button. A count of 0
  // (or an absent count) renders nothing: the row stays exactly as it
  // was before the fold existed.
  const copies = foldCount(hit);
  if (copies > 0) {
    const expanded = opts.expandedFolds.has(hit.id);
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "fold-toggle";
    toggle.setAttribute("aria-expanded", String(expanded));
    toggle.setAttribute("aria-controls", `fold-members-${hit.id}`);
    toggle.textContent = similarCopiesLabel(copies);
    toggle.title = expanded
      ? `Hide the ${similarCopiesLabel(copies)} folded into this row`
      : `Show the ${similarCopiesLabel(copies)} folded into this row`;
    toggle.addEventListener("click", (event) => {
      event.stopPropagation();
      opts.onToggleFold(hit.id);
    });
    li.append(toggle);
    if (expanded) {
      // The payload carries member ids only, so each copy is labeled by
      // its message id and opens through the same path as any other hit.
      const members = document.createElement("ul");
      members.className = "fold-members";
      members.id = `fold-members-${hit.id}`;
      for (const memberId of hit.member_ids ?? []) {
        const item = document.createElement("li");
        item.className = "fold-member";
        const open = document.createElement("button");
        open.type = "button";
        open.className = "fold-member-open";
        open.textContent = `Open copy ${memberId}`;
        open.addEventListener("click", (event) => {
          event.stopPropagation();
          opts.onOpen(memberId);
        });
        item.append(open);
        members.append(item);
      }
      li.append(members);
    }
  }
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

/** Render the flat, stable results list (no conversation regrouping).
 * The container becomes an ARIA grid (rows carry aria-selected), wears
 * the density mode as data-density, and never renders more than
 * `renderCap` (default MAX_RENDERED_ROWS) DOM rows — appended pages
 * beyond the cap wait for the explicit Load more control. */
export function renderFlatResults(el: HTMLElement, opts: FlatResultsOptions): void {
  el.setAttribute("role", "grid");
  el.dataset.density = opts.density ?? "comfortable";
  const cap = opts.renderCap ?? MAX_RENDERED_ROWS;
  const hits = opts.hits.length > cap ? opts.hits.slice(0, cap) : opts.hits;
  el.replaceChildren(...hits.map((hit, at) => resultRow(hit, opts, at)));
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
 * survives re-renders — and resets whenever the timeline no longer
 * hides buckets, so a shrink-then-regrow starts collapsed instead of
 * silently expanded). The active era never hides behind the cap —
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
  if (hidden === 0) {
    // Nothing to hide (at or below the cap): a stale expanded flag
    // renders identically here, but left alone it would resurface as
    // expanded the moment the timeline regrows past the cap. Reset it
    // now so the regrow starts collapsed like a first render.
    delete el.dataset.expanded;
  }
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
  // Neighbors show truncated text, the current row the full body; both
  // mark the active query terms with the same bounded highlight.
  appendSegments(
    body,
    highlightSegments(
      entry.current ? entry.body : truncateBody(entry.body, 140),
      terms,
    ),
  );
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

/** The technical meta table (dl) — every stored field with its raw
 * form one hover away. It renders inside the Provenance fold: the
 * reading surface above shows the editorial hierarchy, this table is
 * the reference copy. */
function metaTable(detail: ConversationContext["anchor"], ctx: ConversationContext): HTMLDListElement {
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
  return meta;
}

/** The reading surface's quiet meta row: date · source · conversation,
 * small muted text with tabular numerals. Uncertain times keep their
 * caveat and tooltip (A04 honesty — the same wording as the rows). */
function metaHierarchyRow(detail: ConversationContext["anchor"]): HTMLDivElement {
  const row = document.createElement("div");
  row.className = "detail-meta";
  const part = (text: string, title?: string) => {
    const span = document.createElement("span");
    span.textContent = text;
    if (title) span.title = title;
    return span;
  };
  const date = part(timeLabel(detail));
  if (
    detail.time_confidence === "legacy_assumed" ||
    detail.time_confidence === "unresolved"
  ) {
    date.title = timeConfidenceDetail(detail.time_confidence);
  }
  const source = part(friendlySourceLabel(detail.source_label));
  if (detail.source_label !== friendlySourceLabel(detail.source_label)) {
    source.title = `Stored source: ${detail.source_label}`;
  }
  row.append(date, " · ", source);
  if (detail.conversation_label) {
    row.append(" · ", part(detail.conversation_label));
  }
  return row;
}

/** Render the v2 reading pane: detail card + context thread + actions. */
export function renderDetail2(el: HTMLElement, opts: Detail2Options): void {
  const { ctx, entries } = opts;
  const detail = ctx.anchor;
  const terms = splitTerms(opts.queryText);

  // The attachment chips' expansion rides a same-message re-render (the
  // era-chip discipline: state on the element, never a measurement).
  const previousCard = el.querySelector<HTMLElement>("article.detail-card");
  const carryChipsExpanded =
    previousCard?.dataset.messageId === String(detail.id) &&
    (previousCard.querySelector<HTMLElement>("ul.media-chips")?.dataset.expanded ?? "false") ===
      "true";

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

  // Editorial meta hierarchy: the subject reads as a display line under
  // the participant headline; date · source · conversation sit in one
  // quiet muted row (metaHierarchyRow) above the body — the full
  // technical dl moves into the Provenance fold.
  const subject = document.createElement("p");
  subject.className = "detail-subject";
  subject.textContent = detail.subject;
  const meta = metaHierarchyRow(detail);

  const body = document.createElement("div");
  body.className = "detail-body";
  const bodyText = document.createElement("div");
  bodyText.className = "body";
  const html = htmlOf(detail.raw_json);
  if (html === null) {
    appendSegments(bodyText, highlightSegments(detail.body, terms));
    body.append(bodyText);
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
    body.append(toggle, bodyText);
    show(true);
  }

  const sections: HTMLElement[] = [header];
  if (detail.subject) sections.push(subject);
  sections.push(meta, body);

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
    // The compact chip row: one line, fixed visible count, "+N more"
    // expander. The full items (inline previews, explicit Save/Open)
    // stay below — one action per chip, the rest one click further.
    const chips = buildMediaChips(
      detail.attachments,
      {
        onExport: opts.onExportMedia,
        onOpenExternal: opts.onOpenExternalMedia,
      },
      carryChipsExpanded,
    );
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
    media.append(heading, chips, list);
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
  // The full meta table (stored forms, certainty wording, ids) lives
  // here — the reading surface above keeps only the editorial row.
  provenanceBody.append(metaTable(detail, ctx));
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

// ---------------------------------------------------------------------------
// Results footer: count line, density toggle, sentinel / load-more
// ---------------------------------------------------------------------------

/** The custom DOM event that activates the load sentinel — the test and
 * no-observer seam (the real app relies on IntersectionObserver, wired
 * by main.ts on the element this renderer produces). */
export const SENTINEL_LOAD_EVENT = "mcv:load-more";

export interface PaginationOptions {
  /** Matching hits accumulated so far (all loaded pages). */
  loaded: number;
  /** Backend total for the current query. */
  total: number;
  density: DensityMode;
  /** Rendered-row ceiling: at/beyond it auto-loading stops and the
   * explicit Load more control takes over. */
  renderCap: number;
  onLoadMore: () => void;
  onToggleDensity: () => void;
}

/**
 * The results footer: the "1–N of T" count line (tabular numerals via
 * app.css), the persisted Comfortable/Compact density toggle, and the
 * list's growth affordance — a quiet sentinel element the moment more
 * pages exist below the cap, an explicit Load more button once the cap
 * is reached. No prev/next paging: the list only ever grows downward.
 */
export function renderPagination(el: HTMLElement, opts: PaginationOptions): void {
  const info = document.createElement("span");
  info.className = "page-info";
  info.textContent =
    opts.total === 0 ? "0 results" : `1–${Math.min(opts.loaded, opts.total)} of ${opts.total}`;

  const density = document.createElement("button");
  density.type = "button";
  density.className = "density-toggle";
  density.textContent = "Compact";
  density.setAttribute("aria-pressed", opts.density === "compact" ? "true" : "false");
  density.title = "Compact result rows (remembered on this machine)";
  density.addEventListener("click", opts.onToggleDensity);

  const more = opts.total > opts.loaded;
  if (more && opts.loaded < opts.renderCap) {
    // Sentinel: an inert element to observe, never scroll-jacking chrome.
    const sentinel = document.createElement("div");
    sentinel.className = "load-sentinel";
    sentinel.dataset.loading = "1";
    sentinel.setAttribute("aria-hidden", "true");
    el.replaceChildren(info, density, sentinel);
    return;
  }
  if (more) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "page-btn load-more-btn";
    button.textContent = "Load more";
    button.title = `Show the next page (the list pauses at ${opts.renderCap} rows to stay smooth)`;
    button.addEventListener("click", opts.onLoadMore);
    el.replaceChildren(info, density, button);
    return;
  }
  el.replaceChildren(info, density);
}

// ---------------------------------------------------------------------------
// Source chips
// ---------------------------------------------------------------------------

export interface SourceChipsOptions {
  rows: SourceChipRow[];
  selected: string[];
  onToggle: (id: string) => void;
}

/** Toggle chips for the source filter (none selected = all sources).
 * Selection keys are stable source-instance ids. Each chip leads with
 * its source's taxonomy icon (mail/chat/file — decorative, aria-hidden;
 * the label stays the accessible text) and, when the row carries one,
 * appends the source's short status span after the label — the label
 * and count text themselves stay intact (the e2e specs assert them by
 * substring), the status adds to the chip's accessible name. */
export function renderSourceChips(el: HTMLElement, opts: SourceChipsOptions): void {
  const chips = opts.rows.map((row) => {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "chip" + (opts.selected.includes(row.id) ? " on" : "");
    chip.dataset.source = row.id;
    chip.setAttribute("aria-pressed", opts.selected.includes(row.id) ? "true" : "false");
    const label = document.createElement("span");
    label.textContent =
      row.count === null ? row.label : `${row.label} (${row.count.toLocaleString()})`;
    chip.append(iconEl(row.icon), label);
    if (row.status) {
      const status = document.createElement("span");
      status.className = "chip-status";
      status.textContent = row.status;
      chip.append(status);
    }
    chip.addEventListener("click", () => opts.onToggle(row.id));
    return chip;
  });
  el.replaceChildren(...chips);
}

// ---------------------------------------------------------------------------
// Statistics dashboard
// ---------------------------------------------------------------------------

/** One row of a count table: a label, its count, and an inline bar. */
interface CountRow {
  label: string;
  /** The untruncated value when `label` is a shortened form (full author
   *  strings); the tooltip defaults to the label. */
  title?: string;
  count: number;
}

/** One bar's label, for the tooltip, the screen reader, and the single
 * bucket's plain-text line alike: "2026-11-01: 6 messages". */
function dayBarLabel(bar: DayBar): string {
  return `${bar.label}: ${formatCount(bar.count)} ${bar.count === 1 ? "message" : "messages"}`;
}

/** Bars wide enough to carry a visible count label; beyond this the
 * labels would collide, and each bar keeps its title/aria-label. */
const DAY_BAR_LABEL_MAX = 12;

function dayChart(days: ArchiveStats["by_day"]): HTMLElement {
  const block = document.createElement("div");
  block.className = "day-chart-block";
  const chart = document.createElement("div");
  chart.className = "day-chart";
  chart.setAttribute("role", "group");
  const bars = dayBars(days, 180);
  if (bars.length === 1) {
    // A single bucket has no distribution to show — one full-width bar
    // would just dress the total up as a chart. Say the count plainly.
    chart.classList.add("single");
    const text = dayBarLabel(bars[0]);
    chart.setAttribute("aria-label", `Messages per day — ${text}`);
    const note = document.createElement("span");
    note.className = "day-single";
    note.textContent = text;
    chart.append(note);
    return chart;
  }
  chart.setAttribute("aria-label", "Messages per day");
  const max = Math.max(0, ...bars.map((b) => b.count));
  const withCounts = bars.length <= DAY_BAR_LABEL_MAX;
  for (const bar of bars) {
    const col = document.createElement("div");
    col.className = "day-bar";
    col.style.height = `${barHeightPct(bar.count, max)}%`;
    // Heat share of the tallest day (0..100) drives the color ramp;
    // engines without color-mix keep the solid accent fallback.
    col.style.setProperty("--bar-heat", String(barHeightPct(bar.count, max)));
    const text = dayBarLabel(bar);
    col.title = text;
    col.setAttribute("role", "img");
    col.setAttribute("aria-label", text);
    if (withCounts) {
      const count = document.createElement("span");
      count.className = "day-bar-count";
      count.textContent = formatCount(bar.count);
      col.append(count);
    }
    chart.append(col);
  }
  block.append(chart);
  if (bars.length > 0) block.append(dayChartAxis(bars));
  return block;
}

/** Time axis under the histogram: the first, middle, and last bucket
 * labels (a day, month, or year, per the bucketing in dayBars). */
function dayChartAxis(bars: DayBar[]): HTMLElement {
  const axis = document.createElement("div");
  axis.className = "day-chart-axis";
  const ticks = [
    bars[0].label,
    bars[Math.floor((bars.length - 1) / 2)].label,
    bars[bars.length - 1].label,
  ];
  axis.append(
    ...[...new Set(ticks)].map((label) => {
      const tick = document.createElement("span");
      tick.textContent = label;
      return tick;
    }),
  );
  return axis;
}

function countTable(caption: string, rows: CountRow[]): HTMLElement {
  const card = document.createElement("div");
  card.className = "stat-table";
  const heading = document.createElement("h3");
  heading.textContent = caption;
  const table = document.createElement("table");
  const max = Math.max(0, ...rows.map((r) => r.count));
  const body = document.createElement("tbody");
  for (const row of rows) {
    const tr = document.createElement("tr");
    const label = document.createElement("td");
    label.className = "label";
    label.textContent = row.label;
    label.title = row.title ?? row.label;
    const num = document.createElement("td");
    num.className = "num";
    num.textContent = formatCount(row.count);
    const bar = document.createElement("td");
    bar.className = "bar";
    const fill = document.createElement("div");
    fill.style.width = `${barHeightPct(row.count, max)}%`;
    bar.append(fill);
    tr.append(label, num, bar);
    body.append(tr);
  }
  table.append(body);
  card.append(heading, table);
  return card;
}

/** Top-author table rows with display-identical authors merged: header
 *  spellings of one identity ('"Ana" <a@b.c>' and 'Ana <a@b.c>') become
 *  one row with the summed count, re-sorted descending. The merged
 *  row's title joins the distinct full identities ("; ") so the
 *  addresses stay one hover away. Merging happens after the backend's
 *  LIMIT, so a variant below the top-N cutoff can be under-counted —
 *  a deliberate display-side trade, not a stats change. */
function topAuthorRows(authors: ArchiveStats["top_authors"]): CountRow[] {
  const byLabel = new Map<string, { count: number; identities: string[] }>();
  for (const a of authors) {
    const label = authorDisplayName(a.author) || "(unknown)";
    const identity = a.author || "(unknown)";
    const merged = byLabel.get(label) ?? { count: 0, identities: [] };
    merged.count += a.count;
    if (!merged.identities.includes(identity)) merged.identities.push(identity);
    byLabel.set(label, merged);
  }
  return [...byLabel]
    .map(([label, m]) => ({ label, title: m.identities.join("; "), count: m.count }))
    .sort((a, b) => b.count - a.count);
}

/** True when a Top chats source is mail: an mbox import or a live IMAP
 *  account (labels "mbox" and "imap:host:port:user"). Mail folder names
 *  come from the provider (Gmail's "All mail Including Spam and Trash"),
 *  not from the user, so the dashboard normalizes their casing; chat
 *  names are user-chosen proper nouns and stay verbatim. */
function isMailSource(source: string): boolean {
  return source === "mbox" || source.startsWith("imap:");
}

/** Sentence-case a mail folder label: first word capitalized, the rest
 *  lowercased ("All mail Including Spam and Trash" → "All mail
 *  including spam and trash"). The raw value stays in the tooltip. */
function sentenceCaseFolder(name: string): string {
  const lowered = name.toLowerCase();
  return lowered.charAt(0).toUpperCase() + lowered.slice(1);
}

export interface DashboardOptions {
  stats: ArchiveStats;
}

/**
 * Render the statistics dashboard: a summary line, the per-day histogram
 * (CSS bars, no chart library), and count tables for sources, busiest
 * folders, and top authors. An empty archive renders the summary only.
 */
export function renderDashboard(el: HTMLElement, opts: DashboardOptions): void {
  const { stats } = opts;
  const summary = document.createElement("p");
  summary.className = "meta";
  summary.textContent = dashboardSummary(stats);
  if (stats.total === 0) {
    el.replaceChildren(summary);
    return;
  }
  const grid = document.createElement("div");
  grid.className = "stat-grid";
  grid.append(
    countTable(
      "By source",
      stats.by_source.map((s) => ({
        label: friendlySourceLabel(s.source),
        title: s.source,
        count: s.count,
      })),
    ),
    countTable(
      "Busiest folders",
      stats.top_chats.map((c) => ({
        // Mail folder names arrive in the provider's casing; chat names
        // are user-chosen and render verbatim. The source reads in plain
        // words (friendlySourceLabel); the raw label stays in the tooltip.
        label: `${friendlySourceLabel(c.source)} · ${isMailSource(c.source) ? sentenceCaseFolder(c.chat_id) : c.chat_id}`,
        title: `${c.source} · ${c.chat_id}`,
        count: c.count,
      })),
    ),
    countTable("Top authors", topAuthorRows(stats.top_authors)),
  );
  el.replaceChildren(summary, dayChart(stats.by_day), grid);
}

/// DOM rendering for the search UI. Every dynamic string goes through
/// textContent (no innerHTML with user data), so message content can never
/// inject markup. The functions are pure builders — tested via happy-dom
/// snapshots in render.test.ts.

import { formatSentAt, snippet as truncateBody } from "./format";
import type { ChatContext, Message, SearchHit, SourceCount } from "./types";
import {
  type HitGroup,
  type PageModel,
  type Segment,
  type ThreadEntry,
  highlightSegments,
  parseSnippet,
  splitTerms,
  threadEntries,
} from "./viewmodel";

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

export interface ResultsOptions {
  groups: HitGroup[];
  activeIndex: number;
  /** Query terms for client-side highlighting when a hit has no snippet. */
  terms: string[];
  onOpen: (id: number) => void;
}

function hitBody(hit: SearchHit, terms: string[]): HTMLElement {
  const body = document.createElement("div");
  body.className = "body";
  if (hit.snippet !== null) {
    // FTS snippet with backend-planted match markers.
    appendSegments(body, parseSnippet(hit.snippet));
  } else {
    // Browse mode (no text query): plain truncated body.
    appendSegments(body, highlightSegments(truncateBody(hit.message.body), terms));
  }
  return body;
}

/** Render grouped, highlighted results into `list` (replaces children). */
export function renderResults(list: HTMLElement, opts: ResultsOptions): void {
  const { groups, activeIndex, terms, onOpen } = opts;
  const children: HTMLElement[] = [];
  let flat = 0;
  for (const group of groups) {
    const section = document.createElement("section");
    section.className = "group";
    const header = document.createElement("h3");
    header.textContent = `${group.label} (${group.hits.length})`;
    const list = document.createElement("ul");
    list.className = "group-hits";
    section.append(header, list);
    for (const hit of group.hits) {
      const index = flat++;
      const m = hit.message;
      const item = document.createElement("li");
      item.className = "hit-item" + (index === activeIndex ? " active" : "");
      item.dataset.index = String(index);
      item.dataset.id = String(m.id);
      if (index === activeIndex) item.setAttribute("aria-current", "true");
      const meta = document.createElement("div");
      meta.className = "meta";
      meta.textContent = `${m.author || "unknown"} · ${formatSentAt(m.sent_at)} · ${m.channel}`;
      item.append(meta, hitBody(hit, terms));
      item.addEventListener("click", () => onOpen(m.id));
      list.append(item);
    }
    children.push(section);
  }
  list.replaceChildren(...children);
}

export interface PaginationHandlers {
  onPrev: () => void;
  onNext: () => void;
}

/** Render the "1–50 of 1,234" line plus prev/next controls. */
export function renderPagination(
  el: HTMLElement,
  model: PageModel,
  handlers: PaginationHandlers,
): void {
  const info = document.createElement("span");
  info.className = "page-info";
  info.textContent =
    model.total === 0
      ? "0 results"
      : `${model.from}–${model.to} of ${model.total} · page ${model.page}/${model.pageCount}`;
  const prev = document.createElement("button");
  prev.type = "button";
  prev.className = "page-btn";
  prev.textContent = "‹ Prev";
  prev.disabled = !model.hasPrev;
  const next = document.createElement("button");
  next.type = "button";
  next.className = "page-btn";
  next.textContent = "Next ›";
  next.disabled = !model.hasNext;
  prev.addEventListener("click", handlers.onPrev);
  next.addEventListener("click", handlers.onNext);
  el.replaceChildren(prev, info, next);
}

export interface SourceChipsOptions {
  available: SourceCount[];
  selected: string[];
  onToggle: (source: string) => void;
}

/** Toggle chips for the source filter (none selected = all sources). */
export function renderSourceChips(el: HTMLElement, opts: SourceChipsOptions): void {
  const chips = opts.available.map((sc) => {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "chip" + (opts.selected.includes(sc.source) ? " on" : "");
    chip.dataset.source = sc.source;
    chip.setAttribute("aria-pressed", opts.selected.includes(sc.source) ? "true" : "false");
    chip.textContent = `${sc.source} (${sc.count})`;
    chip.addEventListener("click", () => opts.onToggle(sc.source));
    return chip;
  });
  el.replaceChildren(...chips);
}

function threadRow(
  entry: ThreadEntry,
  terms: string[],
  onOpen: (id: number) => void,
): HTMLElement {
  const m = entry.message;
  const row = document.createElement("li");
  row.className = "thread-row" + (entry.current ? " current" : "");
  row.dataset.id = String(m.id);
  const meta = document.createElement("div");
  meta.className = "meta";
  meta.textContent = `${m.author || "unknown"} · ${formatSentAt(m.sent_at)}`;
  const body = document.createElement("div");
  body.className = "body";
  if (entry.current) {
    // Full body with whole-word highlights of the query terms.
    appendSegments(body, highlightSegments(m.body, terms));
  } else {
    body.textContent = truncateBody(m.body, 140);
  }
  row.append(meta, body);
  if (!entry.current) {
    row.classList.add("link");
    row.setAttribute("role", "button");
    row.setAttribute("tabindex", "0");
    row.title = "Open this message";
    row.addEventListener("click", () => onOpen(m.id));
  }
  return row;
}

function metaTable(m: Message): HTMLElement {
  const table = document.createElement("dl");
  table.className = "meta-table";
  const rows: [string, string][] = [
    ["author", m.author || "unknown"],
    ["sent", m.sent_at || "unknown"],
    ["source", m.source],
    ["channel", m.channel || "—"],
    ["chat", m.chat_id || "—"],
    ["external id", m.external_id],
  ];
  for (const [k, v] of rows) {
    const dt = document.createElement("dt");
    dt.textContent = k;
    const dd = document.createElement("dd");
    dd.textContent = v;
    table.append(dt, dd);
  }
  return table;
}

export interface DetailOptions {
  ctx: ChatContext;
  queryText: string;
  onOpenMessage: (id: number) => void;
  onClose: () => void;
}

/** Render the message detail panel: full body, meta, thread neighbors. */
export function renderDetail(el: HTMLElement, opts: DetailOptions): void {
  const { ctx, onOpenMessage, onClose } = opts;
  const terms = splitTerms(opts.queryText);
  const article = document.createElement("article");
  article.className = "detail-card";

  const header = document.createElement("header");
  const title = document.createElement("h2");
  title.textContent = ctx.message.author || "unknown author";
  const close = document.createElement("button");
  close.type = "button";
  close.className = "close-btn";
  close.textContent = "✕ Close (Esc)";
  close.addEventListener("click", onClose);
  header.append(title, close);

  const thread = document.createElement("ol");
  thread.className = "thread";
  thread.setAttribute("aria-label", "Chat thread");
  thread.replaceChildren(
    ...threadEntries(ctx).map((entry) => threadRow(entry, terms, onOpenMessage)),
  );

  const hint = document.createElement("p");
  hint.className = "meta";
  hint.textContent = "↑/↓ move through the thread · Esc closes";

  article.append(header, metaTable(ctx.message), thread, hint);
  el.replaceChildren(article);
}

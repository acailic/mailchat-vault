/// DOM rendering for the search UI. Every dynamic string goes through
/// textContent (no innerHTML with user data), so message content can never
/// inject markup. The functions are pure builders — tested via happy-dom
/// snapshots in render.test.ts.

import { formatSentAt, humanSize, snippet as truncateBody } from "./format";
import type {
  ArchiveStats,
  ChatContext,
  Message,
  SearchHit,
  SourceCount,
} from "./types";
import {
  type HitGroup,
  type MessageMedia,
  type PageModel,
  type Segment,
  type ThreadEntry,
  attachmentOf,
  barHeightPct,
  dayBars,
  dashboardSummary,
  formatCount,
  highlightSegments,
  mediaOf,
  parseSnippet,
  splitTerms,
  threadEntries,
} from "./viewmodel";

/** Append segments as text nodes plus `<mark class="hit">` runs. */
function appendSegments(el: HTMLElement, segments: Segment[]): void {
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
  onOpen: (index: number, id: number) => void;
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
      item.setAttribute("tabindex", "0");
      if (index === activeIndex) item.setAttribute("aria-current", "true");
      const meta = document.createElement("div");
      meta.className = "meta";
      meta.textContent = `${m.author || "unknown"} · ${formatSentAt(m.sent_at)} · ${m.channel}`;
      item.append(meta, hitBody(hit, terms));
      const open = () => onOpen(index, m.id);
      item.addEventListener("click", open);
      item.addEventListener("keydown", (event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          event.stopPropagation();
          open();
        }
      });
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
    const open = () => onOpen(m.id);
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

function mediaRow(entry: MessageMedia): HTMLElement {
  const li = document.createElement("li");
  li.className = "media-item";
  const badge = document.createElement("span");
  badge.className = "media-kind";
  badge.textContent = entry.kind;
  const name = document.createElement("span");
  name.className = "media-name";
  name.textContent = entry.name;
  const meta = document.createElement("span");
  meta.className = "meta";
  // The stored path is the key back into the vault's media store.
  meta.title = entry.path;
  meta.textContent = `${humanSize(entry.size)} · ${entry.path}`;
  li.append(badge, name, meta);
  return li;
}

/** The media section of the detail card: extracted files, plus attachment
 * placeholders that found no file in the export. */
function mediaSection(m: Message): HTMLElement | null {
  const media = mediaOf(m.raw_json);
  const attachment = attachmentOf(m.raw_json);
  const unresolved = attachment?.name && !media.some((e) => e.name === attachment.name)
    ? attachment
    : null;
  if (media.length === 0 && !unresolved) return null;

  const section = document.createElement("section");
  section.className = "detail-media";
  const heading = document.createElement("h3");
  heading.textContent = "media";
  const list = document.createElement("ul");
  list.className = "media-list";
  list.append(...media.map(mediaRow));
  if (unresolved) {
    const li = document.createElement("li");
    li.className = "media-item unresolved";
    const badge = document.createElement("span");
    badge.className = "media-kind";
    badge.textContent = unresolved.kind;
    const name = document.createElement("span");
    name.className = "media-name";
    name.textContent = unresolved.name ?? "";
    const note = document.createElement("span");
    note.className = "meta";
    note.textContent = "not in export";
    li.append(badge, name, note);
    list.append(li);
  }
  section.append(heading, list);
  return section;
}

export interface DetailOptions {
  ctx: ChatContext;
  queryText: string;
  onOpenMessage: (id: number) => void;
  /** Export the whole chat thread of the opened message. */
  onExportChat: () => void;
  onClose: () => void;
}

/** Render the message detail panel: full body, meta, thread neighbors. */
export function renderDetail(el: HTMLElement, opts: DetailOptions): void {
  const { ctx, onOpenMessage, onExportChat, onClose } = opts;
  const terms = splitTerms(opts.queryText);
  const article = document.createElement("article");
  article.className = "detail-card";

  const header = document.createElement("header");
  const title = document.createElement("h2");
  title.textContent = ctx.message.author || "unknown author";
  const actions = document.createElement("div");
  actions.className = "header-actions";
  const exportChat = document.createElement("button");
  exportChat.type = "button";
  exportChat.className = "export-chat-btn";
  exportChat.textContent = "⤓ Export chat…";
  exportChat.title = "Export the whole chat thread of this message";
  exportChat.addEventListener("click", onExportChat);
  const close = document.createElement("button");
  close.type = "button";
  close.className = "close-btn";
  close.textContent = "✕ Close (Esc)";
  close.addEventListener("click", onClose);
  actions.append(exportChat, close);
  header.append(title, actions);

  const thread = document.createElement("ol");
  thread.className = "thread";
  thread.setAttribute("aria-label", "Chat thread");
  thread.replaceChildren(
    ...threadEntries(ctx).map((entry) => threadRow(entry, terms, onOpenMessage)),
  );

  const hint = document.createElement("p");
  hint.className = "meta";
  hint.textContent = "↑/↓ move through the thread · Esc closes";

  article.append(header, metaTable(ctx.message));
  const media = mediaSection(ctx.message);
  if (media) article.append(media);
  article.append(thread, hint);
  el.replaceChildren(article);
}

// ---------------------------------------------------------------------------
// Statistics dashboard
// ---------------------------------------------------------------------------

/** One row of a count table: a label, its count, and an inline bar. */
interface CountRow {
  label: string;
  count: number;
}

function dayChart(days: ArchiveStats["by_day"]): HTMLElement {
  const chart = document.createElement("div");
  chart.className = "day-chart";
  chart.setAttribute("role", "img");
  chart.setAttribute("aria-label", "Messages per day histogram");
  const bars = dayBars(days, 180);
  const max = Math.max(0, ...bars.map((b) => b.count));
  for (const bar of bars) {
    const col = document.createElement("div");
    col.className = "day-bar";
    col.style.height = `${barHeightPct(bar.count, max)}%`;
    col.title = `${bar.label}: ${formatCount(bar.count)}`;
    chart.append(col);
  }
  return chart;
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
    label.title = row.label;
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

export interface DashboardOptions {
  stats: ArchiveStats;
}

/**
 * Render the statistics dashboard: a summary line, the per-day histogram
 * (CSS bars, no chart library), and count tables for sources, top chats,
 * and top authors. An empty archive renders the summary only.
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
      stats.by_source.map((s) => ({ label: s.source, count: s.count })),
    ),
    countTable(
      "Top chats",
      stats.top_chats.map((c) => ({
        label: `${c.source} · ${c.chat_id}`,
        count: c.count,
      })),
    ),
    countTable(
      "Top authors",
      stats.top_authors.map((a) => ({
        label: a.author || "(unknown)",
        count: a.count,
      })),
    ),
  );
  el.replaceChildren(summary, dayChart(stats.by_day), grid);
}

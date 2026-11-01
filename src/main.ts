import { invoke } from "@tauri-apps/api/core";
import {
  renderDetail,
  renderPagination,
  renderResults,
  renderSourceChips,
} from "./render";
import type {
  ChatContext,
  IngestSummary,
  SearchHit,
  SearchPage,
  Stats,
} from "./types";
import { groupHits, moveIndex, neighborId, pageModel, splitTerms } from "./viewmodel";

const PAGE_SIZE = 50;

// ---------------------------------------------------------------------------
// Elements
// ---------------------------------------------------------------------------

const searchInput = document.querySelector<HTMLInputElement>("#search")!;
const searchButton = document.querySelector<HTMLButtonElement>("#search-btn")!;
const sourceChips = document.querySelector<HTMLElement>("#source-chips")!;
const authorInput = document.querySelector<HTMLInputElement>("#author")!;
const fromDateInput = document.querySelector<HTMLInputElement>("#from-date")!;
const toDateInput = document.querySelector<HTMLInputElement>("#to-date")!;
const clearFiltersButton = document.querySelector<HTMLButtonElement>("#clear-filters")!;
const statusLine = document.querySelector<HTMLElement>("#status")!;
const resultsList = document.querySelector<HTMLElement>("#results")!;
const pagination = document.querySelector<HTMLElement>("#pagination")!;
const detailPanel = document.querySelector<HTMLElement>("#detail")!;
const statsLabel = document.querySelector<HTMLElement>("#stats")!;
const mboxPathInput = document.querySelector<HTMLInputElement>("#mbox-path")!;
const importButton = document.querySelector<HTMLButtonElement>("#import-btn")!;
const seedButton = document.querySelector<HTMLButtonElement>("#seed-btn")!;
const importStatus = document.querySelector<HTMLElement>("#import-status")!;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

interface UiState {
  text: string;
  sources: string[];
  author: string;
  fromDate: string;
  toDate: string;
  offset: number;
  total: number;
  hits: SearchHit[];
  activeIndex: number;
  detail: ChatContext | null;
}

const state: UiState = {
  text: "",
  sources: [],
  author: "",
  fromDate: "",
  toDate: "",
  offset: 0,
  total: 0,
  hits: [],
  activeIndex: -1,
  detail: null,
};

// ---------------------------------------------------------------------------
// Rendering glue
// ---------------------------------------------------------------------------

function setStatus(text: string, isError = false): void {
  statusLine.textContent = text;
  statusLine.classList.toggle("error", isError);
}

function renderAll(): void {
  const terms = splitTerms(state.text);
  renderResults(resultsList, {
    groups: groupHits(state.hits),
    activeIndex: state.activeIndex,
    terms,
    onOpen: (id) => void openDetail(id),
  });
  renderPagination(pagination, pageModel(state.total, state.offset, PAGE_SIZE), {
    onPrev: () => void runSearch(state.offset - PAGE_SIZE),
    onNext: () => void runSearch(state.offset + PAGE_SIZE),
  });
  if (state.detail) {
    renderDetail(detailPanel, {
      ctx: state.detail,
      queryText: state.text,
      onOpenMessage: (id) => void openDetail(id),
      onClose: closeDetail,
    });
    detailPanel.classList.add("open");
  } else {
    detailPanel.classList.remove("open");
    detailPanel.replaceChildren();
  }
}

function refreshChips(stats: Stats): void {
  renderSourceChips(sourceChips, {
    available: stats.by_source,
    selected: state.sources,
    onToggle: (source) => {
      state.sources = state.sources.includes(source)
        ? state.sources.filter((s) => s !== source)
        : [...state.sources, source];
      refreshChips(stats);
      void runSearch(0);
    },
  });
}

async function refreshStats(): Promise<void> {
  try {
    const stats = await invoke<Stats>("stats");
    const parts = stats.by_source.map((s) => `${s.source}: ${s.count}`).join(", ");
    statsLabel.textContent =
      stats.total === 0 ? "empty archive" : `${stats.total} messages (${parts})`;
    refreshChips(stats);
  } catch {
    statsLabel.textContent = "";
  }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

// Sequential search token: a late response from an earlier query must not
// overwrite the results of the newest one.
let searchSeq = 0;

async function runSearch(offset = 0): Promise<void> {
  const seq = ++searchSeq;
  const text = searchInput.value.trim();
  if (!text && !state.sources.length && !authorInput.value.trim() && !fromDateInput.value && !toDateInput.value) {
    state.hits = [];
    state.total = 0;
    state.offset = 0;
    state.activeIndex = -1;
    setStatus("Type something to search, or set a filter to browse.");
    renderAll();
    return;
  }
  state.text = text;
  state.author = authorInput.value.trim();
  state.fromDate = fromDateInput.value;
  state.toDate = toDateInput.value;
  const requestedOffset = Math.max(0, offset);
  setStatus("Searching…");
  try {
    const page = await invoke<SearchPage>("search_messages", {
      query: {
        text: state.text,
        sources: state.sources,
        author: state.author,
        from_date: state.fromDate,
        to_date: state.toDate,
        limit: PAGE_SIZE,
        offset: requestedOffset,
      },
    });
    if (seq !== searchSeq) return; // a newer search superseded this response
    state.hits = page.hits;
    state.total = page.total;
    state.offset = page.offset;
    state.activeIndex = -1;
    const groups = groupHits(page.hits).length;
    const range =
      page.total === 0
        ? "0 results"
        : `${page.offset + 1}–${Math.min(page.offset + PAGE_SIZE, page.total)} of ${page.total}`;
    setStatus(
      `${range}${page.total > 0 ? ` · ${groups} chat group${groups === 1 ? "" : "s"}` : ""}` +
        (state.text ? ` for “${state.text}”` : ""),
    );
    renderAll();
  } catch (e) {
    if (seq === searchSeq) setStatus(String(e), true);
  }
}

function clearFilters(): void {
  state.sources = [];
  authorInput.value = "";
  fromDateInput.value = "";
  toDateInput.value = "";
  void refreshStats();
  void runSearch(0);
}

// ---------------------------------------------------------------------------
// Detail view
// ---------------------------------------------------------------------------

// Sequential detail-load token: walking the thread fires concurrent
// chat_context invocations; only the newest one may render.
let detailSeq = 0;

async function openDetail(id: number): Promise<void> {
  const seq = ++detailSeq;
  try {
    const ctx = await invoke<ChatContext | null>("chat_context", { id, radius: 8 });
    if (seq !== detailSeq) return; // a newer load superseded this response
    if (ctx) {
      state.detail = ctx;
      renderAll();
      detailPanel.querySelector<HTMLElement>(".detail-card")?.scrollIntoView({ block: "nearest" });
    }
  } catch (e) {
    if (seq === detailSeq) setStatus(String(e), true);
  }
}

function closeDetail(): void {
  state.detail = null;
  renderAll();
}

// ---------------------------------------------------------------------------
// Keyboard navigation
// ---------------------------------------------------------------------------

document.addEventListener("keydown", (event) => {
  const target = event.target as HTMLElement;
  const inEditable =
    target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable;

  if (state.detail) {
    if (event.key === "Escape") {
      closeDetail();
      event.preventDefault();
    } else if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      const dir = event.key === "ArrowUp" ? -1 : 1;
      const id = neighborId(state.detail, dir);
      if (id !== null) {
        void openDetail(id);
      }
      event.preventDefault();
    }
    return;
  }

  if (event.key === "/" && !inEditable) {
    searchInput.focus();
    searchInput.select();
    event.preventDefault();
    return;
  }
  if (inEditable) return;

  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    if (state.hits.length === 0) return;
    const delta = event.key === "ArrowDown" ? 1 : -1;
    state.activeIndex = moveIndex(state.activeIndex, state.hits.length, delta);
    renderAll();
    resultsList
      .querySelector(`.hit-item.active`)
      ?.scrollIntoView({ block: "nearest" });
    event.preventDefault();
  } else if (event.key === "Enter") {
    if (state.activeIndex >= 0 && state.hits[state.activeIndex]) {
      void openDetail(state.hits[state.activeIndex].message.id);
      event.preventDefault();
    }
  } else if (event.key === "Escape") {
    if (state.activeIndex !== -1) {
      state.activeIndex = -1;
      renderAll();
      event.preventDefault();
    }
  }
});

// ---------------------------------------------------------------------------
// Import controls
// ---------------------------------------------------------------------------

async function importMbox(): Promise<void> {
  const path = mboxPathInput.value.trim();
  if (!path) {
    importStatus.textContent = "Enter a path to an .mbox file first.";
    return;
  }
  importButton.disabled = true;
  importStatus.textContent = `Importing ${path}…`;
  try {
    const summary = await invoke<IngestSummary>("import_mbox", { path });
    const failedPart = summary.failed > 0 ? `, ${summary.failed} failed to parse` : "";
    importStatus.textContent =
      `Imported ${summary.inserted} new, ${summary.skipped} already present` +
      `${failedPart} (of ${summary.seen} in ${summary.source}).`;
    await refreshStats();
  } catch (e) {
    importStatus.textContent = `Import failed: ${e}`;
  } finally {
    importButton.disabled = false;
  }
}

async function seedDemo(): Promise<void> {
  seedButton.disabled = true;
  try {
    const summary = await invoke<IngestSummary>("seed_demo");
    importStatus.textContent = `Demo messages: ${summary.inserted} new, ${summary.skipped} already present.`;
    await refreshStats();
  } catch (e) {
    importStatus.textContent = `Seeding failed: ${e}`;
  } finally {
    seedButton.disabled = false;
  }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

searchButton.addEventListener("click", () => void runSearch(0));
searchInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void runSearch(0);
});
authorInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void runSearch(0);
});
fromDateInput.addEventListener("change", () => void runSearch(0));
toDateInput.addEventListener("change", () => void runSearch(0));
clearFiltersButton.addEventListener("click", clearFilters);
importButton.addEventListener("click", () => void importMbox());
seedButton.addEventListener("click", () => void seedDemo());

setStatus("Type something to search, or set a filter to browse.");
renderAll();
void refreshStats();

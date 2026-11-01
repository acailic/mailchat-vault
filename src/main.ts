import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { formatSentAt, snippet } from "./format";
import { describePlan, privacyNote, type AskOutcome, type LlmStatus } from "./llmview";
import {
  renderDetail,
  renderPagination,
  renderResults,
  renderSourceChips,
} from "./render";
import type {
  ChatContext,
  EmbedJobSummary,
  EmbeddingStatus,
  ChatImportOutcome,
  IngestSummary,
  MediaSummary,
  Message,
  SearchHit,
  SearchPage,
  Stats,
  WatchSnapshot,
} from "./types";
import { groupHits, moveIndex, neighborId, pageModel, splitTerms } from "./viewmodel";
import { renderWatchEvents, renderWatchFolders, watchStatusText } from "./watchview";

const PAGE_SIZE = 50;

// ---------------------------------------------------------------------------
// Elements
// ---------------------------------------------------------------------------

interface FolderSyncSummary {
  folder: string;
  uidvalidity: number;
  reset: boolean;
  seen: number;
  inserted: number;
  skipped: number;
  failed: number;
  /** Why the folder failed to sync; null on success. */
  error: string | null;
}

interface ImapSyncOutcome {
  folders: FolderSyncSummary[];
  /** null when a stored password was reused; "os_keyring" | "memory" when
   * the entered password was just saved there. */
  saved_to: "os_keyring" | "memory" | null;
}

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
const chatImportButton = document.querySelector<HTMLButtonElement>("#chat-import-btn")!;
const chatMediaToggle = document.querySelector<HTMLInputElement>("#chat-media")!;
const seedButton = document.querySelector<HTMLButtonElement>("#seed-btn")!;
const importStatus = document.querySelector<HTMLElement>("#import-status")!;
const askInput = document.querySelector<HTMLInputElement>("#ask")!;
const askButton = document.querySelector<HTMLButtonElement>("#ask-btn")!;
const askStatus = document.querySelector<HTMLElement>("#ask-status")!;
const llmNote = document.querySelector<HTMLElement>("#llm-note")!;
const answerBox = document.querySelector<HTMLElement>("#answer")!;
const askNotes = document.querySelector<HTMLElement>("#ask-notes")!;
const askResults = document.querySelector<HTMLUListElement>("#ask-results")!;
const embedButton = document.querySelector<HTMLButtonElement>("#embed-btn")!;
const embedStatus = document.querySelector<HTMLElement>("#embed-status")!;
const imapHostInput = document.querySelector<HTMLInputElement>("#imap-host")!;
const imapPortInput = document.querySelector<HTMLInputElement>("#imap-port")!;
const imapUserInput = document.querySelector<HTMLInputElement>("#imap-user")!;
const imapPassInput = document.querySelector<HTMLInputElement>("#imap-pass")!;
const imapFoldersInput = document.querySelector<HTMLInputElement>("#imap-folders")!;
const imapSyncButton = document.querySelector<HTMLButtonElement>("#imap-sync-btn")!;
const imapStatus = document.querySelector<HTMLElement>("#imap-status")!;
const watchPathInput = document.querySelector<HTMLInputElement>("#watch-path")!;
const watchButton = document.querySelector<HTMLButtonElement>("#watch-btn")!;
const watchAddButton = document.querySelector<HTMLButtonElement>("#watch-add-btn")!;
const watchStatus = document.querySelector<HTMLElement>("#watch-status")!;
const watchFoldersList = document.querySelector<HTMLUListElement>("#watch-folders")!;
const watchEventsList = document.querySelector<HTMLUListElement>("#watch-events")!;

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

function renderAskResults(list: HTMLUListElement, messages: Message[]): void {
  list.replaceChildren(
    ...messages.map((m) => {
      const li = document.createElement("li");
      const meta = document.createElement("div");
      meta.className = "meta";
      meta.textContent = `${m.author || "unknown"} · ${formatSentAt(m.sent_at)} · ${m.source}/${m.channel}${m.chat_id ? "/" + m.chat_id : ""}`;
      const body = document.createElement("div");
      body.className = "body";
      body.textContent = snippet(m.body);
      li.append(meta, body);
      return li;
    }),
  );
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

function renderAsk(outcome: AskOutcome): void {
  if (outcome.answer) {
    answerBox.textContent = outcome.answer;
    answerBox.hidden = false;
  } else {
    answerBox.hidden = true;
  }
  askNotes.textContent = outcome.notes.join(" · ");
  askNotes.hidden = outcome.notes.length === 0;
  const shown = outcome.results as Message[];
  renderAskResults(askResults, shown);
}

async function runAsk(): Promise<void> {
  const question = askInput.value.trim();
  if (!question) {
    askStatus.textContent = "Ask a question first.";
    return;
  }
  askButton.disabled = true;
  askStatus.textContent = "Asking… (translate → search → summarize)";
  try {
    const outcome = await invoke<AskOutcome>("llm_ask", { question, limit: 50 });
    askStatus.textContent = describePlan(outcome);
    renderAsk(outcome);
  } catch (e) {
    askStatus.textContent = String(e);
  } finally {
    askButton.disabled = false;
  }
}

async function refreshLlmNote(): Promise<void> {
  try {
    const status = await invoke<LlmStatus>("llm_status");
    llmNote.textContent = privacyNote(status);
  } catch {
    llmNote.textContent = "";
  }
}

// Local embedding coverage label for the Ask section (incremental job).
function describeEmbedStatus(status: EmbeddingStatus): string {
  if (status.messages === 0) {
    return "no messages to embed yet";
  }
  const model = `${status.model}, ${status.dim}d`;
  if (status.pending === 0) {
    return `local embeddings: all ${status.embedded} embedded (${model})`;
  }
  return `local embeddings: ${status.embedded}/${status.messages} · ${status.pending} pending (${model})`;
}

// Sequential embed-status token: a late response from an earlier refresh
// (e.g. one fired before an embedding job finished) must not overwrite
// newer status text — the job itself joins the sequence when it starts.
let embedSeq = 0;

async function refreshEmbedStatus(): Promise<void> {
  const seq = ++embedSeq;
  try {
    const status = await invoke<EmbeddingStatus>("embed_status");
    if (seq !== embedSeq) return; // a newer refresh/job superseded this response
    embedStatus.textContent = describeEmbedStatus(status);
  } catch {
    if (seq === embedSeq) embedStatus.textContent = "";
  }
}

async function runEmbedJob(): Promise<void> {
  embedSeq++; // supersede any in-flight embed_status refresh
  embedButton.disabled = true;
  embedStatus.textContent = "Embedding… (local, offline)";
  try {
    const summary = await invoke<EmbedJobSummary>("embed_messages", {});
    embedStatus.textContent =
      `Embedded ${summary.embedded} new · ${summary.remaining} pending` +
      ` (${summary.elapsed_ms} ms, ${summary.model})`;
  } catch (e) {
    embedStatus.textContent = `Embedding failed: ${e}`;
  } finally {
    embedButton.disabled = false;
  }
}

// Sequential stats token: a late response from an earlier refresh (e.g. one
// fired before an import finished) must not overwrite newer chips/stats.
let statsSeq = 0;

async function refreshStats(): Promise<void> {
  const seq = ++statsSeq;
  try {
    const stats = await invoke<Stats>("stats");
    if (seq !== statsSeq) return; // a newer refresh superseded this response
    const parts = stats.by_source.map((s) => `${s.source}: ${s.count}`).join(", ");
    statsLabel.textContent =
      stats.total === 0 ? "empty archive" : `${stats.total} messages (${parts})`;
    refreshChips(stats);
  } catch {
    if (seq === statsSeq) statsLabel.textContent = "";
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
  detailSeq++; // supersede any in-flight load so it cannot re-open the panel
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
    void refreshEmbedStatus();
  } catch (e) {
    importStatus.textContent = `Import failed: ${e}`;
  } finally {
    importButton.disabled = false;
  }
}

/** "media: 2 stored, 1 deduped · 3 linked · 1 missing" with only the
 * non-zero parts; empty when nothing happened. */
function describeMedia(media: MediaSummary): string {
  const parts: string[] = [];
  const add = (n: number, label: string) => {
    if (n > 0) parts.push(`${n} ${label}`);
  };
  add(media.stored, "stored");
  add(media.deduped, "deduped");
  add(media.linked, "linked");
  add(media.missing, "missing");
  return parts.length === 0 ? "" : ` Media: ${parts.join(", ")}.`;
}

async function importChatExport(): Promise<void> {
  chatImportButton.disabled = true;
  try {
    const picked = await open({
      multiple: false,
      title: "Import chat export",
      filters: [
        {
          name: "Chat export (WhatsApp .txt, Telegram result.json, Signal .json)",
          extensions: ["txt", "json"],
        },
      ],
    });
    if (typeof picked !== "string") {
      return; // cancelled
    }
    const extractMedia = chatMediaToggle.checked;
    importStatus.textContent = `Importing ${picked}${extractMedia ? " (extracting media)" : ""}…`;
    const outcome = await invoke<ChatImportOutcome>("import_chat_export", {
      path: picked,
      extractMedia: extractMedia,
    });
    const summary = outcome.summary;
    const failedPart = summary.failed > 0 ? `, ${summary.failed} failed to parse` : "";
    importStatus.textContent =
      `Imported ${summary.inserted} new, ${summary.skipped} already present` +
      `${failedPart} (of ${summary.seen} in ${summary.source}).` +
      (outcome.media ? describeMedia(outcome.media) : "");
    await refreshStats();
    void refreshEmbedStatus();
  } catch (e) {
    importStatus.textContent = `Import failed: ${e}`;
  } finally {
    chatImportButton.disabled = false;
  }
}

async function seedDemo(): Promise<void> {
  seedButton.disabled = true;
  try {
    const summary = await invoke<IngestSummary>("seed_demo");
    importStatus.textContent = `Demo messages: ${summary.inserted} new, ${summary.skipped} already present.`;
    await refreshStats();
    void refreshEmbedStatus();
  } catch (e) {
    importStatus.textContent = `Seeding failed: ${e}`;
  } finally {
    seedButton.disabled = false;
  }
}

async function syncImap(): Promise<void> {
  const host = imapHostInput.value.trim();
  const username = imapUserInput.value.trim();
  const port = Number.parseInt(imapPortInput.value, 10);
  const password = imapPassInput.value;
  const folders = imapFoldersInput.value
    .split(",")
    .map((f) => f.trim())
    .filter((f) => f.length > 0);
  if (!host || !username) {
    imapStatus.textContent = "Enter the IMAP host and your username first.";
    return;
  }
  imapSyncButton.disabled = true;
  imapStatus.textContent = `Syncing ${username} over IMAP… (first sync of a large mailbox can take a while)`;
  try {
    const outcome = await invoke<ImapSyncOutcome>("sync_imap", {
      host,
      port: Number.isNaN(port) ? null : port,
      username,
      password,
      folders: folders.length > 0 ? folders : null,
    });
    const lines = outcome.folders.map(
      (f) =>
        `${f.folder}: ${f.inserted} new, ${f.skipped} already present` +
        (f.failed > 0 ? `, ${f.failed} failed to parse` : "") +
        (f.reset ? " (full rescan)" : "") +
        (f.error ? ` — failed: ${f.error}` : ""),
    );
    const note =
      outcome.saved_to === "memory"
        ? " Password kept in memory only for this session (no OS keyring available)."
        : outcome.saved_to === "os_keyring"
          ? " Password stored in the OS keyring."
          : "";
    imapStatus.textContent = lines.join(" · ") + note;
    imapPassInput.value = "";
    await refreshStats();
    void refreshEmbedStatus();
  } catch (e) {
    imapStatus.textContent = `Sync failed: ${e}`;
  } finally {
    imapSyncButton.disabled = false;
  }
}

// ---------------------------------------------------------------------------
// Folder watching
// ---------------------------------------------------------------------------

function renderWatchStatus(snapshot: WatchSnapshot): void {
  watchStatus.textContent = watchStatusText(snapshot);
  renderWatchFolders(watchFoldersList, snapshot, {
    onRemove: (path) => void removeWatchFolder(path),
  });
  renderWatchEvents(watchEventsList, snapshot.events);
}

async function refreshWatchStatus(): Promise<void> {
  try {
    renderWatchStatus(await invoke<WatchSnapshot>("watch_status"));
  } catch {
    // Leave whatever was rendered; the next watch-status event fixes it.
  }
}

async function addWatchFolder(path: string): Promise<void> {
  watchStatus.textContent = `Adding ${path}…`;
  try {
    renderWatchStatus(await invoke<WatchSnapshot>("watch_add_folder", { path }));
    await refreshStats();
  } catch (e) {
    watchStatus.textContent = `Could not watch folder: ${e}`;
  }
}

/** Watch the typed path (Enter or the Watch button). */
async function watchTypedPath(): Promise<void> {
  const path = watchPathInput.value.trim();
  if (!path) {
    watchStatus.textContent = "Type a folder path first, or use Pick folder…";
    return;
  }
  watchButton.disabled = true;
  try {
    await addWatchFolder(path);
    watchPathInput.value = "";
  } finally {
    watchButton.disabled = false;
  }
}

/** Watch a folder picked with the native directory dialog. */
async function pickWatchFolder(): Promise<void> {
  watchAddButton.disabled = true;
  try {
    const picked = await open({
      multiple: false,
      directory: true,
      title: "Choose a folder to watch",
    });
    if (typeof picked !== "string") {
      return; // cancelled
    }
    await addWatchFolder(picked);
  } finally {
    watchAddButton.disabled = false;
  }
}

async function removeWatchFolder(path: string): Promise<void> {
  try {
    renderWatchStatus(await invoke<WatchSnapshot>("watch_remove_folder", { path }));
    await refreshStats();
  } catch (e) {
    watchStatus.textContent = `Could not remove folder: ${e}`;
  }
}

// The backend pushes a fresh snapshot on every watcher change; a newest
// "imported" event also refreshes the source chips/stats.
void listen<WatchSnapshot>("watch-status", (event) => {
  renderWatchStatus(event.payload);
  if (event.payload.events[0]?.kind === "imported") {
    void refreshStats();
  }
});

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

searchButton.addEventListener("click", () => void runSearch(0));
searchInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void runSearch(0);
});
askButton.addEventListener("click", () => void runAsk());
askInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void runAsk();
});
authorInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void runSearch(0);
});
fromDateInput.addEventListener("change", () => void runSearch(0));
toDateInput.addEventListener("change", () => void runSearch(0));
clearFiltersButton.addEventListener("click", clearFilters);
importButton.addEventListener("click", () => void importMbox());
chatImportButton.addEventListener("click", () => void importChatExport());
seedButton.addEventListener("click", () => void seedDemo());
imapSyncButton.addEventListener("click", () => void syncImap());
embedButton.addEventListener("click", () => void runEmbedJob());
watchButton.addEventListener("click", () => void watchTypedPath());
watchPathInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void watchTypedPath();
});
watchAddButton.addEventListener("click", () => void pickWatchFolder());

setStatus("Type something to search, or set a filter to browse.");
renderAll();
void refreshStats();
void refreshLlmNote();
void refreshEmbedStatus();
void refreshWatchStatus();

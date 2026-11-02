import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { formatSentAt, snippet } from "./format";
import { describePlan, privacyNoteFor, type AskOutcome, type LlmStatus } from "./llmview";
import {
  renderDashboard,
  renderDetail,
  renderPagination,
  renderResults,
  renderSourceChips,
} from "./render";
import {
  applyTheme,
  keyStoreNote,
  kindLabel,
  menuItemLabel,
  placeholderForKind,
  prefillForKind,
  uniqueProviderId,
  LOCAL_ID,
  ENV_ID,
  type ProviderKind,
  type ProviderMenuItem,
  type SaveSettingsPayload,
  type SettingsView,
} from "./settings";
import type {
  ArchiveStats,
  ChatContext,
  EmbedJobSummary,
  EmbeddingStatus,
  ChatImportOutcome,
  ExportSummary,  IngestSummary,
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
const dashboard = document.querySelector<HTMLElement>("#dashboard")!;
const dashboardStatus = document.querySelector<HTMLElement>("#dashboard-status")!;
const dashboardRefreshButton = document.querySelector<HTMLButtonElement>("#stats-refresh")!;
const exportFormatSelect = document.querySelector<HTMLSelectElement>("#export-format")!;
const exportButton = document.querySelector<HTMLButtonElement>("#export-btn")!;
const exportStatus = document.querySelector<HTMLElement>("#export-status")!;
const askProvider = document.querySelector<HTMLSelectElement>("#ask-provider")!;
const settingsNote = document.querySelector<HTMLElement>("#settings-note")!;
const envInfo = document.querySelector<HTMLElement>("#env-info")!;
const providerRows = document.querySelector<HTMLElement>("#provider-rows")!;
const addKind = document.querySelector<HTMLSelectElement>("#add-kind")!;
const addProviderButton = document.querySelector<HTMLButtonElement>("#add-provider-btn")!;
const themeSelect = document.querySelector<HTMLSelectElement>("#theme")!;
const summarizeTopInput = document.querySelector<HTMLInputElement>("#summarize-top")!;
const settingsSaveButton = document.querySelector<HTMLButtonElement>("#settings-save-btn")!;

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

// The last `llm_status` response: powers the Ask provider menu and the
// privacy note for the current selection.
let llmStatus: LlmStatus | null = null;

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
      onExportChat: () => void exportChat(),
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
    const provider = askProvider.value || null;
    const outcome = await invoke<AskOutcome>("llm_ask", { question, limit: 50, provider });
    askStatus.textContent = describePlan(outcome);
    renderAsk(outcome);
  } catch (e) {
    askStatus.textContent = String(e);
  } finally {
    askButton.disabled = false;
  }
}

// ---------------------------------------------------------------------------
// Provider menu (multi-provider Ask UI)
// ---------------------------------------------------------------------------

/** The menu entry currently selected in the Ask provider menu. */
function selectedMenuItem(): ProviderMenuItem | null {
  return llmStatus?.providers.find((p) => p.id === askProvider.value) ?? null;
}

/** Rebuild the Ask provider menu from a status. `keepPrevious` keeps the
 * user's current selection when it is still available (a rebuild after a
 * settings save); the first build always preselects the default. */
function buildProviderMenu(status: LlmStatus, keepPrevious: boolean): void {
  const previous = keepPrevious ? askProvider.value : "";
  askProvider.replaceChildren(
    ...status.providers.map((item) => {
      const option = document.createElement("option");
      option.value = item.id;
      option.textContent = menuItemLabel(item);
      option.disabled = !item.available;
      return option;
    }),
  );
  const stillThere =
    previous && status.providers.some((p) => p.id === previous && p.available);
  const wanted = stillThere
    ? previous
    : (status.providers.find((p) => p.id === status.default_provider && p.available)?.id ??
      LOCAL_ID);
  askProvider.value = wanted;
  updateLlmNote();
}

/** Privacy note for the currently selected provider. */
function updateLlmNote(): void {
  const item = selectedMenuItem();
  llmNote.textContent = item
    ? privacyNoteFor(item, llmStatus?.summarize_top ?? 8)
    : "";
}

async function refreshLlm(): Promise<void> {
  try {
    const hadStatus = llmStatus !== null;
    llmStatus = await invoke<LlmStatus>("llm_status");
    buildProviderMenu(llmStatus, hadStatus);
  } catch {
    llmStatus = null;
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

/** Rebuild the menu, preselecting the default (used when a save changed
 * the default provider, so a freshly configured default takes effect). */
function preferDefaultInMenu(): void {
  if (llmStatus) buildProviderMenu(llmStatus, false);
}

// ---------------------------------------------------------------------------
// Settings (providers, default, options)
// ---------------------------------------------------------------------------

function rowInputs(row: HTMLElement): Record<string, HTMLInputElement> {
  const pick = (cls: string) => row.querySelector<HTMLInputElement>(`.${cls}`)!;
  return {
    name: pick("prov-name"),
    base_url: pick("prov-url"),
    model: pick("prov-model"),
    key: pick("prov-key"),
  };
}

/** One provider row: default radio, name/url/model inputs, write-only key
 * field, clear-key and remove buttons. `takenIds` feeds id generation for
 * newly added rows. */
function providerRow(
  profile: { id: string; name: string; kind: ProviderKind; base_url: string; model: string },
  hasKey: boolean,
  isDefault: boolean,
): HTMLElement {
  const row = document.createElement("div");
  row.className = "provider-row";
  row.dataset.id = profile.id;
  row.dataset.kind = profile.kind;

  const radio = document.createElement("input");
  radio.type = "radio";
  radio.name = "default-provider";
  radio.value = profile.id;
  radio.checked = isDefault;
  radio.title = "Use as the default provider for Ask";

  const kind = document.createElement("span");
  kind.className = "kind-label";
  kind.textContent = kindLabel(profile.kind);

  const name = document.createElement("input");
  name.className = "prov-name";
  name.type = "text";
  name.value = profile.name;
  name.placeholder = `${kindLabel(profile.kind)} name`;

  const url = document.createElement("input");
  url.className = "prov-url";
  url.type = "text";
  url.value = profile.base_url;
  url.placeholder = placeholderForKind(profile.kind, "base_url");

  const model = document.createElement("input");
  model.className = "prov-model";
  model.type = "text";
  model.value = profile.model;
  model.placeholder = placeholderForKind(profile.kind, "model");

  const key = document.createElement("input");
  key.className = "prov-key";
  key.type = "password";
  key.autocomplete = "off";
  key.placeholder = hasKey ? "stored — type to replace" : "API key";
  row.dataset.hasKey = String(hasKey);

  const clearKey = document.createElement("button");
  clearKey.type = "button";
  clearKey.className = "small-btn clear-key-btn";
  clearKey.textContent = "Clear key";
  clearKey.hidden = !hasKey;

  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "small-btn remove-prov-btn";
  remove.textContent = "Remove";

  row.append(radio, kind, name, url, model, key, clearKey, remove);
  return row;
}

/** Render the whole Settings form from a view (after boot and after every
 * successful save or key clear). */
function renderSettings(view: SettingsView): void {
  providerRows.replaceChildren(
    ...view.providers.map((p) =>
      providerRow(p, p.has_key, view.default_provider === p.id),
    ),
  );
  const autoRadio = document.querySelector<HTMLInputElement>(
    'input[name="default-provider"][value=""]',
  );
  if (autoRadio)
    autoRadio.checked =
      view.default_provider === null || view.default_provider === ENV_ID;
  themeSelect.value = view.theme;
  summarizeTopInput.value = String(view.summarize_top);

  const envLines: string[] = [];
  if (view.env) {
    envLines.push(
      `Environment fallback: ${view.env.model} via ${view.env.host} (read-only — ZAI_API_KEY/GLM_API_KEY or ANTHROPIC_API_KEY + ANTHROPIC_BASE_URL; used when no default provider is set).`,
    );
  }
  if (view.env_error) envLines.push(`Environment config ignored: ${view.env_error}`);
  if (view.settings_error) envLines.push(`Settings file problem: ${view.settings_error}`);
  envInfo.textContent = envLines.join(" ");

  settingsNote.classList.remove("error");
  settingsNote.textContent = keyStoreNote(view.key_store);
  applyTheme(view.theme);
}

/** The parsed summarize_top for the backend validator: an unparseable
 * entry passes through as 0, which the backend refuses with its clear
 * bounds error — never silently coerced to a valid value. */
function parsedSummarizeTop(value: string): number {
  const n = Number.parseInt(value, 10);
  return Number.isNaN(n) ? 0 : n;
}

/** Collect the form into a `settings_save` payload. */
function collectSettings(): SaveSettingsPayload {
  const rows = [...providerRows.querySelectorAll<HTMLElement>(".provider-row")];
  const providers = rows.map((row) => ({
    id: row.dataset.id!,
    name: rowInputs(row).name.value.trim(),
    kind: row.dataset.kind as ProviderKind,
    base_url: rowInputs(row).base_url.value.trim(),
    model: rowInputs(row).model.value.trim(),
  }));
  const checked = document.querySelector<HTMLInputElement>(
    'input[name="default-provider"]:checked',
  );
  const api_keys = rows
    .map((row) => ({ id: row.dataset.id!, key: rowInputs(row).key.value.trim() }))
    .filter((update) => update.key !== "");
  return {
    providers,
    default_provider: checked && checked.value ? checked.value : null,
    theme: themeSelect.value as SettingsView["theme"],
    summarize_top: parsedSummarizeTop(summarizeTopInput.value),
    api_keys,
  };
}

async function saveSettings(): Promise<void> {
  const payload = collectSettings();
  const previousDefault = llmStatus?.default_provider ?? null;
  settingsSaveButton.disabled = true;
  settingsNote.classList.remove("error");
  settingsNote.textContent = "Saving…";
  try {
    const view = await invoke<SettingsView>("settings_save", { payload });
    renderSettings(view);
    settingsNote.textContent = `Saved. ${keyStoreNote(view.key_store)}`.trim();
    await refreshLlm();
    // A save that changed the default provider switches the menu to it;
    // otherwise the user's current selection survives the rebuild.
    if (llmStatus && llmStatus.default_provider !== previousDefault) {
      preferDefaultInMenu();
    }
  } catch (e) {
    settingsNote.classList.add("error");
    settingsNote.textContent = `Not saved: ${e}`;
  } finally {
    settingsSaveButton.disabled = false;
  }
}

async function loadSettings(): Promise<void> {
  try {
    const view = await invoke<SettingsView>("settings_get");
    renderSettings(view);
  } catch (e) {
    settingsNote.classList.add("error");
    settingsNote.textContent = `Settings unavailable: ${e}`;
  }
}

async function clearProviderKey(id: string): Promise<void> {
  try {
    const view = await invoke<SettingsView>("llm_key_clear", { id });
    renderSettings(view);
    await refreshLlm();
  } catch (e) {
    settingsNote.classList.add("error");
    settingsNote.textContent = `Could not clear key: ${e}`;
  }
}

function addProviderRow(): void {
  const kind = addKind.value as ProviderKind;
  const taken = [...providerRows.querySelectorAll<HTMLElement>(".provider-row")].map(
    (row) => row.dataset.id!,
  );
  const prefill = prefillForKind(kind);
  const id = uniqueProviderId(kind === "zai" ? "zai" : kind, taken);
  providerRows.append(providerRow({ ...prefill, id }, false, false));
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

// Sequential dashboard token, same stale-response guard as refreshStats.
let dashboardSeq = 0;

async function refreshDashboard(): Promise<void> {
  const seq = ++dashboardSeq;
  dashboardStatus.textContent = "";
  try {
    const stats = await invoke<ArchiveStats>("archive_stats", { top: 10 });
    if (seq !== dashboardSeq) return; // a newer refresh superseded this response
    renderDashboard(dashboard, { stats });
  } catch (e) {
    if (seq === dashboardSeq) {
      dashboard.replaceChildren();
      dashboardStatus.textContent = `Statistics unavailable: ${e}`;
    }
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
    void refreshDashboard();
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
          name: "Chat export (WhatsApp .txt, Telegram result.json, Signal .json, Discord messages.json)",
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
    void refreshDashboard();
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
    void refreshDashboard();
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
    void refreshDashboard();
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

/** Returns whether the folder is now watched. */
async function addWatchFolder(path: string): Promise<boolean> {
  watchStatus.textContent = `Adding ${path}…`;
  try {
    renderWatchStatus(await invoke<WatchSnapshot>("watch_add_folder", { path }));
    await refreshStats();
    return true;
  } catch (e) {
    watchStatus.textContent = `Could not watch folder: ${e}`;
    return false;
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
    if (await addWatchFolder(path)) {
      watchPathInput.value = "";
    }
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

// Export
// ---------------------------------------------------------------------------

/** File metadata per export format (IPC id, label, extension). */
const EXPORT_FORMATS: Record<string, { label: string; ext: string }> = {
  csv: { label: "CSV file", ext: "csv" },
  json: { label: "JSON Lines file", ext: "jsonl" },
  mbox: { label: "MBOX file", ext: "mbox" },
};

/** Ask the user where to save an export of the chosen format. */
async function pickExportPath(format: string): Promise<string | null> {
  const meta = EXPORT_FORMATS[format] ?? { label: `${format} file`, ext: format };
  const picked = await save({
    title: "Export",
    defaultPath: `mailchat-export.${meta.ext}`,
    filters: [{ name: meta.label, extensions: [meta.ext] }],
  });
  return typeof picked === "string" ? picked : null;
}

function exportStatusLine(summary: ExportSummary, path: string): string {
  const skipped =
    summary.skipped > 0 ? ` (${summary.skipped} non-email skipped — MBOX carries email rows)` : "";
  const rows = summary.written === 1 ? "1 message" : `${summary.written} messages`;
  return `Exported ${rows}${skipped} to ${path}.`;
}

/** Export every row of the currently executed search (all its pages, under
 * the same filters), not just the visible page. */
async function exportResults(): Promise<void> {
  exportStatus.textContent = "";
  if (state.total === 0) {
    exportStatus.textContent = "Nothing to export — run a search (or set filters) first.";
    return;
  }
  const format = exportFormatSelect.value;
  const path = await pickExportPath(format);
  if (path === null) return; // cancelled
  exportButton.disabled = true;
  exportStatus.textContent = "Exporting…";
  try {
    const summary = await invoke<ExportSummary>("export_search", {
      query: {
        text: state.text,
        sources: state.sources,
        author: state.author,
        from_date: state.fromDate,
        to_date: state.toDate,
        limit: PAGE_SIZE,
        offset: 0,
      },
      format,
      path,
    });
    exportStatus.textContent = exportStatusLine(summary, path);
  } catch (e) {
    exportStatus.textContent = `Export failed: ${e}`;
  } finally {
    exportButton.disabled = false;
  }
}

/** Export the whole chat thread of the message open in the detail view. */
async function exportChat(): Promise<void> {
  const detail = state.detail;
  if (!detail) return;
  exportStatus.textContent = "";
  const format = exportFormatSelect.value;
  const path = await pickExportPath(format);
  if (path === null) return; // cancelled
  exportStatus.textContent = "Exporting chat…";
  try {
    const summary = await invoke<ExportSummary | null>("export_chat", {
      id: detail.message.id,
      format,
      path,
    });
    if (summary === null) {
      exportStatus.textContent = "Export failed: message no longer exists.";
      return;
    }
    exportStatus.textContent = exportStatusLine(summary, path);
  } catch (e) {
    exportStatus.textContent = `Export failed: ${e}`;
  }
}

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
askProvider.addEventListener("change", updateLlmNote);
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
dashboardRefreshButton.addEventListener("click", () => void refreshDashboard());
exportButton.addEventListener("click", () => void exportResults());
addProviderButton.addEventListener("click", addProviderRow);
settingsSaveButton.addEventListener("click", () => void saveSettings());
themeSelect.addEventListener("change", () => applyTheme(themeSelect.value as SettingsView["theme"]));
providerRows.addEventListener("click", (event) => {
  const target = event.target as HTMLElement;
  const row = target.closest<HTMLElement>(".provider-row");
  if (!row || !row.dataset.id) return;
  if (target.classList.contains("clear-key-btn")) {
    void clearProviderKey(row.dataset.id);
  } else if (target.classList.contains("remove-prov-btn")) {
    row.remove();
  }
});

setStatus("Type something to search, or set a filter to browse.");
renderAll();
void refreshStats();
void refreshDashboard();
void refreshLlm();
void refreshEmbedStatus();
void refreshWatchStatus();
void loadSettings();

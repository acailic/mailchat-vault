// Application wiring: composes the explicit state (src/state.ts), IPC
// (src/ipc.ts), and workspace shell (src/workspace.ts) modules with the
// panel builders (render/viewmodel/settings/…). Every panel keeps its
// last-request-wins guard so a late response can never overwrite newer UI
// state (the stale-response contract covered by main.test.ts).

import "./styles/tokens.css";
import "./styles/app.css";

import { open, save } from "@tauri-apps/plugin-dialog";
import { formatSentAt, humanSize, snippet } from "./format";
import {
  objectNameOf,
  renderMediaUsage,
  type MediaUsageView,
} from "./media";
import {
  consentPromptText,
  consentStatusLine,
  describePlan,
  privacyNoteFor,
  splitCitations,
  type AskOutcome,
  type LlmStatus,
} from "./llmview";
import {
  renderDashboard,
  renderPagination,
  renderSourceChips,
} from "./render";
import {
  fillImportScopes,
  renderDetail2,
  renderFilterChips,
  renderFlatResults,
  renderRefreshNotice,
} from "./render2";
import {
  buildRequest,
  captureResults,
  contextCursors,
  contextEntries,
  dateBoundsValid,
  filterChips,
  hasActiveFilters,
  mergeContextPages,
  messageLink,
  messageToText,
  parseMessageLink,
  restoreIndices,
  revisionChanged,
  selectionIds,
  withoutChip,
  type FilterChip,
  type SearchFilters,
} from "./searchstate";
import type {
  AppError,
  AttachmentSummary,
  ImportScope,
} from "./v2types";
import {
  createNewMailNotifier,
  pluginNotifyChannel,
} from "./notify";
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
import {
  SequenceGuard,
  initialUiState,
  type UiState,
} from "./state";
import { initWorkspace, type WorkspaceController } from "./workspace";
import type {
  ChatImportOutcome,
  EmbeddingStatus,
  ExportSummary,
  JobEvent,
  Message,
  SourceInstance,
  Stats,
  SyncScheduleView,
  WatchSnapshot,
} from "./types";
import { moveIndex, pageModel, splitTerms } from "./viewmodel";
import {
  applyJobEvent,
  applyJobsPage,
  hasActiveJob,
  jobsStatusText,
  newJobsTracker,
  renderJobsList,
} from "./jobsview";
import { renderWatchEvents, renderWatchFolders, watchStatusText } from "./watchview";
import { autoSyncStatusText, isIntervalChoice } from "./schedview";
import * as ipc from "./ipc";

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
const sortOrderSelect = document.querySelector<HTMLSelectElement>("#sort-order")!;
const hasAttachmentSelect = document.querySelector<HTMLSelectElement>("#has-attachment")!;
const importScopeSelect = document.querySelector<HTMLSelectElement>("#import-scope")!;
const activeFilterChips = document.querySelector<HTMLElement>("#active-filter-chips")!;
const refreshNotice = document.querySelector<HTMLElement>("#refresh-notice")!;
const selectionFormatSelect = document.querySelector<HTMLSelectElement>("#selection-format")!;
const exportSelectionButton = document.querySelector<HTMLButtonElement>("#export-selection-btn")!;
const selectionStatus = document.querySelector<HTMLElement>("#selection-status")!;
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
const imapAuthSelect = document.querySelector<HTMLSelectElement>("#imap-auth")!;
const imapOauthButton = document.querySelector<HTMLButtonElement>("#imap-oauth-btn")!;
const imapOauthHint = document.querySelector<HTMLElement>("#imap-oauth-hint")!;
const imapFoldersInput = document.querySelector<HTMLInputElement>("#imap-folders")!;
const imapSyncButton = document.querySelector<HTMLButtonElement>("#imap-sync-btn")!;
const imapStatus = document.querySelector<HTMLElement>("#imap-status")!;
const autoSyncToggle = document.querySelector<HTMLInputElement>("#auto-sync-toggle")!;
const autoSyncInterval = document.querySelector<HTMLSelectElement>("#auto-sync-interval")!;
const autoSyncStatus = document.querySelector<HTMLElement>("#auto-sync-status")!;
const watchPathInput = document.querySelector<HTMLInputElement>("#watch-path")!;
const watchButton = document.querySelector<HTMLButtonElement>("#watch-btn")!;
const watchAddButton = document.querySelector<HTMLButtonElement>("#watch-add-btn")!;
const watchStatus = document.querySelector<HTMLElement>("#watch-status")!;
const watchFoldersList = document.querySelector<HTMLUListElement>("#watch-folders")!;
const watchEventsList = document.querySelector<HTMLUListElement>("#watch-events")!;
const jobsStatus = document.querySelector<HTMLElement>("#jobs-status")!;
const jobsList = document.querySelector<HTMLUListElement>("#jobs-list")!;
const dashboard = document.querySelector<HTMLElement>("#dashboard")!;
const dashboardStatus = document.querySelector<HTMLElement>("#dashboard-status")!;
const dashboardRefreshButton = document.querySelector<HTMLButtonElement>("#stats-refresh")!;
const exportFormatSelect = document.querySelector<HTMLSelectElement>("#export-format")!;
// Source instances (id ↔ display label) for the v2 source filter.
let sourceInstances: SourceInstance[] = [];
// The import scopes offered by the import-scope filter.
let importScopes: ImportScope[] = [];
const exportButton = document.querySelector<HTMLButtonElement>("#export-btn")!;
const exportStatus = document.querySelector<HTMLElement>("#export-status")!;
const askProvider = document.querySelector<HTMLSelectElement>("#ask-provider")!;
const consentStatus = document.querySelector<HTMLElement>("#consent-status")!;
const consentRevokeButton = document.querySelector<HTMLButtonElement>("#consent-revoke-btn")!;
const consentPrompt = document.querySelector<HTMLElement>("#consent-prompt")!;
const consentText = document.querySelector<HTMLElement>("#consent-text")!;
const consentAllowButton = document.querySelector<HTMLButtonElement>("#consent-allow-btn")!;
const consentLocalButton = document.querySelector<HTMLButtonElement>("#consent-local-btn")!;
const settingsNote = document.querySelector<HTMLElement>("#settings-note")!;
const mediaUsagePanel = document.querySelector<HTMLElement>("#media-usage")!;
const mediaStoreStatus = document.querySelector<HTMLElement>("#media-store-status")!;
const mediaRefreshButton = document.querySelector<HTMLButtonElement>("#media-refresh-btn")!;
const mediaReconcileButton = document.querySelector<HTMLButtonElement>("#media-reconcile-btn")!;
const envInfo = document.querySelector<HTMLElement>("#env-info")!;
const providerRows = document.querySelector<HTMLElement>("#provider-rows")!;
const addKind = document.querySelector<HTMLSelectElement>("#add-kind")!;
const addProviderButton = document.querySelector<HTMLButtonElement>("#add-provider-btn")!;
const themeSelect = document.querySelector<HTMLSelectElement>("#theme")!;
const summarizeTopInput = document.querySelector<HTMLInputElement>("#summarize-top")!;
const notifyToggle = document.querySelector<HTMLInputElement>("#notify-toggle")!;
const settingsSaveButton = document.querySelector<HTMLButtonElement>("#settings-save-btn")!;

// ---------------------------------------------------------------------------
// State (explicit modules, not loose globals)
// ---------------------------------------------------------------------------

/** Search/results panel state (src/state.ts owns the shape). */
const state: UiState = initialUiState();

/** Workspace shell: view/layout/reading/nav state + DOM effects. */
const workspace: WorkspaceController = initWorkspace();

/** One last-request-wins guard per async panel: a late response from an
 * earlier request must never overwrite newer state (stale-response
 * contract; see main.test.ts). */
const searchGuard = new SequenceGuard();
const detailGuard = new SequenceGuard();
const statsGuard = new SequenceGuard();
const dashboardGuard = new SequenceGuard();
const askGuard = new SequenceGuard();
const embedGuard = new SequenceGuard();

/** The last `llm_status` response: powers the Ask provider menu and the
 * privacy note for the current selection. */
let llmStatus: LlmStatus | null = null;

/** M5 AI consent — session-scoped, deliberately in-memory only: it starts
 * `false` on every app start (a new session asks anew) and is never written
 * to any persistent storage, so consent is remembered per session, not
 * forever. Once granted, asks with a network provider go out; `Revoke`
 * resets it at any time. The state is always visible in the UI. */
let aiConsentGranted = false;

/** Desktop notifications for new mail (roadmap): the saved Settings
 * option, updated from every `settings_get`/`settings_save` response.
 * Starts off: until the first settings load confirms the option is on,
 * stay silent so a persisted opt-out is never violated (the job-event
 * listener registers before settings round-trip back). */
let notifyEnabled = false;

/** The new-mail notifier: native OS notifications after sync/watch
 * cycles store new messages, silent while this window is focused and
 * when the option is off (see src/notify.ts for the full contract). */
const notifier = createNewMailNotifier({
  channel: pluginNotifyChannel(),
  isFocused: () => document.hasFocus(),
  isEnabled: () => notifyEnabled,
});

// ---------------------------------------------------------------------------
// Rendering glue
// ---------------------------------------------------------------------------

function setStatus(text: string, isError = false): void {
  statusLine.textContent = text;
  statusLine.classList.toggle("error", isError);
}

/** Render one ask result row: numbered like the answer's citations
 * ([1], [2], …) and clickable — the evidence trail opens the original
 * message in its conversation context in the reading pane. */
function renderAskResults(list: HTMLUListElement, messages: Message[]): void {
  list.replaceChildren(
    ...messages.map((m, i) => {
      const li = document.createElement("li");
      const open = document.createElement("button");
      open.type = "button";
      open.className = "ask-source";
      open.title = "Open this message in its conversation context";
      const meta = document.createElement("div");
      meta.className = "meta";
      meta.textContent = `[${i + 1}] ${m.author || "unknown"} · ${formatSentAt(m.sent_at)} · ${m.source}/${m.channel}${m.chat_id ? "/" + m.chat_id : ""}`;
      const body = document.createElement("div");
      body.className = "body";
      body.textContent = snippet(m.body);
      open.append(meta, body);
      open.addEventListener("click", () => void openDetail(m.id));
      li.append(open);
      return li;
    }),
  );
}

/** Render the answer with its citations `[n]` as clickable source links:
 * citation `n` opens result `n - 1` — the archived message the summary
 * was built from — in its conversation context. Markers beyond `maxCite`
 * (the summary window: the model never saw those rows) and out-of-range
 * markers (a row not in the results) stay plain text. */
function renderAnswerLinked(answer: string, sources: Message[], maxCite: number): void {
  answerBox.replaceChildren(
    ...splitCitations(answer).map((part) => {
      if (typeof part !== "string") {
        const target = part.cite <= maxCite ? sources[part.cite - 1] : undefined;
        if (!target) return document.createTextNode(`[${part.cite}]`);
        const cite = document.createElement("button");
        cite.type = "button";
        cite.className = "citation";
        cite.dataset.cite = String(part.cite);
        cite.title = `Open source [${part.cite}] — ${target.author || "unknown"} — in its conversation context`;
        cite.textContent = `[${part.cite}]`;
        cite.addEventListener("click", () => void openDetail(target.id));
        return cite;
      }
      return document.createTextNode(part);
    }),
  );
}

function renderAsk(outcome: AskOutcome): void {
  const sources = outcome.results as Message[];
  if (outcome.answer) {
    renderAnswerLinked(outcome.answer, sources, llmStatus?.summarize_top ?? 8);
    answerBox.hidden = false;
  } else {
    answerBox.hidden = true;
  }
  askNotes.textContent = outcome.notes.join(" · ");
  askNotes.hidden = outcome.notes.length === 0;
  renderAskResults(askResults, sources);
}

/** The menu entry an ask with the current selection would use, when it
 * is an *available network* provider (the only case needing consent). */
function selectedNetworkProvider(): ProviderMenuItem | null {
  const item = llmStatus?.providers.find((p) => p.id === askProvider.value) ?? null;
  return item && item.available && item.id !== LOCAL_ID ? item : null;
}

/** Update the always-visible consent state line and prompt visibility for
 * the current provider selection (called on every menu rebuild/change).
 * An open prompt re-renders against the new selection, so the named
 * model/host is always the one a consent click would actually send to. */
function updateConsentUi(): void {
  consentStatus.textContent = consentStatusLine(aiConsentGranted);
  consentRevokeButton.hidden = !aiConsentGranted;
  const network = selectedNetworkProvider();
  if (!network) hideConsentPrompt();
  else if (!consentPrompt.hidden) showConsentPrompt(network);
}

function hideConsentPrompt(): void {
  consentPrompt.hidden = true;
}

/** Show the per-question consent prompt for a network ask without
 * consent: states exactly what would leave the machine. */
function showConsentPrompt(item: ProviderMenuItem): void {
  consentText.textContent = consentPromptText(item, llmStatus?.summarize_top ?? 8);
  consentPrompt.hidden = false;
}

/** Run one ask through the IPC boundary. `consent` is the M5 gate: the
 * backend refuses every network round trip without it. */
async function doAsk(question: string, consent: boolean): Promise<void> {
  const token = askGuard.next();
  askButton.disabled = true;
  askStatus.textContent = consent
    ? "Asking… (translate → search → summarize)"
    : "Asking locally…";
  try {
    const provider = askProvider.value || null;
    const outcome = await ipc.llmAsk({ question, limit: 50, provider, consent });
    if (!askGuard.isCurrent(token)) return;
    askStatus.textContent = describePlan(outcome);
    renderAsk(outcome);
  } catch (e) {
    if (!askGuard.isCurrent(token)) return;
    askStatus.textContent = String(e);
  } finally {
    if (askGuard.isCurrent(token)) askButton.disabled = false;
  }
}

async function runAsk(): Promise<void> {
  const question = askInput.value.trim();
  if (!question) {
    askStatus.textContent = "Ask a question first.";
    return;
  }
  const network = selectedNetworkProvider();
  if (network && !aiConsentGranted) {
    // Explicit consent first: this ask runs fully locally (nothing sent),
    // and the prompt offers to send this same question out on agreement.
    showConsentPrompt(network);
    await doAsk(question, false);
    return;
  }
  await doAsk(question, network !== null && aiConsentGranted);
}

function renderAll(): void {
  const terms = splitTerms(state.filters.text);
  renderFlatResults(resultsList, {
    hits: state.hits,
    activeIndex: state.activeIndex,
    selectedId: state.selectedId,
    selection: state.selection,
    terms,
    onOpen: (id: number) => {
      const at = state.hits.findIndex((h) => h.id === id);
      if (at >= 0) state.activeIndex = at;
      void openDetail(id);
    },
    onToggleSelect: toggleRowSelection,
  });
  renderPagination(pagination, pageModel(state.total, state.offset, PAGE_SIZE), {
    onPrev: () => void runSearch(state.offset - PAGE_SIZE),
    onNext: () => void runSearch(state.offset + PAGE_SIZE),
  });
  renderFilterChips(activeFilterChips, {
    chips: filterChips(state.filters, {
      source: (id) => sourceInstances.find((s) => s.id === id)?.label ?? id,
      import: (id) => importScopes.find((s) => s.import_id === id)?.import_id ?? id,
    }),
    onRemove: (chip) => removeFilterChip(chip),
    onClear: clearFilters,
  });
  renderRefreshNotice(refreshNotice, state.refreshNeeded, () => void runSearch(0));
  if (state.detail) {
    renderDetail2(detailPanel, {
      ctx: state.detail,
      entries: state.detailEntries,
      queryText: state.detailQueryText,
      hasMoreBefore: state.detail.has_more_before,
      hasMoreAfter: state.detail.has_more_after,
      onOpenMessage: (id) => void openDetail(id),
      onLoadEarlier: () => void loadContextPage(true),
      onLoadLater: () => void loadContextPage(false),
      onCopy: () => void copyMessage(),
      onCopyLink: () => void copyLink(),
      onExport: () => void exportSingleMessage(),
      onExportChat: () => void exportChat(),
      onExportMedia: (attachment) => void exportMedia(attachment),
      onOpenExternalMedia: (attachment) => void openMediaExternal(attachment),
      onClose: closeDetail,
    });
    detailPanel.classList.add("open");
  } else {
    detailPanel.classList.remove("open");
    detailPanel.replaceChildren();
  }
  const selected = state.selection.size;
  exportSelectionButton.disabled = selected === 0;
  exportSelectionButton.textContent =
    selected > 0 ? `Export selected (${selected})…` : "Export selected…";
  // The shell mirrors the reading state (drives the medium/compact swap
  // and the wide empty placeholder).
  workspace.store.setReadingOpen(state.detail !== null);
}

function refreshChips(stats: Stats): void {
  renderSourceChips(sourceChips, {
    available: stats.by_source,
    selected: state.sources,
    onToggle: (source) => {
      state.sources = state.sources.includes(source)
        ? state.sources.filter((s) => s !== source)
        : [...state.sources, source];
      // Source scoping is a search action: show the results it produced.
      workspace.store.setView("workspace");
      refreshChips(stats);
      void runSearch(0);
    },
  });
}

/** Load the canonical source instances (id ↔ label) for the v2 filter. */
async function refreshSources(): Promise<void> {
  try {
    sourceInstances = await ipc.sourcesList();
  } catch {
    sourceInstances = []; // chips/stats still work; ids resolve on retry
  }
}

/** Load the import scopes for the import-scope filter chip. */
async function refreshImportScopes(): Promise<void> {
  try {
    importScopes = await ipc.listImportScopesV2();
  } catch {
    importScopes = [];
  }
  fillImportScopes(importScopeSelect, importScopes, state.filters.importJobId);
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

/** Privacy note for the currently selected provider, plus the consent
 * state line (both follow every provider-menu change/rebuild). */
function updateLlmNote(): void {
  const item = selectedMenuItem();
  llmNote.textContent = item
    ? privacyNoteFor(item, llmStatus?.summarize_top ?? 8)
    : "";
  updateConsentUi();
}

async function refreshLlm(): Promise<void> {
  try {
    const hadStatus = llmStatus !== null;
    llmStatus = await ipc.llmStatus();
    buildProviderMenu(llmStatus, hadStatus);
  } catch {
    llmStatus = null;
    llmNote.textContent = "";
    updateConsentUi(); // the consent state line stays meaningful without a menu
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

async function refreshEmbedStatus(): Promise<void> {
  const token = embedGuard.next();
  try {
    const status = await ipc.embedStatus();
    if (!embedGuard.isCurrent(token)) return; // a newer refresh/job superseded this response
    embedStatus.textContent = describeEmbedStatus(status);
  } catch {
    if (embedGuard.isCurrent(token)) embedStatus.textContent = "";
  }
}

/** Submit the incremental embedding job through the v2 background-job
 * protocol: progress streams into the jobs panel (0→total, per batch),
 * and the button stays disabled for as long as an embedding job is
 * queued or running. */
async function runEmbedJob(): Promise<void> {
  embedButton.disabled = true;
  embedStatus.textContent = "Embedding… (local, offline, background job)";
  try {
    await ipc.jobSubmit("embed_messages", {});
    // The panel takes it from here (started/progress/succeeded events).
  } catch (e) {
    embedStatus.textContent = `Embedding job failed to start: ${e}`;
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
  notifyToggle.checked = view.notifications;
  notifyEnabled = view.notifications;

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
    notifications: notifyToggle.checked,
    api_keys,
  };
}

// Enabling notifications is a user gesture, so it is the right moment to
// ask the OS for notification permission (macOS prompts once; the toggle
// itself takes effect on Save like every other option).
notifyToggle.addEventListener("change", () => {
  notifier.onSettingToggled(notifyToggle.checked);
});

async function saveSettings(): Promise<void> {
  const payload = collectSettings();
  const previousDefault = llmStatus?.default_provider ?? null;
  settingsSaveButton.disabled = true;
  settingsNote.classList.remove("error");
  settingsNote.textContent = "Saving…";
  try {
    const view = await ipc.settingsSave(payload);
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
    const view = await ipc.settingsGet();
    renderSettings(view);
  } catch (e) {
    settingsNote.classList.add("error");
    settingsNote.textContent = `Settings unavailable: ${e}`;
  }
}

async function clearProviderKey(id: string): Promise<void> {
  try {
    const view = await ipc.llmKeyClear(id);
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

async function refreshStats(): Promise<void> {
  const token = statsGuard.next();
  try {
    const stats = await ipc.stats();
    if (!statsGuard.isCurrent(token)) return; // a newer refresh superseded this response
    const parts = stats.by_source.map((s) => `${s.source}: ${s.count}`).join(", ");
    statsLabel.textContent =
      stats.total === 0 ? "empty archive" : `${stats.total} messages (${parts})`;
    refreshChips(stats);
  } catch {
    if (statsGuard.isCurrent(token)) statsLabel.textContent = "";
  }
}

async function refreshDashboard(): Promise<void> {
  const token = dashboardGuard.next();
  dashboardStatus.textContent = "";
  try {
    const stats = await ipc.archiveStats(10);
    if (!dashboardGuard.isCurrent(token)) return; // a newer refresh superseded this response
    renderDashboard(dashboard, { stats });
  } catch (e) {
    if (dashboardGuard.isCurrent(token)) {
      dashboard.replaceChildren();
      dashboardStatus.textContent = `Statistics unavailable: ${e}`;
    }
  }
}

// ---------------------------------------------------------------------------
// Search (v2: flat stable ranking, filter chips, empty browse, refresh)
// ---------------------------------------------------------------------------

/** Read the current filter controls into a filter model (names → ids). */
function readFilters(text: string): SearchFilters {
  const labelToIds = new Map<string, string[]>();
  for (const instance of sourceInstances) {
    const ids = labelToIds.get(instance.label) ?? [];
    ids.push(instance.id);
    labelToIds.set(instance.label, ids);
  }
  // A display label may cover several source instances (same IMAP account
  // under different TLS modes): the chip filters every instance carrying
  // that label. An unmapped name still narrows the search — a name that is
  // not a source id matches nothing, visible rather than silently broad.
  const sourceIds = state.sources.flatMap((name) => labelToIds.get(name) ?? [name]);
  return {
    text,
    sourceIds,
    author: authorInput.value.trim(),
    fromDate: fromDateInput.value,
    toDate: toDateInput.value,
    timezone: "",
    hasAttachment:
      hasAttachmentSelect.value === "" ? null : hasAttachmentSelect.value === "with",
    importJobId: importScopeSelect.value === "" ? null : importScopeSelect.value,
    sort: sortOrderSelect.value === "newest" ? "newest" : "relevance",
  };
}

/** Push a filter model back into the controls (chip removal, Clear). */
function writeFilters(f: SearchFilters): void {
  const idToLabel = new Map(sourceInstances.map((s) => [s.id, s.label]));
  state.sources = [...new Set(f.sourceIds.map((id) => idToLabel.get(id) ?? ""))].filter(
    (name) => name !== "",
  );
  authorInput.value = f.author;
  fromDateInput.value = f.fromDate;
  toDateInput.value = f.toDate;
  hasAttachmentSelect.value =
    f.hasAttachment === null ? "" : f.hasAttachment ? "with" : "without";
  importScopeSelect.value = f.importJobId ?? "";
  sortOrderSelect.value = f.sort;
}

/** Remove one applied filter chip and re-run (resets to page one). */
function removeFilterChip(chip: FilterChip): void {
  let next = state.filters;
  if (chip.kind === "source") {
    // One label can stand for several instances (v1 semantics: the filter
    // matches every instance carrying the label) — remove them all.
    const ids = sourceInstances.filter((s) => s.label === chip.label).map((s) => s.id);
    for (const id of ids.length > 0 ? ids : [chip.value]) {
      next = withoutChip(next, { ...chip, value: id });
    }
  } else {
    next = withoutChip(next, chip);
  }
  writeFilters(next);
  void runSearch(0);
}

async function runSearch(offset = 0): Promise<void> {
  const token = searchGuard.next();
  const text = searchInput.value.trim();
  // A pasted message locator opens that message directly (deep link).
  const linked = parseMessageLink(text);
  if (linked !== null) {
    void openDetail(linked);
    return;
  }
  const filters = readFilters(text);
  if (!text && !hasActiveFilters(filters)) {
    state.filters = filters;
    state.hits = [];
    state.total = 0;
    state.offset = 0;
    state.activeIndex = -1;
    state.selectedId = null;
    state.selection.clear();
    state.pageRevision = null;
    state.refreshNeeded = false;
    setStatus("Type something to search, or set a filter to browse.");
    renderAll();
    return;
  }
  // Reject reversed day bounds inline before invocation (UX contract).
  if (!dateBoundsValid(filters.fromDate, filters.toDate)) {
    setStatus(
      `Reversed date range: from ${filters.fromDate} is after to ${filters.toDate}.`,
      true,
    );
    return;
  }
  const requestedOffset = Math.max(0, offset);
  setStatus("Searching…");
  try {
    const page = await ipc.searchArchiveV2(buildRequest(filters, PAGE_SIZE, requestedOffset));
    if (!searchGuard.isCurrent(token)) return; // a newer search superseded this response
    const fresh = requestedOffset === 0;
    if (!fresh && revisionChanged(state.pageRevision, page.archive_revision)) {
      // The archive changed under the pagination: keep the visible rows'
      // revision and offer a refresh instead of silently restarting (A07).
      state.refreshNeeded = true;
    } else if (fresh) {
      state.refreshNeeded = false;
      state.pageRevision = page.archive_revision;
    }
    state.filters = filters;
    state.hits = page.hits;
    state.total = page.total;
    state.offset = page.offset;
    if (fresh) {
      // A fresh submission resets selection and page.
      state.activeIndex = -1;
      state.selectedId = null;
      state.selection.clear();
      state.resultsSnapshot = null;
    }
    const range =
      page.total === 0
        ? "0 results"
        : `${page.offset + 1}–${Math.min(page.offset + PAGE_SIZE, page.total)} of ${page.total}`;
    setStatus(
      `${range}${state.filters.text ? ` for “${state.filters.text}”` : ""}` +
        (state.refreshNeeded ? " · new data available" : ""),
    );
    renderAll();
  } catch (e) {
    if (searchGuard.isCurrent(token)) setStatus(describeError(e), true);
  }
}

/** Render an IPC error readably (structured AppError or legacy string). */
function describeError(e: unknown): string {
  if (e && typeof e === "object" && "code" in e && "message" in e) {
    const err = e as AppError;
    return err.field ? `${err.message} (${err.field})` : err.message;
  }
  return String(e);
}

function clearFilters(): void {
  state.sources = [];
  authorInput.value = "";
  fromDateInput.value = "";
  toDateInput.value = "";
  hasAttachmentSelect.value = "";
  importScopeSelect.value = "";
  sortOrderSelect.value = "relevance";
  void refreshStats();
  void runSearch(0);
}

/** Toggle one result row's export-selection checkbox. */
function toggleRowSelection(id: number): void {
  if (state.selection.has(id)) state.selection.delete(id);
  else state.selection.add(id);
  renderAll();
}

// ---------------------------------------------------------------------------
// Reading pane (v2: detail + chronological context, restorable results)
// ---------------------------------------------------------------------------

const CONTEXT_PAGE = 8;

async function openDetail(id: number): Promise<void> {
  const token = detailGuard.next();
  // Capture the results state once per visit (Back restores it).
  if (!state.detail) {
    state.resultsSnapshot = captureResults(resultsList, state.activeIndex, id);
  }
  try {
    const ctx = await ipc.conversationContextV2({ id, limit: CONTEXT_PAGE });
    if (!detailGuard.isCurrent(token)) return; // a newer load superseded this response
    state.detail = ctx;
    state.detailEntries = contextEntries(ctx);
    state.detailQueryText = state.filters.text;
    state.selectedId = id;
    renderAll();
    workspace.readingOpened();
    detailPanel.querySelector<HTMLElement>(".detail-card")?.scrollIntoView({ block: "nearest" });
  } catch (e) {
    if (!detailGuard.isCurrent(token)) return;
    const err = e as AppError;
    if (err?.code === "not_found") {
      setStatus("Message is no longer in this vault.", true);
    } else {
      setStatus(describeError(e), true);
    }
  }
}

/** Load one more context page (earlier or later) without losing the anchor. */
async function loadContextPage(before: boolean): Promise<void> {
  if (!state.detail) return;
  const { beforeCursor, afterCursor } = contextCursors(state.detailEntries);
  const token = detailGuard.next();
  try {
    const ctx = await ipc.conversationContextV2({
      id: state.detail.anchor.id,
      beforeCursor: before ? beforeCursor : null,
      afterCursor: before ? null : afterCursor,
      limit: CONTEXT_PAGE,
    });
    if (!detailGuard.isCurrent(token)) return;
    state.detailEntries = mergeContextPages(state.detailEntries, ctx);
    state.detail = {
      ...state.detail,
      has_more_before: before ? ctx.has_more_before : state.detail.has_more_before,
      has_more_after: before ? state.detail.has_more_after : ctx.has_more_after,
    };
    renderAll();
  } catch (e) {
    if (detailGuard.isCurrent(token)) setStatus(describeError(e), true);
  }
}

function closeDetail(): void {
  detailGuard.invalidate(); // supersede any in-flight load so it cannot re-open the pane
  const snapshot = state.resultsSnapshot;
  state.resultsSnapshot = null;
  state.detail = null;
  state.detailEntries = [];
  const restored = restoreIndices(state.hits, snapshot);
  state.activeIndex = restored.activeIndex;
  state.selectedId = restored.selectedId;
  renderAll();
  // Restore the scroll position after the rows exist again.
  if (snapshot) resultsList.scrollTop = snapshot.scrollTop;
  workspace.readingClosed();
}

// ---------------------------------------------------------------------------
// Copy / link / selection export (from results and the reading pane)
// ---------------------------------------------------------------------------

async function copyText(text: string, label: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    setStatus(`${label} copied to clipboard.`);
  } catch {
    setStatus(`Could not access the clipboard for ${label}.`, true);
  }
}

function copyMessage(): void {
  if (state.detail) void copyText(messageToText(state.detail.anchor), "Message");
}

function copyLink(): void {
  if (state.detail) void copyText(messageLink(state.detail.anchor.id), "Message link");
}

async function exportSelection(ids: number[]): Promise<void> {
  if (ids.length === 0) {
    selectionStatus.textContent = "Check result rows first, then export them.";
    return;
  }
  const format = selectionFormatSelect.value === "json" ? "json" : "text";
  const ext = format === "json" ? "json" : "txt";
  const path = await save({
    title: "Export selection",
    defaultPath: `mailchat-selection.${ext}`,
    filters: [
      format === "json"
        ? { name: "JSON file", extensions: ["json"] }
        : { name: "Text file", extensions: ["txt"] },
    ],
  });
  if (typeof path !== "string") return;
  try {
    const summary = await ipc.exportMessagesV2({ ids, format, path });
    selectionStatus.textContent =
      `Exported ${summary.written} message${summary.written === 1 ? "" : "s"}` +
      (summary.missing > 0
        ? ` (${summary.missing} no longer in the vault and skipped)`
        : "") +
      ".";
  } catch (e) {
    selectionStatus.textContent = describeError(e);
  }
}

function exportSelectionFromResults(): void {
  // Every checked id exports — across pages too (M3.2 fix contract) —
  // with the visible page's checked rows first, in results order, then
  // the remaining checked ids in check order.
  const visible = selectionIds(state.hits, state.selection);
  const rest = [...state.selection].filter((id) => !visible.includes(id));
  void exportSelection([...visible, ...rest]);
}

function exportSingleMessage(): void {
  if (state.detail) void exportSelection([state.detail.anchor.id]);
}

// Back-to-results (medium/compact): the results area is restored with its
// query, filters, page, selection, and scroll intact.
workspace.onBackToResults(() => closeDetail());

// ---------------------------------------------------------------------------
// Keyboard navigation (search-shortcuts live in workspace.ts)
// ---------------------------------------------------------------------------

document.addEventListener("keydown", (event) => {
  const target = event.target as HTMLElement;
  const inEditable =
    target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable;

  if (event.key === "Escape") {
    // Escape returns/closes without erasing the query: first the compact
    // navigation drawer, then the reading pane, then the selection.
    if (workspace.isNavOpen()) {
      workspace.closeNav();
      event.preventDefault();
    } else if (state.detail) {
      closeDetail();
      event.preventDefault();
    } else if (state.activeIndex !== -1) {
      state.activeIndex = -1;
      renderAll();
      event.preventDefault();
    }
    return;
  }

  if (target instanceof Element && target.closest("button, select, a")) return;

  if (state.detail) {
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      const dir = event.key === "ArrowUp" ? -1 : 1;
      const entries = state.detailEntries;
      const at = entries.findIndex((e) => e.current);
      const target = entries[at + dir];
      if (target) {
        void openDetail(target.id).then(() => workspace.refocusCurrentThreadRow());
      }
      event.preventDefault();
    }
    return;
  }

  if (inEditable) return;

  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    if (state.hits.length === 0) return;
    const rowFocused = resultsList.contains(document.activeElement);
    const delta = event.key === "ArrowDown" ? 1 : -1;
    state.activeIndex = moveIndex(state.activeIndex, state.hits.length, delta);
    renderAll();
    const activeRow = resultsList.querySelector<HTMLElement>(".hit-item.active");
    activeRow?.scrollIntoView({ block: "nearest" });
    if (rowFocused) activeRow?.focus();
    event.preventDefault();
  } else if (event.key === "Enter") {
    if (state.activeIndex >= 0 && state.hits[state.activeIndex]) {
      void openDetail(state.hits[state.activeIndex].id);
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
    const summary = await ipc.importMbox(path);
    const failedPart = summary.failed > 0 ? `, ${summary.failed} failed to parse` : "";
    importStatus.textContent =
      `Imported ${summary.inserted} new, ${summary.skipped} already present` +
      `${failedPart} (of ${summary.seen} in ${summary.source}).`;
    await refreshStats();
    void refreshImportScopes();
    void refreshSources();
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
function describeMedia(media: ChatImportOutcome["media"]): string {
  if (!media) return "";
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
          name: "Chat export (WhatsApp .txt, Telegram result.json, Signal .json, Discord messages.json, or a .zip of any of these)",
          extensions: ["txt", "json", "zip"],
        },
      ],
    });
    if (typeof picked !== "string") {
      return; // cancelled
    }
    const extractMedia = chatMediaToggle.checked;
    importStatus.textContent = `Importing ${picked}${extractMedia ? " (extracting media)" : ""}…`;
    const outcome = await ipc.importChatExport(picked, extractMedia);
    const summary = outcome.summary;
    const failedPart = summary.failed > 0 ? `, ${summary.failed} failed to parse` : "";
    importStatus.textContent =
      `Imported ${summary.inserted} new, ${summary.skipped} already present` +
      `${failedPart} (of ${summary.seen} in ${summary.source}).` +
      describeMedia(outcome.media);
    await refreshStats();
    void refreshImportScopes();
    void refreshSources();
    void refreshEmbedStatus();
    void refreshDashboard();
    if (outcome.media) void refreshMediaUsage(); // media landed in the store
  } catch (e) {
    importStatus.textContent = `Import failed: ${e}`;
  } finally {
    chatImportButton.disabled = false;
  }
}

async function seedDemo(): Promise<void> {
  seedButton.disabled = true;
  try {
    const summary = await ipc.seedDemo();
    importStatus.textContent = `Demo messages: ${summary.inserted} new, ${summary.skipped} already present.`;
    await refreshStats();
    void refreshImportScopes();
    void refreshSources();
    void refreshEmbedStatus();
    void refreshDashboard();
  } catch (e) {
    importStatus.textContent = `Seeding failed: ${e}`;
  } finally {
    seedButton.disabled = false;
  }
}

// ---------------------------------------------------------------------------
// IMAP sync (app password or Google sign-in)
// ---------------------------------------------------------------------------

function imapAuthMode(): ipc.ImapAuthMode {
  return imapAuthSelect.value === "oauth" ? "oauth" : "password";
}

/** Show only the inputs the selected auth mode needs, and refresh the
 * sign-in hint (Google sign-in only). */
function applyImapAuthMode(): void {
  const oauth = imapAuthMode() === "oauth";
  imapPassInput.hidden = oauth;
  imapOauthButton.hidden = !oauth;
  imapOauthHint.textContent = "";
  if (oauth) {
    void refreshOauthHint();
  }
}

async function refreshOauthHint(): Promise<void> {
  const username = imapUserInput.value.trim();
  if (!username) {
    imapOauthHint.textContent = "Enter your Gmail address, then sign in.";
    return;
  }
  try {
    const status = await ipc.oauthStatus({
      host: imapHostInput.value.trim(),
      username,
    });
    if (!status.configured) {
      imapOauthHint.textContent =
        "Gmail sign-in is not configured — set MAILCHAT_GMAIL_CLIENT_ID and MAILCHAT_GMAIL_CLIENT_SECRET (see README → IMAP sync).";
    } else if (status.signed_in) {
      imapOauthHint.textContent = `Signed in as ${username} — Sync uses Google sign-in; no password needed.`;
    } else {
      imapOauthHint.textContent =
        "Not signed in yet — click “Sign in with Google…” (the browser opens; consent once).";
    }
  } catch {
    imapOauthHint.textContent = "";
  }
}

async function oauthLogin(): Promise<void> {
  const host = imapHostInput.value.trim();
  const username = imapUserInput.value.trim();
  if (!username) {
    imapStatus.textContent = "Enter your Gmail address first.";
    return;
  }
  imapOauthButton.disabled = true;
  imapStatus.textContent =
    "Sign-in started — complete it in the browser window that just opened…";
  try {
    const outcome = await ipc.oauthGmailLogin({ host, username });
    const store =
      outcome.saved_to === "memory"
        ? " Refresh token kept in memory only for this session (no OS keyring available)."
        : " Refresh token stored in the OS keyring.";
    imapStatus.textContent = `Signed in as ${outcome.username}.${store}`;
    await refreshOauthHint();
  } catch (e) {
    imapStatus.textContent = `Sign-in failed: ${e}`;
  } finally {
    imapOauthButton.disabled = false;
  }
}

function renderImapSyncOutcome(outcome: ipc.ImapSyncOutcome): void {
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
}

async function syncImap(): Promise<void> {
  const host = imapHostInput.value.trim();
  const username = imapUserInput.value.trim();
  const port = Number.parseInt(imapPortInput.value, 10);
  const auth = imapAuthMode();
  // The password field is irrelevant (and hidden) in OAuth mode; never
  // send a stale value along.
  const password = auth === "password" ? imapPassInput.value : "";
  const folders = imapFoldersInput.value
    .split(",")
    .map((f) => f.trim())
    .filter((f) => f.length > 0);
  if (!username || (auth === "password" && !host)) {
    imapStatus.textContent =
      auth === "password"
        ? "Enter the IMAP host and your username first."
        : "Enter your Gmail address first.";
    return;
  }
  imapSyncButton.disabled = true;
  imapStatus.textContent = `Syncing ${username} over IMAP… (first sync of a large mailbox can take a while)`;
  try {
    const outcome = await ipc.syncImap({
      host,
      port: Number.isNaN(port) ? null : port,
      username,
      password,
      folders: folders.length > 0 ? folders : null,
      auth,
    });
    renderImapSyncOutcome(outcome);
    imapPassInput.value = "";
    // New mail from the manual sync also feeds the notification layer
    // (silent while this window is focused, like every cycle).
    notifier.onSyncOutcome(outcome.folders);
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
// Auto-sync: the opt-in periodic scheduler (M2.4)
// ---------------------------------------------------------------------------

function renderAutoSync(view: SyncScheduleView): void {
  autoSyncToggle.checked = view.enabled;
  // Preserve valid intervals configured outside the standard choices so
  // toggling the schedule cannot silently replace a saved interval.
  autoSyncInterval.querySelectorAll("option[data-custom-interval]").forEach((o) => o.remove());
  if (!isIntervalChoice(view.interval_minutes)) {
    const option = document.createElement("option");
    option.value = String(view.interval_minutes);
    option.textContent = `${view.interval_minutes} min`;
    option.dataset.customInterval = "true";
    autoSyncInterval.append(option);
  }
  autoSyncInterval.value = String(view.interval_minutes);
  autoSyncStatus.textContent = autoSyncStatusText(view);
}

/** Apply a schedule change: the toggle wins for on/off; the interval
 * select rides along whenever it is part of the change. */
async function setAutoSchedule(
  enabled: boolean | null,
  intervalMinutes: number | null,
): Promise<void> {
  try {
    const view = await ipc.syncScheduleSet(enabled, intervalMinutes);
    renderAutoSync(view);
  } catch (e) {
    autoSyncStatus.textContent = `Auto-sync change failed: ${e}`;
  }
}

function onAutoSyncToggle(): void {
  const enabled = autoSyncToggle.checked;
  const minutes = Number.parseInt(autoSyncInterval.value, 10);
  void setAutoSchedule(enabled, Number.isNaN(minutes) ? null : minutes);
}

function onAutoSyncIntervalChange(): void {
  // Changing the interval while off just remembers the choice; while
  // on, it re-arms the schedule at the new interval.
  if (!autoSyncToggle.checked) return;
  const minutes = Number.parseInt(autoSyncInterval.value, 10);
  if (Number.isNaN(minutes)) return;
  void setAutoSchedule(null, minutes);
}

async function refreshAutoSync(): Promise<void> {
  try {
    renderAutoSync(await ipc.syncScheduleGet());
  } catch {
    // Leave whatever was rendered; the next scheduler-status event fixes it.
  }
}

// The backend pushes a fresh view on every scheduler transition; a
// finished scheduled round also refreshes the source stats like any job.
void ipc.onEvent<SyncScheduleView>("scheduler-status", (payload) => {
  renderAutoSync(payload);
});

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
    renderWatchStatus(await ipc.watchStatus());
  } catch {
    // Leave whatever was rendered; the next watch-status event fixes it.
  }
}

/** Returns whether the folder is now watched. */
async function addWatchFolder(path: string): Promise<boolean> {
  watchStatus.textContent = `Adding ${path}…`;
  try {
    renderWatchStatus(await ipc.watchAddFolder(path));
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
    renderWatchStatus(await ipc.watchRemoveFolder(path));
    await refreshStats();
  } catch (e) {
    watchStatus.textContent = `Could not remove folder: ${e}`;
  }
}

// The backend pushes a fresh snapshot on every watcher change; a newest
// "imported" event also refreshes the source chips/stats.
void ipc.onEvent<WatchSnapshot>("watch-status", (payload) => {
  renderWatchStatus(payload);
  notifier.onWatchSnapshot(payload.events);
  if (payload.events[0]?.kind === "imported") {
    void refreshStats();
  }
});

// ---------------------------------------------------------------------------
// Background jobs (v2 protocol: live events + cursor polling recovery)
// ---------------------------------------------------------------------------

const jobs = newJobsTracker();

function renderJobs(): void {
  jobsStatus.textContent = jobsStatusText(jobs);
  renderJobsList(jobsList, jobs);
  // The embedding button tracks its own job: disabled while queued or
  // running, re-enabled once no embedding job is active.
  embedButton.disabled = hasActiveJob(jobs, "embed_messages");
  if (!embedButton.disabled && embedStatus.textContent.startsWith("Embedding…")) {
    embedStatus.textContent = "";
  }
}

/** Poll `job_events_since(cursor)` — the recovery path for missed window
 * events (webview reload, a gap in seq). The page carries both the missed
 * events and a fresh records snapshot. */
async function syncJobs(): Promise<void> {
  try {
    const page = await ipc.jobEventsSince(jobs.cursor);
    applyJobsPage(jobs, page);
    renderJobs();
  } catch {
    // Backend not ready (early boot) — the next job-event retry polls.
    jobs.needsPoll = true;
  }
}

/** A terminal event for an archive-changing kind refreshes the panels. */
function refreshAfterJobEvent(event: JobEvent): void {
  if (
    event.type === "succeeded" ||
    event.type === "failed" ||
    event.type === "cancelled"
  ) {
    void refreshStats();
    void refreshDashboard();
    void refreshEmbedStatus();
    void refreshSources();
    void refreshImportScopes();
    if (event.type === "succeeded" && state.hits.length > 0) {
      // The archive may have changed under the visible results: offer a
      // refresh, never silently move rows (A07).
      state.refreshNeeded = true;
      renderAll();
    }
  }
}

void ipc.onEvent<JobEvent>("job-event", (payload) => {
  applyJobEvent(jobs, payload);
  // After applyJobEvent the tracker knows the kind even for a job whose
  // `submitted` event predates this listener (boot recovery).
  notifier.onJobEvent(payload, jobs.records.get(payload.job)?.kind);
  refreshAfterJobEvent(payload);
  renderJobs();
  if (jobs.needsPoll) {
    void syncJobs(); // seq gap: recover the missed events by polling
  }
});

// Boot recovery: fetch everything retained (also covers a webview reload
// that happened while jobs were running — the cursor restarts at 0).
void syncJobs();

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
    const summary = await ipc.exportSearch({
      query: {
        text: state.filters.text,
        sources: state.sources,
        author: state.filters.author,
        from_date: state.filters.fromDate,
        to_date: state.filters.toDate,
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

/** Export the whole chat thread of the message open in the reading pane. */
async function exportChat(): Promise<void> {
  const detail = state.detail;
  if (!detail) return;
  exportStatus.textContent = "";
  const format = exportFormatSelect.value;
  const path = await pickExportPath(format);
  if (path === null) return; // cancelled
  exportStatus.textContent = "Exporting chat…";
  try {
    const summary = await ipc.exportChat({
      id: detail.anchor.id,
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
// Media objects (M4.3): explicit export / external open from the
// reading pane, and the Settings media-store report
// ---------------------------------------------------------------------------

/** Save a byte-exact copy of one preserved media object at a
 * user-picked path — explicit action, never bundled with a message
 * export. */
async function exportMedia(attachment: AttachmentSummary): Promise<void> {
  const name = objectNameOf(attachment.path);
  if (name === null) {
    setStatus("This attachment is not preserved in the vault.", true);
    return;
  }
  const ext = name.split(".").pop() ?? "bin";
  const picked = await save({
    title: "Save a copy of the media file",
    defaultPath: attachment.name || name,
    filters: [{ name: "Media file", extensions: [ext] }],
  });
  if (typeof picked !== "string") return; // cancelled
  try {
    const outcome = await ipc.mediaExport({ name, path: picked });
    setStatus(`Saved a copy (${humanSize(outcome.size)}) to ${picked}.`);
  } catch (e) {
    setStatus(`Media export failed: ${e}`, true);
  }
}

/** Open one preserved media object with the OS default application.
 * The backend opens a disposable temp copy — the store is never handed
 * to an external writer. */
async function openMediaExternal(attachment: AttachmentSummary): Promise<void> {
  const name = objectNameOf(attachment.path);
  if (name === null) {
    setStatus("This attachment is not preserved in the vault.", true);
    return;
  }
  mediaStoreStatus.textContent = "";
  try {
    await ipc.mediaOpenExternal({ name, displayName: attachment.name });
    setStatus(`Opening ${attachment.name || name} externally…`);
  } catch (e) {
    setStatus(`Open failed: ${e}`, true);
  }
}

/** Load and render the media-store disk usage report (Settings). */
async function refreshMediaUsage(): Promise<void> {
  try {
    const usage: MediaUsageView = await ipc.mediaUsage();
    renderMediaUsage(mediaUsagePanel, usage);
  } catch (e) {
    mediaStoreStatus.textContent = `Media store report failed: ${e}`;
  }
}

/** Run the M4.1 reconciliation (verify hashes) and summarize it. */
async function reconcileMediaStore(): Promise<void> {
  mediaReconcileButton.disabled = true;
  mediaStoreStatus.textContent = "Reconciling (re-hashing every object)…";
  try {
    const report = await ipc.mediaReconcile();
    const clean =
      report.missing.length === 0 &&
      report.orphans.length === 0 &&
      report.corrupt.length === 0 &&
      report.stale_staging.length === 0 &&
      report.foreign.length === 0;
    mediaStoreStatus.textContent = clean
      ? `Store is clean: ${report.objects} objects match ${report.referenced} references.`
      : `Reconcile: ${report.objects} objects · ${report.missing.length} missing · ` +
        `${report.orphans.length} unreferenced · ${report.corrupt.length} corrupt · ` +
        `${report.stale_staging.length} staging · ${report.foreign.length} foreign files.`;
  } catch (e) {
    mediaStoreStatus.textContent = `Reconcile failed: ${e}`;
  } finally {
    mediaReconcileButton.disabled = false;
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
// M5 consent wiring: allow grants session consent and immediately re-asks
// the pending question with the network leg enabled; keep-local and revoke
// keep everything on the fully local path.
consentAllowButton.addEventListener("click", () => {
  aiConsentGranted = true;
  updateConsentUi();
  hideConsentPrompt();
  const question = askInput.value.trim();
  if (question) void doAsk(question, true);
});
consentLocalButton.addEventListener("click", hideConsentPrompt);
consentRevokeButton.addEventListener("click", () => {
  aiConsentGranted = false;
  updateConsentUi();
});
authorInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void runSearch(0);
});
fromDateInput.addEventListener("change", () => void runSearch(0));
toDateInput.addEventListener("change", () => void runSearch(0));
sortOrderSelect.addEventListener("change", () => void runSearch(0));
hasAttachmentSelect.addEventListener("change", () => void runSearch(0));
importScopeSelect.addEventListener("change", () => void runSearch(0));
exportSelectionButton.addEventListener("click", exportSelectionFromResults);
clearFiltersButton.addEventListener("click", clearFilters);
importButton.addEventListener("click", () => void importMbox());
chatImportButton.addEventListener("click", () => void importChatExport());
seedButton.addEventListener("click", () => void seedDemo());
imapAuthSelect.addEventListener("change", applyImapAuthMode);
imapUserInput.addEventListener("change", () => {
  if (imapAuthMode() === "oauth") void refreshOauthHint();
});
imapOauthButton.addEventListener("click", () => void oauthLogin());
imapSyncButton.addEventListener("click", () => void syncImap());
autoSyncToggle.addEventListener("change", onAutoSyncToggle);
autoSyncInterval.addEventListener("change", onAutoSyncIntervalChange);
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
mediaRefreshButton.addEventListener("click", () => void refreshMediaUsage());
mediaReconcileButton.addEventListener("click", () => void reconcileMediaStore());
// The media report loads when Settings is opened (and after imports that
// extracted media) — not eagerly at startup.
workspace.store.subscribe((current) => {
  if (current.view === "settings") void refreshMediaUsage();
});
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
void refreshSources();
void refreshImportScopes();
void refreshStats();
void refreshDashboard();
void refreshLlm();
void refreshEmbedStatus();
void refreshWatchStatus();
void refreshAutoSync();
void loadSettings();

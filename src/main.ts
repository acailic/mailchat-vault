// Application wiring: composes the explicit state (src/state.ts), IPC
// (src/ipc.ts), and workspace shell (src/workspace.ts) modules with the
// panel builders (render/viewmodel/settings/…). Every panel keeps its
// last-request-wins guard so a late response can never overwrite newer UI
// state (the stale-response contract covered by main.test.ts).

import "./styles/tokens.css";
import "./styles/app.css";

import { open, save } from "@tauri-apps/plugin-dialog";
import { folderSummaryLine, formatSentAt, humanSize, snippet } from "./format";
import {
  objectNameOf,
  renderMediaUsage,
  type MediaUsageView,
} from "./media";
import {
  backupDoneLine,
  canCommit,
  lastBackupLine,
  noBackupLine,
  previewLine,
  recoveryLine,
  renderRecoverySlots,
  renderRestoreFlow,
  restoreDoneLine,
  vaultOriginLine,
  gateBusy,
  switchStopped,
  type BackupSummaryView,
  type RestoreFlow,
  type RestorePreviewView,
} from "./backupview";
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
  type SettingsSection,
  type UiState,
} from "./state";
import { initWorkspace, type WorkspaceController } from "./workspace";
import type {
  AccountView,
  ChatImportOutcome,
  EmbeddingStatus,
  ExportSummary,
  IngestSummary,
  JobEvent,
  MediaSummary,
  Message,
  SourceInstance,
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
import {
  passwordStoreNote,
  renderSourcesList,
  accountTitle,
  sourceChipRows,
  syncFailureText,
  syncNowRequest,
  type SyncNowRequest,
} from "./accountsview";
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
const importPickButton = document.querySelector<HTMLButtonElement>("#import-pick-btn")!;
const importFolderButton = document.querySelector<HTMLButtonElement>("#import-folder-btn")!;
const chatMediaToggle = document.querySelector<HTMLInputElement>("#chat-media")!;
const importPreviewEl = document.querySelector<HTMLElement>("#import-preview")!;
const importStatus = document.querySelector<HTMLElement>("#import-status")!;
const importFollowup = document.querySelector<HTMLElement>("#import-followup")!;
const sourcesAddButton = document.querySelector<HTMLButtonElement>("#sources-add-btn")!;
const sourcesAutomaticButton = document.querySelector<HTMLButtonElement>("#sources-automatic-btn")!;
const addChoiceImportButton = document.querySelector<HTMLButtonElement>("#add-choice-import")!;
const addChoiceEmailButton = document.querySelector<HTMLButtonElement>("#add-choice-email")!;
const addBackButton = document.querySelector<HTMLButtonElement>("#add-back-btn")!;
const importBackButton = document.querySelector<HTMLButtonElement>("#import-back-btn")!;
const emailBackButton = document.querySelector<HTMLButtonElement>("#email-back-btn")!;
const automaticBackButton = document.querySelector<HTMLButtonElement>("#automatic-back-btn")!;
const sourcesAutomaticHeading = document.querySelector<HTMLElement>("#sources-automatic-heading")!;
const imapPassLabel = document.querySelector<HTMLElement>("#imap-pass-label")!;
const demoBanner = document.querySelector<HTMLElement>("#demo-banner")!;
const demoLeaveButton = document.querySelector<HTMLButtonElement>("#demo-leave-btn")!;
const firstRunCard = document.querySelector<HTMLElement>("#first-run")!;
const firstRunImportButton = document.querySelector<HTMLButtonElement>("#first-run-import-btn")!;
const firstRunAccountButton = document.querySelector<HTMLButtonElement>("#first-run-account-btn")!;
const firstRunDemoButton = document.querySelector<HTMLButtonElement>("#first-run-demo-btn")!;
const exportControls = document.querySelector<HTMLElement>("#export-controls")!;
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
const sourcesStatus = document.querySelector<HTMLElement>("#sources-status")!;
const sourcesListEl = document.querySelector<HTMLUListElement>("#sources-list")!;
const sourcesRefreshButton = document.querySelector<HTMLButtonElement>("#sources-refresh")!;
const jobsStatus = document.querySelector<HTMLElement>("#jobs-status")!;
const jobsList = document.querySelector<HTMLUListElement>("#jobs-list")!;
const dashboard = document.querySelector<HTMLElement>("#dashboard")!;
const dashboardStatus = document.querySelector<HTMLElement>("#dashboard-status")!;
const dashboardRefreshButton = document.querySelector<HTMLButtonElement>("#stats-refresh")!;
const exportFormatSelect = document.querySelector<HTMLSelectElement>("#export-format")!;
// Source instances (id ↔ display label) for the v2 source filter.
let sourceInstances: SourceInstance[] = [];
/** Account rows (id-keyed identity detail) for chip disambiguation and
 * the Sources list; refreshed with `refreshAccounts`. */
let accountsCache: AccountView[] = [];
/** The last successful `stats` response (label-keyed counts for chips). */
let lastStats: { by_source: { source: string; count: number }[] } | null = null;
/** id → disambiguated chip label (shared by nav chips and filter chips). */
let sourceChipLabels = new Map<string, string>();
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
/** M7 keep-alive toggles: keep running in the background (close hides to
 * the tray) and start at login (OS registration). Both ship off and are
 * rendered from the saved settings like every other option. */
const keepaliveToggle = document.querySelector<HTMLInputElement>("#keepalive-toggle")!;
const autostartToggle = document.querySelector<HTMLInputElement>("#autostart-toggle")!;
const settingsSaveGeneralButton = document.querySelector<HTMLButtonElement>("#settings-save-general")!;
const settingsSaveAiButton = document.querySelector<HTMLButtonElement>("#settings-save-ai")!;
const backupStatus = document.querySelector<HTMLElement>("#backup-status")!;
const backupCreateButton = document.querySelector<HTMLButtonElement>("#backup-create-btn")!;
const restorePickButton = document.querySelector<HTMLButtonElement>("#restore-pick-btn")!;
const restoreFlowPanel = document.querySelector<HTMLElement>("#restore-flow")!;
const vaultOriginEl = document.querySelector<HTMLElement>("#vault-origin")!;
const vaultLocationEl = document.querySelector<HTMLElement>("#vault-location")!;
const recoveryLineEl = document.querySelector<HTMLElement>("#recovery-line")!;
const recoveryRefreshButton = document.querySelector<HTMLButtonElement>("#recovery-refresh-btn")!;
const recoverySlotsEl = document.querySelector<HTMLElement>("#recovery-slots")!;

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
const accountsGuard = new SequenceGuard();

/** The last `llm_status` response: powers the Ask provider menu and the
 * privacy note for the current selection. */
let llmStatus: LlmStatus | null = null;

/** M5 AI consent — session-scoped, deliberately in-memory only: it starts
 * `false` on every app start (a new session asks anew) and is never written
 * to any persistent storage, so consent is remembered per session, not
 * forever. Once granted, asks with a network provider go out; `Revoke`
 * resets it at any time. The state is always visible in the UI. */
let aiConsentGranted = false;

/** U02: whether the isolated sample archive is open (backend-tracked —
 * survives a webview reload; refreshed from `demo_status` at boot). */
let demoActive = false;

/** U02: the personal vault's message count from the last `stats`
 * refresh — drives the first-run card (an empty archive offers the
 * first source instead of a dead search box). While the demo is open
 * this still reflects the personal vault underneath. */
let archiveTotal = 0;

/** Whether the initial newest-first browse already ran once this page
 * (U04: "All messages" shows the archive immediately, not after the
 * first manual search). */
let initialBrowseDone = false;

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
      source: (id) =>
        sourceChipLabels.get(id) ?? sourceInstances.find((s) => s.id === id)?.label ?? id,
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

/** Render the nav source chips from the current caches (ids, labels,
 * counts). Called after stats, sources, or accounts refresh. Also
 * refreshes the id → label map used by the applied-filter chips. */
function renderSourceChipsNow(): void {
  const rows = sourceChipRows(sourceInstances, accountsCache, lastStats?.by_source ?? []);
  sourceChipLabels = new Map(rows.map((r) => [r.id, r.label]));
  renderSourceChips(sourceChips, {
    rows,
    selected: state.sources,
    onToggle: (id) => {
      state.sources = state.sources.includes(id)
        ? state.sources.filter((s) => s !== id)
        : [...state.sources, id];
      // Source scoping is a search action: show the results it produced.
      workspace.store.setView("workspace");
      renderSourceChipsNow();
      void runSearch(0);
    },
  });
}

/** Load the canonical source instances (id ↔ label) for the v2 filter. */
async function refreshSources(): Promise<void> {
  try {
    sourceInstances = await ipc.sourcesList();
  } catch {
    sourceInstances = []; // ids resolve on retry; chips follow the cache
  }
  renderSourceChipsNow();
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
  settingsRendered = true;
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
  keepaliveToggle.checked = view.keep_running_in_background;
  autostartToggle.checked = view.start_at_login;

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

/** Whether the form has been rendered from a real settings view at least
 * once; before that its values are HTML defaults that must not be saved
 * back over the persisted ones. */
let settingsRendered = false;

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
    keep_running_in_background: keepaliveToggle.checked,
    start_at_login: autostartToggle.checked,
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
  if (!settingsRendered) return;
  const payload = collectSettings();
  const previousDefault = llmStatus?.default_provider ?? null;
  settingsSaveGeneralButton.disabled = true;
  settingsSaveAiButton.disabled = true;
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
    settingsSaveGeneralButton.disabled = false;
    settingsSaveAiButton.disabled = false;
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
    lastStats = stats;
    archiveTotal = demoActive ? archiveTotal : stats.total;
    const parts = stats.by_source.map((s) => `${s.source}: ${s.count}`).join(", ");
    statsLabel.textContent =
      stats.total === 0 ? "empty archive" : `${stats.total} messages (${parts})`;
    renderSourceChipsNow();
    updateVaultChrome();
    maybeInitialBrowse();
  } catch {
    if (statsGuard.isCurrent(token)) {
      statsLabel.textContent = "";
      // Without counts the first-run/browse choice is unknown; the
      // search box stays the honest entry point.
      if (!initialBrowseDone) {
        setStatus("Type something to search, or set a filter to browse.");
      }
    }
  }
}

/** U04: once the boot stats land, browse newest-first immediately —
 * "All messages" shows the archive without a first manual search. An
 * empty personal archive instead gets the first-run status line (U02). */
function maybeInitialBrowse(): void {
  if (initialBrowseDone) return;
  if (demoActive) return;
  if (archiveTotal === 0) {
    setStatus("Your archive is empty — add a source to get started.");
    return;
  }
  initialBrowseDone = true;
  void runSearch(0);
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

function readFilters(text: string): SearchFilters {
  // Source selection is by stable instance id (chips and the Sources
  // list both key on source_id); an unknown id matches nothing — visible
  // rather than silently broad.
  return {
    text,
    sourceIds: [...state.sources],
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
  state.sources = [...new Set(f.sourceIds)];
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
  writeFilters(withoutChip(state.filters, chip));
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
  if (!text && !hasActiveFilters(filters) && !demoActive && archiveTotal === 0) {
    // Empty personal archive: the first-run card owns the empty state
    // (U02) — a dead search prompt is not the first thing a new user
    // should see.
    state.filters = filters;
    state.hits = [];
    state.total = 0;
    state.offset = 0;
    state.activeIndex = -1;
    state.selectedId = null;
    state.selection.clear();
    state.pageRevision = null;
    state.refreshNeeded = false;
    setStatus("Your archive is empty — add a source to get started.");
    renderAll();
    return;
  }
  // U04: empty text browses newest-first (the backend enforces the
  // ordering); a filter or text narrows it. No dead ends.
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
// Import flow (U03: native pick → bounded preview → confirm)
// ---------------------------------------------------------------------------

import {
  formatLabel,
  hideImportPreview,
  renderImportPreview,
} from "./importview";

/** The pick currently awaiting confirmation, if any. */
let pendingImport: { path: string; kind: string } | null = null;
/** An import is running (guards double-clicks). */
let importBusy = false;

/** Map a backend import error to something a first-time importer can
 * act on — no raw errno, no connector vocabulary. */
function friendlyImportError(e: unknown, path: string): string {
  const text = e instanceof Error ? e.message : String(e);
  if (/not found|os error 2/i.test(text)) {
    return `Could not read ${path} — the file may have moved or been deleted. Pick it again.`;
  }
  if (/unrecognized|unsupported/i.test(text)) {
    return `${text} Supported inputs: an .mbox email archive, a WhatsApp .txt, a Telegram result.json, a Signal .json, a Discord messages.json, or a folder/.zip containing these.`;
  }
  return `Import failed: ${text}`;
}

/** Open the native picker for one file (or an export folder) and show
 * the bounded preview of what was picked. */
async function pickImportInput(directory: boolean): Promise<void> {
  if (importBusy || demoActive) return;
  const button = directory ? importFolderButton : importPickButton;
  button.disabled = true;
  try {
    const picked = await open(
      directory
        ? {
            multiple: false,
            directory: true,
            title: "Import an export folder",
          }
        : {
            multiple: false,
            title: "Import a file",
            filters: [
              {
                name: "Email or chat export (.mbox, WhatsApp .txt, Telegram/Signal/Discord .json, or a .zip)",
                extensions: ["mbox", "txt", "json", "zip"],
              },
            ],
          },
    );
    if (typeof picked !== "string") {
      return; // cancelled — no changes, no preview
    }
    await showPreview(picked);
  } finally {
    button.disabled = false;
  }
}

/** Preview a picked path: bounded scan, nothing stored. */
async function showPreview(path: string): Promise<void> {
  importStatus.textContent = `Reading ${path}…`;
  importFollowup.hidden = true;
  try {
    const preview = await ipc.previewImport(path);
    pendingImport = { path, kind: preview.kind };
    renderImportPreview(importPreviewEl, preview, {
      path,
      freeBytes: preview.free_bytes,
      busy: importBusy,
      onImport: () => void confirmImport(),
      onCancel: cancelPreview,
    });
    importStatus.textContent =
      preview.messages_seen === 0 && preview.parse_failures === 0
        ? `Nothing importable found in ${formatLabel(preview.kind)} input.`
        : "";
  } catch (e) {
    pendingImport = null;
    hideImportPreview(importPreviewEl);
    importStatus.textContent = friendlyImportError(e, path);
  }
}

/** Discard the pending preview: nothing was stored, nothing to undo. */
function cancelPreview(): void {
  pendingImport = null;
  hideImportPreview(importPreviewEl);
  importStatus.textContent = "";
}

/** Confirm the previewed import: dispatch to the bounded importer for
 * the detected format, then offer "View imported messages" scoped to
 * exactly this run. The preview parameter is the one whose confirm
 * button was clicked; the pending pick must still match it. */
async function confirmImport(): Promise<void> {
  const target = pendingImport;
  if (!target || importBusy) return;
  importBusy = true;
  const extractMedia = chatMediaToggle.checked;
  hideImportPreview(importPreviewEl);
  importStatus.textContent =
    `Importing ${target.path}` +
    (target.kind !== "mbox" && extractMedia ? " (extracting media)" : "") +
    "…";
  try {
    let summary: IngestSummary;
    let importId: string;
    let mediaSummary: MediaSummary | null = null;
    if (target.kind === "mbox") {
      const outcome = await ipc.importMbox(target.path);
      summary = outcome.summary;
      importId = outcome.import_id;
      mediaSummary = outcome.media;
    } else {
      const outcome = await ipc.importChatExport(target.path, extractMedia);
      summary = outcome.summary;
      importId = outcome.import_id;
      mediaSummary = outcome.media;
    }
    const failedPart = summary.failed > 0 ? `, ${summary.failed} failed to parse` : "";
    const mediaPart = describeMedia(mediaSummary);
    importStatus.textContent =
      `Imported ${summary.inserted} new, ${summary.skipped} already present` +
      `${failedPart} (of ${summary.seen} in ${summary.source}).` +
      mediaPart;
    showImportFollowup(importId, summary.inserted);
    initialBrowseDone = true; // the archive view is live from here on
    await refreshStats();
    void refreshImportScopes();
    void refreshSources();
    void refreshAccounts();
    void refreshEmbedStatus();
    void refreshDashboard();
    void runSearch(0); // newest-first browse, ready when the user returns
    if (mediaSummary) void refreshMediaUsage(); // media landed in the store
  } catch (e) {
    importStatus.textContent = friendlyImportError(e, target.path);
  } finally {
    importBusy = false;
    pendingImport = null;
  }
}

/** The post-import affordance: jump to the messages this run brought
 * in, scoped by the import-scope filter. */
function showImportFollowup(importId: string | undefined, inserted: number): void {
  importFollowup.replaceChildren();
  if (!importId || inserted === 0) {
    importFollowup.hidden = true;
    return;
  }
  const view = document.createElement("button");
  view.type = "button";
  view.id = "view-imported-btn";
  view.className = "primary";
  view.textContent = "View imported messages";
  view.addEventListener("click", () => {
    // The scope select may not list this run yet (its refresh is async
    // or returned nothing): add the option so the filter actually
    // selects this import, not silently "any import".
    if (![...importScopeSelect.options].some((o) => o.value === importId)) {
      const option = document.createElement("option");
      option.value = importId;
      option.textContent = `${importId} (${inserted} messages)`;
      importScopeSelect.append(option);
    }
    importScopeSelect.value = importId;
    workspace.store.setView("workspace");
    void runSearch(0);
  });
  importFollowup.append(view);
  importFollowup.hidden = false;
}

// ---------------------------------------------------------------------------
// Isolated sample archive (U02)
// ---------------------------------------------------------------------------

/** Refresh every vault-reading panel after switching between the
 * personal vault and the isolated sample archive. */
async function refreshAfterVaultSwitch(): Promise<void> {
  // Each vault has its own archive revision: reset the results state so
  // a page revision from the other vault can never read as stale data.
  // The query text goes with it — a search typed against one vault is
  // not a meaningful query against the other.
  searchInput.value = "";
  searchGuard.invalidate();
  detailGuard.invalidate();
  closeDetail();
  state.pageRevision = null;
  state.refreshNeeded = false;
  state.hits = [];
  state.total = 0;
  state.offset = 0;
  state.activeIndex = -1;
  state.selectedId = null;
  state.selection.clear();
  state.resultsSnapshot = null;
  await refreshSources();
  await refreshImportScopes();
  await refreshStats();
  void refreshDashboard();
  void refreshAccounts();
  void refreshEmbedStatus();
  updateVaultChrome();
  initialBrowseDone = demoActive || archiveTotal > 0;
  if (initialBrowseDone) {
    void runSearch(0);
  } else {
    // Left the demo onto an empty personal archive: the first-run card
    // owns the state, and the first import re-arms the initial browse.
    setStatus("Your archive is empty — add a source to get started.");
    renderAll();
  }
}

/** Open the isolated sample archive: reads route to a private in-memory
 * demo vault; the personal vault underneath is never touched. */
async function enterDemo(): Promise<void> {
  if (demoActive) return;
  firstRunDemoButton.disabled = true;
  try {
    await ipc.demoEnter();
    demoActive = true;
    workspace.store.setView("workspace");
    await refreshAfterVaultSwitch();
  } catch (e) {
    setStatus(`Could not open the sample archive: ${e}`, true);
  } finally {
    firstRunDemoButton.disabled = false;
  }
}

/** Leave the sample archive: drops the in-memory demo and returns to
 * the personal vault exactly as it was. */
async function leaveDemo(): Promise<void> {
  if (!demoActive) return;
  demoLeaveButton.disabled = true;
  try {
    await ipc.demoLeave();
    demoActive = false;
    await refreshAfterVaultSwitch();
  } catch (e) {
    setStatus(`Could not leave the sample archive: ${e}`, true);
  } finally {
    demoLeaveButton.disabled = false;
  }
}

/** Toggle the vault-dependent chrome: the demo banner, the first-run
 * card (an empty personal archive), and the source controls' demo
 * lockout (the backend refuses writes under the demo regardless). */
function updateVaultChrome(): void {
  const firstRun = !demoActive && archiveTotal === 0;
  firstRunCard.hidden = !firstRun;
  demoBanner.hidden = !demoActive;
  exportControls.hidden = firstRun;
  const locked = demoActive;
  for (const button of [sourcesAddButton, addChoiceImportButton, addChoiceEmailButton, importPickButton, importFolderButton, imapSyncButton, imapOauthButton, watchButton, watchAddButton]) {
    button.disabled = locked;
  }
}


/** "media: 2 stored, 1 deduped · 3 linked · 1 missing" with only the
 * non-zero parts; empty when nothing happened. The unlinked_* honesty
 * counters (M4.2) surface here too: stray files stored, budget drops,
 * and a failed tree walk are shown rather than swallowed. */
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
  if (media.unlinked > 0) {
    parts.push(
      media.unlinked_deduped > 0
        ? `${media.unlinked} unlinked (${media.unlinked_deduped} deduped)`
        : `${media.unlinked} unlinked`,
    );
  } else if (media.unlinked_deduped > 0) {
    parts.push(`${media.unlinked_deduped} unlinked (deduped)`);
  }
  add(media.unlinked_dropped, "dropped (budget)");
  if (media.unlinked_walk_failed > 0) parts.push("walk failed");
  return parts.length === 0 ? "" : ` Media: ${parts.join(", ")}.`;
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
  imapPassLabel.hidden = oauth;
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
  const lines = outcome.folders.map(folderSummaryLine);
  const note = passwordStoreNote(outcome.saved_to);
  imapStatus.textContent = lines.join(" · ") + (note ? ` ${note}` : "");
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
    void refreshAccounts();
  } catch (e) {
    // The classified, actionable failure (never a raw errno) — including
    // where an entered password already lives, because it is saved before
    // the connection is attempted.
    imapStatus.textContent = `Sync failed: ${syncFailureText(e)}`;
    // The failed attempt still registered the account (host/port/user
    // are kept — the user retypes nothing); refresh so the panel shows
    // the new row with its error state instead of a stale empty list.
    void refreshAccounts();
    await refreshStats();
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
// Sources list on the Sources home (listing, view messages, sync now,
// pause / credentials / disconnect behind the Details fold)
// ---------------------------------------------------------------------------

/** The panel status line: a one-line summary of what is listed. */
function describeAccountsStatus(accounts: AccountView[]): string {
  if (accounts.length === 0) return ""; // the list itself shows the empty state
  const imap = accounts.filter((a) => a.kind === "imap").length;
  const imports = accounts.length - imap;
  const parts = [`${accounts.length} account${accounts.length === 1 ? "" : "s"}`];
  if (imap > 0) parts.push(`${imap} email`);
  if (imports > 0) parts.push(`${imports} import${imports === 1 ? "" : "s"}`);
  return parts.join(" · ");
}

function renderAccountsPanel(accounts: AccountView[]): void {
  renderSourcesList(sourcesListEl, accounts, {
    onViewMessages: viewSourceMessages,
    onSyncNow: (account) => void syncAccountNow(account),
    onTogglePause: (account) => void toggleAccountPause(account),
    onUpdateCredentials: (account) => updateAccountCredentials(account),
    onForget: (account) => void forgetAccountCredentials(account),
    onDisconnect: (account) => void disconnectAccount(account),
  });
  sourcesStatus.textContent = describeAccountsStatus(accounts);
}

async function refreshAccounts(): Promise<void> {
  const token = accountsGuard.next();
  try {
    const accounts = await ipc.accountsList();
    if (!accountsGuard.isCurrent(token)) return; // a newer refresh superseded this response
    accountsCache = accounts;
    renderAccountsPanel(accounts);
    renderSourceChipsNow();
  } catch (e) {
    if (accountsGuard.isCurrent(token)) {
      sourcesListEl.replaceChildren();
      sourcesStatus.textContent = `Accounts unavailable: ${e}`;
    }
  }
}

/** The per-folder outcome of one account sync, as one line. */
function accountSyncOutcomeLine(account: AccountView, outcome: ipc.ImapSyncOutcome): string {
  const lines = outcome.folders.map(folderSummaryLine);
  const note = passwordStoreNote(outcome.saved_to);
  // Named by the identity the user knows (address · server), never the
  // stored `imap:host:port:user` connector label.
  return `Synced ${accountTitle(account)}: ${lines.join(" · ") || "no folders"}${note ? ` ${note}` : ""}`;
}

/** Sync one listed account now: `sync_imap` with the account's stored
 * identity and an empty password — the backend resolves the stored
 * keyring credential (or runs the Google sign-in refresh for OAuth). */
async function syncAccountNow(account: AccountView): Promise<void> {
  const request: SyncNowRequest | null = syncNowRequest(account);
  if (!request) {
    sourcesStatus.textContent = `${accountTitle(account)} is an import — nothing to sync.`;
    return;
  }
  sourcesStatus.textContent = `Syncing ${accountTitle(account)}… (first sync of a large mailbox can take a while)`;
  try {
    const outcome = await ipc.syncImap(request);
    await refreshStats();
    void refreshEmbedStatus();
    void refreshDashboard();
    await refreshAccounts();
    sourcesStatus.textContent = accountSyncOutcomeLine(account, outcome);
  } catch (e) {
    // Classified, actionable text (never a raw errno); the account row
    // itself carries the persisted error state after this refresh. The
    // refresh runs first so re-rendering the panel cannot overwrite the
    // failure line the user must see.
    await refreshAccounts();
    sourcesStatus.textContent = `Sync failed for ${accountTitle(account)}: ${syncFailureText(e)}`;
  }
}

/** Pause/resume one email account: per-source exclusion — coordinator
 * rounds skip it while the archive and cursors are kept. */
async function toggleAccountPause(account: AccountView): Promise<void> {
  const name = accountTitle(account);
  try {
    await ipc.sourceSetExcluded(account.source_id, !account.excluded);
    await refreshAccounts();
    sourcesStatus.textContent = account.excluded
      ? `${name} resumed — auto-sync rounds include it again.`
      : `${name} paused — auto-sync skips it; the archive and cursors are kept.`;
  } catch (e) {
    sourcesStatus.textContent = `Could not change ${name}: ${e}`;
  }
}

/** Forget one email account's credentials (keyring + session memory);
 * the archive, cursors, and configuration stay. */
async function forgetAccountCredentials(account: AccountView): Promise<void> {
  const name = accountTitle(account);
  try {
    const removed = await ipc.accountForgetCredentials(account.source_id);
    await refreshAccounts();
    sourcesStatus.textContent = removed
      ? `Forgot stored credentials of ${name} — the archive stays; enter a password or sign in with Google to sync again.`
      : `${name} had no stored credentials.`;
  } catch (e) {
    sourcesStatus.textContent = `Could not forget credentials of ${name}: ${e}`;
  }
}

/** Show one source's messages: scope the search to its stable id
 * (keeping the query text and other filters) and return to results. */
function viewSourceMessages(account: AccountView): void {
  state.sources = [account.source_id];
  renderSourceChipsNow();
  void runSearch(0);
  workspace.store.setView("workspace");
}

/** Reconnection from the source details: open the Connect email form
 * prefilled with the account's identity (folders joined for the input). */
function updateAccountCredentials(account: AccountView): void {
  if (!account.imap) return;
  imapHostInput.value = account.imap.host;
  imapPortInput.value = String(account.imap.port);
  imapUserInput.value = account.imap.username;
  imapFoldersInput.value = account.imap.folders.join(", ");
  imapPassInput.value = "";
  imapStatus.textContent = `Re-enter the password for ${accountTitle(account)}, then Connect and sync.`;
  workspace.store.setSourcesSection("email");
  imapPassInput.focus();
}

/** Disconnect: stop future sync (per-source exclusion) and remove the
 * stored credentials. The local archive always stays; deleting it is a
 * separate, not-yet-built action by decision. */
async function disconnectAccount(account: AccountView): Promise<void> {
  const name = accountTitle(account);
  try {
    await ipc.sourceSetExcluded(account.source_id, true);
    await ipc.accountForgetCredentials(account.source_id);
    await refreshAccounts();
    sourcesStatus.textContent =
      `Disconnected ${name} — future sync is stopped and its credentials were removed; the archive stays.`;
  } catch (e) {
    sourcesStatus.textContent = `Could not disconnect ${name}: ${e}`;
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
    void refreshAccounts();
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
  feedBackupJobEvent(payload);
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
// Storage & backup (M4.4): versioned containers, staged journaled
// restore, and the previous-vault recovery surface
// ---------------------------------------------------------------------------

/** The restore flow's state (see backupview.ts for the shape). */
let restoreFlow: RestoreFlow = { step: "idle" };
/** The job id of an in-flight backup (its success line lands in
 * `backupStatus` via the job-event stream). */
let pendingBackupJob: number | null = null;
/** The job id of an in-flight restore validation. */
let pendingRestoreJob: number | null = null;
/** Set once a restore/return switched the vault: the backend pool is
 * shut down and every further command fails until the app restarts. */
let vaultSwitched = false;
/** Set while a commit or return switch is running: the backend
 * serializes switches, and the UI must not offer a second one. */
let switchInFlight = false;

function renderRestorePanel(): void {
  renderRestoreFlow(
    restoreFlowPanel,
    restoreFlow,
    (archive) => void startRestoreValidation(archive),
    (preview) => void commitRestore(preview),
    () => {
      restoreFlow = { step: "idle" };
      pendingRestoreJob = null;
      renderRestorePanel();
    },
  );
}

/** Pick a destination and start the `backup_create` job (progress in
 * Activity; the success line lands here when it finishes). */
async function createBackup(): Promise<void> {
  if (vaultSwitched || switchInFlight) return;
  const today = new Date().toISOString().slice(0, 10);
  const picked = await save({
    title: "Create a vault backup",
    defaultPath: `mailchat-vault-${today}.mcbackup`,
    filters: [{ name: "Vault backup", extensions: ["mcbackup"] }],
  });
  if (typeof picked !== "string") return; // cancelled
  backupStatus.textContent = `Backing up to ${picked}… (ingest is paused while the snapshot is taken; progress in Activity)`;
  try {
    const handle = await ipc.jobSubmit("backup_create", { dest: picked });
    pendingBackupJob = handle.id;
  } catch (e) {
    backupStatus.textContent = `Backup failed to start: ${e}`;
  }
}

/** Pick a container, inspect it, and offer the validation step. */
async function pickRestoreArchive(): Promise<void> {
  if (vaultSwitched || switchInFlight) return;
  const picked = await open({
    title: "Restore from a vault backup",
    multiple: false,
    directory: false,
    filters: [{ name: "Vault backup", extensions: ["mcbackup"] }],
  });
  const archive = typeof picked === "string" ? picked : null;
  if (archive === null) return; // cancelled
  restoreFlow = { step: "validating", archive };
  renderRestorePanel();
  try {
    const manifest = await ipc.backupInspect(archive);
    restoreFlow = { step: "inspected", archive, manifest };
  } catch (e) {
    restoreFlow = { step: "error", message: `Not a usable backup: ${e}` };
  }
  renderRestorePanel();
}

/** Start the `restore_preview` job: checksum every entry, validate the
 *  database, stage the payload. */
async function startRestoreValidation(archive: string): Promise<void> {
  if (vaultSwitched || switchInFlight) return;
  restoreFlow = { step: "validating", archive };
  renderRestorePanel();
  try {
    const handle = await ipc.jobSubmit("restore_preview", { archive });
    pendingRestoreJob = handle.id;
  } catch (e) {
    restoreFlow = { step: "error", message: `Validation failed to start: ${e}` };
    renderRestorePanel();
  }
}

/** Commit the validated restore: stops the app's vault access and
 *  switches through the journal. A restart is required afterwards. */
async function commitRestore(preview: RestorePreviewView): Promise<void> {
  if (vaultSwitched || switchInFlight || !canCommit(restoreFlow)) return;
  const confirmed = window.confirm(
    `Replace the current vault with this backup?\n\n${previewLine(preview)}\n\nThe current vault is kept as a recovery snapshot. The app will need a restart.`,
  );
  if (!confirmed) return;
  switchInFlight = true;
  backupCreateButton.disabled = true;
  restorePickButton.disabled = true;
  restoreFlow = { step: "committing", preview };
  renderRestorePanel();
  try {
    const outcome = await ipc.restoreCommit(preview.preview_id);
    restoreFlow = { step: "done", outcome };
    vaultSwitched = true;
    backupCreateButton.disabled = true;
    restorePickButton.disabled = true;
    setStatus(restoreDoneLine(outcome) + " Restart the app now.", false);
    void refreshRecovery();
  } catch (e) {
    if (switchStopped(e)) {
      // The backend stopped mid-switch: a restart resolves the journal;
      // nothing further is usable, so the UI must say so and lock.
      vaultSwitched = true;
      backupCreateButton.disabled = true;
      restorePickButton.disabled = true;
      restoreFlow = {
        step: "error",
        message: `Restore stopped mid-switch — restart the app; the switch journal recovers the vault automatically. (${e})`,
      };
      setStatus("Restart the app to recover the vault switch.", true);
    } else if (gateBusy(e)) {
      // Refused while another switch or backup holds the gate: nothing
      // stopped, the staged preview is still committable — keep the
      // validated panel and let the user retry.
      switchInFlight = false;
      backupCreateButton.disabled = false;
      restorePickButton.disabled = false;
      restoreFlow = { step: "validated", preview };
      setStatus("A vault switch or backup is already in progress — the validated restore stays ready; retry the commit when it finishes.", true);
    } else {
      // Refused before anything stopped: the vault and the app are both untouched.
      switchInFlight = false;
      backupCreateButton.disabled = false;
      restorePickButton.disabled = false;
      restoreFlow = { step: "error", message: `Restore refused (the vault was not replaced): ${e}` };
    }
  }
  renderRestorePanel();
}

/** Load and render the recovery surface (previous-vault slots) plus
 *  the U05 restart lines: where the active vault came from (and what
 *  startup recovery did to an interrupted switch) and where the
 *  archive lives — the "what is active now, since when, from where"
 *  answers a restart-after-restore user opens Settings for. */
async function refreshRecovery(): Promise<void> {
  try {
    const status = await ipc.recoveryStatus();
    vaultOriginEl.textContent = status.last_switch ? vaultOriginLine(status.last_switch) : "";
    vaultLocationEl.textContent = `Archive location: ${status.data_dir}`;
    // The staged-preview claim is session-bound: only this session's
    // validated flow can commit; anything else is a leftover from an
    // earlier session and says so.
    recoveryLineEl.textContent = recoveryLine(status, restoreFlow.step === "validated");
    renderRecoverySlots(recoverySlotsEl, status, onReturnSlot, onForgetSlot, {
      returnPending: vaultSwitched || switchInFlight,
      forgetPending: switchInFlight,
    });
  } catch (e) {
    recoveryLineEl.textContent = `Recovery status failed: ${e}`;
  }
}

/** Load the persisted "last verified backup" into the panel's status
 *  line (U05): the record survives restarts, so a fresh session still
 *  shows the vault's safety state. Skipped while a backup job's live
 *  message owns the line, and never clobbers the restart-required
 *  messages a committed switch left. */
async function refreshLastBackup(): Promise<void> {
  if (pendingBackupJob !== null || vaultSwitched) return;
  try {
    const record = await ipc.lastBackup();
    // The guards were true before the await; re-check after it — a
    // backup job may have started (or a switch committed) in between,
    // and the persisted record must not clobber the live line.
    if (pendingBackupJob !== null || vaultSwitched) return;
    backupStatus.textContent = record ? lastBackupLine(record) : noBackupLine();
  } catch {
    // The pool is drained (a switch just ran) or the backend is busy:
    // leave whatever the flow already rendered there.
  }
}

async function onReturnSlot(slotId: string): Promise<void> {
  if (vaultSwitched || switchInFlight) return;
  const confirmed = window.confirm(
    `Return to this previous vault?\n\nThe current vault is kept as a new recovery snapshot. The app will need a restart.`,
  );
  if (!confirmed) return;
  switchInFlight = true;
  backupCreateButton.disabled = true;
  restorePickButton.disabled = true;
  recoveryLineEl.textContent = "Switching back to the previous vault…";
  try {
    const outcome = await ipc.returnToPreviousVault(slotId);
    vaultSwitched = true;
    backupCreateButton.disabled = true;
    restorePickButton.disabled = true;
    recoveryLineEl.textContent =
      restoreDoneLine(outcome) + " Restart the app now.";
    setStatus("Previous vault restored. Restart the app now.", false);
  } catch (e) {
    if (switchStopped(e)) {
      vaultSwitched = true;
      backupCreateButton.disabled = true;
      restorePickButton.disabled = true;
      recoveryLineEl.textContent =
        `Switch stopped mid-way — restart the app; the switch journal recovers the vault automatically. (${e})`;
      setStatus("Restart the app to recover the vault switch.", true);
    } else {
      // Refused before anything stopped: the vault and the app are both untouched.
      switchInFlight = false;
      backupCreateButton.disabled = false;
      restorePickButton.disabled = false;
      recoveryLineEl.textContent = `Return refused (the current vault was not replaced): ${e}`;
    }
  }
}

async function onForgetSlot(slotId: string): Promise<void> {
  // A switch in flight could be consuming this slot; the backend
  // serializes Forget with the same gate, the UI refuses up front.
  if (switchInFlight) return;
  const confirmed = window.confirm(
    "Delete this recovery snapshot for good? The active vault is not touched.",
  );
  if (!confirmed) return;
  try {
    await ipc.recoveryForget(slotId);
  } catch (e) {
    recoveryLineEl.textContent = `Forget failed: ${e}`;
  }
  void refreshRecovery();
}

/** Feed backup/restore job outcomes into their panels (the job stream
 *  carries the results; the tracker only keeps states). */
function feedBackupJobEvent(event: JobEvent): void {
  if (pendingBackupJob !== null && event.job === pendingBackupJob) {
    if (event.type === "succeeded") {
      const summary = event.result as BackupSummaryView;
      backupStatus.textContent = backupDoneLine(summary);
      pendingBackupJob = null;
    } else if (event.type === "failed") {
      backupStatus.textContent = `Backup failed: ${"message" in event.error ? event.error.message : event.error.kind}`;
      pendingBackupJob = null;
    } else if (event.type === "cancelled") {
      backupStatus.textContent = "Backup cancelled — no file was written.";
      pendingBackupJob = null;
    }
  }
  if (pendingRestoreJob !== null && event.job === pendingRestoreJob) {
    if (event.type === "succeeded") {
      restoreFlow = { step: "validated", preview: event.result as RestorePreviewView };
      pendingRestoreJob = null;
    } else if (event.type === "failed") {
      restoreFlow = {
        step: "error",
        message: `Validation failed: ${"message" in event.error ? event.error.message : event.error.kind}`,
      };
      pendingRestoreJob = null;
    } else if (event.type === "cancelled") {
      restoreFlow = { step: "idle" };
      pendingRestoreJob = null;
    }
    renderRestorePanel();
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
importPickButton.addEventListener("click", () => void pickImportInput(false));
importFolderButton.addEventListener("click", () => void pickImportInput(true));
demoLeaveButton.addEventListener("click", () => void leaveDemo());
firstRunImportButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("import");
  importPickButton.focus();
});
firstRunAccountButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("email");
  imapUserInput.focus();
});
firstRunDemoButton.addEventListener("click", () => void enterDemo());
imapAuthSelect.addEventListener("change", applyImapAuthMode);
imapUserInput.addEventListener("change", () => {
  if (imapAuthMode() === "oauth") void refreshOauthHint();
});
imapOauthButton.addEventListener("click", () => void oauthLogin());
imapSyncButton.addEventListener("click", () => void syncImap());
autoSyncToggle.addEventListener("change", onAutoSyncToggle);
autoSyncInterval.addEventListener("change", onAutoSyncIntervalChange);
embedButton.addEventListener("click", () => void runEmbedJob());
sourcesRefreshButton.addEventListener("click", () => void refreshAccounts());
sourcesAddButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("add");
  addChoiceImportButton.focus();
});
addChoiceImportButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("import");
  importPickButton.focus();
});
addChoiceEmailButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("email");
  imapUserInput.focus();
});
sourcesAutomaticButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("automatic");
  sourcesAutomaticHeading.focus();
});
// Back returns to the home section; focus goes back to the trigger that
// opened it, so keyboard users do not land on the (now hidden) Back button.
addBackButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("home");
  sourcesAddButton.focus();
});
importBackButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("home");
  sourcesAddButton.focus();
});
emailBackButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("home");
  sourcesAddButton.focus();
});
automaticBackButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("home");
  sourcesAutomaticButton.focus();
});
// Settings pages (General / Storage & backup / AI assistance): the switcher
// deep-links into the settings view; aria-current follows the store.
const settingsNavButtons = [...document.querySelectorAll<HTMLButtonElement>(".settings-nav-btn")];
for (const button of settingsNavButtons) {
  button.addEventListener("click", () => {
    const target = button.dataset.settingsTarget as SettingsSection;
    if (target) workspace.store.setSettingsSection(target);
  });
}
function syncSettingsNav(): void {
  const { view, settingsSection } = workspace.store.get();
  for (const button of settingsNavButtons) {
    if (view === "settings" && button.dataset.settingsTarget === settingsSection) {
      button.setAttribute("aria-current", "page");
    } else {
      button.removeAttribute("aria-current");
    }
  }
}
workspace.store.subscribe(syncSettingsNav);
syncSettingsNav();
watchButton.addEventListener("click", () => void watchTypedPath());
watchPathInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") void watchTypedPath();
});
watchAddButton.addEventListener("click", () => void pickWatchFolder());
dashboardRefreshButton.addEventListener("click", () => void refreshDashboard());
exportButton.addEventListener("click", () => void exportResults());
addProviderButton.addEventListener("click", addProviderRow);
settingsSaveGeneralButton.addEventListener("click", () => void saveSettings());
settingsSaveAiButton.addEventListener("click", () => void saveSettings());
mediaRefreshButton.addEventListener("click", () => void refreshMediaUsage());
mediaReconcileButton.addEventListener("click", () => void reconcileMediaStore());
backupCreateButton.addEventListener("click", () => void createBackup());
restorePickButton.addEventListener("click", () => void pickRestoreArchive());
recoveryRefreshButton.addEventListener("click", () => void refreshRecovery());
// The media report loads when the Storage & backup page is opened (and
// after imports that extracted media) — not eagerly at startup or on the
// other settings pages. The recovery surface loads with it (previous-vault
// snapshots exist independently of imports).
workspace.store.subscribe((current) => {
  if (current.view === "settings" && current.settingsSection === "storage") {
    void refreshMediaUsage();
    void refreshRecovery();
    void refreshLastBackup();
  }
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

// Boot. The status line and the first-run/browse choice follow the
// first stats + demo-status responses; until then a neutral loading
// line (the e2e waitForApp settles on the stats label, not this).
setStatus("Loading…");
renderAll();
updateVaultChrome();
// The demo lives in the app process: a webview reload while the sample
// archive is open restores the banner instead of silently browsing the
// personal vault.
void ipc
  .demoStatus()
  .then((mode) => {
    demoActive = mode.active;
    updateVaultChrome();
    if (demoActive) void refreshAfterVaultSwitch();
  })
  .catch(() => {});
void refreshSources();
void refreshImportScopes();
void refreshStats();
void refreshDashboard();
void refreshLlm();
void refreshEmbedStatus();
void refreshWatchStatus();
void refreshAutoSync();
void refreshAccounts();
void loadSettings();

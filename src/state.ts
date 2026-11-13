// Explicit frontend state (M3.1): the UI's data model lives here instead of
// loose module-level variables. Two parts:
//
//  1. Pure helpers — layout resolution for the narrow-window states and the
//     SequenceGuard that keeps stale async responses from overwriting newer
//     UI state (the mechanism behind the existing stale-response tests).
//  2. A small observable store for the workspace shell (view, reading,
//     navigation drawer) that src/workspace.ts renders from.
//
// No DOM, no Tauri: unit-tested in state.test.ts.

import type {
  ConversationContext,
  MessageSummary,
} from "./v2types";
import {
  DEFAULT_FILTERS,
  type ContextEntry,
  type ResultsSnapshot,
  type SearchFilters,
} from "./searchstate";

// ---------------------------------------------------------------------------
// Layout (narrow-window states)
// ---------------------------------------------------------------------------

/** The three workspace layouts from the UX contract:
 *  - wide:    >= 1100px — navigation, results, and reading side by side
 *  - medium:  800–1099px — navigation + results; reading replaces results
 *  - compact: < 800px — one content area plus a navigation drawer
 * A 200% zoom halves the effective viewport width, which moves a 1280px
 * window into the compact state instead of clipping content. */
export type LayoutMode = "wide" | "medium" | "compact";

export const WIDE_BREAKPOINT = 1100;
export const COMPACT_BREAKPOINT = 800;

export function resolveLayout(width: number): LayoutMode {
  if (width >= WIDE_BREAKPOINT) return "wide";
  if (width >= COMPACT_BREAKPOINT) return "medium";
  return "compact";
}

// ---------------------------------------------------------------------------
// SequenceGuard: last-request-wins for async IPC round trips
// ---------------------------------------------------------------------------

/**
 * Monotonic token guard for "only the newest request may render". Each
 * in-flight operation takes a token; a response may only apply while it is
 * still the newest token. This is the same rule the four per-panel `seq`
 * counters used to implement inline (search, detail, stats, dashboard,
 * embed status) — centralized here so every panel gets identical
 * stale-response protection.
 */
export class SequenceGuard {
  #current = 0;

  /** Claim the next token before starting a request. */
  next(): number {
    return ++this.#current;
  }

  /** Invalidate outstanding tokens (e.g. when the panel closes). */
  invalidate(): void {
    this.#current++;
  }

  /** Is `token` still the newest claim? */
  isCurrent(token: number): boolean {
    return token === this.#current;
  }
}

// ---------------------------------------------------------------------------
// Workspace shell state
// ---------------------------------------------------------------------------

/** Navigation destinations. Imports, account configuration, and
 * maintenance live outside the reading surface. */
export type WorkspaceView = "workspace" | "sources" | "accounts" | "activity" | "settings";

export const VIEW_TITLES: Record<WorkspaceView, string> = {
  workspace: "All messages",
  sources: "Add source",
  accounts: "Accounts",
  activity: "Activity",
  settings: "Settings",
};

/** The pages inside the Sources view (the merged source screen). */
export type SourcesSection = "home" | "add" | "import" | "email" | "automatic";

/** The pages inside the Settings view. */
export type SettingsSection = "general" | "storage" | "ai";

/** State of the message-reading pane (drives the medium/compact swap and
 * the wide empty state). */
export interface ReadingState {
  /** A message context is open in the reading pane. */
  open: boolean;
}

export interface WorkspaceState {
  view: WorkspaceView;
  layout: LayoutMode;
  reading: ReadingState;
  /** Compact navigation drawer. */
  navOpen: boolean;
  /** The visible Sources page (drives the sources sub-sections). */
  sourcesSection: SourcesSection;
  /** The visible Settings page (drives the settings sub-sections). */
  settingsSection: SettingsSection;
}

export type WorkspaceListener = (state: WorkspaceState) => void;

/** Observable store for the shell. Transitions are pure `set` operations;
 * DOM effects live in the subscriber (src/workspace.ts). */
export class WorkspaceStore {
  #state: WorkspaceState;
  #listeners = new Set<WorkspaceListener>();

  constructor(initial?: Partial<WorkspaceState>) {
    this.#state = {
      view: "workspace",
      layout: "wide",
      reading: { open: false },
      navOpen: false,
      sourcesSection: "home",
      settingsSection: "general",
      ...initial,
    };
  }

  get(): WorkspaceState {
    return this.#state;
  }

  subscribe(listener: WorkspaceListener): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  #set(patch: Partial<WorkspaceState>): void {
    this.#state = { ...this.#state, ...patch };
    for (const listener of this.#listeners) listener(this.#state);
  }

  setView(view: WorkspaceView): void {
    // Leaving the workspace does not close the reading pane; returning
    // restores it (the reading pane is part of the workspace view). Nav
    // destinations land on their home section; the section setters below
    // are the deep links (Add source, first-run, reconnect, …).
    this.#set({
      view,
      navOpen: false,
      sourcesSection: "home",
      settingsSection: "general",
    });
  }

  /** Deep-link into a Sources page (navigates to the sources view). */
  setSourcesSection(section: SourcesSection): void {
    if (this.#state.view === "sources" && this.#state.sourcesSection === section) return;
    this.#set({ view: "sources", sourcesSection: section, navOpen: false });
  }

  /** Deep-link into a Settings page (navigates to the settings view). */
  setSettingsSection(section: SettingsSection): void {
    if (this.#state.view === "settings" && this.#state.settingsSection === section) return;
    this.#set({ view: "settings", settingsSection: section, navOpen: false });
  }

  setLayout(layout: LayoutMode): void {
    if (layout === this.#state.layout) return;
    this.#set({ layout });
  }

  setReadingOpen(open: boolean): void {
    if (open === this.#state.reading.open) return;
    this.#set({ reading: { open } });
  }

  setNavOpen(navOpen: boolean): void {
    if (navOpen === this.#state.navOpen) return;
    this.#set({ navOpen });
  }
}

// ---------------------------------------------------------------------------
// Search/results application state (the panel data behind the workspace)
// M3.2: the search flow runs on the v2 IPC surface — flat MessageSummary
// rows, the v2 reading pane, and the selection/scroll snapshot contract.
// ---------------------------------------------------------------------------

export interface UiState {
  /** The filters of the currently rendered results (canonical). */
  filters: SearchFilters;
  /** Source display names selected via chips (mapped to ids per search). */
  sources: string[];
  offset: number;
  total: number;
  hits: MessageSummary[];
  activeIndex: number;
  /** The row the user opened (Back re-selects it). */
  selectedId: number | null;
  /** Rows checked for the explicit selection export. */
  selection: Set<number>;
  /** Archive revision of the visible results (refresh signal, A07). */
  pageRevision: number | null;
  refreshNeeded: boolean;
  detail: ConversationContext | null;
  /** Merged context rows held in the reading pane. */
  detailEntries: ContextEntry[];
  /** Query text at open time (highlight source for the anchor body). */
  detailQueryText: string;
  /** Captured on open, consumed by Back (selection/scroll restore). */
  resultsSnapshot: ResultsSnapshot | null;
}

export function initialUiState(): UiState {
  return {
    filters: { ...DEFAULT_FILTERS },
    sources: [],
    offset: 0,
    total: 0,
    hits: [],
    activeIndex: -1,
    selectedId: null,
    selection: new Set(),
    pageRevision: null,
    refreshNeeded: false,
    detail: null,
    detailEntries: [],
    detailQueryText: "",
    resultsSnapshot: null,
  };
}

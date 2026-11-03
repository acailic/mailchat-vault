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

import type { SearchHit, ChatContext } from "./types";

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
export type WorkspaceView = "workspace" | "sources" | "activity" | "settings";

export const VIEW_TITLES: Record<WorkspaceView, string> = {
  workspace: "All messages",
  sources: "Add source",
  activity: "Activity",
  settings: "Settings",
};

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
    // restores it (the reading pane is part of the workspace view).
    this.#set({ view, navOpen: false });
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
// ---------------------------------------------------------------------------

export interface UiState {
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

export function initialUiState(): UiState {
  return {
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
}

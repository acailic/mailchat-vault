// Workspace shell controller (M3.1): owns the DOM side of the layout
// contract — the data attributes on #app, the navigation drawer, view
// switching, reading-pane focus management, and the global search
// shortcuts. Panel logic (search, jobs, settings, …) stays in main.ts and
// drives this controller through the WorkspaceStore.

import {
  VIEW_TITLES,
  WorkspaceStore,
  resolveLayout,
  type WorkspaceSection,
  type WorkspaceView,
} from "./state";

export interface WorkspaceController {
  readonly store: WorkspaceStore;
  /** A message was opened in the reading pane (focus the heading when the
   * layout swaps results away). */
  readingOpened(): void;
  /** The reading pane closed; restore focus to the initiating result. */
  readingClosed(): void;
  /** Arrow-walking inside the pane re-rendered it: focus the (new)
   * current thread row wherever the destroyed row's focus fell. */
  refocusCurrentThreadRow(): void;
  /** Whether the compact navigation drawer is open. */
  isNavOpen(): boolean;
  closeNav(): void;
  /** Receive Back-to-results clicks (main.ts owns the detail state). */
  onBackToResults(handler: () => void): void;
}

/** Is the event target something the user types into? */
function isEditable(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  if (!el) return false;
  return (
    el.tagName === "INPUT" ||
    el.tagName === "TEXTAREA" ||
    el.tagName === "SELECT" ||
    el.isContentEditable
  );
}

/** The collapsed-desktop-sidebar choice persists across launches. Guarded:
 * storage is absent in some test/restricted-webview environments. */
const NAV_COLLAPSED_KEY = "mcv.navCollapsed";

function readNavCollapsed(): boolean {
  try {
    return window.localStorage.getItem(NAV_COLLAPSED_KEY) === "1";
  } catch {
    return false;
  }
}

function persistNavCollapsed(collapsed: boolean): void {
  try {
    window.localStorage.setItem(NAV_COLLAPSED_KEY, collapsed ? "1" : "0");
  } catch {
    // best effort only
  }
}

export function initWorkspace(): WorkspaceController {
  const app = document.querySelector<HTMLElement>("#app")!;
  const navToggle = document.querySelector<HTMLButtonElement>("#nav-toggle")!;
  const navScrim = document.querySelector<HTMLElement>("#nav-scrim")!;
  const topbarTitle = document.querySelector<HTMLElement>("#topbar-title")!;
  const backToResults = document.querySelector<HTMLButtonElement>("#back-to-results")!;
  const viewButtons = [...document.querySelectorAll<HTMLButtonElement>(".nav-view-btn")];
  const tabButtons = [...document.querySelectorAll<HTMLButtonElement>(".workspace-tab-btn")];
  const searchInput = document.querySelector<HTMLInputElement>("#search")!;

  const store = new WorkspaceStore({
    layout: resolveLayout(typeof window !== "undefined" ? window.innerWidth : 1280),
    navCollapsed: readNavCollapsed(),
  });

  // The element that opened the reading pane — restored on Back/close.
  let readingTrigger: HTMLElement | null = null;

  // -----------------------------------------------------------------
  // Render: store → DOM attributes and ARIA state
  // -----------------------------------------------------------------

  function render(): void {
    const { view, layout, reading, navOpen, navCollapsed, sourcesSection, settingsSection, workspaceSection } =
      store.get();
    app.dataset.view = view;
    app.dataset.layout = layout;
    app.dataset.reading = reading.open ? "open" : "closed";
    app.dataset.nav = navOpen ? "open" : "closed";
    app.dataset.navCollapsed = String(navCollapsed);
    app.dataset.sourcesSection = sourcesSection;
    app.dataset.settingsSection = settingsSection;
    app.dataset.workspaceSection = workspaceSection;
    for (const button of viewButtons) {
      const target = button.dataset.viewTarget as WorkspaceView | undefined;
      if (target === view) button.setAttribute("aria-current", "page");
      else button.removeAttribute("aria-current");
    }
    for (const button of tabButtons) {
      const tab = button.dataset.workspaceTab as WorkspaceSection | undefined;
      button.setAttribute("aria-selected", tab === workspaceSection ? "true" : "false");
    }
    // The hamburger shows the nav when expanded: on compact that is the
    // drawer, on wide/medium the (possibly collapsed) nav column.
    const navVisible = layout === "compact" ? navOpen : !navCollapsed;
    navToggle.setAttribute("aria-expanded", navVisible ? "true" : "false");
    topbarTitle.textContent = VIEW_TITLES[view];
  }

  store.subscribe(render);
  render();

  // -----------------------------------------------------------------
  // Layout observation: keep data-layout in step with the CSS media
  // queries (resize covers window changes and zoom steps).
  // -----------------------------------------------------------------

  function syncLayout(): void {
    store.setLayout(resolveLayout(window.innerWidth));
  }

  window.addEventListener("resize", syncLayout);
  // matchMedia listeners fire earlier than resize in some engines; guard
  // for environments without the API (tests).
  try {
    const wide = window.matchMedia("(min-width: 1100px)");
    const medium = window.matchMedia("(min-width: 800px)");
    const onChange = () => syncLayout();
    wide.addEventListener?.("change", onChange);
    medium.addEventListener?.("change", onChange);
  } catch {
    // no matchMedia: resize alone drives the layout state
  }

  // -----------------------------------------------------------------
  // Navigation: view switching + compact drawer
  // -----------------------------------------------------------------

  for (const button of viewButtons) {
    button.addEventListener("click", () => {
      const target = button.dataset.viewTarget as WorkspaceView | undefined;
      if (target) store.setView(target);
    });
  }

  for (const button of tabButtons) {
    button.addEventListener("click", () => {
      const tab = button.dataset.workspaceTab as WorkspaceSection | undefined;
      if (tab) store.setWorkspaceSection(tab);
    });
  }

  navToggle.addEventListener("click", () => {
    const { layout, navOpen, navCollapsed } = store.get();
    // Compact: the drawer overlays. Wide/medium: the nav column collapses
    // out of the grid (and the choice is remembered for next launch).
    if (layout === "compact") {
      store.setNavOpen(!navOpen);
    } else {
      store.setNavCollapsed(!navCollapsed);
      persistNavCollapsed(!navCollapsed);
    }
  });
  navScrim.addEventListener("click", () => store.setNavOpen(false));

  // -----------------------------------------------------------------
  // Reading pane: Back returns to the results that opened the message
  // -----------------------------------------------------------------

  let backHandler: (() => void) | null = null;
  backToResults.addEventListener("click", () => backHandler?.());

  // -----------------------------------------------------------------
  // Global shortcuts: Ctrl/Cmd+K or `/` focus the search box (landing on
  // the Search tab first — the box lives in that panel)
  // -----------------------------------------------------------------

  document.addEventListener("keydown", (event) => {
    if ((event.ctrlKey || event.metaKey) && (event.key === "k" || event.key === "K")) {
      event.preventDefault();
      store.setWorkspaceSection("search");
      searchInput.focus();
      searchInput.select();
      return;
    }
    if (event.key === "/" && !isEditable(event.target)) {
      event.preventDefault();
      store.setWorkspaceSection("search");
      searchInput.focus();
      searchInput.select();
    }
  });

  return {
    store,
    readingOpened() {
      // Remember what opened the reading pane so Back can restore it.
      const active = document.activeElement as HTMLElement | null;
      readingTrigger =
        active && active.closest && active.closest("#results-col") ? active : null;
      if (store.get().layout !== "wide") {
        const heading = document.querySelector<HTMLElement>("#detail .detail-card h2");
        if (heading) {
          heading.tabIndex = -1;
          heading.focus();
        }
      } else if (!active || active === document.body || !active.isConnected) {
        // A re-render that destroyed the previously focused element (e.g.
        // arrow-walking to a neighbour) can leave a DETACHED node as
        // activeElement in real webviews — happy-dom resets to body, so
        // cover both (same quirk readingClosed handles).
        const current = document.querySelector<HTMLElement>("#detail .thread-row.current");
        if (current) {
          current.tabIndex = -1;
          current.focus();
        }
      }
    },
    refocusCurrentThreadRow() {
      // Arrow-walking inside the reading pane re-renders it; the pane owns
      // the interaction, so the (new) current row takes focus back no
      // matter where the destroyed row's focus fell (body, the search
      // input, or a rebuilt row — webviews differ; happy-dom resets to
      // body, which is why the unit tests never saw this).
      const current = document.querySelector<HTMLElement>("#detail .thread-row.current");
      if (current) {
        current.tabIndex = -1;
        current.focus();
      }
    },
    readingClosed() {
      const active = document.activeElement;
      // Restore when focus is already gone (body/null) or still sits inside
      // the closing reading pane: a real browser keeps the just-activated
      // Back/Close button focused until style recalc drops it to <body> a
      // frame later, so waiting for body here skips the restore entirely.
      const inReading =
        active instanceof Element && active.closest("#reading") !== null;
      if (!active || active === document.body || inReading) {
        const activeRow = document.querySelector<HTMLElement>(
          '#results .hit-item[aria-current="true"]',
        );
        const restore =
          readingTrigger && readingTrigger.isConnected
            ? readingTrigger
            : (activeRow ?? document.querySelector<HTMLElement>("#results"));
        restore?.focus?.();
      }
      readingTrigger = null;
    },
    isNavOpen() {
      return store.get().navOpen;
    },
    closeNav() {
      store.setNavOpen(false);
    },
    onBackToResults(handler: () => void) {
      backHandler = handler;
    },
  };
}

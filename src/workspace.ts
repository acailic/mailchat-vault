// Workspace shell controller (M3.1): owns the DOM side of the layout
// contract — the data attributes on #app, the navigation drawer, view
// switching, reading-pane focus management, and the global search
// shortcuts. Panel logic (search, jobs, settings, …) stays in main.ts and
// drives this controller through the WorkspaceStore.

import {
  VIEW_TITLES,
  WorkspaceStore,
  resolveLayout,
  type WorkspaceView,
} from "./state";

export interface WorkspaceController {
  readonly store: WorkspaceStore;
  /** A message was opened in the reading pane (focus the heading when the
   * layout swaps results away). */
  readingOpened(): void;
  /** The reading pane closed; restore focus to the initiating result. */
  readingClosed(): void;
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

export function initWorkspace(): WorkspaceController {
  const app = document.querySelector<HTMLElement>("#app")!;
  const navToggle = document.querySelector<HTMLButtonElement>("#nav-toggle")!;
  const navScrim = document.querySelector<HTMLElement>("#nav-scrim")!;
  const topbarTitle = document.querySelector<HTMLElement>("#topbar-title")!;
  const backToResults = document.querySelector<HTMLButtonElement>("#back-to-results")!;
  const viewButtons = [...document.querySelectorAll<HTMLButtonElement>(".nav-view-btn")];
  const searchInput = document.querySelector<HTMLInputElement>("#search")!;

  const store = new WorkspaceStore({
    layout: resolveLayout(typeof window !== "undefined" ? window.innerWidth : 1280),
  });

  // The element that opened the reading pane — restored on Back/close.
  let readingTrigger: HTMLElement | null = null;

  // -----------------------------------------------------------------
  // Render: store → DOM attributes and ARIA state
  // -----------------------------------------------------------------

  function render(): void {
    const { view, layout, reading, navOpen } = store.get();
    app.dataset.view = view;
    app.dataset.layout = layout;
    app.dataset.reading = reading.open ? "open" : "closed";
    app.dataset.nav = navOpen ? "open" : "closed";
    for (const button of viewButtons) {
      const target = button.dataset.viewTarget as WorkspaceView | undefined;
      if (target === view) button.setAttribute("aria-current", "page");
      else button.removeAttribute("aria-current");
    }
    navToggle.setAttribute("aria-expanded", navOpen ? "true" : "false");
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

  navToggle.addEventListener("click", () => store.setNavOpen(!store.get().navOpen));
  navScrim.addEventListener("click", () => store.setNavOpen(false));

  // -----------------------------------------------------------------
  // Reading pane: Back returns to the results that opened the message
  // -----------------------------------------------------------------

  let backHandler: (() => void) | null = null;
  backToResults.addEventListener("click", () => backHandler?.());

  // -----------------------------------------------------------------
  // Global shortcuts: Ctrl/Cmd+K or `/` focus the search box
  // -----------------------------------------------------------------

  document.addEventListener("keydown", (event) => {
    if ((event.ctrlKey || event.metaKey) && (event.key === "k" || event.key === "K")) {
      event.preventDefault();
      searchInput.focus();
      searchInput.select();
      return;
    }
    if (event.key === "/" && !isEditable(event.target)) {
      event.preventDefault();
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
      }
    },
    readingClosed() {
      if (store.get().layout !== "wide") {
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

// Unit tests for the explicit state module (src/state.ts): layout
// resolution for the narrow-window states, the SequenceGuard that powers
// the stale-response contract, and the workspace store transitions.

import { describe, expect, it } from "vitest";
import {
  COMPACT_BREAKPOINT,
  SequenceGuard,
  VIEW_TITLES,
  WIDE_BREAKPOINT,
  WorkspaceStore,
  initialUiState,
  resolveLayout,
} from "./state";

describe("resolveLayout", () => {
  it("wide at 1100px and above", () => {
    expect(resolveLayout(WIDE_BREAKPOINT)).toBe("wide");
    expect(resolveLayout(1280)).toBe("wide");
    expect(resolveLayout(4000)).toBe("wide");
  });

  it("medium from 800px to 1099px", () => {
    expect(resolveLayout(COMPACT_BREAKPOINT)).toBe("medium");
    expect(resolveLayout(999)).toBe("medium");
    expect(resolveLayout(WIDE_BREAKPOINT - 1)).toBe("medium");
  });

  it("compact below 800px", () => {
    expect(resolveLayout(COMPACT_BREAKPOINT - 1)).toBe("compact");
    expect(resolveLayout(640)).toBe("compact");
    expect(resolveLayout(0)).toBe("compact");
  });

  it("200% zoom on a 1280px window lands in compact, not clipped wide", () => {
    // A doubled zoom halves the effective viewport width.
    expect(resolveLayout(1280 / 2)).toBe("compact");
  });
});

describe("SequenceGuard", () => {
  it("tokens are current until a newer one is claimed", () => {
    const guard = new SequenceGuard();
    const first = guard.next();
    expect(guard.isCurrent(first)).toBe(true);
    const second = guard.next();
    expect(guard.isCurrent(first)).toBe(false);
    expect(guard.isCurrent(second)).toBe(true);
  });

  it("invalidate() rejects every outstanding token", () => {
    const guard = new SequenceGuard();
    const token = guard.next();
    guard.invalidate();
    expect(guard.isCurrent(token)).toBe(false);
  });
});

describe("WorkspaceStore", () => {
  it("starts on the workspace view in wide layout with reading closed", () => {
    const store = new WorkspaceStore();
    const s = store.get();
    expect(s.view).toBe("workspace");
    expect(s.layout).toBe("wide");
    expect(s.reading.open).toBe(false);
    expect(s.navOpen).toBe(false);
  });

  it("setView closes the navigation drawer but keeps the reading pane", () => {
    const store = new WorkspaceStore();
    store.setReadingOpen(true);
    store.setNavOpen(true);
    store.setView("settings");
    const s = store.get();
    expect(s.view).toBe("settings");
    expect(s.navOpen).toBe(false);
    expect(s.reading.open).toBe(true); // preserved for the return
  });

  it("notifies subscribers on transitions only", () => {
    const store = new WorkspaceStore();
    const seen: string[] = [];
    store.subscribe((s) => seen.push(`${s.view}:${s.layout}:${s.reading.open}:${s.navOpen}`));
    store.setView("activity");
    store.setLayout("medium");
    store.setReadingOpen(true);
    store.setNavOpen(true);
    store.setReadingOpen(true); // no-op: no notification
    store.setReadingOpen(false);
    expect(seen).toEqual([
      "activity:wide:false:false",
      "activity:medium:false:false",
      "activity:medium:true:false",
      "activity:medium:true:true",
      "activity:medium:false:true",
    ]);
  });

  it("unsubscribe stops notifications", () => {
    const store = new WorkspaceStore();
    let calls = 0;
    const stop = store.subscribe(() => calls++);
    store.setView("sources");
    stop();
    store.setView("settings");
    expect(calls).toBe(1);
  });

  it("VIEW_TITLES covers exactly the four destinations", () => {
    expect(VIEW_TITLES).toEqual({
      workspace: "Archive",
      sources: "Sources",
      activity: "Activity",
      settings: "Settings",
    });
  });
});

describe("WorkspaceStore sub-view sections", () => {
  it("starts on the sources home and the general settings page", () => {
    const store = new WorkspaceStore();
    expect(store.get().sourcesSection).toBe("home");
    expect(store.get().settingsSection).toBe("general");
  });

  it("setSourcesSection navigates to sources, closes the drawer, keeps reading", () => {
    const store = new WorkspaceStore();
    store.setReadingOpen(true);
    store.setView("settings");
    store.setNavOpen(true);
    store.setSourcesSection("import");
    const s = store.get();
    expect(s.view).toBe("sources");
    expect(s.sourcesSection).toBe("import");
    expect(s.navOpen).toBe(false);
    expect(s.reading.open).toBe(true);
  });

  it("setView lands each destination on its home section", () => {
    const store = new WorkspaceStore();
    store.setSourcesSection("email");
    store.setView("sources");
    expect(store.get().sourcesSection).toBe("home");
    store.setSettingsSection("storage");
    store.setView("settings");
    expect(store.get().settingsSection).toBe("general");
  });

  it("section setters are no-ops only when already showing that section", () => {
    const store = new WorkspaceStore();
    let notifications = 0;
    store.subscribe(() => notifications++);
    store.setSourcesSection("home"); // view was workspace → navigates
    expect(notifications).toBe(1);
    store.setSourcesSection("home"); // true no-op
    expect(notifications).toBe(1);
  });

  it("section changes notify subscribers with the new state", () => {
    const store = new WorkspaceStore();
    const seen: string[] = [];
    store.subscribe((s) => seen.push(`${s.view}:${s.settingsSection}`));
    store.setSettingsSection("ai");
    expect(seen).toEqual(["settings:ai"]);
  });
});

describe("WorkspaceStore workspace tabs (Search / Ask)", () => {
  it("starts on the search tab", () => {
    const store = new WorkspaceStore();
    expect(store.get().workspaceSection).toBe("search");
  });

  it("setWorkspaceSection navigates to the workspace, closes the drawer, keeps reading", () => {
    const store = new WorkspaceStore();
    store.setReadingOpen(true);
    store.setView("settings");
    store.setNavOpen(true);
    store.setWorkspaceSection("ask");
    const s = store.get();
    expect(s.view).toBe("workspace");
    expect(s.workspaceSection).toBe("ask");
    expect(s.navOpen).toBe(false);
    expect(s.reading.open).toBe(true);
  });

  it("setView keeps the active workspace tab (like the reading pane)", () => {
    const store = new WorkspaceStore();
    store.setWorkspaceSection("ask");
    store.setView("sources");
    expect(store.get().workspaceSection).toBe("ask");
    store.setView("workspace");
    expect(store.get().workspaceSection).toBe("ask");
  });

  it("setWorkspaceSection is a no-op only when already showing that tab", () => {
    const store = new WorkspaceStore();
    let notifications = 0;
    store.subscribe(() => notifications++);
    store.setWorkspaceSection("search"); // true no-op (view is workspace)
    expect(notifications).toBe(0);
    store.setView("settings");
    store.setWorkspaceSection("search"); // navigates back
    expect(notifications).toBe(2);
    expect(store.get().view).toBe("workspace");
  });
});

describe("WorkspaceStore desktop sidebar collapse", () => {
  it("setNavCollapsed transitions and notifies only on change", () => {
    const store = new WorkspaceStore();
    expect(store.get().navCollapsed).toBe(false);
    let notifications = 0;
    store.subscribe(() => notifications++);
    store.setNavCollapsed(false); // no-op
    store.setNavCollapsed(true);
    store.setNavCollapsed(true); // no-op
    expect(notifications).toBe(1);
    expect(store.get().navCollapsed).toBe(true);
  });

  it("collapsing the sidebar never touches the compact drawer state", () => {
    const store = new WorkspaceStore();
    store.setNavOpen(true);
    store.setNavCollapsed(true);
    expect(store.get().navOpen).toBe(true);
  });
});

describe("initialUiState", () => {
  it("boots empty with no selection", () => {
    const s = initialUiState();
    expect(s.filters.text).toBe("");
    expect(s.sources).toEqual([]);
    expect(s.hits).toEqual([]);
    expect(s.activeIndex).toBe(-1);
    expect(s.selectedId).toBeNull();
    expect(s.selection.size).toBe(0);
    expect(s.detail).toBeNull();
    expect(s.detailEntries).toEqual([]);
    expect(s.resultsSnapshot).toBeNull();
    expect(s.pageRevision).toBeNull();
    expect(s.refreshNeeded).toBe(false);
    expect(s.offset).toBe(0);
    expect(s.total).toBe(0);
  });
});

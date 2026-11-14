# Workspace tabs (Search / Ask) and hideable desktop sidebar

- **Date:** 2026-11-14
- **Status:** Approved (design), implementation in progress
- **Scope:** Workspace (Archive) view layout and the app-shell navigation; no backend, search, or reading-pane changes

## 1. Problem

Two layout problems in the current shell:

- The workspace (Archive) view stacks everything in one long column: the search
  row, filters, results, pagination, export controls — and, far below them, the
  "Ask the archive" section. Ask is buried; reaching it means scrolling past the
  whole results surface, and the two modes (query the index vs. ask a question)
  compete for the same column.
- The left navigation is permanently visible on desktop (≥ 800px). Only the
  compact layout (< 800px) has a hideable drawer. On a laptop screen the nav
  column costs ~13% of the width whether it is needed or not.

## 2. Approach

Both changes extend the existing Phase 1 shell mechanics (state in
`WorkspaceStore`, mirrored as `data-*` attributes on `#app`, CSS-only view
switching — nothing unmounts):

### 2.1 Search / Ask tabs

- New `WorkspaceSection = "search" | "ask"` on `WorkspaceState`, default
  `"search"`, with a `setWorkspaceSection(section)` deep-link setter that also
  navigates to the workspace view (same shape as `setSourcesSection`).
  `setView` deliberately does **not** reset it: the last-used tab survives
  leaving and returning to Archive (the reading pane already behaves this way).
- A `role="tablist"` strip at the top of `#results-col` with two tabs,
  Search and Ask, styled after the settings-page nav row
  (`.settings-nav-btn`). The existing search `<section>` gains
  `id="search-section"` and `role="tabpanel"`; `#ask-section` becomes the Ask
  tabpanel.
- CSS gates the two panels on `data-workspace-section` exactly like the
  sources/settings sub-sections. Both stay mounted, so results, scroll, and the
  last answer survive tab switches.
- The reading pane is untouched: evidence rows in Ask still open messages in
  it, and Back returns to the active tab.
- `Ctrl/Cmd+K` and `/` switch to the Search tab before focusing the box.
- Flows that surface results (`viewSourceMessages`, source-chip toggles,
  import-scope jump, entering the sample archive) deep-link to the Search tab.

### 2.2 Hideable desktop sidebar

- New `navCollapsed: boolean` on `WorkspaceState` (desktop-only concept; the
  compact drawer keeps `navOpen`). The topbar — currently compact-only —
  becomes visible at every width, so its `#nav-toggle` hamburger is always
  reachable.
- On wide/medium layouts the hamburger toggles `navCollapsed`; on compact it
  keeps toggling the drawer. `aria-expanded` reflects "nav visible" for the
  current layout.
- Collapsed desktop state: `#app` drops the nav grid column (`1fr` only,
  `#nav` hidden) — full-width content, no scrim.
- The collapsed choice persists in `localStorage` (`mcv.navCollapsed`) and is
  restored on boot; storage access is guarded (tests, restricted webviews).

## 3. Alternatives considered

- **Ask as a fourth top-level destination** — violates the Phase 1 contract
  ("exactly three primary destinations") and would split the reading pane.
- **Three tabs (Archive / Search / Ask)** — search and browse share one list
  and one state (`UiState`; empty query = newest-first browse), so a separate
  browse tab would either duplicate or fork that state for no user-visible
  gain. Rejected by the user in review.
- **Icon-rail sidebar collapse** — rejected by the user in review (full hide).
- **Floating reopen button when the sidebar is hidden** — the always-visible
  topbar hamburger makes it redundant.

## 4. Testing

- `state.test.ts`: `setWorkspaceSection` deep-link semantics (navigates,
  closes drawer, no-op guard), persistence of the section across `setView`,
  `setNavCollapsed` transition behavior.
- `main.test.ts` (happy-dom boots the real `index.html`): tab clicks flip
  `data-workspace-section` and panel visibility; `Ctrl+K` lands on Search;
  the hamburger collapses the nav on wide layouts and works the drawer on
  compact; `viewSourceMessages` lands on the Search tab.
- e2e `16-workspace-layout`: tab switching and desktop sidebar collapse/expand
  steps.

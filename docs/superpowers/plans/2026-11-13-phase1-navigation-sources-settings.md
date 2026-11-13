# Mailchat Vault — Phase 1: Navigation and Layout Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reorganize the UI around three primary destinations (Archive / Sources / Settings) with Activity as a secondary link, merge the Accounts panel and the Add source view into one Sources screen with sub-views, split Settings into General / Storage & backup / AI assistance pages, and switch source selection to stable source-instance ids — with zero backend changes and every existing capability still reachable.

**Architecture:** The app is one static `index.html` whose views are `<section class="view">` blocks toggled by `data-view` on `#app` (CSS-only visibility, elements never unmount). Sub-views extend the same mechanism: `WorkspaceStore` (src/state.ts) gains `sourcesSection` / `settingsSection` fields, `src/workspace.ts` mirrors them onto `#app` as `data-sources-section` / `data-settings-section`, and CSS shows exactly one sub-section per view. All panel wiring stays in `src/main.ts` over module-scope element constants; ids of moved controls are preserved so wiring survives DOM relocation.

**Tech Stack:** Vanilla TypeScript + Vite + vitest (happy-dom, mocked Tauri IPC), Tauri 2 shell. No framework, no new dependencies, no Rust changes in this phase.

## Global Constraints

- Frontend-only: no new Tauri IPC commands, no `src-tauri`/`crates` changes, no new npm packages.
- Navigation ends up as exactly three primary destinations — **Archive**, **Sources**, **Settings** — plus **Activity** as a secondary link at the bottom of the nav with (a) a count of active operations and (b) a textual error indicator ("needs attention").
- "Add source" offers exactly two choices: "Import files" and "Connect email".
- Gmail server/port (and folders) sit behind an "Advanced" disclosure; Google sign-in remains offered per `oauth_status` (no new gating logic).
- Auto-sync stays one shared opt-in option (off by default, 15-minute default interval) — relocated into Sources → "Automatic imports", not redesigned.
- Disconnect = stop future sync (`source_set_excluded(id, true)`) + remove credentials (`account_forget_credentials(id)`); the local archive always stays; **no new archive-deletion UI**.
- Settings has three pages: General / Storage & backup / AI assistance. The persisted `ai_enabled` switch is **Phase 4** — this phase only relocates the provider configuration.
- Source selection switches from display-name mapping to stable source-instance ids everywhere (`state.sources`, chips, filters).
- Preserve: reading-pane survival across view switches (`WorkspaceStore.setView` keeps `reading`), `SequenceGuard` stale-response protection, textContent-only dynamic strings (never innerHTML), English UI copy, `aria-current` for active destinations, both themes, all layout breakpoints.
- Gates after every task: `npm run check` (tsc --noEmit) clean and `npx vitest run` green. E2e specs are updated in Task 8 but are **not** run on this host (standing WebKitWebDriver limitation, recorded in the roadmap) — never claim a local e2e pass.
- Commit style follows the repo: `feat(ui): …` / `test(ui): …`, imperative, scoped.

---

### Task 1: Sub-view state model in `WorkspaceStore`

**Files:**
- Modify: `src/state.ts`
- Test: `src/state.test.ts`

**Interfaces:**
- Consumes: existing `WorkspaceStore`, `WorkspaceState`, `#set`.
- Produces (used by Tasks 2–7):
  - `type SourcesSection = "home" | "add" | "import" | "email" | "automatic"`
  - `type SettingsSection = "general" | "storage" | "ai"`
  - `WorkspaceState.sourcesSection: SourcesSection` (default `"home"`) and `WorkspaceState.settingsSection: SettingsSection` (default `"general"`)
  - `WorkspaceStore.setSourcesSection(section: SourcesSection): void` — navigates to the sources view on that section, closes the drawer; no-op when already showing it
  - `WorkspaceStore.setSettingsSection(section: SettingsSection): void` — same for settings
  - `setView(view)` additionally resets both sections to their home values (`"home"` / `"general"`)

- [ ] **Step 1: Write the failing tests**

Append to `src/state.test.ts` (inside the file's existing import/describe style — `WorkspaceStore` is already imported there):

```ts
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/state.test.ts`
Expected: FAIL — `sourcesSection` is `undefined`, `setSourcesSection` is not a function.

- [ ] **Step 3: Implement the state changes**

In `src/state.ts`, add the section types directly below the `VIEW_TITLES` map (do not change `WorkspaceView` or `VIEW_TITLES` yet — that is Task 3):

```ts
/** The pages inside the Sources view (the merged source screen). */
export type SourcesSection = "home" | "add" | "import" | "email" | "automatic";

/** The pages inside the Settings view. */
export type SettingsSection = "general" | "storage" | "ai";
```

Extend `WorkspaceState` (after `navOpen: boolean;`):

```ts
  /** The visible Sources page (drives the sources sub-sections). */
  sourcesSection: SourcesSection;
  /** The visible Settings page (drives the settings sub-sections). */
  settingsSection: SettingsSection;
```

Extend the constructor's initial state (after `navOpen: false,`):

```ts
      sourcesSection: "home",
      settingsSection: "general",
```

Replace `setView` and add the section setters:

```ts
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `npx vitest run src/state.test.ts`
Expected: PASS (all new tests plus the existing `WorkspaceStore` tests — `setView` still preserves `reading`).

- [ ] **Step 5: Typecheck and full unit run**

Run: `npm run check && npx vitest run`
Expected: tsc clean; all suites green (no other module consumes the new fields yet).

- [ ] **Step 6: Commit**

```bash
git add src/state.ts src/state.test.ts
git commit -m "feat(ui): sub-view sections in the workspace store (sources/settings pages)"
```

---

### Task 2: Sources view sub-sections (DOM restructure)

This task restructures `#view-sources` into five sub-sections (`home`, `add`, `import`, `email`, `automatic`), moves the existing import / IMAP / auto-sync / watch DOM into them (ids unchanged), reworks the email form with visible labels and an Advanced disclosure, wires the chooser and Back buttons, and teaches the shell to render `data-sources-section`. The Accounts view and the nav rail are untouched (Task 3).

**Files:**
- Modify: `index.html` (replace lines 200–295, the whole `#view-sources` block)
- Modify: `src/workspace.ts` (render section attributes)
- Modify: `src/styles/app.css` (sub-section visibility rules; update the stale header comment)
- Modify: `src/main.ts` (element constants, chooser/Back wiring, guidance removal, first-run retarget, demo lockout, `applyImapAuthMode` password label)
- Test: `src/main.test.ts`

**Interfaces:**
- Consumes: `setSourcesSection` (Task 1).
- Produces (used by Tasks 3–8):
  - DOM ids: `#sources-home`, `#sources-add`, `#sources-import`, `#sources-email`, `#sources-automatic` (sections); `#sources-add-btn`, `#sources-automatic-btn`, `#add-choice-import`, `#add-choice-email`, `#add-back-btn`, `#import-back-btn`, `#email-back-btn`, `#automatic-back-btn`, `#imap-advanced`, `#imap-pass-label` (new controls)
  - `#app` carries `data-sources-section` (values `home|add|import|email|automatic`)
  - All pre-existing ids inside the moved blocks (`#import-pick-btn`, `#imap-host`, `#auto-sync-toggle`, `#watch-*`, …) keep working unchanged.

- [ ] **Step 1: Write the failing tests**

In `src/main.test.ts`, add a new describe block (after the workspace-shell block). It uses the file's existing helpers (`boot`, `$`, `input`, `pressKey`, `tauri`, `vi.waitFor`):

```ts
describe("sources view sub-sections", () => {
  it("renders five sub-sections and defaults to home", async () => {
    await boot();
    const app = $("#app") as HTMLElement;
    expect(app.dataset.sourcesSection).toBe("home");
    for (const id of ["#sources-home", "#sources-add", "#sources-import", "#sources-email", "#sources-automatic"]) {
      expect($(id), id).toBeTruthy();
    }
  });

  it("the Add source chooser navigates to import / email and Back returns home", async () => {
    await boot();
    ($("#sources-add-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("add"),
    );
    ($("#add-choice-import") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("import"),
    );
    ($("#import-back-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
    ($("#add-choice-email") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("email"),
    );
    ($("#email-back-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
  });

  it("the Automatic imports link opens the automatic section and Back returns", async () => {
    await boot();
    ($("#sources-automatic-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("automatic"),
    );
    // The relocated scheduler and watch controls are inside that section.
    expect($("#sources-automatic")!.contains($("#auto-sync-toggle"))).toBe(true);
    expect($("#sources-automatic")!.contains($("#watch-folders"))).toBe(true);
    ($("#automatic-back-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
  });

  it("nav to sources resets to the home section from a deep section", async () => {
    await boot();
    ($("#sources-add-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("add"),
    );
    ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("home"),
    );
  });

  it("the email form keeps ids, hides the password row in oauth mode, and shows server/port under Advanced", async () => {
    await boot();
    ($("#sources-add-btn") as HTMLElement).click();
    ($("#add-choice-email") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("email"),
    );
    expect($("#imap-user")).toBeTruthy();
    expect($("#imap-pass")).toBeTruthy();
    expect($("#imap-advanced")).toBeTruthy();
    // Default: app-password mode shows the password row.
    expect(($("#imap-pass-label") as HTMLElement).hidden).toBe(false);
    input("#imap-auth").value = "oauth";
    fireChange(input("#imap-auth"));
    expect(($("#imap-pass") as HTMLElement).hidden).toBe(true);
    expect(($("#imap-pass-label") as HTMLElement).hidden).toBe(true);
  });

  it("first-run buttons deep-link into the import / email sections", async () => {
    await boot();
    ($("#first-run-import-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("import"),
    );
    ($("#first-run-account-btn") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("email"),
    );
  });
});
```

Note: the first-run card is visible in `boot()` only when the archive is empty. The default `stats` handler in `beforeEach` must return an empty archive for that last test — check what the shared `STATS` fixture holds; if it is non-empty, set `tauri.handlers.set("stats", () => ({ total: 0, by_source: [] }))` inside that test before `boot()`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `npx vitest run src/main.test.ts -t "sources view sub-sections"`
Expected: FAIL — `#sources-home` et al. do not exist; `data-sources-section` is undefined.

- [ ] **Step 3: Replace the `#view-sources` block in `index.html`**

Replace the whole block currently at `index.html:200–295` (from `<!-- Sources: imports and account configuration…` through the closing `</section>` before the Accounts comment) with:

```html
          <!-- Sources: the merged source screen — the source list, the
               Add source chooser, and its two flows. Sub-sections are
               switched by data-sources-section on #app. -->
          <section id="view-sources" class="view" aria-label="Sources">
            <h2 class="view-heading">Sources</h2>

            <!-- Home: the source list (Task 3 moves the Accounts list
                 here) plus the two ways forward. -->
            <section id="sources-home" class="sources-section" aria-label="Your sources">
              <div class="row">
                <button id="sources-add-btn" class="primary" type="button">Add source…</button>
                <div class="spacer"></div>
                <button id="sources-automatic-btn" class="link-btn" type="button" title="Auto-sync and watched folders — imports that happen without a click">Automatic imports…</button>
              </div>
            </section>

            <!-- The Add source chooser: exactly two ways in. -->
            <section id="sources-add" class="sources-section" aria-label="Add a source">
              <div class="row">
                <button id="add-choice-import" class="primary" type="button">Import files…</button>
                <button id="add-choice-email" type="button">Connect email…</button>
                <div class="spacer"></div>
                <button id="add-back-btn" type="button">Back to sources</button>
              </div>
              <p class="meta" style="margin: 6px 0 0">
                Import an export you already have — a file or a folder — or connect an
                email account that syncs over IMAP. Everything lands in the same
                searchable archive.
              </p>
            </section>

            <!-- Import: the U03 picker → preview → confirm flow. -->
            <section id="sources-import" class="sources-section" aria-label="Import files">
              <div class="row">
                <h3>Import files</h3>
                <div class="spacer"></div>
                <button id="import-back-btn" type="button">Back to sources</button>
              </div>
              <div class="row">
                <button id="import-pick-btn" class="primary" type="button">Import file…</button>
                <button id="import-folder-btn" type="button" title="Import a Telegram export folder, a Discord Data Package, or a folder holding one chat export">Import folder…</button>
                <label class="meta" title="Copy media files from the export into the vault's media store (hash-named, deduplicated)">
                  <input id="chat-media" type="checkbox" checked /> extract media
                </label>
              </div>
              <p class="meta" id="import-help" style="margin: 6px 0 0">
                An .mbox email archive, a WhatsApp .txt, a Telegram result.json, a Signal
                conversation .json, a Discord messages.json — or a .zip of any of these.
                You will see a preview before anything is stored; re-importing never
                duplicates.
              </p>
              <div id="import-preview" hidden></div>
              <div id="import-status" class="meta" style="margin-top: 6px" role="status"></div>
              <div id="import-followup" class="row" style="margin-top: 6px" hidden></div>
            </section>

            <!-- Connect email: visible field labels; server/port/folders
                 under Advanced (Gmail works with the defaults). -->
            <section id="sources-email" class="sources-section" aria-label="Connect email">
              <div class="row">
                <h3>Connect email</h3>
                <div class="spacer"></div>
                <button id="email-back-btn" type="button">Back to sources</button>
              </div>
              <div class="row">
                <label for="imap-user">Email address</label>
                <input id="imap-user" type="text" placeholder="you@gmail.com" />
                <label for="imap-auth">Sign-in method</label>
                <select id="imap-auth" style="max-width: 15em" title="How to authenticate with the server">
                  <option value="password">App password</option>
                  <option value="oauth">Google sign-in</option>
                </select>
                <button id="imap-oauth-btn" type="button" hidden>Sign in with Google…</button>
              </div>
              <div class="row">
                <label for="imap-pass" id="imap-pass-label">Password</label>
                <input id="imap-pass" type="password" placeholder="App password" autocomplete="off" />
              </div>
              <div id="imap-oauth-hint" class="meta" style="margin-top: 4px" role="status"></div>
              <details id="imap-advanced">
                <summary class="meta">Advanced — server, port, folders</summary>
                <div class="row" style="margin-top: 6px">
                  <label for="imap-host">Server</label>
                  <input id="imap-host" type="text" placeholder="imap.gmail.com" value="imap.gmail.com" />
                  <label for="imap-port">Port</label>
                  <input id="imap-port" type="number" placeholder="993" value="993" style="max-width: 90px" />
                </div>
                <div class="row" style="margin-top: 6px">
                  <label for="imap-folders">Folders</label>
                  <input id="imap-folders" type="text" placeholder="INBOX (comma-separated folders)" />
                </div>
              </details>
              <div class="row" style="margin-top: 8px">
                <button id="imap-sync-btn" class="primary" type="button">Connect and sync</button>
                <span id="imap-status" class="meta" role="status"></span>
              </div>
              <p id="imap-cred-note" class="meta" style="margin: 4px 0 0">
                The password is stored in the OS keyring when available; without a keyring it is kept
                in memory for this session only — restarting the app then clears it. It is never
                written to the vault archive.
              </p>
            </section>

            <!-- Automatic imports: the shared opt-in scheduler and the
                 watched folders, separate from individual accounts. -->
            <section id="sources-automatic" class="sources-section" aria-label="Automatic imports">
              <div class="row">
                <h3>Automatic imports</h3>
                <div class="spacer"></div>
                <button id="automatic-back-btn" type="button">Back to sources</button>
              </div>
              <p class="meta" style="margin: 6px 0 0">
                Two opt-in helpers that import without a click: a shared auto-sync round over
                every connected email account, and watched folders that pick up new export
                files. Both are off unless you turn them on here.
              </p>
              <div class="row" style="margin-top: 6px">
                <label class="meta" title="Opt-in: round over every configured IMAP source through the background job system. Rounds never overlap; missed time (sleep/offline) never queues.">
                  <input id="auto-sync-toggle" type="checkbox" /> Auto-sync every
                  <select id="auto-sync-interval" style="max-width: 5em" aria-label="Auto-sync interval in minutes">
                    <option value="5">5</option>
                    <option value="15">15</option>
                    <option value="30">30</option>
                    <option value="60">60</option>
                  </select> min
                </label>
              </div>
              <div id="auto-sync-status" class="meta" style="margin-top: 6px; min-height: 1em" role="status"></div>
              <div class="row" style="margin-top: 12px">
                <label class="visually-hidden" for="watch-path">Folder to watch</label>
                <input id="watch-path" type="text" placeholder="/path/to/folder with .mbox / chat exports" />
                <button id="watch-btn" type="button">Watch</button>
                <button id="watch-add-btn" type="button">Pick folder…</button>
              </div>
              <div id="watch-status" class="meta" style="margin-top: 6px; min-height: 1em" role="status"></div>
              <ul id="watch-folders" class="watch-list"></ul>
              <ul id="watch-events" class="watch-list"></ul>
              <p id="watch-hint" class="meta" style="margin: 6px 0 0">
                New or modified .mbox, WhatsApp .txt, and Telegram / Signal .json files in watched
                folders are imported automatically (debounced; re-imports never duplicate).
                Unrecognized files are skipped and reported above.
              </p>
            </section>
          </section>
```

Deleted with the old block: the `#add-source-guidance` paragraph and the old section headings ("Import files" top-level heading is now inside `#sources-import`; "Connect email (IMAP)" and "Folder watching" headings are replaced as above).

- [ ] **Step 4: CSS — sub-section visibility and header-comment fix**

In `src/styles/app.css`:

1. Update the stale file-header comment block at the top (it lists the views) to name: workspace (Archive), sources (Sources), activity (Activity), settings (Settings) — Accounts is gone after Task 3, so write it in terms of the final state minus accounts.
2. Add after the existing per-view show rules (near `#app[data-view="sources"] #view-sources { display: block; }`):

```css
/* --- Sources view: one sub-section at a time ----------------------------- */

.sources-section { display: none; }
#app[data-view="sources"][data-sources-section="home"] #sources-home,
#app[data-view="sources"][data-sources-section="add"] #sources-add,
#app[data-view="sources"][data-sources-section="import"] #sources-import,
#app[data-view="sources"][data-sources-section="email"] #sources-email,
#app[data-view="sources"][data-sources-section="automatic"] #sources-automatic {
  display: block;
}
```

3. Remove `#view-accounts` from the shared scroll/padding selector list only in Task 3 (it still exists this task). No other CSS changes here.

- [ ] **Step 5: `src/workspace.ts` — render the section attributes**

In `render()` (src/workspace.ts:63–76), after `app.dataset.nav = …`:

```ts
    app.dataset.sourcesSection = sourcesSection;
    app.dataset.settingsSection = settingsSection;
```

and extend the destructure at the top of `render()` from `const { view, layout, reading, navOpen } = store.get();` to also include `sourcesSection, settingsSection`.

- [ ] **Step 6: `src/main.ts` — element constants and wiring**

1. Delete the `addSourceGuidance` constant (`const addSourceGuidance = document.querySelector…`) — the element no longer exists.
2. Add the new element constants next to the import constants:

```ts
const sourcesAddButton = document.querySelector<HTMLButtonElement>("#sources-add-btn")!;
const sourcesAutomaticButton = document.querySelector<HTMLButtonElement>("#sources-automatic-btn")!;
const addChoiceImportButton = document.querySelector<HTMLButtonElement>("#add-choice-import")!;
const addChoiceEmailButton = document.querySelector<HTMLButtonElement>("#add-choice-email")!;
const addBackButton = document.querySelector<HTMLButtonElement>("#add-back-btn")!;
const importBackButton = document.querySelector<HTMLButtonElement>("#import-back-btn")!;
const emailBackButton = document.querySelector<HTMLButtonElement>("#email-back-btn")!;
const automaticBackButton = document.querySelector<HTMLButtonElement>("#automatic-back-btn")!;
const imapPassLabel = document.querySelector<HTMLElement>("#imap-pass-label")!;
```

3. In the wiring block, replace the first-run handlers and the accounts-add/guidance block:

```ts
firstRunImportButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("import");
  importPickButton.focus();
});
firstRunAccountButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("email");
  imapUserInput.focus();
});
```

Delete the `accountsAddButton.addEventListener(…)` block and the `workspace.store.subscribe(({ view }) => { if (view !== "sources") addSourceGuidance.hidden = true; })` block, and delete the `accountsAddButton` constant with them. The `#accounts-add` button element stays in the (still-present) Accounts view HTML until Task 3 deletes the whole view; for this one commit it is an inert button with no handler.

4. Add the chooser/Back wiring (with the other view wiring):

```ts
sourcesAddButton.addEventListener("click", () => workspace.store.setSourcesSection("add"));
addChoiceImportButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("import");
  importPickButton.focus();
});
addChoiceEmailButton.addEventListener("click", () => {
  workspace.store.setSourcesSection("email");
  imapUserInput.focus();
});
sourcesAutomaticButton.addEventListener("click", () => workspace.store.setSourcesSection("automatic"));
addBackButton.addEventListener("click", () => workspace.store.setSourcesSection("home"));
importBackButton.addEventListener("click", () => workspace.store.setSourcesSection("home"));
emailBackButton.addEventListener("click", () => workspace.store.setSourcesSection("home"));
automaticBackButton.addEventListener("click", () => workspace.store.setSourcesSection("home"));
```

5. `applyImapAuthMode` (src/main.ts:1607–1615) — also hide the now-visible password label:

```ts
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
```

6. `updateVaultChrome` (src/main.ts:1557–1566) — extend the demo lockout list:

```ts
  for (const button of [sourcesAddButton, addChoiceImportButton, addChoiceEmailButton, importPickButton, importFolderButton, imapSyncButton, imapOauthButton, watchButton, watchAddButton]) {
    button.disabled = locked;
  }
```

- [ ] **Step 7: Update tests broken by the restructure, run everything**

Update in `src/main.test.ts`:
- The "Add account guidance lifecycle" test (in the accounts panel describe) — delete it (guidance is gone; the chooser round-trip is covered by the new block above).
- Any test that asserts the old sources headings ("Connect email (IMAP)", "Folder watching") — change the asserted text to the new headings ("Connect email", "Automatic imports"). Search the suite for `"Connect email (IMAP)"` and `"Folder watching"` and update.
- The auto-sync and folder-watching describes keep working (ids unchanged, clicks fire on hidden elements under happy-dom).

Run: `npx vitest run src/main.test.ts`
Expected: PASS — new sub-section tests green, updated suites green.

Run: `npm run check && npx vitest run`
Expected: tsc clean, full unit suite green.

- [ ] **Step 8: Commit**

```bash
git add index.html src/workspace.ts src/styles/app.css src/main.ts src/main.test.ts
git commit -m "feat(ui): sources view sub-sections — chooser, import, email (labels + Advanced), automatic imports"
```

---

### Task 3: Navigation flip — three destinations, Accounts merged into Sources

The nav rail becomes Archive / Sources / Settings with Activity as a secondary bottom link (badge placeholders added here, wired in Task 7). The Accounts view is deleted; its list moves into `#sources-home`. The workspace title becomes "Archive".

**Files:**
- Modify: `index.html` (nav rail; delete `#view-accounts`; add the list block to `#sources-home`)
- Modify: `src/state.ts` (drop `"accounts"` from the union and titles; rename workspace title)
- Modify: `src/workspace.ts` (nothing new — section attrs already done)
- Modify: `src/styles/app.css` (secondary nav + badge styles; drop `#view-accounts` rules; `#nav-foot` margin change)
- Modify: `src/main.ts` (element renames `#accounts-*` → `#sources-*`; delete `accountsAddButton`; render list on the sources home; boot refresh unchanged)
- Test: `src/state.test.ts`, `src/main.test.ts`

**Interfaces:**
- Consumes: `renderAccounts` (src/accountsview.ts — unchanged this task), `refreshAccounts`/`renderAccountsPanel` (main.ts).
- Produces:
  - `WorkspaceView = "workspace" | "sources" | "activity" | "settings"` (final)
  - `VIEW_TITLES = { workspace: "Archive", sources: "Sources", activity: "Activity", settings: "Settings" }`
  - DOM ids: `#sources-status`, `#sources-list` (the `<ul>`), `#sources-refresh`; `#nav-secondary`, `#activity-count`, `#activity-attention` (hidden placeholders); nav buttons: Archive / Sources / Settings (primary), Activity (secondary).
  - Task 7 consumes `#activity-count` / `#activity-attention`.

- [ ] **Step 1: Write the failing tests**

`src/state.test.ts` — update the `VIEW_TITLES` test (currently asserts non-empty titles) to pin the final map and union:

```ts
it("VIEW_TITLES covers exactly the four destinations", () => {
  expect(VIEW_TITLES).toEqual({
    workspace: "Archive",
    sources: "Sources",
    activity: "Activity",
    settings: "Settings",
  });
});
```

`src/main.test.ts` — in the workspace-shell describe, update the navigation test to the new rail and add the secondary-link assertions:

```ts
it("navigation switches views with aria-current; results stay mounted", async () => {
  await boot();
  const buttons = [...document.querySelectorAll(".nav-view-btn")].map(
    (b) => (b as HTMLElement).dataset.viewTarget,
  );
  expect(buttons).toEqual(["workspace", "sources", "settings", "activity"]);
  const app = $("#app") as HTMLElement;
  const archiveBtn = $('.nav-view-btn[data-view-target="workspace"]') as HTMLElement;
  expect(archiveBtn.getAttribute("aria-current")).toBe("page");
  ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
  await vi.waitFor(() => expect(app.dataset.view).toBe("sources"));
  expect(archiveBtn.getAttribute("aria-current")).toBe(null);
  expect(($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).getAttribute("aria-current")).toBe("page");
  expect($("#results")).toBeTruthy(); // results stay mounted
  expect($("#topbar-title").textContent).toBe("Sources");
});
```

(The exact existing test at this name asserts five destinations and "All messages" — replace its destination list and title expectations rather than adding a duplicate.)

In the accounts-panel describe, replace the "Accounts nav destination + drawer auto-close" test with:

```ts
it("the sources home lists accounts; no Accounts destination exists", async () => {
  await boot();
  tauri.handlers.set("accounts_list", () => [
    {
      source_id: "inst-mail",
      kind: "imap",
      label: "imap:imap.gmail.com:993:ana@gmail.com",
      imap: {
        host: "imap.gmail.com", port: 993, username: "ana@gmail.com",
        folders: ["INBOX"], has_password: true, has_oauth: false,
      },
      message_count: 10, failed_items: 0, state: "connected", excluded: false,
      last_attempt_at: null, last_success_at: "2026-01-02 10:00:00",
      last_error: null, last_error_at: null,
    },
  ]);
  ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
  await vi.waitFor(() => expect($("#sources-list .account-row").textContent).toContain("ana@gmail.com"));
  expect($('.nav-view-btn[data-view-target="accounts"]')).toBe(null);
  expect($("#view-accounts")).toBe(null);
});
```

And update the panel tests that reference `#accounts-status` / `#accounts-list` / `#accounts-refresh` to `#sources-status` / `#sources-list` / `#sources-refresh` (the listing/sync-now/pause/forget assertions themselves stay).

- [ ] **Step 2: Run to verify failure**

Run: `npx vitest run src/state.test.ts src/main.test.ts`
Expected: FAIL — titles mismatch ("All messages"/"Add source"/"Accounts"), `#view-accounts` still present, `#sources-list` missing.

- [ ] **Step 3: `src/state.ts` — final union and titles**

```ts
/** Navigation destinations. The reading surface is Archive; source
 * management, activity, and settings live outside it. */
export type WorkspaceView = "workspace" | "sources" | "activity" | "settings";

export const VIEW_TITLES: Record<WorkspaceView, string> = {
  workspace: "Archive",
  sources: "Sources",
  activity: "Activity",
  settings: "Settings",
};
```

- [ ] **Step 4: `index.html` — nav rail, sources home list, delete Accounts**

1. Replace `ul#nav-views` (index.html:17–35) with:

```html
        <ul id="nav-views">
          <li>
            <button type="button" class="nav-view-btn" data-view-target="workspace" aria-current="page">
              Archive
            </button>
          </li>
          <li>
            <button type="button" class="nav-view-btn" data-view-target="sources">Sources</button>
          </li>
          <li>
            <button type="button" class="nav-view-btn" data-view-target="settings">Settings</button>
          </li>
        </ul>
```

2. After `section#nav-sources` (the source chips block) and before `p#nav-foot`, insert:

```html
        <ul id="nav-secondary" aria-label="Maintenance">
          <li>
            <button type="button" class="nav-view-btn secondary" data-view-target="activity">
              Activity
              <span id="activity-count" class="nav-badge" hidden></span>
              <span id="activity-attention" class="nav-attention" hidden>needs attention</span>
            </button>
          </li>
        </ul>
```

3. Change the topbar default title text (`index.html:50`) from "All messages" to "Archive".
4. Delete the whole `#view-accounts` section (index.html:299–318, including its comment).
5. Inside `#sources-home`, after the Add source row, add the list block (content relocated from the deleted view):

```html
              <div class="row" style="margin-top: 6px">
                <span id="sources-status" class="meta" style="flex: 1" role="status">…</span>
                <div class="spacer"></div>
                <button id="sources-refresh" type="button">Refresh</button>
              </div>
              <ul id="sources-list" class="accounts-list" tabindex="-1" aria-label="Sources"></ul>
```

- [ ] **Step 5: `src/styles/app.css`**

1. Delete `#app[data-view="accounts"] #view-accounts { display: block; }` and remove `#view-accounts` from the shared `#view-sources, #view-accounts, …` scroll/padding rule and from the compact `@media (max-width: 799.98px)` padding list.
2. Move the bottom-of-nav layout from `#nav-foot` to the secondary list, and style the badge/attention flag:

```css
#nav-secondary {
  list-style: none;
  margin: auto 0 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.nav-view-btn.secondary { color: var(--muted); }
.nav-badge {
  display: inline-block;
  min-width: 18px;
  margin-left: 6px;
  padding: 0 6px;
  border-radius: var(--radius-pill);
  background: var(--accent);
  color: var(--accent-fg);
  font-size: 12px;
  text-align: center;
}
.nav-attention {
  margin-left: 6px;
  font-size: 12px;
  color: var(--danger);
}
#nav-foot {
  margin-top: var(--sp-2);
  color: var(--muted);
  font-size: 12px;
}
```

(Replace the existing `#nav-foot { margin-top: auto; … }` rule with the one above.)

- [ ] **Step 6: `src/main.ts` — renames and deletion**

1. Rename the constants and their selectors:

```ts
const sourcesStatus = document.querySelector<HTMLElement>("#sources-status")!;
const sourcesListEl = document.querySelector<HTMLUListElement>("#sources-list")!;
const sourcesRefreshButton = document.querySelector<HTMLButtonElement>("#sources-refresh")!;
```

Delete `accountsStatus`, `accountsListEl`, `accountsRefreshButton`, `accountsAddButton` and update every reference (`renderAccountsPanel`, `refreshAccounts`, `syncAccountNow`, `toggleAccountPause`, `forgetAccountCredentials`, the `#accounts-refresh` wiring — which becomes `sourcesRefreshButton.addEventListener("click", () => void refreshAccounts());`).

- [ ] **Step 7: Run everything**

Run: `npm run check && npx vitest run`
Expected: tsc clean; all suites green. If `main.test.ts` still references `#accounts-*` anywhere (grep it), update those selectors.

- [ ] **Step 8: Commit**

```bash
git add index.html src/state.ts src/styles/app.css src/main.ts src/state.test.ts src/main.test.ts
git commit -m "feat(ui): three primary destinations; Accounts merged into the Sources home"
```

---

### Task 4: Stable-ID source selection

Source chips and `state.sources` switch from display names to stable source-instance ids; same-named sources become independent and visually disambiguated. This is the foundation for the Sources list's "View messages" (Task 5).

**Files:**
- Modify: `src/render.ts` (`SourceChipsOptions`, `renderSourceChips`)
- Modify: `src/accountsview.ts` (new pure helper `sourceChipRows`)
- Modify: `src/main.ts` (`state.sources` semantics, `readFilters`, `writeFilters`, `removeFilterChip`, `refreshChips`, `refreshStats`, `refreshAccounts`, a module accounts cache, the filter-chip label resolver in `renderAll`)
- Modify: `src/state.ts` (doc comment on `UiState.sources`)
- Test: `src/accountsview.test.ts`, `src/main.test.ts`

**Interfaces:**
- Consumes: `SourceInstance` (src/types.ts:269), `AccountView` (types.ts:290), `SourceCount` (types.ts:15).
- Produces:
  - `interface SourceChipRow { id: string; label: string; count: number | null }`
  - `sourceChipRows(instances: SourceInstance[], accounts: AccountView[], counts: SourceCount[]): SourceChipRow[]` (accountsview.ts)
  - `renderSourceChips(el, { rows: SourceChipRow[]; selected: string[]; onToggle: (id: string) => void })` (render.ts — new signature)
  - `state.sources: string[]` now holds **source instance ids**; `filterChips` source values are ids (unchanged); the label resolver in `renderAll` maps id → disambiguated chip label.

- [ ] **Step 1: Write the failing tests**

`src/accountsview.test.ts` — add:

```ts
describe("sourceChipRows", () => {
  const inst = (id: string, label: string): SourceInstance => ({
    id, kind: "legacy", label, connector_key: "mbox", config_json: "{}",
    state: "connected", excluded: false,
    last_attempt_at: null, last_success_at: null, created_at: "2026-01-01 00:00:00",
  });
  const imap = (id: string, label: string, user: string, host: string): AccountView => ({
    source_id: id, kind: "imap", label, imap: {
      host, port: 993, username: user, folders: [], has_password: true, has_oauth: false,
    }, message_count: 0, failed_items: 0, state: "connected", excluded: false,
    last_attempt_at: null, last_success_at: null, last_error: null, last_error_at: null,
  });
  const imp = (id: string, label: string): AccountView => ({
    source_id: id, kind: "import", label, imap: null,
    message_count: 0, failed_items: 0, state: "connected", excluded: false,
    last_attempt_at: null, last_success_at: null, last_error: null, last_error_at: null,
  });

  it("one row per instance, keyed by id, with the label's count when unique", () => {
    const rows = sourceChipRows(
      [inst("a", "mbox"), inst("b", "demo")],
      [imp("a", "mbox"), imp("b", "demo")],
      [{ source: "mbox", count: 3 }, { source: "demo", count: 5 }],
    );
    expect(rows).toEqual([
      { id: "a", label: "mbox", count: 3 },
      { id: "b", label: "demo", count: 5 },
    ]);
  });

  it("duplicate labels are disambiguated and lose the shared count", () => {
    const rows = sourceChipRows(
      [inst("a", "chat"), inst("b", "chat"), inst("c", "chat")],
      [imp("a", "chat"), imap("b", "chat", "ana@gmail.com", "imap.gmail.com"), imp("c", "chat")],
      [{ source: "chat", count: 9 }],
    );
    expect(rows.map((r) => r.id)).toEqual(["a", "b", "c"]);
    expect(rows.every((r) => r.count === null)).toBe(true);
    expect(rows.map((r) => r.label)).toEqual([
      "chat · import",
      "chat · ana@gmail.com · imap.gmail.com",
      "chat · import 2",
    ]);
  });

  it("instances without an account row still appear (id, label, label count)", () => {
    const rows = sourceChipRows([inst("x", "solo")], [], [{ source: "solo", count: 1 }]);
    expect(rows).toEqual([{ id: "x", label: "solo", count: 1 }]);
  });
});
```

(Import `SourceInstance`, `AccountView`, `SourceCount` types and `sourceChipRows` from `"./accountsview"` per the file's existing import style.)

`src/main.test.ts` — in the stats-and-chips describe, add:

```ts
it("chips filter by stable id; two same-named sources stay independent", async () => {
  // Override the boot fixtures BEFORE boot so the first stats/sources
  // refresh renders the duplicate-label pair.
  tauri.handlers.set("sources_list", () => [
    { id: "id-1", kind: "legacy", label: "chat", connector_key: "mbox", config_json: "{}",
      state: "connected", excluded: false, last_attempt_at: null, last_success_at: null,
      created_at: "2026-01-01 00:00:00" },
    { id: "id-2", kind: "legacy", label: "chat", connector_key: "mbox", config_json: "{}",
      state: "connected", excluded: false, last_attempt_at: null, last_success_at: null,
      created_at: "2026-01-01 00:00:00" },
  ]);
  tauri.handlers.set("accounts_list", () => []);
  tauri.handlers.set("stats", () => ({ total: 5, by_source: [{ source: "chat", count: 5 }] }));
  await boot();
  await vi.waitFor(() => expect($$("#source-chips .chip").length).toBe(2));
  const chips = $$("#source-chips .chip") as HTMLElement[];
  expect(chips.map((c) => c.dataset.source)).toEqual(["id-1", "id-2"]);
  chips[0].click();
  await vi.waitFor(() => expect(searchCalls().length).toBeGreaterThan(0));
  const args = searchCalls().at(-1)!.args as { source_ids: string[] };
  expect(args.source_ids).toEqual(["id-1"]);
  // The other same-named chip is still unpressed and independent.
  expect(($$("#source-chips .chip")[1] as HTMLElement).classList.contains("on")).toBe(false);
});
```

(Use the file's existing `$`/`$$` helpers; if no `$$` exists, use `[...document.querySelectorAll(...)]`.)

- [ ] **Step 2: Run to verify failure**

Run: `npx vitest run src/accountsview.test.ts src/main.test.ts -t "sourceChipRows"` and `-t "same-named"`
Expected: FAIL — `sourceChipRows` not exported; chips render label-keyed.

- [ ] **Step 3: Implement `sourceChipRows` in `src/accountsview.ts`**

Add below `accountStatsLine`:

```ts
export interface SourceChipRow {
  /** Stable source-instance id (selection key). */
  id: string;
  /** Disambiguated, user-facing label. */
  label: string;
  /** Message count when it belongs to this row alone; null when the
   * label is shared and a per-row count would be a lie. */
  count: number | null;
}

/** Build the navigation chip rows from the canonical instances, the
 * account rows (identity detail), and the label-keyed counts. Selection
 * is by stable id; two sources that share a display label stay
 * independent and get distinct labels. */
export function sourceChipRows(
  instances: SourceInstance[],
  accounts: AccountView[],
  counts: SourceCount[],
): SourceChipRow[] {
  const accountById = new Map(accounts.map((a) => [a.source_id, a]));
  const countByLabel = new Map(counts.map((c) => [c.source, c.count]));
  const labelCounts = new Map<string, number>();
  for (const s of instances) {
    labelCounts.set(s.label, (labelCounts.get(s.label) ?? 0) + 1);
  }
  const importSeen = new Map<string, number>();
  return instances.map((s) => {
    const duplicated = (labelCounts.get(s.label) ?? 0) > 1;
    if (!duplicated) {
      return { id: s.id, label: s.label, count: countByLabel.get(s.label) ?? null };
    }
    const account = accountById.get(s.id);
    let label: string;
    if (account && account.kind === "imap" && account.imap) {
      label = `${s.label} · ${account.imap.username} · ${account.imap.host}`;
    } else {
      const n = (importSeen.get(s.label) ?? 0) + 1;
      importSeen.set(s.label, n);
      label = n === 1 ? `${s.label} · import` : `${s.label} · import ${n}`;
    }
    return { id: s.id, label, count: null };
  });
}
```

(Add `SourceInstance` and `SourceCount` to the type import at the top of accountsview.ts.)

- [ ] **Step 4: New `renderSourceChips` signature in `src/render.ts`**

```ts
import type { SourceChipRow } from "./accountsview";

export interface SourceChipsOptions {
  rows: SourceChipRow[];
  selected: string[];
  onToggle: (id: string) => void;
}

/** Toggle chips for the source filter (none selected = all sources).
 * Selection keys are stable source-instance ids. */
export function renderSourceChips(el: HTMLElement, opts: SourceChipsOptions): void {
  const chips = opts.rows.map((row) => {
    const chip = document.createElement("button");
    chip.type = "button";
    chip.className = "chip" + (opts.selected.includes(row.id) ? " on" : "");
    chip.dataset.source = row.id;
    chip.setAttribute("aria-pressed", opts.selected.includes(row.id) ? "true" : "false");
    chip.textContent =
      row.count === null ? row.label : `${row.label} (${row.count.toLocaleString()})`;
    chip.addEventListener("click", () => opts.onToggle(row.id));
    return chip;
  });
  el.replaceChildren(...chips);
}
```

(Replace the old signature/body at src/render.ts:142–161.)

- [ ] **Step 5: `src/main.ts` — id-based selection**

1. Module cache for accounts, next to `sourceInstances`:

```ts
/** Account rows (id-keyed identity detail) for chip disambiguation and
 * the Sources list; refreshed with `refreshAccounts`. */
let accountsCache: AccountView[] = [];
```

In `refreshAccounts`, after the guard passes: `accountsCache = accounts; renderSourceChipsNow();`

2. Replace `refreshChips` with a row-based renderer and a helper both `refreshStats` and `refreshAccounts` call. Add the module cache for the last stats next to `sourceInstances`:

```ts
/** The last successful `stats` response (label-keyed counts for chips). */
let lastStats: { by_source: { source: string; count: number }[] } | null = null;
```

In `refreshStats`'s success branch, set `lastStats = stats;` right after the guard, and replace the `refreshChips(stats);` call with `renderSourceChipsNow();`. Delete the old `refreshChips` function.

```ts
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
```

3. `readFilters` — ids pass straight through (delete the label→ids fan-out):

```ts
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
```

4. `writeFilters` — ids pass straight through:

```ts
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
```

5. `removeFilterChip` — the chip value already is the id; delete the label fan-out branch:

```ts
function removeFilterChip(chip: FilterChip): void {
  writeFilters(withoutChip(state.filters, chip));
  void runSearch(0);
}
```

6. The filter-chip label resolver in `renderAll` (main.ts:483–490) — resolve through the same disambiguated labels. Add the module map (refreshed by `renderSourceChipsNow` above):

```ts
/** id → disambiguated chip label (shared by nav chips and filter chips). */
let sourceChipLabels = new Map<string, string>();
```

In `renderAll`:

```ts
    chips: filterChips(state.filters, {
      source: (id) => sourceChipLabels.get(id) ?? sourceInstances.find((s) => s.id === id)?.label ?? id,
      import: (id) => importScopes.find((s) => s.import_id === id)?.import_id ?? id,
    }),
```

7. `src/state.ts` — update the `UiState.sources` doc comment:

```ts
  /** Source instance ids selected via chips / the Sources list (stable
   * ids; passed to the search as `sourceIds`). */
  sources: string[];
```

- [ ] **Step 6: Update existing chip tests, run everything**

In `src/main.test.ts`, the stats-and-chips and v2-filters describes assert name-keyed chips and label fan-out (e.g. `dataset.source` equal to a label, or `source_ids` expanded across same-label instances) — update them to ids. Specifically: any fixture with two instances sharing one label that previously expected both ids in `source_ids` after one chip click now expects only that chip's id.

Run: `npm run check && npx vitest run`
Expected: tsc clean; all suites green.

- [ ] **Step 7: Commit**

```bash
git add src/render.ts src/accountsview.ts src/accountsview.test.ts src/main.ts src/state.ts src/main.test.ts
git commit -m "feat(ui): source selection by stable instance id with disambiguated chips"
```

---

### Task 5: Sources list upgrade — View messages, details fold, disconnect

The sources home rows gain "View messages", per-source "Sync now" stays primary for email, and pause / credentials / disconnect move into an expandable "Details" fold. "Update credentials" deep-links into the email form prefilled from the account.

**Files:**
- Modify: `src/accountsview.ts` (replace `renderAccounts` with `renderSourcesList`; new options and disconnect arming)
- Modify: `src/main.ts` (handlers: `viewSourceMessages`, `updateAccountCredentials`, `disconnectAccount`; rewire `renderAccountsPanel`)
- Test: `src/accountsview.test.ts`, `src/main.test.ts`

**Interfaces:**
- Consumes: `setSourcesSection` (Task 1), id-based `state.sources` (Task 4), `ipc.sourceSetExcluded`, `ipc.accountForgetCredentials`.
- Produces:

```ts
export interface RenderSourcesListOptions {
  onViewMessages: (account: AccountView) => void;
  onSyncNow: (account: AccountView) => void;
  onTogglePause: (account: AccountView) => void;
  onUpdateCredentials: (account: AccountView) => void;
  onForget: (account: AccountView) => void;
  onDisconnect: (account: AccountView) => void;
}
export function renderSourcesList(
  el: HTMLElement,
  accounts: AccountView[],
  opts: RenderSourcesListOptions,
): void;
```

- DOM per row: `li.account-row[data-source-id]` with `.account-main` (title/kind/stats/sync), a primary-actions row (View messages; Sync now for imap), and `details.source-details > summary "Details"` holding Pause/Resume, Update credentials…, Forget credentials, Disconnect… (imap only). Disconnect is two-step: the first click arms it ("Confirm disconnect"), the second fires `onDisconnect`; any re-render resets it.

- [ ] **Step 1: Write the failing tests**

`src/accountsview.test.ts` — replace the `renderAccounts` DOM tests with `renderSourcesList` equivalents (keep the pure-helper tests untouched):

```ts
describe("renderSourcesList", () => {
  function render(accounts: AccountView[]) {
    const el = document.createElement("ul");
    const opts = {
      onViewMessages: vi.fn(),
      onSyncNow: vi.fn(),
      onTogglePause: vi.fn(),
      onUpdateCredentials: vi.fn(),
      onForget: vi.fn(),
      onDisconnect: vi.fn(),
    };
    renderSourcesList(el, accounts, opts);
    return { el, opts };
  }

  it("shows View messages for every row and Sync now only for email", () => {
    const { el } = render([IMAP_ACCOUNT, IMPORT_ACCOUNT]);
    const rows = [...el.querySelectorAll(".account-row")];
    expect(rows.every((r) => r.querySelector(".source-view-btn"))).toBe(true);
    expect(rows[0].querySelector(".account-sync-btn")).toBeTruthy();
    expect(rows[1].querySelector(".account-sync-btn")).toBe(null);
  });

  it("management actions live inside the Details fold", () => {
    const { el, opts } = render([IMAP_ACCOUNT]);
    const details = el.querySelector("details.source-details")!;
    expect(details.querySelector(".account-pause-btn")).toBeTruthy();
    expect(details.querySelector(".source-update-credentials-btn")).toBeTruthy();
    expect(details.querySelector(".account-forget-btn")).toBeTruthy();
    (details.querySelector(".account-pause-btn") as HTMLElement).click();
    expect(opts.onTogglePause).toHaveBeenCalledWith(IMAP_ACCOUNT);
  });

  it("import rows have primary actions but no management fold", () => {
    const { el } = render([IMPORT_ACCOUNT]);
    expect(el.querySelector("details.source-details")).toBe(null);
    expect(el.querySelector(".source-view-btn")).toBeTruthy();
  });

  it("View messages reports its account", () => {
    const { el, opts } = render([IMPORT_ACCOUNT]);
    (el.querySelector(".source-view-btn") as HTMLElement).click();
    expect(opts.onViewMessages).toHaveBeenCalledWith(IMPORT_ACCOUNT);
  });

  it("Disconnect is two-step and resets on re-render", () => {
    const { el, opts } = render([IMAP_ACCOUNT]);
    const btn = el.querySelector(".source-disconnect-btn") as HTMLButtonElement;
    btn.click();
    expect(opts.onDisconnect).not.toHaveBeenCalled();
    expect(btn.textContent).toBe("Confirm disconnect");
    btn.click();
    expect(opts.onDisconnect).toHaveBeenCalledWith(IMAP_ACCOUNT);
    render([IMAP_ACCOUNT]); // re-render disarms
    const again = el.querySelector(".source-disconnect-btn") as HTMLButtonElement;
    expect(again.textContent).toBe("Disconnect…");
  });
});
```

(`IMAP_ACCOUNT` / `IMPORT_ACCOUNT` — reuse the fixtures the file already builds for `renderAccounts`; rename or extract as needed.)

`src/main.test.ts` — extend the sources-screen describe:

```ts
it("View messages scopes the archive to that source id and returns to results", async () => {
  await bootSearched();
  tauri.handlers.set("accounts_list", () => [IMPORT_ACCOUNT_UI]); // id "inst-mbox"
  ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
  await vi.waitFor(() => expect($("#sources-list .source-view-btn")).toBeTruthy());
  ($("#sources-list .source-view-btn") as HTMLElement).click();
  await vi.waitFor(() => expect(($("#app") as HTMLElement).dataset.view).toBe("workspace"));
  const args = searchCalls().at(-1)!.args as { source_ids: string[] };
  expect(args.source_ids).toEqual(["inst-mbox"]);
});

it("Update credentials prefills the email form from the account", async () => {
  await boot();
  tauri.handlers.set("accounts_list", () => [IMAP_ACCOUNT_UI]); // username ana@gmail.com, host imap.gmail.com, folders ["INBOX"]
  ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
  await vi.waitFor(() => expect($("#sources-list .source-update-credentials-btn")).toBeTruthy());
  ($("#sources-list .source-update-credentials-btn") as HTMLElement).click();
  await vi.waitFor(() =>
    expect(($("#app") as HTMLElement).dataset.sourcesSection).toBe("email"),
  );
  expect((input("#imap-user") as HTMLInputElement).value).toBe("ana@gmail.com");
  expect((input("#imap-host") as HTMLInputElement).value).toBe("imap.gmail.com");
  expect((input("#imap-folders") as HTMLInputElement).value).toBe("INBOX");
});

it("Disconnect pauses the source, forgets credentials, and says the archive stays", async () => {
  await boot();
  tauri.handlers.set("accounts_list", () => [IMAP_ACCOUNT_UI]);
  ($('.nav-view-btn[data-view-target="sources"]') as HTMLElement).click();
  await vi.waitFor(() => expect($("#sources-list .source-disconnect-btn")).toBeTruthy());
  const btn = $("#sources-list .source-disconnect-btn") as HTMLButtonElement;
  btn.click();
  btn.click();
  await vi.waitFor(() =>
    expect(tauri.calls.some((c) => c.cmd === "source_set_excluded" && (c.args as { excluded: boolean }).excluded === true)).toBe(true),
  );
  await vi.waitFor(() =>
    expect(tauri.calls.some((c) => c.cmd === "account_forget_credentials")).toBe(true),
  );
  await vi.waitFor(() =>
    expect($("#sources-status").textContent).toContain("archive stays"),
  );
});
```

(Define `IMAP_ACCOUNT_UI` / `IMPORT_ACCOUNT_UI` fixtures in the describe with `source_id` values matching the boot `sources_list` fixture ids — `inst-mbox` — so View messages and chips agree.)

- [ ] **Step 2: Run to verify failure**

Run: `npx vitest run src/accountsview.test.ts src/main.test.ts -t "renderSourcesList"` / `-t "Disconnect"`
Expected: FAIL — `renderSourcesList` not exported; no `.source-view-btn`.

- [ ] **Step 3: Implement `renderSourcesList` in `src/accountsview.ts`**

Replace `RenderAccountsOptions` + `renderAccounts` (keep the empty-state rendering, now reworded):

```ts
export interface RenderSourcesListOptions {
  onViewMessages: (account: AccountView) => void;
  onSyncNow: (account: AccountView) => void;
  onTogglePause: (account: AccountView) => void;
  onUpdateCredentials: (account: AccountView) => void;
  onForget: (account: AccountView) => void;
  onDisconnect: (account: AccountView) => void;
}

/** The empty-state line when no source exists yet. */
export function sourcesEmptyLine(): string {
  return "No sources yet — import an export or connect an email account and it appears here.";
}

/** Render the Sources list (replaces children): one row per source —
 * title, kind, stats, sync state — with View messages (and Sync now for
 * email) as the primary actions, and pause/credentials/disconnect behind
 * a Details fold. Every control is a real focusable button with an
 * account-scoped accessible name. Disconnect is two-step: the first
 * click arms the confirmation, the second fires; a re-render disarms. */
export function renderSourcesList(
  el: HTMLElement,
  accounts: AccountView[],
  opts: RenderSourcesListOptions,
): void {
  if (accounts.length === 0) {
    const empty = document.createElement("li");
    empty.className = "account-row empty";
    const line = document.createElement("span");
    line.className = "meta";
    line.textContent = sourcesEmptyLine();
    empty.append(line);
    el.replaceChildren(empty);
    return;
  }
  const rows = accounts.map((account) => {
    const li = document.createElement("li");
    li.className = "account-row";
    li.dataset.sourceId = account.source_id;

    const main = document.createElement("div");
    main.className = "account-main";
    const title = document.createElement("span");
    title.className = "account-label";
    const displayName = accountTitle(account);
    title.textContent = displayName;
    const titleDetail = accountTitleDetail(account);
    if (titleDetail) title.title = titleDetail;
    const kind = document.createElement("span");
    kind.className = "account-kind meta";
    kind.textContent = accountKindLabel(account);
    const stats = document.createElement("div");
    stats.className = "account-stats meta";
    stats.textContent = accountStatsLine(account);
    const sync = document.createElement("div");
    sync.className = "account-sync meta";
    sync.textContent = describeAccountSync(account);
    main.append(title, kind, stats, sync);
    li.append(main);

    const actions = document.createElement("div");
    actions.className = "account-actions";
    const view = document.createElement("button");
    view.type = "button";
    view.className = "source-view-btn";
    view.textContent = "View messages";
    view.title = `Show this source's messages in the archive (keeps your search text)`;
    view.setAttribute("aria-label", `View messages of source ${displayName}`);
    view.addEventListener("click", () => opts.onViewMessages(account));
    actions.append(view);

    if (account.kind === "imap") {
      const syncNow = document.createElement("button");
      syncNow.type = "button";
      syncNow.className = "account-sync-btn";
      syncNow.textContent = "Sync now";
      syncNow.title = `Sync ${displayName} now, reusing its stored credentials`;
      syncNow.setAttribute("aria-label", `Sync account ${displayName} now`);
      syncNow.addEventListener("click", () => opts.onSyncNow(account));
      actions.append(syncNow);
    }
    li.append(actions);

    if (account.kind === "imap") {
      const details = document.createElement("details");
      details.className = "source-details";
      const summary = document.createElement("summary");
      summary.textContent = "Details";
      details.append(summary);

      const pause = document.createElement("button");
      pause.type = "button";
      pause.className = "account-pause-btn";
      pause.textContent = account.excluded ? "Resume" : "Pause";
      pause.title = account.excluded
        ? `Include ${displayName} in auto-sync rounds again`
        : `Skip ${displayName} in auto-sync rounds (archive and cursors kept)`;
      pause.setAttribute(
        "aria-label",
        `${account.excluded ? "Resume" : "Pause"} account ${displayName}`,
      );
      pause.addEventListener("click", () => opts.onTogglePause(account));

      const update = document.createElement("button");
      update.type = "button";
      update.className = "source-update-credentials-btn";
      update.textContent = "Update credentials…";
      update.title = `Re-enter the password or Google sign-in for ${displayName}`;
      update.setAttribute("aria-label", `Update credentials of account ${displayName}`);
      update.addEventListener("click", () => opts.onUpdateCredentials(account));

      const forget = document.createElement("button");
      forget.type = "button";
      forget.className = "account-forget-btn";
      forget.textContent = "Forget credentials";
      forget.title = `Remove ${displayName}'s stored password and Google sign-in from the OS keyring (the archive stays)`;
      forget.setAttribute("aria-label", `Forget stored credentials of account ${displayName}`);
      forget.addEventListener("click", () => opts.onForget(account));

      const disconnect = document.createElement("button");
      disconnect.type = "button";
      disconnect.className = "source-disconnect-btn";
      disconnect.textContent = "Disconnect…";
      disconnect.title = `Disconnect ${displayName}: stop syncing and remove its credentials — the archived messages stay`;
      disconnect.setAttribute(
        "aria-label",
        `Disconnect account ${displayName} (stops sync, removes credentials, keeps the archive)`,
      );
      let armed = false;
      disconnect.addEventListener("click", () => {
        if (!armed) {
          armed = true;
          disconnect.textContent = "Confirm disconnect";
          disconnect.title = `Really disconnect ${displayName}: stop syncing and remove its credentials — the archived messages stay`;
          return;
        }
        opts.onDisconnect(account);
      });

      details.append(pause, update, forget, disconnect);
      li.append(details);
    }

    return li;
  });
  el.replaceChildren(...rows);
}
```

Delete the old `RenderAccountsOptions` / `renderAccounts` / `accountsEmptyLine` (update any remaining references — `renderAccountsPanel` in main.ts is the only caller; the next step rewrites it). Update the module header comment to describe the Sources list.

- [ ] **Step 4: `src/main.ts` — handlers and rewiring**

1. Import changes: `renderSourcesList`, `sourcesEmptyLine` (if used) instead of `renderAccounts`/`accountsEmptyLine` from `"./accountsview"`.

2. Replace `renderAccountsPanel`:

```ts
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
```

3. New handlers (place near `syncAccountNow`):

```ts
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
```

- [ ] **Step 5: Update the existing sources-screen tests, run everything**

The old accounts-panel tests assert the flat action row (Sync now / Pause / Forget directly on the row) — update them to open the `details` fold first (`row.querySelector("details")!.open = true;` or clicking the summary) before clicking pause/forget. Keep the sync-now payload assertion (`password: ""`, `folders` passthrough) as-is.

Run: `npm run check && npx vitest run`
Expected: tsc clean; all suites green.

- [ ] **Step 6: Commit**

```bash
git add src/accountsview.ts src/accountsview.test.ts src/main.ts src/main.test.ts
git commit -m "feat(ui): sources list with View messages, Details fold, and disconnect"
```

---

### Task 6: Settings pages — General / Storage & backup / AI assistance

**Files:**
- Modify: `index.html` (`#view-settings` restructure)
- Modify: `src/styles/app.css` (page visibility + settings-nav styles)
- Modify: `src/main.ts` (page wiring, per-page save buttons, aria-current subscriber, storage lazy-load)
- Test: `src/main.test.ts`

**Interfaces:**
- Consumes: `setSettingsSection`, `settingsSection` state (Task 1), `data-settings-section` on `#app` (Task 2/3).
- Produces: `#settings-general`, `#settings-storage`, `#settings-ai` (`.settings-page` sections); `.settings-nav-btn[data-settings-target=general|storage|ai]` switcher buttons; `#settings-save-general`, `#settings-save-ai` (both call `saveSettings`). All existing control ids inside the pages are unchanged (`#theme`, `#notify-toggle`, `#keepalive-toggle`, `#autostart-toggle`, `#backup`, `#restore-flow`, `#recovery-slots`, `#media-store`, `#provider-rows`, `#add-kind`, `#summarize-top`, `#settings-note`, `#env-info`).

- [ ] **Step 1: Write the failing tests**

In `src/main.test.ts`, add:

```ts
describe("settings pages", () => {
  it("renders three pages with a switcher; default is General", async () => {
    await boot();
    expect(($("#app") as HTMLElement).dataset.settingsSection).toBe("general");
    const targets = [...document.querySelectorAll(".settings-nav-btn")].map(
      (b) => (b as HTMLElement).dataset.settingsTarget,
    );
    expect(targets).toEqual(["general", "storage", "ai"]);
    // General holds the theme/notifications/background controls; the
    // backup group lives on the storage page; providers on the AI page.
    expect($("#settings-general")!.contains($("#theme"))).toBe(true);
    expect($("#settings-general")!.contains($("#keepalive-toggle"))).toBe(true);
    expect($("#settings-storage")!.contains($("#backup"))).toBe(true);
    expect($("#settings-storage")!.contains($("#media-store"))).toBe(true);
    expect($("#settings-ai")!.contains($("#provider-rows"))).toBe(true);
  });

  it("switching pages moves aria-current and loads storage data on the storage page", async () => {
    await boot();
    tauri.handlers.set("last_backup", () => null);
    ($('.nav-view-btn[data-view-target="settings"]') as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.view).toBe("settings"),
    );
    expect(tauri.calls.some((c) => c.cmd === "media_usage")).toBe(false);
    ($('.settings-nav-btn[data-settings-target="storage"]') as HTMLElement).click();
    await vi.waitFor(() =>
      expect(($("#app") as HTMLElement).dataset.settingsSection).toBe("storage"),
    );
    await vi.waitFor(() =>
      expect(tauri.calls.some((c) => c.cmd === "media_usage")).toBe(true),
    );
    expect(
      ($('.settings-nav-btn[data-settings-target="storage"]') as HTMLElement).getAttribute("aria-current"),
    ).toBe("page");
    expect(
      ($('.settings-nav-btn[data-settings-target="general"]') as HTMLElement).getAttribute("aria-current"),
    ).toBe(null);
  });

  it("each page's Save posts the same full settings payload", async () => {
    await boot();
    ($('.nav-view-btn[data-view-target="settings"]') as HTMLElement).click();
    ($("#settings-save-general") as HTMLElement).click();
    await vi.waitFor(() =>
      expect(tauri.calls.some((c) => c.cmd === "settings_save")).toBe(true),
    );
  });
});
```

- [ ] **Step 2: Run to verify failure**

Run: `npx vitest run src/main.test.ts -t "settings pages"`
Expected: FAIL — no `.settings-nav-btn`, no `#settings-general`.

- [ ] **Step 3: Restructure `#view-settings` in `index.html`**

Replace the section (index.html:345–434) with:

```html
          <!-- Settings: three clearly separated pages — General,
               Storage & backup, AI assistance. -->
          <section id="view-settings" class="view" aria-label="Settings">
            <h2 class="view-heading">Settings</h2>
            <div class="row settings-nav" aria-label="Settings pages">
              <button type="button" class="settings-nav-btn" data-settings-target="general">General</button>
              <button type="button" class="settings-nav-btn" data-settings-target="storage">Storage &amp; backup</button>
              <button type="button" class="settings-nav-btn" data-settings-target="ai">AI assistance</button>
            </div>

            <section id="settings-general" class="settings-page" aria-label="General">
              <h3>General</h3>
              <div class="row">
                <label class="meta">Theme
                  <select id="theme">
                    <option value="system">System</option>
                    <option value="light">Light</option>
                    <option value="dark">Dark</option>
                  </select>
                </label>
                <label class="meta" title="Native desktop notification when a sync or folder-watch cycle brings new mail; silent while the app window is focused">Desktop notifications
                  <input id="notify-toggle" type="checkbox" checked />
                </label>
              </div>
              <div class="row" style="margin-top: 6px">
                <label class="meta" title="Off by default: closing the window hides the app to the tray instead of exiting, so auto-sync, folder watching, and notifications keep working between opens (the tray menu shows, syncs, and quits)">Keep running in background
                  <input id="keepalive-toggle" type="checkbox" />
                </label>
                <label class="meta" title="Off by default: launch the app automatically when you log in (OS login-item registration; applied when you save)">Start at login
                  <input id="autostart-toggle" type="checkbox" />
                </label>
                <div class="spacer"></div>
                <button id="settings-save-general" type="button">Save</button>
              </div>
            </section>

            <section id="settings-storage" class="settings-page" aria-label="Storage and backup">
              <section id="backup" aria-label="Storage and backup">
                <h2>Storage &amp; backup</h2>
                <p class="meta">
                  A backup is one versioned file with the whole vault: a consistent database snapshot,
                  your settings, and every preserved attachment — each part checksummed. Passwords and
                  API keys are never part of a backup; restored accounts need to be reconnected.
                </p>
                <p id="vault-origin" class="meta" role="status"></p>
                <p id="vault-location" class="meta"></p>
                <div class="row">
                  <span id="backup-status" class="meta" style="flex: 1" role="status"></span>
                  <div class="spacer"></div>
                  <button id="backup-create-btn" type="button" title="Write a versioned backup of the whole vault to a file you pick">Create backup…</button>
                  <button id="restore-pick-btn" type="button" title="Validate a backup file and offer a staged, journaled restore">Restore from backup…</button>
                </div>
                <div id="restore-flow"></div>
                <h3 class="meta" style="margin-top: 12px">Previous vaults</h3>
                <div class="row">
                  <span id="recovery-line" class="meta" style="flex: 1" role="status"></span>
                  <div class="spacer"></div>
                  <button id="recovery-refresh-btn" type="button">Refresh</button>
                </div>
                <div id="recovery-slots"></div>
              </section>
              <section id="media-store" aria-label="Media store">
                <h2>Media store</h2>
                <p class="meta">
                  Preserved attachments live in the app data dir, content-addressed by SHA-256 and served to
                  the reading pane read-only. Nothing is ever fetched remotely to display media.
                </p>
                <div class="row">
                  <span id="media-store-status" class="meta" style="flex: 1" role="status"></span>
                  <div class="spacer"></div>
                  <button id="media-refresh-btn" type="button">Refresh</button>
                  <button id="media-reconcile-btn" type="button" title="Verify every object's hash against its name and the archive's references">Reconcile…</button>
                </div>
                <div id="media-usage"></div>
              </section>
            </section>

            <section id="settings-ai" class="settings-page" aria-label="AI assistance">
              <h3>AI assistance</h3>
              <div id="settings-note" class="meta" role="status"></div>
              <div id="env-info" class="meta"></div>
              <div id="provider-rows"></div>
              <div class="row">
                <label class="meta">Add provider
                  <select id="add-kind" aria-label="Provider kind">
                    <option value="zai">z.ai GLM (default)</option>
                    <option value="anthropic">Anthropic</option>
                    <option value="custom">Custom endpoint</option>
                  </select>
                </label>
                <button id="add-provider-btn" type="button">Add provider</button>
              </div>
              <div class="row">
                <label class="meta" title="Default provider for Ask; none = environment variables when set, else local-only">
                  <input type="radio" name="default-provider" value="" /> automatic (environment)
                </label>
                <label class="meta" title="How many top results are sent for the summary">Results for summary
                  <input id="summarize-top" type="number" min="1" max="50" style="max-width: 70px" />
                </label>
                <div class="spacer"></div>
                <button id="settings-save-ai" type="button">Save</button>
              </div>
              <p class="meta" style="font-size: 12px; margin-top: 8px">
                Providers live in settings.json in the app data dir; API keys are stored in the OS keyring
                (session memory when none is available) — never in the settings file or the database.
                Text is set in the bundled Source Sans 3 (SIL OFL 1.1), falling back to your system font.
              </p>
            </section>
          </section>
```

The `#backup` and `#media-store` blocks above are byte-for-byte the current content (index.html:348–371 and 420–433) — moved, not edited. The old wrapper `<section id="settings">…</section>` is gone; the font/keyring footnote paragraph moves to the AI page as shown.

- [ ] **Step 4: CSS**

```css
/* --- Settings view: one page at a time ----------------------------------- */

.settings-page { display: none; }
#app[data-view="settings"][data-settings-section="general"] #settings-general,
#app[data-view="settings"][data-settings-section="storage"] #settings-storage,
#app[data-view="settings"][data-settings-section="ai"] #settings-ai {
  display: block;
}
.settings-nav { margin-bottom: var(--sp-3); }
.settings-nav-btn {
  padding: var(--sp-1) var(--sp-2);
  font: inherit;
  color: var(--text);
  background: transparent;
  border: 1px solid var(--border);
  border-radius: var(--radius-s);
  cursor: pointer;
}
.settings-nav-btn:hover { background: var(--panel-2); }
.settings-nav-btn[aria-current="page"] {
  background: var(--selection);
  border-color: var(--border-strong);
  font-weight: 600;
}
```

- [ ] **Step 5: `src/main.ts` wiring**

1. Replace the `settingsSaveButton` constant with two:

```ts
const settingsSaveGeneralButton = document.querySelector<HTMLButtonElement>("#settings-save-general")!;
const settingsSaveAiButton = document.querySelector<HTMLButtonElement>("#settings-save-ai")!;
```

Update the wiring (`settingsSaveButton.addEventListener` → both new buttons call `() => void saveSettings()`).

2. Page switcher wiring + aria-current, next to the other view wiring:

```ts
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
```

(`SettingsSection` must be added to the `./state` type import in main.ts.)

3. Storage lazy-load — replace the existing settings subscriber:

```ts
workspace.store.subscribe((current) => {
  if (current.view === "settings" && current.settingsSection === "storage") {
    void refreshMediaUsage();
    void refreshRecovery();
    void refreshLastBackup();
  }
});
```

- [ ] **Step 6: Update broken tests, run everything**

- The "storage & backup panel (U05)" test asserting `#view-settings > section` order (`backup` first, `settings` after) — replace with the settings-pages assertions from Step 1 plus a storage-page check that `#settings-storage > #backup` precedes `#media-store`:

```ts
it("the storage page leads with Storage & backup, then the media store", async () => {
  await boot();
  const storage = $("#settings-storage")!;
  const kids = [...storage.querySelectorAll(":scope > section")].map((s) => s.id);
  expect(kids).toEqual(["backup", "media-store"]);
});
```

- The M7 keep-alive toggle tests and the exact `settings_save` payload test keep passing (ids unchanged, `collectSettings` untouched — it reads all inputs regardless of page visibility).

Run: `npm run check && npx vitest run`
Expected: tsc clean; all suites green.

- [ ] **Step 7: Commit**

```bash
git add index.html src/styles/app.css src/main.ts src/main.test.ts
git commit -m "feat(ui): settings split into General / Storage & backup / AI assistance pages"
```

---

### Task 7: Activity badge and attention indicator

**Files:**
- Modify: `src/jobsview.ts` (new `activeJobCount`)
- Modify: `src/main.ts` (badge elements, `renderActivityBadge`, attention set/clear)
- Test: `src/jobsview.test.ts`, `src/main.test.ts`

**Interfaces:**
- Consumes: `#activity-count` / `#activity-attention` (Task 3 DOM), the `jobs` tracker and the `"job-event"` listener (main.ts:1986–2050).
- Produces: `activeJobCount(tracker: JobsTrackerState): number` (jobsview.ts); nav badge shows the count of queued+running jobs; `#activity-attention` ("needs attention") becomes visible when a job fails while Activity is not open, and hides when Activity opens.

- [ ] **Step 1: Write the failing tests**

`src/jobsview.test.ts` — add (fixtures per the file's existing JobEvent builders):

```ts
it("activeJobCount counts queued and running jobs only", () => {
  const tracker = newJobsTracker();
  applyJobEvent(tracker, { seq: 1, type: "submitted", job: 1, kind: "import_mbox", at: "t" });
  applyJobEvent(tracker, { seq: 2, type: "started", job: 1, at: "t" });
  applyJobEvent(tracker, { seq: 3, type: "submitted", job: 2, kind: "imap_sync_all", at: "t" });
  applyJobEvent(tracker, { seq: 4, type: "succeeded", job: 2, at: "t", result: null });
  expect(activeJobCount(tracker)).toBe(1);
});
```

`src/main.test.ts` — add:

```ts
describe("activity badge", () => {
  it("shows the active-job count and clears it when jobs finish", async () => {
    await boot();
    const listeners = tauriEvents.listeners.get("job-event")!;
    const emit = (payload: unknown) => listeners.forEach((l) => l(payload));
    emit({ seq: 1, type: "submitted", job: 7, kind: "import_mbox", at: "t" });
    emit({ seq: 2, type: "started", job: 7, at: "t" });
    await vi.waitFor(() => expect(($("#activity-count") as HTMLElement).hidden).toBe(false));
    expect($("#activity-count").textContent).toBe("1");
    emit({ seq: 3, type: "succeeded", job: 7, at: "t", result: null });
    await vi.waitFor(() => expect(($("#activity-count") as HTMLElement).hidden).toBe(true));
  });

  it("a failed job raises the attention flag until Activity is opened", async () => {
    await boot();
    const listeners = tauriEvents.listeners.get("job-event")!;
    listeners.forEach((l) =>
      l({ seq: 1, type: "submitted", job: 9, kind: "imap_sync_all", at: "t" }),
    );
    listeners.forEach((l) =>
      l({ seq: 2, type: "failed", job: 9, at: "t", error: { kind: "network", message: "down" } }),
    );
    await vi.waitFor(() => expect(($("#activity-attention") as HTMLElement).hidden).toBe(false));
    expect($("#activity-attention").textContent).toContain("needs attention");
    ($('.nav-view-btn[data-view-target="activity"]') as HTMLElement).click();
    await vi.waitFor(() => expect(($("#activity-attention") as HTMLElement).hidden).toBe(true));
  });
});
```

(Use the suite's existing `tauriEvents` mock helper — the notify/jobs tests already emit `job-event` this way; copy their exact payload shape.)

- [ ] **Step 2: Run to verify failure**

Run: `npx vitest run src/jobsview.test.ts src/main.test.ts -t "activeJobCount"` / `-t "activity badge"`
Expected: FAIL — `activeJobCount` not exported; badge stays hidden.

- [ ] **Step 3: Implement**

`src/jobsview.ts` — after `hasActiveJob`:

```ts
/** Number of queued/running jobs — the navigation Activity badge. */
export function activeJobCount(tracker: JobsTrackerState): number {
  return [...tracker.records.values()].filter(
    (r) => r.state.state === "queued" || r.state.state === "running",
  ).length;
}
```

`src/main.ts`:

1. Element constants (near the jobs constants):

```ts
const activityCountBadge = document.querySelector<HTMLElement>("#activity-count")!;
const activityAttentionFlag = document.querySelector<HTMLElement>("#activity-attention")!;
/** A job failed while Activity was not open — surface a textual flag on
 * the nav link until the user opens Activity. */
let activityAttention = false;
```

2. Renderer, called from `renderJobs`:

```ts
function renderActivityBadge(): void {
  const count = activeJobCount(jobs);
  activityCountBadge.textContent = count > 0 ? String(count) : "";
  activityCountBadge.hidden = count === 0;
  activityAttentionFlag.hidden = !activityAttention;
}
```

Append `renderActivityBadge();` inside `renderJobs()` (src/main.ts:1988–1997).

3. Raise attention in the `"job-event"` listener (main.ts:2035–2046), right after `applyJobEvent(jobs, payload);`:

```ts
  if (payload.type === "failed" && workspace.store.get().view !== "activity") {
    activityAttention = true;
  }
```

(`renderJobs` at the end of the listener already re-renders the badge.)

4. Clear on open — add to the wiring block:

```ts
workspace.store.subscribe(({ view }) => {
  if (view === "activity" && activityAttention) {
    activityAttention = false;
    renderActivityBadge();
  }
});
```

- [ ] **Step 4: Run everything**

Run: `npm run check && npx vitest run`
Expected: tsc clean; all suites green.

- [ ] **Step 5: Commit**

```bash
git add src/jobsview.ts src/jobsview.test.ts src/main.ts src/main.test.ts
git commit -m "feat(ui): activity nav badge with active-op count and attention flag"
```

---

### Task 8: E2E spec updates, docs alignment, full verification

**Files:**
- Modify: `tests/e2e/harness/ui.mjs` (section navigation helpers)
- Modify: `tests/e2e/specs/16-workspace-layout.e2e.mjs`, `18-accounts-panel.e2e.mjs` (→ sources screen), `22-backup.e2e.mjs`, `23-backup-restart.e2e.mjs`, `24-first-source.e2e.mjs`, `25-import-preview.e2e.mjs`, `09-settings-providers.e2e.mjs`, `10-settings-guards.e2e.mjs`, `zz-tail-polish.e2e.mjs`
- Modify: `docs/ux-specification.md`, `docs/implementation-roadmap.md`, `README.md`

**Interfaces:**
- Consumes: everything above (final DOM).
- Produces: `ui.openSourcesSection(browser, section)` and `ui.openSettingsSection(browser, section)` helpers; updated specs; docs recording the Phase 1 restructure.

- [ ] **Step 1: Extend the e2e harness**

In `tests/e2e/harness/ui.mjs`, after `openView`:

```js
export async function openSourcesSection(browser, section) {
  await openView(browser, "sources");
  if (section === "home") return;
  if (section === "add") {
    await browser.$("#sources-add-btn").click();
  } else if (section === "import") {
    await openSourcesSection(browser, "add");
    await browser.$("#add-choice-import").click();
  } else if (section === "email") {
    await openSourcesSection(browser, "add");
    await browser.$("#add-choice-email").click();
  } else if (section === "automatic") {
    await browser.$("#sources-automatic-btn").click();
  }
  await browser.waitUntil(async () =>
    (await browser.$("#app").getAttribute("data-sources-section")) === section,
  );
}

export async function openSettingsSection(browser, section) {
  await openView(browser, "settings");
  if (section !== "general") {
    await browser.$(`.settings-nav-btn[data-settings-target="${section}"]`).click();
  }
  await browser.waitUntil(async () =>
    (await browser.$("#app").getAttribute("data-settings-section")) === section,
  );
}
```

- [ ] **Step 2: Update the affected specs**

Per spec (mechanical updates — keep every existing assertion that still describes real behavior):
- `16-workspace-layout`: nav button list/labels (Archive / Sources / Settings + secondary Activity); any screenshot baselines of the nav rail are regenerated with synthetic data per the docs' screenshot rule (commit new fixtures only if the spec compares against committed images).
- `18-accounts-panel.e2e.mjs` → rename to `18-sources-screen.e2e.mjs`: navigate via `openView(browser, "sources")`; assert list rows render on the home section; assert the Details fold exposes pause/forget/disconnect for the IMAP fixture; assert `View messages` lands on the archive scoped to the source id.
- `22-backup` / `23-backup-restart`: replace `#view-settings > section` order assertions with the storage-page assertions (`#settings-storage > section` order `["backup", "media-store"]`), and navigate with `openSettingsSection(browser, "storage")` before interacting with backup controls.
- `24-first-source`: the first-run "Import files…" button now lands on `data-sources-section="import"`; "Connect email…" on `"email"`.
- `25-import-preview`: wrap import interactions with `openSourcesSection(browser, "import")`.
- `09-settings-providers` / `10-settings-guards`: provider controls now live behind `openSettingsSection(browser, "ai")`; save via `#settings-save-ai`.
- `zz-tail-polish`: replace the Add-account guidance round trip with the Add-source chooser round trip (`sources-add-btn` → `add-choice-email` → `email-back-btn` returns home).

Do **not** run the e2e suite as a gate on this host (no WebKitWebDriver — the standing limitation recorded in the roadmap); the specs run in the CI e2e job. Syntax-check them instead:

Run: `node --check tests/e2e/specs/16-workspace-layout.e2e.mjs` (repeat per modified spec, or `for f in tests/e2e/specs/*.mjs; do node --check "$f"; done`)
Expected: no syntax errors.

- [ ] **Step 3: Docs alignment**

1. `docs/ux-specification.md` — in "Workspace and navigation", update the **Current** paragraph: navigation destinations are Archive / Sources / Settings with Activity as a secondary bottom link (active-op count badge, textual "needs attention" flag); the Sources view is the merged screen (home list with View messages / Sync now / Details fold with pause, credentials, disconnect; Add source chooser with Import files and Connect email; Automatic imports section holding auto-sync and folder watching); Settings has General / Storage & backup / AI assistance pages; source chips select by stable instance id with disambiguated duplicate labels. Mark the still-open remainder honestly (per-source status text on chips, import progress surface, date-confirmation flow — unchanged from before).
2. `docs/implementation-roadmap.md` — append a progress-record entry under "Completion tracking" for this slice: revisions (the commits above), changed behaviors (navigation restructure, merged Sources screen, settings pages, id-based selection, disconnect composition of existing commands), tests run (`npx vitest run` full count, `npm run check`), failures/limitations (e2e specs updated but executed only in CI; disconnect is exclusion+forget, not a new backend command; no archive deletion), remaining work (Phase 2–4 of the simplification plan).
3. `README.md` — update the view inventory (no separate Accounts panel; Sources screen; Settings pages).

- [ ] **Step 4: Full local verification**

Run: `npm run check && npx vitest run && npm run build`
Expected: tsc clean; full unit suite green; vite build succeeds.

- [ ] **Step 5: Commit**

```bash
git add tests/e2e docs README.md
git commit -m "test(e2e): update specs for the three-destination navigation; docs record the phase-1 restructure"
```

---

## Verification checklist (Phase 1 exit)

- [ ] Nav shows exactly Archive / Sources / Settings + secondary Activity; no Accounts destination; no "Add source" nav destination.
- [ ] Sources home lists every source (email + imports) with name, type, count, last import/sync, status; primary actions View messages and (email) Sync now; management actions only in the Details fold.
- [ ] "Add source" chooser offers exactly Import files / Connect email; both flows work end-to-end (picker→preview→confirm; IMAP form with visible labels, Advanced server/port/folders, Google sign-in per `oauth_status`).
- [ ] Automatic imports holds auto-sync (off by default) and folder watching; both round-trip.
- [ ] Settings pages: General (theme/notifications/background/login), Storage & backup (backup + restore + previous vaults + media store), AI assistance (providers, default, summarize); every Save posts the full payload; storage data loads only on the storage page.
- [ ] Activity link shows the active-op count; a failure raises "needs attention" until Activity opens.
- [ ] Two same-named sources filter independently (unit-proven); chips and View messages use stable ids.
- [ ] Reading pane survives view switches; Back-to-results still restores selection/scroll; search state untouched by navigation.
- [ ] `npm run check`, `npx vitest run`, `npm run build` green; e2e specs updated (CI executes them).

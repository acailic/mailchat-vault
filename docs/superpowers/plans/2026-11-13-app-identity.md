# App Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a complete in-app identity package — custom stroke icon set, brand-mark lockup, favicon, emoji-glyph replacement, and a polish pass — extending the existing design language (spec: `docs/superpowers/specs/2026-11-13-app-identity-design.md`).

**Architecture:** A static SVG sprite (`<symbol>` definitions) at the end of `index.html` consumed via `<use href="#i-…">`; a `src/icons.ts` helper (`icon()` string + `iconEl()` element) for dynamically rendered modules. Icons are decorative (`aria-hidden`), inherit `currentColor`, and never replace text labels — so e2e selectors (all id/class-based) and accessible names stay stable. The brand mark is inlined directly in the nav `h1` (not via sprite) because its two-tone fill needs CSS variables, which do not cross the `<use>` shadow boundary.

**Tech Stack:** Vanilla TypeScript, plain CSS with design tokens (`src/styles/tokens.css`), Vite, vitest + happy-dom, WebdriverIO e2e. Tauri 2 shell.

## Global Constraints

- **No new dependencies.** Everything is local markup/CSS — CSP is `default-src 'self'` (`src-tauri/tauri.conf.json`).
- **Icons are decorative:** every `<svg>` icon carries `aria-hidden="true" focusable="false"`; a text label always remains next to it.
- **Icons use `currentColor` only** — no raw palette colors in component rules (tokens.css header rule). Status colors pair the glyph with `var(--success)`/`var(--danger)` via the existing row-state classes.
- **Icon geometry:** 24×24 `viewBox`, 1.5px stroke, round caps/joins, ~2px corners; sizing via `.icon` (16px) / `.icon-lg` (20px).
- **All e2e assertions are id/class-based** (verified: `#detail .close-btn`, `.export-chat-btn`, `#nav-toggle`, `data-view-target`). Do not change ids, classes, or remove text labels.
- Commit style: conventional commits, English, matching `git log` (`feat(ui): …`, `test(e2e): …`).
- Test commands: `npm test` (vitest), `npm run check` (tsc), `npm run build`, `npm run test:e2e` (full WebdriverIO suite, needs several minutes).

**Spec deltas this plan implements (documented deviations):**

1. The spec's utility icons (`chevron-down`, `info`, `folder`, `key`) are **deferred** — no wired usage site exists today (YAGNI); the sprite ships the 17 icons that are used. The `chat` and `server` type icons ship in the sprite as part of the type family but only `mail`/`file` are wired (Sources rows key off `AccountView.kind: "imap" | "import"`, `src/types.ts:307`).
2. `#stats` initial value becomes **"Loading…"** (not the spec's "No archive open") — the app always opens an archive, so "No archive open" would be false. Task 6 amends the spec file to match.
3. OS rasters (`src-tauri/icons/*.png|.ico|.icns`) are **kept as-is**; Task 5 adds the vector source `icon.svg` beside them. No Linux tooling exists here to rebuild `.icns` (checked: no `icnsutil`/`png2icns`/`rsvg-convert`; only ImageMagick `convert`), and the existing rasters already match the art.

---

### Task 1: Icon sprite, helper module, and base icon CSS

**Files:**
- Modify: `index.html` (append sprite before `</body>`)
- Create: `src/icons.ts`
- Create: `src/icons.test.ts`
- Modify: `src/styles/app.css` (base `.icon` rules, near the top component section)

**Interfaces:**
- Consumes: nothing (first task).
- Produces: `type IconName` = `"archive" | "sources" | "settings" | "activity" | "menu" | "search" | "close" | "export" | "paperclip" | "check" | "warning" | "error" | "spinner" | "mail" | "chat" | "file" | "server"`; `icon(name: IconName, cls?: string): string`; `iconEl(name: IconName, cls?: string): SVGSVGElement`; sprite symbols `#i-<name>`; CSS `.icon`, `.icon-lg`, `.icon-spin`, `@keyframes icon-spin`.

- [ ] **Step 1: Write the failing test**

Create `src/icons.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { icon, iconEl } from "./icons";

describe("icon helper", () => {
  it("references the sprite symbol with decorative attributes", () => {
    expect(icon("archive")).toBe(
      '<svg class="icon" aria-hidden="true" focusable="false"><use href="#i-archive"></use></svg>',
    );
  });

  it("appends extra classes when given", () => {
    expect(icon("spinner", "icon-spin")).toContain('class="icon icon-spin"');
    expect(icon("spinner", "icon-spin")).toContain('#i-spinner');
  });

  it("builds a DOM element with the same markup", () => {
    const el = iconEl("close");
    expect(el.getAttribute("class")).toBe("icon");
    expect(el.getAttribute("aria-hidden")).toBe("true");
    expect(el.querySelector("use")?.getAttribute("href")).toBe("#i-close");
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `npx vitest run src/icons.test.ts`
Expected: FAIL — `Cannot find module './icons'` (or equivalent resolution error).

- [ ] **Step 3: Create `src/icons.ts`**

```ts
/** Icon names — one entry per `<symbol id="i-…">` in the index.html sprite. */
export type IconName =
  | "archive"
  | "sources"
  | "settings"
  | "activity"
  | "menu"
  | "search"
  | "close"
  | "export"
  | "paperclip"
  | "check"
  | "warning"
  | "error"
  | "spinner"
  | "mail"
  | "chat"
  | "file"
  | "server";

/** Inline icon markup referencing the static sprite in index.html.
 * Decorative by contract: aria-hidden, always accompanied by a text label. */
export function icon(name: IconName, cls = ""): string {
  const classes = cls ? `icon ${cls}` : "icon";
  return `<svg class="${classes}" aria-hidden="true" focusable="false"><use href="#i-${name}"></use></svg>`;
}

/** DOM element variant of `icon` for programmatic rendering. */
export function iconEl(name: IconName, cls = ""): SVGSVGElement {
  const tpl = document.createElement("template");
  tpl.innerHTML = icon(name, cls).trim();
  return tpl.content.firstElementChild as SVGSVGElement;
}
```

- [ ] **Step 4: Add the sprite to `index.html`**

Insert immediately before `</body>`:

```html
    <!-- Icon sprite: stroke-only symbols, 24×24 grid, 1.5px round strokes.
         Referenced via <use href="#i-…">; presentation (color/size/stroke)
         comes from the .icon classes in app.css. Keep symbols free of
         fill/stroke attributes so currentColor inheritance works. -->
    <svg xmlns="http://www.w3.org/2000/svg" hidden>
      <symbol id="i-archive" viewBox="0 0 24 24">
        <rect x="3.5" y="4.5" width="17" height="5" rx="1.2"/>
        <path d="M5 9.5v8.3a1.7 1.7 0 0 0 1.7 1.7h10.6a1.7 1.7 0 0 0 1.7-1.7V9.5"/>
        <path d="M10 13h4"/>
      </symbol>
      <symbol id="i-sources" viewBox="0 0 24 24">
        <path d="M3.5 13.5 6 6h12l2.5 7.5"/>
        <path d="M3.5 13.5v4a1.5 1.5 0 0 0 1.5 1.5h14a1.5 1.5 0 0 0 1.5-1.5v-4"/>
        <path d="M3.5 13.5h5l1.4 2.2h4.2l1.4-2.2h5"/>
      </symbol>
      <symbol id="i-settings" viewBox="0 0 24 24">
        <path d="M4 6.5H7.2"/><circle cx="10" cy="6.5" r="2"/><path d="M12.8 6.5H20"/>
        <path d="M4 12h7.2"/><circle cx="14" cy="12" r="2"/><path d="M16.8 12H20"/>
        <path d="M4 17.5H5.2"/><circle cx="8" cy="17.5" r="2"/><path d="M10.8 17.5H20"/>
      </symbol>
      <symbol id="i-activity" viewBox="0 0 24 24">
        <path d="M3.5 12h4l2.5-6.5 4 13 2.5-6.5h4"/>
      </symbol>
      <symbol id="i-menu" viewBox="0 0 24 24">
        <path d="M4 7h16"/><path d="M4 12h16"/><path d="M4 17h16"/>
      </symbol>
      <symbol id="i-search" viewBox="0 0 24 24">
        <circle cx="11" cy="11" r="6"/><path d="m15.5 15.5 4.5 4.5"/>
      </symbol>
      <symbol id="i-close" viewBox="0 0 24 24">
        <path d="m6 6 12 12"/><path d="M18 6 6 18"/>
      </symbol>
      <symbol id="i-export" viewBox="0 0 24 24">
        <path d="M12 4v10"/><path d="m8 10.5 4 4 4-4"/>
        <path d="M4.5 15.5V18a1.5 1.5 0 0 0 1.5 1.5h12a1.5 1.5 0 0 0 1.5-1.5v-2.5"/>
      </symbol>
      <symbol id="i-paperclip" viewBox="0 0 24 24">
        <path d="M9 11.5l6.3-6.3a3.5 3.5 0 0 1 4.9 4.9l-8.4 8.4a5.5 5.5 0 0 1-7.8-7.8L11 3.7"/>
      </symbol>
      <symbol id="i-check" viewBox="0 0 24 24">
        <path d="m5 12.5 4.5 4.5L19 7.5"/>
      </symbol>
      <symbol id="i-warning" viewBox="0 0 24 24">
        <path d="M12 4.2 21 19.5H3Z"/>
        <path d="M12 10v4.5"/><path d="M12 17.3h.01"/>
      </symbol>
      <symbol id="i-error" viewBox="0 0 24 24">
        <circle cx="12" cy="12" r="7.5"/>
        <path d="m9.5 9.5 5 5"/><path d="m14.5 9.5-5 5"/>
      </symbol>
      <symbol id="i-spinner" viewBox="0 0 24 24">
        <path d="M12 4a8 8 0 1 1-8 8"/>
      </symbol>
      <symbol id="i-mail" viewBox="0 0 24 24">
        <rect x="3.5" y="5.5" width="17" height="13" rx="1.5"/>
        <path d="m4.5 7.5 7.5 5.5 7.5-5.5"/>
      </symbol>
      <symbol id="i-chat" viewBox="0 0 24 24">
        <path d="M4.5 6.5A1.5 1.5 0 0 1 6 5h12a1.5 1.5 0 0 1 1.5 1.5v7a1.5 1.5 0 0 1-1.5 1.5h-7l-4.5 4v-4A1.5 1.5 0 0 1 4.5 13.5Z"/>
        <path d="M9 10h.01"/><path d="M12 10h.01"/><path d="M15 10h.01"/>
      </symbol>
      <symbol id="i-file" viewBox="0 0 24 24">
        <path d="M6 3.5h7.5L18.5 8.5V19a1.5 1.5 0 0 1-1.5 1.5H6A1.5 1.5 0 0 1 4.5 19V5A1.5 1.5 0 0 1 6 3.5Z"/>
        <path d="M13.5 3.5V8.5h5"/><path d="M8.5 12.5h7"/><path d="M8.5 16h7"/>
      </symbol>
      <symbol id="i-server" viewBox="0 0 24 24">
        <rect x="4" y="4.5" width="16" height="6" rx="1.5"/>
        <rect x="4" y="13.5" width="16" height="6" rx="1.5"/>
        <path d="M7.5 7.5h.01"/><path d="M7.5 16.5h.01"/>
      </symbol>
    </svg>
```

- [ ] **Step 5: Add base icon CSS to `src/styles/app.css`**

Add after the existing `[hidden]` guard rule near the top of the file (before the `#app` layout section):

```css
/* --- Icon system: sprite symbols rendered via <use href="#i-…"> -------- */

.icon {
  width: 16px;
  height: 16px;
  flex: none;
  vertical-align: -2px; /* sits on the text baseline when inline */
  fill: none;
  stroke: currentColor;
  stroke-width: 1.5;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.icon-lg {
  width: 20px;
  height: 20px;
}
.icon-spin {
  animation: icon-spin 0.9s linear infinite;
}
@keyframes icon-spin {
  to {
    transform: rotate(360deg);
  }
}
@media (prefers-reduced-motion: reduce) {
  .icon-spin {
    animation: none;
  }
}
```

- [ ] **Step 6: Run the tests and typecheck**

Run: `npx vitest run src/icons.test.ts && npm run check`
Expected: 3 passed; tsc clean.

- [ ] **Step 7: Commit**

```bash
git add index.html src/icons.ts src/icons.test.ts src/styles/app.css
git commit -m "feat(ui): icon sprite, icon() helper, and base icon styles"
```

---

### Task 2: Brand-mark lockup and static wiring (nav, menu toggle, search)

**Files:**
- Modify: `index.html:15` (h1 lockup), `:19-27` (nav buttons), `:36-39` (activity button), `:50-52` (nav toggle), `:69-70` (search button)
- Modify: `src/styles/app.css` (`#app-title`, `.nav-view-btn`, `#nav-toggle` rules)

**Interfaces:**
- Consumes: sprite symbols from Task 1 (`#i-archive`, `#i-sources`, `#i-settings`, `#i-activity`, `#i-menu`, `#i-search`); `.icon`/`.icon-lg` classes.
- Produces: nav buttons contain `<svg class="icon" …><use …></svg><span>Label</span>` markup; `#nav-toggle` accessible name is "Menu"; brand mark classes `.logo-mark`, `.logo-bubble`, `.logo-dots`.

- [ ] **Step 1: Replace the `h1` with the brand lockup**

In `index.html`, replace:

```html
        <h1 id="app-title">Mailchat Vault</h1>
```

with:

```html
        <h1 id="app-title">
          <svg class="icon icon-lg logo-mark" aria-hidden="true" focusable="false">
            <rect x="3" y="4.5" width="13" height="9.5" rx="1.5"/>
            <path d="M3.8 5.9 9.5 10l5.7-4.1"/>
            <path class="logo-bubble" d="M14.6 12h4.1a2 2 0 0 1 2 2v3.2a2 2 0 0 1-2 2h-2.3l-2.1 1.8v-1.8h-.6a2 2 0 0 1-2-2V14a2 2 0 0 1 2-2Z"/>
            <path class="logo-dots" d="M16.3 15.3h.01M18.6 15.3h.01M17.45 17.2h.01"/>
          </svg>
          <span>Mailchat Vault</span>
        </h1>
```

(The mark is inlined, not a sprite `<use>`: its two-tone fill needs `var(--accent)`/`var(--accent-fg)`, and CSS variables do not cross the `<use>` shadow boundary.)

- [ ] **Step 2: Add icons to the three nav view buttons**

Replace the three `#nav-views` buttons (`index.html:19-27`) so each reads (label text unchanged, icon prepended):

```html
            <button type="button" class="nav-view-btn" data-view-target="workspace" aria-current="page">
              <svg class="icon" aria-hidden="true" focusable="false"><use href="#i-archive"></use></svg><span>Archive</span>
            </button>
```

```html
            <button type="button" class="nav-view-btn" data-view-target="sources">
              <svg class="icon" aria-hidden="true" focusable="false"><use href="#i-sources"></use></svg><span>Sources</span>
            </button>
```

```html
            <button type="button" class="nav-view-btn" data-view-target="settings">
              <svg class="icon" aria-hidden="true" focusable="false"><use href="#i-settings"></use></svg><span>Settings</span>
            </button>
```

And the Activity button in `#nav-secondary` (`index.html:36-39`) — badges stay after the label:

```html
            <button type="button" class="nav-view-btn secondary" data-view-target="activity">
              <svg class="icon" aria-hidden="true" focusable="false"><use href="#i-activity"></use></svg><span>Activity</span>
              <span id="activity-count" class="nav-badge" hidden></span>
              <span id="activity-attention" class="nav-attention" hidden>needs attention</span>
            </button>
```

- [ ] **Step 3: Replace the `☰` menu glyph and add the search icon**

Replace the toggle (`index.html:50-52`):

```html
          <button id="nav-toggle" type="button" aria-controls="nav" aria-expanded="false">
            <svg class="icon" aria-hidden="true" focusable="false"><use href="#i-menu"></use></svg><span>Menu</span>
          </button>
```

And the search button (`index.html:70`):

```html
                  <button id="search-btn" class="primary" type="button">
                    <svg class="icon" aria-hidden="true" focusable="false"><use href="#i-search"></use></svg><span>Search</span>
                  </button>
```

- [ ] **Step 4: CSS for lockup and flex button alignment**

In `src/styles/app.css`:

4a. Extend the `#app-title` rule (currently `font-size`/`font-weight`/`margin`, around line 93):

```css
#app-title {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
}
```

4b. Add logo mark color rules right below it:

```css
.logo-mark .logo-bubble {
  fill: var(--accent);
  stroke: none;
}
.logo-mark .logo-dots {
  fill: none;
  stroke: var(--accent-fg);
  stroke-width: 2;
  stroke-linecap: round;
}
```

4c. Change `.nav-view-btn` from `display: block` to flex (rule around `app.css:115`):

```css
.nav-view-btn {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  width: 100%;
  text-align: left;
  padding: var(--sp-1) var(--sp-2);
  font: inherit;
  color: var(--text);
  background: transparent;
  border: 1px solid transparent;
  border-radius: var(--radius-s);
  cursor: pointer;
}
```

4d. Make `#nav-toggle` an inline flex row (find its existing rule; if it has no display rule, add these properties to it):

```css
#nav-toggle {
  display: inline-flex;
  align-items: center;
  gap: var(--sp-1);
}
```

- [ ] **Step 5: Verify — unit tests still green, typecheck, dev render**

Run: `npm run check && npm test`
Expected: all green (no test asserts on nav markup; `main.test.ts` bootstraps `index.html` via happy-dom but asserts on behavior, not nav markup — if any snapshot or assertion fails on the new markup, read the failure and update the expectation to the new markup, keeping assertions on text/behavior, not on icon presence).

- [ ] **Step 6: Commit**

```bash
git add index.html src/styles/app.css
git commit -m "feat(ui): brand-mark lockup and icons in nav, menu toggle, search"
```

---

### Task 3: Replace dynamic text glyphs (close, export, paperclip)

**Files:**
- Modify: `src/render.ts:303` (export button), `:309` (close button)
- Modify: `src/render2.ts:94-105` (attachments badge)
- Modify: `src/render2.test.ts:98` (assertion)
- Modify: `src/__snapshots__/render.test.ts.snap` (via `vitest -u`)
- Modify: `src/styles/app.css` (`kbd` style, `.close-btn`/`.export-chat-btn` alignment)

**Interfaces:**
- Consumes: `icon()` from `src/icons.ts` (Task 1).
- Produces: close button markup `icon("close") + <span>Close</span><kbd>Esc</kbd>`; export button `icon("export") + Export chat…`; attachments badge with `aria-label="attachments: N of M"` and textContent `N/M` (or `N`).

- [ ] **Step 1: Update the attachments assertion first (failing test)**

In `src/render2.test.ts`, replace the assertion at line 98:

```ts
    expect(badges[0].textContent).toBe("📎 1/2");
```

with:

```ts
    expect(badges[0].textContent).toBe("1/2");
    expect(badges[0].getAttribute("aria-label")).toBe("attachments: 1 of 2");
    expect(badges[0].innerHTML).toContain("#i-paperclip");
```

- [ ] **Step 2: Run to verify it fails**

Run: `npx vitest run src/render2.test.ts`
Expected: FAIL — textContent is `"📎 1/2"` and there is no aria-label yet.

- [ ] **Step 3: Rewrite the attachments badge in `src/render2.ts`**

Add the import at the top of `src/render2.ts`:

```ts
import { icon } from "./icons";
```

Replace the badge block (`src/render2.ts:94-105`) — keep `title` logic identical:

```ts
  if (hit.attachment_count > 0) {
    const files = document.createElement("span");
    files.className = "badge attachments";
    files.title =
      hit.available_attachment_count < hit.attachment_count
        ? `${hit.attachment_count} attachment references, ${hit.available_attachment_count} preserved`
        : `${hit.attachment_count} attachments`;
    files.setAttribute(
      "aria-label",
      `attachments: ${hit.available_attachment_count} of ${hit.attachment_count}`,
    );
    files.innerHTML =
      icon("paperclip") +
      (hit.available_attachment_count < hit.attachment_count
        ? `${hit.available_attachment_count}/${hit.attachment_count}`
        : `${hit.attachment_count}`);
    meta.append(" ", files);
  }
```

- [ ] **Step 4: Rewrite the close and export buttons in `src/render.ts`**

Add the import at the top of `src/render.ts`:

```ts
import { icon } from "./icons";
```

Replace (`src/render.ts:303`):

```ts
  exportChat.textContent = "⤓ Export chat…";
```

with:

```ts
  exportChat.innerHTML = `${icon("export")}Export chat…`;
```

Replace (`src/render.ts:309`):

```ts
  close.textContent = "✕ Close (Esc)";
```

with:

```ts
  close.innerHTML = `${icon("close")}<span>Close</span><kbd>Esc</kbd>`;
```

- [ ] **Step 5: Add `kbd` and button alignment CSS to `src/styles/app.css`**

```css
kbd {
  font: 600 11px var(--font-mono);
  padding: 0 4px;
  border: 1px solid var(--border);
  border-bottom-width: 2px;
  border-radius: 3px;
  background: var(--panel-2);
  color: var(--muted);
}

.close-btn,
.export-chat-btn {
  display: inline-flex;
  align-items: center;
  gap: var(--sp-1);
}
```

(If `.close-btn`/`.export-chat-btn` already have rules, merge these properties into them instead of duplicating selectors.)

- [ ] **Step 6: Update snapshots deliberately and run the suite**

Run: `npx vitest run -u src/render.test.ts src/render2.test.ts && npm test`
Expected: snapshot files updated (old `⤓`/`✕`/`📎` strings replaced by the new markup); full unit suite green. Inspect `git diff src/__snapshots__/` — the only changes should be the three detail-panel snapshots' header-actions buttons and the badge markup.

- [ ] **Step 7: Commit**

```bash
git add src/render.ts src/render2.ts src/render2.test.ts src/__snapshots__/render.test.ts.snap src/styles/app.css
git commit -m "feat(ui): icon close/export buttons and paperclip attachment badge"
```

---

### Task 4: Job-state icons and source-type icons

**Files:**
- Modify: `src/jobsview.ts` (`renderJobsList`, around line 227)
- Modify: `src/accountsview.ts` (`renderSourcesList`, around line 253)
- Modify: `src/__snapshots__/jobsview.test.ts.snap`, `src/__snapshots__/accountsview.test.ts.snap` (via `vitest -u`, if these modules snapshot the row markup)
- Modify: `src/styles/app.css` (job state icon colors)

**Interfaces:**
- Consumes: `iconEl()`, `IconName` from `src/icons.ts`.
- Produces: job rows prepend `iconEl("spinner"|"check"|"error")`; Sources rows prepend `iconEl(account.kind === "imap" ? "mail" : "file")`.

- [ ] **Step 1: Add the state glyph to job rows**

In `src/jobsview.ts`, add the import:

```ts
import { iconEl, type IconName } from "./icons";
```

Add this helper above `renderJobsList` (around line 226):

```ts
/** Decorative state glyph for a job row; null when plain text reads best. */
function jobStateIcon(state: string): IconName | null {
  if (state === "running") return "spinner";
  if (state === "succeeded") return "check";
  if (state === "failed") return "error";
  return null;
}
```

In `renderJobsList`, replace:

```ts
      const li = document.createElement("li");
      li.className = `job ${record.state.state}`;
      li.textContent = describeJobState(record);
      return li;
```

with:

```ts
      const li = document.createElement("li");
      li.className = `job ${record.state.state}`;
      const glyph = jobStateIcon(record.state.state);
      if (glyph) li.append(iconEl(glyph, glyph === "spinner" ? "icon-spin" : ""));
      li.append(document.createTextNode(describeJobState(record)));
      return li;
```

- [ ] **Step 2: Add the type glyph to Sources rows**

In `src/accountsview.ts`, add the import:

```ts
import { iconEl } from "./icons";
```

In `renderSourcesList`, right after `main.className = "account-main";` (around line 253), add:

```ts
    main.append(iconEl(account.kind === "imap" ? "mail" : "file"));
```

- [ ] **Step 3: Color the job glyphs by state in `src/styles/app.css`**

```css
.job.succeeded .icon {
  color: var(--success);
}
.job.failed .icon {
  color: var(--danger);
}
```

- [ ] **Step 4: Run tests, update snapshots deliberately**

Run: `npm run check && npx vitest run -u src/jobsview.test.ts src/accountsview.test.ts && npm test`
Expected: typecheck clean; job-row and sources-row snapshots updated; full suite green. Inspect the snapshot diff — only the prepended icon markup should differ.

- [ ] **Step 5: Commit**

```bash
git add src/jobsview.ts src/accountsview.ts src/styles/app.css src/__snapshots__/
git commit -m "feat(ui): job state glyphs and source type icons"
```

---

### Task 5: Favicon and vector icon source

**Files:**
- Create: `public/favicon.svg`
- Create: `public/favicon-32.png` (generated)
- Create: `src-tauri/icons/icon.svg`
- Modify: `index.html:3-8` (head links)

**Interfaces:**
- Consumes: brand-mark geometry from Task 2.
- Produces: `<link rel="icon">`s; `src-tauri/icons/icon.svg` as the vector source of truth beside the existing rasters (rasters unchanged).

- [ ] **Step 1: Sample the brand colors from the existing OS icon art**

Run:

```bash
convert src-tauri/icons/source.png -resize 3x3! -depth 8 txt: | head -15
```

Read the output: the dominant dark color is the navy background (expected near `#1B2E4A`), the bright cyan/teal is the bubble (expected near `#3FB8AF`). Record both hex values; use them verbatim in Steps 2–3. If sampling is ambiguous, use `#1B2E4A` (navy) and `#3FB8AF` (teal) — they match the shipped art.

- [ ] **Step 2: Create `public/favicon.svg`**

Literal colors (no CSS variables — this file is rendered standalone, including by ImageMagick):

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48">
  <rect width="48" height="48" rx="10" fill="NAVY_HEX"/>
  <rect x="8" y="12" width="24" height="17" rx="3" fill="none" stroke="#ffffff" stroke-width="2.5"/>
  <path d="m9.5 14.5 10.5 7.5 10.5-7.5" fill="none" stroke="#ffffff" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>
  <path d="M28.5 26h7.2a3.6 3.6 0 0 1 3.6 3.6v5a3.6 3.6 0 0 1-3.6 3.6h-3.9l-3.3 2.8v-2.8a3.6 3.6 0 0 1-3.6-3.6v-5a3.6 3.6 0 0 1 3.6-3.6Z" fill="TEAL_HEX"/>
  <circle cx="30.5" cy="32.2" r="1.1" fill="#ffffff"/>
  <circle cx="33.6" cy="32.2" r="1.1" fill="#ffffff"/>
  <circle cx="36.7" cy="32.2" r="1.1" fill="#ffffff"/>
</svg>
```

Replace `NAVY_HEX`/`TEAL_HEX` with the sampled values.

- [ ] **Step 3: Generate the PNG fallback**

Run:

```bash
mkdir -p public && convert -background none public/favicon.svg -resize 32x32 public/favicon-32.png && ls -la public/
```

Expected: `favicon.svg` and `favicon-32.png` present, PNG ~1–3 KB. Sanity-check it rendered (not blank): `identify public/favicon-32.png` reports 32x32.

- [ ] **Step 4: Copy the same art as the OS vector source**

Create `src-tauri/icons/icon.svg` with the exact same file content as `public/favicon.svg`. Existing rasters (`*.png`, `.ico`, `.icns`) stay untouched — they already match this art (spec delta #3).

- [ ] **Step 5: Wire the head links in `index.html`**

After the `<title>` line (`index.html:6`), add:

```html
    <link rel="icon" href="/favicon.svg" type="image/svg+xml" />
    <link rel="icon" href="/favicon-32.png" type="image/png" sizes="32x32" />
```

- [ ] **Step 6: Verify the build bundles them**

Run: `npm run build`
Expected: tsc + vite build succeed; `dist/` contains `favicon.svg` and `favicon-32.png` at the root.

- [ ] **Step 7: Commit**

```bash
git add public/ src-tauri/icons/icon.svg index.html
git commit -m "feat(brand): favicon pair and vector icon source"
```

---

### Task 6: Polish pass (stats default, utility classes, active states, spec amendment)

**Files:**
- Modify: `index.html:16` (stats initial), `:62` (demo banner inline style), `:100` (refresh-notice inline style), `:359` (jobs-status inline style), `:367` (dashboard-status inline style)
- Modify: `src/styles/app.css` (utility classes, active states)
- Modify: `docs/superpowers/specs/2026-11-13-app-identity-design.md` (stats-default amendment)

**Interfaces:**
- Consumes: tokens from `src/styles/tokens.css`.
- Produces: utility classes `.grow`, `.mt-s`, `.m-0`; `:active` press feedback on interactive elements; `#stats` initial text "Loading…".

- [ ] **Step 1: Stats initial value**

In `index.html:16`, replace:

```html
        <p id="stats" role="status">…</p>
```

with:

```html
        <p id="stats" role="status">Loading…</p>
```

- [ ] **Step 2: Replace inline styles with utility classes**

In `src/styles/app.css`, add near the `.icon` section:

```css
/* --- Small layout utilities (replace one-off inline styles) ------------- */

.grow {
  flex: 1;
}
.mt-s {
  margin-top: 6px;
}
.m-0 {
  margin: 0;
}
```

In `index.html`:
- Line 62: `<span class="meta" style="flex: 1">Sample archive…</span>` → `<span class="meta grow">Sample archive…</span>`
- Line 100: `<div id="refresh-notice" class="row" style="margin-top: 6px" hidden></div>` → `<div id="refresh-notice" class="row mt-s" hidden></div>`
- Line 359: `<span id="jobs-status" class="meta" style="flex: 1" role="status"></span>` → `<span id="jobs-status" class="meta grow" role="status"></span>`
- Line 367: `<div id="dashboard-status" class="meta" style="margin: 0" role="status"></div>` → `<div id="dashboard-status" class="meta m-0" role="status"></div>`

Search for any remaining `style="` in `index.html` and confirm none are layout one-offs that the utilities cover; leave genuinely unique styles alone.

- [ ] **Step 3: Consistent press feedback**

In `src/styles/app.css`, add after the existing transition rule near line 865 (`.hit-item, .chip, button, .nav-view-btn { transition: … }`):

```css
button:active:not(:disabled),
.chip:active:not(:disabled) {
  transform: translateY(0.5px);
}
@media (prefers-reduced-motion: reduce) {
  button:active:not(:disabled),
  .chip:active:not(:disabled) {
    transform: none;
  }
}
```

- [ ] **Step 4: Empty-state audit (verification only — all four already ship)**

The spec's four empty states already exist; verify each is present and reads well, changing nothing unless one is broken:

| State | Where | Existing implementation |
| --- | --- | --- |
| Empty archive | `#first-run` card (`index.html:104-120`) | heading + sentence + "Import files…" / "Connect email…" actions |
| No search results | `#status` text (`src/render.ts:125`, `src/main.ts` "0 results for …") | status line via `setStatus` |
| Empty Sources list | `sourcesEmptyLine()` (`src/accountsview.ts:119-121`) | meta line in `.account-row.empty` |
| Empty Activity | `jobsStatusText()` (`src/jobsview.ts:126-128`) | "No background jobs yet — imports, IMAP syncs and embedding run here." |

Run `grep -n "first-run" src/main.ts | head -5` and confirm the card is unhidden on the empty-archive path (`main.ts:940`, `:1025`). If any of the four is missing or mangled, fix minimally to "heading/line + one sentence (+ action where one exists)".

- [ ] **Step 5: Amend the spec's stats line**

In `docs/superpowers/specs/2026-11-13-app-identity-design.md`, replace:

```markdown
give `#stats` the initial value "No archive open" instead of `…`
```

with:

```markdown
give `#stats` the initial value "Loading…" instead of `…` (amended during implementation: the app always opens an archive, so "No archive open" would be false)
```

- [ ] **Step 6: Verify**

Run: `npm run check && npm test`
Expected: green (no test asserts the `…` initial or the inline styles; if one does, update it to the new expectation).

- [ ] **Step 7: Commit**

```bash
git add index.html src/styles/app.css docs/superpowers/specs/2026-11-13-app-identity-design.md
git commit -m "polish(ui): stats default, layout utilities, press feedback"
```

---

### Task 7: Full verification pass (e2e suite, screenshots, build)

**Files:**
- Modify: `tests/e2e/fixtures/screenshots/*.png` (regenerated by the suite — they are committed evidence)

**Interfaces:**
- Consumes: everything from Tasks 1–6.
- Produces: a green full suite, refreshed evidence screenshots, a final commit.

- [ ] **Step 1: Build and run the whole unit suite**

Run: `npm run build && npm test`
Expected: clean build; all unit tests green.

- [ ] **Step 2: Run the full e2e suite**

Run: `npm run test:e2e` (allow ≥10 minutes; it launches one Tauri app instance per spec)
Expected: all 42 specs pass. Icons are decorative (`aria-hidden`) and selectors are id/class-based, so no spec should be affected. If a spec fails, read its error — fix the regression (never the spec's assertion) and re-run only the failing spec file first, then the full suite again before continuing.

- [ ] **Step 3: Review and commit refreshed screenshots**

Run: `git status --short tests/e2e/fixtures/screenshots/`
Expected: the four `16-*.png` evidence shots (and any others that capture the UI) show as modified — the new icons/lockup are now in them. View one to confirm the lockup and nav icons are visible, then:

```bash
git add tests/e2e/fixtures/screenshots/
git commit -m "test(e2e): refresh evidence screenshots with the identity pass"
```

- [ ] **Step 4: Final visual check**

Run: `npm run dev`, open the app in a browser at the Vite URL, and check: light and dark mode (Settings → theme), all four views, a search with an attachment hit, the Activity view with a running job (spinner animates; with OS reduced-motion enabled it stays static). Confirm: logo lockup, nav icons, favicon in the browser tab, no `☰`/`✕`/`📎`/`⤓` glyphs anywhere. Stop the dev server afterwards.

- [ ] **Step 5: Report**

Report completion with: commits made, e2e pass count, any deviations from this plan encountered on the way.

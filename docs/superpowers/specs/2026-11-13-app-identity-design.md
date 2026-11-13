# App identity: icon set, logo, favicon, and UI polish

- **Date:** 2026-11-13
- **Status:** Approved (design), pending implementation plan
- **Scope:** In-app identity artifacts and a polish pass; no changes to palette, typography, or layout structure

## 1. Problem

The app has a deliberate visual language (warm-neutral surfaces, forest-green accent, Source Sans 3, full dark/system mode, WCAG 2.2 AA tokens in `src/styles/tokens.css`) and a good custom OS icon set (envelope + teal typing-bubble on navy in `src-tauri/icons/`) — but the OS-level identity never enters the UI:

- The UI has **zero icons**. Navigation (Archive / Sources / Settings / Activity) and all actions are text-only.
- The only icon-like glyphs are emoji/text characters: `☰ Menu` (`index.html:51`), `✕ Close (Esc)` (`src/render.ts:309`), `📎 n/m` (`src/render2.ts:103`).
- No favicon, no in-app logo mark; the nav title is plain text.

Goal: a complete, professional identity package — custom icon set, logo mark in the nav, favicon, emoji-glyph replacement — plus a consistency polish pass, all extending the existing design language rather than replacing it.

## 2. Approach

**Static SVG sprite + `<use>`** (chosen over a TS icon module or icon fonts):

- One hidden `<svg>` block with `<symbol>` definitions at the end of `index.html`.
- Usage: `<svg class="icon" aria-hidden="true"><use href="#i-archive"></use></svg>`.
- Icons use `stroke: currentColor` so they inherit text color and work unchanged in light, dark, and system modes.
- A small `icon(name)` helper returns the same markup as a string for the dynamically rendered parts (`render.ts`, `render2.ts`, `main.ts`, per-view modules).

Rationale: the app is a static HTML shell mutated by TypeScript, with only three runtime dependencies and a strict CSP (`font-src 'self'`, no remote fetches). A sprite adds no dependencies, is CSP-safe, keeps the static shell static, and gives one place to maintain the whole set. An icon module would fight the architecture; icon fonts are obsolete practice.

## 3. Brand mark and favicon

Single source of truth: a clean vector redraw of the existing mark (envelope + teal typing-bubble), from which everything is exported:

1. **Favicon** — `public/favicon.svg` (full-color mark on the navy rounded square) + PNG fallback, wired with `<link rel="icon">` in the `index.html` head. Relevant for browser/dev use, free to add.
2. **Nav lockup** — the mark in accent colors *without* the navy background, 24px, beside the "Mailchat Vault" `h1` in `#nav`; must read on both light and dark canvas tokens.
3. **OS icon set** — regenerate the `src-tauri/icons/` PNG/ICO/ICNS set from the same SVG source (same graphics, vectorized). Tray (`src-tauri/src/tray.rs:69-83`), window, and bundle icons stay visually identical to today.

## 4. Iconography system

- **Geometry:** 24×24 viewBox, 1.5px stroke, round caps and joins, 2px corner radii — matched to Source Sans 3's humanist geometry and the existing `--radius-s: 4px` scale (2px at icon scale).
- **Color:** `currentColor` only. No per-icon palette colors; status icons pair the glyph with the existing `--success` / `--warning` / `--danger` text colors via classes.
- **Sizing:** `.icon` = 16px (inline with text), `.icon-lg` = 20px (nav, toolbars), opt-in larger sizes as needed.
- **Accessibility:** every icon is `aria-hidden="true"` and `focusable="false"` — always decorative; a text label always remains. This also keeps all 42 e2e specs' text/id selectors intact.
- **Naming:** `#i-` prefix + kebab name (`#i-archive`, `#i-paperclip`, …).

**Inventory (~26):**

| Group | Icons |
| --- | --- |
| Nav destinations | archive, sources, settings, activity |
| Actions | close, menu, search, paperclip, import, sync, cancel, retry, external |
| Status | check (success), warning, error, spinner (animated, `prefers-reduced-motion`-safe) |
| Source types | mail, chat, server (IMAP), file (mbox export) |
| Utility | chevron-down, info, folder (backup), key (credentials/keychain) |

**No third-party brand logos** (WhatsApp, Telegram, Discord, Signal): trademark burden plus visual inconsistency with the stroke set. Source chips and pickers get a *type* icon (mail / chat / server / file) ahead of the service name.

## 5. Emoji-glyph replacement

| Today | Becomes |
| --- | --- |
| `☰ Menu` (`index.html:51`) | menu icon + "Menu" text; accessible name unchanged |
| `✕ Close (Esc)` (`src/render.ts:309`) | close icon + "Close" + `kbd` Esc hint |
| `📎 n/m` (`src/render2.ts:103`) | paperclip icon + "n/m", `aria-label="attachments: n of m"` |

Arrows inside keyboard hints (`→`) stay — they are typography, not icons.

## 6. Polish pass

- **Empty states:** empty archive, no-search-results, empty Sources list, empty Activity — each a heading, one explanatory sentence, and a primary action where one exists (e.g. "Add source"). No illustrations.
- **Micro-states:** consistent hover/active treatment across buttons, chips, and list rows; spinner with the running-job rows in Activity.
- **Consistency audit:** spacing and type-scale pass across the four views; remove inline `style` attributes (`index.html:360`, `:368`); give `#stats` the initial value "No archive open" instead of `…`.
- **Motion:** nothing new beyond the spinner; `prefers-reduced-motion` handling already exists (`tokens.css:162`) and the spinner must respect it (static or hidden when reduced).

## 7. Testing and verification

- Full e2e suite (42 WebdriverIO specs) must pass unchanged in its assertions — icons are decorative so selectors and accessible names stay stable.
- Vitest snapshots that contain replaced markup (`render`, `render2`, `main` modules) are updated deliberately, not blurred.
- Evidence screenshot fixtures (`tests/e2e/fixtures/screenshots/16-*.png`, etc.) regenerate.
- Final visual check of every view in light and dark mode through the built app.

## 8. Out of scope

- Palette, typography, and layout-structure changes.
- OS icon redesign (regeneration is same-graphics vectorization only).
- Transition animations, skeleton loading states, onboarding — potential later packages.

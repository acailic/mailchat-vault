# End-to-end tests (tauri-driver + WebDriver)

These specs drive the **real desktop app**: `run.mjs` builds nothing itself
(beyond a first-run convenience build), spawns `tauri-driver`, and talks to
the running binary through WebDriver (WebKitWebDriver on Linux). Every spec
gets a **fresh app instance with an empty database** — the harness points
`XDG_DATA_HOME` at a throwaway temp dir per spec, so tests never touch your
real archive.

## Run locally

Prerequisites (Linux):

- the usual Tauri build deps (`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`,
  `libayatana-appindicator3-dev`, `librsvg2-dev`)
- the WebDriver binary: Debian/Ubuntu package `webkit2gtk-driver`
  (renamed `webkitgtk-webdriver` on Ubuntu 26.04+);
  `WebKitWebDriver` must be findable — the harness checks
  `/usr/libexec/webkit2gtk-4.1/WebKitWebDriver`, `/usr/bin/WebKitWebDriver`,
  and `$PATH`
- `tauri-driver` (`cargo install tauri-driver --locked`), `sqlite3` (only for
  the direct-database assertions; specs degrade gracefully without it)
- an X display; on a headless machine wrap the run in `xvfb-run -a`

```sh
npm run build               # frontend dist embedded into the app binary
cargo build -p mailchat-vault --features tauri/custom-protocol
npm run test:e2e            # or: xvfb-run -a npm run test:e2e
```

The `tauri/custom-protocol` feature is required: it embeds `dist/` into the
binary and ignores the `devUrl`; without it a debug build tries to load
`http://localhost:5173` (any stray vite server) instead of its own assets,
and on a machine without one the window comes up blank.

Useful knobs:

| Variable | Meaning |
| --- | --- |
| `MAILCHAT_E2E_ONLY=<name>` | run a single spec (name substring) |
| `MAILCHAT_E2E_BINARY=<path>` | app binary to drive (default `target/debug/mailchat-vault`) |
| `MAILCHAT_TAURI_DRIVER=<path>` | tauri-driver binary |
| `MAILCHAT_WEBKIT_DRIVER=<path>` | WebKitWebDriver binary |
| `MAILCHAT_E2E_KEEP_DATA=1` | keep the per-spec data dirs (path is printed on failure) |
| `MAILCHAT_E2E_TIMEOUT_MS` | per-spec watchdog (default 600000; cold CI starts are slow) |

## What is covered

| Spec | Path | Status |
| --- | --- | --- |
| `01-empty-archive` | first boot: empty state, DB created empty, zero-result search, empty-query prompt | active |
| `02-import-mbox` | import `.mbox` fixture → confirmed **in the database** (rows, RFC 3339 dates, chat_id, FTS index, decoded base64 body), idempotent re-import, demo seed adds a second source | active |
| `03-search-results` | FTS search: multi-hit queries, author matches, decoded bodies, undated messages, clean zero results, hostile FTS-operator input sanitized | active |
| `04-keyboard-nav` | Enter submits search; (filtered UI) `/` focus, ↑/↓ selection, Enter opens detail, Esc closes/clears | active |
| `05-llm-ask-mock` | LLM Ask against a **mock `/v1/messages` server**: translate → local FTS + **date range including the near-midnight boundary** → RAG answer; provenance line; garbage-LLM fallback with visible note; privacy scope (only matched rows leave the machine); `x-api-key`/model wiring | active |
| `06-source-filter` | filter results by source chips (mbox vs demo) | conditional — activates when the filtered-search UI lands |
| `07-detail-view` | v2 reading pane (M3.2): detail metadata + actions (copy / copy link / export); the fixture's filename-grouped mbox chat is an **unverified** grouping → anchor only with the "could not be verified" notice, no guessed neighbours | conditional — same |
| `08-date-range` | date-range filter UI around the near-midnight boundary; undated messages excluded while bounded | conditional — same |
| `09-signal-import` | import a Signal conversation `.json` (invokes `import_chat_export` directly — the native file chooser cannot be automated) → stats label + signal chip, searchable hit with highlights in the flat results list, detail view with verified-conversation thread context (click-through neighbours), re-import dedup, renamed re-export dedups by conversation uuid, unrecognized file fails loudly and stores nothing | active (needs the filtered-search UI's source chips) |
| `09-discord-import` | Discord Data Package import through the real `import_chat_export` command (invoked via the page's Tauri IPC bridge — the button's native file dialog can't be automated): summary + stats label, FTS search hits in the UI, keyboard-opened detail with chat grouping, idempotent re-import, single-channel-file dedup, DB asserts (guild/DM grouping via `index.json`, snowflake ids, reactions/attachments in `raw_json`, UTC timestamps, FTS sync) | active (needs the filtered-search UI's filters row) |
| `09-imap-oauth` | Gmail Google sign-in UI: auth-mode picker (password ↔ OAuth, default password, field/button visibility toggles), unconfigured / not-signed-in hints from the real `oauth_status` (both environment states), real `oauth_gmail_login` failure path when unconfigured (status carries the backend error), switching back restores password mode | active |
| `10-imap-oauth-sync` | Sync semantics in OAuth mode: empty host + stale hidden password pass the frontend gate and reach the backend oauth branch (empty host defaults to `imap.gmail.com` server-side; backend answers "no Google sign-in stored"), while password mode still gates the empty host frontend-side | active |
| `10-folder-watch` | folder watching: arm a folder (initial scan + skip of unrecognized files), background import of new/modified files, dedup on rewrite, direct DB confirmation, removal stops imports | active |
| `11-stats-dashboard` | statistics dashboard: empty-archive summary, aggregates after demo seed + mbox import, auto-refresh on import, Refresh button, idempotent row (counters must not double-count), counter-vs-table consistency via direct DB check | active |
| `09-settings-providers` | Settings UI: configure a provider through the form (never env vars) → **settings.json on disk holds the profile but never the key**; provider menu in Ask; per-ask **local mode sends nothing**; mis-paired provider (z.ai kind + Anthropic host) refused before saving; clearing the key disables the menu entry | active |
| `10-settings-guards` | Adversarial save/load guards: plaintext `http://` non-loopback base URL **refused before saving** (key never travels unencrypted); parseable-but-invalid `settings.json` refused at **load** with a visible error in a restarted session, nothing enabled, app stays functional | active |
| `12-background-jobs` | background-jobs panel through the real UI: empty-state message, clicking **Update local embeddings** runs the v2 job live (submit → started → progress → succeeded events reach the panel header, panel lists the finished job with its result summary and `succeeded` styling, embed button re-enables), embedding rows confirmed in the DB, and a webview reload rebuilds the panel via the `job_events_since(0)` boot-recovery path | active |
| `13-job-engine-parity` | v2 job engine + IMAP parity driven through the live IPC bridge (`window.__TAURI_INTERNALS__`): two-way keyring password (entered→saved and reused via empty resubmit; empty-with-nothing-stored → documented error, proving v2 parse accepts empty), padded username → ONE durable source instance (DB assert), v2 imap_sync job → typed job record/result, idempotent re-embed (0 new, remaining 0), flat JobEvent wire (`event.type` top-level), `job_events_since(cursor)` returns exactly the gap, 65th in-flight submit → typed queue-full, cancelled-while-queued and running-embed-cancel both end `cancelled` | active |
| `14-imap-coordinator` | IMAP coordinator round as a real v2 job through the live IPC bridge: typed `RoundSummary` result reaches the job record, per-source exclusion partitioning (`sources_list` labels, `source_set_excluded` + re-include), `source_failures` accounting, no-credentials rounds report cleanly, jobs-panel label for the new job kind (the sync paths themselves are covered by vault-core's mock-server suites — no TLS-trustable IMAP server in the container) | active |
| `15-sync-scheduler` | M2.4 periodic-sync scheduler surface: fresh install is opt-in-off at 15 minutes, the IMAP-section auto-sync toggle arms through the real IPC with the countdown rendered ("next round in ~N min"), interval validation (0 refused, 1..=1440 accepted) with a clear error, opt-in persists in the vault `sync_schedule` row (direct DB reads, guarded per suite contract), restart re-arms one full interval out with no immediate round, disabling persists and re-renders the off state via the `scheduler-status` event | active |
| `16-workspace-layout` | M3.1 workspace layout: semantic landmarks/labels/live regions and named buttons; wide (≥1100px) navigation+results+reading side by side with the reading placeholder; medium (800–1099px) reading swap with Back-to-results preserving query/rows; compact (<800px) navigation drawer with aria-expanded and Escape close; 640px (1280 at 200% zoom) stays compact with no horizontal clipping; activity histogram role=img/aria-label; wide/medium/compact/zoom **screenshot fixtures** written to `fixtures/screenshots/` (synthetic demo data) | active |
| `16-search-v2` | M3.2 v2 search surface: flat list (no conversation regrouping), newest/relevance sort choice, attachment filter chip (0 matches on the media-less fixture, chip removal returns to the prompt), import-scope chip listing bounded import runs (exactly the 8 fixture messages), deep link `mailchat://message/<id>` opens the reading pane as navigation without a text query | active |
| `17-search-v2-fixes` | M3.2 v2 fix-round regressions through the live v2 IPC and running app: a 16-message conversation's second "Load earlier" page splices at the front oldest→newest; two source instances sharing one label expand to one applied chip per matching id and removing one chip clears every id the label expanded to; a selection built across result pages counts checks from both pages, writes every checked id (id assembly pinned in `src/main.test.ts` — the GTK save dialog can't be automated), and reading + Back restores page, count, selection, and scroll | active |
| `18-new-mail-notify` | desktop notifications at the live product surface: a folder-watch import storing new messages fires one native OS notification — the real Linux DBus `Notify` call, captured by `dbus-monitor` into `MAILCHAT_E2E_NOTIFY_LOG` — with the count and file while the window is unfocused; a focused-window cycle stays silent; a cycle storing nothing new never notifies; the Settings checkbox toggles the layer at runtime; repeated watch snapshots of the same Imported event never duplicate | active (needs the DBus/dunst wrapper environment; skips without `MAILCHAT_E2E_NOTIFY_LOG`) |
| `19-notify-sync-fail` | failed and empty sync cycles never notify: the v1 Sync button against a dead local port shows "Sync failed" with no OS notification; a succeeded `imap_sync_all` coordinator round with no stored credentials reports `no_credentials`, stores nothing, and never notifies (DBus capture as in 18) | active (same wrapper environment; skips without `MAILCHAT_E2E_NOTIFY_LOG`) |
| `20-notify-settings-persist` | the persisted Settings option: an older settings.json written before the `notifications` field existed loads enabled (checkbox checked on boot); toggling off + saving persists `"notifications": false` to the settings file; a fresh boot over that file starts with the checkbox unchecked (the opt-out survives restart); re-enabling persists `true` again (settings file asserted directly on disk) | active |

### Conditional specs

Three paths (source filter, detail view, date-range UI) belong to the
filtered-search UI (`fm/mailchat-search-ui`). The suite probes for that DOM
at runtime (`#from-date` present?) and **skips those specs with a clear
reason** when it is not in the build, so the suite stays green on any branch
order. The near-midnight date-boundary behaviour itself is already covered
unconditionally by `05-llm-ask-mock` (LLM-planned date range over the same
fixture) and by direct DB assertions in `02-import-mbox`.

All paths run headless under `xvfb-run` — none needs a real desktop beyond
an X display. The CI job (`.github/workflows/ci.yml`, `e2e` job) runs the
full suite on every push/PR.

## Workspace views

Since the M3.1 workspace layout, controls live in navigation destinations:
search/results/reading in **workspace**, imports/IMAP/folder-watching in
**sources**, jobs/statistics in **activity**, and the provider/theme form in
**settings**. `ui.openView(browser, view)` clicks the real nav buttons and
waits for the view to mount; `ui.importMbox`/`ui.seedDemo` open `sources`
first and `ui.search` returns to `workspace`, so specs composed of those
helpers need no extra navigation. `ui.setViewport(browser, w, h)` resizes
the window and waits for the `data-layout` state (wide/medium/compact) to
settle.

## Layout

```
tests/e2e/
  run.mjs            runner: per-spec fresh session, watchdog, reporting
  harness/app.mjs    tauri-driver spawn + WebDriver session + temp data dir
  harness/mock-llm.mjs  scripted Anthropic-compatible mock endpoint
  harness/sqlite.mjs    read the session's vault.db via the sqlite3 CLI
  harness/ui.mjs     shared UI helpers (work on both UI generations)
  harness/assert.mjs tiny assertion helpers
  fixtures/archive.mbox  8-message fixture: near-midnight pair, base64 body,
                     undated mail, distinctive authors/keywords
  fixtures/signal-conversation.json  Signal conversation export: 5 archiveable
                     messages + 3 service entries (call history, key change,
                     profile change)
  specs/*.e2e.mjs     one file per critical path (the `.e2e.mjs` suffix keeps
                     vitest from collecting them as unit tests)
```

Fixtures live in `tests/e2e/fixtures` and are committed; no spec writes
inside the repo.

## Notes for maintainers

- The suite has been validated against **both UI generations**: the current
  classic UI (5 active specs) and the filtered-search UI from
  `fm/mailchat-search-ui` (10 active specs — the conditional paths and the
  arrow-key/`/`/Esc keyboard extras all pass there too; `10-media-extract`
  was validated on this UI generation). `09-signal-import`
  (source chips) and `09-discord-import` (the filters row) need the
  filtered-search UI, so they do not run on the classic UI.
  The `11-stats-dashboard` spec belongs to the statistics dashboard UI
  (added alongside it, after that validation) and postdates
  the two-generation validation.
- WebKitWebDriver quirks the specs deliberately work around: clicking
  `body` targets the page centre (a result row — opens the detail view), so
  focus is parked on the `h1` instead; `findElement` with a volatile
  compound selector (`#detail.open`) can serve stale matches, so panel
  state is read through `execute()`; and keys sent to a detached node never
  reach document-level handlers, so focus is blurred before Esc after a
  re-render replaced the clicked element.

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
| `07-detail-view` | message detail panel + adjacent thread messages, click-through neighbours, Esc | conditional — same |
| `08-date-range` | date-range filter UI around the near-midnight boundary; undated messages excluded while bounded | conditional — same |
| `09-signal-import` | import a Signal conversation `.json` (invokes `import_chat_export` directly — the native file chooser cannot be automated) → stats label + signal chip, searchable hit with highlights, detail view with thread context, re-import dedup, renamed re-export dedups by conversation uuid, unrecognized file fails loudly and stores nothing | active (needs the filtered-search UI's source chips) |
| `09-discord-import` | Discord Data Package import through the real `import_chat_export` command (invoked via the page's Tauri IPC bridge — the button's native file dialog can't be automated): summary + stats label, FTS search hits in the UI, keyboard-opened detail with chat grouping, idempotent re-import, single-channel-file dedup, DB asserts (guild/DM grouping via `index.json`, snowflake ids, reactions/attachments in `raw_json`, UTC timestamps, FTS sync) | active (needs the filtered-search UI's filters row) |
| `09-imap-oauth` | Gmail Google sign-in UI: auth-mode picker (password ↔ OAuth, default password, field/button visibility toggles), unconfigured / not-signed-in hints from the real `oauth_status` (both environment states), real `oauth_gmail_login` failure path when unconfigured (status carries the backend error), switching back restores password mode | active |
| `10-imap-oauth-sync` | Sync semantics in OAuth mode: empty host + stale hidden password pass the frontend gate and reach the backend oauth branch (empty host defaults to `imap.gmail.com` server-side; backend answers "no Google sign-in stored"), while password mode still gates the empty host frontend-side | active |
| `10-folder-watch` | folder watching: arm a folder (initial scan + skip of unrecognized files), background import of new/modified files, dedup on rewrite, direct DB confirmation, removal stops imports | active |
| `11-stats-dashboard` | statistics dashboard: empty-archive summary, aggregates after demo seed + mbox import, auto-refresh on import, Refresh button, idempotent row (counters must not double-count), counter-vs-table consistency via direct DB check | active |

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

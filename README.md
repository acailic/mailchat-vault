# mailchat-vault

Local email & chat archive — Tauri desktop app with SQLite FTS5 full-text search over your own messages. Private tool; nothing leaves the machine.

This is the **v0 foundation + usable search tool**: a complete vertical slice — import, store, dedup, search, filter, browse, read in thread context — on which the rest of the roadmap (IMAP/OAuth, chat formats, LLM-assisted search) will be built as separate tasks.

## What v0 does

- **Tauri 2 desktop app** (Rust backend, vanilla TypeScript + Vite frontend).
- **SQLite storage with FTS5 full-text index** (bundled SQLite via `rusqlite`; no system SQLite needed).
  - `messages(id, source, external_id, channel, chat_id, author, sent_at, body, raw_json)` with `UNIQUE(source, external_id)`.
  - `messages_fts` FTS5 virtual table (external content) kept in sync by triggers on insert/update/delete.
  - Versioned migrations applied automatically on open.
- **Idempotent ingest**: a `Source` trait yields messages; writes dedup on `(source, external_id)`, so re-importing is always safe.
- **Manual/demo source**: import an `.mbox` file (no IMAP needed) — subjects, authors, dates, and decoded text bodies are archived and searchable.
- **Search tool UI** over the FTS5 index:
  - results with **match highlighting** (FTS5 snippets, rendered as `<mark>` runs), **grouped by chat/thread**;
  - **filters**: source chips (email/whatsapp/telegram/… — whatever the archive holds), author substring, and a from–to date range; an empty query with filters set browses newest-first;
  - **result count + pagination** (`1–50 of 1,234 · page 1/25`);
  - **message detail**: full body with highlighted query terms, full metadata, and clickable **thread neighbors** of the same chat;
  - **keyboard navigation**: `/` focus search, `↑`/`↓` move through results, `Enter` open detail, `↑`/`↓` walk the thread, `Esc` close.
- **Performance contract**: queries answer in < 100 ms over 100k messages (asserted by a generated-fixture test; all queries are `LIMIT`-bounded and ride the FTS5 index plus the `sent_at`/`(source, chat_id)` indexes).

## Repository layout

```
crates/vault-core/   # storage (SQLite/FTS5, migrations), ingest trait, mbox source — no UI deps
src-tauri/           # Tauri app shell: commands (search, import_mbox, seed_demo, stats)
src/                 # frontend (vanilla TS): search box, results, import controls
index.html           # single-page shell
```

## Prerequisites

- [Node.js](https://nodejs.org) 22+
- [Rust](https://rustup.rs) stable
- Tauri Linux system libraries (Ubuntu/Debian):

  ```sh
  sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
  ```

  (macOS/Windows need Xcode / MSVC as usual for Tauri.)

## Run in development

```sh
npm install
npm run tauri dev
```

First run tip: click **Load demo** to seed the sample messages, then search for e.g. `vault`, click a hit to read it with its thread neighbors, or set the source/author/date filters to browse. To import real data, point the import field at any `.mbox` file (e.g. exported from Thunderbird).

The database lives in your app data dir (e.g. `~/.local/share/com.acailic.mailchatvault/vault.db` on Linux).

## Tests

```sh
cargo test               # vault-core: storage, migrations, dedup, FTS search, mbox ingest,
                         #   filtered/paged search, chat context, 100k perf fixture
npm test                 # frontend unit + UI-state snapshot tests (vitest)
npm run check            # TypeScript type-check
cargo test --workspace   # also compiles the Tauri app (needs the Linux libs above)
```

CI (`.github/workflows/ci.yml`) runs all of the above plus `clippy` and `fmt --check` on every push and PR.

Testing strategy for the UI: driving the Tauri webview headlessly in CI is
not practical (webkit2gtk driver + display), so the UI is covered by tests
over the query layer (`crates/vault-core`) and snapshot tests of the rendered
UI state (`src/render.test.ts` via happy-dom: results list, pagination,
source chips, detail panel).

## Out of scope for this task (separate tasks)

IMAP/OAuth fetch, chat platform ingest formats, LLM-assisted search, icons/bundling/installers, settings UI, result export, statistics dashboard.

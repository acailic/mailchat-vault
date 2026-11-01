# mailchat-vault

Local email & chat archive — Tauri desktop app with SQLite FTS5 full-text search over your own messages. Private tool; nothing leaves the machine.

This is the **v0 foundation**: a complete vertical slice — import, store, dedup, search, display — on which the rest of the roadmap (IMAP/OAuth, chat formats, LLM-assisted search) will be built as separate tasks.

## What v0 does

- **Tauri 2 desktop app** (Rust backend, vanilla TypeScript + Vite frontend).
- **SQLite storage with FTS5 full-text index** (bundled SQLite via `rusqlite`; no system SQLite needed).
  - `messages(id, source, external_id, channel, chat_id, author, sent_at, body, raw_json)` with `UNIQUE(source, external_id)`.
  - `messages_fts` FTS5 virtual table (external content) kept in sync by triggers on insert/update/delete.
  - Versioned migrations applied automatically on open.
- **Idempotent ingest**: a `Source` trait yields messages; writes dedup on `(source, external_id)`, so re-importing is always safe.
- **Manual/demo source**: import an `.mbox` file (no IMAP needed) — subjects, authors, dates, and decoded text bodies are archived and searchable.
- **Chat imports**: WhatsApp `.txt` exports and Telegram Desktop `result.json` exports — texts, authors, and timestamps are archived and searchable; quoted replies are kept as metadata, attachments are not extracted but recorded in `raw_json`. The format is auto-detected from the file contents, so re-importing the same export never duplicates messages.
- **Search UI**: a window with a search box that hits FTS5 (body + author) and shows relevance-ranked results; free-text input is sanitized so it can never be an FTS query syntax error.

## Repository layout

```
crates/vault-core/   # storage (SQLite/FTS5, migrations), ingest trait, mbox / WhatsApp / Telegram sources — no UI deps
src-tauri/           # Tauri app shell: commands (search, import_mbox, import_chat_export, seed_demo, stats)
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

First run tip: click **Load demo** to seed a few sample messages, then search for e.g. `vault`. To import real data, point the import field at any `.mbox` file (e.g. exported from Thunderbird), or use **Import chat export…** to pick a WhatsApp `Export chat` `.txt` file or a Telegram export's `result.json` (picking the export directory works too).

The database lives in your app data dir (e.g. `~/.local/share/com.acailic.mailchatvault/vault.db` on Linux).

## Tests

```sh
cargo test               # vault-core: storage, migrations, dedup, FTS search, mbox / WhatsApp / Telegram ingest
npm test                 # frontend unit tests (vitest)
npm run check            # TypeScript type-check
cargo test --workspace   # also compiles the Tauri app (needs the Linux libs above)
```

CI (`.github/workflows/ci.yml`) runs all of the above plus `clippy` and `fmt --check` on every push and PR.

## Out of scope for v0 (separate tasks)

IMAP/OAuth fetch, more chat sources (Signal, Discord), automatic folder watching, chat media extraction, LLM-assisted search, icons/bundling/installers, settings UI.

## Notes on chat import details

- **WhatsApp**: export dates carry no UTC offset, so they are stored as-is in the archive's UTC-normalized form; day-first vs month-first ordering is inferred from the file's own dates (day-first when ambiguous). Media placeholders (`image omitted`, `(file attached)`, …) are kept as text and recorded in `raw_json`.
- **Telegram**: `date_unixtime` is preferred for timestamps (timezone-correct); service entries (calls, joins) are skipped; the original message JSON (attachments, reply references included) is kept verbatim in `raw_json`.

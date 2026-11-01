# mailchat-vault

Local email & chat archive — Tauri desktop app with SQLite FTS5 full-text search over your own messages. Private tool; nothing leaves the machine, except what you explicitly send for optional [LLM-assisted search](#llm-assisted-search-v1) (your question + the matching result rows only, and only when an API key is configured).

This is the **v0 foundation**: a complete vertical slice — import, store, dedup, search, display. **v1** adds LLM-assisted natural-language search on top (see below). The rest of the roadmap (IMAP/OAuth, chat platform formats) is built as separate tasks.

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
- **Ask UI (v1)**: natural-language questions translated to FTS5 + date filters via an LLM, with a short summary over the top results — strictly privacy-scoped and fully optional (see below).

## Repository layout

```
crates/vault-core/   # storage (SQLite/FTS5, migrations), ingest trait, mbox / WhatsApp / Telegram sources, LLM search pipeline — no UI deps
src-tauri/           # Tauri app shell: commands (search, llm_ask, llm_status, import_mbox, import_chat_export, seed_demo, stats)
src/                 # frontend (vanilla TS): search + ask UI, import controls
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

## LLM-assisted search (v1)

Ask a natural-language question (“what was said about the vault in October?”); the app translates it into an FTS5 query, runs it locally, then produces a short answer over the top results (RAG style). Entirely optional and fail-safe: without an API key the app behaves exactly like v0's plain full-text search.

### How it works

1. **Translate** — the question is sent to the LLM, which replies with a JSON plan: an FTS5 MATCH expression over `body`/`author` plus an optional sent-date range (it understands relative wording like “last march” against today's date).
2. **Search** — the plan runs locally against SQLite. The LLM's expression is validated against the index first; anything unparseable or referring to unknown columns is replaced by a plain term search (usable date filters are kept). The expression is always passed as a bound MATCH parameter, never spliced into SQL.
3. **Summarize** — a short answer is synthesized from **only the top 8 result rows** (bodies truncated to 500 chars), cited as [1], [2], …

The UI always shows the plan actually used (`FTS: … · dates: … · planned by LLM / fallback`) plus a privacy note describing exactly what the current mode sends.

### Configuration (environment variables only — the key is never stored in the database or the repo)

| Variable | Meaning | Default |
| --- | --- | --- |
| `ANTHROPIC_API_KEY` / `ZAI_API_KEY` / `GLM_API_KEY` | API key, first non-empty wins | unset → LLM off |
| `ANTHROPIC_BASE_URL` | Anthropic-compatible endpoint base URL | `https://api.z.ai/api/anthropic` |
| `MAILCHAT_LLM_MODEL` | model name | `glm-5` |

### Privacy

The product promise is “nothing leaves the machine”; LLM mode is opt-in and strictly scoped: only your question (for translation) and the top-8 matching messages, truncated (for the summary), are sent to the configured provider. No other rows, no database dumps, no embeddings, no key at rest. The summary step is skipped entirely when there are no results.

### Fallback

- No API key → plain FTS5 term search; the UI says so up front.
- LLM error, timeout, or invalid/unusable plan output → plain term search plus a visible note; search results never depend on the LLM succeeding.

### Costs & latency

Measured against `glm-5` at `https://api.z.ai/api/anthropic` (Nov 2026):

- Translate: ~1.5–3 s, roughly 350 input / 100 output tokens.
- Summarize (8 results × ≤500 chars): ~2–5 s, roughly 2,000 input / 200 output tokens.
- One “Ask” ≈ two small requests, a few seconds end-to-end, well under a cent per ask at GLM pricing.

## Tests

```sh
cargo test               # vault-core: storage, migrations, dedup, FTS search, mbox / WhatsApp / Telegram ingest, LLM search pipeline (mock LLM, no network)
npm test                 # frontend unit tests (vitest)
npm run check            # TypeScript type-check
cargo test --workspace   # also compiles the Tauri app (needs the Linux libs above)
```

CI (`.github/workflows/ci.yml`) runs all of the above plus `clippy` and `fmt --check` on every push and PR.

## Out of scope for now (separate tasks)

IMAP/OAuth fetch, more chat sources (Signal, Discord), automatic folder watching, chat media extraction, local embedding models / vector search, multi-provider menus, icons/bundling/installers, settings UI.

## Notes on chat import details

- **WhatsApp**: export dates carry no UTC offset, so they are stored as-is in the archive's UTC-normalized form; day-first vs month-first ordering is inferred from the file's own dates (day-first when ambiguous). Media placeholders (`image omitted`, `(file attached)`, …) are kept as text and recorded in `raw_json`.
- **Telegram**: `date_unixtime` is preferred for timestamps (timezone-correct); service entries (calls, joins) are skipped; the original message JSON (attachments, reply references included) is kept verbatim in `raw_json`.

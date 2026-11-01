# mailchat-vault

Local email & chat archive — Tauri desktop app with SQLite FTS5 full-text search over your own messages. Private tool; nothing leaves the machine, except what you explicitly send for optional [LLM-assisted search](#llm-assisted-search-v1) (your question + the matching result rows only, and only when an API key is configured).

This is the **v0 foundation**: a complete vertical slice — import, store, dedup, search, display. **v1** adds LLM-assisted natural-language search on top (see below), and the first live mail source: **incremental IMAP/Gmail sync** (see below). The rest of the roadmap (chat platform formats, XOAUTH2) will land as separate tasks.

## What v0 does

- **Tauri 2 desktop app** (Rust backend, vanilla TypeScript + Vite frontend).
- **SQLite storage with FTS5 full-text index** (bundled SQLite via `rusqlite`; no system SQLite needed).
  - `messages(id, source, external_id, channel, chat_id, author, sent_at, body, raw_json)` with `UNIQUE(source, external_id)`.
  - `messages_fts` FTS5 virtual table (external content) kept in sync by triggers on insert/update/delete.
  - Versioned migrations applied automatically on open.
- **Idempotent ingest**: a `Source` trait yields messages; writes dedup on `(source, external_id)`, so re-importing is always safe.
- **Manual/demo source**: import an `.mbox` file (no IMAP needed) — subjects, authors, dates, and decoded text bodies are archived and searchable.
- **Chat imports**: WhatsApp `.txt` exports and Telegram Desktop `result.json` exports — texts, authors, and timestamps are archived and searchable; quoted replies are kept as metadata, attachments are not extracted but recorded in `raw_json`. The format is auto-detected from the file contents, so re-importing the same export never duplicates messages.
- **IMAP sync (Gmail & friends)**: incremental, UIDVALIDITY/UID-tracked sync from any IMAP server over host/port/user/password — Gmail via app password in this first round (XOAUTH2 later). Folders map to the message `channel` field; re-syncing never duplicates. Passwords are prompted for, kept in the OS keyring when available and in session memory otherwise — never in the database.
- **Search UI**: a window with a search box that hits FTS5 (body + author) and shows relevance-ranked results; free-text input is sanitized so it can never be an FTS query syntax error.
- **Ask UI (v1)**: natural-language questions translated to FTS5 + date filters via an LLM, with a short summary over the top results — strictly privacy-scoped and fully optional (see below).

## Repository layout

```
crates/vault-core/   # storage (SQLite/FTS5, migrations), ingest trait, mail parsing, mbox / WhatsApp / Telegram sources, IMAP sync, LLM search pipeline — no UI deps
src-tauri/           # Tauri app shell: commands (search, llm_ask, llm_status, import_mbox, import_chat_export, sync_imap, seed_demo, stats) + secret storage
src/                 # frontend (vanilla TS): search + ask UI, import & IMAP sync controls
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

First run tip: click **Load demo** to seed a few sample messages, then search for e.g. `vault`. To import real data, point the import field at any `.mbox` file (e.g. exported from Thunderbird), use **Import chat export…** to pick a WhatsApp `Export chat` `.txt` file or a Telegram export's `result.json` (picking the export directory works too), or fill in the **IMAP sync** section (see below).

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
| `ZAI_API_KEY` / `GLM_API_KEY` | API key for the default z.ai endpoint (`https://api.z.ai/api/anthropic`) | unset → LLM off |
| `ANTHROPIC_API_KEY` | Anthropic key, honored **only** together with an explicit `ANTHROPIC_BASE_URL` | unset → z.ai keys apply |
| `ANTHROPIC_BASE_URL` | endpoint base URL; pairs with `ANTHROPIC_API_KEY`, or must point at the z.ai host (`api.z.ai`, any URL spelling) when a z.ai key is used | unset → `https://api.z.ai/api/anthropic` |
| `MAILCHAT_LLM_MODEL` | model name | `glm-5` |

A mismatched key/endpoint pairing (for example `ANTHROPIC_API_KEY` without `ANTHROPIC_BASE_URL`) is refused with a clear error instead of sending the key anywhere.

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

## IMAP sync

The **IMAP sync** section pulls mail straight from a server into the archive — incrementally: each folder's `UIDVALIDITY` + highest-seen UID are tracked in the vault database, so every sync fetches only new messages. If a folder is recreated (`UIDVALIDITY` changes), it is rescanned and re-fetched messages are deduped by `(source, external_id)` — a reset never creates duplicate rows. The folder name is stored as the message `channel`, and mails archive under the source `imap:<username>`.

### Gmail (app password)

1. Enable 2-step verification on the Google account, then create an **App password** (Google Account → Security → 2-Step Verification → App passwords).
2. In the app: host `imap.gmail.com`, port `993`, username `you@gmail.com`, the app password, folders e.g. `INBOX` (comma-separated; empty means `INBOX`).
3. Click **Sync**. The first sync fetches the whole folder; later syncs only new mail.

### Other servers

Any IMAP server with implicit TLS works the same way: host, port (993), username, password, folders.

### Where the password lives

- Typed passwords are used for the sync and then stored in the **OS keyring** when one is available (Secret Service/GNOME Keyring or KWallet on Linux, Keychain on macOS, Credential Manager on Windows) — service `mailchat-vault`, entry `user@host`.
- With no OS keyring reachable, the password is kept **only in process memory for the current app session** (gone on exit) — the UI says so.
- Passwords are never written to the vault database (a test scans the DB bytes to enforce this) and never logged.
- Leaving the password field empty reuses the stored password; to remove it entirely, delete the entry in your OS keyring manager.

## Tests

```sh
cargo test               # vault-core: storage, migrations, dedup, FTS search, mbox / WhatsApp / Telegram ingest, IMAP sync (mock server), LLM search pipeline (mock LLM, no network), integrity suite (corruption & crash recovery, export/import round-trips, VACUUM/ANALYZE, schema migrations)
npm test                 # frontend unit tests (vitest)
npm run check            # TypeScript type-check
cargo test --workspace   # also compiles the Tauri app (needs the Linux libs above)
```

CI (`.github/workflows/ci.yml`) runs all of the above plus `clippy` and `fmt --check` on every push and PR.

IMAP sync is tested hermetically against an in-process mock IMAP4rev1 server (the greenmail equivalent), including an integration test that ingests the same `.mbox` fixture through both paths and compares the archives. A real-server test (`tests/imap_gmail_manual.rs`) is `#[ignore]`d: it needs live credentials and is **not** run in CI. To run it against your own account:

```sh
MAILCHAT_IMAP_USER=you@gmail.com MAILCHAT_IMAP_PASS=abcd-efgh-ijkl-mnop \
  cargo test --test imap_gmail_manual -- --ignored --nocapture
```

It writes to a throwaway temp directory, never your real vault.

## Out of scope for now (separate tasks)

XOAUTH2 (OAuth) login for Gmail, multi-account UI, more chat sources (Signal, Discord), automatic folder watching, chat media extraction, local embedding models / vector search, multi-provider menus, icons/bundling/installers, settings UI, push notifications.

## Notes on chat import details

- **WhatsApp**: export dates carry no UTC offset, so they are stored as-is in the archive's UTC-normalized form; day-first vs month-first ordering is inferred from the file's own dates (day-first when ambiguous). Media placeholders (`image omitted`, `(file attached)`, …) are kept as text and recorded in `raw_json`.
- **Telegram**: `date_unixtime` is preferred for timestamps (timezone-correct); service entries (calls, joins) are skipped; the original message JSON (attachments, reply references included) is kept verbatim in `raw_json`.

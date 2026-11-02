# mailchat-vault

Local email & chat archive — Tauri desktop app with SQLite FTS5 full-text search over your own messages. Private tool; nothing leaves the machine, except what you explicitly send for optional [LLM-assisted search](#llm-assisted-search-v1) (your question + the matching result rows only, and only when an API key is configured).

This is the **v0 foundation + usable search tool**: a complete vertical slice — import, store, dedup, search, filter, browse, read in thread context. **v1** adds LLM-assisted natural-language search with local-embedding **hybrid ranking** on top (see below), the first live mail source: **incremental IMAP/Gmail sync** (see below), and **automatic folder watching** (see below). The rest of the roadmap (chat platform formats, XOAUTH2) will land as separate tasks.

## What v0 does

- **Tauri 2 desktop app** (Rust backend, vanilla TypeScript + Vite frontend).
- **SQLite storage with FTS5 full-text index** (bundled SQLite via `rusqlite`; no system SQLite needed).
  - **Canonical store** (schema v8): `source_instances` (durable connector entities: kind, label, non-secret configuration, connected state, last attempt/success), `conversations` (stable provider keys), `archive_messages` (ids, payload, and time provenance: resolved UTC instant, original timestamp text, timezone, confidence), `message_identities` (dedup identities — legacy `(source, external_id)` aliases plus connector-scoped keys with semantic revision hashes), `message_occurrences` (import/folder observations), `sync_checkpoints` (IMAP cursors), plus the `archive_fts` virtual table (external content, trigger-maintained). Every read and write targets these tables.
  - **Stable conversation keys**: Telegram/Signal chats key on the provider-guaranteed chat id and WhatsApp chats on a content-derived key (sorted participants + first authored message), so the same chat re-exported under a rename stays one conversation (the label follows the newest name) while different chats sharing a display name stay distinct. **Email reply graph**: `In-Reply-To`/`References` chains group mail into threads — one-hop resolution, so incomplete chains and reference cycles are safe — with replies imported before their roots adopting them into the right thread later. Timestamps carry **explicit timezone interpretation** (exporter-local WhatsApp stamps resolve DST-aware in the machine's IANA zone; Telegram's naive `date` fallback stores the exporter-UTC stamp verbatim; mail offsets and chat epochs are exact) with per-message confidence labels. Legacy databases upgrade in place (migration 7): renamed imports merge, same-name chats split, WhatsApp identities re-key so post-upgrade re-imports dedup, and their legacy blind-Z stamps resolve to stored true UTC instants (machine-zone interpretation), while whatever cannot be safely interpreted stays legacy-unresolved.
  - The legacy `messages` / `messages_fts` / `imap_sync_state` tables from earlier schema versions are preserved untouched as a pre-canonical snapshot; migration 4 copies every row (ids included) into the canonical tables, and nothing writes the legacy tables afterwards. Migration 8 adds the durable source-instance columns (connector identity key, state, attempt/success stamps).
  - Trigger-maintained counter tables (`stats_source`/`stats_day`/`stats_chat`/`stats_author`) power the statistics dashboard with O(groups) reads instead of GROUP BY scans.
  - Versioned migrations applied automatically on open.
- **Idempotent ingest**: a `Source` trait yields messages; writes dedup on the canonical identity — legacy `(source, external_id)` keys map to one `message_identities` row each, and the IMAP connector scopes identities to their durable source instance with a semantic revision hash (the same mail in several folders is one row with one occurrence per folder; two conflicting payloads behind one `Message-ID` are preserved as two revisions) — so re-importing and re-syncing are always safe.
- **Manual/demo source**: import an `.mbox` file (no IMAP needed) — subjects, authors, dates, and decoded text bodies are archived and searchable.
- **Chat imports**: WhatsApp `.txt` exports, Telegram Desktop `result.json` exports, Signal Desktop conversation `.json` exports (e.g. `sigtop export-messages -f json`), and Discord Data Package `messages.json` files — texts, authors, and timestamps are archived and searchable; quoted replies are kept as metadata, and Signal/Discord attachments are recorded in `raw_json` (Discord reactions too). **Media extraction** for WhatsApp and Telegram: files referenced by the export (images, documents, voice notes) are copied into a content-addressed `media/` store in app data — hash-named (SHA-256), deduplicated by content — and linked from the message's `raw_json`, with a media section in the detail view. Extraction can be switched off with the **extract media** toggle; text-only exports import fine (missing files are just counted), and re-importing an export with its files backfills media links into already-stored messages. The format is auto-detected from the file contents, so re-importing the same export never duplicates messages.
- **IMAP sync (Gmail & friends)**: incremental, UIDVALIDITY/UID-tracked sync from any IMAP server over host/port/user/password — Gmail via app password in this first round (XOAUTH2 later). Folders map to the message `channel` field; re-syncing never duplicates. Passwords are prompted for, kept in the OS keyring when available and in session memory otherwise — never in the database.
- **Folder watching**: pick folders (typed path or the native picker) and every new or modified `.mbox` / WhatsApp `.txt` / Telegram `result.json` / Signal conversation `.json` export file under them is imported automatically in the background — watched folders persist across restarts, write bursts are debounced until a file settles, re-imports never duplicate (the same dedup contract as manual import), and skips/errors are shown in the UI without ever stopping the watcher (see below).
- **Statistics dashboard**: archive aggregates in one view — total messages and date range, a per-day histogram (auto-downsampled to months/years when the archive spans too many days), and tables with inline bars for messages per source, top chats, and top authors. Served from trigger-maintained counter tables (schema migration 6), so the whole dashboard answers in **< 100 ms even on 1M messages** (asserted by a generated 1M-fixture test).
- **Search tool UI** over the FTS5 index:
  - results with **match highlighting** (FTS5 snippets, rendered as `<mark>` runs), **grouped by chat/thread**;
  - **filters**: source chips (email/whatsapp/telegram/… — whatever the archive holds), author substring, and a from–to date range; an empty query with filters set browses newest-first;
  - **result count + pagination** (`1–50 of 1,234 · page 1/25`);
  - **export**: the current search results — every row under the active filters, all pages, not just the visible one — or the whole chat thread from the detail view, written to a file you pick (native save dialog) as **CSV** (RFC 4180), **JSON Lines** (one full message object per line; export-only for now — no importer accepts this file yet) or **MBOX** (email rows; non-email rows are skipped and reported);
  - **message detail**: full body with highlighted query terms, full metadata, and clickable **thread neighbors** of the same chat;
  - **keyboard navigation**: `/` focus search, `↑`/`↓` move through results, `Enter` open detail, `↑`/`↓` walk the thread, `Esc` close.
- **Performance contract**: queries answer in < 100 ms over 100k messages (asserted by a generated-fixture test; all queries are `LIMIT`-bounded and ride the FTS5 index plus the `sent_instant`/`(source_instance, chat_id)` indexes); the statistics dashboard answers in < 100 ms over 1M messages.
- **Ask UI (v1)**: natural-language questions translated to FTS5 + date filters via an LLM, with a short summary over the top results — strictly privacy-scoped and fully optional (see below). Results are ranked **hybrid** — FTS5 fused with an offline local-vector leg once embeddings are built (see below).

## Repository layout

```
crates/vault-core/   # storage (SQLite/FTS5, migrations), filtered/paged search queries, archive statistics (dashboard aggregates), ingest trait, mail parsing, mbox / WhatsApp / Telegram / Signal / Discord sources, media extraction into the content-addressed store, IMAP sync, LLM search pipeline, local embeddings + hybrid ranking, folder watching, CSV/JSON/MBOX export — no UI deps
src-tauri/           # Tauri app shell: commands (search_messages, chat_context, llm_ask, llm_status, embed_messages, embed_status, import_mbox, import_chat_export with optional media extraction, sync_imap, watch_add_folder, watch_remove_folder, watch_status, seed_demo, stats, archive_stats, export_search, export_chat) + secret storage + watch-status events
src/                 # frontend (vanilla TS): search + filters + pagination + detail view (with media links), ask UI + embedding controls, statistics dashboard (CSS bar charts, no chart library), import & IMAP sync controls, folder watching panel, export controls
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

First run tip: click **Load demo** to seed the sample messages, then search for e.g. `vault`, click a hit to read it with its thread neighbors, or set the source/author/date filters to browse. Click **Update local embeddings** in the Ask section to build the local vector index (offline) — Ask then ranks results hybrid (FTS + local vectors). To import real data, point the import field at any `.mbox` file (e.g. exported from Thunderbird), use **Import chat export…** to pick a WhatsApp `Export chat` `.txt` file or a Telegram export's `result.json` (picking the export directory works too), or a Signal conversation `.json`, or a Discord Data Package export — the package root, its `messages/` directory, or a single channel `messages.json` — unzipped "Attach Media" WhatsApp exports also copy their files into the vault's media store unless **extract media** is unchecked — or fill in the **IMAP sync** section (see below). To take results out, run a search and press **Export results…** (or open a message and press **Export chat…**), pick a format and a destination — the export always covers every row of the current search, not just the page on screen.

The database lives in your app data dir (e.g. `~/.local/share/com.acailic.mailchatvault/vault.db` on Linux); extracted chat media lives next to it under `media/`, each file named by the SHA-256 of its contents.

## LLM-assisted search (v1)

Ask a natural-language question (“what was said about the vault in October?”); the app translates it into an FTS5 query, runs it locally, then produces a short answer over the top results (RAG style). Entirely optional and fail-safe: without an API key the app behaves exactly like v0's plain full-text search.

### How it works

1. **Translate** — the question is sent to the LLM, which replies with a JSON plan: an FTS5 MATCH expression over `body`/`author` plus an optional sent-date range (it understands relative wording like “last march” against today's date).
2. **Search & rank** — the plan runs locally against SQLite. The LLM's expression is validated against the index first; anything unparseable or referring to unknown columns is replaced by a plain term search (usable date filters are kept). The expression is always passed as a bound MATCH parameter, never spliced into SQL. The FTS ranking is then fused with the local vector leg (see [Local embeddings](#local-embeddings--hybrid-search-v1)) when embeddings exist — a fully local step.
3. **Summarize** — a short answer is synthesized from **only the top 8 result rows** (bodies truncated to 500 chars), cited as [1], [2], …

The UI always shows the plan actually used (`FTS: … · dates: … · ranked: … · planned by LLM / fallback`) plus a privacy note describing exactly what the current mode sends.

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

- No API key → plain FTS5 term search with local hybrid ranking when embeddings exist; the UI says so up front.
- LLM error, timeout, or invalid/unusable plan output → plain term search plus a visible note; search results never depend on the LLM succeeding.
- No embeddings built → plain FTS5 ranking; search results never depend on the vector leg either.

### Costs & latency

Measured against `glm-5` at `https://api.z.ai/api/anthropic` (Nov 2026):

- Translate: ~1.5–3 s, roughly 350 input / 100 output tokens.
- Summarize (8 results × ≤500 chars): ~2–5 s, roughly 2,000 input / 200 output tokens.
- One “Ask” ≈ two small requests, a few seconds end-to-end, well under a cent per ask at GLM pricing.

## Local embeddings & hybrid search (v1)

Ask queries are ranked **hybrid**: the FTS5 result list is fused with a **local vector search** over the same archive — entirely offline, no model downloads, no network.

### Embedding model

The bundled embedder (`hash-ngram-v1`, 512 dims) is dependency-free and deterministic: word unigrams, word bigrams, and character 3-grams of each token are FNV-1a-hashed into signed buckets and L2-normalized. Measured on the fixture families of the test suite: related texts score 0.18–0.55, unrelated ones ≤ 0.04, and a scan floor of 0.08 separates the two. Subword overlap gives recall beyond exact FTS tokens — "deploying" still finds "the deployment kept failing".

Heavier options (fastembed / a GGUF mini model) were considered and deliberately deferred: both need model weights that either download at first run (breaking the offline promise) or ship as a multi-megabyte blob in the repo, plus a native ONNX/GGML runtime build. The `Embedder` trait and the model-id-versioned embedding rows keep a future bundled model a drop-in: it ships a new `id`, the incremental job re-embeds, old rows are replaced.

### Vector index — manual cosine in SQLite (the simple choice)

Embeddings live in a `message_embeddings` table (schema migration 3) as **one 512-byte i8-quantized blob per message**, and a query scans them with an integer dot product in Rust (vectors are stored normalized, so the dot product *is* the cosine; i8 keeps the index ≈ 0.5 KB per message). sqlite-vec was considered and rejected: it adds a C extension dependency for what is, at personal-archive scale, a linear scan — 100k messages ≈ 50 MB of vectors, measured at ~75 ms per scan (best of 5; `tests/vector_perf.rs` enforces the budget). Cost scales linearly with embedded messages, which is the right trade for the archive sizes this app targets.

### Hybrid ranking

FTS5 relevance and cosine similarity have incomparable scales, so the two ranked lists are fused with **Reciprocal Rank Fusion**: score(d) = Σ 1/(60 + rank). A row found by both legs outranks a row found by one. The plan's date bounds apply to the vector leg exactly as to the FTS leg. The Ask status line always shows the mode used: `ranked: hybrid FTS + local vectors`, `ranked: FTS only`, or `ranked: local vectors only`.

### Incremental job

**Update local embeddings** (in the Ask section) embeds only messages still lacking an embedding for the current model, in batches: the first run covers the whole archive (~5 s for 100k messages), later runs only new imports (~90 ms for 500 new messages — both budgets enforced by tests). Deleting a message cascades to its embedding; changing the model id makes every row pending again and re-embeds.

### Fallback & privacy

- No embeddings built yet → Ask is plain FTS5 exactly as before; the pipeline never depends on the vector leg.
- No LLM key → the hybrid ranking is still fully local; nothing leaves the machine.
- Embeddings are computed locally from message bodies and are **never sent anywhere** — not even in LLM mode (only the question and the top-8 result bodies go to the provider, as before).

## Folder watching

The **Folder watching** section turns the archive into a live mirror of your
export folders: type a path (or use **Pick folder…**) and every new or
modified `.mbox`, WhatsApp `.txt`, Telegram `result.json`, or Signal
conversation `.json` file under it — including files inside subfolders — is
imported in the background.

- **Debounced**: files being written or copied fire many filesystem events;
  a file is imported only once it has stayed quiet for ~1.5 s. A truncated
  read self-heals: the next modification re-imports the file and dedup keeps
  the archive consistent.
- **Dedup by contract**: imports go through the ordinary ingest pipeline, so
  `(source, external_id)` uniqueness makes every re-import safe — the initial
  scan of a folder, re-settling, and rewrites of a file all store each message
  exactly once.
- **Never fatal**: unreadable files, unknown formats (reported as *skipped*),
  and database hiccups are recorded in the events list; the watcher keeps
  running. A folder that vanishes mid-session is silently disarmed for the
  session. Skipped `.txt`/`.json` files that are not recognized exports
  are reported, not imported.
- **Persistent**: watched folders are stored in the vault database and
  re-armed on the next start (a folder that is missing at startup is
  reported; it re-arms on the next start once it is present again).

The status line shows how many folders are watched and how many files are
still settling; the events list below shows folder add/removals, imports
(with new/duplicate counts), skips, and errors. A background import
refreshes the source chips and stats like any manual import.

## IMAP sync

The **IMAP sync** section pulls mail straight from a server into the archive — incrementally: each folder's `UIDVALIDITY` + highest-seen UID are tracked in the vault database (keyed by the account's durable source instance), so every sync fetches only new messages. If a folder is recreated (`UIDVALIDITY` changes), it is rescanned and re-fetched messages are deduped through the connector identity — a reset never creates duplicate rows. The folder name is stored as the message `channel`.

Each synced account is a **durable source instance**: its identity is the canonical connector configuration — case-folded host, port, TLS mode, and the exact username — while the password stays only in the OS keyring (or process memory). Reconfiguring credentials preserves the instance; two accounts on different hosts stay distinct even with the same username; and the same mail seen in several folders is one archived message with one folder occurrence per sighting.

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

## Export

The **Export results…** control writes the current search results — every row matching the active text + source/author/date filters, across all pages, in the order the UI lists them — to a file you choose. **Export chat…** (in the message detail panel) writes the whole chat thread of the open message, oldest → newest; a message with no thread exports as a single row. Formats:

- **CSV** — RFC 4180 (`source,external_id,channel,chat_id,author,sent_at,body,raw_json`), UTF-8, fields quoted only when needed; cells beginning with `=`, `+`, `-` or `@` get an apostrophe prefix so spreadsheet apps cannot execute them as formulas.
- **JSON Lines** — one complete message object per line (`.jsonl`); the exact shape the app stores. Export-only for now: no app importer accepts this file yet.
- **MBOX** — classic `From `-separated mailbox rebuilt from the stored row plus its recorded headers (`Message-ID`, `From`, `To`, `Subject`, `Date`); carries email-channel rows only, chat rows are skipped and reported in the status line.

Rows stream straight to the file, so exports of very large result sets don't hold the whole set in memory.

## Tests

```sh
cargo test               # vault-core: storage, migrations, dedup, FTS search, mbox / WhatsApp / Telegram / Discord ingest, media extraction (real small fixture files), filtered/paged search, chat context, archive statistics (incl. the 1M-message dashboard perf contract), 100k FTS perf fixture, IMAP sync (mock server; durable source instances, multi-folder occurrences, conflicting revisions), LLM search pipeline (mock LLM, no network), local embeddings (deterministic embedder, incremental job, cosine scan, hybrid ranking, 100k vector perf budgets), folder watching (real temp folders), export (CSV/JSON/MBOX round-trips incl. unicode + large sets), integrity suite (corruption & crash recovery, export/import round-trips, VACUUM/ANALYZE, schema migrations)
npm test                 # frontend unit + UI-state snapshot tests (vitest)
npm run check            # TypeScript type-check
cargo test --workspace   # also compiles the Tauri app (needs the Linux libs above)
```

The vector layer's budgets over 100k embedded messages (enforced by `tests/vector_perf.rs`): first-run embedding job < 15 s, cosine scan < 200 ms, full local hybrid ask < 250 ms; the FTS < 100 ms contract stays in `search_perf.rs`.

The parser layer is also fuzzed (`crates/vault-core/tests/fuzz/`): a
hand-crafted malformed-input corpus plus deterministic seeded structured
generators assert that no input — truncated files, mixed formats, Unicode
edge cases, duplicate JSON keys, bogus timezones, hostile MIME nesting —
may ever panic a parser, only produce an `Err` or a counted parse
failure. Raise the random rounds locally with
`MC_FUZZ_ITERS=100000 cargo test -p vault-core --test fuzz --release`.

CI (`.github/workflows/ci.yml`) runs all of the above plus `clippy` and `fmt --check` on every push and PR.

IMAP sync is tested hermetically against an in-process mock IMAP4rev1 server (the greenmail equivalent), including an integration test that ingests the same `.mbox` fixture through both paths and compares the archives. A real-server test (`tests/imap_gmail_manual.rs`) is `#[ignore]`d: it needs live credentials and is **not** run in CI. To run it against your own account:

```sh
MAILCHAT_IMAP_USER=you@gmail.com MAILCHAT_IMAP_PASS=abcd-efgh-ijkl-mnop \
  cargo test --test imap_gmail_manual -- --ignored --nocapture
```

It writes to a throwaway temp directory, never your real vault.

Testing strategy for the UI: driving the Tauri webview headlessly in CI is
not practical (webkit2gtk driver + display), so the UI is covered by tests
over the query layer (`crates/vault-core`), snapshot tests of the rendered
UI state (`src/render.test.ts` via happy-dom: results list, pagination,
source chips, detail panel), and wiring tests that boot the real
`index.html` markup against a mocked Tauri IPC bridge and drive it with DOM
events (`src/main.test.ts`: search/empty-query guard, loading/empty/error
states, filters, grouping, pagination, the stale-response guard, the detail
view, keyboard navigation, and the export controls with a mocked save
dialog). Export e2e is not driven through the real OS save dialog for the
same webview reason; the formats themselves are round-trip tested in
`crates/vault-core/tests/export.rs`.

## Out of scope for now (separate tasks)

XOAUTH2 (OAuth) login for Gmail, multi-account UI, multi-provider menus, settings UI, push notifications. The media-extraction layer is source-generic: when further chat sources land, their exports plug into the same hash-named `media/` store and `raw_json` convention.

## Installers & releases

Pushing a version tag (`v*`) builds distributable installers — Linux AppImage + `.deb` (defaults of the enabled Tauri bundling), plus macOS `.dmg` and Windows `.msi`/NSIS `.exe` from the release workflow — as a **draft GitHub Release**. Artifacts are **unsigned** in this round (no code signing; Gatekeeper/SmartScreen caveats apply on macOS/Windows). The app ships with a full icon set (Linux/mac/win) generated from `src-tauri/icons/source.png`. See [docs/release.md](docs/release.md) for the install smoke-test checklist, local build commands, and how to regenerate the icons.

## Notes on chat import details

- **WhatsApp**: export dates carry no UTC offset, so they are interpreted DST-aware in the machine's IANA zone and stored with the resolved UTC instant plus timezone/confidence provenance (a stamp inside a DST gap stays unresolved with the verbatim stamp kept); day-first vs month-first ordering is inferred from the file's own dates (day-first when ambiguous). Media placeholders (`image omitted`, `(file attached)`, …) are kept as text and recorded in `raw_json`; when the exported files sit next to the `.txt` ("Attach Media" export, unzipped), named attachments are matched by file name — case-insensitive full name first, then by stem (for export flavors that re-encode extensions, e.g. `IMG-1.jpg` shipped as `IMG-1.jpeg`) — and copied into the media store, linked from `raw_json` (`media` array). `<Media omitted>` placeholders carry no name and stay placeholders.
- **Telegram**: `date_unixtime` is preferred for timestamps (timezone-correct); service entries (calls, joins) are skipped; the original message JSON (attachments, reply references included) is kept verbatim in `raw_json`, and `photo`/`file` paths are resolved inside the export directory (`..` and symlink escapes are rejected), stored hash-named, and linked via the added `media` array.
- **Signal**: conversation exports are top-level JSON arrays of Signal Desktop's raw message objects (as written by `sigtop export-messages -f json`, one file per conversation). Timestamps (`sent_at`/`timestamp`/`received_at`) are absolute epoch milliseconds, stored as exact UTC; outgoing messages are archived with author `You`; service entries (call history, key changes, profile changes, group changes) are skipped; message ids are namespaced by the conversation uuid, so a re-export under a renamed file dedups instead of duplicating; the whole original message object (attachments, quotes, reactions) is kept verbatim in `raw_json` — media extraction is not implemented for Signal yet.
- **Discord**: the Data Package (Settings → Privacy & Safety → Request Data) stores one `messages.json` per channel under `messages/`; pick the package root, the `messages/` directory, or any single channel file. Timestamps carry an explicit UTC offset and are normalized to UTC; `chat_id` is `guild/channel` for server channels and the `index.json` path (e.g. `Direct Messages/@friend`) for DMs; system entries (joins, pins, calls) are skipped while replies are kept; the original message JSON (reactions, attachment info, edits, mentions included) is kept verbatim in `raw_json`. Message ids are snowflakes namespaced by channel id, so re-importing a renamed channel never duplicates.

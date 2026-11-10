# mailchat-vault

Local email & chat archive — Tauri desktop app with SQLite FTS5 full-text search over your own messages. Private tool; nothing leaves the machine, except what you explicitly send for optional [LLM-assisted search](#llm-assisted-search-v1) (your question + the matching result rows only, and only when an API key is configured).

This is the **v0 foundation + usable search tool**: a complete vertical slice — import, store, dedup, search, filter, browse, read in conversation context. **v1** adds LLM-assisted natural-language search with local-embedding **hybrid ranking** on top (see below), with **multiple configurable providers and a Settings screen**, the first live mail source: **incremental IMAP/Gmail sync** (see below) — Gmail via **Google sign-in (OAuth2/XOAUTH2)** or app password — **automatic folder watching** (see below), and **desktop notifications** for new mail after sync/watch cycles (see below). The rest of the roadmap (chat platform formats) will land as separate tasks.

## What v0 does

- **Tauri 2 desktop app** (Rust backend, vanilla TypeScript + Vite frontend).
- **SQLite storage with FTS5 full-text index** (bundled SQLite via `rusqlite`; no system SQLite needed).
  - **Canonical store** (schema v14): `source_instances` (durable connector entities: kind, label, non-secret configuration, connected state, per-source exclusion flag, last attempt/success, last sync error), `conversations` (stable provider keys), `archive_messages` (ids, payload, and time provenance: resolved UTC instant, original timestamp text, timezone, confidence), `message_identities` (dedup identities — legacy `(source, external_id)` aliases plus connector-scoped keys with semantic revision hashes), `message_occurrences` (import/folder observations), `import_checkpoints` (bounded-import resume positions), `import_issues` (durable per-item parse failures), `sync_checkpoints` (IMAP cursors), `sync_failed_items` (the IMAP failed-item ledger: what fell, where, why, how often retried), plus the `archive_fts` virtual table (external content, trigger-maintained). Every read and write targets these tables.
  - **Safe migration** (M1.4): opening an existing database with pending migrations first refuses any database written by a newer build (before a single byte is touched), preflights free disk space, snapshots the archive file (`<db>.pre-migrate-v<version>.snapshot`, WAL checkpointed first), converts in **one transaction** (all or nothing), then validates the result (`PRAGMA quick_check` plus full-text-index document-count parity and FTS5 `integrity-check`). Any failure — before, within, or after the conversion — restores the original byte-for-byte from the snapshot; a validated conversion cleans the snapshot up. A failed post-conversion validation additionally records a durable `<db>.migration-failed` marker, so later opens refuse with that recorded reason instead of retrying the same deterministic failure forever — the original stays recoverable from the kept snapshot, and recovery is manual by design (fix or restore the archive, then delete the marker). Proven by failure-injection tests at every stage.
  - **Stable conversation keys**: Telegram/Signal chats key on the provider-guaranteed chat id and WhatsApp chats on a content-derived key (sorted participants + first authored message), so the same chat re-exported under a rename stays one conversation (the label follows the newest name) while different chats sharing a display name stay distinct. **Email reply graph**: `In-Reply-To`/`References` chains group mail into threads — one-hop resolution, so incomplete chains and reference cycles are safe — with replies imported before their roots adopting them into the right thread later. Timestamps carry **explicit timezone interpretation** (exporter-local WhatsApp stamps resolve DST-aware in the machine's IANA zone; Telegram's naive `date` fallback stores the exporter-UTC stamp verbatim; mail offsets and chat epochs are exact) with per-message confidence labels. Legacy databases upgrade in place (migration 7): renamed imports merge, same-name chats split, WhatsApp identities re-key so post-upgrade re-imports dedup, and their legacy blind-Z stamps resolve to stored true UTC instants (machine-zone interpretation), while whatever cannot be safely interpreted stays legacy-unresolved.
  - The legacy `messages` / `messages_fts` / `imap_sync_state` tables from earlier schema versions are preserved untouched as a pre-canonical snapshot; migration 4 copies every row (ids included) into the canonical tables, and nothing writes the legacy tables afterwards. Migration 8 adds the durable source-instance columns (connector identity key, state, attempt/success stamps); migration 9 adds the IMAP failed-item ledger and the per-source exclusion flag; migration 10 adds the bounded-import recovery tables (`import_checkpoints` resume positions, `import_issues` durable per-item parse failures); migration 11 seeds the periodic-sync schedule singleton (`sync_schedule`, M2.4: opt-in flag + interval; runtime deadlines are process-local by design).

  - Trigger-maintained counter tables (`stats_source`/`stats_day`/`stats_chat`/`stats_author`) power the statistics dashboard with O(groups) reads instead of GROUP BY scans.
  - Versioned migrations applied automatically on open — guarded by the safe-migration path above (snapshot, rollback, newer-schema refusal).
- **Idempotent ingest**: a `Source` trait yields messages; writes dedup on the canonical identity — legacy `(source, external_id)` keys map to one `message_identities` row each, and the IMAP connector scopes identities to their durable source instance with a semantic revision hash (the same mail in several folders is one row with one occurrence per folder; two conflicting payloads behind one `Message-ID` are preserved as two revisions) — so re-importing and re-syncing are always safe. Production imports stream through the **bounded parsers (M2.2)**: they never hold a whole archive in memory, parse failures land as durable `import_issues` rows, and an interrupted import (crash or cancel) resumes from its committed checkpoint on the next run — re-importing the same file is always safe.
- **Manual/demo source**: import an `.mbox` file (no IMAP needed) — subjects, authors, dates, and decoded text bodies are archived and searchable; each mail's MIME attachments/inline parts (M4.2) are decoded (base64/quoted-printable undone) and copied into the content-addressed `media/` store, linked from the message's `media` entries.
- **Chat imports**: WhatsApp `.txt` exports, Telegram Desktop `result.json` exports, Signal Desktop conversation `.json` exports (e.g. `sigtop export-messages -f json`), and Discord Data Package `messages.json` files — texts, authors, and timestamps are archived and searchable; quoted replies are kept as metadata, and Signal/Discord attachments are recorded in `raw_json` (Discord reactions too). A picked **`.zip`** works too (WhatsApp export zips, Telegram export zips, Discord Data Package zips): it is expanded through the M4.1 import bounds and resolved like a directory pick. **Media extraction** for WhatsApp and Telegram: files referenced by the export (images, documents, voice notes) are copied into a content-addressed `media/` store in app data — hash-named (SHA-256), deduplicated by content — and linked from the message's `raw_json`, with a media section in the detail view. Extraction can be switched off with the **extract media** toggle; text-only exports import fine (each absent file is recorded as an explicit `missing` entry on its message — never silently dropped), re-importing an export with its files backfills media links into already-stored messages, and a whole-tree pick (directory or `.zip`) also stores files no message references — only the media budget running out or the unlinked tree walk failing can leave export files un-stored, and both are counted honestly in the import summary (`unlinked_dropped` / `unlinked_walk_failed`) instead of dropped silently. The format is auto-detected from the file contents, so re-importing the same export never duplicates messages.
- **IMAP sync (Gmail & friends)**: incremental, UIDVALIDITY/UID-tracked sync from any IMAP server over host/port/user — Gmail via **Google sign-in (OAuth2 → SASL XOAUTH2)** or **app password**; any other IMAP server via user/password. Folders map to the message `channel` field; re-syncing never duplicates. Each fetched mail's MIME attachments/inline parts are stored into the media store as the sync parses them (M4.2), deduplicated by content across messages, folders, and accounts. Secrets are prompted for or obtained via OAuth, kept in the OS keyring when available and in session memory otherwise — never in the database. The write unit is the **atomic fetch batch** (M2.3): one batch's messages, its failed-item ledger updates, and the checkpoint advance commit in a single transaction, so an interruption mid-folder leaves the checkpoint exactly at the boundary of what is durably stored; items the vault cannot store (unparseable mail, a uid the server searched but never delivered) are recorded in a **durable failed-item ledger** (what fell, where, why, how often retried — visible via `source_failures`) and retried by every later round until they resolve; a `UIDVALIDITY` reset rescans from UID 1, dedups, and clears the abandoned validity's ledger rows in the same commit.
- **Background jobs (M2.1)**: the archive is served by a database pool — one dedicated **writer worker thread** behind a **bounded write queue** plus **two read connections** (WAL) — so searches never wait behind imports, syncs, or embedding runs. Long operations run as **typed background jobs** (queued → running → succeeded/failed/cancelled, with progress events) exposed through the v2 `job_*` IPC commands; misses in the `job-event` event stream are recovered by **polling with a monotonic cursor**. Network work (an IMAP fetch) never runs inside the write lane, a storage lock, or a transaction (see below).
- **IMAP coordinator (M2.3)**: rounds over **every configured IMAP source at once** — the `imap_sync_all` background job (or a coordinator round through the same API) lists the vault's durable source instances, **skips excluded ones** (per-source exclusion keeps the archive and cursors, only pausing the syncing — `source_set_excluded`), resolves each source's credentials from the OS keyring (stored password, else a stored Google sign-in refreshed into a fresh access token; a source without credentials is reported, not errored), and syncs each through the M2.1 pool: network waits on the job thread, atomic batch commits through the write queue — searches and writes keep working while a round is parked on a stalled server. A source that fails syncs alone and carries its error; the others still sync.
- **Periodic sync scheduler (M2.4)**: an **opt-in** auto-sync — off until you turn it on — that runs one coordinator round every interval (**15 minutes by default**; 5/15/30/60 selectable, 1 minute..1 day accepted) as an ordinary `imap_sync_all` job, so scheduled rounds show up in the jobs panel like manual ones. The schedule persists in the vault and re-arms on the next start. Scheduling never overlaps and never queues missed runs: there is one deadline and at most one scheduled round queued or running — a round that outlasts the interval simply delays the next one — and each deadline anchors at the previous *submission*, so sleep/resume/offline gaps collapse into exactly one catch-up round instead of a burst. Disabling cancels a still-running round (cooperatively); enabling arms one full interval out (never an immediate round). Exposed through `sync_schedule_get` / `sync_schedule_set` and the `scheduler-status` event; the toggle lives in the IMAP sync section.
- **Desktop notifications (new mail after sync/watch)**: when a sync round — the Sync button, a coordinator round, or a scheduled auto-sync round — or a folder-watch import stores **new** messages, the app shows one native OS notification ("2 new messages — INBOX, Archive" / "imported from chat.txt") through the Tauri notification plugin: the same code path on macOS, Windows, and Linux. Notifications stay **silent while the app window is focused** (you are already reading in the app), never fire for a cycle that stored nothing new (skipped/failed/resolved items don't count), are never duplicated when events replay, and can be turned off with the **Desktop notifications** checkbox in Settings (on by default; enabling it asks the OS for permission). A missing permission or unavailable OS notifier degrades silently — the archive data is already safely stored.
- **Folder watching**: pick folders (typed path or the native picker) and every new or modified `.mbox` / WhatsApp `.txt` / Telegram `result.json` / Signal conversation `.json` export file under them is imported automatically in the background — watched folders persist across restarts, write bursts are debounced until a file settles, re-imports never duplicate (the same dedup contract as manual import), and skips/errors are shown in the UI without ever stopping the watcher (see below).
- **Accounts UI (multi-account)**: every configured account — email (IMAP) accounts and file imports (mbox / chat exports) — in one **Accounts** overview: per-account message counts (the same trigger-maintained counters the source chips use), open failed-item backlog, last-sync stamps, the last sync error (what fell, why, when — kept until the next successful sync), pause state, and which credentials are stored. Email accounts are managed right from the row: **Sync now** re-runs `sync_imap` for that account with its stored host/user/folders and an empty password (the backend resolves the keyring secret — password first, else the Google sign-in, exactly the coordinator's precedence), **Pause/Resume** is the per-source exclusion (auto-sync rounds skip it; archive and cursors kept), and **Forget credentials** removes the account's password and Google sign-in from the OS keyring while the archive stays untouched. Accounts are derived from the stored sources + sync cursors — no new tables, no schema break; the per-account stats query is bounded and indexed (one counter row per account).
- **Statistics dashboard**: archive aggregates in one view — total messages and date range, a per-day histogram (auto-downsampled to months/years when the archive spans too many days), and tables with inline bars for messages per source, top chats, and top authors. Served from trigger-maintained counter tables (schema migration 6), so the whole dashboard answers in **< 100 ms even on 1M messages** (asserted by a generated 1M-fixture test).
- **Workspace layout (M3.1)**: the app is a three-area workspace — **source navigation** (All messages, per-source scope chips, Add source, Accounts, Activity, Settings), **results** (search + filters + pagination + export + the secondary Ask mode), and a **reading pane** (full message with thread context) — with independent vertical scrolling. Imports, account configuration, and maintenance live outside the reading surface in navigation destinations. Visuals come from **local design tokens** (semantic CSS custom properties for canvas/panel/text/border/accent/selection/match/status colors, light + dark + System themes via the existing theme setting) and the **bundled Source Sans 3** variable font (latin + latin-ext + italic, SIL OFL 1.1 — license committed at `src/assets/fonts/OFL.txt`; falls back to native UI fonts, nothing is fetched remotely). **Narrow-window states**: ≥ 1100px shows all three areas side by side; 800–1099px shows navigation + results, and opening a message swaps in the reading view with a visible **Back to results** (query, filters, page, selection, and scroll are preserved); below 800px (including 1280px at 200% zoom) everything collapses into one column behind a labeled **navigation drawer** — no horizontal scrolling. **Accessibility**: semantic landmarks (`nav`/`main`/`aside`), labelled controls, `role=status` live regions announcing search/job/status changes, a skip link, focus moved to the message heading when the reading view swaps in (and restored to the initiating result on Back), and `Ctrl/Cmd+K` or `/` to focus search. The frontend is split into explicit modules: typed **IPC boundary** (`src/ipc.ts`), **state** (`src/state.ts` — layout resolution, the last-request-wins `SequenceGuard` behind every panel's stale-response protection, and the workspace store), and the **shell controller** (`src/workspace.ts`). Layout states are proven by the `16-workspace-layout` e2e spec with committed wide/medium/compact/zoom screenshots (`tests/e2e/fixtures/screenshots/`, synthetic data).
- **Search tool UI** over the FTS5 index:
  - results in a **flat list** (no conversation regrouping) with **match highlighting** (FTS5 snippets, rendered as `<mark>` runs), in **relevance or newest order** (your choice);
  - **filters**: source chips (email/whatsapp/telegram/… — whatever the archive holds), author substring, a from–to date range, an attachments with/without selector, and an import-scope selector (browse one bounded import run); an empty query browses newest-first;
  - **result count + pagination** (`1–50 of 1,234 · page 1/25`);
  - **export**: the current search results — every row under the active filters, all pages, not just the visible one — or a **selection** of the rows you check (as UTF-8 text or JSON with provenance), or the whole chat thread from the detail view, written to a file you pick (native save dialog) as **CSV** (RFC 4180), **JSON Lines** (one full message object per line; export-only for now — no importer accepts this file yet) or **MBOX** (email rows; non-email rows are skipped and reported);
  - **message detail** (reading pane): full body with highlighted query terms, full metadata, and the **chronological conversation context** around the message — load earlier/later pages in place; copy, copy link (`mailchat://message/<id>`), and export actions;
  - **keyboard navigation**: `Ctrl/Cmd+K` or `/` focus search, `↑`/`↓` move through results, `Enter` open detail, `↑`/`↓` walk the conversation context, `Esc` close (never erases the query).
- **Media viewing (M4.3)**: preserved image/audio/video attachments render inline in the reading pane through the app's read-only `mcmedia://` protocol — strict hash-named object URLs (`media/<sha256>.<ext>` shapes only), byte ranges, and a hard response CSP; playback is always user-started (no autoplay) and nothing is fetched remotely. Documents and everything else stay explicit rows with **Save a copy…** (byte-exact export via the native save dialog) and **Open externally…** (a disposable 0600 temp copy in a 0700 temp directory is handed to the OS handler — the store object is never touched, only the most recent open's copy survives, and copies are purged again at startup). Settings gains a **Media store** section with a disk-usage report (totals, referenced/orphaned split, per-extension and largest objects) and a Reconcile hash check.
- **Backup & restore (M4.4)**: one versioned `.mcbackup` container holds the whole vault — a consistent SQLite snapshot (taken through SQLite's backup API, never a live-file copy), the settings file, and every referenced media object, each entry checksummed in a manifest written last. Restoring is staged and journaled: the container is re-checksummed and its database validated (integrity, foreign keys, schema ceiling, object presence) in a staging vault outside the active one, the commit re-verifies the staged bytes and refuses a vault that changed since the preview, and the switch itself runs through a two-phase journal (crash → rollback or roll-forward to one verified whole vault, recovered automatically at startup). The replaced vault is always kept as a previous-vault snapshot (Settings → Storage & backup lists them; return to one or explicitly forget it). Passwords and API keys are never backed up or restored — only accounts whose credentials are missing from this machine's keyring need reconnecting after a restore (a same-machine restore with live keyring entries asks for nothing).
- **Performance contract**: queries answer in < 100 ms over 100k messages (asserted by a generated-fixture test; all queries are `LIMIT`-bounded and ride the FTS5 index plus the `sent_instant`/`(source_instance, chat_id)` indexes); the statistics dashboard answers in < 100 ms over 1M messages.
- **Ask UI (v1)**: natural-language questions translated to FTS5 + date filters via an LLM, with a short summary over the top results — strictly privacy-scoped, **consent-gated** (M5: nothing leaves the machine without an explicit, session-scoped agreement), and fully optional (see below). Results are ranked **hybrid** — FTS5 fused with an offline local-vector leg once embeddings are built (see below). A **provider menu** next to the question picks the backend per ask: any provider configured in Settings, the environment fallback, or **local full-text** (nothing sent anywhere). **Answers are source-linked** (M5): citations `[1]`, `[2]`, … in the answer are clickable, as are the numbered result rows beneath — either opens the original archived message in its conversation context in the reading pane.
- **Settings (v1)**: LLM provider configuration in one place — z.ai/GLM (default), Anthropic, or a custom Anthropic-compatible endpoint — plus the default provider, UI theme, and summary scope. Values live in a `settings.json` file (never the database); API keys live in the OS keyring (see below).

## Repository layout

```
crates/vault-core/   # storage (SQLite/FTS5, migrations + safe-migration path: snapshot/preflight/validation/rollback), filtered/paged search queries, archive statistics (dashboard aggregates), ingest trait, bounded streaming ingest (M2.2: batched commits, cancellation, issue records, restart recovery), mail parsing, mbox / WhatsApp / Telegram / Signal / Discord sources, media extraction into the content-addressed store, object store (M4.1: stage→promote→reference lifecycle, capacity budget, reconciliation/integrity tool), import bounds (M4.1: traversal/symlink-safe path resolution, bounded directory walks, bounded ZIP expansion), backup (M4.4: versioned ZIP64 container with checksummed manifest + SQLite-backup-API snapshot, staged restore with SQLite/foreign-key/object validation, two-phase recoverable switch journal, previous-vault recovery surface), IMAP sync (password + OAuth2/XOAUTH2, incl. the SyncStorage trait splitting network from storage, atomic fetch-batch commits, and the failed-item ledger), IMAP coordinator (M2.3: rounds over every configured source, per-source exclusion, credential resolution), periodic sync scheduler (M2.4: opt-in job-backed rounds, no overlap, no missed-run queueing), jobs (M2.1 database pool: writer worker + two read connections + bounded queues + typed job registry/event log), OAuth2 login flow, LLM search pipeline, local embeddings + hybrid ranking, folder watching, CSV/JSON/MBOX export, settings model (providers, pairing rules) — no UI deps
src-tauri/           # Tauri app shell: commands (search_messages, chat_context, llm_ask, llm_status, embed_messages, embed_status, import_mbox, import_chat_export with optional media extraction (directory or .zip picks), sync_imap, sources_list, source_set_excluded, source_failures — the M2.3 source surface, accounts_list, account_forget_credentials — the multi-account overview (per-account counts, sync state, credential kinds; never the secrets), sync_schedule_get, sync_schedule_set — the M2.4 periodic-sync schedule, oauth_gmail_login, oauth_status, watch_add_folder, watch_remove_folder, watch_status, job_submit, job_get, job_list, job_events_since, job_cancel — the v2 background-job protocol (kinds incl. backup_create / restore_preview, M4.4), seed_demo, stats, archive_stats, export_search, export_chat, settings_get, settings_save, llm_key_clear, search_archive_v2 / message_detail_v2 / conversation_context_v2 / list_import_scopes_v2 / export_messages_v2 — the M3.2 v2 search surface over the canonical store, with flat stable ranking, archive-revision refresh signalling, and structured AppError results, media_reconcile — the M4.1 store-vs-archive integrity report, media_usage / media_export / media_open_external — the M4.3 media surface (read-only `mcmedia://` protocol handler + the media_proto module, disk-usage report, byte-exact export, external open of a disposable temp copy), backup_inspect / restore_commit / recovery_status / recovery_forget / return_to_previous_vault — the M4.4 backup/restore + previous-vault recovery surface (startup resolves interrupted switches before the vault opens), demo_enter / demo_leave / demo_status — the U02 isolated sample archive (an in-memory vault: reads route there while open, personal writes refuse, never persisted), preview_import — the U03 bounded import preview (format, samples, bounded counts, media presence, size + free space; nothing stored); llm_ask carries the M5 AI-consent gate — no network round trip without explicit consent, proven by mock-endpoint tests) + LLM provider registry + secret storage (passwords & OAuth refresh tokens, keyring) + watch-status/job-event/scheduler-status events

src/                 # frontend (vanilla TS): workspace shell (src/workspace.ts + src/state.ts + src/ipc.ts — layout/view/reading state, typed IPC boundary) with design tokens and the bundled Source Sans 3 font (src/styles, src/assets/fonts), search + filters + pagination + detail view (inline image/audio/video via the read-only `mcmedia://` protocol — no autoplay — plus per-object Save a copy…/Open externally… actions), ask UI with provider menu + the M5 session consent line/prompt + embedding controls + source-linked answers (clickable citations and source rows open the original message in thread context), settings screen (providers, theme, media-store report, and the M4.4 Storage & backup section — backup creation, staged restore flow, previous-vault recovery), accounts panel (src/accountsview.ts — the multi-account overview: per-account stats/sync-state wording, sync-now request derivation, rows with sync/pause/forget actions), statistics dashboard (CSS bar charts, no chart library), import flow (src/importview.ts — the U03 native-picker → bounded-preview → confirm card, friendly import errors, "View imported messages" follow-up), the U02 first-run card and isolated-sample banner (demo enter/leave, source-control lockout), IMAP sync controls (app password or Google sign-in), folder watching panel, background-jobs panel (v2 event/polling client), export controls, new-mail notification layer (src/notify.ts — digests over sync/watch events, focus/permission/enable gates, silent while the window is focused)
index.html           # single-page workspace shell (navigation / results / reading)
scripts/release/     # release tooling: container build of the Linux AppImage + deb (build-linux.sh), the automated deb/AppImage smoke suites on clean ubuntu 22.04 and 24.04 containers (smoke-linux.sh — apt dependency resolution, .desktop + icon + MIME association, launcher-style launch without a terminal, self-contained AppImage), whole-release download + digest verification with SHA256SUMS publish (release-verify.sh), the published-dmg smoke gate (smoke-macos.sh), the no-CI Windows driver against published installers (windows-smoke-from-release.ps1), the one-command release validation matrix per platform (matrix-linux.sh / matrix-macos.sh / matrix-windows.ps1, M6.4 — full test suite + bundles + smoke; see docs/validation-and-release.md), and the local draft-release assembler (prepare-release.sh, M6.5 — version consistency check, matrix run, bundles, SHA256SUMS, notes from CHANGELOG.md, gated publish checklist; never tags or publishes)
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

First run tip: an empty archive opens on **Add your first source** — click **Import file…** (or **Import folder…** for a Telegram export folder / Discord Data Package) and pick any `.mbox` file (e.g. exported from Thunderbird), a WhatsApp `Export chat` `.txt`, a Telegram export's `result.json`, a Signal conversation `.json`, or a Discord Data Package export; everything shows a bounded **preview** (format, samples, counts, media presence, size) before anything is stored, and **View imported messages** jumps to what the run brought in. Not ready with your own data? **Try a sample archive** — an isolated in-memory demo (search e.g. `vault`, click a hit to read it in context) that never touches your vault; leave it and your archive is exactly as it was. The workspace also browses newest-first on its own (**All messages**), so a fresh import is visible without typing anything. Click **Update local embeddings** in the Ask section to build the local vector index (offline) — Ask then ranks results hybrid (FTS + local vectors). To take results out, run a search and press **Export results…** (or open a message and press **Export chat…**), pick a format and a destination — the export always covers every row of the current search, not just the page on screen.

The database lives in your app data dir (e.g. `~/.local/share/com.acailic.mailchatvault/vault.db` on Linux); extracted chat media lives next to it under `media/`, each file named by the SHA-256 of its contents.

**Media store lifecycle & integrity (M4.1).** Objects are written through an explicit **stage → promote → reference** lifecycle: bytes stream into `media/.staging/` while being hashed and budget-checked, an atomic rename promotes them to `media/<sha256>.<ext>` (never a half-written object in the objects area), and only then does the ingest commit record the digest in the message's `raw_json` — so a crash at any point leaves at worst invisible staging litter (swept later) or a harmless orphan the next import dedups onto, never a dangling reference. Every import tree — a directory walk or a ZIP expansion — goes through the same **import bounds**: `..`/absolute/backslash traversal and symlink escapes are refused, symlink entries never extract, and file-count/byte/depth budgets bound runaway input. Objects are byte-exact copies of their inputs (input-original preservation); the `media_reconcile` command is the integrity tool — it re-hashes the store against every referenced digest and reports missing references, orphans, corruption, staging litter, and foreign files (a clean import reports zero of everything).

**Media references, missing states & re-import enrichment (M4.2).** Every reference a message carries becomes one `media` entry in its `raw_json` — a stored one (`sha256`, `path`, `name`, `kind`, `size`) or an explicit **missing** one (`missing: true` with a reason, `not-in-export`, `unreadable`, or `budget-exceeded`), so the archive never silently drops a reference it could not preserve; the detail view shows missing entries as unavailable attachments rather than as a gap. Email attachments — `.mbox` import and IMAP sync alike — are MIME-decoded by the parser (any `attachment` disposition, an inline part with a `filename`/`name`/`Content-ID`, or a non-`text/*` leaf; the message's own text bodies never count) and staged through the M4.1 lifecycle before their batch commits. A **whole-tree pick** (a directory, or a `.zip` that expands into one) additionally stores files no message references and registers them in the `media_objects` registry — countable by reconciliation as referenced, listable through the vault-core `unlinked_media_objects` library API (the app UI surfaces it in the media-display milestone) — while a bare export file never reaches beyond its own references. Re-importing the same export enriches instead of duplicating: identical bytes hash to the same object (dedup across names, messages, and imports), an already-linked message gains nothing twice, a missing entry is upgraded in place when the bytes finally arrive, and repeated missing markers for the same name are not stacked. Opening a stored object (today a vault-core library API, `resolve_object`; the app UI surfaces it in the media-display milestone) resolves strictly inside the store — it accepts exactly one `<sha256>.<ext>` object name, refusing traversal, sibling names, and anything that is not a stored digest.

## LLM-assisted search (v1)

Ask a natural-language question (“what was said about the vault in October?”); the app translates it into an FTS5 query, runs it locally, then produces a short answer over the top results (RAG style). Entirely optional and fail-safe: without an API key the app behaves exactly like v0's plain full-text search.

### How it works

1. **Translate** — the question is sent to the LLM, which replies with a JSON plan: an FTS5 MATCH expression over `body`/`author` plus an optional sent-date range (it understands relative wording like “last march” against today's date).
2. **Search & rank** — the plan runs locally against SQLite. The LLM's expression is validated against the index first; anything unparseable or referring to unknown columns is replaced by a plain term search (usable date filters are kept). The expression is always passed as a bound MATCH parameter, never spliced into SQL. The FTS ranking is then fused with the local vector leg (see [Local embeddings](#local-embeddings--hybrid-search-v1)) when embeddings exist — a fully local step.
3. **Summarize** — a short answer is synthesized from **only the top 8 result rows** (bodies truncated to 500 chars), cited as [1], [2], …

The UI always shows the plan actually used (`FTS: … · dates: … · ranked: … · planned by LLM / fallback`) plus a privacy note describing exactly what the current mode sends.

### AI consent (M5)

Every network ask passes an **explicit consent gate**, enforced in the backend (not just the UI): `llm_ask` carries a `consent` flag, and **without `consent: true` a network provider is downgraded to a fully local ask before any LLM object is constructed** — no translate or summarize round trip can even start, so nothing leaves the machine (proven by tests against a mock endpoint that record zero requests without consent).

- **Per-question agreement**: the first ask of a session with a network provider selected runs locally and shows a consent prompt stating exactly what would be sent (the question, then the top-8 results, bodies truncated) and to whom (model via host), with **Send & ask for this session** / **Keep asking locally** choices. Agreeing immediately re-asks the same question with the network leg enabled.
- **Per session, never forever**: consent lives in memory only — it resets on every app start and is never written to any file or storage. The always-visible status line shows the current state (`AI consent: granted for this session …` / `not granted — asks run fully locally`), and **Revoke** resets it mid-session; the next ask then asks anew.
- **Answers carry their evidence**: citations `[1]`, `[2]` in the answer are rendered as clickable links, and the numbered source rows beneath the answer match those numbers — either opens the original archived message in its conversation context in the reading pane. Citations beyond the summarized top-8 window (the model never saw those rows) and out-of-range citations stay plain text, never dead links.

### Configuration (Settings UI, or environment variables)

Providers are configured on the **Settings screen**; the key is stored in the OS keyring when one is available (session memory otherwise) — never in the database or the repo, and never in the settings file. Environment variables keep working as a read-only fallback for headless use:

| Variable | Meaning | Default |
| --- | --- | --- |
| `ZAI_API_KEY` / `GLM_API_KEY` | API key for the default z.ai endpoint (`https://api.z.ai/api/anthropic`) | unset → LLM off |
| `ANTHROPIC_API_KEY` | Anthropic key, honored **only** together with an explicit `ANTHROPIC_BASE_URL` | unset → z.ai keys apply |
| `ANTHROPIC_BASE_URL` | endpoint base URL; pairs with `ANTHROPIC_API_KEY`, or must point at the z.ai host (`api.z.ai`, any URL spelling) when a z.ai key is used | unset → `https://api.z.ai/api/anthropic` |
| `MAILCHAT_LLM_MODEL` | model name | `glm-5` |

A mismatched key/endpoint pairing (for example `ANTHROPIC_API_KEY` without `ANTHROPIC_BASE_URL`) is refused with a clear error instead of sending the key anywhere — the same rules the Settings screen enforces per provider (see below).

### Privacy

The product promise is “nothing leaves the machine”; LLM mode is opt-in and strictly scoped: only your question (for translation) and the top-8 matching messages, truncated (for the summary), are sent to the configured provider — and only after the explicit AI consent described above. No other rows, no database dumps, no embeddings, no key at rest. The summary step is skipped entirely when there are no results.

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

## Background jobs (M2.1)

Since M2.1 the app never serves the archive through one guarded
connection. Instead, a **database pool** opens the vault file through
several SQLite connections in WAL mode — each one initialized with the
same pragmas and custom functions — and splits the work:

- a **writer worker thread** owns the one write connection. Writes are
closures submitted over a **bounded queue** (capacity-bounded: backpressure
instead of unbounded memory, one writer instead of lock contention), and
submitters wait for their result — the v1 commands keep their synchronous
IPC shape while gaining the pool's guarantees.
- **two read connections** serve searches, stats, dashboard reads, and
exports; two readers may work at once, and WAL means none of them ever
waits for a commit.
- a **job runner thread** executes long operations (the v2 background
jobs: `embed_messages`, `import_mbox`, `import_chat_export`,
`imap_sync`, `imap_sync_all`, `seed_demo`). A job never holds a
connection: its reads and
writes round-trip the pool, so waiting on the IMAP server — or embedding
a large archive — occupies neither the write lane nor a read slot.
 **No network operation runs inside the writer, a storage lock, or a
transaction**: the IMAP sync (`SyncStorage` + the `PoolSync` adapter)
fetched on its own thread between write round-trips, proven by a test
that parks the mock server mid-fetch and completes a write + a search
while the fetch is stalled. `imap_sync_all` (M2.3) runs the coordinator
round — one source at a time, each with the same atomic batch commits
and the same stall proof at round scale.

Jobs carry **typed states** (`queued` → `running` → `succeeded` /
`failed` / `cancelled`, the latter with typed error kinds: `storage`, `io`,
`import`, `network`, `cancelled`, `queue_full`, `invalid`, `internal`) and report
**progress** (the embedding job, per batch). Every state change appends
an event stamped with a global
monotonic `seq`; the shell emits each one as a `job-event` window event,
and the UI keeps a cursor. **Missed events are recoverable by polling**:
`job_events_since(cursor)` returns exactly the gap plus a fresh job
snapshot, and a `trimmed` flag tells the client the bounded event ring
no longer holds part of the gap — it then reconciles from the snapshot.
The same poll at cursor 0 is the boot recovery after a webview reload.
Cancellation is cooperative (`job_cancel` sets a flag the job checks at
safe points). The **Background jobs** panel in the UI renders the active
and recent jobs with progress; the *Update local embeddings* button now
submits its work through this protocol.

## Settings

The **Settings screen** is where LLM providers are configured — one place, no environment variables needed:

- **Providers**: add any number of named provider profiles. A profile has a *kind* that fixes the key/endpoint pairing rules:
  - **z.ai GLM** (the default) — key for `https://api.z.ai/api/anthropic`; the base URL may be left empty (defaults apply, model defaults to `glm-5`) or must point at the z.ai host;
  - **Anthropic** — the key is only ever sent to an explicitly configured endpoint (never the z.ai host), so base URL and model are explicit (prefilled with `https://api.anthropic.com`);
  - **Custom** — any Anthropic-compatible `/v1/messages` endpoint, base URL and model fully explicit.
  A profile that violates its pairing rule is **refused with a clear error before anything is saved** — the same policy the environment configuration enforces.
- **Default provider**: which backend an Ask uses. None selected = automatic (the environment fallback when valid, else local-only mode).
- **Options**: UI theme (system / light / dark), how many top results feed the RAG summary (1–50, default 8), and the **Desktop notifications** toggle (on by default — see [above](#what-v0-does) for its contract).

### Where values live

- Non-secret settings — provider profiles (kind, base URL, model, name, id), the default provider, and options — live in `settings.json` in the app data dir (e.g. `~/.local/share/com.acailic.mailchatvault/settings.json` on Linux). The file is written atomically and versioned; nothing is ever stored in the vault database.
- **API keys never touch the settings file**: they are stored in the OS keyring (service `mailchat-vault`, entry `llm:<provider-id>`; Secret Service/GNOME Keyring or KWallet on Linux, Keychain on macOS, Credential Manager on Windows). With no keyring reachable, a key is kept only in process memory for the current session and asked for again after a restart — the UI says so.
- **Ask provider menu**: every ask picks its backend — a configured profile (shown with model and host, disabled until it has a key), the environment fallback (read-only, labeled *Environment variables*), or **local full-text**, which sends nothing anywhere. The privacy note under the question always describes exactly what the current selection sends.

## IMAP sync

The **IMAP sync** section pulls mail straight from a server into the archive — incrementally: each folder's `UIDVALIDITY` + highest-seen UID are tracked in the vault database (keyed by the account's durable source instance), so every sync fetches only new messages. If a folder is recreated (`UIDVALIDITY` changes), it is rescanned and re-fetched messages are deduped through the connector identity — a reset never creates duplicate rows. The folder name is stored as the message `channel`.

Each synced account is a **durable source instance**: its identity is the canonical connector configuration — case-folded host, port, TLS mode, and the exact username — while the password stays only in the OS keyring (or process memory). Reconfiguring credentials preserves the instance; two accounts on different hosts stay distinct even with the same username; and the same mail seen in several folders is one archived message with one folder occurrence per sighting.

### Coordinator rounds, atomic batches, failed-item accounting (M2.3)

The **`imap_sync_all`** background job runs one coordinator round over
every configured account at once (the single-account **Sync** button keeps
its explicit per-account form). A round:

- **skips excluded sources** — the per-source exclusion flag
  (`sources_list` / `source_set_excluded`) pauses an account's syncing
  while keeping its archive, cursors, and configuration untouched; an
  explicit single-account sync still runs for an excluded source
  (manual override stays manual);
- **resolves credentials per source** from the OS keyring — a stored
  password, else a stored Google sign-in refreshed into a fresh access
  token; an account with no stored credentials is **reported**
  (`no_credentials`), not errored;
- **never lets one source sink the round** — an unreachable or failing
  account carries its error in the round summary while the others sync.

Every sync — manual or coordinator — commits work in **atomic fetch
batches**: one `UID FETCH` batch's messages, its failed-item ledger rows,
and the checkpoint advance land in a single transaction, so an
interruption (dead connection, killed process) always leaves the cursor
exactly at the boundary of what is durably stored; the next round
resumes from there without duplicates. Items the round accounts but
cannot store are **ledgered durably** — an unparseable mail, or a uid the
server searched but never delivered (typically expunged between the two
commands) — with a fixed reason and an attempt count, visible through
**`source_failures`** (what fell, where, why, how often retried), and
re-fetched by every later round until they resolve. A `UIDVALIDITY`
reset rescans the folder from UID 1, dedups through the connector
identity, and clears the abandoned validity's ledger rows in the same
atomic commit that adopts the new validity.

Two ways to authenticate, picked in the UI:

### Gmail (Google sign-in / OAuth)

The browser-based desktop OAuth2 flow: click **Sign in with Google…**, consent once in the browser window that opens, done — the app receives an access token plus a refresh token and authenticates IMAP via SASL `XOAUTH2`. No app password needed, and every sync refreshes a fresh short-lived access token from the stored refresh token.

**You register the OAuth client yourself** (one-time, free):

1. In [Google Cloud Console](https://console.cloud.google.com/apis/credentials) create an **OAuth client ID** of type **Desktop app** (any project works; no publishing needed for personal use).
2. Add `http://127.0.0.1` as an **Authorized redirect URI** (the app redirects the browser to `http://127.0.0.1:<random port>` — the port is ignored for loopback URIs).
3. Copy the **client ID** and **client secret** into the environment before starting the app:

   | Variable | Meaning |
   | --- | --- |
   | `MAILCHAT_GMAIL_CLIENT_ID` | your OAuth client ID (`…apps.googleusercontent.com`) |
   | `MAILCHAT_GMAIL_CLIENT_SECRET` | your client secret (`GOCSPX-…`) |

   Both must be set (or both unset — then the Google sign-in UI says it is not configured). The scope requested is `https://mail.google.com/` (the scope Gmail's IMAP endpoint requires); the app itself only ever issues read-only IMAP commands (`EXAMINE`, `UID SEARCH`, `UID FETCH`).
4. In the app: choose **Google sign-in (OAuth)**, enter your address, sign in once, then **Sync** like with any other account.

### Gmail (app password)

1. Enable 2-step verification on the Google account, then create an **App password** (Google Account → Security → 2-Step Verification → App passwords).
2. In the app: host `imap.gmail.com`, port `993`, username `you@gmail.com`, the app password, folders e.g. `INBOX` (comma-separated; empty means `INBOX`).
3. Click **Sync**. The first sync fetches the whole folder; later syncs only new mail.

### Other servers

Any IMAP server with implicit TLS works the same way via the app-password path: host, port (993), username, password, folders.

### Where credentials live

- **Typed passwords** are used for the sync and then stored in the **OS keyring** when one is available (Secret Service/GNOME Keyring or KWallet on Linux, Keychain on macOS, Credential Manager on Windows) — service `mailchat-vault`, entry `user@host`.
- **The OAuth refresh token** from a Google sign-in lives in the same OS keyring under the entry `oauth:user@host`. Short-lived access tokens exist only in process memory for the duration of a sync.
- With no OS keyring reachable, the password or refresh token is kept **only in process memory for the current app session** (gone on exit) — the UI says so.
- Secrets are never written to the vault database (tests byte-scan the DB files for the password *and* the refresh token to enforce this) and never logged. `ImapConfig`'s `Debug` output is redacted too.
- Leaving the password field empty reuses the stored password; to remove it entirely, delete the entry in your OS keyring manager. To redo a Google sign-in, sign in again (it replaces the stored refresh token).

## Export

The **Export results…** control writes the current search results — every row matching the active text + source/author/date filters, across all pages, in the order the UI lists them — to a file you choose. **Export chat…** (in the message detail panel) writes the whole chat thread of the open message, oldest → newest; a message with no thread exports as a single row. Formats:

- **CSV** — RFC 4180 (`source,external_id,channel,chat_id,author,sent_at,body,raw_json`), UTF-8, fields quoted only when needed; cells beginning with `=`, `+`, `-` or `@` get an apostrophe prefix so spreadsheet apps cannot execute them as formulas.
- **JSON Lines** — one complete message object per line (`.jsonl`); the exact shape the app stores. Export-only for now: no app importer accepts this file yet.
- **MBOX** — classic `From `-separated mailbox rebuilt from the stored row plus its recorded headers (`Message-ID`, `From`, `To`, `Subject`, `Date`); carries email-channel rows only, chat rows are skipped and reported in the status line.

Rows stream straight to the file, so exports of very large result sets don't hold the whole set in memory.

## Tests

```sh
cargo test               # vault-core: storage, migrations, dedup, FTS search, mbox / WhatsApp / Telegram / Discord ingest, bounded streaming ingest (memory budget, batched commits, cancellation + restart recovery, issue-record idempotence), media extraction (real small fixture files), object store (M4.1: interrupted-write proofs at all three lifecycle points, hash dedup across names/extensions, capacity refusal with refund, reconciliation clean after a clean import and classifying every damaged state, disk-usage report (referenced/orphaned split, per-extension and largest-object breakdowns)), import bounds (M4.1: traversal/symlink refusals, hidden-dir/symlink-skipping walks, hand-crafted hostile ZIP fixtures — traversal, symlink entries, declared-size bombs — rejected with nothing kept, real WhatsApp/Telegram .zip imports end to end), filtered/paged search, chat context, archive statistics (incl. the 1M-message dashboard perf contract), 100k FTS perf fixture, IMAP sync (mock server; durable source instances, OAuth2 login flow (mock OAuth server + mock IMAP, XOAUTH2 end-to-end), multi-folder occurrences, conflicting revisions, atomic fetch-batch commits with mid-batch fault rollback, missing-fetch + unparseable failed-item ledger accounting incl. retry/resolution and reset clearing, durable-across-reopen), jobs (M2.1 pool: search-during-writes concurrency, bounded-queue backpressure, two-reader concurrency, typed job lifecycle/cancellation/progress, event-seq polling recovery incl. ring trimming, IMAP network-stall-does-not-block-writes proof), IMAP coordinator (M2.3: rounds over the real pool — exclusion skips and re-include, no-credentials reporting, interruption mid-fetch leaves checkpoint and archive in agreement, mid-batch storage fault rolls back the whole batch, checkpoint recovery across a pool restart, missing fetch heals next round, one unreachable source fails alone, search + write complete while a round is parked on a stalled network), periodic sync scheduler (M2.4: opt-in rounds as plain imap_sync_all jobs, a round outlasting the interval neither overlaps nor queues, a clock-jump gap collapses to exactly one catch-up round, disable cancels the active round, persisted schedule re-arms one interval out, failed rounds keep the cadence), LLM search pipeline (mock LLM, no network), backup/restore (M4.4: container round-trip incl. unlinked registry objects and settings, WAL-snapshot consistency, failure at every backup boundary leaves no destination and an untouched vault, tampered/damaged/newer-version containers refused, preview→commit invalidation on vault or staged-bytes change, interrupted switches at every journal boundary recovered to one verified vault with the previous vault kept, return-to-previous and explicit forget), settings model (file round-trip, provider pairing rules, menu building), local embeddings (deterministic embedder, incremental job, cosine scan, hybrid ranking, 100k vector perf budgets), folder watching (real temp folders, through the pool), export (CSV/JSON/MBOX round-trips incl. unicode + large sets), integrity suite (corruption & crash recovery, export/import round-trips, VACUUM/ANALYZE, schema migrations, safe migration with injected before/within/after-conversion failures: snapshot kept readable, original restored byte-for-byte, durable failure-marker refusal on later opens, capacity preflight, newer-schema refusal)

npm test                 # frontend unit + UI-state snapshot tests (vitest), incl. settings helpers, the provider-menu / settings wiring, the background-jobs protocol client (cursor tracking, gap detection, trimmed reconciliation, panel rendering), and the auto-sync toggle wiring (status text, scheduler-status events, interval changes), the new-mail notification layer (digests over imap_sync/imap_sync_all/watch results, focus/permission/enable gates, replay dedup), plus the M5 ask consent + evidence wiring (prompt/re-ask/keep-local/revoke, session-scoped reset on a fresh boot, clickable citations and source rows opening the original message in thread context), and the M4.3 media display (mcmedia URL building/validation, inline image/audio/video DOM with no autoplay, the Settings media-store usage report render), and the M4.4 backup/restore view helpers (manifest/preview/restore/recovery summary lines, binary sizes, restore-flow and previous-vault slot rendering, commit gating)
npm run check            # TypeScript type-check
cargo test --workspace   # also compiles the Tauri app + its LLM provider registry, job-dispatch, M5 consent-gate tests (a local TCP recorder as the mock provider endpoint: zero requests without consent, translate+summarize with; needs the Linux libs above), and M4.3 media-protocol tests (name validation, byte ranges, passive-only content types)
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

IMAP sync is tested hermetically against an in-process mock IMAP4rev1 server (the greenmail equivalent), including an integration test that ingests the same `.mbox` fixture through both paths and compares the archives. The OAuth2 login (Google sign-in → XOAUTH2) is tested the same way: an in-process mock OAuth server (`tests/oauth_mock`) plays the authorization + token endpoints (PKCE, single-use codes, refresh grants, consent denial), an HTTP client stands in for the browser following the real loopback redirect, and the mock IMAP server verifies the SASL `XOAUTH2` string — the whole token flow end-to-end, plus the no-persist scan for the refresh token. A real-server test (`tests/imap_gmail_manual.rs`) is `#[ignore]`d: it needs live credentials and is **not** run in CI. To run it against your own account:

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
states, filters, flat results, pagination, the stale-response guard, the
detail view, keyboard navigation, and the export controls with a mocked save
dialog). Export e2e is not driven through the real OS save dialog for the
same webview reason; the formats themselves are round-trip tested in
`crates/vault-core/tests/export.rs`.

## Out of scope for now (separate tasks)

push notifications. The media-extraction layer is source-generic: when further chat sources land, their exports plug into the same hash-named `media/` store and `raw_json` convention.

## Planning & implementation docs

The [docs/ index](docs/README.md) holds the specification set: project assessment (implemented state and gaps), product plan, UX specification, architecture, data & API contracts (identity, time, migration — grounded in the implemented M1 slice), the implementation roadmap with milestone slices and completion records, the validation/release matrix, and architecture decisions.

## Installers & releases

Pushing a version tag (`v*`) builds distributable installers — Linux AppImage + `.deb` (defaults of the enabled Tauri bundling), plus macOS `.dmg` and Windows `.msi`/NSIS `.exe` from the release workflow — as a **draft GitHub Release** (repo workflows are currently disabled by decision — the local build/publish route in [docs/release.md](docs/release.md) is the operative path until they are re-enabled). Artifacts are **unsigned** in this round (no code signing; Gatekeeper/SmartScreen caveats apply on macOS/Windows — exact click paths in [docs/release.md](docs/release.md)); every release carries a `SHA256SUMS` checksum asset, and a release is not complete without one. The Windows side has an automated install/launch/uninstall **+ upgrade-from-previous-release** smoke workflow (`tests/windows-smoke/`), dispatched manually before publishing a draft. The macOS dmg is smoke-tested in CI via `scripts/dmg-smoke-test.sh` (mount, copy the app, run `--version`/`--help`), with a published-artifact gate (`scripts/release/smoke-macos.sh`). The app ships with a full icon set (Linux/mac/win) generated from `src-tauri/icons/source.png`. See [docs/release.md](docs/release.md) for the install smoke-test checklist, local build commands, checksum verification, and how to regenerate the icons.

## Notes on chat import details

- **WhatsApp**: export dates carry no UTC offset, so they are interpreted DST-aware in the machine's IANA zone and stored with the resolved UTC instant plus timezone/confidence provenance (a stamp inside a DST gap stays unresolved with the verbatim stamp kept); day-first vs month-first ordering is inferred from the file's own dates (day-first when ambiguous). Media placeholders (`image omitted`, `(file attached)`, …) are kept as text and recorded in `raw_json`; when the exported files sit next to the `.txt` ("Attach Media" export, unzipped), named attachments are matched by file name — case-insensitive full name first, then by stem (for export flavors that re-encode extensions, e.g. `IMG-1.jpg` shipped as `IMG-1.jpeg`) — and copied into the media store, linked from `raw_json` (`media` array). `<Media omitted>` placeholders carry no name and stay placeholders.
- **Telegram**: `date_unixtime` is preferred for timestamps (timezone-correct); service entries (calls, joins) are skipped; the original message JSON (attachments, reply references included) is kept verbatim in `raw_json`, and `photo`/`file` paths are resolved inside the export directory (`..` and symlink escapes are rejected), stored hash-named, and linked via the added `media` array.
- **Signal**: conversation exports are top-level JSON arrays of Signal Desktop's raw message objects (as written by `sigtop export-messages -f json`, one file per conversation). Timestamps (`sent_at`/`timestamp`/`received_at`) are absolute epoch milliseconds, stored as exact UTC; outgoing messages are archived with author `You`; service entries (call history, key changes, profile changes, group changes) are skipped; message ids are namespaced by the conversation uuid, so a re-export under a renamed file dedups instead of duplicating; the whole original message object (attachments, quotes, reactions) is kept verbatim in `raw_json` — media extraction is not implemented for Signal yet.
- **Discord**: the Data Package (Settings → Privacy & Safety → Request Data) stores one `messages.json` per channel under `messages/`; pick the package root, the `messages/` directory, or any single channel file. Timestamps carry an explicit UTC offset and are normalized to UTC; `chat_id` is `guild/channel` for server channels and the `index.json` path (e.g. `Direct Messages/@friend`) for DMs; system entries (joins, pins, calls) are skipped while replies are kept; the original message JSON (reactions, attachment info, edits, mentions included) is kept verbatim in `raw_json`. Message ids are snowflakes namespaced by channel id, so re-importing a renamed channel never duplicates.

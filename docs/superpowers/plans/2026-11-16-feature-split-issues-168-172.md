# Feature issues #168–#172: split into implementable hardened tickets

> **For agentic workers:** This document is a **triage**, not an implementation. It splits five open feature issues into sequential tickets of at most ~40 minutes of agent work each: [#168 saved searches](https://github.com/acailic/mailchat-vault/issues/168), [#169 collections/tags/notes](https://github.com/acailic/mailchat-vault/issues/169), [#170 unified person view](https://github.com/acailic/mailchat-vault/issues/170), [#171 PDF/image full-text search](https://github.com/acailic/mailchat-vault/issues/171), [#172 archive health overview](https://github.com/acailic/mailchat-vault/issues/172). A follow-up task picks up **exactly one ticket**; each ticket names its goal, exact files/seams, a scoped test command, schema needs, and risks/deferrals. Analysis only — no code accompanies this plan.

**Baseline:** `main` at `83557eb`, schema version 17, all seams verified against the tree on 2026-11-16.

## Architecture seams verified on `main` (shared by every ticket)

- **Schema migrations:** append-only `MIGRATIONS: &[(i64, &str)]` at `crates/vault-core/src/storage.rs:92`, currently ending at version 17 (the `idx_archive_messages_source` drop). Rust backfill halves go in `RUST_MIGRATIONS` (`storage.rs:766`, precedent: migration 7 conversation-key backfill). Never edit an applied version; a new table is one appended migration. Migrations run inside the snapshot/rollback safe-migration path (`Storage::open`, `storage.rs:812`).
- **Backup/restore covers DB tables by construction:** `backup::snapshot_for_backup` (`crates/vault-core/src/backup.rs:434`) snapshots the whole database via the SQLite backup API plus media objects and the settings file. Metadata stored in vault tables therefore survives backup/restore with **no backup code changes** — each feature only needs a round-trip test assertion in `crates/vault-core/tests/backup.rs`.
- **Sample-archive isolation:** the demo vault is a private in-memory database (`demo_enter`, `src-tauri/src/lib.rs:2034`), and personal-vault mutation commands call `state.refuse_in_demo()?` (`src-tauri/src/lib.rs:282-289`). Every new mutation command below reuses that guard; isolation of sample vs. personal metadata then holds by construction.
- **v2 search request model:** `SearchArchiveRequest` (`crates/vault-core/src/search2.rs:92`) already carries every filter #168 must persist (text, `source_ids`, `conversation_id`, `import_job_id`, author, day bounds + explicit `timezone`, `has_attachment`/`time_assumed`/`grouping_unverified`/`files_missing`, `sort`) with a `normalize()` validator (`search2.rs:159`) that rejects invalid input loudly.
- **Canonical message model:** `archive_messages` (content-addressed by `content_hash`, `storage.rs:208`), `message_identities` (`(source_instance_id, namespace, external_key)` → `message_id`, `storage.rs:243`), `conversations` with confidence labels, `source_instances` with canonical UUID ids. Dedup on reimport preserves row ids — the anchor for #169/#170 metadata survival.
- **Job pool:** `jobs::DbPool` (`crates/vault-core/src/jobs.rs:588`) with `submit_job`, `progress`, `is_cancelled` (`jobs.rs:1138`), and the durable `job_ledger` (migration 16, `storage.rs:732`) that records every terminal job state across restarts. Long-running work (#171) goes through this; #172 reads the ledger.
- **Source health records already exist:** `source_instances.last_attempt_at` / `last_success_at` (migration, `storage.rs:529-530`, stamped separately by `record_sync_attempt`/`record_source_success`, `storage.rs:1233-1252`), `last_error_class`/`last_error`/`last_error_at` (migration 14), plus `sync_failed_items` (`storage.rs:548`), `import_issues` (`storage.rs:583`), `watched_folders` (`storage.rs:394`), and backup evidence (`backup::LastBackupRecord` / `recovery_status`, `backup.rs:224`/`backup.rs:1891`).
- **Media store:** content-addressed `media_objects` keyed by `sha256` (`storage.rs:661`), `ObjectStore` (`crates/vault-core/src/object_store.rs`), attachment references carried in `archive_messages.raw_json` with digest-recoverable paths (`AttachmentSummary.path` = `media/<sha256>.<ext>`, `search2.rs:344`). FTS is `archive_fts` (content-table-bound FTS5, `storage.rs:347`).
- **IPC surface:** 57 `#[tauri::command]`s in `src-tauri/src/lib.rs`, registered in `generate_handler!` at `lib.rs:3633`; typed TS wrappers in `src/ipc.ts`, type mirrors in `src/v2types.ts`/`src/types.ts`; the contract doc `docs/data-and-api-contracts.md` has a "v2 IPC surface" section that must be updated in the same change as any new command.
- **Frontend shell:** `WorkspaceView` = `"workspace" | "sources" | "activity" | "settings"` with per-view sections (`src/state.ts:81-100`); the shell/DOM contract lives in `src/workspace.ts`; panel logic stays in per-module files driven from `src/main.ts`; static panel DOM in `index.html`. Tests are co-located vitest (`src/*.test.ts`, happy-dom) — `npm test`, `npm run check` (tsc).
- **Standard gates per ticket** (repo convention, `docs/testing.md`): `cargo test -p vault-core` green, `cargo fmt --check`, `cargo clippy -p vault-core --all-targets -- -D warnings`, plus `npm run check` and `npx vitest run` for UI tickets. The root `Cargo.toml` limits `cargo test` to `vault-core` (the Tauri crate needs WebKitGTK), so `src-tauri` changes compile-verify only where prerequisites exist; **e2e (`tests/e2e/run.mjs`) cannot run in restricted runners** — every e2e acceptance item below is recorded as a follow-up in the ticket that owns it.

**Global constraint — migration numbering:** all five features append to the single `MIGRATIONS` array in `storage.rs`. Schema tickets landing in parallel must coordinate version numbers (rebase and renumber the loser). The wave plan below serializes schema-bearing tickets to avoid this; doc-only and UI-only tickets parallelize freely.

---

## #168 — Saved searches (P2, "start independently")

**Verdict: 3 sequential tickets. Smallest end-to-end surface; recommended first.** The entire persisted filter semantics already exist as `SearchArchiveRequest`; the feature is a CRUD table + panel around it.

### Ticket 168-A: schema + storage CRUD + validation (core)
- **Files:** `crates/vault-core/src/storage.rs` (append migration 18: `saved_searches (id INTEGER PRIMARY KEY, name TEXT NOT NULL, request_json TEXT NOT NULL, created_at, updated_at)`; new `Storage` methods `saved_search_save/list/rename/update/delete`); new `crates/vault-core/src/saved_searches.rs` (thin module: (de)serialize a `SearchArchiveRequest` minus `limit`/`offset`, re-validate through `SearchArchiveRequest::normalize`, resolve `source_ids` against `source_instances` and return an explicit "unavailable source" list instead of dropping the filter — the AC's "must not silently broaden" is enforced here, not in the UI); export from `crates/vault-core/src/lib.rs`.
- **Tests:** new `crates/vault-core/tests/saved_searches.rs` — save/reopen/rename/update/delete round-trip; reopen re-executes and finds newly inserted matching messages (dynamic view, not frozen ids); unavailable `source_ids` flagged; add one backup/restore round-trip assertion to `crates/vault-core/tests/backup.rs` (table survives `snapshot_for_backup`→restore).
- **Command:** `cargo test -p vault-core --test saved_searches` (+ the backup test file).
- **Migration:** one append (v18).
- **Risks/defer:** none significant. Relative dates and notifications are explicit follow-ups in the issue — keep out.

### Ticket 168-B: IPC + panel (save/reopen/rename/delete)
- **Files:** `src-tauri/src/lib.rs` (5 commands with `refuse_in_demo()?`, registered in `generate_handler!` at `lib.rs:3633`); `src/ipc.ts` wrappers; `src/v2types.ts` type mirror; new `src/savedsearches.ts` (panel module: list UI, save-current-search, apply-to-controls) + `index.html` panel section + wiring in `src/main.ts`; map between `src/searchstate.ts` `SearchFilters` and the persisted request (serialize on save, restore filter controls on reopen, then execute page 1).
- **Tests:** `src/savedsearches.test.ts` (happy-dom): keyboard navigation, empty state, save/restore error surfacing; `src/main.test.ts` extension: reopening restores visible filter state and re-executes.
- **Command:** `npx vitest run src/savedsearches.test.ts src/main.test.ts && npm run check`.
- **Migration:** none.
- **Risks/defer:** `main.ts` is 3.5k lines — keep the panel in its own module, wired minimally. Defer the restart+reimport e2e (AC last item) to an e2e-bearing follow-up.

### Ticket 168-C: honesty hardening + docs
- **Files:** `src/savedsearches.ts` (unavailable-source/import banner: restore refuses to broaden, shows what is missing); `docs/features.md` and `docs/data-and-api-contracts.md` (new commands + table); `crates/vault-core/tests/saved_searches.rs` edge cases (duplicate names, empty name, deleted source).
- **Command:** `cargo test -p vault-core --test saved_searches && npx vitest run && npm run check`.
- **Migration:** none.

---

## #169 — Cross-source collections, tags, notes (P2, "alongside saved searches")

**Verdict: 4 sequential tickets. Largest metadata surface, but the canonical model makes most ACs cheap:** memberships hang off `archive_messages.id`, and reimport dedup preserves ids, so "reimporting preserves memberships without duplicates" is inherited from ingest.

### Ticket 169-A: schema + core CRUD (organize module)
- **Files:** `storage.rs` append migration 19: `collections (id, name UNIQUE, note TEXT NOT NULL DEFAULT '', created_at, updated_at)`, `collection_members (collection_id REFERENCES collections ON DELETE CASCADE, message_id REFERENCES archive_messages ON DELETE CASCADE, added_at, UNIQUE(collection_id, message_id))`, `tags (id, name UNIQUE)`, `message_tags (tag_id REFERENCES tags ON DELETE CASCADE, message_id REFERENCES archive_messages ON DELETE CASCADE, UNIQUE(tag_id, message_id))`, `message_notes (message_id INTEGER PRIMARY KEY REFERENCES archive_messages ON DELETE CASCADE, note TEXT NOT NULL, updated_at)` — every FK cascades so archive deletion honestly removes metadata and metadata deletion can never touch the archive. New `crates/vault-core/src/organize.rs`: create/rename/delete collections, add/remove members by canonical id, tag CRUD + apply/remove (rename preserves assignments by id), note get/set/delete. Export from `lib.rs`.
- **Tests:** new `crates/vault-core/tests/organize.rs` — message in multiple collections without content duplication; rename tag keeps assignments; reimport same export (existing ingest path) keeps memberships/tags/notes; delete collection removes only membership rows; backup/restore round-trip assertion in `tests/backup.rs`.
- **Command:** `cargo test -p vault-core --test organize`.
- **Migration:** one append (v19).
- **Risks/defer:** confirm no current migration path ever DELETEs+reinserts canonical rows (it does not — ingest is insert-if-absent keyed on identities; record this invariant in the module doc). Defer nested collections, sharing, AI classification.

### Ticket 169-B: tag + collection filters in search
- **Files:** `search2.rs` — extend `SearchArchiveRequest` with optional `tag_id`/`collection_id` (or names) and plumb through `normalize()`/`search_archive` (`search2.rs:1243`) as id-list pre-filters (bounded IN clauses); `src/v2types.ts` + `src/searchstate.ts` filter model; chips in the filter UI (`src/render2.ts`/`src/main.ts` wiring).
- **Tests:** `crates/vault-core/tests/organize.rs` (filter integration block) or extend `tests/search_page.rs`; `src/searchstate.test.ts`.
- **Command:** `cargo test -p vault-core --test organize --test search_page && npx vitest run src/searchstate.test.ts`.
- **Migration:** none.
- **Risks/defer:** keep the filter pre-filter bounded (cap IN-list size like existing clamps).

### Ticket 169-C: selection/reading-pane actions + collection view
- **Files:** `src-tauri/src/lib.rs` (`organize_*` commands, `refuse_in_demo`); `src/ipc.ts`; `src/organizeview.ts` (new panel: collections list, collection view with provenance per row and open-original via existing `conversationContextV2`/`messageDetailV2`); selection actions in the results list (`src/render2.ts`) and reading pane; `index.html`; `src/state.ts` (extend `WorkspaceView` or add a Sources section — prefer a new `"collections"` section inside the existing workspace/sources pattern to avoid shell churn); `src/main.ts` wiring.
- **Tests:** `src/organizeview.test.ts` + `src/main.test.ts` selection-action coverage.
- **Command:** `npx vitest run src/organizeview.test.ts src/main.test.ts && npm run check`.
- **Migration:** none.
- **Risks/defer:** collection view navigation back to original conversation must reuse the existing context APIs, not new queries.

### Ticket 169-D: notes UI + inert rendering + docs
- **Files:** `src/organizeview.ts`/reading-pane note editor; **render notes as inert text only** (textContent, never innerHTML — notes are user input rendered next to source evidence); `docs/features.md`, `docs/data-and-api-contracts.md`.
- **Tests:** `src/organizeview.test.ts` — note visibly distinguished, reimport-safe, inert rendering (script text appears literally).
- **Command:** `npx vitest run src/organizeview.test.ts && npm run check`.
- **Migration:** none.

---

## #170 — Unified person view (P2, independent; contract-first)

**Verdict: 3 sequential tickets, after #169-A only for calendar reasons (both append migrations); technically independent.** The issue itself demands identity semantics be documented in the domain/data contracts **before** implementation — that is ticket 1.

### Ticket 170-A: contracts + schema + link/unlink core
- **Files:** `docs/data-and-api-contracts.md` (extend "Identity and deduplication" with user-confirmed person associations: separate from immutable imported metadata, anchored on stable keys only, never display names) and `docs/decisions.md` if a new decision entry is warranted (D15 covers canonical identity — reference, don't rewrite); `storage.rs` append migration 20: `persons (id, name, created_at, updated_at)`, `person_identities (person_id REFERENCES persons ON DELETE CASCADE, source_instance_id, namespace, external_key, created_at, UNIQUE (source_instance_id, namespace, external_key))` — the UNIQUE makes double-assignment impossible at the storage layer; new `crates/vault-core/src/persons.rs`: person CRUD, link/unlink with **explicit conflict errors** (identity claimed by another person → error naming both, never silent move), preview query (per-identity message counts + samples via `message_identities` join). Anchor rules: email addresses and provider ids (e.g. Telegram/Signal `<chat-id>/` prefixes, `namespace` values from `message_identities`) are linkable; name-only authors are marked ambiguous and source-scoped, never linkable (matches AC "two unrelated people with the same display name are never automatically linked").
- **Tests:** new `crates/vault-core/tests/persons.rs` — same display name on different sources never auto-links; one person with aliases across sources; conflicting assignment rejected with explanation; unlink reverses (messages unchanged); delete person removes associations only; backup/restore round-trip in `tests/backup.rs`.
- **Command:** `cargo test -p vault-core --test persons`.
- **Migration:** one append (v20).
- **Risks/defer:** chat authors without stable external keys cannot be honestly linked — surface them in the picker as "ambiguous, source-scoped" and stop there. Defer automatic inference, contact-provider upload, conversation merging.

### Ticket 170-B: person timeline query
- **Files:** `persons.rs` — `person_timeline(person_id, filters, page)`: deterministic pagination over `archive_messages` joined through `message_identities` on the person's identity set, source/date filters, each row carrying the original author string/source/time-confidence label (no rewriting); future imports under an already-linked identity appear automatically (join, not snapshot).
- **Tests:** extend `tests/persons.rs` — pagination determinism, new ingest after linking appears, filter isolation between sources.
- **Command:** `cargo test -p vault-core --test persons`.
- **Migration:** none.

### Ticket 170-C: IPC + linking flow + person view UI
- **Files:** `src-tauri/src/lib.rs` (`persons_*` commands, `refuse_in_demo`); `src/ipc.ts`/`src/v2types.ts`; `src/personsview.ts` (create/rename person, identity picker with **preview of identities + affected counts/samples before confirm**, link/unlink with conflict explanation UI, chronological person view with filters and open-original links); `index.html`; `src/state.ts`/`src/main.ts` wiring.
- **Tests:** `src/personsview.test.ts` — preview-before-confirm, conflict dialog, unlink restores prior view.
- **Command:** `npx vitest run src/personsview.test.ts && npm run check`.
- **Migration:** none.

---

## #171 — PDF/image full-text search with OCR (largest risk; native deps are decision-gated)

**Verdict: 3 sequential tickets + 1 explicitly gated stage. Do not bundle with the other features.** `Cargo.toml` has **no PDF or OCR dependency today**, the issue requires engine/licensing/language-pack/distribution choices documented *before* native dependencies, GPL-3.0-only compatibility constrains engine choice, and repo policy forbids implicit runtime model downloads. Ticket 1 is deliberately dependency-free.

### Ticket 171-A: decision record + derived-text schema + job skeleton (no native deps)
- **Files:** `docs/decisions.md` (new decision entry: extraction/OCR engine candidates, licensing vs GPL-3.0-only, language-pack distribution, per-OS packaging implications, opt-in default-off); `storage.rs` append migration 21: `attachment_text (sha256 TEXT PRIMARY KEY REFERENCES media_objects (sha256) ON DELETE CASCADE, status TEXT NOT NULL CHECK (status IN ('pending','ok','failed','corrupt','encrypted','unsupported','budget_exceeded','cancelled')), text TEXT NOT NULL DEFAULT '', extractor TEXT NOT NULL DEFAULT '', extractor_version TEXT NOT NULL DEFAULT '', lang TEXT NOT NULL DEFAULT '', pages INTEGER, bounds_json TEXT NOT NULL DEFAULT '{}', extracted_at TEXT)` and contentless FTS5 `attachment_fts (text, content='')` keyed by `attachment_text` rowid, plus a small derived mapping `attachment_refs (message_id REFERENCES archive_messages ON DELETE CASCADE, sha256, name)` backfilled from `raw_json` attachment paths (digest is recoverable from `media/<sha256>.<ext>` paths) — this table is what makes scope isolation (#171 AC) and #165 file-browsing joins cheap; new `crates/vault-core/src/attach_index.rs` skeleton: status model, budget config (size/pages/pixels/time/concurrency via `bounded.rs` patterns), job submission through `jobs::DbPool` with cancel + per-file isolation (one failure never stops others), settings toggle scaffold in `settings.rs` `Options` (`#[serde(default)]` default-off, the established pattern).
- **Tests:** new `crates/vault-core/tests/attachment_index.rs` — migration + backfill of `attachment_refs` from fixtures with preserved media; status transitions; cancel; duplicate object (same digest, two references) extracted once; remove/rebuild derived data leaves `media_objects` untouched.
- **Command:** `cargo test -p vault-core --test attachment_index`.
- **Migration:** one append (v21).
- **Risks/defer:** if the engine decision is not converging in-ticket, ship only the schema/skeleton and record the blocker — nothing else in #171 can proceed without it.

### Ticket 171-B: text-PDF extraction + search integration
- **Files:** `Cargo.toml` (the chosen **pure-Rust, GPL-compatible** text-extraction crate per 171-A decision); `attach_index.rs` (extract → store derived text labelled with extractor/version → index into `attachment_fts`, digest-deduped); `search2.rs` — expose attachment hits: extend the page with attachment matches (filename, snippet, page when available, message link) constrained by the request's source/date/import scope through `attachment_refs`; `src-tauri/src/lib.rs` (index job commands + settings toggle); `src/v2types.ts`/`src/render2.ts` (attachment-hit rows with "derived/OCR may be inaccurate" labeling from the start).
- **Tests:** extend `tests/attachment_index.rs` — synthetic tiny text-PDF fixtures (generated in-test, never real documents), Unicode, scope isolation (no hits leaking from excluded sources), term-only-in-PDF found with message context; `src/render2.test.ts` hit rows.
- **Command:** `cargo test -p vault-core --test attachment_index && npx vitest run src/render2.test.ts && npm run check`.
- **Migration:** none beyond 171-A.
- **Risks/defer:** office formats, handwriting, transcription are follow-ups per the issue. Rotated-image handling belongs to OCR (171-C), not here.

### Ticket 171-C: OCR stage (gated on 171-A decision) + packaged verification
- **Files:** per decision (engine crate or bundled-binary approach + language packs documented in `docs/decisions.md`); `attach_index.rs` OCR path reusing the same job/status/budget path; `docs/native-verification.md` templates for per-OS engine/language availability checks (Linux/macOS/Windows).
- **Tests:** extend `tests/attachment_index.rs` — scanned-PDF and image fixtures, rotated images, budget/cancel states; packaged tests are per-OS and **cannot run in this runner** — record as release-verification follow-ups (`docs/validation-and-release.md`).
- **Command:** `cargo test -p vault-core --test attachment_index`.
- **Risks/defer:** if engine distribution can't satisfy the no-implicit-download policy per-OS, defer 171-C entirely and keep 171-B's text-PDF value — the issue explicitly stages delivery this way.

---

## #172 — Archive health overview (P2, read-only composition; cheapest high value)

**Verdict: 2 sequential tickets. No new sync/backup logic — a read model over records that already exist** (`last_attempt_at`/`last_success_at` are already separate columns, so "a failed attempt never overwrites last success" is an existing invariant to assert, not build).

### Ticket 172-A: core read model
- **Files:** new `crates/vault-core/src/health.rs` — `ArchiveHealth` view composing, per `source_instances` row: last success, last attempt, latest failure/partial outcome (`last_error_*`, `job_ledger` newest terminal row per kind, `import_issues`, `sync_failed_items`), paused/disconnected state, manual-import classification (file imports: last import snapshot + explicit "external account cannot be checked" wording); media integrity block from existing reconciliation/findings (`media.rs`, `tests/media_findings.rs` surfaces) with verification timestamp/coverage; backup block from `backup::last_backup_record`/`recovery_status` distinguishing never-backed-up / failed / not-checked / verified-with-time. All queries bounded (LIMIT + existing indexes; `idx_archive_messages_source_channel` covers per-source probes). Unknown/not-checked are first-class enum values, never coerced to healthy.
- **Tests:** new `crates/vault-core/tests/health_overview.rs` — exactly the AC matrix: healthy source, failed sync (last success preserved), paused account, partial import, unchecked/damaged media, absent and stale backup evidence; restart survival (reopen storage).
- **Command:** `cargo test -p vault-core --test health_overview`.
- **Migration:** none expected; if media verification turns out not to persist a checked-at timestamp anywhere, add one small append migration for it inside this ticket and say so in the PR.
- **Risks/defer:** drill-downs to #165/#166/#167 surfaces are links-only once those land; no new schedules/telemetry/repair (issue boundary).

### Ticket 172-B: IPC + overview panel
- **Files:** `src-tauri/src/lib.rs` (`health_overview` command); `src/ipc.ts`; `src/healthview.ts` (summary + per-source details, distinct last-attempt/last-success/last-verified/unknown states, links to existing source/job/media/backup panels — no duplicated workflows); `index.html`; `src/state.ts` (extend the Sources view sections) + `src/main.ts` wiring; refresh after job completion via the existing `events_since` job-event stream (`jobs.rs:829`).
- **Tests:** `src/healthview.test.ts` — state distinction rendering, links, refresh on job event, bounded rendering with many sources.
- **Command:** `npx vitest run src/healthview.test.ts && npm run check`.
- **Migration:** none.

---

## Prioritized order and parallelism

**Recommended feature order: #168 → #172 → #169 → #170 → #171.**

Rationale: #168 is the smallest self-contained slice with the highest daily-use value (issue: "recommended early product improvement"); #172 ships a trust screen from existing records with near-zero schema risk; #169 builds the organization substrate #170's UI patterns will reuse; #170 is contract-gated but otherwise independent; #171 is decision-gated, adds native dependencies, and its first ticket is documentation — it can start anytime but must not block the others.

**Wave plan (what can actually run in parallel):**

| Wave | Tickets | Parallel-safe because |
| --- | --- | --- |
| 1 | 168-A ∥ 172-A ∥ 171-A | 168-A and 172-A touch disjoint files (172-A is a new module + new test; no migration expected); 171-A is mostly a decision record. **168-A and 171-A both append migrations — if they land together, one rebases and renumbers (18/19 reserved in landing order).** If coordination is unwanted, run 168-A → 172-A ∥ 171-A. |
| 2 | 168-B ∥ 172-B ∥ 169-A ∥ 171-B* | All UI panels are disjoint new modules (`savedsearches.ts`, `healthview.ts`, `organizeview.ts`); 169-A appends migration 19 alone in this wave. 171-B gated on 171-A decision. |
| 3 | 168-C ∥ 169-B ∥ 170-A | Docs + one migration each in different areas; 170-A's migration renumbers if it lands after 169-A. |
| 4 | 169-C ∥ 170-B ∥ 171-C* | Disjoint (UI vs. core query vs. gated OCR). |
| 5 | 169-D ∥ 170-C | Final UI + docs slices, disjoint modules. |

Intra-feature tickets stay sequential (A before B before C). The only serialization point in the whole plan is `storage.rs` migration numbering across the five A-tickets — everything else parallelizes by module ownership.

**Explicitly deferred (record in follow-ups, not in these tickets):** relative-date saved searches and saved-search notifications (#168); nested collections/sharing/AI classification (#169); automatic identity inference and contact import (#170); office formats, handwriting, transcription, per-OS packaged engine verification (#171); drill-downs into #165/#166/#167 recovery surfaces (#172); and every e2e restart/reimport acceptance item — the vitest + cargo coverage above is the runner-verifiable substitute, and e2e runs only where WebKitGTK exists.

**Verification status of this analysis:** all file paths, line references, table names, migration mechanics, backup behavior, demo isolation, and job/ledger seams were read from `main` at `83557eb`; issue text fetched via `gh` on 2026-11-16. No code was changed. Cargo/npm suites were not executed — this document adds no executable code.

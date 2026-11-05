# Architecture decisions

Status: **accepted design direction**. D03, D04, D08, D10, and D12 are implemented and verified (D03/D12 with M1, D04/D10 with M2, D08's workspace/search halves with M3.1/M3.2 — 2026-11-05, see the [roadmap's completion records](implementation-roadmap.md#completion-tracking)); D05's object-store half is implemented and verified (M4.1, 2026-11-05 — see its amendment); D09's consent/citation halves are implemented and recorded with M5; the rest remain planning defaults until their slices land. These records describe the agreed direction, not completed code unless the amendment says otherwise. Scope changes belong in the [product plan](product-plan.md); wire/schema details belong in [contracts](data-and-api-contracts.md).

## D01: Personal local archive

Choose a single-user desktop archive with optional remote ingestion and AI. Ordinary reading/search works offline. A unified inbox with sending, shared team permissions, and a hosted knowledge service are deferred because they change the product and trust model. Success is finding and preserving an original exchange, not maximizing connector count.

## D02: Retain the current implementation stack

Keep Tauri, Rust, SQLite/FTS5, TypeScript and Vite. Separate feature controllers, IPC, state, view models, rendering and styles as the UI grows. A frontend framework rewrite or backend service would add migration cost without addressing the demonstrated correctness gaps. Revisit only with measured complexity/performance evidence.

## D03: Separate source, conversation, message, and occurrence identity

Use opaque durable source/conversation IDs and preserve source-specific keys as aliases. Store folder/import occurrences separately from messages. Build email context from reply evidence, never folder/subject alone. Keep conflicting revisions and explicitly unresolved legacy records. Canonical v2 tables avoid the old source-string uniqueness constraint; preserve old IDs and retain legacy tables for recovery during this release. The cost is migration complexity and temporary disk duplication; silent identity loss is less acceptable.

*Amendment (2026-11-05): implemented as M1.1–M1.4 and verified (canonical tables with frozen legacy snapshot, connector-scoped identities with semantic revision hashes, provider conversation keys with a one-hop email reply graph, multi-folder occurrences, explicit unresolved records, and the safe-migration snapshot/rollback path). Discord remains on legacy display grouping; user linking of legacy namespaces is designed but not implemented. Details and evidence: [contracts](data-and-api-contracts.md#identity-and-deduplication), [M1 record](implementation-roadmap.md#completion-tracking).*

## D04: Serialized writes and independent readers

Use one writer worker, two read connections, WAL and bounded producer queues. Network/parsing waits happen outside transactions. This directly addresses the current network-held shared mutex without adding a database server or unbounded pool. Checkpoint and result persistence are one transaction. Concurrency limits and initial batch sizes are owned by [Architecture](architecture.md#jobs-and-database-access), with measured validation before release.

*Amendment (2026-11-05): implemented as M2.1–M2.3 and verified — the `DbPool` writer worker + two read connections with per-connection initialization and bounded write/job queues; job bodies hold no connection (network waits never inside the writer, a lock, or a transaction, proven by stalled-network tests); imports and IMAP fetch batches commit atomically with their checkpoints and issue/failed-item ledgers. Measured validation landed with M2.4 (real-path paginated-search baselines, concurrent-ingest measurements; [BASELINE.md](../tests/perf/BASELINE.md)). Deviations recorded honestly: all background jobs serialize on one runner thread (stricter than proposed), the busy timeout is 5 s, and job records are per-process rather than persisted rows. Details: [Architecture: jobs](architecture.md#jobs-and-database-access), [M2 record](implementation-roadmap.md#completion-tracking).*

## D05: Preserve originals and available media locally

Copy source originals and available regular media into SHA-256-addressed storage; keep metadata/relationships in SQLite. Do not rely on external export paths remaining present. Shared bytes are deduplicated, while message revisions/provenance remain separate. Missing/unmatched files stay explicit. Complete available media preservation increases release work and disk requirements; it was expressly selected instead of a text-only release.

*Amendment (2026-11-05): the object-store half is implemented as M4.1 and verified — SHA-256-addressed `media/` storage with the stage→promote→reference lifecycle (no half-written object ever visible, no dangling reference reachable), byte-exact copies with the original name/kind/size kept on every reference, content dedup across names, messages, and imports, traversal/symlink/capacity-fenced ZIP and directory picks, Telegram/WhatsApp media extraction with missing files counted in the import summary, reimport backfill of media links, and store-vs-archive reconciliation (`media_reconcile`). Still open under this decision: mail MIME attachments and preserved raw message bytes, unlinked source files, per-message missing/failed states (M4.2) — so “missing files stay explicit” is today a summary count, not yet per-reference rows — plus safe open/export surfaces (M4.3) and backup/restore of object bytes (M4.4). Signal and Discord attachments remain `raw_json` records only. Details: [Architecture: originals and media](architecture.md#originals-media-and-object-lifecycle), [contracts: media and capacity](data-and-api-contracts.md#media-and-capacity).*

## D06: Consistent, verified backup and staged restore

Use a versioned ZIP64 backup with a SQLite snapshot, objects, and checksum manifest. Pause writes during backup for the first release to simplify consistency while retaining read access. Validate restore in staging and retain the previous vault through a journaled switch. A live database-file copy and in-place unverified restore are rejected because WAL and interrupted writes can invalidate recovery.

## D07: OS protection with explicit encryption limits

Rely on OS account/disk protection and owner-only permissions where supported. Store credentials in the OS keychain with visible session-memory fallback. Database, objects and backups are not encrypted by the app. App-managed encryption and recovery-key UX are deferred by user choice; no branding or UI text may imply they already exist.

## D08: Archive workspace over competing feature panels

Use source navigation, flat search results, and a reading pane. Setup, activity and maintenance have dedicated views. Keep ranking stable and display human labels before technical metadata. Adapt to narrow windows with back navigation. This preserves familiarity and reading context while avoiding competing Search/Ask/import/server sections on one page. The [UX specification](ux-specification.md) owns breakpoints and keyboard rules.

*Amendment (2026-11-05): the workspace/search halves are implemented as M3.1/M3.2 and recorded — the three-area shell with navigation destinations (All messages / Add source / Activity / Settings) and per-source scope chips, design tokens with light/dark/System themes and the bundled Source Sans 3, wide/medium/compact layouts with a 200%-zoom ≡ compact equivalence, the typed IPC boundary and centralized stale-response guard, and the flat stable v2 ranking (rank/id and instant/id orders, unknown times last) with filter chips, honest unknown-time and unverified-grouping presentation, deep links, and selection/scroll restore. Honest deviations: per-source status text and account pages await M3.4, the import surface keeps its interim pre-preview flow until M3.3, and the v1 render module stays active for unmigrated panels until the M3-exit cleanup. Details and evidence: [UX specification](ux-specification.md#workspace-and-navigation), [M3 progress record](implementation-roadmap.md#completion-tracking).*

## D09: AI is optional and payload approval is explicit

AI defaults off regardless of environment keys. Each question has an explicit translation send action; synthesis requires a preview/approval of the exact bounded context. Bind approval to endpoint, configuration and snapshot. Reuse the existing compatible-provider integration and fallback; attachments and arbitrary tools remain outside the pipeline. This adds one deliberate step but makes network disclosure concrete and keeps retrieval useful without AI.

*Amendment (2026-11-05): the consent/citation halves are implemented with M5 and recorded — AI stays off regardless of environment keys, now enforced in the backend: without an explicit, per-session consent a network provider selection is downgraded to a fully local ask before any provider client exists (zero wire requests without consent, proven by mock-endpoint tests), the first network ask of a session shows a per-question disclosure of exactly what would leave, citations `[n]` and the numbered source rows open the original archived message in thread context through the v2 reading pane, and provider failure leaves local results usable. Honest deviations from this decision's text: consent is session-scoped and in-memory only (no persistent switch — resets every app start), the approval is one per-session grant after the first per-question prompt rather than a per-question translation send plus a separate digest-bound approval of the exact preview, and asks do not yet intersect active filters — those remain the M5.1/M5.2 remainder. Details and evidence: [Architecture: optional AI ask layer](architecture.md#optional-ai-ask-layer), [M5 record](implementation-roadmap.md#completion-tracking).*

## D10: Read-only mail ingestion and opt-in scheduling

Keep IMAP read-only and archive content independent of remote deletion. Default to manual sync; optionally enqueue one run every 15 minutes while open, without missed-run accumulation. Retain Gmail app-password support; OAuth is deferred. Source disconnection stops access/removes secrets but does not delete local history. This is an archive, not a bidirectional mailbox replica.

*Amendment (2026-11-05): implemented as M2.3/M2.4 and verified — read-only `EXAMINE` access with atomic fetch-batch commits and the durable failed-item ledger; the opt-in scheduler persists its schedule in the vault, submits each round as an ordinary `imap_sync_all` job, never overlaps rounds, and collapses sleep/resume/offline gaps into one catch-up round (missed time cannot queue). One deviation grew beyond the original text: the Gmail browser OAuth2 flow (XOAUTH2) shipped alongside app passwords, so the “OAuth deferred” clause is superseded — both paths are supported, chosen in the UI. Details: [Architecture: IMAP correctness](architecture.md#imap-correctness), [M2 record](implementation-roadmap.md#completion-tracking).*

## D11: Three-platform release with honest evidence

Target Linux x64, Windows x64 and macOS arm64/x64 for the first polished release. Native compilation, packaging, credentials, upgrade and restore evidence are mandatory per target. Keep the current Linux automated E2E and add repeatable native checks where drivers differ. Signing/notarization are publication dependencies; unavailable credentials or hardware do not count as passing evidence. Automatic updates remain deferred.

## D12: Dates retain provenance and uncertainty

Store original timestamp text separately from a resolved UTC instant and confidence. Require user confirmation of naive import timezone/date order. Preserve legacy assumptions without silently reinterpreting them; date filters use workspace-local day boundaries. The additional metadata and occasional import question are preferable to presenting a false exact time as evidence.

*Amendment (2026-11-05): implemented as M1.3 and verified (per-message instant + original text + zone + confidence; DST-aware exporter-local resolution with fold/gap handling; migration 7 resolves legacy blind-Z stamps without reinterpreting recorded text; v2 search interprets day bounds in a validated IANA zone). The user-confirmation half (`user_confirmed`) awaits the M3.3 import flow; legacy values keep `legacy_assumed` until then. Details: [time semantics](data-and-api-contracts.md#time-semantics).*

## Changing a decision

Add a dated amendment explaining the trigger, affected user behavior, alternatives, compatibility, validation and migration impact. Mark the old decision superseded and link its replacement. Update the owning specification and roadmap slices together; do not leave conflicting instructions for implementers.

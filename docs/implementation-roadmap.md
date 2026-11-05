# Implementation roadmap

Status: **active**. M1 (identity, provenance, time, safe migration) is implemented and verified; see the [completion record](#completion-tracking). Code for later slices — M2, M3.1–M3.2, M4.1, M5, and M6.3 — has landed on `main` ahead of this document; their completion records remain each slice's responsibility, and their checkboxes stay open until that slice records its evidence. This document specifies delivery slices, not an assertion that work has begun; the implemented state is recorded in [Project assessment](project-assessment.md). Each slice must leave existing local search/import behavior usable and include documentation updates. No date estimate is implied before the implementer validates its dependencies and platform environment.

## Execution rules

- Read the [product requirements](product-plan.md#functional-requirements), [UX contract](ux-specification.md), [data/API contract](data-and-api-contracts.md), and [validation matrix](validation-and-release.md#acceptance-scenarios) before implementing the affected slice.
- Keep changes focused on the named slice. Preserve current test coverage while updating assertions for intentional behavior changes; do not delete tests because the new design exposes failures.
- Implement v2 IPC alongside v1 until callers migrate. Do not leave commands writing different canonical stores. Exercise legacy upgrade paths with disposable fixtures.
- Each completion record includes revision, changed behaviors, tests run, failures/limitations, migration/recovery evidence, and remaining work. Mark checkboxes complete only with that evidence.
- Never test migrations, deletion, restore, or imports against a user's personal vault. Use isolated app data roots and synthetic fixtures.

## M1: Identity, provenance, and safe migration

Addresses P01 and F02/F03/F06. Owns [identity, time, and migration contracts](data-and-api-contracts.md#identity-and-deduplication). Depends on the current baseline only.

| Slice | Concrete deliverable | Completion evidence |
| --- | --- | --- |
| M1.1 | Add canonical source, conversation, message, identity-alias, occurrence, and time-provenance tables plus canonical FTS. Preserve legacy tables and row IDs. Route core storage APIs and existing IPC adapters to the new canonical store. | Old-schema fixtures migrate without loss; current writes reach canonical tables only; baseline search and dedup tests pass |
| M1.2 | Durable source instances; host/port-aware IMAP configuration; multi-folder occurrences; connector-scoped identities and preserved conflicting revisions. | Same username/different host and same mail/multiple folders remain correct; conflicting Message-ID payloads survive |
| M1.3 | Stable Telegram/WhatsApp conversation keys, email reply graph, unresolved legacy handling, time confidence and explicit timezone interpretation. | Same-name chats stay separate, renames stay linked, incomplete reply chains/cycles are safe, DST/date tests pass |
| M1.4 | Pre-migration snapshot, capacity preflight, transactional conversion, index validation, rollback/reopen path, and newer-schema refusal. | Failure injected before/within/after migration never strands or modifies the original recoverable archive |

M1 exit: tests A01, A02, A03, A04, and A12 pass; every legacy identity has an alias or explicit unresolved record. Unsupported legacy enrichment does not guess missing originals, hosts, folder membership, or threads. Update the assessment to distinguish implemented corrections from remaining gaps.

## M2: Background jobs and responsive storage

Addresses P04 and F01/F05/F10. Depends on M1 storage and source identities. Owns the [job and database design](architecture.md#jobs-and-database-access).

| Slice | Concrete deliverable | Completion evidence |
| --- | --- | --- |
| M2.1 | Writer worker, two read connections, per-connection initialization, bounded queue, typed job state/errors, and v2 job commands/events. | Concurrent read/write tests and missed-event polling recovery; no network I/O inside a storage lock/transaction |
| M2.2 | Bounded parser interfaces replacing whole-archive collection in production; batched commits, cancellation, issue records, and restart recovery. Keep convenience collect helpers for small parser tests only. | Large generated input stays within the memory budget; cancellation preserves committed data and correct counts |
| M2.3 | IMAP coordinator with fetch outside storage ownership, atomic occurrence/checkpoint commits, durable failed-item accounting, safe UIDVALIDITY reset, and per-source exclusion. | Existing mock IMAP tests plus interruption/missing-fetch/checkpoint regressions; search completes while network is stalled |
| M2.4 | Job-backed manual/15-minute opt-in scheduler; migrate performance harness to actual paginated search and add IPC/concurrent-ingest measurements. | Sleep/resume/offline scheduling does not overlap or queue missed runs; baseline report identifies hardware, query paths, and percentiles |

M2 exit: A05/A06/A11 pass. Report cancellation latency limits honestly. Preserve the current performance baselines as historical evidence; append new results rather than retroactively treating them as v2 measurements.

## M3: Archive workspace and source management

Addresses P02/P03/P08 and F07. Depends on M1/M2 commands. Owns the [workspace and journeys](ux-specification.md#workspace-and-navigation).

| Slice | Concrete deliverable | Completion evidence |
| --- | --- | --- |
| M3.1 | Navigation/results/reading layout, local design tokens, bundled font/license, narrow-window states, semantic controls, and explicit frontend state/IPC modules. | Wide/compact/zoom screenshots and keyboard/screen-reader checks; existing stale-response tests remain meaningful |
| M3.2 | v2 search/detail/context, flat stable ranking, empty browse, filter chips, attachment/import scope, copy/link/text/JSON export, and restorable selection/scroll. | A07; ranking, pagination, unknown-time, context and deep-link tests |
| M3.3 | Unified native picker and bounded import-preview flow, explicit source/chat association, timezone/date/collision resolution, capacity checks, Activity and issue retry. Preview understands directory/ZIP manifests; extraction/preservation completes in M4. | Supported/unsupported sample previews; cancel/partial/success states tested; no path typing required |
| M3.4 | Account setup/test/folders, credential-update flow, status and disconnect/local deletion separation; general settings and isolated sample vault. | Invalid password does not replace working credential; keychain fallback is visible; demo content never mixes into personal vault |

M3 exit: A07/A08 and UX/accessibility checks pass. Remove obsolete v1 frontend rendering and duplicate Ask result components once their callers migrate, retaining adapters needed by the still-active Ask slice. A media reference must remain visibly unavailable until M4 has actually preserved it.

## M4: Media, originals, and recovery

Addresses P05/P06 and F04. Depends on M1/M2 identity/jobs and M3 user choices. Owns [object lifecycle and recovery](architecture.md#originals-media-and-object-lifecycle).

| Slice | Concrete deliverable | Completion evidence |
| --- | --- | --- |
| M4.1 | SHA-256 object store, atomic staging/promote/reference lifecycle, input-original preservation, ZIP/directory bounds, and integrity/reconciliation tools. | Interrupted writes leave no committed dangling references; duplicate bytes occupy one object; traversal/symlink/capacity fixtures pass |
| M4.2 | MIME attachments/inline parts, Telegram explicit file references, WhatsApp named media, unlinked source files, missing/failed states, and safe reimport enrichment. | Available media still opens after external inputs are removed; omitted/unmatched files are represented honestly |
| M4.3 | Local image/audio/video viewing, safe export/external-open controls, narrow media handler/CSP, and disk usage reporting. | No autoplay, active HTML execution, remote-resource fetch, or arbitrary filesystem resolution; large/range reads work |
| M4.4 | Versioned backup manifest/container, SQLite snapshot, checksum validation, staged restore, switch journal, previous-vault recovery and UI. | A09/A10/A12; terminate backup/restore at each critical boundary and recover a verified active vault |

M4 exit: media and original counts/hashes round-trip through backup/restore. Include restored settings and relationships, exclude credentials, require source reconnection. Finalize and rerun large-file capacity checks with actual copying rather than preview estimates.

## M5: Optional AI with source-linked answers

Addresses P07 and F08. Depends on M1/M2 query/jobs and M3 shared reading UI; follows M4 to avoid delaying archival correctness.

| Slice | Concrete deliverable | Completion evidence |
| --- | --- | --- |
| M5.1 | Persisted off-by-default consent switch, existing endpoint/model configuration visibility, keychain/session storage, and environment-config compatibility. | Existing configured keys cause zero provider calls until explicitly enabled and submitted |
| M5.2 | Job-backed prepare/approve/generate flow with filter intersection, exact bounded preview/digest, stale-approval invalidation, and local fallback. | Mock-provider capture proves only the approved payload leaves; changes to endpoint/question/results invalidate approval |
| M5.3 | Citation map and shared reading pane, source evidence scope, unsupported-citation handling, cancellation/error/retry states. | A13; citations navigate correct IDs after search changes and pagination; provider failure preserves local results |

M5 exit: existing LLM contract tests are adapted and pass alongside consent/payload tests. No provider credentials or live requests are required in CI. Remove remaining obsolete v1 commands only after all callers and fixtures migrate.

## M6: Three-platform desktop release

Addresses P09 and F09. Depends on all earlier release milestones; platform compile smoke checks may begin earlier to expose portability problems.

| Slice | Concrete deliverable | Completion evidence |
| --- | --- | --- |
| M6.1 | Linux/Windows/macOS build matrix, app identity/icons, platform prerequisites, restrictive local permissions and keychain tests. | Native compilation and smoke records for every supported architecture; no Linux-only path assumptions |
| M6.2 | Bundling/install/uninstall behavior, upgrade from baseline and previous candidate, signing/notarization configuration, packaged CSP/media validation. | Installed artifacts pass A14; uninstall retains vault by default; reinstall reopens it |
| M6.3 | Release candidate regression run, usability sessions, recovery rehearsal, checksums, release notes and user troubleshooting. | All acceptance gates have recorded evidence; unsigned or untested artifacts are labeled as development builds |

M6 exit: [platform release matrix](validation-and-release.md#platform-and-release-matrix) passes. Signing/notarization credentials are an external dependency; do not publish unsigned artifacts as the completed public release. The implementation task itself does not authorize publishing or acquiring signing accounts.

## Completion tracking

- [x] M1: identity, provenance, time, migration — record below
- [ ] M2: jobs, responsiveness, resumability, performance evidence (code landed: #40 M2.1, #41 M2.3, #42 M2.2, #43 M2.4, #50)
- [ ] M3: workspace, imports, account management, accessibility (code landed: #44 M3.1, #46 M3.2; M3.3/M3.4 open)
- [ ] M4: originals/media, storage, backup/restore (code landed: #49 M4.1; M4.2/M4.3/M4.4 open)
- [ ] M5: explicit AI consent and evidence navigation (code landed: #48; citation-map evidence M5.3 remains to be recorded)
- [ ] M6: three-platform installation, upgrade, and release validation (code landed: #47 M6.3 Linux distributions, #17 bundling; M6.1/M6.2 open)

Update this checklist with links to implementation revisions and validation records. Changes to milestone scope require corresponding changes to the [product plan](product-plan.md), not only a checked box.

### M1 completion record (2026-11-05, baseline `57f40c8`)

**Revisions.** M1.1 canonical store, identity/occurrence/time-provenance tables, canonical FTS, legacy tables frozen with row IDs preserved — #31 (`7bc354b`). M1.3 stable conversation keys (Telegram/Signal provider ids, WhatsApp content key), email reply graph, explicit timezone interpretation, migration-7 backfill — #33 (`27b4383`). M1.2 durable source instances, host/port-aware IMAP identity, multi-folder occurrences, preserved conflicting revisions, migration 8 — #37 (`94e026a`). M1.4 safe migration: newer-schema refusal, capacity preflight, WAL-checkpointed snapshot, single-transaction conversion, FTS validation, byte-for-byte rollback, durable failure marker — #39 (`00d80eb`).

**Changed behaviors.** All storage APIs and the v1 IPC adapters read and write the canonical tables only; dedup moved to the canonical identity alias (legacy) and the connector identity tuple (IMAP: instance + namespace + external key + semantic revision hash); re-imports and re-syncs record occurrences instead of duplicate rows; the same mail in several folders is one row with one occurrence per sighting; conflicting payloads behind one `Message-ID` are preserved as revisions; conversation grouping is rename-stable and display-name-distinct for Telegram/Signal/WhatsApp; email threads resolve one-hop through `In-Reply-To`/`References` with adoption of late-arriving roots; timestamps carry original text, resolved UTC instant, zone, and confidence; legacy assumed values are never silently reinterpreted (migration 7 resolves blind-Z WhatsApp stamps to machine-zone instants and re-keys identities so re-imports dedup).

**Tests run (2026-11-05, `cargo test -p vault-core`, this machine).** `--test integrity` 47 passed (canonical migration, conversation-key backfill, dedup, schema migrations, safe migration with injected before/within/after-conversion failures, corruption, WAL crash, round-trips, VACUUM); `--test conversations` 15; `--test source_instances` 13; `--test imap_sync` 14; `--test imap_mbox_equivalence` 1; `--test whatsapp_ingest` 9; `--test telegram_ingest` 7; `--test signal_ingest` 8; `--test dst_local` 1; `--test storage` 10. Acceptance mapping: A01 (reimport/rename/dedup/identical repeats) covered by ingest + integrity dedup suites; A02 (same username/different host, multi-folder, UIDVALIDITY reset) by source-instances and imap-sync suites; A03 (conflicting revisions, incomplete chains, cycles, no folder-as-thread) by conversations and imap-sync suites; A04 (DST folds/gaps, naive dates, legacy assumed) by dst-local and ingest suites; A12 (upgrade, validation failures, recoverable original) by the safe-migration suite's failure injections.

**Migration/recovery evidence.** Failure injected before the snapshot, mid-snapshot, within the conversion transaction, and after its commit: in every case the original database file is byte-identical afterwards, the snapshot stays independently readable, retrying completes the conversion on the intact original, fresh/up-to-date databases never snapshot, WAL-only legacy rows survive the snapshot path, and newer schemas are refused without touching a byte; a pre-drifted index records a durable refusal marker for later opens. Every legacy identity carries an alias row or an explicit unresolved record (canonical-migration assertions).

**Failures/limitations.** Discord conversations still use legacy display grouping (no provider key yet). Legacy-namespace → account binding, the `user_confirmed` import-time confirmation, and the persisted workspace-timezone setting are designed but not implemented (M3.3-owned surfaces; the schema carries `user_linked`/`user_confirmed` for them). Frontend suites were not rerun for this documentation change.

**Remaining work.** Deferred to owning slices: import preview/date confirmation UX (M3.3), user-facing provenance/uncertainty surfaces (M3), and eventual legacy-snapshot removal (a separate migration once recovery experience justifies it).

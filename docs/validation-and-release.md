# Validation and release

Status: **proposed validation strategy**, with a separate documentation-only verification record below. Test presence is not a passing result. Use the [roadmap](implementation-roadmap.md) to determine which cases apply to a slice.

## Test layers and existing infrastructure

Keep pure TypeScript view-model/render tests, Rust parser/storage tests, mock IMAP/LLM contract tests, property/fuzz/integrity suites, and native desktop E2E. Extend existing suites rather than building parallel test frameworks. Add wire fixtures for v2 IPC types/errors/events and snapshots of synthetic UI states; pair snapshots with behavior assertions.

Use a fresh temporary vault per destructive test and verify its resolved path before startup. Existing desktop E2E already uses isolated data directories; preserve that property. Test keys are synthetic and mock endpoints are loopback-only. Never require a user's mailbox, API key, or personal export in CI.

After installing documented prerequisites, the current baseline commands are:

```sh
npm ci
npm run check
npm test
npm run build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p mailchat-vault --features tauri/custom-protocol
xvfb-run -a npm run test:e2e
```

The final two commands describe the existing Linux E2E route. They are not a promise of the same driver support on every desktop platform. See [the existing harness instructions](../tests/e2e/README.md) for prerequisites. Update its conditional-spec documentation when v2 replaces runtime-probed legacy UI. Release-critical paths must fail if unexpectedly absent; do not accept a skipped source/detail/date test as coverage after those features are required.

## Acceptance scenarios

| ID | Scenario | Required result |
| --- | --- | --- |
| A01 | Reimport unchanged/overlapping exports; renamed files; same-name Telegram chats; repeated identical WhatsApp records | Stable IDs, no duplicate original bytes, deliberate repeats preserved, unprovable collisions explicitly reviewed |
| A02 | Same IMAP username on different hosts/ports; one message in several folders; UIDVALIDITY reset | Distinct configured sources, retained folder occurrences, safe dedup and checkpoint epochs |
| A03 | Same Message-ID/different payload; replies before parents; references cycles; folder with unrelated mail | Revisions preserved, bounded correct reply context, no folder-as-thread inference |
| A04 | UTC offsets, DST 23/25-hour days, overlap/gap timestamps, naive dates, no dates, legacy assumed UTC | Correct local-day bounds, explicit uncertainty, no silent historical reinterpretation |
| A05 | Slow network while searching; concurrent imports; cancel and restart at batch boundaries | Reading stays responsive, no network-held database lock, committed data remains usable and jobs recover |
| A06 | Missing UID/body, parse failure, transport disconnect, per-folder error, cursor commit failure | No unseen UID skipped; durable issues/counts; retry does not duplicate completed work |
| A07 | Empty browse, combined filters, sort, exact count, pagination, rapid stale responses, reading and Back | Stable intended order and selection, same-snapshot counts, previous responses cannot overwrite current state |
| A08 | New user chooses file/directory/ZIP; preview cancellation; wrong format; credential failure/fallback | Clear action/state, no successful empty unsupported import, no invalid secret replacing a working secret |
| A09 | MIME/inline files, media named/missing/unmatched, duplicate bytes, external inputs deleted | Available files copied and openable; honest availability/associations; safe export |
| A10 | ZIP traversal, absolute paths, links, duplicate normalized names, expansion bombs, unsupported/active files | Import boundaries enforced, capacity failures visible, no execution/remote resource loading |
| A11 | Large datasets, query while ingesting, deep pages, long bodies, no matches, broad common terms | Measured bounded memory, responsive UI, recorded p50/p95/p99 and no unacceptable regression |
| A12 | Upgrade, disk-full, corrupt FTS, failed backup, tampered manifest, missing object, interrupted restore switch | Original/previous vault recoverable; no unverified replacement; counts/relationships/hashes validate |
| A13 | AI off/declined, timeout, bad plan, changed filters/provider/results, citation outside approved map | No unauthorized calls/content; safe fallback; exact bounded payload; citations navigate fixed original IDs |
| A14 | Install, first boot, keychain fallback, upgrade, packaged media preview, uninstall/reinstall on each target | Correct paths/permissions; archive retained by default; signed candidate behaves like tested code |

Include injected failures between object promotion and database commit, between checkpoint writes and commit, before/after migration version changes, and before/after each restore rename/journal update. A clean happy-path backup is insufficient evidence of recoverability.

## UX and accessibility

Verify 1280×800, 1100px, 960×680, and 640×480 windows, plus 200% zoom and large OS text settings. Capture wide/compact/empty/loading/partial/error states in light and dark themes with synthetic content. Check long names, multilingual text, emoji, RTL text, and very long bodies/filenames.

Keyboard-only coverage includes search, filters, pagination, message/context navigation, media controls, source setup, cancellation, and backup/restore. Confirm visible focus, correct focus restoration, no inaccessible click-only controls, and native input/media shortcuts. Check screen-reader names, landmarks, heading order, result counts, errors, and throttled progress announcements with NVDA on Windows and VoiceOver on macOS; exercise a Linux screen reader where available. Measure text/non-text contrast against WCAG 2.2 AA requirements and verify reduced-motion behavior.

Recorded so far (M3.1/M3.2, automated half): the e2e 16-workspace-layout and 16-search-v2 specs verify semantic landmarks with labels, `role=status` live regions, named controls, keyboard activation of result/thread rows with focus restoration on Back, Escape-closing of the compact drawer, 1280px@200%-zoom ≡ 640px compact equivalence, and committed wide/medium/compact/zoom screenshots with synthetic data; reduced-motion is honored at the token/CSS level. Still open for the M3 exit: NVDA/VoiceOver/Linux screen-reader session passes, measured WCAG 2.2 AA contrast, and the five-user task sessions below.

Run the five-user task sessions defined in [Product success criteria](product-plan.md#success-criteria). Record task outcomes and concrete confusion; avoid soliciting personal archive content for the sessions.

## Performance and capacity

The [performance baseline](../tests/perf/BASELINE.md) now (M2.4) times the real paginated path — `search_page` with exact COUNT, snippets, filters, ordering, and canonical FTS — at 100k (gated) and 1M (informational), beside IPC-shaped round trips (pool read hop + JSON page serialization), search while a bounded import commits through the write lane, and startup including `DbPool::spawn`; the historical `search_filtered` tables are preserved verbatim below the current results. Still to add: end-to-end IPC/display timing through the webview, percentiles for the v2 command surface, and timed search-under-stalled-network numbers (non-blocking is already proven by tests). A `LIMIT` alone does not cap the cost of ranking/counting every matching row.

Use deterministic synthetic 100k-message fixtures for correctness/regression gates and 1M for informational capacity trends. Include common/frequent/rare terms, author/filename matches, combined source/date filters, no matches, browse, deep pagination, revisions and long bodies. Generate sparse/streamed large files; do not commit giant binaries. Report database/object sizes, import throughput, resident-memory peaks, startup, query timing, and cancellation delay separately.

Initial product targets on the recorded reference desktop:

| Measurement | Acceptance target |
| --- | --- |
| 100k usual local search to displayed page | p95 ≤ 200ms after warmup, including IPC and render |
| 100k common-term worst-case page | p95 ≤ 1s; display busy state promptly |
| UI response to user input/job progress | Visual response within 100ms; expensive work off the UI thread |
| Search with a stalled IMAP connection | Independent of network timeout; same query within 2× idle p95 plus 100ms |
| Ingest working memory | At most 256 MiB above idle for the text corpus, independent of archive length; media streamed, preview decode limits measured separately |
| Cancel acknowledged | Within 200ms; final cancellation after current bounded I/O/atomic batch, with stage shown |
| Warm startup to usable workspace at 100k | ≤ 2s on the recorded reference desktop |

These are proposed targets, not measured results. The M2.4 baseline already records core-path actuals with headroom against several of them (100k frequent-term p95 26.8 ms against the ≤ 200 ms usual-search target; the ubiquitous worst case at 155.5 ms against ≤ 1 s; ingest memory proven bounded by the heavy suite test), but the IPC-to-display composition, cancellation acknowledgment, and stalled-network timing targets remain unmeasured. Record hardware/OS/build flags, warmup, iterations, dataset seed, concurrent workload, and distribution before claiming them. Retain existing CI per-query budgets until new-path runner measurements justify replacements; do not impose desktop timings blindly on shared runners. An unexplained regression greater than 20% on the same controlled machine requires investigation before release. Keep 1M tests informational until an explicit capacity target is adopted.

## Platform and release matrix

| Target | Build/package | Automated evidence | Required native smoke evidence |
| --- | --- | --- | --- |
| Linux x64 | AppImage and Debian package | Core/frontend tests, native build, existing WebKit/Xvfb E2E | Install/run, dialogs, Secret Service available/unavailable, media, upgrade, restore |
| Windows x64 | NSIS installer, signed public executable | Core/frontend tests, native build, supported driver automation where available | Credential Manager, file locks/path rules, install/uninstall retention, upgrade, restore, keyboard/screen reader |
| macOS arm64 and x64 | App bundle/DMG; sign and notarize public builds | Core/frontend tests, native build per architecture | Keychain, permissions/dialogs, media codecs, upgrade/restore, VoiceOver; native or documented hardware evidence per architecture |

Do not assume Linux E2E automation covers macOS/Windows or that cross-compilation proves native behavior. Where the current harness cannot automate a platform, use a repeatable manual checklist with artifact hash, OS/architecture, tester, and observed result. Native CI builds are required; missing native test equipment remains a visible release limitation.

Release order: build candidate → run correctness/recovery/UX/platform checks → sign/notarize applicable artifacts → smoke the packaged signed artifacts → record checksums and known limitations → obtain any separately required publication authorization. Signing credentials and notarization access are external dependencies. Never put them in source, logs, or backups. Automatic updates are deferred.

Keep the app identifier/data location stable. Uninstall retains user data unless the user explicitly selects removal; reinstall reuses it. Older binaries refuse newer schemas. Recovery instructions restore a verified prior snapshot rather than trying a database downgrade. Release notes describe schema changes, backup format compatibility, copied-media behavior, privacy limits, and source limitations.

## Troubleshooting and observability

Support views show job stage, source label, safe error code, retry action, and issue counts. Diagnostic export is explicit and excludes content, passwords, full account addresses, arbitrary raw server output, and local paths by default. No automatic telemetry is added.

Distinguish missing dependencies, compiler/tool environment failures, failing assertions, and skipped tests in reports. For disk-full failures, preserve originals/committed data and offer retry after freeing space. For backup/restore failure, preserve the working vault and identify the failed validation stage. For unavailable keychains, display session-only behavior rather than silently writing plaintext credentials.

## Documentation validation

This documentation change requires no application dependency installation or new product tests. Validate:

- All nine planned documents exist, with a root README link and working relative file/heading links.
- Current-state statements match the inspected baseline and cite repository evidence.
- Every requirement P01–P09 has implementation slices and acceptance coverage A01–A14.
- Proposed types/commands are labeled proposed, public examples are consistent, and no implementation checkbox is falsely completed.
- No accidental application/config changes, trailing whitespace, conflict markers, personal content, or credentials appear in the diff.

Application tests attempted during planning were blocked: frontend dependencies were absent; Rust C dependency compilation failed; browser bridge inspection failed. They are not passing or failing application assertions. Documentation-only checks performed when delivering this set are recorded here:

- 2026-11-01: verified all nine planned documents and the root README integration; checked all 116 relative file/heading links across ten Markdown files.
- Verified final newlines, whitespace, balanced code fences, and absence of merge-conflict markers in all ten files; `git diff --check` passed for tracked changes.
- Checked roadmap references for P01–P09 and A01–A14; reviewed the requirement/slice/scenario mapping and cross-document identity, privacy, media, and recovery rules.
- Confirmed the change contains documentation and the root README only. Application tests were not rerun for this documentation-only change; the earlier environment limitations remain recorded above.

Landing on `main` (2026-11-05, baseline `57f40c8`, M1 documentation update):

- The nine-document set landed from the reviewed branch, with the M1-owned sections (contracts: compatibility boundary, persistence model, identity, time, migration; assessment; roadmap status/checklist; decisions D03/D12; this record) grounded in the implemented code and marked **current**.
- Re-verified all relative file/heading links across the ten Markdown files (nine docs + root README link); headings referenced by other documents were preserved during the rewrite.
- M1 evidence suites rerun green on this machine (see the [M1 completion record](implementation-roadmap.md#completion-tracking) for counts and the A01–A04/A12 mapping).
- No application/config changes in this delivery; `git diff --check` clean.

Landing on `main` (2026-11-05, M2 code at `57f40c8`, spec tree at `1115eac`, M2 documentation update):

- The M2-owned sections are grounded in the implemented code and marked **current**: [Architecture: jobs and database access](architecture.md#jobs-and-database-access) (including IMAP correctness), the persistence-table rows and migration-ledger attribution for migrations 9–11 in [contracts](data-and-api-contracts.md#domain-and-persistence-model), and the job-protocol half of [errors and events](data-and-api-contracts.md#errors-and-events). The roadmap's M2 checkbox completed with the full record (A05/A06/A11 mapping, migration/recovery evidence, limitations); the assessment re-grounded (F01/F05/F10, remaining gaps, verification record); decisions D04/D10 amended; this file's performance paragraph grounded to the M2.4 baseline.
- M2 evidence suites rerun green on this machine — `jobs` 13, `bounded_ingest` 21 + the ignored heavy memory-budget test passed separately, `imap_coordinator` 10, `imap_sync` 14, `scheduler` 12 — see the [M2 completion record](implementation-roadmap.md#completion-tracking).
- Documentation-only checks: relative file/heading links re-verified across the touched files (all referenced headings preserved), final newlines/whitespace checked, no merge-conflict markers, no application/config changes; `git diff --check` clean.

Landing on `main` (2026-11-05, M3.1/M3.2 code at #44 `bfc24db` / #46 `8df0721`, documentation baseline `76be9c6`, M3 documentation update):

- The M3-owned sections are grounded and their status corrected: the workspace/navigation and search/selection sections of the [UX specification](ux-specification.md#workspace-and-navigation) marked **current** with honest interim-state notes for M3.3/M3.4, the shipped v2 reading-surface rows (including `message_detail_v2` as the implemented `get_message_v2`, and the direct-command selection export) and the `AppError` envelope marked **current** in [contracts](data-and-api-contracts.md#v2-ipc-surface). The roadmap gained the [M3 progress record](implementation-roadmap.md#completion-tracking) (M3.1/M3.2 revisions, changed behaviors, landing-time tests with static reproduction, A07 mapping, limitations); the M3 checkbox stays open for M3.3/M3.4 and the human UX/accessibility session evidence; the assessment re-grounded (F07, inventory, gaps, verification record, order); decision D08 amended; this file's UX record extended with the automated-half evidence.
- No application suite was rerun for this documentation-only change (standing docs-only validation decision); the cited M3 artifacts were reproduced statically at `76be9c6` (see the [assessment verification record](project-assessment.md#verification-record-and-limits)).
- Documentation-only checks: relative file/heading links re-verified across the touched files (all referenced headings preserved), final newlines/whitespace checked, no merge-conflict markers, no application/config changes; `git diff --check` clean.

## Maintenance

Append dated benchmark and release-candidate records as implementation progresses, with links to revisions/artifacts. Update commands when build scripts change, and refresh supported-platform evidence for each release. A passing historical run does not waive a newly changed migration, object, or privacy path.

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

Recorded so far (M6, A14 — 2026-11-05; see the [M6 progress record](implementation-roadmap.md#completion-tracking) for the full evidence): install/first-boot/uninstall-retention and upgrade halves are covered at the automation level — the Linux deb/AppImage suites (clean 22.04/24.04, green at #47 landing) prove install, desktop/MIME integration, launcher-style launch, and app-file removal, while the Windows smoke phases (msi/nsis fresh install with `vault.db` created in the app-data path, uninstall leaving the archive, in-place upgrade from the frozen v0.1.0 fixture with identical content digests and a live FTS query) exist and were driven green under Linux against disposable fake installers, but have **no run on a real Windows runner**. The packaged CSP/media configuration is grounded (restrictive CSP, `mcmedia:` protocol), with no packaged-artifact media walkthrough recorded. Still open for A14: keychain fallback per platform, the signed-candidate half (no signing exists), reinstall-reopens-it, and all macOS-target evidence.

Include injected failures between object promotion and database commit, between checkpoint writes and commit, before/after migration version changes, and before/after each restore rename/journal update. A clean happy-path backup is insufficient evidence of recoverability.

Recorded so far (M5, A13 consent/citation halves): the declined/off half — zero provider requests without explicit session consent — is proven by the backend mock-endpoint recorder suite in [lib.rs](../src-tauri/src/lib.rs) (a network selection without consent makes zero recorded requests while the ask still answers locally) and by both e2e sessions of [05-llm-ask-mock](../tests/e2e/specs/05-llm-ask-mock.e2e.mjs); the exact-bounded-payload half is proven by the e2e summary-prompt assertions (only the matched rows, out-of-range and non-matching archive rows excluded, exactly two round trips per consented ask); the citation half — citations navigate fixed original ids in thread context through the v2 reading pane, and out-of-range/beyond-window markers stay plain text — is covered by the frontend wiring suite and both e2e sessions; provider failure preserving local results is covered by the fallback paths (garbage translation → plain term search, summary failure → notes + raw results) in the e2e and mock-LLM pipeline suites. Still open for A13: a dedicated request-timeout test (failures are injected as garbage responses; the 30-second timeout folds into the same fallback path) and the changed-filters/provider/results invalidation half, which has no counterpart until the M5.2 digest-bound approval lands — today only a provider switch re-renders the consent prompt; a granted session consent is not invalidated by question, filter, or result changes.

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
| macOS arm64 and x64 | App bundle/DMG; sign and notarize public builds | Core/frontend tests, native build per architecture, packaged-dmg smoke (`scripts/dmg-smoke-test.sh`) | Keychain, permissions/dialogs, media codecs, upgrade/restore, VoiceOver; native or documented hardware evidence per architecture |

Do not assume Linux E2E automation covers macOS/Windows or that cross-compilation proves native behavior. Where the current harness cannot automate a platform, use a repeatable manual checklist with artifact hash, OS/architecture, tester, and observed result. Native CI builds are required; missing native test equipment remains a visible release limitation.

Release order: build candidate → run correctness/recovery/UX/platform checks → sign/notarize applicable artifacts → smoke the packaged signed artifacts → record checksums and known limitations → obtain any separately required publication authorization. Signing credentials and notarization access are external dependencies. Never put them in source, logs, or backups. Automatic updates are deferred.

Recorded so far (M6, 2026-11-05 — the [M6 progress record](implementation-roadmap.md#completion-tracking) owns the detail). Automated evidence now exists for two of three targets: Linux (the deb/AppImage smoke suites on clean 22.04/24.04 containers, green at #47 landing, wired as the release workflow's `smoke-linux` matrix job) and Windows-as-automation (the four-phase installer smoke with fresh-install, uninstall-retention, and frozen-fixture upgrade proofs — never yet dispatched on a real runner). Native compilation is recorded for every target through the published `v0.1.0` (universal dmg included), whose honest record is: both Release workflow runs concluded failure (the Windows attach step failed after asset upload), no smoke gate ran against the published artifacts, no checksums were published, and the artifacts are unsigned with the disclosure in the notes. Open per target: macOS has the automated dmg smoke on `main` (#62) but no executed run (no macOS host) — the manual checklist remains the only macOS evidence; Windows needs a real-runner dispatch; all targets need checksums, signing/notarization (external dependency), keychain-fallback evidence, and the usability/recovery sessions. No push-triggered CI run on `main` is recorded after the M3.1-era commit (the Windows-smoke workflow header records that repo workflows are disabled by decision and it is dispatched manually).

Keep the app identifier/data location stable. Uninstall retains user data unless the user explicitly selects removal; reinstall reuses it. Older binaries refuse newer schemas. Recovery instructions restore a verified prior snapshot rather than trying a database downgrade. Release notes describe schema changes, backup format compatibility, copied-media behavior, privacy limits, and source limitations.

### Release validation matrix (M6.4)

The M6 exit names this matrix. One command per platform now runs its entire column — the full test suite (frontend + workspace), the packaged release bundles, and the smoke gates against those bundles — so "the matrix passes" is a reproducible run, not an assembled-by-hand claim:

| Column | One command | Runs on |
| --- | --- | --- |
| Linux | `scripts/release/matrix-linux.sh` | any machine with docker (every leg pinned in its own container; no host toolchain beyond docker is trusted) |
| macOS | `scripts/release/matrix-macos.sh` | a real macOS host (node 22 + rust stable + Xcode CLT; the script refuses to run elsewhere) |
| Windows | `pwsh scripts/release/matrix-windows.ps1` | a real Windows host (node 22 + rust stable + WebView2; the script refuses to run elsewhere) |

Legs per column (skips are reported as SKIPPED and never count as green):

| Leg | Linux | macOS | Windows |
| --- | --- | --- | --- |
| `npm ci` → `npm run check` → `npm test` → `npm run build` | suite container (`Dockerfile.suite`, ubuntu 24.04 — mirrors the CI `test` runner) | host toolchain | host toolchain |
| `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` | suite container | host | host |
| `cargo test --workspace` | suite container | host | host |
| `cargo build -p mailchat-vault --features tauri/custom-protocol` | suite container (feeds the e2e leg) | host | host |
| Desktop e2e (`tests/e2e`) | e2e container (`Dockerfile.e2e`: xvfb + WebKitWebDriver + tauri-driver, pinned 24.04) | not applicable — the WebDriver route is the Linux-validated one ([its README](../tests/e2e/README.md)); the column records this rather than silently greening it | not applicable — same reason |
| Release bundles | `build-linux.sh` (pinned 22.04 toolchain → AppImage + deb) | universal dmg exactly as the release workflow builds it | msi + NSIS exe exactly as the release workflow builds them |
| Packaged smoke | deb + AppImage suites on clean 22.04 **and** 24.04 | `scripts/dmg-smoke-test.sh` (mount, copy, universal binary, headless `--version`/`--help`) plus the launch-stays-alive `smoke-macos.sh` once the release-fix tooling is on the branch (reported WAITING until then) | `scripts/windows-smoke.ps1` fresh phases against the built installers (upgrade phases with `-OldMsi`/`-OldExe`), or the published-release route via the release-fix `windows-smoke-from-release.ps1` (`-Published -Tag …`) once that tooling is on the branch |

Recorded status of the three columns (updated as runs land; keep honest):

| Column | Status | Evidence |
| --- | --- | --- |
| Linux | green — full column | one-command run 2026-11-05T11:35–11:55Z at the M6.4 landing tree on the fleet Linux worker (docker host): suite (vitest 353/353, fmt/clippy clean, cargo 596/596), e2e (30 passed + foreign-root pass 1/1, three by-design visible skips), bundles, and deb/AppImage smoke 19/19 on clean 22.04 **and** 24.04 — all four legs PASS; detail in the [M6.4 record](implementation-roadmap.md#completion-tracking) |
| macOS | harness ready, **not run** | needs a macOS host; mac bundles cannot be built from the Linux worker and repo Actions are disabled by decision — the open captain decision below owns this |
| Windows | harness ready, **not run** | needs a Windows host for the real installer smoke (the fake-installer drive of #52 proved the script logic only); same captain decision |

The macOS/Windows columns wait on one captain decision, kept visible here rather than resolved by the matrix itself: **(a)** keep the published `v0.1.0` with its honest record and checksums and defer the next tagged candidate until Actions return; **(b)** provide real macOS/Windows machines and run `matrix-macos.sh` / `matrix-windows.ps1` on them (mac/win bundles are then built locally per platform); **(c)** approve temporary Actions for release tags, where `ci.yml` already carries the macOS dmg-smoke and e2e jobs and `release.yml` the three build jobs. Whichever option is chosen, these one-command columns produce the evidence the matrix needs; nothing here presumes the answer.


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

Landing on `main` (2026-11-05, M5 code at #48 `a0d79dc`, documentation baseline `7ac7b8e`, M5 documentation update):

- The M5-owned sections are grounded and their status corrected: the [UX optional-Ask section](ux-specification.md#optional-ask) marked mixed with a Current paragraph for the consent gate and source-linked answers, the [Architecture: optional AI ask layer](architecture.md#optional-ai-ask-layer) section added as current with honest deviations, and the [AI retrieval contract](data-and-api-contracts.md#search-request-and-response) split into the current consent-gate half and the proposed M5.1/M5.2 remainder. The roadmap gained the [M5 progress record](implementation-roadmap.md#completion-tracking) (revisions, changed behaviors, landing-time tests with static reproduction, A13 mapping, limitations, remaining work); the M5 checkbox stays open for the M5.1 persisted switch and M5.2 job-backed approval flow; the assessment re-grounded (F08, inventory, gaps, verification record, order); decision D09 amended; this file's A13 evidence note added.
- No application suite was rerun for this documentation-only change (standing docs-only validation decision); the cited M5 artifacts were reproduced statically at `7ac7b8e` (see the [assessment verification record](project-assessment.md#verification-record-and-limits)).

Landing on `main` (2026-11-05, M4 documentation update, M4.1 code at `1f2720c`, rebased onto the `ecbf1e4` spec tree after the M5 record and the M4.3 landing):

- The M4-owned sections are grounded and marked **current** where code has landed: [Architecture: originals, media, and object lifecycle](architecture.md#originals-media-and-object-lifecycle) (the M4.1 half — store layout, stage→promote→reference lifecycle, budgets, reconciliation; M4.2/M4.4 remain proposed — the M4.3 media display surface landed with #58 and its acceptance record stays open), the media persistence row and [media and capacity](data-and-api-contracts.md#media-and-capacity) in [contracts](data-and-api-contracts.md), and `media_reconcile` in the compatibility boundary. [Backup and restore](architecture.md#backup-and-restore) stays wholly proposed (M4.4). The assessment was re-grounded (F04, implemented inventory, M4.1 leftovers, verification record); D05 amended as half-implemented; the roadmap status annotated.
- M4.1 evidence suites rerun green on this machine — `object_store` 16, `media_extract` 14, `bounded_ingest` 21 (+1 ignored heavy test, not rerun here), `--lib` 128 — covering interruption proofs at all three lifecycle points, hostile ZIP fixtures (traversal, symlink entries, declared-size bombs) rejected with nothing kept, dedup across names/extensions, reconciliation classification of every damaged state, and reimport backfill. A10's traversal/symlink/expansion refusals and the duplicate-bytes and external-input halves of A09 are covered at the store level; the A09–A12 acceptance records stay open with the M4 milestone.
- Documentation-only checks: relative file/heading links re-verified across the touched files (all referenced headings preserved), final newlines/whitespace checked, no merge-conflict markers, no application/config changes; `git diff --check` clean.

Landing on `main` (2026-11-05, M6 documentation update, release code through `eb94521`, published `v0.1.0` at tag `21c1215`):

- The M6-owned sections are grounded in the landed release code and the recorded release evidence: the [platform and release matrix](#platform-and-release-matrix) paragraph above, the A14 recorded-so-far note in [Acceptance scenarios](#acceptance-scenarios), decision D11's amendment, and the [M6 progress record](implementation-roadmap.md#completion-tracking) (revisions #17/#45/#47/#52, the v0.1.0 evidence record, the #59 notifications grounding, A14 mapping, limitations, remaining work). The assessment was re-grounded (F09, packaging/notifications inventory rows, gaps, verification record); the settings row in [contracts](data-and-api-contracts.md#domain-and-persistence-model) and the [UX state matrix](ux-specification.md#state-and-recovery-matrix) gained the notifications current-state notes; [Architecture: privacy](architecture.md#privacy-credentials-and-failure-reporting) gained the notifications content/permission contract.
- No application suite was rerun for this documentation-only change (standing docs-only validation decision); the cited artifacts were reproduced statically at `eb94521` — every release script/workflow/fixture/spec file named in the record, the Linux smoke suites' checks, the frozen fixture and its pinning test, the digest tool, the 23 notify unit tests, the three notify e2e specs among 33, the `notification:default` capability, and the `Options::notifications` settings field — and the release-side claims (workflow runs, dispatch history, tag target, assets, absence of checksums) were read back from the forge (see the [assessment verification record](project-assessment.md#verification-record-and-limits)).
- Documentation-only checks: relative file/heading links re-verified across the touched files (all referenced headings preserved), final newlines/whitespace checked, no merge-conflict markers, no application/config changes; `git diff --check` clean.

## Maintenance

Append dated benchmark and release-candidate records as implementation progresses, with links to revisions/artifacts. Update commands when build scripts change, and refresh supported-platform evidence for each release. A passing historical run does not waive a newly changed migration, object, or privacy path.

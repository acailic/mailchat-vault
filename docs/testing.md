# Testing

How the suites are organized, what the performance budgets enforce, and how the
network-facing paths are tested hermetically.

## Test suites

```sh
cargo test               # vault-core: storage, migrations, dedup, FTS search, mbox / WhatsApp / Telegram / Discord ingest, bounded streaming ingest (memory budget, batched commits, cancellation + restart recovery, issue-record idempotence), media extraction (real small fixture files), object store (M4.1: interrupted-write proofs at all three lifecycle points, hash dedup across names/extensions, capacity refusal with refund, reconciliation clean after a clean import and classifying every damaged state, disk-usage report (referenced/orphaned split, per-extension and largest-object breakdowns)), import bounds (M4.1: traversal/symlink refusals, hidden-dir/symlink-skipping walks, hand-crafted hostile ZIP fixtures — traversal, symlink entries, declared-size bombs — rejected with nothing kept, real WhatsApp/Telegram .zip imports end to end), filtered/paged search, chat context, archive statistics (incl. the 1M-message dashboard perf contract), 100k FTS perf fixture, IMAP sync (mock server; durable source instances, OAuth2 login flow (mock OAuth server + mock IMAP, XOAUTH2 end-to-end), multi-folder occurrences, conflicting revisions, atomic fetch-batch commits with mid-batch fault rollback, missing-fetch + unparseable failed-item ledger accounting incl. retry/resolution and reset clearing, durable-across-reopen), jobs (M2.1 pool: search-during-writes concurrency, bounded-queue backpressure, two-reader concurrency, typed job lifecycle/cancellation/progress, event-seq polling recovery incl. ring trimming, IMAP network-stall-does-not-block-writes proof), IMAP coordinator (M2.3: rounds over the real pool — exclusion skips and re-include, no-credentials reporting, interruption mid-fetch leaves checkpoint and archive in agreement, mid-batch storage fault rolls back the whole batch, checkpoint recovery across a pool restart, missing fetch heals next round, one unreachable source fails alone, search + write complete while a round is parked on a stalled network), periodic sync scheduler (M2.4: opt-in rounds as plain imap_sync_all jobs, a round outlasting the interval neither overlaps nor queues, a clock-jump gap collapses to exactly one catch-up round, disable cancels the active round, persisted schedule re-arms one interval out, failed rounds keep the cadence), LLM search pipeline (mock LLM, no network), backup/restore (M4.4: container round-trip incl. unlinked registry objects and settings, WAL-snapshot consistency, failure at every backup boundary leaves no destination and an untouched vault, tampered/damaged/newer-version containers refused, preview→commit invalidation on vault or staged-bytes change, interrupted switches at every journal boundary recovered to one verified vault with the previous vault kept, return-to-previous and explicit forget), settings model (file round-trip, provider pairing rules, menu building), local embeddings (deterministic embedder, incremental job, cosine scan, hybrid ranking, 100k vector perf budgets), folder watching (real temp folders, through the pool), export (CSV/JSON/MBOX round-trips incl. unicode + large sets), integrity suite (corruption & crash recovery, export/import round-trips, VACUUM/ANALYZE, schema migrations, safe migration with injected before/within/after-conversion failures: snapshot kept readable, original restored byte-for-byte, durable failure-marker refusal on later opens, capacity preflight, newer-schema refusal)

npm test                 # frontend unit + UI-state snapshot tests (vitest), incl. settings helpers, the provider-menu / settings wiring, the background-jobs protocol client (cursor tracking, gap detection, trimmed reconciliation, panel rendering), and the auto-sync toggle wiring (status text, scheduler-status events, interval changes), the new-mail notification layer (digests over imap_sync/imap_sync_all/watch results, focus/permission/enable gates, replay dedup), plus the M5 ask consent + evidence wiring (prompt/re-ask/keep-local/revoke, session-scoped reset on a fresh boot, clickable citations and source rows opening the original message in thread context), and the M4.3 media display (mcmedia URL building/validation, inline image/audio/video DOM with no autoplay, the Settings media-store usage report render), and the M4.4 backup/restore view helpers (manifest/preview/restore/recovery summary lines, binary sizes, restore-flow and previous-vault slot rendering, commit gating); also the release-smoke self-checks (src/release-smoke.test.ts) that keep the native-verification substrate honest — bash syntax/shellcheck (when installed), PowerShell parse checks (when pwsh exists), off-platform refusals, and the G1 evidence contract (SHA-256/tester/JSON output) in the smoke scripts, runbook and A14 templates; they never substitute for a native run (docs/native-verification.md)
npm run check            # TypeScript type-check
cargo test --workspace   # also compiles the Tauri app + its LLM provider registry, job-dispatch, M5 consent-gate tests (a local TCP recorder as the mock provider endpoint: zero requests without consent, translate+summarize with; needs the Linux libs above), and M4.3 media-protocol tests (name validation, byte ranges, passive-only content types)
```

The vector layer's budgets over 100k embedded messages (enforced by `tests/vector_perf.rs`): first-run embedding job < 15 s, cosine scan < 200 ms, full local hybrid ask < 250 ms; the FTS < 100 ms contract stays in `search_perf.rs`.

## Fuzzing

The parser layer is also fuzzed (`crates/vault-core/tests/fuzz/`): a
hand-crafted malformed-input corpus plus deterministic seeded structured
generators assert that no input — truncated files, mixed formats, Unicode
edge cases, duplicate JSON keys, bogus timezones, hostile MIME nesting —
may ever panic a parser, only produce an `Err` or a counted parse
failure. Raise the random rounds locally with
`MC_FUZZ_ITERS=100000 cargo test -p vault-core --test fuzz --release`.

## Continuous integration

CI (`.github/workflows/ci.yml`) runs all of the above plus `clippy` and `fmt --check` on every push and PR.

> **CI is not starting jobs since 2026-11-14** (account spending limit —
> a billing action outside any repository change). Until that is lifted,
> absent or red PR checks are **not code verdicts**; the operative gate is
> the [interim per-PR local verification protocol](#interim-per-pr-verification-while-ci-is-down)
> below, and the PR template's Local verification block is where results
> are recorded. The permanent execution-route decision is tracked
> separately in #80.

## Interim per-PR verification while CI is down

Since the billing cutoff, verification runs locally and is recorded in
the PR body (`.github/PULL_REQUEST_TEMPLATE.md` carries the block). The
protocol defines three tiers:

| Tier | Machine class | Suites |
| --- | --- | --- |
| **A** | any machine with Node + a Rust toolchain (the root `cargo test` covers `vault-core` only — by design, the workspace `default-members`) | `npm run check`, `npm test`, `cargo test` |
| **B** | Linux with the webkit2gtk stack, or the release suite container (`scripts/release/`) | `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `xvfb-run -a npm run test:e2e` |
| **C** | packaged smoke — the machine class of the artifact being shipped (Linux: docker + `scripts/release/smoke-linux.sh`; the AppImage/deb install+launch gates) | `npm run tauri build` plus the release smoke scripts in [release.md](release.md) |

Which tier a change requires:

| Change class | Required tiers |
| --- | --- |
| Rust-only (`crates/vault-core`) | A, plus `cargo fmt --check` and `cargo clippy --workspace -- -D warnings` (they need no webkit2gtk — any tier A machine may run them) |
| Frontend (`src/`, `index.html`) | A, plus the vitest suites that cover the touched modules |
| UI shell / CSP / packaging (`src-tauri/`, `tauri.conf.json`, layout or iframe/security surface) | B in full (the e2e suite is the only automated driver of the real webview) **and** C: a packaged smoke while CI is down |

Rules of the record:

- Every PR fills the template's verification block from a real run on
  the PR branch: suites, machine class, commit, pass/fail counts.
- A leg this machine cannot run is written as **pending** with the tier
  that retires it — never silently omitted, never backfilled.
- Merging requires every tier the change class demands to be green in
  the record (or explicitly waived in review with the pending leg named
  as follow-up debt).

The post-merge snapshot of the 2026-11-14 UI wave under this protocol —
what ran, what stayed pending and on which machine class each pending
leg retires — is recorded in
[`docs/evidence/verification-2026-11-14.md`](evidence/verification-2026-11-14.md).

## Hermetic IMAP and OAuth testing

IMAP sync is tested hermetically against an in-process mock IMAP4rev1 server (the greenmail equivalent), including an integration test that ingests the same `.mbox` fixture through both paths and compares the archives. The OAuth2 login (Google sign-in → XOAUTH2) is tested the same way: an in-process mock OAuth server (`tests/oauth_mock`) plays the authorization + token endpoints (PKCE, single-use codes, refresh grants, consent denial), an HTTP client stands in for the browser following the real loopback redirect, and the mock IMAP server verifies the SASL `XOAUTH2` string — the whole token flow end-to-end, plus the no-persist scan for the refresh token. A real-server test (`tests/imap_gmail_manual.rs`) is `#[ignore]`d: it needs live credentials and is **not** run in CI. To run it against your own account:

```sh
MAILCHAT_IMAP_USER=you@gmail.com MAILCHAT_IMAP_PASS=abcd-efgh-ijkl-mnop \
  cargo test --test imap_gmail_manual -- --ignored --nocapture
```

It writes to a throwaway temp directory, never your real vault.

## UI testing strategy

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

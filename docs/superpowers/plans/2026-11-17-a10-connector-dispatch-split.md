# A10 public Gmail + Microsoft Graph connectors: blocked scope record and dispatch split

> **For agentic workers:** This document is a **blocked scope record and dispatch plan**, not an implementation. Package A10 of the [archive experience plan](../../archive-experience-plan.md) (section 10) stays **blocked** until the external provider prerequisites below exist. Do not implement any connector, OAuth registration, or provider communication before then. When the prerequisites are confirmed, a follow-up task picks up **exactly one** of the four split tickets (A10.1 first); each names its scope, out-of-scope list, done-when checks, and the runner-verifiable test commands. Nothing in this record changes application behavior.

Date: 2026-11-17. Baseline inspected: `main` at `8950699` (seams re-verified against this tree; the plan itself was authored against `4596a7e` — see the [investigation evidence](../../evidence/archive-experience-investigation-2026-11-17.md)).

Note on numbering: this is package **A10 of the archive experience plan**. Older documents (`implementation-roadmap.md`, `validation-and-release.md`) use "A10" for an unrelated M4-era ZIP-boundary acceptance ID; do not conflate them.

## Why blocked — prerequisites (user action, external lead time)

None of these are repository changes. All are owned by the **provider-account owner** named in the plan's delivery table, and all must be confirmed and recorded **before any ticket in this package is dispatched**:

- [ ] **Google Cloud project** with a registered **public desktop OAuth client** (the app's own client, not the BYO-client route).
- [ ] **Written verification determination for `gmail.readonly`** — a restricted scope. The determination must explicitly cover the **optional remote AI path** (assessment scope depends on server access to restricted data), and must record its sources and date. Promise neither an automatic local-app exemption nor an unavoidable paid assessment; the [investigation](../../evidence/archive-experience-investigation-2026-11-17.md#external-facts-and-corrections) corrected earlier analysis on exactly this point.
- [ ] **Microsoft Entra public desktop app registration** with delegated `Mail.Read`.
- [ ] **Personal and work account consent paths confirmed** for that registration (including observed tenant-consent restriction behavior for work accounts).

Until all four hold, the existing routes stay exactly as they are: the BYO-client OAuth and app-password paths in [imap-sync](../../imap-sync.md) remain clearly labeled **advanced setup** and must never be presented as one-click onboarding (current labeling verified at `docs/imap-sync.md` "Advanced setup"; no change needed or made by this record).

Prerequisite timing is outside repository control and unbounded. The file-import path is the supported consumer route in the meantime; nothing in this package blocks it.

## Standing constraints for every split ticket

These carry over from the plan (section 7) and the package spec; each ticket restates the ones it touches:

1. **Dispatch gate:** no ticket dispatches before the prerequisites above are confirmed by the provider-account owner.
2. **Per-batch atomic commit:** records, occurrences, failures, and the checkpoint commit **together** — the IMAP contract already proves this shape (`commit_batch` in `crates/vault-core/src/imap.rs:636`, tested by `crates/vault-core/tests/imap/atomic_batches.rs`).
3. **Remote deletion changes provider-presence evidence only.** It never automatically removes archived content.
4. **Opaque provider checkpoints.** Gmail history cursors and Graph delta state must **not** be forced into the IMAP `UIDVALIDITY`/`last_uid` shape (`sync_checkpoints` columns; `get_imap_checkpoint` at `crates/vault-core/src/storage.rs:2999`).
5. **Mock/live separation from the start.** Live-account tests never run on the runner; they follow the `#[ignore]`d manual pattern of `crates/vault-core/tests/imap_gmail_manual.rs` and the in-process mock servers in `crates/vault-core/tests/oauth_mock/` and `crates/vault-core/tests/imap_mock/`.
6. **Same ingest contract as file adapters** — the bounded import pipeline (`ingest.rs` `Source`, `bounded.rs` `StreamSource`/`IngestSink`), not a parallel path.
7. **Least necessary grants**; scope disclosure distinguishes what the app does from what the OAuth grant permits.

## Verified seams (shared by the split)

| Seam | Anchor (verified at `8950699`) | Use |
| --- | --- | --- |
| Adapter boundaries | `crates/vault-core/src/chat_export.rs:35` (`ChatFormat`), `crates/vault-core/src/mail.rs`, `ingest.rs:20` (`Source`), `bounded.rs:228`/`bounded.rs:330` (`StreamSource`/`IngestSink`) | A10.1 registry derives from these; connector tickets emit the same contract |
| OAuth client | `crates/vault-core/src/oauth.rs:85` (`GMAIL_SCOPE`), module docs on the BYO-client design and secret handling | A10.3's public client replaces the BYO constant **only in a new connector** — never by editing `GMAIL_SCOPE`, which IMAP login requires |
| Per-batch atomic commit | `imap.rs:615` (`SyncStorage`), `imap.rs:627`/`636` (`checkpoint`/`commit_batch`), `imap_coordinator.rs` (rounds over the job pool) | A10.2 generalizes this; A10.3/A10.4 reuse it |
| Checkpoint storage | `storage.rs:2999` (`get_imap_checkpoint`), `set_imap_state` → `sync_checkpoints` (IMAP-shaped columns) | A10.2 gives non-IMAP cursors an opaque home |
| Chooser wiring | `src/importview.ts:31`/`:55` (`formatLabel`/`formatHint`), `src/accountsview.ts` (account kind/credential labels) | A10.1 makes these derive from the registry instead of parallel hard-coded lists |
| Mock precedents | `tests/imap/atomic_batches.rs`, `tests/imap/partial_resume.rs`, `tests/imap/uidvalidity_reset.rs`, `tests/imap/oauth_sync.rs`, `tests/oauth_mock/mod.rs`, `tests/imap_mock/mod.rs`, `imap_mbox_equivalence.rs`, `imap_gmail_manual.rs` (`#[ignore]`) | The mock/live pattern every connector ticket copies |

---

## A10.1 — Connector capability contract (first dispatch; needs no live accounts)

**Goal.** One static registry of connector facts — provider/kind, accepted formats, auth mode, media support, identity quality, incremental support, guide version/date — so the source chooser and the support documentation derive their options from the same source of truth (plan section 7: "Connector capability contract").

**Scope.**
- A registry module in `crates/vault-core` (near `chat_export.rs`; no Tauri dependency) describing the **currently real** connectors: file adapters (MBOX/EML, WhatsApp, Telegram, Signal, Discord, Messenger, JSONL) and IMAP (BYO OAuth / app password). Gmail API and Graph entries are added by their own tickets, not here.
- Exposure through one IPC command following existing serialization conventions; `docs/data-and-api-contracts.md` updated in the same change.
- Chooser derivation: `formatLabel`/`formatHint` in `src/importview.ts` and the account-kind labels in `src/accountsview.ts` read from the registry facts instead of maintaining parallel hard-coded lists. `docs/features.md`/`docs/support-matrix.md` capability rows cite the same registry fields (guide version included).

**Out of scope.** Any network connector; Gmail API/Graph descriptors; provider communication; changing BYO-client labeling beyond what derivation produces; live accounts of any kind.

**Done when.**
- A contract test asserts the frontend chooser options are derived from (and in sync with) the registry — no parallel provider list can drift silently.
- Registry fields cover provider/kind, accepted formats, auth mode, media support, identity quality, incremental support, guide version.
- `cargo test` and `npm test` green; `npm run check` green.

**Runner checks.** `cargo test -p vault-core` + the affected vitest modules (`src/importview.test.ts`, `src/accountsview.test.ts`) + `npm run check`.

---

## A10.2 — Connector checkpoint and batch-commit seams (mock-tested; needs no live accounts)

**Goal.** Generalize the opaque provider checkpoint and the per-batch atomic commit out of the IMAP path, so Gmail history cursors and Graph delta state have a home that is not the IMAP `UIDVALIDITY` shape.

**Scope.**
- A connector-agnostic checkpoint store (opaque per-connector cursor bytes/type keyed by source instance, alongside — not replacing — `sync_checkpoints`) and a named batch-commit seam (records, occurrences, failures, checkpoint in one transaction) extracted from or wrapped around `SyncStorage` (`imap.rs:615`) and the `imap_coordinator.rs` round driver.
- IMAP behavior unchanged: the existing IMAP tests keep passing against the same semantics, whether IMAP migrates to the new seam in this ticket or stays on its proven path with the new seam proven alongside.

**Out of scope.** Any provider protocol; changing IMAP sync semantics; UI work; credentials handling.

**Done when.**
- Atomicity and resume tests modeled on `tests/imap/atomic_batches.rs` and `tests/imap/partial_resume.rs` pass for the generalized seam: a mid-batch fault rolls back the whole batch; a crash between batches leaves the checkpoint exactly at the boundary of what is durably stored; recovery works across a pool restart.
- A non-IMAP-shaped cursor round-trips through the new store without touching `uidvalidity`/`last_uid` columns.
- `cargo test` green, including the full existing IMAP suite.

**Runner checks.** `cargo test -p vault-core` (new integration test file plus the whole `tests/imap/` suite unchanged-green).

---

## A10.3 — Gmail API read-only connector (needs Google prerequisites)

**Goal.** Consumer Gmail sign-in over a **public** desktop OAuth client with the `gmail.readonly` scope, syncing through the Gmail API — full MIME and media parity with the IMAP path, and **history-cursor checkpoints** (history IDs), never IMAP `UIDVALIDITY`.

**Scope.**
- Public-client OAuth flow following the mock/live separation of `oauth.rs` / `tests/oauth_mock/` (PKCE, loopback redirect, refresh, revoke); scope requested is `gmail.readonly` — `GMAIL_SCOPE` in `oauth.rs:85` stays untouched for the IMAP path.
- Messages.get/list with MIME bodies and attachment media through the existing object store, producing the same ingest contract as the file adapters via A10.2's batch-commit seam.
- History cursor checkpointing; reconnect, revoked-grant, and token-expiry behavior surfaced as honest source states (never destroying a working credential on a transient failure).
- Capability-registry entry (A10.1) and guide (versioned) added in the same change.

**Out of scope.** IMAP changes; sending or mutating Gmail state; whole-Takeout ZIP support (that is A11 territory); live-account tests on the runner; the verification determination itself (a prerequisite, not a deliverable).

**Done when.**
- An in-process mock Gmail API + mock OAuth server covers: first full sync, incremental history sync, mid-batch fault rollback, cursor resume, reconnect after transport failure, revoked grant, expired access token with refresh.
- A parity test ingests the same fixtures through the Gmail API connector and the IMAP path and compares archives, following `imap_mbox_equivalence.rs`.
- A `#[ignore]`d live-account test exists in the `imap_gmail_manual.rs` pattern and is **not** run by any automated check.
- No automated check depends on a live provider account.

**Runner checks.** `cargo test -p vault-core`; registry derivation checks via `npm test` for the chooser entry.

---

## A10.4 — Microsoft Graph delegated connector (needs Microsoft prerequisites)

**Goal.** Outlook/Microsoft 365 mail over Graph **delegated** `Mail.Read` via a public desktop app registration, with personal and work account behavior tested separately.

**Scope.**
- Delegated token acquisition and refresh against a mock identity endpoint; folder traversal; message listing pagination and delta queries including **delta-link expiry/restart**; throttling (429/`Retry-After`) with bounded backoff; token refresh mid-sync.
- Same ingest contract and A10.2 batch-commit seams as A10.3; opaque delta-state checkpoints.
- **Separate personal-account and work-account mock fixtures**, including tenant-consent restriction (work account consent denied by policy must surface as an actionable state, not a generic failure).
- Capability-registry entry (A10.1) and versioned guide added in the same change.

**Out of scope.** Application permissions (mailbox-wide) — delegated only; sending/mutating mail; IMAP-to-Graph migration of existing sources; live-account tests on the runner; the Entra registration itself (a prerequisite).

**Done when.**
- The mock Graph server covers: folder traversal depth, page-walk completion, delta pagination, expired delta link restart without re-importing duplicates (idempotent ingest), throttled responses with backoff, token expiry mid-round, and both consent fixtures (personal; work-with-tenant-restriction).
- Batch atomicity/resume tests pass on the shared seam as in A10.3.
- A `#[ignore]`d live test follows the manual pattern; no automated check depends on a live account.

**Runner checks.** `cargo test -p vault-core`; `npm test` for chooser derivation.

---

## Dispatch order, independence, and further splitting

- **Order once unblocked:** A10.1 → A10.2 → A10.3 ∥ A10.4. A10.1 and A10.2 need no live accounts and no provider registrations; they are still gated behind the package prerequisites by this record's constraint 1, and are the natural first dispatches because they are the cheapest and unblock both connectors.
- **Mergeability:** each ticket merges independently. A10.3 and A10.4 depend on A10.1 (registry) and A10.2 (checkpoint/batch seams) but not on each other; Gmail-first is the recommended pairing if capacity is serialized.
- **Run-budget splitting:** A10.3 and A10.4 are the largest slices in this package. At dispatch, if either exceeds a single implementation run, split it into **core loop** (auth + fetch + ingest + checkpoint, mock-tested) and **hardening** (throttling/expiry/reconnect edge matrix, parity fixtures, docs) — each still merging independently against the same seams.
- Each dispatched ticket carries its own out-of-scope list; **anything not in its numbered scope above starts out of scope.**

## Acceptance shape for every dispatched ticket

- Rust mock-based tests cover the connector's behavior and run on the runner with `cargo test`; frontend derivation checks run with `npm test`; `npm run check` stays green.
- **No automated check depends on a live provider account.** Live-account, consent-path, and verification-determination evidence is recorded as **manual verification** following the interim per-PR protocol in [testing.md](../../testing.md#interim-per-pr-verification-while-ci-is-down), with each pending leg named and the machine class that retires it.
- E2e/WebKitGTK legs (if any UI journey is added) are recorded as pending tier-B/C follow-ups, never silently omitted.

## Risks

| Risk | Handling recorded by this split |
| --- | --- |
| Prerequisite timing unbounded and outside repository control | Package stays blocked; file-import path unaffected; prerequisites are checkboxes above, owned by the provider-account owner |
| The Google determination must assess the optional remote AI path and could reopen that architecture decision | Recorded as an explicit prerequisite bullet; A10.3 is not dispatched on a determination that skipped it |
| A10.3/A10.4 exceed a single run budget | Pre-agreed core-loop/hardening split (above) instead of scope erosion |
| Personal vs. work account divergence only mock-verifiable on the runner | Separate fixtures in A10.4; live consent-path evidence is manual verification, never a runner check |

## Status of this record

Documentation-only change accompanying this ticket: this file, plus a pointer from the plan's A10 delivery-table row. No application code, connector, OAuth registration, or provider communication was implemented or contacted; the existing BYO-client routes and their advanced-setup labeling are unchanged. `npm test`, `npm run check`, and `cargo test` are expected to pass unchanged on this branch (they were run and recorded in the PR verification block).

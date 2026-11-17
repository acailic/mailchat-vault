# Gmail OAuth scope analysis — keep BYO-client as advanced setup (2026-11-15)

Status: **analysis and recommendation recorded** against [#82](https://github.com/acailic/mailchat-vault/issues/82). The launch decision authority is the working go-public plan (kept outside this repository per the 2026-11-14 decision, [docs index](../README.md)); this record is its in-repo evidence surface. Sources were fetched and read 2026-11-15.

## Question

Can `oauth_gmail_login` request less than the full mail scope (least privilege), and what would shipping a public OAuth client require? ([#82](https://github.com/acailic/mailchat-vault/issues/82), go-public review finding S7.)

## Findings

### 1. Gmail XOAUTH2 requires the full restricted scope — read-only commands do not reduce it

Google's XOAUTH2 documentation ([S7](https://developers.google.com/workspace/gmail/imap/xoauth2-protocol#oauth_20_scopes), reviewed 2026-11-15) states:

> The scope for IMAP, POP, and SMTP access is `https://mail.google.com/`.
> - To be approved, your app must show full utilization of `https://mail.google.com/`.
> - If your app does not require `https://mail.google.com/`, migrate to the Gmail API and use more granular restricted scopes.

The SASL error example in the same document shows the refusal shape: a token whose scope does not match gets a 401 challenge whose `scope` field names `https://mail.google.com/` back — i.e. the scope is checked at `AUTHENTICATE`, before any command runs.

The vault's IMAP usage is already strictly read-only (`EXAMINE`, `UID SEARCH`, `UID FETCH` — [imap.rs](../../crates/vault-core/src/imap.rs), asserted by the hermetic suites). But the OAuth *grant*, not the command set, defines the permission: the consent grants read-write mail access either way, and Gmail's endpoint refuses narrower tokens outright.

### 2. No scope-reduction code change is warranted on the IMAP path

Changing [`GMAIL_SCOPE`](../../crates/vault-core/src/oauth.rs) to a narrower URL (e.g. `https://www.googleapis.com/auth/gmail.readonly`) would break every Google sign-in at SASL auth. The constant is now pinned by a unit test with the rationale, so a well-meant least-privilege patch cannot silently break login. This is the deliberate "no code change" outcome of the analysis, not an omission.

### 3. The only real least-privilege path is a Gmail API connector — a separate project

`https://www.googleapis.com/auth/gmail.readonly` is a granular *Gmail API* scope. Using it means a second sync engine (REST `messages.list`/`messages.get` pagination instead of the UIDVALIDITY/UID cursor model, different batching, failure taxonomy, and media parity) — not a scope tweak. It is the verification-friendly public path (see finding 4), and by that fact a separate project ([#82](https://github.com/acailic/mailchat-vault/issues/82)).

### 4. A verified public client with the full mail scope is doubly blocked for this app

- **Restricted-scope verification:** `https://mail.google.com/` is a restricted scope. A public client (credentials shipped in the app) must complete OAuth verification, which for restricted scopes includes a third-party security assessment (CASA tier 2 or above via an authorized lab; annual recertification), consent-screen branding, a privacy policy and home page, and a demo video — multi-week lead time at best, with lab cost. ([Verification requirements](https://support.google.com/cloud/answer/13464321), [security assessment](https://support.google.com/cloud/answer/13465431), [restricted scopes](https://support.google.com/cloud/answer/13464325).)
- **Full-utilization requirement:** S7 says approval requires showing *full utilization* of the full mail scope. An archive client whose IMAP session is deliberately `EXAMINE`-only cannot show full utilization; Google's own guidance directs such apps off IMAP to the Gmail API. A read-only IMAP client is therefore a weak verification case even after paying the assessment.

By contrast, the current BYO-client design needs none of this: Google exempts **personal-use apps with fewer than 100 users** from verification (users click through the "unverified app" screen on their own project), and each user consents on a client they own ([when verification is not needed](https://support.google.com/cloud/answer/13464323)).

## Decision (recorded; product-owner sign-off tracked in #82)

1. **Keep Google sign-in as the labeled advanced, BYO-client setup** — status quo, no new code path advertised.
2. **Do not advertise one-click public Gmail access.** Reviewed README, `index.html`, the first-run/Add-source UI strings, and `docs/imap-sync.md`: none promises public/one-click Gmail; the unconfigured state honestly names the two env vars (product-review F04 already records this as intended).
3. **A Gmail-API read-only connector or a verified public client is a separate project**, to be scoped only if consumer Gmail onboarding becomes a goal. Not a prerequisite for the file-import pilot.
4. The scope constant stays the full mail scope, pinned by test (finding 2).

## USER steps if a public client is ever pursued (outside this repository)

These are Google-console/account actions with external lead time; none can be done from the repo or this runner:

1. Own a Google Cloud project with an OAuth client for the app; configure the consent screen (app name, logo, home page, privacy policy URL) and set publishing status to *In production*.
2. Submit the OAuth verification request declaring the restricted scope `https://mail.google.com/` (or build the Gmail-API connector and declare `gmail.readonly` instead — the stronger case).
3. Complete the security assessment (CASA tier 2+ via an authorized lab; self-scan route may qualify at tier 2) and the annual recertification.
4. Provide the demo video and respond to reviewer follow-ups; budget multi-week lead times and possible rejection on the full-utilization grounds above.
5. Repo-side (follow-up task, not this change): ship the client credential path (env fallback → packaged credential or token-broker) and update the consent/honesty copy and tests.

## Evidence trail

| Claim | Source |
| --- | --- |
| IMAP/POP/SMTP scope is exactly `https://mail.google.com/`; full-utilization requirement; migrate-to-Gmail-API guidance | [XOAUTH2 protocol doc, OAuth 2.0 scopes](https://developers.google.com/workspace/gmail/imap/xoauth2-protocol#oauth_20_scopes) (fetched 2026-11-15) |
| Scope checked at SASL auth; error challenge names the required scope | same document, SASL error-response sections |
| Personal-use (<100 users) apps exempt from verification | [When verification is not needed](https://support.google.com/cloud/answer/13464323) (fetched 2026-11-15) |
| Restricted-scope verification includes CASA assessment, annual recertification | [Verification requirements](https://support.google.com/cloud/answer/13464321) · [Security assessment](https://support.google.com/cloud/answer/13465431) · [Annual recertification](https://support.google.com/cloud/answer/13463816) |
| Vault issues only `EXAMINE`, `UID SEARCH`, `UID FETCH` | [imap.rs](../../crates/vault-core/src/imap.rs) sync path; hermetic IMAP suites |
| Scope constant pinned; BYO rationale in code | [oauth.rs](../../crates/vault-core/src/oauth.rs) `GMAIL_SCOPE` + tests |
| No one-click/public Gmail claim in UI or docs | README, `index.html`, `src/main.ts` sign-in hints, `docs/imap-sync.md`, [product-review F04](../internal/product-review.md) |

## Local verification (interim per-PR protocol, [testing](../testing.md#interim-per-pr-verification-while-ci-is-down))

Change class: Rust-only in `crates/vault-core` (doc comments + one unit test) plus documentation — required tier A plus `fmt`/`clippy`. Machine: this CI-down runner (Linux x86_64; no Docker, no sudo, no WebKitGTK, no system C toolchain). Rust ran user-local (rustup minimal stable) with the same toolchain substitution as the [2026-11-15 UI-wave record](ui-wave-local-verification-2026-11-15.md): `zig cc` 0.17.1 (sha256-verified tarball) as compiler/linker and rustup's `llvm-ar` — bundled SQLite compiled by it.

| Suite | Command | Result |
| --- | --- | --- |
| TypeScript type-check | `npm run check` | pass (clean) |
| Frontend unit tests | `npm test` | pass — 19 files, 605/605 |
| vault-core lib tests | `cargo test -p vault-core --lib` | pass — 263/263, incl. the new `gmail_scope_is_exactly_the_full_mail_scope_google_imap_requires` |
| OAuth flow integration | `cargo test -p vault-core --test oauth_flow` | pass — 10/10 |
| Full vault-core suite | `cargo test -p vault-core --no-fail-fast` | **46 binaries green; one pre-existing failure (below)** |
| Formatting | `cargo fmt -p vault-core -- --check` | `oauth.rs` clean; pre-existing drift in untouched files (below) |
| Lint | `cargo clippy -p vault-core --all-targets -- -D warnings` | `oauth.rs` clean; 3 pre-existing warnings in untouched files (below) |

Not run (pending a tier B/C machine or restored CI, [#80](https://github.com/acailic/mailchat-vault/issues/80)): `cargo test --workspace` and `clippy --workspace` (`src-tauri` needs webkit2gtk), the e2e suite, packaged smoke.

### Pre-existing main-branch findings surfaced by this run (not caused by this change)

1. **`tests/integrity` fails on pristine `main` (`d5d86e3`)** — verified in a clean worktree. `canonical::old_schema_fixture_migrates_without_loss` asserts the applied-migration list is `[1..=16]` while asserting `schema_version() == 17` three lines above; migration 17 and the stale list landed together in `d5d86e3` (PR #140), so every tier-A run of `cargo test` on main fails 46/47 in that binary. Fix is a one-line expectation update; follow-up recommended.
2. **fmt/clippy drift in untouched files** (`jobs.rs`, `search2.rs`, `mbox.rs`, some tests) — this runner's newer rustfmt/clippy formats and lints them differently than they were authored; left alone per scope. `search2.rs`/`mbox.rs` are the same files finding 1's PR touched, so PR #140 likely never ran these gates (CI was already down).

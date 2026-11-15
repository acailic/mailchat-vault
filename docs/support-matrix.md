# Support matrix and open blockers (GP01)

Status: **current as of 2026-11-15**, baseline `origin/main` @ `71c162b`. Labels: **implemented** = code on main with recorded test evidence; **verified on candidate** = fresh-machine/installer-level evidence on a release artifact; **missing**; **blocked** = waiting on an external account, host, or decision. A passing unit test is not a fresh-machine installer result. The working launch plan is kept outside the repository (2026-11-14 decision, [docs index](README.md)); this matrix is its in-repo evidence surface — update it in the same change as any status-affecting slice.

## Claims corrected in this slice

| Where | Was | Now |
| --- | --- | --- |
| [README](../README.md) | "Everything stays on the machine. The only outbound traffic is optional AI-assisted search" | Archive and search stay local and work offline; connecting an account contacts the mail provider (Google sign-in contacts Google; IMAP sync is read-only); optional AI sends the query and top rows after explicit consent; no app encryption — OS account/disk protection at rest |
| [index.html](../index.html) (first-run) | "nothing leaves it" | Same disclosure in short form: search runs locally; connecting an account contacts the provider; AI sends only what the consent prompt shows |
| [tauri.conf.json](../src-tauri/tauri.conf.json) (installer description) | "nothing leaves the machine" | Read-only IMAP named; consent-gated AI named; no app encryption |
| [ai-search.md](ai-search.md) | "The product promise is 'nothing leaves the machine'" | Scoped to the ask pipeline: nothing leaves without the explicit AI consent |

Kept unchanged: the in-app AI consent note ("nothing leaves this machine" when asking locally / without consent) — that claim is scoped, accurate, and asserted by tests ([llmview.test.ts](../src/llmview.test.ts), [main.test.ts](../src/main.test.ts)).

## Capability status

| Capability | Status | Evidence | Caveats |
| --- | --- | --- | --- |
| Local FTS5 search, filters, v2 reading pane | implemented | M3.1/M3.2 records ([roadmap completion tracking](implementation-roadmap.md#completion-tracking)) | — |
| mbox import | implemented | [testing](testing.md) ingest suites | `.eml` + Messenger land with `feat/drop-import` (merged locally, not on `origin/main` at this baseline) |
| WhatsApp / Telegram / Signal / Discord exports | implemented | [testing](testing.md); media honesty counters (PR #70) | Telegram all-chats export unsupported by design (preview must identify it); media preserved only when present in the export |
| Import preview (bounded, native picker) | implemented (first slice) | M3 progress record; [features](features.md) | M3.3 remainders open: source/conversation association, date/timezone confirmation, in-flow progress, issue retry |
| IMAP sync (read-only), Gmail app-password + BYO-client OAuth | implemented | M2.3/M2.4 records; D10 amendment | OAuth stays labeled advanced setup (user-supplied client): the IMAP path cannot request less than the restricted full-mail scope ([2026-11-15 analysis](evidence/gmail-oauth-scope-2026-11-15.md)); a Gmail-API readonly connector or verified public client is a separate project → [#82](https://github.com/acailic/mailchat-vault/issues/82) |
| Scheduled sync, folder watching, new-mail notifications | implemented | D10/M6 records (#59) | — |
| Backup / staged restore / recovery | implemented (unit + integration) | M4.4 completion record (2026-11-07) | Not yet verified on a release candidate (G1 journey) → [#81](https://github.com/acailic/mailchat-vault/issues/81) |
| Export (CSV/JSON/MBOX) | implemented; v2-contract reconciliation in review | [testing](testing.md) export round-trips; PR #77 | — |
| Optional AI ask (consent gate, citations) | implemented | M5 record; zero-wire-request mock-endpoint proofs | Session-scoped consent is a recorded deviation; M5.1 persisted switch and M5.2 job-backed approval open |
| Local embeddings / hybrid ranking | implemented | [ai-search: local embeddings](ai-search.md#local-embeddings--hybrid-search-v1) | Offline; no model downloads |
| Media store / export / external open | implemented (code) | M4.1/M4.3; [features](features.md) | Acceptance records still open (roadmap M4 checkbox) |
| Background keep-alive (tray, autostart, single instance) | implemented | M7 / D10 amendment (2026-11-11) | Default off; GNOME-without-appindicator tray limitation recorded there |
| Three-platform installers + SHA256SUMS | implemented | D11 amendment; [release](release.md) | Unsigned → [#78](https://github.com/acailic/mailchat-vault/issues/78); execution route decided (route (c), 2026-11-14) but blocked — the Release run on tag `v0.2.0` failed because hosted runners cannot start (account payments/spending limit), no draft exists → [#80](https://github.com/acailic/mailchat-vault/issues/80); native host evidence → [#81](https://github.com/acailic/mailchat-vault/issues/81) |
| License / CONTRIBUTING / SECURITY | implemented | LICENSE, CONTRIBUTING.md, SECURITY.md merged in PR #84; `GPL-3.0-only` declared in Cargo.toml (D13) | tests/retrieval/Cargo.toml and tests/perf/Cargo.toml still declare `UNLICENSED` |
| Artifact attestations / authenticated provenance | missing | review finding S6 (attestations need an executing workflow and eligible plan) | Depends on [#80](https://github.com/acailic/mailchat-vault/issues/80) |

## Open blockers

| Issue | Blocker | Blocks |
| --- | --- | --- |
| [#78](https://github.com/acailic/mailchat-vault/issues/78) | Signing/notarization accounts and eligibility (Apple enrollment; Windows route) | G1 |
| [#79](https://github.com/acailic/mailchat-vault/issues/79) | Rights audit (dependency and asset inventory, public-history scan); the license decision itself is recorded as D13 in [decisions](decisions.md) | G0 |
| [#80](https://github.com/acailic/mailchat-vault/issues/80) | Release execution route: route (c) decided 2026-11-14 (Actions re-enabled), but hosted runners are blocked by an account payments/spending limit — the Release run on tag `v0.2.0` (43538226337) failed and no draft exists; publish handoff in [release.md](release.md#v020-publish-handoff-maintainer-checklist) | Release evidence, provenance |
| [#81](https://github.com/acailic/mailchat-vault/issues/81) | Native macOS/Windows verification hosts | G1 |
| [#82](https://github.com/acailic/mailchat-vault/issues/82) | Gmail public OAuth client decision — 2026-11-15 [scope analysis](evidence/gmail-oauth-scope-2026-11-15.md) recommends keeping BYO-client advanced setup (no scope reduction possible on IMAP; public-client verification incl. CASA assessment is a USER step with multi-week lead time); product-owner sign-off pending | Consumer Gmail onboarding (not the file-import pilot) |

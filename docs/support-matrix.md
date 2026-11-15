# Support matrix and open blockers (GP01)

Status: **current as of 2026-11-15**, baseline `origin/main` @ `d5d86e3` (execution-route rows updated by the #80 decision change). Labels: **implemented** = code on main with recorded test evidence; **verified on candidate** = fresh-machine/installer-level evidence on a release artifact; **missing**; **blocked** = waiting on an external account, host, or decision. A passing unit test is not a fresh-machine installer result. The working launch plan is kept outside the repository (2026-11-14 decision, [docs index](README.md)); this matrix is its in-repo evidence surface — update it in the same change as any status-affecting slice.

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
| Import preview (bounded, native picker) | implemented (first slice) | M3 progress record; [features](features.md) | GP03 (2026-11-15) landed the confirmation card (assumed-zone confirm/correct once per run, DST folds/gaps stated), explicit destination-source choice, failed-import retry, completion recovery actions, and format help on the first-run card + add-source chooser. Open remainders: per-entry DST pickers, day/month date-order choice, conversation-level user linking, mail naive Date headers (still UTC-assumed) |
| GP03 five observed first-source sessions | **open (human gate)** | [first-session kit (2026-11-15)](evidence/first-session-kit-2026-11-15.md) | Five sessions with real exports must be run by humans using the kit; scripted fixture journeys (e2e 31) are not participant evidence |
| IMAP sync (read-only), Gmail app-password + BYO-client OAuth | implemented | M2.3/M2.4 records; D10 amendment | OAuth stays labeled advanced setup (user-supplied client): the IMAP path cannot request less than the restricted full-mail scope ([2026-11-15 analysis](evidence/gmail-oauth-scope-2026-11-15.md)); a Gmail-API readonly connector or verified public client is a separate project → [#82](https://github.com/acailic/mailchat-vault/issues/82) |
| Scheduled sync, folder watching, new-mail notifications | implemented | D10/M6 records (#59) | — |
| Backup / staged restore / recovery | implemented (unit + integration); candidate-proof kit ready | M4.4 completion record (2026-11-07); one-command candidate proof: [verify-export-restore.sh](../scripts/release/verify-export-restore.sh) (GP04, runbook: [validation and release](validation-and-release.md#export-and-restore-candidate-proof-gp04)) | Not yet verified on a release candidate — flips only on a green recorded run of that procedure; native-host evidence → [#81](https://github.com/acailic/mailchat-vault/issues/81) |
| Export (CSV/JSON/MBOX) | implemented (v2-contract reconciliation landed, PR #77); candidate-proof kit ready | [testing](testing.md) export round-trips; PR #77; candidate proof runs the same procedure as backup/restore above | Verified-on-candidate only after a green recorded run of [verify-export-restore.sh](../scripts/release/verify-export-restore.sh) |
| Optional AI ask (consent gate, citations) | implemented | M5 record; zero-wire-request mock-endpoint proofs | Session-scoped consent is a recorded deviation; M5.1 persisted switch and M5.2 job-backed approval open |
| Local embeddings / hybrid ranking | implemented | [ai-search: local embeddings](ai-search.md#local-embeddings--hybrid-search-v1) | Offline; no model downloads |
| Media store / export / external open | implemented (code) | M4.1/M4.3; [features](features.md) | Acceptance records still open (roadmap M4 checkbox) |
| Background keep-alive (tray, autostart, single instance) | implemented | M7 / D10 amendment (2026-11-11) | Default off; GNOME-without-appindicator tray limitation recorded there |
| Three-platform installers + SHA256SUMS | implemented | D11 amendments; [release](release.md) | Unsigned → [#78](https://github.com/acailic/mailchat-vault/issues/78); execution route decided (2026-11-15 amendment, [#80](https://github.com/acailic/mailchat-vault/issues/80)): **local native authorized now** (Linux on any docker machine; macOS/Windows wait on hosts → [#81](https://github.com/acailic/mailchat-vault/issues/81)), hosted the default once runners can start again — the `v0.2.0` Release run (43538226337) failed on billing and no draft exists; handoff in [release.md](release.md#v020-publish-handoff-maintainer-checklist) |
| License / CONTRIBUTING / SECURITY | implemented | LICENSE, CONTRIBUTING.md, SECURITY.md merged in PR #84; `GPL-3.0-only` declared in Cargo.toml (D13) | tests/retrieval/Cargo.toml and tests/perf/Cargo.toml still declare `UNLICENSED` |
| Artifact attestations / authenticated provenance | missing (planned; preconditions recorded) | [release.md → Release execution route](release.md#release-execution-route-decided-2026-11-15-80) (review finding S6) | Needs an executing workflow (Actions billing restored — [#80](https://github.com/acailic/mailchat-vault/issues/80); interim protocol in [testing](testing.md#interim-per-pr-verification-while-ci-is-down)) **and** an eligible repository plan (public repo, or an eligible plan for a private one; the public move waits on [#79](https://github.com/acailic/mailchat-vault/issues/79)) before any workflow wiring; `SHA256SUMS` is the integrity baseline until then |

## Open blockers

| Issue | Blocker | Blocks |
| --- | --- | --- |
| [#78](https://github.com/acailic/mailchat-vault/issues/78) | Signing/notarization accounts and eligibility (Apple enrollment; Windows route) | G1 |
| [#79](https://github.com/acailic/mailchat-vault/issues/79) | Rights audit (dependency and asset inventory, public-history scan); the license decision itself is recorded as D13 in [decisions](decisions.md) | G0 |
| [#80](https://github.com/acailic/mailchat-vault/issues/80) | Release execution route: **decided 2026-11-15 (D11 amendment)** — local native builds authorized per OS while hosted runners are billing-blocked (macOS/Windows wait on [#81](https://github.com/acailic/mailchat-vault/issues/81) hosts); hosted route resumes as the default once runners start; attestations planned separately (executing workflow + eligible plan); decision and both-route handoff in [release.md](release.md#release-execution-route-decided-2026-11-15-80) | Release evidence, provenance |
| [#81](https://github.com/acailic/mailchat-vault/issues/81) | Native macOS/Windows verification hosts | G1 |
| [#82](https://github.com/acailic/mailchat-vault/issues/82) | Gmail public OAuth client decision — 2026-11-15 [scope analysis](evidence/gmail-oauth-scope-2026-11-15.md) recommends keeping BYO-client advanced setup (no scope reduction possible on IMAP; public-client verification incl. CASA assessment is a USER step with multi-week lead time); product-owner sign-off pending | Consumer Gmail onboarding (not the file-import pilot) |

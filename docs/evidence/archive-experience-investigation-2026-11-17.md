# Archive experience investigation — 2026-11-17

Status: **source investigation and planning evidence**, not fresh application-test or release evidence.

## Baseline and method

- Repository HEAD: `4596a7ebe257b0fe1f2316b03c8ee4e6b1f91581`.
- Existing modified files at intake: `index.html`, `src/__snapshots__/main.test.ts.snap`, `src/render2.test.ts`, `src/render2.ts`, `src/styles/app.css`, `src/styles/tokens.css`; `.codex/` and `.impeccable/` were untracked. These edits were inspected where relevant and not changed by this task.
- Read source, specifications, prior native-review evidence, and relevant test definitions. Read issues #170, #112 and #108 through `gh-axi`; no remote mutations.
- Used the agent-reach skill's Exa route first; it was unavailable (`Unknown MCP server 'exa'`). Used the available web tool to check official provider/SQLite documentation instead.
- No real vault was opened or changed, no provider account was connected, and no native UI/installer session or application test suite was run. Historical screenshots and recorded passing tests are not fresh verification.

Deliverable: [product and architecture plan](../archive-experience-plan.md).

## Code evidence

| Finding | Inspected evidence | Architectural consequence |
| --- | --- | --- |
| Existing desktop stack is suitable | [package.json](../../package.json), [Cargo workspace](../../Cargo.toml), [Tauri crate](../../src-tauri/Cargo.toml) | Keep Tauri 2, TypeScript and Rust; no rewrite justified by the requested outcomes |
| Search already spans canonical source instances | [search2.rs](../../crates/vault-core/src/search2.rs): `SearchArchiveRequest`, `search_archive`; [main.ts](../../src/main.ts): filter/result wiring | Extend the existing retrieval contract for People/Files; preserve snapshot/revision behavior |
| Navigation already prioritizes Archive/Sources/Settings | [index.html](../../index.html), [workspace.ts](../../src/workspace.ts), [state.ts](../../src/state.ts) | People/Files fit under Archive; avoid duplicating source/settings navigation |
| Vault location is not user-selectable at startup | [Tauri setup](../../src-tauri/src/lib.rs): `app.path().app_data_dir()`, `DbPool::spawn(data_dir.join("vault.db"))`; media protocol and commands also resolve app-data paths | Centralize `VaultSession` before adding a folder picker; path substitution alone is insufficient |
| Location and recovery are already visible | [backupview.ts](../../src/backupview.ts): `RecoveryStatusView.data_dir`; [main.ts](../../src/main.ts): recovery controls | Improve the existing Storage & backup journey rather than introduce a second recovery UI |
| Safe backup/restore foundation exists | [backup.rs](../../crates/vault-core/src/backup.rs), [safe_migration.rs](../../crates/vault-core/src/safe_migration.rs), [backup tests](../../crates/vault-core/tests/backup.rs) | Extend snapshot/object verification and journal recovery for portable location and new relationships |
| Bounded shared import pipeline exists | [chat_export.rs](../../crates/vault-core/src/chat_export.rs), [bounded.rs](../../crates/vault-core/src/bounded.rs), [preview.rs](../../crates/vault-core/src/preview.rs), [import_bounds.rs](../../crates/vault-core/src/import_bounds.rs) | Provider guides and adapters should converge on shared preview/commit/receipt behavior |
| Source variants are narrower than provider names | `ChatFormat` includes WhatsApp, Telegram, Signal, Discord, Messenger and JSONL; mail enters through MBOX/EML; no Skype/Slack adapters found | Do not advertise arbitrary provider exports or every Google Takeout ZIP as supported |
| Message identity is not person identity | [storage.rs](../../crates/vault-core/src/storage.rs): `message_identities`, `message_occurrences`, `archive_messages.author`; [search2.rs](../../crates/vault-core/src/search2.rs): author substring filter | Add participant identities and confirmed links separately; preserve dedup and conversation IDs |
| Media reference storage is mixed | [media.rs](../../crates/vault-core/src/media.rs), [object_store.rs](../../crates/vault-core/src/object_store.rs), `storage.rs` migration 13 `media_objects` | Gallery requires a complete indexed projection over linked and unlinked references |
| Recent terminal jobs persist | `storage.rs` migration 16 `job_ledger`; [jobs.rs](../../crates/vault-core/src/jobs.rs) | Older prose saying all history vanishes on restart is incomplete; use durable source receipts for long-lived health rather than treating bounded history as permanent coverage |
| Tray/scheduler already exist | [tray.rs](../../src-tauri/src/tray.rs), [scheduler.rs](../../crates/vault-core/src/scheduler.rs), [watch.rs](../../crates/vault-core/src/watch.rs) | Add status/reminders through existing jobs; no separate daemon needed |
| Tray status is basic | `tray.rs`: static tooltip and Show / Sync now / Quit | Last update/backup health is an enhancement, not a present feature |
| Gmail OAuth grants differ from app behavior | [oauth.rs](../../crates/vault-core/src/oauth.rs): `GMAIL_SCOPE`; [imap.rs](../../crates/vault-core/src/imap.rs): read-only mail operations | Keep scope disclosure accurate; public read-only Gmail API is a distinct connector |
| Export is not uniformly lossless archival preservation | [export.rs](../../crates/vault-core/src/export.rs): CSV/JSON/MBOX and reconstructed email; [mail.rs](../../crates/vault-core/src/mail.rs): normalized headers/body and bounded retained HTML | Describe export fidelity and original-retention policy explicitly |
| Email rendering already exists | [render2.ts](../../src/render2.ts): `htmlOf`, Rendered/Raw controls, sandboxed iframe with remote-resource restrictions | Preserve safe rendering; improve mail/chat presentation without calling it an absent capability |
| Orchestration is concentrated | `main.ts` 3,508 lines; `src-tauri/src/lib.rs` 4,405; `storage.rs` 4,711 at inspection | Extract feature/lifecycle responsibilities incrementally; line counts are context, not a quality metric |

## Tracker reconciliation

| Issue read on 2026-11-17 | Remote state | Code evidence | Next action proposed |
| --- | --- | --- | --- |
| [#170 — People](https://github.com/acailic/mailchat-vault/issues/170) | Open | No people/link/timeline domain found; issue requires explicit reversible associations and preservation of source evidence | Use as the People implementation scope; extend contracts before migration |
| [#112 — Signal media](https://github.com/acailic/mailchat-vault/issues/112) | Open, body says extraction missing | `media.rs::extract_signal`, bounded import dispatch, `media_extract.rs` Signal attachment/ZIP tests present | Run acceptance against the inspected revision; document remaining mismatch before any closure |
| [#108 — honesty filters](https://github.com/acailic/mailchat-vault/issues/108) | Open, body says unimplemented | `SearchArchiveRequest.time_assumed`, `grouping_unverified`, `files_missing`; frontend controls/wiring present | Verify query semantics, counts, export parity and UI behavior; reconcile status rather than duplicate work |

No issue was closed, edited or created. Other issue references inherited from existing documents, such as #82 and release blockers, were not freshly verified remotely; treat their current state as unknown until implementation intake.

## External facts and corrections

Official pages checked on 2026-11-17. These findings inform design; they do not certify provider approval or account eligibility.

| Question | Finding and source | Effect on this plan |
| --- | --- | --- |
| Gmail minimum IMAP OAuth scope | IMAP uses `https://mail.google.com/`; Google recommends Gmail API granular scopes when full scope is unnecessary. [Official protocol](https://developers.google.com/workspace/gmail/imap/xoauth2-protocol) | Do not replace the scope constant without replacing the connector |
| Does read-only Gmail avoid restricted verification? | `gmail.readonly` remains restricted. [Scope reference](https://developers.google.com/workspace/gmail/api/auth/scopes) | Narrower privilege does not remove the verification workstream |
| Is CASA automatic for every desktop archive? | Google's requirements distinguish verification from assessment and connect assessment to server access to restricted data. [Verification guidance](https://developers.google.com/identity/protocols/oauth2/production-readiness/restricted-scope-verification) | Prior local analysis overstates unconditional assessment; optional remote AI complicates the architecture assessment. Obtain a provider determination; promise neither automatic exemption nor inevitable assessment |
| Can Outlook use delegated read access? | `Mail.Read` supports delegated personal-account access; organizational consent policies also apply. [Permissions](https://learn.microsoft.com/en-us/graph/permissions-reference#mailread), [tenant policies](https://learn.microsoft.com/en-us/entra/identity/enterprise-apps/manage-app-consent-policies) | Plan public-client delegated access and test personal/work accounts separately |
| Is Skype shutdown an upcoming reminder? | Retirement May 5, 2025; published export deadline June 15, 2026. [Microsoft notice](https://support.microsoft.com/en-us/skype/skype-is-retiring-in-may-2025-what-you-need-to-know) | Existing-export import only; expired notices must not generate urgency |
| Can every Slack user export all chats? | Export authority and public/private/DM scope depend on role, plan and approvals. [Slack help](https://slack.com/help/articles/201658943-Export-your-workspace-data) | Explain export prerequisites and actual coverage, including file links versus bytes |
| WhatsApp supported user route | Official help describes chat export. [WhatsApp help](https://faq.whatsapp.com/1180414079177245/) | Retain manual export; validate platform-specific steps at guide implementation; no permanent claim about future APIs |
| Can an active SQLite WAL vault live anywhere? | WAL does not work over a network filesystem. [SQLite documentation](https://www.sqlite.org/wal.html) | Validate active storage separately from backup destinations |

The proposed connector protocols, table names, module layout, status model and delivery ordering are architectural recommendations. They are not claims that an external source prescribes the application design.

## Documentation drift to reconcile

1. [Support matrix](../support-matrix.md) is explicitly dated 2026-11-15, while the inspected tree includes later work. Keep its historical baseline visible; update capability rows during A0 with fresh verification.
2. [Architecture](../architecture.md) contains older current/proposed descriptions, including process-only job history. Migration 16 now provides a terminal-job ledger. Historical milestone text must not override inspected code.
3. [Gmail analysis](gmail-oauth-scope-analysis-2026-11-15.md) correctly rejects an IMAP scope-only change but treats assessment as unconditional. A dated qualification accompanies this plan; public-client requirements need assessment of the actual data flow.
4. Some older documents link to `gmail-oauth-scope-2026-11-15.md`; the existing filename is `gmail-oauth-scope-analysis-2026-11-15.md`. Broader documentation reconciliation remains A0, rather than silently rewriting historical records in this investigation.

## Validation of this deliverable

Documentation-only review: relative links in the new documents, Markdown fence balance, and whitespace checked locally. No application behavior, performance target, user-study threshold, or provider integration was newly proven by this task. See the plan's acceptance table for the work required before making those claims.

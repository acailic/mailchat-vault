# Product plan

Status: **proposed**. Audience: individual desktop users. Decision basis: the user's choices recorded in the [documentation index](README.md#authority-and-status).

The [2026-11-06 product review](product-review.md) separates working capabilities from adoption risks. Its [user success plan](user-success-plan.md) recommends an import-first pilot for a specific user hypothesis, with measurable outcomes and a [research protocol](user-research-playbook.md). These recommendations do not mark the release requirements below complete or remove the three-platform target.

## Purpose and user needs

Mailchat Vault helps a person find a past message, understand the exchange around it, and keep an independent copy of the available content. Its primary value is trustworthy recall across personal mail and chat sources.

The first user is someone with years of exports and one or more mail accounts. They should not need to know filesystem paths, FTS syntax, server UID terminology, or an LLM provider's environment variables to use the finished release.

| Need | Example | Successful outcome |
| --- | --- | --- |
| Locate an exchange | “What did Ana say about the delivery date?” | Search/filter reaches the original message and its real conversation |
| Consolidate history | “I exported this chat again.” | Existing messages remain stable; new messages and missing media are added |
| Preserve evidence | “The original attachment folder is gone.” | Copied available files still open from the vault |
| Trust a result | “Which account and export did this come from?” | Provenance and timestamp certainty are visible |
| Recover the archive | “I am moving to another computer.” | A validated backup restores text, relationships, and files without credentials |
| Ask optionally | “Summarize the messages about the trip.” | Explicit network disclosure, bounded context, and links to original evidence |

## Product principles

1. Preserve information before synthesizing it. Original messages, provenance, media, and uncertainty remain inspectable.
2. Keep local search useful without network access or AI configuration.
3. Never claim completeness when source files omit messages or media.
4. Make long operations observable, cancellable, and recoverable while reading remains available.
5. Keep server behavior read-only. Remote deletions and folder changes do not erase the local archive.
6. Prefer familiar desktop interactions and progressive disclosure over raw configuration on the main screen.

## Release boundary

The first polished release includes all milestones M1–M7 in the [roadmap](implementation-roadmap.md). Earlier internal builds may exercise completed slices, but are not the agreed complete release.

Included sources are mbox, WhatsApp text/media exports, Telegram single-chat JSON/media exports, Signal conversation JSON, Discord data packages, and TLS IMAP including Gmail app passwords and Google sign-in where the OAuth client is configured. The Google path currently requires developer configuration; its presence is not evidence of self-service consumer onboarding. Users can import a file, supported export directory, or ZIP through source-specific paths; the unified selection and preview flow remains required. A Telegram all-chats export is not implicitly supported: preview must identify an unsupported shape rather than report a successful empty import.

“Complete available media” means preserving attachments and files actually present in supplied exports or fetched mail. It includes copied originals, durable attachment links where evidence exists, honest missing-file states, storage reporting, backup, and restore. It does not promise to recover files omitted by an exporter or infer which message owns an unmatched file.

The release supports Linux x64, Windows x64, and macOS Apple Silicon/Intel. OS account permissions and disk protection secure the archive at rest; the app does not encrypt its database or backups. Credentials use the OS keychain, with visible session-only fallback. This limitation appears during setup and backup, not merely in developer documentation.

## Functional requirements

| ID | Requirement | Authoritative specification |
| --- | --- | --- |
| P01 | Stable source/account and conversation identity; repeated imports preserve IDs and provenance | [Contracts: identity](data-and-api-contracts.md#identity-and-deduplication) |
| P02 | Preview imports, confirm ambiguous dates/timezones, and report imported, existing, failed, and missing-file counts | [UX: import](ux-specification.md#import-and-first-success) |
| P03 | Search and browse with source, person, date, and attachment filters; retain selection and readable context | [UX: workspace](ux-specification.md#workspace-and-navigation) |
| P04 | Background import/sync with progress, cancellation, safe retry, and no network-held database lock | [Architecture: jobs](architecture.md#jobs-and-database-access) |
| P05 | Copy available originals/media, open/export safely, and report unavailable files | [Architecture: objects](architecture.md#originals-media-and-object-lifecycle) |
| P06 | Consistent backups, staged validation, restore recovery, and clear credential exclusions | [Architecture: recovery](architecture.md#backup-and-restore) |
| P07 | AI off until enabled; consent is session-scoped and never persisted (decided 2026-11-16, [#181](https://github.com/acailic/mailchat-vault/issues/181) — see the [decision record](#ai-consent-is-session-scoped-decided-2026-11-16-181)); per-question disclosure and per-answer context approval; citations open archived evidence | [UX: Ask](ux-specification.md#optional-ask) |
| P08 | Accessible keyboard operation and usable small-window behavior | [UX: accessibility](ux-specification.md#accessibility-and-interaction) |
| P09 | Install, upgrade, and use keychain/recovery workflows on all supported desktop targets | [Release validation](validation-and-release.md#platform-and-release-matrix) |
| P10 | Optional background keep-alive: close-to-tray, start at login, single instance — each opt-in and off by default, never a headless service | [D10 amendment](decisions.md#d10-read-only-mail-ingestion-and-opt-in-scheduling) |

Manual sync is the default. An opt-in 15-minute schedule runs while the app process is running, avoids overlapping jobs for one source, and does not accumulate missed runs while offline or asleep. On resume it schedules one catch-up run. The process normally lives as long as its window; with the opt-in keep-alive (P10) it can keep running from the tray between window closes and start at login, in which case the schedule, folder watching, and new-mail notifications span those closes. A second launch focuses the existing window instead of opening a second instance over the same vault. Disconnecting stops future sync and removes credentials; archived content remains unless the user separately deletes it.

## Success criteria

- A fresh user can select a supported export, understand the preview, and open a first imported message without typing a path.
- Five representative users complete import → search → conversation → attachment tasks; at least four finish each task without facilitator intervention. Record confusion and revise the affected journey before release.
- Reimporting unchanged data produces zero additional messages and zero duplicate object files. An overlapping export preserves deliberate repeated chat messages.
- Search and reading remain usable during slow sync and large imports; numerical targets and datasets are owned by [Validation](validation-and-release.md#performance-and-capacity).
- Backup/restore produces matching message, relationship, and object checksums; interruption never replaces a working vault with an unverified one.
- Every supported platform passes the release matrix. Missing signing credentials are an explicit release dependency, never evidence that release validation passed.
- No content reaches an AI provider when disabled, when consent is declined, or beyond the approved payload.

Assess success with local test evidence and voluntary usability sessions. Do not add analytics, cloud accounts, or message-content telemetry to measure these goals.

## Deferred work

Sending/replying, shared access, cloud synchronization, a mobile app, further connectors beyond the implemented inventory, OCR, transcription, attachment-content indexing, automatic updates, and app-managed vault encryption are deferred. A separate headless daemon/service binary for background archiving is a non-goal, not deferred work (see the [D10 amendment](decisions.md#d10-read-only-mail-ingestion-and-opt-in-scheduling)): background operation is the tray keep-alive of P10, never a second process over the vault. Binary content is preserved without claiming its contents are searchable.

Gmail OAuth, local hybrid ranking with hash-based vectors, automatic folder watching, Signal/Discord imports, and multi-provider configuration have already landed and are no longer classified as deferred. Their remaining onboarding, source-specific media, retrieval-quality and release gaps are assessed in the [product review](product-review.md) and [implementation assessment](project-assessment.md). Hash-based vector ranking is not a validated claim of synonym, cross-language or general semantic understanding.

AI refinement remains part of the release because AI already exists, but it must not delay correctness work or become necessary for ordinary retrieval. Settings support the existing compatible-provider integration; a provider marketplace and multi-provider comparison UI are deferred.

## Maintenance

The product owner changes scope here first, then updates affected UX, contracts, roadmap slices, and acceptance tests. A newly requested connector or cloud feature requires a fresh scope decision rather than silent expansion of this release.

## Decision records

Dated product decisions that settle an open scope question land here first, then propagate to the affected specifications.

### AI consent is session-scoped (decided 2026-11-16, #181)

M5.1 proposed a persisted off-by-default AI-consent switch in Settings. The shipped implementation instead made consent session-scoped — in-memory only, reset on every app start, never written to any storage — and the [M5 progress record](implementation-roadmap.md#completion-tracking) held the choice between the two open pending this decision. **Decided ([#181](https://github.com/acailic/mailchat-vault/issues/181)): session-scoped consent is the final release behavior; the persisted off-by-default switch is superseded, not deferred.**

- P07's "AI off until enabled" means off at *every* app start: each session begins ungranted, and the explicit per-session consent (per-question prompt, always-visible status line, mid-session Revoke) is the only way network asks are enabled. A consent can never survive a restart — stricter than the proposed switch, whose persisted grant could stay enabled indefinitely.
- No Settings surface, persisted state, schema, or code change follows: the switch was never built, and M5.1's other halves (endpoint/model configuration visibility, keychain/session key storage, environment-config compatibility) shipped with M5 (#48).
- Recorded with a dated [D09 amendment](decisions.md#d09-ai-is-optional-and-payload-approval-is-explicit) and the roadmap's M5.1 decision addendum; the [UX specification](ux-specification.md#optional-ask), [architecture](architecture.md#optional-ai-ask-layer), [contracts](data-and-api-contracts.md#search-request-and-response), and [assessment](project-assessment.md) carry supersession notes where they presented the switch as proposed. M5.1 closes; M5.2 (job-backed prepare/approve/generate) and the M5.3 cancellation/error/retry remainder stay open, so the M5 checkbox stays open.

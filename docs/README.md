# Mailchat Vault implementation documentation

**Product review, 2026-11-06:** the [product review](internal/product-review.md) assesses the installed app, user adoption risks, and current alternatives. The [user success plan](internal/user-success-plan.md) proposes prioritized work and measurable decision gates; the [research playbook](internal/user-research-playbook.md) defines the customer studies needed to validate them. These are evidence and recommendations, not completed implementation or validated demand.

Status: **product specification with the M1 and M2 slices implemented, the M3.1/M3.2 workspace/search halves recorded, the M5 consent/citation halves recorded, and the M6 release tracks recorded**. The specification set was authored and reviewed against the pre-roadmap baseline `a7671f0` (inspected 2026-11-01) and lands on `main` with the M1, M2, M3, M4, M5, and M6 documentation updates. [Project assessment](internal/project-assessment.md), the M1-owned contract sections — [identity](data-and-api-contracts.md#identity-and-deduplication), [time](data-and-api-contracts.md#time-semantics), [migration](data-and-api-contracts.md#migration-and-recovery), and the [persistence model](data-and-api-contracts.md#domain-and-persistence-model) — the M2-owned sections — [Architecture: jobs and database access](architecture.md#jobs-and-database-access) (including IMAP correctness) and the job-protocol half of [errors and events](data-and-api-contracts.md#errors-and-events) — the M3.1/M3.2-owned sections — the [workspace and navigation](ux-specification.md#workspace-and-navigation) and [search/selection](ux-specification.md#search-and-selection) halves of the UX specification plus the shipped v2 reading surface in the [IPC contracts](data-and-api-contracts.md#v2-ipc-surface) — the M4.1 halves — [Architecture: originals, media, and object lifecycle](architecture.md#originals-media-and-object-lifecycle) and [media and capacity](data-and-api-contracts.md#media-and-capacity) — the M5-owned halves — the consent/source-linked-answer part of [UX: optional Ask](ux-specification.md#optional-ask), [Architecture: optional AI ask layer](architecture.md#optional-ai-ask-layer), and the AI-retrieval half of the [search contracts](data-and-api-contracts.md#search-request-and-response) — and the M6-owned release grounding — the [platform and release matrix](validation-and-release.md#platform-and-release-matrix) evidence record, decision D11's amendment, and the [M6 progress record](implementation-roadmap.md#completion-tracking) (which also grounds the #59 new-mail notifications) — are grounded in the implemented code and the recorded release evidence (M2 complete at `57f40c8`, M3.1/M3.2 at #44/#46, the M4.1 object store at `1f2720c`, M5 at #48 `a0d79dc`, the release tracks at #17/#45/#47/#52 with the published `v0.1.0`, inspected 2026-11-05) and marked **current** within their documents. The M4.2 message-media code (#60) and M4.3 media display (#58) are in the tree ahead of their acceptance records. Everything still marked **proposed** is authorized design direction awaiting its slice. Creating these documents does not by itself implement the proposed features.

Mailchat Vault is a personal desktop archive for finding and understanding past email and chat messages. The next release prioritizes reliable retrieval, conversation context, available media preservation, and recovery across Linux, macOS, and Windows. AI is optional.

## Guides

Hands-on guides sit alongside the specification set:

| Guide | Purpose |
| --- | --- |
| [Feature reference](features.md) | Detailed feature documentation: sources and import, workspace, folder watching, background jobs, export, media lifecycle, chat-format notes |
| [AI-assisted search](ai-search.md) | LLM ask pipeline, AI consent, provider configuration, local embeddings and hybrid ranking |
| [IMAP sync](imap-sync.md) | Gmail (Google sign-in / app password) and generic IMAP setup, credential storage |
| [Testing](testing.md) | Test suites, performance budgets, fuzzing, CI, hermetic server setup |
| [Benchmark](benchmark.md) | Published performance results: 1M-message index time, search p50/p95/p99, methodology, and the CI regression gate |
| [Download & install](download.md) | Version × OS × architecture matrix, minimum requirements, checksum verification, limitations, manual upgrade path |
| [Distribution](distribution.md) | Package-manager kit (Homebrew, winget, Scoop, AUR, Flathub): version-bump script, per-manager owner publication steps, flatpak permissions, the signing gate (#78) |
| [Release](release.md) | Installers, local builds, smoke tests, checksums, publishing |
| [Native verification runbook](native-verification.md) | The G1/A14 procedure for real macOS/Windows verification hosts: exact commands per OS, evidence binding (artifact SHA-256, tester, date), and the templates native runs fill |
| [Support matrix](support-matrix.md) | Current capability and claim status with evidence links, plus open go-public blockers |
| [Security](security.md) | What the opt-in at-rest encryption (SQLCipher) does and does not protect: memory, swap, pre-migration backups, key custody |
| [Landing page](landing.md) | Deploy-ready public landing copy: the README's one-sentence promise and comparison table with a single call to action — one self-contained file with no external assets, paste into any static host |

## Reading order

Internal working documents (the product review, user success plan, research playbook, and the dated project assessments) live under [internal/](internal/README.md); the index there explains the split. Public-facing guides stay at this level.

| Document | Purpose | Maintained by |
| --- | --- | --- |
| [Product review](internal/product-review.md) | Strengths, observed UX defects, adoption risks, alternatives, and positioning hypotheses at the inspected 2026-11-06 baseline | Product owner |
| [User success plan](internal/user-success-plan.md) | Prioritized outcomes, work packages mapped to existing milestones, pilot metrics, and proceed/narrow/stop decisions | Product owner |
| [User research playbook](internal/user-research-playbook.md) | Recruitment criteria, interview and usability scripts, comparative retrieval study, and pilot readout templates | Product/research owner |
| [Project assessment](internal/project-assessment.md) | Current implementation, evidence, limitations, and ranked gaps | Engineer changing the assessed subsystem |
| [Product plan](product-plan.md) | Users, outcomes, release boundaries, and success criteria | Product owner |
| [UX specification](ux-specification.md) | Workspace, journeys, visual direction, and interaction states | Designer and frontend implementer |
| [Architecture](architecture.md) | Components, data flows, media lifecycle, and privacy boundaries | Core/backend implementer |
| [Data and API contracts](data-and-api-contracts.md) | Domain rules, persistence, migrations, IPC, and errors | Engineer changing a contract |
| [Implementation roadmap](implementation-roadmap.md) | Ordered, independently verifiable delivery slices | Implementer of each slice |
| [Validation and release](validation-and-release.md) | Test matrix, performance evidence, recovery, and packaging | Test/release owner |
| [Decisions](decisions.md) | Numbered decisions and their tradeoffs | Engineer revising a decision |

For product decisions, start with the product review and user success plan. For implementation, start with the project assessment and product plan, then read the roadmap entry and linked UX and contract sections. Mark a slice complete only with recorded validation evidence.

**Public launch copy is in the repository; launch strategy and monetization stay out.** The public front door — the README first screen (promise, demo GIF slot, download buttons, comparison table) and the deploy-ready [landing page](landing.md) — is repository content, fact-checked against vendor pages (2026-11). Launch-strategy and monetization material (the go-public plan, the app-landscape research, the go-public review, posting schedules, and anything about how Mailchat Vault itself might be priced or monetized) deliberately does not live in this repository. It is kept outside the git tree by decision (2026-11-14) so the repository can later be made public without leaking business strategy; `.gitignore` blocks `docs/private/` as a guard.

## Authority and status

- The product plan owns scope; the UX specification owns interaction behavior; contracts own data and IPC semantics; validation owns release acceptance. Other documents link to these requirements rather than redefining them.
- **Implemented** (or **current**) means present in the inspected code at the stated baseline, with the inspecting change's test evidence recorded. **Proposed** means authorized design direction awaiting code. **Deferred** means outside this release. **Blocked verification** means a check could not establish application behavior.
- The user selected: personal archive; reliable archive/search first; archive workspace; all three desktop platforms; complete available media preservation; OS protection rather than app-managed encryption.
- Work that landed after the proposal was authored (Gmail OAuth sign-in, hybrid embeddings search, folder watching, Signal/Discord imports, and configurable providers) is recorded in the [assessment](internal/project-assessment.md). The product review reconciles those capabilities with their current onboarding and validation limits; a working capability is not automatically a finished user journey.
- The 2026-11-06 review and user success plan recommend a focused pilot without removing the existing three-platform release target or relaxing recovery/privacy acceptance. Their proposed cohort thresholds are decision rules awaiting customer evidence.
- This documentation change is local documentation work. It does not authorize publishing, installing credentials, signing releases, or migrating a user's real vault.

## Glossary

| Term | Meaning |
| --- | --- |
| Vault | One app-owned local database and its copied source/media objects |
| Source kind | Parser/connector category: mbox, WhatsApp, Telegram, Signal, Discord, IMAP, or demo |
| Source instance | A durable configured account or user-chosen import collection; distinct from a source kind |
| Conversation | A stable chat or email thread, independent of its display name or folder |
| Message | One archived message revision within a source instance |
| Occurrence | Evidence that a message was present in an import or remote folder |
| Object | Immutable bytes stored by SHA-256 digest in the vault |
| Attachment | A message-to-file relationship, including explicit missing-file records |
| Import preview | Bounded inspection of format, source identity, dates, files, and capacity before import |
| Job | A tracked background import, sync, media, backup, or restore operation |
| Legacy unresolved | Existing data whose source, conversation, or time interpretation cannot be established safely |

## Maintenance

Update affected contracts, acceptance tests, and roadmap references in the same change as their implementation. Keep the assessment's commit explicit rather than presenting a historical inventory as live status. Record accepted design changes in [Decisions](decisions.md); supersede old decisions instead of deleting their rationale. Use synthetic fixtures and screenshots without personal messages or credentials.

Review the specification when adding a connector, changing schema or identity rules, changing network disclosures, changing supported platforms, or altering backup formats. The initial documentation validation record is in [Validation and release](validation-and-release.md#documentation-validation).

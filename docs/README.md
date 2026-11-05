# Mailchat Vault implementation documentation

Status: **product specification with the M1 slice implemented**. The specification set was authored and reviewed against the pre-roadmap baseline `a7671f0` (inspected 2026-11-01) and lands on `main` with the M1 documentation update. [Project assessment](project-assessment.md) and the M1-owned contract sections — [identity](data-and-api-contracts.md#identity-and-deduplication), [time](data-and-api-contracts.md#time-semantics), [migration](data-and-api-contracts.md#migration-and-recovery), and the [persistence model](data-and-api-contracts.md#domain-and-persistence-model) — are grounded in the implemented code at `57f40c8` (inspected 2026-11-05) and marked **current** within their documents. Everything still marked **proposed** is authorized design direction awaiting its slice. Creating these documents does not by itself implement the proposed features.

Mailchat Vault is a personal desktop archive for finding and understanding past email and chat messages. The next release prioritizes reliable retrieval, conversation context, available media preservation, and recovery across Linux, macOS, and Windows. AI is optional.

## Reading order

| Document | Purpose | Maintained by |
| --- | --- | --- |
| [Project assessment](project-assessment.md) | Current implementation, evidence, limitations, and ranked gaps | Engineer changing the assessed subsystem |
| [Product plan](product-plan.md) | Users, outcomes, release boundaries, and success criteria | Product owner |
| [UX specification](ux-specification.md) | Workspace, journeys, visual direction, and interaction states | Designer and frontend implementer |
| [Architecture](architecture.md) | Components, data flows, media lifecycle, and privacy boundaries | Core/backend implementer |
| [Data and API contracts](data-and-api-contracts.md) | Domain rules, persistence, migrations, IPC, and errors | Engineer changing a contract |
| [Implementation roadmap](implementation-roadmap.md) | Ordered, independently verifiable delivery slices | Implementer of each slice |
| [Validation and release](validation-and-release.md) | Test matrix, performance evidence, recovery, and packaging | Test/release owner |
| [Decisions](decisions.md) | Numbered decisions and their tradeoffs | Engineer revising a decision |

Start with the assessment and product plan. Before implementing a slice, read its roadmap entry and the linked UX and contract sections. Mark a slice complete only with recorded validation evidence.

## Authority and status

- The product plan owns scope; the UX specification owns interaction behavior; contracts own data and IPC semantics; validation owns release acceptance. Other documents link to these requirements rather than redefining them.
- **Implemented** (or **current**) means present in the inspected code at the stated baseline, with the inspecting change's test evidence recorded. **Proposed** means authorized design direction awaiting code. **Deferred** means outside this release. **Blocked verification** means a check could not establish application behavior.
- The user selected: personal archive; reliable archive/search first; archive workspace; all three desktop platforms; complete available media preservation; OS protection rather than app-managed encryption.
- Work that landed after the proposal was authored (Gmail OAuth sign-in, hybrid embeddings search, folder watching, the Discord import) is recorded in the [assessment](project-assessment.md); reconciling the product plan's deferred list with that work belongs to the product owner, not to the implementing slices.
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

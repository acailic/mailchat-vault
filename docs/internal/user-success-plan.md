# User success plan

Date: **2026-11-06**. Status: **proposed product execution plan** based on the [product review](product-review.md). This document prioritizes user outcomes; it does not mark engineering milestones complete, authorize implementation or publication, or replace the [product plan](../product-plan.md), [UX specification](../ux-specification.md), contracts or release acceptance matrix.

## Outcome and boundary

Help a qualified user **bring in supported history, recover a useful original message or file, verify it, and keep a recoverable copy** without learning the implementation.

Start with the independent-professional retrieval hypothesis described in the review. Teach email plus **one chat format selected from actual participant needs** in the initial guided journey. Continue supporting already implemented formats; this is a sequencing choice, not a connector removal. Keep Linux/macOS/Windows as the release target, while labeling any narrower research build honestly.

Do not add a cloud account requirement, message-content telemetry, a mobile app, sending/replying, more providers, or a broad new connector catalog to this plan. Attachment-content search, OCR and transcription remain research questions until observed failed tasks establish their importance. Existing optional AI must remain usable and safe while ordinary local search stays the main path.

## What success means

**Primary outcome:** a user finds the intended original message or file, confirms that it answers the actual question, and can reopen or export it. A result count, generated summary, imported-message count or clicked search button does not count as success.

Use two complementary measures:

- **Task success:** verified successful retrievals / all eligible attempted retrievals. Define the task and source coverage before the attempt; report unsupported-source and import failures separately rather than silently removing them.
- **Comparative value:** success and elapsed effort versus the participant's current method, including installation and source preparation when measuring initial adoption.

For preservation-focused participants, track a different outcome: successful independent restore and reopening of a known message/file. Do not penalize them for having no reason to open an archive every day.

### Proposed thresholds for a small pilot

These are decision rules to test, **not observed results or industry benchmarks**. Record raw numerators and denominators alongside percentages. Five usability participants reveal problems; they cannot establish market size or statistically reliable conversion.

| Stage | Proposed decision threshold | Measurement rule |
| --- | --- | --- |
| Need discovery | At least 5 of 8 qualified interviews describe a concrete recent failed/slow retrieval and can show their existing workflow | Count incidents and available sources; hypothetical enthusiasm does not qualify |
| First-source usability | At least 4 of 5 representative users finish source selection, preview, import and opening a first real message without coaching | Application steps within 10 minutes with an export already available; record installation and export-preparation time separately, plus the full elapsed total |
| Retrieval usability | At least 4 of 5 complete the predefined text and attachment tasks without coaching | Aim for the correct text original within 90 seconds after data is ready; record failures and assistance rather than stopping the timer |
| Comparative value | At least 3 of 5 prefer Mailchat after matched tasks, with an observed reason: higher task success, meaningfully less effort, or useful offline/preservation access | Counterbalance task order and use equivalent questions. Time savings alone do not qualify when the original is wrong |
| Import trust | All five can explain what imported, what is missing, and whether their source was changed | Ask them to explain in their words; “Import successful” is not sufficient evidence |
| Recovery | Every supported pilot backup/restore scenario passes; at least 4 of 5 can complete the user flow without assistance | Compare stored counts and file digests and reopen a known file in a fresh vault; engineering correctness and user comprehension are separate checks |
| Return value | Over a 30-day private pilot, at least 6 of 10 activated participants report a genuine retrieval opportunity; at least 4 of those independently choose Mailchat again and verify a useful result | Report 4/6 opportunity-adjusted reuse **and** 4/10 whole-cohort reuse if those are the actual numbers. A person with no opportunity is not automatically a satisfied user |
| Support burden | First cohort can proceed without repeated developer intervention; target median onboarding assistance of 15 minutes or less per participant | Record total support time, upper range and recurring categories. A low median must not conceal several unusable setups |

Thresholds are directional enough to decide another iteration, not evidence for a public success claim. Revisit them when observed incident frequency contradicts the chosen segment. Any reproducible loss/corruption of archived data or unintended content transmission stops expansion regardless of the averages.

## Target experience

| Journey | Desired behavior | Important failure/recovery states |
| --- | --- | --- |
| First launch | One clear Add your first source action, a one-sentence local-storage explanation, supported-format help, and a separate Try sample archive action | Empty personal archive is distinct from the sample; leaving sample restores personal data; no inactive export or consent controls compete with setup |
| Prepare and import | Choose Email export or the selected chat source; get concise export instructions; pick a file/ZIP/folder; inspect bounded preview and confirm | Unsupported format offers the precise supported alternative; date uncertainty is explicit; cancellation and partial completion state what remains stored |
| Finish import | Show new/existing/failed messages, preserved/missing files, and coverage; offer View imported messages | Unknown totals remain unknown; failed items have explanations/retry; no blanket “everything backed up” statement |
| Browse and search | All messages shows a bounded recent list; search and a small number of useful filters lead to originals | Empty archive, no matches, stale source, unsupported content search and incomplete import are distinguishable |
| Read and use | Conversation/subject, author/date, message body and files lead; Copy and one clear Export action cover common use | Unverified conversation/date notices remain visible in plain language; advanced metadata stays accessible; Back restores query, selection and scroll |
| Maintain sources | Sources answers “what is here and how current is it?” with last success, folders/range and next action | Needs sign-in, offline, paused, partial failure and never-synced states differ; Disconnect preserves the archive and differs from local deletion |
| Preserve and move | Storage & backup shows location, usage, last verified backup and a guided restore | Backup excludes credentials, explains lack of app encryption, validates before switching, and keeps a recoverable prior vault as already specified |
| Use AI optionally | A secondary Ask mode explains destination and evidence scope; ordinary search works throughout | Correctly hidden inactive consent controls; declined/revoked consent has a usable local path; answers open their originals; implement the remaining approved payload-approval contract |

Do not put invented cross-source conversations together to make the demo attractive. A useful unsupported/uncertain state is preferable to misleading confidence.

## Prioritized work packages

IDs below are planning identifiers, not newly filed issues. Size is a relative estimate before engineering discovery: **S** one focused UI correction, **M** a complete bounded workflow, **L** a cross-layer workflow with recovery/platform testing. No precise person-day estimate is implied. Reuse existing M3/M4/M5 work and review its actual acceptance evidence before starting overlapping implementations.

| ID | Priority / size | Deliverable and owner role | Dependency / existing work | Completion evidence |
| --- | --- | --- | --- | --- |
| U01 | P0 / S | Correct inactive consent visibility and audit other hidden rows. Frontend + QA | Review F02; no architecture change needed to describe the fix | Fresh local-only boot has no sending prompt, focusable consent buttons or empty consent space; prompted/allowed/declined/revoked states work in real webview and accessibility tree |
| U02 | P0 / M | First-run guide and genuinely isolated sample archive. Product/design + frontend/core | M3.4 sample-vault boundary; U01 | User chooses first source or sample; sample rows never enter personal vault; switching back/restarting does not lose personal data; 4/5 usability completion |
| U03 | P0 / L | Unified picker, source-specific preparation help, bounded preview, import summary and View imported messages. Frontend/core + QA | M3.3, existing bounded ingestion; discovery selects first chat format | No typed paths; supported/unsupported and partial cases explained; dates/capacity handled per contract; reimport dedups; one real export from each supported pilot path succeeds |
| U04 | P1 / M | Truthful All messages, simpler search controls and content-led reading. Product/design + frontend | M3.1/M3.2; U03 provides first content | Bounded browse is visible; first screen emphasizes search/content; one primary export path exposes formats intentionally; retrieval, Back, selection and keyboard regression checks pass at narrow/wide sizes |
| U05 | P0 before preservation pilot / L | Verified backup/restore and visible recovery status. Core/frontend + QA/release | M4.4 already planned; evaluate existing work instead of duplicating it | Whole-vault restore passes existing A09/A10/A12 contracts; interruption/capacity failures preserve a working vault; reopened messages and attachments match; users understand credentials are excluded |
| U06 | P1 / M–L | Source/account status and recovery actions; honest authentication entry. Frontend/core + product | M3.4 and existing account-panel work; U03/U05 | Sources exposes last success, coverage and failure remedies; pause/disconnect retain archive; credentials do not disappear silently after restart; no consumer Google claim depends on user-created developer credentials |
| U07 | P1 / M | Retrieval quality benchmark and claim calibration. Product/research + search engineer | Existing FTS and hash-vector paths; representative permitted/synthetic cases | At least 30 labeled questions across the chosen sources; top-5 recall, correct-original task success, false-confidence cases and latency recorded; query categories/languages reported separately |
| U08 | P1 / M | Task-based Settings and explainable trust states. Product/design + frontend | U05/U06; M5 remainder remains owned by its existing milestone | General/Sources/Storage & backup precede optional AI; visible labels; source coverage, local storage and network boundaries are understandable to 4/5 participants |
| U09 | P1 / L per platform | Validate the actual distributed candidate on each claimed platform. Release + QA | M6; U01–U08 for the promised workflow | Install, launch, import, retrieval, keychain/session fallback, upgrade, backup/restore and uninstall-retains-data records on the platform; exact artifact/version and support status recorded |
| U10 | P0 learning / research | Discovery, comparative tests, then a supported 30-day pilot. Product/research | [Research playbook](user-research-playbook.md); staged gates below | Recruitment log, task observations, failure reasons, support time and a written proceed/narrow/stop decision; no invented or averaged-away participant evidence |

The U10 pilot's runner kit — screener, day-0/7/30 schedule, tracking log, readout template — is prepared as the [pilot kit](../evidence/u10-pilot-kit-2026-11-15.md) (2026-11-15); its [gate record](../evidence/u10-pilot-gate-2026-11-15.md) names the GP03/GP04 preconditions and holds the no-promotion-without-readout rule.

U01 and U10 discovery are the first actions. U03 and U05 are the substantial adoption/trust investments. U07 starts early enough to find a retrieval-quality problem before UI work is mistaken for a solution. Statistics polish and new connectors wait behind these outcomes.

### Reconcile existing work before dispatch

The read-only issue check on 2026-11-06 found open feature titles for OAuth, Discord import, folder watching, vector search, provider menus, Settings, export, statistics and installers. Several corresponding capabilities are already in the inspected main tree. An open issue title is not proof that the feature is absent; compare its actual acceptance criteria with the current implementation before creating another task or closing it.

In particular, account-management [issue 22](https://github.com/acailic/mailchat-vault/issues/22) already has [proposed account-panel work](https://github.com/acailic/mailchat-vault/pull/53), observed open against `main` at head `30078549de5be5d104b3d8217b6e15ba54712457`. Review that work against U06 rather than implementing a competing panel. U03 and U05 likewise map to the existing M3.3 and M4.4 slices. Repository/forge status can change; verify again at implementation time. This investigation did not change remote issues, branches or pull requests.

## Delivery sequence and decision gates

The following is an **illustrative 8–10 week planning envelope**, assuming one full-time engineer plus fractional product/design/research support and access to pilot users. It is not a delivery commitment. Estimate unfinished M3.3/M3.4/M4.4 work and platform availability first; extend the calendar or reduce pilot exposure rather than dropping recovery requirements.

| Stage | Illustrative window | Work and decision | Exit rule |
| --- | --- | --- | --- |
| 0. Establish a reason to switch | Week 1 | Eight incident interviews; compare existing workflows and closest competitor setup; reproduce F02; define the first 30 retrieval cases | If fewer than 5/8 show concrete pain, revise the segment before substantial feature work. Pick the first chat path from evidence |
| 1. Make first value reachable | Weeks 2–3 | U01/U02 plus a vertical U03 path for email and the chosen chat source; U04 browse/read improvements | Five unassisted usability sessions reach first content and correct originals at the proposed thresholds. A prototype is still a research build |
| 2. Make the archive trustworthy | Weeks 3–6, overlapping where safe | U05 recovery, U06 source status, U07 quality, U08 trust/settings; integrate existing work | No unresolved data-integrity/privacy failures; restore verified; users understand coverage; retrieval benchmark results justify product claims |
| 3. Run a supported private pilot | Roughly weeks 6–10 | U09 on explicitly supported pilot platform(s); ten users with their own source copies; 30-day follow-up; fix repeated blockers | Track full funnel, genuine opportunities, useful returns and support burden. Keep original exports; do not market a research build as the user's sole backup |
| 4. Decide the next investment | After the 30-day follow-up | Write a short gate readout; continue only on observed value; complete remaining M6 platform/release evidence before broad claims | Proceed, narrow, repeat one focused iteration, or stop. Passing tests alone cannot select Proceed |

Discovery may run on a demonstration without a recoverable product. A preservation pilot using people's archives requires U05; a research build must not be sold as a completed archive solution. Live-mail onboarding is a separate route with its own setup evidence, not something inferred from a passing mocked OAuth flow.

### Decision rules

- **Proceed to broader beta:** first-source and retrieval thresholds pass, restore is proven, a meaningful subset returns for real jobs, support problems are bounded, and users can explain a concrete advantage over alternatives. Finish the existing release matrix before claiming all-platform support.
- **Narrow:** one segment/source combination succeeds while others fail. Improve and describe that specific workflow; preserve honesty about the remaining formats.
- **Repeat one focused iteration:** need is clear but a single identified onboarding or relevance problem causes failure. State the hypothesis and pass rule before another cycle.
- **Stop expansion or reconsider reuse/integration:** qualified users repeatedly prefer current tools or the closest alternative, source preparation overwhelms the benefit, or there is no repeat/preservation value. Do not answer a failed hypothesis with more undirected features.

## Measurement without message telemetry

Respect the existing product plan: **do not add analytics, cloud accounts or automatic content reporting**. Use voluntary research observations and participant-controlled records. The app's statistics dashboard describes the archive; it is not a customer-success measurement system.

Keep the following study fields in a local research record: anonymous participant ID, build/platform, source category, stage completed, assistance count, preparation/import/retrieval duration, correct-original yes/no, failure category, opportunity count, chosen tool and support minutes. No message bodies, author identities, account addresses, query text, credentials or file contents are needed. Local event instrumentation, if later proposed, needs an explicit separate design decision; this plan does not authorize it.

Report the funnel as **recruited → installed → source ready → imported → first verified retrieval → activated → genuine later opportunity → useful reuse**. Include dropouts and reasons at every stage. Define activation as one user-owned source imported plus one verified retrieval; track adding a second source separately so cross-source value cannot be claimed from single-source activation.

For search benchmarking, keep the reproducible synthetic/permitted corpus separate from interview notes. Measure p50/p95 latency with corpus size, device and concurrent-import condition; reuse the existing performance contracts rather than interpreting E2E test duration as query latency.

## Risks that can change the plan

| Risk | Early evidence to seek | Response |
| --- | --- | --- |
| Existing tools already solve the job | Matched tasks show no meaningful benefit | Narrow the job or reconsider standalone development |
| Export preparation is the main obstacle | Users cannot obtain the supported JSON/TXT/mbox form or media | Fix source-specific help and supported shapes; do not hide this time from activation measurements |
| Retrieval needs attachment contents or another source | Repeated real tasks fail for the same reason | Reassess scope with evidence before adding OCR/transcription/connectors |
| “Meaning search” expectations exceed current ranking | Synonym, paraphrase or cross-language cases miss | Correct claims, measure alternatives, and choose an improvement based on the benchmark |
| Archive data is stale or appears complete incorrectly | Participants cannot describe coverage/failures | Prioritize Sources and partial-success explanations ahead of new ingestion paths |
| Platform/authentication operations dominate support | Artifact, keychain or login failures recur | Limit the labeled pilot platform and route until repaired; preserve the full release target |
| The job is valuable but rare | Little reuse despite genuine satisfaction with preservation | Evaluate preservation/restore success as its own segment; avoid artificial engagement features |

## Next review

The product owner maintains a dated gate readout containing the tested hypothesis, recruited/completed counts, failed tasks, comparative outcomes, recovery evidence, support costs, and the next decision. Record contradictions with the original recommendation. Until that record exists, this remains an actionable hypothesis-driven plan, not proof that Mailchat Vault will succeed.

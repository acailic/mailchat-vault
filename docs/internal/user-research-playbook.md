# User research playbook

Prepared **2026-11-06** for the [product review](product-review.md) and [user success plan](user-success-plan.md). Status: **study design; no participants recruited or findings collected**. The thresholds in the success plan are proposed decisions, not research results.

## Questions this study must answer

1. Does a specific group have a recurring or costly retrieval problem that existing tools do not solve well?
2. Can those people prepare their history and reach a useful original without developer assistance?
3. Does Mailchat provide an observable advantage over their current workflow and a close alternative?
4. Can people explain source coverage, missing files, freshness and recovery correctly?
5. Does value persist beyond a guided demonstration, at an acceptable support cost?

Recruitment and any recording/contact are future actions requiring their ordinary authorization and participant agreement. This document supplies the protocol; it does not contact people or enable telemetry.

## Recruit by behavior

Begin with **eight discovery interviews**, followed by **five representative usability participants** and, if the gates pass, **ten private-pilot participants**. Reuse people only where useful; identify prior exposure in the analysis. Prefer new participants for first-run usability so the interview/demo does not teach the interface.

Discovery screener:

- In the last 90 days, have you tried to find a past work/project message or attachment and struggled? Describe the last occasion.
- Which apps/accounts held the possible answer? Was the source still accessible?
- What did you try, roughly how long did it take, and what happened if you could not find it?
- Do you have email plus WhatsApp, Telegram or another currently supported export? Can you obtain copies you are comfortable using locally?
- Which desktop and phone do you use? Where were you when you needed the information?
- Have you used an archive/export/search tool before? What made you keep or abandon it?

Include nondevelopers and people with realistic export sizes. Record source availability, operating system, technical comfort, and retrieval frequency. Do not count a participant as qualified solely because they like privacy or AI. Keep preservation-only users as a separate analysis group. Someone needing an unsupported source is useful discovery evidence but should not be silently converted into a supported-path usability success.

Suggested invitation for later authorized use:

> We are studying how people recover old messages and files across email and chat apps. We would like to see a recent example of your current process, including where it was difficult. There is no requirement to upload private messages or connect an account. A later optional session may try a desktop archive using sample data or source copies you choose to keep on your own machine.

## Discovery interview: 30–40 minutes

Do not introduce Mailchat until the existing behavior is understood.

| Time | Prompts | Evidence to record |
| --- | --- | --- |
| 0–5 min | Role, devices, where project conversations live | Segment and source context |
| 5–18 min | “Walk me through the last time you needed an old message or file.” “What did you try next?” “What happened afterward?” | Actual sequence, effort, consequence and workaround |
| 18–25 min | “Show how you would look for it now, without exposing anything you do not want to share.” “Why this tool?” | Observed current method; avoid relying only on recalled timing |
| 25–32 min | “How would you get a copy out?” “What would stop you keeping it on this computer?” “What would happen if you changed computers?” | Export friction, storage/recovery expectations, mobile dependence |
| Final 5–8 min | Present the short concept, then ask “Where would this fit, if anywhere?” “What would you still use instead?” | Concrete next behavior and objections, kept separate from prior need evidence |

Avoid leading prompts such as “Wouldn't one private search box be useful?” or “Would AI solve this?” Separate what the person did, what they say they would do, and the researcher's interpretation. Do not fabricate quotes when a summary is all that was recorded.

Discovery output: a dated table of incidents, source combinations, current methods, preparation obstacles, retrieval consequences and fit/non-fit reasons. Select the first chat onboarding path from this table. Report excluded participants and missing source access.

## Unassisted usability: 45–60 minutes

Use the actual distributed research artifact, not a source-build setup performed invisibly by the facilitator. Record version, platform, starting state and whether a prepared export is supplied. If installation is precompleted, say so and do not claim installation usability.

Ask participants to think aloud. Let them choose the route. A hint, pointed gesture or repaired path counts as intervention; assistance may continue after recording the failure. Do not convert a coached completion into an unassisted success.

| Task | Participant wording | Pass evidence |
| --- | --- | --- |
| 1. First impression | “What does this app do, and what would you do first?” | Participant identifies bringing in supported history, not AI configuration or searching an empty archive |
| 2. Add history | “Bring this email export and this supported chat export into the app.” | Native picker, understandable preview, deliberate source/date decisions, import completes; no typed filesystem path or hidden facilitator repair |
| 3. Confirm capture | “Explain what is now in the archive and what is not.” | Correctly distinguishes new/existing messages, missing media and coverage; does not infer that remote sources were changed |
| 4. Recover an agreement | “Find the original message confirming the changed delivery date.” | Opens the known correct original and explains its author/date/context; a summary or similar-looking message alone fails |
| 5. Recover a file | “Find the attachment associated with that exchange and save a usable copy.” | Correct file opens/exports; deliberately missing file is recognized as missing rather than corruption or successful preservation |
| 6. Refine and return | “Find messages from this person in this period, open one, and return to your results.” | Filters and scope are understood; Back preserves the expected query/selection/position; task works by keyboard where requested |
| 7. Update safely | “Import the same export again, then explain the outcome.” | No duplicate message growth; participant understands the existing/new distinction |
| 8. Check freshness | “You expect a new message. How would you know whether this source is current?” | Finds last success/coverage or accurately identifies an unavailable capability; no assumption that a closed app always syncs |
| 9. Preserve and move | “Make a backup and restore it in this separate test vault.” | Whole-vault recovery succeeds; participant can open the known message/file and explain credential exclusions; never overwrite their personal vault for the study |
| 10. Explain privacy | “What stays on this computer? What could leave it? What happens if you decline AI?” | Understands local search, optional provider transmission and usable local fallback; does not confuse local storage with app-managed encryption |

On the current baseline, tasks blocked by a missing workflow are expected failures to record. Do not hide them behind an engineering demonstration. Re-run after the corresponding work package, with a fresh participant where prior teaching would bias the result.

Use synthetic messages with known answers for comparable tasks; use user-owned copies locally for a separate relevance check. Cover email plus the selected chat source, a known verified conversation, ambiguous/unknown time, duplicate-looking but distinct messages, missing media, and a supported preserved attachment. Existing [E2E fixtures and media suites](../../tests/e2e/README.md) are starting material; the six-message demo alone is insufficient to represent years of personal history.

## Compare value fairly

1. Record the participant's ordinary method before teaching Mailchat.
2. Prepare equivalent retrieval questions whose originals are independently known. Do not use the exact answer learned in the first tool as the second tool's challenge.
3. Counterbalance task/tool order across participants, and report the small sample rather than implying formal significance.
4. Count preparation, installation, export, indexing and maintenance effort separately and in total. A quick search after a developer performed all setup is not end-to-end superiority.
5. Compare against the participant's current tools first. Separately evaluate the closest product, currently [msgvault](https://www.msgvault.io/), with explicit participant agreement and compatible source copies. Its setup and advertised breadth have been reviewed; it has not been installed or benchmarked in this study.
6. Ask which tool the person would use for the next real incident and why. Distinguish a preference from the actual choice later observed in the pilot.

## Retrieval-quality benchmark

Create at least **30 labeled questions** on a reproducible synthetic or explicitly permitted corpus. Record expected message/file IDs, acceptable alternate originals, source coverage and relevance judgments before evaluating ranking. Separate development examples from a held-out set so tuning is not reported as independent validation.

Include exact phrase, remembered partial wording, person/date, duplicate names across sources, different wording for the same concept, a typo, unavailable media, attachment-filename versus attachment-content search, and unknown-time cases. Add Serbian Latin/Cyrillic or other languages only when the cohort needs them, then report those results separately; current hash-vector support is not proof of cross-script semantic retrieval.

Report top-1 accuracy, recall@5 for questions with an available labeled answer, correct-original task success, honest failure for unsupported/unavailable content, and p50/p95 latency with corpus/device conditions. For a question whose only answer is inside an unindexed PDF, an honest scope explanation is useful behavior but not successful retrieval. Generated-answer fluency must not be scored as retrieval correctness.

## Thirty-day pilot

Enroll after the recovery and platform gates in the success plan. Participants keep original exports/source copies and use an explicitly labeled build. Schedule an onboarding check and follow-ups around days 7 and 30; arrange support without collecting their archive contents.

The runnable form of this section — [U10 pilot kit](../evidence/u10-pilot-kit-2026-11-15.md) (2026-11-15): recruitment screener, day-0/7/30 schedule with the support arrangement, participant tracking log, and the gate-readout template with its decision rubric. Its [gate record](../evidence/u10-pilot-gate-2026-11-15.md) states the GP03/GP04 preconditions and the rule that no broader promotion happens before the readout.

For each genuine retrieval opportunity, participants may voluntarily record: a random task label, chosen tool, source category, success/failure, approximate time, assistance needed and why they chose that route. Do not request query text, message bodies, contact names, screenshots of private conversations or credentials.

Follow-up prompts:

- “Since the last session, when did you need an old exchange or file?”
- “Which tool did you reach for, and what happened?”
- “What did you expect to be here that was missing or stale?”
- “Did maintaining/exporting the history take more effort than the lookup saved?”
- “If this app disappeared tomorrow, what would you actually do instead?”
- “What would have to work differently before you trusted it with another source?”

Record no-opportunity participants, dropouts and failures, along with successful returns. Participants may stop at any time. Store only voluntary anonymized study notes in an access-controlled location chosen by the researcher; this repository should hold aggregate findings and synthetic evidence, not private archives.

## Recording templates

One task record:

```text
Participant ID:
Build / platform / prior exposure:
Task ID / source category / source coverage:
Export preparation / installation / import time:
Retrieval time / correct original found:
Assistance count / exact obstacle:
Expected capability versus observed capability:
Current-method comparison:
Participant explanation (summary or consented verbatim quote, labeled):
Researcher interpretation / confidence:
```

One gate readout:

```text
Date / hypothesis / tested build:
Recruited / qualified / completed / withdrew:
Install → source ready → import → verified retrieval → activation counts:
Later opportunities / useful independent reuse / no opportunities:
Task success and failure categories, with raw denominators:
Comparison results and setup effort:
Recovery correctness and comprehension:
Total support time / median / range / repeating causes:
Evidence that contradicts our preferred direction:
Decision: proceed / narrow / one focused iteration / stop expansion
Reason / owner / next hypothesis and pass rule:
```

Write findings only after observation. Link them back to F01–F12 in the review and U01–U10 in the plan. An interview request, passing engineering test, planned feature or attractive demo is not a completed user outcome.

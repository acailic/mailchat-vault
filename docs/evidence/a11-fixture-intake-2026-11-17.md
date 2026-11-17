# A11 fixture intake — 2026-11-17

Status: **prerequisite intake record; no application changes implemented by this document**.

The slice intended for this run: a Takeout Google Chat provider module and
content probes so a real Takeout Google Chat JSON export imports through the
existing bounded flow, with golden fixtures mirroring the real export's layout
and file names and synthetic content only, plus a focused golden ingest test
(preview, commit, stable identity keys, idempotent re-import, missing-media
receipt). Skype and Slack stay blocked scope records.

## Prerequisite intake result

The run's precondition was a user-attached **structure kit** before start:

1. a file listing of the exported Takeout **Mail** and **Google Chat** folders
   (names and sizes),
2. one **redacted Chat conversation JSON** (keys and nesting intact, values
   blanked),
3. the first lines of one **`.mbox` sample** with senders and bodies redacted.

The kit was searched for and **not found**:

| Where checked | Result |
| --- | --- |
| This task's SuperPlane artifacts | Only the run's `plan.md` spec; no attachment |
| Repository tree and full git history (`main`, all refs, stashes) | No Takeout/Google Chat fixture kit; grep for takeout/google chat/hangouts finds only the plan/evidence prose |
| Remote branches (`gh api …/branches`) | Branches exist for A0–A9 slices; none carries an A11 kit |
| GitHub issues and pull requests (`gh issue list --state all`, `gh pr list --state all`) | 68 issues, highest #189; no A11 issue, no kit attached anywhere |
| Runner filesystem (`/tmp`, `$HOME`, task scratch) | Automation files only; no export listing or redacted sample |

## Stop decision

The slice spec's own rule governs: derive the Chat JSON schema from the
attached sample, and when the real structure is not available, stop after
fixture intake and re-plan instead of guessing. This is the same constraint the
plan records at §10/§13 — adapters land only with representative fixtures
("add source only with representative fixtures and a tested preservation
path"), and the A11 ticket body says "no code until fixtures exist". Writing a
Google Chat parser from a schema reconstructed from memory would be exactly the
guess the block exists to prevent, and golden fixtures could not truthfully
claim to mirror an export layout nobody inspected.

Consequences for this run:

- **No adapter code, no fixtures committed.** No Skype, Slack, Mail, MBOX, EML,
  or ZIP behavior changed; the working tree differs from `main` only by this
  record.
- **The A11 tracker issue now exists** — [#229](https://github.com/acailic/mailchat-vault/issues/229) —
  and names the three provider children —
  Takeout, Skype, Slack — each with status, prerequisite, and done-when checks,
  so the follow-up starts from a recorded scope instead of a reopened
  investigation. Takeout is recorded as blocked on the same missing kit (the
  intended first-slice scope is preserved in its body for when the kit lands).

## What unblocks the Takeout child

Re-attach the structure kit exactly as specified — folder listing with names
and sizes, one redacted Chat conversation JSON with keys and nesting intact
(values blanked), and the first lines of one `.mbox` sample with senders and
bodies redacted. Raw export bytes are never needed or wanted in the repository;
fixtures will be synthetic (real layout and names, invented bodies) per
[CONTRIBUTING](../CONTRIBUTING.md). If the sample turns out to be an HTML-only
export or a shape the bounded JSON walkers cannot stream, the follow-up stops
after intake again and re-plans rather than guessing.

## Validation of this deliverable

Documentation-only change. Tier-A suites were run on this branch to confirm the
untouched baseline (`npm run check`, `npm test`, `cargo test -p vault-core`);
exact counts are recorded in the pull request's local-verification block. The
A11 issue body was verified with `gh issue view` after creation. No
application behavior, performance target, or provider integration was newly
proven by this record.

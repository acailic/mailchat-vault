# First-session observation kit — GP03 (dated 2026-11-15)

Purpose: stage the five observed first-source sessions that gate GP03
(go-public plan §3): a person with a real export reaches a known original
message — import → preview → confirm → find → open files → export or
verified backup — **without coaching**. Humans run these sessions; this
kit is everything a facilitator needs so each session takes minutes, not
an hour. The gate itself stays **open** until five records exist below.

## What the facilitator prepares (per session)

- A build of the app under test (record commit hash + platform).
- The participant's own export (their file, their machine — never commit
  content; only the observation record below is kept, synthetic examples
  in [fixtures](../../tests/e2e/fixtures) are for dry runs).
- This sheet printed or open on a second screen. Do **not** coach: the
  only permitted help is reading the app's own text aloud if the
  participant asks where to look.

## Task sheet (drawn from the user-research playbook's unassisted tasks)

Ask the participant to do each task in their own words; tick done /
needed-help / failed and note elapsed time:

1. Get your export into the app and see what it found before anything is
   stored. (watch for: the preview card — did they read the counts? the
   timezone confirmation block if their export carried naive clock times?)
2. The app assumed a timezone for clock times without one — notice it,
   and correct it if wrong. (done = the participant saw the assumption
   unaided; record what they said.)
3. Choose where the import lands (a new source name or an existing one).
4. Make the import happen; if anything fails, get it to succeed without
   starting over. (watch for: the Retry button.)
5. Find one message you know is in there (pick one they name first).
6. Open an attached/linked file from that message, if the export had one.
7. Get the messages (or a found subset) out of the app as an export, or
   verify a backup exists that you could restore.

## Observation record fields (one per session, appended below)

- date, participant code (P1..P5 — no names), platform, app commit
- export kind (WhatsApp / Telegram / mbox / …), rough size, media or not
- per task: done / needed-help / failed + seconds
- verbatims: what confused them, what they praised (quote, not summary)
- frictions: anything they said aloud that the app should have said
  instead (map to an issue when filed)
- verdict per plan §3: reached the known original message uncoached? Y/N
- facilitator notes (what they would have done without the app's hints)

Template:

```
### Session <n> — <date>, P<n>, <platform> @ <commit>
- export: <kind, size, media?>
- tasks: 1:done/45s 2:needed-help/120s 3:done/20s 4:done/… 5:… 6:… 7:…
- verbatims: "…"
- frictions: …
- uncoached reach: Y/N — <one sentence>
- notes: …
```

## Session records

(none yet — the gate is open)

## Fixture pointers (dry runs, no humans)

- mbox: `tests/e2e/fixtures/archive.mbox` (spec 02)
- Telegram naive-date export: written inline by spec
  `31-first-session-confirm` (the naive/confirmed pair)
- Messenger tree / media: `tests/e2e/fixtures/messenger-dyi` (spec 10)

## What "verified backup" means here (task 7)

Either the participant exports a subset (Export… in the archive view) and
opens it outside the app, or Storage & backup → Create backup completes
and the Restore preview validates the container. Both paths count; record
which.

## After the fifth session

Update [support-matrix](../support-matrix.md)'s five-session gate row,
file friction issues, and only then record GP03's journey evidence as
complete in the [roadmap](../implementation-roadmap.md).

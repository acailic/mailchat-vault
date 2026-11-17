# P2·13 near-duplicate backfill intake — 2026-11-17

Status: **blocked-dispatch record; no application changes implemented by this document**.

The slice intended for this run: the resumable SimHash fingerprint backfill
(near-dup 2/5 of NEWWO-101) — a job copying the `files.rs` backfill shape
(job-kind constant, `archive_meta` checkpoint key, fixed batch, per-batch
transaction, outcome struct with `resumed_from`), selecting
`archive_messages` rows without a `message_fingerprints` row past the
checkpoint, hashing the same normalized body text the FTS write path stores,
plus honest progress in the `archive_health` read model, with a
resume-from-checkpoint test in `tests/near_duplicates.rs`.

## Dependency intake result

The task's stated precondition is NEWWO-101 (slice 1/5) being merged: the
`message_fingerprints` table, `crates/vault-core/src/simhash.rs`, and
fingerprints written at import. It was searched for and **not found**:

| Where checked | Result |
| --- | --- |
| `crates/vault-core/src/` listing | No `simhash.rs` |
| Full `CREATE TABLE` inventory in `storage.rs` (24 tables + `schema_migrations`) | No `message_fingerprints`; `message_identities` is the exact SHA-256 revision identity, a different thing |
| Repo-wide grep `simhash|SimHash|message_fingerprints|near_dup` | Only `docs/` prose; zero matches in `crates/` or `src-tauri/` |
| `crates/vault-core/tests/` listing | No `near_duplicates.rs` (the acceptance target does not exist) |
| Remote branches (`gh api …/branches`, all 14) | None near-dup related |
| Pull requests (`gh pr list --state all`, keyword search) | Nothing carries the foundation |
| Issues (`gh issue list --search NEWWO-101`) | Empty |
| Docs | The retrieval triage records item 12 as **Deferred**; "Nothing scheduled; no spike opened" (docs/superpowers/plans/2026-11-14-retrieval-quality-triage.md, line 219) |

## Stop decision

The slice spec's own rule governs: take the table name, column names, and
simhash function signature from the merged foundation slice — "Do not invent
them" — and "an implementer on current main must stop and report rather than
guess that contract." Implementing the backfill here would require inventing
the entire foundation (migration number, table shape, function signature,
import-time write points), which is exactly the guess the dependency rule
exists to prevent: a guessed contract that later disagrees with slice 1 would
collide on the append-only migration list and fork the naming. The spec's risk
section also evaluated folding the foundation in and recommended against it
(one run covering migration + simhash + import writes + backfill + health +
two suites exceeds the single-run limit; the split keeps each part separately
mergeable).

Consequences for this run:

- **No backfill code, no schema change committed.** The working tree differs
  from `main` only by this record.
- **The foundation slice now has a tracker** —
  [#242](https://github.com/acailic/mailchat-vault/issues/242) — with its
  scope and done-when recorded, so slice 1 starts from a recorded scope
  instead of a reopened investigation.

## Seams intake (verified present and ready for the follow-up)

- The pattern to copy is intact in `crates/vault-core/src/files.rs`:
  `FILES_BACKFILL_KIND`, `BACKFILL_CHECKPOINT_KEY`, `BACKFILL_BATCH = 200`,
  `read_checkpoint` (last-id checkpoint in `archive_meta`), per-batch
  committed transactions with idempotent upserts, and an outcome struct with
  `resumed_from`/`is_complete`.
- The shared text source exists: `archive_fts` is a content table over
  `archive_messages(body, author, subject)` kept in sync by triggers
  (`storage.rs` ~358–378) — the body column the import path writes is the
  text the backfill must hash.
- The health read model exists in `crates/vault-core/src/health.rs`
  (`ArchiveHealth`, committed-facts-only, landed via #225) and is the surface
  for the progress fields.

## What unblocks this slice

Merge #242's foundation: `message_fingerprints` table (appended migration),
`simhash.rs` over the FTS-normalized body, import-time writes, and the
`near_duplicates.rs` test target. Then re-dispatch this slice unchanged; its
plan already binds the copy-from patterns above.

## Validation of this deliverable

Documentation-only change; per the repo's standing docs-only validation
decision, suites were not rerun for this record. No application behavior was
changed or newly proven. The named acceptance target
(`cargo test -p vault-core --test near_duplicates`) cannot run on current
main because the test target itself is part of the missing dependency — that
absence is the finding recorded above, verified by directory listing rather
than a compile.

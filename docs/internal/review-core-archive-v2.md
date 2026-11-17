# Review: `review/core-archive-v2` (33b5594) — 2026-11-03

Reviewed vs `main` (a7671f0). +9,783 / −1,508 across 115 files.
Scope: vault-core v2 (archive/runtime/objects/backup/media/jobs/stream/zip_archive/assistant),
tauri commands wiring, e2e suite rework, fixtures.

## Verdict: **merge-ready from test + code-review standpoint**
One blocker remains environmental, not code: the native E2E needs webkit2gtk-driver +
Tauri dev packages (sudo) — see "E2E" below.

## Test evidence (all run on this machine, review branch)

| Suite | Result |
|---|---|
| Rust `cargo test -p vault-core` | **23 test binaries, ALL PASS** (archive_v2, runtime_v2, stream_v2, media_recovery + pre-existing proptest/fuzz/imap-mock/integrity/search-perf/dst-local) |
| Rust stability check | failing-looking test re-run 10/10 ok, single-threaded 19/19 ok, full suite 3× ok |
| Frontend `vitest` | **92/92 PASS** (requires `--configLoader runner` until root-owned `node_modules/.vite-temp` is removed) |
| TypeScript `tsc --noEmit` | PASS |
| E2E (tauri-driver, 8 specs) | **not runnable on this machine yet** — missing `webkit2gtk-driver` + `-dev` packages (sudo apt install, see below) |

### The one-time `search_page` failure — not a regression
First full-suite run showed `chat_context_breaks_ties_by_id_and_skips_other_chats`
returning `["a2"]` instead of `["a1","a2","a3","a4"]`. Investigation:
- DB state verified correct (conversation_id properly derived via `resolve_message_conversation`)
- Exact test replica in isolation: PASSES (1/2 neighbors as expected)
- Original test alone: PASSES; whole binary: PASSES; single-threaded: PASSES; 10× loop + 3× full suite: PASSES
- Root cause: a `cargo build` had been SIGKILLed minutes earlier; the first test run
  executed against a stale/corrupt incremental artifact. Clean rebuild → consistently green.

## Code findings

1. **backup.rs (M4.4)** — checksum-verified manifest, staged restore (`stage.keep()` only
   after `validate_staged` + schema_version match), `verify_db` before both backup and
   restore. Matches the "terminate at critical boundaries" contract. No issues.
2. **objects.rs (M4.1)** — content-addressed sha256 store; `fs::canonicalize` on read
   (path-traversal safe); hex validation before path join. Good.
3. **jobs.rs (M2)** — persisted jobs, cancel/retry, `safe_error` stringifies without
   leaking paths. Good.
4. **runtime.rs** — read/write lock separation; `update_settings` strips a forbidden key
   (the one `unwrap` is on a self-constructed object — safe invariant). Good.
5. **No panics in production paths** — remaining unwraps are `#[cfg(test)]` or
   fixed-length byte-slice invariants in zip_archive (documented-safe pattern).
6. **zip_archive.rs** — hand-rolled reader; suggests a future fuzz target (the existing
   fuzz suite covers chat/mail parsers; ZIP boundary conditions would be a good addition).

## Merge recommendation

1. Merge to `main` after (or together with) one green E2E run on a desktop with the
   native prerequisites. The two sudo commands that unblock it:
   - `sudo apt install -y libwebkit2gtk-4.1-dev libssl-dev libxdo-dev libayatana-appindicator3-dev librsvg2-dev pkg-config webkit2gtk-driver`
   - `sudo rm -rf node_modules/.vite-temp` (root-owned leftover from an earlier session;
     also unblocks plain `npm run build` / `npm test` without the configLoader workaround)
2. Post-merge: keep the review branch until E2E is green, then delete; continue M1/M3
   slices per roadmap.

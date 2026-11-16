# U07 retrieval benchmark — verification and claim-calibration record (2026-11-16)

Status: **complete for this slice** (benchmark instrument verified on current `main` and the claim surfaces wired to it; open track: [#189](https://github.com/acailic/mailchat-vault/issues/189)). Grounding: [user success plan](../user-success-plan.md) work package U07 ("retrieval benchmark results justify product claims"), the [U07 benchmark](../../tests/retrieval/BASELINE.md), and the [slice-1 results](retrieval-slice-1-results-2026-11-14.md) that pre-date it.

## What this slice found and did

The U07 instrument (`tests/retrieval`: 36 labeled questions — 30 answerable + 6 no-answer, 18 en / 18 sr, 15 email / 15 chat originals, 9 categories; recall@1/@5/@10, correct-original task success at top-5, false-confidence and confident-but-wrong counters, latency p50/p95; reported per category, language and channel; PR #76) was present on `main` but **no public-claim surface cited it** — the support matrix's search rows carried no benchmark-calibrated caveats, exactly the half of U07 the plan names ("claim calibration"). This slice:

1. **Re-verified the instrument end to end on the current baseline** (`51a2642`, i.e. after the BM25F weights, LiM ordering, M5 consent and M7 keep-alive work): builds clean, self-check OK, 10k gate pass, both scale points reproduce the recorded 2026-11-12 baseline (below).
2. **Calibrated the claim surfaces**: the support matrix's "Local FTS5 search" and "Local embeddings / hybrid ranking" rows now carry the measured numbers and their forbidden claims ([support matrix](../support-matrix.md#capability-status)); the testing guide's CI section now names the `retrieval.yml` workflow that makes the harness repeatable rather than one-off.
3. **Aligned the harness crate's license** with D13 (`GPL-3.0-only`, was `UNLICENSED` — the support matrix had flagged this file).

## Verification record (2026-11-16, this machine)

Linux x86_64 container, 16 CPUs, rustc 1.100.0 (stable), LTO release profile. Actions is down for billing (the standing [CI cutoff](../testing.md#continuous-integration)), so the record is a local run per the interim protocol. The host image ships no C toolchain; the build used a portable `zig cc` (glibc 2.36 target) as compiler/linker for both the harness workspace and `vault-core` — no product code was modified by that setup.

- `cargo test -p vault-core --test retrieval_eval` (the in-CI ranking-floors companion): **6/6 passed**.
- Harness workspace: `cargo fmt --all -- --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo build --release --locked` clean (the refreshed lockfile is consistent).
- Root `cargo fmt --all -- --check` clean; `cargo test -p vault-core --test stats_perf --test collections` pass (1 + 8) after fixing a pre-existing duplicate-field compile error in `stats_perf.rs` that had broken `cargo test -p vault-core` on the baseline; `cargo clippy -p vault-core --all-targets -- -D warnings` still fails on pre-existing drift in `tests/collections.rs` (unused binding, dead field — untouched per scope, fails identically on the baseline). The full `cargo test -p vault-core` and the npm legs were not run on this machine within this slice's window; they are unchanged by it (no product or frontend code touched).
- `vault-retrieval bench --count 10000 --gate` (seed 42): **gate OK**; corpus 4536 mbox / 3036 whatsapp / 2428 telegram in 8.5 s, embeddings 0.3 s.
- `vault-retrieval bench --count 50000` (seed 42, informational): reproduces the baseline's scale finding.

Headline numbers — identical to the recorded 2026-11-12 baseline (deterministic corpus; no ranking change has landed since):

| Path | r@1 | r@5 (task success) | r@10 | MRR | false conf. | confident-but-wrong | lat p50/p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| FTS @ 10k | 0.53 | 0.57 | 0.57 | 0.55 | 0/6 | 0 | 0.1 / 0.2 |
| Hybrid @ 10k | 0.60 | 0.80 | 0.83 | 0.68 | 6/6 | 6 | 5.1 / 5.8 |
| FTS @ 50k | 0.53 | 0.57 | 0.57 | 0.55 | 0/6 | 0 | 0.1 / 0.2 |
| Hybrid @ 50k | 0.60 | 0.70 | 0.77 | 0.66 | 6/6 | 9 | 24.6 / 27.5 |

Per-category, per-language and per-channel tables are in [`tests/retrieval/BASELINE.md`](../../tests/retrieval/BASELINE.md) and unchanged.

## Claim calibration (what the benchmark supports and forbids)

Unchanged from the baseline record and now wired into the [support matrix](../support-matrix.md#capability-status):

- **Supported:** exact/vocabulary retrieval is fast and top-ranked (exact-term, all-terms phrase, author+topic, attachment-mention all r@5 = 1.00 on the FTS path); diacritic-less Serbian typing works (`racun` finds `račun` via the unicode61 fold); FTS is honest about absence (0/6 false confidence, 0 confident-but-wrong); the hybrid vector leg partially covers word-form variation at small scale.
- **Forbidden:** synonym/paraphrase understanding (hybrid r@5 0.25), cross-language retrieval (0.00), typo-proof search at scale (0.25 at 50k), and any wording that presents hybrid results as evidence the archive contains the answer — the vector leg returns closest matches on every no-answer probe (6/6), so Ask-mode UI language must stay "closest matches", per the product plan's no-misleading-confidence rule.

## What remains (follow-ups, not this slice)

- `tests/perf/Cargo.toml` still declares `UNLICENSED` (support-matrix caveat updated to name only that file).
- `tests/retrieval/Cargo.lock` had drifted stale against vault-core's dependency graph (missing packages; unnoticed because the workflow does not build `--locked`) — refreshed here and verified consistent; `tests/perf/Cargo.lock` likely has the same drift.
- Pre-existing `cargo clippy -p vault-core --all-targets -- -D warnings` failures in `tests/collections.rs` (see the verification record).
- Retrieval-improvement work (learned embedder, query expansion, deeper vector candidates) stays out of scope by the benchmark's own design and must re-run the harness as acceptance evidence.
- The hybrid leg's 6/6 no-answer false confidence is a standing finding: any UI copy change that downgrades it to an empty state is a product decision, tracked by the open [#189](https://github.com/acailic/mailchat-vault/issues/189) track.

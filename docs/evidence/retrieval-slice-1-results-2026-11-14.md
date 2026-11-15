# Retrieval quality slice 1 — results record (2026-11-14)

Status: **complete**. Implementation record for the first retrieval-quality slice, grounded in the [literature review of 2026-11-13](literature-review-2026-11-13.md) (its top three recommendations). Execution: subagent-driven development against the plan at [`docs/superpowers/plans/2026-11-13-retrieval-quality-1.md`](../superpowers/plans/2026-11-13-retrieval-quality-1.md), branch `feat/retrieval-quality-1`. *Merge status corrected 2026-11-14 (see the [recommendations audit](retrieval-recommendations-audit-2026-11-14.md)):* the branch series `af286c5`…`63d1fe6` (fast-forwarded over `fef48e9`, reconciled with the parallel app-identity merge as `ef6bd46`) is **not** an ancestor of `main` — that line diverged. The content reached `main` as a patch-identical rebuilt series, `0e00d85`…`d099b36`, merged via `836e9dd`; every commit pair compares equal by stable patch-id. Everything below describes work whose content is on `main`; the SHAs name the branch-side originals.

## What landed

| Commit | Change |
| --- | --- |
| `af286c5` | Plan: eval harness + subject BM25F + Lost-in-the-Middle ordering |
| `83c7f94` | **Eval harness** — `crates/vault-core/tests/retrieval_eval.rs`: 40-message synthetic corpus (24 email through `insert_mail_message` with production `"{subject}\n\n{text}"` body composition, 16 chat), labeled queries across six families, inline recall@10/nDCG@10, three ranking legs (FTS / vector / fused), reversed-ranking sanity check, per-leg floors ~5–10% below a recorded baseline |
| `c7ece20` | **`subject:` in the Ask prompt** (three columns described, `subject:` filter allowed) + **BM25F column weights** — `bm25(archive_fts, 1.0, 1.0, 3.0)` (body/author/subject, named consts) replacing `ORDER BY rank` at the three planned sites (`search_filtered`, `search_page`, `search_export_stream`) |
| `a968256` | **Authorized extension** — the fourth ranking site, the v2 relevance page in `search2.rs` (the path the workspace UI actually uses). The plan listed three sites; the implementer found and flagged the fourth, the controller authorized the same conversion. Zero `ORDER BY rank` sites remain over `archive_fts`. |
| `4ef65a9` | **Lost-in-the-Middle context ordering** — `summarize_results` feeds the top-8 blocks in order `1,3,5,7,8,6,4,2` (odds ascending, evens descending; strongest at the prompt edges), numbering `[1]..[8]` by rank unchanged; header now "numbered by relevance" |
| `531b044` | CHANGELOG entry (user-facing summary) |
| `63d1fe6` | **Final-review fixes** — two mixed email+chat labeled queries (the axis the weights move), floors re-derived from the 16-query baseline, a weight-sensitive ordering assertion through the v2 path (fails at subject weight 1.0, passes at 3.0), the stale relevance-sort sentence in `data-and-api-contracts.md` corrected |

*Annotation (2026-11-14): the SHAs above are the branch-side (`feat/retrieval-quality-1`) originals — none is an ancestor of `main`. The main-side equivalents, patch-id-identical pairwise, are `0e00d85`, `0bbaf79`, `b2ee1ef`, `9099416`, `12e92f5`, `678e438`, and `d099b36`; `d099b36` is the immediate parent of the merge `836e9dd` that delivered the slice to `main` (provenance: [retrieval-recommendations-audit-2026-11-14.md](retrieval-recommendations-audit-2026-11-14.md)). This annotation covers the `63d1fe6` references throughout this record, including Findings and the Verification record below.*

## Measured results

Harness: `cargo test -p vault-core --test retrieval_eval` (in CI with the rest of the suite). Macro recall@10 / nDCG@10:

| Leg | 14-query baseline (equal weights) | 16-query baseline (shipped weights 1.0/1.0/3.0) | Committed floors |
| --- | --- | --- | --- |
| FTS | 0.857 / 0.886 | 0.859 / 0.879 | 0.78 / 0.80 |
| Vector | 0.833 / 0.819 | 0.839 / 0.827 | 0.76 / 0.75 |
| Fused | 0.952 / 0.945 | 0.943 / 0.941 | 0.89 / 0.89 |

Notes on the numbers:

- **The subject weights are not a macro win on this corpus.** An 11-combination sweep across the allowed box left fused metrics exactly at baseline (RRF consumes only ranks) and moved FTS-leg nDCG slightly down (0.886 → 0.874 on the 14-query set). The observable win is targeted: a message carrying the term in its *subject* now outranks a longer message that merely mentions it in the body — pinned by `subject_hit_outranks_denser_body_hit` through both the v1 and v2 paths (the test fails under equal weights).
- **Floors were re-derived honestly when the query set grew.** Vector floors *rose* with the macro (0.75→0.76, 0.74→0.75); the fused recall floor moved *down* 0.90→0.89 because the mixed-channel queries pull the macro to 0.943 (0.90 would have violated the 5–10%-below-macro rule). Headroom is unchanged (0.052 → 0.053).

## Findings beyond the numbers

1. **Email subject terms double-score.** `mail.rs` composes the subject into the email body prefix, so an email subject-term hit scores in both the subject column (×3) and the body column (×1) — roughly 4× a chat body hit. Measured: a chat-target query's FTS nDCG@10 fell 0.704 → 0.531 under the shipped weights (emails outrank the relevant chats). The two mixed-channel queries added in `63d1fe6` pin this axis; the vector leg rescues it in fusion on the current corpus.
2. **Open decision — the subject-in-body prefix.** The literature review left "does the email subject prefix stay once real weighting exists?" open. It is now measurable with the mixed-channel queries. Removing it changes stored bodies (and therefore revision hashes and re-import dedup comparisons) — deliberately out of this slice; it needs its own change with migration reasoning.
3. **LiM ordering is prompt-side only.** Citation numbering, the summary contract, and the UI source rows are unchanged. The effect on answer quality is not yet measured (no A/B instrument); a U07-style before/after run against `tests/retrieval` is the natural instrument if wanted.
4. **Plan-miss caught in execution:** the plan's "three ranking sites" missed the v2 `search2.rs` site — found by the task implementer, authorized as an extension, and now pinned by its own weight-sensitive test.

## Relationship to the U07 benchmark (`tests/retrieval`, PR #76)

Complementary instruments, deliberately different contracts:

- **`crates/vault-core/tests/retrieval_eval.rs` (this slice)** — an in-CI *quality gate*: fixed 16-query labeled corpus, recall@10/nDCG@10 floors per leg, fails the suite on ranking regressions.
- **`tests/retrieval` / `vault-retrieval` (U07)** — a *measurement instrument* over the product's public API (`search_archive`, `llm::ask` without an LLM): top-5 recall, recall@1/@10, MRR, false-confidence cases, latency percentiles, per category and language, at 10k/50k scales, with recorded baselines in `tests/retrieval/BASELINE.md` and no quality gates by design.

The U07 baseline was recorded after slice 1 landed, i.e. under the shipped BM25F weights and LiM ordering.

## Verification record

- Per-task reviews (spec compliance + code quality) all approved; the Task 2 review independently reproduced the ordering flip, and the final whole-branch review independently reproduced the harness numbers before approving (verdict: ready to merge).
- Final-review findings (2 Important, 2 Minor) fixed in `63d1fe6`; fix re-review approved.
- Gates at merge time: `cargo test -p vault-core` green (41+ suites), `cargo fmt --check` clean, `cargo clippy -p vault-core --all-targets -- -D warnings` clean. The `src-tauri` workspace member cannot compile on the development host used for this slice (no webkit2gtk/javascriptcoregtk dev stack; release builds run in containers per `docs/release.md`) — mechanically verified instead: the branch touches no files outside `crates/vault-core`, `docs`, and `CHANGELOG.md`, and changes no `vault-core` public API, so the Tauri layer cannot be affected.

## What remains

Literature-review recommendations 4–12 remain open: recency/history-aware ranking, local pseudo-relevance feedback + consent-gated Query2doc expansion, citation-faithfulness checks, small-to-big thread-context retrieval, tunable fusion, the bundled ONNX MiniLM embedder, quantization/ANN thresholds, JWZ subject-grouping for threading, and near-duplicate fingerprints. See the [ranked list](literature-review-2026-11-13.md#ranked-recommendations--what-we-could-add) and the [triage plan](../superpowers/plans/2026-11-14-retrieval-quality-triage.md) that slices them into ordered, harness-gated units (R1 first; item 9 decision-gated, item 10 codified, item 12 deferred).

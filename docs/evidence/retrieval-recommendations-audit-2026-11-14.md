# Retrieval recommendations audit — status on main (2026-11-14)

Scope: umbrella follow-up to the [literature review of 2026-11-13](literature-review-2026-11-13.md), which ranked 12 retrieval improvements and recorded items 1–3 as merged in commit `63d1fe6`. That recorded ancestry turned out to be wrong: `63d1fe6` is not on `main` today. This audit re-verifies every one of the 12 recommendations against the code actually on `main` at `ffb9229` (2026-11-14, tip of `origin/main`), resolves how the slice-1 content reached `main` anyway, and records the per-item evidence — file-level for what is present, search-level (pattern + negative result) for what is not — so the backlog can be triaged and the check re-run. Method: history via a deepened local clone plus the GitHub API (`gh api repos/acailic/mailchat-vault/...`); code status via pattern searches over `crates/vault-core/src/` and `crates/vault-core/tests/` with the exact patterns quoted per item. Documentation-only change; no product code, schema, or prompt was touched, and nothing was written to GitHub issues or pull requests.

## The `63d1fe6` discrepancy and its resolution

**The claim.** The [results record](retrieval-slice-1-results-2026-11-14.md) (written 2026-11-14 in `af4e2af`) and the literature review's Status line both say slice 1 (recommendations 1–3) was "merged to `main` as `63d1fe6`".

**The ancestry fact.** `63d1fe6160248eb777fbb4fa90ec08a73e543728` exists on the remote (fetchable by SHA) but heads no branch. Verified two ways on 2026-11-14:

- Local, after deepening the clone: `git merge-base --is-ancestor 63d1fe6 origin/main` → exit 1 (not an ancestor).
- GitHub compare `63d1fe6...main` → `status: "diverged"`, 184 commits on the slice side not on `main`, 88 commits on `main` not on the slice, merge base `9bd209a` ("bootstrap: README + gitignore", 2026-11-01).

So the two histories split at the repository bootstrap; each side carries its own nearly complete copy of the early work.

**How the slice-1 content reached `main`.** The same seven-commit series exists twice — once on the orphaned line (branch `feat/retrieval-quality-1`, based on `fef48e9`) and once rebuilt on the other line (based on `cec751e`) with identical messages, identical author timestamps, and patch-identical diffs, but different parentage and therefore different SHAs. Every pair compares equal under `git diff-tree -p <sha> | git patch-id --stable`:

| Branch-side (orphaned) | Main-side equivalent | Subject |
| --- | --- | --- |
| `af286c5` | `0e00d85` | Plan: eval harness + subject BM25F + context ordering |
| `83c7f94` | `0bbaf79` | Eval harness (`tests/retrieval_eval.rs`) |
| `c7ece20` | `b2ee1ef` | `subject:` in the Ask prompt + BM25F column weights |
| `a968256` | `9099416` | v2 relevance path (`search2.rs`) ranked with the same weights |
| `4ef65a9` | `12e92f5` | Lost-in-the-Middle context ordering |
| `531b044` | `678e438` | CHANGELOG entry |
| `63d1fe6` | `d099b36` | Final-review fixes (mixed-channel queries, floors, v2 assertion) |

The main-side series was merged as `d099b36` by `836e9dd` ("Merge origin/main (retrieval quality slice 1) into local main", 2026-11-13 23:25:14 UTC), which **is** an ancestor of `main` (62 commits below tip `ffb9229`). The delivery chain into today's `main`:

```
0e00d85 → 0bbaf79 → b2ee1ef → 9099416 → 12e92f5 → 678e438 → d099b36
                                                        ↓ (merged)
836e9dd → f83b764 (U07 benchmark) → af4e2af (slice-1 results record) → 641bb4d
                                                        ↓ (merged by 816d341)
… → f9a605b → b75768f → merge/all-branches-20261114 merges → ffb9229 (main tip)
```

Corroborating detail: the orphaned line's own reconciliation merge `ef6bd46` (parents `f488d44` + `63d1fe6`) carries the same commit message as `836e9dd` and the same committer timestamp to the second (2026-11-13 23:25:14 UTC) — the same merge performed on both divergent copies of the repository. Only the `836e9dd` copy is on `main`.

**Conclusion.** The recorded status was right on content, wrong on ancestry: recommendations 1–3 are on `main`, delivered by the patch-identical rebuilt series through `836e9dd`, not by `63d1fe6`. The stale merge claims in the two evidence documents are corrected (or annotated) in this change; the historical commit table in the results record is annotated, not rewritten.

## Per-recommendation status

| # | Recommendation | Status on `main` | Primary evidence |
| --- | --- | --- | --- |
| 1 | Retrieval eval harness | **Present** | `crates/vault-core/tests/retrieval_eval.rs` |
| 2 | `subject:` BM25F weighting | **Present** | `storage.rs:787-789`, `:1686`, `:1813`, `:1876`; `search2.rs:1074`; `llm.rs:456` |
| 3 | Lost-in-the-Middle ordering | **Present** | `llm.rs:677`, `:712`, `:718` |
| 4 | Recency- and history-aware ranking | **Open** | searches below, all negative |
| 5 | PRF + consent-gated LLM expansion | **Open** | searches below, all negative |
| 6 | Citation-faithfulness checks | **Open** | searches below, all negative |
| 7 | Small-to-big thread context | **Open** | searches below, all negative |
| 8 | Tunable fusion | **Open** | `vector.rs:328` unchanged |
| 9 | Bundled real-embedding model | **Open** (seam exists) | `vector.rs:85`, `:106` |
| 10 | Quantization/ANN thresholds codified | **Open** | no decision-rule artifact |
| 11 | Threading: subject grouping, dummy promotion | **Open** | one-hop only |
| 12 | Near-duplicate fingerprints | **Open** | searches below, all negative |

### Item 1 — eval harness: present

`crates/vault-core/tests/retrieval_eval.rs` (947 lines), delivered by `0bbaf79` (final shape `d099b36`): a 40-message synthetic corpus (24 email through `insert_mail_message` with the production `"{subject}\n\n{text}"` body composition, 16 chat), 16 labeled queries across seven families (subject-term, author-name, term+date-range, paraphrase/synonym, chat-slang, multi-term AND, mixed-channel), inline `recall_at`/`ndcg_at` at k=10 (`retrieval_eval.rs:625`, `:639`), three ranking legs (FTS, vector via `HashEmbedder`, fused via `rrf_fuse`), committed floors at `retrieval_eval.rs:72-77` (FTS 0.78/0.80, vector 0.76/0.75, fused 0.89/0.89), and the reversed-ranking sanity check `reversed_ranking_scores_strictly_worse` (`retrieval_eval.rs:872`). Runs with the suite in CI like `tests/vector_perf.rs`.

### Item 2 — `subject:` BM25F weighting: present

- Named weight consts `BM25_WEIGHT_BODY/AUTHOR/SUBJECT = 1.0/1.0/3.0` at `storage.rs:787-789`, with the BM25F rationale documented above them (`storage.rs:770-786`).
- Weighted `ORDER BY bm25(archive_fts, …)` at the three v1 sites — `storage.rs:1686` (`search_filtered`), `:1813-1814` (`search_page`), `:1876-1877` (`search_export_stream`) — and the fourth, v2 site `search2.rs:1074-1075` (the IPC relevance path), importing the same consts (`search2.rs:42`).
- The Ask translation prompt explicitly allows `subject:` (`llm.rs:456`), asserted by a prompt test at `llm.rs:1179-1180`.
- No `ORDER BY rank` over `archive_fts` remains: the pattern `ORDER BY rank` matches only the comment at `storage.rs:779` explaining the old ordering.
- Weight-sensitive tests pin the behavior under both paths: `subject_hit_outranks_denser_body_hit` (`tests/storage.rs:225`, v1 `search_page` + `search_filtered`) and `relevance_page_subject_hit_outranks_denser_body_hit` (`search2.rs` inline tests, `:2056`), each asserting the subject hit outranks the denser body hit — the flip the weights exist to produce.

### Item 3 — Lost-in-the-Middle ordering: present

`lim_order` (`llm.rs:677-681`) places context blocks odds-ascending then evens-descending; used by `summarize_results` at `llm.rs:712` with the header "Messages (numbered by relevance)" at `llm.rs:718`. The `1,3,5,7,8,6,4,2` order is pinned by `lim_order_places_strongest_first_and_last` (`llm.rs:1221-1232`, the n=8 assertion at `:1227`) and end-to-end by `summary_prompt_orders_blocks_lost_in_the_middle` (`llm.rs:1287-1308`, marker-order assertion at `:1296`); the header wording is asserted at `llm.rs:1351-1352`. Citation numbers still follow rank, not position.

### Item 4 — recency- and history-aware ranking: open

Searches over `crates/` (all negative): `time_decay|time-decay|recency|decay_factor|opens_boost|result_opens` → one unrelated test name (`tests/integrity/wal_crash.rs:112`); `clicked|previously_opened|boost` in `crates/vault-core/src/` → doc comments about subject boosting only; `message_opens|result_opens|opened_count` in `crates/vault-core/src/` → no matches (no result-opens table exists). `rrf_fuse` consumes pure ranks with no time or history term (`vector.rs:335-341`), and the FTS ranking orders by `bm25(…), m.id` only.

### Item 5 — pseudo-relevance feedback / LLM expansion: open

Searches over `crates/` (all negative): `pseudo.?relevance|query_expansion|expansion_terms|expand_terms|rocchio` → no matches; `expand` in `llm.rs` → no matches. The translate step still produces a single validated MATCH plan with the plain-term fallback; no term expansion anywhere.

### Item 6 — citation-faithfulness checks: open

Searches over `crates/` (all negative): `faithfulness|citation_check|supported_by|overlap_ratio|verify_citation` → no matches. What exists is rendering and linkage, not verification: `[n]` blocks in the summary prompt (`llm.rs:664`, marker helper at `:1272-1284`) and the pre-existing M5/#48 UI in which every citation opens the exact archived message. No local structural check and no second-pass judge.

### Item 7 — small-to-big thread context: open

`summarize_results` (`llm.rs:698-739`) sends exactly the top-N truncated message blocks plus, since `803f951` (ask-collect-links, 2026-11-14), optional `url · author · date` lines for collected links — links, not thread context. Searches (all negative): `thread_summary|per-thread|conversation_summary` in `crates/vault-core/src/` → no matches; no ±N thread window is fed for cited rows. `chat_context`/`conversation_context_v2` remain reading-pane APIs.

### Item 8 — tunable fusion: open

`RRF_K` is still a hard-coded `pub const RRF_K: u32 = 60` (`vector.rs:328`) consumed by the unweighted `rrf_fuse` (`vector.rs:335-341`). No settings or IPC exposure: `rrf|fusion|opens` in `settings.rs`/`ipc.rs` → no matches. Shipped default unchanged, as the recommendation requires.

### Item 9 — bundled real-embedding model: open (the seam exists)

The `Embedder` trait (`vector.rs:85`) and the model-id-versioned embedding rows remain, with the sole implementation `HashEmbedder` (`MODEL_ID = "hash-ngram-v1"`, `DIM = 512`, `vector.rs:106-111`). Searches over `crates/` for `onnx|minilm|sbert` → only the design note at `vector.rs:5-15` recording that a bundled model stays a drop-in option; no second model, no build-time artifact.

### Item 10 — quantization/ANN thresholds codified: open

No decision-rule artifact exists beyond the recommendation text itself: `product quantization|HNSW|sqlite-vec|usearch` over `docs/` and `crates/` → the linear-scan rationale in `docs/ai-search.md` (predating the review; untouched by the slice commits) and the literature review's own item; no PQ code, no threshold document, no amendment in `docs/decisions.md`.

### Item 11 — threading subject grouping / dummy promotion: open

Threading is still the one-hop In-Reply-To/References resolution documented at `message.rs:34-35`, `mail.rs:155-168`, and `storage.rs:3239`. Searches for JWZ step-4/5 semantics over `crates/vault-core/src/` (all negative): `dummy|promot*` → only object-store/backup-container lifecycle vocabulary; `subject_group|subject_root|subject_thread|normalize_subject` → no matches.

### Item 12 — near-duplicate fingerprints: open

Searches over `crates/` (all negative): `simhash|minhash|sim_hash|min_hash` → no matches; `sketch|fingerprint` in `crates/vault-core/src/` → only `VaultFingerprint` (`storage.rs:66`), the backup/restore identity check, which is unrelated to per-message near-duplicate sketches. No fingerprint column beside the revision hash.

## Corrections made in this change

1. `literature-review-2026-11-13.md` Status line (was line 211): rewritten — items 1–3 are on `main` (delivered by the content-identical series merged as `836e9dd`; `63d1fe6` itself is not an ancestor of `main`), items 4–12 confirmed open by this audit, with a link to this document.
2. `retrieval-slice-1-results-2026-11-14.md` merge sentence (line 3): corrected — the branch series landed on `main` as the patch-identical rebuilt series (`0e00d85`…`d099b36`) merged via `836e9dd`; the `63d1fe6` fast-forward and the `ef6bd46` reconciliation describe the orphaned line, not `main`'s ancestry.
3. The historical commit table in the same document is annotated in place (not rewritten): the SHAs are the branch-side originals, with the main-side equivalents and the equivalence method named. The remaining `63d1fe6` references in that document (the table rows, the Findings note, and the Verification record) describe what those branch commits did and remain true of them; the annotation covers them all.

`CHANGELOG.md` was searched for `63d1fe6`: no references — its slice-1 entries carry no commit SHA and needed no correction. The full pattern search over `docs/` for `63d1fe6|fef48e9|ef6bd46|feat/retrieval-quality-1` found no further merge-status claims (the plan document's merge step is a plan instruction, not a status claim).

## Verification record

- Ancestry: `git fetch --deepen=300 origin main`, then `git merge-base --is-ancestor 63d1fe6 origin/main` → not an ancestor; `git rev-list --count` gives 88 (main not on slice) and 184 (slice not on main), merge base `9bd209a` — matching GitHub's compare API (`status: diverged`, `ahead_by: 184`, `behind_by: 88`).
- Delivery: `836e9dd` verified as an ancestor of `origin/main` (`git rev-list --ancestry-path --count 836e9dd..origin/main` → 62); the commit series and merge chain read directly from the deepened history.
- Content equivalence: all seven commit pairs compare equal by stable patch-id (`git diff-tree -p <sha> | git patch-id --stable`); the `63d1fe6`↔`d099b36` pair independently confirmed by `gh api …/commits?path=crates/vault-core/tests/retrieval_eval.rs` showing the main-side series.
- Remote state: `63d1fe6` heads no branch (`branches-where-head` → 0; `git ls-remote` has no retrieval branch).
- Code status: pattern searches as quoted per item, run against the working tree of `main` at `ffb9229` (clean, up to date with `origin/main`).
- Suites were **not rerun** for this change: `cargo` is not installed on this host, and the change touches no code — consistent with the repository's precedent for documentation-only changes (see the M6/M7 records in the implementation roadmap). If a Rust toolchain is available, `cargo test -p vault-core --test retrieval_eval` should pass unchanged; nothing in this diff can affect it.
- Scope check: `git diff` for this change touches files under `docs/` only; no `crates/`, `src-tauri/`, or frontend files.
- No GitHub issues or pull requests were written; the umbrella issue stays untouched.

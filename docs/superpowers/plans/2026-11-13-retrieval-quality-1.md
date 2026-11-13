# Mailchat Vault — Retrieval Quality Slice 1: Eval Harness, Subject BM25F, Context Ordering

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the top three recommendations from `docs/evidence/literature-review-2026-11-13.md`: (1) a BEIR-style retrieval eval harness over local fixtures so ranking changes are measurable, (2) expose the already-indexed `subject` column to the Ask translator and rank with BM25F-style column weights, (3) order the RAG summary context per *Lost in the Middle* (Liu et al. 2023) instead of strict rank order.

**Architecture:** All three changes are backend-only in `crates/vault-core` (Rust + SQLite FTS5). The canonical `archive_fts` index already has columns `(body, author, subject)` kept in sync by triggers (`crates/vault-core/src/storage.rs:344`-366); email bodies additionally carry the subject as a text prefix because `crates/vault-core/src/mail.rs:201`-206 composes `format!("{subject}\n\n{text}")` — this slice does NOT change that composition (it flows into stored payloads, revision hashes, and export). The Ask pipeline is `crates/vault-core/src/llm.rs`: translate (LLM → `QueryPlan` MATCH expression + dates) → validate/fallback → run (`run_plan`, `search_filtered`) → hybrid rank (`rank_hybrid` fuses FTS ids with `vector_scan` ids via `rrf_fuse`, `RRF_K = 60`, `VECTOR_CANDIDATES = 200`) → summarize (`summarize_results`, top `DEFAULT_SUMMARIZE_TOP = 8` rows, bodies truncated to `SUMMARY_BODY_LIMIT = 500` chars, numbered `[1]..[8]` by rank).

**Tech Stack:** Rust, rusqlite with bundled SQLite FTS5, `chrono`. No new dependencies, no frontend changes, no schema migrations.

## Global Constraints

- **Backend-only:** no changes under `src/`, `src-tauri/`, `index.html`; no new crates; no schema migration (the `subject` column already exists — this was verified against the code and the literature-review file's item 2 was corrected accordingly).
- **Offline promise holds:** the eval harness uses synthetic local fixtures only; no network, no model downloads; `HashEmbedder` (`hash-ngram-v1`, 512 dims) stays the embedding model.
- **Every test asserts.** The eval harness must fail on regressions, not print scores.
- **No behavior change to the plain-search fallback:** `search::build_match_query` semantics stay exactly as-is.
- **Citation numbering is unchanged by Task 3:** blocks stay numbered `[1]..[top]` by rank; only the *order of blocks inside the LLM prompt* changes.
- **Weights are named constants** in `storage.rs`, tuned only within the ranges this plan allows, and only accepted if the Task 1 harness shows no fused nDCG@10 regression.
- Gates after every task: `cargo test -p vault-core` green, `cargo fmt --check` clean, `cargo clippy -p vault-core --all-targets -- -D warnings` clean (run from the worktree root `/home/nistrator/.treehouse/mailchat-vault-631660/5/mailchat-vault` — the main checkout is in use by a parallel session; never touch it).
- Commit style follows the repo: `feat(search): …` / `test(search): …`, imperative, scoped.

---

### Task 1: Retrieval eval harness (`tests/retrieval_eval.rs`)

**Files:**
- Create: `crates/vault-core/tests/retrieval_eval.rs`

**Why first:** Task 2's BM25F weights must be tuned against measurements, not taste. This harness is that measurement, and it gates the merge.

**Steps:**

- [ ] Build a synthetic corpus of **at least 40 messages** in an in-memory `Storage` (`Storage::open_in_memory()`), following the seeding pattern of `crates/vault-core/tests/vector_search.rs` (pin `TZ=UTC` once via a `Mutex`+`Once` guard exactly as that file does):
  - ~24 **email rows** inserted through `Storage::insert_mail_message` (storage.rs:1157), each with a real subject. Mirror the production mail pipeline: the stored `body` passed in the `NewMessage` must be `format!("{subject}\n\n{text}")` (as `mail.rs` composes it), with the subject also passed as the separate `subject` argument — do not invent a body shape production never produces.
  - ~16 **chat rows** through `Storage::insert_message` (subjects stay empty, as the chat path does).
  - Include: several messages sharing a distinctive subject term but different bodies; messages sharing an author; a date-clustered thread; near-duplicate paraphrased bodies (for the vector leg); clearly unrelated filler.
- [ ] Define **at least 12 labeled queries** covering the families: subject-term, author-name, term+date-range, paraphrase/synonym (expects the vector leg to help), chat slang, multi-term AND. Each query lists the `external_id`s of its relevant messages (binary relevance). Store queries + labels as a `const` array with a one-line comment each.
- [ ] Implement metrics inline (no new deps): `recall_at(relevant, ranked, k)` and `ndcg_at(relevant, ranked, k)` (binary gains, ideal DCG as the normalization; log2 discount).
- [ ] Evaluate three legs over every query:
  - **FTS leg:** `llm::run_plan` with a hand-built `PlannedQuery` (construct `QueryPlan` directly — check its visibility; if private, use `storage.search_filtered` with the same MATCH text) at limit 50; ranked ids in order.
  - **Vector leg:** `vector::run_embedding_job` with `HashEmbedder` first, then `storage.vector_scan(...)` at candidates 50.
  - **Fused:** `vector::rrf_fuse(&fts_ids, &vec_ids, 10)`.
- [ ] Assert, in this order:
  1. **Sanity:** a deliberately reversed ranking scores strictly worse than the real fused ranking on aggregate nDCG@10 (proves the metrics detect bad orderings).
  2. **Leg thresholds:** macro-averaged recall@10 and nDCG@10 for FTS, vector, and fused legs each meet a `const` floor. First measure the actual baseline with a temporary run, then commit floors set ~5–10% below the measured values, with the measured baseline recorded in a comment block (e.g. `// baseline 2026-11-13: fts ndcg@10 = 0.87, fused = 0.91 — floors at 0.82 / 0.86`).
  3. **Fusion helps or at least never hurts:** fused recall@10 ≥ FTS-only recall@10 (macro) on this corpus.
- [ ] Run `cargo test -p vault-core --test retrieval_eval` and the full task gates; record the baseline numbers in the task report.

### Task 2: `subject:` in the Ask prompt + BM25F column weights

**Files:**
- Modify: `crates/vault-core/src/llm.rs` (translation prompt, ~lines 352-359; doc comments)
- Modify: `crates/vault-core/src/storage.rs` (three ranking sites: `search_filtered` ~1543, `search_page` FTS branch ~1663, `search_export_stream` ~1725)
- Test: unit/integration tests adjacent to existing ones (llm.rs `mod tests`, `tests/retrieval_eval.rs` floors)

**Steps:**

- [ ] `translation_system_prompt` (llm.rs:352): describe **three** columns — `body` (message text; for email it includes the subject), `author`, `subject` (email subject lines; chat messages have none) — and allow the column filters `body:`, `author:`, and `subject:`. Keep every other instruction identical. (The `validate`/`is_valid_match` path needs no change: `subject:` filters already compile against `archive_fts`; the restriction was prompt-only.)
- [ ] In `storage.rs`, add named constants near the FTS ranking sites: `const BM25_WEIGHT_BODY: f32 = 1.0; const BM25_WEIGHT_AUTHOR: f32 = 1.0; const BM25_WEIGHT_SUBJECT: f32 = 3.0;` with a comment citing the BM25F rationale (subject/author evidence is field-dependent; Robertson & Zaragoza 2009) and noting `bm25()` takes weights in column order `(body, author, subject)`, ascending order (more negative = better, same as `rank`).
- [ ] Replace `ORDER BY rank, m.id` with `ORDER BY bm25(archive_fts, {W_BODY}, {W_AUTHOR}, {W_SUBJECT}), m.id` in **all three** sites — format the constants into the SQL text (they are compile-time constants, not user input; do not bind them as parameters since FTS5 auxiliary functions need constant arguments). Keep `snippet(archive_fts, 0, …)` untouched (column 0 = body).
- [ ] Tune within the allowed range only: subject weight ∈ [2.0, 4.0], author weight ∈ [1.0, 2.0]. Re-run `--test retrieval_eval`; accept a change only if fused macro nDCG@10 and recall@10 are ≥ the Task 1 baseline. Record final values + harness numbers in the task report. If 3.0/1.0 already meets the baseline, keep it.
- [ ] Tests:
  - Prompt: the system prompt contains all three column names and the `subject:` allowance (extend the existing prompt-shape test in llm.rs `mod tests` if one asserts the old two-column text; grep for `two columns` first).
  - Plan flow: a `QueryPlan` with `match_expr = "subject: vault"` validates (`is_valid_match`) and returns the subject-bearing messages through `run_plan`/`search_filtered`.
  - Ordering: with the crafted pair — A: subject `"vault migration plan"`, short body composed `"{subject}\n\n(text without further vault mentions)"`; B: no subject term, body mentioning `vault` twice with a longer body — A ranks above B under the new weights (assert exact order of the two ids from a query for `vault`).
- [ ] Full task gates + report the before/after harness numbers.

### Task 3: Lost-in-the-Middle context ordering in `summarize_results`

**Files:**
- Modify: `crates/vault-core/src/llm.rs` (`summarize_results` ~570, `context_block` ~543, header string ~589)
- Test: llm.rs `mod tests` (uses the pub `MockLlm`, llm.rs:213)

**Steps:**

- [ ] In `summarize_results`, keep numbering by rank (`context_block(m, i + 1)`), but **arrange the blocks** so the strongest evidence sits at the beginning and the end of the prompt: for `n = min(top, results.len())` blocks, order ranks as all odd ranks ascending first, then all even ranks descending (n=8 → `1,3,5,7,8,6,4,2`; n=3 → `1,3,2`; n=2 → `1,2`; n=1 → `1`). Implement as a small pure helper `fn lim_order(n: usize) -> Vec<usize>` (0-based ranks) with the permutation documented as the Lost-in-the-Middle placement (Liu et al. 2023: positional degradation is largest mid-context).
- [ ] Change the user-prompt header from `Messages (most relevant first):` to `Messages (numbered by relevance):` — the block order no longer matches numbering, so the old header would be false.
- [ ] `SUMMARY_SYSTEM_PROMPT` stays unchanged (its `cite as [1], [2] in order of reliance` rule is number-based, not position-based).
- [ ] Tests (MockLlm records every prompt — assert on the recorded user prompt):
  - n=8: the order of `[k]` markers in the prompt text is exactly `1,3,5,7,8,6,4,2`, and each block's content still matches its ranked message (cite the right body prefix).
  - n=1, n=2, n=3 permutations; and `top < results.len()` truncation still takes the top `top` rows before permuting.
  - Header contains `numbered by relevance` and no longer `most relevant first`.
  - Existing summarize tests (`summarize_skips_when_no_results`, etc.) stay green; grep `most relevant first` for stragglers in tests/.
- [ ] Full task gates.

### Wrap-up (controller)

- [ ] CHANGELOG entry under `[0.2.0] - pending tag` (### Added): Ask understands `subject:` filters and ranks subject/author matches higher (BM25F); Ask summary context ordered for mid-context degradation; retrieval gains a measured eval harness. Cross-link the literature review doc.
- [ ] Final whole-branch review, then merge `feat/retrieval-quality-1` into `main` (no-ff) and push.

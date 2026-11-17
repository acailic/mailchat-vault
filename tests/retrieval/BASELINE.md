# Retrieval quality baseline — `vault-retrieval`

Recorded baseline results for the U07 retrieval-quality benchmark
(`docs/internal/user-success-plan.md`, work package U07: "Retrieval quality
benchmark and claim calibration"). The harness lives in this directory;
it measures **only** — improvement work (learned embedding models, query
expansion, pagination changes) is out of scope by design and conditional
on these findings.

The harness is a standalone crate (own workspace, like `tests/perf`)
that depends on `vault-core` by path and exercises **only the product's
public API**: `Storage::insert_message` for the corpus,
`Storage::search_archive` and `llm::ask` (without an LLM) for the
measured search paths, `run_embedding_job` for the vector leg. No
product code is modified by anything in this directory.

## How to run

```sh
cd tests/retrieval
cargo run --release -- bench --count 10000 --json results.json  # default scale
cargo run --release -- bench --count 50000                       # second scale point
cargo run --release -- bench --gate                              # instrument floors (CI)
cargo run --release -- bench --self-check-only                   # corpus/label integrity only
```

Useful flags: `--seed N` (default 42), `--data-dir DIR` (fixture db;
default system temp `/vault-retrieval`, overwritten per identical
count/seed), `--runs N` (timed repetitions per question, median
reported; default 5). Runtime at 10k on the baseline machine: ~4 s
corpus build + ~0.3 s embeddings + measurement.

## What is measured

**Corpus.** Deterministic synthetic archive (same `--count`/`--seed`
builds identical corpora everywhere): everyday email and chat noise
from a fixed 99-word vocabulary, seeded LCG, Zipf-skewed word choice,
bodies chat-short (3–12 words) / email-long (30–120 words), spread over
mbox / WhatsApp / Telegram in a 45/30/25 mix, 2018–2026 dates. The 30
labeled correct originals and 22 deliberate confusers from the question
set are scattered at deterministic positions. The background vocabulary
is disjoint from every token the questions search for, so lexical
difficulty comes from the designed confusers, not accidental overlap —
FTS numbers below are therefore a **best case for the lexical
categories**; real archives are lexically messier.

**Paths** (both are existing product code; nothing new is built):

| path | what runs | what the user surface is |
| --- | --- | --- |
| `fts` | `Storage::search_archive` (v2): FTS5 bm25 relevance, default page (limit 50, snippets + exact total) | the workspace's ordinary search box |
| `hybrid` | `llm::ask` with no LLM: the same FTS5 leg fused with the local hash-vector leg (`hash-ngram-v1`) by reciprocal rank fusion | the local ranking inside optional Ask mode |

The two paths are not latency-comparable 1:1 (the v2 page does
snippets + an exact `COUNT`; the hybrid ask does the vector scan) —
compare each path against itself across runs.

**Question set** (36 labeled questions — U07 floor is 30): 30 answerable
(one correct original each) + 6 no-answer probes; 18 English / 18
Serbian (Latin script); 15 email / 15 chat originals. Categories:
`exact-term` (4), `phrase` (4), `paraphrase` (4), `inflection` (4),
`typo` (4), `attachment` (4), `author-topic` (4), `cross-language` (2),
`no-answer` (6). The attachment category measures finding *the message
that carried the file* via its text; attachment-content search remains
a separate research question (per the product plan) and is not claimed.

**Metrics.**

- **recall@k** — the labeled correct original ranks within the top k of
  the first result page (answerable questions only).
- **correct-original task success** — recall@5: a user scanning the
  first five summaries finds the exact original (U07's named metric).
- **MRR** — mean reciprocal rank (answerable only).
- **false confidence** — no-answer probes returning a non-empty first
  page: the system asserts content it does not have.
- **confident but wrong** — answerable question with a non-empty page
  while the correct original sits outside the top 5.
- **latency** — per question the median of `--runs` repetitions of the
  full call; p50/p95 taken across the question medians. The perf
  harness (`tests/perf/BASELINE.md`) owns the exhaustive latency
  contracts; these numbers place retrieval cost at benchmark scale.

**Self-check** (always runs first): every exact-term question matches
exactly one message and it is the labeled original; every no-answer
query matches nothing; every target row id resolves. A failing
self-check exits before any measurement.

## Baselines — 2026-11-12 (seed 42)

Machine: Linux x86_64, 16 CPUs (rustc stable, release profile, LTO).
Corpus: 10k messages (4536 mbox / 3036 whatsapp / 2428 telegram) built
in 4.2 s; embeddings for all 10k in 0.23 s. 5 timed runs per question.

### FTS path (`search_archive`, v2) — 10k

| category | n | r@1 | r@5 | r@10 | MRR | f-conf | c-wrong | lat p50/p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| exact-term | 4 | 1.00 | 1.00 | 1.00 | 1.00 | 0/0 | 0 | 0.1 / 0.2 |
| phrase | 4 | 0.75 | 1.00 | 1.00 | 0.88 | 0/0 | 0 | 0.2 / 0.2 |
| attachment | 4 | 1.00 | 1.00 | 1.00 | 1.00 | 0/0 | 0 | 0.2 / 0.2 |
| author-topic | 4 | 1.00 | 1.00 | 1.00 | 1.00 | 0/0 | 0 | 0.2 / 0.2 |
| inflection | 4 | 0.00 | 0.00 | 0.00 | 0.00 | 0/0 | 0 | 0.1 / 0.1 |
| typo | 4 | 0.25 | 0.25 | 0.25 | 0.25 | 0/0 | 0 | 0.1 / 0.2 |
| paraphrase | 4 | 0.00 | 0.00 | 0.00 | 0.00 | 0/0 | 0 | 0.1 / 0.1 |
| cross-language | 2 | 0.00 | 0.00 | 0.00 | 0.00 | 0/0 | 0 | 0.1 / 0.1 |
| no-answer | 6 | — | — | — | — | **0/6** | 0 | 0.1 / 0.1 |
| **overall** | 36 | 0.53 | 0.57 | 0.57 | 0.55 | 0/6 | 0 | 0.1 / 0.2 |

By language: en r@5 0.53 / MRR 0.50 · sr r@5 0.60 / MRR 0.60.
By channel: email r@5 0.60 · chat r@5 0.53.

### Hybrid path (`llm::ask`, no LLM — FTS5+RRF) — 10k

Ranking modes across the 36 questions: hybrid-fts-vector ×17,
vector-only ×19 (the vector leg alone answers whenever FTS matches
nothing).

| category | n | r@1 | r@5 | r@10 | MRR | f-conf | c-wrong | lat p50/p95 ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| exact-term | 4 | 1.00 | 1.00 | 1.00 | 1.00 | 0/0 | 0 | 4.2 / 4.7 |
| phrase | 4 | 0.75 | 1.00 | 1.00 | 0.88 | 0/0 | 0 | 4.3 / 4.5 |
| attachment | 4 | 1.00 | 1.00 | 1.00 | 1.00 | 0/0 | 0 | 4.2 / 4.2 |
| author-topic | 4 | 1.00 | 1.00 | 1.00 | 1.00 | 0/0 | 0 | 4.3 / 4.4 |
| inflection | 4 | 0.50 | 1.00 | 1.00 | 0.71 | 0/0 | 0 | 4.2 / 4.4 |
| typo | 4 | 0.25 | 0.75 | 0.75 | 0.36 | 0/0 | 1 | 4.2 / 4.5 |
| paraphrase | 4 | 0.00 | 0.25 | 0.50 | 0.15 | 0/0 | 3 | 4.2 / 4.7 |
| cross-language | 2 | 0.00 | 0.00 | 0.00 | 0.00 | 0/0 | 2 | 4.2 / 4.5 |
| no-answer | 6 | — | — | — | — | **6/6** | 0 | 4.2 / 4.5 |
| **overall** | 36 | 0.60 | 0.80 | 0.83 | 0.68 | 6/6 | 6 | 4.2 / 4.6 |

By language: en r@5 0.80 / MRR 0.60 · sr r@5 0.80 / MRR 0.76.

### Second scale point — 50k (informational)

Corpus built in 41 s, embeddings in 2.4 s. FTS quality identical at
50k (the background vocabulary is disjoint, so FTS match sets do not
change with scale — determinism check, not a claim about real archives);
FTS latency still p50 0.1 / p95 0.2 ms. Hybrid: overall r@5 0.70
(10k: 0.80), latency p50 20.8 / p95 23.1 ms. The degradation sits
exactly in the hybrid's value categories — inflection r@5 1.00 → 0.75,
typo r@5 0.75 → 0.25 — because RRF fusion is rank-based and the fixed
200-candidate vector leg mixes in more noise as the archive grows.

## Observations and claim calibration

What the measured baseline supports — and what it forbids:

1. **The lexical contract is solid.** When the user's words appear in
   the message (exact term, all-terms phrase, author + topic,
   attachment mention), FTS5 bm25 puts the correct original in the top
   5 every time, and top-1 in nearly every case. The one phrase miss
   from the top (r@1 0.75) is a designed near-duplicate outranking the
   current mail — realistic and bounded.
2. **Diacritic-less Serbian typing works on the FTS leg** — measured,
   not assumed: `racun` matches `račun` because SQLite's default
   `unicode61` tokenizer folds Latin diacritics at index time. This is
   worth knowing before any "diacritic normalization" work is proposed.
3. **FTS is honest about absence.** Every no-answer probe returns an
   empty page (0/6 false confidence), and every miss category (wrong
   word forms, paraphrase, cross-language) returns empty rather than
   confident wrong pages (0 confident-but-wrong).
4. **The hybrid vector leg earns its keep on word-form variation** —
   at 10k it lifts inflection recall@5 from 0.00 to 1.00 and typo from
   0.25 to 0.75 — **but that advantage shrinks with archive size**
   (inflection 0.75, typo 0.25 at 50k). The hash embedder's documented
   "not a validated claim of synonym, cross-language or general
   semantic understanding" holds: paraphrase r@5 is 0.25 and
   cross-language is 0.00 even with fusion.
5. **Ask mode never says "nothing found".** The vector leg always
   surfaces something above the 0.08 cosine floor at personal scale
   (6/6 false confidence on no-answer probes). Any UI or claim language
   must present hybrid results as *closest matches*, not as evidence
   the archive contains the answer. This is the single most
   decision-relevant finding for claim calibration.
6. **Latency is not the retrieval problem.** FTS pages are sub-ms at
   50k; the hybrid ask is a linear vector scan (4.2 ms → 20.8 ms p50,
   10k → 50k, consistent with the vector layer's own perf contracts).
7. **Claims the baseline forbids:** synonym/paraphrase understanding,
   cross-language retrieval, typo-proof search at scale, and "AI finds
   what words cannot" as an unqualified statement. **Claims it
   supports:** exact/vocabulary retrieval is fast and top-ranked;
   diacritic-less typing works; word-form variation is partially
   covered by the local vector leg, size-dependently.

These findings condition any future build response (a learned embedding
model, query expansion, deeper vector candidates) — that work is
explicitly out of scope for this benchmark and must re-run this harness
as its acceptance evidence.

## CI gate policy

The `Retrieval` workflow (`.github/workflows/retrieval.yml`) runs the
harness at 10k on every PR with `--gate`. The gate protects **the
instrument, never a measured quality level**: self-check integrity,
FTS exact-term recall@5 = 1.00, FTS phrase recall@5 ≥ 0.75, zero FTS
false confidence, and a measured outcome for every question. If those
floors fail, the corpus or labels rotted (or ranking broke outright)
and the benchmark's numbers cannot be trusted; measured-quality
regressions above those floors are findings, not CI failures.

## Updating this file

After a deliberate ranking change (or periodically), rerun both scales
locally, append a dated section with the same tables, and update the
claim-calibration list only when the measured numbers moved. CI
artifacts (`retrieval-results-10k.json`) carry the full JSON shape for
direct comparison.

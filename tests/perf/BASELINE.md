# Performance baseline — `vault-perf`

Recorded benchmark baselines for the search/storage layer, produced by the
harness in this directory (`tests/perf`). Used to track regressions and to
calibrate the CI latency gate (`.github/workflows/perf.yml`).

The harness is a standalone crate (own workspace) that depends on
`vault-core` by path and exercises **only the product's public API** —
`Storage::insert_message` for fixtures, `Storage::search_filtered` +
`search::build_match_query` for queries, `Storage::open`/`stats()` for
startup. No product code is modified by anything in this directory.

## How to run

```sh
cd tests/perf
cargo run --release -- bench --count 100000            # 100k, informational
cargo run --release -- bench --count 100000 --gate     # 100k, CI budgets enforced
cargo run --release -- bench --count 1000000           # 1M, informational (slow)
```

Useful flags: `--json PATH` (machine-readable results), `--data-dir DIR`
(fixture storage; default `$TMPDIR/vault-perf`), `--iterations/--warmup N`,
`--no-reuse` (force regeneration), `--gate-p95-ms X` (one uniform budget for
all queries instead of the per-query profile), `--gen-only`.

Fixtures are deterministic: the same `(--count, --seed)` regenerates the
same archive everywhere (splitmix64 PRNG, Zipf vocabulary, seeded author
pools), so numbers are comparable across runs on the same machine.

## What is measured

- **Fixture realism**: 45% email (`mbox`) / 30% WhatsApp / 25% Telegram mix;
  bodies from word-count buckets (chat mostly 1–10 words, email 20–2000 with
  a long tail); Zipf-distributed 4096-word vocabulary so term selectivity
  behaves like real text; 192 authors; `sent_at` spread over 2018–2026 with
  business-hours/evening and weekday/weekend skews.
- **Search latency**: the seven queries below against
  `Storage::search_filtered` (FTS5 MATCH + optional inclusive date range +
  `LIMIT`, i.e. exactly what `search_messages` in the Tauri app runs).
  `p50/p95/p99/max` over 60 timed iterations after 5 warmups. Page size is
  the app's default (50) except where noted.
- **Startup**: `Storage::open` (migrations + pragmas) and open + `stats()`,
  the app's launch data path, 10 runs. Warm OS file cache (fixtures were
  just generated or reused).
- **Sizes**: FTS5 index bytes via SQLite `dbstat` (`messages_fts%` shadow
  tables), plus database file and logical size. Measured after
  `wal_checkpoint(TRUNCATE)`.

## Benchmark queries

| label | shape | selectivity at 1M (measured) |
|---|---|---|
| `ubiquitous-term` | single stopword-like term, limit 50 | ~815k hits — worst case: FTS5 must rank every match |
| `frequent-term` | single frequent term, limit 50 | ~75k hits |
| `rare-term` | single rare term, limit 50 | ~3.7k hits |
| `two-term-and` | two terms (implicit AND), limit 50 | ~5.2k hits |
| `author-term` | author handle term, limit 50 | ~18k hits (author column is indexed) |
| `frequent+date-filter` | frequent term + 2022 date range | ~8.9k hits |
| `frequent-paged-200` | frequent term, limit 200 (app's max page) | ~75k hits |

## CI gate policy

The PR job runs `bench --count 100000 --gate` on `ubuntu-latest`. Gates use
**per-query p95 budgets** calibrated ≈10× above the measured local p95 below
(with floors for sub-millisecond queries). Rationale: a naive
`p95 < 100 ms` overall gate is flaky on shared CI runners — the
`ubiquitous-term` query alone sits at ~78 ms p95 on a fast desktop, and
2-vCPU runners are several times slower with much higher variance. The
budgets below catch order-of-magnitude regressions per query class while
leaving ~10× headroom for runner variance; tighten them only with fresh
measurements from the actual CI runners (check the `perf-results-100k`
artifacts of a few runs first).

| query | local p95 (100k) | CI budget |
|---|---:|---:|
| ubiquitous-term | 78 ms | 750 ms |
| frequent-term | 5.9 ms | 75 ms |
| rare-term | 0.5 ms | 15 ms |
| two-term-and | 1.2 ms | 20 ms |
| author-term | 1.8 ms | 25 ms |
| frequent+date-filter | 6.6 ms | 75 ms |
| frequent-paged-200 | 8.4 ms | 100 ms |

The 1M suite runs **nightly and on demand** (`workflow_dispatch`, suite
`1m`) without a gate; its JSON results are uploaded as artifacts for
baseline tracking. Generation of the 1M fixture takes ~20 min locally
(the product's own per-message insert path, WAL checkpointing included) —
too slow and too variance-prone to gate in CI.

## Baselines

Machine: AMD Ryzen 7 5700X (16 threads), Linux x86_64, rustc stable,
release profile (LTO). Seed 42, 5 warmup + 60 timed iterations. Compare
numbers only within the same machine class; use the CI artifacts for
runner-to-runner trends.

### 100k fixture — 2026-11-01 (seed 42)

Fixture: generated in 36.6 s (2735 msg/s); db 128.9 MiB, FTS index
23.5 MiB.

| query | hits | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|
| ubiquitous-term | 81529 | 75.5 | 78.0 | 79.4 | 79.4 |
| frequent-term | 7465 | 5.6 | 5.9 | 6.5 | 6.5 |
| rare-term | 365 | 0.4 | 0.5 | 0.7 | 0.7 |
| two-term-and | 549 | 0.9 | 1.2 | 1.5 | 1.5 |
| author-term | 1811 | 1.4 | 1.8 | 2.1 | 2.1 |
| frequent+date-filter | 890 | 5.8 | 6.6 | 8.8 | 8.8 |
| frequent-paged-200 | 7465 | 6.2 | 8.4 | 9.4 | 9.4 |

Startup: `open` p50 0.13 ms / p95 0.18 ms; `open+stats` p50 2.8 ms /
p95 5.5 ms.

### 1M fixture — 2026-11-01 (seed 42)

Fixture: generated in 1256.8 s (796 msg/s; insert cost grows with index
size — this is the product's real ingest rate at scale); db 1288.7 MiB,
FTS index 232.2 MiB (~18% of the db).

| query | hits | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|
| ubiquitous-term | 814623 | 803.4 | 909.8 | 2887.6 | 2887.6 |
| frequent-term | 74843 | 70.3 | 122.5 | 123.6 | 123.6 |
| rare-term | 3730 | 6.3 | 6.6 | 6.8 | 6.8 |
| two-term-and | 5190 | 11.3 | 13.3 | 13.7 | 13.7 |
| author-term | 18400 | 20.9 | 39.7 | 45.1 | 45.1 |
| frequent+date-filter | 8875 | 68.4 | 73.9 | 82.7 | 82.7 |
| frequent-paged-200 | 74843 | 68.5 | 71.0 | 72.5 | 72.5 |

Startup: `open` p50 0.16 ms / p95 0.18 ms; `open+stats` p50 49.1 ms /
p95 416.1 ms (the tail is `COUNT(*)` over 1M rows with cold-ish page cache).

### Observations worth acting on (product-side, out of scope here)

- `ubiquitous-term` (queries matching a large fraction of the archive) pays
  full ranking over every hit: ~0.8 s p50 at 1M. A relevance-preserving
  optimization (e.g. rank cutoffs or an `ORDER BY` variant) would need a
  product change and its own gate analysis.
- Ingest at 1M runs at ~800 msg/s and slows as the FTS index grows;
  batched ingest (single transaction per source pass) is the obvious
  product-side improvement.

## Updating this file

After a deliberate performance change (or periodically), rerun both suites
locally, append a dated section with the same tables, and update the CI
budget table only when the new steady state justifies it. CI artifacts
(`perf-results-100k.json`, `perf-results-1m.json`) carry the same JSON
shape as `--json` output for direct comparison.

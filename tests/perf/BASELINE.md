# Performance baseline — `vault-perf`

Recorded benchmark baselines for the search/storage layer, produced by the
harness in this directory (`tests/perf`). Used to track regressions and to
calibrate the CI latency gate (`.github/workflows/perf.yml`).

The harness is a standalone crate (own workspace) that depends on
`vault-core` by path and exercises **only the product's public API** —
`Storage::insert_message` for fixtures, `DbPool` +
`Storage::search_page` (the exact paginated query path the
`search_messages` IPC command runs) for queries, `Storage::open` /
`DbPool::spawn` for startup, and the M2.2 bounded-import path
(`BoundedRun` + `PoolSink`) for the concurrent-ingest measurement. No
product code is modified by anything in this directory.

Since M2.4 the timed query path is **`Storage::search_page`** — FTS5
MATCH + facet filters + `LIMIT`/`OFFSET` + the pagination `COUNT`, plus
FTS5 snippet generation — i.e. what the UI actually pays per keystroke
round trip, not the cheaper legacy `search_filtered` shape. The pre-M2.4
tables further below were measured through `search_filtered` and are kept
verbatim as historical evidence; do not compare them 1:1 against the
M2.4 tables (the paginated path does strictly more work per call — see
the observations in each section).

## How to run

```sh
cd tests/perf
cargo run --release -- bench --count 100000            # 100k, informational
cargo run --release -- bench --count 100000 --gate     # 100k, CI budgets enforced
cargo run --release -- bench --count 1000000           # 1M, informational (slow)
```

Useful flags: `--json PATH` (machine-readable results), `--data-dir DIR`
(fixture storage; default `$TMPDIR/vault-perf`), `--iterations/--warmup N`,
`--ingest-count N` (messages for the concurrent-ingest phase, default
50000; `0` skips it), `--no-reuse` (force regeneration), `--gate-p95-ms X`
(one uniform budget for all queries instead of the per-query profile),
`--gen-only`.

Fixtures are deterministic: the same `(--count, --seed)` regenerates the
same archive everywhere (splitmix64 PRNG, Zipf vocabulary, seeded author
pools), so numbers are comparable across runs on the same machine. The
concurrent-ingest phase commits its rows through the product's bounded
import and removes them again afterwards (untimed), so a reused fixture
stays exactly `--count` messages.

## What is measured (M2.4 harness)

- **Fixture realism**: 45% email (`mbox`) / 30% WhatsApp / 25% Telegram mix;
  bodies from word-count buckets (chat mostly 1–10 words, email 20–2000 with
  a long tail); Zipf-distributed 4096-word vocabulary so term selectivity
  behaves like real text; 192 authors; `sent_at` spread over 2018–2026 with
  business-hours/evening and weekday/weekend skews.
- **Paged search latency**: the ten queries below against
  `DbPool.with_read(Storage::search_page)` — the `search_messages` IPC
  data path: FTS5 MATCH (or newest-first browse for empty text), optional
  inclusive local-day date range, `LIMIT`/`OFFSET` pagination, the
  pagination `COUNT(*)`, and FTS5 snippet generation.
  `p50/p95/p99/max` over 60 timed iterations after 5 warmups. Page size is
  the app's default (50) except where noted.
- **IPC-shaped round trips**: the same paged search on a pool read
  connection plus the JSON serialization of the page — the payload that
  crosses the webview boundary. (The webview/OS hop itself is not
  measurable from a core-only harness and is deliberately not estimated.)
- **Search while ingesting**: a bounded import (default 50k chat-shaped
  messages, the M2.2 production path with batched commits through the
  pool's write lane) runs as a pool job while paged searches are timed on
  the read connections — the "query while ingesting" contract (WAL readers
  see committed batches, never wait behind a write). Reported next to the
  same queries' quiet numbers, plus the ingest rate.
- **Startup**: `Storage::open` (migrations + pragmas), open + `stats()`
  (the pre-pool data path), and `DbPool::spawn` + first paged search (the
  M2.1+ launch path: writer + two read connections + job runner), 10 runs
  each. Warm OS file cache (fixtures were just generated or reused).
- **Sizes**: FTS5 index bytes via SQLite `dbstat` (`archive_fts%` shadow
  tables), plus database file and logical size. Measured after
  `wal_checkpoint(TRUNCATE)`.

## Benchmark queries

| label | shape | selectivity at 1M (measured, pre-M2.4 harness) |
|---|---|---|
| `ubiquitous-term` | single stopword-like term, limit 50 | ~815k hits — worst case: FTS5 must rank every match |
| `frequent-term` | single frequent term, limit 50 | ~75k hits |
| `rare-term` | single rare term, limit 50 | ~3.7k hits |
| `two-term-and` | two terms (implicit AND), limit 50 | ~5.2k hits |
| `author-term` | author handle term, limit 50 | ~18k hits (author column is indexed) |
| `frequent+date-filter` | frequent term + 2022 date range | ~8.9k hits |
| `frequent-paged-200` | frequent term, limit 200 (app's max page) | ~75k hits |
| `frequent-deep-page` | frequent term, limit 50 **offset 5000** | deep FTS pagination: rank ≥5050 matches |
| `browse-newest` | empty text (newest-first browse), limit 50 | total archive (index-ordered scan) |
| `browse-deep-page` | empty text, limit 50 **offset 100000** | deep browse: walk the sent index 100k rows in |

## CI gate policy

The PR job runs `bench --count 100000 --gate` on `ubuntu-latest`. Since
the benchmark-page ticket the gate budgets live in the committed
[`baseline.json`](baseline.json) (per-query p95 + the methodology they
were calibrated under + its SHA-256); the harness fails loudly on a hash
mismatch, a missing query entry, or a run whose shape differs from the
file. The public page with methodology and results is
[docs/benchmark.md](../../docs/benchmark.md); the historical rationale is
kept below.

Gates use **per-query p95 budgets** calibrated ≈10× above the measured
local p95
below (with floors for sub-millisecond queries). Rationale: a naive
`p95 < 100 ms` overall gate is flaky on shared CI runners — the
`ubiquitous-term` query alone sits far above 100 ms locally, and 2-vCPU
runners are several times slower with much higher variance. The budgets
below catch order-of-magnitude regressions per query class while leaving
~10× headroom for runner variance; tighten them only with fresh
measurements from the actual CI runners (check the `perf-results-100k`
artifacts of a few runs first). The two deep-page labels are new in M2.4
and not yet measured on CI runners — they carry 12× headroom until the
first artifacts land.

| query | local p95 (100k, M2.4) | CI budget |
|---|---:|---:|
| ubiquitous-term | 155.5 ms | 1500 ms |
| frequent-term | 26.8 ms | 300 ms |
| rare-term | 3.3 ms | 40 ms |
| two-term-and | 7.0 ms | 75 ms |
| author-term | 7.7 ms | 80 ms |
| frequent+date-filter | 31.3 ms | 300 ms |
| frequent-paged-200 | 36.7 ms | 375 ms |
| frequent-deep-page | 255.2 ms | 3000 ms |
| browse-newest | 6.0 ms | 60 ms |
| browse-deep-page | 156.4 ms | 1800 ms |

The 1M suite runs **nightly and on demand** (`workflow_dispatch`, suite
`1m`) without a gate; its JSON results are uploaded as artifacts for
baseline tracking. Generation of the 1M fixture takes ~20–50 min locally
(the product's own per-message insert path, WAL checkpointing included) —
too slow and too variance-prone to gate in CI.

## Baselines — M2.4 harness (paginated search, IPC-shaped, concurrent ingest)

Machine: AMD Ryzen 7 5700X (16 threads), 32 GiB RAM, Linux x86_64,
rustc stable, release profile (LTO). Seed 42, 5 warmup + 60 timed
iterations (30 at 1M). Compare numbers only within the same machine
class; use the CI artifacts for runner-to-runner trends.

### 100k fixture — 2026-11-03 (seed 42, M2.4 harness)

Fixture: generated in 93.6 s (1069 msg/s — the host was busier than for
the pre-M2.4 run below; generation speed is informational, not a
contract); db 186.8 MiB, FTS index 23.6 MiB.

Paged search, `DbPool.with_read(Storage::search_page)`:

| query | hits | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|
| ubiquitous-term | 81529 | 150.6 | 155.5 | 157.0 | 157.0 |
| frequent-term | 7465 | 25.6 | 26.8 | 28.6 | 28.6 |
| rare-term | 365 | 3.3 | 3.3 | 3.3 | 3.3 |
| two-term-and | 549 | 6.9 | 7.0 | 7.1 | 7.1 |
| author-term | 1811 | 7.3 | 7.7 | 7.8 | 7.8 |
| frequent+date-filter | 890 | 30.5 | 31.3 | 31.5 | 31.5 |
| frequent-paged-200 | 7465 | 33.3 | 36.7 | 37.8 | 37.8 |
| frequent-deep-page | 7465 | 242.0 | 255.2 | 290.8 | 290.8 |
| browse-newest | 100000 | 5.9 | 6.0 | 6.0 | 6.0 |
| browse-deep-page | 100000 | 153.2 | 156.4 | 158.5 | 158.5 |

IPC-shaped round trips (pool read hop + JSON page serialization):

| query | hits | median page bytes | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| frequent-term | 7465 | 16,779 | 25.8 | 27.6 | 27.9 |
| rare-term | 365 | 28,827 | 3.4 | 3.4 | 3.6 |

Serialization adds ~0.2–0.3 ms over the bare query — the payload size,
not the hop, would be the lever if this ever grows.

Search while ingesting (50k chat-shaped messages committed through the
write lane at 4,048 msg/s; 360 samples per query):

| query | quiet p95 | during p50 | during p95 | during p99 |
|---|---:|---:|---:|---:|
| frequent-term | 26.8 | 30.0 | 32.4 | 33.6 |
| rare-term | 3.3 | 4.2 | 4.6 | 5.3 |

Readers never waited behind a write (WAL): the degradation is modest
(+21 % p95 on the frequent term, +39 % on the rare term) and comes from
commit churn and cache pressure, not locking.

Startup: `open` p50 0.4 ms / p95 0.6 ms; `open+stats` p50 0.5 ms /
p95 0.7 ms; `DbPool::spawn` + first paged search p50 2.9 ms /
p95 4.2 ms.

### 1M fixture — 2026-11-03 (seed 42, M2.4 harness)

Fixture: generated in 978.7 s (1022 msg/s); db 1865.1 MiB, FTS index
233.1 MiB. (Same seed and generator as the pre-M2.4 1M fixture below,
but ~45 % larger on disk: the per-message insert path changed with the
M2.2/M2.3 work landed after that baseline was recorded. The FTS index
size is unchanged — the growth is in the row-store, and it is the
current tree's truth. The concurrent batched-import rate below is the
M2.2 path, ~6× the old per-message insert rate.)

Paged search, `DbPool.with_read(Storage::search_page)`:

| query | hits | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|---:|
| ubiquitous-term | 814623 | 1354.2 | 1366.8 | 1367.8 | 1367.8 |
| frequent-term | 74843 | 254.9 | 258.0 | 261.7 | 261.7 |
| rare-term | 3730 | 19.2 | 23.2 | 23.4 | 23.4 |
| two-term-and | 5190 | 33.7 | 36.3 | 37.9 | 37.9 |
| author-term | 18400 | 70.7 | 74.1 | 75.7 | 75.7 |
| frequent+date-filter | 8873 | 301.8 | 306.1 | 306.4 | 306.4 |
| frequent-paged-200 | 74843 | 262.7 | 274.2 | 274.6 | 274.6 |
| frequent-deep-page | 74843 | 596.7 | 602.4 | 603.0 | 603.0 |
| browse-newest | 1000000 | 58.7 | 60.0 | 60.8 | 60.8 |
| browse-deep-page | 1000000 | 251.3 | 257.4 | 260.3 | 260.3 |

IPC-shaped round trips:

| query | hits | median page bytes | p50 | p95 | p99 |
|---|---:|---:|---:|---:|---:|
| frequent-term | 74843 | 26,340 | 257.1 | 264.3 | 265.8 |
| rare-term | 3730 | 15,212 | 19.8 | 21.9 | 22.7 |

Search while ingesting (50k messages at 4,815 msg/s; 34 samples per
query):

| query | quiet p95 | during p50 | during p95 | during p99 |
|---|---:|---:|---:|---:|
| frequent-term | 258.0 | 282.9 | 288.6 | 290.7 |
| rare-term | 23.2 | 22.9 | 23.7 | 23.8 |

Startup (fixture just used, warm page cache): `open` p50 0.49 ms /
p95 0.61 ms; `open+stats` p50 0.5 ms / p95 0.6 ms; `DbPool::spawn` +
first paged search p50 20.6 ms / p95 172.4 ms — the tail is the first
query's `COUNT(*)` over 1M rows on the cold side of the cache, the same
shape the pre-M2.4 `open+stats` tail shows below.

#### Observations worth acting on (M2.4)

- The paginated path is 3–4.5× the legacy `search_filtered` cost on
  hit-heavy queries (frequent-term 5.9 → 26.8 ms at 100k): the
  pagination `COUNT(*)` re-scans every match and snippet generation
  adds ~0.2 ms per hit rendered. Both are what the UI actually pays
  today; a deferred-count or snippet-on-demand variant is the obvious
  product-side lever if this ever needs to shrink.
- Deep FTS pagination is linear in offset (frequent-deep-page 255 ms at
  100k / 602 ms at 1M for offset 5000) and deep browse walks the sent
  index to the offset (browse-deep-page 156 ms at 100k for offset
  100k, 257 ms at 1M). Keyset pagination (`WHERE ... > last_seen ORDER
  BY ...`) would flatten both, at the cost of stable absolute page
  numbers — a product decision, out of scope for the harness.
- `ubiquitous-term` pays full ranking over every hit twice (rows +
  count): ~1.35 s p50 at 1M. Same observation as pre-M2.4, now doubled
  by the count leg.

## Baselines — pre-M2.4 harness (historical, `search_filtered`)

The tables below were recorded through the original harness, whose timed
query path was `Storage::search_filtered` (FTS5 MATCH + date filters +
`LIMIT`; no pagination count, no snippets). They are preserved verbatim
as historical evidence of the v1 contract; the M2.4 tables above are the
current truth and the two are not directly comparable.

Machine: AMD Ryzen 7 5700X (16 threads), Linux x86_64, rustc stable,
release profile (LTO). Seed 42, 5 warmup + 60 timed iterations.

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

### Observations worth acting on (pre-M2.4, product-side, out of scope here)

- `ubiquitous-term` (queries matching a large fraction of the archive) pays
  full ranking over every hit: ~0.8 s p50 at 1M. A relevance-preserving
  optimization (e.g. rank cutoffs or an `ORDER BY` variant) would need
  product change and its own gate analysis.
- Ingest at 1M runs at ~800 msg/s and slows as the FTS index grows;
  batched ingest (single transaction per source pass) is the obvious
  product-side improvement (landed since as the M2.2 bounded-import
  path, measured above at 3.7–4.0k msg/s).

## Updating this file

After a deliberate performance change (or periodically), rerun both suites
locally, append a dated section with the same tables, and update the CI
budget table only when the new steady state justifies it. CI artifacts
(`perf-results-100k.json`, `perf-results-1m.json`) carry the same JSON
shape as `--json` output for direct comparison.

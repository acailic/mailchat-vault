# Benchmark: 1M-message index time and search latency

Mailchat Vault publishes measured performance for the search/storage core:
how long a large archive takes to index, and how fast queries come back.
Every number on this page was **measured on a real run** of the
`vault-perf` harness — nothing here is estimated or extrapolated. Where a
measurement does not exist yet, the row says **pending owner run**.

> **Measured on one machine; your numbers will differ.** The tables below
> were recorded on an 8-core/16-thread AMD Ryzen 7 5700X desktop (32 GiB
> RAM, NVMe SSD, Linux x86_64, rustc stable, release profile with LTO) on
> 2026-11-03. Absolute times depend on the machine, the OS page cache,
> and the neighbor workload; compare against your own run of the same
> harness, and compare this page's tables only within the same machine
> class. CI runs on shared 2-vCPU GitHub runners measure the same
> queries with ~10x calibrated headroom (see the [CI gate](#ci-regression-gate)).

## What is measured

The harness ([`tests/perf`](../tests/perf)) drives **only the product's
public API** — no benchmark-specific SQL, no shortcuts:

- **Cold index time** — building the archive through the product's own
  ingest path (`Storage::insert_message`, FTS5 triggers firing, WAL
  checkpoint included). This is the real cost of importing a mailbox, not
  a bulk-load special case.
- **Search latency** — the exact paginated query path the app's
  `search_messages` IPC command runs (`Storage::search_page` via the pool
  read lane): FTS5 MATCH + filters + `LIMIT`/`OFFSET` + the pagination
  `COUNT(*)` + snippet generation.
- **Startup, IPC-shaped round trips, query-while-ingesting** — recorded
  in [`tests/perf/BASELINE.md`](../tests/perf/BASELINE.md); this page
  keeps the headline tables.

### Fixture

The archive is synthetic but realistic, and **deterministic**: the same
`--seed` produces byte-identical fixtures on every machine (splitmix64
PRNG). At the recorded runs: seed 42, 45% email (`mbox`) / 30% WhatsApp /
25% Telegram, bodies drawn from a Zipf-distributed 4096-word vocabulary
(so term selectivity behaves like real text: a stopword-like term hits
~81% of messages, a frequent term ~7.5%, a rare term ~0.4%), 192
authors, `sent_at` spread over 2018–2026 with business-hours/evening and
weekday/weekend skews. The generator is a tool
(`vault-perf bench --gen-only`), **never committed data**.

### Protocol

- **Warmup rule**: 5 untimed warmup runs per query, then 60 timed
  iterations (30 at 1M) on a warm OS file cache. Startup numbers use 10
  runs on a fixture that was just written or reused (warm cache).
- **Percentiles**: nearest-rank — `p50`/`p95`/`p99` are the
  ceil(p/100·n)-th smallest samples, in milliseconds.
- **Query classes** (the ticket's three):

| class | protocol | status |
|---|---|---|
| (a) Prefix FTS query | free-text term(s) as FTS5 MATCH through the paginated path; the ten-query suite below spans stopword-like to rare terms, deep pages, and large pages | **measured** (tables below) |
| (b) Date+source filtered query | text query + inclusive local-day date range (+ source facet in the product query; the recorded suite covers the date leg) | **date leg measured** (`frequent+date-filter` row); a combined date+source row is **pending owner run** |
| (c) Full Ask pipeline, local provider OFF (retrieval only) | the Ask retrieval leg (hybrid search + context assembly) with the local embedding provider disabled — retrieval only, no LLM call | **pending owner run** — protocol defined, no number recorded yet |

## Cold index time

Measured on the reference machine class above (per-message product
ingest path, WAL checkpoint included):

| fixture | messages | index time | rate | database | FTS index |
|---|---:|---:|---:|---:|---:|
| 100k (2026-11-03) | 100,000 | 93.6 s | 1,069 msg/s | 186.8 MiB | 23.6 MiB |
| 1M (2026-11-03) | 1,000,000 | 978.7 s | 1,022 msg/s | 1,865.1 MiB | 233.1 MiB |

(The batched bounded-import path — the production importer since M2.2 —
sustains ~4,000–4,800 msg/s at these sizes; the numbers above are the
per-message path the harness uses to build fixtures. See
[`tests/perf/BASELINE.md`](../tests/perf/BASELINE.md) for the
concurrent-ingest tables.)

## Search latency — 100k messages (2026-11-03, seed 42)

`Storage::search_page`, 5 warmup + 60 timed iterations, page size 50
except where noted:

| query | hits | p50 (ms) | p95 (ms) | p99 (ms) | max (ms) |
|---|---:|---:|---:|---:|---:|
| ubiquitous-term | 81,529 | 150.6 | 155.5 | 157.0 | 157.0 |
| frequent-term | 7,465 | 25.6 | 26.8 | 28.6 | 28.6 |
| rare-term | 365 | 3.3 | 3.3 | 3.3 | 3.3 |
| two-term-and | 549 | 6.9 | 7.0 | 7.1 | 7.1 |
| author-term | 1,811 | 7.3 | 7.7 | 7.8 | 7.8 |
| frequent+date-filter | 890 | 30.5 | 31.3 | 31.5 | 31.5 |
| frequent-paged-200 | 7,465 | 33.3 | 36.7 | 37.8 | 37.8 |
| frequent-deep-page (offset 5000) | 7,465 | 242.0 | 255.2 | 290.8 | 290.8 |
| browse-newest | 100,000 | 5.9 | 6.0 | 6.0 | 6.0 |
| browse-deep-page (offset 100000) | 100,000 | 153.2 | 156.4 | 158.5 | 158.5 |

Startup (warm cache): `Storage::open` p50 0.4 ms / p95 0.6 ms;
`open`+`stats()` p50 0.5 ms / p95 0.7 ms; `DbPool::spawn` + first paged
search p50 2.9 ms / p95 4.2 ms.

## Search latency — 1M messages (2026-11-03, seed 42)

Recorded run on the same machine class, 5 warmup + 30 timed iterations:

| query | hits | p50 (ms) | p95 (ms) | p99 (ms) | max (ms) |
|---|---:|---:|---:|---:|---:|
| ubiquitous-term | 814,623 | 1,354.2 | 1,366.8 | 1,367.8 | 1,367.8 |
| frequent-term | 74,843 | 254.9 | 258.0 | 261.7 | 261.7 |
| rare-term | 3,730 | 19.2 | 23.2 | 23.4 | 23.4 |
| two-term-and | 5,190 | 33.7 | 36.3 | 37.9 | 37.9 |
| author-term | 18,400 | 70.7 | 74.1 | 75.7 | 75.7 |
| frequent+date-filter | 8,873 | 301.8 | 306.1 | 306.4 | 306.4 |
| frequent-paged-200 | 74,843 | 262.7 | 274.2 | 274.6 | 274.6 |
| frequent-deep-page (offset 5000) | 74,843 | 596.7 | 602.4 | 603.0 | 603.0 |
| browse-newest | 1,000,000 | 58.7 | 60.0 | 60.8 | 60.8 |
| browse-deep-page (offset 100000) | 1,000,000 | 251.3 | 257.4 | 260.3 | 260.3 |

Ask pipeline (retrieval only, local provider OFF): **pending owner run**
on the reference machine — the protocol is defined above; no number is
published until it is actually measured.

Reading the tables: the expensive rows are the ones FTS5 must rank over
every match (`ubiquitous-term` pays full ranking twice: rows + count) and
deep `OFFSET` pages, which walk linearly. Keyset pagination
(`search_page` accepts a cursor) flattens deep pages to first-page cost;
see [`tests/perf/BASELINE.md`](../tests/perf/BASELINE.md) for the full
analysis, IPC-shaped round trips, and query-while-ingesting numbers.

## CI regression gate

Every PR runs the harness at 100k with `--gate`: each query's **p95 must
stay under its budget** or the job fails. Budgets are **not hardcoded** —
they live in the committed
[`tests/perf/baseline.json`](../tests/perf/baseline.json) together with
the methodology they were calibrated under (fixture size, seed, warmup
rule, machine class) and a SHA-256 of that methodology. A changed
methodology without recalibrated budgets, a missing query entry, or a
run whose shape differs from the file (wrong `--count`, `--warmup`, …)
fails the gate loudly before any regression can hide.

Budgets are calibrated ≈10x above the measured 100k p95 above (≈12x for
the two deep-page labels, which are not yet measured on CI runners), so
shared-runner variance cannot flake the gate while order-of-magnitude
regressions still trip it. The nightly 1M run is informational (no gate)
with results uploaded as artifacts.

## Refreshing this page

After a deliberate performance change (or periodically), on the
reference machine class:

```sh
cd tests/perf
cargo run --release -- bench --count 100000   # and --count 1000000
```

1. Replace the dated tables above with the new run's numbers (keep the
   date and machine class honest — never mix runs).
2. Recalibrate `baseline.json` budgets if the steady state changed
   (recompute its `methodology_sha256`; the gate refuses a stale file).
3. `cargo test --test bench_protocol` must stay green — it loads the
   committed baseline and checks it covers the suite exactly.
4. Pending rows may only be filled by actually running them; never
   estimate.

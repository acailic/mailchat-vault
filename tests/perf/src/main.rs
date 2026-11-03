//! vault-perf: benchmark harness for mailchat-vault.
//!
//! Measures, against a deterministic synthetic archive:
//! - search latency through **`Storage::search_page`** — the actual
//!   paginated query path the `search_messages` IPC command runs (FTS5
//!   MATCH + filters + `LIMIT`/`OFFSET` + the pagination total), p50/p95/p99;
//! - IPC-shaped round trips: the same paged search on a pool read
//!   connection plus the JSON serialization of the page (the payload
//!   that crosses the webview boundary);
//! - search latency **while a bounded ingest commits through the pool's
//!   write lane** (query while ingesting), plus the ingest rate itself;
//! - app startup cost on a large database (`Storage::open` + `stats()`,
//!   and the M2.1 `DbPool::spawn` + first search launch path);
//! - FTS index and database size (via SQLite `dbstat`).
//!
//! Everything exercises the product's own `vault-core` API — no SQL of
//! the app is duplicated for timing.
//!
//! Run `vault-perf bench --help` for usage. See tests/perf/BASELINE.md for
//! recorded baselines and the CI gate policy.

mod fixture;
mod measure;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use rusqlite::Connection;
use vault_core::bounded::BoundedRun;
use vault_core::jobs::{DbPool, JobError, PoolConfig, PoolSink};
use vault_core::query::SearchQuery;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

use fixture::{
    word_at, IngestSource, AUTHOR_HANDLE_RANK, BENCH_INGEST_SOURCE, DATE_FILTER_FROM,
    DATE_FILTER_TO, FREQUENT_RANK, RARE_RANK, UBIQUITOUS_RANK, UNCOMMON_RANK,
};
use measure::{
    BenchConfig, ConcurrentIngestRow, EnvInfo, FixtureInfo, GateResult, IngestSearchRow, IpcRow,
    LatencyStats, Report, SearchRow, StartupRow,
};

const USAGE: &str = "\
vault-perf bench — mailchat-vault performance harness

Usage: vault-perf bench [OPTIONS]
  --count N          messages in the fixture (default 100000)
  --seed N           fixture seed (default 42)
  --data-dir DIR     fixture directory (default: system temp /vault-perf)
  --iterations N     timed search iterations per query (default 60)
  --warmup N         untimed warmup searches per query (default 5)
  --startup-runs N   storage startup measurements (default 10)
  --ingest-count N   messages committed by the concurrent-ingest phase
                     (default 50000; 0 skips the phase)
  --gate             fail (exit 1) when any query's p95 >= its budget
                     (per-query budgets calibrated for CI; see BASELINE.md)
  --gate-p95-ms X    like --gate but one uniform budget (ms) for all queries
  --json PATH        write the full results as JSON to PATH
  --gen-only         generate the fixture and exit (skip benchmarks)
  --no-reuse         regenerate even when a matching fixture exists
";

struct Args {
    count: u64,
    seed: u64,
    data_dir: Option<PathBuf>,
    iterations: u32,
    warmup: u32,
    startup_runs: u32,
    ingest_count: u64,
    gate: bool,
    gate_p95_ms: Option<f64>,
    json: Option<PathBuf>,
    gen_only: bool,
    no_reuse: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        count: 100_000,
        seed: 42,
        data_dir: None,
        iterations: 60,
        warmup: 5,
        startup_runs: 10,
        ingest_count: 50_000,
        gate: false,
        gate_p95_ms: None,
        json: None,
        gen_only: false,
        no_reuse: false,
    };
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // A single `bench` subcommand word is accepted (and ignored) so the
    // usage line and muscle memory (`vault-perf bench --count ...`) work.
    let raw: &[String] = raw.strip_prefix(&["bench".to_string()][..]).unwrap_or(&raw);
    let mut it = raw.iter().cloned();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} requires a value"));
        match flag.as_str() {
            "--count" => {
                args.count = value("--count")?
                    .parse()
                    .map_err(|e| format!("--count: {e}"))?
            }
            "--seed" => {
                args.seed = value("--seed")?
                    .parse()
                    .map_err(|e| format!("--seed: {e}"))?
            }
            "--data-dir" => args.data_dir = Some(PathBuf::from(value("--data-dir")?)),
            "--iterations" => {
                args.iterations = value("--iterations")?
                    .parse()
                    .map_err(|e| format!("--iterations: {e}"))?
            }
            "--warmup" => {
                args.warmup = value("--warmup")?
                    .parse()
                    .map_err(|e| format!("--warmup: {e}"))?
            }
            "--startup-runs" => {
                args.startup_runs = value("--startup-runs")?
                    .parse()
                    .map_err(|e| format!("--startup-runs: {e}"))?
            }
            "--ingest-count" => {
                args.ingest_count = value("--ingest-count")?
                    .parse()
                    .map_err(|e| format!("--ingest-count: {e}"))?
            }
            "--gate" => args.gate = true,
            "--gate-p95-ms" => {
                args.gate_p95_ms = Some(
                    value("--gate-p95-ms")?
                        .parse()
                        .map_err(|e| format!("--gate-p95-ms: {e}"))?,
                );
            }
            "--json" => args.json = Some(PathBuf::from(value("--json")?)),
            "--gen-only" => args.gen_only = true,
            "--no-reuse" => args.no_reuse = true,
            "--help" | "-h" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if args.count == 0 {
        return Err("--count must be positive".into());
    }
    if args.iterations == 0 || args.warmup < 1 || args.startup_runs < 1 {
        return Err("--iterations/--warmup/--startup-runs must be >= 1 (and warmup >= 1)".into());
    }
    Ok(args)
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("vault-perf: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

fn run() -> Result<i32, String> {
    let args = parse_args()?;
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("vault-perf"));
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| format!("cannot create data dir {}: {e}", data_dir.display()))?;
    let db_path = data_dir.join(format!("fixture-{}-seed{}.db", args.count, args.seed));

    // --- fixture: reuse a matching one, else (re)generate -----------------
    let (fixture_info, generated) = ensure_fixture(&db_path, &args)?;

    if args.gen_only {
        println!(
            "fixture ready: {} ({} messages{} in {:.1}s)",
            db_path.display(),
            args.count,
            if generated { ", generated" } else { ", reused" },
            fixture_info.generation_secs,
        );
        return Ok(0);
    }

    // --- startup: the app's launch data paths ------------------------------
    eprintln!("measuring startup ({} runs)...", args.startup_runs);
    let startup = measure_startup(&db_path, args.startup_runs);

    // --- search latency through the actual paginated path ------------------
    eprintln!(
        "measuring paged search ({} warmup + {} timed runs per query)...",
        args.warmup, args.iterations
    );
    let pool = DbPool::spawn(&db_path, PoolConfig::default(), |_| {})
        .map_err(|e| format!("spawn pool over fixture: {e}"))?;
    let specs = query_suite();
    let gating = args.gate || args.gate_p95_ms.is_some();
    let mut search_rows = Vec::with_capacity(specs.len());
    for spec in &specs {
        let budget = match args.gate_p95_ms {
            Some(uniform) => Some(uniform),
            None if args.gate => Some(spec.p95_budget_ms),
            None => None,
        };
        let mut samples = Vec::with_capacity(args.iterations as usize);
        let mut hits = 0u64;
        for _ in 0..args.warmup {
            run_search(&pool, spec)?;
        }
        for _ in 0..args.iterations {
            let t = Instant::now();
            let total = run_search(&pool, spec)?;
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
            hits = total;
        }
        let stats = LatencyStats::of(samples);
        eprintln!(
            "  {}: p50 {:.1}ms p95 {:.1}ms p99 {:.1}ms ({} hits{}.)",
            spec.label,
            stats.p50_ms,
            stats.p95_ms,
            stats.p99_ms,
            hits,
            match budget {
                Some(b) => format!(
                    ", budget {:.0}ms → {}",
                    b,
                    if stats.p95_ms < b { "ok" } else { "OVER" }
                ),
                None => String::new(),
            }
        );
        search_rows.push(SearchRow {
            label: spec.label,
            query: spec.text.clone(),
            match_expr: if spec.text.is_empty() {
                String::new()
            } else {
                build_match_query(&spec.text)
            },
            date_from: spec.date_from.map(str::to_string),
            date_to: spec.date_to.map(str::to_string),
            limit: spec.limit,
            offset: spec.offset,
            hits,
            p95_budget_ms: budget,
            within_budget: budget.map(|b| stats.p95_ms < b),
            stats,
        });
    }

    // --- IPC-shaped round trips ---------------------------------------------
    eprintln!("measuring IPC-shaped round trips (pool read + JSON page)...");
    let ipc_specs: Vec<&QuerySpec> = specs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i == 1 || *i == 2) // frequent-term, rare-term
        .map(|(_, s)| s)
        .collect();
    let mut ipc_rows = Vec::with_capacity(ipc_specs.len());
    for spec in &ipc_specs {
        for _ in 0..args.warmup {
            run_ipc(&pool, spec)?;
        }
        let mut samples = Vec::with_capacity(args.iterations as usize);
        let mut sizes = Vec::with_capacity(args.iterations as usize);
        let mut hits = 0u64;
        for _ in 0..args.iterations {
            let t = Instant::now();
            let (total, bytes) = run_ipc(&pool, spec)?;
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
            sizes.push(bytes);
            hits = total;
        }
        sizes.sort_unstable();
        let median_payload = sizes[sizes.len() / 2];
        let stats = LatencyStats::of(samples);
        eprintln!(
            "  ipc/{}: p50 {:.1}ms p95 {:.1}ms p99 {:.1}ms (page {} bytes)",
            spec.label, stats.p50_ms, stats.p95_ms, stats.p99_ms, median_payload
        );
        ipc_rows.push(IpcRow {
            label: spec.label,
            hits,
            median_payload_bytes: median_payload,
            stats,
        });
    }

    // --- sizes -------------------------------------------------------------
    let (db_bytes, fts_index_bytes, logical_bytes) = db_sizes(&db_path)?;

    // --- concurrent ingest ---------------------------------------------------
    let concurrent_ingest = if args.ingest_count > 0 {
        eprintln!(
            "measuring search while a bounded ingest commits ({} messages)...",
            args.ingest_count
        );
        let row =
            measure_concurrent_ingest(&pool, &specs, &search_rows, args.ingest_count, args.seed)?;
        eprintln!(
            "  ingest: {} messages in {:.1}s ({:.0} msg/s); search p95 during ingest: {}",
            row.ingest_messages,
            row.ingest_secs,
            row.ingest_msgs_per_sec,
            row.search
                .iter()
                .map(|r| format!("{} {:.1}ms", r.label, r.during.p95_ms))
                .collect::<Vec<_>>()
                .join(", ")
        );
        cleanup_ingest_rows(&db_path)?;
        Some(row)
    } else {
        None
    };
    pool.shutdown();

    // --- gate + report -----------------------------------------------------
    let gate = if gating {
        let worst = search_rows
            .iter()
            .filter(|r| r.p95_budget_ms.is_some())
            .max_by(|a, b| {
                let ra = a.stats.p95_ms / a.p95_budget_ms.unwrap_or(f64::MAX);
                let rb = b.stats.p95_ms / b.p95_budget_ms.unwrap_or(f64::MAX);
                ra.total_cmp(&rb)
            })
            .expect("gating requires budgeted rows");
        Some(GateResult {
            budgets: if args.gate_p95_ms.is_some() {
                "uniform"
            } else {
                "per-query"
            },
            uniform_budget_ms: args.gate_p95_ms,
            worst_label: worst.label.to_string(),
            worst_p95_ms: worst.stats.p95_ms,
            worst_ratio: worst.stats.p95_ms / worst.p95_budget_ms.unwrap_or(f64::MAX),
            passed: search_rows.iter().all(|r| r.within_budget.unwrap_or(true)),
        })
    } else {
        None
    };

    let report = Report {
        harness: concat!("vault-perf ", env!("CARGO_PKG_VERSION")),
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        config: BenchConfig {
            count: args.count,
            seed: args.seed,
            iterations: args.iterations,
            warmup: args.warmup,
            startup_runs: args.startup_runs,
            ingest_count: args.ingest_count,
        },
        environment: EnvInfo::detect(),
        fixture: FixtureInfo {
            db_path: db_path.display().to_string(),
            generated,
            generation_secs: fixture_info.generation_secs,
            rows_per_sec: fixture_info.rows_per_sec,
            db_bytes,
            fts_index_bytes,
            logical_bytes,
        },
        startup,
        search: search_rows,
        ipc: ipc_rows,
        concurrent_ingest,
        gate: gate.clone(),
    };

    print_summary(&report);
    if let Some(path) = &args.json {
        let json = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| format!("write JSON to {}: {e}", path.display()))?;
        eprintln!("results written to {}", path.display());
    }

    match &gate {
        Some(g) if g.passed => Ok(0),
        Some(_) => Ok(1),
        None => Ok(0),
    }
}

// --- fixture management -----------------------------------------------------

struct Generated {
    generation_secs: f64,
    rows_per_sec: f64,
}

fn ensure_fixture(db_path: &Path, args: &Args) -> Result<(Generated, bool), String> {
    if !args.no_reuse && db_path.is_file() {
        if let Ok(storage) = Storage::open(db_path) {
            if let Ok(stats) = storage.stats() {
                if stats.total == args.count {
                    eprintln!(
                        "reusing fixture {} ({} messages)",
                        db_path.display(),
                        args.count
                    );
                    return Ok((
                        Generated {
                            generation_secs: 0.0,
                            rows_per_sec: 0.0,
                        },
                        false,
                    ));
                }
                eprintln!(
                    "existing fixture holds {} messages (want {}); regenerating",
                    stats.total, args.count
                );
            }
        }
    }
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{}", db_path.display(), suffix));
        let _ = std::fs::remove_file(p);
    }
    eprintln!(
        "generating fixture: {} messages, seed {}...",
        args.count, args.seed
    );
    let storage = Storage::open(db_path).map_err(|e| format!("create fixture db: {e}"))?;
    let report = fixture::generate(&storage, args.count, args.seed, &|done| {
        eprintln!("  {}/{} messages", done, args.count);
    })
    .map_err(|e| format!("generate fixture: {e}"))?;
    if report.inserted != report.requested {
        return Err(format!(
            "fixture generation inserted {}/{} messages",
            report.inserted, report.requested
        ));
    }
    drop(storage);
    // Checkpoint so the WAL is folded back into the main file: the on-disk
    // fixture is self-contained and size/startup numbers are stable.
    let conn = Connection::open(db_path).map_err(|e| format!("checkpoint open: {e}"))?;
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .map_err(|e| format!("checkpoint: {e}"))?;
    let gen = Generated {
        generation_secs: report.secs,
        rows_per_sec: report.inserted as f64 / report.secs.max(1e-9),
    };
    eprintln!(
        "generated in {:.1}s ({:.0} msg/s) via the product's idempotent insert path",
        gen.generation_secs, gen.rows_per_sec
    );
    Ok((gen, true))
}

// --- startup ------------------------------------------------------------------

fn measure_startup(db_path: &Path, runs: u32) -> StartupRow {
    let mut open_ms = Vec::with_capacity(runs as usize);
    let mut open_stats_ms = Vec::with_capacity(runs as usize);
    let mut pool_ms = Vec::with_capacity(runs as usize);
    let probe = SearchQuery {
        text: build_match_query(&word_at(RARE_RANK)),
        limit: 10,
        ..Default::default()
    };
    for _ in 0..runs {
        let t = Instant::now();
        let storage = Storage::open(db_path).expect("open storage for startup timing");
        let open_elapsed = t.elapsed().as_secs_f64() * 1000.0;
        let t2 = Instant::now();
        let stats = storage.stats().expect("stats for startup timing");
        let stats_elapsed = t2.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(stats.total);
        open_ms.push(open_elapsed);
        open_stats_ms.push(open_elapsed + stats_elapsed);
        drop(storage);

        // The M2.1+ launch path: the pool (writer + two read connections
        // + job runner) and the first paged search a user's window run.
        let t3 = Instant::now();
        let pool = DbPool::spawn(db_path, PoolConfig::default(), |_| {})
            .expect("spawn pool for startup timing");
        let page = pool.with_read(|s| s.search_page(&probe).expect("probe search"));
        let first_query = t3.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(page.total);
        pool.shutdown();
        pool_ms.push(first_query);
    }
    StartupRow {
        runs,
        open: LatencyStats::of(open_ms),
        open_stats: LatencyStats::of(open_stats_ms),
        pool_spawn: LatencyStats::of(pool_ms),
    }
}

// --- search --------------------------------------------------------------------

/// One benchmark query: the free text exactly as typed into the app (empty
/// text = newest-first browse), optional inclusive date range, page size,
/// page offset, and its CI p95 budget.
///
/// Budgets are calibrated ≈10× the measured local p95 (with floors for
/// sub-millisecond queries) so shared-CI-runner variance cannot flake the
/// gate while order-of-magnitude regressions still trip it. Local p95s and
/// the calibration rationale live in tests/perf/BASELINE.md.
struct QuerySpec {
    label: &'static str,
    text: String,
    date_from: Option<&'static str>,
    date_to: Option<&'static str>,
    limit: u32,
    offset: u32,
    p95_budget_ms: f64,
}

fn query_suite() -> Vec<QuerySpec> {
    vec![
        QuerySpec {
            label: "ubiquitous-term",
            text: word_at(UBIQUITOUS_RANK),
            date_from: None,
            date_to: None,
            limit: 50,
            offset: 0,
            p95_budget_ms: 1500.0,
        },
        QuerySpec {
            label: "frequent-term",
            text: word_at(FREQUENT_RANK),
            date_from: None,
            date_to: None,
            limit: 50,
            offset: 0,
            p95_budget_ms: 300.0,
        },
        QuerySpec {
            label: "rare-term",
            text: word_at(RARE_RANK),
            date_from: None,
            date_to: None,
            limit: 50,
            offset: 0,
            p95_budget_ms: 40.0,
        },
        QuerySpec {
            label: "two-term-and",
            text: format!("{} {}", word_at(FREQUENT_RANK), word_at(UNCOMMON_RANK)),
            date_from: None,
            date_to: None,
            limit: 50,
            offset: 0,
            p95_budget_ms: 75.0,
        },
        QuerySpec {
            label: "author-term",
            text: word_at(AUTHOR_HANDLE_RANK),
            date_from: None,
            date_to: None,
            limit: 50,
            offset: 0,
            p95_budget_ms: 80.0,
        },
        QuerySpec {
            label: "frequent+date-filter",
            text: word_at(FREQUENT_RANK),
            date_from: Some(DATE_FILTER_FROM),
            date_to: Some(DATE_FILTER_TO),
            limit: 50,
            offset: 0,
            p95_budget_ms: 300.0,
        },
        QuerySpec {
            label: "frequent-paged-200",
            text: word_at(FREQUENT_RANK),
            date_from: None,
            date_to: None,
            limit: 200,
            offset: 0,
            p95_budget_ms: 375.0,
        },
        QuerySpec {
            label: "frequent-deep-page",
            text: word_at(FREQUENT_RANK),
            date_from: None,
            date_to: None,
            limit: 50,
            offset: 5_000,
            // New in M2.4 and not yet measured on shared CI runners —
            // 12× headroom instead of the usual 10× until the first
            // perf-results artifacts land (then tighten with data).
            p95_budget_ms: 3000.0,
        },
        QuerySpec {
            label: "browse-newest",
            text: String::new(),
            date_from: None,
            date_to: None,
            limit: 50,
            offset: 0,
            p95_budget_ms: 60.0,
        },
        QuerySpec {
            label: "browse-deep-page",
            text: String::new(),
            date_from: None,
            date_to: None,
            limit: 50,
            offset: 100_000,
            // Same CI-unknown caveat as frequent-deep-page.
            p95_budget_ms: 1800.0,
        },
    ]
}

impl QuerySpec {
    /// The exact `SearchQuery` the app's `search_messages` IPC builds.
    fn query(&self) -> SearchQuery {
        SearchQuery {
            text: self.text.clone(),
            sources: Vec::new(),
            author: String::new(),
            from_date: self.date_from.unwrap_or_default().to_string(),
            to_date: self.date_to.unwrap_or_default().to_string(),
            limit: self.limit,
            offset: self.offset,
        }
    }
}

/// Run one paginated search exactly as the app does (a read-connection
/// hop through the pool); returns the page's `total`.
fn run_search(pool: &Arc<DbPool>, spec: &QuerySpec) -> Result<u64, String> {
    let query = spec.query();
    let total = pool
        .with_read(|s| s.search_page(&query))
        .map_err(|e| format!("query {:?}: {e}", spec.label))?
        .total;
    Ok(total)
}

/// The IPC-shaped round trip: the paged search on a pool read connection
/// plus the JSON serialization of the page — the payload the webview
/// boundary carries. Returns `(total, serialized bytes)`.
fn run_ipc(pool: &Arc<DbPool>, spec: &QuerySpec) -> Result<(u64, u64), String> {
    let query = spec.query();
    let out = pool.with_read(|s| -> Result<(u64, u64), String> {
        let page = s.search_page(&query).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec(&page).map_err(|e| e.to_string())?.len() as u64;
        Ok((page.total, bytes))
    });
    out.map_err(|e| format!("ipc query {:?}: {e}", spec.label))
}

// --- concurrent ingest ----------------------------------------------------------

/// Search while a bounded ingest commits through the pool's write lane.
///
/// The ingest runs as a real pool job (the M2.2 production import path:
/// `BoundedRun` over `PoolSink`, batched commits through the write
/// queue), while the calling thread times paged searches on the pool's
/// read connections — WAL readers see committed batches, never a
/// half-written one, and never wait behind a write. Each label's
/// percentiles during the ingest sit next to the same query's quiet
/// numbers from this run's main suite.
fn measure_concurrent_ingest(
    pool: &Arc<DbPool>,
    specs: &[QuerySpec],
    quiet_rows: &[SearchRow],
    count: u64,
    seed: u64,
) -> Result<ConcurrentIngestRow, String> {
    // The two ends of the cost spectrum: a rare term (index probe) and
    // a frequent term (rank over thousands of hits).
    let labels = ["rare-term", "frequent-term"];
    let specs: Vec<&QuerySpec> = specs.iter().filter(|s| labels.contains(&s.label)).collect();

    let source = IngestSource::new(count, seed ^ 0xC0FFEE ^ count);
    let job_pool = Arc::clone(pool);
    let job = pool
        .try_submit_job("bench_concurrent_ingest", move |_ctx| {
            let mut sink = PoolSink(&job_pool);
            let no_cancel = || false;
            let mut no_progress = |_done: u64, _total: u64, _message: &str| {};
            let run = BoundedRun::new(format!("bench-concurrent-job-{seed}-{count}"));
            run.run(&source, &mut sink, &no_cancel, &mut no_progress)
                .map_err(JobError::from)
                .and_then(|o| {
                    serde_json::to_value(&o.summary).map_err(|e| JobError::Internal {
                        message: e.to_string(),
                    })
                })
        })
        .map_err(|e| format!("submit ingest job: {e}"))?;

    let started = Instant::now();
    let mut samples: Vec<(&'static str, Vec<f64>)> =
        specs.iter().map(|s| (s.label, Vec::new())).collect();
    let mut result_value: Option<u64> = None;
    while let Some(record) = pool.job(job) {
        if let vault_core::jobs::JobState::Succeeded { result } = &record.state {
            result_value = result.get("inserted").and_then(|v| v.as_u64());
        }
        if record.state.is_terminal() {
            break;
        }
        for spec in &specs {
            let t = Instant::now();
            let _total = run_search(pool, spec)?;
            let elapsed = t.elapsed().as_secs_f64() * 1000.0;
            if let Some((_, list)) = samples.iter_mut().find(|(l, _)| *l == spec.label) {
                list.push(elapsed);
            }
        }
    }
    // The job record may already be evicted (or terminal state observed
    // late); fall back to the requested count when the result is unread.
    let ingest_messages = result_value.unwrap_or(count);
    let ingest_secs = started.elapsed().as_secs_f64();

    let search: Vec<IngestSearchRow> = samples
        .into_iter()
        .map(|(label, list)| {
            let quiet = quiet_rows.iter().find(|r| r.label == label);
            IngestSearchRow {
                label,
                samples: list.len() as u32,
                quiet_p50_ms: quiet.map(|q| q.stats.p50_ms).unwrap_or(f64::NAN),
                quiet_p95_ms: quiet.map(|q| q.stats.p95_ms).unwrap_or(f64::NAN),
                quiet_p99_ms: quiet.map(|q| q.stats.p99_ms).unwrap_or(f64::NAN),
                during: LatencyStats::of(list),
            }
        })
        .collect();

    Ok(ConcurrentIngestRow {
        ingest_messages,
        ingest_secs,
        ingest_msgs_per_sec: ingest_messages as f64 / ingest_secs.max(1e-9),
        overlapped: search.iter().any(|r| r.samples > 0),
        search,
    })
}

/// Remove the concurrent-ingest phase's rows (untimed) so a reused
/// fixture stays exactly `--count` messages; checkpoint the WAL after.
fn cleanup_ingest_rows(db_path: &Path) -> Result<(), String> {
    let conn = Connection::open(db_path).map_err(|e| format!("cleanup open: {e}"))?;
    conn.execute(
        "DELETE FROM archive_messages WHERE source_instance_id = \
         (SELECT id FROM source_instances WHERE legacy_namespace = ?1)",
        rusqlite::params![BENCH_INGEST_SOURCE],
    )
    .map_err(|e| format!("cleanup delete: {e}"))?;
    // The import checkpoint of a completed bounded import is removed by
    // the driver itself; the bench source's conversations and instance
    // row are then removable (conversations reference the instance,
    // messages referenced the conversations — both gone now), so the
    // fixture's contents match a pristine generate exactly.
    conn.execute(
        "DELETE FROM conversations WHERE source_instance_id = \
         (SELECT id FROM source_instances WHERE legacy_namespace = ?1)",
        rusqlite::params![BENCH_INGEST_SOURCE],
    )
    .map_err(|e| format!("cleanup conversations: {e}"))?;
    conn.execute(
        "DELETE FROM source_instances WHERE legacy_namespace = ?1",
        rusqlite::params![BENCH_INGEST_SOURCE],
    )
    .map_err(|e| format!("cleanup instance: {e}"))?;
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .map_err(|e| format!("cleanup checkpoint: {e}"))?;
    Ok(())
}

// --- sizes ---------------------------------------------------------------------

fn db_sizes(db_path: &Path) -> Result<(u64, u64, u64), String> {
    let db_bytes = std::fs::metadata(db_path)
        .map_err(|e| format!("stat db: {e}"))?
        .len();
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open size connection: {e}"))?;
    let page_count: i64 = conn
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .map_err(|e| format!("page_count: {e}"))?;
    let page_size: i64 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .map_err(|e| format!("page_size: {e}"))?;
    let fts_pages: Option<i64> = conn
        .query_row(
            "SELECT SUM(pgsize) FROM dbstat WHERE name LIKE 'archive_fts%'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("dbstat: {e}"))?;
    Ok((
        db_bytes,
        fts_pages.unwrap_or(0).max(0) as u64,
        (page_count * page_size) as u64,
    ))
}

// --- human summary ----------------------------------------------------------------

fn print_summary(report: &Report) {
    let f = &report.fixture;
    let gating = report.search.iter().any(|r| r.p95_budget_ms.is_some());
    println!();
    println!("== vault-perf bench ==");
    println!(
        "fixture : {} messages, seed {} ({} in {:.1}s, {:.0} msg/s)",
        report.config.count,
        report.config.seed,
        if f.generated { "generated" } else { "reused" },
        f.generation_secs,
        f.rows_per_sec
    );
    println!(
        "machine : {} ({}/{}), {} cpus, runner {}",
        report.environment.cpu_model.as_deref().unwrap_or("?"),
        report.environment.os,
        report.environment.arch,
        report.environment.cpu_count,
        report.environment.runner,
    );
    println!(
        "sizes   : db {:.1} MiB, logical {:.1} MiB, fts index {:.1} MiB",
        f.db_bytes as f64 / 1048576.0,
        f.logical_bytes as f64 / 1048576.0,
        f.fts_index_bytes as f64 / 1048576.0,
    );
    println!(
        "startup : open p50 {:.1}ms p95 {:.1}ms | open+stats p50 {:.1}ms p95 {:.1}ms | pool+first-search p50 {:.1}ms p95 {:.1}ms",
        report.startup.open.p50_ms,
        report.startup.open.p95_ms,
        report.startup.open_stats.p50_ms,
        report.startup.open_stats.p95_ms,
        report.startup.pool_spawn.p50_ms,
        report.startup.pool_spawn.p95_ms,
    );
    println!();
    println!(
        "paged search latency, Storage::search_page ({} warmup + {} timed runs per query, all times ms):",
        report.config.warmup, report.config.iterations
    );
    if gating {
        println!(
            "  {:<22} {:>10} {:>8} {:>8} {:>8} {:>8} {:>9}",
            "query", "hits", "p50", "p95", "p99", "max", "budget"
        );
        for r in &report.search {
            println!(
                "  {:<22} {:>10} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>9}",
                r.label,
                r.hits,
                r.stats.p50_ms,
                r.stats.p95_ms,
                r.stats.p99_ms,
                r.stats.max_ms,
                match (r.p95_budget_ms, r.within_budget) {
                    (Some(b), Some(true)) => format!("{b:<4.0} ok"),
                    (Some(b), _) => format!("{b:<4.0} OVER"),
                    (None, _) => "-".to_string(),
                }
            );
        }
    } else {
        println!(
            "  {:<22} {:>10} {:>8} {:>8} {:>8} {:>8}",
            "query", "hits", "p50", "p95", "p99", "max"
        );
        for r in &report.search {
            println!(
                "  {:<22} {:>10} {:>8.1} {:>8.1} {:>8.1} {:>8.1}",
                r.label, r.hits, r.stats.p50_ms, r.stats.p95_ms, r.stats.p99_ms, r.stats.max_ms
            );
        }
    }
    println!();
    println!("IPC-shaped round trips (pool read hop + JSON page serialization):");
    println!(
        "  {:<22} {:>10} {:>12} {:>8} {:>8} {:>8}",
        "query", "hits", "page bytes", "p50", "p95", "p99"
    );
    for r in &report.ipc {
        println!(
            "  {:<22} {:>10} {:>12} {:>8.1} {:>8.1} {:>8.1}",
            r.label, r.hits, r.median_payload_bytes, r.stats.p50_ms, r.stats.p95_ms, r.stats.p99_ms
        );
    }
    if let Some(ci) = &report.concurrent_ingest {
        println!();
        println!(
            "search while ingesting ({} msg committed through the write lane at {:.0} msg/s; quiet → during, ms):",
            ci.ingest_messages, ci.ingest_msgs_per_sec
        );
        println!(
            "  {:<22} {:>8} {:>10} {:>10} {:>8} {:>8} {:>8}",
            "query", "samples", "quiet p95", "during p95", "p50", "p95", "p99"
        );
        for r in &ci.search {
            println!(
                "  {:<22} {:>8} {:>10.1} {:>10.1} {:>8.1} {:>8.1} {:>8.1}",
                r.label,
                r.samples,
                r.quiet_p95_ms,
                r.during.p95_ms,
                r.during.p50_ms,
                r.during.p95_ms,
                r.during.p99_ms
            );
        }
    }
    println!();
    match &report.gate {
        Some(g) => {
            println!(
                "gate    : {} budgets; worst {} at p95 {:.1}ms ({:.1}% of budget) → {}",
                g.budgets,
                g.worst_label,
                g.worst_p95_ms,
                g.worst_ratio * 100.0,
                if g.passed { "PASS" } else { "FAIL" }
            );
        }
        None => println!("gate    : none (informational run)"),
    }
}

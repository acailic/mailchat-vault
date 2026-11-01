//! vault-perf: benchmark harness for mailchat-vault.
//!
//! Measures, against a deterministic synthetic archive:
//! - search latency (FTS5 MATCH + date filters + page size), p50/p95/p99;
//! - app startup cost on a large database (`Storage::open` + `stats()`,
//!   the exact data path the Tauri app runs on launch);
//! - FTS index and database size (via SQLite `dbstat`).
//!
//! Everything exercises the product's own `vault-core` API — no SQL of the
//! app is duplicated for timing, only for untimed hit counting.
//!
//! Run `vault-perf bench --help` for usage. See tests/perf/BASELINE.md for
//! recorded baselines and the CI gate policy.

mod fixture;
mod measure;

use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::Connection;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;

use fixture::{
    word_at, AUTHOR_HANDLE_RANK, DATE_FILTER_FROM, DATE_FILTER_TO, FREQUENT_RANK, RARE_RANK,
    UBIQUITOUS_RANK, UNCOMMON_RANK,
};
use measure::{
    BenchConfig, EnvInfo, FixtureInfo, GateResult, LatencyStats, Report, SearchRow, StartupRow,
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
            "--data-dir" => {
                args.data_dir = Some(PathBuf::from(value("--data-dir")?));
            }
            "--iterations" => {
                args.iterations = value("--iterations")?
                    .parse()
                    .map_err(|e| format!("--iterations: {e}"))?;
            }
            "--warmup" => {
                args.warmup = value("--warmup")?
                    .parse()
                    .map_err(|e| format!("--warmup: {e}"))?;
            }
            "--startup-runs" => {
                args.startup_runs = value("--startup-runs")?
                    .parse()
                    .map_err(|e| format!("--startup-runs: {e}"))?;
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

    let storage = Storage::open(&db_path).map_err(|e| format!("open fixture: {e}"))?;

    // --- startup: the app's launch data path ------------------------------
    eprintln!("measuring startup ({} runs)...", args.startup_runs);
    let startup = measure_startup(&db_path, args.startup_runs);

    // --- search latency ----------------------------------------------------
    eprintln!(
        "measuring search ({} warmup + {} timed runs per query)...",
        args.warmup, args.iterations
    );
    let counter = Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("open read-only counter connection: {e}"))?;
    let specs = query_suite();
    let gating = args.gate || args.gate_p95_ms.is_some();
    let mut search_rows = Vec::with_capacity(specs.len());
    for spec in &specs {
        let hits = count_hits(&counter, spec)?;
        let budget = match args.gate_p95_ms {
            Some(uniform) => Some(uniform),
            None if args.gate => Some(spec.p95_budget_ms),
            None => None,
        };
        let mut samples = Vec::with_capacity(args.iterations as usize);
        for _ in 0..args.warmup {
            run_search(&storage, spec)?;
        }
        for _ in 0..args.iterations {
            let t = Instant::now();
            run_search(&storage, spec)?;
            samples.push(t.elapsed().as_secs_f64() * 1000.0);
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
            query: spec.terms.join(" "),
            match_expr: build_match_query(&spec.terms.join(" ")),
            date_from: spec.date_from.map(str::to_string),
            date_to: spec.date_to.map(str::to_string),
            limit: spec.limit,
            hits,
            p95_budget_ms: budget,
            within_budget: budget.map(|b| stats.p95_ms < b),
            stats,
        });
    }

    // --- sizes -------------------------------------------------------------
    let (db_bytes, fts_index_bytes, logical_bytes) = db_sizes(&db_path)?;

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
    }
    StartupRow {
        runs,
        open: LatencyStats::of(open_ms),
        open_stats: LatencyStats::of(open_stats_ms),
    }
}

// --- search --------------------------------------------------------------------

/// One benchmark query: free-text terms (as typed into the app), optional
/// inclusive date range, page size, and its CI p95 budget.
///
/// Budgets are calibrated ≈10× the measured local p95 (with floors for
/// sub-millisecond queries) so shared-CI-runner variance cannot flake the
/// gate while order-of-magnitude regressions still trip it. Local p95s and
/// the calibration rationale live in tests/perf/BASELINE.md.
struct QuerySpec {
    label: &'static str,
    terms: Vec<String>,
    date_from: Option<&'static str>,
    date_to: Option<&'static str>,
    limit: u32,
    p95_budget_ms: f64,
}

fn query_suite() -> Vec<QuerySpec> {
    vec![
        QuerySpec {
            label: "ubiquitous-term",
            terms: vec![word_at(UBIQUITOUS_RANK)],
            date_from: None,
            date_to: None,
            limit: 50,
            p95_budget_ms: 750.0,
        },
        QuerySpec {
            label: "frequent-term",
            terms: vec![word_at(FREQUENT_RANK)],
            date_from: None,
            date_to: None,
            limit: 50,
            p95_budget_ms: 75.0,
        },
        QuerySpec {
            label: "rare-term",
            terms: vec![word_at(RARE_RANK)],
            date_from: None,
            date_to: None,
            limit: 50,
            p95_budget_ms: 15.0,
        },
        QuerySpec {
            label: "two-term-and",
            terms: vec![word_at(FREQUENT_RANK), word_at(UNCOMMON_RANK)],
            date_from: None,
            date_to: None,
            limit: 50,
            p95_budget_ms: 20.0,
        },
        QuerySpec {
            label: "author-term",
            terms: vec![word_at(AUTHOR_HANDLE_RANK)],
            date_from: None,
            date_to: None,
            limit: 50,
            p95_budget_ms: 25.0,
        },
        QuerySpec {
            label: "frequent+date-filter",
            terms: vec![word_at(FREQUENT_RANK)],
            date_from: Some(DATE_FILTER_FROM),
            date_to: Some(DATE_FILTER_TO),
            limit: 50,
            p95_budget_ms: 75.0,
        },
        QuerySpec {
            label: "frequent-paged-200",
            terms: vec![word_at(FREQUENT_RANK)],
            date_from: None,
            date_to: None,
            limit: 200,
            p95_budget_ms: 100.0,
        },
    ]
}

fn run_search(storage: &Storage, spec: &QuerySpec) -> Result<(), String> {
    let match_expr = build_match_query(&spec.terms.join(" "));
    let results = storage
        .search_filtered(&match_expr, spec.date_from, spec.date_to, spec.limit)
        .map_err(|e| format!("query {:?}: {e}", spec.label))?;
    std::hint::black_box(results.len());
    Ok(())
}

/// Untimed hit count mirroring the product's search SQL (for report context).
fn count_hits(conn: &Connection, spec: &QuerySpec) -> Result<u64, String> {
    let match_expr = build_match_query(&spec.terms.join(" "));
    let from: Option<String> = spec.date_from.map(str::to_string);
    let to_exclusive: Option<String> = spec
        .date_to
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .and_then(|d| d.checked_add_days(chrono::Days::new(1)))
        .map(|d| d.to_string());
    let sql = if spec.date_from.is_some() {
        "SELECT COUNT(*) FROM messages_fts \
         JOIN messages AS m ON m.id = messages_fts.rowid \
         WHERE messages_fts MATCH ?1 AND m.sent_at >= ?2 AND m.sent_at < ?3"
    } else {
        "SELECT COUNT(*) FROM messages_fts WHERE messages_fts MATCH ?1"
    };
    let default_from = String::new();
    let default_to = String::new();
    let params: Vec<&dyn rusqlite::ToSql> = if spec.date_from.is_some() {
        vec![
            &match_expr,
            from.as_ref().unwrap_or(&default_from),
            to_exclusive.as_ref().unwrap_or(&default_to),
        ]
    } else {
        vec![&match_expr]
    };
    conn.query_row(sql, params.as_slice(), |row| row.get::<_, i64>(0))
        .map(|n| n as u64)
        .map_err(|e| format!("count {:?}: {e}", spec.label))
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
            "SELECT SUM(pgsize) FROM dbstat WHERE name LIKE 'messages_fts%'",
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
        "startup : open p50 {:.1}ms p95 {:.1}ms | open+stats p50 {:.1}ms p95 {:.1}ms",
        report.startup.open.p50_ms,
        report.startup.open.p95_ms,
        report.startup.open_stats.p50_ms,
        report.startup.open_stats.p95_ms,
    );
    println!();
    println!(
        "search latency ({} warmup + {} timed runs per query, all times ms):",
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

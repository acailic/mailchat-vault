//! Latency sampling, percentile statistics, and report types.

use serde::Serialize;

/// Percentile summary of a set of latency samples, in milliseconds.
#[derive(Debug, Clone, Serialize)]
pub struct LatencyStats {
    pub min_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl LatencyStats {
    /// Nearest-rank percentiles over `samples`; consumes and sorts it.
    pub fn of(mut samples: Vec<f64>) -> Self {
        samples.sort_by(|a, b| a.total_cmp(b));
        let pick = |p: f64| nearest_rank(&samples, p);
        LatencyStats {
            min_ms: samples.first().copied().unwrap_or(f64::NAN),
            p50_ms: pick(50.0),
            p95_ms: pick(95.0),
            p99_ms: pick(99.0),
            max_ms: samples.last().copied().unwrap_or(f64::NAN),
        }
    }
}

/// Nearest-rank percentile: the ceil(p/100·n)-th smallest sample.
fn nearest_rank(sorted: &[f64], percentile: f64) -> f64 {
    assert!((0.0..=100.0).contains(&percentile));
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((percentile / 100.0 * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// One benchmarked search query and its measured latencies.
#[derive(Debug, Clone, Serialize)]
pub struct SearchRow {
    /// Short label used in tables and the gate report.
    pub label: &'static str,
    /// Free-text input exactly as the app would receive it (empty =
    /// newest-first browse).
    pub query: String,
    /// Sanitized FTS5 MATCH expression the app would run (empty for
    /// browse).
    pub match_expr: String,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    /// Page size (`LIMIT`), mirroring the app's pagination knob.
    pub limit: u32,
    /// Page offset (`OFFSET`) — 0 for the first page, large for deep
    /// pages.
    pub offset: u32,
    /// Total rows matching the query (the page's `total`; counted as
    /// part of the timed `search_page` call, exactly what the UI pays).
    pub hits: u64,
    /// p95 budget (ms) when gating; `None` on informational runs.
    pub p95_budget_ms: Option<f64>,
    /// `Some(false)` when gating and p95 >= budget.
    pub within_budget: Option<bool>,
    #[serde(flatten)]
    pub stats: LatencyStats,
}

/// Storage startup measurements on the fixture database.
#[derive(Debug, Clone, Serialize)]
pub struct StartupRow {
    pub runs: u32,
    /// `Storage::open` only (migrations + pragmas).
    pub open: LatencyStats,
    /// `Storage::open` + `stats()` — the pre-pool launch data path.
    pub open_stats: LatencyStats,
    /// `DbPool::spawn` + first paged search on a read connection +
    /// shutdown — the app's launch data path since M2.1 (three
    /// connections, writer + job runner threads, then a query).
    pub pool_spawn: LatencyStats,
}

/// One IPC-shaped measurement: the paged search round trip exactly as
/// the `search_messages` command pays it — a read-connection hop
/// through the pool plus the JSON serialization of the page (the
/// payload that crosses the webview boundary). The webview/OS hop
/// itself is not measurable in a core-only harness and is deliberately
/// not estimated.
#[derive(Debug, Clone, Serialize)]
pub struct IpcRow {
    pub label: &'static str,
    /// Total rows matching the query (the page's `total`).
    pub hits: u64,
    /// Median serialized page size (bytes) — the IPC payload.
    pub median_payload_bytes: u64,
    #[serde(flatten)]
    pub stats: LatencyStats,
}

/// Search latency measured while a bounded ingest commits through the
/// pool's write lane (the M2.2 production import path) on the same
/// database — the "query while ingesting" contract. `quiet_*` carry
/// the same query's percentiles from the uninterrupted main suite of
/// the same run for a direct comparison.
#[derive(Debug, Clone, Serialize)]
pub struct IngestSearchRow {
    pub label: &'static str,
    pub samples: u32,
    pub quiet_p50_ms: f64,
    pub quiet_p95_ms: f64,
    pub quiet_p99_ms: f64,
    #[serde(flatten)]
    pub during: LatencyStats,
}

/// The concurrent-ingest measurement as a whole.
#[derive(Debug, Clone, Serialize)]
pub struct ConcurrentIngestRow {
    /// Messages the ingest job committed.
    pub ingest_messages: u64,
    pub ingest_secs: f64,
    pub ingest_msgs_per_sec: f64,
    /// Whether at least one search completed while the ingest was
    /// still committing (false only on absurdly fast finishes).
    pub overlapped: bool,
    pub search: Vec<IngestSearchRow>,
}

/// Environment the benchmark ran on.
#[derive(Debug, Clone, Serialize)]
pub struct EnvInfo {
    pub os: &'static str,
    pub arch: &'static str,
    pub cpu_count: usize,
    /// Best-effort CPU model (Linux: /proc/cpuinfo).
    pub cpu_model: Option<String>,
    /// "github-actions" when running in CI, else "local".
    pub runner: &'static str,
}

impl EnvInfo {
    pub fn detect() -> Self {
        EnvInfo {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            cpu_count: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            cpu_model: cpu_model(),
            runner: if std::env::var_os("GITHUB_ACTIONS").is_some() {
                "github-actions"
            } else {
                "local"
            },
        }
    }
}

fn cpu_model() -> Option<String> {
    let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    info.lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
}

/// Full benchmark report; also the `--json` output shape.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub harness: &'static str,
    pub started_at: String,
    pub config: BenchConfig,
    pub environment: EnvInfo,
    pub fixture: FixtureInfo,
    pub startup: StartupRow,
    pub search: Vec<SearchRow>,
    /// IPC-shaped paged-search round trips (read hop + serialization).
    pub ipc: Vec<IpcRow>,
    /// Search latency while a bounded ingest commits (query while
    /// ingesting), `None` when the ingest phase was skipped (--ingest-count 0).
    pub concurrent_ingest: Option<ConcurrentIngestRow>,
    pub gate: Option<GateResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchConfig {
    pub count: u64,
    pub seed: u64,
    pub iterations: u32,
    pub warmup: u32,
    pub startup_runs: u32,
    /// Messages the concurrent-ingest phase commits (`0` disables it).
    pub ingest_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FixtureInfo {
    pub db_path: String,
    /// Whether this run generated the fixture or reused an existing one.
    pub generated: bool,
    pub generation_secs: f64,
    pub rows_per_sec: f64,
    /// Total on-disk size of the database file.
    pub db_bytes: u64,
    /// Size of the FTS5 index (dbstat pages of `archive_fts%` tables).
    pub fts_index_bytes: u64,
    /// SQLite page count × page size.
    pub logical_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateResult {
    /// "per-query" (calibrated budgets) or "uniform" (--gate-p95-ms).
    pub budgets: &'static str,
    /// Uniform budget (ms) when `--gate-p95-ms` was used.
    pub uniform_budget_ms: Option<f64>,
    /// Query closest to (or beyond) its budget, by p95÷budget ratio.
    pub worst_label: String,
    pub worst_p95_ms: f64,
    /// p95 ÷ budget for the worst query; 1.0 means at the limit.
    pub worst_ratio: f64,
    pub passed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles() {
        let samples: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(nearest_rank(&samples, 50.0), 50.0);
        assert_eq!(nearest_rank(&samples, 95.0), 95.0);
        assert_eq!(nearest_rank(&samples, 99.0), 99.0);
        assert_eq!(nearest_rank(&samples, 100.0), 100.0);
        assert_eq!(nearest_rank(&samples, 0.0), 1.0);
    }

    #[test]
    fn nearest_rank_small_sets() {
        assert_eq!(nearest_rank(&[5.0], 99.0), 5.0);
        assert_eq!(nearest_rank(&[1.0, 2.0], 95.0), 2.0);
        assert!(nearest_rank(&[], 50.0).is_nan());
    }

    #[test]
    fn stats_are_ordered() {
        let s = LatencyStats::of(vec![9.0, 1.0, 5.0, 3.0, 7.0]);
        assert_eq!(s.min_ms, 1.0);
        assert_eq!(s.max_ms, 9.0);
        assert!(
            s.min_ms <= s.p50_ms
                && s.p50_ms <= s.p95_ms
                && s.p95_ms <= s.p99_ms
                && s.p99_ms <= s.max_ms
        );
    }
}

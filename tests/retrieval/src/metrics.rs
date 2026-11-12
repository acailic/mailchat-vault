//! Metric types and aggregation for the retrieval benchmark.
//!
//! Definitions (mirrored in `tests/retrieval/BASELINE.md`):
//!
//! - **recall@k** — the labeled correct original ranks within the top k
//!   results of the first result page (answerable questions only).
//! - **correct-original task success** — recall@5: a user scanning the
//!   first five summaries finds the exact original. This is U07's task
//!   success proxy; recall@1 and MRR add granularity.
//! - **false confidence** — a `no-answer` probe returns a non-empty
//!   first page: the system asserts content it does not have.
//! - **confident but wrong** — an answerable question whose page is
//!   non-empty while the correct original sits outside the top 5.
//! - **latency** — per question the median of `runs` repetitions of the
//!   full call; percentiles are taken across the question medians.

use serde::Serialize;

/// One measured question on one path.
#[derive(Debug, Clone, Serialize)]
pub struct QuestionResult {
    pub id: String,
    pub text: String,
    pub category: String,
    pub language: String,
    pub channel: String,
    pub answerable: bool,
    /// Total matches reported by the path (FTS page total / hybrid list
    /// length).
    pub hits: u64,
    /// 1-based rank of the correct original within the returned page;
    /// `None` when absent.
    pub rank: Option<u32>,
    /// True when a no-answer probe returned a non-empty page.
    pub false_confident: bool,
    /// Median latency of the measured call, milliseconds.
    pub latency_ms: f64,
    /// Hybrid path only: which legs produced the ranking.
    pub ranking_mode: String,
    /// Design intent of the question, from the question set.
    pub note: String,
}

/// Aggregated stats for one group of questions (overall, a category, a
/// language, a channel).
#[derive(Debug, Clone, Default, Serialize)]
pub struct GroupStats {
    pub n: usize,
    pub answerable: usize,
    pub recall1: f64,
    pub recall5: f64,
    pub recall10: f64,
    pub mrr: f64,
    pub no_answer: usize,
    pub false_confident: usize,
    pub confident_wrong: usize,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub latency_max_ms: f64,
}

/// All results for one path.
#[derive(Debug, Clone, Serialize)]
pub struct PathReport {
    pub path: String,
    pub results: Vec<QuestionResult>,
    pub overall: GroupStats,
    pub by_category: Vec<(String, GroupStats)>,
    pub by_language: Vec<(String, GroupStats)>,
    pub by_channel: Vec<(String, GroupStats)>,
    /// Hybrid path only: ranking-mode distribution across questions.
    pub ranking_modes: Vec<(String, usize)>,
}

/// Nearest-rank percentile of a non-empty sample (0–100). Returns 0 for
/// an empty sample.
pub fn percentile(mut sample: Vec<f64>, p: f64) -> f64 {
    if sample.is_empty() {
        return 0.0;
    }
    sample.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((p / 100.0) * sample.len() as f64).ceil() as usize;
    sample[rank.saturating_sub(1).min(sample.len() - 1)]
}

pub fn median(sample: &[f64]) -> f64 {
    percentile(sample.to_vec(), 50.0)
}

/// Aggregate one group of question results.
pub fn group(results: &[&QuestionResult]) -> GroupStats {
    let mut stats = GroupStats {
        n: results.len(),
        ..Default::default()
    };
    let mut answerable_ranks: Vec<f64> = Vec::new();
    let mut latencies: Vec<f64> = Vec::new();
    for r in results {
        latencies.push(r.latency_ms);
        match r.rank {
            Some(rank) if r.answerable => {
                stats.answerable += 1;
                answerable_ranks.push(1.0 / rank as f64);
                if rank <= 1 {
                    stats.recall1 += 1.0;
                }
                if rank <= 5 {
                    stats.recall5 += 1.0;
                }
                if rank <= 10 {
                    stats.recall10 += 1.0;
                }
            }
            _ if r.answerable => {
                stats.answerable += 1;
                if r.hits > 0 {
                    stats.confident_wrong += 1;
                }
            }
            _ => {
                stats.no_answer += 1;
                if r.false_confident {
                    stats.false_confident += 1;
                }
            }
        }
    }
    let denom = stats.answerable.max(1) as f64;
    stats.recall1 /= denom;
    stats.recall5 /= denom;
    stats.recall10 /= denom;
    stats.mrr = answerable_ranks.iter().sum::<f64>() / denom;
    stats.latency_p50_ms = percentile(latencies.clone(), 50.0);
    stats.latency_p95_ms = percentile(latencies.clone(), 95.0);
    stats.latency_max_ms = latencies.iter().cloned().fold(0.0, f64::max);
    stats
}

/// Build the full report for one path.
pub fn report(path: &str, results: Vec<QuestionResult>) -> PathReport {
    let refs: Vec<&QuestionResult> = results.iter().collect();
    let overall = group(&refs);

    let by_key = |key: fn(&QuestionResult) -> &str| -> Vec<(String, GroupStats)> {
        let mut keys: Vec<String> = results.iter().map(key).map(str::to_string).collect();
        keys.sort();
        keys.dedup();
        keys.into_iter()
            .map(|k| {
                let subset: Vec<&QuestionResult> =
                    refs.iter().copied().filter(|r| key(r) == k).collect();
                (k, group(&subset))
            })
            .collect()
    };

    let mut ranking_modes: Vec<(String, usize)> = Vec::new();
    for r in &results {
        if r.ranking_mode.is_empty() {
            continue;
        }
        if let Some(entry) = ranking_modes.iter_mut().find(|(m, _)| m == &r.ranking_mode) {
            entry.1 += 1;
        } else {
            ranking_modes.push((r.ranking_mode.clone(), 1));
        }
    }
    ranking_modes.sort();

    PathReport {
        path: path.to_string(),
        overall,
        by_category: by_key(|r| &r.category),
        by_language: by_key(|r| &r.language),
        by_channel: by_key(|r| &r.channel),
        ranking_modes,
        results,
    }
}

//! vault-retrieval: U07 retrieval-quality benchmark harness for
//! mailchat-vault.
//!
//! Measures, against a deterministic synthetic corpus (email + chat
//! exports, labeled correct originals, deliberate confusers), through
//! **only the product's own search API**:
//!
//! - **`fts`** — `Storage::search_archive` (v2), FTS5 bm25 relevance,
//!   the default page the workspace runs (snippets + exact total);
//! - **`hybrid`** — the local Ask pipeline (`llm::ask` without an LLM):
//!   the same FTS5 leg fused with the local hash-vector leg by
//!   reciprocal rank fusion — the existing FTS5+RRF ranking.
//!
//! Metrics per U07 (`docs/user-success-plan.md`): top-5 recall
//! (correct-original task success), recall@1/@10, MRR, false-confidence
//! cases, latency percentiles — reported per category and language.
//!
//! This is a measurement instrument, not a quality contract: gates
//! (when enabled) only protect the instrument itself (corpus/label
//! integrity and the lexically-trivial floor), never measured quality
//! levels. Improvement work (embeddings, query expansion, pagination)
//! is out of scope by design and conditional on these findings.
//!
//! Run `vault-retrieval bench --help` for usage. Recorded baselines and
//! the claim-calibration notes live in `tests/retrieval/BASELINE.md`.

mod corpus;
mod metrics;
mod questions;

use std::path::PathBuf;
use std::time::Instant;

use chrono::NaiveDate;
use serde::Serialize;
use vault_core::llm::{ask, RankingMode};
use vault_core::search2::SearchArchiveRequest;
use vault_core::vector::{run_embedding_job, EmbedJobSummary, Embedder, HashEmbedder};

use corpus::BuiltCorpus;
use metrics::{median, report as build_report, PathReport, QuestionResult};
use questions::QUESTIONS;

const USAGE: &str = "\
vault-retrieval bench — mailchat-vault retrieval-quality harness (U07)

Usage: vault-retrieval bench [OPTIONS]
  --count N        total messages in the corpus (default 10000)
  --seed N         corpus seed (default 42)
  --data-dir DIR   where the fixture database is written
                   (default: system temp /vault-retrieval)
  --runs N         timed repetitions per question and path (default 5,
                   median reported)
  --gate           fail (exit 1) on instrument violations: self-check,
                   exact-term recall@5 < 1.0 or phrase recall@5 < 0.75 on
                   the FTS path, or any false-confidence on no-answer
                   probes. These floors protect the instrument, not a
                   measured quality level.
  --json PATH      write the full results as JSON to PATH
  --self-check-only  build the corpus, run the self-check, exit

The corpus is regenerated on every run (deterministic for the same
--count/--seed); the fixture database is left in the data dir for
inspection and overwritten by the next identical run.
";

struct Args {
    count: usize,
    seed: u64,
    data_dir: Option<PathBuf>,
    runs: usize,
    gate: bool,
    json: Option<PathBuf>,
    self_check_only: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        count: 10_000,
        seed: 42,
        data_dir: None,
        runs: 5,
        gate: false,
        json: None,
        self_check_only: false,
    };
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // Accept (and ignore) the `bench` subcommand word, mirroring perf.
    let raw: &[String] = raw.strip_prefix(&["bench".to_string()][..]).unwrap_or(&raw);
    let mut it = raw.iter().cloned();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .ok_or_else(|| format!("{USAGE}\nmissing value for {flag}"))
        };
        match flag.as_str() {
            "--count" => args.count = value()?.parse().map_err(|e| format!("bad count: {e}"))?,
            "--seed" => args.seed = value()?.parse().map_err(|e| format!("bad seed: {e}"))?,
            "--data-dir" => {
                args.data_dir = Some(PathBuf::from(value()?));
            }
            "--runs" => args.runs = value()?.parse().map_err(|e| format!("bad runs: {e}"))?,
            "--gate" => args.gate = true,
            "--json" => args.json = Some(PathBuf::from(value()?)),
            "--self-check-only" => args.self_check_only = true,
            "-h" | "--help" => return Err(USAGE.to_string()),
            other => return Err(format!("{USAGE}\nunknown flag {other:?}")),
        }
    }
    if args.count < 1_000 {
        return Err(format!("{USAGE}\n--count must be >= 1000"));
    }
    if args.runs == 0 {
        return Err(format!("{USAGE}\n--runs must be >= 1"));
    }
    Ok(args)
}

#[derive(Serialize)]
struct CorpusInfo {
    count: usize,
    seed: u64,
    by_source: [u64; 3],
    background: u64,
    targets: u64,
    confusers: u64,
    build_ms: u64,
}

#[derive(Serialize)]
struct EmbeddingInfo {
    model: String,
    dim: u16,
    embedded: u64,
    elapsed_ms: u64,
}

#[derive(Serialize)]
struct EnvInfo {
    os: &'static str,
    arch: &'static str,
    cpus: usize,
}

#[derive(Serialize)]
struct Report {
    harness: &'static str,
    corpus: CorpusInfo,
    embedding: EmbeddingInfo,
    env: EnvInfo,
    paths: Vec<PathReport>,
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprint!("{msg}");
            std::process::exit(2);
        }
    };

    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("vault-retrieval"));
    let db_path = data_dir.join(format!("retrieval-{}-{}.db", args.count, args.seed));
    // Deterministic regeneration: a stale file from an aborted run must
    // never be measured. The generator produces identical bytes for the
    // same (count, seed).
    let _ = std::fs::remove_file(&db_path);

    println!(
        "Building corpus: {} messages, seed {}",
        args.count, args.seed
    );
    let start = Instant::now();
    let built = corpus::build(&db_path, args.count, args.seed);
    let build_ms = start.elapsed().as_millis() as u64;
    let BuiltCorpus {
        storage,
        target_ids,
        by_source,
        background,
        targets,
        confusers,
    } = &built;
    println!(
        "  {count} messages: {mbox} mbox / {wa} whatsapp / {tg} telegram \
         ({background} background, {targets} labeled, {confusers} confusers) in {build_ms} ms",
        count = args.count,
        mbox = by_source[0],
        wa = by_source[1],
        tg = by_source[2],
    );
    println!("  fixture database: {}", db_path.display());

    let answerable = QUESTIONS.iter().filter(|q| q.target.is_some()).count();
    let no_answer = QUESTIONS.len() - answerable;
    let en = QUESTIONS.iter().filter(|q| q.language == "en").count();
    let sr = QUESTIONS.len() - en;

    // ---- self-check: the instrument must be intact before measuring ----
    let violations = corpus::self_check(&built);
    if !violations.is_empty() {
        eprintln!("SELF-CHECK FAILED ({} violations):", violations.len());
        for v in &violations {
            eprintln!("  - {v}");
        }
        std::process::exit(1);
    }
    println!(
        "  self-check: OK — {} questions ({answerable} answerable, {no_answer} no-answer; {en} en / {sr} sr)",
        QUESTIONS.len()
    );
    if args.self_check_only {
        println!("self-check-only: stopping before measurement");
        return;
    }

    // ---- embeddings: the vector leg must cover the whole corpus ----
    let embedder = HashEmbedder::new();
    let start = Instant::now();
    let EmbedJobSummary {
        embedded,
        elapsed_ms: embed_job_ms,
        ..
    } = run_embedding_job(storage, &embedder, None).expect("embedding job");
    let _ = start.elapsed();
    println!(
        "  embeddings: {} ({}) — {embedded} embedded in {embed_job_ms} ms",
        embedder.id(),
        embedder.dim()
    );

    // ---- measurement ----
    let fts_results = measure_fts(storage, target_ids, args.runs);
    let hybrid_results = measure_hybrid(storage, target_ids, args.runs);
    let paths = vec![
        build_report("fts", fts_results),
        build_report("hybrid", hybrid_results),
    ];

    for path in &paths {
        print_path(path);
    }

    // ---- gate: instrument floors only ----
    if args.gate {
        let failures = gate_failures(&paths);
        if failures.is_empty() {
            println!("gate: OK (instrument floors)");
        } else {
            eprintln!("GATE FAILED:");
            for f in failures {
                eprintln!("  - {f}");
            }
            write_json(&args, &built, build_ms, embed_job_ms, &paths);
            std::process::exit(1);
        }
    }

    write_json(&args, &built, build_ms, embed_job_ms, &paths);
}

/// Measure the v2 local search path (`search_archive`, relevance sort,
/// the default 50-row page the workspace runs).
fn measure_fts(
    storage: &vault_core::storage::Storage,
    target_ids: &std::collections::HashMap<&'static str, i64>,
    runs: usize,
) -> Vec<QuestionResult> {
    let mut out = Vec::with_capacity(QUESTIONS.len());
    for q in QUESTIONS {
        let request = SearchArchiveRequest {
            text: q.text.to_string(),
            ..Default::default()
        };
        let mut page = None;
        let mut times: Vec<f64> = Vec::with_capacity(runs);
        for _ in 0..runs {
            let t0 = Instant::now();
            let p = storage.search_archive(&request).expect("search_archive");
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
            page = Some(p);
        }
        let page = page.expect("at least one run");
        let rank = target_ids
            .get(q.id)
            .and_then(|id| page.hits.iter().position(|h| h.id == *id))
            .map(|i| i as u32 + 1);
        out.push(QuestionResult {
            id: q.id.to_string(),
            text: q.text.to_string(),
            category: q.category.to_string(),
            language: q.language.to_string(),
            channel: q.channel.to_string(),
            answerable: q.target.is_some(),
            hits: page.total,
            rank,
            false_confident: !page.hits.is_empty(),
            latency_ms: median(&times),
            ranking_mode: String::new(),
            note: q.note.to_string(),
        });
    }
    out
}

/// Measure the local hybrid Ask pipeline (`llm::ask`, no LLM): FTS5 leg
/// + hash-vector leg fused by RRF.
///
/// `summarize_top` is 0 and no LLM is configured, so nothing leaves the
/// machine and no summarization cost is measured — this is the ranking
/// path.
fn measure_hybrid(
    storage: &vault_core::storage::Storage,
    target_ids: &std::collections::HashMap<&'static str, i64>,
    runs: usize,
) -> Vec<QuestionResult> {
    let embedder = HashEmbedder::new();
    // Fixed reference date: the plain (no-LLM) plan never derives date
    // bounds, so this only keeps runs byte-comparable.
    let today = NaiveDate::from_ymd_opt(2026, 11, 12).expect("valid date");
    let mut out = Vec::with_capacity(QUESTIONS.len());
    for q in QUESTIONS {
        let mut outcome = None;
        let mut times: Vec<f64> = Vec::with_capacity(runs);
        for _ in 0..runs {
            let t0 = Instant::now();
            let o = ask(storage, None, &embedder, q.text, 50, 0, today).expect("ask");
            times.push(t0.elapsed().as_secs_f64() * 1000.0);
            outcome = Some(o);
        }
        let outcome = outcome.expect("at least one run");
        let rank = target_ids
            .get(q.id)
            .and_then(|id| outcome.results.iter().position(|m| m.id == *id))
            .map(|i| i as u32 + 1);
        let mode = match outcome.ranking {
            RankingMode::FtsOnly => "fts-only",
            RankingMode::HybridFtsVector => "hybrid-fts-vector",
            RankingMode::VectorOnly => "vector-only",
        };
        out.push(QuestionResult {
            id: q.id.to_string(),
            text: q.text.to_string(),
            category: q.category.to_string(),
            language: q.language.to_string(),
            channel: q.channel.to_string(),
            answerable: q.target.is_some(),
            hits: outcome.results.len() as u64,
            rank,
            false_confident: !outcome.results.is_empty(),
            latency_ms: median(&times),
            ranking_mode: mode.to_string(),
            note: q.note.to_string(),
        });
    }
    out
}

fn print_path(path: &PathReport) {
    println!();
    let what = if path.path == "fts" {
        "v2 search_archive — FTS5 bm25, default page (limit 50, snippets + total)"
    } else {
        "llm::ask (no LLM) — FTS5 leg + hash-vector leg, RRF fusion"
    };
    println!("== Path: {} ({what}) ==", path.path);
    let o = &path.overall;
    println!(
        "  overall: n={} answerable={} | recall@1={:.2} recall@5={:.2} recall@10={:.2} mrr={:.2}",
        o.n, o.answerable, o.recall1, o.recall5, o.recall10, o.mrr
    );
    println!(
        "  task-success(top-5)={:.2} | false-confidence {}/{} | confident-but-wrong {}",
        o.recall5, o.false_confident, o.no_answer, o.confident_wrong
    );
    println!(
        "  latency ms: p50={:.1} p95={:.1} max={:.1}",
        o.latency_p50_ms, o.latency_p95_ms, o.latency_max_ms
    );
    if !path.ranking_modes.is_empty() {
        let modes: Vec<String> = path
            .ranking_modes
            .iter()
            .map(|(m, n)| format!("{m}×{n}"))
            .collect();
        println!("  ranking modes: {}", modes.join(", "));
    }
    for (label, groups) in [
        ("category", &path.by_category),
        ("language", &path.by_language),
        ("channel", &path.by_channel),
    ] {
        println!("  by {label}:");
        println!(
            "    {:<16}{:>3} {:>6} {:>6} {:>6} {:>6} {:>8} {:>8} {:>8} {:>8}",
            "", "n", "r@1", "r@5", "r@10", "mrr", "f-conf", "c-wrong", "p50ms", "p95ms"
        );
        for (name, g) in groups {
            println!(
                "    {:<16}{:>3} {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>8} {:>8} {:>8.1} {:>8.1}",
                name,
                g.n,
                g.recall1.max(0.0),
                g.recall5.max(0.0),
                g.recall10.max(0.0),
                g.mrr.max(0.0),
                format!("{}/{}", g.false_confident, g.no_answer),
                g.confident_wrong,
                g.latency_p50_ms,
                g.latency_p95_ms
            );
        }
    }
}

/// Instrument floors only — never measured-quality contracts. They
/// catch corpus/label rot and ranking breakage so the benchmark stays a
/// trustworthy instrument; see BASELINE.md's gate policy.
fn gate_failures(paths: &[PathReport]) -> Vec<String> {
    let mut failures = Vec::new();
    let Some(fts) = paths.iter().find(|p| p.path == "fts") else {
        return vec!["fts path missing from report".to_string()];
    };
    for (name, g) in &fts.by_category {
        match name.as_str() {
            "exact-term" if g.recall5 < 1.0 => failures.push(format!(
                "fts exact-term recall@5 = {:.2} (floor 1.00) — corpus or label rot",
                g.recall5
            )),
            "phrase" if g.recall5 < 0.75 => failures.push(format!(
                "fts phrase recall@5 = {:.2} (floor 0.75) — ranking regression on all-terms queries",
                g.recall5
            )),
            _ => {}
        }
    }
    if fts.overall.false_confident > 0 {
        failures.push(format!(
            "fts false-confidence on no-answer probes: {} (floor 0) — background vocabulary collision",
            fts.overall.false_confident
        ));
    }
    for path in paths {
        if path.results.len() != QUESTIONS.len() {
            failures.push(format!(
                "path {} measured {} questions (want {})",
                path.path,
                path.results.len(),
                QUESTIONS.len()
            ));
        }
    }
    failures
}

fn write_json(
    args: &Args,
    built: &BuiltCorpus,
    build_ms: u64,
    embed_job_ms: u64,
    paths: &[PathReport],
) {
    let Some(json_path) = &args.json else { return };
    let full = Report {
        harness: "vault-retrieval",
        corpus: CorpusInfo {
            count: args.count,
            seed: args.seed,
            by_source: built.by_source,
            background: built.background,
            targets: built.targets,
            confusers: built.confusers,
            build_ms,
        },
        embedding: EmbeddingInfo {
            model: HashEmbedder::MODEL_ID.to_string(),
            dim: HashEmbedder::DIM,
            embedded: built.by_source.iter().sum::<u64>(),
            elapsed_ms: embed_job_ms,
        },
        env: EnvInfo {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            cpus: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
        },
        paths: paths.to_vec(),
    };
    let pretty = serde_json::to_string_pretty(&full).expect("serialize report");
    if let Some(parent) = json_path.parent() {
        std::fs::create_dir_all(parent).expect("create json parent dir");
    }
    std::fs::write(json_path, pretty)
        .unwrap_or_else(|e| panic!("write {}: {e}", json_path.display()));
    println!("json report: {}", json_path.display());
}

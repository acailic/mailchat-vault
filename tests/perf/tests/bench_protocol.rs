//! Protocol-logic tests for the benchmark harness: percentile math,
//! gate comparison, baseline parsing (hash mismatch, missing or stale
//! entries), and small-count generator determinism. No timing, no
//! fixtures on disk — protocol only.

use std::collections::BTreeMap;
use std::path::PathBuf;

use vault_core::query::SearchQuery;
use vault_core::storage::Storage;
use vault_perf::fixture;
use vault_perf::measure::LatencyStats;
use vault_perf::protocol::{self, Baseline, GateCheck, Methodology};
use vault_perf::suite::QuerySpec;

fn test_methodology() -> Methodology {
    Methodology {
        schema: 1,
        harness: "vault-perf 0.1.0".into(),
        fixture_count: 100_000,
        seed: 42,
        iterations: 60,
        warmup: 5,
        machine_class: "test machine class".into(),
        calibration: "test calibration rule".into(),
    }
}

fn write_baseline(methodology: &Methodology, budgets: BTreeMap<String, f64>) -> PathBuf {
    let baseline = Baseline {
        methodology: methodology.clone(),
        methodology_sha256: Baseline::methodology_hash(methodology),
        budgets_ms: budgets,
    };
    let path = std::env::temp_dir().join(format!(
        "vault-perf-bench-protocol-{}-{}.json",
        std::process::id(),
        &protocol::sha256_hex(&serde_json::to_vec(&baseline).unwrap())[..12]
    ));
    std::fs::write(&path, serde_json::to_vec_pretty(&baseline).unwrap()).unwrap();
    path
}

// --- percentiles ---------------------------------------------------------------

#[test]
fn percentiles_follow_the_nearest_rank_definition() {
    let samples: Vec<f64> = (1..=100).map(f64::from).collect();
    let stats = LatencyStats::of(samples);
    assert_eq!(stats.min_ms, 1.0);
    assert_eq!(stats.max_ms, 100.0);
    // Nearest-rank: ceil(p/100 * n)-th smallest.
    assert_eq!(stats.p50_ms, 50.0);
    assert_eq!(stats.p95_ms, 95.0);
    assert_eq!(stats.p99_ms, 99.0);
}

#[test]
fn percentiles_are_ordered_and_clamped_on_small_sets() {
    let stats = LatencyStats::of(vec![9.0, 1.0, 5.0]);
    assert_eq!(stats.p50_ms, 5.0);
    assert!(stats.min_ms <= stats.p50_ms && stats.p50_ms <= stats.p95_ms);
    // A single sample is every percentile.
    let one = LatencyStats::of(vec![7.5]);
    assert_eq!((one.p50_ms, one.p95_ms, one.p99_ms), (7.5, 7.5, 7.5));
}

// --- gate comparison -------------------------------------------------------------

#[test]
fn gate_passes_when_every_p95_stays_under_its_budget() {
    let outcome = protocol::evaluate_gate(vec![
        GateCheck {
            label: "rare-term".into(),
            p95_ms: 3.3,
            budget_ms: 40.0,
        },
        GateCheck {
            label: "ubiquitous-term".into(),
            p95_ms: 155.5,
            budget_ms: 1500.0,
        },
    ]);
    assert!(outcome.passed);
    // Worst by ratio, not by absolute p95: ubiquitous is closer (10.4% vs 8.3%).
    assert_eq!(outcome.worst.label, "ubiquitous-term");
    assert!((outcome.worst.ratio() - 155.5 / 1500.0).abs() < 1e-12);
}

#[test]
fn gate_fails_loudly_on_regression_and_reports_the_worst_query() {
    let outcome = protocol::evaluate_gate(vec![
        GateCheck {
            label: "rare-term".into(),
            p95_ms: 3.3,
            budget_ms: 40.0,
        },
        GateCheck {
            label: "frequent-term".into(),
            p95_ms: 26.8,
            budget_ms: 300.0,
        },
        GateCheck {
            label: "browse-newest".into(),
            p95_ms: 61.0, // at/over its 60 ms budget -> regression
            budget_ms: 60.0,
        },
    ]);
    assert!(!outcome.passed);
    assert_eq!(outcome.worst.label, "browse-newest");
    assert!(outcome.worst.ratio() > 1.0);
    // The rule is strict: p95 == budget is a failure, not a pass.
    let boundary = GateCheck {
        label: "exact".into(),
        p95_ms: 10.0,
        budget_ms: 10.0,
    };
    assert!(!boundary.within());
}

// --- baseline parsing ---------------------------------------------------------------

#[test]
fn sha256_matches_published_test_vectors() {
    assert_eq!(
        protocol::sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        protocol::sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn baseline_roundtrips_through_a_file_and_answers_budget_lookups() {
    let mut budgets = BTreeMap::new();
    budgets.insert("rare-term".to_string(), 40.0);
    budgets.insert("frequent-term".to_string(), 300.0);
    let path = write_baseline(&test_methodology(), budgets);

    let loaded = Baseline::load(&path).unwrap();
    assert_eq!(loaded.budget_for("rare-term").unwrap(), 40.0);
    assert_eq!(loaded.budget_for("frequent-term").unwrap(), 300.0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn baseline_hash_mismatch_fails_loudly() {
    let mut budgets = BTreeMap::new();
    budgets.insert("rare-term".to_string(), 40.0);
    let path = write_baseline(&test_methodology(), budgets);

    // Tamper: same recorded hash, different methodology underneath.
    let raw = std::fs::read_to_string(&path).unwrap();
    let tampered = raw.replace("\"warmup\": 5", "\"warmup\": 6");
    assert_ne!(raw, tampered, "tampering must change the file");
    let err = Baseline::from_slice(tampered.as_bytes()).unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("hash mismatch"),
        "error must name the problem, got: {message}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn baseline_missing_or_stale_entries_fail_loudly() {
    let mut budgets = BTreeMap::new();
    budgets.insert("rare-term".to_string(), 40.0);
    let baseline = Baseline {
        methodology: test_methodology(),
        methodology_sha256: Baseline::methodology_hash(&test_methodology()),
        budgets_ms: budgets,
    };

    // Missing: the suite asks for a label the file does not budget.
    let err = baseline.budget_for("browse-newest").unwrap_err();
    assert!(
        err.to_string()
            .contains("no p95 budget for query \"browse-newest\""),
        "got: {err}"
    );

    // Both directions surface in validate_labels: missing + stale extra.
    let mut stale = baseline.budgets_ms.clone();
    stale.insert("ghost-query".to_string(), 1.0);
    let with_stale = Baseline {
        methodology: baseline.methodology.clone(),
        methodology_sha256: baseline.methodology_sha256.clone(),
        budgets_ms: stale,
    };
    let errors = with_stale
        .validate_labels(["rare-term", "frequent-term"])
        .unwrap_err();
    // Both directions: "frequent-term" is unbudgeted, "ghost-query" is stale.
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(matches!(
        errors[0],
        protocol::ProtocolError::MissingBudget(_)
    ));
    assert!(matches!(
        errors[1],
        protocol::ProtocolError::UnknownBudget(_)
    ));
}

#[test]
fn committed_baseline_loads_and_covers_the_query_suite() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("baseline.json");
    let baseline = Baseline::load(&path).expect("committed baseline.json must load");
    let labels: Vec<&str> = QuerySpec::suite().iter().map(|s| s.label).collect();
    baseline
        .validate_labels(labels)
        .expect("committed baseline must budget exactly the suite");
    // Gate budgets stay positive and finite.
    for (label, budget) in &baseline.budgets_ms {
        assert!(
            *budget > 0.0 && budget.is_finite(),
            "budget for {label} must be positive and finite"
        );
    }
}

// --- generator determinism ------------------------------------------------------

/// Hash of every message a fresh in-memory storage holds: browse pages
/// (newest first) serialized to JSON, concatenated, SHA-256'd. Avoids raw
/// database bytes (filesystem noise) while covering content, ordering,
/// and the FTS-visible fields the benchmark queries key off.
fn archive_digest(storage: &Storage) -> String {
    let mut buffer = Vec::new();
    let mut offset = 0u32;
    loop {
        let query = SearchQuery {
            limit: 200,
            offset,
            ..Default::default()
        };
        let page = storage.search_page(&query).unwrap();
        for hit in &page.hits {
            buffer
                .extend_from_slice(&serde_json::to_vec(&hit.message).expect("message serializes"));
        }
        offset += page.hits.len() as u32;
        if page.hits.is_empty() || offset as u64 >= page.total {
            break;
        }
    }
    protocol::sha256_hex(&buffer)
}

#[test]
fn same_seed_generates_identical_archives() {
    let count = 600;
    let a = Storage::open_in_memory().unwrap();
    let b = Storage::open_in_memory().unwrap();
    let ra = fixture::generate(&a, count, 7, &|_| {}).unwrap();
    let rb = fixture::generate(&b, count, 7, &|_| {}).unwrap();
    assert_eq!(ra.inserted, count);
    assert_eq!(rb.inserted, count);

    let (digest_a, digest_b) = (archive_digest(&a), archive_digest(&b));
    assert_eq!(
        digest_a, digest_b,
        "same (count, seed) must produce byte-identical content"
    );

    // And a different seed must not collide with the same digest.
    let c = Storage::open_in_memory().unwrap();
    fixture::generate(&c, count, 8, &|_| {}).unwrap();
    assert_ne!(digest_a, archive_digest(&c));
}

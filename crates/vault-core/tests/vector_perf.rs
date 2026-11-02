//! Performance contract for the local vector layer over 100k messages:
//! the incremental embedding job, the cosine scan, and a full hybrid ask
//! stay within their budgets. (The FTS < 100 ms contract lives in
//! `search_perf.rs`; the vector budgets are separate and documented in the
//! README's "Local embeddings & hybrid search" section — a 128-dim cosine
//! scan touches every embedding by design, so it runs at scan speed rather
//! than index speed.)

use std::time::Instant;

use chrono::NaiveDate;
use vault_core::llm::ask;
use vault_core::message::NewMessage;
use vault_core::storage::Storage;
use vault_core::vector::{run_embedding_job, Embedder, HashEmbedder};

const TARGET_ROWS: usize = 100_000;
/// First-run embedding of the whole archive (hash embedder, batched).
const JOB_BUDGET_MS: u128 = 15_000;
/// One cosine scan over 100k embeddings (top-50).
const SCAN_BUDGET_MS: u128 = 200;
/// One full local ask (FTS leg + vector leg + fusion + hydration).
const HYBRID_BUDGET_MS: u128 = 250;
/// Re-run after importing a few hundred more messages.
const INCREMENTAL_BUDGET_MS: u128 = 1_000;
const RUNS: usize = 5;

/// Tiny deterministic PRNG (same as search_perf.rs, no `rand` dependency).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n
    }
}

const VOCAB: [&str; 24] = [
    "vault",
    "sync",
    "release",
    "deploy",
    "dashboard",
    "budget",
    "meeting",
    "tomorrow",
    "attached",
    "review",
    "approve",
    "keyboard",
    "highlight",
    "filter",
    "author",
    "thread",
    "chat",
    "email",
    "lunch",
    "invoice",
    "receipt",
    "taxes",
    "flight",
    "server",
];

fn fixture() -> Storage {
    let s = Storage::open_in_memory().expect("open storage");
    let mut rng = Lcg(0x7665_6374_6f72); // "vector"
    let mut msgs = Vec::with_capacity(TARGET_ROWS);
    let mut buf = String::with_capacity(400);
    for i in 0..TARGET_ROWS {
        let day = 1 + rng.below(28);
        let month = 1 + rng.below(12);
        let sent_at = format!(
            "2026-{month:02}-{day:02}T{:02}:{:02}:00Z",
            rng.below(24),
            rng.below(60)
        );
        buf.clear();
        let words = 24 + rng.below(16);
        for w in 0..words {
            if w == 5 && i % 100 == 0 {
                buf.push_str("quarterly vault report ");
            } else {
                buf.push_str(VOCAB[rng.below(VOCAB.len())]);
                buf.push(' ');
            }
        }
        msgs.push(NewMessage::new(
            "perf",
            format!("gen-{i}"),
            "fixture",
            sent_at,
            buf.trim(),
        ));
    }
    for m in &msgs {
        s.insert_message(m).expect("seed insert");
    }
    assert_eq!(s.stats().unwrap().total as usize, TARGET_ROWS);
    s
}

fn best_of<F: FnMut()>(label: &str, mut f: F) -> u128 {
    f(); // warm up (page cache, statement preparation)
    let mut best = u128::MAX;
    for _ in 0..RUNS {
        let t = Instant::now();
        f();
        best = best.min(t.elapsed().as_millis());
    }
    eprintln!("{label}: best of {RUNS} = {best} ms");
    best
}

#[test]
fn vector_layer_within_budget_over_100k_messages() {
    let s = fixture();
    let e = HashEmbedder::new();

    // 1) First full embedding pass.
    let job_ms = {
        let t = Instant::now();
        let summary = run_embedding_job(&s, &e, None).expect("embedding job");
        let elapsed = t.elapsed().as_millis();
        assert_eq!(summary.embedded as usize, TARGET_ROWS);
        assert_eq!(summary.remaining, 0);
        eprintln!("embedding job (once) = {elapsed} ms (budget {JOB_BUDGET_MS} ms)");
        assert!(elapsed < JOB_BUDGET_MS, "job took {elapsed} ms");
        elapsed
    };
    assert!(job_ms > 0);

    // 2) Plain cosine scan, top 50.
    let query = e.embed("quarterly vault report review");
    let scan_ms = best_of("vector scan top-50", || {
        let hits = s
            .vector_scan(e.id(), e.dim(), &query, None, None, 50)
            .expect("scan");
        assert_eq!(hits.len(), 50, "the fixture is dense enough to fill a page");
    });
    assert!(
        scan_ms < SCAN_BUDGET_MS,
        "scan best of {RUNS} was {scan_ms} ms, budget {SCAN_BUDGET_MS} ms"
    );

    // 3) Full local hybrid ask (no LLM configured → no network).
    let today = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
    let hybrid_ms = best_of("hybrid ask (local)", || {
        let outcome = ask(&s, None, &e, "quarterly vault report", 50, 8, today).expect("ask");
        assert!(!outcome.results.is_empty());
    });
    assert!(
        hybrid_ms < HYBRID_BUDGET_MS,
        "hybrid ask best of {RUNS} was {hybrid_ms} ms, budget {HYBRID_BUDGET_MS} ms"
    );

    // 4) Incrementality at scale: a few hundred new messages embed fast.
    for i in 0..500 {
        s.insert_message(&NewMessage::new(
            "perf",
            format!("extra-{i}"),
            "fixture",
            "2026-11-01T00:00:00Z",
            "fresh vault sync update message",
        ))
        .expect("insert extra");
    }
    let t = Instant::now();
    let summary = run_embedding_job(&s, &e, None).expect("incremental job");
    let incremental_ms = t.elapsed().as_millis();
    assert_eq!(summary.embedded, 500, "only the new messages are embedded");
    eprintln!(
        "incremental job (500 new) = {incremental_ms} ms (budget {INCREMENTAL_BUDGET_MS} ms)"
    );
    assert!(
        incremental_ms < INCREMENTAL_BUDGET_MS,
        "incremental job took {incremental_ms} ms"
    );
}

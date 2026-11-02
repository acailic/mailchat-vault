//! Performance contract: the dashboard's aggregate query answers in
//! < 100 ms over 1M messages.
//!
//! Generates a deterministic 1M-message fixture (4 sources, ~600 chats,
//! ~190 authors, ~9 years of days, a small share of unknown timestamps and
//! chat-less mails — the realistic shapes the aggregates must exclude) and
//! times `Storage::archive_stats` end to end: total, date range, per-source
//! counts, the day histogram, and the top chats/authors, all in one call.
//! Timing uses the best of several runs to absorb scheduler noise.
//!
//! The bodies are deliberately slim: aggregate speed depends on the index
//! entries, not on text size, and slim bodies keep the fixture (built
//! through the product's idempotent insert path, FTS triggers included)
//! inside a normal test-run budget.

use std::time::Instant;

use chrono::{Days, NaiveDate};
use vault_core::message::NewMessage;
use vault_core::storage::Storage;

const TARGET_ROWS: usize = 1_000_000;
/// Budget from the spec: dashboard aggregates over 1M messages.
const BUDGET_MS: u128 = 100;
const RUNS: usize = 5;

/// Tiny deterministic PRNG (no `rand` dependency):
/// x = x*6364136223846793005 + 1442695040888963407.
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

const SOURCES: [&str; 4] = ["mbox", "whatsapp", "telegram", "imap:user@example.com"];

fn fixture() -> Storage {
    let started = Instant::now();
    let s = Storage::open_in_memory().expect("open storage");
    let mut rng = Lcg(0x7374_6174_7331_4d00); // "stats1M"
                                              // 2018-01-01 + 0..=3191 days ≈ 2018-01-01 .. 2026-11-01.
    let base = NaiveDate::from_ymd_opt(2018, 1, 1).expect("base date");
    let mut msgs = Vec::with_capacity(TARGET_ROWS);
    for i in 0..TARGET_ROWS {
        let source = SOURCES[rng.below(SOURCES.len())];
        // ~1% of messages have an unknown timestamp, ~5% no chat id.
        let sent_at = if rng.below(100) == 0 {
            String::new()
        } else {
            let day = base
                .checked_add_days(Days::new(rng.below(3192) as u64))
                .unwrap_or(base);
            format!(
                "{day}T{:02}:{:02}:{:02}Z",
                rng.below(24),
                rng.below(60),
                rng.below(60)
            )
        };
        let chat_id = if rng.below(20) == 0 {
            String::new()
        } else {
            format!("chat-{:04}", rng.below(600))
        };
        msgs.push(NewMessage {
            source: source.into(),
            external_id: format!("gen-{i}"),
            channel: if source == "mbox" || source.starts_with("imap:") {
                "email"
            } else {
                "chat"
            }
            .into(),
            chat_id,
            author: format!("author-{:03}", rng.below(190)),
            sent_at,
            body: format!("slim body {i}"),
            raw_json: "{}".into(),
        });
    }
    for m in &msgs {
        s.insert_message(m).expect("seed insert");
    }
    assert_eq!(s.stats().unwrap().total as usize, TARGET_ROWS);
    eprintln!(
        "fixture: {TARGET_ROWS} slim rows in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    s
}

#[test]
fn dashboard_stats_under_100ms_over_1m_messages() {
    let s = fixture();

    // Correctness spot-checks before timing: the aggregates saw every row
    // and excluded exactly the undated ones.
    let stats = s.archive_stats(10).unwrap();
    assert_eq!(stats.total as usize, TARGET_ROWS);
    assert!(
        stats.first_sent_at.as_deref().unwrap_or("") < stats.last_sent_at.as_deref().unwrap_or("")
    );
    // The day histogram must cover the ~99% dated rows across the full
    // ~9-year span (every one of the ~3200 distinct days appears).
    let dated: u64 = stats.by_day.iter().map(|d| d.count).sum();
    assert!(
        (TARGET_ROWS as u64 - 15_000..=TARGET_ROWS as u64).contains(&dated),
        "day histogram must cover the ~99% dated rows, got {dated}"
    );
    assert_eq!(
        stats.by_day.len(),
        3192,
        "every distinct fixture day must appear exactly once"
    );
    assert_eq!(
        stats.by_source.iter().map(|s| s.count).sum::<u64>(),
        stats.total
    );
    assert!(stats.top_chats.iter().all(|c| !c.chat_id.is_empty()));
    assert_eq!(stats.top_authors.len(), 10);

    // Best-of-RUNS timing of the whole dashboard data path.
    let mut best = u128::MAX;
    for _ in 0..RUNS {
        let t = Instant::now();
        let out = s.archive_stats(10).unwrap();
        assert_eq!(out.total as usize, TARGET_ROWS);
        best = best.min(t.elapsed().as_millis());
    }
    eprintln!("archive_stats(10): best of {RUNS} = {best} ms (budget {BUDGET_MS} ms)");
    assert!(
        best < BUDGET_MS,
        "archive_stats: best of {RUNS} runs was {best} ms over {TARGET_ROWS} messages, budget {BUDGET_MS} ms"
    );

    // The plain per-source stats call stays inside the same budget.
    let mut best = u128::MAX;
    for _ in 0..RUNS {
        let t = Instant::now();
        let out = s.stats().unwrap();
        assert_eq!(out.total as usize, TARGET_ROWS);
        best = best.min(t.elapsed().as_millis());
    }
    eprintln!("stats(): best of {RUNS} = {best} ms (budget {BUDGET_MS} ms)");
    assert!(
        best < BUDGET_MS,
        "stats: best of {RUNS} runs was {best} ms, budget {BUDGET_MS} ms"
    );
}

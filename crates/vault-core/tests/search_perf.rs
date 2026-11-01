//! Performance contract: search answers in < 100 ms over 100k messages.
//!
//! Generates a deterministic 100k-message fixture (sources, chats, authors,
//! dates, vocabulary) and times representative queries — plain FTS, FTS with
//! every filter, a filtered browse, and a deep pagination page — asserting
//! each answers within the budget. Timing uses the best of several runs to
//! absorb scheduler noise on loaded CI machines.

use std::time::Instant;

use vault_core::message::NewMessage;
use vault_core::query::SearchQuery;
use vault_core::storage::Storage;

const TARGET_ROWS: usize = 100_000;
/// Budget from the spec: upper bound per query over 100k messages.
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

const SOURCES: [&str; 4] = ["email", "whatsapp", "telegram", "mbox"];
const AUTHORS: [&str; 12] = [
    "Ana Sender <ana@corp.example>",
    "Marko Marković",
    "Jelena J.",
    "mom",
    "dad",
    "nina_dev",
    "ognjen",
    "Petra Petrović",
    "vault-bot",
    "Ivan Ivić",
    "Sara S.",
    "Luka L.",
];
// Background vocabulary: everyday words spread across many messages. The
// probe terms ("quarterly", "vault", "report") are intentionally NOT here so
// the timed text queries stay selective, like a realistic search.
const VOCAB: [&str; 30] = [
    "message",
    "archive",
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
    "source",
    "author",
    "date",
    "thread",
    "chat",
    "email",
    "phone",
    "lunch",
    "invoice",
    "receipt",
    "taxes",
    "flight",
    "hotel",
    "ticket",
    "server",
    "cluster",
];
/// Synthetic diluter tokens (`tok0`..`tok1999`) mixed into most bodies.
const FILLER_TOKENS: usize = 2_000;
/// Every Nth body carries the probe phrase → ~1k probe rows of 100k.
const PROBE_EVERY: usize = 100;

fn fixture() -> Storage {
    let s = Storage::open_in_memory().expect("open storage");
    let mut rng = Lcg(0x6d61_696c_6368_6174); // "mailchat"
    let mut msgs = Vec::with_capacity(TARGET_ROWS);
    let mut buf = String::with_capacity(400);
    for i in 0..TARGET_ROWS {
        let source = SOURCES[rng.below(SOURCES.len())];
        let author = AUTHORS[rng.below(AUTHORS.len())];
        let chat = format!("chat-{:04}", rng.below(600));
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
            if w == 5 && i % PROBE_EVERY == 0 {
                buf.push_str("quarterly vault report ");
            } else if rng.below(4) == 0 {
                buf.push_str(&format!("tok{} ", rng.below(FILLER_TOKENS)));
            } else {
                buf.push_str(VOCAB[rng.below(VOCAB.len())]);
                buf.push(' ');
            }
        }
        let m = NewMessage {
            source: source.into(),
            external_id: format!("gen-{i}"),
            channel: if source == "email" || source == "mbox" {
                "email"
            } else {
                "chat"
            }
            .into(),
            chat_id: chat,
            author: author.into(),
            sent_at,
            body: buf.trim().into(),
            raw_json: "{}".into(),
        };
        msgs.push(m);
    }
    for m in &msgs {
        s.insert_message(m).expect("seed insert");
    }
    assert_eq!(s.stats().unwrap().total as usize, TARGET_ROWS);
    s
}

fn best_of<F: FnMut()>(label: &str, mut f: F) {
    // Warm up (page cache, statement preparation) then keep the best run.
    f();
    let mut best = u128::MAX;
    for _ in 0..RUNS {
        let t = Instant::now();
        f();
        best = best.min(t.elapsed().as_millis());
    }
    eprintln!("{label}: best of {RUNS} = {best} ms (budget {BUDGET_MS} ms)");
    assert!(
        best < BUDGET_MS,
        "{label}: best of {RUNS} runs was {best} ms, budget {BUDGET_MS} ms"
    );
}

#[test]
fn search_under_100ms_over_100k_messages() {
    let s = fixture();

    best_of("plain fts search", || {
        let page = s
            .search_page(&SearchQuery {
                text: "quarterly vault".into(),
                limit: 50,
                ..SearchQuery::default()
            })
            .expect("search");
        assert!(!page.hits.is_empty());
        assert!(
            page.total > 0 && page.total < 5_000,
            "probe phrase must stay selective, got {}",
            page.total
        );
    });

    best_of("fts search with all filters", || {
        let page = s
            .search_page(&SearchQuery {
                text: "report".into(),
                sources: vec!["email".into(), "telegram".into()],
                author: "a".into(),
                from_date: "2026-03-01".into(),
                to_date: "2026-06-30".into(),
                limit: 50,
                ..SearchQuery::default()
            })
            .expect("search");
        assert!(page
            .hits
            .iter()
            .all(|h| h.message.source == "email" || h.message.source == "telegram"));
    });

    best_of("filtered browse (no text)", || {
        let page = s
            .search_page(&SearchQuery {
                sources: vec!["whatsapp".into()],
                from_date: "2026-01-01".into(),
                to_date: "2026-12-31".into(),
                limit: 50,
                ..SearchQuery::default()
            })
            .expect("browse");
        assert!(!page.hits.is_empty());
    });

    best_of("deep pagination page", || {
        let page = s
            .search_page(&SearchQuery {
                text: "vault".into(),
                limit: 50,
                offset: 500,
                ..SearchQuery::default()
            })
            .expect("search");
        assert_eq!(page.hits.len(), 50);
    });

    // Correctness spot-checks.
    let page = s
        .search_page(&SearchQuery {
            text: "vault".into(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    assert!(page.hits[0].snippet.is_some());
}

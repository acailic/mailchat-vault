//! Deterministic fixture generation for benchmarks.
//!
//! The generator synthesizes a realistic mail/chat archive: a mix of email
//! (long bodies, business hours, weekday skew) and chat messages (short
//! bodies, evening skew), a Zipf-distributed pseudo-language so term
//! selectivity behaves like real text, author pools, and `sent_at` spread
//! over years.
//!
//! Everything derives from one seed through a splitmix64 PRNG, so the same
//! `(count, seed)` produces an identical fixture on every machine. Word
//! ranks are stable, which is what the benchmark queries key off: searching
//! the rank-0 word hits nearly every message (stopword-like), rank-100 is
//! frequent, rank-2000 is rare.

use chrono::{Datelike, Days, NaiveDate};
use vault_core::message::{InsertOutcome, NewMessage};
use vault_core::storage::Storage;

/// Distinct words in the synthetic vocabulary.
pub const VOCAB_SIZE: usize = 4096;
/// Zipf exponent: larger = heavier skew towards the top-ranked words.
const ZIPF_EXPONENT: f64 = 1.07;

/// Rank whose word appears in nearly every message (stopword-like).
pub const UBIQUITOUS_RANK: usize = 0;
/// Rank whose word appears in a few percent of messages.
pub const FREQUENT_RANK: usize = 100;
/// Rank whose word appears in well under one percent of messages.
pub const UNCOMMON_RANK: usize = 500;
/// Rank reserved for chat author handles (and the author-term query).
pub const AUTHOR_HANDLE_RANK: usize = 600;
/// Rank whose word appears in only a handful of messages per 100k.
pub const RARE_RANK: usize = 2000;

/// Inclusive date range the date-filtered benchmark query restricts to;
/// inside both the email and chat date spans.
pub const DATE_FILTER_FROM: &str = "2022-01-01";
pub const DATE_FILTER_TO: &str = "2022-12-31";

/// Source-name of the synthesized email stream.
pub const MBOX_SOURCE: &str = "mbox";
/// Source-name of the synthesized WhatsApp-style chat stream.
pub const WHATSAPP_SOURCE: &str = "whatsapp";
/// Source-name of the synthesized Telegram-style chat stream.
pub const TELEGRAM_SOURCE: &str = "telegram";

// ---------------------------------------------------------------------------
// Deterministic PRNG (splitmix64): tiny, seedable, identical everywhere.
// ---------------------------------------------------------------------------

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0xA076_1D64_78BD_642F)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform float in `[0, 1)`: the PRNG's top 53 bits scaled by 2^-53.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / 9_007_199_254_740_992.0
    }

    /// Uniform integer in `[0, bound)`.
    pub fn below(&mut self, bound: u64) -> u64 {
        debug_assert!(bound > 0);
        // Rejection sampling keeps the distribution flat for any bound.
        let limit = u64::MAX - (u64::MAX % bound);
        loop {
            let v = self.next_u64();
            if v < limit {
                return v % bound;
            }
        }
    }

    /// Pick an index weighted by `weights` (need not be normalized).
    pub fn weighted(&mut self, weights: &[f64]) -> usize {
        let total: f64 = weights.iter().sum();
        debug_assert!(total > 0.0);
        let mut x = self.next_f64() * total;
        for (i, w) in weights.iter().enumerate() {
            x -= w;
            if x < 0.0 {
                return i;
            }
        }
        weights.len() - 1
    }
}

// ---------------------------------------------------------------------------
// Vocabulary: unique pseudo-words built from syllables, Zipf-distributed.
// ---------------------------------------------------------------------------

const SYLLABLES: [&str; 24] = [
    "an", "bel", "cor", "dan", "el", "fen", "gar", "hal", "il", "jor", "kal", "lem", "mor", "nal",
    "ol", "par", "quel", "ras", "sil", "tor", "ul", "ven", "wol", "yar",
];

/// Word at vocabulary `rank`; injective for rank < 24³.
pub fn word_at(rank: usize) -> String {
    let r = rank as u64;
    format!(
        "{}{}{}",
        SYLLABLES[(r % 24) as usize],
        SYLLABLES[((r / 24) % 24) as usize],
        SYLLABLES[((r / 576) % 24) as usize],
    )
}

pub struct Vocab {
    cdf: Vec<f64>,
}

impl Vocab {
    pub fn new() -> Self {
        let raw: Vec<f64> = (1..=VOCAB_SIZE)
            .map(|rank| 1.0 / (rank as f64).powf(ZIPF_EXPONENT))
            .collect();
        let total: f64 = raw.iter().sum();
        let mut cdf = Vec::with_capacity(raw.len());
        let mut acc = 0.0;
        for w in raw {
            acc += w / total;
            cdf.push(acc);
        }
        Vocab { cdf }
    }

    /// Sample a word rank from the Zipf distribution.
    pub fn sample_rank(&self, rng: &mut Rng) -> usize {
        let x = rng.next_f64();
        match self
            .cdf
            .binary_search_by(|probe| probe.partial_cmp(&x).unwrap())
        {
            Ok(i) => i,
            Err(i) => i.min(VOCAB_SIZE - 1),
        }
    }

    pub fn sample_word(&self, rng: &mut Rng) -> String {
        word_at(self.sample_rank(rng))
    }
}

impl Default for Vocab {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Message synthesis.
// ---------------------------------------------------------------------------

/// Word-count buckets (share, min_words, max_words): most chat messages are
/// tiny, most emails are a few paragraphs, a few emails are very long.
const CHAT_BUCKETS: [(f64, u32, u32); 3] = [(0.70, 1, 10), (0.25, 11, 40), (0.05, 41, 120)];
const EMAIL_BUCKETS: [(f64, u32, u32); 4] = [
    (0.55, 20, 80),
    (0.30, 81, 250),
    (0.12, 251, 700),
    (0.03, 701, 2000),
];
/// Subject-line word counts for emails.
const SUBJECT_BUCKETS: [(f64, u32, u32); 1] = [(1.0, 4, 9)];

/// Per-hour-of-day message weights (email: business hours; chat: evenings).
const EMAIL_HOURS: [f64; 24] = [
    1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, 4.0, 8.0, 12.0, 14.0, 14.0, 14.0, 12.0, 14.0, 14.0, 12.0,
    8.0, 4.0, 2.0, 2.0, 1.0, 1.0, 1.0,
];
const CHAT_HOURS: [f64; 24] = [
    2.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0,
    12.0, 12.0, 10.0, 8.0, 6.0, 4.0,
];
/// Mon..Sun weights (email: workweek; chat: weekends), max weight 14.
const EMAIL_WEEKDAYS: [f64; 7] = [10.0, 14.0, 14.0, 14.0, 13.0, 10.0, 4.0];
const CHAT_WEEKDAYS: [f64; 7] = [8.0, 8.0, 9.0, 9.0, 10.0, 12.0, 12.0];

const AUTHOR_COUNT: usize = 192;

struct Authors {
    /// Chat handles: each a single mid-rank vocabulary word.
    handles: Vec<String>,
    /// Email identities `Name <handle@domain>`.
    addresses: Vec<String>,
}

fn authors(vocab: &Vocab) -> Authors {
    let mut pick = Rng::new(0xC0FFEE);
    let mut name = Rng::new(0xBADF00D);
    let handles = (0..AUTHOR_COUNT)
        .map(|i| word_at(AUTHOR_HANDLE_RANK + i))
        .collect();
    let addresses = (0..AUTHOR_COUNT)
        .map(|_| {
            // Email display names use rare ranks (beyond every query rank)
            // so they stay out of benchmark selectivity.
            let first = capitalized_rare(&mut name);
            let last = capitalized_rare(&mut name);
            let handle = vocab.sample_word(&mut pick);
            let domain = vocab.sample_word(&mut pick);
            format!("{first} {last} <{handle}@{domain}.example>")
        })
        .collect();
    Authors { handles, addresses }
}

fn capitalized_rare(rng: &mut Rng) -> String {
    // Ranks 2500..=4095: never used by benchmark queries.
    let word = word_at(2500 + rng.below(1596) as usize);
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => word,
    }
}

/// Append `words`-worth of pseudo-prose (sentences of 6..=14 words) to
/// `out`, sampled from the bucket table. Returns the word count written.
fn write_words(rng: &mut Rng, vocab: &Vocab, buckets: &[(f64, u32, u32)], out: &mut String) -> u32 {
    let shares: Vec<f64> = buckets.iter().map(|b| b.0).collect();
    let (min, max) = {
        let b = &buckets[rng.weighted(&shares)];
        (b.1.min(b.2), b.1.max(b.2))
    };
    let words = min + rng.below((max - min + 1) as u64) as u32;
    let mut remaining = words;
    while remaining > 0 {
        let take = remaining.min(6 + rng.below(9) as u32);
        let joined = (0..take)
            .map(|_| vocab.sample_word(rng))
            .collect::<Vec<_>>()
            .join(" ");
        let mut chars = joined.chars();
        if let Some(first) = chars.next() {
            out.push_str(&first.to_uppercase().to_string());
            out.push_str(chars.as_str());
        }
        out.push(match rng.below(10) {
            0 => '?',
            1 => '!',
            _ => '.',
        });
        remaining -= take;
        if remaining > 0 {
            out.push(' ');
        }
    }
    words
}

/// A `sent_at` timestamp: a day uniform in `[base, base+span_days)`,
/// rejected proportionally to weekday weights, plus a weighted hour.
fn sent_at(
    rng: &mut Rng,
    base: NaiveDate,
    span_days: u32,
    hours: &[f64; 24],
    weekdays: &[f64; 7],
) -> String {
    loop {
        let day = base
            .checked_add_days(Days::new(rng.below(span_days as u64)))
            .unwrap_or(base);
        let weekday = day.weekday().num_days_from_monday() as usize;
        if rng.next_f64() * 14.0 < weekdays[weekday] {
            let hour = rng.weighted(hours) as u32;
            let minute = rng.below(60) as u32;
            let second = rng.below(60) as u32;
            return format!("{day}T{hour:02}:{minute:02}:{second:02}Z");
        }
    }
}

// ---------------------------------------------------------------------------
// Concurrent-ingest stream (M2.4 harness)
// ---------------------------------------------------------------------------

/// Source name of the concurrent-ingest phase; its rows are removed
/// after the measurement so a reused fixture stays pristine.
pub const BENCH_INGEST_SOURCE: &str = "bench-concurrent";

/// A bounded-import stream of chat-shaped messages for the
/// concurrent-ingest measurement: driven through the product's own
/// `BoundedRun` + `PoolSink` path (batched commits through the pool's
/// write lane), exactly like a production chat-export import.
pub struct IngestSource {
    count: u64,
    seed: u64,
}

impl IngestSource {
    pub fn new(count: u64, seed: u64) -> Self {
        Self { count, seed }
    }
}

impl vault_core::bounded::StreamSource for IngestSource {
    fn name(&self) -> &str {
        BENCH_INGEST_SOURCE
    }

    fn stream(
        &self,
        skip: u64,
        emit: &mut dyn FnMut(
            vault_core::bounded::StreamItem,
        ) -> vault_core::Result<vault_core::bounded::Flow>,
    ) -> vault_core::Result<()> {
        let mut rng = Rng::new(self.seed);
        let vocab = Vocab::new();
        for i in 0..self.count {
            if i < skip {
                continue;
            }
            // Chat-shaped bodies (3–40 words from the same Zipf
            // vocabulary the fixture uses) with a unique external id;
            // sent_at rides the current day so ingest-order and
            // recency do not matter for the measurement.
            let words = 3 + rng.below(38) as usize;
            let body: Vec<String> = (0..words).map(|_| vocab.sample_word(&mut rng)).collect();
            let msg = NewMessage {
                source: BENCH_INGEST_SOURCE.into(),
                external_id: format!("bench-{i:09}"),
                channel: "chat".into(),
                chat_id: format!("bench-chat-{:05}", i / 250),
                author: format!("bench-author-{:03}", rng.below(192)),
                sent_at: format!(
                    "2026-11-{:02}T{:02}:{:02}:00Z",
                    1 + rng.below(28) as u32,
                    rng.below(24) as u32,
                    rng.below(60) as u32
                ),
                body: body.join(" "),
                raw_json: "{}".into(),
                conversation_key: String::new(),
                reply_refs: Vec::new(),
                original_timestamp: String::new(),
                timezone: String::new(),
                time_confidence: String::new(),
                // Post-M2.4 NewMessage fields; the bench fixture keeps
                // every one at its empty/None default so the generated
                // bytes (and thus every recorded baseline) are unchanged.
                attachments: Vec::new(),
                source_label: String::new(),
                original_bytes: None,
                original_ref: None,
            };
            if emit(vault_core::bounded::StreamItem::Message(msg))?
                == vault_core::bounded::Flow::Stop
            {
                return Ok(());
            }
        }
        Ok(())
    }
}

/// Outcome of a fixture generation pass.
pub struct FixtureReport {
    pub requested: u64,
    pub inserted: u64,
    pub secs: f64,
}

/// Generate `count` messages into `storage`, deterministically from `seed`.
///
/// The insert path is the product's own `Storage::insert_message` (one
/// idempotent insert per message, FTS triggers firing), so fixture build
/// speed equals real ingest speed — a measured side result, not a shortcut.
pub fn generate(
    storage: &Storage,
    count: u64,
    seed: u64,
    progress: &dyn Fn(u64),
) -> vault_core::Result<FixtureReport> {
    let started = std::time::Instant::now();
    let mut rng = Rng::new(seed);
    let vocab = Vocab::new();
    let authors = authors(&vocab);
    let mbox_base = NaiveDate::from_ymd_opt(2018, 1, 8).expect("valid base date");
    let chat_base = NaiveDate::from_ymd_opt(2020, 2, 1).expect("valid base date");
    let mut inserted = 0u64;

    for i in 0..count {
        let roll = rng.next_f64();
        let msg = if roll < 0.45 {
            // Email: subject line, long body, thread grouping.
            let author = &authors.addresses[rng.below(AUTHOR_COUNT as u64) as usize];
            let sent = sent_at(&mut rng, mbox_base, 3200, &EMAIL_HOURS, &EMAIL_WEEKDAYS);
            let mut body = String::new();
            let subject_words = write_words(&mut rng, &vocab, &SUBJECT_BUCKETS, &mut body);
            body.push_str("\n\n");
            let _ = write_words(&mut rng, &vocab, &EMAIL_BUCKETS, &mut body);
            NewMessage {
                source: MBOX_SOURCE.into(),
                external_id: format!("m-{i:09}"),
                channel: "email".into(),
                chat_id: format!("thread-{:05}", i / 40),
                author: author.clone(),
                sent_at: sent,
                body,
                raw_json: format!(
                    "{{\"subject_words\":{subject_words},\"mailbox\":\"{}\"}}",
                    if rng.below(4) == 0 {
                        "Archive"
                    } else {
                        "Inbox"
                    }
                ),
                conversation_key: String::new(),
                reply_refs: Vec::new(),
                original_timestamp: String::new(),
                timezone: String::new(),
                time_confidence: String::new(),
                // Post-M2.4 NewMessage fields; the bench fixture keeps
                // every one at its empty/None default so the generated
                // bytes (and thus every recorded baseline) are unchanged.
                attachments: Vec::new(),
                source_label: String::new(),
                original_bytes: None,
                original_ref: None,
            }
        } else {
            // Chat (WhatsApp/Telegram): short body, session grouping.
            let source = if roll < 0.75 {
                WHATSAPP_SOURCE
            } else {
                TELEGRAM_SOURCE
            };
            let author = &authors.handles[rng.below(AUTHOR_COUNT as u64) as usize];
            let sent = sent_at(&mut rng, chat_base, 2400, &CHAT_HOURS, &CHAT_WEEKDAYS);
            let mut body = String::new();
            let _ = write_words(&mut rng, &vocab, &CHAT_BUCKETS, &mut body);
            NewMessage {
                source: source.into(),
                external_id: format!(
                    "{}-{i:09}",
                    if source == WHATSAPP_SOURCE {
                        "wa"
                    } else {
                        "tg"
                    }
                ),
                channel: "chat".into(),
                chat_id: format!("chat-{:05}", i / 300),
                author: author.clone(),
                sent_at: sent,
                body,
                raw_json: "{}".into(),
                conversation_key: String::new(),
                reply_refs: Vec::new(),
                original_timestamp: String::new(),
                timezone: String::new(),
                time_confidence: String::new(),
                // Post-M2.4 NewMessage fields; the bench fixture keeps
                // every one at its empty/None default so the generated
                // bytes (and thus every recorded baseline) are unchanged.
                attachments: Vec::new(),
                source_label: String::new(),
                original_bytes: None,
                original_ref: None,
            }
        };
        if matches!(storage.insert_message(&msg)?, InsertOutcome::Inserted(_)) {
            inserted += 1;
        }
        if i % 25_000 == 24_999 {
            progress(i + 1);
        }
    }
    Ok(FixtureReport {
        requested: count,
        inserted,
        secs: started.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn rng_is_deterministic_and_in_range() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut rng = Rng::new(1);
        for _ in 0..1000 {
            assert!(rng.below(97) < 97);
            assert!((0.0..1.0).contains(&rng.next_f64()));
        }
    }

    #[test]
    fn vocabulary_words_are_unique() {
        let seen: HashSet<String> = (0..VOCAB_SIZE).map(word_at).collect();
        assert_eq!(seen.len(), VOCAB_SIZE);
    }

    #[test]
    fn zipf_sampling_is_heavily_skewed() {
        let vocab = Vocab::new();
        let mut rng = Rng::new(3);
        let mut top8 = 0usize;
        let n = 10_000;
        for _ in 0..n {
            if vocab.sample_rank(&mut rng) < 8 {
                top8 += 1;
            }
        }
        // Top-8 ranks hold roughly a third of the mass with exponent 1.07.
        assert!(top8 > n / 4, "top8 = {top8} of {n}");
    }

    #[test]
    fn query_ranks_yield_distinct_words() {
        let ranks = [
            UBIQUITOUS_RANK,
            FREQUENT_RANK,
            UNCOMMON_RANK,
            AUTHOR_HANDLE_RANK,
            RARE_RANK,
        ];
        let words: HashSet<String> = ranks.iter().map(|r| word_at(*r)).collect();
        assert_eq!(words.len(), ranks.len());
    }

    #[test]
    fn bodies_respect_word_budgets() {
        let mut rng = Rng::new(11);
        let vocab = Vocab::new();
        for _ in 0..200 {
            let mut out = String::new();
            let words = write_words(&mut rng, &vocab, &CHAT_BUCKETS, &mut out);
            assert!((1..=120).contains(&words));
            assert!(!out.is_empty());
        }
        let mut longest = 0;
        for _ in 0..500 {
            let mut out = String::new();
            longest = longest.max(write_words(&mut rng, &vocab, &EMAIL_BUCKETS, &mut out));
        }
        // The rare very-long bucket (701..=2000 words) is reachable and the
        // cap holds; exact 2000 needs the uniform max, so only bound it.
        assert!((1000..=2000).contains(&longest), "longest = {longest}");
    }

    #[test]
    fn generated_fixture_is_wellformed() {
        let storage = Storage::open_in_memory().unwrap();
        let report = generate(&storage, 2000, 42, &|_| {}).unwrap();
        assert_eq!(report.inserted, 2000);
        assert_eq!(storage.stats().unwrap().total, 2000);

        // Plausible source mix: ~45% email (+/- sampling noise).
        let stats = storage.stats().unwrap();
        let mbox = stats
            .by_source
            .iter()
            .find(|s| s.source == MBOX_SOURCE)
            .map(|s| s.count)
            .unwrap_or(0);
        assert!(
            (700..1300).contains(&mbox),
            "mbox share out of expected band: {mbox}"
        );

        // sent_at format: the full-range date filter must accept every
        // timestamp spelling (string comparison against RFC 3339 bounds).
        let hits = storage
            .search_filtered(
                &format!("\"{}\"", word_at(UBIQUITOUS_RANK)),
                Some("2018-01-01"),
                Some("2026-12-31"),
                None,
                false,
                2000,
            )
            .unwrap();
        assert!(!hits.is_empty(), "ubiquitous term must hit");
        for m in &hits {
            chrono::NaiveDateTime::parse_from_str(&m.sent_at, "%Y-%m-%dT%H:%M:%SZ")
                .unwrap_or_else(|e| panic!("bad sent_at {:?}: {e}", m.sent_at));
        }

        // Author-term search finds that author's chat messages (the word
        // can also appear inside sampled email addresses — that is fine).
        let author_hits = storage
            .search_filtered(
                &format!("\"{}\"", word_at(AUTHOR_HANDLE_RANK)),
                None,
                None,
                None,
                false,
                2000,
            )
            .unwrap();
        assert!(!author_hits.is_empty(), "author term must hit");
        assert!(author_hits
            .iter()
            .any(|m| m.channel == "chat" && m.author == word_at(AUTHOR_HANDLE_RANK)));
    }
}

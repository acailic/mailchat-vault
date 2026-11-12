//! Deterministic synthetic corpus for the retrieval benchmark.
//!
//! Builds a file-backed archive through the product's own
//! `Storage::insert_message` — no SQL of the app is duplicated — from:
//!
//! - **background noise**: everyday email and chat messages generated
//!   from a fixed vocabulary by a seeded LCG (same generator shape as
//!   `tests/perf`), spread over mbox / WhatsApp / Telegram sources in a
//!   45 / 30 / 25 mix, with chat-short and email-long bodies and
//!   Zipf-skewed word choice so term selectivity behaves like real text;
//! - **labeled targets and deliberate confusers**: the question set's
//!   correct originals and distractors, scattered at deterministic
//!   positions among the noise.
//!
//! The generator is fully deterministic: the same `count`/`seed` builds
//! the byte-identical corpus on every machine, so quality metrics
//! (recall, ranks) are reproducible; only latency varies by machine.
//!
//! The background vocabulary is deliberately disjoint from every token
//! the question set searches for, so the only lexical bridges to a
//! labeled original are the ones the questions design in.

use std::collections::HashMap;
use std::path::Path;

use vault_core::message::NewMessage;
use vault_core::storage::Storage;

use crate::questions::{ConfuserSpec, TargetSpec, QUESTIONS};

/// Tiny deterministic PRNG (no `rand` dependency), identical constants to
/// `tests/perf`: x = x*6364136223846793005 + 1442695040888963407.
pub struct Lcg(u64);

impl Lcg {
    pub fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n
    }
}

const EMAIL_AUTHORS: [&str; 6] = [
    "Ana Anić <ana.anic@corp.example>",
    "Boris B. <boris@corp.example>",
    "Cvijeta C. <cvijeta@corp.example>",
    "Darko D. <darko@corp.example>",
    "Elena E. <elena@corp.example>",
    "Filip F. <filip@corp.example>",
];
const WHATSAPP_AUTHORS: [&str; 6] = [
    "+381641111111",
    "+381642222222",
    "goran",
    "tanja",
    "vuk",
    "lola",
];
const TELEGRAM_AUTHORS: [&str; 6] = [
    "dev_channel",
    "news_bot",
    "martina",
    "oggi",
    "beograd_chat",
    "vlada",
];
const WHATSAPP_CHATS: [&str; 4] = ["family", "friends", "neighbours", "school"];
const TELEGRAM_CHATS: [&str; 4] = ["general", "news", "projects", "random"];

/// Everyday vocabulary for the background noise. Kept strictly disjoint
/// from every token the question set (queries, targets, confusers)
/// relies on — verified when the corpus is authored, and guarded at
/// runtime by the exact-term / no-answer self-checks.
const VOCAB: [&str; 99] = [
    // English-leaning pool.
    "meeting",
    "agenda",
    "minutes",
    "payment",
    "receipt",
    "budget",
    "report",
    "review",
    "release",
    "server",
    "backup",
    "login",
    "password",
    "newsletter",
    "subscription",
    "delivery",
    "package",
    "address",
    "phone",
    "reminder",
    "hotel",
    "booking",
    "lunch",
    "dinner",
    "coffee",
    "birthday",
    "weekend",
    "holiday",
    "school",
    "doctor",
    "dentist",
    "gym",
    "running",
    "bike",
    "insurance",
    "tax",
    "form",
    "project",
    "client",
    "deadline",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "saturday",
    "sunday",
    "morning",
    "evening",
    "night",
    "home",
    "work",
    "team",
    "update",
    "version",
    "issue",
    "thank",
    "please",
    "call",
    "message",
    "later",
    "soon",
    "ready",
    "done",
    "check",
    "number",
    "total",
    "invoice",
    "confirm",
    // Serbian-leaning pool.
    "sastanak",
    "faktura",
    "uplata",
    "izveštaj",
    "pregled",
    "rok",
    "klijent",
    "projekat",
    "kancelarija",
    "kuća",
    "auto",
    "servis",
    "osiguranje",
    "porez",
    "odmor",
    "vikend",
    "rođendan",
    "ručak",
    "večera",
    "škola",
    "doktor",
    "trening",
    "karta",
    "putovanje",
    "avion",
    "dostava",
    "paket",
    "adresa",
    "telefon",
    "podsetnik",
    "lozinka",
];

/// One generated background message kind (source mix wheel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceKind {
    pub source: &'static str,
    pub channel: &'static str,
}

fn source_kind(spin: usize) -> SourceKind {
    if spin < 9 {
        SourceKind {
            source: "mbox",
            channel: "email",
        }
    } else if spin < 15 {
        SourceKind {
            source: "whatsapp",
            channel: "chat",
        }
    } else {
        SourceKind {
            source: "telegram",
            channel: "chat",
        }
    }
}

fn author_for(kind: SourceKind, pick: usize) -> &'static str {
    match kind.source {
        "mbox" => EMAIL_AUTHORS[pick % EMAIL_AUTHORS.len()],
        "whatsapp" => WHATSAPP_AUTHORS[pick % WHATSAPP_AUTHORS.len()],
        _ => TELEGRAM_AUTHORS[pick % TELEGRAM_AUTHORS.len()],
    }
}

fn chat_for(kind: SourceKind, pick: usize) -> &'static str {
    match kind.source {
        "whatsapp" => WHATSAPP_CHATS[pick % WHATSAPP_CHATS.len()],
        "telegram" => TELEGRAM_CHATS[pick % TELEGRAM_CHATS.len()],
        _ => "",
    }
}

/// Zipf-ish word pick: quadratic index skew over the vocabulary.
fn word(rng: &mut Lcg) -> &'static str {
    let r = rng.below(1000);
    let idx = (r * r * VOCAB.len()) / 1_000_000;
    VOCAB[idx.min(VOCAB.len() - 1)]
}

fn background_body(rng: &mut Lcg, kind: SourceKind) -> String {
    let mut body = String::with_capacity(400);
    if kind.channel == "email" {
        body.push_str("Subject: ");
        for _ in 0..(2 + rng.below(4)) {
            body.push_str(word(rng));
            body.push(' ');
        }
        body.push_str("\n\nHi,\n\n");
        let words = 30 + rng.below(90);
        for w in 0..words {
            body.push_str(word(rng));
            body.push(' ');
            if w % 17 == 16 {
                body.push_str(". ");
            }
        }
        body.push_str(".\n\nBest regards");
    } else {
        let words = 3 + rng.below(10);
        for w in 0..words {
            body.push_str(word(rng));
            if w + 1 < words {
                body.push(' ');
            }
        }
    }
    body
}

fn background_sent_at(rng: &mut Lcg) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:00Z",
        2018 + rng.below(9),
        1 + rng.below(12),
        1 + rng.below(28),
        rng.below(24),
        rng.below(60)
    )
}

fn target_message(t: &TargetSpec) -> NewMessage {
    let mut m = NewMessage::new(t.source, t.external_id, t.author, t.sent_at, t.body);
    m.channel = if t.source == "mbox" { "email" } else { "chat" }.to_string();
    m.chat_id = t.chat_id.to_string();
    m
}

fn confuser_message(c: &ConfuserSpec, slot: usize) -> NewMessage {
    let mut m = NewMessage::new(
        c.source,
        format!("conf-{slot:04}"),
        c.author,
        c.sent_at,
        c.body,
    );
    m.channel = if c.source == "mbox" { "email" } else { "chat" }.to_string();
    m.chat_id = c.chat_id.to_string();
    m
}

/// The built corpus: the storage handle plus the row id of every labeled
/// correct original, keyed by question id.
pub struct BuiltCorpus {
    pub storage: Storage,
    pub target_ids: HashMap<&'static str, i64>,
    /// Messages per source kind (mbox / whatsapp / telegram).
    pub by_source: [u64; 3],
    /// Background vs labeled targets vs confusers.
    pub background: u64,
    pub targets: u64,
    pub confusers: u64,
}

/// A reserved slot: what to insert at a scattered position.
enum Reserved<'a> {
    Target {
        question_id: &'a str,
        spec: &'a TargetSpec,
    },
    Confuser(&'a ConfuserSpec),
}

/// Build the corpus: `count` messages total (background + targets +
/// confusers), file-backed at `db_path`.
pub fn build(db_path: &Path, count: usize, seed: u64) -> BuiltCorpus {
    assert!(
        QUESTIONS.len() >= 30,
        "U07 floor: at least 30 labeled questions"
    );
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture directory");
    }
    let storage = Storage::open(db_path).expect("open storage");

    // Reserved sequence: each question's target (when it has one),
    // followed by its confusers, in question order.
    let mut reserved: Vec<Reserved<'_>> = Vec::new();
    for q in QUESTIONS {
        if let Some(t) = &q.target {
            reserved.push(Reserved::Target {
                question_id: q.id,
                spec: t,
            });
        }
        for c in q.confusers {
            reserved.push(Reserved::Confuser(c));
        }
    }
    assert!(
        count >= reserved.len() * 4,
        "corpus of {count} too small for {} reserved messages",
        reserved.len()
    );
    // Evenly spaced deterministic positions, none at the very start/end.
    let position = |k: usize| (k + 1) * count / (reserved.len() + 1);

    let mut rng = Lcg(seed);
    let mut target_ids = HashMap::new();
    let mut by_source = [0u64; 3];
    let mut counts = (0u64, 0u64, 0u64); // background, targets, confusers
    let mut next_reserved = 0usize;

    for i in 0..count {
        let msg = if next_reserved < reserved.len() && position(next_reserved) == i {
            let slot = &reserved[next_reserved];
            let slot_index = next_reserved;
            next_reserved += 1;
            match slot {
                Reserved::Target { question_id, spec } => {
                    let m = target_message(spec);
                    let outcome = storage.insert_message(&m).expect("insert labeled target");
                    let id = match outcome {
                        vault_core::message::InsertOutcome::Inserted(id)
                        | vault_core::message::InsertOutcome::Duplicate(id) => id,
                    };
                    target_ids.insert(*question_id, id);
                    counts.1 += 1;
                    bump_source(&mut by_source, spec.source);
                    continue;
                }
                Reserved::Confuser(c) => {
                    counts.2 += 1;
                    bump_source(&mut by_source, c.source);
                    confuser_message(c, slot_index)
                }
            }
        } else {
            let kind = source_kind(rng.below(20));
            let author = author_for(kind, rng.below(64));
            let body = background_body(&mut rng, kind);
            let sent_at = background_sent_at(&mut rng);
            let external = format!("bg-{}-{}", kind.source, i);
            let mut m = NewMessage::new(kind.source, external, author, sent_at, body);
            m.channel = kind.channel.to_string();
            m.chat_id = chat_for(kind, rng.below(16)).to_string();
            counts.0 += 1;
            bump_source(&mut by_source, kind.source);
            m
        };
        storage
            .insert_message(&msg)
            .expect("insert background message");
    }
    assert_eq!(next_reserved, reserved.len(), "all reserved slots placed");
    for q in QUESTIONS {
        if q.target.is_some() {
            assert!(
                target_ids.contains_key(q.id),
                "target id captured for {}",
                q.id
            );
        }
    }

    BuiltCorpus {
        storage,
        target_ids,
        by_source,
        background: counts.0,
        targets: counts.1,
        confusers: counts.2,
    }
}

fn bump_source(by_source: &mut [u64; 3], source: &str) {
    match source {
        "mbox" => by_source[0] += 1,
        "whatsapp" => by_source[1] += 1,
        _ => by_source[2] += 1,
    }
}

// ---------------------------------------------------------------------------
// Self-check: the instrument is intact.
// ---------------------------------------------------------------------------

/// Verify the question set against the built corpus:
///
/// - every answerable question's target exists and is reachable by an
///   exact-term probe where the category demands uniqueness;
/// - `exact-term` queries match exactly one message, and it is the
///   labeled original;
/// - `no-answer` queries match nothing (FTS terms are conjunctive; a
///   shared single token such as an author name is legitimate).
///
/// Returns a list of violations; empty means the instrument is sound.
pub fn self_check(corpus: &BuiltCorpus) -> Vec<String> {
    use vault_core::search2::{SearchArchivePage, SearchArchiveRequest};

    let mut violations = Vec::new();
    let probe = |text: &str| -> vault_core::Result<SearchArchivePage> {
        corpus.storage.search_archive(&SearchArchiveRequest {
            text: text.to_string(),
            ..Default::default()
        })
    };

    for q in QUESTIONS {
        match &q.target {
            None => {
                // The full query must match nothing (FTS terms are a
                // conjunctive AND). Individual tokens may legitimately
                // appear — a shared author name or topic word — and the
                // hybrid leg's behavior on such overlap is exactly what
                // the false-confidence metric measures live.
                match probe(q.text) {
                    Ok(page) if page.total > 0 => violations.push(format!(
                        "{}: no-answer query matches {} messages",
                        q.id, page.total
                    )),
                    Err(e) => violations.push(format!("{}: no-answer probe failed: {e}", q.id)),
                    _ => {}
                }
            }
            Some(_) => {
                if !corpus.target_ids.contains_key(q.id) {
                    violations.push(format!("{}: target row id missing", q.id));
                    continue;
                }
                if q.category == "exact-term" {
                    match probe(q.text) {
                        Ok(page) => {
                            if page.total != 1 {
                                violations.push(format!(
                                    "{}: exact-term matches {} messages (want 1)",
                                    q.id, page.total
                                ));
                            } else if let Some(&id) = corpus.target_ids.get(q.id) {
                                if page.hits[0].id != id {
                                    violations.push(format!(
                                        "{}: exact-term hit is not the labeled original",
                                        q.id
                                    ));
                                }
                            }
                        }
                        Err(e) => {
                            violations.push(format!("{}: exact-term probe failed: {e}", q.id))
                        }
                    }
                }
            }
        }
    }
    violations
}

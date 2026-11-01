//! Hybrid FTS+vector ranking in the Ask pipeline: which ranking mode each
//! situation produces, how fusion changes the result list, the plan's date
//! bounds applying to the vector leg too, and the no-embeddings fallback.

use std::sync::{Mutex, Once};

use chrono::NaiveDate;
use vault_core::llm::{
    ask, rank_hybrid, MockLlm, PlanSource, PlannedQuery, QueryPlan, RankingMode,
};
use vault_core::message::NewMessage;
use vault_core::storage::Storage;
use vault_core::vector::{run_embedding_job, HashEmbedder};

static TZ_LOCK: Mutex<()> = Mutex::new(());
static TZ_PIN: Once = Once::new();

fn pin_utc_tz() {
    let _guard = TZ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    TZ_PIN.call_once(|| {
        std::env::set_var("TZ", "UTC");
        let _ = chrono::Local::now();
    });
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
}

fn msg(id: &str, sent_at: &str, body: &str) -> NewMessage {
    NewMessage::new("test", id, "someone", sent_at, body)
}

/// Archive shaped to separate the legs: one lexical hit, one only-semantic
/// hit (shares subwords, not whole FTS tokens), and unrelated noise.
fn seeded() -> Storage {
    pin_utc_tz();
    let s = Storage::open_in_memory().unwrap();
    for (id, sent_at, body) in [
        // FTS matches "vault" AND "sync"; also strongly vector-similar.
        (
            "lex",
            "2026-10-05T10:00:00Z",
            "vault sync planning went fine",
        ),
        // No whole-token match for the question terms (syncing/problems),
        // but heavy subword overlap → vector leg only.
        (
            "sem",
            "2026-10-20T10:00:00Z",
            "the syncing of the vault had problems again",
        ),
        // Out of the plan's October window entirely.
        ("old", "2026-08-01T10:00:00Z", "vault sync back in august"),
        // Unrelated noise.
        (
            "noise",
            "2026-10-10T10:00:00Z",
            "kiwi smoothie recipe ideas",
        ),
    ] {
        s.insert_message(&msg(id, sent_at, body)).unwrap();
    }
    s
}

#[test]
fn ask_fuses_both_legs_when_embeddings_exist() {
    let s = seeded();
    let e = HashEmbedder::new();
    run_embedding_job(&s, &e, None).unwrap();

    // "vault sync" matches lex and old lexically (FTS), and reaches sem
    // only through subword similarity (vector leg) — the fused list has
    // all three, noise-free.
    let outcome = ask(&s, None, &e, "vault sync", 50, 8, today()).unwrap();
    assert_eq!(outcome.ranking, RankingMode::HybridFtsVector);
    assert_eq!(outcome.plan_source, PlanSource::NoLlm);
    let bodies: Vec<&str> = outcome.results.iter().map(|m| m.body.as_str()).collect();
    assert!(
        bodies.contains(&"vault sync planning went fine"),
        "lexical hit present: {bodies:?}"
    );
    assert!(
        bodies.contains(&"the syncing of the vault had problems again"),
        "semantic-only hit surfaced by the vector leg: {bodies:?}"
    );
    assert!(
        bodies.contains(&"vault sync back in august"),
        "the second lexical hit stays: {bodies:?}"
    );
    assert_eq!(outcome.results.len(), 3);
    assert!(
        !bodies.iter().any(|b| b.contains("kiwi")),
        "noise stays below the similarity floor"
    );
    // RRF: rows ranked by BOTH legs (lex, old) beat the vector-only row,
    // whichever of the two leads — their fused scores each sum two lists.
    let top_two: Vec<&str> = bodies.iter().take(2).copied().collect();
    assert!(
        top_two.contains(&"vault sync planning went fine")
            && top_two.contains(&"vault sync back in august"),
        "double-ranked rows win fusion, got {bodies:?}"
    );
    assert_eq!(bodies[2], "the syncing of the vault had problems again");
    assert!(
        outcome.notes.is_empty(),
        "no degradation notes in hybrid mode"
    );
}

#[test]
fn ask_without_embeddings_falls_back_to_fts() {
    let s = seeded();
    let e = HashEmbedder::new();

    let outcome = ask(&s, None, &e, "vault sync", 50, 8, today()).unwrap();
    assert_eq!(outcome.ranking, RankingMode::FtsOnly);
    // FTS-only: the semantic-only row (no "sync" token) is absent.
    let bodies: Vec<&str> = outcome.results.iter().map(|m| m.body.as_str()).collect();
    assert!(bodies.contains(&"vault sync planning went fine"));
    assert!(!bodies.contains(&"the syncing of the vault had problems again"));
    // Notes stay reserved for degradation: coverage lives in the UI's
    // embedding status line, not here.
    assert!(outcome.notes.is_empty());
}

#[test]
fn ask_is_vector_only_when_fts_misses() {
    let s = Storage::open_in_memory().unwrap();
    s.insert_message(&msg(
        "dep",
        "2026-10-01T00:00:00Z",
        "the deployment kept failing yesterday",
    ))
    .unwrap();
    s.insert_message(&msg(
        "noise",
        "2026-10-02T00:00:00Z",
        "weekend barbecue checklist",
    ))
    .unwrap();
    let e = HashEmbedder::new();
    run_embedding_job(&s, &e, None).unwrap();

    // Plain plan quotes every term: "deploying" matches no stored token.
    let outcome = ask(&s, None, &e, "deploying", 50, 8, today()).unwrap();
    assert_eq!(outcome.ranking, RankingMode::VectorOnly);
    assert_eq!(outcome.results.len(), 1);
    assert_eq!(
        outcome.results[0].body,
        "the deployment kept failing yesterday"
    );
}

#[test]
fn plan_date_bounds_apply_to_the_vector_leg() {
    let s = seeded();
    let e = HashEmbedder::new();
    run_embedding_job(&s, &e, None).unwrap();

    // Plan: vault sync in October → the august row is excluded from BOTH
    // legs (FTS filtered, vector scan date-bounded).
    let planned = PlannedQuery {
        plan: QueryPlan {
            match_expr: "\"vault\" \"sync\"".into(),
            date_from: Some("2026-10-01".into()),
            date_to: Some("2026-10-31".into()),
        },
        source: PlanSource::NoLlm,
        notes: Vec::new(),
    };
    let fts = vault_core::llm::run_plan(&s, &planned, 50).unwrap();
    let hybrid = rank_hybrid(&s, &e, "vault sync", &planned, &fts, 50).unwrap();
    assert_eq!(hybrid.ranking, RankingMode::HybridFtsVector);
    let bodies: Vec<&str> = hybrid.results.iter().map(|m| m.body.as_str()).collect();
    assert!(
        !bodies.iter().any(|b| b.contains("august")),
        "date-bounded vector leg excludes the august row: {bodies:?}"
    );
}

#[test]
fn hybrid_ask_with_llm_end_to_end() {
    let s = seeded();
    let e = HashEmbedder::new();
    run_embedding_job(&s, &e, None).unwrap();
    let llm = MockLlm::with_replies([
        "{\"match\": \"\\\"vault\\\" \\\"sync\\\"\", \"date_from\": \"2026-10-01\", \"date_to\": \"2026-10-31\"}",
        "October's vault syncing had problems [2] after planning went fine [1].",
    ]);

    let outcome = ask(
        &s,
        Some(&llm),
        &e,
        "vault sync problems in october",
        50,
        8,
        today(),
    )
    .unwrap();

    assert_eq!(outcome.ranking, RankingMode::HybridFtsVector);
    assert!(outcome.answer.is_some());
    // Exactly two LLM round trips; the hybrid step is entirely local.
    assert_eq!(llm.prompts().len(), 2);
    // The summary prompt sees the fused result set (both legs' rows).
    let summary_user = &llm.prompts()[1].1;
    assert!(summary_user.contains("vault sync planning went fine"));
    assert!(summary_user.contains("the syncing of the vault had problems"));
    assert!(!summary_user.contains("august"));
    assert!(!summary_user.contains("kiwi"));
}

#[test]
fn ask_outcome_serializes_the_ranking_mode() {
    let s = seeded();
    let e = HashEmbedder::new();
    run_embedding_job(&s, &e, None).unwrap();
    let outcome = ask(&s, None, &e, "vault sync", 10, 8, today()).unwrap();
    let json = serde_json::to_value(&outcome).unwrap();
    assert_eq!(json["ranking"], serde_json::json!("hybrid_fts_vector"));
    assert!(json.get("plan_source").is_some(), "field names stay stable");
    let back: vault_core::llm::AskOutcome = serde_json::from_value(json).unwrap();
    assert_eq!(back, outcome);
}

#[test]
fn empty_archive_ask_is_fts_only_without_notes() {
    let s = Storage::open_in_memory().unwrap();
    let e = HashEmbedder::new();
    let outcome = ask(&s, None, &e, "anything", 50, 8, today()).unwrap();
    assert_eq!(outcome.ranking, RankingMode::FtsOnly);
    assert!(outcome.results.is_empty());
    assert!(
        outcome.notes.is_empty(),
        "no embedding suggestion on an empty archive: {:?}",
        outcome.notes
    );
}

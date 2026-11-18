//! Integration tests for the LLM search pipeline (deterministic mock LLM):
//! translation parsing, plan validation/fallback, date-filtered execution,
//! the privacy guarantee (only top result rows enter a prompt), and the
//! no-key fallback path.

use std::collections::HashMap;
use std::sync::{Mutex, Once};

use chrono::NaiveDate;
use vault_core::llm::{
    ask, run_plan, summarize_results, translate, validate, LlmConfig, MockLlm, PlanSource,
    PlannedQuery, QueryPlan, RankingMode, DEFAULT_SUMMARIZE_TOP,
};
use vault_core::message::NewMessage;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;
use vault_core::vector::{run_embedding_job, HashEmbedder};
use vault_core::Error;

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
}

/// Distinctive per-message bodies so prompt-containment assertions are exact.
///
/// The date-boundary expectations (m4 on 2026-10-31 23:59:59Z counting as
/// October) are written against UTC; pin the process TZ so the local-day
/// bound translation cannot shift them on a non-UTC machine. The pin must
/// happen exactly once under exclusive access: tests run on parallel
/// threads, and a concurrent first resolution of `Local` (inside the date
/// bounds) could cache the host TZ before or while `TZ` is mutated.
static TZ_LOCK: Mutex<()> = Mutex::new(());
static TZ_PIN: Once = Once::new();

fn pin_utc_tz() {
    let _guard = TZ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    TZ_PIN.call_once(|| {
        std::env::set_var("TZ", "UTC");
        let _ = chrono::Local::now(); // resolve with the pinned TZ
    });
}

fn seeded() -> Storage {
    pin_utc_tz();
    let s = Storage::open_in_memory().unwrap();
    let rows = [
        (
            "m1",
            "Ana",
            "2026-09-05T10:00:00Z",
            "september marker about the vault migration",
        ),
        (
            "m2",
            "Bob",
            "2026-10-12T08:30:00Z",
            "october first marker about vault sync",
        ),
        (
            "m3",
            "Ana",
            "2026-10-28T21:00:00Z",
            "october second marker on vault pricing",
        ),
        (
            "m4",
            "Cara",
            "2026-10-31T23:59:59Z",
            "october boundary marker on vault costs",
        ),
        (
            "m5",
            "Dov",
            "2026-11-01T09:00:00Z",
            "november marker unrelated",
        ),
        ("m6", "Eve", "", "vault marker without a date"),
    ];
    for (id, author, sent_at, body) in rows {
        let msg = NewMessage::new("test", id, author, sent_at, body);
        s.insert_message(&msg).unwrap();
    }
    // Embeddings exist, so the ask pipeline below runs in hybrid mode —
    // the mode the app is in once the user has run the embedding job.
    run_embedding_job(&s, &HashEmbedder::new(), None).unwrap();
    s
}

#[test]
fn ask_end_to_end_with_mock_llm() {
    let s = seeded();
    let llm = MockLlm::with_replies([
        // translate: vault in October (inclusive end of month)
        "{\"match\": \"\\\"vault\\\"\", \"date_from\": \"2026-10-01\", \"date_to\": \"2026-10-31\"}",
        // summarize
        "October covered sync [2], pricing [3], and costs [4].",
    ]);
    let question = "what was said about the vault in october?";
    let outcome = ask(
        &s,
        Some(&llm),
        &HashEmbedder::new(),
        question,
        50,
        DEFAULT_SUMMARIZE_TOP,
        today(),
    )
    .unwrap();

    assert_eq!(outcome.plan_source, PlanSource::Llm);
    assert_eq!(outcome.ranking, RankingMode::HybridFtsVector);
    assert_eq!(outcome.plan.match_expr, "\"vault\"");
    assert_eq!(outcome.plan.date_from.as_deref(), Some("2026-10-01"));
    assert_eq!(outcome.plan.date_to.as_deref(), Some("2026-10-31"));
    // October vault messages only: m2, m3, and the 2026-10-31T23:59:59Z
    // boundary row m4 (date_to is inclusive). September and November rows
    // and the date-less row are out.
    let bodies: Vec<&str> = outcome.results.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies.len(), 3);
    assert!(bodies.contains(&"october first marker about vault sync"));
    assert!(bodies.contains(&"october second marker on vault pricing"));
    assert!(bodies.contains(&"october boundary marker on vault costs"));
    assert_eq!(
        outcome.answer.as_deref(),
        Some("October covered sync [2], pricing [3], and costs [4].")
    );
    assert!(outcome.notes.is_empty());

    // Privacy: exactly two LLM calls. The translate prompt carries the
    // question and no message content; the summarize prompt carries only
    // the matched October rows.
    let prompts = llm.prompts();
    assert_eq!(prompts.len(), 2);
    let (translate_system, translate_user) = &prompts[0];
    assert!(translate_user.contains(&format!("Question: {question}")));
    assert!(translate_system.contains("FTS5 MATCH syntax"));
    assert!(!translate_user.contains("marker"));
    let summary_user = &prompts[1].1;
    for included in [
        "october first marker about vault sync",
        "october second marker on vault pricing",
        "october boundary marker on vault costs",
    ] {
        assert!(
            summary_user.contains(included),
            "summary must include: {included}"
        );
    }
    for excluded in [
        "september marker about the vault migration",
        "november marker unrelated",
        "vault marker without a date",
    ] {
        assert!(
            !summary_user.contains(excluded),
            "summary must not include: {excluded}"
        );
    }
}

#[test]
fn summary_prompt_is_limited_to_top_n_results() {
    let s = Storage::open_in_memory().unwrap();
    for i in 0..10 {
        let msg = NewMessage::new(
            "test",
            format!("m{i}"),
            "A",
            format!("2026-10-{i:02}T00:00:00Z"),
            format!("ranked marker number {i:02} about vault"),
        );
        s.insert_message(&msg).unwrap();
    }
    let llm = MockLlm::with_replies(["summary"]);
    let results = s
        .search_filtered(&build_match_query("vault"), None, None, None, false, 50)
        .unwrap();
    assert_eq!(results.len(), 10);
    let summarized = summarize_results(&llm, "vault?", &results, 3, &[], &HashMap::new());
    assert_eq!(summarized.answer.as_deref(), Some("summary"));
    let user = &llm.prompts()[0].1;
    // Which three are top-ranked is rank's business, but exactly three
    // bodies may appear in the prompt.
    let seen = (0..10)
        .filter(|i| user.contains(&format!("ranked marker number {i:02}")))
        .count();
    assert_eq!(seen, 3);
}

#[test]
fn ask_without_llm_is_plain_local_search() {
    let s = seeded();
    let outcome = ask(
        &s,
        None,
        &HashEmbedder::new(),
        "vault",
        50,
        DEFAULT_SUMMARIZE_TOP,
        today(),
    )
    .unwrap();
    assert_eq!(outcome.plan_source, PlanSource::NoLlm);
    // Local hybrid ranking is active even without an LLM.
    assert_eq!(outcome.ranking, RankingMode::HybridFtsVector);
    assert_eq!(
        outcome.plan,
        QueryPlan::plain("vault"),
        "no LLM: the plan is the plain term search"
    );
    assert_eq!(outcome.answer, None);
    assert!(outcome.notes.is_empty());
    // Plain search ignores dates: every vault row qualifies, including the
    // date-less one; only the november row (no "vault") is out.
    assert_eq!(outcome.results.len(), 5);
}

#[test]
fn ask_degrades_gracefully_when_llm_fails() {
    let s = seeded();
    let llm = MockLlm::failing("provider exploded");
    let outcome = ask(
        &s,
        Some(&llm),
        &HashEmbedder::new(),
        "vault october",
        50,
        DEFAULT_SUMMARIZE_TOP,
        today(),
    )
    .unwrap();

    // Translation failed and the summary failed; results are still there.
    // Plain fallback is an AND term search (the three October vault rows),
    // and the date-less vector leg surfaces the other vault rows too.
    assert_eq!(outcome.plan_source, PlanSource::LlmFallback);
    assert_eq!(outcome.plan, QueryPlan::plain("vault october"));
    assert_eq!(outcome.ranking, RankingMode::HybridFtsVector);
    assert_eq!(outcome.answer, None);
    let bodies: Vec<&str> = outcome.results.iter().map(|m| m.body.as_str()).collect();
    for expected in [
        "october first marker about vault sync",
        "october second marker on vault pricing",
        "october boundary marker on vault costs",
        "september marker about the vault migration",
        "vault marker without a date",
    ] {
        assert!(
            bodies.contains(&expected),
            "missing {expected} in {bodies:?}"
        );
    }
    assert_eq!(outcome.results.len(), 5);
    assert_eq!(outcome.notes.len(), 2);
    assert!(outcome.notes[0].contains("LLM translation failed"));
    assert!(outcome.notes[1].contains("LLM summary failed"));
}

#[test]
fn invalid_fts5_from_llm_falls_back_but_keeps_dates() {
    let s = seeded();
    // `"vault" (` is not parseable FTS5; the dates are usable.
    let candidate = QueryPlan {
        match_expr: "\"vault\" (".into(),
        date_from: Some("2026-10-01".into()),
        date_to: Some("2026-10-31".into()),
        from: None,
        has_link: None,
        extract_links: false,
    };
    let planned = validate(&s, "vault october", Ok(candidate));
    assert_eq!(planned.source, PlanSource::LlmFallback);
    assert_eq!(planned.plan.match_expr, build_match_query("vault october"));
    assert_eq!(planned.plan.date_from.as_deref(), Some("2026-10-01"));
    assert_eq!(planned.plan.date_to.as_deref(), Some("2026-10-31"));
    assert!(!planned.notes.is_empty());

    let results = run_plan(&s, &planned, 50).unwrap();
    // October rows only, plain term search over the vault markers.
    assert_eq!(results.len(), 3);
}

#[test]
fn unparseable_llm_json_falls_back_to_plain_plan() {
    let s = seeded();
    let llm = MockLlm::with_replies(["I will not produce JSON today"]);
    let planned = validate(&s, "vault", translate(&llm, "vault", today()));
    assert_eq!(planned.source, PlanSource::LlmFallback);
    assert_eq!(planned.plan, QueryPlan::plain("vault"));
    assert_eq!(planned.plan.date_from, None);
}

#[test]
fn storage_date_filters_are_inclusive_and_validated() {
    let s = seeded();

    let all = s
        .search_filtered("\"vault\"", None, None, None, false, 50)
        .unwrap();
    assert_eq!(all.len(), 5, "no bounds: every vault row, incl. date-less");

    let october = s
        .search_filtered(
            "\"vault\"",
            Some("2026-10-01"),
            Some("2026-10-31"),
            None,
            false,
            50,
        )
        .unwrap();
    assert_eq!(october.len(), 3, "inclusive both ends; date-less excluded");

    let from_only = s
        .search_filtered("\"vault\"", Some("2026-10-13"), None, None, false, 50)
        .unwrap();
    assert_eq!(from_only.len(), 2, "m3 and m4 (m2 is on the 12th)");

    assert!(matches!(
        s.search_filtered("\"vault\"", Some("October"), None, None, false, 50),
        Err(Error::InvalidDate(_))
    ));
    assert!(matches!(
        s.search_filtered("\"vault\"", None, Some("2026-13-01"), None, false, 50),
        Err(Error::InvalidDate(_))
    ));
}

#[test]
fn match_expression_vetting() {
    let s = seeded();
    assert!(s.is_valid_match("\"vault\" OR \"chat\""));
    assert!(s.is_valid_match("body:vault"));
    assert!(s.is_valid_match("\"va*\""));
    assert!(!s.is_valid_match("\"vault\" ("));
    assert!(!s.is_valid_match(""));
    assert!(!s.is_valid_match("nosuchcolumn:vault"));
}

#[test]
fn ask_with_empty_plan_expression_returns_no_results() {
    let s = seeded();
    let planned = PlannedQuery {
        plan: QueryPlan {
            match_expr: String::new(),
            date_from: None,
            date_to: None,
            from: None,
            has_link: None,
            extract_links: false,
        },
        source: PlanSource::NoLlm,
        notes: Vec::new(),
    };
    let results = run_plan(&s, &planned, 50).unwrap();
    assert!(results.is_empty());
}

#[test]
fn ask_outcome_is_serializable_for_the_frontend() {
    // The Tauri command returns AskOutcome by value; it must round-trip
    // through serde with the exact field names the frontend expects.
    let s = seeded();
    let outcome = ask(&s, None, &HashEmbedder::new(), "vault", 10, 8, today()).unwrap();
    let json = serde_json::to_value(&outcome).unwrap();
    assert!(json.get("plan_source").is_some());
    assert_eq!(json["plan_source"], serde_json::json!("no_llm"));
    assert_eq!(json["ranking"], serde_json::json!("hybrid_fts_vector"));
    assert!(json["plan"]["match_expr"].is_string());
    assert_eq!(json["results"].as_array().unwrap().len(), 5);
    assert!(json["answer"].is_null());
    let back: vault_core::llm::AskOutcome = serde_json::from_value(json).unwrap();
    assert_eq!(back, outcome);
    assert!(outcome.results.iter().all(|m| m.id > 0));
}

#[test]
fn llm_status_serializes_the_provider_menu() {
    use vault_core::llm::LlmStatus;
    use vault_core::settings::{build_menu, Settings};

    // Local-only status: disabled, menu lists exactly the local entry.
    let disabled = LlmStatus::local_only();
    assert!(!disabled.enabled);
    assert_eq!(disabled.providers.len(), 1);
    assert_eq!(disabled.providers[0].id, "local");
    assert_eq!(disabled.default_provider, None);
    let json = serde_json::to_value(&disabled).unwrap();
    // The exact serde field names the frontend mirrors.
    assert_eq!(json["providers"][0]["id"], "local");
    assert_eq!(json["providers"][0]["available"], true);
    assert!(json.get("default_provider").is_some());

    // A settings-backed menu: the status fields are filled by the shell's
    // registry; here we assert the pure menu building the status is built
    // from — including the env fallback entry.
    let cfg = LlmConfig::from_lookup(|name| (name == "ZAI_API_KEY").then(|| "k".into()))
        .unwrap()
        .expect("ZAI key enables the LLM");
    let settings = Settings::default();
    let menu = build_menu(&settings, |_| false, Some(&cfg));
    let ids: Vec<&str> = menu.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["local", "env"]);
    assert_eq!(menu[1].model, "glm-5");
    assert_eq!(menu[1].host, "api.z.ai");
    assert!(menu[1].available);
}

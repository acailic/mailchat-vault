//! Contract tests for the LLM search layer, pinning the product policies:
//!
//! 1. **Key/endpoint pairing matrix** — every combination of
//!    `ANTHROPIC_API_KEY`, `ZAI_API_KEY`/`GLM_API_KEY`, and
//!    `ANTHROPIC_BASE_URL` either resolves to a matching provider or is
//!    refused outright. A key is never sent to a provider it does not
//!    belong to (continuation of the established policy).
//! 2. **RAG top-N limit** — with the default configuration only the top-8
//!    search results enter the summary prompt, exactly the top-8 and no
//!    others.
//! 3. **Privacy on the wire** — against a local HTTP recorder standing in
//!    for the provider endpoint: the API key rides only in the `x-api-key`
//!    header (never in the request payload or URL), the translate request
//!    carries no archive rows at all, and the summarize request carries only
//!    the top result rows — rows outside the search results never leave the
//!    machine.
//!
//! Plus parity contracts: without a key the app degrades to exactly the
//! plain FTS5 search, and search results never depend on the LLM succeeding.

use std::io::{Read, Write};
use std::net::TcpListener;

use chrono::NaiveDate;
use vault_core::llm::{
    ask, summarize_results, translate, validate, AnthropicLlm, LlmConfig, MockLlm, PlanSource,
    QueryPlan, DEFAULT_SUMMARIZE_TOP,
};
use vault_core::message::NewMessage;
use vault_core::search::build_match_query;
use vault_core::storage::Storage;
use vault_core::vector::HashEmbedder;

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
}

/// Distinctive per-message bodies so prompt/wire-containment assertions are
/// exact: every body contains "vault" plus a unique marker.
fn seeded(rows: usize, marker: &str) -> Storage {
    let s = Storage::open_in_memory().unwrap();
    for i in 0..rows {
        let msg = NewMessage::new(
            "test",
            format!("m{i}"),
            format!("Author {i:02}"),
            format!("2026-10-{i:02}T12:00:00Z"),
            format!("{marker} {i:02} about vault topics"),
        );
        s.insert_message(&msg).unwrap();
    }
    s
}

// ---------------------------------------------------------------------------
// 1. Key/endpoint pairing matrix
// ---------------------------------------------------------------------------

/// What `LlmConfig::from_lookup` must produce for one environment shape.
enum Expect {
    /// LLM stays off: `Ok(None)`.
    Off,
    /// Enabled with exactly this key and base URL.
    On {
        key: &'static str,
        base_url: &'static str,
    },
    /// Refused with an error naming the offending policy.
    Refused(&'static str),
}

struct Case {
    name: &'static str,
    anthropic_key: Option<&'static str>,
    zai_key: Option<&'static str>,
    glm_key: Option<&'static str>,
    base_url: Option<&'static str>,
    expect: Expect,
}

fn lookup_from<'a>(env: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| {
        env.iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| (*v).to_string())
    }
}

fn run_case(case: &Case) {
    let Case {
        name,
        anthropic_key,
        zai_key,
        glm_key,
        base_url,
        expect,
    } = case;
    let mut env: Vec<(&str, &str)> = Vec::new();
    if let Some(v) = anthropic_key {
        env.push(("ANTHROPIC_API_KEY", v));
    }
    if let Some(v) = zai_key {
        env.push(("ZAI_API_KEY", v));
    }
    if let Some(v) = glm_key {
        env.push(("GLM_API_KEY", v));
    }
    if let Some(v) = base_url {
        env.push(("ANTHROPIC_BASE_URL", v));
    }
    let outcome = LlmConfig::from_lookup(lookup_from(&env));
    match expect {
        Expect::Off => assert!(
            matches!(outcome, Ok(None)),
            "{name}: expected LLM off, got {outcome:?}"
        ),
        Expect::On { key, base_url } => {
            let cfg = outcome
                .unwrap_or_else(|e| panic!("{name}: expected a config, got error {e}"))
                .unwrap_or_else(|| panic!("{name}: expected a config, got None"));
            assert_eq!(cfg.api_key, *key, "{name}: wrong key selected");
            assert_eq!(cfg.base_url, *base_url, "{name}: wrong base URL");
            assert_eq!(cfg.model, LlmConfig::DEFAULT_MODEL, "{name}: wrong model");
        }
        Expect::Refused(fragment) => match outcome {
            Err(e) => assert!(
                e.to_string().contains(fragment),
                "{name}: error should mention {fragment:?}, got: {e}"
            ),
            other => panic!("{name}: expected refusal, got {other:?}"),
        },
    }
}

#[test]
fn key_endpoint_pairing_matrix() {
    let zai = LlmConfig::DEFAULT_BASE_URL;
    let cases = [
        // -- no key: always off, whatever the URL says -------------------
        Case {
            name: "nothing set",
            anthropic_key: None,
            zai_key: None,
            glm_key: None,
            base_url: None,
            expect: Expect::Off,
        },
        Case {
            name: "base URL alone enables nothing",
            anthropic_key: None,
            zai_key: None,
            glm_key: None,
            base_url: Some("https://api.anthropic.com"),
            expect: Expect::Off,
        },
        Case {
            name: "z.ai URL alone enables nothing",
            anthropic_key: None,
            zai_key: None,
            glm_key: None,
            base_url: Some(zai),
            expect: Expect::Off,
        },
        // -- z.ai key: default endpoint or an explicit z.ai URL ----------
        Case {
            name: "ZAI key alone",
            anthropic_key: None,
            zai_key: Some("zk"),
            glm_key: None,
            base_url: None,
            expect: Expect::On {
                key: "zk",
                base_url: zai,
            },
        },
        Case {
            name: "GLM key alone",
            anthropic_key: None,
            zai_key: None,
            glm_key: Some("gk"),
            base_url: None,
            expect: Expect::On {
                key: "gk",
                base_url: zai,
            },
        },
        Case {
            name: "ZAI wins over GLM",
            anthropic_key: None,
            zai_key: Some("zk"),
            glm_key: Some("gk"),
            base_url: None,
            expect: Expect::On {
                key: "zk",
                base_url: zai,
            },
        },
        Case {
            name: "ZAI key + explicit z.ai URL",
            anthropic_key: None,
            zai_key: Some("zk"),
            glm_key: None,
            base_url: Some(zai),
            expect: Expect::On {
                key: "zk",
                base_url: zai,
            },
        },
        Case {
            name: "GLM key + explicit z.ai URL (upper-case host)",
            anthropic_key: None,
            zai_key: None,
            glm_key: Some("gk"),
            base_url: Some("https://API.Z.AI/api/anthropic"),
            expect: Expect::On {
                key: "gk",
                base_url: "https://API.Z.AI/api/anthropic",
            },
        },
        // -- z.ai key refused on any foreign endpoint ---------------------
        Case {
            name: "ZAI key + anthropic.com URL",
            anthropic_key: None,
            zai_key: Some("zk"),
            glm_key: None,
            base_url: Some("https://api.anthropic.com"),
            expect: Expect::Refused("refusing to send"),
        },
        Case {
            name: "GLM key + foreign URL",
            anthropic_key: None,
            zai_key: None,
            glm_key: Some("gk"),
            base_url: Some("https://llm.example.invalid/api"),
            expect: Expect::Refused("refusing to send"),
        },
        Case {
            name: "ZAI key + z.ai lookalike host",
            anthropic_key: None,
            zai_key: Some("zk"),
            glm_key: None,
            base_url: Some("https://api.z.ai.example.invalid/api"),
            expect: Expect::Refused("refusing to send"),
        },
        // -- Anthropic key: honored only with its own explicit endpoint --
        Case {
            name: "Anthropic key alone",
            anthropic_key: Some("ak"),
            zai_key: None,
            glm_key: None,
            base_url: None,
            expect: Expect::Refused("ANTHROPIC_BASE_URL"),
        },
        Case {
            name: "Anthropic key + default z.ai URL",
            anthropic_key: Some("ak"),
            zai_key: None,
            glm_key: None,
            base_url: Some(zai),
            expect: Expect::Refused("refusing to send an Anthropic key to z.ai"),
        },
        Case {
            name: "Anthropic key + z.ai URL with port",
            anthropic_key: Some("ak"),
            zai_key: None,
            glm_key: None,
            base_url: Some("https://api.z.ai:443/api/anthropic"),
            expect: Expect::Refused("refusing to send an Anthropic key to z.ai"),
        },
        Case {
            name: "Anthropic key + z.ai URL with userinfo",
            anthropic_key: Some("ak"),
            zai_key: None,
            glm_key: None,
            base_url: Some("https://user:pw@api.z.ai/api/anthropic"),
            expect: Expect::Refused("refusing to send an Anthropic key to z.ai"),
        },
        Case {
            name: "Anthropic key + trailing-dot z.ai host",
            anthropic_key: Some("ak"),
            zai_key: None,
            glm_key: None,
            base_url: Some("https://api.z.ai./api/anthropic"),
            expect: Expect::Refused("refusing to send an Anthropic key to z.ai"),
        },
        Case {
            name: "Anthropic key + its own endpoint",
            anthropic_key: Some("ak"),
            zai_key: None,
            glm_key: None,
            base_url: Some("https://api.anthropic.com"),
            expect: Expect::On {
                key: "ak",
                base_url: "https://api.anthropic.com",
            },
        },
        Case {
            name: "Anthropic key + other explicit foreign endpoint",
            anthropic_key: Some("ak"),
            zai_key: None,
            glm_key: None,
            base_url: Some("https://proxy.example.invalid/llm"),
            expect: Expect::On {
                key: "ak",
                base_url: "https://proxy.example.invalid/llm",
            },
        },
        // -- both keys present: the Anthropic key's rules still decide ----
        Case {
            name: "Anthropic + ZAI keys, no URL",
            anthropic_key: Some("ak"),
            zai_key: Some("zk"),
            glm_key: None,
            base_url: None,
            expect: Expect::Refused("ANTHROPIC_BASE_URL"),
        },
        Case {
            name: "Anthropic + ZAI keys + z.ai URL",
            anthropic_key: Some("ak"),
            zai_key: Some("zk"),
            glm_key: None,
            base_url: Some(zai),
            expect: Expect::Refused("refusing to send an Anthropic key to z.ai"),
        },
        Case {
            name: "Anthropic + ZAI keys + foreign URL",
            anthropic_key: Some("ak"),
            zai_key: Some("zk"),
            glm_key: None,
            base_url: Some("https://api.anthropic.com"),
            expect: Expect::On {
                key: "ak",
                base_url: "https://api.anthropic.com",
            },
        },
        // -- blank values count as unset -----------------------------------
        Case {
            name: "whitespace Anthropic key is unset",
            anthropic_key: Some("   "),
            zai_key: None,
            glm_key: None,
            base_url: None,
            expect: Expect::Off,
        },
        Case {
            name: "whitespace ZAI key falls through to GLM",
            anthropic_key: None,
            zai_key: Some("  "),
            glm_key: Some("gk"),
            base_url: None,
            expect: Expect::On {
                key: "gk",
                base_url: zai,
            },
        },
    ];
    for case in &cases {
        run_case(case);
    }
}

#[test]
fn accepted_config_honors_model_override() {
    let cfg = LlmConfig::from_lookup(lookup_from(&[
        ("ZAI_API_KEY", "zk"),
        ("MAILCHAT_LLM_MODEL", "custom-model"),
    ]))
    .unwrap()
    .expect("enabled");
    assert_eq!(cfg.model, "custom-model");
    assert_eq!(cfg.host(), "api.z.ai");
}

// ---------------------------------------------------------------------------
// 2. RAG top-N limit
// ---------------------------------------------------------------------------

#[test]
fn summary_prompt_carries_exactly_the_default_top_8() {
    // The product contract: by default only the top-8 search results are
    // sent to the LLM, never more.
    assert_eq!(DEFAULT_SUMMARIZE_TOP, 8);

    let s = seeded(12, "topn marker");
    let llm = MockLlm::with_replies(["summary"]);
    let results = s
        .search_filtered(&build_match_query("vault"), None, None, None, false, 50)
        .unwrap();
    assert_eq!(results.len(), 12);

    let summarized = summarize_results(&llm, "vault?", &results, DEFAULT_SUMMARIZE_TOP, &[]);
    assert_eq!(summarized.answer.as_deref(), Some("summary"));
    let user = &llm.prompts()[0].1;

    // Exactly the top-8 ranked rows appear — the same eight the pipeline
    // would send — and none of the remaining four.
    let included: Vec<&str> = results[..DEFAULT_SUMMARIZE_TOP]
        .iter()
        .map(|m| m.body.as_str())
        .collect();
    let excluded: Vec<&str> = results[DEFAULT_SUMMARIZE_TOP..]
        .iter()
        .map(|m| m.body.as_str())
        .collect();
    for body in &included {
        assert!(user.contains(body), "top-8 row missing from prompt: {body}");
    }
    for body in &excluded {
        assert!(
            !user.contains(body),
            "row outside the top-8 leaked into the prompt: {body}"
        );
    }
    let markers_in_prompt = (0..12)
        .filter(|i| user.contains(&format!("topn marker {i:02}")))
        .count();
    assert_eq!(markers_in_prompt, DEFAULT_SUMMARIZE_TOP);
}

#[test]
fn summary_prompt_truncates_long_bodies() {
    // Mirrors the private SUMMARY_BODY_LIMIT (500 chars) after which the
    // context block is cut with an ellipsis, so one huge message cannot
    // become the whole prompt.
    const BODY_LIMIT: usize = 500;
    let s = Storage::open_in_memory().unwrap();
    // 500 chars of plain padding, then a distinctive tail that must never
    // be sent (a repeating-char tail would trivially appear inside the
    // sent prefix).
    let long_body = format!(
        "{}{}{}",
        "vault ".repeat(10),
        "Z".repeat(440),                               // chars 61..=500: sent
        "SECRET-TAIL-".to_string() + &"Q".repeat(100)  // past the limit: never sent
    );
    s.insert_message(&NewMessage::new(
        "test",
        "m0",
        "A",
        "2026-10-01T00:00:00Z",
        long_body.clone(),
    ))
    .unwrap();
    let llm = MockLlm::with_replies(["summary"]);
    let results = s
        .search_filtered(&build_match_query("vault"), None, None, None, false, 50)
        .unwrap();
    let summarized = summarize_results(&llm, "q", &results, DEFAULT_SUMMARIZE_TOP, &[]);
    assert_eq!(summarized.answer.as_deref(), Some("summary"));
    let user = &llm.prompts()[0].1;
    let prefix: String = long_body.chars().take(BODY_LIMIT).collect();
    assert!(user.contains(&prefix), "the first 500 chars must be sent");
    assert!(
        !user.contains("SECRET-TAIL"),
        "nothing past the 500-char limit may be sent"
    );
}

// ---------------------------------------------------------------------------
// 3. Privacy on the wire (local HTTP recorder in place of the provider)
// ---------------------------------------------------------------------------

/// One recorded HTTP request as seen on the socket.
struct Recorded {
    request_line: String,
    headers: String,
    body: String,
}

/// Read a single HTTP/1.1 request (headers + Content-Length body) from the
/// stream.
fn read_request(stream: &mut std::net::TcpStream) -> Recorded {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut chunk).expect("read request");
        assert!(n > 0, "connection closed before headers ended");
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length: usize = head
        .to_ascii_lowercase()
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .map(|v| v.trim().parse().expect("content-length"))
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut chunk).expect("read body");
        assert!(n > 0, "connection closed before body ended");
        buf.extend_from_slice(&chunk[..n]);
    }
    Recorded {
        request_line: head.lines().next().unwrap_or_default().to_string(),
        headers: head,
        body: String::from_utf8_lossy(&buf[header_end..header_end + content_length]).to_string(),
    }
}

/// Serve `replies.len()` requests on an ephemeral local port, answering each
/// with an Anthropic-style JSON body carrying the given text, and return the
/// port plus a join handle yielding the recorded requests.
fn spawn_recorder(replies: Vec<String>) -> (String, std::thread::JoinHandle<Vec<Recorded>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local recorder");
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let mut recorded = Vec::new();
        for reply in &replies {
            let (mut stream, _) = listener.accept().expect("accept");
            let req = read_request(&mut stream);
            let payload = serde_json::json!({
                "content": [{ "type": "text", "text": reply }]
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            stream.write_all(response.as_bytes()).expect("write reply");
            recorded.push(req);
        }
        recorded
    });
    // Trailing slash on purpose: the endpoint join must trim it.
    (format!("http://{addr}/"), handle)
}

#[test]
fn wire_privacy_key_in_header_only_and_only_top_rows_in_body() {
    const KEY: &str = "sk-wire-test-DO-NOT-LEAK-9f1e";
    let question = "what do the vault topics say?";

    let s = seeded(12, "wire marker");
    // A canary row the plan will never match (no "vault" in it): content
    // that must never appear anywhere on the wire.
    s.insert_message(&NewMessage::new(
        "test",
        "canary",
        "Canary",
        "2026-10-05T00:00:00Z",
        "canary secret unrelated trove payload",
    ))
    .unwrap();

    let (base_url, server) = spawn_recorder(vec![
        // translate reply: plain "vault" plan, no dates
        "{\"match\": \"\\\"vault\\\"\", \"date_from\": null, \"date_to\": null}".into(),
        // summarize reply
        "served summary [1].".into(),
    ]);
    let config = LlmConfig {
        base_url,
        api_key: KEY.into(),
        model: "wire-test-model".into(),
    };
    let backend = AnthropicLlm::new(config);

    let outcome = ask(
        &s,
        Some(&backend),
        &HashEmbedder::new(),
        question,
        50,
        DEFAULT_SUMMARIZE_TOP,
        today(),
    )
    .unwrap();

    // The pipeline used the served replies end to end.
    assert_eq!(outcome.plan_source, PlanSource::Llm);
    assert_eq!(outcome.answer.as_deref(), Some("served summary [1]."));
    let results = &outcome.results;
    assert_eq!(results.len(), 12, "every vault row matches the plan");
    assert!(
        results.iter().all(|m| !m.body.contains("canary")),
        "the canary row is not a search result"
    );

    let requests = server.join().expect("recorder thread");
    assert_eq!(requests.len(), 2, "translate + summarize round trips");

    for (label, req) in [("translate", &requests[0]), ("summarize", &requests[1])] {
        // The key rides only in the x-api-key header...
        assert!(
            req.headers
                .to_ascii_lowercase()
                .contains(&format!("x-api-key: {KEY}").to_ascii_lowercase()),
            "{label}: x-api-key header must carry the key"
        );
        assert!(
            req.headers
                .to_ascii_lowercase()
                .contains("anthropic-version:"),
            "{label}: anthropic-version header must be set"
        );
        // ...never in the payload or the URL.
        assert!(
            !req.body.contains(KEY),
            "{label}: PRIVACY VIOLATION — key leaked into the request payload"
        );
        assert_eq!(
            req.request_line, "POST /v1/messages HTTP/1.1",
            "{label}: request line must carry no key or extra query"
        );
        assert!(
            req.body.contains("wire-test-model"),
            "{label}: request must target the configured model"
        );
    }

    // Translate request: the question, and not a single archive row.
    let translate_body = &requests[0].body;
    assert!(translate_body.contains(question));
    assert!(
        !translate_body.contains("wire marker"),
        "translate must send no archive rows"
    );
    assert!(!translate_body.contains("canary"));

    // Summarize request: exactly the top-8 result rows, none of the rest,
    // and never the canary row that was outside the search results.
    let summarize_body = &requests[1].body;
    for body in results[..DEFAULT_SUMMARIZE_TOP].iter().map(|m| &m.body) {
        assert!(
            summarize_body.contains(body.as_str()),
            "top-8 row missing from the wire: {body}"
        );
    }
    for body in results[DEFAULT_SUMMARIZE_TOP..].iter().map(|m| &m.body) {
        assert!(
            !summarize_body.contains(body.as_str()),
            "PRIVACY VIOLATION — row outside the top-8 left the machine: {body}"
        );
    }
    assert!(
        !summarize_body.contains("canary secret unrelated trove payload"),
        "PRIVACY VIOLATION — a row outside the search results left the machine"
    );
}

// ---------------------------------------------------------------------------
// Parity contracts: search works with no key and never depends on the LLM
// ---------------------------------------------------------------------------

#[test]
fn without_a_key_outcome_equals_plain_search_exactly() {
    let s = seeded(12, "plain marker");
    let question = "vault topics";
    let outcome = ask(
        &s,
        None,
        &HashEmbedder::new(),
        question,
        50,
        DEFAULT_SUMMARIZE_TOP,
        today(),
    )
    .unwrap();

    assert_eq!(outcome.plan_source, PlanSource::NoLlm);
    assert_eq!(outcome.plan, QueryPlan::plain(question));
    assert_eq!(outcome.answer, None);
    assert!(outcome.notes.is_empty());
    let direct = s
        .search_filtered(&build_match_query(question), None, None, None, false, 50)
        .unwrap();
    assert_eq!(
        outcome.results, direct,
        "no-key ask must equal plain FTS5 search element-wise, order included"
    );
}

#[test]
fn search_results_never_depend_on_the_llm_succeeding() {
    let s = seeded(12, "degraded marker");
    let question = "vault topics";
    let failing = MockLlm::failing("provider exploded");
    let outcome = ask(
        &s,
        Some(&failing),
        &HashEmbedder::new(),
        question,
        50,
        DEFAULT_SUMMARIZE_TOP,
        today(),
    )
    .unwrap();

    assert_eq!(outcome.plan_source, PlanSource::LlmFallback);
    assert_eq!(outcome.answer, None);
    assert_eq!(
        outcome.results.len(),
        12,
        "every seeded row matches the plain fallback"
    );
    assert_eq!(
        outcome.notes.len(),
        2,
        "both degradation notes must be present"
    );
    assert!(outcome.notes[0].contains("LLM translation failed"));
    assert!(outcome.notes[1].contains("LLM summary failed"));

    let direct = s
        .search_filtered(&build_match_query(question), None, None, None, false, 50)
        .unwrap();
    assert_eq!(
        outcome.results, direct,
        "a dead LLM must not change what search returns"
    );
}

// ---------------------------------------------------------------------------
// Deterministic mock replies for plan queries (translate contract)
// ---------------------------------------------------------------------------

#[test]
fn overlong_match_expression_is_rejected_and_degrades_to_plain() {
    let s = seeded(3, "long marker");
    // Mirrors the private MAX_MATCH_LEN cap (512): a longer expression from
    // the model is unusable, and validate falls back to a plain term search.
    let overlong = format!("\"{}\"", "x".repeat(600));
    let reply = format!(
        "{{\"match\": {}}}",
        serde_json::to_string(&overlong).unwrap()
    );
    let llm = MockLlm::with_replies([reply]);
    let planned = validate(&s, "vault", translate(&llm, "vault", today()));
    assert_eq!(planned.source, PlanSource::LlmFallback);
    assert_eq!(planned.plan, QueryPlan::plain("vault"));
    assert!(!planned.notes.is_empty());
}

#[test]
fn leap_day_dates_are_validated_not_trusted() {
    let s = Storage::open_in_memory().unwrap();

    let ok = MockLlm::with_replies(["{\"match\": \"vault\", \"date_from\": \"2024-02-29\"}"]);
    let plan = translate(&ok, "q", today()).unwrap();
    assert_eq!(plan.date_from.as_deref(), Some("2024-02-29"));

    // 2023 is not a leap year: this date is rejected, and validate degrades.
    let bad = MockLlm::with_replies(["{\"match\": \"vault\", \"date_from\": \"2023-02-29\"}"]);
    let planned = validate(&s, "vault", translate(&bad, "vault", today()));
    assert_eq!(planned.source, PlanSource::LlmFallback);
    assert_eq!(planned.plan, QueryPlan::plain("vault"));
}

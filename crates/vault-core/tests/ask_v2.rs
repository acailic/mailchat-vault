//! Contract tests for the M5.2 job-backed ask flow (v2): prepare →
//! approve → generate over a local TCP recorder standing in for the
//! provider endpoint.
//!
//! 1. **Zero wire requests from prepare** — preparing an ask (payload
//!    preview + digest) is fully local; even with a usable network
//!    endpoint recorded, not one request may arrive.
//! 2. **Generate sends exactly the approved bytes** — one round trip
//!    whose `system`/`user`/`model` are byte-identical to the preview
//!    the digest was computed over; the API key rides only in the
//!    `x-api-key` header.
//! 3. **Stale approvals are refused with nothing sent** — a wrong
//!    digest, a replaced prepare, a changed endpoint, or a changed
//!    result snapshot (`archive_revision` moved) each refuse before any
//!    byte leaves the machine.
//! 4. **Cancellation between steps sends nothing** — a cancellation
//!    observed at the checkpoint between approval consumption and the
//!    send stops the ask with zero requests.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use vault_core::llm::{
    generate_approved_answer, prepare_scoped_ask, AnthropicLlm, AskV2Error, LlmConfig,
    PreparedAskRegistry, PreparedEndpoint,
};
use vault_core::message::NewMessage;
use vault_core::search2::AskScope;
use vault_core::storage::Storage;
use vault_core::vector::HashEmbedder;

const KEY: &str = "sk-v2-ask-test-DO-NOT-LEAK-771a";

/// One recorded HTTP request: request line, full header block, body.
struct Recorded {
    request_line: String,
    headers: String,
    body: String,
}

/// Read one HTTP/1.1 request (headers + Content-Length body).
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

/// A local recorder standing in for the LLM provider endpoint: a
/// nonblocking accept loop that answers every accepted connection
/// Anthropic-style with the next entry of `replies` (a JSON body with no
/// text when the replies run out) and stops when `stop` is set — so a
/// test can prove that *no* request ever arrived by setting `stop` after
/// the call returned and collecting an empty request list.
fn spawn_recorder(
    replies: Vec<String>,
    stop: Arc<AtomicBool>,
) -> (String, std::thread::JoinHandle<Vec<Recorded>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local recorder");
    let addr = listener.local_addr().unwrap();
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let handle = std::thread::spawn(move || {
        let mut recorded = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).expect("blocking stream");
                    let req = read_request(&mut stream);
                    let reply = replies
                        .get(recorded.len())
                        .cloned()
                        .unwrap_or_else(|| "{}".into());
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
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                Err(_) => break,
            }
        }
        recorded
    });
    // Trailing slash on purpose: the endpoint join must trim it.
    (format!("http://{addr}/"), handle)
}

/// A vault seeded with `rows` matching messages from two authors, plus a
/// canary row the question never matches.
fn seeded(rows: usize) -> Storage {
    let s = Storage::open_in_memory().unwrap();
    for i in 0..rows {
        let author = if i % 2 == 0 {
            "ana@example.com"
        } else {
            "bob@example.com"
        };
        s.insert_message(&NewMessage::new(
            "test",
            format!("m{i}"),
            author,
            format!("2026-10-{:02}T12:00:00Z", i % 28 + 1),
            format!("v2 ask marker {i:02} about the vault"),
        ))
        .unwrap();
    }
    s.insert_message(&NewMessage::new(
        "test",
        "canary",
        "Canary",
        "2026-10-05T00:00:00Z",
        "canary secret unrelated trove payload",
    ))
    .unwrap();
    s
}

fn endpoint_of(config: &LlmConfig) -> PreparedEndpoint {
    PreparedEndpoint {
        base_url: config.base_url.clone(),
        model: config.model.clone(),
    }
}

#[test]
fn prepare_makes_zero_wire_requests() {
    let storage = seeded(6);
    let registry = PreparedAskRegistry::new();
    let stop = Arc::new(AtomicBool::new(false));
    let (base_url, server) = spawn_recorder(
        vec![
            // Replies a prepare must never fetch.
            "{\"match\": \"never\"}".into(),
            "the answer nothing may reach".into(),
        ],
        Arc::clone(&stop),
    );
    let config = LlmConfig {
        base_url,
        api_key: KEY.into(),
        model: "v2-ask-model".into(),
    };

    let cancel = || false;
    let mut progress = |_: u64, _: u64, _: &str| {};
    let outcome = prepare_scoped_ask(
        &storage,
        &HashEmbedder::new(),
        &registry,
        &endpoint_of(&config),
        "v2 ask marker vault",
        None,
        50,
        8,
        &cancel,
        &mut progress,
    )
    .unwrap();

    // The prepare answered locally: results, exact preview, digest.
    assert_eq!(outcome.results.len(), 6);
    let payload = outcome.payload.as_ref().expect("results → a payload");
    assert!(payload.user.contains("v2 ask marker"));
    assert!(!outcome.digest.is_empty());

    // Stop the recorder and prove nothing ever arrived.
    stop.store(true, Ordering::SeqCst);
    let requests = server.join().expect("recorder thread");
    assert!(
        requests.is_empty(),
        "prepare must make zero network requests, got {}",
        requests.len()
    );
}

#[test]
fn generate_sends_exactly_the_approved_payload_bytes() {
    let storage = seeded(6);
    let registry = PreparedAskRegistry::new();
    let stop = Arc::new(AtomicBool::new(false));
    let (base_url, server) = spawn_recorder(vec!["approved answer [1].".into()], Arc::clone(&stop));
    let config = LlmConfig {
        base_url,
        api_key: KEY.into(),
        model: "v2-ask-model".into(),
    };
    let backend = AnthropicLlm::new(config);

    let cancel = || false;
    let mut progress = |_: u64, _: u64, _: &str| {};
    let prepared = prepare_scoped_ask(
        &storage,
        &HashEmbedder::new(),
        &registry,
        &endpoint_of(backend.config()),
        "v2 ask marker vault",
        None,
        50,
        8,
        &cancel,
        &mut progress,
    )
    .unwrap();
    let payload = prepared.payload.clone().expect("results → a payload");

    let answer = generate_approved_answer(
        &storage,
        &registry,
        Some(&backend),
        &endpoint_of(backend.config()),
        prepared.prepared_id,
        &prepared.digest,
        &cancel,
        &mut progress,
    )
    .unwrap();
    assert_eq!(answer.answer.as_deref(), Some("approved answer [1]."));
    assert_eq!(answer.window_ids, prepared.window_ids);
    assert_eq!(answer.window_ids.len(), 6);

    stop.store(true, Ordering::SeqCst);
    let requests = server.join().expect("recorder thread");
    assert_eq!(requests.len(), 1, "generate makes exactly one round trip");

    let req = &requests[0];
    // The key rides only in the x-api-key header, never in the payload.
    assert!(
        req.headers
            .to_ascii_lowercase()
            .contains(&format!("x-api-key: {KEY}").to_ascii_lowercase()),
        "x-api-key header must carry the key"
    );
    assert!(
        !req.body.contains(KEY),
        "key leaked into the request payload"
    );
    assert_eq!(req.request_line, "POST /v1/messages HTTP/1.1");

    // The wire body is exactly the approved snapshot: system, user
    // content, and model — nothing else the user did not see.
    let body: serde_json::Value = serde_json::from_str(&req.body).expect("JSON body");
    assert_eq!(body["system"].as_str(), Some(payload.system.as_str()));
    assert_eq!(
        body["messages"][0]["content"].as_str(),
        Some(payload.user.as_str())
    );
    assert_eq!(body["model"].as_str(), Some("v2-ask-model"));
    // The canary row never entered the approved payload nor the wire.
    assert!(!req.body.contains("canary secret unrelated trove payload"));
}

#[test]
fn stale_digest_refusals_leave_nothing_sent() {
    let storage = seeded(4);
    let registry = PreparedAskRegistry::new();
    let stop = Arc::new(AtomicBool::new(false));
    let (base_url, server) = spawn_recorder(vec!["never served".into()], Arc::clone(&stop));
    let config = LlmConfig {
        base_url,
        api_key: KEY.into(),
        model: "v2-ask-model".into(),
    };
    let backend = AnthropicLlm::new(config);
    let endpoint = endpoint_of(backend.config());

    let cancel = || false;
    let mut progress = |_: u64, _: u64, _: &str| {};
    let prepared = prepare_scoped_ask(
        &storage,
        &HashEmbedder::new(),
        &registry,
        &endpoint,
        "v2 ask marker vault",
        None,
        50,
        8,
        &cancel,
        &mut progress,
    )
    .unwrap();

    // A tampered/wrong digest refuses (and leaves the approval intact).
    let err = generate_approved_answer(
        &storage,
        &registry,
        Some(&backend),
        &endpoint,
        prepared.prepared_id,
        "0000000000000000000000000000000000000000000000000000000000000000",
        &cancel,
        &mut progress,
    )
    .unwrap_err();
    assert_eq!(err, AskV2Error::StaleDigest);

    // A changed endpoint (provider config edited between prepare and
    // generate) refuses and consumes the approval.
    let moved = PreparedEndpoint {
        model: "v2-other-model".into(),
        ..endpoint.clone()
    };
    let err = generate_approved_answer(
        &storage,
        &registry,
        Some(&backend),
        &moved,
        prepared.prepared_id,
        &prepared.digest,
        &cancel,
        &mut progress,
    )
    .unwrap_err();
    assert_eq!(err, AskV2Error::StaleDigest);

    // A changed result snapshot (the archive grew) refuses a fresh
    // approval bound to the old revision.
    let snapshot = prepare_scoped_ask(
        &storage,
        &HashEmbedder::new(),
        &registry,
        &endpoint,
        "v2 ask marker vault",
        None,
        50,
        8,
        &cancel,
        &mut progress,
    )
    .unwrap();
    storage
        .insert_message(&NewMessage::new(
            "test",
            "fresh",
            "ana@example.com",
            "2026-10-20T12:00:00Z",
            "a fresh v2 ask marker row",
        ))
        .unwrap();
    let err = generate_approved_answer(
        &storage,
        &registry,
        Some(&backend),
        &endpoint,
        snapshot.prepared_id,
        &snapshot.digest,
        &cancel,
        &mut progress,
    )
    .unwrap_err();
    assert_eq!(err, AskV2Error::ArchiveChanged);

    // A newer prepare replaced the (consumed) approval: generating under
    // the old id refuses.
    let replaced = prepare_scoped_ask(
        &storage,
        &HashEmbedder::new(),
        &registry,
        &endpoint,
        "a different question entirely",
        None,
        50,
        8,
        &cancel,
        &mut progress,
    )
    .unwrap();
    let _ = replaced;
    let err = generate_approved_answer(
        &storage,
        &registry,
        Some(&backend),
        &endpoint,
        snapshot.prepared_id,
        &snapshot.digest,
        &cancel,
        &mut progress,
    )
    .unwrap_err();
    assert_eq!(err, AskV2Error::NoPendingPreparedAsk);

    stop.store(true, Ordering::SeqCst);
    let requests = server.join().expect("recorder thread");
    assert!(
        requests.is_empty(),
        "every stale approval must leave nothing sent, got {}",
        requests.len()
    );
}

#[test]
fn cancelled_between_steps_sends_nothing() {
    let storage = seeded(4);
    let registry = PreparedAskRegistry::new();
    let stop = Arc::new(AtomicBool::new(false));
    let (base_url, server) = spawn_recorder(vec!["never served".into()], Arc::clone(&stop));
    let config = LlmConfig {
        base_url,
        api_key: KEY.into(),
        model: "v2-ask-model".into(),
    };
    let backend = AnthropicLlm::new(config);
    let endpoint = endpoint_of(backend.config());

    let cancel = || false;
    let mut progress = |_: u64, _: u64, _: &str| {};
    let prepared = prepare_scoped_ask(
        &storage,
        &HashEmbedder::new(),
        &registry,
        &endpoint,
        "v2 ask marker vault",
        None,
        50,
        8,
        &cancel,
        &mut progress,
    )
    .unwrap();

    // The cancellation is observed at the checkpoint between approval
    // consumption and the send: the first checkpoint passes, the second
    // (immediately before the round trip) stops the ask.
    let checks = AtomicU64::new(0);
    let cancel_after_first = || checks.fetch_add(1, Ordering::SeqCst) >= 1;
    let err = generate_approved_answer(
        &storage,
        &registry,
        Some(&backend),
        &endpoint,
        prepared.prepared_id,
        &prepared.digest,
        &cancel_after_first,
        &mut progress,
    )
    .unwrap_err();
    assert_eq!(err, AskV2Error::Cancelled);

    stop.store(true, Ordering::SeqCst);
    let requests = server.join().expect("recorder thread");
    assert!(
        requests.is_empty(),
        "a cancelled ask sends nothing, got {}",
        requests.len()
    );
}

#[test]
fn prepare_respects_the_active_workspace_scope() {
    let storage = seeded(6);
    let registry = PreparedAskRegistry::new();
    let endpoint = PreparedEndpoint::local();
    // Author scope: only ana's rows may reach results, preview, and the
    // citation window.
    let scope = AskScope {
        author: Some("ana@example.com".into()),
        ..AskScope::default()
    };
    let cancel = || false;
    let mut progress = |_: u64, _: u64, _: &str| {};
    let outcome = prepare_scoped_ask(
        &storage,
        &HashEmbedder::new(),
        &registry,
        &endpoint,
        "v2 ask marker vault",
        Some(&scope),
        50,
        8,
        &cancel,
        &mut progress,
    )
    .unwrap();
    assert_eq!(outcome.results.len(), 3, "only ana's rows are in scope");
    assert!(outcome
        .results
        .iter()
        .all(|m| m.author == "ana@example.com"));
    let payload = outcome.payload.as_ref().expect("results → a payload");
    assert!(payload.user.contains("ana@example.com"));
    assert!(
        !payload.user.contains("bob@example.com"),
        "out-of-scope row leaked into the preview: {}",
        payload.user
    );
    assert_eq!(outcome.window_ids.len(), 3);
}

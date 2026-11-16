//! LLM-assisted search: natural-language question → FTS5 plan → RAG answer.
//!
//! Three local steps plus two optional LLM round trips per [`ask`]:
//! 1. **Translate**: the question becomes a [`QueryPlan`] — an FTS5 MATCH
//!    expression plus an optional sent-date range. The expression is vetted
//!    against the index ([`Storage::is_valid_match`]) and replaced by a plain
//!    term search whenever the LLM output is unusable.
//! 2. **Rank**: the plan runs locally against FTS5, then — when local
//!    embeddings exist — a vector leg is scanned and both ranked lists are
//!    fused with Reciprocal Rank Fusion ([`rank_hybrid`]). Fully local.
//! 3. **Summarize** (RAG): a short answer synthesized from *only* the top
//!    search-result rows — never the whole archive. This is the product's
//!    privacy promise in LLM mode: only the question and the returned rows
//!    leave the machine, and only when an API key is configured.
//!
//! The provider is any Anthropic-compatible `/v1/messages` endpoint (default:
//! the z.ai GLM endpoint), configured purely through environment variables;
//! the key is never persisted. With no key everything still works: [`ask`]
//! degrades to plain FTS5 term search (plus local hybrid ranking when
//! embeddings exist) with no network traffic.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{Datelike, NaiveDate};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::extract::CollectedLink;
use crate::message::Message;
use crate::search::build_match_query;
use crate::storage::Storage;
use crate::vector::{rrf_fuse, Embedder};
use crate::{Error, Result};

/// How many of the top search results feed the summary, by default.
pub const DEFAULT_SUMMARIZE_TOP: usize = 8;
/// Per-request timeout for LLM calls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// `max_tokens` sent with each request.
const MAX_TOKENS: u32 = 1024;
/// Hard cap on the length of an LLM-produced MATCH expression.
const MAX_MATCH_LEN: usize = 512;
/// Hard cap on the length of an LLM-produced `from` filter.
const MAX_FROM_LEN: usize = 128;
/// Characters of each result body included in the summary prompt.
const SUMMARY_BODY_LIMIT: usize = 500;
/// Lines of collected links appended to the summary prompt (privacy cap).
pub const SUMMARY_LINK_LIMIT: usize = 50;

// ---------------------------------------------------------------------------
// Configuration and provider
// ---------------------------------------------------------------------------

/// Environment-only configuration for the LLM endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmConfig {
    /// Base URL of an Anthropic-compatible endpoint, without `/v1/messages`.
    pub base_url: String,
    /// API key. Read from the environment only; never stored or logged.
    pub api_key: String,
    /// Model name (e.g. `"glm-5"`).
    pub model: String,
}

/// Host of an endpoint URL for provider comparison: semantic parsing via
/// `Url` (lowercase host, userinfo and port excluded), with a single
/// trailing root dot stripped; `None` when the URL does not parse, which
/// every comparison treats as not the z.ai provider.
pub(crate) fn endpoint_host(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    Some(host.strip_suffix('.').unwrap_or(host).to_string())
}

/// Whether `url` is plaintext `http` pointed at a non-loopback host —
/// the one shape refused before any key is sent (mirrors the
/// settings-file policy for provider profiles).
pub fn plaintext_http_off_loopback(url: &str) -> bool {
    Url::parse(url)
        .map(|u| u.scheme() == "http" && !crate::settings::is_loopback_url(url))
        .unwrap_or(false)
}

impl LlmConfig {
    pub const DEFAULT_BASE_URL: &'static str = "https://api.z.ai/api/anthropic";
    pub const DEFAULT_MODEL: &'static str = "glm-5";

    /// Read from the process environment; `Ok(None)` (LLM disabled) when no
    /// API key is set, `Err` when a key is paired with an endpoint from a
    /// different provider.
    ///
    /// Provider selection: `ZAI_API_KEY` / `GLM_API_KEY` select the default
    /// z.ai endpoint ([`Self::DEFAULT_BASE_URL`]); `ANTHROPIC_API_KEY` is
    /// honored only together with an explicit `ANTHROPIC_BASE_URL`, so a key
    /// is never sent to a provider it does not belong to.
    pub fn from_env() -> Result<Option<Self>> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Pure variant of [`Self::from_env`] for tests.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Option<Self>> {
        let value = |name: &str| {
            lookup(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let base_url = value("ANTHROPIC_BASE_URL");
        let anthropic_key = value("ANTHROPIC_API_KEY");
        let zai_key = value("ZAI_API_KEY").or_else(|| value("GLM_API_KEY"));
        let is_zai_provider =
            |url: &str| endpoint_host(url) == endpoint_host(Self::DEFAULT_BASE_URL);
        let (api_key, base_url) = match (anthropic_key, zai_key, base_url) {
            // An Anthropic key is never sent to a z.ai-host endpoint, even
            // when explicitly paired with it there; only its own host honors it.
            (Some(_), _, Some(url)) if is_zai_provider(&url) => {
                return Err(Error::Llm(
                    "ANTHROPIC_API_KEY is paired with the default z.ai endpoint: refusing to \
                     send an Anthropic key to z.ai. Point ANTHROPIC_BASE_URL at the endpoint \
                     that accepts this key, or use ZAI_API_KEY / GLM_API_KEY instead."
                        .into(),
                ))
            }
            // An Anthropic key is honored only with its explicitly paired endpoint.
            (Some(key), _, Some(url)) => (key, url),
            (Some(_), _, None) => {
                return Err(Error::Llm(
                    "ANTHROPIC_API_KEY is set but ANTHROPIC_BASE_URL is not: refusing to send \
                     an Anthropic key to the default z.ai endpoint. Set ANTHROPIC_BASE_URL to \
                     the endpoint that accepts this key, or unset ANTHROPIC_API_KEY and use \
                     ZAI_API_KEY / GLM_API_KEY instead."
                        .into(),
                ))
            }
            // z.ai keys pair with z.ai-host endpoints only.
            (None, Some(key), Some(url)) if is_zai_provider(&url) => (key, url),
            (None, Some(_), Some(url)) => {
                return Err(Error::Llm(format!(
                    "ANTHROPIC_BASE_URL={url} does not match the z.ai key (ZAI_API_KEY / \
                     GLM_API_KEY); refusing to send it to a different provider. Unset \
                     ANTHROPIC_BASE_URL to use the default z.ai endpoint."
                )))
            }
            (None, Some(key), None) => (key, Self::DEFAULT_BASE_URL.into()),
            // No API key: LLM stays off.
            (None, None, _) => return Ok(None),
        };
        Ok(Some(Self {
            base_url,
            model: value("MAILCHAT_LLM_MODEL").unwrap_or_else(|| Self::DEFAULT_MODEL.into()),
            api_key,
        }))
    }

    /// Host part of the base URL, for display (e.g. `"api.z.ai"`); the raw
    /// base URL itself when it does not parse as an absolute URL.
    pub fn host(&self) -> String {
        endpoint_host(&self.base_url).unwrap_or_else(|| self.base_url.clone())
    }

    /// One minimal `/v1/messages` round trip (`max_tokens: 1`) that verifies
    /// base URL, API key, and model together — the Settings "Test
    /// connection" button. Nothing from the vault is sent (a fixed "ping"
    /// message) and the reply text is ignored: a 2xx with a readable body is
    /// success. Errors are classified and user-facing.
    pub fn test_connection(&self) -> Result<()> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 1,
            "messages": [{ "role": "user", "content": "ping" }],
        });
        let response = ureq::post(&url)
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", "2023-06-01")
            .timeout(REQUEST_TIMEOUT)
            .send_json(body);
        match response {
            Ok(response) => response
                .into_json::<serde_json::Value>()
                .map(|_| ())
                .map_err(|e| Error::Llm(format!("unreadable response body: {e}"))),
            Err(ureq::Error::Status(code, response)) => {
                let body = response.into_string().unwrap_or_default();
                Err(Error::Llm(classify_status(code, &body, &self.host())))
            }
            Err(e) => Err(Error::Llm(format!("could not reach {}: {e}", self.host()))),
        }
    }
}

/// Turn a non-2xx test response into a short, actionable message.
fn classify_status(code: u16, body: &str, host: &str) -> String {
    match code {
        401 | 403 => format!("invalid API key (HTTP {code})"),
        404 => format!("no endpoint at this base URL (HTTP 404) — check the base URL ({host})"),
        _ => {
            let detail = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|v| {
                    v["error"]["message"]
                        .as_str()
                        .or_else(|| v["message"].as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| body.chars().take(200).collect());
            if detail.is_empty() {
                format!("HTTP {code}")
            } else {
                format!("HTTP {code}: {detail}")
            }
        }
    }
}

/// A chat-completion backend: one system + user exchange.
///
/// Implemented by [`AnthropicLlm`] (real endpoint) and [`MockLlm`] (tests);
/// the pipeline functions below accept any `&dyn Llm`.
pub trait Llm: Send + Sync {
    /// Complete one exchange, returning the assistant's text.
    fn complete(&self, system: &str, user: &str) -> Result<String>;
}

/// [`Llm`] over an Anthropic-compatible `/v1/messages` endpoint (z.ai GLM).
#[derive(Debug, Clone)]
pub struct AnthropicLlm {
    config: LlmConfig,
}

impl AnthropicLlm {
    pub fn new(config: LlmConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }
}

impl Llm for AnthropicLlm {
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        let url = format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.config.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
        });
        let response = ureq::post(&url)
            .set("x-api-key", &self.config.api_key)
            .set("anthropic-version", "2023-06-01")
            .timeout(REQUEST_TIMEOUT)
            .send_json(body)
            .map_err(|e| Error::Llm(format!("request to {} failed: {e}", self.config.host())))?;
        let parsed: serde_json::Value = response
            .into_json()
            .map_err(|e| Error::Llm(format!("unreadable response body: {e}")))?;
        // GLM replies may lead with a `thinking` block; keep only text parts.
        let mut text = String::new();
        if let Some(parts) = parsed["content"].as_array() {
            for part in parts {
                if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                        text.push_str(t);
                    }
                }
            }
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::Llm("model returned no text".into()));
        }
        Ok(text.to_string())
    }
}

/// Deterministic [`Llm`] for tests: replies in order, records every prompt.
#[derive(Debug, Default)]
pub struct MockLlm {
    replies: Mutex<VecDeque<String>>,
    prompts: Mutex<Vec<(String, String)>>,
    fail_with: Option<String>,
}

impl MockLlm {
    /// Reply to each `complete` call with the next entry, in order.
    pub fn with_replies(replies: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            replies: Mutex::new(replies.into_iter().map(Into::into).collect()),
            prompts: Mutex::new(Vec::new()),
            fail_with: None,
        }
    }

    /// Always fail with this message.
    pub fn failing(message: &str) -> Self {
        Self {
            replies: Mutex::new(VecDeque::new()),
            prompts: Mutex::new(Vec::new()),
            fail_with: Some(message.into()),
        }
    }

    /// All prompts seen so far, in order: `(system, user)`.
    pub fn prompts(&self) -> Vec<(String, String)> {
        self.prompts.lock().unwrap().clone()
    }
}

impl Llm for MockLlm {
    fn complete(&self, system: &str, user: &str) -> Result<String> {
        self.prompts
            .lock()
            .unwrap()
            .push((system.to_string(), user.to_string()));
        if let Some(message) = &self.fail_with {
            return Err(Error::Llm(message.clone()));
        }
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| Error::Llm("mock has no reply left".into()))
    }
}

// ---------------------------------------------------------------------------
// Query plan (step 1: translate)
// ---------------------------------------------------------------------------

/// A validated search plan derived from a natural-language question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPlan {
    /// Raw FTS5 MATCH expression over `body`/`author`.
    pub match_expr: String,
    /// Inclusive lower bound on `sent_at` (`YYYY-MM-DD`), if any.
    pub date_from: Option<String>,
    /// Inclusive upper bound on `sent_at` (`YYYY-MM-DD`), if any.
    pub date_to: Option<String>,
    /// Case-insensitive substring the sender (`author`) must contain.
    pub from: Option<String>,
    /// `Some(true)`: only messages whose body contains a URL. `Some(false)`
    /// is ignored (treated as `None`).
    pub has_link: Option<bool>,
    /// Collect http(s) links from the results into `AskOutcome::collected`.
    pub extract_links: bool,
}

impl QueryPlan {
    /// Plain term-search fallback for `question` — exactly what non-LLM
    /// search does with the same input.
    pub fn plain(question: &str) -> Self {
        Self {
            match_expr: build_match_query(question),
            date_from: None,
            date_to: None,
            from: None,
            has_link: None,
            extract_links: false,
        }
    }

    /// Parse the model's translation reply into a plan, validating shape,
    /// expression length, and date format. Everything is data here: the
    /// expression is a value to be bound as a MATCH parameter, never SQL.
    fn from_llm_reply(reply: &str) -> Result<Self> {
        let raw: RawPlan = serde_json::from_str(
            extract_json_object(reply)
                .ok_or_else(|| Error::Llm("no JSON object in translation reply".into()))?,
        )
        .map_err(|e| Error::Llm(format!("malformed translation JSON: {e}")))?;
        let match_expr = raw.match_field.trim().to_string();
        if match_expr.is_empty() {
            return Err(Error::Llm("translation produced an empty match".into()));
        }
        if match_expr.len() > MAX_MATCH_LEN {
            return Err(Error::Llm("translation match too long".into()));
        }
        let has_link = raw.has_link.filter(|&b| b); // only Some(true) is a filter
        Ok(Self {
            match_expr,
            date_from: canonical_date(&raw.date_from)?,
            date_to: canonical_date(&raw.date_to)?,
            from: canonical_from(&raw.from)?,
            has_link,
            extract_links: raw.extract_links,
        })
    }
}

/// Parse an optional model-supplied date and return it in canonical
/// zero-padded `%Y-%m-%d` form, so it compares correctly as text. Years
/// outside `0000..=9999` are rejected: they cannot be ordered as text
/// against the stored RFC 3339 `sent_at` strings.
fn canonical_date(value: &Option<String>) -> Result<Option<String>> {
    value
        .as_deref()
        .map(|s| {
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .filter(|d| (0..=9999).contains(&d.year()))
                .map(|d| d.to_string())
                .ok_or_else(|| Error::Llm(format!("translation produced a bad date: {s}")))
        })
        .transpose()
}

/// Parse an optional model-supplied sender filter: trimmed, empty → `None`,
/// longer than [`MAX_FROM_LEN`] → plan rejected.
fn canonical_from(value: &Option<String>) -> Result<Option<String>> {
    match value.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.chars().count() <= MAX_FROM_LEN => Ok(Some(s.to_string())),
        Some(s) => Err(Error::Llm(format!(
            "translation produced a too-long from filter: {s}"
        ))),
    }
}

/// Shape of the model's translation reply.
#[derive(Deserialize)]
struct RawPlan {
    #[serde(rename = "match")]
    match_field: String,
    #[serde(default)]
    date_from: Option<String>,
    #[serde(default)]
    date_to: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    has_link: Option<bool>,
    #[serde(default)]
    extract_links: bool,
}

/// The JSON object inside a reply: the whole trimmed text when it starts with
/// `{`, otherwise the span from the first `{` to the last `}` (skips fences
/// and surrounding prose on a best-effort basis).
fn extract_json_object(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        return Some(trimmed);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (end > start).then(|| &trimmed[start..=end])
}

fn translation_system_prompt(today: NaiveDate) -> String {
    let mut prompt = String::from(
        "You convert a natural-language question about a personal message archive into a search plan.\n\n\
         The archive is a SQLite FTS5 full-text index with three columns: `body` (message text; for email it includes the subject), `author` (display name or address), and `subject` (email subject lines; chat messages have none).\n\n\
         FTS5 MATCH syntax you may use: quoted phrases (\"exact phrase\"), prefix terms (\"proj*\"), AND (implicit between terms), OR, NOT, parentheses; terms are case-insensitive; the column filters `body:`, `author:`, and `subject:` are allowed.\n\n\
         Also extract any date range the question implies (for example \"last march\", \"before June\", \"in 2024\"), resolving relative wording against today's date: ",
    );
    prompt.push_str(&today.to_string());
    prompt.push_str(
        ".\n\n\
         Reply with ONLY a JSON object — no markdown fence, no prose — exactly in this shape:\n\
         {\"match\": \"<FTS5 MATCH expression>\", \"date_from\": \"<YYYY-MM-DD or null>\", \"date_to\": \"<YYYY-MM-DD or null>\", \"from\": \"<substring of the sender's name or address, or null>\", \"has_link\": <true or null>, \"extract_links\": <true or false>}\n\n\
         Rules: the match expression must be non-empty, valid FTS5, and use only terms implied by the question; prefer a few precise quoted terms over long expressions. Use \"from\" when the question names a sender; set \"has_link\": true when the question asks for messages containing links; set \"extract_links\": true only when the question explicitly asks to collect, list, or gather links — then also set \"has_link\": true.",
    );
    prompt
}

/// Step 1 of the pipeline: translate a question into a [`QueryPlan`]
/// (one LLM round trip; no storage access).
pub fn translate(llm: &dyn Llm, question: &str, today: NaiveDate) -> Result<QueryPlan> {
    let reply = llm.complete(
        &translation_system_prompt(today),
        &format!("Question: {question}"),
    )?;
    QueryPlan::from_llm_reply(&reply)
}

// ---------------------------------------------------------------------------
// Validation and execution (step 2)
// ---------------------------------------------------------------------------

/// Where the plan actually used by a search came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanSource {
    /// The LLM's plan was accepted.
    Llm,
    /// An LLM is configured but its plan was unusable; plain term search.
    LlmFallback,
    /// No LLM configured; plain term search.
    NoLlm,
    /// Local-only mode was chosen for this ask (provider menu); nothing
    /// was sent anywhere.
    Local,
}

/// A [`QueryPlan`] plus its provenance and any degradation notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedQuery {
    pub plan: QueryPlan,
    pub source: PlanSource,
    pub notes: Vec<String>,
}

/// Vet an LLM-produced plan candidate against the index, falling back to a
/// plain term search when it cannot be used. Usable date filters survive the
/// fallback; only the MATCH expression is replaced.
pub fn validate(storage: &Storage, question: &str, candidate: Result<QueryPlan>) -> PlannedQuery {
    match candidate {
        Ok(plan) if storage.is_valid_match(&plan.match_expr) => PlannedQuery {
            plan,
            source: PlanSource::Llm,
            notes: Vec::new(),
        },
        Ok(plan) => PlannedQuery {
            plan: QueryPlan {
                match_expr: build_match_query(question),
                date_from: plan.date_from,
                date_to: plan.date_to,
                from: plan.from,
                has_link: plan.has_link,
                extract_links: plan.extract_links,
            },
            source: PlanSource::LlmFallback,
            notes: vec![
                "LLM match expression rejected as invalid FTS5; used a plain term search (date and sender filters kept)"
                    .into(),
            ],
        },
        Err(e) => PlannedQuery {
            plan: QueryPlan::plain(question),
            source: PlanSource::LlmFallback,
            notes: vec![format!("LLM translation failed ({e}); used a plain term search")],
        },
    }
}

/// Step 2 of the pipeline: execute a planned query against storage.
pub fn run_plan(storage: &Storage, planned: &PlannedQuery, limit: u32) -> Result<Vec<Message>> {
    run_plan_scoped(storage, planned, None, limit)
}

/// Step 2 of the pipeline under an optional validated workspace scope
/// ([`crate::search2::ResolvedAskScope`]): the scope constrains retrieval
/// alongside the plan, intersecting date bounds (never broadening them)
/// and returning no results when the intersection is empty — never an
/// unscoped fallback.
pub fn run_plan_scoped(
    storage: &Storage,
    planned: &PlannedQuery,
    scope: Option<&crate::search2::ResolvedAskScope>,
    limit: u32,
) -> Result<Vec<Message>> {
    if planned.plan.match_expr.is_empty() {
        return Ok(Vec::new());
    }
    storage.search_filtered_scoped(
        &planned.plan.match_expr,
        planned.plan.date_from.as_deref(),
        planned.plan.date_to.as_deref(),
        planned.plan.from.as_deref(),
        planned.plan.has_link.unwrap_or(false),
        scope,
        limit,
    )
}

// ---------------------------------------------------------------------------
// Hybrid ranking (FTS5 + local vectors, step 2b — fully local)
// ---------------------------------------------------------------------------

/// How many vector candidates feed fusion (before truncating to `limit`).
/// Enough depth for RRF to matter, still a bounded scan cost.
pub const VECTOR_CANDIDATES: u32 = 200;

/// Which legs produced the ranked result list the Ask pipeline returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RankingMode {
    /// FTS5 only (no local embeddings exist, or none matched).
    FtsOnly,
    /// FTS5 and the local vector leg were fused (reciprocal rank fusion).
    HybridFtsVector,
    /// The vector leg only (FTS5 matched nothing).
    VectorOnly,
}

/// Outcome of the hybrid ranking step: fused, hydrated results plus how
/// they were ranked.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridRanking {
    pub results: Vec<Message>,
    pub ranking: RankingMode,
}

/// Rank the FTS leg against the local vector leg and fuse the two lists.
///
/// The question is embedded with `embedder` and scanned against the
/// archive's embeddings under the plan's date bounds (same local-day
/// semantics as the FTS leg); both ranked id lists are then fused with
/// [`rrf_fuse`]. Without embeddings — or when none exist at all — the FTS
/// order stands unchanged ([`RankingMode::FtsOnly`]), so the pipeline never
/// depends on the vector leg. (Embedding coverage is surfaced by the UI's
/// status line; `notes` stays reserved for degradation.)
pub fn rank_hybrid(
    storage: &Storage,
    embedder: &dyn Embedder,
    question: &str,
    planned: &PlannedQuery,
    fts_results: &[Message],
    limit: u32,
) -> Result<HybridRanking> {
    rank_hybrid_scoped(
        storage,
        embedder,
        question,
        planned,
        fts_results,
        None,
        limit,
    )
}

/// [`rank_hybrid`] under an optional validated workspace scope: the
/// vector leg applies exactly the filters the FTS leg applied (including
/// the plan/scope date intersection), so both legs filter identically
/// and only in-scope messages can reach the fused ranking — and from
/// there citations and any consented summary.
pub fn rank_hybrid_scoped(
    storage: &Storage,
    embedder: &dyn Embedder,
    question: &str,
    planned: &PlannedQuery,
    fts_results: &[Message],
    scope: Option<&crate::search2::ResolvedAskScope>,
    limit: u32,
) -> Result<HybridRanking> {
    let counts = storage.embedding_counts(embedder.id())?;
    let vector_ids: Vec<i64> = if counts.embedded == 0 {
        Vec::new()
    } else {
        storage
            .vector_scan_scoped(
                embedder.id(),
                embedder.dim(),
                &embedder.embed(question),
                planned.plan.date_from.as_deref(),
                planned.plan.date_to.as_deref(),
                planned.plan.from.as_deref(),
                planned.plan.has_link.unwrap_or(false),
                scope,
                VECTOR_CANDIDATES.max(limit),
            )?
            .into_iter()
            .map(|hit| hit.message_id)
            .collect()
    };
    let fts_ids: Vec<i64> = fts_results.iter().map(|m| m.id).collect();

    let (ids, ranking) = match (fts_ids.is_empty(), vector_ids.is_empty()) {
        (_, true) => (fts_ids, RankingMode::FtsOnly),
        (true, false) => {
            let mut only = vector_ids;
            only.truncate(limit as usize);
            (only, RankingMode::VectorOnly)
        }
        (false, false) => (
            rrf_fuse(&fts_ids, &vector_ids, limit as usize),
            RankingMode::HybridFtsVector,
        ),
    };
    let results = storage.get_many(&ids)?;
    Ok(HybridRanking { results, ranking })
}

// ---------------------------------------------------------------------------
// Summary (step 3, RAG over top results only)
// ---------------------------------------------------------------------------

const SUMMARY_SYSTEM_PROMPT: &str =
    "You answer a question using only a numbered list of archive messages supplied by the user.\n\
     Rules:\n\
     - Answer in the same language as the question.\n\
     - At most 6 sentences.\n\
     - Use only the listed messages; cite them inline as [1], [2] in order of reliance.\n\
     - If the messages do not answer the question, say so plainly and suggest better search terms.";

/// One numbered, truncated context block per message.
fn context_block(msg: &Message, index: usize) -> String {
    let mut body: String = msg.body.chars().take(SUMMARY_BODY_LIMIT).collect();
    if msg.body.chars().count() > SUMMARY_BODY_LIMIT {
        body.push('…');
    }
    let author = if msg.author.is_empty() {
        "unknown"
    } else {
        &msg.author
    };
    let date = if msg.sent_at.is_empty() {
        "date unknown"
    } else {
        &msg.sent_at
    };
    format!("[{index}] author: {author} · {date}\n{body}")
}

/// Prompt order for the summary context blocks, given `n` ranked
/// messages (0-based ranks): all even indices ascending first, then all
/// odd indices descending — n=8 → `0,2,4,6,7,5,3,1` (1-based
/// `1,3,5,7,8,6,4,2`).
///
/// This is the "Lost in the Middle" placement (Liu et al. 2023,
/// "Lost in the Middle: How Language Models Use Long Contexts",
/// arXiv:2307.03172): positional degradation is largest mid-context, so
/// the strongest evidence (lowest ranks) sits at the beginning and the
/// end of the prompt. Citation numbers still follow rank, not position.
fn lim_order(n: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).filter(|i| i % 2 == 0).collect();
    order.extend((0..n).filter(|i| i % 2 == 1).rev());
    order
}

/// Outcome of the summary step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summarized {
    pub answer: Option<String>,
    pub notes: Vec<String>,
}

/// Step 3 of the pipeline: synthesize a short answer over the top `top`
/// results. Only those rows (truncated) are sent — the privacy guarantee.
/// Skipped entirely when there are no results. Blocks are placed in
/// [`lim_order`] (strongest evidence at the prompt's edges) while their
/// `[k]` numbers stay by rank. When `collected` is non-empty, up to
/// [`SUMMARY_LINK_LIMIT`] `url · author · date` lines are appended and the
/// system prompt gains a rule for describing the links; with no collected
/// links both prompts are built exactly as before.
pub fn summarize_results(
    llm: &dyn Llm,
    question: &str,
    results: &[Message],
    top: usize,
    collected: &[CollectedLink],
) -> Summarized {
    if results.is_empty() {
        return Summarized {
            answer: None,
            notes: Vec::new(),
        };
    }
    let top_messages: Vec<&Message> = results.iter().take(top).collect();
    let contexts: String = lim_order(top_messages.len())
        .into_iter()
        .map(|i| context_block(top_messages[i], i + 1))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut user =
        format!("Question: {question}\n\nMessages (numbered by relevance):\n\n{contexts}");
    let mut system = SUMMARY_SYSTEM_PROMPT.to_string();
    if !collected.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        'links: for link in collected {
            for hit in &link.hits {
                if lines.len() >= SUMMARY_LINK_LIMIT {
                    break 'links;
                }
                let author = if hit.author.is_empty() {
                    "unknown"
                } else {
                    &hit.author
                };
                let date = if hit.sent_at.is_empty() {
                    "date unknown"
                } else {
                    &hit.sent_at
                };
                lines.push(format!("{} · {} · {}", link.url, author, date));
            }
        }
        user.push_str("\n\nCollected links (from the messages above):\n");
        user.push_str(&lines.join("\n"));
        system.push_str(
            "\n- When a collected-links list is present, describe what the links are and who sent them; cite the numbered messages.",
        );
    }
    match llm.complete(&system, &user) {
        Ok(answer) => Summarized {
            answer: Some(answer),
            notes: Vec::new(),
        },
        Err(e) => Summarized {
            answer: None,
            notes: vec![format!(
                "LLM summary failed ({e}); showing raw results only"
            )],
        },
    }
}

// ---------------------------------------------------------------------------
// Pipeline composition
// ---------------------------------------------------------------------------

/// Everything one "ask" returns: the plan used, how the results were
/// ranked, the results, the RAG answer, any collected links, and
/// human-readable degradation notes for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskOutcome {
    pub plan: QueryPlan,
    pub plan_source: PlanSource,
    pub ranking: RankingMode,
    pub results: Vec<Message>,
    pub answer: Option<String>,
    /// Collected links when the plan extracted them; empty otherwise.
    pub collected: Vec<CollectedLink>,
    pub notes: Vec<String>,
}

impl AskOutcome {
    /// Outcome for an empty question: nothing searched, nothing sent.
    pub fn empty() -> Self {
        Self {
            plan: QueryPlan {
                match_expr: String::new(),
                date_from: None,
                date_to: None,
                from: None,
                has_link: None,
                extract_links: false,
            },
            plan_source: PlanSource::NoLlm,
            ranking: RankingMode::FtsOnly,
            results: Vec::new(),
            answer: None,
            collected: Vec::new(),
            notes: Vec::new(),
        }
    }
}

/// The reference pipeline: translate → validate → rank (FTS + local
/// vectors) → summarize.
///
/// Used directly by tests and any future caller that already owns the
/// storage handle. The Tauri command composes the same steps individually so
/// that no storage lock is held across an LLM round trip.
#[allow(clippy::too_many_arguments)]
pub fn ask(
    storage: &Storage,
    llm: Option<&dyn Llm>,
    embedder: &dyn Embedder,
    question: &str,
    limit: u32,
    summarize_top: usize,
    today: NaiveDate,
) -> Result<AskOutcome> {
    let planned = match llm {
        Some(llm) => validate(storage, question, translate(llm, question, today)),
        None => PlannedQuery {
            plan: QueryPlan::plain(question),
            source: PlanSource::NoLlm,
            notes: Vec::new(),
        },
    };
    let fts_results = run_plan(storage, &planned, limit)?;
    let hybrid = rank_hybrid(storage, embedder, question, &planned, &fts_results, limit)?;
    let results = hybrid.results;
    let collected = if planned.plan.extract_links {
        crate::extract::extract_links(&results)
    } else {
        Vec::new()
    };
    let (answer, summary_notes) = match llm {
        Some(llm) => {
            let s = summarize_results(llm, question, &results, summarize_top, &collected);
            (s.answer, s.notes)
        }
        None => (None, Vec::new()),
    };
    let mut notes = planned.notes;
    notes.extend(summary_notes);
    Ok(AskOutcome {
        plan: planned.plan,
        plan_source: planned.source,
        ranking: hybrid.ranking,
        results,
        answer,
        collected,
        notes,
    })
}

/// What the UI needs to know about LLM availability. Never exposes the key.
///
/// The scalar fields describe the **default** selection (what an ask with
/// no explicit provider uses); `providers` is the full provider menu for
/// the Ask UI (local-only, configured profiles, environment fallback).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmStatus {
    pub enabled: bool,
    /// e.g. `"glm-5"`; empty when disabled.
    pub model: String,
    /// Endpoint host, e.g. `"api.z.ai"`; empty when disabled.
    pub host: String,
    /// How many top results feed the summary.
    pub summarize_top: usize,
    /// The provider menu (see [`crate::settings::build_menu`]).
    pub providers: Vec<crate::settings::ProviderMenuItem>,
    /// Id of the default provider (`None` = local-only when nothing is
    /// configured; `Some("env")` when the environment fallback is used).
    pub default_provider: Option<String>,
}

impl LlmStatus {
    /// Status for a local-only setup with no provider menu extras (tests).
    pub fn local_only() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            host: String::new(),
            summarize_top: DEFAULT_SUMMARIZE_TOP,
            providers: vec![crate::settings::ProviderMenuItem {
                id: crate::settings::LOCAL_ID.to_string(),
                name: "Local full-text".to_string(),
                kind: crate::settings::LOCAL_ID.to_string(),
                model: String::new(),
                host: String::new(),
                available: true,
            }],
            default_provider: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::extract_links;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
    }

    #[test]
    fn config_lookup_requires_a_key() {
        assert!(LlmConfig::from_lookup(|_| None).unwrap().is_none());
        assert!(
            LlmConfig::from_lookup(|name| (name == "ANTHROPIC_API_KEY").then(|| "  ".into()))
                .unwrap()
                .is_none()
        );

        let cfg = LlmConfig::from_lookup(|name| (name == "ZAI_API_KEY").then(|| "k".into()))
            .unwrap()
            .expect("ZAI key enables the LLM");
        assert_eq!(cfg.api_key, "k");
        assert_eq!(cfg.base_url, LlmConfig::DEFAULT_BASE_URL);
        assert_eq!(cfg.model, LlmConfig::DEFAULT_MODEL);
    }

    #[test]
    fn anthropic_key_needs_explicit_base_url() {
        // ANTHROPIC_API_KEY alone is refused (would go to the z.ai endpoint).
        let err = LlmConfig::from_lookup(|name| (name == "ANTHROPIC_API_KEY").then(|| "k".into()))
            .unwrap_err();
        assert!(err.to_string().contains("ANTHROPIC_BASE_URL"));

        // Even an explicit z.ai URL never receives an Anthropic key.
        let err = LlmConfig::from_lookup(|name| {
            (name == "ANTHROPIC_API_KEY" || name == "ANTHROPIC_BASE_URL").then(|| {
                if name == "ANTHROPIC_API_KEY" {
                    "k".to_string()
                } else {
                    LlmConfig::DEFAULT_BASE_URL.to_string()
                }
            })
        })
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("refusing to send an Anthropic key to z.ai"));

        // With an explicit base URL it is honored, and that URL is used.
        let cfg = LlmConfig::from_lookup(|name| match name {
            "ANTHROPIC_API_KEY" => Some("k".into()),
            "ANTHROPIC_BASE_URL" => Some("https://api.anthropic.com".into()),
            "MAILCHAT_LLM_MODEL" => Some("m1".into()),
            _ => None,
        })
        .unwrap()
        .expect("explicit URL pairs the Anthropic key");
        assert_eq!(
            (
                cfg.api_key.as_str(),
                cfg.base_url.as_str(),
                cfg.model.as_str()
            ),
            ("k", "https://api.anthropic.com", "m1")
        );
        assert_eq!(cfg.host(), "api.anthropic.com");
    }

    #[test]
    fn zai_key_is_refused_on_a_foreign_endpoint() {
        let err = LlmConfig::from_lookup(|name| match name {
            "GLM_API_KEY" => Some("k2".into()),
            "ANTHROPIC_BASE_URL" => Some("https://example.invalid/api".into()),
            _ => None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("refusing to send"));

        // The explicit z.ai URL itself is accepted (redundant but consistent).
        let cfg = LlmConfig::from_lookup(|name| match name {
            "ZAI_API_KEY" => Some("k".into()),
            "ANTHROPIC_BASE_URL" => Some(LlmConfig::DEFAULT_BASE_URL.into()),
            _ => None,
        })
        .unwrap()
        .expect("explicit z.ai URL matches the z.ai key");
        assert_eq!(cfg.base_url, LlmConfig::DEFAULT_BASE_URL);
    }

    #[test]
    fn url_variants_cannot_bypass_provider_pairing() {
        // Any spelling of the z.ai host is the z.ai provider: an Anthropic
        // key paired with any of these is refused.
        for variant in [
            "https://api.z.ai",
            "https://api.z.ai/",
            "https://api.z.ai/api/anthropic",
            "https://api.z.ai/api/anthropic/",
            "https://api.z.ai/api/anthropic///",
            "https://API.Z.AI/api/anthropic",
            "https://api.z.ai:443/api/anthropic",
            "https://api.z.ai:/api/anthropic",
            "https://api.z.ai./api/anthropic",
            "https://user@api.z.ai/api/anthropic",
            "https://proxy:pw@api.z.ai/api/anthropic",
            "http://api.z.ai/api/anthropic",
            "https://api.z.ai/api/Anthropic",
            "https://api.z.ai/some/other/path",
        ] {
            let err = LlmConfig::from_lookup(|name| match name {
                "ANTHROPIC_API_KEY" => Some("k".into()),
                "ANTHROPIC_BASE_URL" => Some(variant.into()),
                _ => None,
            })
            .unwrap_err();
            assert!(
                err.to_string()
                    .contains("refusing to send an Anthropic key to z.ai"),
                "variant should be refused: {variant}"
            );
        }

        // The same spellings, paired with a z.ai key, are accepted as the
        // z.ai provider.
        for variant in [
            "https://api.z.ai",
            "https://api.z.ai/api/anthropic/",
            "https://API.Z.AI/api/anthropic",
            "https://api.z.ai:443/api/anthropic",
            "https://api.z.ai:/api/anthropic",
            "https://api.z.ai./api/anthropic",
            "https://user@api.z.ai/api/anthropic",
            "http://api.z.ai/api/anthropic",
        ] {
            let cfg = LlmConfig::from_lookup(|name| match name {
                "ZAI_API_KEY" => Some("k".into()),
                "ANTHROPIC_BASE_URL" => Some(variant.into()),
                _ => None,
            })
            .unwrap()
            .expect("variant is the z.ai provider");
            assert_eq!(cfg.base_url, variant);
        }

        // A different host fails closed in both directions.
        for key in ["ZAI_API_KEY", "GLM_API_KEY"] {
            let err = LlmConfig::from_lookup(|name| match name {
                name if name == key => Some("k".into()),
                "ANTHROPIC_BASE_URL" => Some("https://sub.api.z.ai.example.com/api".into()),
                _ => None,
            })
            .unwrap_err();
            assert!(err.to_string().contains("refusing to send"));
        }
    }

    #[test]
    fn translate_canonicalizes_non_padded_dates() {
        let llm = MockLlm::with_replies(["{\"match\": \"x\", \"date_from\": \"2026-10-1\", \
              \"date_to\": \"2026-11-9\"}"]);
        let plan = translate(&llm, "q", today()).unwrap();
        assert_eq!(plan.date_from.as_deref(), Some("2026-10-01"));
        assert_eq!(plan.date_to.as_deref(), Some("2026-11-09"));
    }

    #[test]
    fn default_config_host_is_z_ai() {
        let cfg = LlmConfig::from_lookup(|name| (name == "ZAI_API_KEY").then(|| "k".into()))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.host(), "api.z.ai");
    }

    #[test]
    fn translate_parses_plain_json() {
        let llm = MockLlm::with_replies([
            "{\"match\": \"\\\"vault\\\" OR \\\"archive\\\"\", \"date_from\": \"2026-01-01\", \"date_to\": null}",
        ]);
        let plan = translate(&llm, "anything about the vault?", today()).unwrap();
        assert_eq!(plan.match_expr, "\"vault\" OR \"archive\"");
        assert_eq!(plan.date_from.as_deref(), Some("2026-01-01"));
        assert_eq!(plan.date_to, None);
    }

    #[test]
    fn translate_parses_new_filter_fields() {
        let llm = MockLlm::with_replies([
            "{\"match\": \"deploy\", \"from\": \"ana@\", \"has_link\": true, \"extract_links\": true}",
        ]);
        let plan = translate(&llm, "collect links ana sent", today()).unwrap();
        assert_eq!(plan.from.as_deref(), Some("ana@"));
        assert_eq!(plan.has_link, Some(true));
        assert!(plan.extract_links);
    }

    #[test]
    fn from_blank_becomes_none_and_false_has_link_becomes_none() {
        let llm = MockLlm::with_replies([
            "{\"match\": \"x\", \"from\": \"   \", \"has_link\": false, \"extract_links\": false}",
        ]);
        let plan = translate(&llm, "q", today()).unwrap();
        assert_eq!(plan.from, None);
        assert_eq!(plan.has_link, None);
        assert!(!plan.extract_links);
    }

    #[test]
    fn too_long_from_rejects_the_plan() {
        let llm = MockLlm::with_replies([format!(
            "{{\"match\": \"x\", \"from\": \"{}\"}}",
            "a".repeat(129)
        )]);
        assert!(translate(&llm, "q", today()).is_err());
    }

    #[test]
    fn plain_plan_has_no_filters() {
        let plan = QueryPlan::plain("hello world");
        assert_eq!(plan.from, None);
        assert_eq!(plan.has_link, None);
        assert!(!plan.extract_links);
    }

    #[test]
    fn translation_prompt_documents_new_fields() {
        let prompt = translation_system_prompt(today());
        for needle in [
            "\"from\"",
            "\"has_link\"",
            "\"extract_links\"",
            "collect, list, or gather links",
        ] {
            assert!(
                prompt.contains(needle),
                "prompt must mention {needle}: {prompt}"
            );
        }
    }

    #[test]
    fn translate_parses_fenced_json_with_prose() {
        let llm = MockLlm::with_replies([
            "Sure!\n```json\n{\"match\": \"pricing\", \"date_from\": null, \"date_to\": \"2026-03-31\"}\n```\nDone.",
        ]);
        let plan = translate(&llm, "pricing talks", today()).unwrap();
        assert_eq!(plan.match_expr, "pricing");
        assert_eq!(plan.date_from, None);
        assert_eq!(plan.date_to.as_deref(), Some("2026-03-31"));
    }

    #[test]
    fn translate_rejects_bad_output() {
        for bad in [
            "no json here at all",
            "{\"date_from\": \"2026-01-01\"}", // match missing
            "{\"match\": \"   \"}",            // match blank
            "{\"match\": \"ok\", \"date_from\": \"January\"}", // bad date
            "{\"match\": \"ok\", \"date_to\": \"+262143-12-31\"}", // unorderable wide year
        ] {
            let llm = MockLlm::with_replies([bad]);
            assert!(
                translate(&llm, "q", today()).is_err(),
                "should reject: {bad}"
            );
        }
        let ok = MockLlm::with_replies(["{\"match\": \"x\"}"]);
        assert!(translate(&ok, "q", today()).is_ok());
    }

    #[test]
    fn translation_system_prompt_offers_all_three_columns() {
        let prompt = translation_system_prompt(today());
        assert!(prompt.contains("three columns"), "{prompt}");
        for column in ["`body`", "`author`", "`subject`"] {
            assert!(
                prompt.contains(column),
                "prompt must name {column}: {prompt}"
            );
        }
        assert!(
            prompt.contains("`subject` (email subject lines; chat messages have none)"),
            "the subject column needs its chat caveat: {prompt}"
        );
        assert!(
            prompt.contains("the column filters `body:`, `author:`, and `subject:` are allowed"),
            "the subject: filter must be explicitly allowed: {prompt}"
        );
        assert!(!prompt.contains("two columns"), "{prompt}");
    }

    #[test]
    fn extract_json_object_variants() {
        assert_eq!(extract_json_object("  {\"a\":1}  "), Some("{\"a\":1}"));
        assert_eq!(extract_json_object("pre {\"a\":1} post"), Some("{\"a\":1}"));
        assert_eq!(extract_json_object("no braces"), None);
    }

    #[test]
    fn context_block_truncates_and_numbers() {
        let msg = Message {
            id: 1,
            source: "s".into(),
            source_key: String::new(),
            external_id: "e".into(),
            channel: "email".into(),
            chat_id: String::new(),
            author: String::new(),
            sent_at: String::new(),
            body: "x".repeat(SUMMARY_BODY_LIMIT + 10),
            raw_json: "{}".into(),
        };
        let block = context_block(&msg, 3);
        assert!(block.starts_with("[3] author: unknown · date unknown\n"));
        let body_line = block.lines().nth(1).unwrap();
        assert_eq!(body_line.chars().count(), SUMMARY_BODY_LIMIT + 1); // + ellipsis
        assert!(body_line.ends_with('…'));
    }

    #[test]
    fn summarize_skips_when_no_results() {
        let llm = MockLlm::with_replies(Vec::<String>::new());
        let s = summarize_results(&llm, "q", &[], DEFAULT_SUMMARIZE_TOP, &[]);
        assert_eq!(s.answer, None);
        assert!(llm.prompts().is_empty(), "no LLM call without results");
    }

    #[test]
    fn lim_order_places_strongest_first_and_last() {
        // Exact permutations from the plan (0-based ranks).
        assert_eq!(lim_order(0), Vec::<usize>::new());
        assert_eq!(lim_order(1), vec![0]);
        assert_eq!(lim_order(2), vec![0, 1]);
        assert_eq!(lim_order(3), vec![0, 2, 1]);
        assert_eq!(lim_order(8), vec![0, 2, 4, 6, 7, 5, 3, 1]); // 1-based 1,3,5,7,8,6,4,2

        // Invariants for any n: a permutation of 0..n, rank 0 (the
        // strongest evidence) placed first.
        for n in 0..=13usize {
            let order = lim_order(n);
            let mut sorted = order.clone();
            sorted.sort();
            assert_eq!(sorted, (0..n).collect::<Vec<_>>(), "n={n}");
            if n > 0 {
                assert_eq!(order.first(), Some(&0), "n={n}");
            }
        }
    }

    /// Fixture: message with a body that may contain links.
    fn link_msg(id: i64, author: &str, body: &str) -> Message {
        Message {
            id,
            source: "test".into(),
            source_key: String::new(),
            external_id: format!("l{id}"),
            channel: "email".into(),
            chat_id: String::new(),
            author: author.into(),
            sent_at: "2026-10-01T00:00:00Z".into(),
            body: body.into(),
            raw_json: "{}".into(),
        }
    }

    /// Fixture: message of rank `rank` with a distinctive body marker.
    fn lim_msg(rank: usize) -> Message {
        Message {
            id: rank as i64,
            source: "test".into(),
            source_key: String::new(),
            external_id: format!("lim{rank}"),
            channel: "email".into(),
            chat_id: String::new(),
            author: format!("author{rank}"),
            sent_at: "2026-10-01".into(),
            body: format!("lim marker {rank:02} about vault"),
            raw_json: "{}".into(),
        }
    }

    /// The `[k]` citation markers of the summary prompt, in prompt order.
    fn marker_order(user: &str, n: usize) -> Vec<usize> {
        let mut located: Vec<(usize, usize)> = (1..=n)
            .map(|k| {
                let at = user
                    .find(&format!("[{k}] author"))
                    .unwrap_or_else(|| panic!("block [{k}] missing from prompt: {user}"));
                (at, k)
            })
            .collect();
        located.sort();
        located.into_iter().map(|(_, k)| k).collect()
    }

    #[test]
    fn summary_prompt_orders_blocks_lost_in_the_middle() {
        let msgs: Vec<Message> = (0..8).map(lim_msg).collect();
        let llm = MockLlm::with_replies(["summary"]);
        let s = summarize_results(&llm, "vault?", &msgs, DEFAULT_SUMMARIZE_TOP, &[]);
        assert_eq!(s.answer.as_deref(), Some("summary"));
        let user = &llm.prompts()[0].1;

        // The block order in the prompt is 1,3,5,7,8,6,4,2 — odd ranks
        // ascending, then even ranks descending.
        assert_eq!(marker_order(user, 8), vec![1, 3, 5, 7, 8, 6, 4, 2]);

        // Numbering stays by rank: block [k] carries ranked message k-1.
        for k in 1..=8usize {
            let rank = k - 1;
            assert!(
                user.contains(&format!(
                    "[{k}] author: author{rank} · 2026-10-01\nlim marker {rank:02} about vault"
                )),
                "block [{k}] must cite ranked message {rank}: {user}"
            );
        }
    }

    #[test]
    fn summary_prompt_permutations_for_small_n() {
        for (n, expected) in [(1usize, vec![1usize]), (2, vec![1, 2]), (3, vec![1, 3, 2])] {
            let msgs: Vec<Message> = (0..n).map(lim_msg).collect();
            let llm = MockLlm::with_replies(["summary"]);
            summarize_results(&llm, "vault?", &msgs, n, &[]);
            let user = &llm.prompts()[0].1;
            assert_eq!(marker_order(user, n), expected, "n={n}");
        }
    }

    #[test]
    fn summary_prompt_truncates_top_rows_before_permuting() {
        let msgs: Vec<Message> = (0..5).map(lim_msg).collect();
        let llm = MockLlm::with_replies(["summary"]);
        summarize_results(&llm, "vault?", &msgs, 3, &[]);
        let user = &llm.prompts()[0].1;

        // Exactly the top-3 ranked rows are sent, then permuted (1,3,2).
        for rank in 0..3 {
            assert!(
                user.contains(&format!("lim marker {rank:02} about vault")),
                "top row {rank} missing: {user}"
            );
        }
        for rank in 3..5 {
            assert!(
                !user.contains(&format!("lim marker {rank:02}")),
                "row outside the top-3 leaked into the prompt: {user}"
            );
        }
        assert_eq!(marker_order(user, 3), vec![1, 3, 2]);
    }

    #[test]
    fn summary_prompt_header_says_numbered_by_relevance() {
        let msgs: Vec<Message> = (0..2).map(lim_msg).collect();
        let llm = MockLlm::with_replies(["summary"]);
        summarize_results(&llm, "vault?", &msgs, DEFAULT_SUMMARIZE_TOP, &[]);
        let user = &llm.prompts()[0].1;
        assert!(
            user.contains("Messages (numbered by relevance):"),
            "header must say numbered by relevance: {user}"
        );
        assert!(
            !user.contains("most relevant first"),
            "the old header is false once block order differs from numbering: {user}"
        );
    }

    #[test]
    fn summary_prompt_includes_the_collected_links_section() {
        let msgs = vec![link_msg(1, "ana", "see https://example.com/a")];
        let collected = extract_links(&msgs);
        let llm = MockLlm::with_replies(["summary"]);
        let s = summarize_results(&llm, "collect links", &msgs, 8, &collected);
        assert_eq!(s.answer.as_deref(), Some("summary"));
        let user = &llm.prompts()[0].1;
        assert!(
            user.contains("Collected links (from the messages above):"),
            "{user}"
        );
        assert!(
            user.contains("https://example.com/a · ana · 2026-10-01T00:00:00Z"),
            "{user}"
        );
        // The system prompt gains the links rule only when links exist.
        assert!(llm.prompts()[0].0.contains("describe what the links are"));
    }

    #[test]
    fn summary_link_lines_are_capped() {
        let body = (0..60)
            .map(|i| format!("https://example.com/{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let msgs = vec![link_msg(1, "ana", &body)];
        let collected = extract_links(&msgs);
        let llm = MockLlm::with_replies(["summary"]);
        summarize_results(&llm, "q", &msgs, 8, &collected);
        let user = &llm.prompts()[0].1;
        assert!(user.contains("https://example.com/49"), "{user}");
        assert!(!user.contains("https://example.com/50"), "{user}");
    }

    #[test]
    fn summary_without_links_is_unchanged() {
        let msgs = vec![lim_msg(0)];
        let llm = MockLlm::with_replies(["summary"]);
        summarize_results(&llm, "vault?", &msgs, 8, &[]);
        assert!(!llm.prompts()[0].0.contains("describe what the links are"));
        assert!(!llm.prompts()[0].1.contains("Collected links"));
    }

    #[test]
    fn ask_collects_links_only_when_the_plan_extracts() {
        use crate::storage::Storage;
        use crate::vector::HashEmbedder;
        let storage = Storage::open_in_memory().unwrap();
        storage
            .insert_message(&crate::message::NewMessage::new(
                "test",
                "l1",
                "ana",
                "2026-10-01T00:00:00Z",
                "see https://example.com/a about deploys",
            ))
            .unwrap();
        let embedder = HashEmbedder;
        // Local mode (no LLM): the plain plan never extracts.
        let local = ask(
            &storage,
            None,
            &embedder,
            "collect links about deploys",
            50,
            8,
            today(),
        )
        .unwrap();
        assert!(local.collected.is_empty());
        // An LLM that plans extraction: the link is collected and the
        // summary prompt carries the collected-links section.
        let llm = MockLlm::with_replies([
            "{\"match\": \"deploys\", \"has_link\": true, \"extract_links\": true}".to_string(),
            "one link collected [1].".to_string(),
        ]);
        let out = ask(
            &storage,
            Some(&llm),
            &embedder,
            "collect links about deploys",
            50,
            8,
            today(),
        )
        .unwrap();
        assert_eq!(out.collected.len(), 1);
        assert_eq!(out.collected[0].url, "https://example.com/a");
        assert!(
            llm.prompts()[1].1.contains("Collected links"),
            "{}",
            llm.prompts()[1].1
        );
    }

    #[test]
    fn classify_status_maps_common_failures() {
        // Invalid key.
        assert!(classify_status(401, "", "api.z.ai").contains("invalid API key (HTTP 401)"));
        assert!(classify_status(403, "{}", "h").contains("invalid API key (HTTP 403)"));
        // Wrong base URL.
        assert!(classify_status(404, "", "api.example").contains("no endpoint at this base URL"));
        // Provider error body (Anthropic-style nested message) is surfaced.
        let body =
            r#"{"error":{"type":"invalid_request_error","message":"model: nope not found"}}"#;
        assert!(classify_status(400, body, "h").contains("model: nope not found"));
        // Flat message field also honored.
        assert!(
            classify_status(400, r#"{"message":"bad request detail"}"#, "h")
                .contains("bad request detail")
        );
        // Non-JSON body: raw excerpt.
        assert!(classify_status(500, "boom", "h").contains("HTTP 500: boom"));
        // Empty body: code only.
        assert_eq!(classify_status(502, "", "h"), "HTTP 502");
    }

    #[test]
    fn plaintext_http_guard_matches_settings_policy() {
        assert!(plaintext_http_off_loopback("http://api.example.com"));
        assert!(!plaintext_http_off_loopback("https://api.example.com"));
        assert!(!plaintext_http_off_loopback("http://127.0.0.1:8080"));
        assert!(!plaintext_http_off_loopback("http://localhost:9"));
        assert!(!plaintext_http_off_loopback("http://[::1]:9"));
        assert!(!plaintext_http_off_loopback("not a url"));
    }

    // ------------------------------------------------------------------
    // Workspace scope: retrieval, ranking, and the summary stay inside it
    // ------------------------------------------------------------------

    use crate::search2::{validate_ask_scope, AskScope};

    /// Canonical id of the source instance behind a legacy namespace.
    fn canonical_id(storage: &Storage, namespace: &str) -> String {
        storage
            .connection()
            .query_row(
                "SELECT id FROM source_instances WHERE legacy_namespace = ?1",
                [namespace],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
    }

    /// Both same-label sources' messages answer the question (FTS term
    /// and embedding text), so any leak must come from missing scoping.
    const SCOPE_QUESTION: &str = "scoped marker about deploys";

    /// Two source instances sharing one display label ("Same Label"),
    /// distinct canonical ids, one dated message each (authors differ),
    /// both embedded with the question text so the vector leg finds both.
    fn scoped_fixture() -> (Storage, String, String) {
        let storage = Storage::open_in_memory().unwrap();
        let embedder = crate::vector::HashEmbedder;
        for (ns, author, body) in [
            (
                "legacy-a",
                "ana@example.com",
                "scoped alpha marker about deploys",
            ),
            (
                "legacy-b",
                "bob@example.com",
                "scoped beta marker about deploys",
            ),
        ] {
            storage
                .insert_message(&crate::message::NewMessage::new(
                    ns,
                    format!("{ns}-m1"),
                    author,
                    "2026-10-05T12:00:00Z",
                    body,
                ))
                .unwrap();
        }
        // The UI shows the same label for both; only canonical ids can
        // tell them apart.
        storage
            .connection()
            .execute("UPDATE source_instances SET label = 'Same Label'", [])
            .unwrap();
        let ids: Vec<(i64, Vec<f32>)> = storage
            .connection()
            .prepare("SELECT id FROM archive_messages ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .map(|id| (id, embedder.embed(SCOPE_QUESTION)))
            .collect();
        storage
            .embed_messages_batch(embedder.id(), embedder.dim(), &ids)
            .unwrap();
        let id_a = canonical_id(&storage, "legacy-a");
        let id_b = canonical_id(&storage, "legacy-b");
        (storage, id_a, id_b)
    }

    fn scoped_plan(from: Option<&str>, to: Option<&str>) -> PlannedQuery {
        PlannedQuery {
            plan: QueryPlan {
                match_expr: "\"scoped\"".into(),
                date_from: from.map(str::to_string),
                date_to: to.map(str::to_string),
                from: None,
                has_link: None,
                extract_links: false,
            },
            source: PlanSource::Llm,
            notes: Vec::new(),
        }
    }

    fn source_scope(id: &str) -> AskScope {
        AskScope {
            source_ids: vec![id.to_string()],
            ..AskScope::default()
        }
    }

    #[test]
    fn scoped_ask_isolates_same_label_sources_by_canonical_id() {
        let (storage, id_a, id_b) = scoped_fixture();
        let planned = scoped_plan(None, None);

        // Unscoped, both sources' messages match.
        let unscoped = run_plan(&storage, &planned, 50).unwrap();
        assert_eq!(unscoped.len(), 2);

        // Scoped to A's canonical id: only A's message on the FTS leg…
        let scope = validate_ask_scope(&storage, &source_scope(&id_a)).unwrap();
        let fts = run_plan_scoped(&storage, &planned, Some(&scope), 50).unwrap();
        assert_eq!(fts.len(), 1);
        assert!(fts[0].body.contains("alpha"));

        // …and on the vector/hybrid leg.
        let embedder = crate::vector::HashEmbedder;
        let hybrid = rank_hybrid_scoped(
            &storage,
            &embedder,
            SCOPE_QUESTION,
            &planned,
            &fts,
            Some(&scope),
            50,
        )
        .unwrap();
        assert!(!hybrid.results.is_empty());
        assert!(
            hybrid.results.iter().all(|m| m.body.contains("alpha")),
            "{:?}",
            hybrid.results
        );

        // Scoped to B's canonical id: B's message, not A's — the shared
        // display label never blurs the boundary.
        let scope_b = validate_ask_scope(&storage, &source_scope(&id_b)).unwrap();
        let fts_b = run_plan_scoped(&storage, &planned, Some(&scope_b), 50).unwrap();
        assert_eq!(fts_b.len(), 1);
        assert!(fts_b[0].body.contains("beta"));
    }

    #[test]
    fn scoped_ask_with_out_of_range_dates_returns_nothing_on_both_legs() {
        let (storage, _id_a, _id_b) = scoped_fixture();
        let planned = scoped_plan(None, None);
        // Every fixture message is from 2026-10; a 2027 window is empty.
        let scope = validate_ask_scope(
            &storage,
            &AskScope {
                from_date: Some("2027-01-01".into()),
                to_date: Some("2027-01-31".into()),
                timezone: Some("UTC".into()),
                ..AskScope::default()
            },
        )
        .unwrap();
        let fts = run_plan_scoped(&storage, &planned, Some(&scope), 50).unwrap();
        assert!(fts.is_empty(), "{fts:?}");
        let embedder = crate::vector::HashEmbedder;
        let hybrid = rank_hybrid_scoped(
            &storage,
            &embedder,
            SCOPE_QUESTION,
            &planned,
            &fts,
            Some(&scope),
            50,
        )
        .unwrap();
        assert!(hybrid.results.is_empty(), "{:?}", hybrid.results);
    }

    #[test]
    fn empty_plan_scope_date_intersection_returns_no_results_without_fallback() {
        let (storage, _id_a, _id_b) = scoped_fixture();
        // The plan narrows to October, the scope to November: the archive
        // answers the plan unscoped, so any row here would mean a
        // fallback to unscoped retrieval.
        let planned = scoped_plan(Some("2026-10-01"), Some("2026-10-31"));
        let scope = validate_ask_scope(
            &storage,
            &AskScope {
                from_date: Some("2026-11-01".into()),
                to_date: Some("2026-11-30".into()),
                timezone: Some("UTC".into()),
                ..AskScope::default()
            },
        )
        .unwrap();
        assert!(!run_plan(&storage, &planned, 50).unwrap().is_empty());
        let fts = run_plan_scoped(&storage, &planned, Some(&scope), 50).unwrap();
        assert!(fts.is_empty(), "{fts:?}");
        let embedder = crate::vector::HashEmbedder;
        let hybrid = rank_hybrid_scoped(
            &storage,
            &embedder,
            SCOPE_QUESTION,
            &planned,
            &fts,
            Some(&scope),
            50,
        )
        .unwrap();
        assert!(hybrid.results.is_empty(), "{:?}", hybrid.results);
    }

    #[test]
    fn scope_author_intersects_plan_from_conjunctively() {
        let (storage, _id_a, _id_b) = scoped_fixture();
        let mut planned = scoped_plan(None, None);
        planned.plan.from = Some("ana".into());

        // Plan alone: ana's message.
        assert_eq!(run_plan(&storage, &planned, 50).unwrap().len(), 1);

        // Scope author combines as a conjunction: ana ∩ bob is empty…
        let disjoint = validate_ask_scope(
            &storage,
            &AskScope {
                author: Some("bob".into()),
                ..AskScope::default()
            },
        )
        .unwrap();
        assert!(run_plan_scoped(&storage, &planned, Some(&disjoint), 50)
            .unwrap()
            .is_empty());

        // …while ana ∩ example.com keeps exactly ana's message.
        let overlapping = validate_ask_scope(
            &storage,
            &AskScope {
                author: Some("example.com".into()),
                ..AskScope::default()
            },
        )
        .unwrap();
        let results = run_plan_scoped(&storage, &planned, Some(&overlapping), 50).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].author, "ana@example.com");
    }

    #[test]
    fn scoped_summary_prompt_carries_only_in_scope_messages() {
        let (storage, id_a, _id_b) = scoped_fixture();
        // The same composition run_ask uses: translate → validate →
        // scoped run + rank → summarize. The provider may only ever see
        // the scoped rows.
        let llm = MockLlm::with_replies([
            "{\"match\": \"scoped\"}".to_string(),
            "summary [1].".to_string(),
        ]);
        let planned = validate(
            &storage,
            SCOPE_QUESTION,
            translate(&llm, SCOPE_QUESTION, today()),
        );
        let scope = validate_ask_scope(&storage, &source_scope(&id_a)).unwrap();
        let fts = run_plan_scoped(&storage, &planned, Some(&scope), 50).unwrap();
        assert_eq!(fts.len(), 1);
        let embedder = crate::vector::HashEmbedder;
        let hybrid = rank_hybrid_scoped(
            &storage,
            &embedder,
            SCOPE_QUESTION,
            &planned,
            &fts,
            Some(&scope),
            50,
        )
        .unwrap();
        let summarized = summarize_results(
            &llm,
            SCOPE_QUESTION,
            &hybrid.results,
            DEFAULT_SUMMARIZE_TOP,
            &[],
        );
        assert_eq!(summarized.answer.as_deref(), Some("summary [1]."));
        let user = &llm.prompts()[1].1;
        assert!(user.contains("alpha"), "in-scope row missing: {user}");
        assert!(
            !user.contains("beta"),
            "out-of-scope row leaked into the summary prompt: {user}"
        );
    }
}

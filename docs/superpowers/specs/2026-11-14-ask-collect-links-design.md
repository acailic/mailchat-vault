# Ask the archive: collect links — design

Date: 2026-11-14 · Status: approved (user) · Branch: `feat/ask-collect-links`

## Problem

Ask (natural-language search) can find and summarize messages, but a question
like *"collect all links from messages sender X sent"* cannot be answered:
the LLM plan has no structured sender filter, no link-only filter, and there
is no extraction step — the answer is always a prose summary, never a
collected list, and nothing collected can be exported.

## Goal

One ask that filters by sender / link-presence, extracts every http(s) URL
from the matching messages **locally**, and resolves into a collected,
cited, exportable list. Provider AI stays optional: the collected list is
deterministic local output and renders fully without any LLM.

## Non-goals

- URL normalization / `utm_*` stripping — dedup is exact-string.
- `mailto:` links, email-address extraction, attachment names (deferred).
- Pagination of the collected list in the UI.
- Agent/tool-calling loops (one translate + one summarize round trip, as today).

## Design

### 1. Query plan (`crates/vault-core/src/llm.rs`)

`QueryPlan` gains three fields:

```rust
pub struct QueryPlan {
    pub match_expr: String,
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub from: Option<String>,   // NEW: substring on author, case-insensitive
    pub has_link: Option<bool>, // NEW: only Some(true) is honored
    pub extract_links: bool,    // NEW
}
```

- Translation reply JSON becomes
  `{"match": …, "date_from": …, "date_to": …, "from": …, "has_link": …, "extract_links": …}`
  (all new keys optional, serde defaults; booleans/`null`).
- `from` validation: trim; empty → `None`; longer than 128 chars → plan
  rejected (the existing `validate` fallback path applies).
- `has_link: Some(false)` is ignored (treated as `None`) — the prompt
  instructs the model to use it only when links are required.
- The `validate` fallback that replaces an invalid MATCH expression keeps
  `from`/`has_link` (same as it keeps usable dates today). The hard-error
  fallback (`translate` failed) drops everything, as today.
- `translation_system_prompt` documents the new fields: `from` = substring
  of the sender's name/address; set `has_link: true` when the question asks
  for links; set `extract_links: true` only when the question explicitly
  asks to collect/list/gather links (then also `has_link: true`).

### 2. Storage filters (`crates/vault-core/src/storage.rs`)

`search_filtered` and `vector_scan` gain two bound parameters applied
**before** LIMIT on both legs:

```sql
AND (:from IS NULL OR m.author LIKE '%' || :from || '%')
AND (:require_link = 0 OR m.body LIKE '%http%://%')
```

SQLite `LIKE` is ASCII-case-insensitive — sufficient for names/addresses.
`has_link` is approximate by design (`http…://` anywhere in the body); the
exact URL set comes from the extraction step, not this filter.

### 3. Extraction (`crates/vault-core/src/extract.rs`, new)

```rust
pub struct LinkHit {
    pub message_id: i64,
    pub author: String,
    pub sent_at: String,
    pub context: String, // ±60 chars around the URL, whitespace collapsed
}
pub struct CollectedLink {
    pub url: String,
    pub hits: Vec<LinkHit>,
}
pub const MAX_COLLECTED_LINKS: usize = 200;
pub fn extract_links(messages: &[Message]) -> Vec<CollectedLink>;
```

- Hand-rolled scanner, no new dependencies: find `http://` / `https://`
  (case-insensitive), take until whitespace or a closing `>"'`, then strip
  trailing `.,;:!?`; strip a trailing `)` only when the URL contains no `(`.
- Dedup exact URL strings; hits are appended in message (rank) order.
- Extraction stops once 200 unique URLs are collected.

### 4. Resolve (`AskOutcome`, summary, consent)

- `AskOutcome` gains `collected: Vec<CollectedLink>` (empty unless the plan
  extracted). Extraction runs over the **full** result list (today's ask
  limit, 50) — not just the summarized top 8.
- When links were collected, `summarize_results` appends a
  `Collected links:` section (URL · sender · date, capped at 50 lines) to
  the summary prompt, and the summary system prompt gains the rule: when a
  collected-links list is present, describe what the links are and who sent
  them, citing the numbered messages as usual.
- **Consent (M5) stays truthful:** the static consent prompt and privacy
  note texts (`llmview.ts`) are extended to state that when the question
  asks to collect links, the collected link list (URLs + senders, capped at
  50) is also sent. The list itself never requires the LLM: it renders and
  exports fully in local mode.

### 5. UI (`index.html`, `src/main.ts`, `src/llmview.ts`, `src/styles/app.css`)

- New `#ask-collected` block under the ask answer: count line
  ("N links from M messages"), one row per link — sender · date · URL ·
  context — each row opens the citing message in thread context (same
  `openDetail` the citations use); one row can hold several hits, expandable
  (details/summary element).
- `describePlan` shows the new filters (`from: x · links only`).
- "Export links…" button with a text/JSON format select, mirroring the
  existing selection-export control; a small generic save path writes the
  serialized list through the same save-dialog flow the selection export
  uses. Text and JSON serialization are pure helpers in `llmview.ts`
  (unit-tested).
- `llmview.ts` mirrors the new `CollectedLink`/`LinkHit`/`QueryPlan`
  shapes (serde snake_case, as all existing mirrors).

### 6. Tests

- Rust `llm.rs`: parsing of new fields; `from` empty→`None`, >128→rejected;
  invalid-match fallback keeps `from`/`has_link`; prompt names new fields;
  summary prompt includes the capped links section.
- Rust `storage.rs`: `search_filtered`/`vector_scan` honor `from`
  (case-insensitive substring) and `require_link` before LIMIT.
- Rust `extract.rs`: basic extraction, scheme case-insensitivity, dedup,
  trailing punctuation, unbalanced-paren rule, context bounds, 200 cap.
- TS `llmview.test.ts`: describePlan segments, consent text mentions the
  link list, text/JSON serialization of collected links.
- TS `main.test.ts`: collected block rendering (hidden when empty, rows
  open the right message), export handler wiring.

### 7. Docs

`docs/ai-search.md` gains a "Collect links" section documenting the flow,
the filters, the local-only guarantee, and the consent wording.

## Privacy summary

Nothing new leaves the machine except: the question (as today) and, only
when extraction is active, the collected link list (≤50 URL·sender·date
lines) appended to the summary prompt. Filtering and extraction are pure
SQL/regex over local data.

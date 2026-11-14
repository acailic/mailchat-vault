# Ask-Collect-Links Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One Ask question can filter by sender / link-presence, extract every http(s) URL locally, and resolve into a collected, cited, exportable list.

**Architecture:** The LLM translation plan (`QueryPlan`) gains `from`/`has_link`/`extract_links`; both search legs (FTS5 + vector) apply the filters as SQL bound parameters before LIMIT; a new `extract.rs` scans result bodies locally; `AskOutcome.collected` carries the list to the UI, which renders cited rows and exports them through a save dialog + a tiny `save_text` command.

**Tech Stack:** Rust (vault-core, rusqlite, serde), TypeScript (Vite, vitest + happy-dom), Tauri v2 commands. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-14-ask-collect-links-design.md` (approved).

## Global Constraints

- No new crate/npm dependencies (hand-rolled URL scanner, no `regex`).
- All SQL values are bound parameters — never spliced into SQL text.
- Branch: `feat/ask-collect-links`; conventional commits (`feat:`, `test:`, `docs:`).
- Rust tests: `cargo test -p vault-core` from the repo root. TS tests: `npx vitest run <file>`. Type check: `npm run check`.
- serde mirrors in `src/llmview.ts` use snake_case field names exactly.
- Nothing new leaves the machine except the question (as today) and, when extraction is active, the collected-link list (≤50 `URL · sender · date` lines) in the summary prompt. Consent texts must say so.

---

### Task 1: Local link extraction (`extract.rs`)

**Files:**
- Create: `crates/vault-core/src/extract.rs`
- Modify: `crates/vault-core/src/lib.rs:80` (add `pub mod extract;` after `pub mod export;`)

**Interfaces:**
- Consumes: `crate::message::Message` (fields `id: i64`, `author: String`, `sent_at: String`, `body: String`).
- Produces (used by Tasks 4, 5, 6): `pub struct LinkHit { message_id: i64, author: String, sent_at: String, context: String }`, `pub struct CollectedLink { url: String, hits: Vec<LinkHit> }`, `pub const MAX_COLLECTED_LINKS: usize`, `pub fn extract_links(messages: &[Message]) -> Vec<CollectedLink>`.

- [ ] **Step 1: Write the failing tests**

Create `crates/vault-core/src/extract.rs` with only the types and a stub `pub fn extract_links(_messages: &[Message]) -> Vec<CollectedLink> { Vec::new() }`, plus this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: i64, author: &str, sent_at: &str, body: &str) -> Message {
        Message {
            id,
            source: "test".into(),
            external_id: format!("e{id}"),
            channel: "email".into(),
            chat_id: String::new(),
            author: author.into(),
            sent_at: sent_at.into(),
            body: body.into(),
            raw_json: "{}".into(),
        }
    }

    #[test]
    fn extracts_plain_urls_with_sender_and_context() {
        let out = extract_links(&[msg(
            1,
            "Ana <ana@example.com>",
            "2026-10-01T10:00:00Z",
            "see https://example.com/a for the draft",
        )]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://example.com/a");
        assert_eq!(out[0].hits.len(), 1);
        assert_eq!(out[0].hits[0].message_id, 1);
        assert_eq!(out[0].hits[0].author, "Ana <ana@example.com>");
        assert_eq!(out[0].hits[0].sent_at, "2026-10-01T10:00:00Z");
        assert!(out[0].hits[0].context.contains("see"), "{}", out[0].hits[0].context);
        assert!(out[0].hits[0].context.contains("for the draft"));
    }

    #[test]
    fn scheme_match_is_case_insensitive() {
        let out = extract_links(&[msg(1, "a", "2026-01-01T00:00:00Z", "go to HTTPS://Example.COM/x now")]);
        assert_eq!(out.len(), 1);
        // The URL is preserved verbatim beyond the scheme.
        assert_eq!(out[0].url, "HTTPS://Example.COM/x");
    }

    #[test]
    fn dedups_by_exact_url_across_messages() {
        let out = extract_links(&[
            msg(1, "ana", "2026-01-01T00:00:00Z", "https://example.com/a"),
            msg(2, "bo", "2026-01-02T00:00:00Z", "first https://example.com/a again"),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].hits.len(), 2);
        assert_eq!(out[0].hits[0].message_id, 1);
        assert_eq!(out[0].hits[1].message_id, 2);
    }

    #[test]
    fn strips_trailing_punctuation_and_unbalanced_paren() {
        let out = extract_links(&[msg(
            1, "a", "2026-01-01T00:00:00Z",
            "one https://example.com/a, two https://example.com/b).",
        )]);
        assert_eq!(out[0].url, "https://example.com/a");
        assert_eq!(out[1].url, "https://example.com/b");
    }

    #[test]
    fn keeps_balanced_parens_inside_the_url() {
        let out = extract_links(&[msg(1, "a", "2026-01-01T00:00:00Z",
            "https://en.wikipedia.org/wiki/Foo_(bar) end")]);
        assert_eq!(out[0].url, "https://en.wikipedia.org/wiki/Foo_(bar)");
    }

    #[test]
    fn stops_at_whitespace_and_quotes() {
        let out = extract_links(&[msg(1, "a", "2026-01-01T00:00:00Z",
            "<a href=\"https://example.com/x\">link</a>")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://example.com/x");
    }

    #[test]
    fn context_is_bounded_and_whitespace_collapsed() {
        let out = extract_links(&[msg(1, "a", "2026-01-01T00:00:00Z",
            &format!("{} https://example.com/a {}", "w".repeat(200), "n".repeat(200)))]);
        let ctx = &out[0].hits[0].context;
        assert!(ctx.chars().count() <= 141, "{ctx}"); // 60 + url + 60 (+ellipses)
        assert!(!ctx.contains("\n"));
    }

    #[test]
    fn caps_unique_urls_at_max() {
        let body = (0..MAX_COLLECTED_LINKS + 10)
            .map(|i| format!("https://example.com/{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let out = extract_links(&[msg(1, "a", "2026-01-01T00:00:00Z", &body)]);
        assert_eq!(out.len(), MAX_COLLECTED_LINKS);
    }

    #[test]
    fn requires_a_host_character() {
        let out = extract_links(&[msg(1, "a", "2026-01-01T00:00:00Z", "empty https:// and https://x ok")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].url, "https://x");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vault-core extract:: 2>&1 | tail -5`
Expected: compile of test module succeeds, assertions FAIL (stub returns empty).

- [ ] **Step 3: Implement the scanner**

Replace the stub with the real implementation (file header comment + code):

```rust
//! Local link extraction over archived message bodies: a hand-rolled
//! http(s) scanner, deterministic and dependency-free. Part of the
//! ask-collect-links feature (see
//! docs/superpowers/specs/2026-11-14-ask-collect-links-design.md).

use serde::{Deserialize, Serialize};

use crate::message::Message;

/// Stop scanning once this many unique URLs are collected.
pub const MAX_COLLECTED_LINKS: usize = 200;
/// How far the context snippet reaches around the URL, each side (chars).
const CONTEXT_RADIUS: usize = 60;
/// Longest URL accepted (guards against pathological matches).
const MAX_URL_LEN: usize = 2048;

/// One occurrence of a URL inside one message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkHit {
    pub message_id: i64,
    pub author: String,
    pub sent_at: String,
    /// Up to ~60 chars around the URL, whitespace collapsed.
    pub context: String,
}

/// A URL deduplicated across messages, with every hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectedLink {
    pub url: String,
    pub hits: Vec<LinkHit>,
}

/// Characters that end a URL mid-text (whitespace also ends it).
const END_CHARS: &[char] = &['"', '\'', '<', '>'];
/// Trailing prose punctuation stripped off a scanned URL.
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', '"', '\''];

/// Next `http://`/`https://` at or after `from` (case-insensitive):
/// byte index of the scheme start and of the `://` end.
fn find_scheme(body: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = body.as_bytes();
    let mut at = from;
    while at + 8 <= bytes.len() {
        let rest = &body[at..];
        let len = if rest.len() >= 8 && rest[..8].eq_ignore_ascii_case("https://") {
            8
        } else if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case("http://") {
            7
        } else {
            at += 1;
            continue;
        };
        return Some((at, at + len));
    }
    None
}

/// Context around `start..end` in `body`: ±60 chars on char boundaries,
/// whitespace runs collapsed to single spaces.
fn context_around(body: &str, start: usize, end: usize) -> String {
    let chars: Vec<(usize, char)> = body.char_indices().collect();
    let pos = |byte| chars.partition_point(|(b, _)| *b < byte);
    let (lo, hi) = (pos(start), pos(end));
    let from = lo.saturating_sub(CONTEXT_RADIUS);
    let to = (hi + CONTEXT_RADIUS).min(chars.len());
    chars[from..to]
        .iter()
        .map(|(_, c)| *c)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Scan one body for URLs: `(url, start, end)` triples in order.
fn scan_urls(body: &str) -> Vec<(String, usize, usize)> {
    let mut found = Vec::new();
    let mut search_from = 0;
    while let Some((scheme_at, scheme_end)) = find_scheme(body, search_from) {
        search_from = scheme_end;
        let rest = &body[scheme_end..];
        let take = rest
            .find(|c: char| c.is_whitespace() || END_CHARS.contains(&c))
            .unwrap_or(rest.len());
        let mut url_end = scheme_end + take;
        let mut url = body[scheme_at..url_end].to_string();
        // Strip trailing prose punctuation; a closing paren only when the
        // URL opened none (unbalanced = prose, balanced = part of the URL).
        loop {
            let trimmed = url.trim_end_matches(|c| TRAILING.contains(&c));
            let trimmed = trimmed.strip_suffix(')')
                .filter(|_| !trimmed.contains('('))
                .map(str::to_string)
                .unwrap_or_else(|| trimmed.to_string());
            if trimmed.len() == url.len() {
                break;
            }
            url = trimmed;
        }
        url_end = scheme_at + url.len();
        let has_host = url.len() > url.find("://").map_or(8, |p| p + 3);
        if has_host && url.len() <= MAX_URL_LEN {
            found.push((url, scheme_at, url_end));
            search_from = url_end;
        }
    }
    found
}

/// Collect http(s) links from `messages` (rank order), deduplicated by
/// exact URL string. Stops entirely once [`MAX_COLLECTED_LINKS`] unique
/// URLs are collected.
pub fn extract_links(messages: &[Message]) -> Vec<CollectedLink> {
    let mut collected: Vec<CollectedLink> = Vec::new();
    'messages: for msg in messages {
        for (url, start, end) in scan_urls(&msg.body) {
            let hit = LinkHit {
                message_id: msg.id,
                author: msg.author.clone(),
                sent_at: msg.sent_at.clone(),
                context: context_around(&msg.body, start, end),
            };
            if let Some(existing) = collected.iter_mut().find(|l| l.url == url) {
                existing.hits.push(hit);
            } else {
                if collected.len() >= MAX_COLLECTED_LINKS {
                    break 'messages;
                }
                collected.push(CollectedLink { url, hits: vec![hit] });
            }
        }
    }
    collected
}
```

- [ ] **Step 4: Register the module**

In `crates/vault-core/src/lib.rs`, after `pub mod export;` add `pub mod extract;` (alphabetical).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p vault-core extract:: 2>&1 | tail -5`
Expected: all 9 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/vault-core/src/extract.rs crates/vault-core/src/lib.rs
git commit -m "feat(core): local http(s) link extraction over message bodies"
```

---

### Task 2: Plan filters (`from`, `has_link`, `extract_links`) in `llm.rs`

**Files:**
- Modify: `crates/vault-core/src/llm.rs` (QueryPlan at ~line 266, RawPlan ~329, prompt ~352, validate ~409)

**Interfaces:**
- Consumes: nothing new.
- Produces (used by Tasks 3, 4): `QueryPlan { match_expr, date_from, date_to, from: Option<String>, has_link: Option<bool>, extract_links: bool }`; `QueryPlan::plain` fills `None/None/false`.

- [ ] **Step 1: Write the failing tests**

Add to the `mod tests` in `crates/vault-core/src/llm.rs`:

```rust
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
            "{{\"match\": \"x\", \"from\": \"{}\"}}", "a".repeat(129)
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
            assert!(prompt.contains(needle), "prompt must mention {needle}: {prompt}");
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vault-core llm::tests::translate_parses_new_filter_fields 2>&1 | tail -5`
Expected: compile error — `QueryPlan` has no field `from`.

- [ ] **Step 3: Extend QueryPlan, RawPlan, prompt, validate**

In `crates/vault-core/src/llm.rs`:

3a. Add near the other limits (after `const MAX_MATCH_LEN`):
```rust
/// Hard cap on the length of an LLM-produced `from` filter.
const MAX_FROM_LEN: usize = 128;
```

3b. `QueryPlan` — add fields and doc comments:
```rust
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
```

3c. `QueryPlan::plain` — add `from: None, has_link: None, extract_links: false` to the struct literal.

3d. `QueryPlan::from_llm_reply` — extend the final `Ok(Self { … })`:
```rust
        let has_link = raw.has_link.filter(|&b| b); // only Some(true) is a filter
        Ok(Self {
            match_expr,
            date_from: canonical_date(&raw.date_from)?,
            date_to: canonical_date(&raw.date_to)?,
            from: canonical_from(&raw.from)?,
            has_link,
            extract_links: raw.extract_links,
        })
```

3e. Add next to `canonical_date`:
```rust
/// Parse an optional model-supplied sender filter: trimmed, empty → `None`,
/// longer than [`MAX_FROM_LEN`] → plan rejected.
fn canonical_from(value: &Option<String>) -> Result<Option<String>> {
    match value.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.chars().count() <= MAX_FROM_LEN => Ok(Some(s.to_string())),
        Some(s) => Err(Error::Llm(format!("translation produced a too-long from filter: {s}"))),
    }
}
```

3f. `RawPlan` — add:
```rust
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    has_link: Option<bool>,
    #[serde(default)]
    extract_links: bool,
```

3g. `translation_system_prompt` — in the final rules paragraph, after the existing `Reply with ONLY a JSON object` sentence and its shape line, replace the shape line and extend rules so it reads:
```rust
         Reply with ONLY a JSON object — no markdown fence, no prose — exactly in this shape:\n\
         {\"match\": \"<FTS5 MATCH expression>\", \"date_from\": \"<YYYY-MM-DD or null>\", \"date_to\": \"<YYYY-MM-DD or null>\", \"from\": \"<substring of the sender's name or address, or null>\", \"has_link\": <true or null>, \"extract_links\": <true or false>}\n\n\
         Rules: the match expression must be non-empty, valid FTS5, and use only terms implied by the question; prefer a few precise quoted terms over long expressions. Use \"from\" when the question names a sender; set \"has_link\": true when the question asks for messages containing links; set \"extract_links\": true only when the question explicitly asks to collect, list, or gather links — then also set \"has_link\": true.",
```
(Keep the date-resolution sentence above unchanged.)

3h. `validate` — the `Ok(plan)` invalid-match fallback branch rebuilds the plan; carry the new fields through:
```rust
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
```

3i. `AskOutcome::empty` — the inner `QueryPlan { … }` literal needs `from: None, has_link: None, extract_links: false` too.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vault-core llm:: 2>&1 | tail -5`
Expected: all llm tests PASS (existing tests compile because they construct plans only via `translate`/`plain`).

- [ ] **Step 5: Commit**

```bash
git add crates/vault-core/src/llm.rs
git commit -m "feat(core): from/has_link/extract_links filters in the ask query plan"
```

---

### Task 3: SQL filters on both search legs (`storage.rs`)

**Files:**
- Modify: `crates/vault-core/src/storage.rs` (`search_filtered` ~line 1531, `vector_scan` ~2347)
- Modify: `crates/vault-core/src/llm.rs` (`run_plan` ~437, `rank_hybrid` ~486 — pass the new args)

**Interfaces:**
- Consumes: Task 2's `QueryPlan` fields.
- Produces: `Storage::search_filtered(match_query, date_from, date_to, from_filter: Option<&str>, require_link: bool, limit: u32)`, `Storage::vector_scan(model, dim, query, date_from, date_to, from_filter: Option<&str>, require_link: bool, limit: u32)`. Callers exist only in `llm.rs`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `crates/vault-core/src/storage.rs`:

```rust
    fn seeded_links() -> Storage {
        let s = Storage::open_in_memory().unwrap();
        let rows = [
            ("ana@example.com", "see https://example.com/a now"),
            ("Ana Example <ana@example.com>", "no links here, just words"),
            ("bo@example.com", "docs at https://example.com/b and HTTP://example.com/c"),
            ("carol@example.com", "nothing to see"),
        ];
        for (i, (author, body)) in rows.iter().enumerate() {
            s.insert_message(&crate::message::NewMessage::new(
                "test",
                format!("m{i}"),
                author,
                "2026-01-01T00:00:00Z",
                body,
            ))
            .unwrap();
        }
        s
    }

    #[test]
    fn search_filtered_from_is_case_insensitive_substring() {
        let s = seeded_links();
        let out = s
            .search_filtered("example", None, None, Some("ANA@"), false, 50)
            .unwrap();
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out.iter().all(|m| m.author.to_lowercase().contains("ana@")));
    }

    #[test]
    fn search_filtered_require_link_keeps_only_url_bodies() {
        let s = seeded_links();
        let out = s.search_filtered("example", None, None, None, true, 50).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|m| m.body.contains("://")));
    }

    #[test]
    fn search_filtered_combines_from_and_require_link() {
        let s = seeded_links();
        let out = s
            .search_filtered("example", None, None, Some("bo@"), true, 50)
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].author, "bo@example.com");
    }

    #[test]
    fn vector_scan_applies_the_same_filters() {
        let s = seeded_links();
        // Embed two messages (ids 1 and 3) with identical normalized vectors.
        let model = "test-embed";
        s.embed_messages_batch(model, 2, &[(1, vec![1.0, 0.0]), (3, vec![1.0, 0.0])])
            .unwrap();
        // Unfiltered: both ids are candidates.
        let all = s.vector_scan(model, 2, &[1.0, 0.0], None, None, None, false, 10).unwrap();
        assert_eq!(all.len(), 2, "{all:?}");
        // Sender filter: only message 1 (ana) survives.
        let filtered = s
            .vector_scan(model, 2, &[1.0, 0.0], None, None, Some("ana"), false, 10)
            .unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].message_id, 1);
        // Link filter: only message 1 (has a URL) survives.
        let linked = s
            .vector_scan(model, 2, &[1.0, 0.0], None, None, None, true, 10)
            .unwrap();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].message_id, 1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vault-core storage::tests::search_filtered_from 2>&1 | tail -5`
Expected: compile error — `search_filtered` takes 4 args.

- [ ] **Step 3: Implement**

3a. `search_filtered` — new signature and SQL (the two new lines go after the date lines in the `WHERE`; `%`/`_` inside the value pass through as LIKE wildcards — document that in the doc comment):

```rust
    pub fn search_filtered(
        &self,
        match_query: &str,
        date_from: Option<&str>,
        date_to: Option<&str>,
        from_filter: Option<&str>,
        require_link: bool,
        limit: u32,
    ) -> Result<Vec<Message>> {
```

SQL (replace `?4` limit with `?6`, add two lines before `ORDER BY`):
```sql
            WHERE archive_fts MATCH ?1
              AND (?2 IS NULL OR m.sent_instant >= ?2)
              AND (?3 IS NULL OR m.sent_instant < ?3)
              AND ((?2 IS NULL AND ?3 IS NULL) OR m.sent_instant IS NOT NULL)
              AND (?4 IS NULL OR m.author LIKE '%' || ?4 || '%')
              AND (?5 = 0 OR m.body LIKE '%http%://%')
            ORDER BY bm25(archive_fts, {BM25_WEIGHT_BODY}, {BM25_WEIGHT_AUTHOR}, {BM25_WEIGHT_SUBJECT}), m.id
            LIMIT ?6
```
Bind: `params![match_query, from, to_exclusive, from_filter, require_link, limit]`.

3b. `vector_scan` — new params after `date_to`: `from_filter: Option<&str>, require_link: bool`. The join is now needed when *any* filter is active:
```rust
        let (sql, has_filters) = if from.is_some() || to_exclusive.is_some() || from_filter.is_some() || require_link {
            (
                "SELECT e.message_id, e.embedding
                 FROM message_embeddings AS e
                 JOIN archive_messages AS m ON m.id = e.message_id
                 WHERE e.model = ?1
                   AND (?2 IS NULL OR m.sent_instant >= ?2)
                   AND (?3 IS NULL OR m.sent_instant < ?3)
                   AND ((?2 IS NULL AND ?3 IS NULL) OR m.sent_instant IS NOT NULL)
                   AND (?4 IS NULL OR m.author LIKE '%' || ?4 || '%')
                   AND (?5 = 0 OR m.body LIKE '%http%://%')",
                true,
            )
        } else {
            ("SELECT message_id, embedding FROM message_embeddings WHERE model = ?1", false)
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = if has_filters {
            stmt.query(params![model, from, to_exclusive, from_filter, require_link])?
        } else {
            stmt.query(params![model])?
        };
```

3c. `llm.rs::run_plan` — pass the plan's filters:
```rust
    storage.search_filtered(
        &planned.plan.match_expr,
        planned.plan.date_from.as_deref(),
        planned.plan.date_to.as_deref(),
        planned.plan.from.as_deref(),
        planned.plan.has_link.unwrap_or(false),
        limit,
    )
```

3d. `llm.rs::rank_hybrid` — the `storage.vector_scan(…)` call gains `planned.plan.from.as_deref(), planned.plan.has_link.unwrap_or(false),` after the two date args.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vault-core 2>&1 | tail -5`
Expected: whole crate PASS (no other callers of the two methods).

- [ ] **Step 5: Commit**

```bash
git add crates/vault-core/src/storage.rs crates/vault-core/src/llm.rs
git commit -m "feat(core): sender/link SQL filters on the FTS and vector legs"
```

---

### Task 4: Pipeline wiring — extraction, resolve summary, `AskOutcome.collected`

**Files:**
- Modify: `crates/vault-core/src/llm.rs` (imports, `summarize_results` ~588, `ask` ~663, `AskOutcome` ~629, tests)

**Interfaces:**
- Consumes: Task 1 `extract_links`, Task 2 plan fields.
- Produces (used by Tasks 5, 6): `AskOutcome { …, collected: Vec<CollectedLink> }` (serde snake_case); `summarize_results(llm, question, results, top, collected: &[CollectedLink]) -> Summarized`; `pub const SUMMARY_LINK_LIMIT: usize = 50`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `llm.rs` (and fix the mechanical compile breakage: every existing `summarize_results(&llm, …, N)` call in tests gains a final `, &[]` argument):

```rust
    use crate::extract::{extract_links, CollectedLink};

    fn link_msg(id: i64, author: &str, body: &str) -> Message {
        Message {
            id,
            source: "test".into(),
            external_id: format!("l{id}"),
            channel: "email".into(),
            chat_id: String::new(),
            author: author.into(),
            sent_at: "2026-10-01T00:00:00Z".into(),
            body: body.into(),
            raw_json: "{}".into(),
        }
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
        assert!(user.contains("https://example.com/a · ana · 2026-10-01T00:00:00Z"), "{user}");
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
        let local =
            ask(&storage, None, &embedder, "collect links about deploys", 50, 8, today()).unwrap();
        assert!(local.collected.is_empty());
        // An LLM that plans extraction: the link is collected and the
        // summary prompt carries the collected-links section.
        let llm = MockLlm::with_replies([
            "{\"match\": \"deploys\", \"has_link\": true, \"extract_links\": true}".to_string(),
            "one link collected [1].".to_string(),
        ]);
        let out =
            ask(&storage, Some(&llm), &embedder, "collect links about deploys", 50, 8, today())
                .unwrap();
        assert_eq!(out.collected.len(), 1);
        assert_eq!(out.collected[0].url, "https://example.com/a");
        assert!(llm.prompts()[1].1.contains("Collected links"), "{}", llm.prompts()[1].1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p vault-core llm:: 2>&1 | tail -5`
Expected: compile error — `summarize_results` takes 4 args / no `collected` field.

- [ ] **Step 3: Implement**

3a. Import at the top of `llm.rs`: `use crate::extract::CollectedLink;` (and `#[cfg(test)] use crate::extract::extract_links;` inside `mod tests` — or a plain `use` in the tests module as shown).

3b. Next to `SUMMARY_BODY_LIMIT`:
```rust
/// Lines of collected links appended to the summary prompt (privacy cap).
pub const SUMMARY_LINK_LIMIT: usize = 50;
```

3c. `SUMMARY_SYSTEM_PROMPT` stays a const; in `summarize_results` build the system prompt conditionally and append the links block to the user prompt:

```rust
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
    let mut user = format!("Question: {question}\n\nMessages (numbered by relevance):\n\n{contexts}");
    let mut system = SUMMARY_SYSTEM_PROMPT.to_string();
    if !collected.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        'links: for link in collected {
            for hit in &link.hits {
                if lines.len() >= SUMMARY_LINK_LIMIT {
                    break 'links;
                }
                let author = if hit.author.is_empty() { "unknown" } else { &hit.author };
                let date = if hit.sent_at.is_empty() { "date unknown" } else { &hit.sent_at };
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
        // … unchanged arms
```

3d. `AskOutcome` — add `pub collected: Vec<CollectedLink>,` (document: "Collected links when the plan extracted them; empty otherwise."); `AskOutcome::empty()` sets `collected: Vec::new()`.

3e. `ask()` — after `let results = hybrid.results;`, extract and thread through:
```rust
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
```
and add `collected,` to the final `AskOutcome { … }`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p vault-core 2>&1 | tail -5`
Expected: PASS (all existing summarize call sites updated with `, &[]`).

- [ ] **Step 5: Commit**

```bash
git add crates/vault-core/src/llm.rs
git commit -m "feat(core): collect links in the ask pipeline and resolve them in the summary"
```

---

### Task 5: Tauri wiring — `run_ask` extraction + `save_text` command

**Files:**
- Modify: `src-tauri/src/lib.rs` (`run_ask` ~1050, new command near `export_messages_v2` ~946, `generate_handler` list ~3241)
- Modify: `src/ipc.ts` (new `saveText`)

**Interfaces:**
- Consumes: Task 4 signatures (`summarize_results(…, &collected)`, `AskOutcome.collected`).
- Produces (used by Task 7): `ipc.saveText({ path, contents }): Promise<null>` → Tauri command `save_text(path, contents)`.

- [ ] **Step 1: Write the failing test**

In `src-tauri/src/lib.rs` `mod tests` (~line 3302), next to the existing `run_ask` consent tests, add (reusing `ask_pool`, `recorder_selection`, `spawn_llm_recorder`, `ask_today` exactly as the neighbors do):

```rust
    #[test]
    fn run_ask_collects_links_when_the_plan_extracts() {
        let (_tmp, pool) = ask_pool(2);
        // One message must carry a URL for the extractor to find; the plan's
        // has_link filter keeps only that row.
        pool.submit_write(move |s| {
            s.insert_message(&vault_core::message::NewMessage::new(
                "test",
                "links".into(),
                "Author 00",
                "2026-10-01T12:00:00Z",
                "consent marker 42 about vault topics and https://example.com/a".to_string(),
            ))?;
            Ok(())
        })
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (base_url, server) = spawn_llm_recorder(
            vec![
                "{\"match\": \"consent\", \"has_link\": true, \"extract_links\": true}".into(),
                "collected one link [1].".into(),
            ],
            Arc::clone(&stop),
        );

        let outcome = run_ask(
            &ActiveVault::Pool(Arc::clone(&pool)),
            &HashEmbedder::new(),
            recorder_selection(base_url),
            true,
            "collect all consent links",
            50,
            8,
            ask_today(),
            true,
        )
        .unwrap();

        assert!(outcome.plan.extract_links);
        assert_eq!(outcome.collected.len(), 1, "{:?}", outcome.collected);
        assert_eq!(outcome.collected[0].url, "https://example.com/a");
        assert_eq!(outcome.collected[0].hits[0].author, "Author 00");

        stop.store(true, Ordering::SeqCst);
        let bodies = server.join().expect("recorder thread");
        assert_eq!(bodies.len(), 2, "translate + summarize round trips");
        // The summarize round trip carries the collected-links section.
        assert!(bodies[1].contains("Collected links"), "{}", bodies[1]);
        pool.shutdown();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mailchat-vault --lib run_ask_collects 2>&1 | tail -5`
Expected: FAIL — `AskOutcome` has no field `collected` on the wired path.

- [ ] **Step 3: Implement**

3a. In `run_ask` (src-tauri/src/lib.rs), after the results/ranking block and before the summarize block:
```rust
    let collected = if planned.plan.extract_links {
        vault_core::extract::extract_links(&results)
    } else {
        Vec::new()
    };
```
pass `&collected` as the new last arg of `llm::summarize_results(…)`, and add `collected,` to the returned `AskOutcome { … }`. Check the exact `vault_core` import style used at the top of the file (`use vault_core::…`) and follow it.

3b. New command next to `export_messages_v2`:
```rust
/// Write a UTF-8 text file to an absolute path the frontend picked with
/// its save dialog (used by the collected-links export; no dialog logic
/// lives here).
#[tauri::command]
fn save_text(path: String, contents: String) -> Result<(), String> {
    std::fs::write(&path, contents.as_bytes()).map_err(|e| format!("write {path}: {e}"))
}
```

3c. Add `save_text,` to the `generate_handler![…]` list (next to the export commands).

3d. `src/ipc.ts` — in the Export section:
```ts
/** Write a UTF-8 text file to a path picked with the save dialog. */
export function saveText(args: { path: string; contents: string }): Promise<null> {
  return invoke<null>("save_text", args);
}
```

- [ ] **Step 4: Run tests + type check**

Run: `cargo test -p mailchat-vault --lib 2>&1 | tail -5` and `npm run check`
Expected: PASS, no type errors.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lib.rs src/ipc.ts
git commit -m "feat(app): wire link collection through llm_ask; add save_text export command"
```

---

### Task 6: TS display layer — types, plan line, consent texts, serializers

**Files:**
- Modify: `src/llmview.ts`
- Test: `src/llmview.test.ts`

**Interfaces:**
- Consumes: serde shapes from Tasks 2/4.
- Produces (used by Task 7): `LinkHit`, `CollectedLink`, `AskOutcome.collected?: CollectedLink[]`, `QueryPlan.{from, has_link, extract_links}`, `collectedCountLine(links)`, `linksToText(links)`, `linksToJson(links)`.

- [ ] **Step 1: Write the failing tests**

Add to `src/llmview.test.ts`:

```ts
import {
  collectedCountLine,
  linksToJson,
  linksToText,
  // …existing imports
} from "./llmview";

const links = [
  {
    url: "https://example.com/a",
    hits: [
      { message_id: 1, author: "ana", sent_at: "2026-10-01T00:00:00Z", context: "see the link" },
      { message_id: 2, author: "bo", sent_at: "2026-10-02T00:00:00Z", context: "again" },
    ],
  },
];

describe("collected links", () => {
  it("counts links and citing messages", () => {
    expect(collectedCountLine(links)).toBe("1 link from 2 messages");
    expect(collectedCountLine([])).toBe("0 links from 0 messages");
  });

  it("serializes to text with one block per link", () => {
    const text = linksToText(links);
    expect(text).toContain("https://example.com/a (2 hits)");
    expect(text).toContain("- ana · 2026-10-01T00:00:00Z · see the link");
  });

  it("serializes to pretty JSON", () => {
    expect(JSON.parse(linksToJson(links))).toEqual(links);
  });
});

describe("consent texts mention the link list", () => {
  it("consent prompt states the links cap", () => {
    expect(consentPromptText(zaiItem, 8)).toContain(
      "collect links, the collected link list (URLs and senders, capped at 50)",
    );
  });
  it("privacy note states the links cap", () => {
    expect(privacyNoteFor(zaiItem, 8)).toContain("capped at 50");
  });
});

describe("describePlan shows the new filters", () => {
  it("renders from and links-only segments", () => {
    const line = describePlan({
      plan: { match_expr: "x", date_from: null, date_to: null, from: "ana@", has_link: true, extract_links: true },
      plan_source: "llm",
      ranking: "fts_only",
    });
    expect(line).toContain("from: ana@");
    expect(line).toContain("links only");
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `npx vitest run src/llmview.test.ts 2>&1 | tail -5`
Expected: FAIL — `collectedCountLine` not exported.

- [ ] **Step 3: Implement in `src/llmview.ts`**

3a. Extend the mirrored types:
```ts
export interface QueryPlan {
  match_expr: string;
  date_from: string | null;
  date_to: string | null;
  from: string | null;
  has_link: boolean | null;
  extract_links: boolean;
}

/** Mirrors `vault_core::extract::LinkHit` (serde field names). */
export interface LinkHit {
  message_id: number;
  author: string;
  sent_at: string;
  context: string;
}

/** Mirrors `vault_core::extract::CollectedLink` (serde field names). */
export interface CollectedLink {
  url: string;
  hits: LinkHit[];
}

export interface AskOutcome {
  plan: QueryPlan;
  plan_source: PlanSource;
  ranking: RankingMode;
  results: unknown[];
  answer: string | null;
  notes: string[];
  /** Present when the plan extracted links (older fixtures may omit it). */
  collected?: CollectedLink[];
}
```

3b. `describePlan` — insert after the dates push:
```ts
  if (outcome.plan.from) parts.push(`from: ${outcome.plan.from}`);
  if (outcome.plan.has_link) parts.push("links only");
```

3c. Extend `consentPromptText` return (append before the final backtick segment):
```ts
    ` matching messages (bodies truncated to 500 chars), would be sent to ` +
    `${item.model} via ${item.host} to translate and summarize the ask. ` +
    `When your question asks to collect links, the collected link list ` +
    `(URLs and senders, capped at 50) is sent too.`
```
And in `privacyNoteFor`, in the network sentence, after `for the summary` add: `, plus the collected link list (URLs and senders, capped at 50) when your question asks to collect links`.

3d. New helpers at the end of the file:
```ts
// ---------------------------------------------------------------------------
// Collected links (ask-collect-links)
// ---------------------------------------------------------------------------

/** Glanceable count line for the collected block. */
export function collectedCountLine(links: CollectedLink[]): string {
  const messages = new Set(links.flatMap((l) => l.hits.map((h) => h.message_id))).size;
  const linkWord = links.length === 1 ? "link" : "links";
  const msgWord = messages === 1 ? "message" : "messages";
  return `${links.length} ${linkWord} from ${messages} ${msgWord}`;
}

/** Text export: one block per link, one line per citing hit. */
export function linksToText(links: CollectedLink[]): string {
  return links
    .map((link) => {
      const hits = link.hits
        .map((h) => `- ${h.author || "unknown"} · ${h.sent_at || "date unknown"} · ${h.context}`)
        .join("\n");
      return `${link.url} (${link.hits.length} hit${link.hits.length === 1 ? "" : "s"})\n${hits}`;
    })
    .join("\n\n");
}

/** JSON export: the raw collected shape, pretty-printed. */
export function linksToJson(links: CollectedLink[]): string {
  return JSON.stringify(links, null, 2);
}
```

- [ ] **Step 4: Fix existing fixtures, run tests**

Existing tests in `src/llmview.test.ts` that construct `QueryPlan` need the three new fields (`from: null, has_link: null, extract_links: false`); add them. Run: `npx vitest run src/llmview.test.ts 2>&1 | tail -5` → PASS. Then `npm run check` → clean.

- [ ] **Step 5: Commit**

```bash
git add src/llmview.ts src/llmview.test.ts
git commit -m "feat(ui): collected-link types, plan line, consent wording, serializers"
```

---

### Task 7: Ask UI — collected block, cited rows, export

**Files:**
- Modify: `index.html` (ask section ~line 209)
- Modify: `src/main.ts` (element lookups ~190, `renderAsk` ~409, new handlers near `exportSelection` ~1225)
- Modify: `src/styles/app.css` (ask block ~613-662)
- Test: `src/main.test.ts`

**Interfaces:**
- Consumes: Task 5 `ipc.saveText`, Task 6 helpers/types, existing `openDetail(id)`, `save` from `@tauri-apps/plugin-dialog`, `describeError`.
- Produces: DOM ids `#ask-collected`, `#ask-collected-count`, `#ask-collected-format`, `#ask-collected-export`, `#ask-collected-status`, `#ask-collected-list`.

- [ ] **Step 1: Write the failing test**

In `src/main.test.ts`, next to the existing ask test (~line 2058, `tauri.handlers.set("llm_ask", …)`), add (the file's helpers are `$`, `input`, `pressKey`, `boot`, and `vi.waitFor`):

```ts
it("renders collected links with cited hits and exports them", async () => {
  await boot();
  tauri.handlers.set("llm_ask", () => ({
    plan: { match_expr: "example", date_from: null, date_to: null, from: "ana@", has_link: true, extract_links: true },
    plan_source: "llm",
    ranking: "fts_only",
    results: [],
    answer: "here is what I found",
    notes: [],
    collected: [
      {
        url: "https://example.com/a",
        hits: [
          { message_id: 7, author: "ana", sent_at: "2026-10-01T00:00:00Z", context: "see the link" },
        ],
      },
    ],
  }));
  tauri.handlers.set("conversation_context_v2", () => ({}));
  input("#ask").value = "collect links from ana";
  pressKey(input("#ask"), "Enter");
  await vi.waitFor(() => expect($("#ask-collected").hidden).toBe(false));
  expect($("#ask-collected-count").textContent).toBe("1 link from 1 message");
  expect($("#ask-collected-list summary").textContent).toContain("https://example.com/a");
  // A hit opens its citing message in thread context.
  ($("#ask-collected-list .ask-source") as HTMLElement).click();
  await vi.waitFor(() =>
    expect(tauri.calls.some((c) => c.cmd === "conversation_context_v2" && (c.args as { id: number }).id === 7)).toBe(true),
  );
  // Export goes through the save dialog and save_text.
  ($("#ask-collected-export") as HTMLElement).click();
  await vi.waitFor(() =>
    expect(tauri.calls.some((c) => c.cmd === "save_text")).toBe(true),
  );
  const call = tauri.calls.find((c) => c.cmd === "save_text")!;
  expect((call.args as { contents: string }).contents).toContain("https://example.com/a");
});
```

(The dialog `save` mock already resolves to `dialog.savedPath`, so the export path is deterministic.)

- [ ] **Step 2: Run test to verify it fails**

Run: `npx vitest run src/main.test.ts 2>&1 | tail -8`
Expected: FAIL — `#ask-collected` does not exist.

- [ ] **Step 3: Implement**

3a. `index.html` — inside `#ask-section`, between `<div id="answer" hidden></div>` and `<div id="ask-notes" hidden></div>`:
```html
                <!-- ask-collect-links: the locally extracted, deduplicated
                     link list with the messages that cite each link. -->
                <div id="ask-collected" hidden>
                  <div class="row" style="margin-top: 6px">
                    <span id="ask-collected-count" class="meta" role="status"></span>
                    <label class="visually-hidden" for="ask-collected-format">Export format for collected links</label>
                    <select id="ask-collected-format" title="File format for the collected links">
                      <option value="text" selected>Text</option>
                      <option value="json">JSON</option>
                    </select>
                    <button id="ask-collected-export" type="button" title="Save the collected links to a file">Export links…</button>
                    <span id="ask-collected-status" class="meta" role="status"></span>
                  </div>
                  <ul id="ask-collected-list" aria-label="Collected links (each hit opens in thread context)"></ul>
                </div>
```

3b. `src/main.ts` — element lookups after the existing ask lookups (~line 196):
```ts
const askCollected = document.querySelector<HTMLElement>("#ask-collected")!;
const askCollectedCount = document.querySelector<HTMLElement>("#ask-collected-count")!;
const askCollectedFormat = document.querySelector<HTMLSelectElement>("#ask-collected-format")!;
const askCollectedExport = document.querySelector<HTMLButtonElement>("#ask-collected-export")!;
const askCollectedStatus = document.querySelector<HTMLElement>("#ask-collected-status")!;
const askCollectedList = document.querySelector<HTMLUListElement>("#ask-collected-list")!;
```
Add `collectedCountLine`, `linksToText`, `linksToJson` and type `CollectedLink` to the existing `./llmview` import.

3c. `renderAsk` — after the answer block:
```ts
  const collected = outcome.collected ?? [];
  if (collected.length > 0) {
    askCollectedCount.textContent = collectedCountLine(collected);
    renderCollected(collected);
    askCollected.hidden = false;
  } else {
    askCollected.hidden = true;
  }
```

3d. New render + export functions near `renderAsk`:
```ts
/** One <li> per link: a details/summary holding one clickable hit row per
 * citing message (same open-in-thread-context behavior as citations). */
function renderCollected(links: CollectedLink[]): void {
  askCollectedList.replaceChildren();
  for (const link of links) {
    const li = document.createElement("li");
    const details = document.createElement("details");
    details.open = links.length === 1 && link.hits.length === 1;
    const summary = document.createElement("summary");
    summary.textContent =
      link.hits.length === 1 ? link.url : `${link.url} (${link.hits.length} hits)`;
    details.append(summary);
    for (const hit of link.hits) {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "ask-source";
      const meta = document.createElement("span");
      meta.className = "meta";
      meta.textContent = `${hit.author || "unknown"} · ${hit.sent_at || "date unknown"}`;
      const ctx = document.createElement("span");
      ctx.textContent = hit.context;
      row.append(meta, document.createElement("br"), ctx);
      row.addEventListener("click", () => void openDetail(hit.message_id));
      details.append(row);
    }
    li.append(details);
    askCollectedList.append(li);
  }
}

/** Save the collected links through the OS save dialog (text or JSON). */
async function exportCollected(links: CollectedLink[]): Promise<void> {
  const format = askCollectedFormat.value === "json" ? "json" : "text";
  const ext = format === "json" ? "json" : "txt";
  const path = await save({
    title: "Export collected links",
    defaultPath: `mailchat-links.${ext}`,
    filters: [
      format === "json"
        ? { name: "JSON file", extensions: ["json"] }
        : { name: "Text file", extensions: ["txt"] },
    ],
  });
  if (typeof path !== "string") return;
  try {
    await ipc.saveText({ path, contents: format === "json" ? linksToJson(links) : linksToText(links) });
    askCollectedStatus.textContent = `Exported ${links.length} link${links.length === 1 ? "" : "s"}.`;
  } catch (e) {
    askCollectedStatus.textContent = describeError(e);
  }
}

askCollectedExport.addEventListener("click", () => {
  const collected = lastCollected;
  if (collected) void exportCollected(collected);
});
```
Module state `let lastCollected: CollectedLink[] | null = null;` set in `renderAsk` (`lastCollected = collected.length > 0 ? collected : null;`) — export always saves what is on screen.

3e. `src/styles/app.css` — append to the ask block (after the `#ask-results li` rule, ~line 662):
```css
/* ask-collect-links: one details/summary per URL, one .ask-source row per hit. */
#ask-collected-list { list-style: none; margin: var(--sp-1) 0 0; padding: 0; font-size: var(--text-meta); }
#ask-collected-list li { padding: 3px 0; }
#ask-collected-list summary { cursor: pointer; color: var(--accent); }
#ask-collected-list .ask-source {
  display: block;
  width: 100%;
  text-align: left;
  font: inherit;
  color: inherit;
  background: none;
  border: none;
  border-radius: var(--radius-m);
  padding: 3px 4px;
  cursor: pointer;
}
#ask-collected-list .ask-source:hover, #ask-collected-list .ask-source:focus-visible {
  background: var(--selection);
}
#ask-collected-list .ask-source .meta { color: var(--accent); }
```

- [ ] **Step 4: Run tests + type check**

Run: `npx vitest run src/main.test.ts 2>&1 | tail -8` → PASS (including all pre-existing tests). Then `npm run check` → clean.

- [ ] **Step 5: Commit**

```bash
git add index.html src/main.ts src/styles/app.css src/main.test.ts
git commit -m "feat(ui): collected-links block with cited hits and text/JSON export"
```

---

### Task 8: Documentation

**Files:**
- Modify: `docs/ai-search.md`
- Modify: `docs/superpowers/specs/2026-11-14-ask-collect-links-design.md` (Status line)

**Interfaces:** none.

- [ ] **Step 1: Add the "Collect links" section**

In `docs/ai-search.md`, after the "AI consent (M5)" section, add:

```markdown
## Collect links (ask-collect-links)

A question like *“collect all links ana sent about deploys”* can carry three
extra plan fields the LLM may set: `from` (case-insensitive substring the
sender must contain), `has_link` (only messages whose body contains a URL),
and `extract_links`. The first two are SQL bound filters applied on both the
FTS and the vector leg **before** the result limit; the third runs a local,
dependency-free scanner over the result bodies and returns every http(s) URL
deduplicated, each with sender, date, a ±60-char context, and a clickable
citations trail into the original messages. The list renders and exports
(Text/JSON) fully locally — without any LLM.

Privacy: when extraction is active, the summary prompt additionally contains
the collected link list (`URL · sender · date`, capped at 50 lines). The
consent prompt says exactly this before anything is sent.
```

- [ ] **Step 2: Update the spec status**

In `docs/superpowers/specs/2026-11-14-ask-collect-links-design.md`, change the Status line to `Status: implemented`.

- [ ] **Step 3: Commit**

```bash
git add docs/ai-search.md docs/superpowers/specs/2026-11-14-ask-collect-links-design.md
git commit -m "docs: collect-links section in ai-search.md; spec implemented"
```

---

## Final verification

- [ ] `cargo test -p vault-core 2>&1 | tail -3` — PASS
- [ ] `cargo test -p mailchat-vault --lib 2>&1 | tail -3` — PASS
- [ ] `npx vitest run 2>&1 | tail -3` — PASS
- [ ] `npm run check` — clean
- [ ] `cargo clippy -p vault-core --all-targets 2>&1 | tail -3` — no new warnings
- [ ] Push `feat/ask-collect-links`, open a squash-merge PR to `main` (repo workflow), summarize the feature in the PR body.

# Mailchat Vault — Retrieval Quality Triage: Slices for Literature-Review Items 4–12

> **For agentic workers:** This document is a **triage**, not one implementable slice. It dispositions the nine open recommendations of the [literature review](../../evidence/literature-review-2026-11-13.md) into ordered slices (R1–R5), one decision-gated strategic item, one codified decision rule, and one explicit deferral. A follow-up task picks up **exactly one slice**; its steps use checkbox (`- [ ]`) syntax for tracking. REQUIRED SUB-SKILL for implementation tasks: superpowers:subagent-driven-development.

**Goal:** Every open recommendation (items 4–12) gets a slice assignment with order and dependencies, an explicit deferral, or a decision-gated marker — each slice naming its goal, target files and seams, harness- or suite-based acceptance, required gates, and the constraints it touches. Items 1–3 already landed (delivery: merged as `836e9dd`; measured results in [retrieval-slice-1-results-2026-11-14.md](../../evidence/retrieval-slice-1-results-2026-11-14.md)); this plan starts from that baseline.

**Architecture (seams verified on `main` at `67d7daa`, confirming the [recommendations audit](../../evidence/retrieval-recommendations-audit-2026-11-14.md)):**

- **Fusion and ranking:** `pub const RRF_K: u32 = 60` and `rrf_fuse` (pure rank fusion, ids in/ids out) at `crates/vault-core/src/vector.rs:328-351`, consumed only by `rank_hybrid` (`crates/vault-core/src/llm.rs:591-634`), which is called from the `ask` pipeline (`llm.rs:826`; Tauri composition in `run_ask`/`llm_ask`, `src-tauri/src/lib.rs:1004+`). **No opens table or time-decay logic exists anywhere** — the audit's searches were all negative; the ranking slice adds both, and the opens table plus its recording surface is that slice's largest sub-task.
- **Settings toggles:** the `Options` serde-backfill pattern at `crates/vault-core/src/settings.rs:83-126` — explicit `#[serde(default)]` for default-off booleans (older settings files backfill), range-checked numerics, one `Default` impl; surfaced on the Settings General page (`index.html` `#settings-general`, ~512–556) and round-tripped whole through `settings_get`/`settings_save`. The Tauri layer already threads one option into the ask pipeline this way (`summarize_top`, `src-tauri/src/lib.rs:1031`).
- **Query expansion:** the translate step at `crates/vault-core/src/llm.rs:452-477` (`translation_system_prompt`, `translate`), `QueryPlan`/`from_llm_reply` (`llm.rs:335`, `:368`), and `validate` with the plain-term fallback (`llm.rs:509-537`); `Storage::is_valid_match` (`crates/vault-core/src/storage.rs:1725`) is the free re-validation seam for any rewritten MATCH expression.
- **Ask output (faithfulness, small-to-big):** `SUMMARY_SYSTEM_PROMPT`, `context_block`, `lim_order`, `summarize_results` at `crates/vault-core/src/llm.rs:640-758`; citations render client-side via `splitCitations` (`src/llmview.ts:157-169`) and `renderAnswerLinked` (`src/main.ts:433-449`), which today only clamp out-of-range markers to plain text. Thread windows exist as reading-pane APIs (`Storage::chat_context`, `storage.rs:1915`; `conversation_context_v2` IPC, `src-tauri/src/lib.rs:396`).
- **Threading:** `resolve_email_thread` (one-hop In-Reply-To/References, conversations keyed `email:thread:<root-most ref>`) and `adopt_refs` at `crates/vault-core/src/storage.rs:3271-3340`; subject grouping does not exist. Note the `conversations.confidence` CHECK admits only `verified`/`user_linked`/`legacy_unresolved` (`storage.rs:202`) — a distinct confidence value for subject-grouped threads is a rebuild migration, and the slice must decide and record that. Tests live in `crates/vault-core/tests/conversations.rs` and `crates/vault-core/tests/integrity/conversation_keys.rs`.
- **Consent:** the M5 gate — `llm_ask`'s `consent` parameter and `run_ask`'s downgrade-to-local (`src-tauri/src/lib.rs:1004-1045`), bound client-side by the session-scoped, never-persisted `aiConsentGranted` (`src/main.ts:350-355`). Every LLM-involving task below stays inside this gate.
- **Harness:** `crates/vault-core/tests/retrieval_eval.rs` — 40-message corpus (24 email with production `"{subject}\n\n{text}"` body composition, 16 chat), 16 labeled queries across seven families, inline recall@10/nDCG@10, three legs (FTS / vector / fused), reversed-ranking sanity check, committed floors at lines 72–77 (FTS 0.78/0.80, vector 0.76/0.75, fused 0.89/0.89) sitting ~5–10% below the recorded baseline (fused macro 0.943/0.941). This is the acceptance instrument for ranking and expansion slices; threading extends `tests/conversations.rs`; Ask-output slices extend `tests/llm_contract.rs`, `tests/hybrid_ask.rs`, and the harness where ranking is affected.
- **Schema migrations:** append-only `MIGRATIONS: &[(i64, &str)]` (`storage.rs:92`); never edit an applied version — a new table (the opens table) is one appended migration plus the `SUPPORTED_SCHEMA_VERSION` bump.

**Tech Stack:** This change is documentation-only. Implementation slices are Rust (`vault-core`, rusqlite with bundled SQLite FTS5), TypeScript (Vite/vitest) for UI-touching slices, Tauri v2 commands for new IPC. **No scheduled slice adds an external crate or npm dependency.**

## Global Constraints (bind every slice)

- **Measurable before tuned.** Every slice runs on the merged eval harness (`retrieval_eval.rs`) before any tuning; weight or threshold changes are accepted only against measured macro recall@10/nDCG@10, never taste. Harness honesty rule (slice-1 precedent): floors sit ~5–10% below measured macros, baselines are recorded in comment blocks, and floors are re-derived — up or down — only against re-measured numbers when the query set changes.
- **Offline promise holds.** No runtime model downloads, ever. No scheduled slice touches the network.
- **Consent.** Any LLM round trip (expansion terms, critique pass) stays inside the existing session consent gate, reuses already-bounded payloads, and never persists consent. Consent copy changes ship in the same change as the payload change they describe.
- **No ranking telemetry.** Opens recording, decay, and any personalization signal stay in local SQLite tables and never leave the machine; nothing about ranking is reported anywhere.
- **Excluded by the evidence** (restated from the review's not-recommended list; stays out of every slice): ML-based threading, folder-first organization features, late-interaction token-vector indexes, any runtime model download, and cloud-side or background ranking telemetry.
- **Out of scope for every slice:** the email subject-in-body prefix change (`mail.rs` composes `"{subject}\n\n{text}"`; open decision recorded in the slice-1 results, Finding 2). It alters stored bodies, revision hashes, and re-import dedup comparisons, and needs its own migration-reasoned change.
- **Standard gates** for every implementation slice (from slice 1): `cargo test -p vault-core` green, `cargo fmt --check` clean, `cargo clippy -p vault-core --all-targets -- -D warnings` clean. UI-touching slices add `npm run check` and `npx vitest run`. E2e/WebKitGTK cannot run on development hosts (the standing limitation every e2e-bearing slice records); CI owns it.
- **Commit style** follows the repo: `feat(search): …` / `test(search): …` / `docs: …`, imperative, scoped.

## Disposition of items 4–12

| Item | Recommendation | Disposition | Order / dependencies |
| --- | --- | --- | --- |
| 4 | Recency- and history-aware ranking | **Slice R1**, first half (decay toggle; opens table + recording + boost) | 1st implementation slice; may land ahead of item 8 |
| 8 | Tunable fusion (exposed k, optional weighted variant) | **Slice R1**, second half | Ordered after item 4 inside R1; splittable into its own slice without changing this triage |
| 5 | Local PRF + consent-gated LLM expansion | **Slice R2** | After R1 — both reshape the fused inputs; sequencing avoids double tuning |
| 6 | Citation-faithfulness checks | **Slice R3** | Independent of R1/R2/R5 |
| 7 | Small-to-big thread-context retrieval | **Slice R4** | After R3 (shared `summarize_results` seams); benefits from R5 landing (soft, not required) |
| 11 | JWZ threading steps 4–5 | **Slice R5** | Independent; can land any time |
| 9 | Bundled ONNX int8 MiniLM-class embedder | **Strategic, decision-gated — blocked**; not scheduled | Requires the explicit native-dependency decision; see the strategic section |
| 10 | Quantization/ANN thresholds | **Codified in this document** (decision rule, no code) | Immediate; re-measured when triggers fire |
| 12 | Near-duplicate SimHash/MinHash fingerprints | **Deferred**; spike only when triggered | Until duplicates are an observed complaint; see the deferral section |

Ordering rationale: R1 first because it is the cheap win the review ranked highest among the open items and because both of its halves modify fused scoring — doing them as one (ordered) slice avoids tuning fusion twice. R2 second because expansion changes what the FTS leg feeds fusion; measuring it after R1's shape is final keeps the harness deltas attributable. R3/R4 are Ask-output quality on seams disjoint from ranking (R3 before R4: both touch `summarize_results` and the citation path). R5 is fully independent (storage/conversations only). Items 9, 10, 12 are not implementation slices (decision-gated, codified, deferred respectively).

---

## Slice R1 — Ranking: recency decay, local opens, tunable fusion (items 4 + 8)

**Goal:** Make fused ranking recency- and history-aware behind default-off toggles (item 4), then expose fusion tuning while unweighted RRF k=60 stays the shipped default (item 8). Both halves modify fused scoring, so they are one ordered slice to avoid double tuning.

**Sizing and split rule:** Three ordered tasks. If a single run cannot carry all three, split after Task 1+2 (item 4) and let Task 3 (item 8) go in the next slice — the order and acceptance are unchanged. The opens table (Task 2) is the largest sub-task: it adds a new table and a recording surface with no existing pattern to copy; size it explicitly.

**Target files and seams:** `vector.rs:328-351` (`RRF_K`, `rrf_fuse` — gains parameterized k/weights and, for decay, a score-preserving variant or a post-fusion re-rank hook), `llm.rs:591-634` (`rank_hybrid` — threads settings, applies decay using `Message::sent_at` via `storage.get_many`, boosts opened ids), `llm.rs:808-851` and `src-tauri/src/lib.rs:1004+` (pipeline/IPC threading, following the `summarize_top` pattern at `lib.rs:1031`), `settings.rs:83-126` (new `Options` fields, serde-backfilled default-off), `storage.rs:92` migrations (opens table), `src-tauri/src/lib.rs:385` (`message_detail_v2` — the recording surface), `index.html` `#settings-general` + its save path (toggle UI), `src/main.test.ts` / settings specs (UI coverage).

### Task 1: Time-decay toggle (item 4a) — off by default

- [ ] Add `Options::recency_bias` (bool, `#[serde(default)]` — default **off**: personal archives stay neutral until asked) and a range-checked `recency_half_life_days` (e.g. 1..=3650, default 365) following the serde-backfill pattern; settings round-trip tests mirror the existing options tests.
- [ ] Apply the decay multiplier to fused scores in `rank_hybrid` after `rrf_fuse` (join `sent_at` through `get_many`; messages with unknown time are unaffected); deterministic tie-breaks stay (score desc, then id asc — same contract as `rrf_fuse`).
- [ ] Unit tests in `llm.rs`/`vector.rs`: with the toggle off the fused order is **byte-identical** to today's; with it on, a newer relevant message outranks an older one of equal fused strength within the half-life math; unknown-`sent_at` rows never panic and stay ordered by the old rule.
- [ ] Harness: run `retrieval_eval.rs` with defaults — macros and floors unchanged (decay off must be a no-op); add a targeted ordering assertion on the existing date-clustered corpus pair (Kestrel thread) with a decay-on config, or extend the corpus with one dated pair if the existing fixtures cannot express it.
- [ ] Settings UI: the toggle on the General page with its save round trip; `main.test.ts`/settings coverage per the existing option-toggle tests.

### Task 2: Local opens table, recording surface, opens-boost (item 4b)

- [ ] New table `message_opens` (e.g. `message_id INTEGER NOT NULL REFERENCES archive_messages(id), opened_at TEXT NOT NULL, PRIMARY KEY (message_id)` or an append-only counter — pick one shape, document why) via one appended `MIGRATIONS` entry and the version bump; `tests/integrity` coverage follows the existing migration tests.
- [ ] Recording surface: record one open per `message_detail_v2` serve (the UI's open-in-context path) — local SQLite only, never a network or telemetry surface; document that in the table's comment and the feature reference.
- [ ] Boost: previously-opened messages get a small, bounded rank bonus in `rank_hybrid` (e.g. an additive RRF-scale term or bounded score multiplier) behind `Options::history_boost` (bool, default **off** — cold-start care: no boost without recorded opens; empty table ⇒ identical behavior). Cap the effect so it cannot dominate relevance (the review's risk note).
- [ ] Unit tests: recording writes exactly one row per open; boost math is bounded and deterministic; with the toggle off or the table empty, output is byte-identical to today.
- [ ] Harness: defaults leave floors untouched; add seeded-opens ordering assertions (open one of two equally-ranked rows, assert the boosted order under the toggle) — seed through the public recording path, not direct SQL.
- [ ] UI: nothing visible beyond the toggle (opens are not a user-facing list in this slice).

### Task 3: Tunable fusion (item 8) — unweighted RRF k=60 stays the default

- [ ] Parameterize `rrf_fuse` (k; optional per-leg weight for a weighted-RRF or normalized-score convex-combination variant — pick one, justify against Bruch et al. 2022, keep the unweighted path the shipped default and exactly the current formula at k=60).
- [ ] Expose `Options::rrf_k` (range-checked, e.g. 1..=1000, serde default 60) and, if the weighted variant ships, its weight (default = unweighted); thread through `rank_hybrid` via the caller like `summarize_top`.
- [ ] Sweep k (and the weight, if present) across the harness corpus in the task report (a `const` table of tried values + measured macros); accept any non-default recommendation only if fused macro recall@10 and nDCG@10 are ≥ the recorded baseline — else keep defaults and record that.
- [ ] Unit tests: parameterized `rrf_fuse` at the default equals today's output for the existing fixtures; weights/k boundaries clamp; determinism holds.
- [ ] Harness: floors unchanged at shipped defaults (the floors pin the default path); the sweep is recorded evidence, not a gate change.
- [ ] Settings UI for k only if the product wants it visible; otherwise document it as an advanced option — the slice decides and records.

**Acceptance (slice):** All three gates green; harness floors unchanged at shipped defaults with a re-measured baseline comment; decay/boost proven no-ops when off (byte-identical ordering asserted); each effect proven on when toggled; opens recorded locally only; sweep recorded.

**Gates:** Standard gates (three cargo gates; `npm run check` + `npx vitest run` — UI touched).

**Constraints touched:** no-telemetry (opens table stays local; recording is not exposed as a metric anywhere), offline (untouched), consent (untouched — no LLM involved), harness honesty (floors re-derived only against re-measured macros).

---

## Slice R2 — Query expansion: local PRF + consent-gated LLM terms (item 5)

**Goal:** Fix vocabulary mismatch on short/weak queries two ways: (a) fully local pseudo-relevance feedback that OR-expands a weak FTS result set (Rocchio-style), and (b) expansion terms requested in the existing translate response and merged into the MATCH (Query2doc-style) — the plain-term fallback unchanged in every failure mode.

**Target files and seams:** `llm.rs:452-477` (`translation_system_prompt` gains an `expansion_terms` field — 5–10 terms implied by the question; `translate`/`from_llm_reply` parse it with the same reject-on-garbage discipline as `from`), `llm.rs:509-537` (`validate` — merged MATCH re-validated by `is_valid_match`; invalid ⇒ today's fallback verbatim), `llm.rs:540-552` (`run_plan`), a new local PRF helper (pure function in `llm.rs` or `query.rs`: takes the top few FTS hits, picks discriminating terms, OR-merges into the MATCH; triggered only on a weak result set — define "weak" as a measured threshold, e.g. hit count below N), `storage.rs:1725` (`is_valid_match` re-validation). No new deps; no new round trips.

**Steps:**

- [ ] Local PRF first (works with no LLM configured): weak-set trigger, term selection (documented, deterministic — e.g. top tf terms of top-k hits absent from the query), OR-merge, `is_valid_match` gate, re-run; if the expanded plan validates but returns nothing, keep the original results (never worse).
- [ ] Consent-gated LLM terms: prompt asks for `"expansion_terms": ["…", …]` in the same JSON; merged only when the merged expression validates; count/length caps like `MAX_MATCH_LEN`; misbehaving terms degrade to the unexpanded plan, never to a broken one.
- [ ] Provenance: the merged plan is visible as a note (e.g. "expanded with local terms: …") in `PlannedQuery::notes`/`describePlan`, so the UI stays honest about what ran.
- [ ] Unit tests (`MockLlm`): expansion-term parsing (valid, missing, oversized, garbage); merged-plan validation both ways; fallback path byte-identical to today when the LLM reply lacks terms; PRF determinism and the never-worse rule.
- [ ] `tests/llm_contract.rs`: the translate contract gains the optional field (old replies without it still parse — serde default).
- [ ] `tests/hybrid_ask.rs`: end-to-end with and without expansion.
- [ ] Harness: macros must not regress at shipped behavior; add or adapt labeled vocabulary-mismatch queries (the paraphrase/synonym family) with a recorded baseline comment; expansion accepted only if the affected family's macros improve without degrading the others.

**Acceptance (slice):** Standard gates green; harness floors hold with any new queries' baselines recorded; fallback behavior proven unchanged in every failure mode; every expansion path re-validated by `is_valid_match`.

**Gates:** Standard gates (cargo only unless the plan-note UI text changes, which adds `npm run check` + vitest).

**Constraints touched:** consent (LLM terms requested inside the existing consented translate round trip — same payload class, no extra round trip; consent copy updated only if the merged plan is deemed a new disclosure — the slice records the decision), offline (PRF half is pure local), harness honesty (family-level deltas recorded).

---

## Slice R3 — Citation faithfulness on Ask output (item 6)

**Goal:** Close the loop the citation UI opened: a local structural check that every `[n]` resolves to a real row in the summary window and each answer sentence overlaps its cited row's text — with a UX that annotates rather than silently fails — plus an optional consented critique pass as a second task.

**Target files and seams:** `llm.rs:640-758` (`summarize_results` — its `Summarized`/`AskOutcome` gains a faithfulness report computed after the answer), a new pure checker (in `llm.rs` or a small module: sentence-split the answer, map markers to rows via the same `[1]..[top]` contract as `context_block`/`splitCitations`, compute lexical overlap against the cited row's body), `src/llmview.ts:157-169` + `src/main.ts:433-449` (annotation rendering — out-of-range marker clamping stays exactly as-is, distinct from verification), `src-tauri/src/lib.rs:1047+` (`run_ask` — wires the report; the optional critique pass adds one more consented round trip over the same bounded payload).

### Task 1: Local structural check (S)

- [ ] Checker semantics: every citation index must resolve (1..=summary window); every answer sentence carrying citations must share lexical overlap with at least one cited row (token-overlap threshold chosen and pinned by tests); uncited sentences are reported but do not fail the answer (per ALCE's citation recall/precision framing).
- [ ] Failure mode is annotation, not refusal: the report renders in the Ask panel (e.g. a status line "3 of 4 cited sentences verified against their sources"); UX copy states the paraphrase limitation (lexical checks have false negatives) — the review's explicit risk note.
- [ ] Unit tests (`MockLlm`): answers with resolvable/unresolvable markers, supportive/paraphrased/unrelated sentences, empty answers, marker-at-window-edge; the reversed case (marker beyond `maxCite`) matches today's plain-text clamp.
- [ ] `tests/llm_contract.rs` + `tests/hybrid_ask.rs`: the report is part of the ask outcome contract (serde shape mirrored in `src/llmview.ts`, snake_case).
- [ ] `src/main.test.ts`/`src/llmview.test.ts`: annotation rendering for verified/unverified sentences.
- [ ] Harness: ranking unaffected — run to confirm floors hold (no floor change expected).

### Task 2 (optional, separate follow-up task if not carried): Consented critique pass (M)

- [ ] One additional LLM round trip inside the existing session consent gate: "is each cited sentence supported by row n?" over the same bounded payload; verdicts merged into the report; output gating only as annotation (no silent refusal) unless a product decision says otherwise — recorded in the slice.
- [ ] Consent copy updated in the same change (the extra round trip is a disclosure).
- [ ] Tests: recorder-backed `run_ask` tests in `src-tauri` (the `ask_pool`/`spawn_llm_recorder` pattern) asserting exactly one extra round trip under consent and none without; zero-network behavior unchanged.

**Acceptance (slice):** Standard gates green (+ `npm run check`, vitest — UI touched); every citation path (resolve + overlap) covered by unit tests; annotation, not refusal, is the shipped failure mode; critique pass (if carried) proven consent-gated and payload-bounded.

**Gates:** Standard gates plus frontend gates; e2e specs (Ask mock) updated only if the answer surface changes shape — CI owns their run.

**Constraints touched:** consent (Task 2 only — same gate, extra round trip disclosed), offline (Task 1 is pure local), no excluded items.

---

## Slice R4 — Small-to-big: thread context around cited messages (item 7)

**Goal:** When the summarizer cites a message, feed the surrounding thread window (±N messages or a character budget) for that row — the top-8 selection unchanged — with the payload growth disclosed in consent copy. Per-thread summaries (RAPTOR-lite) are an optional follow-up task, not this slice.

**Target files and seams:** `llm.rs:649-758` (`context_block` gains an optional window block per cited row, bounded; `summarize_results` threads it), `storage.rs:1915` (`chat_context` — the existing radius-window query is the data source for cited rows' neighbors), `src-tauri/src/lib.rs:1047+` (`run_ask` fetches windows — decide where the fetch lives so no storage lock crosses an LLM round trip, per the existing composition note), `src/llmview.ts` (`consentPromptText`/`privacyNoteFor` — the payload sentence grows), `tests/llm_contract.rs`, `tests/hybrid_ask.rs`, `src/llmview.test.ts`.

**Steps:**

- [ ] Window contract: for each of the top rows actually cited (or all top rows — pick one, record why), fetch ±N neighbors (start N=2) or a per-thread character budget (start ~2 KB, a named `const`); windows are labeled as context (e.g. a `Context around [k]:` block), never renumbered — citation numbers stay by rank.
- [ ] Consent copy: the prompt/privacy note sentence grows to say surrounding thread messages (bounded) are sent for cited rows — tests pin the wording like the collected-links cap wording is pinned.
- [ ] Unit tests (`MockLlm`): prompt shape with/without windows; budget truncation; a cited row in a thread vs. a standalone message; no windows when the toggle is off (if a toggle ships) or when no context exists.
- [ ] `tests/hybrid_ask.rs`: end-to-end prompt contents for a thread-linked corpus fixture.
- [ ] Harness: ranking unchanged (selection untouched) — run to confirm floors hold.
- [ ] Optional follow-up task (not this slice unless explicitly added): per-thread summaries computed incrementally at import and indexed like messages — if picked up, it is its own slice with its own harness acceptance and a staleness-on-re-import story.

**Acceptance (slice):** Standard gates green (+ frontend gates — consent copy touched); window budget pinned by tests; consent copy accurate to the shipped payload; ranking floors hold.

**Gates:** Standard gates plus frontend gates.

**Constraints touched:** consent (payload growth disclosed — the cap is the disclosure), offline (local fetch only), harness honesty (ranking untouched — confirmed, not assumed).

---

## Slice R5 — Threading: JWZ steps 4–5 (item 11)

**Goal:** Extend one-hop threading with (a) placeholder parents with child promotion (RFC 5256 REFERENCES semantics: siblings referencing a missing message-id group under the shared root key, children promoted when the parent is absent) and (b) `Re:`-stripped subject grouping for unlinked messages — toggleable and guarded against over-merging.

**Target files and seams:** `storage.rs:3271-3340` (`resolve_email_thread`/`adopt_refs` — the `email:thread:<root-most ref>` keying is the placeholder-parent mechanism; step 4 is mostly present, the slice verifies and completes child promotion for every reachable case), new subject-normalization + grouping (a pure helper: strip `Re:`/`Fwd:`/`Fw:` prefixes case-insensitively, collapse whitespace; group under a distinct provider key namespace, e.g. `email:subject:<normalized>`), `storage.rs:202` (the `conversations.confidence` CHECK admits only `verified`/`user_linked`/`legacy_unresolved` — a distinct `subject_grouped` value is a rebuild migration; the slice either carries it or records why `verified`/`legacy_unresolved` was kept), tests in `tests/conversations.rs` and `tests/integrity/conversation_keys.rs`.

**Steps:**

- [ ] Verify and complete step 4 (dummy/placeholder parents): every shape where multiple messages reference the same missing mid, where the root arrives last, and where a reply cross-links two threads — pin each with a conversations test (several exist; extend rather than rewrite).
- [ ] Subject grouping (step 5): normalize subjects; **skip generic subjects** (a documented stoplist/min-distinctiveness guard — "Re: hi" must not merge the world); group only unlinked messages (never re-group an existing `email:thread:` conversation); JWZ container ordering for the label.
- [ ] Toggle: subject grouping behind an `Options` bool (serde default decision recorded in the slice — recommend off until measured, per the over-merge risk; the guard reduces but does not eliminate it).
- [ ] Conversation-key integrity: `tests/integrity/conversation_keys.rs` extended for the new key namespace; no re-keying of existing conversations.
- [ ] Idempotence: re-importing the same mbox twice must not split or duplicate groups (the conversations dedup tests are the pattern).
- [ ] Acceptance runs on the owning suites (`tests/conversations.rs` + `tests/integrity/conversation_keys.rs`) — threading does not route through the eval harness; run the harness once to confirm no ranking side effects.

**Acceptance (slice):** Standard gates green; every JWZ step-4/5 shape pinned by a named test; generic-subject guard proven; toggle default recorded; no integrity regressions.

**Gates:** Standard gates (cargo only — no UI beyond the optional toggle, which adds the frontend gates if shipped).

**Constraints touched:** no ML threading (excluded list — this slice is exactly the heuristic alternative the evidence endorses), offline, no consent involvement.

---

## Strategic — Bundled ONNX int8 MiniLM-class embedder (item 9): DECISION-GATED, NOT SCHEDULED

**Status: blocked on an explicit user decision.** The ONNX runtime is a native build dependency previously ruled out for v1; that cost is the decision. **No slice schedules this item unless the user approves the dependency.** If approved, it becomes its own strategic slice with these preconditions (recorded now so the decision is informed):

- **Seam (exists today):** the `Embedder` trait (`vector.rs:85-93`) and model-id-versioned embedding rows (`run_embedding_job`, `vector.rs:282`; `embedding_counts`) were built for a drop-in; `HashEmbedder` (`hash-ngram-v1`, 512-dim) stays the zero-download default.
- **Build-time artifact only, never a runtime download** — the blob ships with the installer; its size (order 20–25 MB int8) stated in release notes and settings copy. This is the offline promise's hard edge.
- New model id ⇒ existing rows re-embed through the existing incremental job (no big-bang migration).
- Embedding quality proven on the eval harness **before** any switch is default: fused macro recall@10/nDCG@10 must beat the hash-ngram-v1 baseline on the same corpus.
- Scan cost measured at 384-dim (expected lower than today's 512) against the `vector_perf.rs` budgets; budgets updated in the same change.

**Gates if ever scheduled:** standard gates plus release/packaging evidence (bundle size, artifact checksums) — the release slices' pattern.

**Constraints touched:** the offline promise (build-time artifact only), the native-dependency decision (the blocker), harness honesty.

---

## Codified — Quantization/ANN thresholds (item 10): decision rule, no code

The rule (recorded here per the review; implement past it only when a trigger fires):

1. **Linear scan stays** while p95 vector scan fits the Ask latency budget: measured ~75 ms per 100k messages today (literature review); the CI guard is `SCAN_BUDGET_MS = 200` in `crates/vault-core/tests/vector_perf.rs` over a 100k synthetic corpus. Extrapolated pressure point: 1M messages ≈ 0.75 s of scan and ~512 MB of i8 blobs (512 B/message).
2. **First step past the threshold: product quantization** codes with the existing i8 blobs demoted to a re-rank set (Jégou et al. 2011 — keeps the simple scan codepath; ~8× memory cut at modest recall loss). PQ's recall loss **must be measured on the eval harness before any switch ships**; anisotropic variants (Guo et al. 2020) are the refined follow-up, not the first step.
3. **Graph indexes (HNSW via sqlite-vec/usearch) only after PQ is insufficient** — corpus past a few million messages or dimensions past PQ's comfort. Each adds native dependencies and tuning; each is its own decision, never bundled with a routine slice.
4. **Triggers to re-measure:** `vector_perf.rs` budget failures in CI; archives passing 100k measured messages on real hardware; any change of embedder dimension (i.e., if item 9 is ever approved).

---

## Deferred — Near-duplicate fingerprints (item 12)

**Deferred until duplicates are an observed complaint.** Evidence source when triggered: the failed-item ledger (IMAP) and re-import/dedup complaints. **Spike only** when triggered: a 64-bit SimHash or MinHash sketch per message body stored beside the revision hash (~8–100 B/message) with Hamming-distance/Jaccard lookups in plain SQLite tables (Charikar 2002; Broder 1997; Manku et al. 2007 — no index structure beyond tables needed at personal scale). Nothing scheduled; no spike opened by this triage.

---

### Wrap-up (controller)

- [x] Triage document created at `docs/superpowers/plans/2026-11-14-retrieval-quality-triage.md` following the slice-1 plan format (this change).
- [x] All nine items (4–12) dispositioned: slice with order/dependencies (4, 5, 6, 7, 8, 11), decision-gated marker (9), codified rule (10), explicit deferral with reason (12).
- [x] Status pointer added to the literature review's status block; the slice-1 results record's remaining-work list cross-references the triage (the implementation roadmap's remaining-work lists do not mention items 4–12 — verified by search; nothing to update there).
- [x] Docs-only scope: verified by the content checklist above, per the repo's standing docs-only validation decision (suites not rerun for documentation changes — the pattern recorded throughout the implementation roadmap's milestone records).
- [ ] Follow-ups: one task per slice (R1 first) picks up exactly one bounded unit from this plan.

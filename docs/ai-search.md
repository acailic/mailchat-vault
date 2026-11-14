# AI-assisted search (v1)

Ask a natural-language question (“what was said about the vault in October?”); the app translates it into an FTS5 query, runs it locally, then produces a short answer over the top results (RAG style). Entirely optional and fail-safe: without an API key the app behaves exactly like v0's plain full-text search.

## How it works

1. **Translate** — the question is sent to the LLM, which replies with a JSON plan: an FTS5 MATCH expression over `body`/`author` plus an optional sent-date range (it understands relative wording like “last march” against today's date).
2. **Search & rank** — the plan runs locally against SQLite. The LLM's expression is validated against the index first; anything unparseable or referring to unknown columns is replaced by a plain term search (usable date filters are kept). The expression is always passed as a bound MATCH parameter, never spliced into SQL. The FTS ranking is then fused with the local vector leg (see [Local embeddings](#local-embeddings--hybrid-search-v1)) when embeddings exist — a fully local step.
3. **Summarize** — a short answer is synthesized from **only the top 8 result rows** (bodies truncated to 500 chars), cited as [1], [2], …

The UI always shows the plan actually used (`FTS: … · dates: … · ranked: … · planned by LLM / fallback`) plus a privacy note describing exactly what the current mode sends.

## AI consent (M5)

Every network ask passes an **explicit consent gate**, enforced in the backend (not just the UI): `llm_ask` carries a `consent` flag, and **without `consent: true` a network provider is downgraded to a fully local ask before any LLM object is constructed** — no translate or summarize round trip can even start, so nothing leaves the machine (proven by tests against a mock endpoint that record zero requests without consent).

- **Per-question agreement**: the first ask of a session with a network provider selected runs locally and shows a consent prompt stating exactly what would be sent (the question, then the top-8 results, bodies truncated) and to whom (model via host), with **Send & ask for this session** / **Keep asking locally** choices. Agreeing immediately re-asks the same question with the network leg enabled.
- **Per session, never forever**: consent lives in memory only — it resets on every app start and is never written to any file or storage. The always-visible status line shows the current state (`AI consent: granted for this session …` / `not granted — asks run fully locally`), and **Revoke** resets it mid-session; the next ask then asks anew.
- **Answers carry their evidence**: citations `[1]`, `[2]` in the answer are rendered as clickable links, and the numbered source rows beneath the answer match those numbers — either opens the original archived message in its conversation context in the reading pane. Citations beyond the summarized top-8 window (the model never saw those rows) and out-of-range citations stay plain text, never dead links.

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

## Configuration (Settings UI, or environment variables)

Providers are configured on the **Settings screen**; the key is stored in the OS keyring when one is available (session memory otherwise) — never in the database or the repo, and never in the settings file. The environment variables below keep working as a fallback for headless use — and are themselves editable in Settings (see *Environment fallback overrides* below):

| Variable | Meaning | Default |
| --- | --- | --- |
| `ZAI_API_KEY` / `GLM_API_KEY` | API key for the default z.ai endpoint (`https://api.z.ai/api/anthropic`) | unset → LLM off |
| `ANTHROPIC_API_KEY` | Anthropic key, honored **only** together with an explicit `ANTHROPIC_BASE_URL` | unset → z.ai keys apply |
| `ANTHROPIC_BASE_URL` | endpoint base URL; pairs with `ANTHROPIC_API_KEY`, or must point at the z.ai host (`api.z.ai`, any URL spelling) when a z.ai key is used | unset → `https://api.z.ai/api/anthropic` |
| `MAILCHAT_LLM_MODEL` | model name | `glm-5` |

A mismatched key/endpoint pairing (for example `ANTHROPIC_API_KEY` without `ANTHROPIC_BASE_URL`) is refused with a clear error instead of sending the key anywhere — the same rules the Settings screen enforces per provider (see [Settings screen](#settings-screen) below).

**Environment fallback overrides.** Settings → AI assistance has an *Environment variables* section that overrides this fallback: values entered there win over the process environment, layered **keyring override → settings override → process env**. The two API-key overrides are write-only fields stored in the OS keyring (entries `llm:env:ZAI_API_KEY` / `llm:env:ANTHROPIC_API_KEY`), never in the settings file; the `ANTHROPIC_BASE_URL` and `MAILCHAT_LLM_MODEL` overrides are non-secret and live in `settings.json` (`env_overrides`). The pairing rules above apply unchanged.

**Test connection.** Every provider profile and the environment fallback has a **Test connection** button that sends exactly one minimal `/v1/messages` request — `max_tokens: 1`, a fixed "ping" message, nothing from the vault — using the values currently typed (falling back to stored ones), and reports the latency or a classified failure: invalid API key (HTTP 401/403), no endpoint at this base URL (404), the provider's error detail (for example an unknown model), or an unreachable host. Plaintext `http` to a non-loopback host is refused before any request is sent. The click itself is the consent for this single request — it is not part of the per-session Ask consent gate above.

## Privacy

Nothing leaves the machine without the explicit AI consent above; LLM mode is opt-in and strictly scoped: only your question (for translation) and the top-8 matching messages, truncated (for the summary), are sent to the configured provider. No other rows, no database dumps, no embeddings, no key at rest. The summary step is skipped entirely when there are no results.

## Fallback

- No API key → plain FTS5 term search with local hybrid ranking when embeddings exist; the UI says so up front.
- LLM error, timeout, or invalid/unusable plan output → plain term search plus a visible note; search results never depend on the LLM succeeding.
- No embeddings built → plain FTS5 ranking; search results never depend on the vector leg either.

## Costs & latency

Measured against `glm-5` at `https://api.z.ai/api/anthropic` (Nov 2026):

- Translate: ~1.5–3 s, roughly 350 input / 100 output tokens.
- Summarize (8 results × ≤500 chars): ~2–5 s, roughly 2,000 input / 200 output tokens.
- One “Ask” ≈ two small requests, a few seconds end-to-end, well under a cent per ask at GLM pricing.

## Local embeddings & hybrid search (v1)

Ask queries are ranked **hybrid**: the FTS5 result list is fused with a **local vector search** over the same archive — entirely offline, no model downloads, no network.

### Embedding model

The bundled embedder (`hash-ngram-v1`, 512 dims) is dependency-free and deterministic: word unigrams, word bigrams, and character 3-grams of each token are FNV-1a-hashed into signed buckets and L2-normalized. Measured on the fixture families of the test suite: related texts score 0.18–0.55, unrelated ones ≤ 0.04, and a scan floor of 0.08 separates the two. Subword overlap gives recall beyond exact FTS tokens — "deploying" still finds "the deployment kept failing".

Heavier options (fastembed / a GGUF mini model) were considered and deliberately deferred: both need model weights that either download at first run (breaking the offline promise) or ship as a multi-megabyte blob in the repo, plus a native ONNX/GGML runtime build. The `Embedder` trait and the model-id-versioned embedding rows keep a future bundled model a drop-in: it ships a new `id`, the incremental job re-embeds, old rows are replaced.

### Vector index — manual cosine in SQLite (the simple choice)

Embeddings live in a `message_embeddings` table (schema migration 3) as **one 512-byte i8-quantized blob per message**, and a query scans them with an integer dot product in Rust (vectors are stored normalized, so the dot product *is* the cosine; i8 keeps the index ≈ 0.5 KB per message). sqlite-vec was considered and rejected: it adds a C extension dependency for what is, at personal-archive scale, a linear scan — 100k messages ≈ 50 MB of vectors, measured at ~75 ms per scan (best of 5; `tests/vector_perf.rs` enforces the budget). Cost scales linearly with embedded messages, which is the right trade for the archive sizes this app targets.

### Hybrid ranking

FTS5 relevance and cosine similarity have incomparable scales, so the two ranked lists are fused with **Reciprocal Rank Fusion**: score(d) = Σ 1/(60 + rank). A row found by both legs outranks a row found by one. The plan's date bounds apply to the vector leg exactly as to the FTS leg. The Ask status line always shows the mode used: `ranked: hybrid FTS + local vectors`, `ranked: FTS only`, or `ranked: local vectors only`.

### Incremental job

**Update local embeddings** (in the Ask section) embeds only messages still lacking an embedding for the current model, in batches: the first run covers the whole archive (~5 s for 100k messages), later runs only new imports (~90 ms for 500 new messages — both budgets enforced by tests). Deleting a message cascades to its embedding; changing the model id makes every row pending again and re-embeds.

### Fallback & privacy

- No embeddings built yet → Ask is plain FTS5 exactly as before; the pipeline never depends on the vector leg.
- No LLM key → the hybrid ranking is still fully local; nothing leaves the machine.
- Embeddings are computed locally from message bodies and are **never sent anywhere** — not even in LLM mode (only the question and the top-8 result bodies go to the provider, as before).

## Settings screen

The **Settings screen** is where LLM providers are configured — one place, no environment variables needed:

- **Providers**: add any number of named provider profiles. A profile has a *kind* that fixes the key/endpoint pairing rules:
  - **z.ai GLM** (the default) — key for `https://api.z.ai/api/anthropic`; the base URL may be left empty (defaults apply, model defaults to `glm-5`) or must point at the z.ai host;
  - **Anthropic** — the key is only ever sent to an explicitly configured endpoint (never the z.ai host), so base URL and model are explicit (prefilled with `https://api.anthropic.com`);
  - **Custom** — any Anthropic-compatible `/v1/messages` endpoint, base URL and model fully explicit.
  A profile that violates its pairing rule is **refused with a clear error before anything is saved** — the same policy the environment configuration enforces.
- **Default provider**: which backend an Ask uses. None selected = automatic (the environment fallback when valid, else local-only mode).
- **Environment fallback**: the *Environment variables* section (see [Configuration](#configuration-settings-ui-or-environment-variables)) sets or overrides the process-env fallback — API keys into the OS keyring, base URL and model into `settings.json` — and carries a **Test connection** button, as does every provider row.
- **Options**: UI theme (system / light / dark), how many top results feed the RAG summary (1–50, default 8), the **Desktop notifications** toggle (on by default — see the [feature reference](features.md#what-the-app-does) for its contract), and the two **background keep-alive** toggles (both off by default): *keep running in the background* (close hides to the tray instead of exiting) and *start automatically at login* — see the M7 bullet in the [feature reference](features.md#what-the-app-does).

### Where values live

- Non-secret settings — provider profiles (kind, base URL, model, name, id), the default provider, the environment-fallback overrides (`env_overrides`: `ANTHROPIC_BASE_URL`, `MAILCHAT_LLM_MODEL`), and options — live in `settings.json` in the app data dir (e.g. `~/.local/share/com.acailic.mailchatvault/settings.json` on Linux). The file is written atomically and versioned; nothing is ever stored in the vault database.
- **API keys never touch the settings file**: they are stored in the OS keyring (service `mailchat-vault`, entry `llm:<provider-id>`, and `llm:env:ZAI_API_KEY` / `llm:env:ANTHROPIC_API_KEY` for the environment-fallback overrides; Secret Service/GNOME Keyring or KWallet on Linux, Keychain on macOS, Credential Manager on Windows). With no keyring reachable, a key is kept only in process memory for the current session and asked for again after a restart — the UI says so.
- **Ask provider menu**: every ask picks its backend — a configured profile (shown with model and host, disabled until it has a key), the environment fallback (labeled *Environment variables*, editable in Settings), or **local full-text**, which sends nothing anywhere. The privacy note under the question always describes exactly what the current selection sends.

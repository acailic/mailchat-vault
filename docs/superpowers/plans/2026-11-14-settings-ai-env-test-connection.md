# Settings AI: env-variable overrides + Test connection — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In Settings → AI assistance, let the user set the environment-variable fallback (`ZAI_API_KEY`, `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`, `MAILCHAT_LLM_MODEL`) from the UI, and add a per-provider plus per-env "Test connection" button that makes one minimal `/v1/messages` call.

**Architecture:** UI-entered env values layer *on top of* the process environment before the existing `LlmConfig::from_lookup` pairing policy, so no pairing/error message changes. Non-secret overrides live in `settings.json` (`EnvOverrides`); the two API-key overrides live in the OS keyring under `env:<VAR>` (reusing `Secrets::save_llm_key`, entries `llm:env:ZAI_API_KEY` / `llm:env:ANTHROPIC_API_KEY`). `LlmRegistry` owns the layering (keyring override → settings override → process env) with the process-env lookup injected for testability. A new `llm_test` IPC command resolves a target's config (form values as typed, falling back to stored values) and calls `LlmConfig::test_connection()` (a `max_tokens: 1` "ping"; nothing from the vault is sent).

**Tech Stack:** Rust (vault-core crate + Tauri 2 shell), vanilla TypeScript + Vite, vitest, cargo test.

## Global Constraints

- UI code never calls Tauri `invoke` directly — every command goes through a typed wrapper in `src/ipc.ts`.
- API keys never appear in `settings.json` or in any IPC *response* — only `has_key`-style booleans cross back.
- Layer order for the environment fallback: **keyring override → settings-file override → process environment** (UI-entered values win). For Test connection, the currently-typed form value wins over everything.
- The pairing policy and error messages in `LlmConfig::from_lookup` (crates/vault-core/src/llm.rs) stay byte-identical — layering happens in the lookup closure, not in the policy.
- `SETTINGS_VERSION` stays `1` (additive `#[serde(default)]` fields; serde ignores unknown fields in old builds).
- `llm_test` is NOT gated by the M5 Ask consent flag (explicit button click, fixed "ping" payload, no vault data), but the `llm_ask`/`run_ask` consent gate is untouched.
- No network calls in unit tests; error classification is tested as pure functions.
- Plaintext-http guard: refuse to send a key over `http://` to a non-loopback host, same rule as provider profiles.
- Repo docs and code comments in English.
- Test commands: `cargo test -p vault-core`, `cargo test -p mailchat-vault` (src-tauri package), `npm test`, `npm run check`. CI on GitHub is broken (billing) — local runs are the verification.

---

### Task 1: vault-core — `EnvOverrides` settings field

**Files:**
- Modify: `crates/vault-core/src/settings.rs` (struct `Settings` ~line 190, `Default` ~204, `normalize` ~305, `validate` ~325, `check_no_plaintext_http` ~250, tests ~546)

**Interfaces:**
- Produces: `pub struct EnvOverrides { pub anthropic_base_url: String, pub llm_model: String }` (serde snake_case field names `anthropic_base_url`, `llm_model`), and `Settings.env_overrides: EnvOverrides` with `#[serde(default)]`. Consumed by Task 3.

- [ ] **Step 1: Write failing tests** (append to `mod tests` in settings.rs)

```rust
#[test]
fn env_overrides_backfill_when_absent_and_round_trip() {
    let dir = std::env::temp_dir().join(format!("mcv-settings-env-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("settings.json");
    // A file written before the field existed loads with empty overrides.
    let older = serde_json::json!({
        "version": SETTINGS_VERSION,
        "providers": [],
        "default_provider": null,
        "options": {"theme": "system", "summarize_top": 8}
    });
    fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
    let loaded = Settings::load(&path).unwrap().unwrap();
    assert_eq!(loaded.env_overrides, EnvOverrides::default());
    // Values round-trip through save/load.
    let mut s = loaded;
    s.env_overrides = EnvOverrides {
        anthropic_base_url: "https://api.anthropic.com".into(),
        llm_model: "claude-sonnet-4-5".into(),
    };
    s.save(&path).unwrap();
    assert_eq!(Settings::load(&path).unwrap().unwrap().env_overrides, s.env_overrides);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn env_overrides_normalize_trims() {
    let mut s = Settings::default();
    s.env_overrides.anthropic_base_url = " https://api.anthropic.com/ ".into();
    s.env_overrides.llm_model = " glm-5 ".into();
    s.normalize();
    assert_eq!(s.env_overrides.anthropic_base_url, "https://api.anthropic.com");
    assert_eq!(s.env_overrides.llm_model, "glm-5");
}

#[test]
fn env_overrides_validate_url_shape_and_transport() {
    // Not a URL: refused.
    let mut s = Settings::default();
    s.env_overrides.anthropic_base_url = "not a url".into();
    let err = s.validate().unwrap_err().to_string();
    assert!(err.contains("absolute http(s) URL"), "got: {err}");
    // Plaintext http off loopback: refused (the key must stay encrypted).
    s.env_overrides.anthropic_base_url = "http://api.anthropic.com".into();
    let err = s.validate().unwrap_err().to_string();
    assert!(err.contains("plaintext http"), "got: {err}");
    // Loopback http stays allowed (local mock endpoints).
    s.env_overrides.anthropic_base_url = "http://127.0.0.1:8080".into();
    s.validate().unwrap();
    // Empty override: nothing to check.
    s.env_overrides.anthropic_base_url = String::new();
    s.validate().unwrap();
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vault-core env_overrides`
Expected: compile error (`EnvOverrides` not found).

- [ ] **Step 3: Implement**

Add after `Options` (near line 109):

```rust
/// Non-secret environment-variable overrides — the Settings "Environment
/// variables" section. Values entered there win over the process
/// environment for the environment fallback; the two API-key overrides
/// live in the OS keyring (`env:ZAI_API_KEY` / `env:ANTHROPIC_API_KEY`),
/// never in this file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EnvOverrides {
    /// `ANTHROPIC_BASE_URL` override; empty = not overridden.
    #[serde(default)]
    pub anthropic_base_url: String,
    /// `MAILCHAT_LLM_MODEL` override; empty = not overridden.
    #[serde(default)]
    pub llm_model: String,
}
```

In `Settings` add the field (after `options`):

```rust
    /// Environment-variable overrides (non-secret; see [`EnvOverrides`]).
    #[serde(default)]
    pub env_overrides: EnvOverrides,
```

and in `Default for Settings` add `env_overrides: EnvOverrides::default(),`.

In `normalize()` append:

```rust
self.env_overrides.anthropic_base_url = self.env_overrides
    .anthropic_base_url
    .trim()
    .trim_end_matches('/')
    .to_string();
self.env_overrides.llm_model = self.env_overrides.llm_model.trim().to_string();
```

Generalize `check_no_plaintext_http` so the label is caller-supplied (its message currently hardcodes `provider “{name}”`): change signature to `fn check_no_plaintext_http(label: &str, base_url: &str)` and the message to `format!("{label}: the API key must not travel over plaintext http — use https for {base_url} (or a localhost/127.0.0.1 endpoint for testing)")`. Update its three existing call sites to pass `&format!("provider “{name}”")` so their messages stay byte-identical.

In `validate()`, after the providers loop (before the `default_provider` check), add:

```rust
if !self.env_overrides.anthropic_base_url.is_empty() {
    if !is_http_url(&self.env_overrides.anthropic_base_url) {
        return Err(settings_error(
            "ANTHROPIC_BASE_URL override must be an absolute http(s) URL".into(),
        ));
    }
    check_no_plaintext_http(
        "ANTHROPIC_BASE_URL override",
        &self.env_overrides.anthropic_base_url,
    )?;
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p vault-core`
Expected: all pass (new + existing; `save_then_load_round_trips` still passes because the new field round-trips too).

- [ ] **Step 5: Commit**

```bash
git add crates/vault-core/src/settings.rs
git commit -m "feat(settings): env_overrides for the environment fallback (non-secret)"
```

---

### Task 2: vault-core — `LlmConfig::test_connection` + classification

**Files:**
- Modify: `crates/vault-core/src/llm.rs` (add to `impl LlmConfig` after `host()`, ~line 152; tests at end of file's test module or a new one)

**Interfaces:**
- Produces: `pub fn test_connection(&self) -> Result<()>` on `LlmConfig`; `pub fn plaintext_http_off_loopback(url: &str) -> bool` (free fn in llm.rs). Consumed by Tasks 3/4.

- [ ] **Step 1: Write failing tests** (in llm.rs tests module; find it via `#[cfg(test)]` at file end — follow its style)

```rust
#[test]
fn classify_status_maps_common_failures() {
    // Invalid key.
    assert!(classify_status(401, "", "api.z.ai").contains("invalid API key (HTTP 401)"));
    assert!(classify_status(403, "{}", "h").contains("invalid API key (HTTP 403)"));
    // Wrong base URL.
    assert!(classify_status(404, "", "api.example").contains("no endpoint at this base URL"));
    // Provider error body (Anthropic-style nested message) is surfaced.
    let body = r#"{"error":{"type":"invalid_request_error","message":"model: nope not found"}}"#;
    assert!(classify_status(400, body, "h").contains("model: nope not found"));
    // Flat message field also honored.
    assert!(classify_status(400, r#"{"message":"bad request detail"}"#, "h").contains("bad request detail"));
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vault-core classify_status`
Expected: compile error (functions not found).

- [ ] **Step 3: Implement**

Make settings.rs's loopback check reusable: change `fn is_loopback_url` in `crates/vault-core/src/settings.rs` to `pub(crate) fn is_loopback_url`. Then in llm.rs add (near `endpoint_host`):

```rust
/// Whether `url` is plaintext `http` pointed at a non-loopback host —
/// the one shape refused before any key is sent (mirrors the
/// settings-file policy for provider profiles).
pub fn plaintext_http_off_loopback(url: &str) -> bool {
    Url::parse(url)
        .map(|u| u.scheme() == "http" && !crate::settings::is_loopback_url(url))
        .unwrap_or(false)
}
```

In `impl LlmConfig` (after `host()`):

```rust
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
```

And the free classification fn (module level, below the `impl`):

```rust
/// Turn a non-2xx test response into a short, actionable message.
fn classify_status(code: u16, body: &str, host: &str) -> String {
    match code {
        401 | 403 => "invalid API key (HTTP {code})".replace("{code}", &code.to_string()),
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
```

(Simplify the 401/403 arm to a plain `format!` — the `.replace` form above is only to avoid confusion: write `format!("invalid API key (HTTP {code})")`.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p vault-core`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/vault-core/src/llm.rs crates/vault-core/src/settings.rs
git commit -m "feat(llm): minimal test_connection round trip with classified errors"
```

---

### Task 3: src-tauri — registry env layer, views/payloads, `test_config`

**Files:**
- Modify: `src-tauri/src/registry.rs` (whole file rework of `LlmRegistry` + IPC types + tests)

**Interfaces:**
- Consumes: `vault_core::settings::EnvOverrides` (Task 1), `LlmConfig::from_lookup`, `vault_core::llm::plaintext_http_off_loopback` (Task 2).
- Produces (used by Tasks 4/5):
  - `pub const ENV_ZAI_KEY_ID: &str = "env:ZAI_API_KEY";` and `pub const ENV_ANTHROPIC_KEY_ID: &str = "env:ANTHROPIC_API_KEY";`
  - `LlmRegistry::load(settings: vault_core::Result<Option<Settings>>, process_env: Box<dyn Fn(&str) -> Option<String> + Send + Sync>, load_key: impl Fn(&str) -> Option<String>) -> Self` (replaces the old `env` parameter — env is now computed internally)
  - `pub fn test_config(&self, payload: &LlmTestPayload) -> Result<LlmConfig, String>`
  - `pub struct EnvOverridesView { pub base_url: String, pub model: String, pub zai_key: bool, pub anthropic_key: bool }` (Serialize), field `SettingsView.env_overrides`
  - `SaveSettingsPayload` += `#[serde(default)] env_overrides: EnvOverridesPayload` (`{ base_url, model }`) and `#[serde(default)] env_api_keys: Vec<ApiKeyUpdate>` (id ∈ {`ZAI_API_KEY`, `ANTHROPIC_API_KEY`})
  - `pub struct LlmTestPayload { pub provider: String, #[serde(default)] pub base_url: String, #[serde(default)] pub model: String, #[serde(default)] pub key: String, #[serde(default)] pub zai_key: String, #[serde(default)] pub anthropic_key: String }` (Deserialize)

- [ ] **Step 1: Update the test helper and write failing tests.** Adapt the existing `registry()` helper (env now injected via the process-env closure, not a precomputed config):

```rust
fn registry(
    settings: Settings,
    keys: &[(&str, &str)],
    env_vars: &[(&str, &str)],
) -> LlmRegistry {
    let key_map: HashMap<String, String> = keys
        .iter()
        .map(|(id, k)| (id.to_string(), k.to_string()))
        .collect();
    let env_map: HashMap<String, String> = env_vars
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
    LlmRegistry::load(
        Ok(Some(settings)),
        Box::new(move |n| env_map.get(n).cloned()),
        |id| key_map.get(id).cloned(),
    )
}
```

Replace every existing `registry(x, keys, Some(env_config()))` call with explicit vars — `env_config()` was `{base_url:"http://127.0.0.1:1/api", api_key:"env-key", model:"env-model"}`, i.e. `&[("ANTHROPIC_API_KEY", "env-key"), ("ANTHROPIC_BASE_URL", "http://127.0.0.1:1/api"), ("MAILCHAT_LLM_MODEL", "env-model")]`; `None` becomes `&[]`. `LlmRegistry::load(Err(..), Err(..), ..)`-style tests (e.g. `load_reports_env_and_settings_errors`) now pass a process_env closure returning `None` and produce the env error via vars that violate pairing, e.g. `&[("ANTHROPIC_API_KEY", "k")]` (key without base URL) — assert the same message text as before.

New failing tests:

```rust
#[test]
fn env_overrides_layer_over_the_process_environment() {
    let mut settings = Settings::default();
    settings.env_overrides = vault_core::settings::EnvOverrides {
        anthropic_base_url: "http://127.0.0.1:2/api".into(),
        llm_model: "override-model".into(),
    };
    // Keyring override beats the process env; process env fills the rest.
    let r = registry(
        settings,
        &[("env:ZAI_API_KEY", "stored-zai")],
        &[("ZAI_API_KEY", "process-zai"), ("MAILCHAT_LLM_MODEL", "process-model")],
    );
    match r.select(Some(ENV_ID)) {
        Selection::Provider { config, .. } => {
            assert_eq!(config.api_key, "stored-zai");
            assert_eq!(config.base_url, "http://127.0.0.1:2/api");
            assert_eq!(config.model, "override-model");
        }
        other => panic!("env should resolve: {other:?}"),
    }
    // Without the keyring key the process env is used verbatim.
    let mut settings = Settings::default();
    settings.env_overrides.llm_model = "override-model".into();
    let r = registry(settings, &[], &[("ZAI_API_KEY", "process-zai")]);
    match r.select(Some(ENV_ID)) {
        Selection::Provider { config, .. } => {
            assert_eq!(config.api_key, "process-zai");
            assert_eq!(config.base_url, LlmConfig::DEFAULT_BASE_URL);
            assert_eq!(config.model, "override-model");
        }
        other => panic!("env should resolve: {other:?}"),
    }
}

#[test]
fn env_pairing_error_surfaces_in_view() {
    let mut settings = Settings::default();
    settings.env_overrides.anthropic_base_url =
        "https://api.z.ai/api/anthropic".into(); // z.ai host + Anthropic key
    let r = registry(settings, &[("env:ANTHROPIC_API_KEY", "k")], &[]);
    let view = SettingsView::from(&r);
    assert!(view.env.is_none());
    assert!(view.env_error.as_deref().unwrap().contains("ANTHROPIC_API_KEY"), "got: {:?}", view.env_error);
    match r.select(Some(ENV_ID)) {
        Selection::Local { reason } => assert!(reason.contains("not configured")),
        other => panic!("env must degrade: {other:?}"),
    }
}

#[test]
fn view_carries_env_overrides_and_flags_but_no_keys() {
    let mut settings = Settings::default();
    settings.env_overrides = vault_core::settings::EnvOverrides {
        anthropic_base_url: "https://api.anthropic.com".into(),
        llm_model: "m".into(),
    };
    let r = registry(
        settings,
        &[("env:ZAI_API_KEY", "secret-zai"), ("env:ANTHROPIC_API_KEY", "secret-anthropic")],
        &[],
    );
    let view = SettingsView::from(&r);
    assert_eq!(view.env_overrides.base_url, "https://api.anthropic.com");
    assert_eq!(view.env_overrides.model, "m");
    assert!(view.env_overrides.zai_key);
    assert!(view.env_overrides.anthropic_key);
    let json = serde_json::to_string(&view).unwrap();
    assert!(!json.contains("secret-zai"));
    assert!(!json.contains("secret-anthropic"));
}

#[test]
fn old_payloads_without_env_fields_parse() {
    let payload: SaveSettingsPayload = serde_json::from_str(
        r#"{"providers":[],"default_provider":null,"theme":"system","summarize_top":8,"api_keys":[]}"#,
    )
    .unwrap();
    assert_eq!(payload.env_overrides, EnvOverridesPayload::default());
    assert!(payload.env_api_keys.is_empty());
}

#[test]
fn test_config_env_target_layers_form_values_over_stored() {
    let mut settings = Settings::default();
    settings.env_overrides.llm_model = "stored-model".into();
    let r = registry(settings, &[("env:ZAI_API_KEY", "stored-zai")], &[]);
    // Typed form key + model win; URL falls back to the default.
    let payload = LlmTestPayload {
        provider: ENV_ID.into(),
        base_url: String::new(),
        model: "typed-model".into(),
        key: String::new(),
        zai_key: "typed-zai".into(),
        anthropic_key: String::new(),
    };
    let config = r.test_config(&payload).unwrap();
    assert_eq!((config.api_key.as_str(), config.model.as_str(), config.base_url.as_str()),
        ("typed-zai", "typed-model", LlmConfig::DEFAULT_BASE_URL));
    // Empty form fields fall back to stored override + keyring.
    let payload = LlmTestPayload::defaulting(ENV_ID);
    let config = r.test_config(&payload).unwrap();
    assert_eq!((config.api_key.as_str(), config.model.as_str()), ("stored-zai", "stored-model"));
}

#[test]
fn test_config_env_target_errors_are_actionable() {
    // Nothing configured at all.
    let r = registry(Settings::default(), &[], &[]);
    let err = r.test_config(&LlmTestPayload::defaulting(ENV_ID)).unwrap_err();
    assert!(err.contains("no API key"), "got: {err}");
    // Pairing violation: typed Anthropic key + z.ai base URL.
    let payload = LlmTestPayload {
        provider: ENV_ID.into(),
        base_url: "https://api.z.ai/api/anthropic".into(),
        model: String::new(),
        key: String::new(),
        zai_key: String::new(),
        anthropic_key: "k".into(),
    };
    let err = r.test_config(&payload).unwrap_err();
    assert!(err.contains("refusing to send an Anthropic key"), "got: {err}");
    // Plaintext http off loopback: refused before any network.
    let payload = LlmTestPayload {
        provider: ENV_ID.into(),
        base_url: "http://api.example.com".into(),
        model: String::new(),
        key: String::new(),
        zai_key: "k".into(),
        anthropic_key: String::new(),
    };
    let err = r.test_config(&payload).unwrap_err();
    assert!(err.contains("plaintext http"), "got: {err}");
}

#[test]
fn test_config_profile_target_uses_form_or_stored_key() {
    let settings = settings_with(vec![profile("glm", ProviderKind::Zai)], None);
    let r = registry(settings.clone(), &[("glm", "stored")], &[]);
    let payload = LlmTestPayload {
        provider: "glm".into(),
        base_url: String::new(),
        model: "typed-model".into(),
        key: String::new(),
        zai_key: String::new(),
        anthropic_key: String::new(),
    };
    let config = r.test_config(&payload).unwrap();
    assert_eq!((config.api_key.as_str(), config.model.as_str()), ("stored", "typed-model"));
    // Typed key works without a stored one (unsaved row).
    let r = registry(settings, &[], &[]);
    let payload = LlmTestPayload { key: "typed".into(), ..LlmTestPayload::defaulting("glm") };
    assert_eq!(r.test_config(&payload).unwrap().api_key, "typed");
    // No key anywhere: actionable error.
    let err = r.test_config(&LlmTestPayload::defaulting("glm")).unwrap_err();
    assert!(err.contains("no API key"), "got: {err}");
    // Unknown provider.
    let err = r.test_config(&LlmTestPayload::defaulting("nope")).unwrap_err();
    assert!(err.contains("unknown provider"), "got: {err}");
    // Plaintext http guard applies to profiles too.
    let payload = LlmTestPayload {
        provider: "glm".into(),
        base_url: "http://api.example.com".into(),
        key: "k".into(),
        ..LlmTestPayload::defaulting("glm")
    };
    assert!(r.test_config(&payload).unwrap_err().contains("plaintext http"));
}

#[test]
fn drop_key_handles_env_key_ids() {
    let r = registry(Settings::default(), &[("env:ZAI_API_KEY", "k")], &[]);
    assert!(SettingsView::from(&r).env_overrides.zai_key);
    let mut r = r;
    r.drop_key(ENV_ZAI_KEY_ID);
    assert!(!SettingsView::from(&r).env_overrides.zai_key);
    assert!(r.select(Some(ENV_ID)).matches_local_with_reason());
}
```

Add this tiny test helper next to the tests (and use it above; plain `match` also acceptable if simpler):

```rust
trait SelectionExt {
    fn matches_local_with_reason(&self) -> bool;
}
impl SelectionExt for Selection {
    fn matches_local_with_reason(&self) -> bool {
        matches!(self, Selection::Local { .. })
    }
}
```

`LlmTestPayload` needs a convenience constructor for tests (put it on the struct):

```rust
impl LlmTestPayload {
    /// Payload targeting `provider` with every form field empty.
    pub fn defaulting(provider: &str) -> Self {
        Self {
            provider: provider.into(),
            ..Self::default()
        }
    }
}
```

(derive `Default` on `LlmTestPayload`.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p mailchat-vault registry`
Expected: compile errors (`ENV_ZAI_KEY_ID`, `env_overrides`, new `load` signature missing).

- [ ] **Step 3: Implement.** All in `src-tauri/src/registry.rs`:

Imports: add `vault_core::llm::plaintext_http_off_loopback` and `vault_core::settings::EnvOverrides`.

New consts near `ENV_NAME`:

```rust
/// Keyring ids for the environment-variable key overrides (stored with
/// `Secrets::save_llm_key`, so the entries are `llm:env:ZAI_API_KEY` and
/// `llm:env:ANTHROPIC_API_KEY`). Shared with the Settings UI (clear-key).
pub const ENV_ZAI_KEY_ID: &str = "env:ZAI_API_KEY";
pub const ENV_ANTHROPIC_KEY_ID: &str = "env:ANTHROPIC_API_KEY";

/// The two keyring override ids, for reload loops.
fn env_key_ids() -> [&'static str; 2] {
    [ENV_ZAI_KEY_ID, ENV_ANTHROPIC_KEY_ID]
}
```

Struct changes — replace the `env: Option<LlmConfig>` input plumbing with internal layering:

```rust
pub struct LlmRegistry {
    settings: Settings,
    /// Profile id → API key (from the keyring / session memory).
    keys: HashMap<String, String>,
    /// Keyring override values for the environment fallback
    /// (`env:ZAI_API_KEY` / `env:ANTHROPIC_API_KEY`).
    env_keys: HashMap<String, String>,
    /// Process-environment lookup (injected so tests stay deterministic).
    process_env: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    /// The layered environment fallback, if validly configured.
    env: Option<LlmConfig>,
    /// Why the environment configuration was refused, if it was.
    env_error: Option<String>,
    /// Why the settings file could not be loaded, if it could not.
    settings_error: Option<String>,
    /// Where API keys were last saved (for the Settings UI note).
    key_store: Option<SecretStore>,
}
```

New `load` (same settings handling as today, env computed at the end):

```rust
pub fn load(
    settings: vault_core::Result<Option<Settings>>,
    process_env: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
    load_key: impl Fn(&str) -> Option<String>,
) -> Self {
    let (settings, settings_error) = match settings {
        Ok(settings) => {
            let mut settings = settings.unwrap_or_default();
            settings.normalize();
            match settings.validate() {
                Ok(()) => (settings, None),
                Err(e) => (Settings::default(), Some(e.to_string())),
            }
        }
        Err(e) => (Settings::default(), Some(e.to_string())),
    };
    let mut registry = Self {
        settings,
        keys: HashMap::new(),
        env_keys: HashMap::new(),
        process_env,
        env: None,
        env_error: None,
        settings_error,
        key_store: None,
    };
    registry.reload_keys(load_key);
    registry.recompute_env();
    registry
}

/// The layered environment lookup: keyring override → settings-file
/// override → process environment. UI-entered values win.
fn env_lookup(&self, name: &str) -> Option<String> {
    let overridden = match name {
        "ZAI_API_KEY" => self.env_keys.get(ENV_ZAI_KEY_ID).cloned(),
        "ANTHROPIC_API_KEY" => self.env_keys.get(ENV_ANTHROPIC_KEY_ID).cloned(),
        "ANTHROPIC_BASE_URL" => {
            let url = self.settings.env_overrides.anthropic_base_url.trim();
            (!url.is_empty()).then(|| url.to_string())
        }
        "MAILCHAT_LLM_MODEL" => {
            let model = self.settings.env_overrides.llm_model.trim();
            (!model.is_empty()).then(|| model.to_string())
        }
        _ => None,
    };
    overridden.or_else(|| (self.process_env)(name))
}

/// Recompute the environment fallback through the unchanged pairing
/// policy (`LlmConfig::from_lookup`).
fn recompute_env(&mut self) {
    let layered = LlmConfig::from_lookup(|name| self.env_lookup(name));
    let (env, env_error) = match layered {
        Ok(env) => (env, None),
        Err(e) => (None, Some(e.to_string())),
    };
    self.env = env;
    self.env_error = env_error;
}
```

`reload_keys` gains the env ids; `update` recomputes env; `drop_key` covers both maps:

```rust
pub fn reload_keys(&mut self, load_key: impl Fn(&str) -> Option<String>) {
    self.keys = self
        .settings
        .providers
        .iter()
        .filter_map(|p| load_key(&p.id).map(|k| (p.id.clone(), k)))
        .collect();
    self.env_keys = env_key_ids()
        .into_iter()
        .filter_map(|id| load_key(id).map(|k| (id.to_string(), k)))
        .collect();
}

pub fn update(
    &mut self,
    settings: Settings,
    key_store: Option<SecretStore>,
    load_key: impl Fn(&str) -> Option<String>,
) {
    self.settings = settings;
    self.settings_error = None;
    self.key_store = key_store;
    self.reload_keys(load_key);
    self.recompute_env();
}

pub fn drop_key(&mut self, provider_id: &str) {
    self.keys.remove(provider_id);
    self.env_keys.remove(provider_id);
    self.recompute_env();
}
```

IPC types — add `EnvOverridesView`, extend `SettingsView`, `SaveSettingsPayload`, add `EnvOverridesPayload`, `LlmTestPayload`:

```rust
/// The environment-variable overrides as the Settings UI sees them:
/// non-secret values plus key-presence flags (never the keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct EnvOverridesView {
    /// ANTHROPIC_BASE_URL override value ("" = not overridden).
    pub base_url: String,
    /// MAILCHAT_LLM_MODEL override value ("" = not overridden).
    pub model: String,
    /// Whether a ZAI_API_KEY override is stored.
    pub zai_key: bool,
    /// Whether an ANTHROPIC_API_KEY override is stored.
    pub anthropic_key: bool,
}
```

`SettingsView` gains `pub env_overrides: EnvOverridesView,` (after `env`), filled in `From<&LlmRegistry>`:

```rust
env_overrides: EnvOverridesView {
    base_url: settings.env_overrides.anthropic_base_url.clone(),
    model: settings.env_overrides.llm_model.clone(),
    zai_key: registry.env_keys.contains_key(ENV_ZAI_KEY_ID),
    anthropic_key: registry.env_keys.contains_key(ENV_ANTHROPIC_KEY_ID),
},
```

```rust
/// Non-secret env overrides inside [`SaveSettingsPayload`]. Empty values
/// mean "no override".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct EnvOverridesPayload {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
}
```

`SaveSettingsPayload` gains (after `api_keys`):

```rust
/// Environment-variable overrides (ANTHROPIC_BASE_URL / MAILCHAT_LLM_MODEL).
    #[serde(default)]
    pub env_overrides: EnvOverridesPayload,
    /// API-key overrides for the environment fallback. `id` is the
    /// variable name (`ZAI_API_KEY` / `ANTHROPIC_API_KEY`); non-empty key
    /// stores, empty clears. Ids not mentioned keep their stored key.
    #[serde(default)]
    pub env_api_keys: Vec<ApiKeyUpdate>,
```

```rust
/// One "Test connection" request: the target (`env` or a profile id)
/// plus the form values as currently typed, so an unsaved edit can be
/// tested as typed. Empty fields fall back to the stored/effective
/// values; empty key fields fall back to the stored key.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct LlmTestPayload {
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub zai_key: String,
    #[serde(default)]
    pub anthropic_key: String,
}
```

`test_config` on `LlmRegistry` (after `select_default`):

```rust
/// Build the exact [`LlmConfig`] a "Test connection" click should probe:
/// the target's stored/effective values with the currently-typed form
/// values layered on top. Fails with a user-facing message when there is
/// nothing to test or the configuration refuses itself (pairing,
/// plaintext http).
pub fn test_config(&self, payload: &LlmTestPayload) -> std::result::Result<LlmConfig, String> {
    let trim = |v: &str| v.trim().to_string();
    let config = if payload.provider == ENV_ID {
        let zai = trim(&payload.zai_key);
        let anthropic = trim(&payload.anthropic_key);
        let base_url = trim(&payload.base_url);
        let model = trim(&payload.model);
        let lookup = |name: &str| -> Option<String> {
            match name {
                "ZAI_API_KEY" => (!zai.is_empty())
                    .then(|| zai.clone())
                    .or_else(|| self.env_keys.get(ENV_ZAI_KEY_ID).cloned()),
                "ANTHROPIC_API_KEY" => (!anthropic.is_empty())
                    .then(|| anthropic.clone())
                    .or_else(|| self.env_keys.get(ENV_ANTHROPIC_KEY_ID).cloned()),
                "ANTHROPIC_BASE_URL" => (!base_url.is_empty())
                    .then(|| base_url.clone())
                    .or_else(|| self.env_lookup(name)),
                "MAILCHAT_LLM_MODEL" => (!model.is_empty())
                    .then(|| model.clone())
                    .or_else(|| self.env_lookup(name)),
                _ => self.env_lookup(name),
            }
        };
        match LlmConfig::from_lookup(lookup) {
            Ok(Some(config)) => config,
            Ok(None) => {
                return Err("no API key set for the environment fallback — type one to test".into())
            }
            Err(e) => return Err(e.to_string()),
        }
    } else {
        let profile = self
            .settings
            .providers
            .iter()
            .find(|p| p.id == payload.provider)
            .ok_or_else(|| format!("unknown provider “{}”", payload.provider))?;
        let mut config = Self::config_for(profile);
        if !trim(&payload.base_url).is_empty() {
            config.base_url = trim(&payload.base_url);
        }
        if !trim(&payload.model).is_empty() {
            config.model = trim(&payload.model);
        }
        config.api_key = {
            let typed = trim(&payload.key);
            if typed.is_empty() {
                self.keys
                    .get(&profile.id)
                    .cloned()
                    .ok_or("no API key stored — type one to test")?
            } else {
                typed
            }
        };
        config
    };
    if plaintext_http_off_loopback(&config.base_url) {
        return Err(format!(
            "refusing to send the API key over plaintext http — use https for {} (or a localhost endpoint for testing)",
            config.base_url
        ));
    }
    Ok(config)
}
```

- [ ] **Step 4: Fix compile fallout in `src-tauri/src/lib.rs` only as far as it compiles** (setup currently calls the old `load` with an env result — temporarily keep it compiling by adapting the call site minimally; Task 4 does the real wiring). If a temporary shim is needed, make it obviously temporary:

```rust
let registry = LlmRegistry::load(
    settings,
    Box::new(|name| std::env::var(name).ok()),
    |id| secrets.load_llm_key(id),
);
```

and delete the now-unused `let env = LlmConfig::from_env();` block (plus unused import if it warns).

Run: `cargo test -p mailchat-vault`
Expected: all pass (new + adapted existing).

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/registry.rs src-tauri/src/lib.rs
git commit -m "feat(registry): env-override layer over process env + test_config resolution"
```

---

### Task 4: src-tauri — `llm_test` command + save-flow env keys

**Files:**
- Modify: `src-tauri/src/lib.rs` (`settings_save` ~1177, new command near `llm_status` ~1147, handler list ~3322)

**Interfaces:**
- Consumes: `registry::{LlmTestPayload, test_config}`, `LlmConfig::test_connection`, `SaveSettingsPayload.env_overrides`/`env_api_keys`, `vault_core::settings::EnvOverrides`.
- Produces: IPC command `llm_test(payload) -> LlmTestResult { ok: bool, latency_ms: u64, detail: String }` (consumed by Task 5).

- [ ] **Step 1: Write a failing test** for the result shape (pure part): if `lib.rs` has a tests module, add a minimal serialization test; if `LlmTestResult` serializes as expected. If there is no natural pure seam, this task's test cycle is: `cargo test -p mailchat-vault` must stay green and `cargo check` must pass with the new command — state that explicitly in the report. Add the struct test where it fits:

```rust
#[test]
fn llm_test_result_serializes_flat() {
    let result = LlmTestResult { ok: false, latency_ms: 12, detail: "invalid API key (HTTP 401)".into() };
    let json = serde_json::to_string(&result).unwrap();
    assert!(json.contains(r#""ok":false"#));
    assert!(json.contains(r#""latency_ms":12"#));
    assert!(json.contains("invalid API key"));
}
```

- [ ] **Step 2: Implement.** In `lib.rs`:

Imports: extend the `use crate::registry::{...}` list with `LlmTestPayload` (and whatever is missing).

Add near `llm_status`:

```rust
/// `LlmTestResult` — the outcome of one "Test connection" click.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LlmTestResult {
    pub ok: bool,
    pub latency_ms: u64,
    /// What happened: "{host} replied on model {model}" or the
    /// classified failure message.
    pub detail: String,
}

/// One "Test connection" round trip against a provider profile or the
/// environment fallback (`env`). The explicit button click is the
/// consent: the request is a fixed "ping" with `max_tokens: 1` and sends
/// nothing from the vault, so it stands outside the Ask consent gate.
#[tauri::command]
fn llm_test(state: State<AppState>, payload: LlmTestPayload) -> Result<LlmTestResult, String> {
    // Resolve the config without holding the registry lock across the
    // network round trip (same pattern as llm_ask).
    let config = {
        let registry = state
            .registry
            .lock()
            .map_err(|e| format!("registry lock poisoned: {e}"))?;
        registry.test_config(&payload)?
    };
    let start = std::time::Instant::now();
    let outcome = config.test_connection();
    let latency_ms = start.elapsed().as_millis() as u64;
    let detail = match &outcome {
        Ok(()) => format!("{} replied on model {}", config.host(), config.model),
        Err(e) => e.to_string(),
    };
    Ok(LlmTestResult { ok: outcome.is_ok(), latency_ms, detail })
}
```

`settings_save`: in the `Settings { ... }` construction add:

```rust
env_overrides: vault_core::settings::EnvOverrides {
    anthropic_base_url: payload.env_overrides.base_url.clone(),
    llm_model: payload.env_overrides.model.clone(),
},
```

After the provider-id validation loop (before `settings.save(...)`), validate env key ids:

```rust
const ENV_KEY_VARS: [&str; 2] = ["ZAI_API_KEY", "ANTHROPIC_API_KEY"];
for update in &payload.env_api_keys {
    let var = update.id.trim();
    if !ENV_KEY_VARS.contains(&var) {
        return Err(format!("env API key update for unknown variable “{var}”"));
    }
}
```

After the provider-key application loop (keys only change once the file is written), apply env keys:

```rust
for update in &payload.env_api_keys {
    let id = format!("env:{}", update.id.trim());
    let key = update.key.trim();
    if key.is_empty() {
        secrets.clear_llm_key(&id);
    } else {
        key_store = Some(secrets.save_llm_key(&id, key));
    }
}
```

Register the command in `tauri::generate_handler![...]` (after `llm_key_clear`): `llm_test,`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p mailchat-vault && cargo test -p vault-core`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/lib.rs
git commit -m "feat(ipc): llm_test command + env-override key persistence in settings_save"
```

---

### Task 5: Frontend — Settings UI (env section + Test buttons)

**Files:**
- Modify: `src/settings.ts` (types + consts + helpers), `src/ipc.ts` (`llmTest`), `index.html` (`#settings-ai` ~531), `src/main.ts` (element refs ~279, `providerRow` ~836, `renderSettings` ~899, `collectSettings` ~948, new handlers), `src/app.css` (`.provider-row` status line), `src/settings.test.ts`

**Interfaces:**
- Consumes: IPC `llm_test` (Task 4), `SettingsView.env_overrides` / `SaveSettingsPayload.env_overrides`+`env_api_keys` (Task 3), `llm_key_clear` with ids `env:ZAI_API_KEY` / `env:ANTHROPIC_API_KEY`.
- Produces: `llmTest(payload: LlmTestPayload): Promise<LlmTestResult>` in ipc.ts; `ENV_ZAI_KEY_ID`/`ENV_ANTHROPIC_KEY_ID` consts + `envKeyPlaceholder`/`envTestPayload` helpers in settings.ts.

- [ ] **Step 1: Write failing tests** in `src/settings.test.ts` (follow the file's existing import/style):

```ts
describe("env overrides", () => {
  it("key placeholders mark stored overrides", () => {
    expect(envKeyPlaceholder(false)).toBe("API key");
    expect(envKeyPlaceholder(true)).toBe("stored — type to replace");
  });

  it("envTestPayload targets env with trimmed form values", () => {
    expect(
      envTestPayload({
        base_url: " https://api.anthropic.com ",
        model: " claude-x ",
        zai_key: " k-zai ",
        anthropic_key: "",
      }),
    ).toEqual({
      provider: "env",
      base_url: "https://api.anthropic.com",
      model: "claude-x",
      key: "",
      zai_key: "k-zai",
      anthropic_key: "",
    });
  });

  it("keyring override ids mirror the backend", () => {
    expect(ENV_ZAI_KEY_ID).toBe("env:ZAI_API_KEY");
    expect(ENV_ANTHROPIC_KEY_ID).toBe("env:ANTHROPIC_API_KEY");
  });
});
```

- [ ] **Step 2: Run to verify failure**

Run: `npm test -- settings`
Expected: FAIL (imports missing).

- [ ] **Step 3: Implement `src/settings.ts` additions** (mirroring backend types):

```ts
/** Mirrors `src-tauri::registry::EnvOverridesView` — the Settings env
 * section: non-secret values plus key-presence flags (never the keys). */
export interface EnvOverridesView {
  base_url: string;
  model: string;
  zai_key: boolean;
  anthropic_key: boolean;
}

/** Non-secret env overrides inside `settings_save`. */
export interface EnvOverridesPayload {
  base_url: string;
  model: string;
}

/** One "Test connection" request (mirrors `LlmTestPayload`): form values
 * as typed; empty fields fall back to stored/effective values. */
export interface LlmTestPayload {
  provider: string;
  base_url: string;
  model: string;
  key: string;
  zai_key: string;
  anthropic_key: string;
}

/** Mirrors `LlmTestResult`. */
export interface LlmTestResult {
  ok: boolean;
  latency_ms: number;
  detail: string;
}

/** Keyring override ids (backend `registry::ENV_*_KEY_ID`). */
export const ENV_ZAI_KEY_ID = "env:ZAI_API_KEY";
export const ENV_ANTHROPIC_KEY_ID = "env:ANTHROPIC_API_KEY";

/** Placeholder for the write-only env key inputs. */
export function envKeyPlaceholder(stored: boolean): string {
  return stored ? "stored — type to replace" : "API key";
}

/** Payload for the env-fallback Test button (form values as typed). */
export function envTestPayload(input: {
  base_url: string;
  model: string;
  zai_key: string;
  anthropic_key: string;
}): LlmTestPayload {
  return {
    provider: ENV_ID,
    base_url: input.base_url.trim(),
    model: input.model.trim(),
    key: "",
    zai_key: input.zai_key.trim(),
    anthropic_key: input.anthropic_key.trim(),
  };
}
```

Extend `SettingsView` with `env_overrides: EnvOverridesView;` (after `env`) and `SaveSettingsPayload` with:

```ts
  env_overrides: EnvOverridesPayload;
  /** Env key overrides: id is the variable name; non-empty = store, empty = clear. */
  env_api_keys: Array<{ id: "ZAI_API_KEY" | "ANTHROPIC_API_KEY"; key: string }>;
```

- [ ] **Step 4: `src/ipc.ts`** — add after `llmKeyClear` (import the new types the way ipc.ts imports the others):

```ts
export function llmTest(payload: LlmTestPayload): Promise<LlmTestResult> {
  return invoke<LlmTestResult>("llm_test", { payload });
}
```

- [ ] **Step 5: `index.html`** — inside `#settings-ai`, between `<div id="env-info">` and `<div id="provider-rows">`:

```html
<!-- Environment fallback overrides: the env vars the backend reads,
     editable here. Values set here override the process environment;
     API keys go to the OS keyring exactly like provider keys. -->
<div id="env-overrides">
  <div class="row">
    <label class="meta" title="z.ai GLM API key; overrides ZAI_API_KEY/GLM_API_KEY from the environment">
      ZAI_API_KEY
      <input id="env-zai-key" type="password" autocomplete="off" placeholder="API key" />
    </label>
    <label class="meta" title="Anthropic API key; overrides ANTHROPIC_API_KEY. Only ever sent to the base URL below — never to z.ai">
      ANTHROPIC_API_KEY
      <input id="env-anthropic-key" type="password" autocomplete="off" placeholder="API key" />
    </label>
  </div>
  <div class="row">
    <label class="meta" title="Overrides ANTHROPIC_BASE_URL — the endpoint that accepts the Anthropic key (required with an Anthropic key)">
      ANTHROPIC_BASE_URL
      <input id="env-base-url" type="text" placeholder="https://api.anthropic.com" />
    </label>
    <label class="meta" title="Overrides MAILCHAT_LLM_MODEL — model for the environment fallback">
      MAILCHAT_LLM_MODEL
      <input id="env-model" type="text" placeholder="glm-5 (default)" />
    </label>
  </div>
  <div class="row">
    <button id="env-test-btn" type="button" title="Send one minimal request (max_tokens=1, no vault data) to check the key, endpoint, and model">Test connection</button>
    <span id="env-test-status" class="meta" style="flex: 1; min-height: 1em" role="status"></span>
    <button id="env-clear-keys-btn" type="button" class="small-btn" hidden>Clear keys</button>
  </div>
  <p class="meta" style="margin: 4px 0 0">
    Values set here override the process environment for the environment fallback
    (used when no default provider is set); keys are stored in the OS keyring, never
    in the settings file.
  </p>
</div>
```

- [ ] **Step 6: `src/main.ts`.** Element refs near the existing settings ones (~line 279):

```ts
const envZaiKey = document.querySelector<HTMLInputElement>("#env-zai-key")!;
const envAnthropicKey = document.querySelector<HTMLInputElement>("#env-anthropic-key")!;
const envBaseUrl = document.querySelector<HTMLInputElement>("#env-base-url")!;
const envModel = document.querySelector<HTMLInputElement>("#env-model")!;
const envTestButton = document.querySelector<HTMLButtonElement>("#env-test-btn")!;
const envTestStatus = document.querySelector<HTMLElement>("#env-test-status")!;
const envClearKeysButton = document.querySelector<HTMLButtonElement>("#env-clear-keys-btn")!;
```

`providerRow()`: add a Test button and a per-row status line (append after `remove`; keep the existing append order otherwise):

```ts
const test = document.createElement("button");
test.type = "button";
test.className = "small-btn test-btn";
test.textContent = "Test";
test.title = "Send one minimal request (max_tokens=1, no vault data) to check this provider";

const testStatus = document.createElement("span");
testStatus.className = "meta prov-test-status";
testStatus.setAttribute("role", "status");
```

`renderSettings()`: after the toggles, fill the env section and update the `env-info` copy (drop "read-only"):

```ts
const env = view.env_overrides;
envBaseUrl.value = env.base_url;
envModel.value = env.model;
envZaiKey.value = "";
envZaiKey.placeholder = envKeyPlaceholder(env.zai_key);
envAnthropicKey.value = "";
envAnthropicKey.placeholder = envKeyPlaceholder(env.anthropic_key);
envClearKeysButton.hidden = !env.zai_key && !env.anthropic_key;
envTestStatus.textContent = "";
envTestStatus.classList.remove("error");
```

and in the `envLines` block replace the first pushed line with:

```ts
envLines.push(
  `Environment fallback: ${view.env.model} via ${view.env.host} (ZAI_API_KEY/GLM_API_KEY or ANTHROPIC_API_KEY + ANTHROPIC_BASE_URL; values set below override the process environment; used when no default provider is set).`,
);
```

`collectSettings()`: add to the returned object and collect env keys:

```ts
const env_api_keys = (
  [
    { id: "ZAI_API_KEY" as const, key: envZaiKey.value.trim() },
    { id: "ANTHROPIC_API_KEY" as const, key: envAnthropicKey.value.trim() },
  ] as const
).filter((u) => u.key !== "");
// ...in the returned payload:
env_overrides: { base_url: envBaseUrl.value.trim(), model: envModel.value.trim() },
env_api_keys: [...env_api_keys],
```

New handlers (near `clearProviderKey`):

```ts
/** Run one Test-connection round trip and render the outcome. */
async function runLlmTest(
  payload: LlmTestPayload,
  button: HTMLButtonElement,
  status: HTMLElement,
): Promise<void> {
  button.disabled = true;
  status.classList.remove("error");
  status.textContent = "Testing…";
  try {
    const result = await ipc.llmTest(payload);
    status.textContent = result.ok
      ? `OK — ${result.latency_ms} ms · ${result.detail}`
      : `Failed: ${result.detail}`;
    status.classList.toggle("error", !result.ok);
  } catch (e) {
    status.textContent = `Failed: ${e}`;
    status.classList.add("error");
  } finally {
    button.disabled = false;
  }
}

function testPayloadForRow(row: HTMLElement): LlmTestPayload {
  const inputs = rowInputs(row);
  return {
    provider: row.dataset.id!,
    base_url: inputs.base_url.value.trim(),
    model: inputs.model.value.trim(),
    key: inputs.key.value.trim(),
    zai_key: "",
    anthropic_key: "",
  };
}
```

Wire events next to the existing provider-row delegation (find where `clear-key-btn` / `remove-prov-btn` clicks are handled; follow the same delegation style; if rows have no delegation, add one on `providerRows`):

```ts
providerRows.addEventListener("click", (e) => {
  const target = e.target as HTMLElement;
  const testBtn = target.closest<HTMLButtonElement>(".test-btn");
  if (testBtn) {
    const row = testBtn.closest<HTMLElement>(".provider-row");
    if (row) {
      const status = row.querySelector<HTMLElement>(".prov-test-status");
      if (status) void runLlmTest(testPayloadForRow(row), testBtn, status);
    }
  }
});

envTestButton.addEventListener("click", () => {
  void runLlmTest(
    envTestPayload({
      base_url: envBaseUrl.value,
      model: envModel.value,
      zai_key: envZaiKey.value,
      anthropic_key: envAnthropicKey.value,
    }),
    envTestButton,
    envTestStatus,
  );
});

envClearKeysButton.addEventListener("click", async () => {
  try {
    await ipc.llmKeyClear(ENV_ZAI_KEY_ID);
    const view = await ipc.llmKeyClear(ENV_ANTHROPIC_KEY_ID);
    renderSettings(view);
    await refreshLlm();
  } catch (e) {
    settingsNote.classList.add("error");
    settingsNote.textContent = `Could not clear keys: ${e}`;
  }
});
```

If the existing code already delegates `.provider-row` clicks elsewhere, integrate the `.test-btn` branch there instead of a second listener.

- [ ] **Step 7: `src/app.css`** — check how `.provider-row` lays out (grid or flex). Make the per-row status a full-width line below the inputs:

```css
.prov-test-status {
  grid-column: 1 / -1;
  min-height: 1em;
}
```

(If `.provider-row` is flex, use `flex-basis: 100%;` instead — match what keeps the row layout intact for the pre-existing controls.)

- [ ] **Step 8: Run tests + type check**

Run: `npm test && npm run check`
Expected: all pass, tsc clean.

- [ ] **Step 9: Commit**

```bash
git add src/settings.ts src/ipc.ts index.html src/main.ts src/app.css src/settings.test.ts
git commit -m "feat(settings): env-variable override section + per-provider Test connection"
```

---

### Task 6: Docs

**Files:**
- Modify: `docs/ai-search.md` (provider configuration section; check `grep -rn "ZAI_API_KEY" docs/` for other spots that describe the fallback as read-only)

**Interfaces:** none (docs only).

- [ ] **Step 1:** Update `docs/ai-search.md`'s provider/env configuration section:
  - The environment fallback is now editable in Settings → AI assistance; UI values override the process environment (layer order: keyring override → settings override → process env).
  - API-key overrides are stored in the OS keyring (`llm:env:ZAI_API_KEY`, `llm:env:ANTHROPIC_API_KEY`), never in settings.json.
  - "Test connection" (per provider and for the env fallback) sends one minimal `/v1/messages` request (`max_tokens: 1`, fixed "ping", no vault data); the click itself is the consent — it is not part of the Ask consent gate; failures are classified (invalid key, wrong base URL, unknown model, unreachable host).
- [ ] **Step 2:** `grep -rn "read-only" docs/ | grep -i env` — fix any stale "read-only environment fallback" phrasing the same way.

- [ ] **Step 3: Commit**

```bash
git add docs/
git commit -m "docs: Settings env overrides + Test connection in ai-search.md"
```

---

## Final verification (controller)

- `cargo test --workspace` green, `npm test` green, `npm run check` clean.
- Final whole-branch review via `superpowers:requesting-code-review` template with `scripts/review-package $(git merge-base main HEAD) HEAD`.

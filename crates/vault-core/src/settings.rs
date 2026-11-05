//! App settings: the user-editable configuration surface.
//!
//! Everything configurable through the Settings UI lives in one JSON file
//! (`settings.json` in the app data dir) — never in the vault database.
//! API keys are deliberately absent from the file: they are stored in the
//! OS keyring (session-memory fallback; see `src-tauri/src/secrets.rs`),
//! and this module only records per-provider metadata.
//!
//! The LLM provider model generalizes the environment-variable policy of
//! [`crate::llm::LlmConfig`] into named provider profiles whose *kind*
//! fixes the key/endpoint pairing rules:
//!
//! - `zai` — the z.ai GLM endpoint (the default provider). An explicit
//!   base URL must point at the z.ai host (any spelling); the model
//!   defaults to `glm-5`.
//! - `anthropic` — Anthropic keys are only ever sent to an explicitly
//!   configured endpoint, never the z.ai host; both base URL and model are
//!   explicit.
//! - `custom` — any Anthropic-compatible `/v1/messages` endpoint, with an
//!   explicit base URL and model.
//!
//! Environment variables keep working as a read-only fallback provider:
//! when the settings file defines no default, the env-configured endpoint
//! (if any) is used exactly as before Settings existed.
//!
//! This module is pure (paths are injected by the caller) and covered by
//! unit tests; the Tauri shell glues it to the keyring and the UI.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::llm::LlmConfig;

/// Current settings file format version.
pub const SETTINGS_VERSION: u32 = 1;
/// File name of the settings file inside the app data dir.
pub const SETTINGS_FILE: &str = "settings.json";

/// Maximum results that may feed the RAG summary.
pub const MAX_SUMMARIZE_TOP: u32 = 50;

/// One LLM provider kind, fixing the pairing rules (see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// The z.ai GLM endpoint (default base URL and model).
    Zai,
    /// Anthropic — explicit endpoint only, never the z.ai host.
    Anthropic,
    /// Any Anthropic-compatible endpoint, fully explicit.
    Custom,
}

impl ProviderKind {
    /// Human-facing label (also the default provider name).
    pub fn label(self) -> &'static str {
        match self {
            Self::Zai => "z.ai GLM",
            Self::Anthropic => "Anthropic",
            Self::Custom => "Custom",
        }
    }
}

/// UI theme preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the OS `prefers-color-scheme`.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// Misc app options (the non-provider part of Settings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Options {
    /// UI color scheme.
    pub theme: Theme,
    /// How many top results feed the RAG summary (1..=MAX_SUMMARIZE_TOP).
    pub summarize_top: u32,
    /// Desktop notifications for new mail after a sync/watch cycle
    /// (cross-platform native notifications; silent while the app window
    /// is focused). Older settings files without the field default to
    /// enabled.
    #[serde(default = "default_notifications")]
    pub notifications: bool,
}

/// `Options::notifications` serde default (see the field docs).
fn default_notifications() -> bool {
    true
}

impl Default for Options {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            summarize_top: crate::llm::DEFAULT_SUMMARIZE_TOP as u32,
            notifications: default_notifications(),
        }
    }
}

/// One configured LLM provider profile. Carries no key material: the API
/// key lives in the OS keyring under the profile `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderProfile {
    /// Stable opaque id (also the keyring entry name). Assigned once when
    /// the provider is added and never regenerated.
    pub id: String,
    /// Display name; empty means the kind label.
    pub name: String,
    /// Which pairing rules apply.
    pub kind: ProviderKind,
    /// Endpoint base URL (without `/v1/messages`). Empty = the kind's
    /// default (only `zai` has one).
    pub base_url: String,
    /// Model name. Empty = the kind's default (only `zai` has one).
    pub model: String,
}

impl ProviderProfile {
    /// Display name (explicit or the kind label).
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            self.kind.label()
        } else {
            self.name.trim()
        }
    }

    /// Effective base URL after kind defaults are applied.
    pub fn effective_base_url(&self) -> String {
        match self.kind {
            ProviderKind::Zai => {
                if self.base_url.trim().is_empty() {
                    LlmConfig::DEFAULT_BASE_URL.to_string()
                } else {
                    self.base_url.trim().to_string()
                }
            }
            _ => self.base_url.trim().to_string(),
        }
    }

    /// Effective model name after kind defaults are applied.
    pub fn effective_model(&self) -> String {
        match self.kind {
            ProviderKind::Zai => {
                if self.model.trim().is_empty() {
                    LlmConfig::DEFAULT_MODEL.to_string()
                } else {
                    self.model.trim().to_string()
                }
            }
            _ => self.model.trim().to_string(),
        }
    }

    /// Endpoint host for display (e.g. `api.z.ai`).
    pub fn host(&self) -> String {
        endpoint_host(&self.effective_base_url()).unwrap_or_else(|| self.effective_base_url())
    }
}

/// The whole settings file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Format version (see [`SETTINGS_VERSION`]).
    pub version: u32,
    /// Configured provider profiles (never contain keys).
    pub providers: Vec<ProviderProfile>,
    /// Id of the default provider profile; `None` = automatic (the
    /// environment-configured endpoint, else local-only mode).
    pub default_provider: Option<String>,
    /// Misc options.
    pub options: Options,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            providers: Vec::new(),
            default_provider: None,
            options: Options::default(),
        }
    }
}

/// Reserved provider ids (pseudo-providers in the Ask menu).
pub const LOCAL_ID: &str = "local";
pub const ENV_ID: &str = "env";

/// Host of an endpoint URL for provider comparison — same semantics as the
/// env policy in [`crate::llm`]: lowercase host via `Url`, userinfo and
/// port excluded, one trailing root dot stripped; `None` when unparseable.
fn endpoint_host(url: &str) -> Option<String> {
    crate::llm::endpoint_host(url)
}

/// Whether `url` parses as an absolute http(s) URL with a host.
fn is_http_url(url: &str) -> bool {
    Url::parse(url)
        .map(|u| {
            matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())
        })
        .unwrap_or(false)
}

/// Whether `url` parses and points at a loopback host (`localhost`,
/// `127.0.0.0/8`, `::1`) — the one place plaintext http stays allowed.
fn is_loopback_url(url: &str) -> bool {
    Url::parse(url)
        .map(|u| match u.host() {
            Some(url::Host::Domain(d)) => d == "localhost" || d.ends_with(".localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        })
        .unwrap_or(false)
}

/// Refuse a base URL whose scheme is plaintext `http` on a non-loopback
/// host: a stored API key must never travel unencrypted.
fn check_no_plaintext_http(name: &str, base_url: &str) -> crate::Result<()> {
    let plaintext = Url::parse(base_url)
        .map(|u| u.scheme() == "http")
        .unwrap_or(false);
    if plaintext && !is_loopback_url(base_url) {
        return Err(settings_error(format!(
            "provider “{name}”: the API key must not travel over plaintext http — use https \
             for {base_url} (or a localhost/127.0.0.1 endpoint for testing)"
        )));
    }
    Ok(())
}

/// The z.ai host, for pairing checks (mirrors the env policy).
fn zai_host() -> String {
    endpoint_host(LlmConfig::DEFAULT_BASE_URL).expect("default base URL parses")
}

impl Settings {
    /// Load from `path`. `Ok(None)` when the file does not exist (fresh
    /// install → defaults). `Err` for anything unreadable, corrupt, or a
    /// format version this build does not understand — the caller keeps
    /// running on defaults and surfaces the error in the Settings UI.
    pub fn load(path: &Path) -> crate::Result<Option<Self>> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(crate::Error::Io(e)),
        };
        let settings: Self = serde_json::from_slice(&bytes)
            .map_err(crate::Error::Json)
            .map_err(|e| crate::Error::Settings(format!("invalid {}: {e}", path.display())))?;
        if settings.version != SETTINGS_VERSION {
            return Err(crate::Error::Settings(format!(
                "settings file {} has version {} but this build supports {}",
                path.display(),
                settings.version,
                SETTINGS_VERSION
            )));
        }
        Ok(Some(settings))
    }

    /// Save atomically (write to `<path>.tmp`, then rename over `path`).
    /// The file never contains API keys.
    pub fn save(&self, path: &Path) -> crate::Result<()> {
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(crate::Error::Json)?;
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Normalize in place: trim all free-text fields and default empty
    /// provider names to the kind label.
    pub fn normalize(&mut self) {
        for p in &mut self.providers {
            p.id = p.id.trim().to_string();
            p.name = p.name.trim().to_string();
            p.base_url = p.base_url.trim().trim_end_matches('/').to_string();
            p.model = p.model.trim().to_string();
        }
        if let Some(default) = &mut self.default_provider {
            let d = default.trim().to_string();
            if d.is_empty() {
                self.default_provider = None;
            } else {
                *default = d;
            }
        }
    }

    /// Validate the pairing rules and cross-references. Returns the first
    /// problem as a human-readable error (shown in the Settings UI before
    /// anything is saved or any key is sent anywhere).
    pub fn validate(&self) -> crate::Result<()> {
        let mut seen = HashSet::new();
        let mut known_ids = HashSet::new();
        for p in &self.providers {
            let name = p.display_name();
            if p.id.is_empty() || !valid_id(&p.id) {
                return Err(settings_error(format!(
                    "provider “{name}”: id “{}” is invalid (lowercase letters, digits and dashes)",
                    p.id
                )));
            }
            if p.id == LOCAL_ID || p.id == ENV_ID {
                return Err(settings_error(format!(
                    "provider “{name}”: id “{}” is reserved",
                    p.id
                )));
            }
            if !seen.insert(p.id.clone()) {
                return Err(settings_error(format!(
                    "two providers share the id “{}” (rename one)",
                    p.id
                )));
            }
            known_ids.insert(p.id.clone());
            match p.kind {
                ProviderKind::Zai => {
                    if !p.base_url.is_empty() {
                        if !is_http_url(&p.base_url) {
                            return Err(settings_error(format!(
                                "provider “{name}”: base URL must be an absolute http(s) URL"
                            )));
                        }
                        if endpoint_host(&p.base_url).as_deref() != Some(zai_host().as_str()) {
                            return Err(settings_error(format!(
                                "provider “{name}”: a z.ai key is only ever sent to the z.ai \
                                 endpoint, but the base URL points at {}. Remove the base URL to \
                                 use the default {}",
                                p.host(),
                                LlmConfig::DEFAULT_BASE_URL
                            )));
                        }
                        check_no_plaintext_http(name, &p.base_url)?;
                    }
                }
                ProviderKind::Anthropic => {
                    if p.base_url.is_empty() {
                        return Err(settings_error(format!(
                            "provider “{name}”: an Anthropic key is only sent to an explicitly \
                             configured endpoint — set a base URL (e.g. https://api.anthropic.com)"
                        )));
                    }
                    if !is_http_url(&p.base_url) {
                        return Err(settings_error(format!(
                            "provider “{name}”: base URL must be an absolute http(s) URL"
                        )));
                    }
                    if endpoint_host(&p.base_url) == Some(zai_host()) {
                        return Err(settings_error(format!(
                            "provider “{name}”: refusing to send an Anthropic key to the z.ai \
                             endpoint — point the base URL at the endpoint that accepts this key"
                        )));
                    }
                    check_no_plaintext_http(name, &p.base_url)?;
                    if p.model.is_empty() {
                        return Err(settings_error(format!(
                            "provider “{name}”: a model name is required for Anthropic providers"
                        )));
                    }
                }
                ProviderKind::Custom => {
                    if p.base_url.is_empty() {
                        return Err(settings_error(format!(
                            "provider “{name}”: a custom provider needs a base URL"
                        )));
                    }
                    if !is_http_url(&p.base_url) {
                        return Err(settings_error(format!(
                            "provider “{name}”: base URL must be an absolute http(s) URL"
                        )));
                    }
                    check_no_plaintext_http(name, &p.base_url)?;
                    if p.model.is_empty() {
                        return Err(settings_error(format!(
                            "provider “{name}”: a model name is required for custom providers"
                        )));
                    }
                }
            }
        }
        if let Some(default) = &self.default_provider {
            if !known_ids.contains(default) {
                return Err(settings_error(format!(
                    "default provider “{default}” is not configured"
                )));
            }
        }
        if self.options.summarize_top == 0 || self.options.summarize_top > MAX_SUMMARIZE_TOP {
            return Err(settings_error(format!(
                "summarize_top must be between 1 and {MAX_SUMMARIZE_TOP}"
            )));
        }
        Ok(())
    }
}

/// Provider ids are opaque but constrained: lowercase slug, so they work
/// as keyring entry names and are easy to mirror in the frontend.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
        && !id.ends_with('-')
}

/// Slugify free text into a valid id base (`"My Provider!"` → `"my-provider"`).
pub fn slugify(text: &str) -> String {
    let mut slug: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    slug.trim_matches('-').to_string()
}

/// First free id with the given base: `base`, `base-2`, `base-3`, …
/// (the frontend mirrors this when adding providers).
pub fn unique_id(base: &str, taken: &HashSet<String>) -> String {
    let base = {
        let slug = slugify(base);
        if slug.is_empty() {
            "provider".to_string()
        } else {
            slug
        }
    };
    let mut candidate = base.clone();
    let mut n = 1;
    while taken.contains(&candidate) {
        n += 1;
        candidate = format!("{base}-{n}");
    }
    candidate
}

/// One entry of the provider menu shown in the Ask UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderMenuItem {
    /// Selection id sent back with `llm_ask` (`local`, `env`, or a profile id).
    pub id: String,
    /// Display name (e.g. `Work GLM`).
    pub name: String,
    /// `local`, `env`, `zai`, `anthropic`, or `custom` (kind string).
    pub kind: String,
    /// Effective model (empty for `local`).
    pub model: String,
    /// Endpoint host (empty for `local`).
    pub host: String,
    /// Whether this entry can be selected right now (profile has a key).
    pub available: bool,
}

/// Build the provider menu: local-only mode first, then the configured
/// profiles (in settings order), then the environment fallback. `has_key`
/// reports whether a profile's API key is stored; `env` is the validly
/// configured env fallback (if any).
pub fn build_menu(
    settings: &Settings,
    has_key: impl Fn(&str) -> bool,
    env: Option<&LlmConfig>,
) -> Vec<ProviderMenuItem> {
    let mut items = vec![ProviderMenuItem {
        id: LOCAL_ID.to_string(),
        name: "Local full-text".to_string(),
        kind: LOCAL_ID.to_string(),
        model: String::new(),
        host: String::new(),
        available: true,
    }];
    for p in &settings.providers {
        items.push(ProviderMenuItem {
            id: p.id.clone(),
            name: p.display_name().to_string(),
            kind: match p.kind {
                ProviderKind::Zai => "zai",
                ProviderKind::Anthropic => "anthropic",
                ProviderKind::Custom => "custom",
            }
            .to_string(),
            model: p.effective_model(),
            host: p.host(),
            available: has_key(&p.id),
        });
    }
    if let Some(env) = env {
        items.push(ProviderMenuItem {
            id: ENV_ID.to_string(),
            name: "Environment variables".to_string(),
            kind: ENV_ID.to_string(),
            model: env.model.clone(),
            host: env.host(),
            available: true,
        });
    }
    items
}

fn settings_error(message: String) -> crate::Error {
    crate::Error::Settings(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, kind: ProviderKind, base_url: &str, model: &str) -> ProviderProfile {
        ProviderProfile {
            id: id.into(),
            name: String::new(),
            kind,
            base_url: base_url.into(),
            model: model.into(),
        }
    }

    fn settings_with(providers: Vec<ProviderProfile>) -> Settings {
        let mut s = Settings {
            providers,
            ..Settings::default()
        };
        s.normalize();
        s
    }

    #[test]
    fn load_missing_file_is_none_and_defaults_apply() {
        let dir = std::env::temp_dir().join(format!("mcv-settings-{}", std::process::id()));
        let path = dir.join("nope-settings.json");
        assert!(Settings::load(&path).unwrap().is_none());
        assert_eq!(Settings::default().options.summarize_top, 8);
        assert_eq!(Settings::default().options.theme, Theme::System);
        assert!(Settings::default().options.notifications);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("mcv-settings-rt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let mut s = settings_with(vec![profile("glm", ProviderKind::Zai, "", "")]);
        s.default_provider = Some("glm".into());
        s.options.theme = Theme::Dark;
        s.options.summarize_top = 12;
        s.save(&path).unwrap();
        let loaded = Settings::load(&path).unwrap().expect("file exists");
        assert_eq!(loaded, s);
        // The file never contains key material — nothing to leak by design.
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("api_key"));
        assert!(!text.contains("sk-"));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_rejects_corrupt_and_future_versions() {
        let dir = std::env::temp_dir().join(format!("mcv-settings-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        assert!(Settings::load(&path).is_err());
        let future = serde_json::json!({"version": 99, "providers": [], "default_provider": null, "options": {"theme": "system", "summarize_top": 8}});
        fs::write(&path, serde_json::to_vec(&future).unwrap()).unwrap();
        let err = Settings::load(&path).unwrap_err().to_string();
        assert!(err.contains("version 99"), "got: {err}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn notifications_option_round_trips_and_backfills() {
        let dir = std::env::temp_dir().join(format!("mcv-settings-ntf-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        // A file written before the option existed: the field is absent
        // and must backfill to the enabled default, not refuse to load.
        let older = serde_json::json!({
            "version": SETTINGS_VERSION,
            "providers": [],
            "default_provider": null,
            "options": {"theme": "system", "summarize_top": 8}
        });
        fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
        let loaded = Settings::load(&path).unwrap().expect("file exists");
        assert!(loaded.options.notifications);
        // An explicit opt-out survives a save/load round trip.
        let mut s = loaded;
        s.options.notifications = false;
        s.save(&path).unwrap();
        assert!(
            !Settings::load(&path)
                .unwrap()
                .unwrap()
                .options
                .notifications
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn zai_provider_defaults_and_pairing() {
        // Empty URL/model → defaults; valid as-is.
        let s = settings_with(vec![profile("glm", ProviderKind::Zai, "", "")]);
        s.validate().unwrap();
        let p = &s.providers[0];
        assert_eq!(p.effective_base_url(), LlmConfig::DEFAULT_BASE_URL);
        assert_eq!(p.effective_model(), "glm-5");
        assert_eq!(p.host(), "api.z.ai");
        assert_eq!(p.display_name(), "z.ai GLM");

        // Explicit z.ai spellings are accepted.
        for url in [
            "https://api.z.ai/api/anthropic",
            "https://API.Z.AI/api/anthropic/",
            "https://api.z.ai:443",
            "https://api.z.ai./api/anthropic",
        ] {
            let s = settings_with(vec![profile("glm", ProviderKind::Zai, url, "glm-4.6")]);
            s.validate()
                .unwrap_or_else(|e| panic!("{url} should pass: {e}"));
        }

        // A z.ai key is never sent elsewhere.
        let s = settings_with(vec![profile(
            "glm",
            ProviderKind::Zai,
            "https://api.anthropic.com",
            "",
        )]);
        let err = s.validate().unwrap_err().to_string();
        assert!(
            err.contains("only ever sent to the z.ai endpoint"),
            "got: {err}"
        );

        // Nor to an unparseable URL.
        let s = settings_with(vec![profile("glm", ProviderKind::Zai, "not a url", "")]);
        assert!(s.validate().is_err());

        // Plaintext http: refused off loopback, so the z.ai key never
        // travels unencrypted.
        let s = settings_with(vec![profile(
            "glm",
            ProviderKind::Zai,
            "http://api.z.ai/api/anthropic",
            "",
        )]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("plaintext http"), "got: {err}");
    }

    #[test]
    fn anthropic_provider_requires_explicit_non_zai_endpoint_and_model() {
        // Empty base URL: refused (an Anthropic key is never implicit).
        let s = settings_with(vec![profile(
            "claude",
            ProviderKind::Anthropic,
            "",
            "claude-x",
        )]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("explicitly configured endpoint"), "got: {err}");

        // The z.ai host: refused, in any spelling.
        for url in [
            "https://api.z.ai",
            "https://api.z.ai/api/anthropic",
            "https://API.Z.AI",
            "https://api.z.ai:443/api/anthropic",
        ] {
            let s = settings_with(vec![profile(
                "claude",
                ProviderKind::Anthropic,
                url,
                "claude-x",
            )]);
            let err = s.validate().unwrap_err().to_string();
            assert!(
                err.contains("refusing to send an Anthropic key"),
                "{url}: {err}"
            );
        }

        // Its own endpoint with a model: fine.
        let s = settings_with(vec![profile(
            "claude",
            ProviderKind::Anthropic,
            "https://api.anthropic.com",
            "claude-sonnet-4-5",
        )]);
        s.validate().unwrap();

        // Missing model: refused.
        let s = settings_with(vec![profile(
            "claude",
            ProviderKind::Anthropic,
            "https://api.anthropic.com",
            "",
        )]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("model name is required"), "got: {err}");

        // Plaintext http off loopback: refused (key must stay encrypted);
        // localhost endpoints stay allowed for local mocks.
        let s = settings_with(vec![profile(
            "claude",
            ProviderKind::Anthropic,
            "http://api.anthropic.com",
            "claude-x",
        )]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("plaintext http"), "got: {err}");
        let s = settings_with(vec![profile(
            "claude",
            ProviderKind::Anthropic,
            "http://127.0.0.1:8080",
            "claude-x",
        )]);
        s.validate().unwrap();
    }

    #[test]
    fn custom_provider_needs_url_and_model() {
        let s = settings_with(vec![profile("mine", ProviderKind::Custom, "", "m")]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("needs a base URL"), "got: {err}");
        let s = settings_with(vec![profile(
            "mine",
            ProviderKind::Custom,
            "http://127.0.0.1:9999/api",
            "",
        )]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("model name is required"), "got: {err}");
        let s = settings_with(vec![profile(
            "mine",
            ProviderKind::Custom,
            "http://127.0.0.1:9999/api",
            "local-model",
        )]);
        s.validate().unwrap();
        assert_eq!(s.providers[0].host(), "127.0.0.1");

        // Plaintext http off loopback: refused for custom providers too —
        // the stored key must never travel unencrypted.
        let s = settings_with(vec![profile(
            "mine",
            ProviderKind::Custom,
            "http://api.example.com",
            "m",
        )]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("plaintext http"), "got: {err}");
    }

    #[test]
    fn ids_must_be_valid_unique_and_unreserved() {
        for bad in [
            "",
            "-x",
            "x-",
            "UPPER",
            "with space",
            "a".repeat(65).as_str(),
        ] {
            let s = settings_with(vec![profile(bad, ProviderKind::Zai, "", "")]);
            assert!(s.validate().is_err(), "id {bad:?} should be invalid");
        }
        for reserved in [LOCAL_ID, ENV_ID] {
            let s = settings_with(vec![profile(reserved, ProviderKind::Zai, "", "")]);
            let err = s.validate().unwrap_err().to_string();
            assert!(err.contains("reserved"), "got: {err}");
        }
        let s = settings_with(vec![
            profile("glm", ProviderKind::Zai, "", ""),
            profile("glm", ProviderKind::Custom, "http://x.example", "m"),
        ]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("share the id"), "got: {err}");
    }

    #[test]
    fn default_provider_must_exist() {
        let mut s = settings_with(vec![profile("glm", ProviderKind::Zai, "", "")]);
        s.default_provider = Some("gone".into());
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("not configured"), "got: {err}");
        s.default_provider = Some("glm".into());
        s.validate().unwrap();
    }

    #[test]
    fn summarize_top_bounds() {
        let mut s = settings_with(vec![]);
        s.options.summarize_top = 0;
        assert!(s.validate().is_err());
        s.options.summarize_top = MAX_SUMMARIZE_TOP + 1;
        assert!(s.validate().is_err());
        s.options.summarize_top = MAX_SUMMARIZE_TOP;
        s.validate().unwrap();
    }

    #[test]
    fn normalize_trims_and_defaults_names() {
        let mut s = Settings {
            providers: vec![ProviderProfile {
                id: " glm ".into(),
                name: "  ".into(),
                kind: ProviderKind::Zai,
                base_url: " https://api.z.ai/api/anthropic/ ".into(),
                model: " glm-5 ".into(),
            }],
            default_provider: Some(" ".into()),
            options: Options::default(),
            version: SETTINGS_VERSION,
        };
        s.normalize();
        assert_eq!(s.providers[0].id, "glm");
        assert_eq!(s.providers[0].display_name(), "z.ai GLM");
        assert_eq!(s.providers[0].base_url, "https://api.z.ai/api/anthropic");
        assert_eq!(s.providers[0].model, "glm-5");
        assert_eq!(s.default_provider, None);
    }

    #[test]
    fn unique_id_slugs_and_dedupes() {
        let taken: HashSet<String> = ["glm", "glm-2"].into_iter().map(String::from).collect();
        assert_eq!(unique_id("GLM", &taken), "glm-3");
        assert_eq!(unique_id("My Work Provider!", &taken), "my-work-provider");
        assert_eq!(unique_id("!!!", &taken), "provider");
        assert_eq!(unique_id("glm", &HashSet::new()), "glm");
    }

    #[test]
    fn menu_lists_local_profiles_and_env_in_order() {
        let s = settings_with(vec![
            profile("glm", ProviderKind::Zai, "", ""),
            profile("mine", ProviderKind::Custom, "http://127.0.0.1:9/api", "m"),
        ]);
        let env = LlmConfig {
            base_url: "http://127.0.0.1:1/api".into(),
            api_key: "k".into(),
            model: "env-model".into(),
        };
        let menu = build_menu(&s, |id| id == "glm", Some(&env));
        let ids: Vec<&str> = menu.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["local", "glm", "mine", "env"]);
        assert!(menu[0].available);
        assert_eq!(menu[1].model, "glm-5");
        assert_eq!(menu[1].host, "api.z.ai");
        assert!(menu[1].available, "glm has a key");
        assert!(!menu[2].available, "mine has no key");
        assert_eq!(menu[3].model, "env-model");
        assert_eq!(menu[3].host, "127.0.0.1");
        assert_eq!(menu[3].name, "Environment variables");

        // Without env config the menu ends at the profiles.
        let menu = build_menu(&s, |_| false, None);
        assert_eq!(menu.last().unwrap().id, "mine");
    }
}

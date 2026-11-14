//! The LLM provider registry: settings-file profiles + OS-keyring keys +
//! the environment fallback, resolved into what one ask actually uses.
//!
//! Selection rules (see also `vault_core::settings`):
//! - an explicit provider id (`llm_ask`'s `provider` argument) wins;
//!   `local` means local-only, `env` the environment fallback;
//! - no explicit id → the settings default provider, else the environment
//!   fallback (backward compatible with pre-Settings env configuration),
//!   else local-only;
//! - a profile without a stored API key degrades to local-only with a
//!   note — a missing key never turns into an error or a network call.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use vault_core::llm::{plaintext_http_off_loopback, LlmConfig, LlmStatus};
use vault_core::settings::{
    build_menu, ProviderMenuItem, ProviderProfile, Settings, Theme, ENV_ID, LOCAL_ID,
};

use crate::secrets::SecretStore;

/// The pseudo-provider name shown for the environment fallback.
const ENV_NAME: &str = "Environment variables";

/// Keyring ids for the environment-variable key overrides (stored with
/// `Secrets::save_llm_key`, so the entries are `llm:env:ZAI_API_KEY` and
/// `llm:env:ANTHROPIC_API_KEY`). Shared with the Settings UI (clear-key).
pub const ENV_ZAI_KEY_ID: &str = "env:ZAI_API_KEY";
pub const ENV_ANTHROPIC_KEY_ID: &str = "env:ANTHROPIC_API_KEY";

/// The two keyring override ids, for reload loops.
fn env_key_ids() -> [&'static str; 2] {
    [ENV_ZAI_KEY_ID, ENV_ANTHROPIC_KEY_ID]
}

/// Which LLM backend one ask resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// A usable provider: profile id (`env` for the environment fallback)
    /// plus a ready-to-use config (with the API key).
    Provider {
        id: String,
        name: String,
        config: LlmConfig,
    },
    /// Local-only plain search. `reason` is a user-facing note (empty when
    /// local mode was simply the default and nothing is configured).
    Local { reason: String },
}

impl Selection {
    fn local(reason: impl Into<String>) -> Self {
        Self::Local {
            reason: reason.into(),
        }
    }
}

/// Runtime state over the settings file: resolved keys, the environment
/// fallback, and any load-time diagnostics surfaced in the Settings UI.
pub struct LlmRegistry {
    settings: Settings,
    /// Profile id → API key (from the keyring / session memory).
    keys: HashMap<String, String>,
    /// Keyring override values for the environment fallback
    /// (`env:ZAI_API_KEY` / `env:ANTHROPIC_API_KEY`).
    env_keys: HashMap<String, String>,
    /// Process-environment lookup (injected so tests stay deterministic).
    #[allow(clippy::type_complexity)] // the injected lookup, kept inline to mirror `load`
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

impl LlmRegistry {
    /// Build from the load-time pieces: settings file contents (or the
    /// error that prevented loading), the process-environment lookup (the
    /// bottom layer of the environment fallback, under the keyring and
    /// settings-file overrides), and a loader for stored keys. A parseable
    /// but invalid settings file is treated like a corrupt one — defaults
    /// apply and the validation error is surfaced — so the pairing policy
    /// enforced at save time holds from boot on, for externally edited
    /// files too.
    #[allow(clippy::type_complexity)] // the injected lookup, same type as the field
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
        // `reload_keys` is the single funnel that derives the env fallback.
        registry.reload_keys(load_key);
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

    /// Re-read the stored key of every configured profile, plus the two
    /// environment-fallback key overrides, then recompute the environment
    /// fallback so the derived state never lags the reloaded keys.
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
        self.recompute_env();
    }

    /// The settings currently in effect.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Replace the settings (after a successful save) and re-read keys
    /// (which also recomputes the environment fallback).
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
    }

    /// Forget a single key at runtime (after a keyring clear): a profile's
    /// key by profile id, or an environment-fallback key override by its
    /// `env:` id ([`ENV_ZAI_KEY_ID`] / [`ENV_ANTHROPIC_KEY_ID`]).
    pub fn drop_key(&mut self, provider_id: &str) {
        self.keys.remove(provider_id);
        self.env_keys.remove(provider_id);
        self.recompute_env();
    }

    /// Build a resolved [`LlmConfig`] for one profile (no key lookup).
    fn config_for(profile: &ProviderProfile) -> LlmConfig {
        LlmConfig {
            base_url: profile.effective_base_url(),
            model: profile.effective_model(),
            api_key: String::new(),
        }
    }

    /// Resolve what an ask should use. `requested` is the explicit provider
    /// id from the Ask UI menu, if any.
    pub fn select(&self, requested: Option<&str>) -> Selection {
        let profile_by_id = |id: &str| self.settings.providers.iter().find(|p| p.id == id);
        match requested {
            Some(LOCAL_ID) => {
                Selection::local("local mode selected — nothing sent to any provider")
            }
            Some(ENV_ID) => match &self.env {
                Some(env) => Selection::Provider {
                    id: ENV_ID.into(),
                    name: ENV_NAME.into(),
                    config: env.clone(),
                },
                None => Selection::local(format!(
                    "environment provider is not configured{}",
                    self.env_error
                        .as_deref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                )),
            },
            Some(id) => match profile_by_id(id) {
                Some(profile) => match self.keys.get(id) {
                    Some(key) => {
                        let mut config = Self::config_for(profile);
                        config.api_key = key.clone();
                        Selection::Provider {
                            id: id.to_string(),
                            name: profile.display_name().to_string(),
                            config,
                        }
                    }
                    None => Selection::local(format!(
                        "provider “{}” has no API key stored — set one in Settings",
                        profile.display_name()
                    )),
                },
                None => Selection::local(format!("unknown provider “{id}”")),
            },
            None => self.select_default(),
        }
    }

    /// The implicit selection: settings default → env fallback → local. A
    /// default that references no configured profile (a settings file
    /// edited outside the app) falls through to the env fallback, exactly
    /// like `None`.
    fn select_default(&self) -> Selection {
        if let Some(default) = &self.settings.default_provider {
            if let Some(profile) = self.settings.providers.iter().find(|p| &p.id == default) {
                return match self.keys.get(default) {
                    Some(key) => {
                        let mut config = Self::config_for(profile);
                        config.api_key = key.clone();
                        Selection::Provider {
                            id: default.clone(),
                            name: profile.display_name().to_string(),
                            config,
                        }
                    }
                    None => Selection::local(format!(
                        "default provider “{}” has no API key stored — set one in Settings",
                        profile.display_name()
                    )),
                };
            }
        }
        match &self.env {
            Some(env) => Selection::Provider {
                id: ENV_ID.into(),
                name: ENV_NAME.into(),
                config: env.clone(),
            },
            None => Selection::local(String::new()),
        }
    }

    /// Build the exact [`LlmConfig`] a "Test connection" click should probe:
    /// the target's stored/effective values with the currently-typed form
    /// values layered on top. Fails with a user-facing message when there is
    /// nothing to test or the configuration refuses itself (pairing,
    /// plaintext http).
    // Not yet called outside tests; Task 4's `llm_test` command is the
    // caller (drop this `allow` there).
    #[allow(dead_code)]
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
                        .or_else(|| self.env_keys.get(ENV_ZAI_KEY_ID).cloned())
                        .or_else(|| self.env_lookup(name)),
                    "ANTHROPIC_API_KEY" => (!anthropic.is_empty())
                        .then(|| anthropic.clone())
                        .or_else(|| self.env_keys.get(ENV_ANTHROPIC_KEY_ID).cloned())
                        .or_else(|| self.env_lookup(name)),
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
                    return Err(
                        "no API key set for the environment fallback — type one to test".into(),
                    )
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

    /// Id of the default provider for the menu preselection: the settings
    /// default (only while it references a configured profile — a dangling
    /// id from an externally edited file falls through exactly like
    /// [`Self::select_default`]), else `env` when the environment fallback
    /// is usable, else `None` (nothing configured → the UI preselects
    /// local-only).
    pub fn default_provider_id(&self) -> Option<String> {
        self.settings
            .default_provider
            .as_ref()
            .filter(|default| self.settings.providers.iter().any(|p| &p.id == *default))
            .cloned()
            .or_else(|| self.env.as_ref().map(|_| ENV_ID.to_string()))
    }

    /// The provider menu for the Ask UI.
    pub fn menu(&self) -> Vec<ProviderMenuItem> {
        build_menu(
            &self.settings,
            |id| self.keys.contains_key(id),
            self.env.as_ref(),
        )
    }

    /// [`LlmStatus`] for the UI: scalar fields describe the default
    /// selection; `providers` is the menu; never exposes any key.
    pub fn status(&self) -> LlmStatus {
        let selection = self.select_default();
        let (enabled, model, host) = match &selection {
            Selection::Provider { config, .. } => (true, config.model.clone(), config.host()),
            Selection::Local { .. } => (false, String::new(), String::new()),
        };
        LlmStatus {
            enabled,
            model,
            host,
            summarize_top: self.settings.options.summarize_top as usize,
            providers: self.menu(),
            default_provider: self.default_provider_id(),
        }
    }
}

// ---------------------------------------------------------------------------
// IPC payloads (settings_get / settings_save / llm_key_clear / llm_test)
// ---------------------------------------------------------------------------

/// One provider profile as the Settings UI sees it: the file data plus
/// whether a key is stored (the key itself never crosses the IPC boundary
/// back to the frontend).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderView {
    #[serde(flatten)]
    pub profile: ProviderProfile,
    /// Whether an API key is stored for this profile.
    pub has_key: bool,
}

/// Read-only info about the environment fallback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvProviderView {
    pub model: String,
    pub host: String,
}

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

/// Response of `settings_get` (and of a successful `settings_save`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingsView {
    pub providers: Vec<ProviderView>,
    pub default_provider: Option<String>,
    pub theme: Theme,
    pub summarize_top: u32,
    /// Desktop notifications for new mail after sync/watch cycles.
    pub notifications: bool,
    /// Keep running in the background when the window is closed (M7:
    /// close hides to the tray instead of exiting; opt-in, default off).
    pub keep_running_in_background: bool,
    /// Start the app automatically at login (M7; opt-in, default off).
    pub start_at_login: bool,
    /// The environment-fallback provider, when validly configured.
    pub env: Option<EnvProviderView>,
    /// The environment-variable overrides entered in Settings (non-secret
    /// values + key-presence flags).
    pub env_overrides: EnvOverridesView,
    /// Why the environment configuration was refused, if it was.
    pub env_error: Option<String>,
    /// Why the settings file could not be loaded, if it could not.
    pub settings_error: Option<String>,
    /// Where API keys were last saved (`os_keyring` / `memory`).
    pub key_store: Option<SecretStore>,
}

/// Request of `settings_save`: the complete intended settings plus API-key
/// updates. A non-empty `key` stores/overwrites the key for that provider
/// id; an empty string clears it; ids not mentioned keep their stored key.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SaveSettingsPayload {
    pub providers: Vec<ProviderProfile>,
    pub default_provider: Option<String>,
    pub theme: Theme,
    pub summarize_top: u32,
    /// Desktop notifications for new mail after sync/watch cycles.
    /// Absent in older payloads → the option's enabled default.
    #[serde(default = "default_true")]
    pub notifications: bool,
    /// Keep running in the background when the window is closed (M7).
    /// Absent in older payloads → disabled (strict opt-in, unlike
    /// `notifications`: closing must keep exiting until asked).
    #[serde(default)]
    pub keep_running_in_background: bool,
    /// Start the app automatically at login (M7). Absent in older
    /// payloads → disabled.
    #[serde(default)]
    pub start_at_login: bool,
    pub api_keys: Vec<ApiKeyUpdate>,
    /// Environment-variable overrides (ANTHROPIC_BASE_URL / MAILCHAT_LLM_MODEL).
    #[serde(default)]
    pub env_overrides: EnvOverridesPayload,
    /// API-key overrides for the environment fallback. `id` is the
    /// variable name (`ZAI_API_KEY` / `ANTHROPIC_API_KEY`); non-empty key
    /// stores, empty clears. Ids not mentioned keep their stored key.
    #[serde(default)]
    pub env_api_keys: Vec<ApiKeyUpdate>,
}

/// `SaveSettingsPayload::notifications` serde default.
fn default_true() -> bool {
    true
}

/// One API-key update inside [`SaveSettingsPayload`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApiKeyUpdate {
    pub id: String,
    /// Non-empty = store; empty = clear.
    pub key: String,
}

/// Non-secret env overrides inside [`SaveSettingsPayload`]. Empty values
/// mean "no override".
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
pub struct EnvOverridesPayload {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
}

/// One "Test connection" request: the target (`env` or a profile id)
/// plus the form values as currently typed, so an unsaved edit can be
/// tested as typed. Empty fields fall back to the stored/effective
/// values; empty key fields fall back to the stored key (for the env
/// target, then on to the process environment — the runtime layer
/// order). Profile targets read `key`; the env target reads
/// `zai_key`/`anthropic_key`; each target ignores the other's fields.
// Not yet constructed outside tests; Task 4's `llm_test` command is the
// caller (drop this `allow` there).
#[allow(dead_code)]
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

#[cfg(test)]
impl LlmTestPayload {
    /// Payload targeting `provider` with every form field empty.
    pub fn defaulting(provider: &str) -> Self {
        Self {
            provider: provider.into(),
            ..Self::default()
        }
    }
}

impl From<&LlmRegistry> for SettingsView {
    fn from(registry: &LlmRegistry) -> Self {
        let settings = &registry.settings;
        Self {
            providers: settings
                .providers
                .iter()
                .map(|p| ProviderView {
                    profile: p.clone(),
                    has_key: registry.keys.contains_key(&p.id),
                })
                .collect(),
            default_provider: registry.default_provider_id(),
            theme: settings.options.theme,
            summarize_top: settings.options.summarize_top,
            notifications: settings.options.notifications,
            keep_running_in_background: settings.options.keep_running_in_background,
            start_at_login: settings.options.start_at_login,
            env: registry.env.as_ref().map(|e| EnvProviderView {
                model: e.model.clone(),
                host: e.host(),
            }),
            env_overrides: EnvOverridesView {
                base_url: settings.env_overrides.anthropic_base_url.clone(),
                model: settings.env_overrides.llm_model.clone(),
                zai_key: registry.env_keys.contains_key(ENV_ZAI_KEY_ID),
                anthropic_key: registry.env_keys.contains_key(ENV_ANTHROPIC_KEY_ID),
            },
            env_error: registry.env_error.clone(),
            settings_error: registry.settings_error.clone(),
            key_store: registry.key_store,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vault_core::settings::ProviderKind;

    fn profile(id: &str, kind: ProviderKind) -> ProviderProfile {
        let mut p = ProviderProfile {
            id: id.into(),
            name: String::new(),
            kind,
            base_url: match kind {
                ProviderKind::Anthropic => "https://api.anthropic.com",
                ProviderKind::Custom => "http://127.0.0.1:9/api",
                ProviderKind::Zai => "",
            }
            .into(),
            model: match kind {
                ProviderKind::Zai => "",
                _ => "m",
            }
            .into(),
        };
        p.base_url = p.base_url.trim().into();
        p
    }

    fn settings_with(providers: Vec<ProviderProfile>, default_provider: Option<&str>) -> Settings {
        Settings {
            providers,
            default_provider: default_provider.map(String::from),
            ..Settings::default()
        }
    }

    /// The env-var shape the old precomputed `env_config()` helper stood
    /// for: an Anthropic key explicitly paired with a loopback base URL,
    /// plus a model override — now fed through the injected process env.
    fn env_vars() -> &'static [(&'static str, &'static str)] {
        &[
            ("ANTHROPIC_API_KEY", "env-key"),
            ("ANTHROPIC_BASE_URL", "http://127.0.0.1:1/api"),
            ("MAILCHAT_LLM_MODEL", "env-model"),
        ]
    }

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

    trait SelectionExt {
        fn matches_local_with_reason(&self) -> bool;
    }

    impl SelectionExt for Selection {
        fn matches_local_with_reason(&self) -> bool {
            matches!(self, Selection::Local { .. })
        }
    }

    #[test]
    fn nothing_configured_degrades_to_local() {
        let r = registry(Settings::default(), &[], &[]);
        assert_eq!(
            r.select(None),
            Selection::Local {
                reason: String::new()
            }
        );
        assert_eq!(r.default_provider_id(), None);
        let status = r.status();
        assert!(!status.enabled);
        assert_eq!(status.providers.len(), 1); // local only
        assert_eq!(status.default_provider, None);
    }

    #[test]
    fn env_fallback_is_the_default_when_no_file_default() {
        let r = registry(Settings::default(), &[], env_vars());
        match r.select(None) {
            Selection::Provider { id, config, .. } => {
                assert_eq!(id, ENV_ID);
                assert_eq!(config.api_key, "env-key");
            }
            other => panic!("env should be selected: {other:?}"),
        }
        assert_eq!(r.default_provider_id(), Some(ENV_ID.into()));
        assert!(r.status().enabled);
        assert_eq!(r.status().model, "env-model");
        assert_eq!(r.status().providers.last().unwrap().id, ENV_ID);
    }

    #[test]
    fn file_default_wins_over_env_and_carries_the_key() {
        let settings = settings_with(vec![profile("glm", ProviderKind::Zai)], Some("glm"));
        let r = registry(settings, &[("glm", "k")], env_vars());
        match r.select(None) {
            Selection::Provider { id, name, config } => {
                assert_eq!((id.as_str(), name.as_str()), ("glm", "z.ai GLM"));
                assert_eq!(config.base_url, LlmConfig::DEFAULT_BASE_URL);
                assert_eq!(config.model, "glm-5");
                assert_eq!(config.api_key, "k");
            }
            other => panic!("glm should be selected: {other:?}"),
        }
        assert_eq!(r.default_provider_id(), Some("glm".into()));
    }

    #[test]
    fn default_without_key_degrades_with_a_note() {
        let settings = settings_with(vec![profile("glm", ProviderKind::Zai)], Some("glm"));
        let r = registry(settings, &[], &[]);
        match r.select(None) {
            Selection::Local { reason } => {
                assert!(reason.contains("no API key"), "got: {reason}");
            }
            other => panic!("should degrade to local: {other:?}"),
        }
        let status = r.status();
        assert!(!status.enabled);
        // The menu still lists glm, marked unavailable.
        assert!(
            !status
                .providers
                .iter()
                .find(|i| i.id == "glm")
                .unwrap()
                .available
        );
    }

    #[test]
    fn invalid_settings_are_ignored_at_load_with_the_error_surfaced() {
        let mut settings = settings_with(vec![profile("glm", ProviderKind::Zai)], Some("glm"));
        settings.providers[0].base_url = "http://attacker.example/api".into();
        let r = registry(settings, &[("glm", "stored-zai-key")], env_vars());
        // The whole file is refused: defaults apply, the validation error is
        // surfaced, and the stored key is unreachable — nothing selects glm.
        let view = SettingsView::from(&r);
        assert!(
            view.settings_error.as_deref().unwrap().contains("z.ai"),
            "got: {:?}",
            view.settings_error
        );
        assert!(view.providers.is_empty());
        match r.select(Some("glm")) {
            Selection::Local { reason } => assert!(reason.contains("unknown provider")),
            other => panic!("glm must be unreachable: {other:?}"),
        }
        match r.select(None) {
            Selection::Provider { id, config, .. } => {
                assert_eq!(id, ENV_ID);
                assert_eq!(config.api_key, "env-key");
                assert_ne!(config.api_key, "stored-zai-key");
            }
            other => panic!("env should be the fallback: {other:?}"),
        }
        assert_eq!(r.default_provider_id(), Some(ENV_ID.into()));
        let status = r.status();
        assert!(status.enabled);
        assert!(!json_contains(&view, "stored-zai-key"));
    }

    fn json_contains(view: &SettingsView, needle: &str) -> bool {
        serde_json::to_string(view)
            .map(|s| s.contains(needle))
            .unwrap_or(false)
    }

    #[test]
    fn explicit_selection_and_local_mode() {
        let settings = settings_with(
            vec![
                profile("glm", ProviderKind::Zai),
                profile("mine", ProviderKind::Custom),
            ],
            None,
        );
        let r = registry(settings, &[("glm", "k")], &[]);
        match r.select(Some("mine")) {
            Selection::Local { reason } => assert!(reason.contains("no API key")),
            other => panic!("mine has no key: {other:?}"),
        }
        match r.select(Some("glm")) {
            Selection::Provider { config, .. } => assert_eq!(config.api_key, "k"),
            other => panic!("glm should resolve: {other:?}"),
        }
        match r.select(Some(LOCAL_ID)) {
            Selection::Local { reason } => assert!(reason.contains("local mode")),
            other => panic!("local should resolve: {other:?}"),
        }
        match r.select(Some("nope")) {
            Selection::Local { reason } => assert!(reason.contains("unknown provider")),
            other => panic!("unknown should degrade: {other:?}"),
        }
        match r.select(Some(ENV_ID)) {
            Selection::Local { reason } => assert!(reason.contains("not configured")),
            other => panic!("env unconfigured should degrade: {other:?}"),
        }
    }

    #[test]
    fn select_env_uses_the_env_config() {
        let r = registry(Settings::default(), &[], env_vars());
        match r.select(Some(ENV_ID)) {
            Selection::Provider { id, config, .. } => {
                assert_eq!(id, ENV_ID);
                assert_eq!(config.model, "env-model");
            }
            other => panic!("env should resolve: {other:?}"),
        }
    }

    #[test]
    fn load_reports_env_and_settings_errors() {
        // The settings error comes in as before; the env error now arises
        // from the (injected) process env itself — a key without the base
        // URL it must be paired with violates the unchanged pairing policy.
        let env_map: HashMap<String, String> = [("ANTHROPIC_API_KEY".to_string(), "k".to_string())]
            .into_iter()
            .collect();
        let r = LlmRegistry::load(
            Err(vault_core::Error::Settings("bad file".into())),
            Box::new(move |n| env_map.get(n).cloned()),
            |_| None,
        );
        let view = SettingsView::from(&r);
        assert_eq!(
            view.settings_error.as_deref(),
            Some("settings error: bad file")
        );
        assert!(view
            .env_error
            .as_deref()
            .unwrap()
            .contains("ANTHROPIC_BASE_URL"));
        assert_eq!(view.providers.len(), 0);
        assert_eq!(view.theme, Theme::System);
        assert!(view.notifications);
    }

    #[test]
    fn view_carries_notifications_and_old_payloads_default_enabled() {
        let mut settings = Settings::default();
        settings.options.notifications = false;
        let r = registry(settings, &[], &[]);
        assert!(!SettingsView::from(&r).notifications);
        // A payload from a frontend older than the option (field absent)
        // parses as the enabled default, never a silent opt-out.
        let payload: SaveSettingsPayload = serde_json::from_str(
            r#"{"providers":[],"default_provider":null,"theme":"system","summarize_top":8,"api_keys":[]}"#, 
        )
        .unwrap();
        assert!(payload.notifications);
        let explicit: SaveSettingsPayload = serde_json::from_str(
            r#"{"providers":[],"default_provider":null,"theme":"system","summarize_top":8,"notifications":false,"api_keys":[]}"#, 
        )
        .unwrap();
        assert!(!explicit.notifications);
    }

    #[test]
    fn view_carries_keepalive_and_old_payloads_default_off() {
        // M7: the view surfaces both keep-alive options, and payloads from
        // frontends older than M7 (fields absent) parse as disabled —
        // keep-alive is strictly opt-in, so an old payload can never
        // silently keep the process running or register autostart.
        let mut settings = Settings::default();
        settings.options.keep_running_in_background = true;
        settings.options.start_at_login = true;
        let r = registry(settings, &[], &[]);
        let view = SettingsView::from(&r);
        assert!(view.keep_running_in_background);
        assert!(view.start_at_login);
        // Pre-M7 payload shape (no keep-alive fields, notifications
        // explicit) → both options parse disabled.
        let older: SaveSettingsPayload = serde_json::from_str(
            r#"{"providers":[],"default_provider":null,"theme":"system","summarize_top":8,"notifications":true,"api_keys":[]}"#,
        )
        .unwrap();
        assert!(!older.keep_running_in_background);
        assert!(!older.start_at_login);
        // An explicit opt-in payload round-trips both fields.
        let explicit: SaveSettingsPayload = serde_json::from_str(
            r#"{"providers":[],"default_provider":null,"theme":"system","summarize_top":8,"keep_running_in_background":true,"start_at_login":true,"api_keys":[]}"#,
        )
        .unwrap();
        assert!(explicit.keep_running_in_background);
        assert!(explicit.start_at_login);
    }

    #[test]
    fn update_swaps_settings_and_keys() {
        let mut r = registry(Settings::default(), &[], &[]);
        let settings = settings_with(vec![profile("glm", ProviderKind::Zai)], Some("glm"));
        r.update(settings, Some(SecretStore::Memory), |id| {
            (id == "glm").then(|| "fresh".to_string())
        });
        match r.select(None) {
            Selection::Provider { config, .. } => assert_eq!(config.api_key, "fresh"),
            other => panic!("glm should resolve after update: {other:?}"),
        }
        assert_eq!(SettingsView::from(&r).key_store, Some(SecretStore::Memory));
    }

    #[test]
    fn drop_key_makes_provider_unavailable() {
        let settings = settings_with(vec![profile("glm", ProviderKind::Zai)], None);
        let mut r = registry(settings, &[("glm", "k")], &[]);
        assert!(
            r.status()
                .providers
                .iter()
                .find(|i| i.id == "glm")
                .unwrap()
                .available
        );
        r.drop_key("glm");
        assert!(
            !r.status()
                .providers
                .iter()
                .find(|i| i.id == "glm")
                .unwrap()
                .available
        );
    }

    #[test]
    fn view_never_contains_key_material() {
        let settings = settings_with(vec![profile("glm", ProviderKind::Zai)], None);
        let r = registry(settings, &[("glm", "super-secret-key")], env_vars());
        let json = serde_json::to_string(&SettingsView::from(&r)).unwrap();
        assert!(!json.contains("super-secret-key"));
        assert!(!json.contains("env-key"));
        assert!(json.contains("\"has_key\":true"));
    }

    #[test]
    fn env_overrides_layer_over_the_process_environment() {
        let mut settings = Settings::default();
        settings.env_overrides = vault_core::settings::EnvOverrides {
            anthropic_base_url: "http://127.0.0.1:2/api".into(),
            llm_model: "override-model".into(),
        };
        // Keyring override beats the process env; process env fills the
        // rest. (The key must be the Anthropic one: the unchanged pairing
        // policy refuses a ZAI key paired with an explicit non-z.ai URL.)
        let r = registry(
            settings,
            &[("env:ANTHROPIC_API_KEY", "stored-anthropic")],
            &[
                ("ANTHROPIC_API_KEY", "process-anthropic"),
                ("MAILCHAT_LLM_MODEL", "process-model"),
            ],
        );
        match r.select(Some(ENV_ID)) {
            Selection::Provider { config, .. } => {
                assert_eq!(config.api_key, "stored-anthropic");
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
        settings.env_overrides.anthropic_base_url = "https://api.z.ai/api/anthropic".into(); // z.ai host + Anthropic key
        let r = registry(settings, &[("env:ANTHROPIC_API_KEY", "k")], &[]);
        let view = SettingsView::from(&r);
        assert!(view.env.is_none());
        assert!(
            view.env_error
                .as_deref()
                .unwrap()
                .contains("ANTHROPIC_API_KEY"),
            "got: {:?}",
            view.env_error
        );
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
            &[
                ("env:ZAI_API_KEY", "secret-zai"),
                ("env:ANTHROPIC_API_KEY", "secret-anthropic"),
            ],
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
        assert_eq!(
            (
                config.api_key.as_str(),
                config.model.as_str(),
                config.base_url.as_str()
            ),
            ("typed-zai", "typed-model", LlmConfig::DEFAULT_BASE_URL)
        );
        // Empty form fields fall back to stored override + keyring.
        let payload = LlmTestPayload::defaulting(ENV_ID);
        let config = r.test_config(&payload).unwrap();
        assert_eq!(
            (config.api_key.as_str(), config.model.as_str()),
            ("stored-zai", "stored-model")
        );
    }

    #[test]
    fn test_config_env_keys_fall_back_to_the_process_environment() {
        // No stored override: the process-env key is what a Test click
        // probes, mirroring runtime resolution (keyring override →
        // settings-file override → process environment).
        let r = registry(Settings::default(), &[], &[("ZAI_API_KEY", "process-zai")]);
        let config = r.test_config(&LlmTestPayload::defaulting(ENV_ID)).unwrap();
        assert_eq!(config.api_key, "process-zai");
    }

    #[test]
    fn test_config_env_target_errors_are_actionable() {
        // Nothing configured at all.
        let r = registry(Settings::default(), &[], &[]);
        let err = r
            .test_config(&LlmTestPayload::defaulting(ENV_ID))
            .unwrap_err();
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
        assert!(
            err.contains("refusing to send an Anthropic key"),
            "got: {err}"
        );
        // Plaintext http off loopback: refused before any network. (The
        // Anthropic key pairs legally with this non-z.ai URL, so the
        // refusal can only come from the plaintext-http guard.)
        let payload = LlmTestPayload {
            provider: ENV_ID.into(),
            base_url: "http://api.example.com".into(),
            model: String::new(),
            key: String::new(),
            zai_key: String::new(),
            anthropic_key: "k".into(),
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
        assert_eq!(
            (config.api_key.as_str(), config.model.as_str()),
            ("stored", "typed-model")
        );
        // Typed key works without a stored one (unsaved row).
        let r = registry(settings, &[], &[]);
        let payload = LlmTestPayload {
            key: "typed".into(),
            ..LlmTestPayload::defaulting("glm")
        };
        assert_eq!(r.test_config(&payload).unwrap().api_key, "typed");
        // No key anywhere: actionable error.
        let err = r
            .test_config(&LlmTestPayload::defaulting("glm"))
            .unwrap_err();
        assert!(err.contains("no API key"), "got: {err}");
        // Unknown provider.
        let err = r
            .test_config(&LlmTestPayload::defaulting("nope"))
            .unwrap_err();
        assert!(err.contains("unknown provider"), "got: {err}");
        // Plaintext http guard applies to profiles too.
        let payload = LlmTestPayload {
            provider: "glm".into(),
            base_url: "http://api.example.com".into(),
            key: "k".into(),
            ..LlmTestPayload::defaulting("glm")
        };
        assert!(r
            .test_config(&payload)
            .unwrap_err()
            .contains("plaintext http"));
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

    #[test]
    fn reload_keys_recomputes_the_env_fallback() {
        // A key reload alone must leave the derived env state consistent:
        // a freshly stored override becomes selectable, a removed one
        // stops being so.
        let mut r = registry(Settings::default(), &[], &[]);
        assert!(r.select(Some(ENV_ID)).matches_local_with_reason());
        r.reload_keys(|id| (id == ENV_ZAI_KEY_ID).then(|| "fresh".to_string()));
        match r.select(Some(ENV_ID)) {
            Selection::Provider { config, .. } => assert_eq!(config.api_key, "fresh"),
            other => panic!("env should resolve after reload: {other:?}"),
        }
        r.reload_keys(|_| None);
        assert!(r.select(Some(ENV_ID)).matches_local_with_reason());
    }
}

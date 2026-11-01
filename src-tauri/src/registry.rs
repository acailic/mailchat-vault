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
use vault_core::llm::{LlmConfig, LlmStatus};
use vault_core::settings::{
    build_menu, ProviderMenuItem, ProviderProfile, Settings, Theme, ENV_ID, LOCAL_ID,
};

use crate::secrets::SecretStore;

/// The pseudo-provider name shown for the environment fallback.
const ENV_NAME: &str = "Environment variables";

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
    /// Valid env-var configuration, if any (never contains a profile key).
    env: Option<LlmConfig>,
    /// Why the env configuration was refused (pairing policy), if it was.
    env_error: Option<String>,
    /// Why the settings file could not be loaded, if it could not.
    settings_error: Option<String>,
    /// Where API keys were last saved (for the Settings UI note).
    key_store: Option<SecretStore>,
}

impl LlmRegistry {
    /// Build from the load-time pieces: settings file contents (or the
    /// error that prevented loading), the env-var configuration (or its
    /// error), and a loader for stored profile keys.
    pub fn load(
        settings: vault_core::Result<Option<Settings>>,
        env: vault_core::Result<Option<LlmConfig>>,
        load_key: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let (settings, settings_error) = match settings {
            Ok(settings) => (settings.unwrap_or_default(), None),
            Err(e) => (Settings::default(), Some(e.to_string())),
        };
        let (env, env_error) = match env {
            Ok(env) => (env, None),
            Err(e) => (None, Some(e.to_string())),
        };
        let mut registry = Self {
            settings,
            keys: HashMap::new(),
            env,
            env_error,
            settings_error,
            key_store: None,
        };
        registry.reload_keys(load_key);
        registry
    }

    /// Re-read the stored key of every configured profile.
    pub fn reload_keys(&mut self, load_key: impl Fn(&str) -> Option<String>) {
        self.keys = self
            .settings
            .providers
            .iter()
            .filter_map(|p| load_key(&p.id).map(|k| (p.id.clone(), k)))
            .collect();
    }

    /// The settings currently in effect.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Replace the settings (after a successful save) and re-read keys.
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

    /// Forget a single profile's key at runtime (after a keyring clear).
    pub fn drop_key(&mut self, provider_id: &str) {
        self.keys.remove(provider_id);
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

    /// The implicit selection: settings default → env fallback → local.
    fn select_default(&self) -> Selection {
        if let Some(default) = &self.settings.default_provider {
            let profile = self
                .settings
                .providers
                .iter()
                .find(|p| &p.id == default)
                .expect("validated: default references a configured provider");
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
        match &self.env {
            Some(env) => Selection::Provider {
                id: ENV_ID.into(),
                name: ENV_NAME.into(),
                config: env.clone(),
            },
            None => Selection::local(String::new()),
        }
    }

    /// Id of the default provider for the menu preselection: the settings
    /// default, else `env` when the environment fallback is usable, else
    /// `None` (nothing configured → the UI preselects local-only).
    pub fn default_provider_id(&self) -> Option<String> {
        self.settings
            .default_provider
            .clone()
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
// IPC payloads (settings_get / settings_save / llm_key_clear)
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

/// Response of `settings_get` (and of a successful `settings_save`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingsView {
    pub providers: Vec<ProviderView>,
    pub default_provider: Option<String>,
    pub theme: Theme,
    pub summarize_top: u32,
    /// The environment-fallback provider, when validly configured.
    pub env: Option<EnvProviderView>,
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
    pub api_keys: Vec<ApiKeyUpdate>,
}

/// One API-key update inside [`SaveSettingsPayload`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ApiKeyUpdate {
    pub id: String,
    /// Non-empty = store; empty = clear.
    pub key: String,
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
            env: registry.env.as_ref().map(|e| EnvProviderView {
                model: e.model.clone(),
                host: e.host(),
            }),
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

    fn env_config() -> LlmConfig {
        LlmConfig {
            base_url: "http://127.0.0.1:1/api".into(),
            api_key: "env-key".into(),
            model: "env-model".into(),
        }
    }

    fn registry(settings: Settings, keys: &[(&str, &str)], env: Option<LlmConfig>) -> LlmRegistry {
        let key_map: HashMap<String, String> = keys
            .iter()
            .map(|(id, k)| (id.to_string(), k.to_string()))
            .collect();
        LlmRegistry::load(Ok(Some(settings)), Ok(env), |id| key_map.get(id).cloned())
    }

    #[test]
    fn nothing_configured_degrades_to_local() {
        let r = registry(Settings::default(), &[], None);
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
        let r = registry(Settings::default(), &[], Some(env_config()));
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
        let r = registry(settings, &[("glm", "k")], Some(env_config()));
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
        let r = registry(settings, &[], None);
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
    fn explicit_selection_and_local_mode() {
        let settings = settings_with(
            vec![
                profile("glm", ProviderKind::Zai),
                profile("mine", ProviderKind::Custom),
            ],
            None,
        );
        let r = registry(settings, &[("glm", "k")], None);
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
        let r = registry(Settings::default(), &[], Some(env_config()));
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
        let r = LlmRegistry::load(
            Err(vault_core::Error::Settings("bad file".into())),
            Err(vault_core::Error::Llm(
                "ANTHROPIC_API_KEY is set but ANTHROPIC_BASE_URL is not".into(),
            )),
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
    }

    #[test]
    fn update_swaps_settings_and_keys() {
        let mut r = registry(Settings::default(), &[], None);
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
        let mut r = registry(settings, &[("glm", "k")], None);
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
        let r = registry(settings, &[("glm", "super-secret-key")], Some(env_config()));
        let json = serde_json::to_string(&SettingsView::from(&r)).unwrap();
        assert!(!json.contains("super-secret-key"));
        assert!(!json.contains("env-key"));
        assert!(json.contains("\"has_key\":true"));
    }
}

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
//! fixes the key/endpoint pairing rules. Kinds are data — id, display
//! name, wire protocol, auth style, and per-provider defaults (see
//! [`ProviderKind::defaults`]) — not separate modules:
//!
//! - `zai` — the z.ai GLM endpoint (the default provider). An explicit
//!   base URL must point at the z.ai host (any spelling); the model
//!   defaults to `glm-5`. Native `/v1/messages` wire.
//! - `anthropic` — Anthropic keys are only ever sent to an explicitly
//!   configured endpoint, never the z.ai host; both base URL and model are
//!   explicit. Native `/v1/messages` wire.
//! - `openai`, `gemini`, `azure_openai`, `bedrock`, `mistral`, `groq`,
//!   `openrouter`, `ollama`, `lmstudio` — the catalog kinds, each with
//!   versioned defaults (model, base URL, temperature, max tokens,
//!   timeout) in this module. All except `anthropic`/`zai` speak an
//!   OpenAI-compatible chat-completions wire ([`crate::llm::OpenAiLlm`]).
//!   `azure_openai` adds a deployment name + api-version; `bedrock` adds
//!   a region. `ollama` and `lmstudio` are the keyless local kinds
//!   ([`AuthStyle::None`]).
//! - `custom` — any OpenAI-compatible `/chat/completions` endpoint, with
//!   an explicit base URL and model.
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
    /// OpenAI (chat completions).
    Openai,
    /// Google Gemini (its OpenAI-compatible endpoint).
    Gemini,
    /// Azure OpenAI — deployment name + api-version, resource-specific
    /// base URL.
    AzureOpenai,
    /// AWS Bedrock — region-scoped.
    Bedrock,
    /// Mistral (chat completions).
    Mistral,
    /// Groq (chat completions).
    Groq,
    /// OpenRouter (chat completions).
    Openrouter,
    /// A local Ollama server (chat completions, no key required).
    Ollama,
    /// A local LM Studio server (chat completions, no key required).
    Lmstudio,
    /// Any OpenAI-compatible endpoint, fully explicit.
    Custom,
}

/// Every provider kind, in menu order (see [`ProviderKind::defaults`]
/// for the per-kind data).
pub const ALL_KINDS: [ProviderKind; 12] = [
    ProviderKind::Zai,
    ProviderKind::Anthropic,
    ProviderKind::Openai,
    ProviderKind::Gemini,
    ProviderKind::AzureOpenai,
    ProviderKind::Bedrock,
    ProviderKind::Mistral,
    ProviderKind::Groq,
    ProviderKind::Openrouter,
    ProviderKind::Ollama,
    ProviderKind::Lmstudio,
    ProviderKind::Custom,
];

/// Default [`ProviderProfile::api_version`] for `azure_openai` (pinned
/// Azure OpenAI api-version; a versionable constant).
pub const DEFAULT_AZURE_API_VERSION: &str = "2024-10-21";

/// Default [`ProviderProfile::region`] for `bedrock`.
pub const DEFAULT_BEDROCK_REGION: &str = "us-east-1";

/// Base URL of the Bedrock runtime in `region`
/// (`https://bedrock-runtime.<region>.amazonaws.com`).
pub fn bedrock_base_url(region: &str) -> String {
    format!("https://bedrock-runtime.{region}.amazonaws.com")
}

/// Cross-region inference-profile prefix Bedrock serves a default model
/// through in `region`: `us.`/`eu.`/`apac.` for the US, Europe and
/// Asia-Pacific region families, empty elsewhere (regions without a
/// cross-region profile keep the unprefixed, region-local model id).
fn bedrock_model_prefix(region: &str) -> &'static str {
    if region.starts_with("us") {
        "us."
    } else if region.starts_with("eu") {
        "eu."
    } else if region.starts_with("ap") {
        "apac."
    } else {
        ""
    }
}

/// How a kind authenticates (data for the client path; the API key
/// itself lives in the OS keyring, never in settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStyle {
    /// `Authorization: Bearer <key>` (OpenAI-compatible providers).
    Bearer,
    /// `x-api-key` + `anthropic-version` headers (native Anthropic wire).
    XApiKey,
    /// No key (local Ollama / LM Studio; a key may still be configured).
    None,
    /// AWS SigV4 (Bedrock; region + keyring credentials).
    AwsSigV4,
}

/// The request wire a kind speaks (which client path serves it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wire {
    /// Native Anthropic-compatible `/v1/messages`
    /// ([`crate::llm::AnthropicLlm`]; z.ai and Anthropic).
    AnthropicMessages,
    /// OpenAI-compatible `/chat/completions`
    /// ([`crate::llm::OpenAiLlm`]).
    OpenAiChat,
    /// Bedrock's own invoke/converse wire (data only in this slice; the
    /// client path is a follow-up).
    AwsBedrock,
}

/// The versioned per-kind defaults (constants in vault-core, not UI
/// strings): what a profile of the kind uses when a field is left empty.
/// `azure_openai` and `custom` carry no default model/base URL — a
/// deployment/resource URL (azure) or an explicit endpoint (custom) is
/// definitionally user-supplied. `lmstudio` has the default endpoint but
/// no default model (the served model is user-loaded).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderDefaults {
    /// Default model id (empty = user-supplied; azure/custom/lmstudio).
    pub model: &'static str,
    /// Default endpoint base URL (empty = user-supplied; azure/custom).
    pub base_url: &'static str,
    /// Default sampling temperature.
    pub temperature: f64,
    /// Default `max_tokens` per request.
    pub max_tokens: u32,
    /// Default per-request timeout.
    pub timeout_secs: u64,
}

impl ProviderKind {
    /// Stable kind id string (also the menu/UI kind tag).
    pub fn id(self) -> &'static str {
        match self {
            Self::Zai => "zai",
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
            Self::Gemini => "gemini",
            Self::AzureOpenai => "azure_openai",
            Self::Bedrock => "bedrock",
            Self::Mistral => "mistral",
            Self::Groq => "groq",
            Self::Openrouter => "openrouter",
            Self::Ollama => "ollama",
            Self::Lmstudio => "lmstudio",
            Self::Custom => "custom",
        }
    }

    /// Human-facing label (also the default provider name).
    pub fn label(self) -> &'static str {
        match self {
            Self::Zai => "z.ai GLM",
            Self::Anthropic => "Anthropic",
            Self::Openai => "OpenAI",
            Self::Gemini => "Google Gemini",
            Self::AzureOpenai => "Azure OpenAI",
            Self::Bedrock => "AWS Bedrock",
            Self::Mistral => "Mistral",
            Self::Groq => "Groq",
            Self::Openrouter => "OpenRouter",
            Self::Ollama => "Ollama",
            Self::Lmstudio => "LM Studio",
            Self::Custom => "Custom",
        }
    }

    /// How this kind authenticates (see [`AuthStyle`]).
    pub fn auth(self) -> AuthStyle {
        match self {
            Self::Zai | Self::Anthropic => AuthStyle::XApiKey,
            Self::Ollama | Self::Lmstudio => AuthStyle::None,
            Self::Bedrock => AuthStyle::AwsSigV4,
            Self::Openai
            | Self::Gemini
            | Self::AzureOpenai
            | Self::Mistral
            | Self::Groq
            | Self::Openrouter
            | Self::Custom => AuthStyle::Bearer,
        }
    }

    /// Which request wire this kind speaks (see [`Wire`]).
    pub fn wire(self) -> Wire {
        match self {
            Self::Zai | Self::Anthropic => Wire::AnthropicMessages,
            Self::Bedrock => Wire::AwsBedrock,
            Self::Openai
            | Self::Gemini
            | Self::AzureOpenai
            | Self::Mistral
            | Self::Groq
            | Self::Openrouter
            | Self::Ollama
            | Self::Lmstudio
            | Self::Custom => Wire::OpenAiChat,
        }
    }

    /// The versioned defaults for profiles of this kind.
    pub fn defaults(self) -> ProviderDefaults {
        // Shared sampling defaults: low temperature for deterministic
        // RAG answers, the pipeline's max-tokens cap, the standard
        // request timeout (local Ollama inference gets a longer leash).
        const T: f64 = crate::llm::DEFAULT_SAMPLING_TEMPERATURE;
        const MAX_TOKENS: u32 = crate::llm::DEFAULT_MAX_TOKENS;
        const TIMEOUT: u64 = 30;
        match self {
            Self::Zai => ProviderDefaults {
                model: LlmConfig::DEFAULT_MODEL,
                base_url: LlmConfig::DEFAULT_BASE_URL,
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: TIMEOUT,
            },
            Self::Anthropic => ProviderDefaults {
                model: "claude-sonnet-4-5",
                base_url: "https://api.anthropic.com",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: TIMEOUT,
            },
            Self::Openai => ProviderDefaults {
                model: "gpt-4o-mini",
                base_url: "https://api.openai.com/v1",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: TIMEOUT,
            },
            Self::Gemini => ProviderDefaults {
                model: "gemini-2.0-flash",
                base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: TIMEOUT,
            },
            Self::AzureOpenai => ProviderDefaults {
                // Deployment name and resource URL are user-supplied.
                model: "",
                base_url: "",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: TIMEOUT,
            },
            Self::Bedrock => ProviderDefaults {
                model: "anthropic.claude-sonnet-4-5",
                // Effective default derived from the region
                // ([`bedrock_base_url`]); the constant itself is empty.
                base_url: "",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: TIMEOUT,
            },
            Self::Mistral => ProviderDefaults {
                model: "mistral-large-latest",
                base_url: "https://api.mistral.ai/v1",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: TIMEOUT,
            },
            Self::Groq => ProviderDefaults {
                model: "llama-3.3-70b-versatile",
                base_url: "https://api.groq.com/openai/v1",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: TIMEOUT,
            },
            Self::Openrouter => ProviderDefaults {
                model: "openrouter/auto",
                base_url: "https://openrouter.ai/api/v1",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: TIMEOUT,
            },
            Self::Ollama => ProviderDefaults {
                model: "llama3.2",
                base_url: "http://127.0.0.1:11434/v1",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: 120,
            },
            Self::Lmstudio => ProviderDefaults {
                // The served model is user-loaded in LM Studio, so there
                // is no sensible default model id — only the endpoint.
                model: "",
                base_url: "http://127.0.0.1:1234/v1",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: 120,
            },
            Self::Custom => ProviderDefaults {
                // A custom endpoint is fully explicit by definition.
                model: "",
                base_url: "",
                temperature: T,
                max_tokens: MAX_TOKENS,
                timeout_secs: TIMEOUT,
            },
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
    /// Keep the app running in the background when the window is closed
    /// (M7: window close hides to the tray instead of exiting). Strictly
    /// opt-in, so older settings files without the field default to
    /// **disabled** — closing must keep its pre-M7 meaning (exit) until
    /// the user asks for keep-alive (unlike `notifications`, which
    /// backfills to enabled because it merely continues the documented
    /// pre-Settings behavior).
    #[serde(default)]
    pub keep_running_in_background: bool,
    /// Start the app automatically when the user logs in (M7: OS
    /// login-item registration, applied when Settings are saved).
    /// Strictly opt-in; older files default to disabled.
    #[serde(default)]
    pub start_at_login: bool,
    /// Improve semantic search with a generated draft (HyDE, Gao et
    /// al. ACL 2023): when a consented provider backend exists, one
    /// extra provider call writes a short hypothetical answer to the
    /// question and that text — not the raw question — is embedded as
    /// the dense query vector. Strictly opt-in (it sends the question
    /// to the provider, the same data translate already sends); older
    /// settings files without the field default to **disabled**.
    #[serde(default)]
    pub semantic_draft: bool,
    /// Transcribe audio attachments (voice notes) locally with an
    /// optional whisper.cpp sidecar so archive search matches spoken
    /// words ([`crate::transcription`]). Strictly opt-in — transcription
    /// is offline CPU work that must never start on its own — so older
    /// settings files without the field backfill to **disabled**, and
    /// while it is off nothing is enqueued or transcribed.
    #[serde(default)]
    pub transcribe_audio: bool,
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
            keep_running_in_background: false,
            start_at_login: false,
            semantic_draft: false,
            transcribe_audio: false,
        }
    }
}

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

/// One configured LLM provider profile. Carries no key material: the API
/// key lives in the OS keyring under the profile `id`.
///
/// The sampling and provider-extra fields are additive and optional
/// (`#[serde(default)]`, skipped when `None`): settings files written by
/// older builds load unchanged, and files written with them stay
/// loadable by older builds (serde ignores unknown fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderProfile {
    /// Stable opaque id (also the keyring entry name). Assigned once when
    /// the provider is added and never regenerated.
    pub id: String,
    /// Display name; empty means the kind label.
    pub name: String,
    /// Which pairing rules apply.
    pub kind: ProviderKind,
    /// Endpoint base URL (without `/v1/messages` or `/chat/completions`).
    /// Empty = the kind's default.
    pub base_url: String,
    /// Model name. Empty = the kind's default.
    pub model: String,
    /// Sampling temperature override; `None` = the kind default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// `max_tokens` override; `None` = the kind default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Per-request timeout override (seconds); `None` = the kind default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    /// `azure_openai` only: the deployment name (the deployed model).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_name: Option<String>,
    /// `azure_openai` only: the api-version query parameter.
    /// `None` = [`DEFAULT_AZURE_API_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_version: Option<String>,
    /// `bedrock` only: the AWS region. `None` =
    /// [`DEFAULT_BEDROCK_REGION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
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
        if !self.base_url.trim().is_empty() {
            return self.base_url.trim().to_string();
        }
        match self.kind {
            // Bedrock's endpoint is derived from the region.
            ProviderKind::Bedrock => bedrock_base_url(&self.effective_region()),
            _ => self.kind.defaults().base_url.to_string(),
        }
    }

    /// Effective model name after kind defaults are applied.
    pub fn effective_model(&self) -> String {
        if !self.model.trim().is_empty() {
            self.model.trim().to_string()
        } else {
            match self.kind {
                // On Azure the deployment name is the model id.
                ProviderKind::AzureOpenai => self
                    .deployment_name
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_string(),
                // On Bedrock the default model rides the region's
                // cross-region inference profile (`us.`/`eu.`/`apac.` —
                // see [`bedrock_model_prefix`]); `defaults()` keeps the
                // unprefixed constant and the prefix is applied per
                // profile. An explicit model still wins (the match arm
                // above).
                ProviderKind::Bedrock => format!(
                    "{}{}",
                    bedrock_model_prefix(&self.effective_region()),
                    self.kind.defaults().model
                ),
                _ => self.kind.defaults().model.to_string(),
            }
        }
    }

    /// Effective sampling temperature (override or kind default).
    pub fn effective_temperature(&self) -> f64 {
        self.temperature
            .unwrap_or_else(|| self.kind.defaults().temperature)
    }

    /// Effective `max_tokens` (override or kind default).
    pub fn effective_max_tokens(&self) -> u32 {
        self.max_tokens
            .unwrap_or_else(|| self.kind.defaults().max_tokens)
    }

    /// Effective per-request timeout (override or kind default).
    pub fn effective_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.timeout_secs
                .unwrap_or_else(|| self.kind.defaults().timeout_secs),
        )
    }

    /// Effective Azure api-version (override or the pinned default).
    pub fn effective_api_version(&self) -> String {
        self.api_version
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_AZURE_API_VERSION)
            .to_string()
    }

    /// Effective Bedrock region (override or the default).
    pub fn effective_region(&self) -> String {
        self.region
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_BEDROCK_REGION)
            .to_string()
    }

    /// Endpoint host for display (e.g. `api.z.ai`).
    pub fn host(&self) -> String {
        endpoint_host(&self.effective_base_url()).unwrap_or_else(|| self.effective_base_url())
    }
}

/// The whole settings file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Environment-variable overrides (non-secret; see [`EnvOverrides`]).
    #[serde(default)]
    pub env_overrides: EnvOverrides,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            providers: Vec::new(),
            default_provider: None,
            options: Options::default(),
            env_overrides: EnvOverrides::default(),
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
pub(crate) fn is_loopback_url(url: &str) -> bool {
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
/// host: a stored API key must never travel unencrypted. `label` names the
/// offender in the message (e.g. `provider “Work GLM”`).
fn check_no_plaintext_http(label: &str, base_url: &str) -> crate::Result<()> {
    let plaintext = Url::parse(base_url)
        .map(|u| u.scheme() == "http")
        .unwrap_or(false);
    if plaintext && !is_loopback_url(base_url) {
        return Err(settings_error(format!(
            "{label}: the API key must not travel over plaintext http — use https for \
             {base_url} (or a localhost/127.0.0.1 endpoint for testing)"
        )));
    }
    Ok(())
}

/// The z.ai host, for pairing checks (mirrors the env policy).
fn zai_host() -> String {
    endpoint_host(LlmConfig::DEFAULT_BASE_URL).expect("default base URL parses")
}

/// The per-kind pairing rules for one provider profile: a key is never
/// sent to a provider it does not belong to — a z.ai key only to the
/// z.ai host, an Anthropic key never there and only to an explicit
/// endpoint, a custom endpoint fully explicit — plus URL shape and the
/// plaintext-http refusal. This is exactly the `kind` match inside
/// [`Settings::validate`], factored out so other call sites (e.g. the
/// "Test connection" probe, which composes typed form values over the
/// stored ones) enforce the same rules a save would enforce.
pub fn check_profile_pairing(profile: &ProviderProfile) -> crate::Result<()> {
    let name = profile.display_name();
    match profile.kind {
        ProviderKind::Zai => {
            if !profile.base_url.is_empty() {
                if !is_http_url(&profile.base_url) {
                    return Err(settings_error(format!(
                        "provider “{name}”: base URL must be an absolute http(s) URL"
                    )));
                }
                if endpoint_host(&profile.base_url).as_deref() != Some(zai_host().as_str()) {
                    return Err(settings_error(format!(
                        "provider “{name}”: a z.ai key is only ever sent to the z.ai \
                         endpoint, but the base URL points at {}. Remove the base URL to \
                         use the default {}",
                        profile.host(),
                        LlmConfig::DEFAULT_BASE_URL
                    )));
                }
                check_no_plaintext_http(&format!("provider “{name}”"), &profile.base_url)?;
            }
        }
        ProviderKind::Anthropic => {
            if profile.base_url.is_empty() {
                return Err(settings_error(format!(
                    "provider “{name}”: an Anthropic key is only sent to an explicitly \
                     configured endpoint — set a base URL (e.g. https://api.anthropic.com)"
                )));
            }
            if !is_http_url(&profile.base_url) {
                return Err(settings_error(format!(
                    "provider “{name}”: base URL must be an absolute http(s) URL"
                )));
            }
            if endpoint_host(&profile.base_url) == Some(zai_host()) {
                return Err(settings_error(format!(
                    "provider “{name}”: refusing to send an Anthropic key to the z.ai \
                     endpoint — point the base URL at the endpoint that accepts this key"
                )));
            }
            check_no_plaintext_http(&format!("provider “{name}”"), &profile.base_url)?;
            if profile.model.is_empty() {
                return Err(settings_error(format!(
                    "provider “{name}”: a model name is required for Anthropic providers"
                )));
            }
        }
        // The hosted OpenAI-compatible catalog kinds: base URL and model
        // are optional (kind defaults apply); an explicit base URL must
        // be a well-formed encrypted (or loopback) http(s) URL.
        ProviderKind::Openai
        | ProviderKind::Gemini
        | ProviderKind::Mistral
        | ProviderKind::Groq
        | ProviderKind::Openrouter
        | ProviderKind::Ollama
        | ProviderKind::Lmstudio => {
            if !profile.base_url.is_empty() {
                if !is_http_url(&profile.base_url) {
                    return Err(settings_error(format!(
                        "provider “{name}”: base URL must be an absolute http(s) URL"
                    )));
                }
                check_no_plaintext_http(&format!("provider “{name}”"), &profile.base_url)?;
            }
        }
        ProviderKind::AzureOpenai => {
            if profile.base_url.is_empty() {
                return Err(settings_error(format!(
                    "provider “{name}”: an Azure OpenAI provider needs the resource base URL \
                     (e.g. https://<resource>.openai.azure.com)"
                )));
            }
            if !is_http_url(&profile.base_url) {
                return Err(settings_error(format!(
                    "provider “{name}”: base URL must be an absolute http(s) URL"
                )));
            }
            check_no_plaintext_http(&format!("provider “{name}”"), &profile.base_url)?;
            if profile.deployment_name.as_deref().is_none_or(str::is_empty) {
                return Err(settings_error(format!(
                    "provider “{name}”: an Azure OpenAI provider needs a deployment name"
                )));
            }
        }
        ProviderKind::Bedrock => {
            if let Some(region) = profile.region.as_deref() {
                let region = region.trim();
                if !region.is_empty()
                    && !region
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                {
                    return Err(settings_error(format!(
                        "provider “{name}”: AWS region “{region}” is not a valid region slug"
                    )));
                }
            }
            if !profile.base_url.is_empty() {
                if !is_http_url(&profile.base_url) {
                    return Err(settings_error(format!(
                        "provider “{name}”: base URL must be an absolute http(s) URL"
                    )));
                }
                // Host pinning: Bedrock credentials (SigV4-signed for the
                // region) are only ever sent to the chosen region's
                // Bedrock runtime host — not another region's runtime,
                // an unrelated endpoint, or the z.ai host.
                let expected = bedrock_base_url(&profile.effective_region());
                if endpoint_host(&profile.base_url) != endpoint_host(&expected) {
                    return Err(settings_error(format!(
                        "provider “{name}”: Bedrock credentials are only ever sent to the \
                         region's Bedrock runtime endpoint, but the base URL points at {}. \
                         Remove the base URL to use the default {expected}",
                        profile.host()
                    )));
                }
                check_no_plaintext_http(&format!("provider “{name}”"), &profile.base_url)?;
            }
        }
        ProviderKind::Custom => {
            if profile.base_url.is_empty() {
                return Err(settings_error(format!(
                    "provider “{name}”: a custom provider needs a base URL"
                )));
            }
            if !is_http_url(&profile.base_url) {
                return Err(settings_error(format!(
                    "provider “{name}”: base URL must be an absolute http(s) URL"
                )));
            }
            check_no_plaintext_http(&format!("provider “{name}”"), &profile.base_url)?;
            if profile.model.is_empty() {
                return Err(settings_error(format!(
                    "provider “{name}”: a model name is required for custom providers"
                )));
            }
        }
    }
    Ok(())
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
            p.deployment_name = p.deployment_name.take().map(|v| v.trim().to_string());
            p.api_version = p.api_version.take().map(|v| v.trim().to_string());
            p.region = p.region.take().map(|v| v.trim().to_string());
        }
        if let Some(default) = &mut self.default_provider {
            let d = default.trim().to_string();
            if d.is_empty() {
                self.default_provider = None;
            } else {
                *default = d;
            }
        }
        self.env_overrides.anthropic_base_url = self
            .env_overrides
            .anthropic_base_url
            .trim()
            .trim_end_matches('/')
            .to_string();
        self.env_overrides.llm_model = self.env_overrides.llm_model.trim().to_string();
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
            check_profile_pairing(p)?;
        }
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
    /// `local`, `env`, or a provider kind id (`zai`, `anthropic`,
    /// `openai`, …; see [`ProviderKind::id`]).
    pub kind: String,
    /// Effective model (empty for `local`).
    pub model: String,
    /// Endpoint host (empty for `local`).
    pub host: String,
    /// Whether this entry can be selected right now (a keyless kind is
    /// always usable; keyed kinds need a stored API key).
    pub available: bool,
}

/// Build the provider menu: local-only mode first, then the configured
/// profiles (in settings order), then the environment fallback. `has_key`
/// reports whether a profile's API key is stored — keyless kinds
/// (`AuthStyle::None`: local Ollama / LM Studio) are available without
/// one; `env` is the validly configured env fallback (if any).
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
            kind: p.kind.id().to_string(),
            model: p.effective_model(),
            host: p.host(),
            available: p.kind.auth() == AuthStyle::None || has_key(&p.id),
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
            temperature: None,
            max_tokens: None,
            timeout_secs: None,
            deployment_name: None,
            api_version: None,
            region: None,
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
        // M7 keep-alive is strictly opt-in: the defaults (and therefore a
        // fresh install) never keep the process running or autostart it.
        assert!(!Settings::default().options.keep_running_in_background);
        assert!(!Settings::default().options.start_at_login);
        // Local voice-note transcription is strictly opt-in too.
        assert!(!Settings::default().options.transcribe_audio);
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
    fn keepalive_options_round_trip_and_backfill_off() {
        let dir = std::env::temp_dir().join(format!("mcv-settings-ka-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        // A file written before the M7 options existed: both fields are
        // absent and must backfill to OFF (strict opt-in) while the
        // notifications backfill stays enabled — one file, two rules.
        let older = serde_json::json!({
            "version": SETTINGS_VERSION,
            "providers": [],
            "default_provider": null,
            "options": {"theme": "system", "summarize_top": 8, "notifications": true}
        });
        fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
        let loaded = Settings::load(&path).unwrap().expect("file exists");
        assert!(loaded.options.notifications);
        assert!(!loaded.options.keep_running_in_background);
        assert!(!loaded.options.start_at_login);
        assert!(
            !loaded.options.transcribe_audio,
            "transcription must backfill off in older files"
        );
        // An explicit opt-in survives a save/load round trip, on disk and
        // back, together with an explicit notifications opt-out.
        let mut s = loaded;
        s.options.keep_running_in_background = true;
        s.options.start_at_login = true;
        s.options.transcribe_audio = true;
        s.options.notifications = false;
        s.save(&path).unwrap();
        let reloaded = Settings::load(&path).unwrap().unwrap();
        assert!(reloaded.options.keep_running_in_background);
        assert!(reloaded.options.start_at_login);
        assert!(reloaded.options.transcribe_audio);
        assert!(!reloaded.options.notifications);
        assert_eq!(reloaded, s);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn semantic_draft_option_round_trips_and_backfills_off() {
        assert!(
            !Settings::default().options.semantic_draft,
            "the HyDE draft is strictly opt-in: a fresh install never drafts"
        );
        let dir = std::env::temp_dir().join(format!("mcv-settings-hyde-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        // A file written before the option existed: the field is absent
        // and must backfill to OFF, not refuse to load.
        let older = serde_json::json!({
            "version": SETTINGS_VERSION,
            "providers": [],
            "default_provider": null,
            "options": {"theme": "system", "summarize_top": 8}
        });
        fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
        let loaded = Settings::load(&path).unwrap().expect("file exists");
        assert!(!loaded.options.semantic_draft);
        // An explicit opt-in survives a save/load round trip.
        let mut s = loaded;
        s.options.semantic_draft = true;
        s.save(&path).unwrap();
        assert!(
            Settings::load(&path)
                .unwrap()
                .unwrap()
                .options
                .semantic_draft
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
                temperature: None,
                max_tokens: None,
                timeout_secs: None,
                deployment_name: None,
                api_version: None,
                region: None,
            }],
            default_provider: Some(" ".into()),
            options: Options::default(),
            env_overrides: EnvOverrides::default(),
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

    #[test]
    fn menu_marks_keyless_kinds_available_without_a_key() {
        // A keyless local kind (ollama / lmstudio) is selectable with no
        // stored key — the server needs no auth; a keyed kind without a
        // key stays unavailable exactly as before.
        let s = settings_with(vec![
            profile("loc", ProviderKind::Ollama, "", ""),
            profile("studio", ProviderKind::Lmstudio, "", ""),
            profile("glm", ProviderKind::Zai, "", ""),
        ]);
        let menu = build_menu(&s, |_| false, None);
        assert!(menu[1].available, "ollama is keyless");
        assert!(menu[2].available, "lmstudio is keyless");
        assert!(!menu[3].available, "zai without a key is not");
        assert_eq!(menu[1].kind, "ollama");
        assert_eq!(menu[2].kind, "lmstudio");
        assert_eq!(menu[2].model, "", "lmstudio has no default model");
        // A stored key on a keyless kind changes nothing (still usable).
        let menu = build_menu(&s, |id| id == "studio", None);
        assert!(menu[1].available);
        assert!(menu[2].available);
    }

    #[test]
    fn lmstudio_round_trips_keyless_with_empty_model() {
        // The fully local preset: no key anywhere, no model id (the
        // served model is user-loaded in LM Studio) — the profile is
        // valid as-is and survives a save/load round trip unchanged.
        let dir = std::env::temp_dir().join(format!("mcv-settings-lms-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let mut s = settings_with(vec![profile("studio", ProviderKind::Lmstudio, "", "")]);
        s.default_provider = Some("studio".into());
        s.normalize();
        s.validate().unwrap();
        let p = &s.providers[0];
        assert_eq!(p.effective_base_url(), "http://127.0.0.1:1234/v1");
        assert_eq!(p.effective_model(), "");
        assert_eq!(p.effective_timeout(), std::time::Duration::from_secs(120));
        assert_eq!(p.kind.auth(), AuthStyle::None);
        s.save(&path).unwrap();
        let loaded = Settings::load(&path).unwrap().expect("file exists");
        assert_eq!(loaded, s, "the keyless lmstudio profile round-trips");
        // The file stays key-free.
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("api_key"));
        fs::remove_dir_all(&dir).ok();

        // An explicit non-loopback http override is still refused (the
        // plaintext-http guard applies to lmstudio too), and a malformed
        // URL is refused as a URL shape problem.
        let s = settings_with(vec![profile(
            "studio",
            ProviderKind::Lmstudio,
            "http://lm.example.com:1234/v1",
            "m",
        )]);
        assert!(s
            .validate()
            .unwrap_err()
            .to_string()
            .contains("plaintext http"));
        let s = settings_with(vec![profile(
            "studio",
            ProviderKind::Lmstudio,
            "not a url",
            "",
        )]);
        assert!(s
            .validate()
            .unwrap_err()
            .to_string()
            .contains("http(s) URL"));
    }

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
        assert_eq!(
            Settings::load(&path).unwrap().unwrap().env_overrides,
            s.env_overrides
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn env_overrides_normalize_trims() {
        let mut s = Settings::default();
        s.env_overrides.anthropic_base_url = " https://api.anthropic.com/ ".into();
        s.env_overrides.llm_model = " glm-5 ".into();
        s.normalize();
        assert_eq!(
            s.env_overrides.anthropic_base_url,
            "https://api.anthropic.com"
        );
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

    // ------------------------------------------------------------------
    // Provider catalog: kinds, defaults, sampling fields (AI providers
    // backend substrate)
    // ------------------------------------------------------------------

    #[test]
    fn registry_defaults_table_is_total() {
        // Every kind appears exactly once in the catalog, exposes its
        // data (id, label, auth style, wire), and has defaults with a
        // model, base URL, and timeout — except azure_openai and custom,
        // whose endpoint and model are definitionally user-supplied (a
        // resource/deployment URL and an explicit endpoint), bedrock,
        // whose base URL is derived from the region, and lmstudio,
        // whose model is user-loaded but whose endpoint is defaulted.
        assert_eq!(ALL_KINDS.len(), 12);
        let mut seen = std::collections::HashSet::new();
        for kind in ALL_KINDS {
            assert!(seen.insert(kind.id()), "duplicate kind id {}", kind.id());
            assert!(!kind.label().is_empty(), "{} needs a label", kind.id());
            let defaults = kind.defaults();
            assert!(
                defaults.max_tokens > 0,
                "{} needs a max-tokens default",
                kind.id()
            );
            assert!(
                defaults.timeout_secs > 0,
                "{} needs a timeout default",
                kind.id()
            );
            assert!(
                (0.0..=2.0).contains(&defaults.temperature),
                "{} has an out-of-range default temperature",
                kind.id()
            );
            match kind {
                ProviderKind::AzureOpenai | ProviderKind::Custom => {
                    assert_eq!(defaults.model, "", "{} model is user-supplied", kind.id());
                    assert_eq!(
                        defaults.base_url,
                        "",
                        "{} base URL is user-supplied",
                        kind.id()
                    );
                }
                ProviderKind::Lmstudio => {
                    // The served model is user-loaded in LM Studio: no
                    // default model id, but the endpoint is defaulted.
                    assert_eq!(defaults.model, "", "lmstudio model is user-supplied");
                    assert!(
                        Url::parse(defaults.base_url).is_ok(),
                        "lmstudio needs a parseable default base URL, got {:?}",
                        defaults.base_url
                    );
                }
                ProviderKind::Bedrock => {
                    assert!(!defaults.model.is_empty(), "bedrock needs a model default");
                    // The effective default base URL derives from the
                    // region template.
                    let empty = profile("b", ProviderKind::Bedrock, "", "");
                    assert_eq!(
                        empty.effective_base_url(),
                        bedrock_base_url(DEFAULT_BEDROCK_REGION)
                    );
                }
                _ => {
                    assert!(
                        !defaults.model.is_empty(),
                        "{} needs a model default",
                        kind.id()
                    );
                    assert!(
                        Url::parse(defaults.base_url).is_ok(),
                        "{} needs a parseable default base URL, got {:?}",
                        kind.id(),
                        defaults.base_url
                    );
                }
            }
        }
    }

    #[test]
    fn registry_kind_data_maps_wires_and_auth() {
        // Native Anthropic wire: z.ai + anthropic. OpenAI-compatible:
        // the catalog kinds + custom. Bedrock: its own (follow-up) wire.
        for kind in [ProviderKind::Zai, ProviderKind::Anthropic] {
            assert_eq!(kind.wire(), Wire::AnthropicMessages, "{kind:?}");
            assert_eq!(kind.auth(), AuthStyle::XApiKey, "{kind:?}");
        }
        for kind in [
            ProviderKind::Openai,
            ProviderKind::Gemini,
            ProviderKind::AzureOpenai,
            ProviderKind::Mistral,
            ProviderKind::Groq,
            ProviderKind::Openrouter,
            ProviderKind::Ollama,
            ProviderKind::Lmstudio,
            ProviderKind::Custom,
        ] {
            assert_eq!(kind.wire(), Wire::OpenAiChat, "{kind:?}");
        }
        assert_eq!(ProviderKind::Bedrock.wire(), Wire::AwsBedrock);
        assert_eq!(ProviderKind::Openai.auth(), AuthStyle::Bearer);
        assert_eq!(ProviderKind::Ollama.auth(), AuthStyle::None);
        assert_eq!(ProviderKind::Lmstudio.auth(), AuthStyle::None);
        assert_eq!(ProviderKind::Bedrock.auth(), AuthStyle::AwsSigV4);
        // Kind ids round-trip through serde (the settings file stores
        // them as the snake_case tag).
        for kind in ALL_KINDS {
            let text = serde_json::to_string(&kind).unwrap();
            let back: ProviderKind = serde_json::from_str(&text).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn sampling_fields_round_trip_two_providers_without_crosstalk() {
        let dir = std::env::temp_dir().join(format!("mcv-settings-smp-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        // Two providers of different kinds with distinct sampling
        // overrides and distinct base URLs: nothing of one may leak
        // into the other through a save + reopen.
        let mut s = Settings {
            providers: vec![
                ProviderProfile {
                    id: "openai-main".into(),
                    name: "OpenAI".into(),
                    kind: ProviderKind::Openai,
                    base_url: String::new(),
                    model: String::new(),
                    temperature: Some(0.7),
                    max_tokens: Some(512),
                    timeout_secs: Some(45),
                    deployment_name: None,
                    api_version: None,
                    region: None,
                },
                ProviderProfile {
                    id: "ollama-local".into(),
                    name: "Local Ollama".into(),
                    kind: ProviderKind::Ollama,
                    base_url: "http://127.0.0.1:11434/v1".into(),
                    model: "llama3.2".into(),
                    temperature: Some(0.0),
                    max_tokens: Some(2048),
                    timeout_secs: Some(300),
                    deployment_name: None,
                    api_version: None,
                    region: None,
                },
            ],
            default_provider: Some("ollama-local".into()),
            ..Settings::default()
        };
        s.normalize();
        s.validate().unwrap();
        s.save(&path).unwrap();
        let loaded = Settings::load(&path).unwrap().expect("file exists");
        assert_eq!(loaded, s, "the whole file round-trips");
        // Provider choice survives.
        assert_eq!(loaded.default_provider.as_deref(), Some("ollama-local"));
        // Credential references (keyring ids) survive.
        assert_eq!(
            loaded
                .providers
                .iter()
                .map(|p| p.id.as_str())
                .collect::<Vec<_>>(),
            ["openai-main", "ollama-local"]
        );
        // The three sampling fields survive per provider, without
        // crosstalk.
        let (openai, ollama) = (&loaded.providers[0], &loaded.providers[1]);
        assert_eq!(
            (openai.temperature, openai.max_tokens, openai.timeout_secs),
            (Some(0.7), Some(512), Some(45))
        );
        assert_eq!(
            (ollama.temperature, ollama.max_tokens, ollama.timeout_secs),
            (Some(0.0), Some(2048), Some(300))
        );
        // Effective values follow each profile's own overrides/kind.
        assert_eq!(openai.effective_temperature(), 0.7);
        assert_eq!(openai.effective_max_tokens(), 512);
        assert_eq!(
            openai.effective_timeout(),
            std::time::Duration::from_secs(45)
        );
        assert_eq!(ollama.effective_temperature(), 0.0);
        assert_eq!(ollama.effective_max_tokens(), 2048);
        assert_eq!(
            ollama.effective_timeout(),
            std::time::Duration::from_secs(300)
        );
        // An untouched profile uses the kind defaults (no crosstalk from
        // the sibling's overrides either).
        let plain = profile("g", ProviderKind::Groq, "", "");
        assert_eq!(
            plain.effective_temperature(),
            ProviderKind::Groq.defaults().temperature
        );
        assert_eq!(
            plain.effective_max_tokens(),
            ProviderKind::Groq.defaults().max_tokens
        );
        assert_eq!(
            plain.effective_timeout(),
            std::time::Duration::from_secs(ProviderKind::Groq.defaults().timeout_secs)
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn provider_settings_shape_stays_additive_for_older_readers() {
        let dir = std::env::temp_dir().join(format!("mcv-settings-add-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        // A file written before the sampling/extra fields existed (the
        // exact pre-change shape) loads unchanged, with the new fields
        // backfilled to None.
        let older = serde_json::json!({
            "version": SETTINGS_VERSION,
            "providers": [{
                "id": "mine", "name": "Mine", "kind": "custom",
                "base_url": "http://127.0.0.1:9/api", "model": "m"
            }],
            "default_provider": "mine",
            "options": {"theme": "system", "summarize_top": 8}
        });
        fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
        let loaded = Settings::load(&path).unwrap().expect("older file loads");
        let p = &loaded.providers[0];
        assert_eq!(p.kind, ProviderKind::Custom);
        assert_eq!(p.temperature, None);
        assert_eq!(p.max_tokens, None);
        assert_eq!(p.timeout_secs, None);
        assert_eq!(p.deployment_name, None);
        assert_eq!(p.api_version, None);
        assert_eq!(p.region, None);
        // Saving with the new fields set keeps the shape additive: the
        // optional fields are absent when None, so an older build (which
        // ignores unknown JSON fields) still reads the file.
        let mut s = loaded;
        s.providers[0].temperature = Some(0.4);
        s.providers[0].max_tokens = Some(256);
        s.providers[0].timeout_secs = Some(90);
        s.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"temperature\": 0.4"));
        let reloaded = Settings::load(&path).unwrap().unwrap();
        assert_eq!(reloaded.providers[0].temperature, Some(0.4));
        assert_eq!(reloaded.providers[0].max_tokens, Some(256));
        assert_eq!(reloaded.providers[0].timeout_secs, Some(90));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn catalog_kinds_apply_defaults_and_validate_overrides() {
        // Bare catalog profiles validate with pure defaults…
        for kind in [
            ProviderKind::Openai,
            ProviderKind::Gemini,
            ProviderKind::Mistral,
            ProviderKind::Groq,
            ProviderKind::Openrouter,
            ProviderKind::Ollama,
        ] {
            let s = settings_with(vec![profile("x", kind, "", "")]);
            s.validate()
                .unwrap_or_else(|e| panic!("{} with defaults should pass: {e}", kind.id()));
            let p = &s.providers[0];
            assert!(!p.effective_base_url().is_empty(), "{}", kind.id());
            assert!(!p.effective_model().is_empty(), "{}", kind.id());
        }
        // …an explicit base URL override wins and must be a URL…
        let s = settings_with(vec![profile(
            "oai",
            ProviderKind::Openai,
            "https://proxy.example.com/v1",
            "gpt-4o",
        )]);
        s.validate().unwrap();
        assert_eq!(
            s.providers[0].effective_base_url(),
            "https://proxy.example.com/v1"
        );
        // …a malformed URL is refused, and plaintext http off loopback
        // is refused for hosted kinds too.
        let s = settings_with(vec![profile("oai", ProviderKind::Openai, "not a url", "")]);
        assert!(s
            .validate()
            .unwrap_err()
            .to_string()
            .contains("http(s) URL"));
        let s = settings_with(vec![profile(
            "groq",
            ProviderKind::Groq,
            "http://api.groq.com/openai/v1",
            "",
        )]);
        assert!(s
            .validate()
            .unwrap_err()
            .to_string()
            .contains("plaintext http"));
        // Ollama's default is loopback http: allowed as-is.
        let s = settings_with(vec![profile("loc", ProviderKind::Ollama, "", "")]);
        s.validate().unwrap();
        assert_eq!(
            s.providers[0].effective_base_url(),
            ProviderKind::Ollama.defaults().base_url
        );
    }

    #[test]
    fn azure_requires_resource_url_and_deployment() {
        let s = settings_with(vec![profile("az", ProviderKind::AzureOpenai, "", "")]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("resource base URL"), "got: {err}");
        let s = settings_with(vec![profile(
            "az",
            ProviderKind::AzureOpenai,
            "https://res.openai.azure.com",
            "",
        )]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("deployment name"), "got: {err}");
        // Complete: deployment name + optional api-version (pinned
        // default applies); the deployment is the effective model.
        let mut s = settings_with(vec![profile(
            "az",
            ProviderKind::AzureOpenai,
            "https://res.openai.azure.com",
            "",
        )]);
        s.providers[0].deployment_name = Some(" my-gpt ".into());
        s.providers[0].api_version = Some(" 2025-01-01 ".into());
        s.normalize();
        s.validate().unwrap();
        let p = &s.providers[0];
        assert_eq!(p.deployment_name.as_deref(), Some("my-gpt"));
        assert_eq!(p.effective_model(), "my-gpt");
        assert_eq!(p.effective_api_version(), "2025-01-01");
        // Default api-version when unset.
        s.providers[0].api_version = None;
        assert_eq!(
            s.providers[0].effective_api_version(),
            DEFAULT_AZURE_API_VERSION
        );
    }

    #[test]
    fn bedrock_derives_base_url_from_region() {
        // Bare profile: default region template.
        let s = settings_with(vec![profile("b", ProviderKind::Bedrock, "", "")]);
        s.validate().unwrap();
        let p = &s.providers[0];
        assert_eq!(p.effective_region(), DEFAULT_BEDROCK_REGION);
        assert_eq!(
            p.effective_base_url(),
            "https://bedrock-runtime.us-east-1.amazonaws.com"
        );
        // An explicit region redirects the derived endpoint.
        let mut s = settings_with(vec![profile("b", ProviderKind::Bedrock, "", "")]);
        s.providers[0].region = Some(" eu-central-1 ".into());
        s.normalize();
        s.validate().unwrap();
        assert_eq!(s.providers[0].effective_region(), "eu-central-1");
        assert_eq!(
            s.providers[0].effective_base_url(),
            "https://bedrock-runtime.eu-central-1.amazonaws.com"
        );
        // Region slugs are validated.
        let mut s = settings_with(vec![profile("b", ProviderKind::Bedrock, "", "")]);
        s.providers[0].region = Some("Not A Region".into());
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("not a valid region slug"), "got: {err}");
        // The defaults table keeps the unprefixed model constant; the
        // region prefix is applied per profile (see below).
        assert_eq!(
            ProviderKind::Bedrock.defaults().model,
            "anthropic.claude-sonnet-4-5"
        );
    }

    #[test]
    fn bedrock_pairs_only_with_the_regions_runtime_host() {
        // An explicit base URL is accepted only when its host is the
        // chosen region's Bedrock runtime host (any spelling, port,
        // path — same host-comparison semantics as the z.ai pairing).
        let ok = [
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            "https://bedrock-runtime.us-east-1.amazonaws.com/",
            "https://BEDROCK-RUNTIME.US-EAST-1.AMAZONAWS.COM",
            "https://bedrock-runtime.us-east-1.amazonaws.com:443",
        ];
        for url in ok {
            let s = settings_with(vec![profile("b", ProviderKind::Bedrock, url, "")]);
            s.validate()
                .unwrap_or_else(|e| panic!("{url} should pass: {e}"));
        }
        // An explicit region pairs with that region's runtime host.
        let mut s = settings_with(vec![profile(
            "b",
            ProviderKind::Bedrock,
            "https://bedrock-runtime.eu-central-1.amazonaws.com",
            "",
        )]);
        s.providers[0].region = Some("eu-central-1".into());
        s.validate().unwrap();

        // Another region's runtime host: refused — the credentials are
        // SigV4-signed for the chosen region, not that one.
        let mut s = settings_with(vec![profile(
            "b",
            ProviderKind::Bedrock,
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            "",
        )]);
        s.providers[0].region = Some("eu-central-1".into());
        let err = s.validate().unwrap_err().to_string();
        assert!(
            err.contains(
                "Bedrock credentials are only ever sent to the region's Bedrock \
                          runtime endpoint"
            ),
            "got: {err}"
        );

        // An unrelated https host, another spelling of another region's
        // runtime, and the z.ai endpoint: all refused with the pairing
        // error — the credentials are SigV4-signed for the chosen
        // region's runtime only.
        for url in [
            "https://api.openai.com/v1",
            "https://api.z.ai",
            "https://bedrock-runtime.us-west-2.amazonaws.com",
        ] {
            let s = settings_with(vec![profile("b", ProviderKind::Bedrock, url, "")]);
            let err = s.validate().unwrap_err().to_string();
            assert!(
                err.contains("only ever sent to the region's Bedrock runtime endpoint"),
                "{url}: {err}"
            );
        }
        // The unparseable one is refused as a URL shape problem.
        let s = settings_with(vec![profile("b", ProviderKind::Bedrock, "not a url", "")]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("absolute http(s) URL"), "got: {err}");
        // Plaintext http to the pinned host stays refused (encrypted
        // transport only); loopback stays allowed for local mocks.
        let s = settings_with(vec![profile(
            "b",
            ProviderKind::Bedrock,
            "http://bedrock-runtime.us-east-1.amazonaws.com",
            "",
        )]);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("plaintext http"), "got: {err}");
    }

    #[test]
    fn bedrock_default_model_follows_region_inference_profile() {
        // The default model rides the region's cross-region inference
        // profile prefix; an explicit model always wins.
        let cases = [
            ("us-east-1", "us.anthropic.claude-sonnet-4-5"),
            ("us-west-2", "us.anthropic.claude-sonnet-4-5"),
            ("eu-central-1", "eu.anthropic.claude-sonnet-4-5"),
            ("eu-west-3", "eu.anthropic.claude-sonnet-4-5"),
            ("ap-south-1", "apac.anthropic.claude-sonnet-4-5"),
            ("ap-northeast-1", "apac.anthropic.claude-sonnet-4-5"),
            // Regions outside the three inference-profile families keep
            // the unprefixed, region-local model id.
            ("ca-central-1", "anthropic.claude-sonnet-4-5"),
            ("sa-east-1", "anthropic.claude-sonnet-4-5"),
            ("me-central-1", "anthropic.claude-sonnet-4-5"),
            ("af-south-1", "anthropic.claude-sonnet-4-5"),
        ];
        for (region, expected) in cases {
            let mut s = settings_with(vec![profile("b", ProviderKind::Bedrock, "", "")]);
            s.providers[0].region = Some(region.into());
            assert_eq!(
                s.providers[0].effective_model(),
                expected,
                "region {region}"
            );
        }
        // No region: the default region's prefix applies.
        let s = settings_with(vec![profile("b", ProviderKind::Bedrock, "", "")]);
        assert_eq!(
            s.providers[0].effective_model(),
            "us.anthropic.claude-sonnet-4-5"
        );
        // An explicit model wins over any region-derived default.
        let mut s = settings_with(vec![profile(
            "b",
            ProviderKind::Bedrock,
            "",
            " my-own-model ",
        )]);
        s.providers[0].region = Some("ap-south-1".into());
        s.normalize();
        assert_eq!(s.providers[0].effective_model(), "my-own-model");
    }
}

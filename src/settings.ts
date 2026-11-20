// Pure settings helpers and types (unit tested, no DOM wiring): mirrors
// `vault-core::settings` and `src-tauri::registry` for the Settings UI and
// the Ask provider menu. API keys never cross this boundary — only the
// `has_key` flag does.

/** Mirrors `vault_core::settings::ProviderKind` (snake_case serde). */
export type ProviderKind =
  | "zai"
  | "anthropic"
  | "openai"
  | "gemini"
  | "azure_openai"
  | "bedrock"
  | "mistral"
  | "groq"
  | "openrouter"
  | "ollama"
  | "lmstudio"
  | "custom";

/** UI theme choices: the backend set plus "vesper", a frontend-only
 * warm-paper palette (the backend enum does not know it — it is saved as
 * "dark" plus a machine-local localStorage flag; see main.ts). */
export type Theme = "system" | "light" | "dark" | "vesper";

/** Themes the backend `vault_core::settings::Theme` enum accepts
 * (snake_case serde) — the settings_get/settings_save contract. */
export type StoredTheme = Exclude<Theme, "vesper">;

/** Mirrors `vault_core::settings::ProviderProfile` (serde field names).
 * The sampling and provider-extra fields are additive and optional:
 * absent = the kind default (or not applicable to the kind). */
export interface ProviderProfile {
  id: string;
  name: string;
  kind: ProviderKind;
  base_url: string;
  model: string;
  temperature?: number;
  max_tokens?: number;
  timeout_secs?: number;
  deployment_name?: string;
  api_version?: string;
  region?: string;
}

/** One provider row in the Settings UI (`settings_get` response item). */
export interface ProviderView extends ProviderProfile {
  /** Whether an API key is stored (never the key itself). */
  has_key: boolean;
}

/** Read-only info about the environment fallback provider. */
export interface EnvProviderView {
  model: string;
  host: string;
}

/** Mirrors `src-tauri::registry::EnvOverridesView` — the Settings env
 * section: non-secret values plus key-presence flags (never the keys). */
export interface EnvOverridesView {
  base_url: string;
  model: string;
  zai_key: boolean;
  anthropic_key: boolean;
}

/** Mirrors `src-tauri::registry::SettingsView` — the `settings_get` /
 * `settings_save` / `llm_key_clear` response. */
export interface SettingsView {
  providers: ProviderView[];
  default_provider: string | null;
  theme: StoredTheme;
  summarize_top: number;
  /** Desktop notifications for new mail after sync/watch cycles. */
  notifications: boolean;
  /** Keep running in the background when the window is closed (M7:
   * close hides to the tray instead of exiting; opt-in, default off). */
  keep_running_in_background: boolean;
  /** Start the app automatically at login (M7; opt-in, default off). */
  start_at_login: boolean;
  env: EnvProviderView | null;
  env_overrides: EnvOverridesView;
  env_error: string | null;
  settings_error: string | null;
  key_store: "os_keyring" | "memory" | null;
}

/** Payload of `settings_save` (mirrors `src-tauri::registry::SaveSettingsPayload`). */
export interface SaveSettingsPayload {
  providers: ProviderProfile[];
  default_provider: string | null;
  theme: StoredTheme;
  summarize_top: number;
  /** Desktop notifications for new mail after sync/watch cycles. */
  notifications: boolean;
  /** Keep running in the background when the window is closed (M7). */
  keep_running_in_background: boolean;
  /** Start the app automatically at login (M7). */
  start_at_login: boolean;
  /** Non-empty key = store for that provider id; empty string = clear. */
  api_keys: Array<{ id: string; key: string }>;
  env_overrides: EnvOverridesPayload;
  /** Env key overrides: id is the variable name; non-empty = store, empty = clear. */
  env_api_keys: Array<{ id: "ZAI_API_KEY" | "ANTHROPIC_API_KEY"; key: string }>;
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

/** Mirrors `vault_core::settings::ProviderMenuItem` — one Ask-menu entry. */
export interface ProviderMenuItem {
  id: string;
  name: string;
  kind: string;
  model: string;
  host: string;
  available: boolean;
}

/** Pseudo-provider ids (reserved in the backend). */
export const LOCAL_ID = "local";
export const ENV_ID = "env";

/** Keyring override ids (backend `registry::ENV_*_KEY_ID`). */
export const ENV_ZAI_KEY_ID = "env:ZAI_API_KEY";
export const ENV_ANTHROPIC_KEY_ID = "env:ANTHROPIC_API_KEY";

/** Maximum results that may feed the RAG summary (backend contract). */
export const MAX_SUMMARIZE_TOP = 50;

const KIND_LABELS: Record<ProviderKind, string> = {
  zai: "z.ai GLM",
  anthropic: "Anthropic",
  openai: "OpenAI",
  gemini: "Google Gemini",
  azure_openai: "Azure OpenAI",
  bedrock: "AWS Bedrock",
  mistral: "Mistral",
  groq: "Groq",
  openrouter: "OpenRouter",
  ollama: "Ollama",
  lmstudio: "LM Studio",
  custom: "Custom",
};

/** Every kind in catalog (menu) order — mirrors `vault_core::settings::ALL_KINDS`. */
export const ALL_KINDS: readonly ProviderKind[] = [
  "zai",
  "anthropic",
  "openai",
  "gemini",
  "azure_openai",
  "bedrock",
  "mistral",
  "groq",
  "openrouter",
  "ollama",
  "lmstudio",
  "custom",
];

/** The keyless local kinds (backend `AuthStyle::None`): usable without
 * any stored API key — the local server needs no auth. */
export const KEYLESS_KINDS: readonly ProviderKind[] = ["ollama", "lmstudio"];

/** Kinds the Settings UI offers. `bedrock` exists in the backend catalog
 * (part 1) but its configuration UI is part 3 — it renders and saves
 * unchanged when present in a settings file, it just cannot be added. */
export const SELECTABLE_KINDS: readonly ProviderKind[] = ALL_KINDS.filter(
  (k) => k !== "bedrock",
);

/** Mirrors `vault_core::settings::DEFAULT_AZURE_API_VERSION`. */
export const DEFAULT_AZURE_API_VERSION = "2024-10-21";

/** The versioned per-kind defaults, mirrored from
 * `vault_core::settings::ProviderKind::defaults` (pinned by unit tests so
 * drift is caught at the value level). Empty model/base URL = the field is
 * user-supplied by definition (azure, custom; bedrock's URL derives from
 * its region; lmstudio's model is user-loaded in the app it serves). */
export interface ProviderDefaults {
  model: string;
  base_url: string;
  temperature: number;
  max_tokens: number;
  timeout_secs: number;
}

export const KIND_DEFAULTS: Record<ProviderKind, ProviderDefaults> = {
  zai: {
    model: "glm-5",
    base_url: "https://api.z.ai/api/anthropic",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 30,
  },
  anthropic: {
    model: "claude-sonnet-4-5",
    base_url: "https://api.anthropic.com",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 30,
  },
  openai: {
    model: "gpt-4o-mini",
    base_url: "https://api.openai.com/v1",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 30,
  },
  gemini: {
    model: "gemini-2.0-flash",
    base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 30,
  },
  azure_openai: {
    model: "",
    base_url: "",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 30,
  },
  bedrock: {
    model: "anthropic.claude-sonnet-4-5",
    base_url: "",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 30,
  },
  mistral: {
    model: "mistral-large-latest",
    base_url: "https://api.mistral.ai/v1",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 30,
  },
  groq: {
    model: "llama-3.3-70b-versatile",
    base_url: "https://api.groq.com/openai/v1",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 30,
  },
  openrouter: {
    model: "openrouter/auto",
    base_url: "https://openrouter.ai/api/v1",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 30,
  },
  ollama: {
    model: "llama3.2",
    base_url: "http://127.0.0.1:11434/v1",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 120,
  },
  lmstudio: {
    model: "", // the served model is user-loaded in LM Studio
    base_url: "http://127.0.0.1:1234/v1",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 120,
  },
  custom: {
    model: "",
    base_url: "",
    temperature: 0.2,
    max_tokens: 1024,
    timeout_secs: 30,
  },
};

/** Which per-kind fields the Settings form shows (the others stay hidden
 * but keep their stored value so a save round-trips unchanged).
 * `base_url` is user-supplied only for azure_openai, the local kinds
 * (ollama, lmstudio), and custom — the fixed-default kinds show a
 * default hint instead of an input. On azure_openai the deployment name
 * is the model id, so the plain model input is hidden there. */
export interface KindFieldVisibility {
  base_url: boolean;
  model: boolean;
  deployment_name: boolean;
  api_version: boolean;
}

export function kindFieldVisibility(kind: ProviderKind): KindFieldVisibility {
  return {
    base_url:
      kind === "azure_openai" || kind === "ollama" || kind === "lmstudio" || kind === "custom",
    model: kind !== "azure_openai",
    deployment_name: kind === "azure_openai",
    api_version: kind === "azure_openai",
  };
}

/** Model choices for the per-provider dropdown: the kind's default model
 * when it has one (azure/custom have none — typed input there). */
export function modelChoicesForKind(kind: ProviderKind): string[] {
  const model = KIND_DEFAULTS[kind].model;
  return model ? [model] : [];
}

/** Effective model for the at-a-glance summary (mirrors the backend
 * `effective_model`: on Azure the deployment name is the model id). */
export function effectiveModel(profile: Pick<
  ProviderProfile,
  "kind" | "model" | "deployment_name"
>): string {
  const typed = profile.model.trim();
  if (typed) return typed;
  if (profile.kind === "azure_openai") {
    return (profile.deployment_name ?? "").trim() || "deployment name not set";
  }
  return KIND_DEFAULTS[profile.kind].model || "model not set";
}

export function kindLabel(kind: ProviderKind): string {
  return KIND_LABELS[kind];
}

/** Prefill for a freshly added provider of `kind`: the kind defaults are
 * auto-filled (model and base URL; azure/custom start empty because both
 * are user-supplied there); the sampling overrides stay empty so the kind
 * default applies. */
export function prefillForKind(kind: ProviderKind): ProviderProfile {
  const d = KIND_DEFAULTS[kind];
  return { id: "", name: "", kind, base_url: d.base_url, model: d.model };
}

/** Placeholders for empty per-kind inputs (the backend default, if any). */
export function placeholderForKind(
  kind: ProviderKind,
  field: "base_url" | "model" | "temperature" | "max_tokens" | "timeout_secs" | "api_version",
): string {
  const d = KIND_DEFAULTS[kind];
  switch (field) {
    case "base_url":
      if (kind === "azure_openai") return "https://<resource>.openai.azure.com";
      if (kind === "custom") return "https://example.invalid/api";
      if (kind === "bedrock") return "https://bedrock-runtime.<region>.amazonaws.com";
      return `${d.base_url} (default)`;
    case "model":
      if (kind === "custom") return "model name";
      return d.model ? `${d.model} (default)` : "model name";
    case "temperature":
      return `${d.temperature} (default)`;
    case "max_tokens":
      return `${d.max_tokens} (default)`;
    case "timeout_secs":
      return `${d.timeout_secs} s (default)`;
    case "api_version":
      return `${DEFAULT_AZURE_API_VERSION} (default)`;
  }
}

/** Numeric bounds for the Advanced-section inputs (UI guard rails; empty
 * means the kind default applies). */
export const TEMPERATURE_BOUNDS = { min: 0, max: 2 } as const;
export const MAX_TOKENS_BOUNDS = { min: 1, max: 32768 } as const;
export const TIMEOUT_BOUNDS = { min: 1, max: 600 } as const;

/** Form values of one provider row as typed (strings, untrimmed ok). */
export interface ProviderFormValues {
  kind: ProviderKind;
  base_url: string;
  model: string;
  deployment_name: string;
  api_version: string;
  temperature: string;
  max_tokens: string;
  timeout_secs: string;
}

/** Inline-validation errors keyed by field (empty object = valid). These
 * mirror the backend pairing rules that would refuse the save anyway
 * (`vault_core::settings::check_profile_pairing`) plus the numeric bounds,
 * so the mistake is caught in the form before any IPC round trip. */
export function validateProviderForm(
  values: ProviderFormValues,
): Partial<Record<keyof ProviderFormValues, string>> {
  const errors: Partial<Record<keyof ProviderFormValues, string>> = {};
  const url = values.base_url.trim();
  const vis = kindFieldVisibility(values.kind);
  const urlRequired =
    values.kind === "anthropic" ||
    values.kind === "azure_openai" ||
    values.kind === "custom";
  if (urlRequired && url === "") {
    errors.base_url =
      values.kind === "custom"
        ? "a base URL is required"
        : "a base URL is required (e.g. https://api.anthropic.com)";
  } else if (url !== "") {
    let parsed: URL | null = null;
    try {
      parsed = new URL(url);
    } catch {
      // fall through to the malformed message
    }
    if (!parsed || (parsed.protocol !== "http:" && parsed.protocol !== "https:")) {
      errors.base_url = "must be an absolute http(s) URL";
    } else if (
      parsed.protocol === "http:" &&
      parsed.hostname !== "localhost" &&
      parsed.hostname !== "127.0.0.1" &&
      parsed.hostname !== "[::1]"
    ) {
      errors.base_url =
        "the API key must not travel over plaintext http — use https (or a localhost endpoint)";
    } else if (values.kind === "zai" && parsed.hostname !== "api.z.ai") {
      errors.base_url = "a z.ai key is only ever sent to the z.ai endpoint — remove the base URL to use the default";
    }
  }
  if (vis.model) {
    const model = values.model.trim();
    const modelRequired =
      values.kind === "anthropic" || values.kind === "custom";
    if (modelRequired && model === "") {
      errors.model = "a model name is required";
    }
  }
  if (vis.deployment_name && values.deployment_name.trim() === "") {
    errors.deployment_name = "a deployment name is required (it is the model id on Azure)";
  }
  if (vis.api_version) {
    const v = values.api_version.trim();
    if (v !== "" && !/^\d{4}-\d{2}-\d{2}$/.test(v)) {
      errors.api_version = "expected a YYYY-MM-DD api-version";
    }
  }
  const num = (raw: string): number | null => {
    const t = raw.trim();
    if (t === "") return null;
    const n = Number(t);
    return Number.isFinite(n) ? n : NaN;
  };
  const temp = num(values.temperature);
  if (
    temp !== null &&
    (Number.isNaN(temp) || temp < TEMPERATURE_BOUNDS.min || temp > TEMPERATURE_BOUNDS.max)
  ) {
    errors.temperature = "must be a number between 0 and 2 (empty = kind default)";
  }
  const tokens = num(values.max_tokens);
  if (
    tokens !== null &&
    (Number.isNaN(tokens) || tokens < MAX_TOKENS_BOUNDS.min || tokens > MAX_TOKENS_BOUNDS.max)
  ) {
    errors.max_tokens = "must be a whole number between 1 and 32768 (empty = kind default)";
  }
  const timeout = num(values.timeout_secs);
  if (
    timeout !== null &&
    (Number.isNaN(timeout) || timeout < TIMEOUT_BOUNDS.min || timeout > TIMEOUT_BOUNDS.max)
  ) {
    errors.timeout_secs = "must be a whole number between 1 and 600 (empty = kind default)";
  }
  return errors;
}

/** The active provider/model line for the top of the page: what Ask uses
 * right now, at a glance. */
export function activeProviderSummary(view: {
  providers: Array<ProviderView>;
  default_provider: string | null;
  env: EnvProviderView | null;
}): string {
  const id = view.default_provider;
  if (id && id !== ENV_ID) {
    const p = view.providers.find((x) => x.id === id);
    if (p) {
      const model = effectiveModel(p);
      const url = (p.base_url || KIND_DEFAULTS[p.kind].base_url).trim();
      let host = "unknown host";
      try {
        if (url) host = new URL(url).host;
      } catch {
        // keep the fallback
      }
      return `Active provider: ${providerName(p)} — ${model} via ${host}`;
    }
  }
  if (view.env) {
    return `Active provider: environment fallback — ${view.env.model} via ${view.env.host}`;
  }
  return "Active provider: none — Ask runs fully locally until a default provider or environment variables are set";
}

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

/** First free id with the given base (`base`, `base-2`, …) — mirrors
 * `vault_core::settings::unique_id` so new rows never collide. */
export function uniqueProviderId(base: string, taken: readonly string[]): string {
  const slug =
    base
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/-+/g, "-")
      .replace(/^-|-$/g, "") || "provider";
  const used = new Set(taken);
  if (!used.has(slug)) return slug;
  for (let n = 2; ; n++) {
    const candidate = `${slug}-${n}`;
    if (!used.has(candidate)) return candidate;
  }
}

/** Display name: explicit or the kind label. */
export function providerName(p: Pick<ProviderView, "name" | "kind">): string {
  return p.name.trim() || kindLabel(p.kind);
}

/** Ask-menu label for one provider entry (mirrors backend host/model). */
export function menuItemLabel(item: ProviderMenuItem): string {
  if (item.id === LOCAL_ID) return "Local full-text (no LLM)";
  const model = item.model || "default model";
  const host = item.host || "unknown host";
  const suffix = item.available ? "" : " — no API key";
  return `${item.name} (${model} · ${host})${suffix}`;
}

/** Where keys were last saved, as a UI sentence (null → nothing saved yet). */
export function keyStoreNote(store: SettingsView["key_store"]): string {
  if (store === "os_keyring") {
    return "API keys are stored in the OS keyring (service mailchat-vault).";
  }
  if (store === "memory") {
    return "No OS keyring available — API keys are kept in session memory only and will be requested again after a restart.";
  }
  return "";
}

/** The Settings version readout line: the build-stamped version the
 * `app_version` IPC returned from the running app (never a frontend
 * constant, so dev and packaged builds each show their own). */
export function versionReadout(version: string): string {
  return `Version ${version.trim()}`;
}

/** Apply a theme choice to the document (idempotent). "system" clears
 * the explicit theme so the prefers-color-scheme media query in the
 * design tokens decides; explicit light/dark pin the token palette.
 * "vesper" pins the warm-paper palette the same way, but maps the
 * color-scheme property to "dark" ("vesper" is not a keyword the
 * engine would recognize). */
export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  root.style.colorScheme =
    theme === "system" ? "" : theme === "vesper" ? "dark" : theme;
  if (theme === "system") {
    delete root.dataset.theme;
  } else {
    root.dataset.theme = theme;
  }
}

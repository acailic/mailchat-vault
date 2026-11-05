// Pure settings helpers and types (unit tested, no DOM wiring): mirrors
// `vault-core::settings` and `src-tauri::registry` for the Settings UI and
// the Ask provider menu. API keys never cross this boundary — only the
// `has_key` flag does.

/** Mirrors `vault_core::settings::ProviderKind` (snake_case serde). */
export type ProviderKind = "zai" | "anthropic" | "custom";

/** Mirrors `vault_core::settings::Theme` (snake_case serde). */
export type Theme = "system" | "light" | "dark";

/** Mirrors `vault_core::settings::ProviderProfile` (serde field names). */
export interface ProviderProfile {
  id: string;
  name: string;
  kind: ProviderKind;
  base_url: string;
  model: string;
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

/** Mirrors `src-tauri::registry::SettingsView` — the `settings_get` /
 * `settings_save` / `llm_key_clear` response. */
export interface SettingsView {
  providers: ProviderView[];
  default_provider: string | null;
  theme: Theme;
  summarize_top: number;
  /** Desktop notifications for new mail after sync/watch cycles. */
  notifications: boolean;
  env: EnvProviderView | null;
  env_error: string | null;
  settings_error: string | null;
  key_store: "os_keyring" | "memory" | null;
}

/** Payload of `settings_save` (mirrors `src-tauri::registry::SaveSettingsPayload`). */
export interface SaveSettingsPayload {
  providers: ProviderProfile[];
  default_provider: string | null;
  theme: Theme;
  summarize_top: number;
  /** Desktop notifications for new mail after sync/watch cycles. */
  notifications: boolean;
  /** Non-empty key = store for that provider id; empty string = clear. */
  api_keys: Array<{ id: string; key: string }>;
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

/** Maximum results that may feed the RAG summary (backend contract). */
export const MAX_SUMMARIZE_TOP = 50;

const KIND_LABELS: Record<ProviderKind, string> = {
  zai: "z.ai GLM",
  anthropic: "Anthropic",
  custom: "Custom",
};

export function kindLabel(kind: ProviderKind): string {
  return KIND_LABELS[kind];
}

/** Prefill for a freshly added provider of `kind` (label, URL, model). */
export function prefillForKind(kind: ProviderKind): ProviderProfile {
  switch (kind) {
    case "zai":
      // z.ai: defaults apply when left empty.
      return { id: "", name: "", kind, base_url: "", model: "" };
    case "anthropic":
      // Anthropic: the endpoint must be explicit (never z.ai).
      return {
        id: "",
        name: "",
        kind,
        base_url: "https://api.anthropic.com",
        model: "claude-sonnet-4-5",
      };
    case "custom":
      return { id: "", name: "", kind, base_url: "", model: "" };
  }
}

/** Placeholders for empty per-kind inputs (the backend default, if any). */
export function placeholderForKind(kind: ProviderKind, field: "base_url" | "model"): string {
  if (kind === "zai") {
    return field === "base_url" ? "https://api.z.ai/api/anthropic (default)" : "glm-5 (default)";
  }
  return field === "base_url" ? "https://example.invalid/api" : "model name";
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

/** Apply a theme choice to the document (idempotent). "system" clears
 * the explicit theme so the prefers-color-scheme media query in the
 * design tokens decides; explicit light/dark pin the token palette. */
export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  root.style.colorScheme = theme === "system" ? "" : theme;
  if (theme === "system") {
    delete root.dataset.theme;
  } else {
    root.dataset.theme = theme;
  }
}

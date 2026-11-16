// Unit tests for the pure settings helpers (no DOM wiring beyond the
// happy-dom document for applyTheme).
// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from "vitest";
import {
  ALL_KINDS,
  DEFAULT_AZURE_API_VERSION,
  KEYLESS_KINDS,
  KIND_DEFAULTS,
  SELECTABLE_KINDS,
  activeProviderSummary,
  applyTheme,
  effectiveModel,
  envKeyPlaceholder,
  envTestPayload,
  keyStoreNote,
  kindFieldVisibility,
  kindLabel,
  menuItemLabel,
  modelChoicesForKind,
  placeholderForKind,
  prefillForKind,
  providerName,
  uniqueProviderId,
  validateProviderForm,
  versionReadout,
  ENV_ANTHROPIC_KEY_ID,
  ENV_ZAI_KEY_ID,
  type ProviderFormValues,
  type ProviderMenuItem,
  type ProviderView,
  type SettingsView,
} from "./settings";

describe("prefillForKind", () => {
  it("z.ai auto-fills its kind defaults (model and endpoint)", () => {
    expect(prefillForKind("zai")).toEqual({
      id: "",
      name: "",
      kind: "zai",
      base_url: "https://api.z.ai/api/anthropic",
      model: "glm-5",
    });
  });

  it("Anthropic starts explicit: api.anthropic.com and a model", () => {
    const p = prefillForKind("anthropic");
    expect(p.base_url).toBe("https://api.anthropic.com");
    expect(p.model).not.toBe("");
    expect(p.kind).toBe("anthropic");
  });

  it("azure starts empty: resource URL and deployment name are user-supplied", () => {
    expect(prefillForKind("azure_openai")).toEqual({
      id: "",
      name: "",
      kind: "azure_openai",
      base_url: "",
      model: "",
    });
  });

  it("custom starts empty but is required to be filled", () => {
    expect(prefillForKind("custom")).toEqual({
      id: "",
      name: "",
      kind: "custom",
      base_url: "",
      model: "",
    });
    expect(placeholderForKind("custom", "base_url")).toContain("https://");
    expect(placeholderForKind("custom", "model")).toContain("model");
  });

  it("z.ai placeholders name the backend defaults", () => {
    expect(placeholderForKind("zai", "base_url")).toContain("api.z.ai");
    expect(placeholderForKind("zai", "model")).toContain("glm-5");
  });

  it("the local kinds prefill their endpoints; lmstudio's model stays empty", () => {
    expect(prefillForKind("ollama")).toEqual({
      id: "",
      name: "",
      kind: "ollama",
      base_url: "http://127.0.0.1:11434/v1",
      model: "llama3.2",
    });
    expect(prefillForKind("lmstudio")).toEqual({
      id: "",
      name: "",
      kind: "lmstudio",
      base_url: "http://127.0.0.1:1234/v1",
      model: "", // the served model is user-loaded in LM Studio
    });
    expect(placeholderForKind("lmstudio", "base_url")).toBe(
      "http://127.0.0.1:1234/v1 (default)",
    );
    expect(placeholderForKind("lmstudio", "model")).toBe("model name");
    expect(placeholderForKind("lmstudio", "timeout_secs")).toBe("120 s (default)");
    expect(KEYLESS_KINDS).toEqual(["ollama", "lmstudio"]);
  });
});

describe("uniqueProviderId", () => {
  it("slugs and dedupes against existing ids", () => {
    expect(uniqueProviderId("custom", [])).toBe("custom");
    expect(uniqueProviderId("custom", ["custom"])).toBe("custom-2");
    expect(uniqueProviderId("custom", ["custom", "custom-2"])).toBe("custom-3");
    expect(uniqueProviderId("zai", ["custom"])).toBe("zai");
    expect(uniqueProviderId("Work GLM!", ["zai"])).toBe("work-glm");
    expect(uniqueProviderId("!!!", [])).toBe("provider");
  });
});

describe("labels", () => {
  it("kind labels and provider display names", () => {
    expect(kindLabel("zai")).toBe("z.ai GLM");
    expect(kindLabel("anthropic")).toBe("Anthropic");
    expect(kindLabel("custom")).toBe("Custom");
    expect(providerName({ name: "", kind: "zai" })).toBe("z.ai GLM");
    expect(providerName({ name: "Work GLM", kind: "zai" })).toBe("Work GLM");
    expect(providerName({ name: "  ", kind: "anthropic" })).toBe("Anthropic");
  });

  it("every catalog kind has a label, in catalog order", () => {
    expect(ALL_KINDS).toEqual([
      "zai", "anthropic", "openai", "gemini", "azure_openai", "bedrock",
      "mistral", "groq", "openrouter", "ollama", "lmstudio", "custom",
    ]);
    for (const kind of ALL_KINDS) {
      expect(kindLabel(kind)).not.toBe("");
    }
    expect(kindLabel("azure_openai")).toBe("Azure OpenAI");
    expect(kindLabel("bedrock")).toBe("AWS Bedrock");
    expect(kindLabel("ollama")).toBe("Ollama");
    expect(kindLabel("lmstudio")).toBe("LM Studio");
  });

  it("the selectable kinds omit bedrock (its configuration UI is part 3)", () => {
    expect(SELECTABLE_KINDS).not.toContain("bedrock");
    expect(SELECTABLE_KINDS).toHaveLength(ALL_KINDS.length - 1);
  });
});

/** The defaults mirror pins `vault_core::settings::ProviderKind::defaults`
 * at the value level so drift from the backend catalog is caught here. */
describe("KIND_DEFAULTS mirror", () => {
  it("z.ai defaults to the GLM endpoint pair", () => {
    expect(KIND_DEFAULTS.zai).toEqual({
      model: "glm-5",
      base_url: "https://api.z.ai/api/anthropic",
      temperature: 0.2,
      max_tokens: 1024,
      timeout_secs: 30,
    });
  });

  it("every kind pins model, base URL, and the shared sampling defaults", () => {
    expect(KIND_DEFAULTS.anthropic).toMatchObject({
      model: "claude-sonnet-4-5",
      base_url: "https://api.anthropic.com",
    });
    expect(KIND_DEFAULTS.openai).toMatchObject({
      model: "gpt-4o-mini",
      base_url: "https://api.openai.com/v1",
    });
    expect(KIND_DEFAULTS.gemini).toMatchObject({
      model: "gemini-2.0-flash",
      base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
    });
    expect(KIND_DEFAULTS.mistral).toMatchObject({
      model: "mistral-large-latest",
      base_url: "https://api.mistral.ai/v1",
    });
    expect(KIND_DEFAULTS.groq).toMatchObject({
      model: "llama-3.3-70b-versatile",
      base_url: "https://api.groq.com/openai/v1",
    });
    expect(KIND_DEFAULTS.openrouter).toMatchObject({
      model: "openrouter/auto",
      base_url: "https://openrouter.ai/api/v1",
    });
    expect(KIND_DEFAULTS.ollama).toMatchObject({
      model: "llama3.2",
      base_url: "http://127.0.0.1:11434/v1",
      timeout_secs: 120, // local inference gets the longer leash
    });
    expect(KIND_DEFAULTS.lmstudio).toEqual({
      // the served model is user-loaded in LM Studio: no default id
      model: "",
      base_url: "http://127.0.0.1:1234/v1",
      temperature: 0.2,
      max_tokens: 1024,
      timeout_secs: 120,
    });
    expect(KIND_DEFAULTS.bedrock).toMatchObject({
      model: "anthropic.claude-sonnet-4-5",
      base_url: "", // derived from the region server-side
    });
    for (const kind of ALL_KINDS) {
      expect(KIND_DEFAULTS[kind].temperature).toBe(0.2);
      expect(KIND_DEFAULTS[kind].max_tokens).toBe(1024);
      expect(KIND_DEFAULTS[kind].timeout_secs).toBe(
        kind === "ollama" || kind === "lmstudio" ? 120 : 30,
      );
    }
  });

  it("azure and custom carry no default model or base URL (user-supplied)", () => {
    expect(KIND_DEFAULTS.azure_openai.model).toBe("");
    expect(KIND_DEFAULTS.azure_openai.base_url).toBe("");
    expect(KIND_DEFAULTS.custom.model).toBe("");
    expect(KIND_DEFAULTS.custom.base_url).toBe("");
  });
});

describe("kindFieldVisibility", () => {
  it("the base URL field shows only for azure, the local kinds, and custom", () => {
    for (const kind of ALL_KINDS) {
      const visible = kindFieldVisibility(kind).base_url;
      expect(visible).toBe(
        kind === "azure_openai" ||
          kind === "ollama" ||
          kind === "lmstudio" ||
          kind === "custom",
      );
    }
  });

  it("deployment name and api-version are azure-only; the plain model input hides there", () => {
    expect(kindFieldVisibility("azure_openai")).toEqual({
      base_url: true,
      model: false,
      deployment_name: true,
      api_version: true,
    });
    for (const kind of ALL_KINDS.filter((k) => k !== "azure_openai")) {
      const vis = kindFieldVisibility(kind);
      expect(vis.deployment_name).toBe(false);
      expect(vis.api_version).toBe(false);
      expect(vis.model).toBe(true);
    }
  });
});

describe("modelChoicesForKind (dropdown data)", () => {
  it("offers the kind default where one exists", () => {
    expect(modelChoicesForKind("zai")).toEqual(["glm-5"]);
    expect(modelChoicesForKind("openai")).toEqual(["gpt-4o-mini"]);
    expect(modelChoicesForKind("ollama")).toEqual(["llama3.2"]);
    expect(modelChoicesForKind("bedrock")).toEqual(["anthropic.claude-sonnet-4-5"]);
  });

  it("azure, custom, and lmstudio have no default model to offer (typed input)", () => {
    expect(modelChoicesForKind("azure_openai")).toEqual([]);
    expect(modelChoicesForKind("custom")).toEqual([]);
    expect(modelChoicesForKind("lmstudio")).toEqual([]);
  });
});

describe("effectiveModel", () => {
  it("falls back to the kind default", () => {
    expect(effectiveModel({ kind: "zai", model: "", deployment_name: undefined })).toBe("glm-5");
    expect(effectiveModel({ kind: "zai", model: " glm-4.6 ", deployment_name: undefined })).toBe("glm-4.6");
  });

  it("on Azure the deployment name is the model id", () => {
    expect(
      effectiveModel({ kind: "azure_openai", model: "", deployment_name: "glm-deploy" }),
    ).toBe("glm-deploy");
    expect(
      effectiveModel({ kind: "azure_openai", model: "", deployment_name: undefined }),
    ).toBe("deployment name not set");
  });

  it("azure and custom say so when nothing is set", () => {
    expect(effectiveModel({ kind: "custom", model: "", deployment_name: undefined })).toBe("model not set");
  });
});

describe("placeholders", () => {
  it("advanced placeholders name the kind default; empty means default", () => {
    expect(placeholderForKind("zai", "temperature")).toBe("0.2 (default)");
    expect(placeholderForKind("zai", "max_tokens")).toBe("1024 (default)");
    expect(placeholderForKind("ollama", "timeout_secs")).toBe("120 s (default)");
    expect(placeholderForKind("zai", "timeout_secs")).toBe("30 s (default)");
  });

  it("the azure api-version placeholder names the pinned default", () => {
    expect(DEFAULT_AZURE_API_VERSION).toBe("2024-10-21");
    expect(placeholderForKind("azure_openai", "api_version")).toContain("2024-10-21");
  });

  it("kinds with a default model name it; custom asks for one", () => {
    expect(placeholderForKind("anthropic", "model")).toContain("claude-sonnet-4-5");
    expect(placeholderForKind("azure_openai", "model")).toBe("model name");
    expect(placeholderForKind("custom", "model")).toBe("model name");
  });
});

describe("validateProviderForm", () => {
  const valid = (extra: Partial<ProviderFormValues> = {}): ProviderFormValues => ({
    kind: "zai",
    base_url: "https://api.z.ai/api/anthropic",
    model: "glm-5",
    deployment_name: "",
    api_version: "",
    temperature: "",
    max_tokens: "",
    timeout_secs: "",
    ...extra,
  });

  it("a prefilled row is valid as rendered (only the API key is missing)", () => {
    for (const kind of SELECTABLE_KINDS) {
      const p = prefillForKind(kind);
      const errors = validateProviderForm(
        valid({ kind, base_url: p.base_url, model: p.model }),
      );
      if (kind === "azure_openai" || kind === "custom") {
        // definitionally user-supplied: the prefill alone is not enough
        expect(Object.keys(errors)).toContain("base_url");
      } else {
        expect(errors).toEqual({});
      }
    }
  });

  it("base URL shape: absolute http(s) required, plaintext refused off localhost", () => {
    expect(validateProviderForm(valid({ kind: "custom", base_url: "not a url", model: "m" })).base_url)
      .toContain("absolute http(s) URL");
    expect(validateProviderForm(valid({ kind: "custom", base_url: "ftp://x.example", model: "m" })).base_url)
      .toContain("absolute http(s) URL");
    expect(validateProviderForm(valid({ kind: "custom", base_url: "http://example.com/api", model: "m" })).base_url)
      .toContain("plaintext http");
    expect(validateProviderForm(valid({ kind: "custom", base_url: "http://127.0.0.1:9/api", model: "m" })).base_url)
      .toBeUndefined();
    expect(validateProviderForm(valid({ kind: "custom", base_url: "https://api.example.com/v1", model: "m" })).base_url)
      .toBeUndefined();
  });

  it("z.ai keys only ever go to the z.ai endpoint", () => {
    expect(validateProviderForm(valid({ base_url: "https://other.example/api" })).base_url)
      .toContain("z.ai");
    expect(validateProviderForm(valid({ base_url: "" })).base_url).toBeUndefined();
  });

  it("anthropic, azure, and custom need their base URL; custom/anthropic need a model", () => {
    expect(validateProviderForm(valid({ kind: "anthropic", base_url: "", model: "" })).base_url)
      .toContain("base URL is required");
    expect(validateProviderForm(valid({ kind: "azure_openai", base_url: "" })).base_url)
      .toContain("base URL is required");
    expect(validateProviderForm(valid({ kind: "custom", base_url: "", model: "" })).base_url)
      .toContain("base URL is required");
    expect(validateProviderForm(valid({ kind: "custom", base_url: "https://api.example.com", model: "" })).model)
      .toContain("model name is required");
  });

  it("azure requires a deployment name and a dated api-version", () => {
    const azure = (extra: Partial<ProviderFormValues> = {}) =>
      validateProviderForm(
        valid({ kind: "azure_openai", base_url: "https://res.openai.azure.com", model: "", ...extra }),
      );
    expect(azure().deployment_name).toContain("deployment name");
    expect(azure({ deployment_name: "glm-deploy" }).deployment_name).toBeUndefined();
    expect(azure({ deployment_name: "glm-deploy", api_version: "latest" }).api_version)
      .toContain("YYYY-MM-DD");
    expect(azure({ deployment_name: "glm-deploy", api_version: "2024-10-21" }).api_version)
      .toBeUndefined();
  });

  it("numeric bounds: out-of-range and non-numeric values error, empty is fine", () => {
    expect(validateProviderForm(valid({ temperature: "3" })).temperature).toContain("0 and 2");
    expect(validateProviderForm(valid({ temperature: "-1" })).temperature).toBeTruthy();
    expect(validateProviderForm(valid({ temperature: "hot" })).temperature).toBeTruthy();
    expect(validateProviderForm(valid({ temperature: "0.7" })).temperature).toBeUndefined();
    expect(validateProviderForm(valid({ max_tokens: "0" })).max_tokens).toBeTruthy();
    expect(validateProviderForm(valid({ max_tokens: "999999" })).max_tokens).toBeTruthy();
    expect(validateProviderForm(valid({ max_tokens: "2048" })).max_tokens).toBeUndefined();
    expect(validateProviderForm(valid({ timeout_secs: "0" })).timeout_secs).toBeTruthy();
    expect(validateProviderForm(valid({ timeout_secs: "700" })).timeout_secs).toBeTruthy();
    expect(validateProviderForm(valid({ timeout_secs: "45" })).timeout_secs).toBeUndefined();
  });
});

describe("activeProviderSummary", () => {
  const provider = (extra: Partial<ProviderView> = {}): ProviderView => ({
    id: "zai",
    name: "",
    kind: "zai",
    base_url: "",
    model: "",
    has_key: true,
    ...extra,
  });

  it("names the default provider with its effective model and host", () => {
    expect(
      activeProviderSummary({
        providers: [provider()],
        default_provider: "zai",
        env: null,
      }),
    ).toBe("Active provider: z.ai GLM — glm-5 via api.z.ai");
  });

  it("falls back to the environment fallback, then to local-only", () => {
    expect(
      activeProviderSummary({
        providers: [],
        default_provider: null,
        env: { model: "env-model", host: "127.0.0.1" },
      }),
    ).toContain("environment fallback — env-model via 127.0.0.1");
    expect(
      activeProviderSummary({ providers: [], default_provider: null, env: null }),
    ).toContain("none");
  });
});

describe("menu labels", () => {
  it("name model and host, and flag missing keys", () => {
    const glm: ProviderMenuItem = {
      id: "zai",
      name: "z.ai GLM",
      kind: "zai",
      model: "glm-5",
      host: "api.z.ai",
      available: true,
    };
    expect(menuItemLabel(glm)).toBe("z.ai GLM (glm-5 · api.z.ai)");
    expect(menuItemLabel({ ...glm, available: false })).toBe(
      "z.ai GLM (glm-5 · api.z.ai) — no API key",
    );
    expect(menuItemLabel({ ...glm, id: "local", model: "", host: "" })).toBe(
      "Local full-text (no LLM)",
    );
  });
});

describe("keyStoreNote", () => {
  it("explains where keys live", () => {
    expect(keyStoreNote("os_keyring")).toContain("OS keyring");
    expect(keyStoreNote("memory")).toContain("session memory");
    expect(keyStoreNote(null)).toBe("");
    const view = { key_store: "memory" } as SettingsView;
    expect(keyStoreNote(view.key_store)).toContain("restart");
  });
});

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

describe("versionReadout", () => {
  it("labels whatever version the running app reports", () => {
    expect(versionReadout("0.2.0")).toBe("Version 0.2.0");
    expect(versionReadout(" 1.2.3-beta.4 ")).toBe("Version 1.2.3-beta.4");
  });
});

describe("applyTheme", () => {
  afterEach(() => {
    applyTheme("system");
  });

  it("forces light/dark and resets to system", () => {
    applyTheme("dark");
    expect(document.documentElement.style.colorScheme).toBe("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
    applyTheme("light");
    expect(document.documentElement.style.colorScheme).toBe("light");
    expect(document.documentElement.dataset.theme).toBe("light");
    applyTheme("system");
    expect(document.documentElement.style.colorScheme).toBe("");
    expect(document.documentElement.dataset.theme).toBeUndefined();
  });

  it("pins vesper as an explicit theme with a dark color-scheme", () => {
    applyTheme("vesper");
    expect(document.documentElement.dataset.theme).toBe("vesper");
    // "vesper" is not a color-scheme keyword; the engine gets "dark".
    expect(document.documentElement.style.colorScheme).toBe("dark");
    applyTheme("system");
    expect(document.documentElement.style.colorScheme).toBe("");
    expect(document.documentElement.dataset.theme).toBeUndefined();
  });
});

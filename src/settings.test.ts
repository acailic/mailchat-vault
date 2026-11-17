// Unit tests for the pure settings helpers (no DOM wiring beyond the
// happy-dom document for applyTheme).
// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from "vitest";
import {
  applyTheme,
  envKeyPlaceholder,
  envTestPayload,
  keyStoreNote,
  kindLabel,
  menuItemLabel,
  placeholderForKind,
  prefillForKind,
  providerName,
  uniqueProviderId,
  ENV_ANTHROPIC_KEY_ID,
  ENV_ZAI_KEY_ID,
  type ProviderMenuItem,
  type SettingsView,
} from "./settings";

describe("prefillForKind", () => {
  it("z.ai starts empty (defaults apply server-side)", () => {
    expect(prefillForKind("zai")).toEqual({
      id: "",
      name: "",
      kind: "zai",
      base_url: "",
      model: "",
    });
  });

  it("Anthropic starts explicit: api.anthropic.com and a model", () => {
    const p = prefillForKind("anthropic");
    expect(p.base_url).toBe("https://api.anthropic.com");
    expect(p.model).not.toBe("");
    expect(p.kind).toBe("anthropic");
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

  it("menu labels name model and host, and flag missing keys", () => {
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

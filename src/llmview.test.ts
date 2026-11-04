import { describe, expect, it } from "vitest";
import {
  consentPromptText,
  consentStatusLine,
  describePlan,
  privacyNote,
  privacyNoteFor,
  splitCitations,
  type AskOutcome,
  type LlmStatus,
} from "./llmview";
import { LOCAL_ID, type ProviderMenuItem } from "./settings";

const zaiItem: ProviderMenuItem = {
  id: "zai",
  name: "z.ai GLM",
  kind: "zai",
  model: "glm-5",
  host: "api.z.ai",
  available: true,
};

const envItem: ProviderMenuItem = {
  id: "env",
  name: "Environment variables",
  kind: "env",
  model: "mock-model",
  host: "127.0.0.1",
  available: true,
};

const localItem: ProviderMenuItem = {
  id: LOCAL_ID,
  name: "Local full-text",
  kind: LOCAL_ID,
  model: "",
  host: "",
  available: true,
};

const enabled: LlmStatus = {
  enabled: true,
  model: "glm-5",
  host: "api.z.ai",
  summarize_top: 8,
  providers: [localItem, zaiItem],
  default_provider: "zai",
};

const disabled: LlmStatus = {
  enabled: false,
  model: "",
  host: "",
  summarize_top: 8,
  providers: [localItem],
  default_provider: null,
};

describe("describePlan", () => {
  it("shows the LLM plan with match expression, dates, and hybrid ranking", () => {
    const outcome: Pick<AskOutcome, "plan" | "plan_source" | "ranking"> = {
      plan: { match_expr: '"vault" OR "archive"', date_from: "2026-10-01", date_to: "2026-10-31" },
      plan_source: "llm",
      ranking: "hybrid_fts_vector",
    };
    expect(describePlan(outcome)).toBe(
      'FTS: "vault" OR "archive" · dates: 2026-10-01 → 2026-10-31 · ' +
        "ranked: hybrid FTS + local vectors (planned by LLM)",
    );
  });

  it("marks the fallback, FTS-only ranking, and omits missing dates", () => {
    const outcome: Pick<AskOutcome, "plan" | "plan_source" | "ranking"> = {
      plan: { match_expr: '"vault"', date_from: null, date_to: null },
      plan_source: "llm_fallback",
      ranking: "fts_only",
    };
    expect(describePlan(outcome)).toBe(
      'FTS: "vault" · ranked: FTS only (LLM plan unusable — plain term search)',
    );
  });

  it("marks the no-LLM mode and the vector-only ranking", () => {
    const noLlm: Pick<AskOutcome, "plan" | "plan_source" | "ranking"> = {
      plan: { match_expr: "", date_from: null, date_to: null },
      plan_source: "no_llm",
      ranking: "fts_only",
    };
    expect(describePlan(noLlm)).toBe(
      "FTS: (none) · ranked: FTS only (no LLM configured — plain term search)",
    );
    const vectorOnly: Pick<AskOutcome, "plan" | "plan_source" | "ranking"> = {
      plan: { match_expr: '"deploying"', date_from: null, date_to: null },
      plan_source: "no_llm",
      ranking: "vector_only",
    };
    expect(describePlan(vectorOnly)).toBe(
      'FTS: "deploying" · ranked: local vectors only (no LLM configured — plain term search)',
    );
  });

  it("marks explicit local mode", () => {
    const outcome: Pick<AskOutcome, "plan" | "plan_source" | "ranking"> = {
      plan: { match_expr: '"vault"', date_from: null, date_to: null },
      plan_source: "local",
      ranking: "hybrid_fts_vector",
    };
    expect(describePlan(outcome)).toBe(
      'FTS: "vault" · ranked: hybrid FTS + local vectors (local full-text — nothing sent to any provider)',
    );
  });
});

describe("privacyNoteFor", () => {
  it("states exactly what is sent for a profile provider", () => {
    const note = privacyNoteFor(zaiItem, 8);
    expect(note).toContain("LLM assist is on (glm-5 via api.z.ai)");
    expect(note).toContain("top 8 result messages");
    expect(note).toContain("truncated to 500 chars");
    expect(note).toContain("Nothing else leaves this machine");
    expect(note).toContain("never sent");
    expect(note).toContain("never written to the settings file");
  });

  it("states the fallback when disabled", () => {
    const note = privacyNote(disabled);
    expect(note).toContain("LLM assist is off");
    expect(note).toContain("fully locally");
    expect(note).toContain("hybrid vector ranking");
    expect(note).toContain("nothing leaves this machine");
    expect(note).toContain("ANTHROPIC_API_KEY");
  });

  it("states the environment-only key handling for the env provider", () => {
    const note = privacyNoteFor(envItem, 5);
    expect(note).toContain("mock-model via 127.0.0.1");
    expect(note).toContain("read from the environment only");
    expect(note).toContain("top 5 result messages");
  });

  it("states the local fallback for local mode and keyless providers", () => {
    const localNote = privacyNoteFor(localItem, 8);
    expect(localNote).toContain("LLM assist is off");
    expect(localNote).toContain("nothing leaves this machine");
    expect(localNote).toContain("Configure a provider in Settings");

    const keyless = privacyNoteFor({ ...zaiItem, available: false }, 8);
    expect(keyless).toContain("LLM assist is off");
  });
});

describe("privacyNote", () => {
  it("describes the default provider", () => {
    expect(privacyNote(enabled)).toBe(privacyNoteFor(zaiItem, 8));
  });

  it("describes local mode when nothing is configured", () => {
    expect(privacyNote(disabled)).toBe(privacyNoteFor(localItem, 8));
    expect(privacyNote(disabled)).toContain("nothing leaves this machine");
  });
});

// ---------------------------------------------------------------------------
// M5: AI consent + source-linked answers
// ---------------------------------------------------------------------------

describe("consentStatusLine", () => {
  it("states both session states distinctly", () => {
    const granted = consentStatusLine(true);
    const notGranted = consentStatusLine(false);
    expect(granted).toContain("granted for this session");
    expect(notGranted).toContain("not granted");
    expect(notGranted).toContain("fully locally");
    expect(granted).not.toBe(notGranted);
  });
});

describe("consentPromptText", () => {
  it("names the provider, model, and exactly what would leave", () => {
    const text = consentPromptText(zaiItem, 8);
    expect(text).toContain("glm-5 via api.z.ai");
    expect(text).toContain("top 8 matching messages");
    expect(text).toContain("truncated to 500 chars");
    expect(text).toContain("for this session");
  });
});

describe("splitCitations", () => {
  it("splits an answer into text runs and citation markers", () => {
    expect(splitCitations("October covered sync [1] and problems [2].")).toEqual([
      "October covered sync ",
      { cite: 1 },
      " and problems ",
      { cite: 2 },
      ".",
    ]);
  });

  it("keeps leading, trailing, adjacent, and bracket-free text intact", () => {
    expect(splitCitations("[1] all of it")).toEqual([{ cite: 1 }, " all of it"]);
    expect(splitCitations("no citations here")).toEqual(["no citations here"]);
    expect(splitCitations("adjacent [1][2] markers")).toEqual([
      "adjacent ",
      { cite: 1 },
      { cite: 2 },
      " markers",
    ]);
    expect(splitCitations("")).toEqual([]);
  });

  it("leaves non-citation brackets alone", () => {
    expect(splitCitations("a [not a number] b [10] c")).toEqual([
      "a [not a number] b ",
      { cite: 10 },
      " c",
    ]);
  });
});

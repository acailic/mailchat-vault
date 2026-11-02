import { describe, expect, it } from "vitest";
import { describePlan, privacyNote, type AskOutcome, type LlmStatus } from "./llmview";

const enabled: LlmStatus = {
  enabled: true,
  model: "glm-5",
  host: "api.z.ai",
  summarize_top: 8,
};

const disabled: LlmStatus = { enabled: false, model: "", host: "", summarize_top: 8 };

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
});

describe("privacyNote", () => {
  it("states exactly what is sent when enabled", () => {
    const note = privacyNote(enabled);
    expect(note).toContain("glm-5 via api.z.ai");
    expect(note).toContain("top 8 result messages");
    expect(note).toContain("truncated to 500 chars");
    expect(note).toContain("Nothing else leaves this machine");
    expect(note).toContain("never sent");
  });

  it("states the fallback when disabled", () => {
    const note = privacyNote(disabled);
    expect(note).toContain("LLM assist is off");
    expect(note).toContain("fully locally");
    expect(note).toContain("hybrid vector ranking");
    expect(note).toContain("nothing leaves this machine");
    expect(note).toContain("ANTHROPIC_API_KEY");
  });
});

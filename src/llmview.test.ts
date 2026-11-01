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
  it("shows the LLM plan with match expression and dates", () => {
    const outcome: Pick<AskOutcome, "plan" | "plan_source"> = {
      plan: { match_expr: '"vault" OR "archive"', date_from: "2026-10-01", date_to: "2026-10-31" },
      plan_source: "llm",
    };
    expect(describePlan(outcome)).toBe(
      'FTS: "vault" OR "archive" · dates: 2026-10-01 → 2026-10-31 (planned by LLM)',
    );
  });

  it("marks the fallback and omits missing dates", () => {
    const outcome: Pick<AskOutcome, "plan" | "plan_source"> = {
      plan: { match_expr: '"vault"', date_from: null, date_to: null },
      plan_source: "llm_fallback",
    };
    expect(describePlan(outcome)).toBe(
      'FTS: "vault" (LLM plan unusable — plain term search)',
    );
  });

  it("marks the no-LLM mode", () => {
    const outcome: Pick<AskOutcome, "plan" | "plan_source"> = {
      plan: { match_expr: "", date_from: null, date_to: null },
      plan_source: "no_llm",
    };
    expect(describePlan(outcome)).toBe("FTS: (none) (no LLM configured — plain term search)");
  });
});

describe("privacyNote", () => {
  it("states exactly what is sent when enabled", () => {
    const note = privacyNote(enabled);
    expect(note).toContain("glm-5 via api.z.ai");
    expect(note).toContain("top 8 result messages");
    expect(note).toContain("truncated to 500 chars");
    expect(note).toContain("Nothing else leaves this machine");
  });

  it("states the fallback when disabled", () => {
    const note = privacyNote(disabled);
    expect(note).toContain("LLM assist is off");
    expect(note).toContain("nothing leaves this machine");
    expect(note).toContain("ANTHROPIC_API_KEY");
  });
});

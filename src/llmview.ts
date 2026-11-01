// Pure display helpers for LLM-assisted search (unit tested, no DOM).

/** Mirrors `vault_core::llm::QueryPlan` (serde field names). */
export interface QueryPlan {
  match_expr: string;
  date_from: string | null;
  date_to: string | null;
}

/** Mirrors `vault_core::llm::PlanSource` (snake_case serde). */
export type PlanSource = "llm" | "llm_fallback" | "no_llm";

/** Mirrors `vault_core::llm::LlmStatus` (serde field names). */
export interface LlmStatus {
  enabled: boolean;
  model: string;
  host: string;
  summarize_top: number;
}

/** Mirrors `vault_core::llm::AskOutcome` (serde field names). */
export interface AskOutcome {
  plan: QueryPlan;
  plan_source: PlanSource;
  results: unknown[];
  answer: string | null;
  notes: string[];
}

const PLAN_SOURCE_LABELS: Record<PlanSource, string> = {
  llm: "planned by LLM",
  llm_fallback: "LLM plan unusable — plain term search",
  no_llm: "no LLM configured — plain term search",
};

/** One-line transparency description of the plan actually used. */
export function describePlan(outcome: Pick<AskOutcome, "plan" | "plan_source">): string {
  const parts = [`FTS: ${outcome.plan.match_expr || "(none)"}`];
  const dates = [outcome.plan.date_from, outcome.plan.date_to].filter(Boolean).join(" → ");
  if (dates) parts.push(`dates: ${dates}`);
  return `${parts.join(" · ")} (${PLAN_SOURCE_LABELS[outcome.plan_source]})`;
}

/** Privacy notice stating exactly what leaves the machine in this mode. */
export function privacyNote(status: LlmStatus): string {
  if (!status.enabled) {
    return (
      "LLM assist is off — no usable API key configured. Ask runs as a plain local " +
      "full-text search; nothing leaves this machine. Set ZAI_API_KEY (or GLM_API_KEY) " +
      "in the environment to enable, or ANTHROPIC_API_KEY together with ANTHROPIC_BASE_URL."
    );
  }
  return (
    `LLM assist is on (${status.model} via ${status.host}): your question is sent ` +
    `to the provider to translate it into a search, and afterwards only the top ` +
    `${status.summarize_top} result messages (bodies truncated to 500 chars) are sent ` +
    "for the summary. Nothing else leaves this machine; the API key is read from " +
    "the environment only."
  );
}

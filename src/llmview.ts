// Pure display helpers for LLM-assisted search (unit tested, no DOM).

import { LOCAL_ID, type ProviderMenuItem } from "./settings";

/** Mirrors `vault_core::llm::QueryPlan` (serde field names). */
export interface QueryPlan {
  match_expr: string;
  date_from: string | null;
  date_to: string | null;
}

/** Mirrors `vault_core::llm::PlanSource` (snake_case serde). */
export type PlanSource = "llm" | "llm_fallback" | "no_llm" | "local";

/** Mirrors `vault_core::llm::RankingMode` (snake_case serde). */
export type RankingMode = "fts_only" | "hybrid_fts_vector" | "vector_only";

/** Mirrors `vault_core::llm::LlmStatus` (serde field names). */
export interface LlmStatus {
  enabled: boolean;
  model: string;
  host: string;
  summarize_top: number;
  /** The provider menu (see `src/settings.ts`). */
  providers: ProviderMenuItem[];
  default_provider: string | null;
}

/** Mirrors `vault_core::llm::AskOutcome` (serde field names). */
export interface AskOutcome {
  plan: QueryPlan;
  plan_source: PlanSource;
  ranking: RankingMode;
  results: unknown[];
  answer: string | null;
  notes: string[];
}

const PLAN_SOURCE_LABELS: Record<PlanSource, string> = {
  llm: "planned by LLM",
  llm_fallback: "LLM plan unusable — plain term search",
  no_llm: "no LLM configured — plain term search",
  local: "local full-text — nothing sent to any provider",
};

const RANKING_LABELS: Record<RankingMode, string> = {
  hybrid_fts_vector: "hybrid FTS + local vectors",
  fts_only: "FTS only",
  vector_only: "local vectors only",
};

/** One-line transparency description of the plan actually used. */
export function describePlan(
  outcome: Pick<AskOutcome, "plan" | "plan_source" | "ranking">,
): string {
  const parts = [`FTS: ${outcome.plan.match_expr || "(none)"}`];
  const dates = [outcome.plan.date_from, outcome.plan.date_to].filter(Boolean).join(" → ");
  if (dates) parts.push(`dates: ${dates}`);
  parts.push(`ranked: ${RANKING_LABELS[outcome.ranking]}`);
  return `${parts.join(" · ")} (${PLAN_SOURCE_LABELS[outcome.plan_source]})`;
}

/**
 * Privacy notice stating exactly what leaves the machine when asking with
 * the given provider-menu entry (see `privacyNote` for the default one).
 */
export function privacyNoteFor(item: ProviderMenuItem, summarizeTop: number): string {
  if (!item.available || item.id === LOCAL_ID) {
    return (
      "LLM assist is off for this ask — it runs fully locally as a plain local " +
      "full-text search plus local hybrid vector ranking once embeddings are " +
      "built; nothing leaves this machine. Configure a provider in Settings (z.ai GLM, Anthropic, or a " +
      "custom endpoint), or set ZAI_API_KEY / GLM_API_KEY or ANTHROPIC_API_KEY + " +
      "ANTHROPIC_BASE_URL in the environment."
    );
  }
  const keySentence =
    item.kind === "env"
      ? "the API key is read from the environment only"
      : "the API key is stored outside the app (OS keyring when available) and is never written to the settings file or the archive database";
  return (
    `LLM assist is on (${item.model} via ${item.host}): your question is sent ` +
    `to the provider to translate it into a search, and afterwards only the top ` +
    `${summarizeTop} result messages (bodies truncated to 500 chars) are sent ` +
    `for the summary. Nothing else leaves this machine; embeddings for hybrid ` +
    `ranking are computed locally and never sent; ${keySentence}.`
  );
}

/** Privacy notice for the default provider of a status. */
export function privacyNote(status: LlmStatus): string {
  const item =
    status.providers.find((p) => p.id === status.default_provider) ??
    status.providers.find((p) => p.id === LOCAL_ID);
  if (!item) return "";
  return privacyNoteFor(item, status.summarize_top);
}

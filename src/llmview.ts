// Pure display helpers for LLM-assisted search (unit tested, no DOM).

import { LOCAL_ID, type ProviderMenuItem } from "./settings";

/** Mirrors `vault_core::llm::QueryPlan` (serde field names). */
export interface QueryPlan {
  match_expr: string;
  date_from: string | null;
  date_to: string | null;
  from: string | null;
  has_link: boolean | null;
  extract_links: boolean;
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

/** Mirrors `vault_core::extract::LinkHit` (serde field names). */
export interface LinkHit {
  message_id: number;
  author: string;
  sent_at: string;
  context: string;
}

/** Mirrors `vault_core::extract::CollectedLink` (serde field names). */
export interface CollectedLink {
  url: string;
  hits: LinkHit[];
}

/** Mirrors `vault_core::llm::AskOutcome` (serde field names). */
export interface AskOutcome {
  plan: QueryPlan;
  plan_source: PlanSource;
  ranking: RankingMode;
  results: unknown[];
  answer: string | null;
  notes: string[];
  /** Present when the plan extracted links (older fixtures may omit it). */
  collected?: CollectedLink[];
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
  if (outcome.plan.from) parts.push(`from: ${outcome.plan.from}`);
  if (outcome.plan.has_link) parts.push("links only");
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
      "full-text search, with smarter ranking once the local search index is " +
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
    `for the summary, plus the collected link list (URLs, senders and dates, capped at 50) ` +
    `when your question asks to collect links. Nothing else leaves this machine; the local ` +
    `search index used for ranking is computed on this machine and never sent; ${keySentence}.`
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

// ---------------------------------------------------------------------------
// M5: explicit AI consent (per-session, visible, never persisted)
// ---------------------------------------------------------------------------

/** One-line description of the session AI-consent state (always visible). */
export function consentStatusLine(granted: boolean): string {
  return granted
    ? "AI consent: granted for this session — your question and the top results may be sent to the selected provider."
    : "AI consent: not granted — asks run fully locally; nothing is sent to any provider.";
}

/** The consent prompt shown before the first network ask of a session:
 * states exactly what would leave the machine, for this provider. */
export function consentPromptText(item: ProviderMenuItem, summarizeTop: number): string {
  return (
    `Allow sending for this session? Your question, then the top ${summarizeTop} ` +
    `matching messages (bodies truncated to 500 chars), would be sent to ` +
    `${item.model} via ${item.host} to translate and summarize the ask. ` +
    `When your question asks to collect links, the collected link list ` +
    `(URLs, senders and dates, capped at 50) is sent too.`
  );
}

// ---------------------------------------------------------------------------
// M5: source-linked answers (citations → archived messages)
// ---------------------------------------------------------------------------

/** A citation marker parsed out of an answer (`[1]`, `[2]`, …). */
export interface CitationMarker {
  cite: number;
}

/** Split an answer into plain text runs and citation markers `[n]`. The
 * summary prompt numbers the context blocks `[1]…[top]` in the same order
 * as the returned results, so marker `n` maps to result `n - 1` — the
 * clickable evidence trail from the answer to the original messages. */
export function splitCitations(answer: string): Array<string | CitationMarker> {
  const parts: Array<string | CitationMarker> = [];
  const re = /\[(\d{1,3})\]/g;
  let last = 0;
  let match: RegExpExecArray | null;
  while ((match = re.exec(answer)) !== null) {
    if (match.index > last) parts.push(answer.slice(last, match.index));
    parts.push({ cite: Number(match[1]) });
    last = match.index + match[0].length;
  }
  if (last < answer.length) parts.push(answer.slice(last));
  return parts;
}

/** The evidence-scope caption under an answer: how many matching
 * messages the summary was actually built from (the summarize step
 * sends only the top rows, never the whole result list). */
export function evidenceScopeCaption(n: number): string {
  return `Based on ${n} matching message${n === 1 ? "" : "s"}`;
}

/** The visible note naming citation markers that cannot be linked (no
 * matching source row): they stay plain text, never dead links — the
 * note says so instead of silently downgrading them. */
export function unsupportedCitationNote(numbers: number[]): string {
  const markers = numbers.map((n) => `[${n}]`).join(", ");
  if (numbers.length === 1) {
    return `Citation ${markers} has no matching source and stays plain text.`;
  }
  return `Citations ${markers} have no matching source and stay plain text.`;
}

// ---------------------------------------------------------------------------
// Collected links (ask-collect-links)
// ---------------------------------------------------------------------------

/** Glanceable count line for the collected block. */
export function collectedCountLine(links: CollectedLink[]): string {
  const messages = new Set(links.flatMap((l) => l.hits.map((h) => h.message_id))).size;
  const linkWord = links.length === 1 ? "link" : "links";
  const msgWord = messages === 1 ? "message" : "messages";
  return `${links.length} ${linkWord} from ${messages} ${msgWord}`;
}

/** Text export: one block per link, one line per citing hit. */
export function linksToText(links: CollectedLink[]): string {
  return links
    .map((link) => {
      const hits = link.hits
        .map((h) => `- ${h.author || "unknown"} · ${h.sent_at || "date unknown"} · ${h.context}`)
        .join("\n");
      return `${link.url} (${link.hits.length} hit${link.hits.length === 1 ? "" : "s"})\n${hits}`;
    })
    .join("\n\n");
}

/** JSON export: the raw collected shape, pretty-printed. */
export function linksToJson(links: CollectedLink[]): string {
  return JSON.stringify(links, null, 2);
}

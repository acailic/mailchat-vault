/// Empty-state builder (the four named states: empty search results,
/// no sources, import idle, backup never-run): one inline line-art
/// illustration, a display-size headline, a two-sentence body, and one
/// CTA — the group the staggered entrance animates (app.css
/// `.empty-state`). Pure builder: every dynamic string lands through
/// textContent, tested via happy-dom in emptystate.test.ts.

import { iconEl, type IconName } from "./icons";

/** The illustration symbols live in the index.html sprite beside the
 * UI icons, on a 48-grid (line art, currentColor, no fill/stroke
 * attributes — presentation comes from app.css). */
export type EmptyStateIcon = IconName;

export interface EmptyStateSpec {
  /** Sprite symbol name (drawn without the `i-` prefix). */
  icon: EmptyStateIcon;
  /** Display-size headline: names the place (one short sentence). */
  headline: string;
  /** Two short active sentences: what this place is, what to do next. */
  copy: string;
  /** The single next action. */
  ctaLabel: string;
  /** Optional longer explanation for the CTA's tooltip. */
  ctaTitle?: string;
  onCta: () => void;
  /** Whether the group itself carries role="status" (default true —
   * the announcement convention used across views). Pass false when a
   * sibling status line already announces the state (the search
   * results "0 results" line). */
  live?: boolean;
}

/** Build one empty-state group: illustration → headline → copy → CTA.
 * The illustration is decorative by contract (aria-hidden, like every
 * sprite icon); the headline and copy are the accessible content. */
export function emptyStateEl(spec: EmptyStateSpec): HTMLElement {
  const group = document.createElement("div");
  group.className = "empty-state";
  if (spec.live !== false) group.setAttribute("role", "status");

  const art = iconEl(spec.icon, "empty-state-art");
  const headline = document.createElement("h3");
  headline.className = "empty-state-headline";
  headline.textContent = spec.headline;
  const copy = document.createElement("p");
  copy.className = "empty-state-copy";
  copy.textContent = spec.copy;
  const actions = document.createElement("div");
  actions.className = "empty-state-cta";
  const cta = document.createElement("button");
  cta.type = "button";
  cta.className = "primary";
  cta.textContent = spec.ctaLabel;
  if (spec.ctaTitle) cta.title = spec.ctaTitle;
  cta.addEventListener("click", () => spec.onCta());
  actions.append(cta);

  group.append(art, headline, copy, actions);
  return group;
}

// Drop-to-search onboarding (P0·02, frontend slice): the pure model
// behind the first-run journey — a dropped archive is auto-detected by
// the bounded preview (no mode picker), imported one at a time, and the
// completion state carries only counts the real outcome summary
// reported (inserted/skipped/failed — never an invented total, never a
// faked receipt id; the job result carries none, so the card shows
// none). Unit-tested like src/format.ts helpers: no DOM, no IPC — the
// wiring in src/main.ts feeds the events its own round trips produce.
//
// The rolling-rate ETA estimator is equally honest: it only speaks when
// a total is known AND a rate has been observed, it never goes up as
// progress advances (monotonic), and it can never produce NaN — the
// unknown states render as "estimating…" (ESTIMATING_TEXT). The live
// processed-bytes/phase plumbing is Part 2; this module consumes
// whatever counters the current job events already carry.

import type { ArchiveStats, ChatImportOutcome, ImportPreview } from "./types";

// ---------------------------------------------------------------------------
// Drop → detect → import → completion state machine
// ---------------------------------------------------------------------------

/** The onboarding flow's phases, in pipeline order. */
export type OnboardingPhase =
  | "idle" // nothing dropped yet
  | "detecting" // a path's bounded preview (the auto-detect) is in flight
  | "awaiting-confirm" // the detected preview waits for the user's Confirm
  | "importing" // the confirmed import runs as a background job
  | "completed" // the run finished: real counts, search ready
  | "failed"; // detect or import failed (message says what)

/** What the completion state keeps from a run's outcome summary — the
 * counts the job result actually carried. Deliberately no receipt id:
 * the serialized outcome shape has none (see importOutcomeOf in
 * src/main.ts), and one is never invented here. */
export interface ImportCompletion {
  source: string;
  seen: number;
  inserted: number;
  skipped: number;
  failed: number;
}

/** The machine's state: one active path at a time (the importer is
 * serial — extra drops queue, they never run in parallel) plus the
 * queue of paths waiting behind it. */
export interface OnboardingState {
  phase: OnboardingPhase;
  /** Paths waiting for their turn, in drop order. */
  queue: string[];
  /** The path currently detected/awaiting confirm/importing. */
  active: string | null;
  /** The detected preview for `active` (its `kind` is the detection). */
  preview: ImportPreview | null;
  /** Set when `phase` is "failed"; says why, in plain words. */
  error: string | null;
  /** Set when `phase` is "completed" — the run's real counts. */
  completion: ImportCompletion | null;
}

export function initialOnboardingState(): OnboardingState {
  return {
    phase: "idle",
    queue: [],
    active: null,
    preview: null,
    error: null,
    completion: null,
  };
}

/** One step of the flow, named for the wiring seam that produces it. */
export type OnboardingEvent =
  | { type: "drop"; paths: string[] }
  | { type: "detect-started"; path: string }
  | { type: "detected"; path: string; preview: ImportPreview }
  | { type: "detect-failed"; path: string; message: string }
  | { type: "import-started"; path: string }
  | { type: "import-succeeded"; path: string; outcome: ChatImportOutcome }
  | { type: "import-failed"; path: string; message: string }
  | { type: "import-cancelled"; path: string }
  | { type: "reset" };

/** Reduce one event. Pure: the input state is never mutated. */
export function reduceOnboarding(
  state: OnboardingState,
  event: OnboardingEvent,
): OnboardingState {
  switch (event.type) {
    case "drop": {
      const paths = event.paths.filter((p) => p !== "");
      if (paths.length === 0) return state;
      // A drop mid-flow never interrupts: the new paths join the queue
      // behind whatever the flow is doing (one import at a time).
      return { ...state, queue: [...state.queue, ...paths] };
    }
    case "detect-started":
      return {
        ...state,
        phase: "detecting",
        active: event.path,
        preview: null,
        error: null,
        completion: null,
      };
    case "detected":
      if (state.active !== event.path) return state;
      return { ...state, phase: "awaiting-confirm", preview: event.preview };
    case "detect-failed":
      if (state.active !== event.path) return state;
      return { ...state, phase: "failed", error: event.message };
    case "import-started":
      if (state.active !== event.path) return state;
      return { ...state, phase: "importing" };
    case "import-succeeded":
      if (state.active !== event.path) return state;
      return {
        ...state,
        phase: "completed",
        completion: completionOf(event.outcome),
      };
    case "import-failed":
      if (state.active !== event.path) return state;
      return { ...state, phase: "failed", error: event.message };
    case "import-cancelled":
      if (state.active !== event.path) return state;
      // A cancelled run returns to the start state — nothing was
      // finished, nothing is claimed.
      return { ...initialOnboardingState(), queue: state.queue };
    case "reset":
      return { ...initialOnboardingState(), queue: state.queue };
  }
}

/** The completion view-model from a real outcome: exactly the summary's
 * counts. No receipt id — the result shape carries none. */
export function completionOf(outcome: ChatImportOutcome): ImportCompletion {
  const { source, seen, inserted, skipped, failed } = outcome.summary;
  return { source, seen, inserted, skipped, failed };
}

/** The honest wait line for paths queued behind the flow: imports run
 * one at a time (the bounded importer is serial), so the queue says so
 * instead of implying parallel imports. Empty when nothing waits. */
export function queueWaitLine(waiting: number): string {
  if (waiting <= 0) return "";
  return waiting === 1
    ? "1 more archive waiting — imports run one at a time."
    : `${waiting} more archives waiting — imports run one at a time.`;
}

/** The most frequent sender of the freshly imported archive — the seed
 * that makes the completion search box an example query the user can
 * just run. Ordering is not assumed: the maximum count wins (ties keep
 * the backend's order). Null when stats carry no named author with
 * messages (nothing invented). */
export function topAuthorSeed(stats: ArchiveStats): string | null {
  let top: { author: string; count: number } | null = null;
  for (const a of stats.top_authors) {
    if (a.author === "" || a.count <= 0) continue;
    if (top === null || a.count > top.count) top = a;
  }
  return top ? top.author : null;
}

// ---------------------------------------------------------------------------
// Rolling-rate ETA estimator
// ---------------------------------------------------------------------------

/** What an unknown or not-yet-computable ETA renders as — the honest
 * "still working it out" instead of an invented number. */
export const ESTIMATING_TEXT = "estimating…";

interface RateSample {
  done: number;
  atMs: number;
}

/**
 * ETA from a rolling rate over recent progress samples.
 *
 * Honesty rules (unit-tested):
 * - `etaMs` returns null, never NaN: while no total is known, while
 *   fewer than two samples exist, and while the observed rate is not
 *   positive, there is no defensible number.
 * - The returned ETA never increases while a run's samples keep
 *   arriving (monotonic non-increasing within a run): a later, slower
 *   window may compute a larger estimate, but the user is never shown
 *   an ETA that moved backwards… in time — the estimate only tightens.
 * - `startRun` clears history (and the monotonic clamp) so each import
 *   is estimated on its own samples alone.
 */
export class RollingEta {
  readonly #windowSize: number;
  #samples: RateSample[] = [];
  #lastShownMs: number | null = null;

  constructor(windowSize = 8) {
    this.#windowSize = Math.max(2, windowSize);
  }

  /** Forget previous samples and the monotonic clamp (a new run's rate
   * has nothing to do with the last one's). */
  startRun(): void {
    this.#samples = [];
    this.#lastShownMs = null;
  }

  /** Record one progress observation (`done` = messages processed so
   * far, `atMs` = the observation time). Duplicated or rewound counters
   * are ignored — the rate window only moves forward. */
  sample(done: number, atMs: number): void {
    if (!Number.isFinite(done) || !Number.isFinite(atMs) || done < 0) return;
    const last = this.#samples[this.#samples.length - 1];
    if (last && (atMs < last.atMs || done < last.done)) return;
    if (last && atMs === last.atMs && done === last.done) return;
    this.#samples.push({ done, atMs });
    if (this.#samples.length > this.#windowSize) this.#samples.shift();
  }

  /** The current rate (units per ms) over the sample window; null while
   * unknown (fewer than two distinct samples). */
  #rate(): number | null {
    const first = this.#samples[0];
    const last = this.#samples[this.#samples.length - 1];
    if (!first || !last) return null;
    const units = last.done - first.done;
    const ms = last.atMs - first.atMs;
    if (units <= 0 || ms <= 0) return null;
    return units / ms;
  }

  /** Remaining-ms estimate for `total` units at the rolling rate,
   * clamped monotonically; null whenever honesty beats a number
   * (unknown total, no rate yet, or nothing left to do). */
  etaMs(total: number): number | null {
    const last = this.#samples[this.#samples.length - 1];
    if (!last || !Number.isFinite(total) || total <= 0) return null;
    if (last.done >= total) return 0;
    const rate = this.#rate();
    if (rate === null) return null;
    const estimate = (total - last.done) / rate;
    if (!Number.isFinite(estimate) || estimate < 0) return null;
    const clamped =
      this.#lastShownMs === null ? estimate : Math.min(estimate, this.#lastShownMs);
    this.#lastShownMs = clamped;
    return clamped;
  }
}

/** "estimating…" while `ms` is null; otherwise a compact human
 * duration (never NaN — the same null rule as etaMs). */
export function etaText(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms) || ms < 0) return ESTIMATING_TEXT;
  const seconds = Math.round(ms / 1000);
  if (seconds < 10) return "under 10 seconds";
  if (seconds < 60) return `~${seconds} seconds`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `~${minutes} minute${minutes === 1 ? "" : "s"}`;
  const hours = Math.round(minutes / 60);
  return `~${hours} hour${hours === 1 ? "" : "s"}`;
}

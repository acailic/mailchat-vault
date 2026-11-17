// Motion-vocabulary tests: the committed stylesheets are the contract,
// asserted on the real CSS text (same read-the-files pattern as
// a11y.test.ts, minus the DOM half — these are pure stylesheet checks).
// The vocabulary: --motion-fast/--motion-standard/--motion-slow +
// --ease-out/--ease-press tokens, the four .mv-* utility classes, and
// ONE consolidated prefers-reduced-motion block that removes movement
// while keeping opacity/color feedback.
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const tokensCss = readFileSync(join(here, "styles", "tokens.css"), "utf8");
const appCss = readFileSync(join(here, "styles", "app.css"), "utf8");

/** The body of the first `selector { … }` rule (not comment-safe). */
function ruleBody(css: string, selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const body = css.match(new RegExp(`${escaped}\\s*\\{([^}]*)\\}`))?.[1];
  expect(body, `rule ${selector} exists`).toBeDefined();
  return body ?? "";
}

/** Every comma-separated piece of every `transition:` declaration. */
function transitionPieces(css: string): string[] {
  const pieces: string[] = [];
  for (const match of css.matchAll(/transition:\s*([^;]+);/g)) {
    for (const piece of match[1].split(",")) {
      const p = piece.trim().replace(/\s+/g, " ");
      if (p) pieces.push(p);
    }
  }
  return pieces;
}

// ---------------------------------------------------------------------------
// tokens.css: the shared vocabulary
// ---------------------------------------------------------------------------

describe("motion tokens (tokens.css)", () => {
  it("defines the five tokens with the specified values", () => {
    expect(tokensCss).toMatch(/--motion-fast:\s*120ms;/);
    expect(tokensCss).toMatch(/--motion-standard:\s*180ms;/);
    expect(tokensCss).toMatch(/--motion-slow:\s*240ms;/);
    expect(tokensCss).toMatch(/--ease-out:\s*cubic-bezier\(\s*0\.23,\s*1,\s*0\.32,\s*1\s*\)/);
    expect(tokensCss).toMatch(/--ease-press:\s*cubic-bezier\(/);
  });

  it("never zeroes the durations (reduced motion removes movement, not feedback)", () => {
    expect(tokensCss).not.toMatch(/--motion-[\w-]+:\s*0ms/);
    expect(tokensCss).not.toContain("prefers-reduced-motion");
  });
});

// ---------------------------------------------------------------------------
// app.css: the four movement utility classes
// ---------------------------------------------------------------------------

describe("movement utility classes (app.css)", () => {
  it("ships .mv-enter, .mv-press, .mv-dialog-in, .mv-fade-only", () => {
    for (const cls of [".mv-enter", ".mv-press", ".mv-dialog-in", ".mv-fade-only"]) {
      expect(appCss).toMatch(new RegExp(`${cls.replace(".", "\\.")}\\s*\\{`));
    }
  });

  it("animates only transform/opacity (plus color feedback) in the .mv-* classes", () => {
    const allowed = ["transform", "opacity", "background-color", "color"];
    for (const cls of [".mv-enter", ".mv-press", ".mv-dialog-in", ".mv-fade-only"]) {
      for (const piece of transitionPieces(ruleBody(appCss, cls))) {
        const property = piece.split(" ")[0];
        expect(allowed, `${cls} transitions ${property}`).toContain(property);
      }
    }
  });

  it("gives .mv-enter a fast fade and a standard 4px-rise transform", () => {
    const body = ruleBody(appCss, ".mv-enter");
    expect(body).toMatch(/opacity var\(--motion-fast\) var\(--ease-out\)/);
    expect(body).toMatch(/transform var\(--motion-standard\) var\(--ease-out\)/);
    expect(appCss).toContain("translateY(4px)"); // documented pre-state
  });

  it("presses at scale(0.96) — never below 0.95 — on the press curve", () => {
    expect(ruleBody(appCss, ".mv-press")).toMatch(
      /transform var\(--motion-fast\) var\(--ease-press\)/,
    );
    expect(appCss).toMatch(/\.mv-press:active:not\(:disabled\)\s*\{\s*transform:\s*scale\(0\.96\);/);
    expect(appCss).not.toMatch(/scale\(0\.9[0-4]/);
  });

  it("settles dialogs at scale(0.98) over the slow token", () => {
    const body = ruleBody(appCss, ".mv-dialog-in");
    expect(body).toMatch(/opacity var\(--motion-standard\) var\(--ease-out\)/);
    expect(body).toMatch(/transform var\(--motion-slow\) var\(--ease-out\)/);
    expect(appCss).toContain("scale(0.98)"); // documented pre-state
  });

  it("keeps .mv-fade-only free of any transform transition", () => {
    expect(ruleBody(appCss, ".mv-fade-only")).not.toContain("transform");
  });
});

// ---------------------------------------------------------------------------
// app.css: one complete reduced-motion block
// ---------------------------------------------------------------------------

describe("consolidated reduced-motion block (app.css)", () => {
  const marker = "@media (prefers-reduced-motion: reduce)";
  const occurrences = appCss.split(marker).length - 1;
  const [before, reduced] = appCss.split(marker);

  it("is the single reduced-motion/no-preference query in app.css", () => {
    expect(occurrences).toBe(1);
    expect(appCss).not.toContain("prefers-reduced-motion: no-preference");
  });

  it("strips movement from every movement class and the #nav drawer", () => {
    expect(reduced).toBeDefined();
    for (const selector of [".mv-enter", ".mv-press", ".mv-dialog-in", "#nav"]) {
      expect(reduced, `${selector} covered`).toContain(selector);
    }
    expect(ruleBody(reduced, ".mv-enter")).not.toContain("transform");
    expect(ruleBody(reduced, ".mv-dialog-in")).not.toContain("transform");
    expect(ruleBody(reduced, ".mv-press")).toContain("transition: none");
    expect(ruleBody(reduced, "#nav")).toContain("transition: none");
    expect(ruleBody(reduced, '#app[data-nav="open"] #nav')).toContain("transition: none");
  });

  it("disables the ambient keyframe animations", () => {
    const spin = ruleBody(reduced, ".icon-spin");
    expect(spin).toContain("animation: none");
    const traverse = ruleBody(reduced, ".progress-track.indeterminate::after");
    expect(traverse).toContain("animation: none");
    expect(traverse).toMatch(/left:\s*35%/);
  });

  it("keeps color/opacity transitions outside the block at token durations", () => {
    // The grouped color-feedback rule stays outside the reduced-motion block.
    expect(before).toMatch(
      /\.hit-item,\s*\.chip,\s*button,\s*\.nav-view-btn\s*\{[^}]*transition:\s*background-color var\(--motion-fast\) var\(--ease-out\)/,
    );
    // The ambient traverse animation runs unconditionally outside the block
    // (no no-preference guard) — the block above is what disables it.
    expect(before).toMatch(/animation:\s*progress-traverse/);
  });
});

// ---------------------------------------------------------------------------
// app.css: no raw durations or easing on transition declarations
// ---------------------------------------------------------------------------

describe("tokenized transitions (app.css)", () => {
  it("allows only var(--motion*)/var(--ease*) pieces, `none`, and the 0s visibility companion", () => {
    const pieces = transitionPieces(appCss);
    expect(pieces.length).toBeGreaterThan(0);
    for (const piece of pieces) {
      const shape =
        piece === "none" ||
        /^visibility 0s linear( var\(--motion-(fast|standard|slow)\))?$/.test(piece) ||
        /^[\w-]+ var\(--motion-(fast|standard|slow)\) var\(--ease-(out|press)\)$/.test(piece);
      expect(shape, `transition piece "${piece}"`).toBe(true);
    }
  });
});

// ---------------------------------------------------------------------------
// app.css: the results-list entrance + count line (signature surface)
// ---------------------------------------------------------------------------

describe("results-list motion and numerals (app.css)", () => {
  it("the empty→results entrance runs once as a single group at the standard token", () => {
    const body = ruleBody(appCss, "#results.enter-results");
    expect(body).toMatch(
      /animation:\s*results-enter var\(--motion-standard\) var\(--ease-out\)/,
    );
  });

  it("reduced motion disables the entrance entirely", () => {
    const at = appCss.indexOf("@media (prefers-reduced-motion: reduce)");
    expect(at).toBeGreaterThanOrEqual(0);
    const block = appCss.slice(at);
    expect(block).toMatch(/#results\.enter-results\s*\{\s*animation:\s*none;\s*\}/);
  });

  it("the count lines render tabular numerals", () => {
    expect(ruleBody(appCss, ".page-info")).toMatch(/font-variant-numeric:\s*tabular-nums/);
    expect(ruleBody(appCss, "#status")).toMatch(/font-variant-numeric:\s*tabular-nums/);
  });
});

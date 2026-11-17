// Typography-scale tests (a11y.test.ts pattern: assert against the
// committed CSS text, never claimed constants). tokens.css must ship a
// rem modular ramp (xs…2xl, ratio ≈ 1.25) with the legacy size names
// remapped onto it as aliases, display + density tokens beside it;
// app.css must name a ramp step for every font-size (no ad-hoc px/rem
// literals, `font:` shorthands included), render tabular numerals on
// every numeric selector, and give the three workspace panes one
// shared 8px-grid horizontal padding token.
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const tokensCss = readFileSync(join(here, "styles", "tokens.css"), "utf8");
const appCss = readFileSync(join(here, "styles", "app.css"), "utf8");

// The light-default :root block owns every type token (themes only
// override palette colors), same block-slicing idea as a11y.test.ts.
const rootBlock = tokensCss.slice(
  tokensCss.indexOf(":root {"),
  tokensCss.indexOf(':root[data-theme="dark"]'),
);

/** Value of one --custom-property inside the :root block. */
function token(name: string): string {
  return rootBlock.match(new RegExp(`--${name}:\\s*([^;]+);`))?.[1]?.trim() ?? "";
}

/** Extract one app.css rule block by its exact selector text. */
function rule(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return appCss.match(new RegExp(`${escaped}\\s*\\{[^}]*\\}`, "s"))?.[0] ?? "";
}

// ---------------------------------------------------------------------------
// The modular ramp (tokens.css :root)
// ---------------------------------------------------------------------------

describe("modular type scale (tokens.css)", () => {
  // The legacy-to-ramp mapping pins the steps: 12px→xs, 13px→sm,
  // 15px→base, 18px→lg, 24px→xl (2xl = xl × 1.25). Exact values, so
  // any drift fails here.
  const ramp: Record<string, number> = {
    xs: 0.75,
    sm: 0.8125,
    base: 0.9375,
    lg: 1.125,
    xl: 1.5,
    "2xl": 1.875,
  };

  it("defines every step in rem at the pinned size", () => {
    for (const [step, rem] of Object.entries(ramp)) {
      expect(token(`text-${step}`), `--text-${step}`).toBe(`${rem}rem`);
    }
  });

  it("keeps the ratio near 1.25 between adjacent steps", () => {
    const rems = Object.values(ramp);
    const ratios = rems.slice(1).map((rem, i) => rem / rems[i]);
    for (const ratio of ratios) {
      expect(ratio).toBeGreaterThanOrEqual(1.05);
      expect(ratio).toBeLessThanOrEqual(1.35);
    }
    const geometricMean = ratios.reduce((acc, r) => acc * r, 1) ** (1 / ratios.length);
    expect(geometricMean).toBeGreaterThanOrEqual(1.15);
    expect(geometricMean).toBeLessThanOrEqual(1.3);
  });

  it("remaps the legacy size names onto ramp steps as aliases", () => {
    expect(token("text-meta")).toBe("var(--text-sm)");
    expect(token("text-body")).toBe("var(--text-base)");
    expect(token("text-section")).toBe("var(--text-lg)");
    expect(token("text-page")).toBe("var(--text-xl)");
  });

  it("defines the display variant (weight 600–700, tighter tracking)", () => {
    const weight = Number(token("text-display-weight"));
    expect(weight).toBeGreaterThanOrEqual(600);
    expect(weight).toBeLessThanOrEqual(700);
    expect(token("text-display-tracking")).toMatch(/^-0\.0\d+em$/);
  });

  it("defines the two density line-heights", () => {
    expect(token("lh-comfortable")).toBe("1.5");
    expect(token("lh-compact")).toBe("1.35");
  });

  it("retires the pre-ramp --text-body-lh token", () => {
    expect(rootBlock).not.toContain("--text-body-lh");
  });
});

// ---------------------------------------------------------------------------
// Scale hygiene: every font-size in app.css names a ramp step
// ---------------------------------------------------------------------------

describe("font-size hygiene (app.css)", () => {
  const declarations = [
    ...appCss.matchAll(/(font|font-size)\s*:\s*([^;]+);/g),
  ].map((m) => ({ property: m[1], value: m[2].trim() }));

  it("has only font declarations to inspect", () => {
    expect(declarations.length).toBeGreaterThan(10);
  });

  it("leaves no raw px or rem literal in any font or font-size declaration", () => {
    const offenders = declarations.filter((d) => /\d+(?:\.\d+)?(?:px|rem)\b/.test(d.value));
    expect(offenders, JSON.stringify(offenders)).toEqual([]);
  });

  it("sets every font-size from a --text-* ramp token", () => {
    const offenders = declarations
      .filter((d) => d.property === "font-size")
      .filter((d) => !/^var\(--text-(xs|sm|base|lg|xl|2xl|meta|body|section|page)\)$/.test(d.value));
    expect(offenders, JSON.stringify(offenders)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// Tabular numerals on the numeric UI
// ---------------------------------------------------------------------------

describe("tabular numerals (app.css)", () => {
  const numericSelectors = [
    ".meta",
    ".page-info",
    "#stats",
    ".nav-badge",
    ".stat-table td.num",
    ".media-usage-table",
    ".day-bar-count",
    ".day-chart-axis",
    ".jobs-list",
  ];

  it("defines the .tnum utility", () => {
    expect(rule(".tnum")).toMatch(/font-variant-numeric:\s*tabular-nums/);
  });

  it.each(numericSelectors)("%s renders tabular digits", (selector) => {
    const block = rule(selector);
    expect(block, `rule for ${selector}`).not.toBe("");
    expect(block).toMatch(/font-variant-numeric:\s*tabular-nums/);
  });
});

// ---------------------------------------------------------------------------
// Display variant, density line-heights, shared pane edge
// ---------------------------------------------------------------------------

describe("hierarchy and alignment (app.css)", () => {
  it("applies the display variant to pane headers and empty-state headlines", () => {
    for (const selector of [".view-heading", ".first-run-title", "#reading-empty"]) {
      const block = rule(selector);
      expect(block, `rule for ${selector}`).not.toBe("");
      expect(block).toContain("var(--text-display-weight)");
      expect(block).toContain("var(--text-display-tracking)");
    }
  });

  it("sets body and message-row line-height from the density tokens", () => {
    const body = appCss.match(/(?:^|\})\s*body\s*\{[^}]*\}/ms)?.[0] ?? "";
    expect(body).toMatch(/line-height:\s*var\(--lh-comfortable\)/);
    expect(rule(".hit-item")).toMatch(/line-height:\s*var\(--lh-comfortable\)/);
  });

  it("gives the three panes one shared 8px-grid horizontal padding token", () => {
    expect(token("pane-pad-x")).toBe("var(--sp-2)");
    for (const selector of ["#results-col", ".hit-item", "#detail"]) {
      const block = rule(selector);
      expect(block, `rule for ${selector}`).not.toBe("");
      expect(block).toMatch(new RegExp(`padding:[^;]*var\\(--pane-pad-x\\)`));
    }
  });
});

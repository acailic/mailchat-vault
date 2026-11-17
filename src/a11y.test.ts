// @vitest-environment happy-dom
// Accessibility-convention tests for the a11y-polish fixes: the WCAG
// contrast math for the placeholder pair is computed from the committed
// token hex values in tokens.css (never from claimed constants), the CSS
// rules for the four fixes are asserted on the real app.css, and the
// #nav-toggle DOM/tab order is asserted against the real index.html
// (same load pattern as main.test.ts: body markup into happy-dom,
// scripts stripped, nothing executed).
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { Window } from "happy-dom";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const tokensCss = readFileSync(join(here, "styles", "tokens.css"), "utf8");
const appCss = readFileSync(join(here, "styles", "app.css"), "utf8");
const mainTs = readFileSync(join(here, "main.ts"), "utf8");
const indexHtml = readFileSync(join(here, "..", "index.html"), "utf8");

/** Parse `selector { body }` pairs (comments stripped, brace-safe). */
function cssRules(css: string): { selector: string; body: string }[] {
  const noComments = css.replace(/\/\*[\s\S]*?\*\//g, "");
  return [...noComments.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((m) => ({
    selector: m[1].trim(),
    body: m[2],
  }));
}

// ---------------------------------------------------------------------------
// WCAG contrast math over the committed token values
// ---------------------------------------------------------------------------

/** Parse the `--name: #rrggbb` custom properties of one tokens.css block. */
function tokenBlock(from: string, to: string): Record<string, string> {
  const start = tokensCss.indexOf(from);
  const end = tokensCss.indexOf(to, start);
  const block = tokensCss.slice(start, end);
  const vars: Record<string, string> = {};
  for (const match of block.matchAll(/--([\w-]+):\s*(#[0-9a-fA-F]{6})\b/g)) {
    vars[match[1]] = match[2].toLowerCase();
  }
  return vars;
}

const light = tokenBlock(":root {", ':root[data-theme="dark"]');
const dark = tokenBlock(':root[data-theme="dark"]', ':root[data-theme="vesper"]');
const vesper = tokenBlock(':root[data-theme="vesper"]', '/* "System" theme');

/** WCAG 2.x relative luminance of a #rrggbb color. */
function luminance(hex: string): number {
  const linear = [1, 3, 5]
    .map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
    .map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

/** WCAG 2.x contrast ratio between two #rrggbb colors (≥ 1). */
function contrast(fg: string, bg: string): number {
  const [l1, l2] = [luminance(fg), luminance(bg)].sort((a, b) => b - a);
  return (l1 + 0.05) / (l2 + 0.05);
}

describe("placeholder contrast (computed from committed token values)", () => {
  it("meets 4.5:1 on panel and panel-2 in every theme", () => {
    for (const [themeName, theme] of [
      ["light", light],
      ["dark", dark],
      ["vesper", vesper],
    ] as const) {
      for (const surface of ["panel", "panel-2"] as const) {
        const ratio = contrast(theme["muted"], theme[surface]);
        expect(ratio, `${themeName} --muted on --${surface}`).toBeGreaterThanOrEqual(4.5);
      }
    }
  });

  it("records the measured ratios beside the ::placeholder rule (tokens.css convention)", () => {
    const at = appCss.indexOf("::placeholder");
    expect(at).toBeGreaterThanOrEqual(0);
    // The comment next to the pair must carry today's measured values, so
    // retuning a token forces the recorded numbers to be re-measured.
    const comment = appCss.slice(Math.max(0, at - 500), at);
    expect(comment).toContain(`${contrast(light["muted"], light["panel"]).toFixed(2)}:1`);
    expect(comment).toContain(`${contrast(light["muted"], light["panel-2"]).toFixed(2)}:1`);
    expect(comment).toContain(`${contrast(dark["muted"], dark["panel"]).toFixed(2)}:1`);
    expect(comment).toContain(`${contrast(dark["muted"], dark["panel-2"]).toFixed(2)}:1`);
    expect(comment).toContain(`${contrast(vesper["muted"], vesper["panel"]).toFixed(2)}:1`);
    expect(comment).toContain(`${contrast(vesper["muted"], vesper["panel-2"]).toFixed(2)}:1`);
  });
});

// ---------------------------------------------------------------------------
// CSS rules for the four fixes
// ---------------------------------------------------------------------------

describe("a11y CSS rules (app.css)", () => {
  it("styles ::placeholder with the muted token at full opacity", () => {
    expect(appCss).toMatch(/::placeholder\s*\{\s*color:\s*var\(--muted\);\s*opacity:\s*1;\s*\}/);
  });

  it("pins every native checkbox to the accent token", () => {
    expect(appCss).toMatch(
      /input\[type="checkbox"\]\s*\{\s*accent-color:\s*var\(--accent\);\s*\}/,
    );
  });

  it("gives .row-select a 24px hit area without shifting the row", () => {
    expect(appCss).toMatch(/\.row-select\s*\{[^}]*padding:\s*4px;[^}]*margin:\s*-4px;/s);
  });

  it("sizes .icon-btn as a 32px target", () => {
    const rule = appCss.match(/\.icon-btn\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(rule).toMatch(/width:\s*32px/);
    expect(rule).toMatch(/height:\s*32px/);
  });
});

// ---------------------------------------------------------------------------
// Topbar Menu tab order (the real index.html)
// ---------------------------------------------------------------------------

/** Load the real index.html body markup into a fresh happy-dom window. */
function loadIndexBody() {
  const win = new Window();
  const html = readFileSync(join(here, "..", "index.html"), "utf8");
  const start = html.indexOf("<body>") + "<body>".length;
  const end = html.lastIndexOf("</body>");
  win.document.body.innerHTML = html
    .slice(start, end)
    .replace(/<script[\s\S]*?<\/script>/g, "");
  return win.document;
}

describe("topbar Menu tab order (index.html)", () => {
  it("places #nav-toggle after #main inside #content (view content first)", () => {
    const doc = loadIndexBody();
    const content = doc.querySelector("#content");
    const main = doc.querySelector("#main");
    const toggle = doc.querySelector("#nav-toggle");
    expect(content?.contains(toggle!)).toBe(true);
    expect(content?.contains(main!)).toBe(true);
    expect(main!.compareDocumentPosition(toggle!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("keeps #topbar first inside #content (the header stays the top row)", () => {
    const doc = loadIndexBody();
    expect(doc.querySelector("#content")!.firstElementChild!.id).toBe("topbar");
  });

  it("uses no positive tabindex", () => {
    const doc = loadIndexBody();
    for (const el of doc.querySelectorAll("[tabindex]")) {
      expect(Number(el.getAttribute("tabindex"))).toBeLessThanOrEqual(0);
    }
  });
});

// ---------------------------------------------------------------------------
// Global focus ring (identity pass): one accent rule, no overrides
// ---------------------------------------------------------------------------

describe("global focus ring (app.css, tokens.css)", () => {
  const rules = cssRules(appCss);

  it("sets exactly one outline on a focus state — the global accent rule", () => {
    const outlineOnFocus = rules.filter(
      (r) => r.selector.includes(":focus") && r.body.includes("outline:"),
    );
    expect(outlineOnFocus).toHaveLength(1);
    expect(outlineOnFocus[0].selector).toBe(":focus-visible");
    expect(outlineOnFocus[0].body).toMatch(/outline:\s*2px solid var\(--accent\)/);
    expect(outlineOnFocus[0].body).toMatch(/outline-offset:\s*([1-9]\d*)px/);
  });

  it("retires the separate --focus-ring token from every theme block", () => {
    expect(tokensCss).not.toContain("--focus-ring");
  });
});

// ---------------------------------------------------------------------------
// Accent-usage invariants (the rule documented beside the palette)
// ---------------------------------------------------------------------------

describe("accent-usage invariants (app.css)", () => {
  const rules = cssRules(appCss);

  it("colors text with the accent only on interactive affordances", () => {
    // Allowlist per the tokens.css rule: link/btn/chip/summary/tab/nav
    // families, hit rows, thread rows, ask-source rows, icon buttons.
    // The property match is exact (word-boundary, var(--accent) value):
    // border-color, accent-color, and var(--accent-fg) are not text.
    const allow = ["btn", "button", "link", "chip", "summary", "hit", "tab", "nav-view", "thread-row", "ask-source"];
    const textAccent = (body: string) => /(?<![-\w])color:\s*var\(--accent\)\s*;/.test(body);
    const violators = rules.filter(
      (r) => textAccent(r.body) && !allow.some((t) => r.selector.includes(t)),
    );
    expect(violators.map((r) => r.selector)).toEqual([]);
    // The invariant must bite: at least the link, citation, and
    // ask-source affordances exercise it.
    expect(rules.filter((r) => textAccent(r.body)).length).toBeGreaterThanOrEqual(3);
  });

  it("fills the accent background only on primary, badge, and data rules", () => {
    // Allowlist per the tokens.css rule: primary buttons, small badges,
    // chart ramps and data fills (day histogram, usage/progress bars).
    const allow = ["primary", "badge", "day-bar", "usage-bar", "progress"];
    const fillAccent = (body: string) => /(?<![-\w])background:\s*var\(--accent\)\s*;/.test(body);
    const violators = rules.filter(
      (r) => fillAccent(r.body) && !allow.some((t) => r.selector.includes(t)),
    );
    expect(violators.map((r) => r.selector)).toEqual([]);
    expect(rules.filter((r) => fillAccent(r.body)).length).toBeGreaterThanOrEqual(4);
  });
});

// ---------------------------------------------------------------------------
// Branded first paint: splash markup, theme script, hide seam
// ---------------------------------------------------------------------------

describe("branded first paint (index.html, main.ts)", () => {
  it("ships #boot-splash as an aria-hidden direct body child outside #app/#content", () => {
    const doc = loadIndexBody();
    const splash = doc.querySelector("#boot-splash");
    expect(splash).toBeTruthy();
    expect(splash!.getAttribute("aria-hidden")).toBe("true");
    expect(splash!.parentElement).toBe(doc.body);
    expect(doc.querySelector("#app")!.contains(splash!)).toBe(false);
    expect(doc.querySelector("#content")!.contains(splash!)).toBe(false);
    // #topbar stays the first child of #content (splash adds no chrome).
    expect(doc.querySelector("#content")!.firstElementChild!.id).toBe("topbar");
  });

  it("carries the inline pre-paint theme script with splash backstops in the head", () => {
    const head = indexHtml.slice(0, indexHtml.indexOf("<body>"));
    // The theme mirror of resolveTheme: localStorage mcv.themeVesper
    // resolved into the root data-theme before any module loads.
    expect(head).toMatch(/<script>[\s\S]*mcv\.themeVesper[\s\S]*dataset\.theme[\s\S]*<\/script>/);
    // Backstops so a failed module load can never strand the splash:
    // a load listener plus a hard 2-second timeout in the same script.
    expect(head).toMatch(/<script>[\s\S]*getElementById\("boot-splash"\)[\s\S]*<\/script>/);
    expect(head).toMatch(/setTimeout\(\s*\w+,\s*2000\s*\)/);
    expect(head).toMatch(/addEventListener\(\s*"load"/);
    // Reduced motion swaps instantly: the fade transition is disabled.
    expect(head).toMatch(/prefers-reduced-motion:\s*reduce[\s\S]*#boot-splash\.hide\s*\{\s*transition:\s*none/);
  });

  it("hides the splash from main.ts at the first-render seam", () => {
    expect(mainTs).toContain('getElementById("boot-splash")');
    expect(mainTs).toMatch(/renderAll\(\);\s*hideBootSplash\(\);/);
  });
});

// ---------------------------------------------------------------------------
// Uniform hover, press, disabled states across every clickable family
// ---------------------------------------------------------------------------

describe("uniform hover, press, disabled states (app.css)", () => {
  const families = [
    "button",
    ".chip",
    ".nav-view-btn",
    ".settings-nav-btn",
    ".workspace-tab-btn",
    "summary",
    ".hit-item",
    ".thread-row.link",
    ".ask-source",
  ];
  const rules = cssRules(appCss);
  const reduced = appCss.slice(appCss.indexOf("@media (prefers-reduced-motion: reduce)"));
  const reducedRules = cssRules(reduced);

  it("gives every clickable family a visible hover change", () => {
    for (const family of families) {
      expect(
        rules.some((r) => r.selector.includes(`${family}:hover`)),
        `${family} has a :hover rule`,
      ).toBe(true);
    }
  });

  it("presses every clickable family through the .mv-press scale", () => {
    for (const family of families) {
      const press = rules.find((r) => r.selector.includes(`${family}:active`));
      expect(press, `${family} has an :active press rule`).toBeTruthy();
      expect(press!.body).toMatch(/transform:\s*scale\(0\.96\)/);
    }
  });

  it("keeps color feedback but removes the press movement under reduced motion", () => {
    for (const family of families) {
      const override = reducedRules.find((r) => r.selector.includes(`${family}:active`));
      expect(override, `${family} reduced-motion :active override`).toBeTruthy();
      expect(override!.body).toMatch(/transform:\s*none/);
      const transition = reducedRules.find((r) => r.selector.includes(family) && r.body.includes("transition:"));
      expect(transition, `${family} keeps a color transition under reduce`).toBeTruthy();
      expect(transition!.body).not.toContain("transform var(");
    }
  });

  it("keeps disabled controls muted and not-allowed", () => {
    // Every disable-able family is <button>-based (chips, nav/settings/
    // workspace tabs, ask-source rows), so one rule covers them all.
    const disabled = rules.find((r) => r.selector === "button:disabled");
    expect(disabled).toBeTruthy();
    expect(disabled!.body).toMatch(/color:\s*var\(--muted\)/);
    expect(disabled!.body).toMatch(/cursor:\s*not-allowed/);
  });
});

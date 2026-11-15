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
const dark = tokenBlock(':root[data-theme="dark"]', '/* "System" theme');

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
  it("meets 4.5:1 on panel and panel-2 in both themes", () => {
    for (const [themeName, theme] of [["light", light], ["dark", dark]] as const) {
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

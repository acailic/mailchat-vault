// @vitest-environment happy-dom
// Empty-state tests (src/emptystate.ts + the wiring contract): the
// builder's DOM shape (illustration, headline, copy, CTA — decorative
// art, textContent strings, role="status" convention), and the
// stylesheet contract in the a11y.test.ts style — regex over the real
// app.css/tokens.css text: the staggered entrance consumes the motion
// tokens, and the consolidated reduced-motion block declares no
// animation so the identical group renders instantly. The per-view
// wiring (search/sources/import/backup + first-run dismissal) lives in
// main.test.ts and accountsview.test.ts.
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { emptyStateEl } from "./emptystate";

const here = dirname(fileURLToPath(import.meta.url));
const tokensCss = readFileSync(join(here, "styles", "tokens.css"), "utf8");
const appCss = readFileSync(join(here, "styles", "app.css"), "utf8");

// ---------------------------------------------------------------------------
// Builder DOM (happy-dom)
// ---------------------------------------------------------------------------

describe("emptyStateEl (the illustration → headline → copy → CTA group)", () => {
  const build = (over: Partial<Parameters<typeof emptyStateEl>[0]> = {}) =>
    emptyStateEl({
      icon: "empty-search",
      headline: "No messages match",
      copy: "Your search ran against the whole archive and found nothing. Try fewer words.",
      ctaLabel: "Clear search",
      onCta: () => {},
      ...over,
    });

  it("renders the four parts in order with the sprite illustration decorative", () => {
    const group = build();
    const parts = [...group.children].map((c) => c.className);
    expect(parts).toEqual([
      "icon empty-state-art",
      "empty-state-headline",
      "empty-state-copy",
      "empty-state-cta",
    ]);
    const art = group.querySelector(".empty-state-art")!;
    expect(art.getAttribute("aria-hidden")).toBe("true");
    expect(art.querySelector("use")?.getAttribute("href")).toBe("#i-empty-search");
    expect(group.querySelector(".empty-state-headline")!.textContent).toBe(
      "No messages match",
    );
    expect(group.querySelector(".empty-state-copy")!.textContent).toContain(
      "whole archive",
    );
  });

  it("wires exactly one CTA to the handler", () => {
    const onCta = vi.fn();
    const group = build({ onCta });
    const buttons = group.querySelectorAll("button");
    expect(buttons.length).toBe(1);
    expect(buttons[0].textContent).toBe("Clear search");
    buttons[0].click();
    buttons[0].click();
    expect(onCta).toHaveBeenCalledTimes(2);
  });

  it("carries role=status by default and drops it for sibling-announced states", () => {
    expect(build().getAttribute("role")).toBe("status");
    expect(build({ live: false }).getAttribute("role")).toBeNull();
  });

  it("lands every string through textContent (nothing interpolated as markup)", () => {
    const group = build({
      headline: '<script>x</script>',
      copy: "<img src=x onerror=1>",
      ctaLabel: "<b>Go</b>",
    });
    expect(group.querySelector("script")).toBeNull();
    expect(group.querySelector("img")).toBeNull();
    expect(group.querySelector("b")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Stylesheet contract (app.css / tokens.css, the a11y.test.ts pattern)
// ---------------------------------------------------------------------------

describe("empty-state styles (app.css)", () => {
  const marker = "@media (prefers-reduced-motion: reduce)";
  const [before, reduced] = appCss.split(marker);

  it("heads the group with the display step of the type ramp", () => {
    expect(tokensCss).toMatch(/--text-2xl:\s*1\.875rem;/);
    const rule = appCss.match(/\.empty-state-headline\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(rule).toMatch(/font-size:\s*var\(--text-2xl\)/);
    expect(rule).toMatch(/font-weight:\s*var\(--text-display-weight\)/);
  });

  it("staggers children over the slow token with stepped calc delays", () => {
    expect(appCss).toMatch(
      /@keyframes empty-state-in\s*\{[^}]*opacity:\s*0;[^}]*transform:\s*translateY\(4px\);[^}]*\}/s,
    );
    expect(appCss).toMatch(
      /\.empty-state > \*\s*\{\s*animation:\s*empty-state-in var\(--motion-slow\) var\(--ease-out\) both;/s,
    );
    // Illustration → headline → copy → CTA: three delay steps, all
    // derived from --motion-slow (no raw durations).
    for (const step of ["calc(var(--motion-slow) / 2)", "var(--motion-slow)", "calc(var(--motion-slow) * 1.5)"]) {
      expect(appCss).toContain(step);
    }
  });

  it("animates only opacity and transform in the entrance keyframes", () => {
    const keyframes = appCss.match(/@keyframes empty-state-in\s*\{([\s\S]*?)\n\}/)?.[1] ?? "";
    const animated = [...keyframes.matchAll(/(opacity|transform|[a-z-]+)\s*:/g)].map((m) => m[1]);
    expect(new Set(animated)).toEqual(new Set(["opacity", "transform"]));
  });

  it("declares no entrance animation on the reduced-motion path", () => {
    expect(reduced).toBeDefined();
    const rule = reduced.match(/\.empty-state > \*\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(rule).toContain("animation: none");
    // The entrance rules themselves live outside the reduce block only
    // as token-consuming declarations — no second reduced-motion query
    // exists (the consolidated block is the single source of truth).
    expect(appCss.split(marker).length - 1).toBe(1);
    expect(before).not.toContain("prefers-reduced-motion: no-preference");
  });
});

// ---------------------------------------------------------------------------
// No filler copy in the shipped views (acceptance: product writing only)
// ---------------------------------------------------------------------------

describe("no lorem or TODO filler in views", () => {
  const viewFiles = [
    join(here, "..", "index.html"),
    join(here, "emptystate.ts"),
    join(here, "main.ts"),
    join(here, "accountsview.ts"),
    join(here, "backupview.ts"),
    join(here, "render2.ts"),
  ];

  it("keeps every shipped view free of placeholder copy", () => {
    for (const file of viewFiles) {
      const text = readFileSync(file, "utf8");
      expect(text, file).not.toMatch(/lorem/i);
      expect(text, file).not.toMatch(/\bTODO\b/);
    }
  });
});

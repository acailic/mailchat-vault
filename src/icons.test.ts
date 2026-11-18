// @vitest-environment happy-dom
// Icon-family contract (identity pass): one sprite, one style. The
// tests below parse the real index.html sprite and the real app.css so
// a mixed-style icon can never land silently:
//   - every <symbol> sits on the same 24×24 viewBox grid,
//   - the symbol set and the IconName union in icons.ts match exactly
//     (no orphan symbols, no unreachable names),
//   - the .icon rule supplies the family styling (stroke currentColor,
//     1.5px, no fills, round caps and joins).
// Markup audit (index.html + the src/*.ts renderers) for emoji or
// text-glyph icons on interactive elements: none found — the arrows in
// the import/menu-path help text are words in prose, the ↑/↓ inside
// <kbd> are literal key labels, and the ▸/▾/‹ carets are CSS-generated
// disclosure markers mirroring the native <summary> marker (matching
// the documented affordance language), not sprite-family violations.
// Violations, when a future audit finds them, are fixed in the same
// pass that introduces them; these tests then guard the contract.
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { icon, iconEl } from "./icons";

const here = dirname(fileURLToPath(import.meta.url));
const indexHtml = readFileSync(join(here, "..", "index.html"), "utf8");
const appCss = readFileSync(join(here, "styles", "app.css"), "utf8");
const iconsTs = readFileSync(join(here, "icons.ts"), "utf8");

describe("icon helper", () => {
  it("references the sprite symbol with decorative attributes", () => {
    expect(icon("archive")).toBe(
      '<svg class="icon" aria-hidden="true" focusable="false"><use href="#i-archive"></use></svg>',
    );
  });

  it("appends extra classes when given", () => {
    expect(icon("spinner", "icon-spin")).toContain('class="icon icon-spin"');
    expect(icon("spinner", "icon-spin")).toContain('#i-spinner');
  });

  it("builds a DOM element with the same markup", () => {
    const el = iconEl("close");
    expect(el.getAttribute("class")).toBe("icon");
    expect(el.getAttribute("aria-hidden")).toBe("true");
    expect(el.querySelector("use")?.getAttribute("href")).toBe("#i-close");
  });
});

describe("icon sprite family contract (index.html)", () => {
  /** The sprite symbols as { id, attrs } pairs. */
  const symbols = [...indexHtml.matchAll(/<symbol\b([^>]*)>/g)].map((m) => {
    const attrs = m[1];
    return {
      id: attrs.match(/id="i-([\w-]+)"/)?.[1] ?? "",
      viewBox: attrs.match(/viewBox="([^"]*)"/)?.[1] ?? "",
    };
  });

  it("has a sprite with symbols to check", () => {
    expect(symbols.length).toBeGreaterThan(0);
  });

  it("draws every symbol on the same 24×24 grid", () => {
    for (const symbol of symbols) {
      expect(symbol.viewBox, `symbol i-${symbol.id}`).toBe("0 0 24 24");
    }
  });

  it("keeps the symbol set and the IconName union exactly equal", () => {
    const unionBlock = iconsTs.match(/export type IconName =([\s\S]*?);/)?.[1] ?? "";
    const unionNames = [...unionBlock.matchAll(/"([\w-]+)"/g)].map((m) => m[1]);
    expect(unionNames.length).toBeGreaterThan(0);
    const spriteNames = symbols.map((s) => s.id);
    expect([...new Set(spriteNames)].length, "no duplicate symbols").toBe(spriteNames.length);
    expect(spriteNames.filter((id) => !unionNames.includes(id)), "symbols missing from IconName").toEqual([]);
    expect(unionNames.filter((name) => !spriteNames.includes(name)), "IconName entries with no symbol").toEqual([]);
    expect(new Set(spriteNames).size).toBe(new Set(unionNames).size);
  });
});

describe("icon family styling (app.css .icon)", () => {
  it("supplies the stroke contract: currentColor, 1.5px, unfilled, round joins", () => {
    const body = appCss.match(/\.icon\s*\{([^}]*)\}/)?.[1] ?? "";
    expect(body).toMatch(/fill:\s*none/);
    expect(body).toMatch(/stroke:\s*currentColor/);
    expect(body).toMatch(/stroke-width:\s*1\.5/);
    expect(body).toMatch(/stroke-linecap:\s*round/);
    expect(body).toMatch(/stroke-linejoin:\s*round/);
  });
});

// @vitest-environment happy-dom
import { describe, expect, it } from "vitest";
import { icon, iconEl } from "./icons";

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

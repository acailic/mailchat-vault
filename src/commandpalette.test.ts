// @vitest-environment happy-dom
// Unit tests for the command palette (src/commandpalette.ts): the
// registry, the fuzzy matcher (ranking + matched-letter indices), the
// recents persistence (mcv.paletteRecents, capped), the Cmd/Ctrl+K open,
// the keyboard model (arrows / aria-activedescendant tracking, Enter,
// Escape + focus restore, Tab focus trap), platform-aware labels, and
// the committed CSS (opacity-only pointer enter, reduced-motion path).
// Same harness pattern as main.test.ts: a fresh happy-dom window per
// test with the module re-imported, so document listeners and
// localStorage never leak between tests.
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { Window } from "happy-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CommandPalette } from "./commandpalette";

let win: Window;
let palette: CommandPalette;
let searches: string[];
let ran: string[];

type Module = typeof import("./commandpalette");
let mod: Module;

/** Fresh window + fresh module instance + a two-command registry. */
async function fresh(platform = "Linux x86_64"): Promise<void> {
  win = new Window();
  vi.stubGlobal("window", win);
  vi.stubGlobal("document", win.document);
  vi.stubGlobal("navigator", win.navigator);
  vi.resetModules();
  mod = await import("./commandpalette");
  mod.resetCommandRegistry();
  searches = [];
  ran = [];
  mod.registerCommand({ id: "theme", title: "Set theme: Dark", section: "actions", run: () => ran.push("theme") });
  mod.registerCommand({ id: "search", title: "Go to: Search", section: "views", run: () => ran.push("search") });
  palette = mod.initCommandPalette({
    onSearch: (q) => searches.push(q),
    platform,
  });
}

function pressKey(target: EventTarget, key: string, init: KeyboardEventInit = {}): void {
  target.dispatchEvent(
    new win.KeyboardEvent("keydown", { key, bubbles: true, cancelable: true, ...init }),
  );
}

function openViaKeyboard(ctrl = true): void {
  pressKey(win.document, "k", ctrl ? { ctrlKey: true } : { metaKey: true });
}

function type(text: string): void {
  const input = win.document.querySelector<HTMLInputElement>("#command-palette-input")!;
  input.value = text;
  input.dispatchEvent(new win.Event("input", { bubbles: true }));
}

function options(): HTMLElement[] {
  return [...win.document.querySelectorAll("#command-palette-list [role='option']")] as HTMLElement[];
}

beforeEach(() => fresh());
afterEach(() => vi.unstubAllGlobals());

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

describe("command registry", () => {
  it("registers, lists in order, and looks up by id", async () => {
    await fresh();
    expect(mod.listCommands().map((c) => c.id)).toEqual(["theme", "search"]);
    expect(mod.lookupCommand("theme")?.title).toBe("Set theme: Dark");
    expect(mod.lookupCommand("nope")).toBeUndefined();
  });

  it("re-registering an id replaces it in place", async () => {
    await fresh();
    mod.registerCommand({ id: "search", title: "Go to: Search view", section: "views", run: () => {} });
    expect(mod.listCommands().map((c) => c.title)).toEqual(["Set theme: Dark", "Go to: Search view"]);
    expect(mod.listCommands()).toHaveLength(2);
  });

  it("resetCommandRegistry empties the registry", async () => {
    await fresh();
    mod.resetCommandRegistry();
    expect(mod.listCommands()).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------
// Fuzzy matching
// ---------------------------------------------------------------------------

describe("fuzzyMatch", () => {
  it("matches case-insensitively and reports the matched-letter indices", () => {
    const m = mod.fuzzyMatch("std", "Set theme: Dark");
    expect(m).not.toBeNull();
    // Greedy left-to-right subsequence: the t of "Set", the D of "Dark".
    expect(m!.indices.map((i) => "Set theme: Dark"[i])).toEqual(["S", "t", "D"]);
  });

  it("returns null when a letter is missing", () => {
    expect(mod.fuzzyMatch("zzz", "Set theme: Dark")).toBeNull();
  });

  it("an empty query matches everything with no highlight", () => {
    expect(mod.fuzzyMatch("", "anything")).toEqual({ score: 0, indices: [] });
  });

  it("ranks contiguous runs and word-start hits above scattered letters", () => {
    const tight = mod.fuzzyMatch("abc", "abc")!;
    const scattered = mod.fuzzyMatch("abc", "axbxc")!;
    expect(tight.score).toBeGreaterThan(scattered.score);
    const wordStart = mod.fuzzyMatch("ab", "a b")!;
    const midWord = mod.fuzzyMatch("ab", "axb")!;
    expect(wordStart.score).toBeGreaterThan(midWord.score);
  });
});

// ---------------------------------------------------------------------------
// Platform-aware labels
// ---------------------------------------------------------------------------

describe("platform-aware shortcut labels", () => {
  it("Apple platforms get ⌘K, everything else Ctrl+K", async () => {
    await fresh();
    expect(mod.paletteOpenShortcut("MacIntel")).toBe("⌘K");
    expect(mod.paletteOpenShortcut("iPhone")).toBe("⌘K");
    expect(mod.paletteOpenShortcut("Linux x86_64")).toBe("Ctrl+K");
    expect(mod.paletteOpenShortcut("Win32")).toBe("Ctrl+K");
  });

  it("the palette input row shows the platform's label", async () => {
    await fresh("MacIntel");
    openViaKeyboard();
    expect(win.document.querySelector(".command-palette-inputrow kbd")?.textContent).toBe("⌘K");
  });

  it("non-Apple platforms show Ctrl+K", async () => {
    await fresh("Linux x86_64");
    openViaKeyboard();
    expect(win.document.querySelector(".command-palette-inputrow kbd")?.textContent).toBe("Ctrl+K");
  });
});

// ---------------------------------------------------------------------------
// Open / close / combobox wiring
// ---------------------------------------------------------------------------

describe("palette open/close", () => {
  it("mounts lazily: nothing exists until the first Cmd/Ctrl+K", async () => {
    await fresh();
    expect(win.document.querySelector(".command-palette-overlay")).toBeNull();
    openViaKeyboard(true);
    expect(win.document.querySelector(".command-palette-overlay")).not.toBeNull();
  });

  it("Ctrl+K and Cmd+K both open; keyboard open is instant (no enter class)", async () => {
    await fresh();
    openViaKeyboard(true);
    expect(palette.isOpen()).toBe(true);
    pressKey(win.document.querySelector("#command-palette-input")!, "Escape");
    openViaKeyboard(false); // Cmd
    expect(palette.isOpen()).toBe(true);
    const overlay = win.document.querySelector<HTMLElement>(".command-palette-overlay")!;
    expect(overlay.classList.contains("palette-enter")).toBe(false);
  });

  it("exposes the combobox pattern with aria-activedescendant on the first option", async () => {
    await fresh();
    openViaKeyboard();
    const input = win.document.querySelector<HTMLInputElement>("#command-palette-input")!;
    expect(input.getAttribute("role")).toBe("combobox");
    expect(input.getAttribute("aria-expanded")).toBe("true");
    expect(input.getAttribute("aria-controls")).toBe("command-palette-list");
    const list = win.document.querySelector("#command-palette-list")!;
    expect(list.getAttribute("role")).toBe("listbox");
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-cmd-theme");
    expect(options()[0].getAttribute("aria-selected")).toBe("true");
  });

  it("groups options under Actions, Views, and Recent searches headers", async () => {
    await fresh();
    win.localStorage.setItem("mcv.paletteRecents", JSON.stringify(["quarterly report"]));
    openViaKeyboard();
    const sections = [...win.document.querySelectorAll(".command-palette-section")].map(
      (el) => el.textContent,
    );
    expect(sections).toEqual(["Actions", "Views", "Recent searches"]);
    // Empty query: every command plus the recent, no fallback search row.
    expect(options().map((o) => o.id)).toEqual([
      "palette-cmd-theme",
      "palette-cmd-search",
      "palette-recent-0",
    ]);
  });

  it("Escape closes and returns focus to the trigger", async () => {
    await fresh();
    const trigger = win.document.createElement("button");
    win.document.body.append(trigger);
    trigger.focus();
    openViaKeyboard();
    const input = win.document.querySelector<HTMLInputElement>("#command-palette-input")!;
    expect(win.document.activeElement).toBe(input);
    pressKey(input, "Escape");
    expect(palette.isOpen()).toBe(false);
    expect(win.document.querySelector<HTMLElement>(".command-palette-overlay")!.hidden).toBe(true);
    expect(win.document.activeElement).toBe(trigger);
  });

  it("Cmd/Ctrl+K toggles: a second press closes", async () => {
    await fresh();
    openViaKeyboard();
    openViaKeyboard();
    expect(palette.isOpen()).toBe(false);
  });

  it("pointer open applies the opacity-only enter class", async () => {
    await fresh();
    palette.open({ pointer: true });
    const overlay = win.document.querySelector<HTMLElement>(".command-palette-overlay")!;
    expect(overlay.classList.contains("palette-enter")).toBe(true);
    expect(overlay.style.opacity).toBe("1");
  });
});

// ---------------------------------------------------------------------------
// Filtering, navigation, execution
// ---------------------------------------------------------------------------

describe("keyboard model", () => {
  it("typing filters by fuzzy match and highlights the matched letters", async () => {
    await fresh();
    openViaKeyboard();
    type("dark");
    const first = options()[0];
    expect(first.id).toBe("palette-cmd-theme");
    const marks = [...first.querySelectorAll("mark")].map((m) => m.textContent);
    expect(marks.join("")).toBe("Dark"); // matched letters keep their case
    // "Go to: Search" has no "dark" letters — filtered out entirely.
    expect(options().some((o) => o.id === "palette-cmd-search")).toBe(false);
  });

  it("a typed query always offers the fallback search row", async () => {
    await fresh();
    openViaKeyboard();
    type("nothing matches zqx");
    expect(options().map((o) => o.id)).toEqual(["palette-search"]);
    expect(options()[0].textContent).toContain("Search for “nothing matches zqx”");
  });

  it("ArrowDown/ArrowUp move aria-activedescendant and aria-selected, wrapping", async () => {
    await fresh();
    openViaKeyboard();
    const input = win.document.querySelector<HTMLInputElement>("#command-palette-input")!;
    pressKey(input, "ArrowDown");
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-cmd-search");
    expect(options()[1].getAttribute("aria-selected")).toBe("true");
    pressKey(input, "ArrowDown"); // past the end → wraps to the first
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-cmd-theme");
    pressKey(input, "ArrowUp"); // before the start → wraps to the last
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-cmd-search");
  });

  it("Home and End jump to the first and last option", async () => {
    await fresh();
    openViaKeyboard();
    const input = win.document.querySelector<HTMLInputElement>("#command-palette-input")!;
    pressKey(input, "End");
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-cmd-search");
    pressKey(input, "Home");
    expect(input.getAttribute("aria-activedescendant")).toBe("palette-cmd-theme");
  });

  it("Enter runs the active command, closes the palette, and restores focus", async () => {
    await fresh();
    const trigger = win.document.createElement("button");
    win.document.body.append(trigger);
    trigger.focus();
    openViaKeyboard();
    const input = win.document.querySelector<HTMLInputElement>("#command-palette-input")!;
    type("go");
    pressKey(input, "Enter");
    expect(ran).toEqual(["search"]);
    expect(palette.isOpen()).toBe(false);
    expect(win.document.activeElement).toBe(trigger);
  });

  it("Enter on the fallback row runs the search callback", async () => {
    await fresh();
    openViaKeyboard();
    const input = win.document.querySelector<HTMLInputElement>("#command-palette-input")!;
    type("quarterly");
    pressKey(input, "Enter");
    expect(ran).toEqual([]);
    expect(searches).toEqual(["quarterly"]);
  });

  it("Tab is trapped: focus stays in the palette input", async () => {
    await fresh();
    openViaKeyboard();
    const input = win.document.querySelector<HTMLInputElement>("#command-palette-input")!;
    pressKey(input, "Tab", { shiftKey: true });
    pressKey(input, "Tab");
    expect(win.document.activeElement).toBe(input);
  });

  it("mousedown on an option runs it (pointer path)", async () => {
    await fresh();
    openViaKeyboard();
    const option = options()[1];
    option.dispatchEvent(new win.MouseEvent("mousedown", { bubbles: true, cancelable: true }));
    expect(ran).toEqual(["search"]);
    expect(palette.isOpen()).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Recent searches persistence
// ---------------------------------------------------------------------------

describe("recent searches", () => {
  it("Enter on the fallback row records the query to mcv.paletteRecents", async () => {
    await fresh();
    openViaKeyboard();
    type("vault report");
    pressKey(win.document.querySelector("#command-palette-input")!, "Enter");
    expect(win.localStorage.getItem("mcv.paletteRecents")).toBe(JSON.stringify(["vault report"]));
  });

  it("re-running a query dedups to the front and the cap holds at 8", async () => {
    await fresh();
    win.localStorage.setItem(
      "mcv.paletteRecents",
      JSON.stringify(["q1", "q2", "q3", "q4", "q5", "q6", "q7", "q8"]),
    );
    openViaKeyboard();
    type("q3");
    pressKey(win.document.querySelector("#command-palette-input")!, "Enter");
    openViaKeyboard();
    type("newest");
    pressKey(win.document.querySelector("#command-palette-input")!, "Enter");
    expect(JSON.parse(win.localStorage.getItem("mcv.paletteRecents")!)).toEqual([
      "newest",
      "q3",
      "q1",
      "q2",
      "q4",
      "q5",
      "q6",
      "q7",
    ]);
  });

  it("recents show under their section, filter by query, and re-run via Enter", async () => {
    await fresh();
    win.localStorage.setItem("mcv.paletteRecents", JSON.stringify(["vault report", "chat export"]));
    openViaKeyboard();
    type("vau");
    expect(options().map((o) => o.id)).toEqual(["palette-recent-0", "palette-search"]);
    pressKey(win.document.querySelector("#command-palette-input")!, "Enter");
    expect(searches).toEqual(["vault report"]);
    // Re-running moves the query to the front.
    expect(JSON.parse(win.localStorage.getItem("mcv.paletteRecents")!)[0]).toBe("vault report");
  });
});

// ---------------------------------------------------------------------------
// Committed CSS (same read-the-file stance as a11y.test.ts)
// ---------------------------------------------------------------------------

describe("palette CSS (app.css)", () => {
  const css = readFileSync(
    join(dirname(fileURLToPath(import.meta.url)), "styles", "app.css"),
    "utf8",
  );

  it("the overlay itself carries no transition (keyboard opens are instant)", () => {
    const rule = css.match(/\.command-palette-overlay\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(rule).not.toContain("transition");
  });

  it("pointer-open enter is opacity-only at the motion token", () => {
    expect(css).toMatch(
      /\.command-palette-overlay\.palette-enter\s*\{\s*transition:\s*opacity var\(--motion-fast\) var\(--ease-out\);\s*\}/,
    );
  });

  it("reduced-motion removes the palette enter transition entirely", () => {
    const block = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));
    expect(block).toMatch(
      /\.command-palette-overlay\.palette-enter\s*\{\s*transition:\s*none;\s*\}/,
    );
  });

  it("styles surfaces with committed token pairs (panel/selection/match)", () => {
    const paletteRule = css.match(/\.command-palette\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(paletteRule).toContain("background: var(--panel)");
    const selected = css.match(
      /\.command-palette-option\[aria-selected="true"\]\s*\{[^}]*\}/s,
    )?.[0] ?? "";
    expect(selected).toContain("background: var(--selection)");
    const mark = css.match(/\.command-palette-option mark\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(mark).toContain("var(--match)");
    expect(mark).toContain("var(--match-ink)");
  });
});

// Command palette (Cmd/Ctrl+K): the keyboard-first surface for every
// power-user action. Three pieces, deliberately dependency-free so any
// feature can register commands without touching this file:
//
//  1. A registry — registerCommand({id, title, section, shortcut, run}).
//  2. A subsequence fuzzy matcher that reports the matched-letter
//     indices (the highlight source) and a score (the ranking source).
//  3. A lazily-mounted combobox overlay (role=combobox/listbox/option,
//     aria-activedescendant tracking, focus trap, focus restore).
//
// Recent searches persist under mcv.paletteRecents (same guarded-storage
// convention as mcv.navCollapsed). Keyboard opens render instantly — a
// high-frequency action never animates (Rauno frequency rule); only the
// pointer path fades in, opacity-only, and prefers-reduced-motion keeps
// that path opacity-only too (see the palette rules in app.css).

/** Section of the palette a registered command lists under. */
export type PaletteSection = "actions" | "views";

export interface PaletteCommand {
  /** Stable id (registry re-registers the same id in place). */
  id: string;
  /** The fuzzy-matched label, shown highlighted. */
  title: string;
  section: PaletteSection;
  /** Optional hint shown right-aligned (already platform-shaped). */
  shortcut?: string;
  run(): void;
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/** Registration order = display order within a section (stable ranking). */
const commands: PaletteCommand[] = [];

/** Register (or replace, by id) one command. */
export function registerCommand(command: PaletteCommand): void {
  const at = commands.findIndex((c) => c.id === command.id);
  if (at >= 0) commands[at] = command;
  else commands.push(command);
}

export function lookupCommand(id: string): PaletteCommand | undefined {
  return commands.find((c) => c.id === id);
}

export function listCommands(): readonly PaletteCommand[] {
  return commands;
}

/** Test seam: drop every registration. */
export function resetCommandRegistry(): void {
  commands.length = 0;
}

// ---------------------------------------------------------------------------
// Fuzzy matching: case-insensitive subsequence with a small deterministic
// score (consecutive and word-start runs beat scattered letters).
// ---------------------------------------------------------------------------

export interface FuzzyMatch {
  score: number;
  /** Indices into `text` of the matched letters (highlight source). */
  indices: number[];
}

export function fuzzyMatch(query: string, text: string): FuzzyMatch | null {
  if (!query) return { score: 0, indices: [] };
  const haystack = text.toLowerCase();
  const needle = query.toLowerCase();
  const indices: number[] = [];
  let score = 0;
  let from = 0;
  for (let i = 0; i < needle.length; i++) {
    const at = haystack.indexOf(needle[i], from);
    if (at < 0) return null;
    indices.push(at);
    score += at === 0 || /\W/.test(haystack[at - 1]) ? 8 : 3;
    if (indices.length > 1 && at === indices[indices.length - 2] + 1) score += 4;
    score -= Math.min(at - from, 8); // scattered matches rank lower
    from = at + 1;
  }
  return { score, indices };
}

// ---------------------------------------------------------------------------
// Recent searches (guarded localStorage, mcv.* convention)
// ---------------------------------------------------------------------------

const RECENTS_KEY = "mcv.paletteRecents";
const RECENTS_CAP = 8;

function readRecents(): string[] {
  try {
    const raw = window.localStorage.getItem(RECENTS_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : [];
    return Array.isArray(parsed) ? parsed.filter((q): q is string => typeof q === "string") : [];
  } catch {
    return [];
  }
}

/** Record one executed search query: front, deduped, capped. */
function recordRecent(query: string): void {
  const trimmed = query.trim();
  if (!trimmed) return;
  const next = [trimmed, ...readRecents().filter((q) => q !== trimmed)].slice(0, RECENTS_CAP);
  try {
    window.localStorage.setItem(RECENTS_KEY, JSON.stringify(next));
  } catch {
    // best effort only
  }
}

// ---------------------------------------------------------------------------
// Platform-aware shortcut labels
// ---------------------------------------------------------------------------

/** Apple platforms render ⌘; everything else spells Ctrl. */
export function isApplePlatform(platform: string): boolean {
  return /mac|iphone|ipad|ipod/i.test(platform);
}

export function paletteOpenShortcut(platform: string): string {
  return isApplePlatform(platform) ? "⌘K" : "Ctrl+K";
}

// ---------------------------------------------------------------------------
// Palette overlay
// ---------------------------------------------------------------------------

export interface CommandPalette {
  open(options?: { pointer?: boolean }): void;
  close(): void;
  isOpen(): boolean;
}

/** One selectable row after filtering. */
interface Row {
  kind: "command" | "recent" | "search";
  section: PaletteSection | "recents";
  id: string;
  title: string;
  shortcut?: string;
  match: FuzzyMatch;
  run(): void;
}

/** Display order: Actions, Views, Recent searches (the fallback search
 * row lists last, outside every section). */
const SECTION_ORDER: ReadonlyArray<{ key: PaletteSection | "recents"; label: string }> = [
  { key: "actions", label: "Actions" },
  { key: "views", label: "Views" },
  { key: "recents", label: "Recent searches" },
];

/** Wire the palette: mounts on first open, owns Cmd/Ctrl+K globally. */
export function initCommandPalette(options: {
  onSearch: (query: string) => void;
  platform?: string;
}): CommandPalette {
  const platform = options.platform ?? (typeof navigator !== "undefined" ? navigator.platform : "");
  let overlay: HTMLElement | null = null;
  let input: HTMLInputElement | null = null;
  let list: HTMLElement | null = null;
  let rows: Row[] = [];
  let active = 0;
  let trigger: HTMLElement | null = null;
  let openState = false;

  function ensureMounted(): void {
    if (overlay) return;
    overlay = document.createElement("div");
    overlay.className = "command-palette-overlay";
    overlay.hidden = true;
    const dialog = document.createElement("div");
    dialog.className = "command-palette";
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    dialog.setAttribute("aria-label", "Command palette");
    const inputRow = document.createElement("div");
    inputRow.className = "command-palette-inputrow";
    input = document.createElement("input");
    input.id = "command-palette-input";
    input.type = "text";
    input.placeholder = "Type a command or search…";
    input.setAttribute("role", "combobox");
    input.setAttribute("aria-expanded", "true");
    input.setAttribute("aria-controls", "command-palette-list");
    input.setAttribute("aria-autocomplete", "list");
    input.setAttribute("aria-label", "Search commands");
    const openHint = document.createElement("kbd");
    openHint.textContent = paletteOpenShortcut(platform);
    inputRow.append(input, openHint);
    list = document.createElement("div");
    list.id = "command-palette-list";
    list.className = "command-palette-list";
    list.setAttribute("role", "listbox");
    list.setAttribute("aria-label", "Commands");
    dialog.append(inputRow, list);
    overlay.append(dialog);
    overlay.addEventListener("mousedown", (event) => {
      if (event.target === overlay) close();
    });
    input.addEventListener("input", () => {
      active = 0;
      render(input!.value);
    });
    input.addEventListener("keydown", onKeyDown);
    document.body.append(overlay);
  }

  /** Title with the matched letters wrapped in <mark>. */
  function titled(text: string, match: FuzzyMatch): DocumentFragment {
    const frag = document.createDocumentFragment();
    let from = 0;
    for (const at of match.indices) {
      if (at > from) frag.append(text.slice(from, at));
      const mark = document.createElement("mark");
      mark.textContent = text[at];
      frag.append(mark);
      from = at + 1;
    }
    frag.append(text.slice(from));
    return frag;
  }

  /** Commands ranked by score inside each section; recents keep their
   * recency order; a typed query always offers the fallback search row. */
  function visibleRows(query: string): Row[] {
    const matched: Row[] = [];
    for (const command of commands) {
      const match = fuzzyMatch(query, command.title);
      if (match)
        matched.push({
          kind: "command",
          section: command.section,
          id: `palette-cmd-${command.id}`,
          title: command.title,
          shortcut: command.shortcut,
          match,
          run: () => command.run(),
        });
    }
    matched.sort((a, b) => b.match.score - a.match.score);
    const grouped: Row[] = [];
    for (const section of SECTION_ORDER) {
      if (section.key === "recents") {
        for (const recent of readRecents()) {
          const match = fuzzyMatch(query, recent);
          if (match)
            grouped.push({
              kind: "recent",
              section: "recents",
              id: `palette-recent-${grouped.filter((r) => r.kind === "recent").length}`,
              title: recent,
              match,
              run: () => {
                recordRecent(recent);
                options.onSearch(recent);
              },
            });
        }
      } else {
        grouped.push(...matched.filter((row) => row.section === section.key));
      }
    }
    if (query.trim()) {
      const trimmed = query.trim();
      grouped.push({
        kind: "search",
        section: "actions",
        id: "palette-search",
        title: `Search for “${trimmed}”`,
        match: { score: 0, indices: [] },
        run: () => {
          recordRecent(trimmed);
          options.onSearch(trimmed);
        },
      });
    }
    return grouped;
  }

  function render(query: string): void {
    rows = visibleRows(query);
    active = Math.min(active, Math.max(rows.length - 1, 0));
    list!.replaceChildren();
    let lastSection = "";
    rows.forEach((row, i) => {
      if (row.kind !== "search" && row.section !== lastSection) {
        lastSection = row.section;
        const head = document.createElement("div");
        head.className = "command-palette-section";
        head.setAttribute("role", "presentation");
        head.textContent = SECTION_ORDER.find((s) => s.key === row.section)!.label;
        list!.append(head);
      }
      const option = document.createElement("div");
      option.id = row.id;
      option.className = "command-palette-option";
      option.setAttribute("role", "option");
      option.setAttribute("aria-selected", i === active ? "true" : "false");
      option.append(titled(row.title, row.match));
      if (row.shortcut) {
        const kbd = document.createElement("kbd");
        kbd.textContent = row.shortcut;
        option.append(kbd);
      }
      option.addEventListener("mousedown", (event) => {
        event.preventDefault(); // keep focus in the input (run restores it)
        runRow(i);
      });
      list!.append(option);
    });
    input!.setAttribute(
      "aria-activedescendant",
      rows.length > 0 ? rows[active].id : "",
    );
  }

  function runRow(i: number): void {
    const row = rows[i];
    if (!row) return;
    close();
    row.run();
  }

  function setActive(i: number): void {
    active = i;
    const options = list!.querySelectorAll("[role='option']");
    options.forEach((el, at) => el.setAttribute("aria-selected", at === active ? "true" : "false"));
    input!.setAttribute("aria-activedescendant", rows.length > 0 ? rows[active].id : "");
    options[active]?.scrollIntoView?.({ block: "nearest" });
  }

  function onKeyDown(event: KeyboardEvent): void {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (rows.length === 0) return;
      const delta = event.key === "ArrowDown" ? 1 : -1;
      setActive((active + delta + rows.length) % rows.length);
    } else if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      setActive(rows.length === 0 ? 0 : event.key === "Home" ? 0 : rows.length - 1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      runRow(active);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation(); // the palette consumes Escape first
      close();
    } else if (event.key === "Tab") {
      // Focus trap: the input is the only focusable element inside.
      event.preventDefault();
    }
  }

  function open(optionsIn?: { pointer?: boolean }): void {
    ensureMounted();
    if (openState) return;
    trigger = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    openState = true;
    overlay!.hidden = false;
    if (optionsIn?.pointer) {
      // Pointer path only: opacity-only enter (keyboard opens stay instant).
      overlay!.classList.add("palette-enter");
      overlay!.style.opacity = "0";
      void overlay!.offsetHeight; // commit the pre-state before transitioning
      overlay!.style.opacity = "1";
    }
    input!.value = "";
    active = 0;
    render("");
    input!.focus();
  }

  function close(): void {
    if (!openState) return;
    openState = false;
    overlay!.classList.remove("palette-enter");
    overlay!.style.opacity = "";
    overlay!.hidden = true;
    const restore = trigger;
    trigger = null;
    if (restore && restore.isConnected) restore.focus();
  }

  document.addEventListener("keydown", (event) => {
    if ((event.ctrlKey || event.metaKey) && (event.key === "k" || event.key === "K")) {
      event.preventDefault();
      if (openState) close();
      else open();
    }
  });

  return { open, close, isOpen: () => openState };
}

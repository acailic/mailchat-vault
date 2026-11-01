// @vitest-environment happy-dom
// Unit tests for the folder-watching view helpers: status wording, event
// descriptions, and the DOM builders (folder list with Remove buttons,
// event list) — all through textContent, snapshot-tested.
import { beforeEach, describe, expect, it } from "vitest";
import type { WatchEvent, WatchSnapshot } from "./types";
import {
  describeWatchEvent,
  renderWatchEvents,
  renderWatchFolders,
  watchStatusText,
} from "./watchview";

const EMPTY: WatchSnapshot = { running: true, folders: [], pending: 0, events: [] };

const snapshot = (over: Partial<WatchSnapshot>): WatchSnapshot => ({
  ...EMPTY,
  ...over,
});

describe("watchStatusText", () => {
  it("describes the empty state", () => {
    expect(watchStatusText(EMPTY)).toBe(
      "No folders watched yet — add one to import exports automatically.",
    );
  });

  it("counts folders, singular and plural, and pending files", () => {
    expect(watchStatusText(snapshot({ folders: ["/a"] }))).toContain("Watching 1 folder");
    expect(watchStatusText(snapshot({ folders: ["/a", "/b"], pending: 3 }))).toBe(
      "Watching 2 folders · 3 files settling — new/modified .mbox, WhatsApp .txt and Telegram .json files import in the background.",
    );
    expect(watchStatusText(snapshot({ folders: ["/a"], pending: 1 }))).toContain(
      "1 file settling",
    );
  });

  it("reports a stopped watcher", () => {
    expect(watchStatusText(snapshot({ running: false }))).toContain("not running");
  });
});

describe("describeWatchEvent", () => {
  it("describes imports with dedup-aware counts", () => {
    const e: WatchEvent = {
      kind: "imported",
      path: "/x/a.mbox",
      summary: { source: "mbox", seen: 3, inserted: 2, skipped: 1, failed: 0 },
    };
    expect(describeWatchEvent(e)).toBe(
      "imported /x/a.mbox: 2 new, 1 already present",
    );
  });

  it("mentions parse failures and plain new imports", () => {
    expect(
      describeWatchEvent({
        kind: "imported",
        path: "/x/b.mbox",
        summary: { source: "mbox", seen: 5, inserted: 3, skipped: 0, failed: 2 },
      }),
    ).toBe("imported /x/b.mbox: 3 new, 2 failed to parse");
    expect(
      describeWatchEvent({
        kind: "imported",
        path: "/x/c.mbox",
        summary: { source: "mbox", seen: 2, inserted: 2, skipped: 0, failed: 0 },
      }),
    ).toBe("imported /x/c.mbox: 2 new");
  });

  it("describes skips, failures, and folder changes", () => {
    expect(
      describeWatchEvent({ kind: "skipped", path: "/x/n.txt", reason: "not an export" }),
    ).toBe("skipped /x/n.txt (not an export)");
    expect(describeWatchEvent({ kind: "failed", path: "/x/gone", error: "io error" })).toBe(
      "failed /x/gone — io error",
    );
    expect(describeWatchEvent({ kind: "folder_added", path: "/x" })).toBe(
      "now watching /x",
    );
    expect(describeWatchEvent({ kind: "folder_removed", path: "/x" })).toBe(
      "stopped watching /x",
    );
  });
});

describe("renderWatchFolders", () => {
  let el: HTMLUListElement;
  const removed: string[] = [];

  beforeEach(() => {
    el = document.createElement("ul");
    removed.length = 0;
  });

  it("renders one row per folder with a Remove button wired to the path", () => {
    renderWatchFolders(el, snapshot({ folders: ["/a", "/b/b"] }), {
      onRemove: (p) => removed.push(p),
    });
    expect(el.children.length).toBe(2);
    expect(el.children[0].querySelector(".path")!.textContent).toBe("/a");
    const btn = el.children[1].querySelector<HTMLButtonElement>(".watch-remove")!;
    expect(btn.textContent).toBe("Remove");
    btn.click();
    expect(removed).toEqual(["/b/b"]);
  });

  it("replaces previous content", () => {
    renderWatchFolders(el, snapshot({ folders: ["/a"] }), { onRemove: () => {} });
    renderWatchFolders(el, snapshot({ folders: [] }), { onRemove: () => {} });
    expect(el.children.length).toBe(0);
  });
});

describe("renderWatchEvents", () => {
  it("renders newest first, capped at 20, with kind classes", () => {
    const el = document.createElement("ul");
    const events: WatchEvent[] = [
      {
        kind: "imported",
        path: "/x/a.mbox",
        summary: { source: "mbox", seen: 1, inserted: 1, skipped: 0, failed: 0 },
      },
      { kind: "skipped", path: "/x/n.txt", reason: "not an export" },
      { kind: "failed", path: "/x/gone", error: "io error" },
      { kind: "folder_added", path: "/x" },
    ];
    renderWatchEvents(el, events);
    expect(el.children.length).toBe(4);
    expect(el.children[0].textContent).toBe("imported /x/a.mbox: 1 new");
    expect(el.children[1].className).toBe("watch-event skipped");
    expect(el.children[2].className).toBe("watch-event failed");
    expect(el.innerHTML).toMatchSnapshot();
  });

  it("caps very long histories at 20 entries", () => {
    const el = document.createElement("ul");
    const events: WatchEvent[] = Array.from({ length: 30 }, (_, i) => ({
      kind: "folder_removed" as const,
      path: `/f${i}`,
    }));
    renderWatchEvents(el, events);
    expect(el.children.length).toBe(20);
    expect(el.children[0].textContent).toContain("/f0");
  });
});

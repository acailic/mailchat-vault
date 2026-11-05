// @vitest-environment happy-dom
// DOM tests for the M4.3 media display: URL construction and name
// validation, inline-kind decisions, no-autoplay media elements, and
// the Settings media-store usage report.

import { describe, expect, it } from "vitest";
import {
  buildMediaItem,
  inlineKind,
  mediaUrl,
  objectNameOf,
  renderMediaUsage,
  type MediaUsageView,
} from "./media";
import type { AttachmentSummary } from "./v2types";

const SHA = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

function attachment(over: Partial<AttachmentSummary> = {}): AttachmentSummary {
  return {
    name: "photo.jpg",
    kind: "image",
    size: 1234,
    available: true,
    path: `media/${SHA}.jpg`,
    ...over,
  };
}

describe("mediaUrl / objectNameOf", () => {
  it("builds the protocol URL from a store path", () => {
    expect(mediaUrl(`media/${SHA}.jpg`)).toBe(`mcmedia://local/${SHA}.jpg`);
  });

  it("rejects anything that is not a store object path", () => {
    expect(mediaUrl(null)).toBeNull();
    expect(mediaUrl("")).toBeNull();
    expect(mediaUrl("media/notes.txt")).toBeNull();
    expect(mediaUrl(`media/${SHA}`)).toBeNull();
    expect(mediaUrl(`media/${SHA.toUpperCase()}.jpg`)).toBeNull();
    expect(mediaUrl(`media/${SHA}.jpg/extra`)).toBeNull();
    expect(mediaUrl(`media/../${SHA}.jpg`)).toBeNull();
    expect(mediaUrl(`/etc/passwd`)).toBeNull();
    expect(objectNameOf(`media/${SHA}.a.very/long`)).toBeNull();
  });
});

describe("inlineKind", () => {
  it("maps media kinds to passive inline renderers", () => {
    expect(inlineKind(attachment({ kind: "image" }))).toBe("image");
    expect(inlineKind(attachment({ kind: "photo" }))).toBe("image");
    expect(inlineKind(attachment({ kind: "gif" }))).toBe("image");
    expect(inlineKind(attachment({ kind: "audio" }))).toBe("audio");
    expect(inlineKind(attachment({ kind: "voice_message" }))).toBe("audio");
    expect(inlineKind(attachment({ kind: "video" }))).toBe("video");
    expect(inlineKind(attachment({ kind: "video_file" }))).toBe("video");
    // Stickers render only in raster formats.
    expect(inlineKind(attachment({ kind: "sticker", path: `media/${SHA}.webp` }))).toBe("image");
    expect(inlineKind(attachment({ kind: "sticker", path: `media/${SHA}.tgs` }))).toBeNull();
  });

  it("documents and unknown kinds never render inline", () => {
    expect(inlineKind(attachment({ kind: "document", path: `media/${SHA}.pdf` }))).toBeNull();
    expect(inlineKind(attachment({ kind: "document", path: `media/${SHA}.html` }))).toBeNull();
    expect(inlineKind(attachment({ kind: "file", path: `media/${SHA}.bin` }))).toBeNull();
  });
});

describe("buildMediaItem", () => {
  it("renders an image with a lazy <img> from the media protocol", () => {
    const item = buildMediaItem(attachment(), { onExport: () => {}, onOpenExternal: () => {} });
    const img = item.querySelector<HTMLImageElement>("img");
    expect(img?.getAttribute("src")).toBe(`mcmedia://local/${SHA}.jpg`);
    expect(img?.getAttribute("alt")).toBe("photo.jpg");
    expect(img?.getAttribute("loading")).toBe("lazy");
  });

  it("renders audio and video with controls, no autoplay, minimal preload", () => {
    const audioItem = buildMediaItem(attachment({ kind: "audio", path: `media/${SHA}.ogg` }), {
      onExport: () => {},
      onOpenExternal: () => {},
    });
    const audio = audioItem.querySelector("audio");
    expect(audio).not.toBeNull();
    expect(audio?.hasAttribute("controls")).toBe(true);
    expect(audio?.hasAttribute("autoplay")).toBe(false);
    expect(audio?.getAttribute("preload")).toBe("none");
    expect(audio?.getAttribute("src")).toBe(`mcmedia://local/${SHA}.ogg`);

    const videoItem = buildMediaItem(attachment({ kind: "video", path: `media/${SHA}.mp4` }), {
      onExport: () => {},
      onOpenExternal: () => {},
    });
    const video = videoItem.querySelector("video");
    expect(video).not.toBeNull();
    expect(video?.hasAttribute("controls")).toBe(true);
    expect(video?.hasAttribute("autoplay")).toBe(false);
    expect(video?.getAttribute("preload")).toBe("metadata");
  });

  it("never embeds documents — row and explicit actions only", () => {
    const item = buildMediaItem(attachment({ kind: "document", name: "evil.html", path: `media/${SHA}.html` }), {
      onExport: () => {},
      onOpenExternal: () => {},
    });
    expect(item.querySelector("iframe, embed, object")).toBeNull();
    expect(item.querySelector("img, audio, video")).toBeNull();
    expect(item.querySelector("button.media-save-btn")).not.toBeNull();
    expect(item.querySelector("button.media-open-btn")).not.toBeNull();
  });

  it("wires the explicit export / open-external actions", () => {
    const calls: string[] = [];
    const item = buildMediaItem(attachment(), {
      onExport: (a) => calls.push(`export:${a.name}`),
      onOpenExternal: (a) => calls.push(`open:${a.name}`),
    });
    (item.querySelector("button.media-save-btn") as HTMLElement).click();
    (item.querySelector("button.media-open-btn") as HTMLElement).click();
    expect(calls).toEqual(["export:photo.jpg", "open:photo.jpg"]);
  });

  it("unavailable attachments stay honest rows without actions or media", () => {
    const item = buildMediaItem(attachment({ available: false, path: null }), {
      onExport: () => {},
      onOpenExternal: () => {},
    });
    expect(item.classList.contains("unresolved")).toBe(true);
    expect(item.textContent).toContain("not in export");
    expect(item.querySelector("button, img, audio, video")).toBeNull();
  });
});

describe("renderMediaUsage", () => {
  const usage: MediaUsageView = {
    objects: 3,
    total_bytes: 3_000_000,
    referenced: 2,
    referenced_bytes: 2_900_000,
    orphan_objects: 1,
    orphan_bytes: 100_000,
    by_extension: [
      { ext: "jpg", objects: 2, bytes: 2_500_000 },
      { ext: "pdf", objects: 1, bytes: 500_000 },
    ],
    largest: [{ sha256: SHA, path: `media/${SHA}.jpg`, size: 2_000_000 }],
  };

  it("summarizes totals, the split, and the per-extension table", () => {
    const el = document.createElement("div");
    renderMediaUsage(el, usage);
    const text = el.textContent ?? "";
    expect(text).toContain("3 objects");
    expect(text).toContain("2 referenced");
    expect(text).toContain("1 unreferenced");
    const rows = el.querySelectorAll("tr");
    expect(rows.length).toBe(2);
    expect(rows[0].textContent).toContain(".jpg");
    const bars = el.querySelectorAll(".usage-bar");
    expect(bars.length).toBe(2);
  });

  it("lists the largest objects and handles a null report", () => {
    const el = document.createElement("div");
    renderMediaUsage(el, usage);
    expect(el.textContent).toContain("Largest objects:");
    const empty = document.createElement("div");
    renderMediaUsage(empty, null);
    expect(empty.textContent).toContain("No usage report yet.");
  });
});

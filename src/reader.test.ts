// @vitest-environment happy-dom
// Reader-surface tests (editorial layout, phases A + B): the reading
// measure's meta hierarchy (subject line, quiet date · source ·
// conversation row, technical dl in the Provenance fold) and the
// attachment chips (icon + name + size, the "+N more" expander,
// keyboard activation, expansion surviving re-renders). The mechanical
// CSS guarantees (72ch measure cap, .tnum, .mv-enter reduced-motion
// path) are asserted against the committed stylesheet text, the
// a11y.test.ts pattern — the visual rhythm audit itself stays human.

import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { renderDetail2 } from "./render2";
import { MEDIA_CHIP_VISIBLE, buildMediaChips } from "./media";
import { contextEntries } from "./searchstate";
import type { AttachmentSummary, ConversationContext } from "./v2types";

const here = dirname(fileURLToPath(import.meta.url));
const appCss = readFileSync(join(here, "styles", "app.css"), "utf8");

// Timestamps render timezone-independently (formatSentAt is covered by
// format.test.ts); the local clock only pads fixed fields here.
vi.mock("./format", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./format")>();
  return {
    ...actual,
    formatSentAt: (iso: string) => iso || "unknown time",
  };
});

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

const ATTACHMENTS: AttachmentSummary[] = [
  attachment({ name: "spec.pdf", kind: "document", path: `media/${SHA}.pdf` }),
  attachment({ name: "chart.png", kind: "image", path: `media/${SHA}.png` }),
  attachment({ name: "voice.ogg", kind: "voice_message", path: `media/${SHA}.ogg` }),
  attachment({ name: "clip.mp4", kind: "video_file", path: `media/${SHA}.mp4` }),
  attachment({ name: "lost.jpg", kind: "image", size: 0, available: false, path: null }),
  attachment({ name: "slides.pdf", kind: "document", path: `media/${SHA}.pdf` }),
  attachment({ name: "logo.gif", kind: "gif", path: `media/${SHA}.gif` }),
];

function ctx(over: Partial<ConversationContext["anchor"]> = {}): ConversationContext {
  return {
    anchor: {
      id: 3,
      source_id: "s1",
      source_label: "mbox",
      channel: "email",
      chat_id: "thread-a",
      external_id: "e2e-m-3",
      conversation_id: "c1",
      conversation_label: "thread-a",
      conversation_confidence: "verified",
      author: "Ana",
      subject: "Delivery date",
      body: "The vault arrives",
      sent_at: "2026-01-02T10:00:00Z",
      original_timestamp: "Fri, 02 Jan 2026 11:00:00 +0100",
      timezone: "Europe/Zagreb",
      time_confidence: "legacy_assumed",
      attachments: ATTACHMENTS,
      occurrences: [],
      revisions: [],
      raw_json: "{\"keep\": 1}",
      ...over,
    },
    before: [
      {
        id: 1,
        author: "Ana",
        sent_at: "2026-01-01T10:00:00Z",
        time_confidence: "explicit_offset",
        body: "earlier message",
      },
    ],
    after: [],
    unverified_conversation: false,
    has_more_before: true,
    has_more_after: false,
  };
}

function render(
  el: HTMLElement,
  context: ConversationContext,
  onOpenExternalMedia: (a: AttachmentSummary) => void = () => {},
): HTMLElement {
  renderDetail2(el, {
    ctx: context,
    entries: contextEntries(context),
    queryText: "",
    hasMoreBefore: false,
    hasMoreAfter: false,
    onOpenMessage: () => {},
    onLoadEarlier: () => {},
    onLoadLater: () => {},
    onCopy: () => {},
    onCopyLink: () => {},
    onExport: () => {},
    onExportChat: () => {},
    onExportMedia: () => {},
    onOpenExternalMedia,
    onClose: () => {},
  });
  return el;
}

// ---------------------------------------------------------------------------
// Meta hierarchy (phase A)
// ---------------------------------------------------------------------------

describe("reader meta hierarchy", () => {
  it("shows the subject as a display line and the quiet date · source · conversation row", () => {
    const el = render(document.createElement("aside"), ctx());
    const card = el.querySelector(".detail-card") as HTMLElement;
    const subject = card.querySelector(".detail-subject") as HTMLElement;
    expect(subject.textContent).toBe("Delivery date");
    const meta = card.querySelector(".detail-meta") as HTMLElement;
    // The date keeps its honesty caveat and tooltip (A04: the same
    // wording as the result rows), the source reads friendly, the
    // conversation is the label alone — qualifiers stay technical.
    expect(meta.textContent).toContain("2026-01-02T10:00:00Z (assumed)");
    expect(meta.textContent).toContain("Mail import");
    expect(meta.textContent).toContain("thread-a");
    const date = meta.querySelector("span") as HTMLElement;
    expect(date.title).toContain("The time was assumed during import");
    // The body is its own element below the row — never inside it.
    expect(meta.querySelector(".body")).toBeNull();
    expect(card.querySelector(".detail-body .body")?.textContent).toContain("arrives");
  });

  it("an uncertain time carries its caveat on the row; an exact one does not", () => {
    const el = render(
      document.createElement("aside"),
      ctx({ time_confidence: "explicit_offset" }),
    );
    const meta = el.querySelector(".detail-meta") as HTMLElement;
    expect(meta.textContent).not.toContain("(assumed)");
    expect(meta.textContent).toContain("2026-01-02T10:00:00Z");
  });

  it("no subject renders no subject line (chat messages)", () => {
    const el = render(document.createElement("aside"), ctx({ subject: null }));
    expect(el.querySelector(".detail-subject")).toBeNull();
    expect(el.querySelector(".detail-meta")).not.toBeNull();
  });

  it("the technical dl moves into the Provenance fold, rows intact", () => {
    const el = render(document.createElement("aside"), ctx());
    const table = el.querySelector(".meta-table") as HTMLElement;
    expect(table).not.toBeNull();
    // The reference copy sits beside the occurrences, under Provenance.
    expect(table.closest("details.detail-provenance")).not.toBeNull();
    const text = table.textContent ?? "";
    for (const row of ["Mail import", "Delivery date", "thread-a (verified)", "e2e-m-3"]) {
      expect(text).toContain(row);
    }
  });
});

// ---------------------------------------------------------------------------
// Attachment chips (phase B)
// ---------------------------------------------------------------------------

describe("attachment chips", () => {
  it("render icon, name, and size for the visible count", () => {
    const el = render(document.createElement("aside"), ctx());
    const chips = [...el.querySelectorAll(".media-chips > li")] as HTMLElement[];
    expect(chips.length).toBe(MEDIA_CHIP_VISIBLE + 1); // 4 chips + expander
    const chip = el.querySelector("button.media-chip") as HTMLElement;
    // The first attachment is a document: the file glyph, not an image one.
    expect(chip.querySelector("svg.icon use")?.getAttribute("href")).toBe("#i-file");
    expect(chip.querySelector(".media-chip-name")?.textContent).toBe("spec.pdf");
    const size = chip.querySelector(".media-chip-size") as HTMLElement;
    expect(size.textContent).toBe("1.2 KB");
    expect(size.classList.contains("tnum")).toBe(true);
  });

  it("icon follows the attachment kind (image, audio, video, document)", () => {
    const handlers = { onExport: () => {}, onOpenExternal: () => {} };
    const list = buildMediaChips(
      [
        attachment({ name: "a.png", kind: "image" }),
        attachment({ name: "b.ogg", kind: "voice_message" }),
        attachment({ name: "c.mp4", kind: "video_file" }),
        attachment({ name: "d.pdf", kind: "document" }),
      ],
      handlers,
    );
    const hrefs = [...list.querySelectorAll("button.media-chip svg.icon use")].map(
      (use) => use.getAttribute("href"),
    );
    expect(hrefs).toEqual(["#i-image", "#i-audio", "#i-video", "#i-file"]);
  });

  it("the expander names the hidden count and reveals the rest", () => {
    const el = render(document.createElement("aside"), ctx());
    const more = el.querySelector("button.media-chip-more") as HTMLElement;
    expect(more.textContent).toBe(`+${ATTACHMENTS.length - MEDIA_CHIP_VISIBLE} more`);
    expect(more.getAttribute("aria-expanded")).toBe("false");
    more.click();
    // Every attachment is now a chip, beside the "Fewer" toggle.
    expect(el.querySelectorAll("li .media-chip").length).toBe(ATTACHMENTS.length);
    const fewer = el.querySelector("button.media-chip-more") as HTMLElement;
    expect(fewer.textContent).toBe("Fewer");
    expect(fewer.getAttribute("aria-expanded")).toBe("true");
  });

  it("at or below the cap there is no expander", () => {
    const el = render(
      document.createElement("aside"),
      ctx({ attachments: ATTACHMENTS.slice(0, MEDIA_CHIP_VISIBLE) }),
    );
    expect(el.querySelector("button.media-chip-more")).toBeNull();
    expect(el.querySelectorAll("li .media-chip").length).toBe(MEDIA_CHIP_VISIBLE);
  });

  it("chips are keyboard-operable buttons; Enter opens externally", () => {
    const opened: string[] = [];
    const el = render(
      document.createElement("aside"),
      ctx(),
      (a) => opened.push(a.name),
    );
    const chip = el.querySelector("button.media-chip") as HTMLElement;
    // A real <button>: intrinsically in the tab order, no negative
    // tabindex, activation on the keyboard path.
    expect(chip.tagName).toBe("BUTTON");
    expect(chip.getAttribute("tabindex")).not.toBe("-1");
    chip.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(opened).toEqual(["spec.pdf"]);
    chip.click();
    expect(opened).toEqual(["spec.pdf", "spec.pdf"]);
  });

  it("unpreserved references render quiet inert chips, never actions", () => {
    const el = render(
      document.createElement("aside"),
      ctx({
        attachments: [
          attachment({ name: "gone.jpg", size: 0, available: false, path: null }),
        ],
      }),
    );
    const chip = el.querySelector(".media-chip") as HTMLElement;
    expect(chip.tagName).toBe("SPAN");
    expect(chip.classList.contains("unresolved")).toBe(true);
    expect(chip.textContent).toContain("not in export");
    expect(el.querySelector(".media-chips button")).toBeNull();
  });

  it("expansion survives a re-render of the same message and resets for another", () => {
    const el = document.createElement("aside");
    render(el, ctx());
    (el.querySelector("button.media-chip-more") as HTMLElement).click();
    // Same message (context page load / background refresh): still open.
    render(el, ctx());
    const chips = el.querySelector("ul.media-chips") as HTMLElement;
    expect(chips.dataset.expanded).toBe("true");
    expect(el.querySelectorAll("li .media-chip").length).toBe(ATTACHMENTS.length);
    // Another message starts collapsed.
    render(el, ctx({ id: 4 }));
    const fresh = el.querySelector("ul.media-chips") as HTMLElement;
    expect(fresh.dataset.expanded).toBeUndefined();
    expect(el.querySelector("button.media-chip-more")?.textContent).toBe(
      `+${ATTACHMENTS.length - MEDIA_CHIP_VISIBLE} more`,
    );
  });

  it("the full media items stay below the chips (previews and explicit actions)", () => {
    const el = render(document.createElement("aside"), ctx());
    const media = el.querySelector("section.detail-media") as HTMLElement;
    expect(media.querySelector("ul.media-chips")).not.toBeNull();
    expect(media.querySelectorAll("ul.media-list li.media-item").length).toBe(
      ATTACHMENTS.length,
    );
    expect(media.querySelector("button.media-open-btn")).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Mechanical CSS guarantees (a11y.test.ts pattern: committed text only)
// ---------------------------------------------------------------------------

describe("reader stylesheet rules (app.css)", () => {
  it("caps the reading measure at 72ch, centered for the reading column", () => {
    const body = appCss.match(/\.detail-card \.body\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(body).toMatch(/max-width:\s*72ch/);
    expect(body).not.toContain("78ch");
    const column =
      appCss.match(/\.detail-card \.detail-body \.body\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(column).toMatch(/margin-inline:\s*auto/);
  });

  it("gives subject and meta row the same measure", () => {
    for (const selector of [".detail-subject", ".detail-meta"]) {
      const block = appCss.match(new RegExp(`${selector.replace(".", "\\.")}\\s*\\{[^}]*\\}`, "s"))?.[0] ?? "";
      expect(block, `rule for ${selector}`).not.toBe("");
      expect(block).toMatch(/max-width:\s*72ch/);
    }
  });

  it("the meta row is muted small text with tabular digits", () => {
    const block = appCss.match(/\.detail-meta\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(block).toMatch(/color:\s*var\(--muted\)/);
    expect(block).toMatch(/font-size:\s*var\(--text-meta\)/);
    expect(block).toMatch(/font-variant-numeric:\s*tabular-nums/);
  });

  it("ships the .tnum utility for numeric spans", () => {
    expect(appCss).toMatch(/\.tnum\s*\{\s*font-variant-numeric:\s*tabular-nums;\s*\}/);
  });

  it(".mv-enter resolves to an opacity-only path under reduced motion", () => {
    const marker = "@media (prefers-reduced-motion: reduce)";
    const reduced = appCss.slice(appCss.lastIndexOf(marker));
    const enter = reduced.match(/\.mv-enter\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(enter).toMatch(/transition:\s*opacity/);
    expect(enter).not.toContain("transform");
  });

  it("the reading body breathes at the reading line-height token", () => {
    const block = appCss.match(/\.detail-card \.detail-body \.body\s*\{[^}]*\}/s)?.[0] ?? "";
    expect(block).toMatch(/line-height:\s*var\(--lh-reading\)/);
  });
});

# Demo video script — 30–45 s launch video

Scene-by-scene script for the launch video. The scenes extend the demo
GIF ([`docs/screenshots/demo.gif`](../../screenshots/README.md#demo-gif-readme-front-door))
to a calm 40 seconds with captions instead of cursor-speed runs. The
same rules as every published asset apply: synthetic fixture data only
(`tests/e2e/fixtures/archive.mbox`), light theme, 1280×800 capture
window, and capture with the headless recipe the demo GIF documents
(v0.2.x binary on `Xvfb`, `xdotool` driving the real UI, `ffmpeg -f
x11grab` recording). Status: **pending capture** — a capture-capable
run owns the recording; this script needs no new decisions.

- Total length: ~40 s (target band 30–45 s).
- Output: H.264 MP4, `yuv420p`, 1280×800 (Product Hunt accepts it; the
  gallery images, not the video, carry the 1270×760 crop).
- Thumbnail: [`gallery/hero.png`](gallery/hero.png).
- Captions: Simplified Technical English, short lines, bottom third.
- No voice-over needed; captions carry the story. If a voice-over is
  added later, the caption text is the script.

| # | Time | Action on screen | Caption |
| --- | --- | --- | --- |
| 1 | 0:00–0:05 | First-run screen: "Add your first source" card, cursor moves to "Import file…" | Your email and chats, one private archive — on your machine. |
| 2 | 0:05–0:13 | Native file chooser opens `tests/e2e/fixtures/archive.mbox`; import preview appears with bounded rows and counts | Drop in a Gmail Takeout, mbox, or chat export. Preview first — nothing is stored until you confirm. |
| 3 | 0:13–0:19 | Confirm; job progress completes; workspace opens with the imported messages | One archive. Mail and chats, side by side. |
| 4 | 0:19–0:27 | Type `vault` in the search box; four hits return with highlighted matches | Find any message in seconds — search stays on this device. |
| 5 | 0:27–0:35 | Top hit opens in the reading pane; the match is highlighted in the message body; Back to results returns to the list | Full text, conversation context, provenance — fully offline. |
| 6 | 0:35–0:40 | Hold on the settled wide workspace; end card: name + tagline + repository URL | Mailchat Vault — free and open source. Linux, macOS, Windows. |

## Capture notes

- Scenes 1–3 follow the import spec journey
  (`tests/e2e/specs/02-import-mbox.e2e.mjs`); scenes 4–6 follow the
  workspace spec journey (`tests/e2e/specs/16-workspace-layout.e2e.mjs`).
  The demo GIF's recipe section records the exact Xvfb/xdotool/ffmpeg
  invocation; reuse it with a 40 s recording window and scene pauses.
- Pause 1–2 s on scenes 1, 3, and 6 so the caption is readable — the
  GIF runs faster than a viewer can read.
- The typing in scene 4 must be slower than in the GIF (the GIF types
  at automation speed for compression, not readability).
- End card: text on a plain light background, the name spelled
  "Mailchat Vault", the repository URL, no download or star numbers.
- After capture: verify no frame contains anything but fixture data,
  then register the video in the [pack README](README.md) and the
  listing links.

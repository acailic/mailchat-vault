# Product Hunt launch pack

The launch-ready public surface for the Product Hunt debut, as
repository content: listing copy, gallery images, the maker first
comment, and the demo video script. Launch strategy and monetization
stay outside the repository ([docs index](../../README.md)); the claim
authority is the [support matrix](../../support-matrix.md), and the
positioning wording is single-sourced in
[`docs/landing.md`](../../landing.md) (checked verbatim by
`npm run check:docs`).

## Contents

| File | What it is |
| --- | --- |
| [`listing.md`](listing.md) | Tagline (≤ 60 characters), two-line description, gallery order, links |
| [`first-comment.md`](first-comment.md) | Maker first comment: what it is, why local-first, what is intentionally not in v0.2.x |
| [`video-script.md`](video-script.md) | 30–45 s scene-by-scene demo video script keyed to the demo GIF scenes |
| [`gallery/hero.png`](gallery/hero.png) | Hero frame: the search scene from the demo GIF (see below) |
| [`gallery/search.png`](gallery/search.png) | Search with highlighted hits, wide workspace |
| [`gallery/reading.png`](gallery/reading.png) | Reading pane: open message replaces the results area |

## Gallery assets and regeneration recipes

Every gallery image is exactly **1270×760** (Product Hunt gallery size),
light theme, synthetic fixture data only. Nothing is hand-drawn: each
image regenerates from a committed source with one documented command
run from the repository root. The crops keep the 1270:760 aspect ratio
(never stretch): crop to the ratio first, scale second.

| Image | Source | Recipe (run from the repository root) |
| --- | --- | --- |
| `gallery/hero.png` | `docs/screenshots/demo.gif` frame 230 (0-indexed `n=229`, ~15.3 s — the settled search scene: four highlighted hits, top hit open in the reading pane) | `ffmpeg -y -i docs/screenshots/demo.gif -vf "select=eq(n\,229),crop=960:574:0:13,scale=1270:760" -frames:v 1 docs/launch/product-hunt/gallery/hero.png` |
| `gallery/search.png` | `tests/e2e/fixtures/screenshots/16-wide.png` (1280×800) | `ffmpeg -y -i tests/e2e/fixtures/screenshots/16-wide.png -vf "crop=1270:760:5:20" docs/launch/product-hunt/gallery/search.png` |
| `gallery/reading.png` | `tests/e2e/fixtures/screenshots/16-medium.png` (900×700) | `ffmpeg -y -i tests/e2e/fixtures/screenshots/16-medium.png -vf "crop=900:538:0:81,scale=1270:760" docs/launch/product-hunt/gallery/reading.png` |

Crop math, so the numbers are not magic:

- 1270:760 aspect ≈ 1.671; a source is first center-cropped to that
  ratio (960×600 → 960×574 at y=13; 900×700 → 900×538 at y=81), then
  scaled up to 1270×760.
- `search.png` needs no scaling: 1280×800 minus a 5 px/20 px border is
  already 1270×760.
- The hero upscale (×1.32) is slightly soft; if review rejects it, the
  documented fallback hero is `gallery/search.png` (same subject,
  native 1280×800 pixels) — swap the order in
  [`listing.md`](listing.md#gallery-order) accordingly.

Verify dimensions after any regeneration:

```sh
ffprobe -v error -select_streams v:0 -show_entries stream=width,height \
  -of csv=p=0 docs/launch/product-hunt/gallery/*.png
```

The fixture screenshots regenerate by re-running the workspace-layout
e2e spec; the demo GIF by its
[recipe](../../screenshots/README.md#demo-gif-readme-front-door) — see
[docs/screenshots/README.md](../../screenshots/README.md).

## Pending capture (owned by a follow-up run)

This runner cannot run Tauri, WebKitGTK, or the e2e suite, so two
gallery images and the video ship as recipes, marked pending. A
capture-capable follow-up owns them; its done-when is the original
gallery acceptance.

| Asset | Recipe | Status |
| --- | --- | --- |
| `gallery/import-flow.png` | Capture the import preview step per [docs/screenshots/README.md → Import preview and confirm](../../screenshots/README.md#import-preview-and-confirm) (`tests/e2e/specs/02-import-mbox.e2e.mjs`, sample archive on the first-run screen, light theme, 1280×800), then crop: `ffmpeg -y -i <capture>.png -vf "crop=1270:760:5:20" docs/launch/product-hunt/gallery/import-flow.png` | pending capture |
| `gallery/sources-settings.png` | Capture settings per [docs/screenshots/README.md → Settings — AI assistance](../../screenshots/README.md#settings--ai-assistance) (`tests/e2e/specs/09-settings-providers.e2e.mjs`, fresh vault with no key entered, light theme, 1280×800), then the same `crop=1270:760:5:20` | pending capture |
| demo video | [`video-script.md`](video-script.md), recorded with the demo GIF's headless Xvfb/xdotool/ffmpeg recipe | pending capture |

When a pending asset lands: add it to the table above with its exact
command, and update the gallery order in [`listing.md`](listing.md).

## Owner upload steps

1. **Product Hunt gallery** — upload the images in the
   [`listing.md` gallery order](listing.md#gallery-order); `hero.png`
   first (it becomes the feed card).
2. **GitHub repository social preview** — upload
   [`public/social-preview.png`](../../../public/social-preview.png)
   (the exact file, 1280×640; no new image is drawn): repository
   Settings → General → "Social preview" → Upload an image. This is
   what shows on link unfurls for the repository URL.
3. **Landing page social cards** — the `og:`/`twitter:` meta in
   [`docs/landing.md`](../../landing.md) is committed with a relative
   `og:image`. When deploying: host `public/social-preview.png` next to
   the deployed page and set `og:image` to the absolute URL on the
   deploy domain (`https://<deploy-domain>/social-preview.png`) —
   social scrapers need an absolute URL. The HTML comment above the
   meta tags in `docs/landing.md` records this step.
4. **Go-live gate** — the listing goes live only after the owner
   actions on the [launch checklist](../../launch-checklist.md):
   repository visibility (after the rights audit,
   [#79](https://github.com/acailic/mailchat-vault/issues/79)) and
   Actions billing. [#78](https://github.com/acailic/mailchat-vault/issues/78)
   (signing/notarization) blocks any signed-installer claim, not the
   listing itself.

## Constraints

- Every image and video uses synthetic fixture data only
  (`tests/e2e/fixtures/`) — real message content never enters a
  published asset. This rule has no exceptions.
- Light theme, 1280×800 capture window per the
  [screenshot recipes](../../screenshots/README.md); gallery output
  cropped to 1270×760.
- Copy in Simplified Technical English; the application name is spelled
  "Mailchat Vault" everywhere.
- Badges and claims stay honest: no download or star numbers the
  repository cannot show; no signed/notarized claim while
  [#78](https://github.com/acailic/mailchat-vault/issues/78) is open.

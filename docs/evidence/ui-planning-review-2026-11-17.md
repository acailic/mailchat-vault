# UI planning review evidence — 2026-11-17

Status: **planning evidence, not a live UI audit or completed implementation**.

Deliverables: [professional UI plan](../ui-professionalization-plan.md), [design-system specification](../ui-design-system-plan.md).

## Method and limitations

Read Impeccable's `SKILL.md`, ran `impeccable context --target index.html` once, and read Shape, Operate, Init and New-work guidance. Context reported an incumbent implementation but no root `PRODUCT.md`, `DESIGN.md` or surface brief. The user requested plans saved in `docs`; the documents use the supplied product brief and current code as evidence and remain proposed rather than pretending a new identity was confirmed or implemented.

Preference questions offered refinement versus replacement and the first-pass priority. The proposed defaults are incumbent refinement and search/reading first; unanswered preferences are not recorded as user approval or persistent skill configuration.

Inspected baseline remains `4596a7ebe257b0fe1f2316b03c8ee4e6b1f91581` plus existing working-tree UI changes. Read tokens/styles, index markup, result/reader construction, the UX contract, archive experience plan and provenance proposal. No UI code was changed and no app, native webview or installer was launched for this planning pass.

Viewed these existing images:

| Image | Provenance | What it supports |
| --- | --- | --- |
| [Native reading review](product-review-2026-11-06/reading.png) | Prior review at `cb8b67d`, 2026-11-06; synthetic demo | Historical dark-theme layout and earlier toolbar/metadata density |
| [Wide fixture](../../tests/e2e/fixtures/screenshots/16-wide.png) | Last changed at `d6e4dec`, 2026-11-14 | Later light-theme navigation/result/reader structure |
| [Compact fixture](../../tests/e2e/fixtures/screenshots/16-compact.png) | Existing synthetic e2e screenshot | Compact navigation-drawer composition |

Current source differs from these images: sidebar width/padding, result author/provenance hierarchy, Search/Ask tabs, toolbar grouping and reader typography have changed. No screenshot is labeled a current render of the working tree. The historical dark image and later light image are not a matched theme comparison; use fresh captures during UI0.

## Findings and recommendations

| Finding | Evidence | Recommendation | Confidence boundary |
| --- | --- | --- | --- |
| The incumbent identity is coherent | Warm/green tokens, bundled Source Sans 3, existing line icons, inspected fixtures | Refine rather than invent a new palette/font family | Design recommendation, not a user-tested preference |
| Reader title emphasizes author even when a subject exists | `render2.ts`: `title.textContent = displayAuthor`; subject is metadata | Subject/conversation-led reader, sender/date beneath | Verified source structure; final layout needs render review |
| Technical fields precede content | `render2.ts`: original timestamp/message ID in `meta-table` before body | Move technical detail into origin record while leaving caveats visible | Source-grounded hierarchy proposal |
| Some important refinements are already in progress | Existing diff in index/render2/styles/tokens | Preserve and build on current work; avoid duplicate redesign | Inspected diff, no fresh execution claim |
| Source/status/import UX has a solid existing base | UX contract, preview/progress/account markup and architecture investigation | Standardize and finish flows instead of replacing them wholesale | Functional acceptance remains separate |
| Base text colors have useful contrast headroom | Token calculations below | Retain palette; measure composite states during implementation | Static opaque-color calculation only |
| Border roles need separation | Existing controls use `--border-strong`, around 2:1 against panel | Add proposed control-boundary token; keep decorative rules quiet | Potential issue where border alone identifies control; not a blanket conformance verdict |
| Existing screenshots cannot certify current UI | Fixture dates and current diff | UI0 fresh native capture with exact revision/theme/state | Explicit evidence limitation |

## Reproducible color calculations

Parsed opaque six-digit hex colors from the light and explicit-dark blocks in `src/styles/tokens.css`. Converted each sRGB channel `c` to linear light as `c / 12.92` when `c <= 0.04045`, otherwise `((c + 0.055) / 1.055)^2.4`. Relative luminance is `0.2126 R + 0.7152 G + 0.0722 B`; contrast is `(lighter + 0.05) / (darker + 0.05)`.

| Pair | Light ratio | Dark ratio |
| --- | --- | --- |
| Primary text / panel | 14.84:1 | 12.56:1 |
| Muted text / panel | 5.68:1 | 6.25:1 |
| Muted text / canvas | 5.36:1 | 7.09:1 |
| Accent foreground / accent | 6.39:1 | 7.22:1 |
| Match ink / match background | 9.27:1 | 5.02:1 |
| Warning / panel | 5.93:1 | 6.91:1 |
| Danger / panel | 6.54:1 | 6.19:1 |
| Success / panel | 6.47:1 | 7.30:1 |
| Decorative border / panel | 1.49:1 | 1.45:1 |
| Existing strong border / panel | 2.11:1 | 1.97:1 |
| Focus ring / panel | 8.75:1 | 8.43:1 |

Proposed control-boundary values were separately calculated:

| Color | Panel | Canvas | Secondary panel |
| --- | --- | --- | --- |
| Light `#7e8880` | 3.67:1 | 3.46:1 | 3.20:1 |
| Dark `#718177` | 3.65:1 | 4.14:1 | 3.24:1 |

These figures do not cover opacity, rendered antialiasing, native-control geometry, text on images, all hover/selected combinations, forced colors or assistive-technology use. The existing dark search-match calculation was independently reproduced. No accessibility certification is claimed.

## Deliverable validation

Check new-document relative links, Markdown fence balance and whitespace; review plan consistency with the archive architecture and existing feature boundaries. No application suite is required to validate prose-only changes, and no application-test pass is claimed. UI implementation must produce the fresh captures and state evidence defined in UI0–UI7.

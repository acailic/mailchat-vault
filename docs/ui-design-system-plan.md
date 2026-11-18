# UI design-system specification

Date: 2026-11-17. Status: **proposed refinement of the incumbent system, not an implemented component library**.

Companion to the [professional UI plan](ui-professionalization-plan.md). Current authority is [tokens.css](../src/styles/tokens.css), [app.css](../src/styles/app.css), current components and the [UX specification](ux-specification.md). Values explicitly labeled proposed below require rendered validation before adoption. Existing working-tree refinements are included as inspected source, not assumed shipped behavior.

## 1. Visual principles

1. Content leads. Give messages and files more attention than their controls.
2. Precision earns trust. Alignment, type, state and exact language carry the identity.
3. Use a small visual vocabulary repeatedly. Familiar controls should look and behave the same across screens.
4. Preserve evidence. A visually quiet interface must still show missing files, assumed dates and source identity.
5. Color has a job. Accent selects or acts; semantic colors communicate states with text; amber marks search matches.

No new display font, decorative gradient, glass panel, hero illustration, oversized metric tile, animated page entrance, or floating command palette is required for this application. A clear product-specific need must justify additional visual machinery.

## 2. Typography

Retain bundled **Source Sans 3** and the current native fallback stack. No remote font fetch. Keep the existing monospace stack for locators and technical details only. Use rem-based implementation equivalents with a fixed scale; typography changes by role, not viewport width.

| Role | Target size / line height | Weight | Notes |
| --- | --- | --- | --- |
| Small supporting text | 13 px / 18–20 px | 400 | Source/date/help text; avoid 11–12 px for essential information |
| UI/body/result text | 15 px / 22–24 px | 400 | Current body-size foundation |
| Label/action/result sender | 15 px / 20–22 px | 600 | Do not make every label semibold |
| Reading prose | **Proposed 16 px / ~26 px** | 400 | Plain email and chat body; preserve reasonable user scaling |
| Section title | 18 px / 24 px | 600 | Existing scale role |
| Page/reader title | 24 px / 30 px | 600 | Existing scale role; long titles wrap without displacing actions |

Use tabular numerals for counts, progress and aligned timestamps. Sentence case for labels/actions; uppercase limited to short optional section markers. Do not use uppercase or tracking as a substitute for hierarchy.

Prose measure: target 65–75ch, with 72ch a practical default. Forms: 640–720 px maximum. Wide content does not mean every sentence should span the window. Truncate result previews where necessary, but show complete message content, errors, uncertainty and confirmation consequences.

## 3. Spacing and geometry

Retain the 4 px base scale: **4, 8, 12, 16, 24, 32, 40**. The 32/40 values are already in the inspected working tree. Prefer named role aliases over adding arbitrary component offsets.

| Role | Target |
| --- | --- |
| Icon ↔ label | 8 px |
| Label ↔ field | 4–8 px |
| Field ↔ helper/error | 4 px |
| Related field groups | 16 px |
| Sections | 24–32 px |
| Standard view padding | 24 px; 12–16 px in compact layout |
| Reader padding | 24–32 px desktop; 16 px compact |
| Results | 12 px vertical / 12–16 px horizontal padding, content-driven height |
| Small/medium radius | Existing 4/6 px |
| Chips | Pill radius only for filter tokens, not every button or container |
| Separators | 1 px; muted decorative border |

Use borders and surface changes for structure. Shadow is reserved for an actual overlay above content; ordinary result/source rows do not need elevation. Use logical properties (`padding-inline`, `margin-block`) and `min-width: 0` in flex/grid text tracks. Avoid hardcoded row heights for variable or translated content.

## 4. Semantic color roles

The palette stays restrained. The following base values are present in inspected tokens:

| Role | Light | Dark | Use |
| --- | --- | --- | --- |
| Canvas | `#faf8f4` | `#191d1b` | App background |
| Panel | `#ffffff` | `#222825` | Content/reader/control surface |
| Secondary panel | `#f2efe8` | `#2a312d` | Quiet supporting regions |
| Text | `#1f2a24` | `#e7ece8` | Primary content |
| Muted text | `#5d6a60` | `#9faaa1` | Secondary metadata, not disabled opacity |
| Accent | `#2d6a4f` | `#7fb99a` | Primary action and current item |
| Accent foreground | `#ffffff` | `#10241a` | Text on accent |
| Match background / ink | `#f5d66b` / `#3a2f00` | `#7a5f18` / `#ffe9a8` | Search-term highlight |
| Success | `#256b3c` | `#7fc492` | A proven successful operation, with text |
| Warning | `#8a5a00` | `#d9a84e` | A caveat requiring attention, with text |
| Danger | `#b3261e` | `#f08a80` | Failure or destructive consequence, with text |

Preserve current `--selection-soft` and `--nav-surface` intent, but check their actual computed values in all themes. Do not use amber search highlights as the sole warning treatment; warning also has icon/text/context.

### New role proposed: control boundary

The existing decorative `--border` and `--border-strong` values are too faint to treat as a universal sole boundary for controls. Add **`--control-border`**, leaving quiet decorative rules unchanged:

| Proposed token | Light | Dark | Calculated contrast against panel / canvas / secondary panel |
| --- | --- | --- | --- |
| `--control-border` | `#7e8880` | `#718177` | Light 3.67 / 3.46 / 3.20; dark 3.65 / 4.14 / 3.24 |

Apply where boundary recognition depends on it: inputs, unselected checkboxes/custom indicators, and necessary outlined controls. Do not darken every separator to this value. Verify native-control rendering; a token alone does not control every OS checkbox pixel.

Keep visible focus distinct from selection: existing `--focus-ring`, 2 px outline, offset with clear separation. Test adjacent backgrounds, clipping and forced colors. Avoid disabled opacity on an entire form group when its label/helper text must remain readable.

The measured base pairs are in the [evidence record](evidence/ui-planning-review-2026-11-17.md). Measurements are calculations on token values, not proof of browser rendering or complete accessibility.

## 5. Component vocabulary

Implement these as reusable local primitives/helpers and CSS recipes when their owning features change. Do not add a framework solely to obtain them.

| Component | Anatomy | Required states / behavior |
| --- | --- | --- |
| Button | Label; optional leading 16 px icon; primary/secondary/quiet/danger variants | Default, hover, focus, pressed, disabled, pending; pending retains width and action label |
| Icon button | 16–20 px icon within deliberate hit target; accessible name | Same states; tooltip on hover and focus; no title-only accessible name |
| Field | Persistent label, input, optional help, associated error | Empty, populated, focus, invalid, disabled, read-only, pending validation; preserve entered value on error |
| Select/checkbox | Native control where adequate, visible label | Checked/selected, focus, disabled, error, indeterminate if meaningful |
| Filter chip | Scope label/value + named remove action | Default, hover, focus; wrapping; not used for static metadata indiscriminately |
| Status line | Icon if useful, plain text, optional relevant action | Idle, pending, partial, failed, complete; no color-only meaning |
| Result row | Independent selection checkbox + message action + source/time/files | Hover, opened, checked, focus, pending stale data; composite states do not override each other |
| Origin record | Short summary + labeled disclosure + definition/occurrence list | Expanded/collapsed; unknown/assumed/missing/conflict information remains intelligible |
| Attachment | Name/type/size/status + preview + explicit actions | Loading, available, missing, unsupported preview, corrupt; never fake a preserved file |
| Source row | Name/kind + coverage/last operation + primary and secondary actions | Ready, updating, paused, needs sign-in, manual, partial, error |
| Dialog/drawer | Heading, purpose, content, action footer | Open/close, focus trap when modal, Escape when safe, focus restoration, long-content scroll |
| Empty state | Specific heading, brief reason, one useful primary action | Empty vault, no search match, no people, no files; no repeated generic illustration |
| Receipt | Operation result, counts, coverage caveats, next action | Complete, partial, cancelled, failed; durable facts rather than transient toast only |

Targets: 36 px standard desktop button height, 40 px form controls, 40–44 px search/primary onboarding controls. Icon targets should generally be 32–36 px on desktop and larger where touch is relevant. Check actual target dimensions/spacing; visual glyph size is not target size. A minimum target policy must not be inferred from padding declarations alone.

Transient feedback is not the sole record for a completed import or failed backup. Toasts may acknowledge Copy; consequential work belongs in a source receipt or Activity and relevant page status.

## 6. Motion and loading

Retain the current **120 ms / 180 ms** motion tokens. They cover hover/focus/reveal feedback; no need to extend durations just because a guideline permits it. Reduced motion sets nonessential transitions to zero. No page-load choreography, moving result reorder animation, or autoplay.

Use a skeleton only for an initially empty loading region. Keep existing rows during an update with a pending label, so loading does not cause a needless context loss. Delay a purely decorative loading placeholder briefly to avoid flashing on near-instant reads; test the threshold instead of delaying real results.

Indeterminate progress shows phase and counts. Determinate progress uses a real denominator. Success feedback appears after the operation is durably complete, not merely after command submission. Avoid repeated live-region announcements for every counter tick.

## 7. Layout and overlays

Maintain one scroll owner per primary pane. A long dialog may own its content scroll, but its title/actions remain reachable at low window heights and large text. Popovers/menus must escape clipped list containers and stay within the viewport; choose a compatible native/top-layer or portal approach only after checking supported Tauri webviews.

At the existing 1100/800 px breakpoints, switch structure rather than shrinking fonts. Stack labels/values in compact reading views. Actions wrap in logical order; the primary action cannot move ahead of explanatory text through visual positioning that contradicts keyboard order.

Do not conceal functionality solely to achieve a clean screenshot. A collapsed sidebar needs an obvious labeled way back; an overflow menu needs discoverable naming; a clipped filename needs an accessible/full detail route.

## 8. Content boundaries and evidence marks

Use a small shared vocabulary: **Timezone assumed**, **No date recorded**, **Grouping unverified**, **File not included**, **Another revision exists**. Final wording must match the exact backend fact; do not collapse “not checked” and “missing.”

Marks are small text treatments beside the fact they qualify, optionally with a line icon. They are not all warning pills. Show multiple caveats as readable lines when crowded; never truncate the caveat into an unexplained symbol. Detailed provenance remains accessible through the record disclosure.

Use neutral avatars only where they improve recognition. Do not obtain external profile photos or brand assets as part of normal archive browsing. Source logos, if introduced, need local assets and accessible text; the source's actual identity is its account/collection label.

## 9. Adoption and verification

1. Inventory existing controls and map them to these roles before changing CSS.
2. Add only missing semantic tokens; migrate one component family at a time and retain equivalent behavior.
3. Preserve user edits and add/update focused render/state tests for intentional behavior changes. Do not write tests that merely repeat CSS declarations.
4. Inspect native rendered light/dark/System states at wide, medium, compact and high zoom with real-length synthetic content.
5. Verify focus order, escape/return, target geometry, loading/error/disabled behavior, contrast and overflow.
6. Update approved UX/design documentation and screenshot evidence with the exact implementation revision. This proposal alone is not a component-library completion record.

Open optional choices: compact density, resizable panes, and a stronger rebrand if requested. None should be silently introduced during the first refinement pass.

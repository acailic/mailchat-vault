# A visual archetype for provenance — Design

Date: 2026-11-14
Status: proposed — not implemented
Issue: #116. Pairs with the honesty-filters design (#108), which makes
the same trust metadata queryable.

## Problem

The critique's verdict: "grounded but anonymous — a disciplined skeleton
wearing someone else's coat." The honesty system says the right things in
the wrong clothes:

- **Provenance is a collapsed fold.** The Provenance disclosure
  (`details.detail-provenance` with `ul.provenance-list`,
  `src/render2.ts:765-801`) and the Technical details fold below it
  (`src/render2.ts:803-815`) carry classes that `src/styles/app.css`
  never styles — the archive's acquisition record renders with the
  browser's default disclosure look, visually equal to the raw-JSON dump
  it sits beside.
- **Uncertainty is a parenthetical.** "(assumed)" and "(unresolved)"
  ride inside the result row's time span (`timeLabel`,
  `src/render2.ts:59-68`), "(unverified grouping)" inside a `dd`
  (`src/render2.ts:617-631`), file honesty inside a bare "n/m" badge
  (`span.badge.attachments`, `src/render2.ts:120-137` — the `badge`
  class is also unstyled).
- **The reference library has no objects in it.** The ux-specification
  names "a quiet reference-library aesthetic" (Visual direction,
  `docs/ux-specification.md`) but no surface contains an artifact of
  that world. Product-review finding F08 is the consequence: the reader
  meets a flat metadata table, and the provenance that would justify
  trust is the least visible thing on the screen.

The honesty system should gain **form, not decoration** (Operate-mode
restraint): a border-and-type archetype built from existing tokens, not
imagery.

## The four surfaces, as they exist today

1. **Reading-pane header** — `article.detail-card`'s `header` (author
   `h2` + `.header-actions`, `src/render2.ts:569-602`) over
   `dl.meta-table` (`src/render2.ts:604-646`, styled
   `src/styles/app.css:980-988`). The medium/compact swap layouts add
   `#reading-toolbar` (`index.html:260`, `app.css:868`); wide has none.
   This is where a reader decides whether to trust the record.
2. **Source rows** — the rows that attribute a source: the result row's
   meta line ending in `hit.source_label` (`src/render2.ts:116`), the
   card's `source` row, and the occurrence rows of `.provenance-list`
   ("scope · item … · observed … · import …", `src/render2.ts:770-779`).
   Dense by design ("dense but comfortable message rows", ux-spec).
3. **Sub-view empty space** — Sources/Activity/Settings are
   measure-bounded to `max-width: 720px`, centered
   (`src/styles/app.css:384-398`), so wide windows leave empty canvas
   beside the column. Space exists, but nothing per-message lives in a
   sub-view.
4. **Reading-pane margin** — beside `.detail-card .body` (max 78ch,
   `app.css:901`) when the reading column
   (`minmax(var(--reading-min), 6fr)` of the wide grid, `app.css:312`)
   exceeds the text measure; absent entirely in the swap layouts, and
   thin at `--reading-min: 400px` (`src/styles/tokens.css:86`).

## The four candidates against those surfaces

- **Catalog card** — the library's item record: one bordered card of
  ruled label/value lines with the accession history at its foot. It is
  pure border-and-type; the app already has the recipe (`#first-run`,
  `.preview-card`, `.import-progress-card`: `1px solid
  var(--border-strong)`, `var(--radius-m)`, `var(--panel)` —
  `app.css:594`, `:619`, `:660`).
- **Stamp** — the archival access/property mark: small, bordered,
  uppercase, one ink. Reduces without loss to a bordered label; the app
  already prints these (`.media-kind`, `app.css:1023-1030`).
- **Ledger row** — one ruled line per recorded event with tabular
  figures, no imagery. Occurrences are already ledger entries in
  wording; the app already rules table rows (`.stat-table td`,
  `app.css:1209-1213`) and sets tabular numerals (`.day-chart-axis`,
  `app.css:1169-1177`).
- **Library pocket** — the sleeve glued inside a cover that holds the
  borrower card. Its identity is a skeuomorphic shape (sleeve, tab,
  shadow depth); there is no border-and-type reduction that still reads
  as a pocket.

| Surface | Catalog card | Stamp | Ledger row | Library pocket |
| --- | --- | --- | --- | --- |
| Reading-pane header | **primary** — the record itself | mark vocabulary inside the card | interior ruling of the card | duplicate container, skeuomorph |
| Source rows | no — a row is an entry, not a record | **yes** — qualifiers become marks | **yes** — occurrence rows ruled | no |
| Sub-view empty space | decoration with no content | decoration with no content | decoration with no content | decoration with no content |
| Reading-pane margin | card would fragment across measure | a call-number edge is possible but measure-hungry | a marginal column steals reading width | no |

## Chosen archetype: the catalog card

The detail pane's metadata becomes **the catalog card of the archived
message**: one bordered, ruled panel that carries the cataloging data
(source, from, conversation, date), the accession record (occurrences,
imports) at its foot, and the uncertainty marks stamped onto the exact
values they qualify. The other two survivors are absorbed as subordinate
treatments — the **stamp** is how uncertainty words are set, the
**ledger row** is how the accession lines are ruled. The card survives
the restraint test the pocket fails: it is borders, type, and existing
tokens, nothing else.

Why it wins:

- **It covers all the honesty content as one form.** The stamp alone
  cannot hold a record (occurrences, ids, revisions); the ledger alone
  has no identity for a whole record. Only the card is simultaneously
  metadata, acquisition history, and the surface the stamps live on.
- **It explains the product.** "This message is a cataloged item; here
  is its card, here is how it was acquired, here is what is uncertain"
  is the reason the app exists — trustworthy recall with visible
  provenance — rendered as an object instead of a fold. No generic
  local-search tool shows one.
- **It is F08-compatible.** F08 asks for progressive disclosure without
  hiding uncertainty and for the body keeping priority. The card gives
  the existing metadata *form*, not added prominence: same rows, same
  words, one bounded panel, and the provenance fold's facts become
  visible in the card's foot instead of invisible behind a summary.
- **It is native and cheap.** A bordered definition list over a scroll
  pane uses no new controls, no motion, no icons, no palette; it works
  unchanged in wide, medium, and compact.

**Rejected: library pocket.** No border-and-type reduction exists — the
pocket is its own shape (sleeve, tab, depth), which is imagery, the
exact thing the constraint forbids. It also duplicates the card's
container role with less room for content.

**Rejected: stamp as the primary.** A stamp marks an act or a state; it
cannot carry a record. As the primary it would decorate the header with
a large mark while provenance stayed in the fold — decoration, not form.
Kept as the mark vocabulary.

**Rejected: ledger row as the primary.** It styles a record's interior
but gives the reading pane no identity; on result rows it would add
rules to a list whose density is deliberate. Kept as interior ruling.

**Rejected surfaces.** *Sub-view empty space*: an archetype placed where
no provenance lives is the constraint's failure mode — decoration
unmoored from content. *The reading-pane margin*: a marginalia column
costs text measure at `--reading-min` and does not exist in the swap
layouts; it is the natural future home for a call-number edge if the
wide layout ever gains slack, not a first placement.

## Concrete placements

### 1. The card (reading-pane header surface)

- `dl.meta-table` adopts the existing card recipe: `border: 1px solid
  var(--border-strong); border-radius: var(--radius-m); background:
  var(--panel-2); padding: var(--sp-2) var(--sp-3)` — the inset archival
  surface `.thread-row` and `.media-item` already use (`app.css:990`,
  `:1008`). The two-column grid (`app.css:982`) and the `--text-meta`
  sizing stay.
- **Ledger ruling inside the card:** each dt/dd pair separates with
  `border-bottom: 1px solid var(--border)` (last row without) — the
  ruled lines of a catalog card, the same rule `.stat-table td` uses.
- **The accession foot:** `details.detail-provenance` moves inside the
  card as its last row (the Technical details fold stays a sibling
  below, unchanged — raw JSON is not part of the catalog record). The
  `<summary>` stops reading as a bare label and carries the first facts
  in the occurrence vocabulary: one occurrence →
  "Provenance — observed {date} · {scope}"; several →
  "Provenance — observed {n} times, earliest {date}"; none → the
  existing honest line "No recorded observations." (`src/render2.ts:783`).
  Expanded, `ul.provenance-list` renders the ledger rows: ruled with
  `border-bottom: 1px solid var(--border)`, numerals in
  `font-variant-numeric: tabular-nums`, set at `--text-meta` in
  `var(--muted)` — item ids and timestamps align down the card. The
  revisions list (`src/render2.ts:786-799`) keeps its place in the same
  fold.
- The `.unverified-note` paragraph (`src/render2.ts:712-717`) stays
  below the card, full sentence unchanged — it is the explanation, not
  the mark.

### 2. The stamps (source-rows surface, restrained)

One shared label recipe — `.media-kind`'s (`app.css:1023-1030`):
`font-size: 11px; text-transform: uppercase; letter-spacing: 0.04em;
padding: 1px 6px; border: 1px solid var(--border-strong);
border-radius: var(--radius-pill)` — in **one ink**, `var(--muted)`,
like a library's single-color access stamp. Uncertainty is not an error
state; no `--warning`/`--danger`. Applied only where a qualifier already
prints today, with no new elements and no new words:

- the date and conversation `dd` values in the card ("(assumed)" → a
  stamp reading ASSUMED; "(unverified grouping)" → UNVERIFIED GROUPING);
- `span.time` in result rows (same two time qualifiers,
  `src/render2.ts:107-115`);
- `span.badge.attachments` (the honesty ratio "n/m", already
  `aria-label`ed) and `span.badge.conflict` ("revisions").

Tooltips keep carrying the full sentences (`timeConfidenceDetail`,
`src/format.ts:86`; the unverified-conversation explanation,
`src/render2.ts:625-629`). Result rows gain no ruling and no card form —
their density is the ux-spec's own instruction. Thread/context rows
(`.thread-row`) stay untouched: the context list is chronological
reading, and restamping every neighbor would repeat what the card says
once.

## Wording

Stamps and the accession line reuse the detail view's own vocabulary —
the one the honesty-filters design shares: "assumed", "unresolved",
"no time recorded" (`timeConfidenceLabel`, `src/format.ts:65`),
"unverified grouping" (`conversationConfidenceLabel`,
`src/format.ts:107`), "not preserved" and the "n of m" attachment counts
(`src/render2.ts:120-137`, `src/searchstate.ts:398`, `src/media.ts:90`),
"observed"/"import" (`src/render2.ts:775-776`). Never "inaccurate" or
"broken" (honesty-filters rule). The archetype carries the words; it
does not rewrite them.

## Accessibility

- **Contrast:** every proposed pairing is already in service and already
  measured for WCAG 2.2 AA (`tokens.css` header): `--text`/`--muted` on
  `--panel`/`--panel-2`, `--border`/`--border-strong` rules. No new
  color pairing is introduced; if implementation ever adds one, it must
  be measured, per the ux-spec's rule that conformance is verified, not
  claimed from CSS.
- **Never color- or shape-alone:** every stamp is a word; state is text
  even where the bordered mark is missed. Tooltips and `aria-label`s are
  unchanged.
- **Motion:** none added. The card and stamps are static; the disclosure
  stays the native `details` element, and any hover treatment would use
  the existing `--motion-fast` background transition under the
  `prefers-reduced-motion` guard already in `tokens.css:178-184`.
- **Screen readers:** the accession summary now announces facts
  ("observed 2 times, earliest …") instead of the bare word
  "Provenance"; the card keeps the `dl`/`dt`/`dd` semantics and the fold
  keeps native disclosure semantics and keyboard operation.

## Testing and verification (for the implementation task)

- happy-dom render tests (`src/render2.test.ts`): stamp spans replace
  parentheticals with the same words; the accession summary states
  counts and earliest date; "No recorded observations." survives.
- e2e specs asserting the parenthetical forms are updated deliberately,
  per the app-identity precedent (decorative form changes; accessible
  names stay stable).
- Visual pass in light and dark themes across wide, medium, and compact,
  per the ux-specification maintenance rule (validate wide, compact,
  empty, error, and keyboard flows before accepting a visual change).
- `npm run check`, `npm test`, and the affected e2e specs pass.

## Out of scope

- Any change under `src/`, `src-tauri/`, or `tests/` — this spike
  selects and specifies; implementation is a separate task after spec
  approval.
- The honesty-filters implementation (#108) and any edit to its design;
  the app-identity design and its icon inventory (stamps are type, not
  icons — no sprite additions).
- Palette, typography, or layout-structure changes; new tokens.
- F08's hierarchy reordering (subject/conversation priority, action
  weight) — that is F08's own task; this spike only gives the existing
  metadata its form.
- The two rejected placements (sub-view space, reading-pane margin) and
  the pocket/stamp/ledger-as-primary alternatives, recorded above.

# Honesty markers as filters — Design

Date: 2026-11-14
Status: proposed — not implemented

## Problem

The archive is honest per message: the reading pane labels assumed dates
("assumed", "no time recorded"), unresolved groupings ("unverified
grouping"), and reference-only attachments ("not preserved"). But honesty
is not queryable — "everything whose date is a guess" requires manual
inspection. Honesty markers should be filters, the way sources and dates
already are.

## The three candidate filters and their data availability

1. **Dates assumed** — `messages.time_confidence` (M1.1 provenance:
   `explicit_offset | user_confirmed | legacy_assumed | unresolved |
   missing`). Filter set: assumed or missing vs exact/confirmed. The v2
   row (`MessageSummary.time_confidence`) already carries it to the UI.
2. **Grouping unverified** — `conversation_confidence` (`verified |
   user_linked | legacy_unresolved`); `MessageSummary.conversation_id` is
   null when no resolved grouping exists, so the filter is "resolved into
   a conversation vs not (or unresolved)". Same availability.
3. **Files missing** — reference-vs-preserved: `attachment_count >
   available_attachment_count` per row (the media-store join behind
   `AttachmentSummary.available`). Available today.

All three are WHERE clauses over existing columns — no schema change.

## Request / filter-model extension shape

Mirror the `has_attachment` pattern (`Option<bool>` = tri-state) in
`SearchArchiveRequest` (Rust `search2.rs` + `src/v2types.ts`) and
`SearchFilters` (`src/searchstate.ts`):

- `time_assumed: bool | null` — true = `legacy_assumed | unresolved |
  missing`, false = exact/confirmed.
- `grouping_unverified: bool | null` — true = null/unresolved
  conversation, false = verified/user-linked.
- `files_missing: bool | null` — true = references without preserved
  bytes.

One tri-state each, not a vocabulary picker: users ask "is this
trustworthy?", not "which of five confidences?".

## UI placement

Three toggles in the Filters fold beside "Has files" (same control
recipe), applied state as removable chips in the existing
applied-filter system (`filterChips` kinds: `time-assumed`,
`grouping-unverified`, `files-missing`); Clear clears them with
everything else. No fold if all three default off — dead chrome rule.

## Honest-uncertainty wording

Chip labels say what is uncertain, in the detail view's own vocabulary:
"dates assumed", "grouping unverified", "files missing" — never
"inaccurate" or "broken". Counts stay off the chips; the honesty is the
filter's subject, not a quantity.

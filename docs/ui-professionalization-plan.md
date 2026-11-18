# Professional UI plan for Mailchat Vault

Date: 2026-11-17. Status: **proposed, implementation-ready design plan; application UI not changed by this document**.

Method: Impeccable's planning and Operate guidance, the supplied product brief, current frontend source, and inspected historical screenshots. The companion [design-system specification](ui-design-system-plan.md) defines tokens and components; the [review evidence](evidence/ui-planning-review-2026-11-17.md) records what was actually inspected and measured.

Working defaults: refine the existing visual identity; prioritize search and reading. These were offered as preferences during planning and remain assumptions unless the user selects otherwise. This is a proposed brief, not a claim of confirmed visual approval. The request is for plans in `docs`; no root `PRODUCT.md`/`DESIGN.md`, skill configuration, or UI implementation was created as a side effect.

## 1. Design brief

**Audience and situation.** A person has valuable email and chat history spread across accounts and exports. They may arrive worried about losing it, and return months later with an incomplete memory of a message, person, or file. They need familiar controls, truthful preservation status, and a readable original.

**Primary outcome.** Find the right item, understand its context, and know what the archive actually retained. First-time users must be able to reach that experience through a guided import. Expert controls should be available without determining the first impression.

**Mode.** Archive, Sources, Settings and Activity are Operate surfaces; message bodies also require Read-quality typography. Professional quality means precise hierarchy, consistent behavior, legible state, and resilience to real content. The app should feel like a well-made desktop reference tool.

**Visual authority.** Keep the incumbent Source Sans 3, warm neutrals, forest-green action color, existing line icons, and three-destination navigation. Extend them deliberately. The previous [archive experience plan](archive-experience-plan.md) supplies product scope and backend dependencies; this document owns the proposed presentation and interaction details.

**Signature.** Every item has an understandable origin and an honest preservation record. Express that through a compact provenance treatment and explicit uncertainty beside the relevant fact. Do not add an ornamental vault, padlock, trust score, or green “safe” badge unsupported by actual evidence.

## 2. What to preserve and improve

| Area | Keep | Improve |
| --- | --- | --- |
| Identity | Warm light theme, green accent, locally bundled type, restrained dark theme | Consistent contrast roles, spacing, control sizing and type hierarchy |
| Navigation | Archive / Sources / Settings, secondary Activity, hideable sidebar | One clear Archive view switch; fewer redundant heading/toolbar rows |
| Search | Explicit Enter/Search, local queries, visible filters, stable results and refresh | Prominent query, clearer result anatomy, separation of opened item and bulk selection |
| Reader | Original source/context, safe HTML rendering, attachment controls | Subject/conversation before sender metadata; content before technical fields; channel-specific layout |
| Trust | Assumed times, unverified grouping, missing-file information | Plain-language, consistent marks; one expandable acquisition record |
| Import | Shared preview, native pick/drop, progress and recovery | Provider instructions, clear step hierarchy, a useful completion receipt |
| Settings | Task-based subsections and explicit persistence | Standard form anatomy, local validation, unambiguous save scope |
| Future capabilities | People and Files from the product plan | Specify coherent surfaces now; expose them in production only when their contracts work |

The current working tree already contains improvements to result hierarchy, toolbar grouping, sidebar spacing and reading typography. Integrate those changes when implementation starts; do not overwrite them with an older screenshot. Historical screenshots show prior problems and visual lineage, not a fresh measurement of the current rendered application.

## 3. Information hierarchy and navigation

Keep the sidebar at 224 CSS px in the standard desktop layout, matching the current working-tree token. Show the product name, primary destinations, a bounded source-filter section, Activity and a short local-search statement. If source count is large, show a searchable disclosure rather than a wall of chips. Provider/account labels remain distinct.

Inside Archive, the target navigation is **Messages / People / Files**. Search belongs to the active view; its scope is written in the field label and nearby filters. Optional **Ask** is a secondary mode action with explicit network/consent status. Avoid two stacked, equal-weight tab bars for Search/Ask and Messages/People/Files.

Transition in stages: keep the current Search/Ask navigation until the new archive views and their state model exist. Do not ship dead People/Files tabs or a fake gallery to make the layout look finished. Prototypes and screenshots must label unavailable features as proposed.

At rest, use one primary filled action per task region. Search is primary in Archive; Add source on Sources; Import on confirmation; Create backup on the storage page. Secondary actions use neutral buttons or text. Destructive actions are contextual and visually distinct at the point of confirmation, not permanently prominent.

## 4. Messages workspace

### Composition

```text
┌ Sidebar ───────┬ Archive workspace ────────────────┬ Reader ────────────────┐
│ Mailchat Vault │ Messages  People  Files      Ask │ Apartment lease         │
│               │ [Search messages…]      [Search] │ Ana → You · 18 Jun 2019  │
│ Archive       │ Filters (2)   Sort: Relevance    │ Gmail · Personal        │
│ Sources       │ [2019 ×] [Has files ×]           │                         │
│ Settings      │ 42 results                Export │ Message content…        │
│               │                                  │                         │
│ Sources       │ Ana                    18 Jun    │ [Lease.pdf · 240 KB]     │
│ Personal mail │ Apartment lease                  │                         │
│ WhatsApp      │ Here's the signed agreement…     │ Earlier/later messages  │
│               │ Gmail · Personal   1 file kept   │                         │
│ Activity      │ ──────────────────────────────── │ Origin and preservation ▸│
└───────────────┴──────────────────────────────────┴─────────────────────────┘
```

This is a hierarchy sketch, not a pixel-approved mockup. Synthetic names and counts illustrate content roles only.

At 1440 px, start with 224 px navigation, about 456 px results and the remaining 760 px reader. At 1280 px, use about 400 px results and 656 px reader. Honor existing 320 px result and 400 px reader minimums. A draggable divider is optional later; if added, it needs keyboard resizing, limits, a reset action and persisted user preference. Do not make it a prerequisite for improving the layout.

The first visible result should normally begin within approximately 176 px of the archive content area's top when there are no exceptional banners or expanded filters. This is a composition budget to verify, not a fixed height that clips translated text or larger type. Avoid spending half the window on controls.

### Search controls

- A 40–44 px search field with a persistent accessible label and an adjacent Search button; placeholder is an example/help cue, not the only label.
- Search remains explicit on Enter/button; no remote suggestions or unexpected request on each keystroke.
- Second line: Filters with active count, sort, and a contextual Clear filters action. Clear filters preserves typed query; a separate field clear empties only text. If clearing everything is offered, label it “Clear search and filters.”
- Show removable chips only for active filters. A source filter names the account/collection, not just “Gmail.” Each removal control has a specific accessible name.
- Keep the existing native disclosure behavior initially. A richer popover must escape scroll clipping and work in supported Tauri webviews; do not replace a functioning disclosure for appearance alone.
- Search count and Export align in a quiet results-header row. Pagination sits with the results, not between reading actions. Bulk selection replaces that row with “3 selected,” Export selected and Clear selection.
- Loading retains prior results, explicitly labeled as previous, with `aria-busy` and a small pending indicator. Blank initial loading can use fixed-size skeleton rows. A completed request announces a concise result count without moving keyboard focus.

### Result anatomy

| Order | Content | Treatment |
| --- | --- | --- |
| 1 | Sender/display name and date | Sender 15 px semibold; date 13 px, aligned to end when space permits |
| 2 | Email subject or chat title, if available | 15 px medium; distinct from snippet; no invented title |
| 3 | Matching body excerpt | 15 px normal, up to two visual lines; match highlighting stays legible on selection |
| 4 | Source instance and file availability | 13 px; wrap safely; missing-file text must not disappear behind an ellipsis |

Use rows and fine separators, not a card around every result. Start with 12 px vertical padding and 12–16 px horizontal padding; let rows grow for important uncertainty or translated labels. Do not enforce a fixed row height on variable content. Small collections should still look intentional; large collections should retain practical scan density.

Distinguish **opened item**, **keyboard focus**, and **bulk selection**: an opened item gets a narrow accent edge and soft neutral-green surface; bulk selection gets the checked checkbox plus count; focus gets its own outline. Hover must not erase any of them. A result checkbox is an independent control; it never opens the message accidentally. Do not use a listbox pattern containing nested buttons and checkboxes; retain a semantic list with explicit interactive controls.

## 5. Reader: content first, evidence available

### Email

The main title is the subject, falling back to “Email from [sender]” if absent. Below it, show sender → recipient summary, sent date and source instance. Expand complete To/Cc and technical headers through labeled details. Original timestamps and internal identifiers belong in the acquisition/details section unless needed to explain a visible uncertainty.

Message content follows immediately. Retain the sandboxed Rendered/Raw implementation and blocked remote resources; a design pass must not weaken it. If renaming Raw to Plain text, update the visible and accessible labels together while preserving behavior. Make the reader surface and email frame boundaries clear without adding nested decorative cards.

Use 16 px reading text with roughly 1.6 line height and a 65–75 character measure for plain text. Preserve original HTML's useful structure within the secure frame; provide plain text when an email's own layout is unusable. Never show a “Reply” composer in an archive-only product.

### Chats

Use a conversation title, participant summary, date dividers and restrained bubbles. Group consecutive messages by known sender without hiding a sender change. Use neutral alignment until self identity is known; do not infer “me” from message order, device ownership, or a friendly label. Group chats keep their actual participants when opened from People.

A search-opened anchor has a visible “Matching message” treatment that does not rely on color. Load earlier/later content without jumping the anchor. Show unknown dates separately and explain unverified grouping rather than manufacturing neighboring messages.

### Files and origin

File cards show filename, type, size when known, preserved/missing status and explicit Open / Save a copy actions. Unavailable previews still have usable metadata; no autoplay. Missing references offer a relevant reimport/help route instead of a nonfunctional download button.

The reader's acquisition record is titled **Origin and preservation**. Its collapsed summary contains source and import/sync observation facts plus any material caveat; expansion shows recorded occurrences and technical details. A small ruled definition list echoes the existing catalog-card proposal. It must not compete with the message body or become a decorative “certificate.”

This plan extends [the provenance archetype proposal](superpowers/specs/2026-11-14-provenance-archetype-design.md): retain its ruled record and textual uncertainty marks, but apply the content-first hierarchy requested here. This is a proposed refinement, not an assertion that the older proposal has been implemented or formally superseded.

Reader actions are Copy, Copy link, Export and Close/Back. Export gets a text label where space permits; icon utilities require accessible names and hover/focus descriptions. On compact layouts, utility actions move to an accessible overflow menu, while Back remains visible.

## 6. People and Files surfaces

These designs depend on A5/A6 in the [architecture delivery plan](archive-experience-plan.md#11-delivery-plan). They are not frontend-only polish tasks.

### People

The list shows user-created name, linked identity count, source labels and observed date range. Use a neutral initial/avatar fallback, not fabricated profile photos. Empty state: “Bring one person's messages together,” with **Create person** and a brief explanation that links are manually confirmed.

A person page leads with name, confirmed identities and a searchable timeline. “Manage identities” opens a dedicated panel. Linking steps are **Select identities → Review messages affected → Confirm links**. Show source/account, stable identity when available, ambiguity, counts and samples. Same-name suggestions never appear as preconfirmed matches.

Conflicts show the existing assignment and require an explicit choice. Unlinking explains that messages stay in the archive and previews the effect. Person deletion removes organization only, using that exact consequence in the confirmation. The timeline's filters, file availability and provenance use the same components as Messages.

### Files

Start with **All / Photos / Videos / Audio / Documents**, filename search and source/date/availability filters. Photos use a regular grid with stable tile geometry; Documents and Audio use a readable list by default. Avoid masonry that causes shifting reading order or makes keyboard navigation unpredictable.

Each tile has a bounded preview, filename, type and a link to source context. Multiple references to identical bytes are shown as “Used in 3 messages,” not silently discarded. Separate “Found in export, no linked message” items. Missing files use explicit placeholders and stay available through an availability filter.

Opening a file keeps the current filter and scroll position. Use the existing controlled media protocol, lazy thumbnails and decoder limits. A file detail can show all message references. Until content indexing exists, search copy says **Search filenames**, not **Search inside files**.

## 7. Sources and guided import

Sources is a readable list, not a grid of identical marketing cards. Each row carries source label/type, observed coverage, last import/update, and a specific status. Actions prioritize **View messages**, then **Update now** for connected accounts or **Import another export** for manual sources. Credential management, pause and disconnect live in Details.

Add source first asks **Import files** or **Connect email**, then shows provider-specific help within that route. This avoids advertising seamless sign-in for providers with advanced setup. Use text and small source icons; icons must not imply a connector exists.

```text
Get your export → Review what will be imported → Import → Review receipt
                  Source: WhatsApp / Family
                  Dates: observed sample, not full-history guarantee
                  Files: present / missing / not checked
                  Timezone: confirm when assumed
                  [Back]                       [Import]
```

Keep the form within approximately 640–720 px, aligned to a predictable content edge. A compact step label supplies orientation; avoid a large wizard graphic. Do not disable reading/search once a background import starts. Activity and the source row carry progress after navigation.

The completion receipt has a factual title such as **Messages added to your archive**, followed by new/already-present/failed counts and kept/missing files. Lead with **View imported messages**; secondary actions are Review issues and Add another source. No confetti or absolute “All safe” claim.

A partial import is not styled as an unqualified success. Use “Import finished with 3 issues,” keep successful data accessible, and offer a precise recovery action. If total count is unknown, show phase and processed count instead of a fabricated percentage.

## 8. First run, storage, Activity and optional AI

### First run

One short explanation, one primary **Add messages** action, and secondary **Open existing archive** and **Try sample archive** when available. Show the current/proposed storage location and how to change it, but do not display an unavailable relocation action before A7 is implemented. Keep privacy disclosure short and accurate. Detailed provider instructions appear after a source choice, not as a long checklist competing with the first action.

### Storage and backup

Use three clearly separated groups: **Archive location**, **Backups**, **Recovery**. Location is a selectable path with an OS reveal action where supported. Backup status says what was verified and when, with destination and a note if it is on the same physical drive when that can be established. Do not claim physical independence from paths alone.

Restore preview compares the current and proposed archive, explains replacement/reconnection, and leads to one explicit confirmation. Validation errors preserve the current archive. A disconnected external drive gets a dedicated recovery state with Locate archive / Retry / Choose another archive as supported; never an empty archive pretending to be a successful launch.

### Activity

Active operations come first; completed history follows as compact rows. Each row has operation, source, phase/state, time, counts and the relevant Cancel / Retry / View result action. Expand technical details on demand. A bounded recent-history list must not masquerade as the archive's permanent completeness record.

### AI

Keep ordinary search visually primary. Ask displays whether processing is local or remote, the configured destination and the actual consent scope. Retain local results if a provider fails. Do not draw an exact-payload approval UI until the backend binds approval to those bytes. Session consent must say “for this session.” Generated answers never acquire the visual status of an original archived message.

## 9. State and copy contract

| State | Example copy | Required action/behavior |
| --- | --- | --- |
| Empty archive | “Add messages to start your archive.” | Add source; isolated sample |
| No matches | “No messages match this search.” | Preserve query; show active restrictions; clear filters independently |
| Search pending | “Searching… Previous results shown.” | Retain rows; stale responses cannot replace newer ones |
| Search failed | “Search couldn't finish. Your previous results are still here.” | Retry; accessible error; no false empty result |
| Assumed timezone | “Timezone assumed” | Adjacent explanation of the actual assumption; correct only through supported flow |
| Unverified grouping | “Conversation grouping unverified” | Explain that neighboring messages cannot be confirmed |
| Missing attachment | “File wasn't included in this export.” | Show original filename; help/reimport route if applicable |
| Partial import | “1,240 messages added. 3 records need attention.” | Review retained issue details; open imported messages |
| Connected account expired | “Sign in again to continue updates.” | Reconnect; archived messages remain available |
| Manual source | “Export imported Nov 1.” | Never substitute “Up to date” without evidence |
| No verified backup | “No verified backup recorded.” | Create backup; explain archive versus second copy |
| Backup failure | “Backup wasn't completed. Your archive is unchanged.” | Reason/destination; Retry when safe |
| Link conflict | “This identity is linked to another person.” | Review current assignment; explicit reassignment |
| Settings saved | “Changes saved.” | Local status near Save; clear dirty state only on success |

Examples are templates, not new product claims. Copy must use real counts and reasons. Unknown values are shown as unknown. Avoid technical identifiers in the primary flow; keep them available in details/support export. Notifications and global status contain no message content or sensitive names by default.

## 10. Responsive and accessibility behavior

| Effective width | Composition | Interaction |
| --- | --- | --- |
| 1100 px and above | Sidebar + results + reader | Independent scrolling; opening a message preserves result focus until the user enters reader |
| 800–1099 px | Sidebar + active content | Reader replaces results; visible Back restores result, scroll, query and filters |
| Below 800 px | One active content area, navigation drawer | Back and main action stay visible; toolbar wraps; no hidden horizontal controls |
| Narrow stress widths / high zoom | Same compact model | Stack metadata label/value pairs; wrap actions; scroll long dialogs; no page-level horizontal clipping |

Use the existing breakpoints consistently in CSS and state logic. Desktop support does not imply a mobile app; 390 px checks are reflow stress tests. Test 640×480, 900×700, 1100×800, 1280×800, 1440×900, and effective 200% zoom, plus the user's actual viewport during implementation.

Preserve Cmd/Ctrl+K and `/` focus-search behavior, with `/` ignored in editable content. Arrow navigation operates only in its owning list; Enter opens; Escape closes the topmost safe transient surface before changing the reader. Native fields/media retain native behavior. Tabs use proper tab focus and arrow-key behavior; dialogs/drawers restore focus to their trigger and cannot leave background controls active while modal.

All actions need visible labels or accessible names; essential uncertainty is not tooltip-only. Announce search completion, job state transitions and errors without streaming every progress tick. Focus, selected, checked, error and loading states must remain distinguishable in both themes and forced-color environments. Use logical spacing properties and natural wrapping for long/localized text; preserve bidirectional message content with isolated metadata.

The project retains its existing WCAG 2.2 AA target. Color calculations alone do not establish conformance; native webview keyboard, assistive-technology, target-size and reflow checks are required.

## 11. Implementation sequence

UI packages extend A0–A12 in the [archive delivery plan](archive-experience-plan.md#11-delivery-plan). Sizes are relative: S focused, M several coordinated changes, L a complete feature/state system. No delivery dates are implied.

| Package | Scope / size | Dependencies | Acceptance |
| --- | --- | --- | --- |
| UI0 | Current baseline capture and design inventory / S | Existing working-tree changes | Capture current native light/dark wide/compact states with synthetic data; list current versus proposed components; preserve user edits |
| UI1 | Tokens, controls and component states / M | UI0 | Shared typography/spacing/action hierarchy; stronger control-boundary token; contrast and keyboard states verified; no product behavior changes |
| UI2 | Search/results/reader hierarchy / M | UI1 | Content-first email, readable chat context, clear selection/focus, functional filters/export; long-content and stale-response behavior unchanged |
| UI3 | Sources, first import and receipt / M | UI1; A1/A2 for receipts | Complete file → preview → import → result journey; partial/cancel/error states; honest provider entry |
| UI4 | Storage, backup, Activity and settings / M | UI1; A1/A3; A7 for location controls | Clear update/import/backup distinctions; restore preview; correct save scope; external-drive failure route when backend exists |
| UI5 | People / L | UI1/UI2; A5 | Reviewable, reversible identity links; conflict/empty/unknown-time states; shared reader and filters |
| UI6 | Files / L | UI1/UI2; A6 | Stable list/grid; bounded local previews; missing/unlinked/multiple-reference states; source context and export |
| UI7 | Responsive, accessibility and polish closure / M | Runs with each package | Native light/dark/system checks; keyboard/reader/zoom; all material findings resolved or explicitly recorded; refreshed synthetic screenshots |

Recommended first increment: **UI0 → UI1 → UI2**, then UI3/UI4. Do not postpone accessible error states to the final polish package. People and Files follow their domain contracts; meanwhile their screen specifications remain ready to implement.

Implementation should extract feature controllers and shared UI primitives as described in A4, while keeping the existing TypeScript/Tauri stack. Favor a small local component vocabulary over introducing a new component framework for appearance. Remove duplicate CSS or inline overrides only when the relevant component is migrated and its consumers are verified.

## 12. Review and acceptance procedure

For each implemented package, first inspect the rendered app in a batch across relevant widths, themes and states. Fix material issues together, then capture again. Use Impeccable's appropriate audit/critique/polish/harden workflows at implementation time, following their then-current instructions. A planning document is not a passed design review.

Required synthetic content pack:

- Empty vault, one message, 50-result page and a large archive with realistic source counts.
- Same display name across sources; unusually long sender/address; missing subject; long unbroken filename/URL.
- Multi-paragraph email, HTML email, long chat, group chat, emoji, accented Latin, Cyrillic and right-to-left content.
- No date, assumed date, unverified group, missing file, multiple references, invalid export and partial import.
- Loading, offline, revoked sign-in, failed save, low disk, cancelled job, missing drive, corrupt backup and restored archive.

Definition of professional finish:

1. Within a few seconds, the user can identify the current task, content scope and primary action.
2. A result communicates who/what/when/source without opening it, and the reader prioritizes the actual message.
3. Every interactive component has tested applicable states; no click target depends on hover-only discovery.
4. The same spacing/type/control vocabulary applies to Archive, Sources, Settings and Activity.
5. No clipped actions, overlapping text, accidental nested scrolling or lost position at supported sizes/zoom.
6. Search, import, People and Files preserve source truth and expose material uncertainty.
7. A user can explain the difference between an imported archive, a recent update and a verified backup.
8. Candidate screenshots and usability observations accompany test results; no screenshots of stale fixtures are presented as current UI proof.

Reuse the existing five-user first-session protocol and retrieval tasks. Record actual completion, misunderstandings and support needed; do not invent user feedback or turn visual preference into evidence of task success.

## 13. Decisions and handoff

Recommended design direction is sufficiently specified for UI0–UI2. Remaining choices are a possible identity replacement if the user requests it, optional density preference after real-content testing, and optional resizable panes. None is required to save or review this plan.

The design-system companion is a **target specification**. Adopt changes into the existing UX contract and implementation evidence as they land. Keep visual decisions in documentation and semantic tokens; do not scatter fresh conventions across individual screens. A future implementation must re-inspect the working tree and skill instructions before editing UI.

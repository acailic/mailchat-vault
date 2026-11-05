# UX specification

Status: **mixed**. This is the interaction contract for [P02–P09](product-plan.md#functional-requirements). The workspace/navigation and search/selection sections are **current**, grounded in the M3.1/M3.2 implementation (#44/#46, recorded in the [roadmap](implementation-roadmap.md#completion-tracking)); the import/first-success (M3.3), connected-email (M3.4), media/storage (M4), and optional-Ask sections remain the reviewed **proposed** design for their owning slices. Existing behavior is assessed separately in [Project assessment](project-assessment.md).

## Workspace and navigation

The archive workspace has three areas: source navigation, results, and reading. Search remains local and is the primary action. Imports, account configuration, and maintenance live outside the reading surface.

```text
Desktop, width >= 1100px
┌──────────────────┬────────────────────────────┬─────────────────────────────┐
│ Mailchat Vault   │ Search messages…      [Go] │ Ana · Delivery date         │
│ All messages    │ [Person] [Dates] [Files]    │ Date · source · conversation │
│ Sources         │ [Active filters] [Clear]   │ Original message            │
│   Personal mail │ 1–50 of 1,234   Relevance  │                             │
│   Family chats  │ Author / subject / date    │ [Available attachment]      │
│                 │ Match snippet…             │ Conversation context        │
│ [Add source]    │ Next result…               │ [Load earlier] [Load later] │
│ Activity        │                            │ Provenance ▸                │
│ Settings        │ [Previous]       [Next]    │                             │
└──────────────────┴────────────────────────────┴─────────────────────────────┘
```

At 1100px and above use a 208px navigation column, a results area of at least 320px, and a reading area of at least 400px. Allocate remaining width to the reading area; give each area independent vertical scrolling. The initial window is 1280×800 where available, with 640×480 minimum; respect smaller available screens.

From 800px to 1099px show navigation and results; opening a message replaces the results area with a reading view and a visible “Back to results” action. Below 800px show one content area and a labeled navigation toggle. Preserve query, filters, page, selection, and scroll when returning. All controls remain reachable at 200% zoom; a viewport narrower than the nominal minimum uses the compact layout rather than horizontal clipping.

**Current (M3.1).** The three-area shell, the 1100px/800px breakpoints with independent per-area scrolling, the 1280×800 default window with a 640×480 minimum, the medium-layout Back-to-results swap, and the compact drawer (labeled toggle, `aria-expanded`, Escape close, removed from the accessibility tree when closed) are implemented in [workspace.ts](../src/workspace.ts)/[state.ts](../src/state.ts), with semantic design tokens ([tokens.css](../src/styles/tokens.css); light/dark/System via the theme setting) and the bundled Source Sans 3 (SIL OFL 1.1 license committed at [src/assets/fonts](../src/assets/fonts)); 1280px at 200% zoom resolves to the compact layout with no horizontal clipping. Navigation destinations are All messages, Add source, Activity, and Settings, with per-source scope chips in the navigation column; imports and IMAP configuration live in the Add source view and job progress in Activity, and a started job keeps running when navigating away. Not yet implemented: per-source status text ("Syncing"/"Needs sign-in"/"Last synced") on the chips (M3.4) and the Storage & backup settings group (M4.4); the Add source view still hosts the interim import/sync controls rather than the dedicated setup views below (M3.3/M3.4).

### Navigation rules

- All messages browses newest first, including on first launch with an existing archive. An empty vault shows setup guidance instead.
- Selecting a source scopes the current search and resets pagination. It does not erase typed search text. “All messages” clears source scope while retaining other filters.
- Source labels are user-facing names, not provider namespace strings. A status indicator has text such as “Syncing”, “Needs sign-in”, or “Last synced today at 09:20”. File imports show “Last imported”.
- Add source opens a dedicated setup view with “Import files” and “Connect email”. Activity lists jobs; Settings contains Appearance, AI, Storage & backup, and source management links.
- Closing setup returns to the previous workspace. Once a job starts it continues when navigating away; its progress remains in Activity.

### Search and selection

Submit on Enter or Search. Filter changes submit immediately; reset to page one. Do not add remote suggestions or automatic search on every keystroke in this release. Empty text means browse. Text defaults to Relevance; empty text defaults to Newest. Users may choose Newest for text searches; preserve that choice until cleared.

Show applied source, person, date, and “Has attachment” filters as removable chips. An attachment filter matches both available and explicitly missing references; show file availability on the result. Date bounds use the workspace timezone and include the entire selected days. Reject reversed ranges inline before invocation; unknown dates are excluded when bounded and explained in the filter help.

Results are a flat ordered list; do not regroup them by conversation. Each row shows sender, subject or conversation label, date, source, two-line snippet, and attachment availability when relevant. Use backend hit markers for highlighting. Show “No date” or a time-uncertainty indicator instead of a fabricated timestamp.

Keep 50 results per page, exact count, and Previous/Next. Preserve existing stale-response protection. After archive changes, mark results as having new data and offer Refresh; do not move rows or change selected content under the user's cursor. A fresh submission resets selection and page; returning from reading restores both.

Reading shows the full plain-text message first, then attachments and bounded chronological context. Email exposes From, To, Cc, subject, and reply relationships. Chat exposes participants, replies, and conversation name. Load earlier/later context in pages without losing the anchor. An unresolved legacy grouping shows “Conversation could not be verified” and the selected message alone, not guessed neighbors.

**Current (M3.2).** Search/selection runs on the v2 reading surface (`search_archive_v2` / `message_detail_v2` / `conversation_context_v2` / `list_import_scopes_v2` / `export_messages_v2`): flat results with stable ranking, removable filter chips (source, author, dates, "has attachment", import scope) plus a sort choice, 50-per-page exact counts with Previous/Next, stale-response protection centralized in `SequenceGuard`/`Sequence`, the archive-revision "new data" notice with Refresh, backend hit-marker highlighting, "No date" for unknown instants, context pages by message-id cursor without losing the anchor, the honest unverified-grouping reading pane, copy message with provenance / copy `mailchat://message/<id>` link, pasted-locator deep links, and restorable selection and scroll on Back. Snippets are bounded to 500 Unicode characters including hit markers (a 240-character body prefix when browsing). Reversed day bounds are rejected inline before invocation. Selection export writes UTF-8 text or JSON with provenance for the checked rows in their order (bounded to 10,000 messages per file) to a native-picked destination; the reading pane shows provenance behind its expandable disclosure with raw JSON behind the "Technical details" fold. Not yet implemented: persisting the workspace timezone (each request defaults to the OS zone).

Copy message includes readable text and provenance; Copy link uses an internal `mailchat://message/<id>` locator handled within the app. It is a vault-local reference, not a public URL and not a cross-vault identifier. Export selection offers UTF-8 text or JSON with provenance; attached files require a separate explicit export selection. Metadata disclosure includes original timestamp, source occurrence, import job, and legacy uncertainty; raw JSON stays behind “Technical details”.

## Import and first success

**Proposed (M3.3)** — the current surface is the interim pre-preview flow: chat exports use the native picker, while the mbox field still accepts a typed path, and imports start immediately with job progress in Activity; there is no preview/confirmation step, sample vault, or capacity check yet.

1. Empty vault presents “Add your first source”, a short local-storage explanation, and a secondary “Try a sample archive”. Sample data uses an isolated temporary vault; leaving it restores the personal vault without mixing demo rows into real data.
2. Choose Import files. Native selection accepts mbox, chat TXT/JSON, a directory, or ZIP. Show source-specific export instructions locally; unsupported formats receive an actionable explanation.
3. Preview shows detected format, chosen source collection, conversation identity if known, sample messages, timestamp interpretation, detected/missing media, total input bytes, and free-space estimate. Preview is bounded; do not claim exact message counts without a complete scan.
4. Existing source selection is explicit. Telegram stable chat IDs may identify the conversation within that source. WhatsApp prompts “New conversation” or “Add to an existing conversation”; display the last import and a small sample before linking.
5. Ambiguous date order requires day-first/month-first selection. Naive timestamps require a timezone, preselected from the OS and explicitly confirmed. DST overlaps require choosing the earlier/later interpretation for affected entries; gaps remain unresolved with their original text unless corrected. Permit import with an explicit unresolved status.
6. Explain that supplied files will be copied and that absent media cannot be recovered. Show available space and configurable capacity limits before starting; lack of required space disables Start with guidance.
7. Start returns a job immediately. Display stage, messages processed, new/existing/failed counts, media bytes copied, and Cancel. Use indeterminate progress when the total is unknown.
8. Completion offers “View imported messages” with an import-job filter. Partial success offers “Review issues” and “Retry failed items”. Show skipped system/service entries separately from parse failures.

Cancel file selection makes no changes. Cancelling preview discards temporary files. Cancelling a running import preserves committed records and files; the summary states that partial data remains. Retry targets the failed/unfinished work, not a duplicate source.

## Connected email

**Proposed (M3.4)** — today the app-password/Google-sign-in IMAP controls live in the Add source view (sources panel) with no account page, credential-update flow, or disconnect/local-deletion separation yet.

Choose Gmail preset or Custom IMAP. Keep host, port, and TLS details under Advanced for Gmail. Use visible labels for email/username and password; explain app-password requirements without promising OAuth. Validate host, port, and required values before testing the connection.

Connection testing runs in the background and does not save an invalid replacement password over a working one. After success, select folders (INBOX default), name the account, and choose manual sync or opt-in 15-minute sync. The credential store notice reads “Saved in your system keychain” or “Kept for this session only”. Do not present memory fallback as persistent storage.

An account page shows folders, last attempt/success, message/media counts, failures, Sync now, Cancel, credentials, and Disconnect. Authentication failure offers Update credentials; network failure offers Retry. A folder failure does not conceal success in other folders. Disconnect confirmation explains that scheduled access stops and credentials are removed while archived content remains.

Deleting local archived content is a separate action with source name and counts. It never deletes remote mail. Require confirmation, exclude concurrent jobs for that source, and remove shared object bytes only when unreferenced.

## Media and storage

Attachment rows show filename, type, size, and availability. Available images have bounded thumbnails and an accessible full preview; audio/video have user-initiated controls with no autoplay. Unsupported formats offer Export or Open externally after an explicit user action. PDF and office files use external opening in this release. Executable files have Export only. Never render HTML mail or active attachments as application HTML.

Missing references say “File was not included in this export”. Failed copies say “Could not preserve this file” with Retry and the job issue. Unmatched export files appear under the source's Files tab as “Not linked to a message”; do not assign them by proximity or filename guessing.

Storage & backup shows database, originals, media, staging, and free-space sizes. Backup opens a destination dialog, explains that the backup is not app-encrypted and excludes credentials, then reports progress. Restore first shows backup metadata, integrity results, required disk space, and a warning that switching archives replaces the active view. The old vault remains recoverable. Settings offers “Return to previous vault” after a successful switch. Do not automatically delete recovery copies.

## Optional Ask

Ask is a secondary mode beside Search in the results area, not a competing full-width section. Ordinary search remains available throughout.

AI settings default off even if provider keys are present. Show endpoint host, model, and credential availability; store newly entered keys in the OS keychain or session memory. Preserve compatible environment configuration as an available configuration source, but it never overrides the consent switch.

For each question, show that the question and date/timezone context will go to the selected endpoint. “Find matching messages” is the explicit send action. Run translation, validate the plan, intersect with active source/person/date/file filters, and show local results. Explain fallback in plain language; keep FTS details expandable.

Before synthesis, show a preview of the exact numbered context: at most eight results, body text capped at 500 Unicode characters each, bounded author metadata and timestamps, no attachments or raw JSON. “Generate answer” approves that payload and the question for that endpoint. Changing the endpoint, question, filters, or result snapshot invalidates approval. Declining leaves local results usable. No persistent blanket approval is offered in this release.

Answers label their evidence scope (“Based on 6 matching messages”). Citation buttons open the referenced original in the same reading pane. Unknown citation numbers are not clickable and cause a visible unsupported-citation note. Failure leaves results available and offers retry. Never label a generated answer as proof of archive completeness.

## Accessibility and interaction

- Use real buttons, labels, landmarks, headings, and native fields. Do not implement mouse-only list items. Keep results as a semantic list with focusable message actions.
- Cmd/Ctrl+K or `/` focuses search; ignore `/` while typing in editable fields. Arrow keys move result selection only when results own focus; Enter opens; Escape returns/closes without erasing the query. Tab always reaches controls. Native audio/video and text inputs retain native key behavior.
- In compact reading views, focus the message heading on open and restore the initiating result on Back. In wide layouts preserve keyboard focus on the selected result unless the user enters the reading pane.
- Setup/confirmation dialogs trap focus, have a labeled purpose, close with Escape when safe, and restore the trigger. Progress never steals focus.
- Announce completed searches, job transitions, and errors using appropriate live regions; throttle progress announcements. Visible text accompanies icons and colors.
- Target WCAG 2.2 AA contrast and interaction behavior. Verify with the checks in [Validation](validation-and-release.md#ux-and-accessibility), rather than claiming conformance from CSS alone.

## Visual direction

Use a quiet reference-library aesthetic with dense but comfortable message rows. Bundle Source Sans 3 with its license, falling back to native UI fonts. Body text is 15px/1.5, metadata 13px, section titles 18px, and page titles 24px. Use a 4px spacing scale, 8–12px row padding, restrained 4–6px radii, and borders rather than decorative card shadows.

Define semantic tokens for canvas, panel, text, muted text, border, accent, selection, match, success, warning, and danger. Start with warm neutral light surfaces and a forest-green accent; provide a dark equivalent and System/Light/Dark settings. Final token values must pass measured contrast. Use an amber search-match highlight distinct from selection. Keep message lines near 70–85 characters where space permits.

Motion is limited to 120–180ms opacity/transform state transitions and disabled for reduced-motion preferences. Do not animate message reorderings, use autoplay media, fetch fonts remotely, or decorate the workspace with metrics unrelated to retrieval.

## State and recovery matrix

| State | Presentation | Available recovery |
| --- | --- | --- |
| Empty vault | First-source guidance | Import, connect, sample |
| No matches | Query and active filters; no blame | Edit terms, clear filters, browse source |
| Loading search | Existing rows visibly marked as previous results; busy status | Submit replacement search |
| Search failure | Last successful results labeled as previous | Retry; edit filters |
| Missing selected message | “Message is no longer in this vault” | Back to results and refresh |
| Partial import/sync | Counts plus item/folder issue list | Retry issues; open successful data |
| Cancelled job | Committed progress and remaining work | Resume/retry |
| Low disk space | Stage and required/available bytes | Choose capacity/destination; retry after freeing space |
| Keychain unavailable | Session-only notice | Continue for session or retry storage |
| Unsupported preview | Preserved file metadata | Export or explicitly open externally |
| Restore validation failure | Specific damaged/missing components | Keep current vault; select another backup |

## Maintenance

Update this document with interaction changes and keep the [roadmap](implementation-roadmap.md) acceptance cases aligned. Screenshots and prototypes added during implementation must use synthetic data. Validate the wide, compact, empty, error, and keyboard flows before accepting a visual redesign.

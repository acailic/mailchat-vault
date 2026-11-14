# Email rendered preview (Rendered/Raw toggle) — Design

Date: 2026-11-14
Status: Approved by the user (approach A: HTML in `raw_json` + sandboxed iframe)

## Problem

The reading pane renders every message body as plain text (`renderDetail2`,
`src/render2.ts`). Emails that carry an HTML part get no browser-like
preview at all — worse, HTML-only mails surface their raw HTML markup as
the body text, because ingest keeps only the `text/plain` part
(`extract_text`, `crates/vault-core/src/mail.rs`) and falls back to the
undecoded-structure top-level body when no text part exists.

The user asked for a browser-like preview beside the raw text: a
**Rendered/Raw toggle** on the email detail view.

## Decision

Approach A — store the HTML part of each mail inside the existing
`raw_json` and render it, on demand, in a fully sandboxed iframe.
No database schema change, no new IPC channel: `raw_json` already flows
to the detail view.

Scope agreed with the user:

- **New imports only.** Existing archives keep their raw-text view; there
  is no backfill/re-import mechanism in this feature.
- **Security-first rendering.** No scripts, no remote subresources
  (tracking pixels blocked by construction), no navigation.

## Backend — `crates/vault-core/src/mail.rs`

- New `extract_html(mail: &ParsedMail) -> Option<String>`, symmetric to
  `extract_text`: walks the MIME tree, collects `text/html` leaf bodies
  (decoded via the existing `decoded_body`), joins them the same way.
- In `parse_raw_mail_canonical`, when the extracted HTML is non-empty and
  at most **512 KB** (decoded bytes), add it to `raw_json` under the key
  `"html"`. Oversized or empty → key omitted entirely.
- Unchanged on purpose:
  - `body` (FTS stays subject + plain text; HTML is never searched),
  - `revision_hash` (identity semantics stay stable — HTML is
    presentational),
  - dedup/skip logic (HTML lands only in newly stored rows).
- All three ingest paths (`.eml`/folder/zip, `.mbox`, IMAP) share
  `parse_raw_mail`, so one change covers them all.

## Frontend viewmodel — `src/viewmodel.ts`

- `htmlOf(rawJson: string): string | null` following the existing
  `mediaOf`/`attachmentOf` pattern: parse the JSON, return the `"html"`
  string when present, `null` otherwise (invalid JSON → `null`).

## Detail view — `src/render2.ts` (`renderDetail2`)

- When `htmlOf(detail.raw_json)` returns HTML, the "Message" section gets
  a segmented **Rendered | Raw** control; **Rendered is the default** for
  that message. No HTML → today's rendering, unchanged, no toggle.
- Rendered mode: a single `<iframe>` with:
  - `sandbox` attribute with **no** allow-* flags (no scripts, unique
    origin, no popups, no navigation),
  - `srcdoc` containing
    `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:">`
    followed by the stored HTML — inline styles render, everything remote
    is blocked.
- Raw mode: exactly the current body rendering, including search-term
  highlighting. Highlighting does not apply in Rendered mode.
- The per-message mode is view state only — not persisted (YAGNI).
- Styling in `src/styles/app.css` with existing design tokens: full-width
  iframe, capped height with internal scrolling.

## Security

- Stored HTML never reaches `innerHTML` of the main document — its only
  destination is the iframe's `srcdoc` attribute.
- `sandbox=""` + CSP `default-src 'none'` means: no script execution, no
  remote images/CSS/fonts (tracking pixels cannot phone home), no frame
  navigation, no access to the app origin.
- The existing test posture ("never executes message content") is
  extended, not weakened.

## Testing

- **Rust (`mail.rs` unit tests):** `multipart/alternative` mail stores the
  `text/html` body in `raw_json.html`; plain-only mail omits the key;
  HTML over 512 KB is dropped (key omitted); html-only mail keeps today's
  body fallback; existing archive rows are untouched (no migration).
- **Vitest:** `htmlOf` contract; toggle appears iff HTML exists; default
  mode is Rendered; iframe carries `sandbox` and `srcdoc` with the CSP
  meta; Raw mode still highlights terms; existing no-execution tests pass.

## Non-goals

- Backfill/re-import of existing messages.
- Loading remote content (images, CSS) — permanently blocked here.
- Opening links from the rendered view.
- `cid:` inline images (attachments keep flowing through the existing
  attachment UI).
- HTML→text conversion for search/`body`.

## Alternatives considered

- **B: dedicated `html` column + new IPC channel** — cleaner separation,
  but schema/query/IPC churn for the same result. Rejected.
- **C: DOMPurify sanitization into the main DOM** — new dependency and a
  harder-to-prove security story than an empty sandbox. Rejected.

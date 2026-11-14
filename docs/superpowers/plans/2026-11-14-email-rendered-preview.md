# Email Rendered Preview (Rendered/Raw Toggle) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Store each email's `text/html` part in `raw_json` at ingest and show a Rendered/Raw toggle in the v2 reading pane, rendering HTML only inside a fully sandboxed iframe.

**Architecture:** One backend change (`extract_html` in `vault-core/src/mail.rs` adds an `"html"` key to the existing `raw_json`) flows to the frontend through the existing detail IPC — no schema or IPC changes. The reading pane (`src/render2.ts`) gains a segmented Rendered/Raw toggle; Rendered builds an `<iframe sandbox srcdoc>` with a deny-all CSP. Spec: `docs/superpowers/specs/2026-11-14-email-rendered-preview-design.md`.

**Tech Stack:** Rust (`mailparse`, `serde_json`), TypeScript + vitest (happy-dom), hand-rolled DOM builders (no framework).

## Global Constraints

- Run all commands from the repo root: `/home/nistrator/storage/github-storage/firstmate/projects/mailchat-vault`.
- Work on branch `feat/email-rendered-preview` (create from `main` in Task 1; do NOT touch the default branch directly). Repo workflow is squash-merge PRs.
- CI gates that every task must keep green: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `npm run check`, `npm test`. Run `cargo fmt` before each Rust commit.
- No new dependencies (npm or crates.io).
- Stored HTML may ONLY ever reach the DOM as an iframe `srcdoc` attribute — never `innerHTML`/`insertAdjacentHTML` of the main document.
- HTML size cap: **512 × 1024 bytes** of the decoded string (`MAX_HTML_BYTES`); oversized → key omitted.
- `body` (FTS) and `revision_hash` semantics must NOT change.
- Comments, identifiers, commit messages: English. Conventional commit style (`feat: …`, `test: …`).
- Commit only the files your task touches; never `git add -A`. An untracked `.codex/` directory exists — leave it alone.

---

### Task 1: Backend — keep `text/html` in `raw_json`

**Files:**
- Modify: `crates/vault-core/src/mail.rs` (add `MAX_HTML_BYTES`, `extract_html`, insert into `raw_json` in `parse_raw_mail_canonical`; tests in the existing `mod tests`)

**Interfaces:**
- Consumes: existing `decoded_body(mail: &ParsedMail<'_>) -> Option<String>` (mail.rs:494) and the `raw_json` construction block (mail.rs:220-240).
- Produces: `raw_json` objects that may carry a `"html"` string key for newly ingested mails. Later tasks rely on exactly this key name.

- [ ] **Step 1: Create the feature branch**

```bash
git checkout -b feat/email-rendered-preview
```

Expected: branch created from current `main`.

- [ ] **Step 2: Write the failing tests**

In `crates/vault-core/src/mail.rs`, inside `mod tests` (starts at mail.rs:622), add these helpers/tests (after the existing `moderate_nesting_still_parses` test):

```rust
    /// A multipart/alternative mail with a plain and a quoted-printable
    /// HTML part — the shape the rendered preview is built for.
    fn alternative_mail() -> Vec<u8> {
        "\
From: a@b.c\nSubject: alt\nDate: Mon, 2 Nov 2026 10:00:00 +0100\n\
MIME-Version: 1.0\nContent-Type: multipart/alternative; boundary=\"bb\"\n\n\
--bb\nContent-Type: text/plain; charset=utf-8\n\nplain telo\n\
--bb\nContent-Type: text/html; charset=utf-8\n\
Content-Transfer-Encoding: quoted-printable\n\n<p>telo</p>\n\
--bb--\n"
            .as_bytes()
            .to_vec()
    }

    #[test]
    fn html_part_is_kept_in_raw_json_for_the_rendered_preview() {
        let parsed = parse_raw_mail_canonical(&alternative_mail(), "mbox", "f").unwrap();
        // FTS body stays plain text — HTML never leaks into it.
        assert_eq!(parsed.message.body, "alt\n\nplain telo");
        let raw: serde_json::Value =
            serde_json::from_str(&parsed.message.raw_json).unwrap();
        assert_eq!(raw["html"], "<p>telo</p>");
    }

    #[test]
    fn plain_only_mail_has_no_html_key() {
        let mail = b"From: a@b.c\nSubject: s\n\nsamo tekst\n";
        let parsed = parse_raw_mail_canonical(mail, "mbox", "f").unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&parsed.message.raw_json).unwrap();
        assert!(raw.get("html").is_none());
    }

    #[test]
    fn oversized_html_is_dropped() {
        let big = "<p>".to_string() + &"x".repeat(MAX_HTML_BYTES) + "</p>";
        let mail = format!(
            "From: a@b.c\nSubject: s\nDate: Mon, 2 Nov 2026 10:00:00 +0100\n\
MIME-Version: 1.0\nContent-Type: multipart/alternative; boundary=\"bb\"\n\n\
--bb\nContent-Type: text/plain\n\nkratko\n\
--bb\nContent-Type: text/html\n\n{big}\n--bb--\n"
        );
        let parsed = parse_raw_mail_canonical(mail.as_bytes(), "mbox", "f").unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&parsed.message.raw_json).unwrap();
        assert!(raw.get("html").is_none());
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

```bash
cargo test -p vault-core mail::tests
```

Expected: compile ERROR — `MAX_HTML_BYTES` not found, so the whole test binary fails to build. (`plain_only_mail_has_no_html_key` may pass once it compiles; that is fine, it guards regressions.)

- [ ] **Step 4: Implement**

In `crates/vault-core/src/mail.rs`:

(a) Add next to `MAX_MIME_DEPTH` (mail.rs:19):

```rust
/// Maximum decoded `text/html` size kept for the rendered preview
/// (`raw_json.html`); larger bodies are dropped and the reading pane
/// falls back to the raw text view.
const MAX_HTML_BYTES: usize = 512 * 1024;
```

(b) Add next to `extract_text` (mail.rs:473), mirroring it:

```rust
/// Extract every `text/html` leaf body, joined — the rendered-preview
/// counterpart of [`extract_text`]. `None` when the mail carries no
/// HTML part at all.
fn extract_html(mail: &ParsedMail<'_>) -> Option<String> {
    if mail.subparts.is_empty() {
        return if mail.ctype.mimetype == "text/html" {
            Some(decoded_body(mail).unwrap_or_default())
        } else {
            None
        };
    }
    let leaves: Vec<String> = mail.subparts.iter().filter_map(extract_html).collect();
    if leaves.is_empty() {
        None
    } else {
        Some(leaves.join("\n\n"))
    }
}
```

(c) In `parse_raw_mail_canonical`, after the `let text = …` line (mail.rs:158) add:

```rust
    // Rendered-preview HTML: the decoded text/html part(s), kept only
    // when present and within the size bound. It never enters `body`
    // (FTS stays plain text) or the revision hash (presentation only).
    let html = extract_html(&mail).filter(|h| h.len() <= MAX_HTML_BYTES);
```

(d) Inside the existing `if let Some(obj) = raw_json.as_object_mut()` block (mail.rs:227), after the `references` insert, add:

```rust
        if let Some(html) = &html {
            obj.insert("html".into(), json!(html));
        }
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cargo test -p vault-core mail::tests
cargo fmt
```

Expected: all `mail::tests` PASS (including the pre-existing ones — the `mail_with_attachments` fixture contains a `text/html` part, and none of the existing tests assert exact `raw_json` contents).

- [ ] **Step 6: Commit**

```bash
git add crates/vault-core/src/mail.rs
git commit -m "feat(mail): keep the text/html part in raw_json for the rendered preview"
```

---

### Task 2: Viewmodel — `htmlOf`

**Files:**
- Modify: `src/viewmodel.ts` (add `htmlOf` next to `mediaOf`/`attachmentOf`)
- Test: `src/viewmodel.test.ts`

**Interfaces:**
- Consumes: existing private `parseRawJson(rawJson: string): Record<string, unknown>` (src/viewmodel.ts:90).
- Produces: `export function htmlOf(rawJson: string): string | null` — the decoded HTML body when the key `"html"` is a non-empty string, else `null`. Task 3 calls exactly this.

- [ ] **Step 1: Write the failing tests**

In `src/viewmodel.test.ts`, add `htmlOf` to the existing import from `"./viewmodel"`, then add (next to the existing `mediaOf`/`attachmentOf` describe blocks, around viewmodel.test.ts:248):

```ts
describe("htmlOf", () => {
  it("returns the stored HTML body when present", () => {
    expect(htmlOf(JSON.stringify({ html: "<p>hi</p>" }))).toBe("<p>hi</p>");
  });

  it("returns null when absent, empty, non-string, or the JSON is invalid", () => {
    expect(htmlOf(JSON.stringify({ subject: "s" }))).toBeNull();
    expect(htmlOf(JSON.stringify({ html: "" }))).toBeNull();
    expect(htmlOf(JSON.stringify({ html: 7 }))).toBeNull();
    expect(htmlOf("not json")).toBeNull();
  });
});
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
npx vitest run src/viewmodel.test.ts
```

Expected: FAIL — `htmlOf` is not exported.

- [ ] **Step 3: Implement**

In `src/viewmodel.ts`, right after `attachmentOf` (src/viewmodel.ts:118):

```ts
/** The stored rendered-HTML body of an email message, when present
 * (invalid raw_json → null; empty or non-string → null). */
export function htmlOf(rawJson: string): string | null {
  const html = parseRawJson(rawJson).html;
  return typeof html === "string" && html.length > 0 ? html : null;
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
npx vitest run src/viewmodel.test.ts
```

Expected: PASS (all viewmodel tests).

- [ ] **Step 5: Commit**

```bash
git add src/viewmodel.ts src/viewmodel.test.ts
git commit -m "feat(viewmodel): htmlOf reads the stored email HTML body"
```

---

### Task 3: Reading pane — Rendered/Raw toggle with sandboxed iframe

**Files:**
- Modify: `src/render2.ts` (import `htmlOf`; rework the "Message" body block in `renderDetail2`, around render2.ts:404-412)
- Modify: `src/styles/app.css` (detail styles region, after `.detail-card .body` at app.css:707)
- Test: `src/render2.test.ts`

**Interfaces:**
- Consumes: `htmlOf(rawJson: string): string | null` from Task 2; existing `appendSegments` (from `./render`), `highlightSegments` (from `./viewmodel`); `detail.raw_json` and `detail.body` on `MessageDetail`.
- Produces: DOM contract — toggle `div.mode-toggle[role="group"]` containing `button.mode-btn` ("Rendered", "Raw") with `aria-pressed`; rendered mode shows `iframe.rendered-frame` with `sandbox=""` and a `srcdoc` starting with the CSP meta; raw mode shows the current `.body` rendering.

- [ ] **Step 1: Write the failing tests**

In `src/render2.test.ts`, inside the existing reading-pane `describe` (the one holding `"raw JSON stays collapsed behind Technical details"`), append:

```ts
  const htmlCtx: ConversationContext = {
    ...ctx,
    anchor: {
      ...ctx.anchor,
      raw_json: JSON.stringify({ html: "<p>telo pisma</p>" }),
    },
  };

  it("email HTML renders inside a sandboxed iframe behind a Rendered/Raw toggle", () => {
    const el = render({ ctx: htmlCtx, entries: contextEntries(htmlCtx) });
    const buttons = [...el.querySelectorAll("button.mode-btn")] as HTMLButtonElement[];
    expect(buttons.map((b) => b.textContent)).toEqual(["Rendered", "Raw"]);
    // Default mode is Rendered: sandboxed iframe, CSP meta, the HTML
    // itself only ever inside the srcdoc attribute.
    const frame = el.querySelector("iframe.rendered-frame") as HTMLIFrameElement;
    expect(frame).not.toBeNull();
    expect(frame.getAttribute("sandbox")).toBe("");
    expect(frame.getAttribute("srcdoc")).toContain("default-src 'none'");
    expect(frame.getAttribute("srcdoc")).toContain("<p>telo pisma</p>");
    expect(el.querySelector(".detail-card .body")?.innerHTML).not.toContain(
      "<p>telo pisma</p>",
    );
    expect(buttons[0].getAttribute("aria-pressed")).toBe("true");
    // Raw restores today's highlighted plain-text body and drops the frame.
    buttons[1].click();
    expect(el.querySelector("iframe.rendered-frame")).toBeNull();
    expect(buttons[0].getAttribute("aria-pressed")).toBe("false");
    expect(buttons[1].getAttribute("aria-pressed")).toBe("true");
    expect(el.querySelector(".detail-card .body")?.textContent).toContain("arrives");
    // Rendered comes back.
    buttons[0].click();
    expect(el.querySelector("iframe.rendered-frame")).not.toBeNull();
  });

  it("no stored HTML: no toggle, plain body stays", () => {
    const el = render(); // fixture raw_json is {"keep": 1}
    expect(el.querySelector("button.mode-btn")).toBeNull();
    expect(el.querySelector("iframe.rendered-frame")).toBeNull();
    expect(el.querySelector(".detail-card .body")?.textContent).toContain("arrives");
  });
```

(`ctx` and `render` are the file's existing fixture/helpers; `contextEntries` is already imported.)

- [ ] **Step 2: Run the tests to verify they fail**

```bash
npx vitest run src/render2.test.ts
```

Expected: the new first test FAILS (no `iframe.rendered-frame`, no `mode-btn`); the second PASSES (regression guard).

- [ ] **Step 3: Implement in `src/render2.ts`**

(a) Add `htmlOf` to the existing `./viewmodel` import block (which already has `highlightSegments`, `parseSnippet`, `splitTerms`):

```ts
import {
  highlightSegments,
  htmlOf,
  parseSnippet,
  splitTerms,
} from "./viewmodel";
```

(b) Replace the body block in `renderDetail2` (currently exactly these lines, render2.ts:404-412):

```ts
  const body = document.createElement("div");
  body.className = "detail-body";
  const bodyHeading = document.createElement("h3");
  bodyHeading.textContent = "Message";
  const bodyText = document.createElement("div");
  bodyText.className = "body";
  appendSegments(bodyText, highlightSegments(detail.body, terms));
  body.append(bodyHeading, bodyText);
```

with:

```ts
  const body = document.createElement("div");
  body.className = "detail-body";
  const bodyHeading = document.createElement("h3");
  bodyHeading.textContent = "Message";
  const bodyText = document.createElement("div");
  bodyText.className = "body";
  const html = htmlOf(detail.raw_json);
  if (html === null) {
    appendSegments(bodyText, highlightSegments(detail.body, terms));
    body.append(bodyHeading, bodyText);
  } else {
    // Rendered/Raw toggle. The stored HTML is presentation from an
    // untrusted sender: it is only ever placed into a sandboxed
    // iframe's srcdoc (no scripts, unique origin, no navigation, and
    // a CSP that blocks every remote subresource — tracking pixels
    // included). Raw is today's highlighted plain text.
    const toggle = document.createElement("div");
    toggle.className = "mode-toggle";
    toggle.setAttribute("role", "group");
    toggle.setAttribute("aria-label", "Message view mode");
    const renderedBtn = document.createElement("button");
    renderedBtn.type = "button";
    renderedBtn.className = "mode-btn";
    renderedBtn.textContent = "Rendered";
    const rawBtn = document.createElement("button");
    rawBtn.type = "button";
    rawBtn.className = "mode-btn";
    rawBtn.textContent = "Raw";
    const show = (asRendered: boolean) => {
      bodyText.replaceChildren();
      if (asRendered) {
        const frame = document.createElement("iframe");
        frame.className = "rendered-frame";
        frame.title = "Rendered message body";
        frame.setAttribute("sandbox", "");
        frame.setAttribute(
          "srcdoc",
          '<!doctype html><meta http-equiv="Content-Security-Policy" ' +
            'content="default-src \'none\'; style-src \'unsafe-inline\'; ' +
            'img-src data:">' +
            html,
        );
        bodyText.append(frame);
      } else {
        appendSegments(bodyText, highlightSegments(detail.body, terms));
      }
      renderedBtn.setAttribute("aria-pressed", String(asRendered));
      rawBtn.setAttribute("aria-pressed", String(!asRendered));
    };
    renderedBtn.addEventListener("click", () => show(true));
    rawBtn.addEventListener("click", () => show(false));
    toggle.append(renderedBtn, rawBtn);
    show(true);
    body.append(bodyHeading, toggle, bodyText);
  }
```

- [ ] **Step 4: Add the styles**

In `src/styles/app.css`, right after the `.detail-card .body { max-width: 78ch; }` rule (app.css:707):

```css
/* Rendered/Raw toggle for email bodies: quiet segmented control over
 * the plain-text body. */
.detail-card .mode-toggle {
  display: flex;
  gap: var(--sp-1);
  margin: 0 0 var(--sp-2);
}
.detail-card .mode-btn { font-size: 12px; padding: 3px 10px; }
.detail-card .mode-btn[aria-pressed="true"] {
  background: var(--panel-2);
  border-color: var(--text);
}
/* Emails are authored against a white page; the frame renders one. */
.detail-card iframe.rendered-frame {
  width: 100%;
  height: min(60vh, 480px);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-s);
  background: #fff;
}
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
npx vitest run src/render2.test.ts
npm run check
```

Expected: all render2 tests PASS (old ones unchanged); type-check clean.

- [ ] **Step 6: Commit**

```bash
git add src/render2.ts src/render2.test.ts src/styles/app.css
git commit -m "feat(ui): Rendered/Raw toggle renders stored email HTML in a sandboxed iframe"
```

---

### Task 4: Full-gate verification

**Files:**
- None created; this task only runs the complete CI gate set and fixes any fallout in files from Tasks 1-3 (small fixes only — anything structural goes back to the user).

**Interfaces:**
- Consumes: everything from Tasks 1-3.
- Produces: a verified branch ready for a squash-merge PR.

- [ ] **Step 1: Run the full gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm run check
npm test
```

Expected: all PASS. If anything fails, fix within the files this feature touched and amend/commit with `fix: …`.

- [ ] **Step 2: Confirm no stray changes**

```bash
git status --short
```

Expected: only `.codex/` untracked (pre-existing, ignored); nothing modified.

- [ ] **Step 3: Report**

Report: branch name, commit list (`git log --oneline main..feat/email-rendered-preview`), gate results. Do NOT open a PR or push unless asked.

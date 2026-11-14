# `.eml` + Messenger Import Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make "Drop in exports from Gmail, Outlook, WhatsApp, Telegram or Messenger… `.mbox · .eml · .json · .txt`" true at the backend and existing import UI entry points: a single `.eml` (or a folder/zip of them) imports as mail; a Facebook Messenger DYI export (single `message_1.json`, a thread folder, or the whole `messages/` tree — plain or zipped) imports as chat with media.

**Architecture:** Two new sources in `vault-core` following the per-format pattern (parser + integration test + fuzz generator + e2e): `.eml` rides the existing mbox mail path (`parse_raw_mail`, `MediaPass::mail`); Messenger is a new `messenger.rs` `StreamSource` modeled on `telegram.rs`, detected by JSON structure and resolved like the Discord package for tree picks. Spec: `docs/superpowers/specs/2026-11-13-eml-messenger-import-design.md`.

**Tech Stack:** Rust (`vault-core`, Tauri shell), vanilla TypeScript frontend, vitest, tauri-driver e2e.

## Global Constraints

- Detection order in `chat_export`: **Discord → Messenger → Telegram → Signal → WhatsApp** (Messenger must win over the Telegram "has a `messages` array" probe) — pinned by tests.
- Dedup namespaces: `.eml` uses the existing `"mbox"` source; Messenger uses `MESSENGER_SOURCE = "messenger"`.
- No new UI surfaces: only label/hint/filter/routing strings in the existing import flow.
- Working tree has another session's uncommitted files (`docs/README.md`, `docs/evidence/*`, `docs/go-public-plan.md`). **Stage only files you touched** — never `git add -A`/`git add .`.
- Gates before every commit of a Rust task: `cargo test -p vault-core && cargo clippy -p vault-core --all-targets -- -D warnings && cargo fmt --check` (run `cargo fmt` if it complains). TS tasks: `npm test && npm run check`.
- Repo docs are English; comments follow the dense doc-comment style of neighboring code.

---

### Task 1: Bare `.eml` parses through the mbox stream

**Files:**
- Modify: `crates/vault-core/src/mbox.rs`
- Test: `crates/vault-core/src/mbox.rs` (inline `mod tests`)

**Interfaces:**
- Produces: `stream_mbox` gains `.eml` semantics (no behavior change for real mbox files: any file whose first line starts with `From ` behaves exactly as today).

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `mbox.rs`:

```rust
    const EML: &[u8] = b"From: ana@example.com\nTo: bane@example.com\nSubject: Zdravo\nMessage-ID: <eml-1@example.com>\nDate: Fri, 13 Nov 2026 10:00:00 +0100\n\nPozdrav iz .eml datoteke.\n";

    #[test]
    fn bare_rfc822_message_parses_as_one_email() {
        // No leading `From ` separator: the file is a single .eml.
        let collected = parse_mbox(EML, "outlook").unwrap();
        assert_eq!(collected.failed, 0);
        assert_eq!(collected.messages.len(), 1);
        assert!(collected.messages[0].body.contains("Pozdrav iz"));
        assert!(collected.messages[0].author.contains("ana@example.com"));
    }

    #[test]
    fn unescaped_from_line_in_bare_message_is_body_text() {
        // In .eml a body line may start with `From ` — it must not split
        // the message.
        let mail = b"Subject: eml\nMessage-ID: <e2@example.com>\n\nline one\nFrom the desk of Ana\nline two\n";
        let collected = parse_mbox(mail, "f").unwrap();
        assert_eq!(collected.messages.len(), 1);
        assert!(collected.messages[0].body.contains("From the desk of Ana"));
        assert!(collected.messages[0].body.contains("line two"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vault-core bare_rfc822`
Expected: FAIL — `messages.len()` is 0 (bytes before the first separator are dropped today).

- [ ] **Step 3: Implement**

Restructure `stream_mbox` (keep `offer_part` and everything else untouched, add one parameter):

```rust
pub fn stream_mbox<R: BufRead>(
    reader: R,
    chat_id: &str,
    skip: u64,
    emit: &mut dyn FnMut(StreamItem) -> Result<Flow>,
) -> Result<()> {
    let mut skip = skip;
    let mut reader = BufReader::new(reader);
    let mut part: Vec<u8> = Vec::new();
    let mut started = false;
    let mut failure: Option<crate::Error> = None;
    let mut stopped = false;
    let mut line = Vec::new();
    // Pre-read the first line: a file that does not lead with a `From `
    // separator line is a bare RFC 5322 message (.eml) — the whole file
    // is one message, and `From ` lines inside its body are body text,
    // not separators (legal unescaped in .eml; an .mbox always leads
    // with `From `).
    let mut first: Vec<u8> = Vec::new();
    if reader.read_until(b'\n', &mut first)? == 0 {
        return Ok(()); // empty file: nothing to import
    }
    let mut pending = Some(first);
    if !pending.as_ref().unwrap().starts_with(b"From ") {
        drop(pending);
        let mut whole = Vec::new();
        std::mem::swap(&mut whole, &mut { pending.take().unwrap_or_default() });
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            whole.extend_from_slice(&line);
        }
        offer_part(&whole, chat_id, &mut skip, emit, &mut failure, &mut stopped, false);
        if let Some(e) = failure {
            return Err(e);
        }
        return Ok(());
    }
    loop {
        line.clear();
        if let Some(first) = pending.take() {
            line = first;
        } else if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        let at_line_start = part.is_empty() || *part.last().unwrap() == b'\n';
        if at_line_start && line.starts_with(b"From ") {
            if started && !offer_part(&part, chat_id, &mut skip, emit, &mut failure, &mut stopped, true) {
                break;
            }
            started = true;
            part = std::mem::take(&mut line);
            continue;
        }
        if started {
            part.extend_from_slice(&line);
        }
        // Bytes before the first separator line are dropped (the
        // whole-file splitter never emitted them either).
    }
    if started && failure.is_none() && !stopped {
        offer_part(&part, chat_id, &mut skip, emit, &mut failure, &mut stopped, true);
    }
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(())
}
```

Simplify the awkward `whole` construction to a plain `if let Some(f) = pending.take() { let mut whole = f; … }` — the above is the required behavior, not the required spelling. Change `offer_part` to take `separator: bool`:

```rust
fn offer_part(
    part: &[u8],
    chat_id: &str,
    skip: &mut u64,
    emit: &mut dyn FnMut(StreamItem) -> Result<Flow>,
    failure: &mut Option<crate::Error>,
    stopped: &mut bool,
    separator: bool,
) -> bool {
    // A `From sender date` separator line heads the part; drop it. A
    // bare .eml part (no separator) parses whole.
    let raw = if separator {
        match part.iter().position(|&b| b == b'\n') {
            Some(nl) => &part[nl + 1..],
            None => return true, // separator line with no content: skipped
        }
    } else {
        part
    };
    if raw.is_empty() {
        return true;
    }
    // … rest unchanged
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p vault-core mbox::`
Expected: PASS (new tests + the two existing nesting tests).

- [ ] **Step 5: Commit**

```bash
git add crates/vault-core/src/mbox.rs
git commit -m "feat(core): import bare .eml messages through the mbox stream"
```

---

### Task 2: `EmlDirSource` — a folder of `.eml` files

**Files:**
- Modify: `crates/vault-core/src/mbox.rs`
- Modify: `crates/vault-core/src/lib.rs` (only if `mbox` is not already re-exported broadly — check what `lib.rs` exports from `mbox` and match: export `EmlDirSource` the same way `MboxSource` is exported)

**Interfaces:**
- Produces: `pub struct EmlDirSource` with `new(path: impl Into<PathBuf>)`, implementing `Source` (name `"mbox"`) and `StreamSource` (yields each `*.eml` file of the directory, sorted by name, one message per file; unreadable/unparseable files count as `StreamItem::Failed`, the run continues).

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn eml_dir_imports_each_file_as_one_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("b.eml"),
            b"Subject: drugi\nMessage-ID: <b@x>\n\nBody B\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("a.eml"),
            b"Subject: prvi\nMessage-ID: <a@x>\n\nBody A\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"ignored").unwrap();

        let source = EmlDirSource::new(dir.path());
        let collected = source.collect().unwrap();
        assert_eq!(source.name(), MBOX_SOURCE);
        assert_eq!(collected.messages.len(), 2);
        assert_eq!(collected.failed, 0);
        // Sorted by file name: a.eml first.
        assert!(collected.messages[0].body.contains("Body A"));
        assert!(collected.messages[1].body.contains("Body B"));
    }

    #[test]
    fn eml_dir_counts_broken_files_and_continues() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("good.eml"), b"Subject: ok\nMessage-ID: <g@x>\n\nOK\n").unwrap();
        std::fs::write(dir.path().join("bad.eml"), b"\x80\x81 not a mail").unwrap();
        let collected = EmlDirSource::new(dir.path()).collect().unwrap();
        assert_eq!(collected.messages.len(), 1);
        assert_eq!(collected.failed, 1);
    }

    #[test]
    fn eml_dir_without_eml_files_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(EmlDirSource::new(dir.path()).collect().is_err());
    }
```

(`tempfile` is already a vault-core dev-dependency — verify in `crates/vault-core/Cargo.toml` before using.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vault-core eml_dir`
Expected: FAIL — `EmlDirSource` not found.

- [ ] **Step 3: Implement**

In `mbox.rs` (after `MboxSource`), mirroring the collect/stream split and the `chat_id`-from-stem convention:

```rust
/// A picked directory of `.eml` files (e.g. messages dragged out of
/// Outlook): every `*.eml` imports as one mail, sorted by file name.
/// Unreadable or unparseable files count as failed; the run continues.
#[derive(Debug, Clone)]
pub struct EmlDirSource {
    path: PathBuf,
}

impl EmlDirSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The `*.eml` files of the directory, sorted by name (single level —
    /// the same flat-listing contract as the Discord channel files).
    fn files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            let path = entry?.path();
            let is_eml = path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("eml"));
            if is_eml {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }
}

impl Source for EmlDirSource {
    fn name(&self) -> &str {
        MBOX_SOURCE
    }

    fn collect(&self) -> Result<Collected> {
        let mut messages = Vec::new();
        let mut failed: u64 = 0;
        for file in self.files()? {
            let bytes = fs::read(&file)?;
            match parse_raw_mail(&bytes, MBOX_SOURCE, "") {
                Some(msg) => messages.push(msg),
                None => failed += 1,
            }
        }
        if messages.is_empty() && failed == 0 {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "no .eml files found under {} — pick the folder that holds them",
                    self.path.display()
                ),
            )));
        }
        Ok(Collected { messages, failed })
    }
}

impl StreamSource for EmlDirSource {
    fn name(&self) -> &str {
        MBOX_SOURCE
    }

    fn stream(&self, skip: u64, emit: &mut dyn FnMut(StreamItem) -> Result<Flow>) -> Result<()> {
        let files = self.files()?;
        let mut failure: Option<crate::Error> = None;
        let mut stopped = false;
        {
            let mut state = crate::bounded::ConsumerState {
                skip,
                emit,
                stopped: false,
                failure: &mut failure,
            };
            for file in &files {
                let outcome = match fs::read(file) {
                    Ok(bytes) => match parse_raw_mail(&bytes, MBOX_SOURCE, "") {
                        Some(msg) => EntryOutcome::Message(msg),
                        None => EntryOutcome::Failed {
                            external_ref: String::new(),
                            reason: "mail part failed to parse".to_string(),
                        },
                    },
                    Err(e) => EntryOutcome::Failed {
                        external_ref: file.display().to_string(),
                        reason: format!("unreadable file: {e}"),
                    },
                };
                if !state.offer(outcome) {
                    stopped = state.stopped;
                    break;
                }
            }
            stopped = state.stopped;
        }
        if let Some(e) = failure {
            return Err(e);
        }
        let _ = stopped;
        Ok(())
    }
}
```

Check `Error::Io` construction against `crates/vault-core/src/error.rs` (adapt to the actual variant/From-impl — `io::Error` conversions already appear in `discord.rs`; reuse the same spelling).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p vault-core mbox::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/vault-core/src/mbox.rs crates/vault-core/src/lib.rs
git commit -m "feat(core): EmlDirSource imports a folder of .eml files"
```

---

### Task 3: Route `.eml` through preview, the Tauri command, and the frontend

**Files:**
- Modify: `crates/vault-core/src/preview.rs` (file branch of `resolve_preview_source`, ~line 188; dir/zip fallback ~line 141)
- Modify: `src-tauri/src/lib.rs` (`import_mbox`, line ~1337)
- Modify: `src/main.ts` (routing ~1421, filter ~1354, error text ~1332)
- Modify: `src/importview.ts` (`formatLabel`, `formatHint`)
- Modify: `index.html` (`#import-help` static list — grep it)
- Test: `crates/vault-core/src/preview.rs` (inline tests), `src/importview.test.ts`, `src/main.test.ts`

**Interfaces:**
- Consumes: `EmlDirSource` (Task 2), `.eml`-lenient `MboxSource` (Task 1).
- Produces: `ImportPreview.kind === "eml"` for a single `.eml`, a folder of them, or a zip of them; `importMbox` accepts all three path shapes.

- [ ] **Step 1: Write the failing preview tests** (inline `mod tests` in `preview.rs`, following the existing `previews_mbox` shape)

```rust
    #[test]
    fn previews_eml_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mail.eml");
        std::fs::write(&path, b"From: a@b.c\nSubject: eml prev\nMessage-ID: <p@x>\n\nPozdrav\n").unwrap();
        let preview = preview_import(&path).expect("eml preview");
        assert_eq!(preview.kind, "eml");
        assert_eq!(preview.source, "mbox");
        assert_eq!(preview.messages_seen, 1);
    }

    #[test]
    fn previews_eml_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("1.eml"), b"Subject: a\nMessage-ID: <1@x>\n\nA\n").unwrap();
        std::fs::write(dir.path().join("2.eml"), b"Subject: b\nMessage-ID: <2@x>\n\nB\n").unwrap();
        let preview = preview_import(dir.path()).expect("eml dir preview");
        assert_eq!(preview.kind, "eml");
        assert_eq!(preview.messages_seen, 2);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vault-core previews_eml`
Expected: FAIL — kind is an error/"unrecognized" today.

- [ ] **Step 3: Implement preview detection**

In `preview.rs`:

```rust
/// Does the file start like a bare mail (an RFC 5322 header line —
/// `Name: value`)? Only consulted when the mbox `From ` sniff failed.
/// (Bounded: one line.)
fn looks_like_eml(path: &Path) -> Result<bool> {
    let file = fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let Some((name, _rest)) = first.split_once(':') else {
        return Ok(false);
    };
    Ok(!name.is_empty()
        && name
            .bytes()
            .all(|b| (33..=126).contains(&b) && b != b':'))
}
```

In `resolve_preview_source`, the file branch: after the `is_mbox` check add:

```rust
        let is_eml = picked
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("eml"))
            || looks_like_eml(picked)?;
        if is_eml {
            return Ok((
                PreviewSource {
                    kind: "eml".into(),
                    source: MBOX_SOURCE.to_string(),
                    stream: Box::new(MboxSource::new(picked)),
                    export_root: None,
                    tree_input: false,
                    size_root: picked.to_path_buf(),
                },
                None,
            ));
        }
```

In `resolve_dir_source`, before mapping the chat-export error, add an `.eml` fallback (a folder the chat side does not recognize, holding `.eml` files, is an Outlook drag-out):

```rust
fn resolve_dir_source(root: &Path, display: &Path) -> Result<PreviewSource> {
    match chat_export::resolve_stream_source(root) {
        Ok((format, file, stream)) => { /* existing mapping body unchanged */ }
        Err(e) => {
            let has_eml = walk_eml_count(root); // bounded single level, see below
            if has_eml > 0 {
                return Ok(PreviewSource {
                    kind: "eml".into(),
                    source: MBOX_SOURCE.to_string(),
                    stream: Box::new(mbox::EmlDirSource::new(root)),
                    export_root: None,
                    tree_input: true,
                    size_root: root.to_path_buf(),
                });
            }
            Err(Error::ChatImport(format!( /* existing error text */ )))
        }
    }
}

/// `.eml` files directly inside the directory (bounded: one read_dir).
fn walk_eml_count(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| {
                    e.path().is_file()
                        && e.path()
                            .extension()
                            .and_then(|x| x.to_str())
                            .is_some_and(|x| x.eq_ignore_ascii_case("eml"))
                })
                .count()
        })
        .unwrap_or(0)
}
```

Keep the existing Ok-arm body byte-identical (restructure only around the match). The zip branch already funnels into `resolve_dir_source`, so a zip of `.eml`s gets the fallback for free.

- [ ] **Step 4: Tauri command accepts the new shapes**

In `src-tauri/src/lib.rs` `import_mbox`, replace `let source = MboxSource::new(&path);` with a dir-aware choice (a bare `.eml` file needs no change — `MboxSource` parses it since Task 1):

```rust
        let path = Path::new(&path);
        let source: Box<dyn vault_core::bounded::StreamSource + Send> = if path.is_dir() {
            Box::new(vault_core::mbox::EmlDirSource::new(path))
        } else {
            Box::new(vault_core::mbox::MboxSource::new(path))
        };
```

Verify the `vault_core::mbox` / `bounded::StreamSource` re-export paths against `crates/vault-core/src/lib.rs` (export `EmlDirSource` wherever `MboxSource` is exported) and that `run.run(source.as_ref(), …)` compiles with `&dyn StreamSource`. Keep `import_id_for("mbox-import", …)` and `MediaPass::mail` unchanged.

- [ ] **Step 5: Frontend routing + labels**

`src/main.ts`:
- routing: `if (target.kind === "mbox")` → `if (target.kind === "mbox" || target.kind === "eml")`
- media-status line: `target.kind !== "mbox" && extractMedia` → `target.kind !== "mbox" && target.kind !== "eml" && extractMedia`
- picker filter (file pick): name → `"Email or chat export (.mbox, .eml, WhatsApp .txt, Telegram/Signal/Discord/Messenger .json, or a .zip)"`, extensions → `["mbox", "eml", "txt", "json", "zip"]`
- `friendlyImportError` supported-inputs sentence → `Supported inputs: an .mbox email archive, a bare .eml (or a folder/zip of them), a WhatsApp .txt, a Telegram result.json, a Signal .json, a Discord messages.json, a Messenger message_*.json (or its export folder/.zip), or a folder/.zip containing these.`

`src/importview.ts`:
- `formatLabel`: `case "eml": return "Email messages (.eml)";`
- `formatHint`: `case "eml": return "Drag messages out of Outlook (or any mail app) as .eml files; a folder of them imports together.";`

`index.html`: update the `#import-help` static format list with `.eml` (and, in Task 8, Messenger).

- [ ] **Step 6: Tests**

Add `formatLabel("eml")` / `formatHint("eml")` cases to `src/importview.test.ts` mirroring the existing entries. If `src/main.test.ts` has an import-routing table, add the `eml → importMbox` row.

Run: `cargo test -p vault-core preview && cargo test --workspace && npm test && npm run check`
Expected: PASS everywhere.

- [ ] **Step 7: Commit**

```bash
git add crates/vault-core/src/preview.rs src-tauri/src/lib.rs src/main.ts src/importview.ts src/importview.test.ts src/main.test.ts index.html
git commit -m "feat: route .eml files, folders, and zips through the mail import"
```

---

### Task 4: Messenger detection and `ChatFormat` wiring

**Files:**
- Create: `crates/vault-core/src/messenger.rs`
- Modify: `crates/vault-core/src/lib.rs` (`pub mod messenger;` beside `pub mod telegram;`)
- Modify: `crates/vault-core/src/chat_export.rs` (`ChatFormat`, both detectors, `resolve_stream_source` error text)
- Test: `crates/vault-core/src/chat_export.rs` (inline tests), `crates/vault-core/src/messenger.rs` (inline tests)

**Interfaces:**
- Produces: `messenger::MESSENGER_SOURCE: &str = "messenger"`, `messenger::is_messenger_export(&[u8]) -> bool`, `messenger::is_messenger_export_reader(impl Read) -> bool`, `chat_export::ChatFormat::Messenger`.
- Detection contract: true only for a top-level JSON **object** carrying a `participants` **array** and a `messages` **array** whose first element is an object with `sender_name` or `timestamp_ms`.

- [ ] **Step 1: Write the failing detection tests** (in `messenger.rs`, `mod tests`)

```rust
    fn thread_bytes() -> Vec<u8> {
        br#"{"participants": [{"name": "Ana Peric"}], "messages": [{"sender_name": "Ana Peric", "timestamp_ms": 1762981200000, "content": "Zdravo!"}], "title": "Ana Peric", "thread_path": "inbox/anaperic_1234567890"}"#.to_vec()
    }

    #[test]
    fn detects_messenger_thread_json() {
        assert!(is_messenger_export(&thread_bytes()));
    }

    #[test]
    fn telegram_shape_is_not_messenger() {
        // Telegram: messages array but no participants array / sender_name.
        assert!(!is_messenger_export(
            br#"{"name": "x", "id": 42, "messages": [{"id": 1, "type": "message", "date": "2026-11-01T10:00:00", "text": "hi"}]}"#
        ));
        assert!(!is_messenger_export(br#"{"messages": []}"#));
    }
```

And in `chat_export.rs` `mod tests` (the ordering pin — this is the critical one):

```rust
    #[test]
    fn messenger_thread_json_is_not_telegram() {
        // The Telegram probe is "has a messages array"; Messenger must be
        // checked first or a DYI file would be imported as Telegram.
        let messenger = br#"{"participants": [{"name": "Ana"}], "messages": [{"sender_name": "Ana", "timestamp_ms": 1, "content": "hi"}]}"#;
        assert_eq!(detect_chat_format(messenger), Some(ChatFormat::Messenger));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vault-core messenger`
Expected: FAIL — module/functions don't exist.

- [ ] **Step 3: Implement the probes**

`crates/vault-core/src/messenger.rs` (module doc-comment first, in the house style — see `telegram.rs:1-20`):

```rust
//! Facebook Messenger (DYI JSON) export source: parses the
//! `messages/inbox/<thread>/message_*.json` files written by
//! "Download Your Information" with format set to JSON.
//!
//! [full doc-comment: text + media in raw_json; conversation key from
//! the thread folder's numeric suffix; synthetic external ids (FB
//! carries none); merged message_N parts in numeric order]

use std::io::BufReader;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use serde_json::Value;

use crate::bounded::{EntryOutcome, Flow, StreamItem, StreamSource};
use crate::ingest::{Collected, Source};
use crate::message::NewMessage;
use crate::Result;

/// Source name and dedup namespace for Messenger imports.
pub const MESSENGER_SOURCE: &str = "messenger";

/// Detection-only walk of a DYI message file (streamed: only the first
/// `messages` element is materialized, `participants` elements are
/// zero-sized `IgnoredAny`s), so detecting an arbitrarily large export
/// stays bounded. True only for an object with a `participants` array
/// plus a `messages` array whose first element carries `sender_name` or
/// `timestamp_ms` — the combination no other supported format has.
pub fn is_messenger_export(bytes: &[u8]) -> bool {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    de.deserialize_map(DetectVisitor).unwrap_or(false)
}

/// [`is_messenger_export`] streamed from a reader (the caller rewinds
/// between probes, as `chat_export::detect_chat_format_reader` does).
pub fn is_messenger_export_reader<R: std::io::Read>(reader: R) -> bool {
    let mut de = serde_json::Deserializer::from_reader(BufReader::new(reader));
    de.deserialize_map(DetectVisitor).unwrap_or(false)
}

struct DetectVisitor;

impl<'de> serde::de::Visitor<'de> for DetectVisitor {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a Messenger export object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<bool, A::Error> {
        let mut participants = false;
        let mut sender_shaped = false;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "participants" => {
                    let _ = map.next_value::<Vec<serde::de::IgnoredAny>>()?; // array-ness
                    participants = true;
                    if sender_shaped {
                        return Ok(true);
                    }
                }
                "messages" => {
                    sender_shaped = map.next_value_seed(FirstElementSender)?;
                    if participants {
                        return Ok(true);
                    }
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(participants && sender_shaped)
    }
}

/// Reads only the first element of the `messages` array and reports
/// whether it is a Messenger message object.
struct FirstElementSender;

impl<'de> serde::de::DeserializeSeed<'de> for FirstElementSender {
    type Value = bool;

    fn deserialize<D: serde::de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<bool, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for FirstElementSender {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a messages array")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<bool, A::Error> {
        let Some(first) = seq.next_element::<Value>()? else {
            return Ok(false);
        };
        let Some(obj) = first.as_object() else {
            return Ok(false);
        };
        Ok(obj.contains_key("sender_name") || obj.contains_key("timestamp_ms"))
    }
}
```

(The early `return Ok(true)` leaves the reader mid-stream — fine: detection is a bool, and `deserialize_map` here is driven directly, without the `end()` check `from_slice` would apply.)

- [ ] **Step 4: Wire into `chat_export.rs`**

```rust
use crate::messenger::{is_messenger_export, is_messenger_export_reader, MessengerSource};

pub enum ChatFormat {
    WhatsApp,
    Telegram,
    Signal,
    Discord,
    Messenger,
}
```

In `detect_chat_format`: insert between the Discord and Telegram checks —
`if is_messenger_export(bytes) { return Some(ChatFormat::Messenger); }`. Same in `detect_chat_format_reader` (with `file.rewind()?;` before it, matching the neighbors). Update the two "expected a …" error strings in `resolve_stream_source`/`resolve_export_file` to name Messenger. `resolve_stream_source`'s match gets `ChatFormat::Messenger => Box::new(MessengerSource::new(&file))` — add a minimal placeholder-free stub now? No: this task only compiles the **detection**; `MessengerSource` arrives in Task 5. To keep every commit green, this task adds the match arm together with a minimal `MessengerSource` (path field, `Source::name`, `collect`/`stream` returning `Err`), clearly marked, replaced wholesale in Task 5. Add to `messenger.rs`:

```rust
/// Reads Messenger DYI export files and yields their messages.
/// (Streaming implementation lands with the parser task; detection
/// ships first so the format never misroutes to Telegram.)
#[derive(Debug, Clone)]
pub struct MessengerSource {
    path: PathBuf,
}

impl MessengerSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}
```

plus `impl Source`/`impl StreamSource` stubs whose `collect`/`stream` return `Err(Error::ChatImport("messenger import not wired yet".into()))` (imports: `std::fs` not needed yet).

- [ ] **Step 5: Run the tests**

Run: `cargo test -p vault-core chat_export:: messenger::`
Expected: PASS (detection tests + ordering pin + all existing detection tests unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/vault-core/src/messenger.rs crates/vault-core/src/lib.rs crates/vault-core/src/chat_export.rs
git commit -m "feat(core): detect Messenger DYI exports ahead of the Telegram probe"
```

---

### Task 5: Messenger single-file source

**Files:**
- Modify: `crates/vault-core/src/messenger.rs` (replace the stub bodies)
- Modify: `crates/vault-core/src/whatsapp.rs` (`fnv1a_fields` → `pub(crate)`, one-word visibility change; find it near line 409's caller)
- Test: `crates/vault-core/tests/messenger_ingest.rs` (new), inline `mod tests`

**Interfaces:**
- Consumes: `whatsapp::fnv1a_fields(&[&str]) -> u64` (made `pub(crate)` — the frozen WhatsApp external-id formula stays byte-identical; only visibility changes).
- Produces: `MessengerSource` fully implementing `Source` + `StreamSource` for a **single `message_*.json` file** (folder/tree mode is Task 6); message shape per the mapping table below.

**Mapping (from the spec, frozen):**

| DYI field | `NewMessage` |
| --- | --- |
| `sender_name` | `author` |
| `timestamp_ms` | `sent_at` RFC 3339 UTC (ms dropped), `timezone "UTC"`, `time_confidence "explicit_offset"`; absent → `sent_at ""`, confidence `"missing"` |
| `content` | `body` |
| message object | `raw_json` verbatim |
| — | `external_id = format!("generated-{:016x}-{}", fnv1a_fields(&[thread_key, sender, ts_text, body]), seen)` (WhatsApp occurrence-counter pattern) |
| `title` / participants | `chat_id` |
| `thread_path` / parent folder suffix `…_<digits>` | `conversation_key = "messenger:thread:<digits>"`; absent → `""` (legacy chat_id grouping) |

Skip rule: entries that are not objects, or that have neither `content` nor any of `photos`, `videos`, `audio_files`, `files`, `gifs`, `sticker`, are `EntryOutcome::Skip`; entries without `sender_name` AND without `timestamp_ms` are `Failed` (not Messenger-shaped).

- [ ] **Step 1: Write the failing tests**

`crates/vault-core/tests/messenger_ingest.rs` (harness mirrors `telegram_ingest.rs`: `Storage::open_in_memory`, `vault_core::ingest::ingest`, `storage.search_filtered(&build_match_query(term), None, None, 10)`):

```rust
use vault_core::ingest::{ingest, IngestSummary, Source};
use vault_core::search::build_match_query;
use vault_core::storage::Storage;
use vault_core::messenger::{MessengerSource, MESSENGER_SOURCE};

const THREAD_JSON: &str = r#"{
  "participants": [{"name": "Ana Peric"}, {"name": "Marko Horvat"}],
  "title": "Ana Peric",
  "thread_path": "inbox/anaperic_1234567890",
  "is_still_participant": true,
  "messages": [
    {"sender_name": "Ana Peric", "timestamp_ms": 1762981200000, "content": "Zdravo! Kako si?"},
    {"sender_name": "Marko Horvat", "timestamp_ms": 1762981260000, "content": "Dobro, hvala."},
    {"sender_name": "Ana Peric", "timestamp_ms": 1762981320000,
     "photos": [{"uri": "messages/inbox/anaperic_1234567890/photos/ana.jpg", "creation_timestamp": 1762981320}]},
    {"sender_name": "Ana Peric", "timestamp_ms": 1762981380000, "reactions": [{"reaction": "❤"}]}
  ]
}"#;

#[test]
fn parses_single_thread_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("message_1.json");
    std::fs::write(&path, THREAD_JSON).unwrap();

    let source = MessengerSource::new(&path);
    assert_eq!(source.name(), MESSENGER_SOURCE);
    let summary = ingest(&Storage::open_in_memory().unwrap(), &source).unwrap();
    // 3 messages: the reactions-only entry has no body and no media → skipped.
    assert_eq!(summary, IngestSummary { source: MESSENGER_SOURCE.into(), seen: 3, inserted: 3, skipped: 0, failed: 0 });
}

#[test]
fn single_file_maps_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("message_1.json");
    std::fs::write(&path, THREAD_JSON).unwrap();

    let collected = MessengerSource::new(&path).collect().unwrap();
    let first = &collected.messages[0];
    assert_eq!(first.source, MESSENGER_SOURCE);
    assert_eq!(first.channel, "chat");
    assert_eq!(first.chat_id, "Ana Peric");
    assert_eq!(first.author, "Ana Peric");
    assert_eq!(first.sent_at, "2025-11-13T10:20:00Z");
    assert_eq!(first.timezone, "UTC");
    assert_eq!(first.time_confidence, "explicit_offset");
    assert_eq!(first.original_timestamp, "1762981200000");
    assert_eq!(first.conversation_key, "messenger:thread:1234567890");
    assert!(first.body.contains("Zdravo"));
    // The photo message keeps its whole object in raw_json.
    let photo = &collected.messages[2];
    let raw: serde_json::Value = serde_json::from_str(&photo.raw_json).unwrap();
    assert_eq!(raw["photos"][0]["uri"], "messages/inbox/anaperic_1234567890/photos/ana.jpg");
}

#[test]
fn reimport_is_idempotent_and_rename_stable() {
    let storage = Storage::open_in_memory().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("message_1.json");
    std::fs::write(&path, THREAD_JSON).unwrap();
    assert_eq!(ingest(&storage, &MessengerSource::new(&path)).unwrap().inserted, 3);
    let again = ingest(&storage, &MessengerSource::new(&path)).unwrap();
    assert_eq!(again.inserted, 0);
    assert_eq!(again.skipped, 3);
    assert_eq!(storage.stats().unwrap().total, 3);
}

#[test]
fn imported_thread_is_full_text_searchable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("message_1.json");
    std::fs::write(&path, THREAD_JSON).unwrap();
    let storage = Storage::open_in_memory().unwrap();
    ingest(&storage, &MessengerSource::new(&path)).unwrap();
    assert!(!storage.search_filtered(&build_match_query("zdravo"), None, None, 10).unwrap().is_empty());
}
```

Verify `1762981200000` ↔ `2025-11-13T10:20:00Z` with `date -u -d @1762981200` before relying on it in the assertion (compute, don't trust this plan's arithmetic); if it differs, fix the test constant, not the code.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vault-core --test messenger_ingest`
Expected: FAIL — stub returns `Err`.

- [ ] **Step 3: Implement**

Replace the stub `Source`/`StreamSource` impls in `messenger.rs` (add these items; two-pass streaming mirrors `telegram.rs:72-118` exactly — header pass via `serde_json::from_reader` into a header struct with `messages: Vec<IgnoredAny>`, then `Deserializer::from_reader(...).deserialize_map(MessagesVisitor { ctx, occ, state })`):

```rust
/// The header view of one thread file: `title`/`thread_path`/`participants`
/// are read small; `messages` is enforced to be an array and skipped
/// element by element (zero-sized `IgnoredAny`s).
#[derive(Debug, serde::Deserialize)]
struct MessengerHeader {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    thread_path: Option<String>,
    #[serde(default)]
    participants: Vec<ParticipantName>,
    #[serde(default)]
    messages: Vec<serde::de::IgnoredAny>,
}

#[derive(Debug, serde::Deserialize)]
struct ParticipantName {
    #[serde(default)]
    name: Option<String>,
}

/// Conversation context shared by every entry of one thread: display
/// grouping, stable dedup thread key, and the conversation key.
struct ThreadContext {
    chat_id: String,
    thread_key: String,
    conversation_key: String,
}

impl ThreadContext {
    fn from_header(header: &MessengerHeader, folder_stem: &str) -> Self {
        let participants = header
            .participants
            .iter()
            .filter_map(|p| p.name.as_deref().filter(|n| !n.is_empty()))
            .collect::<Vec<_>>()
            .join(", ");
        let chat_id = header
            .title
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .filter(|c| !c.is_empty())
            .or_else(|| (!participants.is_empty()).then(|| participants.clone()))
            .unwrap_or_else(|| folder_stem.to_string());
        let thread_key = thread_id(header.thread_path.as_deref(), folder_stem);
        let conversation_key = if thread_key.is_empty() {
            String::new()
        } else {
            format!("messenger:thread:{thread_key}")
        };
        Self { chat_id, thread_key, conversation_key }
    }
}

/// The stable thread id: the trailing `_<digits>` group of the thread
/// folder name, from `thread_path` (e.g. `inbox/ana_1234567890`) or the
/// folder stem — whichever carries digits. Empty when neither does
/// (a hand-trimmed export): legacy chat_id grouping applies.
fn thread_id(thread_path: Option<&str>, folder_stem: &str) -> String {
    for candidate in [thread_path.unwrap_or("").rsplit('/').next().unwrap_or(""), folder_stem] {
        if let Some(pos) = candidate.rfind('_') {
            let digits = &candidate[pos + 1..];
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                return digits.to_string();
            }
        }
    }
    String::new()
}

/// Fields whose presence makes an entry a media message even without
/// `content` (Telegram's service-entry skip, scoped to chat metadata).
const MEDIA_FIELDS: [&str; 6] = ["photos", "videos", "audio_files", "files", "gifs", "sticker"];

fn messenger_entry(entry: &Value, ctx: &ThreadContext, occurrence: &mut HashMap<u64, u32>) -> EntryOutcome {
    let Some(obj) = entry.as_object() else {
        return EntryOutcome::Failed { external_ref: String::new(), reason: "entry is not a JSON object".to_string() };
    };
    let body = obj.get("content").and_then(Value::as_str).unwrap_or_default().to_string();
    let has_media = MEDIA_FIELDS.iter().any(|f| obj.get(*f).is_some_and(|v| match v {
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
        _ => false,
    }));
    let sender = obj.get("sender_name").and_then(Value::as_str);
    let ts = obj.get("timestamp_ms").and_then(Value::as_i64);
    if sender.is_none() && ts.is_none() {
        return EntryOutcome::Failed { external_ref: String::new(), reason: "entry is not a Messenger message".to_string() };
    }
    if body.is_empty() && !has_media {
        return EntryOutcome::Skip;
    }
    let author = sender.unwrap_or_default().to_string();
    let ts_text = ts.map(|t| t.to_string()).unwrap_or_default();
    let sent_at = ts
        .and_then(|ms| Utc.timestamp_millis_opt(ms).single())
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    // FB exports carry no message ids: a content hash over the stable
    // thread key + author + timestamp + body, with an occurrence counter
    // for byte-identical repeats (the WhatsApp formula).
    let base = crate::whatsapp::fnv1a_fields(&[&ctx.thread_key, &author, &ts_text, &body]);
    let seen = occurrence.entry(base).or_insert(0);
    let external_id = format!("generated-{base:016x}-{seen}");
    *seen += 1;

    let mut msg = NewMessage::new(MESSENGER_SOURCE, external_id, author, sent_at, body);
    msg.channel = "chat".to_string();
    msg.chat_id = ctx.chat_id.clone();
    msg.conversation_key = ctx.conversation_key.clone();
    msg.raw_json = entry.to_string();
    msg.original_timestamp = ts_text;
    msg.timezone = "UTC".to_string();
    msg.time_confidence = if ts.is_some() { "explicit_offset" } else { "missing" }.to_string();
    EntryOutcome::Message(msg)
}
```

`Source::collect` (single file): read bytes, `serde_json::from_slice::<MessengerExport>` where `MessengerExport` mirrors `TelegramExport` (`title`, `thread_path`, `participants: Vec<Value>`, `messages: Vec<Value>`), build ctx (folder stem = parent dir name), loop entries. `StreamSource::stream` (single file): the two-pass form — build ctx from the header pass using `self.path.parent().file_name()` as folder stem, then the `MessagesVisitor` walk carrying `occ: &mut HashMap<u64, u32>`; error mapping identical to `telegram.rs:108-117`. In `whatsapp.rs` change `fn fnv1a_fields` → `pub(crate) fn fnv1a_fields` (add nothing else).

Also add the inline detection tests from Task 4's Step 1 if not yet present, plus `broken_json_reports_error` (`collect` on `b"{not json"` → `Err`).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p vault-core --test messenger_ingest messenger:: && cargo test -p vault-core whatsapp::`
Expected: PASS (WhatsApp id tests still green — visibility-only change).

- [ ] **Step 5: Commit**

```bash
git add crates/vault-core/src/messenger.rs crates/vault-core/src/whatsapp.rs crates/vault-core/tests/messenger_ingest.rs
git commit -m "feat(core): Messenger DYI single-file source (streaming, dedup, FTS)"
```

---

### Task 6: Messenger tree picks — thread walk and resolution

**Files:**
- Modify: `crates/vault-core/src/messenger.rs`
- Modify: `crates/vault-core/src/chat_export.rs` (`resolve_export_file`, `resolve_stream_source`, `import_bounded` media block)
- Test: both files' inline tests, `crates/vault-core/tests/messenger_ingest.rs`

**Interfaces:**
- Produces: `MessengerSource::new` accepts a **file**, a **thread folder** (`message_*.json` directly inside), or a **DYI root/messages/inbox** directory; `is_messenger_export_dir(&Path) -> bool`; media uris resolve against the DYI root (Task 7 uses this).

- [ ] **Step 1: Write the failing tests**

In `tests/messenger_ingest.rs` (fixture builder + three tests):

```rust
use std::path::PathBuf;

/// A two-thread DYI tree: root/messages/inbox/<thread>/message_N.json.
fn dyi_tree(dir: &std::path::Path) -> PathBuf {
    let ana = dir.join("messages/inbox/ana_1111");
    let grupa = dir.join("messages/inbox/grupa_2222");
    std::fs::create_dir_all(&ana).unwrap();
    std::fs::create_dir_all(&grupa).unwrap();
    std::fs::write(
        ana.join("message_1.json"),
        r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
            "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "iz prve polovine"}]}"#,
    ).unwrap();
    // message_2 sorts AFTER message_10 numerically only if sorted by number:
    std::fs::write(
        ana.join("message_10.json"),
        r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
            "messages": [{"sender_name": "Ana", "timestamp_ms": 1762984800000, "content": "deset"}]}"#,
    ).unwrap();
    std::fs::write(
        ana.join("message_2.json"),
        r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
            "messages": [{"sender_name": "Ana", "timestamp_ms": 1762982400000, "content": "dva"}]}"#,
    ).unwrap();
    std::fs::write(
        grupa.join("message_1.json"),
        r#"{"participants": [{"name": "Bane"}], "title": "Grupa", "thread_path": "inbox/grupa_2222",
            "messages": [{"sender_name": "Bane", "timestamp_ms": 1762981201000, "content": "iz grupe"}]}"#,
    ).unwrap();
    dir.to_path_buf()
}

#[test]
fn tree_import_merges_parts_in_numeric_order_per_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    let collected = MessengerSource::new(&root).collect().unwrap();
    assert_eq!(collected.messages.len(), 4);
    // Thread "ana": numeric order — 1, 2, 10 (name sort would give 1, 10, 2).
    let ana: Vec<&vault_core::message::NewMessage> = collected
        .messages
        .iter()
        .filter(|m| m.conversation_key == "messenger:thread:1111")
        .collect();
    assert_eq!(ana.len(), 3);
    assert_eq!(ana[0].body, "iz prve polovine");
    assert_eq!(ana[1].body, "dva");
    assert_eq!(ana[2].body, "deset");
    assert_eq!(ana[0].chat_id, "Ana");
}

#[test]
fn thread_folder_pick_merges_its_parts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    let thread = root.join("messages/inbox/ana_1111");
    let collected = MessengerSource::new(&thread).collect().unwrap();
    assert_eq!(collected.messages.len(), 3);
    assert!(collected.messages.iter().all(|m| m.conversation_key == "messenger:thread:1111"));
}

#[test]
fn tree_import_through_chat_export_is_dedup_idempotent() {
    use vault_core::chat_export::import_bounded;
    use vault_core::storage::Storage;
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    let storage = Storage::open_in_memory().unwrap();
    let mut sink = crate::test_sink(&storage); // see note below
    let first = import_bounded(&mut sink, &root, false, None, &|| false, &mut |_, _, _| {}).unwrap();
    assert_eq!(first.summary.inserted, 4);
    let second = import_bounded(&mut sink, &root, false, None, &|| false, &mut |_, _, _| {}).unwrap();
    assert_eq!(second.summary.inserted, 0);
    assert_eq!(second.summary.skipped, 4);
}
```

Note: `import_bounded` takes `&mut dyn IngestSink`; `Storage` implements it directly (see `bounded.rs:316-322` docs). Drop the `test_sink` helper and pass `&mut storage` (adjust to the real `Storage: IngestSink` impl — read `bounded.rs`'s `IngestSink` doc line first and mirror how `bounded_ingest.rs` tests call it).

Also inline in `chat_export.rs` tests:

```rust
    #[test]
    fn messenger_dir_resolves_over_the_export_files_error() {
        // A thread folder holds multiple message_*.json files; today that
        // errors ("pick one file") — for Messenger it is one thread pick.
        let dir = tempfile::tempdir().unwrap();
        let thread = dir.path().join("ana_1111");
        std::fs::create_dir_all(&thread).unwrap();
        std::fs::write(thread.join("message_1.json"),
            r#"{"participants": [{"name": "Ana"}], "messages": [{"sender_name": "Ana", "timestamp_ms": 1, "content": "x"}]}"#).unwrap();
        std::fs::write(thread.join("message_2.json"),
            r#"{"participants": [{"name": "Ana"}], "messages": [{"sender_name": "Ana", "timestamp_ms": 2, "content": "y"}]}"#).unwrap();
        let (format, file, _source) = resolve_stream_source(&thread).unwrap();
        assert_eq!(format, ChatFormat::Messenger);
        assert!(file.is_dir());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vault-core --test messenger_ingest`
Expected: FAIL — dir picks unsupported.

- [ ] **Step 3: Implement the thread walk**

In `messenger.rs`:

```rust
use std::fs;
use std::path::Path;

/// True when `name` is a DYI message file: `message_<number>.json`.
fn is_message_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    name.starts_with("message_")
        && path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("json"))
}

/// The numeric suffix of `message_<n>.json` (files without one sort last).
fn message_number(path: &Path) -> u64 {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.rsplit_once('_'))
        .and_then(|(_, n)| n.parse().ok())
        .unwrap_or(u64::MAX)
}

/// Thread directories under a Messenger pick: every directory that
/// directly holds `message_*.json` files, found by a depth-capped walk —
/// the pick may be the DYI root, `messages/`, `messages/inbox/`, or a
/// single thread folder. A directory with its own message files never
/// descends (thread folders hold only their own parts). Hidden and
/// macOS-junk entries never count (the shared fence rule).
fn thread_dirs(root: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut has_own = false;
    let mut subdirs = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.starts_with('.') || name == "__MACOSX" || name.starts_with("._") {
            continue;
        }
        if path.is_dir() {
            subdirs.push(path);
        } else if is_message_file(&path) {
            has_own = true;
        }
    }
    if has_own {
        out.push(root.to_path_buf());
        return Ok(());
    }
    if depth == 0 {
        return Ok(());
    }
    subdirs.sort();
    for dir in subdirs {
        thread_dirs(&dir, depth - 1, out)?;
    }
    Ok(())
}

/// The message files of one thread in numeric order (`message_2` before
/// `message_10`).
fn thread_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() && is_message_file(&path) {
            files.push(path);
        }
    }
    files.sort_by_key(|f| (message_number(f), f.to_path_buf()));
    Ok(files)
}

/// True when the tree holds at least one Messenger thread directory.
pub fn is_messenger_export_dir(root: &Path) -> bool {
    let mut dirs = Vec::new();
    thread_dirs(root, 6, &mut dirs).is_ok() && !dirs.is_empty()
}
```

Extend `MessengerSource`: `collect` — if `self.path.is_file()`, single-file path (Task 5 body); else walk `thread_dirs(path, 6)`; error when empty (`"no Messenger message_*.json files found under {path}"`, InvalidInput io error, the Discord spelling); per thread: ctx from the **numerically first** file's header with the thread folder's stem, `occurrence` map shared across that thread's files, entries in file order. `stream` — same iteration wrapped in `ConsumerState`, mirroring `discord.rs:87-127` (per-file two-pass streaming via the Task 5 visitor; stop/failure break the thread loop, error mapping identical).

- [ ] **Step 4: Wire resolution in `chat_export.rs`**

`resolve_export_file`, after the `is_discord_export_dir` check inside the walk loop:

```rust
            if is_messenger_export_dir(&dir) {
                return Ok(dir);
            }
```

Change the export-files scan to keep kinds — `let mut export_files: Vec<(PathBuf, ChatFormat)>` filled with `Ok(Some(format)) => export_files.push((entry.path(), format))` — and replace the resolution block:

```rust
            if export_files.len() == 1 {
                return Ok(export_files.remove(0).0);
            }
            // Several Messenger message files are one thread's parts —
            // the pick is the thread folder itself.
            if !export_files.is_empty()
                && export_files.iter().all(|(_, f)| *f == ChatFormat::Messenger)
            {
                return Ok(dir);
            }
            if export_files.len() > 1 {
                return Err(Error::ChatImport(format!(
                    "directory holds {} export files; pick one file explicitly",
                    export_files.len()
                )));
            }
```

`resolve_stream_source`, dir branch:

```rust
    if file.is_dir() {
        if is_discord_export_dir(&file) {
            let source = DiscordSource::new(file.clone());
            return Ok((ChatFormat::Discord, file, Box::new(source)));
        }
        let source = MessengerSource::new(file.clone());
        return Ok((ChatFormat::Messenger, file, Box::new(source)));
    }
```

`import_bounded`, media block — Messenger joins the media kinds **only for a tree pick** (single-file picks keep refs in `raw_json` only, per spec), rooted at the DYI root with no single export file, and `unlinked` only when the pick **is** the root (a thread-folder pick must not sweep the whole DYI tree):

```rust
            let kind = match format {
                ChatFormat::Telegram => Some(MediaKind::Telegram),
                ChatFormat::WhatsApp => Some(MediaKind::WhatsApp),
                ChatFormat::Messenger if file.is_dir() => Some(MediaKind::Messenger),
                ChatFormat::Signal | ChatFormat::Discord | ChatFormat::Messenger => None,
            };
```

and replace the `MediaPass` construction so Messenger gets its own root handling (keep Telegram/WhatsApp exactly as today):

```rust
            if let Some(kind) = kind {
                let pass = if kind == MediaKind::Messenger {
                    let root = messenger_export_root(&file);
                    MediaPass {
                        kind,
                        export_root: root.as_path(),
                        export_file: None,
                        data_dir,
                        // Only a whole-tree pick sweeps unreferenced files.
                        unlinked: tree_pick && root == effective,
                    }
                } else {
                    MediaPass {
                        kind,
                        export_root: export_root.as_path(),
                        export_file: Some(export_file.as_path()),
                        data_dir,
                        unlinked: tree_pick,
                    }
                };
                run.media = Some(pass);
            }
```

with the helper in `messenger.rs`:

```rust
/// The DYI root a Messenger tree pick resolves media against: the pick
/// itself when it holds `messages/`, else the nearest ancestor that
/// does (a `messages/`, `inbox/`, or thread-folder pick climbs back
/// up), capped at three levels. Falls back to the pick itself.
pub fn messenger_export_root(pick: &Path) -> PathBuf {
    let mut cur = pick.to_path_buf();
    for _ in 0..4 {
        if cur.join("messages").is_dir() {
            return cur;
        }
        match cur.parent() {
            Some(parent) => cur = parent.to_path_buf(),
            None => break,
        }
    }
    pick.to_path_buf()
}
```

(`export_file` in `import_bounded` is `file.canonicalize()`; for a Messenger dir pick `file` **is** the dir, so also guard: when `file.is_dir()`, compute `export_file` as `file.canonicalize()` too — it is only used by Telegram/WhatsApp arms, so the guard just keeps the existing code compiling; simplest is to only canonicalize `file` when `!file.is_dir()` and pass the unresolved dir where needed. Read the current lines 266-293 and keep Telegram/WhatsApp byte-identical in behavior.) Import in `chat_export.rs`: `use crate::messenger::{is_messenger_export_dir, messenger_export_root, is_messenger_export, is_messenger_export_reader, MessengerSource};`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p vault-core --test messenger_ingest chat_export::`
Expected: PASS. Also `cargo test -p vault-core` whole crate (resolution changes touch shared paths).

- [ ] **Step 6: Commit**

```bash
git add crates/vault-core/src/messenger.rs crates/vault-core/src/chat_export.rs crates/vault-core/tests/messenger_ingest.rs
git commit -m "feat(core): Messenger DYI tree picks — thread walk, numeric part merge"
```

---

### Task 7: Messenger media pass

**Files:**
- Modify: `crates/vault-core/src/bounded.rs` (`MediaKind::Messenger` + dispatch arm at ~610)
- Modify: `crates/vault-core/src/media.rs` (`extract_messenger`, `messenger_refs`, `messenger_entry_names`)
- Test: `crates/vault-core/tests/messenger_ingest.rs`, `crates/vault-core/src/media.rs` inline tests (mirror the telegram ones — grep `extract_telegram` in `media.rs` tests)

**Interfaces:**
- Consumes: `MediaKind::Messenger` set by Task 6's `import_bounded`.
- Produces: `pub fn extract_messenger(messages: &mut [NewMessage], export_root: &Path, data_dir: &Path, summary: &mut MediaSummary, budget: &mut Capacity, consumed: &mut HashSet<PathBuf>) -> Result<()>` — same contract as `extract_telegram` (`media.rs:285`).

- [ ] **Step 1: Write the failing test**

In `tests/messenger_ingest.rs`:

```rust
#[test]
fn tree_import_extracts_referenced_media() {
    use vault_core::chat_export::import_bounded;
    let tmp = tempfile::tempdir().unwrap();
    let root = dyi_tree(tmp.path());
    let ana = root.join("messages/inbox/ana_1111/photos");
    std::fs::create_dir_all(&ana).unwrap();
    std::fs::write(ana.join("ana.jpg"), b"jpgbytes").unwrap();
    std::fs::write(
        root.join("messages/inbox/ana_1111/message_1.json"),
        r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
            "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "slika",
            "photos": [{"uri": "messages/inbox/ana_1111/photos/ana.jpg", "creation_timestamp": 1762981200}]}]}"#,
    ).unwrap();
    // (dyi_tree wrote a message_1 without photos; overwrite as above, and
    // remove the other two part files for a focused count.)
    std::fs::remove_file(root.join("messages/inbox/ana_1111/message_2.json")).ok();
    std::fs::remove_file(root.join("messages/inbox/ana_1111/message_10.json")).ok();

    let data = tempfile::tempdir().unwrap();
    let storage = vault_core::storage::Storage::open_in_memory().unwrap();
    let outcome = import_bounded(
        &mut /* sink as in Task 6 */,
        &root,
        true,
        Some(data.path()),
        &|| false,
        &mut |_, _, _| {},
    )
    .unwrap();
    let media = outcome.media.expect("media pass ran");
    assert_eq!(media.referenced >= 1, true);
    assert_eq!(media.stored, 1);
    // The stored link rides in raw_json.
    // (query one stored message row and assert its raw_json contains "media")
}
```

(Adjust the sink binding and the raw_json assertion to the real APIs — `outcome.summary` plus a `Storage` query for the thread's message; mirror how `telegram_ingest`/`bounded_ingest` assert media summaries — read `crates/vault-core/tests/bounded_ingest.rs` first.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p vault-core --test messenger_ingest tree_import_extracts`
Expected: FAIL — `MediaKind::Messenger` doesn't exist / media is `None`.

- [ ] **Step 3: Implement**

`bounded.rs`: add `Messenger` to `MediaKind` (doc comment: DYI JSON; refs are root-relative `uri` strings) and the dispatch arm beside the Telegram one:

```rust
                    MediaKind::Messenger => crate::media::extract_messenger(
                        /* same argument shape as the MediaKind::Telegram arm */
                    ),
```

`media.rs`, modeled directly on `extract_telegram`/`telegram_refs` (`media.rs:285-385`):

```rust
/// The media reference fields of one DYI message object, in a stable
/// order: `(uri, None, None)` — every DYI media field is an array of
/// objects (or a single object for `sticker`) whose `uri` is the path,
/// relative to the DYI root.
fn messenger_refs(obj: &Map<String, Value>) -> Vec<(String, Option<String>, Option<String>)> {
    let mut refs = Vec::new();
    for field in ["photos", "videos", "audio_files", "files", "gifs"] {
        if let Some(Value::Array(items)) = obj.get(field) {
            for item in items {
                if let Some(uri) = item.get("uri").and_then(Value::as_str) {
                    if !uri.is_empty() {
                        refs.push((uri.to_string(), None, None));
                    }
                }
            }
        }
    }
    if let Some(uri) = obj
        .get("sticker")
        .and_then(Value::as_object)
        .and_then(|s| s.get("uri"))
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
    {
        refs.push((uri.to_string(), None, None));
    }
    refs
}
```

`extract_messenger`: copy `extract_telegram`'s body (`media.rs:285-341`) with `telegram_refs(obj)` → `messenger_refs(obj)` and the same `safe_child`/`store`/missing-entry handling — uris resolve against `export_root` (Task 6 guarantees it is the DYI root). Add `pub fn messenger_entry_names(obj: &Map<String, Value>) -> Vec<String>` mirroring `telegram_entry_names` (`media.rs:396-430`): the refs' `uri` basenames via `file_display_name`. Then grep `telegram_entry_names(` across `crates/vault-core/src/` and wire `messenger_entry_names` at every call site that switches on source/media format (the M4.2 F2/R3F1 reader seam — e.g. attachment summaries in search/detail rendering), matching how Telegram's is selected.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p vault-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/vault-core/src/bounded.rs crates/vault-core/src/media.rs crates/vault-core/tests/messenger_ingest.rs
git commit -m "feat(core): media pass for Messenger DYI exports"
```

---

### Task 8: Messenger preview + frontend labels

**Files:**
- Modify: `crates/vault-core/src/preview.rs` (`format_name`, media-ref hints)
- Modify: `src/importview.ts`, `src/main.ts` (messenger strings), `index.html` (`#import-help`)
- Test: `crates/vault-core/src/preview.rs` inline tests, `src/importview.test.ts`

**Interfaces:**
- Consumes: Tasks 4-6 resolution (`resolve_stream_source` now yields `ChatFormat::Messenger` for dir picks).
- Produces: `ImportPreview.kind === "messenger"`.

- [ ] **Step 1: Write the failing preview test** (inline, mirroring `previews_discord_package_directory` at `preview.rs:443`)

```rust
    #[test]
    fn previews_messenger_tree() {
        let dir = tempfile::tempdir().unwrap();
        let thread = dir.path().join("messages/inbox/ana_1111");
        std::fs::create_dir_all(&thread).unwrap();
        std::fs::write(
            thread.join("message_1.json"),
            r#"{"participants": [{"name": "Ana"}], "title": "Ana", "thread_path": "inbox/ana_1111",
                "messages": [{"sender_name": "Ana", "timestamp_ms": 1762981200000, "content": "zdravo"}]}"#,
        )
        .unwrap();
        let preview = preview_import(dir.path()).expect("messenger preview");
        assert_eq!(preview.kind, "messenger");
        assert_eq!(preview.source, "messenger");
        assert_eq!(preview.messages_seen, 1);
    }
```

- [ ] **Step 2: Implement**

`format_name`: `ChatFormat::Messenger => "messenger",`. In the preview media-refs accounting (find where `telegram_ref_paths` at `preview.rs:215` is used and how the source kind selects it), add `messenger_ref_paths(obj: &Map<String, Value>) -> Vec<&str>` returning the `uri` strings of `messenger_refs`' fields (the same five arrays + `sticker`) and wire it for the messenger kind.

`src/importview.ts`:
- `formatLabel`: `case "messenger": return "Messenger (Facebook) export";`
- `formatHint`: `case "messenger": return "Facebook → Settings → Your Facebook information → Download profile information — set Format to JSON; import messages/ or the whole export.";`

`index.html` `#import-help`: add Messenger to the format list (and confirm the Task 3 `.eml` line is in). `src/main.ts`: nothing new beyond Task 3's error sentence (already names Messenger).

- [ ] **Step 3: Run tests**

Run: `cargo test -p vault-core preview:: && npm test`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/vault-core/src/preview.rs src/importview.ts src/importview.test.ts index.html
git commit -m "feat: preview and labels for Messenger exports"
```

---

### Task 9: Messenger fuzz generator

**Files:**
- Create: `crates/vault-core/tests/fuzz/gen_messenger.rs`
- Modify: the property harness that registers generators (find it via `grep -rn "gen_signal" crates/vault-core/tests/`)

- [ ] **Step 1: Read the harness first**

Read `crates/vault-core/tests/fuzz/gen_signal.rs` and the property test that consumes it (likely `property/` or `fuzz/mod` style). The generator below adapts to that file's exact API (seed type, `Vec<(relative path, bytes)>` vs. single-file return).

- [ ] **Step 2: Implement the generator**

Core content (adjust the RNG/plumbing to the harness's shape):

```rust
/// One DYI-shaped thread: participants, a title, a thread_path with a
/// numeric suffix, and N messages — text, media-uri-only, and
/// no-body-no-media (skippable) entries mixed, some with identical
/// timestamps+sender (the occurrence-counter case), plus a broken
/// trailing entry in some seeds.
pub fn gen_thread(seed: u64, thread_id: u64, messages: usize) -> (String, String) {
    // returns (relative_path, json_text): "messages/inbox/{name}_{id}/message_1.json"
}
```

Generator rules (all deterministic from the seed): thread folder names `thread{thread_id}`; ids `1000 + thread_id`; message mix cycled `[text, media-photos, text, media-audio_files, reactions-only]`; timestamps strictly increasing within a thread, **equal** for two adjacent entries every 7th message; 1-in-5 seeds appends a `"not json"` broken file. Property assertions (in the harness's style): re-import is idempotent (`inserted` then all `skipped`), counts match the non-skippable entries, no panic across 200 seeds, and search finds a known body string.

- [ ] **Step 3: Run**

Run: `cargo test -p vault-core --test property` (or the harness's name — check `docs/testing.md` for the fuzz invocation) 
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/vault-core/tests/fuzz/gen_messenger.rs
git commit -m "test(core): seeded Messenger DYI fuzz generator"
```

---

### Task 10: e2e — Messenger fixture + spec, `.eml` case

**Files:**
- Create: `tests/e2e/fixtures/messenger-dyi/messages/inbox/ana_1111/message_1.json` (+ `message_2.json`)
- Modify: `tests/e2e/specs/02-import-mbox.mjs` (add an `.eml` case)
- Create: `tests/e2e/specs/10-messenger-import.mjs`

- [ ] **Step 1: Fixture**

`message_1.json` (adapted `THREAD_JSON` from Task 5, thread `ana_1111`, one photo message with uri `messages/inbox/ana_1111/photos/ana.jpg` + a real `photos/ana.jpg` byte file); `message_2.json` with two later messages.

- [ ] **Step 2: Spec**

Read `tests/e2e/specs/09-signal-import.mjs` and `tests/e2e/harness/` first; mirror its structure: launch fresh app → Sources → Import folder → pick `tests/e2e/fixtures/messenger-dyi` → assert the preview card shows `Messenger (Facebook) export` and the message count → confirm → assert search finds `zdravo` scoped to the import. Same for the `.eml` case inside `02-import-mbox.mjs`: pick `tests/e2e/fixtures/…/*.eml` (write a tiny `outlook.eml` fixture), preview shows `Email messages (.eml)`, import, search finds its subject/body.

- [ ] **Step 3: Run**

Run: `npm run e2e -- --spec 10-messenger-import` and `--spec 02-import-mbox` (check `package.json` scripts / `docs/testing.md` for the runner invocation and required env — the harness needs a Linux webkit2gtk driver build).
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add tests/e2e/fixtures/messenger-dyi tests/e2e/fixtures/outlook.eml tests/e2e/specs/10-messenger-import.mjs tests/e2e/specs/02-import-mbox.mjs
git commit -m "test(e2e): Messenger DYI import and .eml mail import specs"
```

---

### Task 11: Docs + full gates

**Files:**
- Modify: `docs/features.md`, `README.md`, `CHANGELOG.md`

- [ ] **Step 1: Update docs**

- `README.md` line 9 bullet → `Imports \`.mbox\`/\`.eml\` mail and WhatsApp, Telegram, Signal, Discord, and Messenger exports — single files, folders, or \`.zip\``
- `docs/features.md`: extend the import/formats sections — grep every `Signal` format list and add Messenger with a short note (DYI JSON only; HTML out of scope; thread key from folder suffix; parts merged numerically; media for tree picks, refs-only for single files) and `.eml` (bare RFC 5322, folder-of-eml, same `mbox` namespace so Takeout + .eml overlap dedups).
- `CHANGELOG.md`: Unreleased → `### Added` — `.eml` single-file/folder/zip mail import; Messenger (Facebook) DYI JSON import with media; preview labels.

- [ ] **Step 2: Full gates**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check && npm test && npm run check`
Expected: all PASS.

- [ ] **Step 3: Commit**

```bash
git add docs/features.md README.md CHANGELOG.md
git commit -m "docs: eml and Messenger import support"
```

---

## Self-Review (done at plan time)

1. **Spec coverage:** detection order (T4 pin), single `.eml` (T1/T3), `.eml` folder/zip (T2/T3), Messenger single-file (T5), thread folder + DYI tree + zip (T6 via existing zip expansion), numeric merge (T6), mapping/ids/keys (T5), media tree-pick + refs-only single-file (T6/T7), namespace rules (T1 `mbox`, T4 `messenger`), error handling (fence reuse T6, no repair), frontend content-only (T3/T8), tests matrix (T5/T6/T7/T9/T10), docs (T11). Gap check: none open.
2. **Placeholders:** Task 9/10 deliberately start with "read the harness file first" and give the content to adapt — the harness APIs were not read at plan time; the executor must check them before coding (flagged inline, not hidden).
3. **Type consistency:** `EmlDirSource` name/ctor used identically in T2/T3; `is_messenger_export(_reader)`/`is_messenger_export_dir`/`messenger_export_root` produced in T4/T6 and consumed where named; `MediaKind::Messenger` produced T7, set T6.

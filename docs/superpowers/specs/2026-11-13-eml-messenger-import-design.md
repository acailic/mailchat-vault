# Design: `.eml` and Messenger (Facebook) import sources

Date: 2026-11-13 · Branch: `feat/drop-import` · Status: design presented 2026-11-13; user review pending (scope and depth confirmed interactively; approach and full design proceeded on best judgment)

## Goal

Make this product promise true at the backend and existing UI entry points
("Import file… / Import folder…"):

> Drop in exports from Gmail, Outlook, WhatsApp, Telegram or Messenger.
> Everything becomes searchable — and never leaves your machine.
> `.mbox · .eml · .json · .txt`

| Promise | Today |
| --- | --- |
| Gmail (Takeout `.mbox`) | works |
| Outlook (`.eml`) | **new** — no `.eml` source |
| WhatsApp (`.txt`) | works |
| Telegram (`.json`) | works |
| Messenger (DYI `.json`) | **new** — no parser |

Out of scope (later branches): the drag-and-drop "Drop archives here" surface,
Facebook HTML exports, Instagram DYI messages.

## Approach

Two new sources following the existing per-format pattern (parser module +
integration test + fuzz generator + e2e fixture), plugged into the existing
bounded-ingest pipeline. No pipeline restructuring.

## `.eml` — mail side

1. **Lenient `stream_mbox`**: when the file does not start with a `From `
   separator line, treat the whole file as one message body and hand it to
   `parse_raw_mail` (the shared mbox/IMAP mapping: subject, reply refs, MIME
   attachments all come free). A bare RFC 5322 message becomes importable
   as-is.
2. **`EmlDirSource`**: a picked directory of `.eml` files imports as one run —
   bounded walk (`import_bounds`), files sorted by name, one message per file.
   A `.zip` pick that expands to `.eml` files (and holds no chat export)
   routes to the same `EmlDirSource` over the extracted tree — the decision
   lives in `preview_import`, which already expands zips for detection.
3. **Namespace**: source stays `"mbox"` — mail is one logical provider, so the
   same message imported via `.mbox` and via `.eml` deduplicates
   (`source + external_id` key; `external_id` is the `Message-ID`, same
   fallbacks as today).
4. **Detection** in `preview_import`: `.eml` extension, or content sniff —
   first line matches a header line (`Name: value`, RFC 5322 field syntax),
   checked only when the mbox `From ` sniff fails. Preview `kind` = `"eml"`;
   the frontend routes `kind === "eml"` to the existing `importMbox`
   command.

## Messenger — chat side

New `crates/vault-core/src/messenger.rs`, modeled on `telegram.rs`. Source
name / dedup namespace: `MESSENGER_SOURCE = "messenger"`.

### Detection (by structure, not extension)

`is_messenger_export(bytes)` / `is_messenger_export_reader(reader)`: a
top-level JSON object holding a `participants` array plus a `messages` array
whose elements carry `sender_name`/`timestamp_ms`.

**Ordering matters and must be pinned by tests:** Messenger is checked after
Discord but **before Telegram** — the current Telegram probe is "has a
`messages` array", which a Messenger file would satisfy. New order:
Discord → Messenger → Telegram → Signal → WhatsApp.

### Package mode (like the Discord package)

- `is_messenger_export_dir(dir)`: the tree contains a `messages/inbox/` or
  `messages/archived_threads/` subtree with thread folders holding
  `message_*.json`. Checked at the picked root and after the existing
  single-subdirectory descent (DYI zips wrap one `facebook-*/` root).
- `MessengerSource` walks thread folders through the bounded-walk fence:
  each folder is one conversation; `message_1.json`, `message_2.json`, …
  merge in **numeric** suffix order (`message_10` after `message_9`).
- A directory whose export files are all Messenger `message_*.json` (the
  user picked a single thread folder) merges as one conversation; when
  `thread_path` values differ across files, group by `thread_path`.
- Zip picks already expand via `expand_zip_bounded`; the extracted tree is
  then a directory pick, and `tree_pick` unlinked-media semantics apply
  unchanged.

### Message mapping

| DYI field | `NewMessage` |
| --- | --- |
| `sender_name` | `author` |
| `timestamp_ms` (UTC ms) | `sent_at` (RFC 3339 UTC, ms truncated), `time_confidence = "explicit_offset"`, `timezone = "UTC"` |
| `content` | `body` |
| whole message object | `raw_json` (verbatim, Telegram pattern) |
| (none — FB carries no ids) | `external_id` = synthetic hash, pattern from `whatsapp.rs` (thread key + timestamp + sender + body). Pick-scope caveat: the occurrence counter spans one pick's thread, so importing a lone `message_2.json` before the whole thread re-numbers byte-identical repeats — the later whole-thread import stores them again |
| `title` | conversation label |
| `thread_path` / folder name suffix `…_<digits>` | `conversation_key = "messenger:thread:<digits>"`; no numeric suffix → empty conversation key (legacy chat_id grouping), matching the implementation |

Messages with neither body nor media references are skipped (Telegram
service-entry pattern).

### Media

- **Tree pick (folder/zip):** new `MediaKind::Messenger`; `uri` fields under
  `photos`, `videos`, `audio_files`, `files` resolve against the DYI root and
  copy into the content-addressed store via the existing `MediaPass`; a
  `media` array lands in `raw_json` (existing behavior).
- **Single-file pick:** references are recorded in `raw_json` only — the DYI
  root is unknown, so no copying (documented limitation, same stance as
  Signal/Discord attachments today).

## Error handling

- Unrecognized content → `None` from detection (existing behavior, unchanged).
- Oversized archives / symlink tricks → refused by `import_bounds` before any
  message is stored.
- Non-UTF-8 or malformed JSON → counted through the existing
  parse-failure/import-issues accounting; not repaired (YAGNI).
- Cancel/resume rides the existing `BoundedRun` checkpoint system.

## Frontend — content updates only, no new surfaces

Picker filter gains `eml`; `formatLabel` gains `eml` ("Email (.eml)") and
`messenger` ("Messenger (Facebook)"); `friendlyImportError` supported-inputs
sentence updated; import-help format list updated; one routing condition
(`kind === "eml"` → `importMbox`).

## Testing

- `crates/vault-core/tests/messenger_ingest.rs` — single file, numeric
  `message_N` ordering, thread-folder merge, DYI tree, zip, dedup on
  re-import, conversation keys, media references, detection-order pin
  (Messenger file must not be detected as Telegram).
- `crates/vault-core/tests/fuzz/gen_messenger.rs` — seeded generator for the
  property suite.
- `mbox.rs` unit tests — leading `From ` optional; `EmlDirSource` ordering.
- `preview.rs` tests — `.eml` by extension and by sniff; messenger dir.
- e2e: `tests/e2e/fixtures/messenger-dyi/` fixture + new
  `10-messenger-import.mjs`; extend the mbox spec with an `.eml` case.
- `npm test` — label/routing updates.
- Gates: `cargo test --workspace`, `clippy`, `fmt --check`, `npm test`,
  `npm run check`.

## Documentation

`docs/features.md` (import list + chat-format notes), README feature bullet,
`CHANGELOG.md` entry.

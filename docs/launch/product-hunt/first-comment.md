# Maker first comment — Product Hunt launch

Draft for the maker's first comment on the launch page. The "what it is"
paragraph is the canonical positioning block from
[`docs/landing.md`](../../landing.md), quoted verbatim between the
positioning-block markers — `npm run check:docs` enforces that this
quote matches the source byte for byte. Do not reword one copy without
the others.

Paste everything below the line as the first comment.

---

Hi Product Hunt! I'm the maker. Thanks for stopping by — here is what
Mailchat Vault is, in one paragraph:

<!-- positioning-block:start -->
Mailchat Vault is one private, searchable archive for your email and your chats, on your own machine: drop in a Gmail Takeout export or a WhatsApp, Telegram, Signal, Discord, or Messenger export and find any message in seconds — fully offline, with no cloud service holding your messages.

- Email and chats in one archive: `.mbox`/`.eml` files and IMAP/Gmail sync, plus WhatsApp, Telegram, Signal, Discord, and Messenger exports.
- Full-text search with highlighted hits, filters, and conversation context, fully offline.
- Free and open source (GPL-3.0-only), with installers for Linux, macOS, and Windows.

Local-first privacy: search stays on this device.
<!-- positioning-block:end -->

## Why local-first

Your message archive is years of private life in one file. That is why
the archive and the search engine live on your machine and work fully
offline — search stays on this device. Network use is separate and
explicit: connecting an account contacts your mail provider (Google
sign-in contacts Google; IMAP sync is read-only), and the optional
AI-assisted search sends only the query and the top matching rows, and
only after an explicit consent prompt that resets every app start. At
rest, the archive relies on your OS account and disk protection;
credentials go in the OS keychain. The source code (GPL-3.0-only) is
the proof — every claim above is checkable in the repository.

## Not in v0.2.x — on purpose

To keep the core honest, these are deliberately not in this release:

- Sending, replying, or editing mail — the archive is read-only; even
  IMAP sync never deletes from the server.
- Cloud synchronization or accounts of any kind — there is no cloud
  service; no server holds your messages.
- A mobile app.
- Automatic updates — releases ship with a `SHA256SUMS` file and a
  one-line install script that verifies every download.
- OCR and attachment-content indexing — attachments are preserved, not
  text-searched.

One honest limitation worth knowing before you download: installers are
not signed or notarized yet
([#78](https://github.com/acailic/mailchat-vault/issues/78) tracks it),
so macOS and Windows show an extra warning on first run. Every release
carries `SHA256SUMS`; the download guide shows how to verify against it.

Downloads and the version matrix: [download &
install](https://github.com/acailic/mailchat-vault/blob/main/docs/download.md).
Questions and export-format war stories very welcome — I'll be here all
day.

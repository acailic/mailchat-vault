# Changelog

Notable, user-facing changes per release. Internal-only work (docs
grounding, test plumbing) is summarized where it changes what testers
and packagers run, not item by item.

## [0.2.0] - pending tag

The media release: attachments and chat-export media are now preserved
and viewable in the app, AI answers run only behind your explicit
consent, new mail can notify you, and the Linux packages install as
real desktop citizens. Vaults from 0.1.0 upgrade in place, safely.

### Added

- **Email attachments** — synced mail now stores its MIME attachments
  and inline parts (images, documents, media) alongside the message.
  Attachments that are missing or failed to transfer are shown as such
  instead of silently disappearing, and re-importing an export enriches
  earlier messages with media that arrived later.
- **Inline media viewing** — images, audio, and video from your archive
  open directly in the reading pane through a read-only, sandboxed
  `mcmedia://` protocol. No autoplay, no remote-resource fetching, no
  active content execution; unsupported or missing media stays visibly
  unavailable rather than pretending to work.
- **Chat-export media preservation** — photos, videos, and voice notes
  inside WhatsApp/Telegram/Discord exports are extracted into a
  content-addressed store: identical files are stored once regardless
  of name or location, every object is checksum-verified, interrupted
  imports never leave half-written files behind, and a disk-usage view
  reports what the media store actually holds.
- **AI answers behind explicit consent** — asking an AI provider about
  your archive is now gated: nothing leaves your machine until you
  grant consent for the current session, and the grant is never
  persisted — every app start asks anew. Answers come back with
  numbered citations that jump to the original messages; without a key
  or without consent, the ask falls back to plain local search. Without
  consent there are zero network calls, verified by test.
- **New-mail desktop notifications** — when a sync or folder watch
  brings in new mail, a native desktop notification fires (macOS,
  Windows, Linux). Notifications stay silent while the app window is
  focused, honor the on/off switch in Settings and the OS-level
  permission, and never interrupt the app if the OS refuses them.
- **Headless `--version` / `--help`** — the packaged binary answers
  `--version`/`-V` and `--help`/`-h` and exits before touching any
  GUI, so installed copies can be smoke-tested without a display.
- **Linux desktop integration** — the `.deb` and AppImage now install
  launcher entries with icons, register the `.mbox` file type so
  "open with Mailchat Vault" works from the file manager, and the
  AppImage carries its entire runtime (WebKitGTK included), running on
  a clean Ubuntu with none of the app libraries installed.

### Changed

- **New workspace layout** — the main window is now a three-pane
  workspace (navigation / results / reading) with a consistent design
  system, replacing the single-pane v0 flow.
- **Search over the canonical store** — search, message detail, and
  conversation context now run over the canonical archive tables,
  giving stable results and honest "the archive changed under your
  pagination" behavior instead of silent restarts.

### Fixed

- Configured periodic-sync intervals are preserved across restarts
  (previously a normalization could reset them), and job status events
  are reported consistently.

### For testers and packagers

- One-command release validation per platform:
  `scripts/release/matrix-linux.sh` (any docker host),
  `matrix-macos.sh` / `matrix-windows.ps1` (real hosts; refuse
  off-platform). A full Linux column run is green at this tree.
- Automated Windows installer smoke (fresh install, uninstall-retention,
  in-place upgrade from 0.1.0 with a frozen archive fixture) is wired
  and waiting for a real runner; the macOS dmg smoke
  (`scripts/dmg-smoke-test.sh`) runs wherever a dmg exists.
- `scripts/release/prepare-release.sh` assembles the local
  draft-release candidate (bundles, checksums, notes) without tagging
  or publishing anything — see `docs/release.md`.

### Upgrading from 0.1.0

- Existing vaults upgrade automatically on first open (schema 11 → 13:
  the media object registry and archive metadata). The migration takes
  a verified snapshot first, converts in a single transaction, and
  restores the original byte-for-byte on any failure; a database
  written by a newer build is refused by older binaries by design.
- No credentials are migrated or moved; account connections carry over
  as they were.

### Known limitations (honest disclosure)

- **Artifacts are unsigned** — macOS: Gatekeeper blocks the first
  launch of a downloaded copy (right-click → Open → Open, or
  `xattr -cr /Applications/mailchat-vault.app`); Windows: SmartScreen
  shows "Windows protected your PC" → *More info* → *Run anyway*.
  Real signing/notarization is an external dependency, deliberately not
  acquired by this codebase.
- **macOS/Windows bundles for this release wait on a maintainer
  decision** — real machines versus temporarily re-enabling GitHub
  Actions; the Linux column is fully validated at this tree.
- **No backup/restore yet** — the versioned backup feature is planned
  but not shipped in 0.2.0; copy the data directory manually if you
  need a snapshot.
- **No multi-account yet** — the multi-account accounts panel (PR #53)
  is open and unmerged, so 0.2.0 still holds one account per vault.
- **No auto-update** — new releases are a fresh download; there is no
  in-app updater.

## [0.1.0] - 2026-11-04

Initial published release.

- Local email & chat archive with full-text search: import WhatsApp,
  Telegram, Discord, Signal exports and mbox files; incremental IMAP
  sync (app passwords and Gmail OAuth); filtered, paged search with
  highlights and thread context; export to CSV, JSON Lines, and MBOX.
- Local embeddings + hybrid Ask ranking, LLM-assisted search layer
  with a provider menu (pre-consent-gate; 0.2.0 gates it).
- Job engine with progress and cancellation, resumable bounded imports,
  periodic sync, folder watching, statistics dashboard.
- Desktop packaging: Linux AppImage + deb, macOS universal dmg,
  Windows msi/NSIS — unsigned (see disclosure above).
- Safe vault migrations with snapshot/rollback; older binaries refuse
  newer databases.

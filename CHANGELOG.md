# Changelog

Notable, user-facing changes per release. Internal-only work (docs
grounding, test plumbing) is summarized where it changes what testers
and packagers run, not item by item.

## [0.2.0] - pending tag

The media release: attachments and chat-export media are now preserved
and viewable in the app, AI answers run only behind your explicit
consent, new mail can notify you, and the Linux packages install as
real desktop citizens. Vaults from 0.1.0 upgrade in place, safely.

### Added (first-sync failure loop — resilience)

- **A failed first sync keeps the account** — connecting an email account
  that fails (wrong host, wrong port, server down) still registers the
  account with everything you entered: host, port, username, folders.
  The accounts panel shows the row with what fell, why, and when — and
  that state survives restarts, so you fix the typo instead of retyping
  the whole form.
- **Understandable sync errors** — failures are classified and stated in
  plain terms instead of raw OS error codes: "the server address could
  not be resolved (DNS) — check the host name", "nothing answered at
  that host and port — check the server address and port (993 for IMAP
  over TLS)", "the server rejected the login — check the username and
  password (Gmail requires an app password…)", and timeout / no-route /
  TLS variants. The same classification covers the "Sync now" action and
  scheduled rounds.
- **Honest password rules** — the sync form states up front where a
  password lives (OS keyring when available, session memory otherwise —
  and that restarting the app then clears it), and every sync result,
  success or failure, repeats where the entered password was just kept —
  a failed connection attempt has already saved it.

### Added (first-source journey)

- **First-run guidance** — an empty archive now presents "Add your
  first source" (import files, connect email, or try a sample archive)
  instead of an empty search box.
- **Isolated sample archive** — "Try a sample archive" opens a private
  in-memory demo vault: searches, reading, and stats answer from it,
  while the personal vault is never touched and every import/sync
  refuses until you leave the sample. Demo content can no longer mix
  into a personal archive.
- **Import preview** — every import starts from the native file/folder
  picker and shows a bounded preview before anything is stored: detected
  format, sample messages, message counts (lower bounds for huge
  inputs), media references and provably-missing files, input size, and
  free-space guidance that disables Import when the archive likely does
  not fit. No more typed paths or raw `os error` messages.
- **"View imported messages"** — a finished import offers a one-click
  jump to the messages that run brought in, scoped to exactly that
  import.
- **Truthful "All messages"** — the workspace browses newest-first on
  boot and after imports instead of showing zero rows until something
  is typed or filtered.

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
- **Multi-account accounts panel** — every configured account (email
  and file imports) in one Accounts overview: per-account message
  counts, last-sync stamps, pause state, and which credentials are
  stored (never the secrets themselves). Email accounts can be synced,
  paused, and have their keyring credentials forgotten right from the
  row, while the archive stays untouched. Email accounts are named by
  the identity you know them by (address · server) instead of the raw
  `imap:host:port:user` token, which stays reachable as a tooltip, and
  every panel status line uses the same name.
- **Readable certainty labels in the reading pane** — raw internal
  tokens like `explicit_offset` and `legacy_unresolved` no longer reach
  the screen: the date row says `(exact)`, `(confirmed)`, `(assumed)`,
  `(unresolved)` or `(no time recorded)` with a one-sentence tooltip
  explaining what the archive actually knows, an unverified conversation
  row reads `(unverified grouping)`, and Copy message / text exports
  carry the same wording on both the TypeScript and Rust sides.
- **“Add account…” arrives with context** — the Accounts panel's
  “Add account…” button no longer dead-ends on the Add source page: a
  status line says where the import and connect-email paths live and
  that the new account appears back in the Accounts panel (it retires
  once you navigate away).
- **Headless `--version` / `--help`** — the packaged binary answers
  `--version`/`-V` and `--help`/`-h` and exits before touching any
  GUI, so installed copies can be smoke-tested without a display.
- **Linux desktop integration** — the `.deb` and AppImage now install
  launcher entries with icons, register the `.mbox` file type so
  "open with Mailchat Vault" works from the file manager, and the
  AppImage carries its entire runtime (WebKitGTK included), running on
  a clean Ubuntu with none of the app libraries installed.

### Changed

- **The vault's backup state survives restarts** — Settings →
  Storage & backup now leads the Settings view (the safety and
  location questions before provider tuning) and answers them plainly:
  where the archive lives, when the last verified backup was made
  (with its size, captured counts, and destination — flagged honestly
  if the file is no longer there), and, after a restore or a return to
  a previous vault, exactly what is active now, since when, and from
  where — including what an interrupted switch's startup recovery
  decided (rolled back or completed). A synced folder's status line
  also reports its media counters (stored/deduped/missing attachments)
  alongside the message counts.
- **New workspace layout** — the main window is now a three-pane
  workspace (navigation / results / reading) with a consistent design
  system, replacing the single-pane v0 flow.
- **Search over the canonical store** — search, message detail, and
  conversation context now run over the canonical archive tables,
  giving stable results and honest "the archive changed under your
  pagination" behavior instead of silent restarts.
- **Imports say what happened to media** — the import status line now
  reports the media pass's counters beyond message counts: stored,
  deduplicated, and missing attachment counts, and for whole-tree
  picks also stray files stored with no message linked ("N unlinked"),
  files skipped because the media budget ran out ("N dropped
  (budget)"), and a failed tree walk ("walk failed"). Mbox imports
  report their attachment counters too, instead of dropping them.

### Fixed

- The AI-consent prompt buttons no longer render on the very first boot,
  on an empty archive, before any question is asked: the `hidden`
  attribute now always wins over layout rules such as the results-row
  `display: flex` (the same guard covers the archive-changed refresh
  notice, which had the same shape).
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

# Changelog

Notable, user-facing changes per release. Internal-only work (docs
grounding, test plumbing) is summarized where it changes what testers
and packagers run, not item by item.

## [0.2.0] - 2026-11-17

### Added (tamper-evident export verification)

- Every readable offline HTML export bundle now carries a
  `manifest.json`: one entry per exported message (its external id, the
  archive's stored digest of the message, and the digest of its retained
  original bytes when the archive retained one), the run receipts of the
  import/sync runs that covered each message's source, and the SHA-256
  of every page and asset file — all chained together git-commit-style
  (each line hashes the previous) into a single head hash. Re-exporting
  unchanged data reproduces the same head hash; changing any chained
  page, asset, or recorded digest afterward breaks it. The vault-core
  verifier (`verify_bundle`) recomputes the chain from the bundle alone
  and names the first divergence — the message, page, or asset that no
  longer matches; a bundle without its manifest reports clearly
  incomplete. A `README.md` inside the bundle states plainly what the
  manifest proves and what it does not: it is tamper evidence from the
  moment of export, not third-party notarization, and receipts are
  evidence of what runs observed, not a completeness claim. (The in-app
  "Verify an export" surface ships with the frontend slice; the verifier
  is a vault-core API today.)

### Changed (Vesper is the default theme)

- A fresh vault now opens in Vesper: with no saved theme choice (and no
  machine-local record), the first settings render applies the Vesper
  palette and records the choice, so restarts stay Vesper regardless of
  the OS color scheme. Vaults that saved Light or Dark keep their theme,
  and picking Light, Dark, or System — including switching back from the
  Vesper default — records the explicit choice so it sticks.

### Added (per-source status on the navigation chips)

- Every source chip in the navigation column now says what state its
  source is in, right beside the label and count: "Syncing…" while a
  manual sync or a scheduled sync round runs, "Needs sign-in" when no
  credentials are stored or the last sync failed on them, "Sync failed
  — <reason>" for other recorded failures, "Paused" for excluded
  accounts, "Last synced <time>" ("Never synced" before the first
  success) for connected email, and "Imported <time>" for file imports.
  The label, count, and one-click source filtering are unchanged.

### Added (GP03 — first-session journey)

- The import preview confirms what it assumed: exports with clock times
  but no timezone (WhatsApp, Telegram's fallback dates) state the count
  and the assumed zone, which you confirm or correct once per run;
  affected messages record the confirmed label (`user_confirmed`).
  Ambiguous DST times keep the first occurrence and removed ones stay
  unresolved with their original text — both stated, never hidden.
- The import destination is explicit: the derived source name is
  preselected (nothing new to do on a fresh vault) and can be replaced
  with a new name or an existing import source; re-imports into the same
  source never duplicate.
- A failed import offers Retry in place — the same path and options
  re-submit and resume without duplicating.
- A finished import links its counts to recovery actions: parse failures
  open Activity's explanation, missing media say how to re-export with
  media and import again.
- "What can I import?" (supported formats + per-format how-to-export
  hints) now appears on the first-run card and the Add-source chooser,
  not only inside the import section.

### Added (watched-folder retry)

- A failed automatic import can be retried once its cause is resolved,
  without touching the source file or restarting the app: the watcher
  re-runs the same import unit (a Messenger part retries its whole
  thread) under its unchanged import identity, so already stored
  messages stay single. Retries for files whose watched folder was
  removed are refused; a permanently failing input is never requeued
  automatically (no retry loop), and each watcher snapshot now carries
  per-unit import states (pending/running/succeeded/failed). The Retry
  affordance in the watching panel ships separately.

### Added (Vesper — a fourth theme)

- **Vesper** — Settings → Appearance gains a fourth theme choice: a warm,
  paper-like palette — warm charcoal surfaces, warm off-white text, muted
  sand secondary text, and a warm orange accent — applied through the same
  design-token seam as the existing themes and held to the same contrast
  floor. Light, dark, and system are visually unchanged, and the Vesper
  choice persists across launches (#214).

### Added (saved searches)

- A search can now be saved as a named definition and re-run later exactly
  as defined: the full v2 request (source ids, author, day bounds and
  timezone, attachment/honesty filters, import scope, sort order) is
  persisted, and re-running re-executes the live query — newly imported
  matching messages appear — while canonical source ids keep label renames
  and duplicate labels from redirecting it (#196). The saved-search UI in
  the workspace ships separately.

### Added (collections)

- Messages can be grouped across sources into named collections — an
  additive organization layer over the archive: membership references
  canonical messages, is idempotent per (collection, message), and never
  touches message text, timestamps, provenance, deduplication, or
  conversation identity (#195). The collections UI ships separately.

### Added (people — confirmed identities across sources)

- The vault can hold people records with user-confirmed identity links
  across sources, so the same correspondent in two accounts resolves to
  one person (#207). The merge also retired latent lint/compile debt on
  main that blocked verification. The people UI ships separately.

### Added (what's-new boot digest)

- One vault-side call now answers "what changed since my last visit":
  newly archived messages per source plus finished or failed imports and
  syncs, with a vault-scoped last-visit cutoff that advances exactly once
  per read — the next call counts only newer changes, and a first visit
  is a deliberately quiet, empty digest (#201). The boot-surface UI in
  the window ships separately.

### Added (attachment index and files search)

- Attachments and unlinked export files gain one honest index: a derived,
  repairable projection spans both the per-message media references and
  the media-object registry, records availability as preserved or missing
  per reference, backfills resumably, and answers archive-wide files
  queries from it (#211). The Files view ships separately.

### Added (deep-page search — keyset cursors)

- Both search paths (full-text and browse) return a stable, opaque
  next-page cursor, and sending it back fetches the following page at
  constant cost — a deep page costs the same as the first page instead of
  degrading linearly with offset (#200). Deterministic ordering is
  preserved across pages. The workspace's own paging UI adopts the
  cursor separately.

### Added (AI provider registry — internal substrate)

- The provider registry behind Settings' AI configuration became data:
  every provider kind carries its auth style, request wire, and versioned
  defaults (model, base URL, temperature, max tokens, timeout), with
  per-profile sampling overrides and one OpenAI-compatible client shared
  by the openai/groq/openrouter/ollama/custom kinds (#210). No visible
  Settings change yet; the configuration UI lands with its own slice.

The media release: attachments and chat-export media are now preserved
and viewable in the app, AI answers run only behind your explicit
consent, new mail can notify you, and the Linux packages install as
real desktop citizens. Vaults from 0.1.0 upgrade in place, safely.

### Fixed (navigation review)

- Exporting search results preserves source-instance selection, attachment and
  import filters, date/timezone bounds, and sort order across every page.
  Older export callers keep their existing source-name contract.
- JSON Lines exports — search-result subsets and whole-conversation chat
  exports alike — carry a portable source identity (`source_key`), so a
  subset exported from two sources sharing a display label — and a mail
  Message-ID — imports with both messages retained instead of the second
  collapsing onto the first as an apparent duplicate. Older JSON Lines files
  import exactly as before.
- Activity attention survives missed-event recovery and stays dismissed when an
  acknowledged failure event is replayed.
- Corrected two native E2E calls that aborted the source-selection and Add source
  chooser scenarios before their assertions ran.

### Fixed (parked review follow-ups)

- The time-period strip collapses again after the timeline shrinks: a strip
  expanded with "Older…" and then rendered with nothing left to hide starts
  collapsed when the timeline grows past the cap again, instead of silently
  re-opening expanded.
- Export popover buttons in the reading pane render at the smaller meta text
  size again, matching the captions and format selects beside them.
- A failed statistics fetch at import completion no longer marks the archive
  view live on a stale total: the honest "type something to search" line stays
  until the next successful refresh drives the newest-first browse.
- Schema housekeeping (migration 17): the redundant single-column
  archive-messages index — covered by migration 15's composite — is dropped;
  vaults upgrade in place as always.

### Fixed (accessibility)

- Placeholder text renders in the muted token at full opacity — the browser
  default gray failed the 4.5:1 contrast floor on dark surfaces (measured
  pairs recorded beside the rule, per the tokens.css convention). Native
  checkboxes render with the palette accent instead of the GTK dark-theme
  amber, and the result-row selection checkbox gained a 24px hit area via a
  padding halo. Reading-header icon buttons (copy, link, export, close)
  are 32px targets with a stronger hover border. The topbar Menu button is
  no longer the first Tab stop inside content — view controls come first,
  while the button keeps its visual place in the topbar.

### Fixed (CSV export)

- CSV exports no longer corrupt rows whose cells get the spreadsheet-formula
  apostrophe and also need quoting: a body like `- hello, world` exported as
  `'\"- hello, world\"` with the apostrophe outside the quotes, so strict CSV
  readers saw 9 columns instead of 8 and shifted the data. The apostrophe is
  now part of the field value before quoting (`\"'- hello, world\"`); cells
  that don't need quotes and cells without a prefix export byte-identical to
  before.

### Fixed (export durability)

- A failed message export no longer destroys the file it was writing to.
  Every streaming export (search results, selection, whole conversation)
  now writes into a private temporary sibling of the chosen destination
  and replaces it only after the complete file is written, flushed, and
  synced — a failure partway through leaves a previously valid export
  byte-for-byte unchanged, and an export to a new path that fails leaves
  no half-written file behind.

### Added (JSON Lines import)

- **JSON Lines import** — the `.jsonl` files the app exports now import
  back: picking one runs the ordinary bounded preview → confirm → import
  flow, each row keeping its original source and external id, so an
  exported subset lands identically in another vault and importing the
  same file twice stores each message exactly once. Malformed lines
  become per-item issue records instead of aborting the run; media
  references in a row's `raw_json` stay references (media bytes are not
  transferred — the backup container remains the whole-vault tool).
  Closes the export/import round-trip for the app's machine-readable
  format.

### Added (.eml & Messenger import)

- **`.eml` mail import** — single `.eml` messages (dragged out of any
  mail app), a folder of them, or a `.zip` of them import directly,
  attachments and all. All mail picks share the mbox identity
  namespace, so the same message in a Google Takeout `.mbox` and a
  loose `.eml` stores exactly once.
- **Messenger (Facebook) import** — "Download Your Information"
  exports with Format set to JSON: a single thread file, a thread
  folder, the whole `messages/` tree, or a `.zip`. One thread's
  `message_1.json` … `message_N.json` parts merge in numeric order;
  media (photos and other referenced files) is extracted for
  whole-tree picks, single-file picks keep their references recorded.
  The HTML export flavor is out of scope.
- **Preview knows the new formats** — the import preview detects and
  labels `.eml` picks ("Email messages (.eml)") and Messenger exports
  ("Messenger (Facebook) export"), with a how-to-export hint for each.

### Added (drag-and-drop import)

- **Drop archives anywhere on the window to import them** — files and
  folders alike route into the exact same bounded preview → confirm →
  import flow the Import file… / Import folder… buttons produce;
  nothing is ever stored without the confirmed preview. Two dashed
  drop zones advertise the surface — on the first-run "Add your first
  source" card and in Sources → Import, both reading "Drop archives
  here — .mbox · .eml · .json · .txt" — and highlight while a drag is
  over the window. Dropping several paths at once previews them one at
  a time in drop order (the waiting count shows in the section's
  status line); a drop while an import runs waits until it finishes,
  and drops are ignored while the sample archive is open, like the
  picker buttons. Routed through the Tauri 2 drag-drop events (real
  file/folder paths) with the window's `dragDropEnabled` setting pinned
  on. Manual smoke (on a desktop): drag one `.mbox` and one export
  folder onto the window — each previews with the same card the pickers
  show.

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

### Added (background keep-alive — always-fresh archive)

- **Keep running in the background (opt-in, off by default)** — a new
  Settings toggle: closing the window hides the app to a **tray icon**
  instead of exiting, so the opt-in auto-sync schedule, folder watching,
  and new-mail notifications keep working between opens. The tray menu
  offers *Show Mailchat Vault*, *Sync now* (exactly the manual sync-all
  round — an ordinary job, visible in the jobs panel), and *Quit*.
- **Start automatically at login (opt-in, off by default)** — a second
  Settings toggle registers the app as an OS login item (applied the
  moment Settings are saved, removed when it is turned off).
- **Single instance** — launching the app a second time focuses the
  existing window instead of opening a second app over the same vault.
- **Honest degradation** — with the toggle off, closing exits exactly as
  before; where the tray icon cannot be built (the appindicator library
  is missing on Linux, or the tray build fails), keep-alive falls back
  to that same close-and-exit — loudly logged, and never a hidden,
  unreachable process via a missing tray. (A desktop that accepts the
  icon but renders no status area — e.g. GNOME without the AppIndicator
  extension — leaves the icon invisible; recovery is a second launch,
  which the single-instance guard focuses.) There is deliberately no
  headless daemon — background operation is this tray keep-alive, never
  a second process over the vault (`docs/decisions.md`, D10 amendment).
- The Linux deb now **declares** its runtime dependencies explicitly
  (`libwebkit2gtk-4.1-0`, `libgtk-3-0`, and the tray's
  `libayatana-appindicator3-1` — the bundler writes the Depends field
  verbatim from this declaration); the AppImage does not bundle the
  dlopen'd tray library, so on systems without it the tray degrades as
  above. Empirical packaged-tray evidence is owned by the release
  validation route.

### Added (retrieval quality — search and Ask)

- **Subject matches rank higher** — full-text results now score matches
  per field (BM25F-style weights: subject 3, body 1, author 1), so a
  message whose *subject* carries your term outranks one that merely
  mentions it in a long body. Applies everywhere FTS ranking is used:
  plain search, exports, and the Ask pipeline.
- **Ask can target subjects** — the AI translation step now knows about
  the `subject:` column filter ("mail about the vault migration" plans
  `subject: vault migration`), in addition to `body:` and `author:`.
- **Smarter summary context order** — the rows sent to the AI for an
  answer are arranged strongest-first *and* strongest-last instead of
  strict rank order, following the "Lost in the Middle" finding that
  models attend least to the middle of their context. Citations [1],
  [2]… keep their rank-based numbering.
- **A measured retrieval test suite** — a labeled fixture corpus with
  recall@10/nDCG@10 gates over the FTS, vector, and fused ranking legs
  now guards every ranking change (see
  `docs/evidence/literature-review-2026-11-13.md` for the evidence
  behind these changes).

### Added (retrieval quality benchmark — measurement track)

- **A retrieval-quality benchmark now runs on every PR** — the U07
  measurement track from the user success plan: 36 labeled questions
  (English and Serbian, email and chat originals, nine categories
  including paraphrase, word-form variation, typos and no-answer
  probes) over a deterministic 10k-message synthetic archive, measured
  through the app's real search paths — the ordinary FTS5 search and
  the local FTS5+RRF hybrid ranking. Recall, MRR, false-confidence and
  latency baselines with claim-calibration notes are recorded in
  `tests/retrieval/BASELINE.md`; the CI gate protects the instrument
  (corpus/label integrity, lexically-trivial floors), never a measured
  quality level.

### Fixed (retrieval benchmark — claim calibration, U07)

- Public search-quality claims are now wired to the U07 benchmark: the
  support matrix's search and hybrid-ranking rows carry the measured
  numbers and their forbidden claims (no synonym/paraphrase,
  cross-language, or typo-at-scale claims; hybrid results are closest
  matches, never proof the archive contains the answer). The benchmark
  was re-verified on the current baseline at both scales (numbers
  unchanged); record in
  `docs/evidence/u07-retrieval-benchmark-2026-11-16.md`. The `vault-retrieval`
  harness crate's license is aligned with D13 (`GPL-3.0-only`, was
  `UNLICENSED`).

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
- **Media integrity findings resolve to affected messages** — the
  integrity check now names what a damaged object affects: each missing
  or corrupt file resolves to the archived messages that reference it
  (a message to open, with its conversation and date) and to the known
  import that stored it, in bounded pages with totals — read-only and
  diagnostic, with recovery left to you (restore a known-good backup or
  re-import the original export). Backend surface this release; the
  report UI follows.
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
  in-place upgrade from 0.1.0 with a frozen archive fixture) runs on
  demand from the Windows smoke workflow while Actions can start jobs;
  with runners billing-blocked, the same gate runs locally
  (`scripts/windows-smoke.ps1` against built installers, or
  `scripts/release/windows-smoke-from-release.ps1` against a published
  release). The macOS dmg smoke (`scripts/dmg-smoke-test.sh`) runs
  wherever a dmg exists — including inside the Release workflow, before
  the dmg is attached to the draft.
- `scripts/release/prepare-release.sh` assembles the local
  draft-release candidate (bundles, checksums, notes) without tagging
  or publishing anything — `--tag-exists-ok` permits post-tag local
  assembly under the local execution route; see `docs/release.md`.
- Native verification (G1) is now a runbook with machine-readable
  evidence: `scripts/release/smoke-macos.sh` and
  `scripts/windows-smoke.ps1` (driven by
  `scripts/release/windows-smoke-from-release.ps1`) each write a JSON
  record binding the run's result to host, tester, UTC timestamps, and
  the SHA-256 of every artifact smoked — pass `--tester` / `-Tester`.
  The exact per-OS commands and the A14 evidence templates native runs
  fill are in `docs/native-verification.md`; the runs themselves still
  need real macOS/Windows hosts (#81).

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
- **Release builds: local native route while hosted runners are
  billing-blocked** — route (c) (re-enabled Actions) was decided
  2026-11-14, but the account's payments/spending limit has kept
  GitHub-hosted runners from starting (the v0.2.0 Release run failed in
  seconds), so workflows cannot currently build or gate anything. The
  authorized route meanwhile (decided 2026-11-15, #80) is local native
  builds per OS — Linux on any docker machine, macOS/Windows on real
  hosts — through the same gates and one combined `SHA256SUMS`. What
  remains a limitation either way: only the Linux column is validated
  on the maintainer's own machines at this tree; macOS and Windows
  need physical test hosts (#81). Artifact attestations are not
  available on any route until an executing workflow and an eligible
  repository plan exist (review S6); `SHA256SUMS` is the integrity
  baseline.
- **Backups are manual and never carry credentials** — versioned
  backup/restore is shipped in 0.2.0 (Settings → Storage & backup);
  what remains a limitation: backups run only when you start one (no
  automatic schedule), and passwords/API keys are deliberately never
  backed up, so an account needs reconnecting after a restore when
  this machine's keyring lacks its credentials.
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

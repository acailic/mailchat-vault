# Releases, installers & smoke testing

How mailchat-vault gets packaged, what the artifacts are, and the basic
install smoke test to run before publishing a release.

## What a release contains

Pushing a version tag (`v*`, e.g. `v0.1.0`, matching `version` in
`src-tauri/tauri.conf.json` and the workspace `Cargo.toml`) triggers
[`.github/workflows/release.yml`](../.github/workflows/release.yml). It
builds a **draft** GitHub Release with:

| Platform | Runner | Artifacts |
| --- | --- | --- |
| Linux x86_64 | `ubuntu-22.04` (oldest supported LTS) | `mailchat-vault_<v>_amd64.AppImage`, `mailchat-vault_<v>_amd64.deb` |
| macOS (universal) | `macos-latest` | `mailchat-vault_<v>_universal.dmg`, `.app` tarball |
| Windows x64 | `windows-latest` | `mailchat-vault_<v>_x64_en-US.msi`, NSIS `.exe` setup |

The release stays a **draft** until someone runs the smoke test below and
publishes it. The macOS dmg is additionally smoke-tested automatically
(see [macOS](#macos)): the release workflow mounts it, copies the app
out, and runs `--version`/`--help` before attaching it, and the same test
runs on every push/PR via the `macos-dmg-smoke` job in
[`.github/workflows/ci.yml`](../.github/workflows/ci.yml). The bundles
are also attached as workflow-run artifacts (`linux-bundles`,
`macos-bundles`, `windows-bundles`) for inspection before publishing.
Version bump flow: raise `version` in `src-tauri/tauri.conf.json` and the
workspace `Cargo.toml`, commit, tag `v<version>`, push the tag.

> **Honest status (2026-11-15, per D11 — tags and workflow YAML are not
> evidence):** route (c) was decided on 2026-11-14 (PR #85): CI and Release
> workflows are **enabled** again, and the `v0.2.0` tag (commit `71ac2d4`,
> on `main`) was pushed to build all three platforms onto a draft release.
> The Release run on that tag
> ([43538226337](https://github.com/acailic/mailchat-vault/actions/runs/43538226337))
> failed in seconds — **GitHub-hosted runners cannot start because recent
> account payments failed or the spending limit needs increasing** (see
> Billing & plans in the account settings), and every CI and Retrieval run
> since re-enabling fails the same way. **No v0.2.0 release or draft
> exists**, so `SHA256SUMS` at publish time and smoke gates against
> published artifacts have not happened. Publishing waits on the billing
> fix — the exact handoff is the
> [v0.2.0 publish checklist](#v020-publish-handoff-maintainer-checklist)
> below. Until then the workflow above is again the documented path, and
> the local route (build per platform: `scripts/release/build-linux.sh` on
> Linux; `npm run tauri build -- --bundles app,dmg` on macOS;
> `npm run tauri build -- --bundles msi,nsis` on Windows; publish with
> `gh release create/upload`, gated on the local smoke suites + checksums)
> remains the fallback a maintainer can run without Actions.

The same packaging can be built locally without any host toolchain
(besides docker) with:

```sh
scripts/release/build-linux.sh   # AppImage + deb in target/release/bundle
```

Bundles land in `target/release/bundle/` (workspace root).

On macOS/Windows (or with the host toolchain installed), the same
packaging builds directly:

```sh
npm run tauri build -- --bundles app,dmg    # e.g. on macOS
npm run tauri build -- --bundles msi,nsis   # e.g. on Windows
```

Bundles land under the workspace `target/release/bundle/` (or
`target/universal-apple-darwin/release/bundle/` for a `--target
universal-apple-darwin` build).

### Desktop integration (Linux)

Both Linux artifacts carry the freedesktop integration a launcher
needs:

- **deb** — `/usr/share/applications/mailchat-vault.desktop`
  (`Type=Application`, `Terminal=false` → launches from the menu/dock
  with no terminal), hicolor icons under
  `/usr/share/icons/hicolor/<size>/apps/mailchat-vault.png`, and a
  `MimeType=application/mbox` association: the package declares the
  runtime `Depends` (`libwebkit2gtk-4.1-0`, `libgtk-3-0`, …) so
  `apt install ./x.deb` resolves the full runtime itself, and its
  `postinst` (from `src-tauri/packaging/deb-postinst`) refreshes the
  desktop/MIME/icon databases.
- **AppImage** — bundles the app *and* its runtime (webkit2gtk, GTK,
  the WebKit helper processes): it runs on a clean Ubuntu with none of
  those packages installed. The bundled `mailchat-vault.desktop`, icon
  and `.DirIcon` carry the same launcher/MIME metadata for desktop
  integration tools (e.g. AppImageLauncher).

The `.mbox` association makes "open a mailbox archive" work from the
file manager: `.mbox` files (`application/mbox`) offer Mailchat Vault
as the handler (Viewer role; the app imports the file's mail).

## Checksums & verifying a published release

Every release carries a `SHA256SUMS` asset: one `sha256sum -c`-compatible
line per release artifact. Verify a download against it:

```sh
gh release download v0.1.0 -p SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS   # macOS: shasum -a 256 -c SHA256SUMS
```

To verify (and optionally checksum/smoke) a **whole published release** in
one command — downloading every asset, checking each file's sha256 against
GitHub's own server-side asset digests so a tampered download cannot pass:

```sh
scripts/release/release-verify.sh v0.1.0              # download + verify
scripts/release/release-verify.sh v0.1.0 --attach --smoke  # + publish SHA256SUMS, run Linux smoke
```

`--attach` uploads the generated `SHA256SUMS` to the release (idempotent,
`--clobber`). Publishing rule: **no release is complete without its
SHA256SUMS asset** (v0.1.0 originally shipped without one; it was attached
retroactively — see the roadmap M6 record).

## Code-signing status (this round: none)

- **Linux** — AppImage and `.deb` need no signing; nothing is signed.
- **macOS** — the `.dmg`/`.app` are **unsigned** (ad-hoc signature only,
  no Developer ID). See the [macOS chapter](#macos) for the Gatekeeper
  first-launch instructions and the notarization plan.
- **Windows** — `.msi`/`.exe` are **unsigned**; SmartScreen will show
  "Windows protected your PC" → *More info* → *Run anyway*.

Adding real signing later means wiring the secrets into the release
workflow (`APPLE_CERTIFICATE` + `APPLE_CERTIFICATE_PASSWORD` +
`APPLE_SIGNING_IDENTITY` + notarization credentials for Developer ID
notarization on macOS; an EV/code-signing certificate for Windows) — for
macOS the exact steps are written out in
[Notarization TODO](#notarization-todo-apple-developer-account-required);
Windows signing stays out of scope.

## macOS

### Artifacts

Each release ships a **universal** `mailchat-vault_<v>_universal.dmg`
and a `.app.tar.gz` (the same app, tarballed — for scriptable installs).
The universal dmg shipped and was confirmed on the `v0.1.0` release, and
the build is guarded: the binary inside is checked to contain **both
`x86_64` and `arm64`** slices, so one dmg runs on Intel and Apple Silicon
Macs alike.

### Install

1. Download `mailchat-vault_<v>_universal.dmg` from the release assets.
2. Open it and drag **Mailchat Vault** to `/Applications`.
3. Get past Gatekeeper once (below), then launch normally — from now on
   it opens like any other app.

### Gatekeeper first launch (unsigned app — required)

The app is **not notarized** this round, so Gatekeeper blocks the first
launch of a downloaded copy with *“… cannot be opened because the
developer cannot be verified”* (or, after an OS update, *“damaged and
can't be opened”*). That is expected. Either:

- **Right-click** the app in `/Applications` → **Open** → **Open** in the
  dialog (once is enough for that copy), or
- clear the quarantine flag Terminal-style:
  ```sh
  xattr -cr /Applications/mailchat-vault.app
  ```

The prompt returns only for a **freshly downloaded** copy (the quarantine
flag is per-download). It disappears entirely once notarization is in —
see the TODO below.

### Automated dmg smoke test

`scripts/dmg-smoke-test.sh` checks a dmg end-to-end headlessly: mount,
copy the `.app` out (the scripted drag), verify the universal slices,
then run `mailchat-vault --version` and `--help` and check exit codes
and output. No install, no window server needed.

```sh
# against a built dmg, pinning the version it must print:
scripts/dmg-smoke-test.sh target/universal-apple-darwin/release/bundle/dmg/mailchat-vault_0.1.0_universal.dmg 0.1.0
# ...or against a downloaded dmg, without pinning the version:
scripts/dmg-smoke-test.sh ~/Downloads/mailchat-vault_0.1.0_universal.dmg
```

It runs in two places automatically:

- the `macos-dmg-smoke` job in `ci.yml` builds the universal dmg on a
  macOS runner and runs the script on every push/PR;
- the `build-macos` job of `release.yml` runs it on the freshly built dmg
  **before** attaching it to the draft release — an unverified dmg never
  reaches a release.

A second, complementary gate covers the *published* artifact end-to-end:
`scripts/release/smoke-macos.sh --release v0.1.0` (or `--dir <dir-with-dmg>`)
mounts the published dmg, checks bundle id/version/universal slices/ad-hoc
signature, copies the app out, clears quarantine and requires the app to
stay alive — see the macOS step of the install smoke test below.

Walk the [UI checklist](#ui-checklist-any-platform-2-minutes) manually
once per release on top of that.

### Known limitations

- **Unsigned / not notarized** — every fresh download hits the Gatekeeper
  prompt above until
  [notarization](#notarization-todo-apple-developer-account-required)
  lands. Nothing about the app itself is broken; the signature is ad-hoc.
- **No auto-update** — new releases are a fresh dmg download; there is no
  in-app updater.
- The smoke test exercises launch + CLI, not the full UI; the manual UI
  checklist is still the human gate before publishing.

### Notarization TODO (Apple Developer account required)

Notarization is **blocked on an Apple Developer Program membership**
(~US$99/year, one account for the team) — that purchase is a maintainer
decision, deliberately outside this codebase. Once the account exists,
the wiring is exactly this — no code changes needed:

1. **Certificates**: in the developer portal (or Xcode → Settings →
   Accounts → Manage Certificates) create a **Developer ID Application**
   certificate; export it as a `.p12` with a password.
2. **Notary credentials**: create an app-specific password at
   appleid.apple.com for `notarytool` (used with the team's Apple ID).
3. **Repo secrets** (Settings → Secrets and variables → Actions):
   - `APPLE_CERTIFICATE` — the `.p12`, base64: `base64 -i cert.p12 | pbcopy`
   - `APPLE_CERTIFICATE_PASSWORD` — the `.p12` export password
   - `APPLE_SIGNING_IDENTITY` — e.g. `Developer ID Application: Name (TEAMID)`
   - `APPLE_ID` — the team Apple ID
   - `APPLE_PASSWORD` — the app-specific password from step 2
   - `APPLE_TEAM_ID`
4. **Expose them to the `build-macos` job** in `release.yml` as `env:`
   entries. The Tauri bundler/tauri-action read exactly these names:
   with them present it signs with the Developer ID, submits to Apple's
   notary service, and staples the ticket automatically.
5. **Verify**: `spctl -a -vv -t exec` on the shipped binary should say
   `accepted, Notarized Developer ID`; a downloaded dmg then opens with
   no Gatekeeper prompt.
6. **Docs**: drop the Gatekeeper section above, update the release-notes
   body in `release.yml`, and note the minimum supported macOS version.

### SmartScreen on Windows — the full story

The installers are unsigned, so Windows treats every downloaded copy as
"unrecognized". What the user sees and exactly what to click:

1. **Download** — the browser may warn first: *“mailchat-vault_…msi isn't
   commonly downloaded”* (Edge) or a similar line in Chrome. Choose
   **Keep** / **Keep anyway**. This is a browser reputation warning, not
   SmartScreen itself.
2. **Before the installer runs** — a blue full-screen dialog:
   *“Windows protected your PC — Microsoft Defender SmartScreen prevented
   an unrecognized app from starting.”* Click **More info** (the single
   line under *Run anyway*'s place), which reveals **Run anyway**; click
   it. For the `.msi` and the NSIS `.exe` the flow is identical.
3. **If the dialog only offers *Don't run*** and no *More info* appears,
   the file is fully blocked by policy — clear the Mark-of-the-Web first:
   right-click the downloaded file → **Properties** → tick **Unblock** →
   *OK* (or in PowerShell: `Unblock-File .\mailchat-vault_…_x64-setup.exe`),
   then run it again.
4. **After install** — the installed app itself is unsigned too, so the
   *first launch* can raise the same SmartScreen dialog once more; same
   answer (*More info* → *Run anyway*). Subsequent launches are quiet
   once Windows records the choice.

Why this happens: no code-signing certificate exists in this round, and
SmartScreen additionally builds reputation per signed identity — an
unsigned binary starts from zero either way. The long-term fix is wiring
an EV certificate (or Azure Trusted Signing) into the release workflow;
until then, the click path above is expected behavior, **not** a defect.

## Automated Windows smoke (M6.2)

Before publishing any release's Windows assets, run the **Windows smoke**
workflow (Actions → *Windows smoke* → *Run workflow*): on a real
`windows-latest` runner it installs through **both** installers (`.msi`
and NSIS `.exe`), launches the app, uninstalls, and performs an
**in-place upgrade from the previous release** seeded with a frozen
archive fixture — asserting the archive survives the upgrade intact
and the uninstallers never delete user data. Details, contract and
nuances: [`tests/windows-smoke/README.md`](../tests/windows-smoke/README.md).

Publish a release only when this workflow is green on the tag being
released (in addition to the manual Linux smoke below).

**Local route (no CI needed):** on any Windows box with `gh` authenticated
and the WebView2 runtime, the same gate runs against the *published*
installers in one command:

```pwsh
pwsh scripts/release/windows-smoke-from-release.ps1 -Tag v0.1.0 -AllowDataWipe
# upgrade phases (needs cargo for the digest tool):
pwsh scripts/release/windows-smoke-from-release.ps1 -Tag v0.1.1 -OldTag v0.1.0 -BuildInspect -AllowDataWipe
```

It downloads the release's `.msi`/`.exe` and drives
`scripts/windows-smoke.ps1` (fresh install → launch → vault.db → uninstall
carries the archive) against exactly the bytes users download.

## Preparing a release (local draft — nothing published)

`scripts/release/prepare-release.sh` assembles the complete local
draft-release candidate for the version named in
`src-tauri/tauri.conf.json` (or `--version X.Y.Z`): it verifies the
version is bumped consistently (tauri.conf.json, workspace `Cargo.toml`,
`package.json`, `Cargo.lock`) and that no `v<version>` tag exists yet,
runs this platform's full validation matrix column, builds the Linux
bundles, and writes into `target/release/draft-release/v<version>/`:

- the release assets it could build on this host,
- `SHA256SUMS` over all matching-version assets in the draft dir,
  including assets copied in from other hosts under the real-hosts
  flow,
- `RELEASE-NOTES.md` — the version's [`CHANGELOG.md`](../CHANGELOG.md)
  section plus the standing unsigned-artifacts disclosure,
- `PUBLISH-CHECKLIST.md` — the remaining tag/publish/smoke steps in
  gate order, waiting on the maintainer's host/Actions decision.

It never tags, never calls `gh release`, and never touches Actions:
publishing stays a separate human decision by design. `--skip-matrix`
and `--skip-bundles` exist for iteration only — a candidate is never
certified from a run that used them.

Version bump flow: raise `version` in `src-tauri/tauri.conf.json` and the
workspace `Cargo.toml` (both crates and `Cargo.lock` follow),
matching `package.json`, add the `CHANGELOG.md` section, then run the
prep script; tag `v<version>` and push only at the publish gate.

## v0.2.0 publish handoff (maintainer checklist)

The tag exists; the release does not. Recorded state, verified 2026-11-15:
tag `v0.2.0` at commit `71ac2d4` (on `main`, documented route (c) in PR
#85); Release run
[43538226337](https://github.com/acailic/mailchat-vault/actions/runs/43538226337)
on that tag failed because hosted runners cannot start (account payments /
spending limit); every CI and Retrieval run since re-enabling fails the
same way; no v0.2.0 release or draft exists. Per D11, none of this counts
as release evidence — the steps below finish it once hosted runners are
unblocked. Publishing waits on them; do not tag or re-tag anything else in
the meantime.

1. **Unblock hosted runners (account owner action, not a repo setting).**
   GitHub account → *Settings* → *Billing & plans*: fix the failed payment
   method and/or raise the spending limit so Actions can start jobs again.
   Verify with any workflow run (e.g. the CI run on `main`): it must get
   past job start instead of failing in seconds with the payments
   annotation.
2. **Re-run the failed Release run on the tag** — Actions → *Release* →
   run 43538226337 → *Re-run all jobs* (the `create-release` job is
   idempotent; it drafts the release if none exists). If re-run is not
   available, delete and re-push the tag at the same commit:
   `git push origin :refs/tags/v0.2.0 && git tag v0.2.0 71ac2d4 && git
   push origin v0.2.0`. The tag currently points at `71ac2d4`; moving it
   to a newer `main` commit is a maintainer decision that must be recorded
   here if taken.
3. **Verify the draft.** `gh release view v0.2.0` shows a **draft** with
   all six assets (Linux AppImage + `.deb`, macOS universal `.dmg` +
   `.app.tar.gz`, Windows `.msi` + NSIS `.exe`); the in-workflow gates are
   green: `smoke-linux` on clean 22.04 **and** 24.04, and the dmg smoke
   that runs before the dmg is attached. Run the manually dispatched
   **Windows smoke** workflow (Actions → *Windows smoke* → *Run workflow*)
   on the tag being released and require green — it covers install,
   launch, uninstall-keeps-data, and the in-place upgrade from v0.1.0.
4. **Attach `SHA256SUMS` at publish time.** From a checkout of the tag:
   `scripts/release/release-verify.sh v0.2.0 --attach --smoke` — it
   downloads the draft's assets, verifies each against GitHub's
   server-side asset digests, uploads `SHA256SUMS` (idempotent), and can
   chain the Linux smoke against the published bytes. Publishing rule: no
   release is complete without its `SHA256SUMS` asset (v0.1.0 shipped
   without one; it was attached retroactively — do not repeat that).
5. **Smoke against the published artifacts, or label honestly.** After
   publishing: `release-verify.sh v0.2.0 --smoke` (Linux),
   `pwsh scripts/release/windows-smoke-from-release.ps1 -Tag v0.2.0
   -OldTag v0.1.0 -BuildInspect -AllowDataWipe` (Windows), and
   `scripts/release/smoke-macos.sh --release v0.2.0` (macOS, needs a Mac).
   If any of these gates cannot run against the published artifacts,
   either hold the publish or explicitly label the release a **development
   build** in its title/notes — under D11 an executed gate or an honest
   label are the only acceptable states.
6. **Close the loop in docs.** Update the [download page](download.md)
   release-status section, the [support matrix](support-matrix.md) row for
   the execution route (#80), and the M6 record in the
   [implementation roadmap](implementation-roadmap.md) with the published
   evidence; the v0.2.0 CHANGELOG section already reflects route (c).

## App icon

The icon set (`src-tauri/icons/`: PNGs for Linux, `icon.icns` for macOS,
`icon.ico` for Windows) is generated from the committed master
`src-tauri/icons/source.png` (1024×1024, transparency). To regenerate after
editing the source:

```sh
npx tauri icon src-tauri/icons/source.png
```

(The mobile/Store extras that command also emits are not committed; the
committed set is what `bundle.icon` lists.)

## Install smoke test (run before publishing a draft)

The basic contract: the package **installs**, the app **launches**, and the
core v0 flow (demo data → search → open a message) **works**.

### One-command validation matrix (M6.4)

The smoke sections below are the individual gates; the release
validation matrix assembles the whole column per platform — full test
suite, release bundles, and the smoke gates against those bundles —
into one command each:

```sh
scripts/release/matrix-linux.sh              # Linux column (docker only)
scripts/release/matrix-macos.sh               # on a real macOS host
pwsh scripts/release/matrix-windows.ps1       # on a real Windows host
```

The Linux command is the complete, reproducible Linux column in one
shot (suite/e2e/bundles/smoke legs, each pinned in its own container).
The macOS/Windows commands are ready and waiting for their hosts — see
[the matrix and its recorded status](validation-and-release.md#release-validation-matrix-m64).
Run the matrix (or at minimum every gate below) on every candidate
before publishing; a leg skipped with a `--skip-*`/`-Skip*` flag is
never certification.

### Linux — automated smoke (22.04 + 24.04)

The Linux acceptance gate is automated (M6.3); on any machine with
docker:

```sh
scripts/release/build-linux.sh    # if bundles are not built yet
scripts/release/smoke-linux.sh    # deb + AppImage on clean 22.04 and 24.04
```

Each suite runs in a pristine `ubuntu:22.04` / `ubuntu:24.04`
container that ships **none of the application libraries** (no
webkit2gtk, no GTK). What is verified per artifact:

- **deb** — `apt install ./x.deb` resolves the declared runtime by
  itself; `dpkg` status installed; `/usr/bin/mailchat-vault` present;
  desktop file valid (`desktop-file-validate`) with
  `Type=Application`/`Terminal=false`, owned by the package; hicolor
  icons installed and owned by the package; `MimeType=application/mbox`
  declared **and** registered in `mimeinfo.cache` by postinst;
  `xdg-mime query default application/mbox` resolves to
  `mailchat-vault.desktop`; the app launched the way a launcher does
  (Exec line, no terminal, under X) maps a window and stays alive;
  `apt remove` takes the desktop file, icons and binary with it.
- **AppImage** — extracts (`--appimage-extract`) with the desktop
  entry (same fields), icon, `.DirIcon` and the WebKit helper processes
  inside the bundle; runs via `--appimage-extract-and-run` under Xvfb
  and stays alive **with zero app packages on the system** — the
  AppImage is self-contained (that is the "clean 22.04" acceptance:
  nothing beyond what a desktop session itself provides).

The smoke containers only add the pieces a real desktop always has
(X display via Xvfb, GL software rendering, freedesktop tools) — see
`scripts/release/Dockerfile.smoke`. They run with
`--security-opt seccomp=unconfined` because Ubuntu 24.04's WebKitGTK
uses a bubblewrap sandbox that needs unprivileged user namespaces,
which docker's default seccomp profile blocks; this relaxes only the
ephemeral test container.

A per-suite PASS/FAIL summary prints at the end; logs land in
`target/release/smoke-logs/`. The same script is wired into
`.github/workflows/release.yml` (`smoke-linux` job) for when workflow
runs are re-enabled; publish a release only with a green run (or a
green local run of both suites on both versions).

### Linux `.deb` (manual, on a real desktop)

The automated smoke above covers install/launch/integration on clean
containers; on a real desktop session you can additionally walk the UI
checklist below. Manual install:

```sh
# install (apt resolves the declared runtime: libwebkit2gtk-4.1 etc.)
sudo apt install ./mailchat-vault_0.1.0_amd64.deb
dpkg -s mailchat-vault | grep Status        # → "install ok installed"
command -v mailchat-vault                   # → /usr/bin/mailchat-vault
ls /usr/share/applications/mailchat-vault.desktop   # desktop entry exists

# headless launch check (no display needed): the app must stay alive 20 s
xvfb-run -a timeout 20s mailchat-vault; echo "exit=$?"
# exit=124 (timeout had to kill it) == PASS; any other exit = crash
```

Then in a desktop session launch “Mailchat Vault” from the
applications menu (no terminal) and walk the UI checklist below.
Uninstall with `sudo apt remove mailchat-vault`.

### Linux AppImage (manual)

```sh
chmod +x mailchat-vault_0.1.0_amd64.AppImage

# headless check; --appimage-extract-and-run avoids needing FUSE
xvfb-run -a timeout 20s ./mailchat-vault_0.1.0_amd64.AppImage --appimage-extract-and-run; echo "exit=$?"
# exit=124 == PASS

# desktop use: just run it (needs FUSE) or keep --appimage-extract-and-run
./mailchat-vault_0.1.0_amd64.AppImage
```

### UI checklist (any platform, ~2 minutes)

1. Launch the app → the “Mailchat Vault” window opens with the icon in the
   taskbar/dock.
2. Click **Try a sample archive** → the sample banner appears and the stats
   line shows sample messages; leaving the sample restores the empty archive.
3. Search `vault` → results appear with highlighted matches in a flat list.
4. Click a result → the detail pane opens; `↑`/`↓` walk the conversation context; `Esc` closes.
5. Quit normally (window close) → no error dialogs.

### macOS

Automated gates — two complementary scripts, run on a macOS machine:

- `scripts/dmg-smoke-test.sh <dmg>` — headless CLI-answer checks: mount,
  copy the `.app` out, verify the universal slices, then `--version` and
  `--help` with exit codes and output (see
  [macOS → Automated dmg smoke test](#automated-dmg-smoke-test)); runs
  automatically in CI and in the release workflow before attaching the dmg.
- `scripts/release/smoke-macos.sh --release v0.1.0   # or --dir <dir-with-dmg>` —
  the published-artifact gate (M6.2 macOS half): mounts the **published**
  `.dmg`, checks the `.app` (bundle id, version, universal arm64+x86_64
  slices, ad-hoc signature present), copies it out the way a user does,
  clears the quarantine attribute (`xattr -cr` — the documented Gatekeeper
  path), launches it and requires it to stay alive. Honest record: not yet
  executed — no macOS host this round.

Manual route: open the `.dmg`, drag the app to `/Applications`, clear the
quarantine attribute (`xattr -cr` above, or right-click → *Open* once), then
run the UI checklist.

### Windows

Run the `.msi` (or the NSIS `.exe`) → *Next/Install* → launch “Mailchat
Vault” from the Start menu (past the SmartScreen prompt — see the
[SmartScreen section](#smartscreen-on-windows--the-full-story) for the
exact click path) → run the UI checklist.

For an automated run of the install → launch → uninstall cycle on a
GitHub Windows runner — plus the upgrade-from-previous-release archive
check — use the **Windows smoke** workflow instead; see
[Automated Windows smoke](#automated-windows-smoke-m62) above.

Publish the release only when the smoke test passed on at least the Linux
artifacts (that is the platform this round was validated on), and the
**Windows smoke** workflow is green when the release ships Windows
assets. The macOS dmg is already smoke-tested automatically by the
release workflow (see [macOS](#macos)); walking its UI checklist on a
Mac is the remaining manual step before publishing it.

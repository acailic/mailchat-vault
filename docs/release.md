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
publishes it. The bundles are also attached as workflow-run artifacts
(`linux-bundles`, `macos-bundles`, `windows-bundles`) for inspection before
publishing. Version bump flow: raise `version` in `src-tauri/tauri.conf.json`
and the workspace `Cargo.toml`, commit, tag `v<version>`, push the tag.

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

## Code-signing status (this round: none)

- **Linux** — AppImage and `.deb` need no signing; nothing is signed.
- **macOS** — the `.dmg`/`.app` are **unsigned** (ad-hoc signature only, no
  Developer ID). On first launch Gatekeeper blocks a downloaded app:
  right-click → *Open*, or after moving it to `/Applications`:
  ```sh
  xattr -cr /Applications/mailchat-vault.app
  ```
- **Windows** — `.msi`/`.exe` are **unsigned**; SmartScreen will show
  "Windows protected your PC" → *More info* → *Run anyway*.

Adding real signing later means wiring the secrets into the release
workflow (`APPLE_CERTIFICATE` + `APPLE_CERTIFICATE_PASSWORD` +
`APPLE_SIGNING_IDENTITY` for Developer ID notarization; an EV/code-signing
certificate for Windows) — intentionally out of scope here.

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
2. Click **Load demo** → the stats line shows demo messages.
3. Search `vault` → results appear with highlighted matches in a flat list.
4. Click a result → the detail pane opens; `↑`/`↓` walk the conversation context; `Esc` closes.
5. Quit normally (window close) → no error dialogs.

### macOS

Open the `.dmg`, drag the app to `/Applications`, clear the quarantine
attribute (`xattr -cr` above, or right-click → *Open* once), then run the UI
checklist.

### Windows

Run the `.msi` (or the NSIS `.exe`) → *Next/Install* → launch “Mailchat
Vault” from the Start menu (past the SmartScreen prompt) → run the UI
checklist.

Publish the release only when the smoke test passed on at least the Linux
artifacts (that is the platform this round was validated on).

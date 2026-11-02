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

The same packaging can be built locally with:

```sh
npm run tauri build                     # uses the default targets: appimage + deb
npm run tauri build -- --bundles app,dmg    # e.g. on macOS
npm run tauri build -- --bundles msi,nsis   # e.g. on Windows
```

Bundles land in `src-tauri/target/release/bundle/`.

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

### Linux `.deb`

```sh
# install (apt resolves libwebkit2gtk-4.1 runtime automatically)
sudo apt install ./mailchat-vault_0.1.0_amd64.deb
dpkg -s mailchat-vault | grep Status        # → "install ok installed"
command -v mailchat-vault                   # → /usr/bin/mailchat-vault
ls /usr/share/applications/mailchat-vault.desktop   # desktop entry exists

# headless launch check (no display needed): the app must stay alive 20 s
xvfb-run -a timeout 20s mailchat-vault; echo "exit=$?"
# exit=124 (timeout had to kill it) == PASS; any other exit = crash
```

Then in a desktop session walk the UI checklist below. Uninstall with
`sudo apt remove mailchat-vault`.

### Linux AppImage

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
3. Search `vault` → results appear with highlighted matches, grouped by chat.
4. Click a result → the detail pane opens; `↑`/`↓` walk thread neighbors; `Esc` closes.
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

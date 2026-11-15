# Download & install

The canonical download reference for Mailchat Vault: which installers exist
for which version, OS, and architecture; what a machine needs; how to verify
a download; the stated limitations; and how to upgrade manually. This page
agrees with the [README](../README.md), the [CHANGELOG](../CHANGELOG.md),
and each release page. The maintainer-side build, smoke, and publish
procedure lives in [release.md](release.md).

## Downloads

Releases are published at
<https://github.com/acailic/mailchat-vault/releases>.

### Version × OS × architecture matrix

The current published release is **v0.1.0**. Every release ships the same
asset set on all three platforms, and every release carries a `SHA256SUMS`
asset (see [verify your download](#verify-your-download)):

| Version | OS | Architecture | Assets |
| --- | --- | --- | --- |
| v0.1.0 | Linux | x86_64 | `mailchat-vault_0.1.0_amd64.deb` — apt package, resolves its own runtime dependencies; `mailchat-vault_0.1.0_amd64.AppImage` — self-contained, bundles the app and its runtime (incl. WebKitGTK) |
| v0.1.0 | macOS | universal — Apple Silicon (arm64) + Intel (x86_64) | `mailchat-vault_0.1.0_universal.dmg` — one dmg runs on both Mac kinds (the build is verified to contain both slices); `mailchat-vault.app.tar.gz` — the same `.app`, tarballed for scriptable installs |
| v0.1.0 | Windows | x64 | `mailchat-vault_0.1.0_x64_en-US.msi` (WiX); `mailchat-vault_0.1.0_x64-setup.exe` (NSIS setup) |

### Release status

**v0.2.0 is tagged but not yet released.** The tag `v0.2.0` (commit
`71ac2d4`, on `main`) exists and the decided release route — route (c),
2026-11-14: CI and Release workflows enabled on GitHub-hosted runners — is
in place, but the Release run on that tag failed because hosted runners
currently cannot start (an account payments/spending-limit failure), so no
v0.2.0 release or draft exists. Publishing waits on that billing fix; the
maintainer handoff is the
[publish checklist in release.md](release.md#v020-publish-handoff-maintainer-checklist).
Until a v0.2.0 release is published and smoke-gated, **v0.1.0 is the only
installable release**. The honest per-release evidence record lives in
[release.md](release.md) and the [support matrix](support-matrix.md).

## Minimum requirements

For binary installs (no development toolchain needed):

| Platform | Requirements |
| --- | --- |
| Linux | 64-bit; verified on clean Ubuntu 22.04 and 24.04 desktops. The `.deb` declares its runtime (WebKitGTK 4.1, GTK 3, …) so `apt install ./x.deb` resolves everything; the AppImage runs on a clean install with none of those packages (use `--appimage-extract-and-run` if you have no FUSE). |
| macOS | Intel or Apple Silicon Macs — the dmg is universal. Unsigned (ad-hoc signature only): the first launch of a downloaded copy needs the one-time Gatekeeper step in [limitations](#limitations) below. |
| Windows | 64-bit Windows with the WebView2 runtime (preinstalled on current Windows 10/11 installs). Unsigned: the installer and first launch show the SmartScreen path in [limitations](#limitations). |

For source builds, additionally: [Node.js](https://nodejs.org) 22+, Rust
stable (via [rustup](https://rustup.rs)), the platform toolchain (Xcode on
macOS, MSVC on Windows), and on Ubuntu/Debian the Tauri system libraries —
see [build from source](#build-from-source).

## Build from source

```sh
npm install
npm run tauri dev
```

Prerequisites and the Tauri system packages for Ubuntu/Debian are listed in
the [README quickstart](../README.md#quickstart); the full local-build
routes (including release bundles per platform) are in
[release.md](release.md#what-a-release-contains).

## Verify your download

Every release carries a `SHA256SUMS` asset — one `sha256sum -c`-compatible
line per release artifact. After downloading an installer (and
`SHA256SUMS`), verify it:

```sh
gh release download v0.1.0 -p SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS   # macOS: shasum -a 256 -c SHA256SUMS
```

To verify a whole published release in one command — downloading every asset
and checking each file's sha256 against GitHub's own server-side asset
digests, so a tampered download cannot pass:

```sh
scripts/release/release-verify.sh v0.1.0
```

## Limitations

Stated honestly (see the [support matrix](support-matrix.md) for the full
status and evidence record):

- **Artifacts are unsigned.** Linux needs no signing. On macOS, Gatekeeper
  blocks the first launch of a downloaded copy — right-click the app →
  *Open* → *Open*, or `xattr -cr /Applications/mailchat-vault.app`. On
  Windows, SmartScreen shows *“Windows protected your PC”* → *More info* →
  *Run anyway*. The exact click paths are in
  [release.md](release.md#code-signing-status-this-round-none).
- **No auto-update.** There is no in-app updater; upgrades are a fresh
  download over the old install — see [upgrading](#upgrading).
- **No app-level encryption.** The app does not encrypt the archive or
  backups itself; at rest they rely on your OS account and disk protection.

## Upgrading

Auto-update is deferred by decision
([D11](decisions.md#d11-three-platform-release-with-honest-evidence)), so
upgrading is manual: download the new version's installer and install it
over the old one.

- **Linux** — `.deb`: `sudo apt install ./mailchat-vault_<v>_amd64.deb`
  over the previous install. AppImage: replace the old file with the new
  one.
- **macOS** — open the new `.dmg` and drag **Mailchat Vault** over the old
  app in `/Applications` (the Gatekeeper step applies to each freshly
  downloaded copy).
- **Windows** — run the new `.msi` or `setup.exe` over the previous
  install.

Your archive survives in place: the vault database lives in the app data
dir (e.g. `~/.local/share/com.acailic.mailchatvault/vault.db` on Linux) and
upgrades run snapshot-backed migrations on it — the release upgrade gate
asserts that an archive seeded on the previous release comes through an
in-place upgrade with identical content, and that uninstalling never
deletes user data. An older app version refuses a newer vault database by
design (migrations roll back on failure), so after upgrading, going back to
an older binary means restoring a backup made before the upgrade, not
reopening the upgraded vault.

## Support

- **Security reports** go through [SECURITY.md](../SECURITY.md), never
  public issues.
- **Bugs and feature requests**:
  [GitHub issues](https://github.com/acailic/mailchat-vault/issues).
- **Documentation**: the [docs index](README.md) and the
  [feature reference](features.md); release and packaging details are in
  [release.md](release.md).

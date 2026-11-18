# Distribution: package managers

The maintainer-side kit for publishing Mailchat Vault through package
managers (Homebrew, winget, Scoop, AUR, Flatpak): what exists in the
repository today, the one-command version bump, and the exact owner
steps each manager needs. **Nothing is published from this repository
yet, and nothing here submits anything** — every submission is an owner
action with owner-held credentials (see [what cannot be done from
CI](#what-cannot-be-done-from-ci)).

People search package managers before search engines; a presence there
signals maturity. The gate in front of that is signing: **issue #78
(owner signing)**. Until it lands, every artifact this kit would point
at is unsigned and trips the OS warnings users otherwise hit only on a
manual download (the exact click paths are in
[download.md](download.md#limitations)). Publishing a manifest that
silently installs a warned artifact is worse than not publishing, so
every manifest ships with a leading **BLOCKED — DO NOT SUBMIT until
signing lands (#78)** note, and the rewrite script never removes those
notes.

## The kit

| Path | Manager | Shape |
| --- | --- | --- |
| `packaging/manifests/homebrew/mailchat-vault.rb` | Homebrew | cask (dmg), token `mailchat-vault` |
| `packaging/manifests/winget/acailic.mailchat-vault*.yaml` | winget | three-file manifest set per the winget-pkgs schema (`version`, `installer`, `defaultLocale`), `PackageIdentifier: acailic.mailchat-vault` |
| `packaging/manifests/scoop/mailchat-vault.json` | Scoop | JSON manifest against the x64 MSI |
| `packaging/manifests/aur/PKGBUILD` | AUR | binary-release PKGBUILD, package `mailchat-vault-bin` |
| `packaging/manifests/flatpak/com.acailic.mailchatvault.json` | Flathub | flatpak-builder JSON stub, app ID `com.acailic.mailchatvault` (the Tauri identifier) |
| `scripts/update-manifests.sh` | all | version + sha256 rewriter (below) |

Every manifest carries placeholder `0.0.0` / 64-zero checksum fields.
The manifests deliberately do **not** live under `dist/` — that is the
vite build output the Tauri bundle embeds — and the App ID is the
existing `com.acailic.mailchatvault` from `src-tauri/tauri.conf.json`,
not a GitHub-flavored ID.

## Release-day flow (version bump)

On every release — after the `SHA256SUMS` asset exists (the file
`scripts/release/prepare-release.sh` writes; see
[release.md](release.md)):

```sh
scripts/update-manifests.sh --from-release v0.2.0        # dry run: prints a unified diff, changes nothing
scripts/update-manifests.sh --from-release v0.2.0 --write  # apply
# or, with a local SHA256SUMS:
scripts/update-manifests.sh --version 0.2.0 --checksums target/release/draft-release/v0.2.0/SHA256SUMS
```

The script rewrites the version and the three checksums the kit
consumes — universal dmg (Homebrew), x64 MSI (winget, Scoop), amd64
deb (AUR, Flatpak stub) — in **all** manifests atomically (every new
file content is rendered to a temp file; originals move into place
only after every render succeeded). Review the diff, commit, and open
a PR; that PR is the publication review. The BLOCKED notes survive the
rewrite untouched.

The checks run automatically on every PR:

- `npm test` runs `tests/packaging/manifests.test.ts` — parse
  validation for every manifest (JSON parse for Scoop/Flatpak, YAML
  parse for winget, `bash -n` for the PKGBUILD, structural assertions
  for the cask) plus the script smoke test (dry run prints a diff and
  changes nothing; `--write` on a temp copy rewrites version and
  sha256 everywhere).
- `.github/workflows/ci.yml` is meant to carry a small `manifests`
  job running the same spec. **Two disclaimers**: it is unwired until
  Actions runs again (billing; the CI-debt record is issue #102) — per
  the repository's evidence rule, workflow YAML is not evidence, and
  the job only counts after a green run post-repair; and the change
  that landed this kit could not push workflow files itself (the
  automation token lacks `workflows` permission), so the job's patch
  rides along with the landing PR as an owner-applied follow-up. Until
  then the local spec is the gate.

## Owner publication steps, per manager

None of these can run from CI; each needs an owner-held account or
credential. All of them are **gated on #78 landing signed artifacts**
first. Order is a suggestion; each is independent.

### Homebrew (cask)

1. Sign-off that #78 landed: the dmg is signed + notarized, and the
   BLOCKED note can go.
2. `brew audit --cask mailchat-vault --online` and `brew style` on a
   local copy of the cask (real validation; the kit's structural
   check is not an audit substitute).
3. Fork [Homebrew/homebrew-cask](https://github.com/Homebrew/homebrew-cask),
   add `Casks/mailchat-vault.rb`, open a PR. The tap's maintainers
   review; the first submission usually triggers a discussion of the
   signing/notarization status.
4. Later releases: same script bump, same PR flow (homebrew-cask
   botically flags stale versions; the update PR is small).

### winget (winget-pkgs)

1. #78 gate, as above, for the MSI.
2. Validate locally with the
   [winget-create](https://github.com/microsoft/winget-create) /
   `winget validate` tooling against the three yaml files.
3. Fork [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs),
   place the three manifests under
   `manifests/a/acailic/mailchat-vault/<version>/`, open a PR.
   Merging normally goes through the winget-pkgs bot pipeline; the
   one-time publisher verification (Azure DevOps PAT or a signed
   publisher assertion) is owner-held.
4. Later releases: same layout under a new `<version>/` directory.

### Scoop

1. #78 gate for the MSI.
2. Decide the channel: an owner-maintained bucket
   (e.g. `acailic/scoop-bucket`) is the honest first step — Extras
   (the main community bucket) has its own review bar and maintainer
   sponsorship requirements. Plan for the owner bucket.
3. Create the bucket repo, copy `mailchat-vault.json` in, verify with
   `scoop install`/`scoop checkup` on a Windows host.
4. Later releases: script bump + commit to the bucket repo (this one
   *can* be automated later — the bucket is first-party — but the
   bucket repo itself is owner infrastructure; it does not exist yet).

### AUR

1. #78 gate for the deb.
2. Create an AUR account (owner-held; SSH key upload required).
3. `git clone ssh://aur@aur.archlinux.org/mailchat-vault-bin.git`,
   copy in `PKGBUILD` (fill the Maintainer line), generate
   `.SRCINFO` (`makepkg --printsrcinfo > .SRCINFO`), commit, push.
4. Validate on an Arch host: `makepkg -si`, `namcap` on the package.
   The PKGBUILD is binary-release based (a `-bin` package) on purpose:
   building the Rust workspace plus the web frontend on user machines
   means tens of minutes of compilation and toolchain churn per
   release — the comment block in the PKGBUILD documents this. A
   source-based `mailchat-vault` package can follow if demand exists.
5. Later releases: script bump, refresh `.SRCINFO`, push.

### Flathub

1. #78 gate, **and** the stub must become a real recipe first: the
   JSON in the kit wires app ID, runtime, and finish-args, with the
   release .deb as a placeholder source so the version/checksum
   plumbing is exercised — it does not yet build the app inside the
   sandbox. Someone must write and verify a `flatpak-builder` recipe
   (the Tauri flatpak packaging guides are the starting point).
2. Create a [Flathub](https://flathub.org) account (owner-held) and
   request the `com.acailic.mailchatvault` app ID through Flathub's
   new-repo process.
3. Submit the finished manifest to the `flathub/<app-id>` repository
   and pass Flathub's review; validate locally with
   `flatpak-builder --force-clean --repo=test-repo build-dir <manifest>`
   before submitting.

## Flatpak finish-args, each permission explained

| Finish-arg | Why the app needs it |
| --- | --- |
| `--socket=wayland` | Show the window on native Wayland sessions (the default on current Linux desktops). |
| `--socket=fallback-x11` | Show the window on X11 sessions and XWayland-only setups. `fallback-x11` (not plain `--socket=x11`) is the narrow form: it grants X11 only when Wayland is unavailable. |
| `--share=network` | IMAP sync (mail fetched read-only from the user's servers) and, after explicit consent, optional AI-provider calls. Everything else works offline; without network the app cannot sync mail at all, so this is not optional. |
| `--filesystem=xdg-download:ro` | The narrowest workable filesystem access for the archive pick: the first-session flow drags an export (Takeout mbox, WhatsApp/Telegram/Signal/Discord exports) in, and those downloads live in `~/Downloads`; flatpak drag-and-drop does not go through the document portal, so read-only sight of `xdg-download` is what makes the drop target work. Read-only, one directory. Watching a folder (folder-watch sources) outside `~/Downloads`, or exporting archives to arbitrary paths, would need a broader grant or a portal-based picker — decide before the real Flathub submission, not after. |

No device, IPC, or session-bus permissions are requested; the app
needs none of them.

## What cannot be done from CI

- **Trusted publishing credentials are owner-held**: the homebrew-cask
  PR (any GitHub account can open it, but landing a *signed, notarized*
  cask and keeping it maintained implies maintainer standing), the
  winget-pkgs publisher verification (Azure DevOps token or signed
  publisher assertion), the AUR SSH key, the Scoop bucket repository,
  and the Flathub account. None of these tokens may live in repository
  CI secrets for this project's threat model; submissions are manual
  by design.
- **GitHub Actions is currently down** (account billing; the CI-debt
  record is issue #102). The `manifests` job in `ci.yml` is written
  but unwired until runners return; per the evidence rule, YAML is not
  evidence — the operative local gate is the vitest spec.
- **Real per-manager validation** (`brew audit`, `winget validate`,
  `namcap`, `flatpak-builder`) needs each manager's toolchain and
  native hosts; the kit's checks are parse/shape checks only.

## Relationship to the release process

This page extends [release.md](release.md) (the build/smoke/publish
procedure and the `SHA256SUMS` format) — it does not change it. The
release execution route decision (#80) governs how artifacts are
produced; this kit consumes published artifacts only. Owner settings
actions outside this repository that #78 or publication may require
(Apple Developer / notary keys, Windows code-signing certificate,
Flathub/AUR accounts) are listed here precisely because they cannot be
part of a repository change.

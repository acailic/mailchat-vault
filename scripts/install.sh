#!/bin/sh
# install.sh — one-command installer/upgrader for Mailchat Vault
# (POSIX sh, Linux + macOS). User-facing docs:
# https://github.com/acailic/mailchat-vault — docs/download.md
# "Install in one line".
#
#   curl -fsSL https://raw.githubusercontent.com/acailic/mailchat-vault/main/scripts/install.sh | sh
#
# Read this script before piping it to a shell — piping is a trust
# decision. What it does:
#
#   1. detects the OS, architecture, and Linux distro family, and checks
#      its own needs (curl or wget, sha256sum or shasum, tar); it needs
#      no gh, Node, or Rust
#   2. resolves the latest PUBLISHED release from the GitHub REST API
#      (unauthenticated; --version vX.Y.Z or $MAILCHAT_VERSION pins a
#      tag instead); draft releases are refused with a clear error
#   3. picks and installs the right asset:
#      - Debian/Ubuntu with apt (and sudo, or root): the .deb via apt,
#        so WebKitGTK and other dependencies resolve themselves;
#        without apt/sudo it falls back to the AppImage
#      - other Linux: the AppImage into ~/.local/bin, plus a .desktop
#        entry and icon, and a --appimage-extract-and-run wrapper when
#        FUSE is missing
#      - macOS: the universal mailchat-vault.app.tar.gz (that asset has
#        no version prefix — matched by suffix) into /Applications, or
#        ~/Applications when that is not writable, then xattr -cr so
#        the first launch skips the Gatekeeper block
#   4. verifies every download against the release's SHA256SUMS BEFORE
#      installing anything; on a mismatch or missing sums it aborts,
#      installs nothing, and removes its temp files
#   5. installs over any previous version (in-place upgrade) and never
#      writes to or removes the vault data dir
#      (~/.local/share/com.acailic.mailchatvault/ on Linux,
#      ~/Library/Application Support/com.acailic.mailchatvault/ on macOS)
#   6. prints where it installed, the version, and how to uninstall,
#      then launches the app (--no-launch skips)
#
# Flags:
#   --version vX.Y.Z   install a specific published, non-draft version
#   --no-launch        do not start the app afterwards
#   --yes              non-interactive (CI): skip the --uninstall prompt;
#                      a sudo password may still be asked where needed
#   --uninstall        remove the app, KEEP all user data
#   -h | --help        show usage
#
# Environment:
#   MAILCHAT_VERSION     same as --version
#   MAILCHAT_OS_RELEASE  os-release file to read instead of /etc/os-release
#   MAILCHAT_FUSE_DEVICE FUSE device to probe instead of /dev/fuse
#                        (both let unusual hosts and the self-check suite
#                        steer platform detection)
#
# Artifacts are unsigned (#78). On macOS this script clears the
# quarantine attribute itself — the honest trade-off is that you are
# trusting this script instead of clicking through Gatekeeper.
#
# Checksum handling and asset naming follow scripts/release/release-verify.sh
# and scripts/release/prepare-release.sh.
set -eu

REPO="acailic/mailchat-vault"
APP="mailchat-vault"
DATA_DIR_LINUX=".local/share/com.acailic.mailchatvault"
DATA_DIR_MACOS="Library/Application Support/com.acailic.mailchatvault"
README_URL="https://github.com/$REPO#readme"

say() { printf '== %s\n' "$*"; }
note() { printf '   %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'USAGE'
usage: scripts/install.sh [--version vX.Y.Z] [--no-launch] [--yes] [--uninstall] [-h|--help]

  --version vX.Y.Z  install a specific published version (or MAILCHAT_VERSION)
  --no-launch       do not launch the app after installing
  --yes             non-interactive (CI): skip confirmation prompts
  --uninstall       remove the app, keep all user data
  -h, --help        show this help

Installs or upgrades the latest published Mailchat Vault release:
  Linux (Debian/Ubuntu):  .deb via apt (dependencies resolve automatically)
  Linux (other):          AppImage into ~/.local/bin (+ .desktop entry, icon)
  macOS:                  universal .app.tar.gz into /Applications
Every download is verified against the release's SHA256SUMS before anything
is installed, and the vault data dir is never touched.

Docs: docs/download.md in https://github.com/acailic/mailchat-vault
USAGE
}

VERSION_PIN=""
NO_LAUNCH=0
ASSUME_YES=0
UNINSTALL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --version)
      [ $# -ge 2 ] || die "--version needs a value (e.g. --version v0.2.0)"
      VERSION_PIN="$2"; shift 2 ;;
    --no-launch) NO_LAUNCH=1; shift ;;
    --yes|-y)    ASSUME_YES=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help)   usage; exit 0 ;;
    *)
      printf 'unknown option: %s\n\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[ -n "${HOME:-}" ] || die "HOME is not set — cannot locate the user profile"

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
  Linux|Darwin) ;;
  *) die "unsupported OS '$OS' — this installer covers Linux and macOS" ;;
esac

data_dir() {
  if [ "$OS" = "Darwin" ]; then
    printf '%s\n' "$HOME/$DATA_DIR_MACOS"
  else
    printf '%s\n' "$HOME/$DATA_DIR_LINUX"
  fi
}

report_data_kept() {
  if [ -d "$(data_dir)" ]; then
    say "user data KEPT (untouched): $(data_dir)"
  else
    say "no user data found at $(data_dir) — and none was deleted"
  fi
}

deb_package_installed() {
  command -v dpkg-query >/dev/null 2>&1 \
    && dpkg-query -W -f '${Status}' "$APP" 2>/dev/null | grep -q 'install ok installed'
}

uninstall() {
  say "uninstalling Mailchat Vault (user data is always kept)"
  if [ "$OS" = "Darwin" ]; then
    found=0
    for dir in "/Applications" "$HOME/Applications"; do
      if [ -d "$dir/$APP.app" ]; then
        rm -rf "$dir/$APP.app"
        say "removed $dir/$APP.app"
        found=1
      fi
    done
    if [ "$found" -eq 0 ]; then
      warn "no installed app found under /Applications or $HOME/Applications"
    fi
    report_data_kept
    note "to also delete your archive, remove $(data_dir) yourself"
    exit 0
  fi
  if deb_package_installed; then
    if [ "$(id -u)" = "0" ]; then
      apt-get remove -y "$APP"
    elif command -v sudo >/dev/null 2>&1; then
      # shellcheck disable=SC2086  # SUDO is deliberately unquoted
      sudo apt-get remove -y "$APP"
    else
      die "the apt package '$APP' is installed but sudo is unavailable — run: apt-get remove $APP"
    fi
    say "removed the apt package $APP"
  else
    bin_dir="$HOME/.local/bin"
    data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
    rm -f "$bin_dir/$APP" "$bin_dir/$APP.AppImage" \
          "$data_home/applications/$APP.desktop" \
          "$data_home/icons/$APP.png"
    say "removed $bin_dir/$APP.AppImage, its launcher, and its desktop entry"
  fi
  report_data_kept
  note "to also delete your archive, remove $(data_dir) yourself"
  exit 0
}

if [ "$UNINSTALL" -eq 1 ]; then
  if [ "$ASSUME_YES" -ne 1 ]; then
    # Probe by opening, not by -r: /dev/tty is mode-readable on hosts
    # without a controlling terminal, but the open fails there.
    if ! ( : </dev/tty ) 2>/dev/null; then
      die "--uninstall asks for confirmation but no terminal is available (CI?) — pass --yes"
    fi
    printf 'Remove Mailchat Vault? Your archive and settings are KEPT. [y/N] '
    read -r answer </dev/tty || answer=""
    case "$answer" in
      y|Y|yes|Yes|YES) ;;
      *) say "aborted — nothing was removed"; exit 0 ;;
    esac
  fi
  uninstall
fi

# ------------------------------------------------------------- self-check

if command -v curl >/dev/null 2>&1; then
  FETCHER="curl"
elif command -v wget >/dev/null 2>&1; then
  FETCHER="wget"
else
  die "neither curl nor wget found — one of them is required to download"
fi

if command -v sha256sum >/dev/null 2>&1; then
  hash256() { sha256sum "$@"; }
elif command -v shasum >/dev/null 2>&1; then
  hash256() { shasum -a 256 "$@"; }
else
  die "neither sha256sum nor shasum found — required to verify downloads"
fi
command -v tar >/dev/null 2>&1 || die "tar not found — required to unpack the macOS app"

TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/mailchat-install.XXXXXXXX")" \
  || die "cannot create a temp directory"
cleanup() { rm -rf "$TMPROOT"; }
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

fetch() { # fetch <url> <dest-file>
  if [ "$FETCHER" = "curl" ]; then
    curl -fsSL -o "$2" "$1"
  else
    wget -q -O "$2" "$1"
  fi
}

# ------------------------------------------------------ release resolution
#
# releases/latest returns the newest published, non-draft, non-prerelease
# release; the pinned path goes through releases/tags/<tag> (which cannot
# see drafts unauthenticated, but the answer is checked anyway so a
# draft never installs silently).

PIN="${MAILCHAT_VERSION:-}"
[ -z "$VERSION_PIN" ] || PIN="$VERSION_PIN"

if [ -n "$PIN" ]; then
  case "$PIN" in
    v*) TAG="$PIN" ;;
    *)  TAG="v$PIN" ;;
  esac
  case "$TAG" in
    *[!A-Za-z0-9._-]*)
      die "'$PIN' does not look like a version (expected vX.Y.Z, e.g. v0.2.0)" ;;
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) die "'$PIN' does not look like a version (expected vX.Y.Z, e.g. v0.2.0)" ;;
  esac
  API_URL="https://api.github.com/repos/$REPO/releases/tags/$TAG"
else
  TAG=""
  API_URL="https://api.github.com/repos/$REPO/releases/latest"
fi

say "resolving the release to install from $API_URL"
fetch "$API_URL" "$TMPROOT/release.json" || die "could not resolve the release from the GitHub API \
(network down, rate-limited, or the release does not exist — see $README_URL)"

TAG="$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$TMPROOT/release.json" | head -n 1)"
[ -n "$TAG" ] || die "could not read the release tag from the API response"
VERSION="${TAG#v}"
if grep -q '"draft": *true' "$TMPROOT/release.json"; then
  die "release $TAG is a DRAFT — drafts are not installable; pick a published version"
fi

# Asset names (the macOS .app.tar.gz carries no version prefix — all
# matching below is by pattern/suffix, never by full constructed name).
ASSETS="$TMPROOT/assets.txt"
sed -n 's/.*"name": *"\([^"]*\)".*/\1/p' "$TMPROOT/release.json" \
  | grep -E '^(mailchat-vault|SHA256SUMS)' > "$ASSETS" || true

asset_url() { printf 'https://github.com/%s/releases/download/%s/%s\n' "$REPO" "$TAG" "$1"; }

find_asset() { # find_asset <regex> — first matching asset name, or empty
  grep -E "$1" "$ASSETS" | head -n 1 || true
}

download_asset() { # download_asset <asset-name>
  fetch "$(asset_url "$1")" "$TMPROOT/$1" \
    || die "could not download $1 from release $TAG"
}

verify_asset() { # verify_asset <asset-name> <file> — the gate before any install
  vname="$1"; vfile="$2"
  vsums="$TMPROOT/SHA256SUMS"
  if [ ! -f "$vsums" ]; then
    fetch "$(asset_url SHA256SUMS)" "$vsums" \
      || die "release $TAG has no downloadable SHA256SUMS — refusing to install unverified"
  fi
  vwant="$(awk -v n="$vname" '$2 == n || $2 == "*"n { print $1; exit }' "$vsums")"
  if [ -z "$vwant" ]; then
    die "SHA256SUMS on $TAG has no entry for '$vname' — refusing to install unverified"
  fi
  vgot="$(hash256 "$vfile" | awk '{print $1}')"
  if [ "$vgot" != "$vwant" ]; then
    die "checksum mismatch for $vname — NOTHING was installed, the download was discarded
     expected sha256 $vwant
     received sha256 $vgot"
  fi
  note "verified sha256 $(printf '%s' "$vgot" | cut -c 1-16)…  $vname"
}

list_assets_to_stderr() {
  warn "assets available on $TAG:"
  sed 's/^/  /' "$ASSETS" >&2
}

# ------------------------------------------------------------ platform pick

case "$ARCH" in
  x86_64|amd64)  AARCH="amd64" ;;
  aarch64|arm64) AARCH="arm64" ;;
  *) die "unsupported architecture '$ARCH' (this installer ships x86_64 and arm64 builds)" ;;
esac

DEBIAN_FAMILY=0
if [ "$OS" = "Linux" ]; then
  os_release="${MAILCHAT_OS_RELEASE:-/etc/os-release}"
  if [ -r "$os_release" ]; then
    ids="$(sed -n 's/^ID=//p;s/^ID_LIKE=//p' "$os_release" | tr -d '"' | tr '\n' ' ')"
    case "$ids" in
      *debian*|*ubuntu*) DEBIAN_FAMILY=1 ;;
    esac
  fi
fi

USE_DEB=0
SUDO=""
if [ "$OS" = "Linux" ] && [ "$DEBIAN_FAMILY" -eq 1 ] && [ "$AARCH" = "amd64" ] \
   && command -v apt-get >/dev/null 2>&1; then
  if [ "$(id -u)" = "0" ]; then
    USE_DEB=1
  elif command -v sudo >/dev/null 2>&1; then
    USE_DEB=1
    SUDO="sudo"
  else
    note "no sudo available — installing the AppImage instead of the .deb"
  fi
fi

# ------------------------------------------------------------------ install

INSTALLED_WHERE=""
LAUNCH_CMD=""

install_deb() { # install_deb <asset-name>
  dasset="$1"
  say "Debian-family Linux with apt — installing the .deb (apt resolves WebKitGTK and other dependencies)"
  if deb_package_installed; then
    say "an installed apt package was found — upgrading in place (the vault data dir is never touched)"
  fi
  download_asset "$dasset"
  verify_asset "$dasset" "$TMPROOT/$dasset"
  if [ -n "$SUDO" ]; then
    say "running: sudo apt-get install -y $dasset"
  else
    say "running: apt-get install -y $dasset"
  fi
  if [ -n "$SUDO" ]; then
    # shellcheck disable=SC2086  # SUDO is deliberately unquoted
    $SUDO apt-get install -y "$TMPROOT/$dasset"
  else
    apt-get install -y "$TMPROOT/$dasset"
  fi
  INSTALLED_WHERE="/usr/bin/$APP (apt package)"
  LAUNCH_CMD="$APP"
}

install_appimage() {
  aasset="$(find_asset "^mailchat-vault_${VERSION}_${AARCH}[.]AppImage$")"
  if [ -z "$aasset" ]; then
    list_assets_to_stderr
    die "no ${AARCH} AppImage on release $TAG"
  fi
  say "Linux without the deb route — installing the AppImage into ~/.local/bin (no root needed)"
  bin_dir="$HOME/.local/bin"
  data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
  if [ -e "$bin_dir/$APP.AppImage" ]; then
    say "existing AppImage found — replacing it in place (the vault data dir is never touched)"
  fi
  download_asset "$aasset"
  verify_asset "$aasset" "$TMPROOT/$aasset"
  mkdir -p "$bin_dir" "$data_home/applications" "$data_home/icons"
  appimage_file="$bin_dir/$APP.AppImage"
  cp "$TMPROOT/$aasset" "$appimage_file"
  chmod 0755 "$appimage_file"

  fuse=0
  if { command -v fusermount >/dev/null 2>&1 || command -v fusermount3 >/dev/null 2>&1; } \
     && [ -e "${MAILCHAT_FUSE_DEVICE:-/dev/fuse}" ]; then
    fuse=1
  fi
  wrapper="$bin_dir/$APP"
  if [ "$fuse" -eq 1 ]; then
    note "FUSE detected — the launcher runs the AppImage directly"
    {
      printf '#!/bin/sh\n'
      printf '# launcher written by mailchat-vault scripts/install.sh\n'
      printf 'exec "%s" "$@"\n' "$appimage_file"
    } > "$wrapper"
  else
    note "no FUSE detected — the launcher uses --appimage-extract-and-run (works everywhere)"
    {
      printf '#!/bin/sh\n'
      printf '# launcher written by mailchat-vault scripts/install.sh\n'
      printf 'exec "%s" --appimage-extract-and-run "$@"\n' "$appimage_file"
    } > "$wrapper"
  fi
  chmod 0755 "$wrapper"

  # Desktop integration: icon extracted from the AppImage's own payload
  # (it ships mailchat-vault.png next to its .desktop, like the deb does).
  exdir="$TMPROOT/appimage-extract"
  mkdir -p "$exdir"
  icon=""
  if ( cd "$exdir" && "$appimage_file" --appimage-extract >/dev/null 2>&1 ) \
     && [ -f "$exdir/squashfs-root/$APP.png" ]; then
    cp "$exdir/squashfs-root/$APP.png" "$data_home/icons/$APP.png"
    icon="$data_home/icons/$APP.png"
  else
    warn "could not extract the bundled icon — the desktop entry keeps a name-based Icon (cosmetic only)"
  fi
  [ -n "$icon" ] || icon="$APP"
  cat > "$data_home/applications/$APP.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Mailchat Vault
Comment=Private, searchable archive for email and chats
Exec=$wrapper
Icon=$icon
Terminal=false
MimeType=application/mbox;
Categories=Office;Utility;
DESKTOP
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$data_home/applications" || true
  fi
  INSTALLED_WHERE="$appimage_file (launcher: $wrapper)"
  LAUNCH_CMD="$wrapper"
}

install_macos() {
  masset="$(find_asset '[.]app[.]tar[.]gz$')"
  if [ -z "$masset" ]; then
    list_assets_to_stderr
    die "no macOS .app.tar.gz asset on release $TAG (it is named mailchat-vault.app.tar.gz — no version prefix)"
  fi
  say "macOS — installing the universal app bundle"
  if [ -d "/Applications/$APP.app" ] || [ -d "$HOME/Applications/$APP.app" ]; then
    say "existing app found — replacing it in place (the vault data dir is never touched)"
  fi
  download_asset "$masset"
  verify_asset "$masset" "$TMPROOT/$masset"
  unpack="$TMPROOT/unpack"
  mkdir -p "$unpack"
  tar -xzf "$TMPROOT/$masset" -C "$unpack"
  app_dir=""
  for d in "$unpack"/*.app; do
    if [ -d "$d" ]; then
      app_dir="$d"
      break
    fi
  done
  [ -n "$app_dir" ] || die "the archive $masset did not contain a .app bundle"
  dest_dir="/Applications"
  if [ ! -w "$dest_dir" ]; then
    dest_dir="$HOME/Applications"
    mkdir -p "$dest_dir"
    note "/Applications is not writable — using $dest_dir"
  fi
  if [ -d "$dest_dir/$APP.app" ]; then
    rm -rf "$dest_dir/$APP.app"
  fi
  mv "$app_dir" "$dest_dir/"
  # Unsigned artifacts (#78): clear the quarantine attributes the
  # download set, so the first launch does not hit the Gatekeeper block.
  if command -v xattr >/dev/null 2>&1; then
    if xattr -cr "$dest_dir/$APP.app" 2>/dev/null; then
      note "cleared the quarantine attribute — first launch will not hit Gatekeeper"
    else
      warn "could not clear quarantine — on first launch use right-click -> Open -> Open"
    fi
  else
    warn "xattr not found — on first launch use right-click -> Open -> Open"
  fi
  INSTALLED_WHERE="$dest_dir/$APP.app"
  MACOS_DEST="$dest_dir"
}

if [ "$OS" = "Darwin" ]; then
  install_macos
else
  done_deb=0
  if [ "$USE_DEB" -eq 1 ]; then
    deb_asset="$(find_asset "^mailchat-vault_${VERSION}_${AARCH}[.]deb$")"
    if [ -n "$deb_asset" ]; then
      install_deb "$deb_asset"
      done_deb=1
    else
      note "release $TAG ships no ${AARCH} .deb — falling back to the AppImage"
    fi
  fi
  if [ "$done_deb" -eq 0 ]; then
    install_appimage
  fi
fi

# ------------------------------------------------------------------ finish

say "Mailchat Vault $VERSION installed"
note "location:   $INSTALLED_WHERE"
note "release:    $TAG"
note "your archive is untouched: $(data_dir)"
note "uninstall:  curl -fsSL https://raw.githubusercontent.com/$REPO/main/scripts/install.sh | sh -s -- --uninstall"
note "            (keeps your data; delete $(data_dir) yourself to also remove the archive)"

if [ "$NO_LAUNCH" -eq 1 ]; then
  note "not launching (--no-launch)"
  exit 0
fi

if [ "$OS" = "Darwin" ]; then
  if command -v open >/dev/null 2>&1; then
    say "launching $MACOS_DEST/$APP.app"
    open "$MACOS_DEST/$APP.app" || warn "open failed — launch Mailchat Vault from your Applications folder"
  else
    warn "open not found — launch Mailchat Vault from your Applications folder"
  fi
else
  if command -v "$LAUNCH_CMD" >/dev/null 2>&1; then
    say "launching $LAUNCH_CMD"
    ( nohup "$LAUNCH_CMD" >/dev/null 2>&1 & ) || true
  else
    warn "$LAUNCH_CMD not on PATH yet — open a new terminal or your desktop menu"
  fi
fi

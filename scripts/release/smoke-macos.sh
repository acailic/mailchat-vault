#!/usr/bin/env bash
# macOS release smoke — verifies a PUBLISHED .dmg/.app artifact set the
# way smoke-linux.sh does for the deb/AppImage: against the artifact a
# user would actually download, on the OS a user would run it on.
#
# This is the macOS half of the M6.2 install/launch contract. It must
# run ON macOS (it mounts the dmg and inspects the app bundle), on any
# stock machine with nothing preinstalled beyond the OS:
#
#   scripts/release/smoke-macos.sh --dir DIR          # artifacts on disk
#   scripts/release/smoke-macos.sh --release v0.1.0   # download from GitHub
#   --skip-updater  --dir mode with no updater .app.tar.gz on disk
#                     (local built bundles ship none; --release requires it)
#
# Checks (per artifact):
#   dmg   mounts read-only; contains exactly the .app; unmounts clean
#   tgz   validates as gzip tar; contains a .app/Contents/Info.plist
#           whose CFBundleShortVersionString matches the artifact name
#   app   Info.plist bundle id/name/version sane; executable present
#         universal binary (arm64 + x86_64) via lipo
#         ad-hoc code signature present (codesign -v) — this release
#           round ships unsigned-by-design, so we assert the signature
#           EXISTS and is ad-hoc, not that it is a Developer ID
#   app   launches, stays alive $ALIVE_SECONDS, then quits cleanly
#         (first launch on a downloaded copy hits Gatekeeper — the
#          script clears the quarantine xattr first, matching the
#          documented user path)
#
# A per-check PASS/FAIL summary prints at the end; logs land in the
# --logs directory (default <repo>/target/release/smoke-logs).
#
# NOTE: this cannot run on Linux (no hdiutil/codesign); CI runners and
# this repo's docker-based Linux suites cannot substitute for it. A
# green run of this script is the macOS release evidence.
set -uo pipefail

here="$(cd "$(dirname "$BASH_SOURCE")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"

DIR=""
RELEASE=""
LOGS="$repo/target/release/smoke-logs"
ALIVE_SECONDS=20
APP_ID="com.acailic.mailchatvault"
SKIP_UPDATER=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir)          DIR="$2"; shift 2 ;;
    --release)      RELEASE="$2"; shift 2 ;;
    --logs)         LOGS="$2"; shift 2 ;;
    --alive)        ALIVE_SECONDS="$2"; shift 2 ;;
    --skip-updater) SKIP_UPDATER=1; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: smoke-macos.sh must run on macOS (hdiutil/codesign/lipo required)" >&2
  exit 2
fi
command -v hdiutil >/dev/null || { echo "error: hdiutil not found" >&2; exit 2; }
command -v codesign >/dev/null || { echo "error: codesign not found" >&2; exit 2; }

if [[ -n "$RELEASE" ]]; then
  DIR="${DIR:-$repo/target/release/verify/$RELEASE}"
  echo "== downloading release $RELEASE assets to $DIR"
  gh release download "$RELEASE" --dir "$DIR" --clobber --pattern '*.dmg' --pattern '*.app.tar.gz'
fi
[[ -n "$DIR" ]] || { echo "usage: $0 --dir DIR | --release TAG" >&2; exit 2; }

dmg="$(find "$DIR" -name '*.dmg' 2>/dev/null | head -1)"
if [[ -z "$dmg" ]]; then
  echo "error: no .dmg found under $DIR" >&2
  echo "  download the release assets first: gh release download <tag> --dir '$DIR'" >&2
  exit 2
fi
mkdir -p "$LOGS"
log="$LOGS/macos-$(date -u +%Y%m%dT%H%M%SZ).log"
exec > >(tee -a "$log") 2>&1

PASS=0; FAIL=0
check() { # check <name> <command...> — command exit 0 == pass
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then
    PASS=$((PASS+1)); echo "PASS: $name"
  else
    FAIL=$((FAIL+1)); echo "FAIL: $name"
  fi
}

echo "== macos suite: $(basename "$dmg") on $(sw_vers -productVersion) ($(uname -m))"

# ------------------------------------------------------------------- dmg
mountpoint="$(mktemp -d)/dmg"
mkdir -p "$mountpoint"
cleanup_mount() {
  if [[ -d "$mountpoint" ]]; then
    hdiutil detach "$mountpoint" -quiet 2>/dev/null || :
  fi
}
trap cleanup_mount EXIT
trap 'cleanup_mount; exit 130' INT
trap 'cleanup_mount; exit 143' TERM
check "dmg attaches read-only" hdiutil attach "$dmg" -mountpoint "$mountpoint" -readonly -nobrowse -quiet
app_count="$(find "$mountpoint" -maxdepth 1 -name '*.app' | wc -l | tr -d ' ')"
check "dmg contains exactly one .app bundle" test "$app_count" -eq 1 -a -d "$mountpoint"/*.app
app_in_dmg="$(find "$mountpoint" -maxdepth 1 -name '*.app' | head -1)"

# --------------------------------------------------------------- updater
# The .app.tar.gz is the updater artifact — a published release asset,
# so the smoke verifies it too: archive valid, .app structure present,
# and its bundled version matches the release version (the tag, or the
# version-bearing dmg name in --dir mode).
app_tgz="$(find "$DIR" -name '*.app.tar.gz' 2>/dev/null | head -1)"
if [[ -z "$app_tgz" && "$SKIP_UPDATER" -eq 1 && -z "$RELEASE" ]]; then
  echo "note: no .app.tar.gz under $DIR — skipping the updater-archive checks (--skip-updater)"
elif [[ -z "$app_tgz" ]]; then
  echo "error: no .app.tar.gz found under $DIR" >&2
  echo "  (pass --skip-updater only when smoking local built bundles that ship no updater artifact)" >&2
  exit 2
else
check "updater archive is a valid gzip tar" tar -tzf "$app_tgz"
plist_member="$(tar -tzf "$app_tgz" | grep -m1 '\.app/Contents/Info\.plist$')"
check "updater archive contains a .app/Contents/Info.plist" test -n "$plist_member"
tar_version="$(tar -xOzf "$app_tgz" "$plist_member" 2>/dev/null | plutil -extract CFBundleShortVersionString raw - 2>/dev/null)"
expected_version="${RELEASE#v}"
[[ -n "$expected_version" ]] || expected_version="$(basename "$dmg")"
check "updater version matches the release version" \
  bash -c '[[ -n "$1" && "$2" == *"$1"* ]]' bash "$tar_version" "$expected_version"
fi

# ------------------------------------------------------------------- app
APP="$app_in_dmg"
plist="$APP/Contents/Info.plist"
check "Info.plist exists" test -f "$plist"
check "bundle identifier is $APP_ID" \
  bash -c 'plutil -extract CFBundleIdentifier raw "$1" | grep -qx "$2"' bash "$plist" "$APP_ID"
check "bundle version matches the artifact name" \
  bash -c 'v="$(plutil -extract CFBundleShortVersionString raw "$1")"; [[ -n "$v" && "$2" == *"$v"* ]]' bash "$plist" "$dmg"
exe_rel="$(plutil -extract CFBundleExecutable raw "$plist" 2>/dev/null)"
exe="$APP/Contents/MacOS/$exe_rel"
check "bundle executable exists and is executable" test -n "$exe_rel" -a -x "$exe"

check "universal binary: arm64 + x86_64 slices present" \
  bash -c 'lipo -archs "$1" | tr " " "\n" | sort | tr "\n" " " | grep -q "arm64 x86_64"' bash "$exe"

# Unsigned-by-design round: assert the signature is ad-hoc (the app
# would be killed by the kernel without one), NOT a Developer ID.
sig="$(codesign -dv "$APP" 2>&1 | grep -m1 'Signature=')"
check "code signature is ad-hoc" bash -c 'grep -q "Signature=adhoc" <<< "$1"' bash "$sig"
echo "     (signature: ${sig:-none})"

# --------------------------------------------------------------- launch
#
# Copy the app out of the readonly mount (a user drags it out), strip
# the quarantine attribute a browser download sets (the documented
# xattr -cr path), then launch and require it to stay alive.
run_dir="$(mktemp -d)"
cp -R "$APP" "$run_dir/"
xattr -cr "$run_dir/$(basename "$APP")" 2>/dev/null || true

"$run_dir/$(basename "$APP")/Contents/MacOS/$exe_rel" >/dev/null 2>&1 &
app_pid=$!
alive=1
i=1
while [ "$i" -le "$ALIVE_SECONDS" ]; do
  sleep 1
  if ! kill -0 "$app_pid" 2>/dev/null; then
    wait "$app_pid" 2>/dev/null
    echo "app exited early with status $? after <${i}s"
    alive=0
    break
  fi
  i=$((i+1))
done
if [[ "$alive" -eq 1 ]]; then
  kill "$app_pid" 2>/dev/null
  wait "$app_pid" 2>/dev/null
fi
check "app launches and stays alive ${ALIVE_SECONDS}s" test "$alive" -eq 1

check "dmg detaches cleanly" hdiutil detach "$mountpoint" -quiet

# --------------------------------------------------------------- summary
echo
echo "==================== macos smoke summary ===================="
echo "passed: $PASS   failed: $FAIL   (log: $log)"
[[ "$FAIL" -eq 0 ]] && echo "RESULT: PASS" || echo "RESULT: FAIL"
echo "==============================================================="
[[ "$FAIL" -eq 0 ]]

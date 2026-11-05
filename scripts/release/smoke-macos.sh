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
#
# Checks (per artifact):
#   dmg   mounts read-only; contains exactly the .app; unmounts clean
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

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir)          DIR="$2"; shift 2 ;;
    --release)      RELEASE="$2"; shift 2 ;;
    --logs)         LOGS="$2"; shift 2 ;;
    --alive)        ALIVE_SECONDS="$2"; shift 2 ;;
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
check "dmg attaches read-only" hdiutil attach "$dmg" -mountpoint "$mountpoint" -readonly -nobrowse -quiet
app_in_dmg="$(find "$mountpoint" -maxdepth 1 -name '*.app' | head -1)"
check "dmg contains the .app bundle" test -n "$app_in_dmg" -a -d "$app_in_dmg"

# ------------------------------------------------------------------- app
APP="$app_in_dmg"
plist="$APP/Contents/Info.plist"
check "Info.plist exists" test -f "$plist"
check "bundle identifier is $APP_ID" \
  bash -c "plutil -extract CFBundleIdentifier raw \"$plist\" | grep -qx '$APP_ID'"
check "bundle version matches the artifact name" \
  bash -c "v=\"\$(plutil -extract CFBundleShortVersionString raw \"$plist\")\"; [[ \"$dmg\" == *\"\${v}\"* ]]"
exe_rel="$(plutil -extract CFBundleExecutable raw "$plist" 2>/dev/null)"
exe="$APP/Contents/MacOS/$exe_rel"
check "bundle executable exists and is executable" test -n "$exe_rel" -a -x "$exe"

check "universal binary: arm64 + x86_64 slices present" \
  bash -c "lipo -archs \"$exe\" | tr ' ' '\\n' | sort | tr '\\n' ' ' | grep -q 'arm64 x86_64'"

# Unsigned-by-design round: assert an ad-hoc signature exists (the app
# would be killed by the kernel without one), NOT a Developer ID.
sig="$(codesign -dv "$APP" 2>&1 | grep -m1 'Signature=')"
check "code signature present (ad-hoc expected this round)" test -n "$sig"
echo "     (signature: ${sig:-none})"

# --------------------------------------------------------------- launch
#
# Copy the app out of the readonly mount (a user drags it out), strip
# the quarantine attribute a browser download sets (the documented
# xattr -cr path), then launch and require it to stay alive.
run_dir="$(mktemp -d)"
cp -R "$APP" "$run_dir/"
xattr -cr "$run_dir/$(basename "$APP")" 2>/dev/null || true

launch_rc=0
timeout $((ALIVE_SECONDS + 15))s "$run_dir/$(basename "$APP")/Contents/MacOS/$exe_rel" >/dev/null 2>&1 || launch_rc=$?
# exit 124 == timeout had to kill it == it stayed alive the whole window
check "app launches and stays alive ${ALIVE_SECONDS}s (exit 124 expected)" \
  test "$launch_rc" -eq 124

check "dmg detaches cleanly" hdiutil detach "$mountpoint" -quiet

# --------------------------------------------------------------- summary
echo
echo "==================== macos smoke summary ===================="
echo "passed: $PASS   failed: $FAIL   (log: $log)"
[[ "$FAIL" -eq 0 ]] && echo "RESULT: PASS" || echo "RESULT: FAIL"
echo "==============================================================="
[[ "$FAIL" -eq 0 ]]

#!/usr/bin/env bash
# dmg smoke test (macOS) — M6.1 release quality gate.
#
# Verifies a packaged mailchat-vault .dmg is a working universal app,
# headlessly, without installing anything:
#
#   1. mounts the dmg read-only,
#   2. copies the .app bundle out of the mounted volume (like a user drag),
#   3. checks the binary is universal (x86_64 + arm64),
#   4. runs `mailchat-vault --version` and `--help` and checks exit code
#      and output (the flags print and exit 0 before any GUI work).
#
# Usage:
#   scripts/dmg-smoke-test.sh <path/to/mailchat-vault_v_universal.dmg> [expected-version]
#
# `expected-version` (optional, e.g. 0.1.0) pins `--version` to an exact
# string; without it any `x.y.z` version is accepted. CI passes the
# version from src-tauri/tauri.conf.json.
#
# Exit status: 0 = all checks passed, 1 = a check failed. Must run on
# macOS (uses hdiutil, ditto, lipo, xattr).

set -euo pipefail

fail() { echo "FAIL: $*" >&2; exit 1; }

DMG="${1:-}"
EXPECTED_VERSION="${2:-}"
[ -n "$DMG" ] || fail "usage: $0 <path/to/mailchat-vault_v_universal.dmg> [expected-version]"
[ "$(uname -s)" = "Darwin" ] || fail "this smoke test must run on macOS (needs hdiutil, ditto, lipo)"
[ -f "$DMG" ] || fail "dmg not found: $DMG"
case "$DMG" in
    *.dmg) ;;
    *) fail "not a .dmg: $DMG" ;;
esac

WORK="$(mktemp -d "${TMPDIR:-/tmp}/mailchat-dmg-smoke.XXXXXX")"
MOUNT_POINT=""
cleanup() {
    if [ -n "$MOUNT_POINT" ]; then
        hdiutil detach "$MOUNT_POINT" -quiet -force >/dev/null 2>&1 || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

# --- 1. mount -------------------------------------------------------------
if ! MOUNT_INFO="$(hdiutil attach -nobrowse -readonly "$DMG" 2>&1)"; then
    fail "hdiutil attach failed: $MOUNT_INFO"
fi
MOUNT_POINT="$(printf '%s\n' "$MOUNT_INFO" | grep '/Volumes/' | tail -n1 | awk -F'\t' '{print $NF}' || true)"
[ -n "$MOUNT_POINT" ] && [ -d "$MOUNT_POINT" ] \
    || fail "could not locate the mounted volume in: $MOUNT_INFO"

# --- 2. copy the .app out (the scripted "drag to Applications") -----------
APP_PATH="$(find "$MOUNT_POINT" -maxdepth 1 -name '*.app' -print -quit)"
[ -n "$APP_PATH" ] || fail "no .app bundle found inside the dmg"
APP_COPY="$WORK/$(basename "$APP_PATH")"
ditto "$APP_PATH" "$APP_COPY" || fail "copying the .app bundle out of the dmg failed"
# A downloaded dmg carries a quarantine flag; this smoke test checks the
# binary works, not Gatekeeper (documented in docs/release.md), so clear it.
xattr -dr com.apple.quarantine "$APP_COPY" 2>/dev/null || true
hdiutil detach "$MOUNT_POINT" -quiet || fail "unmounting $MOUNT_POINT failed"
MOUNT_POINT=""

# --- 3. structure + universal binary --------------------------------------
[ -f "$APP_COPY/Contents/Info.plist" ] || fail "Contents/Info.plist missing in the copied .app"
BIN="$APP_COPY/Contents/MacOS/mailchat-vault"
[ -x "$BIN" ] || fail "app binary missing or not executable: $BIN"

ARCHS="$(lipo -archs "$BIN")"
case "$ARCHS" in
    *arm64*) ;;
    *) fail "universal binary lacks arm64 (lipo -archs said: $ARCHS)" ;;
esac
case "$ARCHS" in
    *x86_64*) ;;
    *) fail "universal binary lacks x86_64 (lipo -archs said: $ARCHS)" ;;
esac

# --- 4. headless launch: --version / --help --------------------------------
if ! VERSION_OUT="$("$BIN" --version 2>&1)"; then
    fail "--version exited non-zero; output: $VERSION_OUT"
fi
if [ -n "$EXPECTED_VERSION" ]; then
    [ "$VERSION_OUT" = "mailchat-vault $EXPECTED_VERSION" ] \
        || fail "--version printed '$VERSION_OUT', expected 'mailchat-vault $EXPECTED_VERSION'"
else
    [[ "$VERSION_OUT" =~ ^mailchat-vault\ [0-9]+\.[0-9]+\.[0-9]+$ ]] \
        || fail "--version printed unexpected output: $VERSION_OUT"
fi

if ! HELP_OUT="$("$BIN" --help 2>&1)"; then
    fail "--help exited non-zero; output: $HELP_OUT"
fi
echo "$HELP_OUT" | grep -q 'Usage: mailchat-vault' \
    || fail "--help output does not contain a usage line: $HELP_OUT"

echo "PASS: $DMG"
echo "  app:      $(basename "$APP_PATH")"
echo "  archs:    $ARCHS"
echo "  version:  $VERSION_OUT"

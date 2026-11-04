#!/usr/bin/env bash
# AppImage smoke suite — runs INSIDE the smoke container (root, /artifacts ro).
#
# Verifies the AppImage release artifact on a clean Ubuntu:
#   1. The smoke image has NONE of the app runtime installed
#      (no libwebkit2gtk-4.1, no libgtk-3, ...): if the app runs, the
#      AppImage bundles its own dependencies (AC: AppImage works on
#      clean 22.04 without additional dependencies).
#   2. `--appimage-extract` yields the freedesktop integration files
#      a desktop uses for launcher/icon/MIME integration:
#      <name>.desktop (Terminal=false, Icon=, MimeType=application/mbox),
#      .DirIcon + icon png, and the app binary.
#   3. The AppImage launches (extract-and-run; FUSE-free) under a
#      display and stays alive.
set -u

ARTIFACTS="${ARTIFACTS:-/artifacts}"
DESKTOP_NAME="mailchat-vault"
BIN="mailchat-vault"
LAUNCH_SECONDS="${LAUNCH_SECONDS:-15}"

PASS=0
FAIL=0

check() { # check <name> <command...> — command exit 0 == pass
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then
    PASS=$((PASS+1)); echo "PASS: $name"
  else
    FAIL=$((FAIL+1)); echo "FAIL: $name"
  fi
}

check_grep() { # check_grep <name> <pattern> <file>
  local name="$1" pattern="$2" file="$3"
  check "$name" grep -Eq "$pattern" "$file"
}

fail_now() { echo "FATAL: $*" >&2; exit 2; }

# ----------------------------------------------------------- clean system

for pkg in libwebkit2gtk-4.1-0 libjavascriptcoregtk-4.1-0 \
           libsoup-3.0-1 libgtk-3-0 librsvg2-2; do
  check "clean system: $pkg not installed" \
    bash -c "! dpkg-query -W -f='\${Status}' '$pkg' 2>/dev/null | grep -q 'install ok installed'"
done

appimage="$(find "$ARTIFACTS" -name '*.AppImage' 2>/dev/null | sort | head -1)"
[[ -n "$appimage" ]] || fail_now "no .AppImage found under $ARTIFACTS"
echo "== appimage suite: $(basename "$appimage") on $(. /etc/os-release && echo "$PRETTY_NAME")"

# The artifact mount is read-only; the file is usually already
# executable from the build (chmod is best-effort).
chmod +x "$appimage" 2>/dev/null || true

# --------------------------------------------- desktop integration payload

work="$(mktemp -d)"
( cd "$work" && "$appimage" --appimage-extract >/dev/null 2>&1 )
squashfs="$work/squashfs-root"   # AppImage type-2 extract dir name

check "AppImage extracts (--appimage-extract)" test -x "$squashfs/AppRun"
desktop="$squashfs/${DESKTOP_NAME}.desktop"
check "bundled desktop entry present (${DESKTOP_NAME}.desktop)" test -f "$desktop"
check "bundled icon present (icon png)" \
  test -f "$squashfs/${DESKTOP_NAME}.png"
check "bundled .DirIcon symlink present" test -e "$squashfs/.DirIcon"
check "app binary present in bundle (usr/bin/$BIN)" \
  test -x "$squashfs/usr/bin/$BIN"

check_grep "bundled desktop entry: Terminal=false" '^Terminal=false$' "$desktop"
check_grep "bundled desktop entry: Type=Application" '^Type=Application$' "$desktop"
check_grep "bundled desktop entry: Icon= is set" '^Icon=.+' "$desktop"
check_grep "bundled desktop entry: MIME association application/mbox" \
  '^MimeType=.*application/mbox' "$desktop"
check_grep "bundled desktop entry: Exec points into the bundle" \
  '^Exec=.+' "$desktop"

# The WebKit helper processes must be inside the bundle (they cannot be
# dlopen'd from a system that does not ship webkit at all).
for helper in WebKitWebProcess WebKitNetworkProcess; do
  check "bundled webkit helper: $helper" \
    bash -c "find '$squashfs' -name '$helper' -type f | grep -q ."
done

# -------------------------------------------------------------- launch it
#
# --appimage-extract-and-run: no FUSE needed (containers/CI have none;
# on a real desktop plain double-click uses FUSE — equivalent launch).

launch_rc=0
rm -f /tmp/appimage-window-seen
xvfb-run -a -s "-screen 0 1280x800x24" \
  timeout $((LAUNCH_SECONDS + 45))s bash -c "
    setsid '$appimage' --appimage-extract-and-run </dev/null >/tmp/appimage-launch.log 2>&1 &
    for _ in \$(seq 1 $LAUNCH_SECONDS); do
      sleep 1
      if xwininfo -root -tree 2>/dev/null | grep -qi 'mailchat-vault'; then
        touch /tmp/appimage-window-seen
      fi
      pgrep -x '$BIN' >/dev/null || exit 1   # died early -> fail fast
    done
    pgrep -x '$BIN' >/dev/null || exit 1
    exit 124                                # still alive at deadline
  " || launch_rc=$?
pkill -x "$BIN" 2>/dev/null || true

if [[ -f /tmp/appimage-window-seen ]]; then
  PASS=$((PASS+1)); echo "PASS: app maps a window under X"
else
  FAIL=$((FAIL+1)); echo "FAIL: no X window seen for mailchat-vault"
fi

if [[ "$launch_rc" == "124" ]]; then
  PASS=$((PASS+1)); echo "PASS: AppImage runs on the clean system and stays alive ${LAUNCH_SECONDS}s (no app packages installed)"
else
  FAIL=$((FAIL+1)); echo "FAIL: AppImage launch on clean system (rc=$launch_rc)"; echo "--- last log lines:"; tail -n 20 /tmp/appimage-launch.log 2>/dev/null
fi

# ----------------------------------------------------------------- report

echo
echo "== appimage suite result: $PASS passed, $FAIL failed"
[[ "$FAIL" -eq 0 ]] || exit 1
exit 0

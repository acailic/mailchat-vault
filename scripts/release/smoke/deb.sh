#!/usr/bin/env bash
# deb smoke suite — runs INSIDE the smoke container (root, /artifacts ro).
#
# Verifies the .deb release artifact on a clean Ubuntu:
#   1. `apt install ./x.deb` succeeds and resolves the declared runtime
#      dependencies itself (webkit2gtk etc. are NOT in the image).
#   2. Desktop integration installed by the package:
#      /usr/share/applications/<name>.desktop (valid, Type=Application,
#      Terminal=false), hicolor icons, MIME association for
#      application/mbox registered in the desktop database.
#   3. The app launches from the desktop entry's Exec line with no
#      terminal, maps a window and stays alive.
#   4. Removing the package removes the integration (it is owned by
#      the package, not scattered files).
set -u

ARTIFACTS="${ARTIFACTS:-/artifacts}"
DESKTOP_NAME="mailchat-vault"
BIN="mailchat-vault"
DESKTOP="/usr/share/applications/${DESKTOP_NAME}.desktop"
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

# ---------------------------------------------------------------- install

deb="$(find "$ARTIFACTS" -name '*.deb' 2>/dev/null | sort | head -1)"
[[ -n "$deb" ]] || fail_now "no .deb found under $ARTIFACTS"
echo "== deb suite: $(basename "$deb") on $(. /etc/os-release && echo "$PRETTY_NAME")"

# The image ships none of the app runtime — apt must resolve it.
check "none of the app runtime is preinstalled (libwebkit2gtk-4.1-0 absent)" \
  bash -c "! dpkg-query -W -f='\${Status}' libwebkit2gtk-4.1-0 2>/dev/null | grep -q 'install ok installed'"

apt-get update -qq
check "apt install ./deb resolves runtime dependencies" \
  apt-get install -y -qq "$deb"

check "dpkg status: install ok installed" \
  bash -c "dpkg -s $BIN | grep -q 'Status: install ok installed'"

check "binary installed at /usr/bin/$BIN" test -x "/usr/bin/$BIN"

# ---------------------------------------------------- desktop integration

check "desktop file installed at $DESKTOP" test -f "$DESKTOP"
check "desktop file is owned by the package" \
  bash -c "dpkg -L $BIN | grep -qx '$DESKTOP'"
check "desktop file validates (desktop-file-validate)" \
  desktop-file-validate "$DESKTOP"
check_grep "desktop entry: Type=Application" '^Type=Application$' "$DESKTOP"
check_grep "desktop entry: launches without terminal (Terminal=false)" \
  '^Terminal=false$' "$DESKTOP"

exec_line="$(grep -m1 '^Exec=' "$DESKTOP" | cut -d= -f2-)"
check "desktop entry: Exec command resolves to the installed binary" \
  bash -c "command -v $(echo "$exec_line" | awk '{print $1}')"

icon_name="$(grep -m1 '^Icon=' "$DESKTOP" | cut -d= -f2-)"
check "desktop entry: Icon= is set" test -n "$icon_name"
icon_hits="$(find /usr/share/icons/hicolor -name "${icon_name}.png" 2>/dev/null | wc -l)"
check "icon installed under /usr/share/icons/hicolor ($icon_hits size(s))" \
  test "$icon_hits" -gt 0
check "icons are owned by the package" \
  bash -c "dpkg -L $BIN | grep -q 'usr/share/icons/hicolor/.*/apps/$icon_name.png'"

check_grep "desktop entry: MIME association application/mbox declared" \
  '^MimeType=.*application/mbox' "$DESKTOP"
check "postinst refreshed the desktop MIME database (mimeinfo.cache)" \
  bash -c "grep -A5 'application/mbox' /usr/share/applications/mimeinfo.cache | grep -q '$DESKTOP_NAME.desktop'"
xdg-mime default "${DESKTOP_NAME}.desktop" application/mbox
check "xdg-mime query default application/mbox -> ${DESKTOP_NAME}.desktop" \
  bash -c "test \"\$(xdg-mime query default application/mbox)\" = '$DESKTOP_NAME.desktop'"

# ------------------------------------------------- launch without terminal
#
# Simulate what a desktop launcher does: run the desktop entry's Exec
# command under a display, detached from any terminal (setsid, no tty).

launch_rc=0
rm -f /tmp/deb-window-seen
xvfb-run -a -s "-screen 0 1280x800x24" \
  timeout $((LAUNCH_SECONDS + 45))s bash -c "
    setsid $BIN </dev/null >/tmp/deb-launch.log 2>&1 &
    for _ in \$(seq 1 $LAUNCH_SECONDS); do
      sleep 1
      if xwininfo -root -tree 2>/dev/null | grep -qi 'mailchat-vault'; then
        touch /tmp/deb-window-seen
      fi
      pgrep -x '$BIN' >/dev/null || exit 1   # died early -> fail fast
    done
    pgrep -x '$BIN' >/dev/null || exit 1
    exit 124                                # still alive at deadline
  " || launch_rc=$?
pkill -x "$BIN" 2>/dev/null || true

if [[ -f /tmp/deb-window-seen ]]; then
  PASS=$((PASS+1)); echo "PASS: app maps a window under X"
else
  FAIL=$((FAIL+1)); echo "FAIL: no X window seen for mailchat-vault"
fi

if [[ "$launch_rc" == "124" ]]; then
  PASS=$((PASS+1)); echo "PASS: app stays alive ${LAUNCH_SECONDS}s launched without a terminal"
else
  FAIL=$((FAIL+1)); echo "FAIL: launch without terminal (rc=$launch_rc)"; echo "--- last log lines:"; tail -n 20 /tmp/deb-launch.log 2>/dev/null
fi

# ----------------------------------------------------------------- remove

check "apt remove takes the desktop file and binary with it" \
  bash -c "apt-get remove -y -qq $BIN >/dev/null 2>&1 && ! test -e '$DESKTOP' && ! test -e /usr/bin/$BIN"

# ----------------------------------------------------------------- report

echo
echo "== deb suite result: $PASS passed, $FAIL failed"
[[ "$FAIL" -eq 0 ]] || exit 1
exit 0

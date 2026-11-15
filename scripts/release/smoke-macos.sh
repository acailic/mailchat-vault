#!/usr/bin/env bash
# macOS release smoke — verifies a PUBLISHED .dmg/.app artifact set the
# way smoke-linux.sh does for the deb/AppImage: against the artifact a
# user would actually download, on the OS a user would run it on.
#
# This is the macOS half of the M6.2 install/launch contract and the
# automated leg of the G1 native verification runbook
# (docs/native-verification.md). It must run ON macOS (it mounts the
# dmg and inspects the app bundle), on any stock machine with nothing
# preinstalled beyond the OS:
#
#   scripts/release/smoke-macos.sh --dir DIR          # artifacts on disk
#   scripts/release/smoke-macos.sh --release v0.1.0   # download from GitHub
#   --tester "Name"       who ran it (recorded in the evidence JSON;
#                         default: $MC_SMOKE_TESTER, git user.name, whoami)
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
# --logs directory (default <repo>/target/release/smoke-logs). Every
# run also writes a machine-readable JSON evidence record next to the
# log (macos-<stamp>.json): result, host, tester, UTC timestamps, the
# repo commit, and the SHA-256 of every artifact smoked — the fields
# the G1 gate binds evidence to. That JSON, not a screenshot, is what
# a native run attaches (see docs/evidence/native-verification-templates/).
#
# NOTE: this cannot run on Linux (no hdiutil/codesign); CI runners and
# this repo's docker-based Linux suites cannot substitute for it. A
# green run of this script is the macOS release evidence.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"

DIR=""
RELEASE=""
LOGS="$repo/target/release/smoke-logs"
ALIVE_SECONDS=20
APP_ID="com.acailic.mailchatvault"
SKIP_UPDATER=0
TESTER="${MC_SMOKE_TESTER:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir)          DIR="$2"; shift 2 ;;
    --release)      RELEASE="$2"; shift 2 ;;
    --logs)         LOGS="$2"; shift 2 ;;
    --alive)        ALIVE_SECONDS="$2"; shift 2 ;;
    --tester)       TESTER="$2"; shift 2 ;;
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
command -v shasum >/dev/null || { echo "error: shasum not found (evidence hashing needs it)" >&2; exit 2; }

if [[ -n "$RELEASE" ]]; then
  command -v gh >/dev/null || { echo "error: gh not found (--release downloads assets via gh)" >&2; exit 2; }
  DIR="${DIR:-$repo/target/release/verify/$RELEASE}"
  echo "== downloading release $RELEASE assets to $DIR"
  repo_slug="$(cd "$repo" && gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)"
  if [[ -n "$repo_slug" ]]; then
    gh release download "$RELEASE" --repo "$repo_slug" --dir "$DIR" --clobber \
      --pattern '*.dmg' --pattern '*.app.tar.gz'
  else
    gh release download "$RELEASE" --dir "$DIR" --clobber \
      --pattern '*.dmg' --pattern '*.app.tar.gz'
  fi
fi
[[ -n "$DIR" ]] || { echo "usage: $0 --dir DIR | --release TAG [--tester NAME]" >&2; exit 2; }

# Pick the dmg deterministically: newest mtime wins; a directory with
# several dmgs must never silently smoke an arbitrary find() order.
newest_match() { # newest_match <dir> <glob> — echoes the newest match
  local best="" f
  while IFS= read -r f; do
    [[ -e "$f" ]] || continue
    if [[ -z "$best" || "$f" -nt "$best" ]]; then best="$f"; fi
  done < <(find "$1" -name "$2" 2>/dev/null)
  [[ -n "$best" ]] && printf '%s\n' "$best"
}

dmg_count="$(find "$DIR" -name '*.dmg' 2>/dev/null | wc -l | tr -d ' ')"
dmg="$(newest_match "$DIR" '*.dmg')"
if [[ -z "$dmg" ]]; then
  echo "error: no .dmg found under $DIR" >&2
  echo "  download the release assets first: gh release download <tag> --dir '$DIR'" >&2
  exit 2
fi
if [[ "$dmg_count" -gt 1 ]]; then
  echo "warning: $dmg_count dmgs under $DIR — smoking the newest by mtime:" >&2
  find "$DIR" -name '*.dmg' 2>/dev/null | sort >&2
  echo "  chosen: $dmg" >&2
fi

mkdir -p "$LOGS"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
log="$LOGS/macos-${stamp}.log"
json="$LOGS/macos-${stamp}.json"
exec > >(tee -a "$log") 2>&1

STARTED_UTC="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if [[ -z "$TESTER" ]]; then
  TESTER="$(git -C "$repo" config user.name 2>/dev/null || true)"
fi
if [[ -z "$TESTER" ]]; then
  TESTER="$(whoami)"
fi
HOST_OS="$(sw_vers -productName 2>/dev/null || echo macOS)"
HOST_VER="$(sw_vers -productVersion 2>/dev/null || echo unknown)"
HOST_ARCH="$(uname -m)"
REPO_COMMIT="$(git -C "$repo" rev-parse HEAD 2>/dev/null || echo unknown)"

PASS=0; FAIL=0
CHECK_NAMES=()
CHECK_STATUSES=()
check() { # check <name> <command...> — command exit 0 == pass
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then
    PASS=$((PASS+1)); echo "PASS: $name"
    CHECK_NAMES+=("$name"); CHECK_STATUSES+=(PASS)
  else
    FAIL=$((FAIL+1)); echo "FAIL: $name"
    CHECK_NAMES+=("$name"); CHECK_STATUSES+=(FAIL)
  fi
}

echo "== macos suite: $(basename "$dmg") on ${HOST_OS} ${HOST_VER} (${HOST_ARCH})"
echo "   tester: ${TESTER}   started: ${STARTED_UTC}   commit: ${REPO_COMMIT}"

dmg_sha="$(shasum -a 256 "$dmg" | awk '{print $1}')"
echo "   artifact: $(basename "$dmg") sha256=${dmg_sha}"

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
app_in_dmg="$(find "$mountpoint" -maxdepth 1 -name '*.app' 2>/dev/null | sort | head -1)"
app_count="$(find "$mountpoint" -maxdepth 1 -name '*.app' 2>/dev/null | wc -l | tr -d ' ')"
check "dmg contains exactly one .app bundle" test "$app_count" -eq 1 -a -n "$app_in_dmg" -a -d "$app_in_dmg"

# --------------------------------------------------------------- updater
# The .app.tar.gz is the updater artifact — a published release asset,
# so the smoke verifies it too: archive valid, .app structure present,
# and its bundled version matches the release version (the tag, or the
# version-bearing dmg name in --dir mode).
app_tgz="$(newest_match "$DIR" '*.app.tar.gz')"
app_tgz_sha=""
if [[ -z "$app_tgz" && "$SKIP_UPDATER" -eq 1 && -z "$RELEASE" ]]; then
  echo "note: no .app.tar.gz under $DIR — skipping the updater-archive checks (--skip-updater)"
elif [[ -z "$app_tgz" ]]; then
  echo "error: no .app.tar.gz found under $DIR" >&2
  echo "  (pass --skip-updater only when smoking local built bundles that ship no updater artifact)" >&2
  exit 2
else
  app_tgz_sha="$(shasum -a 256 "$app_tgz" | awk '{print $1}')"
  echo "   artifact: $(basename "$app_tgz") sha256=${app_tgz_sha}"
check "updater archive is a valid gzip tar" tar -tzf "$app_tgz"
plist_member="$(tar -tzf "$app_tgz" | grep -m1 '\.app/Contents/Info\.plist$')"
check "updater archive contains a .app/Contents/Info.plist" test -n "$plist_member"
tar_version="$(tar -xOzf "$app_tgz" "$plist_member" 2>/dev/null | plutil -extract CFBundleShortVersionString raw - 2>/dev/null)"
expected_version="${RELEASE#v}"
[[ -n "$expected_version" ]] || expected_version="$(basename "$dmg")"
# shellcheck disable=SC2016  # $1/$2 are filled in by the bash -c argv, not by this shell
check "updater version matches the release version" \
  bash -c '[[ -n "$1" && "$2" == *"$1"* ]]' bash "$tar_version" "$expected_version"
fi

# ------------------------------------------------------------------- app
APP="$app_in_dmg"
plist="$APP/Contents/Info.plist"
check "Info.plist exists" test -f "$plist"
# shellcheck disable=SC2016
check "bundle identifier is $APP_ID" \
  bash -c 'plutil -extract CFBundleIdentifier raw "$1" | grep -qx "$2"' bash "$plist" "$APP_ID"
# shellcheck disable=SC2016
check "bundle version matches the artifact name" \
  bash -c 'v="$(plutil -extract CFBundleShortVersionString raw "$1")"; [[ -n "$v" && "$2" == *"$v"* ]]' bash "$plist" "$dmg"
exe_rel="$(plutil -extract CFBundleExecutable raw "$plist" 2>/dev/null)"
exe="$APP/Contents/MacOS/$exe_rel"
check "bundle executable exists and is executable" test -n "$exe_rel" -a -x "$exe"

# shellcheck disable=SC2016
check "universal binary: arm64 + x86_64 slices present" \
  bash -c 'lipo -archs "$1" | tr " " "\n" | sort | tr "\n" " " | grep -q "arm64 x86_64"' bash "$exe"

# Unsigned-by-design round: assert the signature is ad-hoc (the app
# would be killed by the kernel without one), NOT a Developer ID.
sig="$(codesign -dv "$APP" 2>&1 | grep -m1 'Signature=')"
# shellcheck disable=SC2016
check "code signature is ad-hoc" \
  bash -c 'grep -q "Signature=adhoc" <<< "$1"' bash "$sig"
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

# --------------------------------------------------------------- evidence
json_escape() { # minimal JSON string escaping (backslash + double quote)
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  printf '%s' "$s"
}

{
  printf '{\n'
  printf '  "schema": "mailchat-vault/smoke-macos/1",\n'
  if [[ "$FAIL" -eq 0 ]]; then printf '  "result": "PASS",\n'; else printf '  "result": "FAIL",\n'; fi
  printf '  "started_utc": "%s",\n' "$(json_escape "$STARTED_UTC")"
  printf '  "finished_utc": "%s",\n' "$(json_escape "$(date -u +%Y-%m-%dT%H:%M:%SZ)")"
  printf '  "host": { "os": "%s", "os_version": "%s", "arch": "%s" },\n' \
    "$(json_escape "$HOST_OS")" "$(json_escape "$HOST_VER")" "$(json_escape "$HOST_ARCH")"
  printf '  "repo": { "commit": "%s" },\n' "$(json_escape "$REPO_COMMIT")"
  printf '  "tester": "%s",\n' "$(json_escape "$TESTER")"
  if [[ -n "$RELEASE" ]]; then
    printf '  "release": "%s",\n' "$(json_escape "$RELEASE")"
  else
    printf '  "release": null,\n'
  fi
  printf '  "alive_seconds": %s,\n' "$ALIVE_SECONDS"
  printf '  "skip_updater": %s,\n' "$SKIP_UPDATER"
  printf '  "artifacts": [\n'
  printf '    { "name": "%s", "sha256": "%s" }' "$(json_escape "$(basename "$dmg")")" "$(json_escape "$dmg_sha")"
  if [[ -n "$app_tgz" ]]; then
    printf ',\n    { "name": "%s", "sha256": "%s" }\n' "$(json_escape "$(basename "$app_tgz")")" "$(json_escape "$app_tgz_sha")"
  else
    printf '\n'
  fi
  printf '  ],\n'
  printf '  "checks": [\n'
  i=0
  while [[ "$i" -lt "${#CHECK_NAMES[@]}" ]]; do
    printf '    { "name": "%s", "status": "%s" }' "$(json_escape "${CHECK_NAMES[$i]}")" "${CHECK_STATUSES[$i]}"
    i=$((i+1))
    if [[ "$i" -lt "${#CHECK_NAMES[@]}" ]]; then printf ',\n'; else printf '\n'; fi
  done
  printf '  ],\n'
  printf '  "passed": %s,\n' "$PASS"
  printf '  "failed": %s\n' "$FAIL"
  printf '}\n'
} > "$json"

# --------------------------------------------------------------- summary
echo
echo "==================== macos smoke summary ===================="
echo "tester: $TESTER   started: $STARTED_UTC   host: ${HOST_OS} ${HOST_VER} (${HOST_ARCH})"
echo "commit: $REPO_COMMIT"
echo "artifact: $(basename "$dmg") sha256=${dmg_sha}"
[[ -n "$app_tgz" ]] && echo "artifact: $(basename "$app_tgz") sha256=${app_tgz_sha}"
echo "passed: $PASS   failed: $FAIL   (log: $log)"
echo "evidence json: $json"
if [[ "$FAIL" -eq 0 ]]; then echo "RESULT: PASS"; else echo "RESULT: FAIL"; fi
echo "==============================================================="
[[ "$FAIL" -eq 0 ]]

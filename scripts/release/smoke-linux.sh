#!/usr/bin/env bash
# Linux release smoke — runs the deb and AppImage suites for a bundle
# set against clean Ubuntu 22.04 and 24.04 containers.
#
# This is the M6.3 acceptance gate:
#   AC1  smoke passes on both 22.04 and 24.04
#   AC2  .desktop file + icon installed with the deb (verified in-suite)
#   AC3  AppImage works on clean 22.04 without additional dependencies
#
# Usage:
#   scripts/release/smoke-linux.sh [--bundles DIR] [--versions "22.04 24.04"]
#                                  [--suites "deb appimage"] [--logs DIR]
#
#   --bundles  where the .deb / .AppImage live
#              (default: <repo>/target/release/bundle)
#   --logs     directory for per-run logs
#              (default: <repo>/target/release/smoke-logs)
#
# Requires docker. Build the bundles first:
#   scripts/release/build-linux.sh
#
# Note on container security options: the appimage/deb containers run
# with --security-opt seccomp=unconfined because Ubuntu 24.04's
# WebKitGTK (pulled in by the deb's declared dependencies) uses a
# bubblewrap sandbox that needs unprivileged user namespaces, which
# docker's default seccomp profile blocks. This only relaxes the
# ephemeral smoke container, not the app.
set -uo pipefail

here="$(cd "$(dirname "$BASH_SOURCE")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"

BUNDLES="$repo/target/release/bundle"
VERSIONS="22.04 24.04"
SUITES="deb appimage"
LOGS="$repo/target/release/smoke-logs"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bundles)  BUNDLES="$2"; shift 2 ;;
    --versions) VERSIONS="$2"; shift 2 ;;
    --suites)   SUITES="$2"; shift 2 ;;
    --logs)     LOGS="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

debs="$(find "$BUNDLES" -name '*.deb' 2>/dev/null | head -1)"
appimages="$(find "$BUNDLES" -name '*.AppImage' 2>/dev/null | head -1)"
if [[ -z "$debs" || -z "$appimages" ]]; then
  missing=()
  [[ -z "$debs" ]] && missing+=('.deb')
  [[ -z "$appimages" ]] && missing+=('.AppImage')
  echo "error: no ${missing[*]} bundle found under $BUNDLES" >&2
  echo "build them first: scripts/release/build-linux.sh" >&2
  exit 2
fi
echo "bundles: $debs"
echo "         $appimages"

mkdir -p "$LOGS"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
overall=0
summary=()

for version in $VERSIONS; do
  image="mailchat-vault-smoke:${version}"
  echo
  echo "==== preparing clean ubuntu:${version} smoke image"
  if ! docker build -q --build-arg UBUNTU_VERSION="$version" \
        -f "$here/Dockerfile.smoke" -t "$image" "$here/smoke" \
        >"$LOGS/image-${version}-${stamp}.log" 2>&1; then
    echo "error: failed to build smoke image for ${version}; see $LOGS/image-${version}-${stamp}.log" >&2
    overall=1
    summary+=("image ${version}: BUILD-FAIL")
    continue
  fi

  for suite in $SUITES; do
    log="$LOGS/${suite}-${version}-${stamp}.log"
    echo
    echo "==== suite '$suite' on ubuntu:${version} (log: $log)"
    # --security-opt seccomp=unconfined: see note at top of file.
    if docker run --rm \
         --security-opt seccomp=unconfined \
         -e ARTIFACTS=/artifacts \
         -v "$BUNDLES:/artifacts:ro" \
         -v "$here/smoke:/smoke-scripts:ro" \
         "$image" \
         bash "/smoke-scripts/${suite}.sh" 2>&1 | tee "$log"; then
      summary+=("${suite} ${version}: PASS")
    else
      summary+=("${suite} ${version}: FAIL")
      overall=1
    fi
  done
done

echo
echo "==================== smoke summary ===================="
printf '%s\n' "${summary[@]}"
echo "======================================================="
exit "$overall"

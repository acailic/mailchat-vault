#!/usr/bin/env bash
# matrix-linux — ONE COMMAND for the entire Linux column of the release
# validation matrix (roadmap M6.4; see docs/validation-and-release.md,
# "Release validation matrix").
#
# Runs, in order, everything the matrix's Linux column names, each leg
# inside the pinned container it belongs to so the result never depends
# on what happens to be installed on the host (docker + git only):
#
#   suite    (mailchat-vault-suite:24.04, Dockerfile.suite)
#              npm ci; npm run check; npm test; npm run build;
#              cargo fmt --all -- --check;
#              cargo clippy --workspace --all-targets -- -D warnings;
#              cargo test --workspace;
#              cargo build -p mailchat-vault --features tauri/custom-protocol
#   e2e      (mailchat-vault-e2e:24.04, Dockerfile.e2e)
#              xvfb-run -a --server-args="-screen 0 1280x960x24" \
#                npm run test:e2e
#            (against the binary/dist the suite leg just built — every
#             spec gets a fresh throwaway data dir; see tests/e2e/README)
#   bundles  (scripts/release/build-linux.sh)
#              pinned 22.04 toolchain container builds the real release
#              AppImage + deb into target/release/bundle/
#   smoke    (scripts/release/smoke-linux.sh)
#              deb + AppImage suites on clean ubuntu 22.04 AND 24.04
#
# The suite and e2e legs share the mounted repo (node_modules/, dist/,
# target/ survive between them) and the mailchat-build-cargo cache
# volume, so the second leg reuses the first one's compilation.
#
# Usage:
#   scripts/release/matrix-linux.sh [--logs DIR]
#                                   [--skip-suite|--skip-e2e|
#                                    --skip-bundles|--skip-smoke]
#
# A leg skipped with --skip-* is reported as SKIPPED in the summary and
# does not fail the run; use the flags to iterate on one leg, never to
# certify a release (a certification run uses no skip flags).
#
# Logs land per leg under --logs (default
# <repo>/target/release/matrix-logs/<timestamp>/). Exit status is 0 only
# when every requested leg passed.
set -uo pipefail

here="$(cd "$(dirname "$BASH_SOURCE")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"

LOGS="$repo/target/release/matrix-logs/$(date -u +%Y%m%dT%H%M%SZ)"
SKIP_SUITE=0
SKIP_E2E=0
SKIP_BUNDLES=0
SKIP_SMOKE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --logs)         LOGS="$2"; shift 2 ;;
    --skip-suite)   SKIP_SUITE=1; shift ;;
    --skip-e2e)     SKIP_E2E=1; shift ;;
    --skip-bundles) SKIP_BUNDLES=1; shift ;;
    --skip-smoke)   SKIP_SMOKE=1; shift ;;
    *) echo "usage: $0 [--logs DIR] [--skip-suite] [--skip-e2e] [--skip-bundles] [--skip-smoke]" >&2; exit 2 ;;
  esac
done

command -v docker >/dev/null || { echo "error: docker not found" >&2; exit 2; }
docker info >/dev/null 2>&1 || { echo "error: docker daemon unreachable" >&2; exit 2; }

mkdir -p "$LOGS"

SUITE_IMAGE="mailchat-vault-suite:24.04"
E2E_IMAGE="mailchat-vault-e2e:24.04"

# Run as the invoking user's uid/gid so files the containers write into
# the mounted repo (node_modules/, dist/, target/) are owned by the
# host user, not root — same contract as scripts/release/build-linux.sh.
run_uid="$(id -u)"
run_gid="$(id -g)"

overall=0
declare -a summary=()

record() { # record <leg> <status> — append to the end-of-run summary
  summary+=("$(printf '%-10s %s' "$1" "$2")")
}

# ------------------------------------------------------------- helper

# Run a docker container with the standard matrix mounts.
#   matrix_container <image> <log> -- <command...>
matrix_container() {
  local image="$1" log="$2"; shift 2
  [[ "${1:-}" == "--" ]] && shift
  docker run --rm \
    -u "${run_uid}:${run_gid}" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -v "$repo":/src \
    -w /src \
    -v mailchat-build-cargo:/cargo \
    "$image" \
    "$@" 2>&1 | tee "$log"
  return "${PIPESTATUS[0]}"
}

# Like matrix_container, but the container starts as root (the command
# itself is responsible for dropping privileges where needed) — used by
# the e2e leg to plant its root-owned hostile state.
matrix_container_root() {
  local image="$1" log="$2"; shift 2
  [[ "${1:-}" == "--" ]] && shift
  docker run --rm \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -v "$repo":/src \
    -w /src \
    -v mailchat-build-cargo:/cargo \
    "$image" \
    "$@" 2>&1 | tee "$log"
  return "${PIPESTATUS[0]}"
}

echo "== matrix-linux: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "   repo:   $repo @ $(git -C "$repo" rev-parse --short HEAD) ($(git -C "$repo" branch --show-current 2>/dev/null || echo detached))"
echo "   host:   $(uname -sr) ($(uname -m)), $(nproc) cpus"
echo "   logs:   $LOGS"
{
  echo "matrix-linux $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "repo $(git -C "$repo" rev-parse HEAD) ($(git -C "$repo" branch --show-current 2>/dev/null || echo detached))"
  echo "host $(uname -sr) $(uname -m) $(nproc)cpus"
  docker --version
} > "$LOGS/environment.txt"

# ---------------------------------------------------------------- images

need_build=()
if [[ "$SKIP_SUITE" -eq 0 ]] && ! docker image inspect "$SUITE_IMAGE" >/dev/null 2>&1; then
  need_build+=("$SUITE_IMAGE")
fi
if [[ "$SKIP_E2E" -eq 0 ]]; then
  # e2e builds FROM the suite image, so a missing suite image is also an
  # e2e-image prerequisite.
  if ! docker image inspect "$SUITE_IMAGE" >/dev/null 2>&1; then
    [[ " ${need_build[*]} " == *" $SUITE_IMAGE "* ]] || need_build+=("$SUITE_IMAGE")
  fi
  if ! docker image inspect "$E2E_IMAGE" >/dev/null 2>&1; then
    need_build+=("$E2E_IMAGE")
  fi
fi
if [[ "${#need_build[@]}" -gt 0 ]]; then
  echo
  echo "==== preparing matrix images: ${need_build[*]}"
  if [[ " ${need_build[*]} " == *" $SUITE_IMAGE "* ]]; then
    echo "-- building $SUITE_IMAGE (first run pulls the toolchain; later runs are cached)"
    if docker build -q -f "$here/Dockerfile.suite" -t "$SUITE_IMAGE" "$here" \
         >"$LOGS/image-suite.log" 2>&1; then
      echo "   $SUITE_IMAGE: built"
    else
      echo "error: building $SUITE_IMAGE failed; see $LOGS/image-suite.log" >&2
      record "images" "FAIL ($SUITE_IMAGE build)"
      printf '%s\n' "${summary[@]}"
      exit 1
    fi
  fi
  if [[ " ${need_build[*]} " == *" $E2E_IMAGE "* ]]; then
    echo "-- building $E2E_IMAGE (installs tauri-driver; cached after the first run)"
    if docker build -q -f "$here/Dockerfile.e2e" -t "$E2E_IMAGE" "$here" \
         >"$LOGS/image-e2e.log" 2>&1; then
      echo "   $E2E_IMAGE: built"
    else
      echo "error: building $E2E_IMAGE failed; see $LOGS/image-e2e.log" >&2
      record "images" "FAIL ($E2E_IMAGE build)"
      printf '%s\n' "${summary[@]}"
      exit 1
    fi
  fi
  record "images" "ok"
fi

# ----------------------------------------------------------------- suite

if [[ "$SKIP_SUITE" -eq 1 ]]; then
  record "suite" "SKIPPED (--skip-suite)"
else
  echo
  echo "==== leg 1/4: full test suite (in $SUITE_IMAGE)"
  if matrix_container "$SUITE_IMAGE" "$LOGS/suite.log" -- bash -eu -c '
        set -o pipefail
        step() { echo; echo "== [suite] $1"; }
        step "npm ci"
        npm ci --no-audit --no-fund
        step "npm run check (tsc)"
        npm run check
        step "npm test (vitest)"
        npm test
        step "npm run build (frontend dist for the embedded app)"
        npm run build
        step "cargo fmt --all -- --check"
        cargo fmt --all -- --check
        step "cargo clippy --workspace --all-targets -- -D warnings"
        cargo clippy --workspace --all-targets -- -D warnings
        step "cargo test --workspace"
        cargo test --workspace
        step "cargo build -p mailchat-vault --features tauri/custom-protocol"
        cargo build -p mailchat-vault --features tauri/custom-protocol
      '; then
    record "suite" "PASS"
  else
    record "suite" "FAIL (see $LOGS/suite.log)"
    overall=1
  fi
fi

# ------------------------------------------------------------------- e2e

if [[ "$SKIP_E2E" -eq 1 ]]; then
  record "e2e" "SKIPPED (--skip-e2e)"
else
  echo
  echo "==== leg 2/4: desktop e2e under xvfb (in $E2E_IMAGE)"
  echo "              (binary + dist/ reused from the suite leg; every spec gets a fresh data dir)"
  # The container starts as root only so it can plant the one hostile
  # state an unprivileged runner cannot create: a root-owned 0777
  # directory at the media preview root. Both e2e passes themselves run
  # as the invoking host user (setpriv), like every other matrix leg.
  #   pass A: the whole suite — the media-foreign-dir spec skips itself
  #           without its planted root (same pattern as the notify specs
  #           without their bus capture)
  #   pass B: that spec alone, against the freshly planted foreign root
  if matrix_container_root "$E2E_IMAGE" "$LOGS/e2e.log" -- bash -eu -c '
        run_as() {
          setpriv --reuid='"$run_uid"' --regid='"$run_gid"' --clear-groups \
            env HOME=/tmp PATH="$PATH" "$@"
        }
        echo "== [e2e] pass A: full suite"
        run_as xvfb-run -a --server-args="-screen 0 1280x960x24" npm run test:e2e
        echo
        echo "== [e2e] pass B: foreign-owned preview root spec (planted as root)"
        rm -rf /tmp/mailchat-media-open
        install -d -m 0777 /tmp/mailchat-media-open
        run_as env MAILCHAT_E2E_ONLY=media-foreign \
          xvfb-run -a --server-args="-screen 0 1280x960x24" npm run test:e2e
      '; then
    record "e2e" "PASS (full suite + foreign-preview-root pass)"
  else
    record "e2e" "FAIL (see $LOGS/e2e.log)"
    overall=1
  fi
fi

# --------------------------------------------------------------- bundles

if [[ "$SKIP_BUNDLES" -eq 1 ]]; then
  record "bundles" "SKIPPED (--skip-bundles)"
else
  echo
  echo "==== leg 3/4: release bundles (build-linux.sh — pinned 22.04 toolchain)"
  if "$here/build-linux.sh" >"$LOGS/bundles.log" 2>&1; then
    echo "   built:"
    ls -1 "$repo"/target/release/bundle/appimage/*.AppImage \
          "$repo"/target/release/bundle/deb/*.deb 2>/dev/null | sed 's/^/     /'
    record "bundles" "PASS"
  else
    echo "error: bundle build failed; see $LOGS/bundles.log" >&2
    record "bundles" "FAIL (see $LOGS/bundles.log)"
    overall=1
  fi
fi

# ----------------------------------------------------------------- smoke

if [[ "$SKIP_SMOKE" -eq 1 ]]; then
  record "smoke" "SKIPPED (--skip-smoke)"
else
  echo
  echo "==== leg 4/4: deb + AppImage smoke on clean ubuntu 22.04 and 24.04"
  if "$here/smoke-linux.sh" --logs "$LOGS/smoke" 2>&1 | tee "$LOGS/smoke.log"; then
    record "smoke" "PASS"
  else
    record "smoke" "FAIL (see $LOGS/smoke.log)"
    overall=1
  fi
fi

# --------------------------------------------------------------- summary

echo
echo "==================== Linux matrix summary ===================="
printf '%s\n' "${summary[@]}"
echo "logs: $LOGS"
if [[ "$overall" -eq 0 ]]; then
  echo "RESULT: PASS — Linux column green"
else
  echo "RESULT: FAIL — at least one leg failed"
fi
echo "==============================================================="
exit "$overall"

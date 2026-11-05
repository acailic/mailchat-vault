#!/usr/bin/env bash
# matrix-macos — the macOS column of the release validation matrix
# (roadmap M6.4; see docs/validation-and-release.md, "Release
# validation matrix").
#
# MUST RUN ON macOS — it builds and launches the real macOS artifacts
# (hdiutil/codesign/lipo); the Linux matrix harness cannot substitute
# for it (mirroring the rule in scripts/release/smoke-macos.sh). Run it
# on any stock mac with nothing preinstalled beyond the OS plus the
# documented prerequisites (node 22, rust stable, Xcode command line
# tools; `git` comes with the CLT).
#
# Legs, in order:
#
#   suite    the full test suite, host toolchain (no container — this
#            is the same command set as the CI `test` job / the Linux
#            suite leg):
#              npm ci; npm run check; npm test; npm run build;
#              cargo fmt --all -- --check;
#              cargo clippy --workspace --all-targets -- -D warnings;
#              cargo test --workspace;
#              cargo build -p mailchat-vault --features tauri/custom-protocol
#   bundles  the universal release dmg, exactly the way the release
#            workflow builds it:
#              npm run tauri build -- --target universal-apple-darwin
#                                    --bundles app,dmg
#   smoke    scripts/dmg-smoke-test.sh against the built dmg
#            (mount, copy out the .app, verify universal binary,
#             headless --version/--help), then — when the release-fix
#            tooling is present in the tree — scripts/release/
#            smoke-macos.sh, the deeper launch-stays-alive smoke, run
#            against the same artifact set. When smoke-macos.sh is
#            absent (release-fix tools not yet on this branch), that
#            half is reported WAITING, not failed.
#
# The desktop e2e route (tests/e2e) is Linux-only today — the WebDriver
# stack it drives is the Linux WebKitGTK one — so it is NOT a macOS
# matrix leg; the macOS column of the matrix records it as not
# applicable rather than silently green.
#
# Usage:
#   scripts/release/matrix-macos.sh [--logs DIR]
#                                   [--skip-suite|--skip-bundles|--skip-smoke]
#
# Logs land per leg under --logs (default
# <repo>/target/release/matrix-logs/<timestamp>-macos/). Exit status is
# 0 only when every requested leg passed.
set -uo pipefail

here="$(cd "$(dirname "$BASH_SOURCE")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"

LOGS="$repo/target/release/matrix-logs/$(date -u +%Y%m%dT%H%M%SZ)-macos"
SKIP_SUITE=0
SKIP_BUNDLES=0
SKIP_SMOKE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --logs)         LOGS="$2"; shift 2 ;;
    --skip-suite)   SKIP_SUITE=1; shift ;;
    --skip-bundles) SKIP_BUNDLES=1; shift ;;
    --skip-smoke)   SKIP_SMOKE=1; shift ;;
    *) echo "usage: $0 [--logs DIR] [--skip-suite] [--skip-bundles] [--skip-smoke]" >&2; exit 2 ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: matrix-macos.sh must run on macOS (it builds/launches real macOS artifacts)" >&2
  exit 2
fi

mkdir -p "$LOGS"

overall=0
declare -a summary=()
record() { summary+=("$(printf '%-10s %s' "$1" "$2")"); }

echo "== matrix-macos: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "   repo:  $repo @ $(git -C "$repo" rev-parse --short HEAD)"
echo "   host:  $(sw_vers -productName) $(sw_vers -productVersion) ($(uname -m))"
echo "   logs:  $LOGS"
{
  echo "matrix-macos $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "repo $(git -C "$repo" rev-parse HEAD)"
  sw_vers
  uname -m
  node --version; rustc --version
} > "$LOGS/environment.txt" 2>&1

# ----------------------------------------------------------------- suite

if [[ "$SKIP_SUITE" -eq 1 ]]; then
  record "suite" "SKIPPED (--skip-suite)"
else
  echo
  echo "==== leg 1/3: full test suite (host toolchain)"
  if bash -eu -o pipefail -c '
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
      ' >"$LOGS/suite.log" 2>&1; then
    record "suite" "PASS"
  else
    echo "error: suite leg failed; see $LOGS/suite.log" >&2
    record "suite" "FAIL (see $LOGS/suite.log)"
    overall=1
  fi
fi

# -------------------------------------------------------------- bundles

dmg=""
if [[ "$SKIP_BUNDLES" -eq 1 ]]; then
  record "bundles" "SKIPPED (--skip-bundles)"
else
  echo
  echo "==== leg 2/3: universal release dmg (tauri build, as the release workflow does)"
  if bash -eu -o pipefail -c '
        npm run tauri build -- --target universal-apple-darwin --bundles app,dmg
      ' >"$LOGS/bundles.log" 2>&1; then
    dmg="$(find "$repo/target/universal-apple-darwin/release/bundle/dmg" \
             -maxdepth 1 -name '*.dmg' -print -quit 2>/dev/null)"
    if [[ -n "$dmg" ]]; then
      echo "   built: $dmg"
      record "bundles" "PASS"
    else
      echo "error: tauri build succeeded but no dmg found under target/universal-apple-darwin/" >&2
      record "bundles" "FAIL (no dmg)"
      overall=1
    fi
  else
    echo "error: bundle build failed; see $LOGS/bundles.log" >&2
    record "bundles" "FAIL (see $LOGS/bundles.log)"
    overall=1
  fi
fi

# ---------------------------------------------------------------- smoke

if [[ "$SKIP_SMOKE" -eq 1 ]]; then
  record "smoke" "SKIPPED (--skip-smoke)"
else
  echo
  echo "==== leg 3/3: dmg smoke"
  version="$(node -p "require('$repo/src-tauri/tauri.conf.json').version")"
  if [[ -z "$dmg" ]]; then
    dmg="$(find "$repo/target/universal-apple-darwin/release/bundle/dmg" \
             -maxdepth 1 -name '*.dmg' -print -quit 2>/dev/null)"
  fi
  if [[ -z "$dmg" ]]; then
    echo "error: no dmg to smoke — run the bundles leg (or drop --skip-bundles) first" >&2
    record "smoke" "FAIL (no dmg)"
    overall=1
  else
    if "$repo/scripts/dmg-smoke-test.sh" "$dmg" "$version" \
         >"$LOGS/smoke-dmg.log" 2>&1; then
      record "smoke" "dmg-smoke-test: PASS"
    else
      echo "error: dmg-smoke-test failed; see $LOGS/smoke-dmg.log" >&2
      record "smoke" "dmg-smoke-test: FAIL (see $LOGS/smoke-dmg.log)"
      overall=1
    fi
    # The deeper launch-stays-alive smoke from the release-fix tooling:
    # present only on branches that carry scripts/release/smoke-macos.sh.
    if [[ -x "$here/smoke-macos.sh" ]]; then
      if "$here/smoke-macos.sh" --dir "$(dirname "$dmg")" --skip-updater \
           --logs "$LOGS/smoke-macos" >"$LOGS/smoke-macos.log" 2>&1; then
        record "smoke" "smoke-macos: PASS"
      else
        echo "error: smoke-macos.sh failed; see $LOGS/smoke-macos.log" >&2
        record "smoke" "smoke-macos: FAIL (see $LOGS/smoke-macos.log)"
        overall=1
      fi
    else
      echo "note: scripts/release/smoke-macos.sh not present on this branch —"
      echo "      the launch-stays-alive smoke half is WAITING on the release-fix"
      echo "      tooling landing; only dmg-smoke-test ran."
      record "smoke" "smoke-macos: WAITING (release-fix tools not on this branch)"
    fi
  fi
fi

# -------------------------------------------------------------- summary

echo
echo "==================== macOS matrix summary ====================="
printf '%s\n' "${summary[@]}"
echo "logs: $LOGS"
if [[ "$overall" -eq 0 ]]; then
  echo "RESULT: PASS — requested macOS legs green"
else
  echo "RESULT: FAIL — at least one leg failed"
fi
echo "================================================================"
exit "$overall"

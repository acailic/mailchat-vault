#!/usr/bin/env bash
# release-verify — verify a PUBLISHED GitHub release end-to-end:
#
#   1. download every release asset,
#   2. check each file's sha256 against GitHub's own server-side asset
#      digest (the `sha256:` digests the Releases API reports), so a
#      tampered or truncated download cannot pass,
#   3. write a `sha256sum -c`-compatible SHA256SUMS manifest,
#   4. optionally (--attach) upload SHA256SUMS to the release, and
#   5. optionally (--smoke) run the Linux smoke suites against the
#      downloaded (already digest-verified) artifacts.
#
# This is the local-release half of the release gates: repo Actions are
# currently disabled by decision, so the publish path is local build +
# `gh release`, and this script is what turns "assets are on the
# release" into "assets are on the release AND verified AND smoked".
# Run it after publishing (or against any older release, e.g. to add
# the checksums that v0.1.0 shipped without).
#
# Usage:
#   scripts/release/release-verify.sh <tag> [options]
#
#   --dir DIR     where to download/verify (default: <repo>/target/release/verify/<tag>)
#   --attach      upload the SHA256SUMS manifest to the release (idempotent)
#   --smoke       also run scripts/release/smoke-linux.sh on the downloads
#   --skip-download  verify files already present in --dir (sha256 only)
#
# Requires: gh (authenticated), sha256sum (or shasum -a 256 on macOS).
set -euo pipefail

here="$(cd "$(dirname "$BASH_SOURCE")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"

TAG="${1:-}"
[[ -n "$TAG" ]] || { echo "usage: $0 <tag> [--dir DIR] [--attach] [--smoke] [--skip-download]" >&2; exit 2; }
shift || true

DIR="$repo/target/release/verify/$TAG"
ATTACH=0
SMOKE=0
DOWNLOAD=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dir)           DIR="$2"; shift 2 ;;
    --attach)        ATTACH=1; shift ;;
    --smoke)         SMOKE=1; shift ;;
    --skip-download) DOWNLOAD=0; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

command -v gh >/dev/null || { echo "error: gh not installed/authenticated" >&2; exit 2; }
if command -v sha256sum >/dev/null 2>&1; then
  hash256() { sha256sum "$@"; }
elif command -v shasum >/dev/null 2>&1; then
  hash256() { shasum -a 256 "$@"; }
else
  echo "error: neither sha256sum nor shasum found" >&2
  exit 2
fi

echo "== release-verify: $TAG (repo $(gh repo view --json nameWithOwner -q .nameWithOwner))"
REPO="$(gh repo view --json nameWithOwner -q .nameWithOwner)"

# ------------------------------------------------------- download & digest

mkdir -p "$DIR"
if [[ "$DOWNLOAD" -eq 1 ]]; then
  echo "== downloading assets to $DIR"
  gh release download "$TAG" --dir "$DIR" --clobber
fi

echo "== fetching server-side digests from the Releases API"
gh api "repos/$REPO/releases/tags/$TAG" \
  --jq '.assets[] | select(.name != "SHA256SUMS") | "\(.digest)\t\(.name)"' > "$DIR/.api-digests"

n=0
fail=0
while IFS= read -r line || [[ -n "$line" ]]; do
  if [[ -z "$line" ]]; then continue; fi
  n=$((n+1))
  want="${line%%$'\t'*}"; want="${want#sha256:}"
  name="${line##*$'\t'}"
  file="$DIR/$name"
  if [[ ! -f "$file" ]]; then
    echo "FAIL: $name — not downloaded to $DIR"; fail=1
  else
    got="$(hash256 "$file" | awk '{print $1}')"
    if [[ "$got" == "$want" ]]; then
      echo "PASS: $name (sha256 ${got:0:16}…)"
    else
      echo "FAIL: $name — downloaded sha256 $got != server digest $want"; fail=1
    fi
  fi
done < "$DIR/.api-digests"
[[ "$n" -gt 0 ]] || { echo "error: no assets found on release $TAG" >&2; exit 1; }
[[ "$fail" -eq 0 ]] || { echo "error: digest verification failed" >&2; exit 1; }

# ------------------------------------------------------------- SHA256SUMS

sums="$DIR/SHA256SUMS"
(
  cd "$DIR"
  shopt -s nullglob
  files=( *.AppImage *.deb *.dmg *.msi *.exe *.app.tar.gz )
  [[ "${#files[@]}" -gt 0 ]] && hash256 "${files[@]}" || :
) > "$sums"
[[ -s "$sums" ]] || { echo "error: no release assets found in $DIR" >&2; exit 1; }
echo "== wrote $sums:"
cat "$sums"

if [[ "$ATTACH" -eq 1 ]]; then
  echo "== attaching SHA256SUMS to release $TAG"
  gh release upload "$TAG" "$sums" --clobber
  echo "   https://github.com/$REPO/releases/tag/$TAG"
fi

# ------------------------------------------------------------------ smoke

if [[ "$SMOKE" -eq 1 ]]; then
  echo "== running Linux smoke suites on the verified downloads"
  "$here/smoke-linux.sh" --bundles "$DIR" --logs "$DIR/smoke-logs"
fi

echo "== release-verify: all checks passed for $TAG"

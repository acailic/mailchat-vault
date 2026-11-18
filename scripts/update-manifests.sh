#!/usr/bin/env bash
# update-manifests — rewrite version + sha256 across the package-manager
# manifest kit (packaging/manifests/; P0·04, see docs/distribution.md).
#
# What this script does (and deliberately does not do):
#
#   DOES   - read a release's SHA256SUMS (the format
#            scripts/release/prepare-release.sh writes) either from a
#            local file or straight from the GitHub release via gh
#          - rewrite the placeholder version (0.0.0) and placeholder
#            sha256 (64 zeros) in every manifest: Homebrew cask,
#            winget installer yaml, Scoop json, AUR PKGBUILD, Flatpak
#            stub
#          - do all of that atomically: every new file content is
#            rendered to a temp file first, originals are replaced by
#            mv only after every render succeeded
#          - default is a DRY RUN: print a unified diff, change nothing;
#            --write applies it
#
#   DOES NOT - submit anything to any package manager (homebrew-cask,
#             winget-pkgs, AUR, Scoop buckets, Flathub) — those are
#             owner actions with owner-held credentials
#           - touch release assets, tags, or workflows
#           - lift the signing gate (#78): manifests stay unsubmitted
#             while artifacts are unsigned; the blocked-note comments
#             are preserved (only version/sha256 placeholders change)
#
# Usage:
#   scripts/update-manifests.sh (--version X.Y.Z --checksums <file> |
#                                --from-release vX.Y.Z)
#                               [--write]
#
#   --version X.Y.Z     version whose assets the checksums describe
#   --checksums <file>  SHA256SUMS file (sha256sum -c format, as
#                       written by scripts/release/prepare-release.sh)
#   --from-release tag  fetch SHA256SUMS from the GitHub release via
#                       gh (reads GITHUB_TOKEN) and derive the version
#                       from the vX.Y.Z tag
#   --write             apply the rewrite; without it, print the diff
#                       and change nothing
#
# Requires: bash, awk, diff, mktemp; gh only for --from-release.
set -euo pipefail

here="$(cd "$(dirname "$BASH_SOURCE")" && pwd)"
manifests_dir="$here/../packaging/manifests"
repo_slug="acailic/mailchat-vault"

VERSION=""
CHECKSUMS=""
FROM_RELEASE=""
WRITE=0

usage() {
  echo "usage: $0 (--version X.Y.Z --checksums <file> | --from-release vX.Y.Z) [--write]" >&2
  exit 2
}

fail() { echo "error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)       [[ $# -ge 2 ]] || usage; VERSION="$2"; shift 2 ;;
    --checksums)     [[ $# -ge 2 ]] || usage; CHECKSUMS="$2"; shift 2 ;;
    --from-release)  [[ $# -ge 2 ]] || usage; FROM_RELEASE="$2"; shift 2 ;;
    --write)         WRITE=1; shift ;;
    *)               usage ;;
  esac
done

[[ -d "$manifests_dir" ]] || fail "manifest kit not found at $manifests_dir"

tmp_cleanup() { [[ -n "${TMPDIR_ROOT:-}" ]] && rm -rf "$TMPDIR_ROOT"; return 0; }
trap tmp_cleanup EXIT
TMPDIR_ROOT="$(mktemp -d)"
CHECKSUMS_FILE=""

# --- input resolution --------------------------------------------------------

if [[ -n "$FROM_RELEASE" ]]; then
  [[ -z "$VERSION" && -z "$CHECKSUMS" ]] || usage
  [[ "$FROM_RELEASE" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || fail "--from-release wants a vX.Y.Z tag, got '$FROM_RELEASE'"
  command -v gh >/dev/null 2>&1 || fail "gh is required for --from-release (not installed)"
  VERSION="${FROM_RELEASE#v}"
  CHECKSUMS_FILE="$TMPDIR_ROOT/SHA256SUMS"
  echo "== fetching SHA256SUMS for $FROM_RELEASE via gh =="
  gh release download "$FROM_RELEASE" --repo "$repo_slug" \
    --pattern SHA256SUMS --output "$CHECKSUMS_FILE" \
    || fail "could not download SHA256SUMS for $FROM_RELEASE (does the release exist and carry that asset?)"
elif [[ -n "$VERSION" && -n "$CHECKSUMS" ]]; then
  [[ -f "$CHECKSUMS" ]] || fail "checksums file '$CHECKSUMS' does not exist"
  CHECKSUMS_FILE="$CHECKSUMS"
else
  usage
fi

[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || fail "version must be X.Y.Z, got '$VERSION'"

# --- checksum extraction -----------------------------------------------------
# The manifests consume three assets (docs/download.md): the universal
# dmg (Homebrew), the x64 MSI (winget, Scoop), and the amd64 .deb (AUR,
# Flatpak stub). Everything else in SHA256SUMS is ignored.
sha_for() {
  local asset="$1" sha
  sha="$(awk -v a="$asset" '
    { f = $2; sub(/^\*/, "", f) }
    f == a && !done { print $1; done = 1 }
    END { exit(done ? 0 : 1) }
  ' "$CHECKSUMS_FILE")" || fail "SHA256SUMS has no entry for '$asset' (release $VERSION)"
  sha="${sha,,}"
  [[ "$sha" =~ ^[0-9a-f]{64}$ ]] || fail "entry for '$asset' is not a sha256: '$sha'"
  printf '%s' "$sha"
}

DMG_ASSET="mailchat-vault_${VERSION}_universal.dmg"
MSI_ASSET="mailchat-vault_${VERSION}_x64_en-US.msi"
DEB_ASSET="mailchat-vault_${VERSION}_amd64.deb"

SHA_DMG="$(sha_for "$DMG_ASSET")"
SHA_MSI="$(sha_for "$MSI_ASSET")"
SHA_DEB="$(sha_for "$DEB_ASSET")"
echo "== ${FROM_RELEASE:-v$VERSION} checksums: dmg ${SHA_DMG:0:12}… msi ${SHA_MSI:0:12}… deb ${SHA_DEB:0:12}…"

# --- per-manifest rewrite ----------------------------------------------------
# relpath:ordered asset tags (the order placeholder checksums appear in
# the file). Files without entries only get the version rewritten.
declare -a KIT=(
  "homebrew/mailchat-vault.rb:dmg"
  "winget/acailic.mailchat-vault.yaml:"
  "winget/acailic.mailchat-vault.installer.yaml:msi"
  "winget/acailic.mailchat-vault.locale.en-US.yaml:"
  "scoop/mailchat-vault.json:msi"
  "aur/PKGBUILD:deb"
  "flatpak/com.acailic.mailchatvault.json:deb"
)

PLACEHOLDER_SHA="0000000000000000000000000000000000000000000000000000000000000000"

render_file() {   # $1=file  $2=comma-joined ordered shas ("" = none)
  awk -v newver="$VERSION" \
      -v ph="$PLACEHOLDER_SHA" -v shas_csv="$2" -v fname="$1" '
    BEGIN { n = split(shas_csv, S, ",") }
    {
      line = $0
      gsub(/0\.0\.0/, newver, line)
      while ((i = index(line, ph)) > 0) {
        if (++k > n) {
          printf "error: %s: more placeholder checksums than assets\n", fname > "/dev/stderr"
          exit 3
        }
        line = substr(line, 1, i - 1) S[k] substr(line, i + 64)
      }
      print line
    }
    END {
      if (n > 0 && k == 0)
        printf "warning: %s: no placeholder checksum found (already updated?)\n", fname > "/dev/stderr"
    }
  ' "$1"
}

shas_for_tag() {
  local tags="$1" tag out="" tag_list
  IFS=',' read -ra tag_list <<< "$tags"
  for tag in "${tag_list[@]}"; do
    case "$tag" in
      dmg) out+="${out:+,}$SHA_DMG" ;;
      msi) out+="${out:+,}$SHA_MSI" ;;
      deb) out+="${out:+,}$SHA_DEB" ;;
      *)   fail "unknown asset tag '$tag'" ;;
    esac
  done
  printf '%s' "$out"
}

declare -a targets=() temps=()
for entry in "${KIT[@]}"; do
  idx="${#targets[@]}"
  rel="${entry%%:*}"; tags="${entry#*:}"
  file="$manifests_dir/$rel"
  [[ -f "$file" ]] || fail "manifest missing from kit: $rel"
  new="$TMPDIR_ROOT/new.$idx"
  err="$TMPDIR_ROOT/err.$idx"
  if ! render_file "$file" "$(shas_for_tag "$tags")" > "$new" 2> "$err"; then
    cat "$err" >&2
    fail "rendering failed for $rel — nothing was written"
  fi
  cat "$err" >&2
  targets+=("$rel"); temps+=("$new")
done

# --- dry-run diff or atomic write ---------------------------------------------
if [[ "$WRITE" != 1 ]]; then
  echo "== dry run: diff (nothing is written; pass --write to apply) =="
  for idx in "${!targets[@]}"; do
    rel="${targets[$idx]}"
    if diff -u --label "a/$rel" --label "b/$rel" \
         "$manifests_dir/$rel" "${temps[$idx]}"; then
      echo "  $rel: no changes"
    fi
  done
  echo "== dry run complete: no files changed =="
else
  echo "== write: updating ${#targets[@]} manifests (temp file + move) =="
  for idx in "${!targets[@]}"; do
    rel="${targets[$idx]}"
    dest="$manifests_dir/$rel"
    staged="${dest}.tmp.$$"
    cp "${temps[$idx]}" "$staged"
    mv "$staged" "$dest"
    echo "  updated: $rel"
  done
  echo "== done: review with git diff, then commit (publication stays an owner action; docs/distribution.md) =="
fi

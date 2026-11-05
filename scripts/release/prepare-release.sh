#!/usr/bin/env bash
# prepare-release — assemble a LOCAL draft-release candidate for the next
# version (roadmap M6.5; see docs/release.md, "Preparing a release").
#
# What this script does (and deliberately does not do):
#
#   DOES   - verify the version is bumped consistently everywhere and that
#            no v<version> tag exists yet (preparation must precede tagging)
#          - optionally run the full release validation matrix for this
#            host's platform (the release-verify plan, M6.4)
#          - build the Linux release bundles (docker-pinned toolchain)
#          - write SHA256SUMS for every produced bundle
#          - render RELEASE-NOTES.md from the version's CHANGELOG.md
#            section plus the standing unsigned-artifacts disclosure
#          - write PUBLISH-CHECKLIST.md — the exact remaining steps,
#            gated on the captain's open (a)/(b)/(c) host/Actions decision
#            (docs/validation-and-release.md, "Release validation matrix")
#
#   DOES NOT - create or push any git tag
#            - call `gh release` (create/publish/upload) in any form
#            - enable or dispatch any GitHub Actions workflow
#
#   Publishing stays a human decision by design (M6 exit rule); this
#   script only makes sure that decision can be executed in one sitting.
#
# Everything lands under target/release/draft-release/v<version>/ —
# git-ignored build output, never committed.
#
# Usage:
#   scripts/release/prepare-release.sh [--version X.Y.Z]
#                                      [--skip-matrix] [--skip-bundles]
#
#   --skip-matrix    do not run the validation matrix (iteration only —
#                    a release candidate is never certified without it)
#   --skip-bundles   do not build bundles; assemble notes/checksums from
#                    the bundles already in any target/*/release/bundle/
#                    (they must exist and match the version)
#
# Requires: git, jq, sha256sum, and — unless both skips are passed —
# docker (the matrix and bundle legs run in pinned containers).
set -euo pipefail

here="$(cd "$(dirname "$BASH_SOURCE")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"

VERSION=""
SKIP_MATRIX=0
SKIP_BUNDLES=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)      VERSION="$2"; shift 2 ;;
    --skip-matrix)  SKIP_MATRIX=1; shift ;;
    --skip-bundles) SKIP_BUNDLES=1; shift ;;
    *) echo "usage: $0 [--version X.Y.Z] [--skip-matrix] [--skip-bundles]" >&2; exit 2 ;;
  esac
done

fail() { echo "error: $*" >&2; exit 1; }

# Every bundle root the platform builds actually write: build-linux.sh
# and the Windows matrix write target/release/bundle/, the macOS matrix
# builds the universal dmg under the per-target
# target/universal-apple-darwin/release/bundle/.
find_bundles() {
  local -a roots=("$repo/target/release/bundle")
  local d
  for d in "$repo"/target/*/release/bundle; do
    [[ -d "$d" ]] && roots+=("$d")
  done
  find "${roots[@]}" \
    \( -name "mailchat-vault_${VERSION}_*.AppImage" \
    -o -name "mailchat-vault_${VERSION}_*.deb" \
    -o -name "mailchat-vault_${VERSION}_*.dmg" \
    -o -name "mailchat-vault_${VERSION}_*.app.tar.gz" \
    -o -name "mailchat-vault_${VERSION}_*.msi" \
    -o -name "mailchat-vault_${VERSION}_*-setup.exe" \) -print0 2>/dev/null
}

# --- version resolution -----------------------------------------------------
# The version lives in four places that must always agree:
# src-tauri/tauri.conf.json, the workspace [workspace.package] version in
# Cargo.toml (both crates inherit it), package.json, and the two workspace
# package entries Cargo.lock carries.
version_from_conf() { jq -r .version "$repo/src-tauri/tauri.conf.json"; }

if [[ -z "$VERSION" ]]; then
  VERSION="$(version_from_conf)"
fi

DRAFT_DIR="$repo/target/release/draft-release/v$VERSION"

verify_stage() {
  echo "== verify: version consistency =="
  local conf cargo pkg lock_app lock_core
  conf="$(version_from_conf)"
  cargo="$(awk '/^\[workspace\.package\]/{f=1;next} /^\[/{f=0} f && /^version = /{sub(/^version = "/,""); sub(/"$/,""); print; exit}' "$repo/Cargo.toml")"
  pkg="$(jq -r .version "$repo/package.json")"
  lock_app="$(awk '/^name = "mailchat-vault"$/{getline; print; exit}' "$repo/Cargo.lock" | sed 's/version = "\(.*\)"/\1/')"
  lock_core="$(awk '/^name = "vault-core"$/{getline; print; exit}' "$repo/Cargo.lock" | sed 's/version = "\(.*\)"/\1/')"

  [[ "$conf" == "$VERSION" ]]    || fail "src-tauri/tauri.conf.json says '$conf', expected '$VERSION'"
  [[ "$cargo" == "$VERSION" ]]   || fail "Cargo.toml [workspace.package] says '$cargo', expected '$VERSION'"
  [[ "$pkg" == "$VERSION" ]]     || fail "package.json says '$pkg', expected '$VERSION'"
  [[ "$lock_app" == "$VERSION" ]]  || fail "Cargo.lock mailchat-vault says '$lock_app', expected '$VERSION'"
  [[ "$lock_core" == "$VERSION" ]] || fail "Cargo.lock vault-core says '$lock_core', expected '$VERSION'"

  echo "  version $VERSION consistent in tauri.conf.json, Cargo.toml, package.json, Cargo.lock"

  echo "== verify: changelog section =="
  grep -q "^## \[$VERSION\]" "$repo/CHANGELOG.md" \
    || fail "CHANGELOG.md has no '## [$VERSION]' section"
  echo "  CHANGELOG.md carries a [$VERSION] section"

  echo "== verify: no tag yet =="
  if git -C "$repo" rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
    fail "tag v$VERSION already exists — preparation must precede tagging; bump to the next version or delete the tag deliberately"
  fi
  echo "  no v$VERSION tag exists (correct: tagging is a separate, gated step)"
}

matrix_stage() {
  echo "== matrix: release validation column for this host =="
  case "$(uname -s)" in
    Linux)
      echo "  running scripts/release/matrix-linux.sh (full Linux column)"
      "$repo/scripts/release/matrix-linux.sh"
      ;;
    Darwin)
      echo "  running scripts/release/matrix-macos.sh (needs a real macOS host)"
      "$repo/scripts/release/matrix-macos.sh"
      ;;
    *)
      fail "no matrix column for '$(uname -s)' — run matrix-linux.sh on Linux or matrix-macos.sh/matrix-windows.ps1 on their hosts"
      ;;
  esac
  echo "  matrix PASSED — this platform's column is green for v$VERSION"
}

bundles_stage() {
  echo "== bundles: Linux release bundles =="
  echo "  building via scripts/release/build-linux.sh (pinned 22.04 toolchain container)"
  "$repo/scripts/release/build-linux.sh"
}

assemble_stage() {
  echo "== assemble: local draft-release candidate =="
  mkdir -p "$DRAFT_DIR"
  # drop this script's own generated artifacts, keep everything else —
  # under the (b) flow, bundles copied here from other hosts must
  # survive a re-run so one combined SHA256SUMS can be written
  rm -f "$DRAFT_DIR"/SHA256SUMS "$DRAFT_DIR"/RELEASE-NOTES.md \
        "$DRAFT_DIR"/PUBLISH-CHECKLIST.md "$DRAFT_DIR"/MATRIX-RUN.md
  if compgen -G "$DRAFT_DIR/mailchat-vault_${VERSION}_*" >/dev/null; then
    echo "  note: existing mailchat-vault_${VERSION}_* assets in the draft dir are kept and included in the combined checksums"
  fi

  # 1. copy the bundles
  local -a found=()
  while IFS= read -r -d '' f; do found+=("$f"); done < <(find_bundles)
  if [[ ${#found[@]} -eq 0 ]]; then
    fail "no v$VERSION bundles found in any target/*/release/bundle/ — run without --skip-bundles first (or build the platform bundles on their host)"
  fi
  for f in "${found[@]}"; do
    cp "$f" "$DRAFT_DIR/"
    echo "  asset: $(basename "$f")"
  done

  # 2. checksums over exactly what this candidate ships
  ( cd "$DRAFT_DIR" && sha256sum mailchat-vault_"$VERSION"_* > SHA256SUMS )
  echo "  wrote SHA256SUMS ($(wc -l < "$DRAFT_DIR/SHA256SUMS") entries)"

  # 3. release notes: the version's changelog section + the standing
  #    unsigned-artifacts disclosure (same facts the release workflow's
  #    RELEASE_BODY carries, kept in one place here)
  awk -v ver="$VERSION" '
    on && /^## \[/ { exit }
    $0 == "## [" ver "]" || index($0, "## [" ver "] ") == 1 { on=1 }
    on { print }
  ' "$repo/CHANGELOG.md" > "$DRAFT_DIR/RELEASE-NOTES.md"
  [[ -s "$DRAFT_DIR/RELEASE-NOTES.md" ]] || fail "could not extract the [$VERSION] section from CHANGELOG.md"
  cat >> "$DRAFT_DIR/RELEASE-NOTES.md" <<'NOTES'

---

## Install & smoke-test notes

See the assets below and `docs/release.md` for install and smoke-test
instructions. All artifacts are **unsigned** in this round.

- **Linux** — `AppImage` (self-contained: runs on a clean Ubuntu 22.04+)
  and `.deb` (launcher entry, icons, `.mbox` association).
- **macOS** — open the `.dmg` and drag the app to `/Applications`. The
  app is unsigned (ad-hoc signature only): right-click → *Open* →
  *Open* once, or `xattr -cr /Applications/mailchat-vault.app` first.
- **Windows** — first run shows SmartScreen: *More info* → *Run anyway*.

Checksums for every asset are in `SHA256SUMS`.
NOTES
  echo "  wrote RELEASE-NOTES.md"

  # 4. the publish checklist — every remaining step, all gated on the
  #    captain's (a)/(b)/(c) decision; nothing in it runs from here
  cat > "$DRAFT_DIR/PUBLISH-CHECKLIST.md" <<CHECKLIST
# v$VERSION publish checklist — NOT EXECUTED by prepare-release.sh

This candidate was assembled locally. Nothing has been tagged,
published, or uploaded, and no Actions workflow was enabled or
dispatched. Publishing requires the maintainer decision recorded in
\`docs/validation-and-release.md\` ("Release validation matrix"):

- **(a) defer** — keep the current published release; this local
  candidate stays warm until Actions return. Nothing to run.
- **(b) real hosts** — run \`scripts/release/matrix-macos.sh\` on a real
  macOS host and \`pwsh scripts/release/matrix-windows.ps1\` on a real
  Windows host; the macOS bundle leg produces the dmg under
  \`target/universal-apple-darwin/release/bundle/\`, the Windows leg the
  msi/exe under \`target/release/bundle/\`. This script scans every
  \`target/*/release/bundle/\` root, so re-running it on each host (or
  copying the bundles here) makes every asset land in this directory
  with one combined \`SHA256SUMS\`.
- **(c) temporary Actions** — with release tags approved, pushing the
  tag triggers \`.github/workflows/release.yml\`, which drafts the
  release and builds all six assets itself; then dispatch the Windows
  smoke workflow on the tag.

Whichever path is chosen, the gate order below is the same.

## Gate order (execute by hand, in order)

1. **Stamp the changelog date** — replace \`- pending tag\` in
   \`CHANGELOG.md\`'s \`## [$VERSION]\` heading with the release date,
   commit, and re-run \`scripts/release/prepare-release.sh --version $VERSION\`
   so RELEASE-NOTES.md carries it.
2. **Confirm the matrix** — the matrix run's per-leg logs live in the
   \`target/release/matrix-logs/\` directory the matrix script prints
   when it finishes; under (b), both additional platform columns
   must be green before their assets ship; under (c), the workflow's
   own smoke jobs (macOS dmg smoke, Linux smoke) must be green on the
   tag.
3. **Tag** (only after the go decision):
   \`\`\`sh
   git tag -a v$VERSION -m "Mailchat Vault v$VERSION"
   git push origin v$VERSION        # under (c) this triggers release.yml
   \`\`\`
4. **Draft the release** (under (a)/(b); under (c) the workflow already
   did it):
   \`\`\`sh
   gh release create v$VERSION --draft \\
     --title "Mailchat Vault v$VERSION" \\
     --notes-file "$DRAFT_DIR/RELEASE-NOTES.md"
   gh release upload v$VERSION "$DRAFT_DIR"/mailchat-vault_${VERSION}_* "$DRAFT_DIR/SHA256SUMS" --clobber
   \`\`\`
5. **Smoke the actual release assets** — never certify from local
   bundles alone when the release was assembled by a workflow:
   download from the draft, verify against \`SHA256SUMS\`, and run the
   platform gates (\`smoke-linux.sh\` against the published artifacts;
   \`scripts/dmg-smoke-test.sh <downloaded dmg> $VERSION\`; the Windows
   smoke workflow against the tag).
6. **Publish** — \`gh release edit v$VERSION --draft=false\` — only when
   every gate above is green, with the unsigned-artifacts disclosure
   left in the notes.
CHECKLIST
  echo "  wrote PUBLISH-CHECKLIST.md"
}

verify_stage
if [[ "$SKIP_MATRIX" != 1 ]]; then matrix_stage; else echo "== matrix: SKIPPED (--skip-matrix; not a certification run) =="; fi
if [[ "$SKIP_BUNDLES" != 1 ]]; then bundles_stage; fi
assemble_stage

echo
echo "Draft-release candidate for v$VERSION assembled locally:"
ls -1 "$DRAFT_DIR"
echo
echo "NOT tagged, NOT published, no Actions touched — the remaining"
echo "steps live in $DRAFT_DIR/PUBLISH-CHECKLIST.md and wait on the"
echo "captain's (a)/(b)/(c) host/Actions decision."

#!/usr/bin/env bash
# Build the Linux bundles (AppImage + deb) inside a pinned toolchain
# container, so a release-quality build does not depend on the host
# having the Tauri system libraries installed.
#
# Artifacts land in <repo>/target/release/bundle/{appimage,deb}.
#
# Usage:
#   scripts/release/build-linux.sh [--no-cache]
#
# Requires: docker. The first run builds the toolchain image
# (a few minutes); cargo/npm caches live in the docker volume
# mailchat-build-cargo, so later runs are incremental.
set -euo pipefail

repo="$(git -C "$(dirname "$BASH_SOURCE")" rev-parse --show-toplevel)"
image_tag="mailchat-vault-build:22.04"

no_cache=""
if [[ "${1:-}" == "--no-cache" ]]; then
  no_cache="--no-cache"
fi

docker build $no_cache -f "$repo/scripts/release/Dockerfile.build" -t "$image_tag" "$repo/scripts/release"

# Run as the invoking user's uid/gid so the produced target/ files are
# owned by the host user, not root.
run_uid="$(id -u)"
run_gid="$(id -g)"

docker run --rm \
  -u "${run_uid}:${run_gid}" \
  -e HOME=/tmp \
  -e CARGO_HOME=/cargo \
  -v "$repo:/src" \
  -w /src \
  -v mailchat-build-cargo:/cargo \
  "$image_tag" \
  bash -lc 'npm ci && npm run tauri build -- --bundles appimage,deb'

echo
echo "Bundles:"
ls -1 "$repo"/target/release/bundle/appimage/*.AppImage "$repo"/target/release/bundle/deb/*.deb

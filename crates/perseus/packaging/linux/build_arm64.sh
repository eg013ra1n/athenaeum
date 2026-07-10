#!/usr/bin/env bash
# Builds the headless arm64 .deb + tar.gz in an arm64 Debian container.
#
# Usage: build_arm64.sh <version>   (version stamps the tar.gz filename)
#
# The rust image tag must match the channel pinned in rust-toolchain.toml
# (currently 1.96.1 → rust:1.96-bookworm ships exactly that patch). Bump both
# together. A host-side cargo registry cache is mounted so crate downloads
# survive re-runs (fresh containers otherwise re-fetch the whole index).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"
VERSION="$1"
CARGO_CACHE="${PERSEUS_DEB_CARGO_CACHE:-$HOME/.cache/perseus-deb-cargo}"
mkdir -p "$CARGO_CACHE"
docker run --rm --platform linux/arm64 \
  -v "$ROOT":/work -w /work \
  -v "$CARGO_CACHE":/usr/local/cargo/registry \
  rust:1.96-bookworm bash -c '
  set -euo pipefail
  cargo install cargo-deb --locked
  cd crates/perseus
  cargo deb --variant headless --no-default-features -- -p perseus
  cd /work
  tar czf target/debian/perseus-'"$VERSION"'-linux-arm64.tar.gz -C target/release perseus
'
echo "artifacts in target/debian/"

#!/usr/bin/env bash
# Assert that the six version files agree with a release tag.
# Env: TAG (required, e.g. v0.6.4 or v0.6.4-beta.1); REPO_ROOT (optional).
# tauri.conf.json carries X.Y.Z-N for a beta (its bundler rejects the dotted
# form), so the tag's -beta.N maps to -N there and nowhere else.
set -euo pipefail

: "${TAG:?TAG must be set (the release tag, with its leading v)}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"

VERSION="${TAG#v}"
TAURI_VERSION="$(printf '%s' "$VERSION" | sed -E 's/-beta\.([0-9]+)$/-\1/')"

read_json_version() {  # $1 = file — the top-level "version" key
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$1"
}
read_cargo_version() { # $1 = file — the first `version = "…"` line (the [package] one)
  sed -nE 's/^version = "([^"]+)".*/\1/p' "$1" | head -n1
}

fail=0
check() { # $1 = relative file, $2 = actual, $3 = expected
  echo "$1: $2"
  if [ "$2" != "$3" ]; then
    echo "ERROR: $1 is $2, tag $TAG expects $3" >&2
    fail=1
  fi
}

check "package.json" "$(read_json_version "$REPO_ROOT/package.json")" "$VERSION"
for crate in athenaeum-core athenaeum-tauri athenaeum-web perseus; do
  check "crates/$crate/Cargo.toml" "$(read_cargo_version "$REPO_ROOT/crates/$crate/Cargo.toml")" "$VERSION"
done
check "crates/athenaeum-tauri/tauri.conf.json" "$(read_json_version "$REPO_ROOT/crates/athenaeum-tauri/tauri.conf.json")" "$TAURI_VERSION"

if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "ok: all six versions match $VERSION"

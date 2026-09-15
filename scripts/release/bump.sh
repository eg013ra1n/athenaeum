#!/usr/bin/env bash
# Bump the release version in the six files that carry it, refresh Cargo.lock,
# and prove the six agree. Usage: scripts/release/bump.sh X.Y.Z[-beta.N]
# Env: REPO_ROOT (optional), BUMP_SKIP_CARGO=1 (skip the Cargo.lock refresh; tests).
set -euo pipefail

NEW="${1:-}"
if ! printf '%s' "$NEW" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$'; then
  echo "ERROR: version must look like X.Y.Z or X.Y.Z-beta.N (got '${NEW}')" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TAURI_NEW="$(printf '%s' "$NEW" | sed -E 's/-beta\.([0-9]+)$/-\1/')"

# Line-level rewrites keep each file's formatting byte-for-byte except the
# version itself: the FIRST top-level "version" key in the JSON files (line 5
# in package.json, line 4 in tauri.conf.json) and the FIRST `version = "…"`
# line in each Cargo.toml (the [package] one; dependency tables come later).
rewrite_first() { # $1 = file, $2 = python regex with one group for the value, $3 = new value
  python3 - "$1" "$2" "$3" <<'PY'
import re, sys
path, pattern, new = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path, encoding="utf-8").read()
rx = re.compile(pattern, re.M)
m = rx.search(text)
if not m:
    sys.exit(f"ERROR: no version line matched {pattern!r} in {path}")
text = text[:m.start(1)] + new + text[m.end(1):]
open(path, "w", encoding="utf-8").write(text)
PY
}

rewrite_first "$REPO_ROOT/package.json"                              '^  "version": "([^"]+)"' "$NEW"
for crate in athenaeum-core athenaeum-tauri athenaeum-web perseus; do
  rewrite_first "$REPO_ROOT/crates/$crate/Cargo.toml"                '^version = "([^"]+)"'     "$NEW"
done
rewrite_first "$REPO_ROOT/crates/athenaeum-tauri/tauri.conf.json"    '^  "version": "([^"]+)"' "$TAURI_NEW"

if [ "${BUMP_SKIP_CARGO:-0}" != "1" ]; then
  # cargo metadata re-resolves the workspace and rewrites Cargo.lock's own
  # package entries — the same effect as `cargo check`, without compiling.
  (cd "$REPO_ROOT" && cargo metadata --format-version 1 --quiet > /dev/null)
fi

CHECK_VERSIONS="$(cd "$SCRIPT_DIR/../../.gitlab/ci/scripts" && pwd)/check_versions.sh"
TAG="v$NEW" REPO_ROOT="$REPO_ROOT" "$CHECK_VERSIONS"

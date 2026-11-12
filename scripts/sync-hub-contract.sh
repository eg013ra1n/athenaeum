#!/usr/bin/env bash
# Copy the hub's recorded contract transcripts into this repo.
#
# Usage: scripts/sync-hub-contract.sh [HUB_DIR]
#   HUB_DIR  the hub checkout (default: ../athenaeum-hub next to this repo).
#
# The transcripts are tests/fixtures/contract/*.json in the hub repo. They land
# in crates/athenaeum-core/tests/fixtures/hub_contract/, next to HUB_COMMIT, a
# single line `<hub sha, 12 chars> <commit date>` naming the hub commit they
# were copied from. Nothing there is edited by hand: this script overwrites it.
# It refuses (message on stderr, exit 1) when the hub's fixtures folder has
# uncommitted changes, so HUB_COMMIT always names what was copied.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HUB="${1:-$REPO_ROOT/../athenaeum-hub}"
SRC_REL="tests/fixtures/contract"
DEST="$REPO_ROOT/crates/athenaeum-core/tests/fixtures/hub_contract"

if [ ! -d "$HUB" ]; then
  echo "ERROR: hub folder '$HUB' does not exist; pass the hub checkout as the first argument" >&2
  exit 1
fi
if ! git -C "$HUB" rev-parse --git-dir >/dev/null 2>&1; then
  echo "ERROR: '$HUB' is not a git checkout" >&2
  exit 1
fi
if [ ! -d "$HUB/$SRC_REL" ]; then
  echo "ERROR: '$HUB/$SRC_REL' does not exist: this hub checkout has no recorded transcripts" >&2
  exit 1
fi

DIRTY="$(git -C "$HUB" status --porcelain -- "$SRC_REL")"
if [ -n "$DIRTY" ]; then
  echo "ERROR: '$HUB/$SRC_REL' has uncommitted changes; commit them in the hub first so HUB_COMMIT names what is copied:" >&2
  printf '%s\n' "$DIRTY" >&2
  exit 1
fi

shopt -s nullglob
FILES=("$HUB/$SRC_REL"/*.json)
if [ "${#FILES[@]}" -eq 0 ]; then
  echo "ERROR: '$HUB/$SRC_REL' holds no *.json transcripts" >&2
  exit 1
fi

SHA="$(git -C "$HUB" rev-parse --short=12 HEAD)"
DATE="$(git -C "$HUB" log -1 --format=%cs)"

mkdir -p "$DEST"
rm -f "$DEST"/*.json
cp "${FILES[@]}" "$DEST"/
printf '%s %s\n' "$SHA" "$DATE" > "$DEST/HUB_COMMIT"

echo "copied ${#FILES[@]} transcript(s) from hub $SHA ($DATE) to ${DEST#"$REPO_ROOT"/}"

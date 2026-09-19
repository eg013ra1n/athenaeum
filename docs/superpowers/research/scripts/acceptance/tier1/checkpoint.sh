#!/bin/zsh
# One stacking compute Tier A checkpoint: a fresh copy of the template
# catalog, a full set-204 (LDN1272-test reduced set) stacking run on THIS
# worktree's own release build, extracted, and byte/stage-compared against
# an earlier checkpoint.
#
#   checkpoint.sh <name> [base-name]
#
# <name> names the copy .athenaeum-acc/tierA-<name> — always a FRESH `cp -R`
# of .athenaeum-acc/tierA-template (never the template itself, never a
# previous checkpoint re-run in place). [base-name] is the checkpoint to
# compare against, default "baseline" (.athenaeum-acc/tierA-baseline — the
# Tier A ruler, run 5 on main 548eeec3, recorded in the plan's ledger).
#
# Refuses to START (before touching anything) while Time Machine is backing
# up or a cargo/rustc process from another session is alive — either one
# would distort a wall-clock measurement. Picks the first free port from
# 8950 up, so a leftover server from an earlier checkpoint attempt never
# collides with this one.
set -euo pipefail
NAME="${1:?checkpoint name}"
BASE="${2:-baseline}"
# Both names become path components under $ACC_ROOT (and <name> feeds an
# `rm -rf`): a bare word only — no separator, no dot, no quote.
for n in "$NAME" "$BASE"; do
  case "$n" in
    ''|*[!A-Za-z0-9_-]*) echo "refusing checkpoint name '$n': letters, digits, _ and - only" >&2; exit 1 ;;
  esac
done
ACC_ROOT="$HOME/.athenaeum-acc"
TEMPLATE="$ACC_ROOT/tierA-template"
COPY="$ACC_ROOT/tierA-$NAME"
BASE_DIR="$ACC_ROOT/tierA-$BASE"
REPO="$(cd "$(dirname "$0")/../../../../../.." && pwd)"
SCRIPTS="$REPO/docs/superpowers/research/scripts/acceptance"

RUNNING="$(tmutil status 2>/dev/null | grep -o 'Running = [0-9]' | grep -o '[0-9]' || echo '?')"
if [ "$RUNNING" != "0" ]; then
  echo "refusing to start: tmutil status reports Running = $RUNNING (Time Machine is backing up)" >&2
  exit 1
fi
if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; then
  echo "refusing to start: a cargo/rustc process is alive on this machine" >&2
  exit 1
fi

if [ ! -d "$TEMPLATE" ]; then
  echo "template not found: $TEMPLATE" >&2
  exit 1
fi

echo "copying $TEMPLATE -> $COPY"
rm -rf "$COPY"
cp -R "$TEMPLATE" "$COPY"

# The three dir settings (calibration.library_dir, stacking.working_dir,
# stacking.output_dir) all point somewhere under the template's own path —
# one blanket substring replace catches all three (and nothing else: no
# other settings value embeds the template's own folder name).
sqlite3 "$COPY/athenaeum.db" \
  "UPDATE settings SET value = replace(value, 'tierA-template', 'tierA-$NAME') WHERE value LIKE '%tierA-template%';"

PORT=8950
while lsof -i ":$PORT" >/dev/null 2>&1; do
  PORT=$((PORT + 1))
done
echo "using port $PORT"

export ATH_ACC_EXTRA_PATHS="/Volumes/BigMac/Users/astrobureau/Pictures"
zsh "$SCRIPTS/tier1/tier1-run.sh" "$REPO" "$COPY" "$PORT" 204

python3 "$SCRIPTS/tier1/tier1-extract.py" "$COPY" > "$COPY/extract.txt"
python3 "$SCRIPTS/tier1/tier1-compare.py" "$BASE_DIR" "$COPY" > "$COPY/compare.txt"

echo
echo "== $COPY/extract.txt =="
cat "$COPY/extract.txt"
echo
echo "== $COPY/compare.txt (vs $BASE_DIR) =="
cat "$COPY/compare.txt"

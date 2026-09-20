#!/bin/zsh
# Re-run an OLDER commit's build against the SAME catalog, right after the
# checkpoint it is the ruler for.
#
#   rerun-build.sh <commit> <name> [port]
#
# Builds <commit> in a throwaway worktree with its own cargo target dir, then
# drives it with the CURRENT harness scripts (copied in — an older tree
# predates checkpoint.sh / ATH_ACC_EXTRA_PATHS / the loud-refusal fix), on a
# fresh copy of the template catalog at .athenaeum-acc/tierA-<name>.
#
# WHY this exists (ruling R-TA-9): this Mac drifts ±10–15 % across a
# build-heavy session, so a tier's total is only readable against a baseline
# re-run in the SAME machine state — never against a number recorded on a
# different evening. Tier C's own headline (21.73 → 14.47 min) is exactly such
# a bracket: the tierC checkpoint, then this script on the Tier A head, back
# to back. It was written ad hoc for that bracket and lives here now because
# the harness is reused, never rebuilt per cycle (owner, 2026-09-18).
#
# The submodule step is not boilerplate (the C-19/C-3 incident, 2026-09-20):
# the gitlinks name commits that exist ONLY in this worktree's own submodule
# checkouts — unpushed, rewritten history — so `submodule update` must clone
# from there, and git refuses a `file://`-class transport unless
# `protocol.file.allow=always` says otherwise. The first attempt at the Tier C
# bracket died on `transport 'file' not allowed` after the checkpoint it was
# meant to bracket had already run, costing the whole pairing.
#
# Refusals mirror checkpoint.sh's, for the same reasons: a wall-clock
# measurement is worthless while Time Machine or another session's cargo is
# running, and <name> becomes a path component that feeds an `rm -rf`.
if [ -z "${ZSH_VERSION:-}" ]; then
  exec zsh "$0" "$@"
fi
set -uo pipefail

COMMIT="${1:?commit to build}"
NAME="${2:?checkpoint name}"
PORT="${3:-8952}"

# This script lives at <repo>/docs/superpowers/research/scripts/acceptance/tier1/.
CUR="$(cd "$(dirname "$0")/../../../../../.." && pwd)"
SCRIPTS="$CUR/docs/superpowers/research/scripts/acceptance"
ACC="$HOME/.athenaeum-acc"
TEMPLATE="$ACC/tierA-template"
COPY="$ACC/tierA-$NAME"
# The throwaway worktree + its target dir: the session scratchpad when the
# harness runs under one, else $TMPDIR. Never inside the repo — a second
# checkout under <repo> would be picked up by cargo and by the scan roots.
WT_ROOT="${ATH_ACC_SCRATCH:-${TMPDIR:-/tmp}}"
WT="$WT_ROOT/acc-base-$COMMIT"

case "$NAME" in
  ''|*[!A-Za-z0-9_-]*)
    echo "refusing name '$NAME': letters, digits, _ and - only" >&2; exit 1 ;;
esac
# `template` is the pristine catalog every checkpoint copies FROM and
# `baseline` is the Tier A ruler — this script `rm -rf`s its target.
if [ "$NAME" = "template" ] || [ "$NAME" = "baseline" ]; then
  echo "refusing name '$NAME': it is read, never rewritten" >&2; exit 1
fi

RUNNING="$(tmutil status 2>/dev/null | grep -o 'Running = [0-9]' | grep -o '[0-9]' || echo '?')"
if [ "$RUNNING" != "0" ]; then
  echo "refusing to start: tmutil status reports Running = $RUNNING (Time Machine is backing up)" >&2
  exit 1
fi
if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; then
  echo "refusing to start: a cargo/rustc process is alive on this machine" >&2
  exit 1
fi
if pgrep -f "target-acc/release/athenaeum-web" >/dev/null 2>&1; then
  echo "refusing to start: an acceptance server is still alive" >&2
  exit 1
fi
if [ ! -d "$TEMPLATE" ]; then
  echo "template not found: $TEMPLATE" >&2
  exit 1
fi

echo "== worktree at $COMMIT ($WT)"
rm -rf "$WT"
git -C "$CUR" worktree prune
git -C "$CUR" worktree add --detach "$WT" "$COMMIT" || exit 1
git -C "$WT" submodule init rustafits solvemyastro || exit 1
git -C "$WT" config submodule.rustafits.url "$CUR/rustafits"
git -C "$WT" config submodule.solvemyastro.url "$CUR/solvemyastro"
git -C "$WT" -c protocol.file.allow=always submodule update rustafits solvemyastro || exit 1

# The CURRENT harness drives the OLD build — see the header.
mkdir -p "$WT/docs/superpowers/research/scripts/acceptance/tier1"
cp "$SCRIPTS/server.sh" "$WT/docs/superpowers/research/scripts/acceptance/server.sh"
cp "$SCRIPTS/tier1/tier1-run.sh" "$WT/docs/superpowers/research/scripts/acceptance/tier1/tier1-run.sh"
cp "$SCRIPTS/tier1/tier1-extract.py" "$WT/docs/superpowers/research/scripts/acceptance/tier1/tier1-extract.py"

echo "== building $COMMIT ($(date -u +%FT%TZ))"
(cd "$WT" && CARGO_TARGET_DIR="$WT/.superpowers/target-acc" cargo build -p athenaeum-web --release 2>&1 | tail -3) || exit 1

echo "== copy template -> $COPY"
rm -rf "$COPY"
cp -R "$TEMPLATE" "$COPY"
sqlite3 "$COPY/athenaeum.db" \
  "UPDATE settings SET value = replace(value, 'tierA-template', 'tierA-$NAME') WHERE value LIKE '%tierA-template%';"
# prepare-catalog.sh strips any per-set `paths` override (it would send 50 GB
# of working tree to whatever absolute path it names); refuse if one came back.
if [ "$(sqlite3 "$COPY/athenaeum.db" "SELECT COUNT(*) FROM stacking_set_config WHERE json_extract(config_json, '\$.paths') IS NOT NULL;")" != "0" ]; then
  echo "refusing: a set config in $COPY carries a paths override" >&2
  exit 1
fi

while lsof -i ":$PORT" >/dev/null 2>&1; do
  PORT=$((PORT + 1))
done
echo "== run on port $PORT ($(date -u +%FT%TZ))"
export ATH_ACC_EXTRA_PATHS="${ATH_ACC_EXTRA_PATHS:-$HOME/Pictures}"
zsh "$WT/docs/superpowers/research/scripts/acceptance/tier1/tier1-run.sh" \
  "$WT" "$COPY" "$PORT" "${ATH_ACC_SET_ID:-204}" || exit 1

python3 "$SCRIPTS/tier1/tier1-extract.py" "$COPY" > "$COPY/extract.txt" 2>&1
echo "== $COPY/extract.txt =="
cat "$COPY/extract.txt"
echo "== done $(date -u +%FT%TZ)"

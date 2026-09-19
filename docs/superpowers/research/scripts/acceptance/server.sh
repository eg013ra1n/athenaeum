#!/bin/zsh
# Serve a release athenaeum-web against an acceptance catalog copy.
#
#   server.sh <work-dir> [port]
#
# Builds (incrementally) into the separate target dir .superpowers/target-acc
# so the dev build cache is untouched, then execs the server with the copy's DB,
# the volumes the catalog references as allowed paths, and the work dir as the
# export dir. Logs land in <work-dir>/logs/ (the web convention: next to the DB).
# Stop it with Ctrl-C or `pkill -f target-acc/release/athenaeum-web`.
#
# ATH_ACC_EXTRA_PATHS (comma-separated) is appended to the hard-coded
# ATHENAEUM_ALLOWED_PATHS volume list below — perf tier A's reduced set 204
# lives under /Volumes/BigMac/Users/astrobureau/Pictures/ldn_test, outside
# every volume the list otherwise names.
set -euo pipefail
WORK="${1:?work dir}"
PORT="${2:-8934}"
REPO="$(cd "$(dirname "$0")/../../../../.." && pwd)"
export CARGO_TARGET_DIR="$REPO/.superpowers/target-acc"
(cd "$REPO" && cargo build -p athenaeum-web --release 2>&1 | tail -2)
export ATHENAEUM_DB_PATH="$WORK/athenaeum.db"
export ATHENAEUM_PORT="$PORT"
ALLOWED_PATHS="/Volumes/bigbase,/Volumes/bigbase2,/Volumes/bigbase3,/Volumes/Universe,$WORK"
if [ -n "${ATH_ACC_EXTRA_PATHS:-}" ]; then
  ALLOWED_PATHS="$ALLOWED_PATHS,$ATH_ACC_EXTRA_PATHS"
fi
export ATHENAEUM_ALLOWED_PATHS="$ALLOWED_PATHS"
export ATHENAEUM_EXPORT_DIR="$WORK/export"
export ATHENAEUM_LOG="${ATHENAEUM_LOG:-info,athenaeum_core::calibration_library=debug,athenaeum_core::api::masters=debug}"
exec "$CARGO_TARGET_DIR/release/athenaeum-web"

#!/bin/zsh
# Make an isolated copy of the dev catalog for an acceptance run.
#
#   prepare-catalog.sh <work-dir> [master-format]
#
# Writes <work-dir>/athenaeum.db (a consistent SQLite .backup of the dev
# catalog — the dev DB runs in WAL mode, a plain cp can miss the tail), creates
# <work-dir>/{library,export,stackwork}, and points the COPY's settings at them
# so a master build, an export or a stacking run lands in <work-dir>, never in
# the owner's real library. Optional second argument sets
# calibration.master_format (fits|xisf) in the copy.
set -euo pipefail
WORK="${1:?work dir}"
FORMAT="${2:-}"
DEV_DB="$HOME/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db"
mkdir -p "$WORK/library" "$WORK/export" "$WORK/stackwork"
rm -f "$WORK/athenaeum.db"
sqlite3 "$DEV_DB" ".backup '$WORK/athenaeum.db'"
sqlite3 "$WORK/athenaeum.db" "INSERT OR REPLACE INTO settings (key, value) VALUES ('calibration.library_dir', '$WORK/library');"
if [ -n "$FORMAT" ]; then
  sqlite3 "$WORK/athenaeum.db" "INSERT OR REPLACE INTO settings (key, value) VALUES ('calibration.master_format', '$FORMAT');"
fi
echo "catalog copy: $WORK/athenaeum.db ($(sqlite3 "$WORK/athenaeum.db" 'SELECT COUNT(*) FROM frames') frames)"
sqlite3 "$WORK/athenaeum.db" "SELECT key, value FROM settings WHERE key LIKE 'calibration.%';"

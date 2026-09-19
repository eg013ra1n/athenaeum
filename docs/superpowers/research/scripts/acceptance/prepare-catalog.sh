#!/bin/zsh
# Make an isolated copy of the dev catalog for an acceptance run.
#
#   prepare-catalog.sh <work-dir> [master-format] [set-id]
#
# Writes <work-dir>/athenaeum.db (a consistent SQLite .backup of the source
# catalog — a WAL-mode DB can miss the tail under a plain cp), creates
# <work-dir>/{library,export,stackwork,stackout}, and points the COPY's
# settings at them so a master build, an export or a stacking run lands in
# <work-dir>, never in the owner's real library. Optional second argument
# sets calibration.master_format (fits|xisf) in the copy.
#
# ATH_ACC_DB overrides the source catalog path (default: the dev app-data
# DB below) — perf tier A Task 0's checkpoint copies the tierA-template DB
# instead of re-reading the live dev catalog on every checkpoint.
#
# Optional third argument (set-id) points THAT frame set's stacking config
# at the copy's own folders, same as the calibration library dir above:
# if the set has no `stacking_set_config` row yet, one is copied from set
# 109's (production LDN 1272) row first — config_json and the NOT NULL
# updated_at column both copied verbatim, excluded_frame_ids_json reset to
# '[]' (those ids name set 109's own members, not set-id's); either way the
# row's config_json is then rewritten in place: output.format -> "fits",
# output.cleanup -> "keepAll", and the config's own `paths` object REMOVED
# entirely (a per-set `paths` override — set 204's own config carried one —
# sends the run into the owner's real folders instead of `<work-dir>`; the
# removal falls back to the global stacking.working_dir/output_dir below via
# `PathsConfig`'s own `#[serde(default)]`). Idempotent: a set whose config
# already matches this shape (e.g. an already-prepared template copy) is
# rewritten to the same values.
set -euo pipefail
WORK="${1:?work dir}"
FORMAT="${2:-}"
SET_ID="${3:-}"
DEV_DB="$HOME/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db"
SRC_DB="${ATH_ACC_DB:-$DEV_DB}"
mkdir -p "$WORK/library" "$WORK/export" "$WORK/stackwork" "$WORK/stackout"
rm -f "$WORK/athenaeum.db"
sqlite3 "$SRC_DB" ".backup '$WORK/athenaeum.db'"
sqlite3 "$WORK/athenaeum.db" "INSERT OR REPLACE INTO settings (key, value) VALUES ('calibration.library_dir', '$WORK/library');"
if [ -n "$FORMAT" ]; then
  sqlite3 "$WORK/athenaeum.db" "INSERT OR REPLACE INTO settings (key, value) VALUES ('calibration.master_format', '$FORMAT');"
fi
if [ -n "$SET_ID" ]; then
  sqlite3 "$WORK/athenaeum.db" <<SQL
INSERT INTO stacking_set_config (frames_set_id, config_json, excluded_frame_ids_json, updated_at)
SELECT $SET_ID, config_json, '[]', updated_at
FROM stacking_set_config
WHERE frames_set_id = 109
  AND NOT EXISTS (SELECT 1 FROM stacking_set_config WHERE frames_set_id = $SET_ID);

UPDATE stacking_set_config
SET config_json = json_remove(
      json_set(
        json_set(config_json, '\$.output.format', 'fits'),
        '\$.output.cleanup', 'keepAll'
      ),
      '\$.paths'
    )
WHERE frames_set_id = $SET_ID;

INSERT OR REPLACE INTO settings (key, value) VALUES ('stacking.working_dir', '$WORK/stackwork');
INSERT OR REPLACE INTO settings (key, value) VALUES ('stacking.output_dir', '$WORK/stackout');
SQL
  echo "set $SET_ID stacking config: $(sqlite3 "$WORK/athenaeum.db" "SELECT config_json FROM stacking_set_config WHERE frames_set_id = $SET_ID")"
fi
echo "catalog copy: $WORK/athenaeum.db ($(sqlite3 "$WORK/athenaeum.db" 'SELECT COUNT(*) FROM frames') frames)"
sqlite3 "$WORK/athenaeum.db" "SELECT key, value FROM settings WHERE key LIKE 'calibration.%' OR key LIKE 'stacking.%dir';"

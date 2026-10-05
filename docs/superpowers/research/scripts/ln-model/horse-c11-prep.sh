#!/bin/zsh
# Horse C11 acceptance preparation (spec §11.4): a catalog COPY whose set-182 lights lose the
# manual offset-50 dark links (sets 1242 and 1315).
#
#   horse-c11-prep.sh <acc-dir> <port>
#
# <acc-dir> must be under ~/.athenaeum-acc/. The copy comes from prepare-catalog.sh (a read-only
# .backup of the dev catalog); every write below happens on the COPY only. Never touches the
# production catalog.
set -euo pipefail
ACC="${1:?acc dir (under ~/.athenaeum-acc/)}"
PORT="${2:?port}"
HERE="${0:A:h}"
ROOT="$HOME/.athenaeum-acc"
case "$ACC" in
  "$ROOT"/?*) ;;
  *) echo "refusing: $ACC is not under $ROOT/" >&2; exit 2 ;;
esac
case "$ACC" in
  *..*) echo "refusing: $ACC contains '..'" >&2; exit 2 ;;
esac
"$HERE/../acceptance/prepare-catalog.sh" "$ACC"
# Lights reach a set through session_members -> sessions -> imaging_nights.
CHANGES=$(sqlite3 "$ACC/athenaeum.db" <<'SQL'
DELETE FROM calibration_set_to_frames
WHERE source_type = 'frame' AND calibration_type = 'Dark' AND calibration_set_id IN (1242, 1315)
  AND source_id IN (SELECT sm.frame_id FROM session_members sm
                    JOIN sessions s ON s.id = sm.session_id
                    JOIN imaging_nights n ON n.id = s.imaging_night_id
                    WHERE n.frames_set_id = 182);
SELECT changes();
SQL
)
echo "dark links deleted from the copy: $CHANGES"
cat <<NEXT

Next steps (server on port $PORT, started with the copy: acceptance/server.sh $ACC $PORT):
  1. python3 $HERE/horse-c11-config.py <run-8.json> $ACC/athenaeum.db [--fixed-1024] > $ACC/c11-config.json
  2. $HERE/../acceptance/api.sh set_stacking_config "\$(cat $ACC/c11-config.json)" $PORT
  3. $HERE/../acceptance/api.sh set_frame_set_reference '{"framesSetId": 182, "frameId": 10666}' $PORT
  4. start the run (api.sh start_stacking {"setId":182}) and read it with tier1-compare.py --ln-grids / halo_profile.py / nebula_boxes.py
NEXT

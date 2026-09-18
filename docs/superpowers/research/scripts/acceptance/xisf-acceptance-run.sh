#!/bin/zsh
# The 2026-09-18 XISF acceptance run, as the sequence of API calls it was —
# kept so the next container-related change can be re-checked the same way.
# What it measures: (1) a calibration master built as XISF registers like a
# scanned one and calibrates lights identically to its FITS twin built by the
# same engine; (2) a calibrated light exported as XISF equals the FITS export;
# (3) a rebuild keeps the file's own container; (4) a stacking run over a set
# holding FITS and XISF lights side by side yields the same master light as
# the all-FITS set. NOT a one-shot script — each phase waits on SSE events and
# the frame/set ids below are the ones minted on the day; read the research
# note `docs/superpowers/research/2026-09-18-xisf-acceptance-run.md` for the
# results and re-mint ids when repeating.
#
# Prerequisites: prepare-catalog.sh <W> xisf; server.sh <W>; the probe
#   cargo build -p athenaeum-core --release --features render --example xisf_convert_probe
# and a `mono20.tsv` (frame_id|fwhm|path) of the frames to copy — the 2026-09-18
# pick was the 20 sharpest mono LDN 1272 frames by the last run's metrics:
#   SELECT a.frame_id, json_extract(a.payload_json,'$.channels[0].fwhmPx'), f.path
#   FROM stacking_artifacts a JOIN frames fr ON fr.id=a.frame_id JOIN files f ON f.id=fr.file_id
#   WHERE a.frames_set_id=109 AND a.kind='metrics' AND a.group_key LIKE 'mono%'
#   ORDER BY 2 ASC LIMIT 20;
set -euo pipefail
W="${1:?work dir}"
A="$(dirname "$0")/api.sh"
PROBE="$(cd "$(dirname "$0")/../../../../.." && pwd)/.superpowers/target-acc/release/examples/xisf_convert_probe"

# 1. Two scratch roots: mixed/ (odd rows copied as FITS, even rows converted to
#    XISF with ADU bounds) and pure/ (all 20 as FITS). NB: never name a zsh
#    loop variable `path` — it is tied to $PATH.
mkdir -p "$W/mixed" "$W/pure"; i=0; args=()
while IFS='|' read -r fid fwhm src; do
  i=$((i+1)); base=$(basename "$src"); cp "$src" "$W/pure/$base"
  if [ $((i % 2)) -eq 0 ]; then args+=("$src" "$W/mixed/${base%.fits}.xisf"); else cp "$src" "$W/mixed/$base"; fi
done < "$W/mono20.tsv"
"$PROBE" "${args[@]}"

# 2. Catalog them, one set per root, auto-link, then the manual dark (the
#    library's master dark has OFFSET 200 against the lights' 30 — the
#    original LDN 1272 links are manual overrides too).
R_M=$($A add_scan_root "{\"path\":\"$W/mixed\"}" | python3 -c 'import json,sys;print(json.load(sys.stdin)["id"])')
R_P=$($A add_scan_root "{\"path\":\"$W/pure\"}"  | python3 -c 'import json,sys;print(json.load(sys.stdin)["id"])')
$A start_scan "{\"rootId\":$R_M}" >/dev/null; $A start_scan "{\"rootId\":$R_P}" >/dev/null
IDS_M=$(sqlite3 "$W/athenaeum.db" "SELECT json_group_array(fr.id) FROM files f JOIN frames fr ON fr.file_id=f.id WHERE f.path LIKE '$W/mixed/%';")
IDS_P=$(sqlite3 "$W/athenaeum.db" "SELECT json_group_array(fr.id) FROM files f JOIN frames fr ON fr.file_id=f.id WHERE f.path LIKE '$W/pure/%';")
S_M=$($A create_frame_set_from_selection "{\"name\":\"ACC mixed 20\",\"frame_ids\":$IDS_M}")   # snake_case on this route
S_P=$($A create_frame_set_from_selection "{\"name\":\"ACC pure 20\",\"frame_ids\":$IDS_P}")
$A find_calibration_for_frame_set "{\"frameSetId\":$S_M}" >/dev/null
$A find_calibration_for_frame_set "{\"frameSetId\":$S_P}" >/dev/null
IDS_ALL=$(sqlite3 "$W/athenaeum.db" "SELECT json_group_array(fr.id) FROM files f JOIN frames fr ON fr.file_id=f.id WHERE f.path LIKE '$W/%';")
$A manual_assign_calibration "{\"frameIds\":$IDS_ALL,\"calibrationSetId\":1749,\"calibrationType\":\"Dark\"}"

# 3. Masters: un-supersede the raw source set of the existing FITS master
#    (1679 → 1749 on 2026-09-18) in the COPY, build it as XISF, then again as
#    FITS with the setting flipped — two masters, one engine, one source.
sqlite3 "$W/athenaeum.db" "UPDATE calibration_set SET superseded_by_set_id=NULL WHERE id=1679;"
$A start_master_build '{"setId":1679,"recipe":{}}'      # wait for master-build-complete on /api/events
sqlite3 "$W/athenaeum.db" "UPDATE calibration_set SET superseded_by_set_id=NULL WHERE id=1679; UPDATE settings SET value='fits' WHERE key='calibration.master_format';"
$A start_master_build '{"setId":1679,"recipe":{}}'      # wait again
# imgcmp.py <library>/…/master_dark_….fits <library>/…/master_dark_….xisf  → expect max |diff| = 0

# 4. Exports on the pure set: relink the Dark links to the XISF master (B, fits
#    out; C, xisf out) and to the FITS twin (D); compare B↔D and B↔C with
#    imgcmp.py — expect max |diff| = 0 for every frame.
#    export_to_wbpp {frameSetId, outputDir, useSymlinks:false, exportMode:"calibratedLights",
#                    flatNorm:true, hotPixel:true, debayer:true, format:"fits"|"xisf"}

# 5. rebuild_master {masterSetId:<xisf master>} with the setting at fits — the
#    file must stay .xisf (mtime moves, signature XISF0100).

# 6. Stacking: set_stacking_paths {working, output} under $W, then
#    start_stacking {setId:<pure>} and {setId:<mixed>} with the global config;
#    imgcmp.py the two master lights — expect max |diff| = 0, 20 calibrated
#    artifacts per set, only the "no plate solve" warning in the log.

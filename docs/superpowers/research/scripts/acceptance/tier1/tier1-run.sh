#!/bin/zsh
# One acceptance run: REPO (worktree with the code to measure), W (catalog copy), PORT,
# SET_ID (frame set to stack, default 109 — the full production LDN 1272 set; perf
# tier A's reduced set is 204). Starts the release server, subscribes to SSE, starts
# stacking SET_ID from Calibrate, waits for stacking-complete, records wall time,
# stops the server. ATH_ACC_EXTRA_PATHS, if set in this process's own environment,
# reaches server.sh unchanged — plain env inheritance across the `zsh "$SRV" ...`
# child below, nothing this script does explicitly.
set -uo pipefail
REPO="$1"; W="$2"; PORT="$3"; SET_ID="${4:-109}"
SRV="$REPO/docs/superpowers/research/scripts/acceptance/server.sh"
export ATHENAEUM_LOG="info,athenaeum_core::stacking=debug,athenaeum_core::export::calibrated_generator=debug,athenaeum_core::integration=debug,athenaeum_core::calibration_library=debug"
zsh "$SRV" "$W" "$PORT" > "$W/server.out" 2>&1 &
SRV_PID=$!
for i in $(seq 1 240); do
  if curl -sS -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/get_stacking_plan" -H 'content-type: application/json' -d "{\"setId\":$SET_ID}" 2>/dev/null | grep -q '^200$'; then break; fi
  sleep 5
done
curl -sN "http://127.0.0.1:$PORT/api/events" > "$W/events.log" 2>/dev/null &
SSE_PID=$!
sleep 2
T0=$(date +%s); echo "start $(date -u +%FT%TZ) epoch=$T0" > "$W/run.txt"
RESP=$(curl -sS -X POST "http://127.0.0.1:$PORT/api/start_stacking" -H 'content-type: application/json' -d "{\"setId\":$SET_ID,\"rerunFrom\":\"calibrate\"}")
echo "$RESP" >> "$W/run.txt"; echo >> "$W/run.txt"
# A refused start (a plan blocker such as `space`, a bad set id, a server
# error) answers without a runId; waiting for `stacking-complete` after that
# is a four-hour hang (checkpoint g3, 2026-09-20: 99.2 GB needed, 96.8 free).
# Fail loudly instead, with the server's own answer.
if ! printf '%s' "$RESP" | grep -q '"runId"'; then
  echo "start_stacking refused for set $SET_ID: $RESP" >&2
  kill $SSE_PID 2>/dev/null
  pkill -f "$REPO/.superpowers/target-acc/release/athenaeum-web" 2>/dev/null
  exit 1
fi
for i in $(seq 1 480); do
  if grep -q 'stacking-complete' "$W/events.log" 2>/dev/null; then break; fi
  sleep 30
done
T1=$(date +%s); echo "end $(date -u +%FT%TZ) epoch=$T1 wall_s=$((T1-T0))" >> "$W/run.txt"
grep -A1 'stacking-complete' "$W/events.log" | head -3 >> "$W/run.txt"
kill $SSE_PID 2>/dev/null
pkill -f "$REPO/.superpowers/target-acc/release/athenaeum-web" 2>/dev/null
sleep 3
echo "done $W" >> "$W/run.txt"
cat "$W/run.txt"

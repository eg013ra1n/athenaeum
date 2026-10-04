#!/bin/zsh
# One project stacking run against an acceptance catalog copy (plan 6b-3 D21):
# tier1-run.sh's project twin.
#
#   project-run.sh <repo> <work-dir> <port> <project-id>
#
# Starts the release server on <work-dir>, subscribes to SSE, posts
# `start_project_stacking {projectId, rerunFrom: "calibrate"}`, waits for the
# `stacking-complete` event of THAT run id, records the wall time in
# <work-dir>/run-project.txt (beside tier1-run.sh's run.txt, never over it)
# and stops the server. Refuses to start while Time Machine is backing up or
# another cargo/rustc is alive, as checkpoint.sh does: a refusal is answered
# by stopping the backup or waiting for the build, never by editing this file.
if [ -z "${ZSH_VERSION:-}" ]; then
  exec zsh "$0" "$@"
fi
set -uo pipefail
REPO="${1:?repo}"; W="${2:?work dir}"; PORT="${3:?port}"; PID="${4:?project id}"
case "$PID" in
  ''|*[!A-Za-z0-9-]*) echo "refusing project id '$PID': letters, digits and - only" >&2; exit 1 ;;
esac
RUNNING="$(tmutil status 2>/dev/null | grep -o 'Running = [0-9]' | grep -o '[0-9]' || echo '?')"
if [ "$RUNNING" != "0" ]; then
  echo "refusing to start: tmutil status reports Running = $RUNNING (Time Machine is backing up)" >&2
  exit 1
fi
if pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; then
  echo "refusing to start: a cargo/rustc process is alive on this machine" >&2
  exit 1
fi
SRV_PID=""; SSE_PID=""; CLEANED=0
cleanup() {
  # Idempotent: the explicit call at the end and the EXIT trap must not both act.
  [ "$CLEANED" = "1" ] && return 0
  CLEANED=1
  [ -n "$SSE_PID" ] && kill "$SSE_PID" 2>/dev/null
  if [ -n "$SRV_PID" ]; then
    # server.sh execs the server, so SRV_PID is the server itself.
    kill "$SRV_PID" 2>/dev/null
  else
    # Fallback only when no PID was ever recorded; never after one was, so a late
    # cleanup cannot kill the server of the NEXT run.
    pkill -f "$REPO/.superpowers/target-acc/release/athenaeum-web" 2>/dev/null
  fi
  return 0
}
# zsh resumes after a trapped signal, so INT/TERM must exit explicitly.
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM
trap cleanup EXIT
SRV="$REPO/docs/superpowers/research/scripts/acceptance/server.sh"
export ATHENAEUM_LOG="info,athenaeum_core::stacking=debug,athenaeum_core::integration=debug"
zsh "$SRV" "$W" "$PORT" > "$W/server-project.out" 2>&1 &
SRV_PID=$!
BODY="{\"projectId\":\"$PID\"}"
for i in $(seq 1 240); do
  if curl -sS -o /dev/null -w '%{http_code}' -X POST "http://127.0.0.1:$PORT/api/get_project_stacking_plan" -H 'content-type: application/json' -d "$BODY" 2>/dev/null | grep -q '^200$'; then break; fi
  sleep 5
done
curl -sN "http://127.0.0.1:$PORT/api/events" > "$W/events-project.log" 2>/dev/null &
SSE_PID=$!
sleep 2
T0=$(date +%s); echo "start $(date -u +%FT%TZ) epoch=$T0 project=$PID" > "$W/run-project.txt"
RESP=$(curl -sS -X POST "http://127.0.0.1:$PORT/api/start_project_stacking" -H 'content-type: application/json' -d "{\"projectId\":\"$PID\",\"rerunFrom\":\"calibrate\"}")
echo "$RESP" >> "$W/run-project.txt"; echo >> "$W/run-project.txt"
if ! printf '%s' "$RESP" | grep -q '"runId"'; then
  echo "start_project_stacking refused for $PID: $RESP" >&2
  exit 1
fi
RUN_ID=$(printf '%s' "$RESP" | sed -E 's/.*"runId":([0-9]+).*/\1/')
FINISHED=0
for i in $(seq 1 480); do
  if grep -A1 'stacking-complete' "$W/events-project.log" 2>/dev/null | grep -q "\"runId\":${RUN_ID}[,}]"; then FINISHED=1; break; fi
  sleep 30
done
T1=$(date +%s); echo "end $(date -u +%FT%TZ) epoch=$T1 wall_s=$((T1-T0)) run_id=$RUN_ID" >> "$W/run-project.txt"
STATUS=0
if [ "$FINISHED" != "1" ]; then
  # Review M10: a 4-hour wait that ends without the event is a timeout,
  # never a finished run.
  echo "TIMED OUT after 4 h waiting for stacking-complete of run $RUN_ID" | tee -a "$W/run-project.txt" >&2
  STATUS=1
fi
COMPLETE=$(grep -A1 'stacking-complete' "$W/events-project.log" | grep "\"runId\":${RUN_ID}[,}]")
printf '%s\n' "$COMPLETE" >> "$W/run-project.txt"
if [ "$FINISHED" = "1" ] && ! printf '%s' "$COMPLETE" | grep -q '"success":true'; then
  echo "run $RUN_ID finished WITHOUT success: $COMPLETE" | tee -a "$W/run-project.txt" >&2
  STATUS=1
fi
cleanup
sleep 3
cat "$W/run-project.txt"
exit $STATUS

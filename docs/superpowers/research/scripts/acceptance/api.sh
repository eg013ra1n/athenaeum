#!/bin/zsh
# One command against the acceptance server, JSON in, JSON out.
#
#   api.sh <command> ['<json args>'] [port]
#
# Every Tauri command is mirrored at POST /api/<command> with the same camelCase
# argument object. Examples:
#   api.sh get_master_build_preview '{"setId": 918, "recipe": {}}'
#   api.sh start_master_build '{"setId": 918, "recipe": {}}'
#   api.sh get_stacking_plan '{"frameSetId": 109}'
set -euo pipefail
CMD="${1:?command}"
ARGS="${2:-{\}}"
PORT="${3:-8934}"
curl -sS -X POST "http://127.0.0.1:$PORT/api/$CMD" -H 'content-type: application/json' -d "$ARGS"
echo

#!/bin/bash
# PostToolUse (Edit|Write): record that a Rust source file changed this session so
# the Stop hook (check-rust.sh) knows to run one cargo check at turn end.
# Intentionally cheap — no cargo, no output. Receives JSON on stdin.

INPUT=$(cat)
FILE_PATH=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty')

# Only Rust files mark the turn dirty.
[[ "$FILE_PATH" == *.rs ]] || exit 0

SESSION=$(echo "$INPUT" | jq -r '.session_id // "default"')
touch "${TMPDIR:-/tmp}/claude-rust-dirty-${SESSION}" 2>/dev/null
exit 0

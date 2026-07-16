#!/bin/bash
# Stop hook: once per turn, if any .rs changed this session (flag set by
# mark-rust-dirty.sh), run a single workspace `cargo check` and report ONLY the
# error lines back into context. Silent on success; runs no cargo on
# chat/non-Rust turns. Non-blocking (inform-only) — explicit `cargo test`/`build`
# remain the real completion gate. Receives JSON on stdin.

INPUT=$(cat)
SESSION=$(echo "$INPUT" | jq -r '.session_id // "default"')
FLAG="${TMPDIR:-/tmp}/claude-rust-dirty-${SESSION}"

# Nothing Rust changed this turn -> do nothing (no cargo).
[ -f "$FLAG" ] || exit 0
rm -f "$FLAG"

cd "$CLAUDE_PROJECT_DIR" || exit 0

# Isolated build dir (2026-07-16): agent-session cargo must never share the
# user's ./target — concurrent writers poisoned rustc incremental caches twice
# (linker "symbol not found" on the user's interactive tauri dev build).
export CARGO_TARGET_DIR="$CLAUDE_PROJECT_DIR/target-agent"

OUTPUT=$(cargo check --workspace --message-format=short 2>&1)
if [ $? -eq 0 ]; then
  exit 0          # clean compile -> silent
fi

# Keep only error lines, cap to bound token cost.
ERRORS=$(printf '%s\n' "$OUTPUT" | grep -E '(^error|: error)' | head -40)
[ -z "$ERRORS" ] && exit 0

jq -n --arg ctx "cargo check --workspace failed:
$ERRORS" \
  '{hookSpecificOutput: {hookEventName: "Stop", additionalContext: $ctx}}'
exit 0

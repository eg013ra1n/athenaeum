#!/bin/bash
# PostToolUse hook: run cargo check after editing Rust files
# Receives JSON input on stdin with tool_input.file_path

INPUT=$(cat)
FILE_PATH=$(echo "$INPUT" | jq -r '.tool_input.file_path // empty')

# Only process .rs files
if [[ "$FILE_PATH" != *.rs ]]; then
  exit 0
fi

# Run cargo check from the workspace root
cd "$CLAUDE_PROJECT_DIR" || exit 0
OUTPUT=$(cargo check --workspace --message-format=short 2>&1)
EXIT_CODE=$?

if [ $EXIT_CODE -ne 0 ]; then
  # Show only errors (first 20 lines)
  ERRORS=$(echo "$OUTPUT" | head -20)
  echo "$ERRORS" >&2
  exit 1
fi

exit 0

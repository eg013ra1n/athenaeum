#!/usr/bin/env bash
# Truncate stdin to <= 3900 bytes and append a "Full notes" tail line if cut.
# RELEASE_URL must be set in the environment — that's where the tail link points.
# Output: stdout. The total output is guaranteed <= 4096 bytes (the Discord embed
# description and Telegram message limit), assuming RELEASE_URL is < ~150 bytes.
set -euo pipefail

: "${RELEASE_URL:?RELEASE_URL must be set}"

LIMIT=3900
input=$(cat)
input_len=${#input}

if [ "$input_len" -le "$LIMIT" ]; then
  printf '%s' "$input"
  exit 0
fi

# Truncate at LIMIT bytes. `head -c` is byte-based, which is what we want
# for a Discord/Telegram char limit (Discord measures in code points but a
# byte limit is a safe under-approximation for any UTF-8 input).
head -c "$LIMIT" <<< "$input"
printf '\n\n…\n\nFull notes: %s\n' "$RELEASE_URL"

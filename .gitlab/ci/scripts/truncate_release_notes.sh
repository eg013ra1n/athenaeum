#!/usr/bin/env bash
# Truncate stdin to <= 3900 bytes and append a "Full notes" tail line if cut.
# RELEASE_URL must be set in the environment — that's where the tail link points.
# Output: stdout. The total output is guaranteed <= 4096 bytes (the Discord embed
# description and Telegram message limit), assuming RELEASE_URL is < ~150 bytes.
set -euo pipefail

: "${RELEASE_URL:?RELEASE_URL must be set}"

LIMIT=3900
input=$(cat)
# Byte count, not character count: head -c below is byte-based, so we must
# compare like-for-like. ${#input} would count UTF-8 code points and let
# em-dash-heavy notes bypass the budget.
input_bytes=$(printf '%s' "$input" | wc -c | tr -d ' ')

if [ "$input_bytes" -le "$LIMIT" ]; then
  printf '%s' "$input"
  exit 0
fi

# Truncate at LIMIT bytes. head -c is byte-based, which is what we want
# for a Discord/Telegram code-point limit (LIMIT bytes ≤ LIMIT code points
# in UTF-8). The iconv -c pass drops a partial codepoint that head may have
# left at the cut, so downstream JSON serialisation can't choke on invalid
# UTF-8 bytes from a mid-em-dash split.
head -c "$LIMIT" <<< "$input" | iconv -f utf-8 -t utf-8 -c
printf '\n\n…\n\nFull notes: %s\n' "$RELEASE_URL"

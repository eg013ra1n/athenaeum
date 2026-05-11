#!/usr/bin/env bash
# Post the contents of RELEASE_NOTES.md to a Telegram chat via the Bot API.
# Required env: TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID, CI_COMMIT_TAG, CI_PROJECT_URL.
# Optional env: RELEASE_NOTES_PATH (default: RELEASE_NOTES.md), DRY_RUN=1.
#
# Same soft-fail / skip-on-missing-secret semantics as notify_discord.sh.
set -euo pipefail

if [ -z "${TELEGRAM_BOT_TOKEN:-}" ]; then
  echo "Skipping Telegram notification — TELEGRAM_BOT_TOKEN not set in CI/CD variables."
  exit 0
fi
if [ -z "${TELEGRAM_CHAT_ID:-}" ]; then
  echo "Skipping Telegram notification — TELEGRAM_CHAT_ID not set in CI/CD variables."
  exit 0
fi

: "${CI_COMMIT_TAG:?CI_COMMIT_TAG must be set}"
: "${CI_PROJECT_URL:?CI_PROJECT_URL must be set}"

NOTES_PATH="${RELEASE_NOTES_PATH:-RELEASE_NOTES.md}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RELEASE_URL="${CI_PROJECT_URL}/-/releases/${CI_COMMIT_TAG}"

RESP_TMP=$(mktemp -t telegram_response.XXXXXX)
trap 'rm -f "$RESP_TMP"' EXIT

if echo "$CI_COMMIT_TAG" | grep -q '\-beta'; then
  TITLE="Athenaeum ${CI_COMMIT_TAG} (beta) released"
else
  TITLE="Athenaeum ${CI_COMMIT_TAG} released"
fi

# Pipeline: raw markdown -> Telegram HTML -> truncate.
# Order matters: convert first, THEN truncate, so we never split a tag in half.
# We pass the body through truncate_release_notes.sh (limit 3900), then do a
# second tighter pass below: Telegram's hard cap is 4096 bytes and the bold
# title + "Release page" anchor we add eats ~140 bytes, so the body alone has
# to fit in ~3950. The truncate helper's "Full notes: …" tail can push past
# that, so we re-cap with a slightly smaller limit while preserving the tail.
if [ -s "$NOTES_PATH" ]; then
  BODY=$(
    "$SCRIPT_DIR/md_to_telegram_html.sh" < "$NOTES_PATH" \
      | RELEASE_URL="$RELEASE_URL" "$SCRIPT_DIR/truncate_release_notes.sh"
  )
else
  BODY="Athenaeum ${CI_COMMIT_TAG} is out — see the release page for details."
fi

FOOTER=$(printf '<a href="%s">Release page</a>' "$RELEASE_URL")
HEADER=$(printf '<b>%s</b>' "$TITLE")
TELEGRAM_LIMIT=4096
SEP_BYTES=4  # two "\n\n" separators between header/body/footer
header_bytes=$(printf '%s' "$HEADER" | wc -c | tr -d ' ')
footer_bytes=$(printf '%s' "$FOOTER" | wc -c | tr -d ' ')
body_budget=$((TELEGRAM_LIMIT - header_bytes - footer_bytes - SEP_BYTES))
body_bytes=$(printf '%s' "$BODY" | wc -c | tr -d ' ')
if [ "$body_bytes" -gt "$body_budget" ]; then
  # Body overshot — preserve the trailing "Full notes: <URL>" line if present
  # (the truncate helper adds it when it cuts), then byte-cap the prefix to
  # the remaining budget. Decoding via Python with errors=ignore drops any
  # partial codepoint left at the head -c boundary.
  TAIL_LINE=$(printf '%s' "$BODY" | grep -E '^Full notes: ' | tail -n 1 || true)
  if [ -n "$TAIL_LINE" ]; then
    tail_with_seps=$(printf '\n\n%s\n' "$TAIL_LINE")
    tail_bytes=$(printf '%s' "$tail_with_seps" | wc -c | tr -d ' ')
    prefix_budget=$((body_budget - tail_bytes))
    PREFIX=$(printf '%s' "$BODY" | head -c "$prefix_budget" \
      | python3 -c 'import sys; sys.stdout.write(sys.stdin.buffer.read().decode("utf-8", errors="ignore"))')
    BODY=$(printf '%s%s' "$PREFIX" "$tail_with_seps")
  else
    BODY=$(printf '%s' "$BODY" | head -c "$body_budget" \
      | python3 -c 'import sys; sys.stdout.write(sys.stdin.buffer.read().decode("utf-8", errors="ignore"))')
  fi
fi

# Title is bold; body is HTML-formatted; trailing line is an HTML anchor to
# the release page (which itself links to artfrom.space downloads). Anchors
# keep the rendered message short while still embedding the URL we assert on.
TEXT=$(printf '%s\n\n%s\n\n%s' "$HEADER" "$BODY" "$FOOTER")

PAYLOAD=$(
  CHAT_ID="$TELEGRAM_CHAT_ID" \
  TEXT="$TEXT" \
  python3 -c '
import json, os
payload = {
    "chat_id": os.environ["CHAT_ID"],
    "parse_mode": "HTML",
    "disable_web_page_preview": False,
    "text": os.environ["TEXT"],
}
print(json.dumps(payload))
'
) || {
  echo "WARNING: Failed to build Telegram JSON payload. Pipeline continues."
  exit 0
}

if [ "${DRY_RUN:-}" = "1" ]; then
  echo "$PAYLOAD"
  exit 0
fi

URL="https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/sendMessage"
# Use --data @- (stdin) rather than --data "$PAYLOAD" so the message body
# isn't visible to other jobs via ps(1) on shared runners.
http_code=$(
  printf '%s' "$PAYLOAD" | curl --silent --show-error --max-time 30 \
    --output "$RESP_TMP" \
    --write-out '%{http_code}' \
    --request POST \
    --header 'Content-Type: application/json' \
    --data @- \
    "$URL" \
    || echo "000"
)

if [ "$http_code" -ge 200 ] && [ "$http_code" -lt 300 ]; then
  echo "Telegram notification posted (HTTP $http_code)."
else
  echo "WARNING: Telegram notification failed (HTTP $http_code). Pipeline continues."
  echo "Response body:"
  cat "$RESP_TMP" 2>/dev/null || true
fi
exit 0

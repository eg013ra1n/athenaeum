#!/usr/bin/env bash
# Post the contents of RELEASE_NOTES.md to a Telegram chat via the Bot API.
# Required env: TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID, CI_COMMIT_TAG.
# Optional env: RELEASE_NOTES_PATH (default: RELEASE_NOTES.md), DRY_RUN=1,
#               RELEASE_NOTES_BASE_URL (default: https://artfrom.space/blog).
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

NOTES_PATH="${RELEASE_NOTES_PATH:-RELEASE_NOTES.md}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# Public release-notes URL on the docs site (artfrom.space/blog/v021/, etc.)
# instead of CI_PROJECT_URL which leaks the local GitLab host to public chats.
# Starlight slugifies the post filename by DROPPING dots (v0.2.1.md -> /blog/v021/),
# so the tag must be de-dotted or the link 404s.
RELEASE_NOTES_BASE_URL="${RELEASE_NOTES_BASE_URL:-https://artfrom.space/blog}"
RELEASE_SLUG="$(printf '%s' "$CI_COMMIT_TAG" | tr -d '.')"
RELEASE_URL="${RELEASE_NOTES_BASE_URL}/${RELEASE_SLUG}/"

RESP_TMP=$(mktemp -t telegram_response.XXXXXX)
trap 'rm -f "$RESP_TMP"' EXIT

if echo "$CI_COMMIT_TAG" | grep -q '\-beta'; then
  TITLE="Athenaeum ${CI_COMMIT_TAG} (beta) released"
else
  TITLE="Athenaeum ${CI_COMMIT_TAG} released"
fi

# Body budget: 4096 (Telegram cap) − ~184 bytes for the bold title and
# the two-link footer = ~3912. We pass LIMIT=3700 to the truncator for
# comfortable headroom; HTML conversion can grow byte counts unpredictably.
#
# Pipeline: raw markdown → Telegram HTML → byte-truncate at LIMIT=3700.
# Convert first, THEN truncate, so the cut never lands inside an HTML tag.
if [ -s "$NOTES_PATH" ]; then
  BODY=$(
    "$SCRIPT_DIR/md_to_telegram_html.sh" < "$NOTES_PATH" \
      | LIMIT=3700 RELEASE_URL="$RELEASE_URL" "$SCRIPT_DIR/truncate_release_notes.sh"
  )
else
  BODY="Athenaeum ${CI_COMMIT_TAG} is out — see the release page for details."
fi

# Bold title + body + one-line footer with both links.
FOOTER="<a href=\"https://artfrom.space/releases/download/\">Download</a> · <a href=\"${RELEASE_URL}\">Release page</a>"
TEXT=$(printf '<b>%s</b>\n\n%s\n\n%s' "$TITLE" "$BODY" "$FOOTER")

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
elif [ "$http_code" -ge 400 ] && [ "$http_code" -lt 500 ]; then
  # 4xx is a configuration error (revoked bot token, wrong chat id) — it will
  # fail every future release identically, so surface it as a job failure.
  # The job is allow_failure: the pipeline still passes, but the yellow "!"
  # is visible instead of rotting green (401 went unnoticed for 4 releases).
  echo "ERROR: Telegram notification rejected (HTTP $http_code) — check TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID."
  echo "Response body:"
  cat "$RESP_TMP" 2>/dev/null || true
  exit 1
else
  # Transport trouble (timeout, 5xx) is transient — the build already shipped.
  echo "WARNING: Telegram notification failed (HTTP $http_code). Pipeline continues."
  echo "Response body:"
  cat "$RESP_TMP" 2>/dev/null || true
fi
exit 0

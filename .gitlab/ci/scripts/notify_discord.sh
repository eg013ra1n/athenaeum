#!/usr/bin/env bash
# Post the contents of RELEASE_NOTES.md to a Discord channel via webhook.
# Required env: DISCORD_WEBHOOK_URL, CI_COMMIT_TAG.
# Optional env: RELEASE_NOTES_PATH (default: RELEASE_NOTES.md), DRY_RUN=1,
#               RELEASE_NOTES_BASE_URL (default: https://artfrom.space/blog).
#
# Behaviour:
#   - DISCORD_WEBHOOK_URL empty -> log + exit 0 (so the first pipeline after
#     merging the change doesn't go red while secrets are being added).
#   - Transport error / 5xx from Discord -> log warning + exit 0 (the build
#     is already shipped; chat outages don't fail the pipeline).
#   - 4xx from Discord -> exit 1 (config error: dead webhook — visible as an
#     allow_failure yellow "!" instead of rotting green forever).
#   - DRY_RUN=1 -> print the JSON payload to stdout instead of POSTing.
set -euo pipefail

if [ -z "${DISCORD_WEBHOOK_URL:-}" ]; then
  echo "Skipping Discord notification — DISCORD_WEBHOOK_URL not set in CI/CD variables."
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

RESP_TMP=$(mktemp -t discord_response.XXXXXX)
trap 'rm -f "$RESP_TMP"' EXIT

if echo "$CI_COMMIT_TAG" | grep -q '\-beta'; then
  TITLE="Athenaeum ${CI_COMMIT_TAG} (beta) released"
  COLOR=15976499  # #f39c12 amber
else
  TITLE="Athenaeum ${CI_COMMIT_TAG} released"
  COLOR=3066993   # #2ecc71 green
fi

if [ -s "$NOTES_PATH" ]; then
  BODY=$(RELEASE_URL="$RELEASE_URL" "$SCRIPT_DIR/truncate_release_notes.sh" < "$NOTES_PATH")
else
  BODY="Athenaeum ${CI_COMMIT_TAG} is out — see the release page for details."
fi

PAYLOAD=$(
  TITLE="$TITLE" \
  BODY="$BODY" \
  RELEASE_URL="$RELEASE_URL" \
  COLOR="$COLOR" \
  python3 -c '
import json, os
payload = {
    "embeds": [{
        "title": os.environ["TITLE"],
        "url":   os.environ["RELEASE_URL"],
        "description": os.environ["BODY"],
        "color": int(os.environ["COLOR"]),
        "fields": [{
            "name":  "Download",
            "value": "[artfrom.space/releases/download](https://artfrom.space/releases/download/)",
        }],
    }],
}
print(json.dumps(payload))
'
) || {
  echo "WARNING: Failed to build Discord JSON payload. Pipeline continues."
  exit 0
}

if [ "${DRY_RUN:-}" = "1" ]; then
  echo "$PAYLOAD"
  exit 0
fi

# Use --data @- (stdin) rather than --data "$PAYLOAD" so the embed body
# isn't visible to other jobs via ps(1) on shared runners.
http_code=$(
  printf '%s' "$PAYLOAD" | curl --silent --show-error --max-time 30 \
    --output "$RESP_TMP" \
    --write-out '%{http_code}' \
    --request POST \
    --header 'Content-Type: application/json' \
    --data @- \
    "$DISCORD_WEBHOOK_URL" \
    || echo "000"
)

if [ "$http_code" -ge 200 ] && [ "$http_code" -lt 300 ]; then
  echo "Discord notification posted (HTTP $http_code)."
elif [ "$http_code" -ge 400 ] && [ "$http_code" -lt 500 ]; then
  # 4xx is a configuration error (dead webhook URL) — it will fail every
  # future release identically, so surface it as a job failure. The job is
  # allow_failure: the pipeline still passes, but the yellow "!" is visible.
  echo "ERROR: Discord notification rejected (HTTP $http_code) — check DISCORD_WEBHOOK_URL."
  echo "Response body:"
  cat "$RESP_TMP" 2>/dev/null || true
  exit 1
else
  # Transport trouble (timeout, 5xx) is transient — the build already shipped.
  echo "WARNING: Discord notification failed (HTTP $http_code). Pipeline continues."
  echo "Response body:"
  cat "$RESP_TMP" 2>/dev/null || true
fi
exit 0

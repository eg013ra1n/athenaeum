#!/usr/bin/env bash
# Keep the Docker Hub repository description in sync with which version the
# rolling `:latest` / `:beta` tags currently point at (hub.docker.com has no
# other way to show this short of comparing image digests by hand).
#
# Required env: DOCKERHUB_USERNAME, DOCKERHUB_TOKEN, DOCKERHUB_REPO
#               (e.g. vsharifov/athenaeum), CHANNEL_TAG (latest|beta),
#               VERSION (e.g. 0.2.3), CI_COMMIT_TAG (e.g. v0.2.3).
#
# Behaviour (same soft-fail / skip-on-missing-secret semantics as
# notify_discord.sh / notify_telegram.sh):
#   - Any required env var empty -> log + exit 0.
#   - Docker Hub login / fetch / update failing for any reason (auth, network,
#     unexpected response shape) -> log a warning + exit 0. The image is
#     already pushed by the time this runs; a Hub Web UI hiccup must not fail
#     the release pipeline.
#
# What it does:
#   1. Logs in to the Docker Hub v2 API to get a short-lived JWT.
#   2. Fetches the repository's current `full_description`.
#   3. Rewrites a managed block (delimited by HTML comments) containing a
#      "Tag | Version | Git tag | Published (UTC)" table, updating only the
#      row for $CHANNEL_TAG and preserving any other channel's row already in
#      the block. Everything outside the block is left untouched; if no block
#      exists yet, one is appended at the end of the existing description.
#   4. PATCHes the repository with the merged description.
#
# JSON is built/parsed with python3 rather than jq — this repo's other CI
# helpers (notify_discord.sh, notify_telegram.sh, the `release` job inline
# script) all do the same and python3 is already assumed present on the
# runner; no need to introduce a second JSON tool.
set -euo pipefail

require_env() {
  local name="$1" desc="$2" value
  value="${!name:-}"
  if [ -z "$value" ]; then
    echo "Skipping Docker Hub description update — ${name} not set (${desc})."
    exit 0
  fi
}

require_env DOCKERHUB_USERNAME "Docker Hub username"
require_env DOCKERHUB_TOKEN "Docker Hub access token"
require_env DOCKERHUB_REPO "Docker Hub repo, e.g. vsharifov/athenaeum"
require_env CHANNEL_TAG "expected latest|beta"
require_env VERSION "release version, e.g. 0.2.3"
require_env CI_COMMIT_TAG "git tag, e.g. v0.2.3"

case "$CHANNEL_TAG" in
  latest | beta) ;;
  *)
    echo "WARNING: unexpected CHANNEL_TAG '${CHANNEL_TAG}' (expected latest|beta). Skipping Docker Hub description update."
    exit 0
    ;;
esac

RESP_TMP=$(mktemp -t dockerhub_response.XXXXXX)
trap 'rm -f "$RESP_TMP"' EXIT

REPO_URL="https://hub.docker.com/v2/repositories/${DOCKERHUB_REPO}/"

# 1. Log in for a short-lived JWT (Docker Hub's v2 API does not accept the
#    access token directly as a Bearer/Basic credential for these endpoints).
LOGIN_PAYLOAD=$(
  DOCKERHUB_USERNAME="$DOCKERHUB_USERNAME" \
  DOCKERHUB_TOKEN="$DOCKERHUB_TOKEN" \
  python3 -c '
import json, os
print(json.dumps({
    "username": os.environ["DOCKERHUB_USERNAME"],
    "password": os.environ["DOCKERHUB_TOKEN"],
}))
'
)

http_code=$(
  printf '%s' "$LOGIN_PAYLOAD" | curl --silent --show-error --max-time 30 \
    --output "$RESP_TMP" \
    --write-out '%{http_code}' \
    --request POST \
    --header 'Content-Type: application/json' \
    --data @- \
    "https://hub.docker.com/v2/users/login" \
    || echo "000"
)

if [ "$http_code" -lt 200 ] || [ "$http_code" -ge 300 ]; then
  echo "WARNING: Docker Hub login failed (HTTP $http_code). Skipping description update."
  cat "$RESP_TMP" 2>/dev/null || true
  exit 0
fi

JWT=$(python3 -c '
import json, sys
try:
    with open(sys.argv[1]) as f:
        print(json.load(f).get("token", ""))
except Exception:
    print("")
' "$RESP_TMP")

if [ -z "$JWT" ]; then
  echo "WARNING: Docker Hub login response had no token. Skipping description update."
  exit 0
fi

# 2. Fetch the current description.
http_code=$(
  curl --silent --show-error --max-time 30 \
    --output "$RESP_TMP" \
    --write-out '%{http_code}' \
    --request GET \
    --header "Authorization: JWT ${JWT}" \
    "$REPO_URL" \
    || echo "000"
)

if [ "$http_code" -lt 200 ] || [ "$http_code" -ge 300 ]; then
  echo "WARNING: Fetching Docker Hub repository info failed (HTTP $http_code). Skipping description update."
  cat "$RESP_TMP" 2>/dev/null || true
  exit 0
fi

CURRENT_DESCRIPTION=$(python3 -c '
import json, sys
try:
    with open(sys.argv[1]) as f:
        print(json.load(f).get("full_description") or "")
except Exception:
    print("")
' "$RESP_TMP")

# 3. Merge the managed version-table block into the description.
PUBLISHED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
NEW_DESCRIPTION=$(
  CHANNEL_TAG="$CHANNEL_TAG" \
  VERSION="$VERSION" \
  CI_COMMIT_TAG="$CI_COMMIT_TAG" \
  PUBLISHED_AT="$PUBLISHED_AT" \
  python3 -c '
import os
import re
import sys

current = sys.stdin.read()
channel = os.environ["CHANNEL_TAG"]
version = os.environ["VERSION"]
git_tag = os.environ["CI_COMMIT_TAG"]
published = os.environ["PUBLISHED_AT"]

START = "<!-- athenaeum:versions:start -->"
END = "<!-- athenaeum:versions:end -->"
NOTE = (
    "Check a running container'"'"'s version: it is the first line of "
    "`docker logs <container>` and in the About page."
)

rows = {"latest": None, "beta": None}

match = re.search(re.escape(START) + r"(.*?)" + re.escape(END), current, re.DOTALL)
if match:
    for line in match.group(1).splitlines():
        line = line.strip()
        if not line.startswith("|"):
            continue
        cells = [c.strip() for c in line.strip("|").split("|")]
        if len(cells) != 4:
            continue
        if cells[0] in rows:
            rows[cells[0]] = cells

# Overwrite only the row for the channel this run just published.
rows[channel] = [channel, version, git_tag, published]

table_lines = [
    "| Tag | Version | Git tag | Published (UTC) |",
    "| --- | --- | --- | --- |",
]
for tag in ("latest", "beta"):
    row = rows[tag]
    if row:
        table_lines.append("| {} | {} | {} | {} |".format(*row))

block = "\n".join([START, *table_lines, "", NOTE, END])

if match:
    updated = current[: match.start()] + block + current[match.end() :]
elif current.strip():
    updated = current.rstrip("\n") + "\n\n" + block
else:
    updated = block

sys.stdout.write(updated)
' <<< "$CURRENT_DESCRIPTION"
)

# 4. PATCH the repository with the merged description.
PATCH_PAYLOAD=$(
  NEW_DESCRIPTION="$NEW_DESCRIPTION" \
  python3 -c '
import json, os
print(json.dumps({"full_description": os.environ["NEW_DESCRIPTION"]}))
'
)

http_code=$(
  printf '%s' "$PATCH_PAYLOAD" | curl --silent --show-error --max-time 30 \
    --output "$RESP_TMP" \
    --write-out '%{http_code}' \
    --request PATCH \
    --header "Authorization: JWT ${JWT}" \
    --header 'Content-Type: application/json' \
    --data @- \
    "$REPO_URL" \
    || echo "000"
)

if [ "$http_code" -ge 200 ] && [ "$http_code" -lt 300 ]; then
  echo "Docker Hub description updated for ${DOCKERHUB_REPO} (HTTP $http_code)."
else
  echo "WARNING: Docker Hub description update failed (HTTP $http_code). Pipeline continues."
  cat "$RESP_TMP" 2>/dev/null || true
fi
exit 0

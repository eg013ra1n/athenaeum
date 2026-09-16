#!/usr/bin/env bash
# Bash test runner for CI notification helper scripts.
# Usage: .gitlab/ci/scripts/test/run_tests.sh
# Exits 0 if all assertions pass, non-zero if any failed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HELPERS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$HELPERS_DIR/../../.." && pwd)"
FIXTURES_DIR="$SCRIPT_DIR/fixtures"

PASS=0
FAIL=0

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "  ok: $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $label"
    echo "    expected: $expected"
    echo "    actual:   $actual"
    FAIL=$((FAIL + 1))
  fi
}

assert_contains() {
  local label="$1" needle="$2" haystack="$3"
  # `--` so a needle starting with "-" (e.g. a markdown list marker) isn't
  # parsed as a grep option on BSD/macOS.
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    echo "  ok: $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $label"
    echo "    needle missing: $needle"
    echo "    in: $haystack"
    FAIL=$((FAIL + 1))
  fi
}

assert_not_contains() {
  local label="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF -- "$needle"; then
    echo "  FAIL: $label"
    echo "    needle present: $needle"
    echo "    in: $haystack"
    FAIL=$((FAIL + 1))
  else
    echo "  ok: $label"
    PASS=$((PASS + 1))
  fi
}

# Exact-line match (unlike assert_contains' substring match, this doesn't
# false-positive on e.g. "IMAGE:0.2.3" being a substring of "IMAGE:0.2.3-amd64").
assert_file_has_line() {
  local label="$1" needle="$2" file="$3"
  if grep -qFx -- "$needle" "$file"; then
    echo "  ok: $label"
    PASS=$((PASS + 1))
  else
    echo "  FAIL: $label"
    echo "    line missing: $needle"
    echo "    in file ($file):"
    sed 's/^/      /' "$file"
    FAIL=$((FAIL + 1))
  fi
}

echo "== CI notification helper tests =="

echo
echo "-- truncate_release_notes.sh --"

# Short input passes through unchanged (no tail).
out=$(RELEASE_URL="https://example.com/release" "$HELPERS_DIR/truncate_release_notes.sh" < "$FIXTURES_DIR/short.md")
assert_contains "short input keeps body" "small" "$out"
assert_not_contains "short input has no truncation tail" "Full notes" "$out"
# Bytes, not chars — the truncator's budget is byte-based (matches Discord/Telegram caps).
short_len=$(printf '%s' "$out" | wc -c | tr -d ' ')
if [ "$short_len" -lt 4096 ]; then
  echo "  ok: short output fits in 4096 bytes (actual: $short_len)"
  PASS=$((PASS + 1))
else
  echo "  FAIL: short output exceeded 4096 bytes (actual: $short_len)"
  FAIL=$((FAIL + 1))
fi

# Long input is truncated and gets the tail.
out=$(RELEASE_URL="https://example.com/release" "$HELPERS_DIR/truncate_release_notes.sh" < "$FIXTURES_DIR/long.md")
assert_contains "long input gets truncation tail" "Full notes: https://example.com/release" "$out"
long_len=$(printf '%s' "$out" | wc -c | tr -d ' ')
if [ "$long_len" -le 4096 ]; then
  echo "  ok: long output fits in 4096 bytes (actual: $long_len)"
  PASS=$((PASS + 1))
else
  echo "  FAIL: long output exceeded 4096 bytes (actual: $long_len)"
  FAIL=$((FAIL + 1))
fi

echo
echo "-- md_to_telegram_html.sh --"

out=$("$HELPERS_DIR/md_to_telegram_html.sh" < "$FIXTURES_DIR/markdown_features.md")

# HTML escaping happens BEFORE other conversions, so a literal "<" in input
# becomes "&lt;" in output. A "<b>" produced by us is also fine — the escaping
# only runs on the original input characters, not on tags we add.
assert_contains "escape ampersand" "10 &amp; 10" "$out"
assert_contains "escape less-than"   "5 &lt; 10"  "$out"
assert_contains "escape greater-than" "10 &gt; 5"  "$out"

# **bold** -> <b>bold</b>
assert_contains "bold conversion"   "<b>bold</b>"   "$out"

# *italic* -> <i>italic</i>
assert_contains "italic conversion" "<i>italic</i>" "$out"

# `inline code` -> <code>inline code</code>
assert_contains "code conversion"   "<code>inline code</code>" "$out"

# [link](url) -> <a href="url">link</a>
assert_contains "link conversion"   '<a href="https://example.com">link</a>' "$out"

# Headings become bold lines, with NO leading "# " hash chars.
assert_contains "heading 1 -> bold" "<b>Heading One</b>"   "$out"
assert_contains "heading 2 -> bold" "<b>Heading Two</b>"   "$out"
assert_contains "heading 3 -> bold" "<b>Heading Three</b>" "$out"
assert_not_contains "no leading hashes remain" "# Heading" "$out"

# List markers stay as plain dashes/asterisks at line start.
assert_contains "dash list item kept"     "- list item one" "$out"
assert_contains "asterisk list item kept" "* list item three" "$out"

echo
echo "-- notify_discord.sh --"

# Skip cleanly when DISCORD_WEBHOOK_URL is empty.
out=$(
  DISCORD_WEBHOOK_URL="" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_discord.sh" 2>&1
)
assert_contains "skip when webhook unset" "Skipping Discord" "$out"

# Dry-run for stable tag prints a JSON payload with the expected shape.
out=$(
  DRY_RUN=1 \
  DISCORD_WEBHOOK_URL="https://discord.com/api/webhooks/dummy" \
  CI_COMMIT_TAG="v0.2.0" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_discord.sh"
)
assert_contains "dry-run prints title" "Athenaeum v0.2.0 released" "$out"
assert_contains "dry-run prints stable color" "3066993" "$out"
assert_contains "dry-run prints release URL" "https://example.com/blog/v020/" "$out"
assert_contains "dry-run includes download field" "artfrom.space/releases/download" "$out"
assert_not_contains "stable title omits beta marker" "(beta)" "$out"

# Beta tag uses the beta colour and labels the title.
out=$(
  DRY_RUN=1 \
  DISCORD_WEBHOOK_URL="https://discord.com/api/webhooks/dummy" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_discord.sh"
)
assert_contains "beta title labels beta" "Athenaeum v0.2.0-beta.10 (beta) released" "$out"
assert_contains "beta uses amber color" "15976499" "$out"

# Missing RELEASE_NOTES.md falls back to a one-liner (does not fail).
out=$(
  DRY_RUN=1 \
  DISCORD_WEBHOOK_URL="https://discord.com/api/webhooks/dummy" \
  CI_COMMIT_TAG="v0.2.0" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="/nonexistent/RELEASE_NOTES.md" \
  "$HELPERS_DIR/notify_discord.sh"
)
assert_contains "fallback body when notes missing" "is out" "$out"

echo
echo "-- notify_telegram.sh --"

# Skip cleanly when TELEGRAM_BOT_TOKEN is empty (chat id alone isn't enough).
out=$(
  TELEGRAM_BOT_TOKEN="" \
  TELEGRAM_CHAT_ID="123" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_telegram.sh" 2>&1
)
assert_contains "skip when bot token unset" "Skipping Telegram" "$out"

# Skip cleanly when TELEGRAM_CHAT_ID is empty.
out=$(
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_telegram.sh" 2>&1
)
assert_contains "skip when chat id unset" "Skipping Telegram" "$out"

# Dry-run for stable tag.
out=$(
  DRY_RUN=1 \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_telegram.sh"
)
assert_contains "dry-run prints chat id" "@athenaeum_releases" "$out"
assert_contains "dry-run uses HTML parse mode" "HTML" "$out"
assert_contains "dry-run wraps title in bold" "<b>Athenaeum v0.2.0 released</b>" "$out"
assert_contains "dry-run converts inline code in body" "<code>inline code</code>" "$out"
assert_contains "dry-run includes release URL" "https://example.com/blog/v020/" "$out"
assert_not_contains "stable title omits beta marker" "(beta)" "$out"

# Beta tag.
out=$(
  DRY_RUN=1 \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_telegram.sh"
)
assert_contains "beta title labels beta" "<b>Athenaeum v0.2.0-beta.10 (beta) released</b>" "$out"

# Long input gets truncated AND HTML-converted.
out=$(
  DRY_RUN=1 \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/long.md" \
  "$HELPERS_DIR/notify_telegram.sh"
)
assert_contains "long telegram body has truncation tail" "Full notes: https://example.com/blog/v020-beta10/" "$out"

# Missing RELEASE_NOTES.md falls back to a one-liner.
out=$(
  DRY_RUN=1 \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="/nonexistent/RELEASE_NOTES.md" \
  "$HELPERS_DIR/notify_telegram.sh"
)
assert_contains "fallback body when notes missing" "is out" "$out"

# --- live-HTTP branches via the curl mock (no real network) ---
# A 4xx is a config error (revoked bot token, dead webhook): it must FAIL the
# job — allow_failure renders it as a yellow "!" — instead of exiting green.
# A real 401 rotted unseen for four releases behind the old soft-fail.
rc=0
out=$(
  PATH="$FIXTURES_DIR/mock_bin:$PATH" \
  MOCK_CURL_NOTIFY_HTTP=401 \
  MOCK_CURL_NOTIFY_BODY='{"ok":false,"error_code":401,"description":"Unauthorized"}' \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_telegram.sh" 2>&1
) || rc=$?
assert_eq "telegram 401 exits non-zero" "1" "$rc"
assert_contains "telegram 401 names the variables" "check TELEGRAM_BOT_TOKEN / TELEGRAM_CHAT_ID" "$out"
assert_contains "telegram 401 echoes the response body" "Unauthorized" "$out"

# Transport trouble / 5xx stays soft: the build already shipped.
rc=0
out=$(
  PATH="$FIXTURES_DIR/mock_bin:$PATH" \
  MOCK_CURL_NOTIFY_HTTP=503 \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_telegram.sh" 2>&1
) || rc=$?
assert_eq "telegram 5xx stays soft" "0" "$rc"
assert_contains "telegram 5xx warns and continues" "WARNING: Telegram notification failed (HTTP 503)" "$out"

# Happy path still reports the post.
out=$(
  PATH="$FIXTURES_DIR/mock_bin:$PATH" \
  MOCK_CURL_NOTIFY_HTTP=200 \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_telegram.sh" 2>&1
)
assert_contains "telegram 200 posts" "Telegram notification posted (HTTP 200)" "$out"

# Discord mirrors the same 4xx-is-config-error contract.
rc=0
out=$(
  PATH="$FIXTURES_DIR/mock_bin:$PATH" \
  MOCK_CURL_NOTIFY_HTTP=404 \
  DISCORD_WEBHOOK_URL="https://discord.com/api/webhooks/dummy" \
  CI_COMMIT_TAG="v0.2.0" \
  RELEASE_NOTES_BASE_URL="https://example.com/blog" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_discord.sh" 2>&1
) || rc=$?
assert_eq "discord 404 exits non-zero" "1" "$rc"
assert_contains "discord 404 names the variable" "check DISCORD_WEBHOOK_URL" "$out"

echo
echo "-- update_dockerhub_description.sh --"

# Mock `curl` (fixtures/mock_bin/curl) so no test hits the real Docker Hub
# API. It's steered by MOCK_CURL_* env vars — see that file's header comment.
MOCK_BIN="$FIXTURES_DIR/mock_bin"
DOCKERHUB_SCRIPT="$HELPERS_DIR/update_dockerhub_description.sh"

# Skip cleanly when a required env var is unset — DOCKERHUB_USERNAME here.
out=$(
  DOCKERHUB_USERNAME="" \
  DOCKERHUB_TOKEN="dummy" \
  DOCKERHUB_REPO="vsharifov/athenaeum" \
  CHANNEL_TAG="latest" \
  VERSION="0.2.3" \
  CI_COMMIT_TAG="v0.2.3" \
  "$DOCKERHUB_SCRIPT" < /dev/null 2>&1
)
assert_contains "skip when DOCKERHUB_USERNAME unset" "Skipping Docker Hub description update" "$out"
assert_contains "skip message names the missing var" "DOCKERHUB_USERNAME not set" "$out"

# Skip cleanly on an unexpected CHANNEL_TAG value too (defensive; the caller
# only ever passes latest|beta, but the script must not silently corrupt the
# description if that ever drifts).
out=$(
  DOCKERHUB_USERNAME="user" \
  DOCKERHUB_TOKEN="tok" \
  DOCKERHUB_REPO="vsharifov/athenaeum" \
  CHANNEL_TAG="nightly" \
  VERSION="0.2.3" \
  CI_COMMIT_TAG="v0.2.3" \
  "$DOCKERHUB_SCRIPT" < /dev/null 2>&1
)
assert_contains "skip on unexpected CHANNEL_TAG" "unexpected CHANNEL_TAG" "$out"

# Fresh description (no managed block yet) -> block is appended, original
# content preserved.
PATCH_TMP=$(mktemp -t dockerhub_patch_body.XXXXXX)
trap 'rm -f "$PATCH_TMP"' EXIT
out=$(
  PATH="$MOCK_BIN:$PATH" \
  MOCK_CURL_GET_BODY_FILE="$FIXTURES_DIR/dockerhub_fresh_description.json" \
  MOCK_CURL_CAPTURE_PATCH_BODY="$PATCH_TMP" \
  DOCKERHUB_USERNAME="user" \
  DOCKERHUB_TOKEN="tok" \
  DOCKERHUB_REPO="vsharifov/athenaeum" \
  CHANNEL_TAG="latest" \
  VERSION="0.2.3" \
  CI_COMMIT_TAG="v0.2.3" \
  "$DOCKERHUB_SCRIPT" < /dev/null 2>&1
)
assert_contains "fresh-description run reports success" "Docker Hub description updated" "$out"
patch_body=$(cat "$PATCH_TMP")
assert_contains "fresh description preserved" "FITS/XISF catalog manager" "$patch_body"
assert_contains "block appended with start marker" "athenaeum:versions:start" "$patch_body"
assert_contains "block appended with end marker" "athenaeum:versions:end" "$patch_body"
assert_contains "new latest row present" "| latest | 0.2.3 | v0.2.3 |" "$patch_body"
assert_contains "static help line present" "and in the About page." "$patch_body"

# Existing block with both rows -> only the CHANNEL_TAG row is replaced, the
# other channel's row and any trailing content are preserved verbatim.
out=$(
  PATH="$MOCK_BIN:$PATH" \
  MOCK_CURL_GET_BODY_FILE="$FIXTURES_DIR/dockerhub_existing_versions.json" \
  MOCK_CURL_CAPTURE_PATCH_BODY="$PATCH_TMP" \
  DOCKERHUB_USERNAME="user" \
  DOCKERHUB_TOKEN="tok" \
  DOCKERHUB_REPO="vsharifov/athenaeum" \
  CHANNEL_TAG="latest" \
  VERSION="0.2.3" \
  CI_COMMIT_TAG="v0.2.3" \
  "$DOCKERHUB_SCRIPT" < /dev/null 2>&1
)
assert_contains "existing-block run reports success" "Docker Hub description updated" "$out"
patch_body=$(cat "$PATCH_TMP")
assert_contains "latest row replaced with new version" "| latest | 0.2.3 | v0.2.3 |" "$patch_body"
assert_not_contains "stale latest row is gone" "| latest | 0.2.2 | v0.2.2 |" "$patch_body"
assert_contains "beta row preserved untouched" "| beta | 0.2.3-beta.1 | v0.2.3-beta.1 | 2026-06-30T08:00:00Z |" "$patch_body"
assert_contains "trailing content preserved" "Some trailing content that must survive." "$patch_body"
assert_contains "leading content preserved" "Athenaeum Docker image." "$patch_body"

# API failure (login rejected) -> warning logged, script still exits 0 so the
# release pipeline is never failed by a Docker Hub outage.
rc=0
out=$(
  PATH="$MOCK_BIN:$PATH" \
  MOCK_CURL_LOGIN_HTTP=500 \
  DOCKERHUB_USERNAME="user" \
  DOCKERHUB_TOKEN="tok" \
  DOCKERHUB_REPO="vsharifov/athenaeum" \
  CHANNEL_TAG="latest" \
  VERSION="0.2.3" \
  CI_COMMIT_TAG="v0.2.3" \
  "$DOCKERHUB_SCRIPT" < /dev/null 2>&1
) || rc=$?
assert_contains "login failure logs a warning" "WARNING: Docker Hub login failed" "$out"
assert_eq "login failure still exits 0" "0" "$rc"

# API failure (PATCH rejected after a good login/GET) -> same soft-fail.
rc=0
out=$(
  PATH="$MOCK_BIN:$PATH" \
  MOCK_CURL_GET_BODY_FILE="$FIXTURES_DIR/dockerhub_fresh_description.json" \
  MOCK_CURL_PATCH_HTTP=503 \
  DOCKERHUB_USERNAME="user" \
  DOCKERHUB_TOKEN="tok" \
  DOCKERHUB_REPO="vsharifov/athenaeum" \
  CHANNEL_TAG="beta" \
  VERSION="0.2.3-beta.2" \
  CI_COMMIT_TAG="v0.2.3-beta.2" \
  "$DOCKERHUB_SCRIPT" < /dev/null 2>&1
) || rc=$?
assert_contains "patch failure logs a warning" "WARNING: Docker Hub description update failed" "$out"
assert_eq "patch failure still exits 0" "0" "$rc"

rm -f "$PATCH_TMP"

echo
echo "-- dockerhub_merge_manifest.sh --"

# Mocks `docker` (fixtures/mock_bin/docker) and reuses the `curl` mock above
# (update_dockerhub_description.sh, which this script calls on success, hits
# the same mocked registry HTTP calls) so no test touches a real Docker
# daemon, registry, or the Docker Hub API.
MERGE_SCRIPT="$HELPERS_DIR/dockerhub_merge_manifest.sh"
CREATE_ARGS_TMP=$(mktemp -t dockerhub_create_args.XXXXXX)
trap 'rm -f "$CREATE_ARGS_TMP"' EXIT

# Skip cleanly when Docker Hub credentials are unset — same soft-fail
# semantics as the arch build jobs and the other CI helpers in this dir.
rc=0
out=$(
  DOCKERHUB_USERNAME="" \
  DOCKERHUB_TOKEN="tok" \
  IMAGE="docker.io/vsharifov/athenaeum" \
  VERSION="0.2.3" \
  CHANNEL_TAG="latest" \
  CI_COMMIT_TAG="v0.2.3" \
  "$MERGE_SCRIPT" 2>&1
) || rc=$?
assert_contains "skip when DOCKERHUB_USERNAME unset" "skipping Docker Hub manifest merge" "$out"
assert_eq "skip still exits 0" "0" "$rc"

# Both arch tags present -> imagetools create merges both into the version
# tag + channel tag, then update_dockerhub_description.sh is invoked.
: > "$CREATE_ARGS_TMP"
rc=0
out=$(
  PATH="$MOCK_BIN:$PATH" \
  MOCK_DOCKER_AMD64_EXISTS=1 \
  MOCK_DOCKER_ARM64_EXISTS=1 \
  MOCK_DOCKER_CAPTURE_CREATE_ARGS="$CREATE_ARGS_TMP" \
  MOCK_CURL_GET_BODY_FILE="$FIXTURES_DIR/dockerhub_fresh_description.json" \
  DOCKERHUB_USERNAME="user" \
  DOCKERHUB_TOKEN="tok" \
  IMAGE="docker.io/vsharifov/athenaeum" \
  VERSION="0.2.3" \
  CHANNEL_TAG="latest" \
  CI_COMMIT_TAG="v0.2.3" \
  "$MERGE_SCRIPT" 2>&1
) || rc=$?
assert_eq "both-arch run exits 0" "0" "$rc"
assert_contains "both-arch run finds amd64 tag" "Found docker.io/vsharifov/athenaeum:0.2.3-amd64" "$out"
assert_contains "both-arch run finds arm64 tag" "Found docker.io/vsharifov/athenaeum:0.2.3-arm64" "$out"
assert_contains "both-arch run calls description script" "Docker Hub description updated" "$out"
assert_file_has_line "create includes plain version tag"  "docker.io/vsharifov/athenaeum:0.2.3"       "$CREATE_ARGS_TMP"
assert_file_has_line "create includes channel tag"         "docker.io/vsharifov/athenaeum:latest"      "$CREATE_ARGS_TMP"
assert_file_has_line "create includes amd64 source"        "docker.io/vsharifov/athenaeum:0.2.3-amd64" "$CREATE_ARGS_TMP"
assert_file_has_line "create includes arm64 source"        "docker.io/vsharifov/athenaeum:0.2.3-arm64" "$CREATE_ARGS_TMP"

# arm64 tag absent (e.g. the macOS job failed / Docker Desktop was closed) ->
# LOUD warning, manifest still created amd64-only, description script still
# runs.
: > "$CREATE_ARGS_TMP"
rc=0
out=$(
  PATH="$MOCK_BIN:$PATH" \
  MOCK_DOCKER_AMD64_EXISTS=1 \
  MOCK_DOCKER_ARM64_EXISTS=0 \
  MOCK_DOCKER_CAPTURE_CREATE_ARGS="$CREATE_ARGS_TMP" \
  MOCK_CURL_GET_BODY_FILE="$FIXTURES_DIR/dockerhub_fresh_description.json" \
  DOCKERHUB_USERNAME="user" \
  DOCKERHUB_TOKEN="tok" \
  IMAGE="docker.io/vsharifov/athenaeum" \
  VERSION="0.2.3" \
  CHANNEL_TAG="latest" \
  CI_COMMIT_TAG="v0.2.3" \
  "$MERGE_SCRIPT" 2>&1
) || rc=$?
assert_eq "arm64-absent run exits 0" "0" "$rc"
assert_contains "arm64-absent run warns" "WARNING: docker.io/vsharifov/athenaeum:0.2.3-arm64 not found on Docker Hub" "$out"
assert_contains "arm64-absent run still calls description script" "Docker Hub description updated" "$out"
assert_file_has_line "amd64-only create includes plain version tag" "docker.io/vsharifov/athenaeum:0.2.3"       "$CREATE_ARGS_TMP"
assert_file_has_line "amd64-only create includes amd64 source"      "docker.io/vsharifov/athenaeum:0.2.3-amd64" "$CREATE_ARGS_TMP"
arm64_lines=$(grep -Fxc "docker.io/vsharifov/athenaeum:0.2.3-arm64" "$CREATE_ARGS_TMP" || true)
assert_eq "amd64-only create excludes arm64 source" "0" "$arm64_lines"

# amd64 tag absent -> hard error, exit 1 (nothing to publish without it),
# imagetools create must never run.
: > "$CREATE_ARGS_TMP"
rc=0
out=$(
  PATH="$MOCK_BIN:$PATH" \
  MOCK_DOCKER_AMD64_EXISTS=0 \
  MOCK_DOCKER_CAPTURE_CREATE_ARGS="$CREATE_ARGS_TMP" \
  DOCKERHUB_USERNAME="user" \
  DOCKERHUB_TOKEN="tok" \
  IMAGE="docker.io/vsharifov/athenaeum" \
  VERSION="0.2.3" \
  CHANNEL_TAG="latest" \
  CI_COMMIT_TAG="v0.2.3" \
  "$MERGE_SCRIPT" 2>&1
) || rc=$?
assert_eq "amd64-absent run exits 1" "1" "$rc"
assert_contains "amd64-absent run reports error" "ERROR: docker.io/vsharifov/athenaeum:0.2.3-amd64 not found on Docker Hub" "$out"
assert_eq "amd64-absent run never calls imagetools create" "" "$(cat "$CREATE_ARGS_TMP")"

rm -f "$CREATE_ARGS_TMP"

echo
echo "-- artifact_names.sh --"

# shellcheck disable=SC1091
. "$HELPERS_DIR/artifact_names.sh"

out=$(VERSION=0.6.4 artifact_name athenaeum macos arm64 dmg)
assert_eq "athenaeum dmg name" "athenaeum-0.6.4-macos-arm64.dmg" "$out"
out=$(VERSION=0.6.4-beta.1 artifact_name athenaeum windows x64 exe setup)
assert_eq "windows setup name with variant and beta" "athenaeum-0.6.4-beta.1-windows-x64-setup.exe" "$out"
out=$(VERSION=0.6.4 artifact_name perseus linux arm64 tar.gz)
assert_eq "perseus tarball name" "perseus-0.6.4-linux-arm64.tar.gz" "$out"
out=$(alias_name athenaeum linux x64 AppImage)
assert_eq "alias drops the version only" "athenaeum-linux-x64.AppImage" "$out"

rc=0
out=$(VERSION=0.6.4 artifact_name athenaeum linux amd64 deb 2>&1) || rc=$?
assert_eq "amd64 is refused as an arch" "1" "$rc"
assert_contains "refusal names the vocabulary" "arch must be x64 or arm64" "$out"

inventory=$(VERSION=0.6.4 all_release_artifacts)
assert_eq "inventory has 13 shipped files" "13" "$(printf '%s\n' "$inventory" | wc -l | tr -d ' ')"
assert_contains "inventory: msi" "athenaeum windows x64 msi - windows athenaeum-0.6.4-windows-x64.msi athenaeum-windows-x64.msi" "$inventory"
assert_contains "inventory: perseus x64 deb" "perseus linux x64 deb - perseus perseus-0.6.4-linux-x64.deb perseus-linux-x64.deb" "$inventory"
assert_not_contains "inventory never says amd64" "amd64" "$inventory"
assert_not_contains "inventory never says aarch64" "aarch64" "$inventory"
assert_not_contains "inventory never says x86_64" "x86_64" "$inventory"

legacy=$(VERSION=0.6.4 legacy_alias_pairs)
assert_contains "legacy: old macOS alias maps to the new file" "macos Athenaeum-macos-aarch64.dmg athenaeum-0.6.4-macos-arm64.dmg" "$legacy"
assert_contains "legacy: old perseus deb alias" "perseus perseus-linux-amd64.deb perseus-0.6.4-linux-x64.deb" "$legacy"

ua=$(VERSION=0.7.0 updater_artifacts)
assert_eq "updater: 5 lines" "5" "$(printf '%s\n' "$ua" | wc -l | tr -d ' ')"
assert_file_has_line "updater: macos arm64 app.tar.gz" "athenaeum macos arm64 app.tar.gz - macos athenaeum-0.7.0-macos-arm64.app.tar.gz darwin-aarch64" <(printf '%s\n' "$ua")
assert_file_has_line "updater: macos x64 app.tar.gz" "athenaeum macos x64 app.tar.gz - macos athenaeum-0.7.0-macos-x64.app.tar.gz darwin-x86_64" <(printf '%s\n' "$ua")
assert_file_has_line "updater: windows nsis" "athenaeum windows x64 exe setup windows athenaeum-0.7.0-windows-x64-setup.exe windows-x86_64-nsis" <(printf '%s\n' "$ua")
assert_file_has_line "updater: windows msi" "athenaeum windows x64 msi - windows athenaeum-0.7.0-windows-x64.msi windows-x86_64-msi" <(printf '%s\n' "$ua")
assert_file_has_line "updater: linux appimage" "athenaeum linux x64 AppImage - linux athenaeum-0.7.0-linux-x64.AppImage linux-x86_64" <(printf '%s\n' "$ua")
assert_eq "updater: no plain windows key" "0" "$(printf '%s\n' "$ua" | awk '{print $8}' | grep -cx 'windows-x86_64' || true)"

echo
echo "-- check_versions.sh / bump.sh --"

VT_TMP=$(mktemp -d -t version_tree.XXXXXX)
cp -R "$FIXTURES_DIR/version_tree/." "$VT_TMP/"

# Matching tag passes and lists all six.
rc=0
out=$(REPO_ROOT="$VT_TMP" TAG="v0.6.3" "$HELPERS_DIR/check_versions.sh" 2>&1) || rc=$?
assert_eq "check: matching tag exits 0" "0" "$rc"
assert_contains "check: lists package.json" "package.json: 0.6.3" "$out"
assert_contains "check: lists perseus" "crates/perseus/Cargo.toml: 0.6.3" "$out"
assert_contains "check: success line" "ok: all six versions match 0.6.3" "$out"

# Mismatching tag fails and names the file.
rc=0
out=$(REPO_ROOT="$VT_TMP" TAG="v0.6.4" "$HELPERS_DIR/check_versions.sh" 2>&1) || rc=$?
assert_eq "check: mismatch exits 1" "1" "$rc"
assert_contains "check: mismatch names a file" "ERROR: package.json is 0.6.3, tag v0.6.4 expects 0.6.4" "$out"

# bump rewrites all six (tauri gets the -N form for a beta) and re-checks.
rc=0
out=$(REPO_ROOT="$VT_TMP" BUMP_SKIP_CARGO=1 "$REPO_DIR/scripts/release/bump.sh" 0.7.0-beta.2 2>&1) || rc=$?
assert_eq "bump: exits 0" "0" "$rc"
assert_contains "bump: package.json rewritten" '"version": "0.7.0-beta.2"' "$(cat "$VT_TMP/package.json")"
assert_contains "bump: perseus Cargo.toml rewritten" 'version = "0.7.0-beta.2"' "$(cat "$VT_TMP/crates/perseus/Cargo.toml")"
assert_not_contains "bump: dependency version lines untouched" 'serde = { version = "0.7.0-beta.2"' "$(cat "$VT_TMP/crates/perseus/Cargo.toml")"
assert_contains "bump: tauri.conf gets the -N form" '"version": "0.7.0-2"' "$(cat "$VT_TMP/crates/athenaeum-tauri/tauri.conf.json")"
assert_contains "bump: re-check passes" "ok: all six versions match 0.7.0-beta.2" "$out"

# A malformed version is refused before touching anything.
rc=0
out=$(REPO_ROOT="$VT_TMP" BUMP_SKIP_CARGO=1 "$REPO_DIR/scripts/release/bump.sh" 0.7 2>&1) || rc=$?
assert_eq "bump: malformed version exits 1" "1" "$rc"
assert_contains "bump: malformed version message" "ERROR: version must look like X.Y.Z or X.Y.Z-beta.N" "$out"
assert_contains "bump: nothing rewritten on refusal" '"version": "0.7.0-beta.2"' "$(cat "$VT_TMP/package.json")"

rm -rf "$VT_TMP"

echo
echo "-- gen_release_post.sh --"

out=$(TAG=v0.7.0 RELEASE_DATE=2026-10-01 "$HELPERS_DIR/gen_release_post.sh" < "$FIXTURES_DIR/release_notes_sample.md")
assert_contains "post: title" "title: Athenaeum v0.7.0" "$out"
assert_contains "post: date" "date: 2026-10-01" "$out"
assert_contains "post: author" "  - vilen" "$out"
assert_contains "post: tag" "  - release" "$out"
assert_contains "post: excerpt strips the Athenaeum vX.Y.Z: prefix and re-capitalizes, YAML-quoted" 'excerpt: "A \"quoted\" tagline — with a dash & an ampersand."' "$out"
assert_not_contains "post: body does not repeat the tagline" "*Athenaeum v0.7.0: a" "$out"
assert_contains "post: body keeps the sections" "## Bug Fixes" "$out"
first_body_line=$(printf '%s\n' "$out" | awk 'f&&NF{print;exit} /^---$/{c++; if(c==2)f=1}')
assert_eq "post: body starts at the first heading" "## What's New" "$first_body_line"
out=$("$HELPERS_DIR/gen_release_post.sh" --tagline < "$FIXTURES_DIR/release_notes_sample.md")
assert_eq "post: --tagline strips the Athenaeum vX.Y.Z: prefix and re-capitalizes" 'A "quoted" tagline — with a dash & an ampersand.' "$out"
# Regression: TAG with double quote must not execute code (injection safety).
out=$(TAG='v0.7.0"x' RELEASE_DATE=2026-10-01 "$HELPERS_DIR/gen_release_post.sh" < "$FIXTURES_DIR/release_notes_sample.md" 2>&1)
assert_contains "post: TAG injection is safe" 'title: Athenaeum v0.7.0"x' "$out"

echo
echo "-- update_download_page.sh --"

DP_TMP=$(mktemp -t download_page.XXXXXX)
cp "$FIXTURES_DIR/download_page_sample.md" "$DP_TMP"
rc=0
out=$(PAGE="$DP_TMP" TAG=v0.7.0 RELEASE_DATE=2026-10-01 TAGLINE="A tagline | with a pipe" "$HELPERS_DIR/update_download_page.sh" 2>&1) || rc=$?
assert_eq "page: stable update exits 0" "0" "$rc"
page=$(cat "$DP_TMP")
assert_not_contains "page: old block gone" "old block that must be replaced" "$page"
assert_contains "page: versioned msi link" "[athenaeum-0.7.0-windows-x64.msi](https://artfrom.space/builds/v0.7.0/windows/athenaeum-0.7.0-windows-x64.msi)" "$page"
assert_contains "page: versioned arm64 dmg link" "[athenaeum-0.7.0-macos-arm64.dmg](https://artfrom.space/builds/v0.7.0/macos/athenaeum-0.7.0-macos-arm64.dmg)" "$page"
assert_contains "page: AppImage link" "builds/v0.7.0/linux/athenaeum-0.7.0-linux-x64.AppImage" "$page"
assert_contains "page: markers survive" "<!-- latest-build:end -->" "$page"
assert_contains "page: history row inserted with the pipe escaped" "| v0.7.0 | 2026-10-01 | A tagline \| with a pipe |" "$page"
assert_contains "page: old row kept" "| v0.6.3 | 2026-09-15 | old row |" "$page"
# newest row directly under the marker
after_marker=$(grep -A1 -F '<!-- version-history:rows -->' "$DP_TMP" | tail -n1)
assert_contains "page: new row is first" "| v0.7.0 |" "$after_marker"

# The Latest Build block must never drift from the inventory: every athenaeum
# filename all_release_artifacts knows about has to appear on the page.
while read -r product os arch ext variant subdir filename alias; do
  [ "$product" = "athenaeum" ] || continue
  assert_contains "page: inventory filename $filename present" "$filename" "$page"
done < <(VERSION=0.7.0 all_release_artifacts)

# Same tag again: the row is replaced, not duplicated.
PAGE="$DP_TMP" TAG=v0.7.0 RELEASE_DATE=2026-10-01 TAGLINE="Second wording" "$HELPERS_DIR/update_download_page.sh" >/dev/null 2>&1
assert_eq "page: re-run keeps one v0.7.0 row" "1" "$(grep -c '^| v0.7.0 |' "$DP_TMP")"
assert_contains "page: re-run row carries the new wording" "| v0.7.0 | 2026-10-01 | Second wording |" "$(cat "$DP_TMP")"

# Beta: history row only, Latest Build untouched.
before_block=$(sed -n '/latest-build:start/,/latest-build:end/p' "$DP_TMP")
PAGE="$DP_TMP" TAG=v0.7.1-beta.1 RELEASE_DATE=2026-10-02 TAGLINE="Beta" "$HELPERS_DIR/update_download_page.sh" >/dev/null 2>&1
after_block=$(sed -n '/latest-build:start/,/latest-build:end/p' "$DP_TMP")
assert_eq "page: beta leaves Latest Build alone" "$before_block" "$after_block"
assert_contains "page: beta row added" "| v0.7.1-beta.1 | 2026-10-02 | Beta |" "$(cat "$DP_TMP")"

# Missing marker is a loud failure.
printf '# no markers\n' > "$DP_TMP"
rc=0
out=$(PAGE="$DP_TMP" TAG=v0.7.0 RELEASE_DATE=2026-10-01 TAGLINE="x" "$HELPERS_DIR/update_download_page.sh" 2>&1) || rc=$?
assert_eq "page: missing marker exits 1" "1" "$rc"
assert_contains "page: names the marker" "ERROR: marker <!-- latest-build:start --> not found" "$out"
rm -f "$DP_TMP"

echo
echo "-- docs_publish.sh (dry run against a local clone) --"

DOCS_TMP=$(mktemp -d -t docs_repo.XXXXXX)
git -C "$DOCS_TMP" init -q -b main
mkdir -p "$DOCS_TMP/src/content/docs/blog" "$DOCS_TMP/src/content/docs/releases"
cp "$FIXTURES_DIR/download_page_sample.md" "$DOCS_TMP/src/content/docs/releases/download.md"
git -C "$DOCS_TMP" -c user.name=t -c user.email=t@t add -A
git -C "$DOCS_TMP" -c user.name=t -c user.email=t@t commit -q -m init
rc=0
out=$(DRY_RUN=1 DOCS_REPO_LOCAL="$DOCS_TMP" TAG=v0.7.0 RELEASE_DATE=2026-10-01 RELEASE_NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/docs_publish.sh" 2>&1) || rc=$?
assert_eq "publish: dry run exits 0" "0" "$rc"
assert_contains "publish: would commit the post" "src/content/docs/blog/v0.7.0.md" "$out"
assert_contains "publish: would commit the page" "src/content/docs/releases/download.md" "$out"
assert_contains "publish: commit message" "docs: v0.7.0 release post + download-page row" "$out"
assert_contains "publish: dry run does not push" "DRY_RUN=1: not pushing" "$out"
rm -rf "$DOCS_TMP"

# The token must never reach the clone URL: a refused clone (connection
# refused) must fail loudly without ever echoing the token anywhere in the
# combined output.
rc=0
out=$(DOCS_REPO_TOKEN="SECRET-TOKEN-XYZ" DOCS_REPO_URL="http://127.0.0.1:9/nothing.git" TAG=v0.7.0 \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/docs_publish.sh" 2>&1) || rc=$?
if [ "$rc" -ne 0 ]; then
  echo "  ok: publish: refused clone exits non-zero (actual: $rc)"
  PASS=$((PASS + 1))
else
  echo "  FAIL: publish: refused clone should exit non-zero (actual: $rc)"
  FAIL=$((FAIL + 1))
fi
assert_not_contains "publish: token never appears in the output" "SECRET-TOKEN-XYZ" "$out"

echo
echo "-- github_checks_gate.sh --"

rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_GITHUB_BODY_FILE="$FIXTURES_DIR/github_checks_success.json" \
  CI_COMMIT_SHA=deadbeef GATE_POLL_SECONDS=0 "$HELPERS_DIR/github_checks_gate.sh" 2>&1) || rc=$?
assert_eq "gate: all green exits 0" "0" "$rc"
assert_contains "gate: reports both checks" "ok: Build and test (Windows) — success" "$out"

rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_GITHUB_BODY_FILE="$FIXTURES_DIR/github_checks_failure.json" \
  CI_COMMIT_SHA=deadbeef GATE_POLL_SECONDS=0 "$HELPERS_DIR/github_checks_gate.sh" 2>&1) || rc=$?
assert_eq "gate: a failed check exits 1" "1" "$rc"
assert_contains "gate: names the failed check" "ERROR: Build and test (Windows) concluded failure" "$out"

# pending, then success — the gate polls and passes.
SEQ_TMP=$(mktemp -d -t gate_seq.XXXXXX)
cp "$FIXTURES_DIR/github_checks_pending.json" "$SEQ_TMP/1.json"
cp "$FIXTURES_DIR/github_checks_success.json" "$SEQ_TMP/2.json"
rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_GITHUB_SEQUENCE_DIR="$SEQ_TMP" MOCK_CURL_COUNTER_FILE="$SEQ_TMP/count" \
  CI_COMMIT_SHA=deadbeef GATE_POLL_SECONDS=0 "$HELPERS_DIR/github_checks_gate.sh" 2>&1) || rc=$?
assert_eq "gate: pending then green exits 0" "0" "$rc"
assert_contains "gate: says it waited" "waiting: Build and test (Windows) is in_progress" "$out"
assert_eq "gate: polled exactly twice" "2" "$(cat "$SEQ_TMP/count")"
rm -rf "$SEQ_TMP"

# no check runs at all and the initial wait exhausted → fail, name the sha.
rc=0
out=$(PATH="$MOCK_BIN:$PATH" CI_COMMIT_SHA=deadbeef GATE_POLL_SECONDS=0 GATE_INITIAL_WAIT_SECONDS=0 \
  "$HELPERS_DIR/github_checks_gate.sh" 2>&1) || rc=$?
assert_eq "gate: no runs exits 1" "1" "$rc"
assert_contains "gate: no-runs message" "ERROR: no GitHub check runs for deadbeef" "$out"

# failure + pending: the gate must fail on the same poll when one check fails,
# regardless of what its siblings are doing. Regression test for verdict aggregation.
SEQ_TMP=$(mktemp -d -t gate_fail_regress.XXXXXX)
cp "$FIXTURES_DIR/github_checks_failure_then_pending.json" "$SEQ_TMP/1.json"
cp "$FIXTURES_DIR/github_checks_success.json" "$SEQ_TMP/2.json"
rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_GITHUB_SEQUENCE_DIR="$SEQ_TMP" MOCK_CURL_COUNTER_FILE="$SEQ_TMP/count" \
  CI_COMMIT_SHA=deadbeef GATE_POLL_SECONDS=0 "$HELPERS_DIR/github_checks_gate.sh" 2>&1) || rc=$?
assert_eq "gate: failure beats pending, exits 1 on first poll" "1" "$rc"
assert_contains "gate: names the failed check on first poll" "ERROR: Build, test and typecheck concluded failure" "$out"
assert_eq "gate: does not poll again after failure" "1" "$(cat "$SEQ_TMP/count")"
rm -rf "$SEQ_TMP"

# A malformed 200 body (rate-limit HTML served as 200) must warn and retry,
# never crash the script with a Python traceback under `set -e`.
rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_GITHUB_BODY_FILE="$FIXTURES_DIR/github_checks_notjson.txt" \
  CI_COMMIT_SHA=deadbeef GATE_POLL_SECONDS=0 GATE_TIMEOUT_SECONDS=0 "$HELPERS_DIR/github_checks_gate.sh" 2>&1) || rc=$?
assert_eq "gate: non-JSON 200 body times out" "1" "$rc"
assert_contains "gate: non-JSON 200 body warns" "warning: GitHub answered 200 with a body that is not JSON" "$out"
assert_not_contains "gate: non-JSON 200 body has no traceback" "Traceback" "$out"

echo
echo "-- verify_release.sh --"

# Pin the mock rsign's own argv contract before trusting it to stand in for
# the real rsign2 binary: a wrong flag order (or any shape other than
# `verify -p PUB -x SIG FILE`, six args) is a usage error (exit 2), not a
# silent pass or a "signature does not verify".
rc=0
out=$("$MOCK_BIN/rsign" verify -x a -p b c 2>&1) || rc=$?
assert_eq "mock rsign: wrong arg order exits 2" "2" "$rc"
assert_contains "mock rsign: usage text" "usage: rsign verify -p PUB -x SIG FILE" "$out"

common=(PATH="$MOCK_BIN:$PATH" CI_COMMIT_TAG=v0.7.0 VERIFY_POLL_SECONDS=0 VERIFY_BLOG_TIMEOUT_SECONDS=0 \
  TAURI_CONF_PATH="$FIXTURES_DIR/updater_tauri.conf.json" MOCK_CURL_MANIFEST_FILE="$FIXTURES_DIR/updater_manifest_good.json")
rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: everything present exits 0" "0" "$rc"
assert_contains "verify: checks 13 artifacts" "ok: 13 artifacts present" "$out"
assert_contains "verify: docker both arches" "ok: docker.io/vsharifov/athenaeum:0.7.0 has amd64 + arm64" "$out"
assert_contains "verify: channel tag matches" "ok: :latest -> sha256:aaaa" "$out"
assert_contains "verify: blog reachable" "ok: blog https://artfrom.space/blog/v070/" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" \
  MOCK_CURL_MISSING_ARTIFACTS="athenaeum-0.7.0-macos-x64.dmg" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: a missing installer exits 1" "1" "$rc"
assert_contains "verify: names the missing URL" "ERROR: HTTP 404 for https://artfrom.space/builds/v0.7.0/macos/athenaeum-0.7.0-macos-x64.dmg" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" \
  MOCK_CURL_ARTIFACT_LENGTH=12 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: a tiny file exits 1" "1" "$rc"
assert_contains "verify: names the size" "ERROR: 12 bytes (< 1048576) at https://artfrom.space/builds/v0.7.0/windows/athenaeum-0.7.0-windows-x64.msi" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_amd64only.json" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: arm64 missing exits 1 by default" "1" "$rc"
assert_contains "verify: arm64 message names the escape hatch" "ERROR: docker.io/vsharifov/athenaeum:0.7.0 has no arm64 image (set RELEASE_ALLOW_AMD64_ONLY=1 to accept)" "$out"

rc=0
out=$(env "${common[@]}" RELEASE_ALLOW_AMD64_ONLY=1 MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_amd64only.json" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: arm64 missing accepted with the variable" "0" "$rc"
assert_contains "verify: still warns" "WARNING: publishing amd64-only" "$out"

rc=0
out=$(env "${common[@]}" VERIFY_DOCKER_ATTEMPTS=1 MOCK_CURL_HUB_HTTP=404 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: docker tag absent exits 1" "1" "$rc"
assert_contains "verify: docker absent message" "ERROR: Docker Hub has no tag 0.7.0" "$out"

# The tag API is eventually consistent right after imagetools create: with
# the default attempt count, a persistent non-200 retries up to the last
# attempt (printing it) before the ERROR fires.
rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_HTTP=404 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: docker tag absent (default attempts) exits 1" "1" "$rc"
assert_contains "verify: docker retry shows the last attempt" "attempt 3/3" "$out"
assert_contains "verify: docker retry still errors after the last attempt" "ERROR: Docker Hub has no tag 0.7.0" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_notjson.txt" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: non-JSON docker body exits 1" "1" "$rc"
assert_contains "verify: non-JSON docker message" "ERROR: Docker Hub answered 200 for tag 0.7.0 with a body that is not JSON" "$out"
assert_not_contains "verify: no traceback on non-JSON" "Traceback" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_CURL_BLOG_HTTP=404 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: blog 404 past the budget exits 1" "1" "$rc"
assert_contains "verify: blog message" "ERROR: blog https://artfrom.space/blog/v070/ still HTTP 404 after" "$out"

rc=0
out=$(env "${common[@]}" SKIP_DOCKER_CHECK=1 SKIP_BLOG_CHECK=1 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: skips honoured" "0" "$rc"
assert_contains "verify: says what it skipped" "skipped: docker check (SKIP_DOCKER_CHECK=1)" "$out"

assert_contains "verify: updater manifest ok" "ok: updater manifest v0.7.0 — 5 platforms, every URL present, every signature verified" "$out"

rc=0
out=$(env "${common[@]}" SKIP_DOCKER_CHECK=1 SKIP_BLOG_CHECK=1 SKIP_UPDATER_CHECK=1 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: updater skip honoured exits 0" "0" "$rc"
assert_contains "verify: updater check can be skipped" "skipped: updater manifest check (SKIP_UPDATER_CHECK=1)" "$out"

rc=0
UM_BAD=$(mktemp -t um_bad.XXXXXX)
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); d["version"]="0.6.9"; json.dump(d,open(sys.argv[2],"w"))' "$FIXTURES_DIR/updater_manifest_good.json" "$UM_BAD"
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_CURL_MANIFEST_FILE="$UM_BAD" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: wrong manifest version exits 1" "1" "$rc"
assert_contains "verify: names the version mismatch" "ERROR: updater manifest version is 0.6.9, tag v0.7.0 expects 0.7.0" "$out"

rc=0
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); del d["platforms"]["linux-x86_64"]; json.dump(d,open(sys.argv[2],"w"))' "$FIXTURES_DIR/updater_manifest_good.json" "$UM_BAD"
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_CURL_MANIFEST_FILE="$UM_BAD" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: missing platform key exits 1" "1" "$rc"
assert_contains "verify: names the missing key" "ERROR: updater manifest has no entry for linux-x86_64" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_RSIGN_FAIL_FILES="athenaeum-0.7.0-linux-x64.AppImage" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: a bad signature exits 1" "1" "$rc"
assert_contains "verify: names the file whose signature failed" "ERROR: updater signature does not verify for https://artfrom.space/builds/v0.7.0/linux/athenaeum-0.7.0-linux-x64.AppImage" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_CURL_MANIFEST_HTTP=404 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: no manifest exits 1" "1" "$rc"
assert_contains "verify: names the manifest URL" "ERROR: HTTP 404 for https://artfrom.space/updates/v0.7.0.json" "$out"

rc=0
python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); d["platforms"]["linux-x86_64"]["signature"]="not*base64!"; json.dump(d,open(sys.argv[2],"w"))' "$FIXTURES_DIR/updater_manifest_good.json" "$UM_BAD"
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_CURL_MANIFEST_FILE="$UM_BAD" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: invalid base64 signature exits 1" "1" "$rc"
assert_contains "verify: names the base64 failure" "ERROR: updater signature for https://artfrom.space/builds/v0.7.0/linux/athenaeum-0.7.0-linux-x64.AppImage is not valid base64" "$out"

rm -f "$UM_BAD"

echo
echo "-- create_gitlab_release.sh --"

REL_TMP=$(mktemp -t release_body.XXXXXX)
rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_CAPTURE_RELEASE_BODY="$REL_TMP" CI_COMMIT_TAG=v0.7.0 CI_API_V4_URL=http://gitlab.local/api/v4 CI_PROJECT_ID=1 CI_JOB_TOKEN=t \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/create_gitlab_release.sh" 2>&1) || rc=$?
assert_eq "release: exits 0" "0" "$rc"
payload=$(cat "$REL_TMP")
assert_contains "release: tag" '"tag_name": "v0.7.0"' "$payload"
assert_contains "release: name" '"name": "Athenaeum v0.7.0"' "$payload"
assert_contains "release: description carries the notes" "## Bug Fixes" "$payload"
assert_contains "release: msi link" 'https://artfrom.space/builds/v0.7.0/windows/athenaeum-0.7.0-windows-x64.msi' "$payload"
assert_contains "release: perseus arm64 deb link" 'https://artfrom.space/builds/v0.7.0/perseus/perseus-0.7.0-linux-arm64.deb' "$payload"
assert_eq "release: 13 asset links" "13" "$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["assets"]["links"]))' "$REL_TMP")"
rm -f "$REL_TMP"

rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_RELEASE_HTTP=409 CI_COMMIT_TAG=v0.7.0 CI_API_V4_URL=http://gitlab.local/api/v4 CI_PROJECT_ID=1 CI_JOB_TOKEN=t \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/create_gitlab_release.sh" 2>&1) || rc=$?
assert_eq "release: 409 (already exists) exits 0" "0" "$rc"
assert_contains "release: 409 warns kept as is" "warning: GitLab Release v0.7.0 already exists — kept as is" "$out"

rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_RELEASE_HTTP=500 CI_COMMIT_TAG=v0.7.0 CI_API_V4_URL=http://gitlab.local/api/v4 CI_PROJECT_ID=1 CI_JOB_TOKEN=t \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/create_gitlab_release.sh" 2>&1) || rc=$?
assert_eq "release: 500 API error exits 1" "1" "$rc"
assert_contains "release: 500 API error message" "ERROR: GitLab release API answered HTTP 500" "$out"

echo
echo "-- .gitlab-ci.yml shape --"

# Class protection for the build:windows bug found in review: an unquoted
# PowerShell script line containing ": " parses as a YAML mapping, not a
# string, and GitLab rejects the whole config. Load every job (top-level
# keys, including hidden `.`-prefixed templates; a non-dict top-level value
# like `stages` is skipped) and assert every `script`/`before_script`/
# `after_script` entry is a str, or a nested list of str — never a dict.
ci_shape_out=$(python3 - "$REPO_DIR/.gitlab-ci.yml" <<'PYEOF'
import sys
import yaml

with open(sys.argv[1]) as f:
    doc = yaml.safe_load(f)

def entry_ok(e):
    if isinstance(e, str):
        return True
    if isinstance(e, list):
        return all(isinstance(x, str) for x in e)
    return False

errors = []
for job_name, job in doc.items():
    if not isinstance(job, dict):
        continue
    for key in ("script", "before_script", "after_script"):
        if key not in job:
            continue
        entries = job[key]
        if not isinstance(entries, list):
            errors.append("%s.%s: not a list (%s)" % (job_name, key, type(entries).__name__))
            continue
        for i, e in enumerate(entries):
            if not entry_ok(e):
                errors.append("%s.%s[%d]: %s" % (job_name, key, i, type(e).__name__))

if errors:
    for e in errors:
        print(e)
else:
    print("ok")
PYEOF
)
assert_eq "ci yaml: every script entry is a string" "ok" "$ci_shape_out"

echo
echo "-- gen_updater_manifest.sh --"

UM_STAGE=$(mktemp -d -t updater_stage.XXXXXX)
mkdir -p "$UM_STAGE/macos" "$UM_STAGE/windows" "$UM_STAGE/linux"
printf 'SIGMACARM\n' > "$UM_STAGE/macos/athenaeum-0.7.0-macos-arm64.app.tar.gz.sig"
printf 'SIGMACX64'   > "$UM_STAGE/macos/athenaeum-0.7.0-macos-x64.app.tar.gz.sig"
printf 'SIGNSIS'     > "$UM_STAGE/windows/athenaeum-0.7.0-windows-x64-setup.exe.sig"
printf 'SIGMSI'      > "$UM_STAGE/windows/athenaeum-0.7.0-windows-x64.msi.sig"
printf 'SIGAPPIMG'   > "$UM_STAGE/linux/athenaeum-0.7.0-linux-x64.AppImage.sig"
UM_OUT=$(mktemp -t updater_manifest.XXXXXX)
rc=0
CI_COMMIT_TAG=v0.7.0 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" PUB_DATE=2026-10-01T12:00:00Z \
  "$HELPERS_DIR/gen_updater_manifest.sh" > "$UM_OUT" 2>/dev/null || rc=$?
assert_eq "manifest: exits 0" "0" "$rc"
um_field() { python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$UM_OUT" "$1"; }
assert_eq "manifest: version without v" "0.7.0" "$(um_field 'd["version"]')"
assert_eq "manifest: pub_date passed through" "2026-10-01T12:00:00Z" "$(um_field 'd["pub_date"]')"
assert_eq "manifest: 5 platforms" "5" "$(um_field 'len(d["platforms"])')"
assert_eq "manifest: arm64 url" "https://artfrom.space/builds/v0.7.0/macos/athenaeum-0.7.0-macos-arm64.app.tar.gz" "$(um_field 'd["platforms"]["darwin-aarch64"]["url"]')"
assert_eq "manifest: arm64 signature trimmed" "SIGMACARM" "$(um_field 'd["platforms"]["darwin-aarch64"]["signature"]')"
assert_eq "manifest: nsis key" "https://artfrom.space/builds/v0.7.0/windows/athenaeum-0.7.0-windows-x64-setup.exe" "$(um_field 'd["platforms"]["windows-x86_64-nsis"]["url"]')"
assert_eq "manifest: msi key" "SIGMSI" "$(um_field 'd["platforms"]["windows-x86_64-msi"]["signature"]')"
assert_eq "manifest: no plain windows key" "False" "$(um_field '"windows-x86_64" in d["platforms"]')"
assert_eq "manifest: notes are the whole file" "same" "$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print("same" if d["notes"]==open(sys.argv[2],encoding="utf-8").read() else "differs")' "$UM_OUT" "$FIXTURES_DIR/release_notes_sample.md")"
assert_contains "manifest: notes keep a quoted tagline" 'a \"quoted\" tagline' "$(cat "$UM_OUT")"

# A tag literally named "v" makes VERSION empty. updater_artifacts() aborts on
# that (via its own `${VERSION:?}` guard), but a `while read < <(...)` process
# substitution never surfaces a producer's exit status — without the
# up-front inventory count, the script would have silently emitted a valid
# manifest with "platforms": {} at exit 0. Run against the still-fully-valid
# stage so the failure can only be attributed to the empty inventory itself.
rc=0
out=$(CI_COMMIT_TAG=v STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: tag literally 'v' (empty VERSION) exits 1" "1" "$rc"
assert_contains "manifest: names the empty inventory" "ERROR: updater inventory is empty" "$out"

# strptime() alone accepts non-zero-padded components ("2026-10-1" parses
# fine) — the length check catches what the format string doesn't. Still
# against the fully-valid stage so this is the ONLY thing that can fail.
rc=0
out=$(CI_COMMIT_TAG=v0.7.0 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" PUB_DATE="2026-10-1T12:00:00Z" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: a non-zero-padded PUB_DATE exits 1" "1" "$rc"

rc=0
out=$(CI_COMMIT_TAG=v0.7.0-beta.2 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: beta tag exits 1 when its files are missing (stage holds 0.7.0)" "1" "$rc"
assert_contains "manifest: names the missing signature" "ERROR: missing signature $UM_STAGE/macos/athenaeum-0.7.0-beta.2-macos-arm64.app.tar.gz.sig" "$out"

rm -f "$UM_STAGE/linux/athenaeum-0.7.0-linux-x64.AppImage.sig"
rc=0
out=$(CI_COMMIT_TAG=v0.7.0 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: one missing sig exits 1" "1" "$rc"
assert_contains "manifest: names it" "athenaeum-0.7.0-linux-x64.AppImage.sig" "$out"
: > "$UM_STAGE/linux/athenaeum-0.7.0-linux-x64.AppImage.sig"
rc=0
out=$(CI_COMMIT_TAG=v0.7.0 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: an EMPTY sig exits 1" "1" "$rc"

rc=0
out=$(CI_COMMIT_TAG=v0.7.0 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" PUB_DATE="yesterday" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: a non-RFC3339 PUB_DATE exits 1" "1" "$rc"

# A whitespace-only .sig is non-empty at the filesystem level (bash's `-s`
# check passes it through), so this exercises the PYTHON-side guard
# (`signature = s.read().strip()`), distinct from the bash-side missing/empty
# check above. Last mutation of $UM_STAGE before cleanup — nothing after
# this needs a valid linux signature.
printf '\n' > "$UM_STAGE/linux/athenaeum-0.7.0-linux-x64.AppImage.sig"
rc=0
out=$(CI_COMMIT_TAG=v0.7.0 STAGE_DIR="$UM_STAGE" NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/gen_updater_manifest.sh" 2>&1) || rc=$?
assert_eq "manifest: a whitespace-only sig exits 1" "1" "$rc"
assert_contains "manifest: names the whitespace-only sig's path" "$UM_STAGE/linux/athenaeum-0.7.0-linux-x64.AppImage.sig" "$out"

rm -rf "$UM_STAGE" "$UM_OUT"

echo
echo "Passed: $PASS  Failed: $FAIL"
[ "$FAIL" -eq 0 ]

#!/usr/bin/env bash
# Bash test runner for CI notification helper scripts.
# Usage: .gitlab/ci/scripts/test/run_tests.sh
# Exits 0 if all assertions pass, non-zero if any failed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HELPERS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
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
echo "Passed: $PASS  Failed: $FAIL"
[ "$FAIL" -eq 0 ]

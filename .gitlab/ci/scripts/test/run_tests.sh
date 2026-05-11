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
short_len=${#out}
if [ "$short_len" -lt 4096 ]; then
  echo "  ok: short output fits in 4096 chars (actual: $short_len)"
  PASS=$((PASS + 1))
else
  echo "  FAIL: short output exceeded 4096 chars (actual: $short_len)"
  FAIL=$((FAIL + 1))
fi

# Long input is truncated and gets the tail.
out=$(RELEASE_URL="https://example.com/release" "$HELPERS_DIR/truncate_release_notes.sh" < "$FIXTURES_DIR/long.md")
assert_contains "long input gets truncation tail" "Full notes: https://example.com/release" "$out"
long_len=${#out}
if [ "$long_len" -le 4096 ]; then
  echo "  ok: long output fits in 4096 chars (actual: $long_len)"
  PASS=$((PASS + 1))
else
  echo "  FAIL: long output exceeded 4096 chars (actual: $long_len)"
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
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_discord.sh" 2>&1
)
assert_contains "skip when webhook unset" "Skipping Discord" "$out"

# Dry-run for stable tag prints a JSON payload with the expected shape.
out=$(
  DRY_RUN=1 \
  DISCORD_WEBHOOK_URL="https://discord.com/api/webhooks/dummy" \
  CI_COMMIT_TAG="v0.2.0" \
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_discord.sh"
)
assert_contains "dry-run prints title" "Athenaeum v0.2.0 released" "$out"
assert_contains "dry-run prints stable color" "3066993" "$out"
assert_contains "dry-run prints release URL" "https://gitlab.com/eg013ra1n/athenaeum/-/releases/v0.2.0" "$out"
assert_contains "dry-run includes download field" "artfrom.space/releases/download" "$out"
assert_not_contains "stable title omits beta marker" "(beta)" "$out"

# Beta tag uses the beta colour and labels the title.
out=$(
  DRY_RUN=1 \
  DISCORD_WEBHOOK_URL="https://discord.com/api/webhooks/dummy" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
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
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
  RELEASE_NOTES_PATH="/nonexistent/RELEASE_NOTES.md" \
  "$HELPERS_DIR/notify_discord.sh"
)
assert_contains "fallback body when notes missing" "is out" "$out"

echo
echo "Passed: $PASS  Failed: $FAIL"
[ "$FAIL" -eq 0 ]

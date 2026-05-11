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
  if printf '%s' "$haystack" | grep -qF "$needle"; then
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
  if printf '%s' "$haystack" | grep -qF "$needle"; then
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
echo "Passed: $PASS  Failed: $FAIL"
[ "$FAIL" -eq 0 ]

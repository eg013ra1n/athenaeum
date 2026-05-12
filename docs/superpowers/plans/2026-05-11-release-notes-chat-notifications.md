# Release-Notes Chat Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two GitLab CI jobs (`notify:discord`, `notify:telegram`) that post `RELEASE_NOTES.md` to a Discord channel and a Telegram chat after a `v*` tag's GitLab Release is created.

**Architecture:** Two siblings under `needs: [release]` in the existing `release` stage. Each job is a thin shell wrapper that calls a small helper script in `.gitlab/ci/scripts/`. The non-trivial logic — truncation, markdown→HTML conversion, payload assembly, soft-fail posting — lives in those scripts so it can be unit-tested locally with bash.

**Tech Stack:** GitLab CI YAML, POSIX `sh` + `sed` + `python3` (for JSON escaping; already required by the existing `release` job), `curl` for HTTP, Discord webhooks, Telegram Bot API.

**Spec:** `docs/superpowers/specs/2026-05-11-release-notes-chat-notifications-design.md`

**Files created:**

- `.gitlab/ci/scripts/truncate_release_notes.sh` — stdin → ≤ 3900 chars + tail line if truncated
- `.gitlab/ci/scripts/md_to_telegram_html.sh` — stdin → Telegram-safe HTML on stdout
- `.gitlab/ci/scripts/notify_discord.sh` — assembles JSON, POSTs to webhook, soft-fails
- `.gitlab/ci/scripts/notify_telegram.sh` — assembles JSON, POSTs to Bot API, soft-fails
- `.gitlab/ci/scripts/test/fixtures/short.md` — 200-char RELEASE_NOTES sample (no truncation expected)
- `.gitlab/ci/scripts/test/fixtures/long.md` — 5000-char RELEASE_NOTES sample (truncation expected)
- `.gitlab/ci/scripts/test/fixtures/markdown_features.md` — bold, italic, code, links, headings, list — covers every conversion rule
- `.gitlab/ci/scripts/test/run_tests.sh` — single-entry test runner that calls each test fixture and asserts on output

**Files modified:**

- `.gitlab-ci.yml` — add the two jobs at the bottom of the file

---

### Task 1: Test infrastructure & fixtures

This task lays the groundwork. No production code yet — just fixtures and an empty test runner that can be expanded in later tasks.

**Files:**

- Create: `.gitlab/ci/scripts/test/fixtures/short.md`
- Create: `.gitlab/ci/scripts/test/fixtures/long.md`
- Create: `.gitlab/ci/scripts/test/fixtures/markdown_features.md`
- Create: `.gitlab/ci/scripts/test/run_tests.sh`

- [ ] **Step 1: Create the fixture directories**

```bash
mkdir -p .gitlab/ci/scripts/test/fixtures
```

- [ ] **Step 2: Write the short fixture** (`.gitlab/ci/scripts/test/fixtures/short.md`)

```markdown
## What's New

- One **small** thing.
- Another small thing with `inline code`.
```

- [ ] **Step 3: Write the long fixture** (`.gitlab/ci/scripts/test/fixtures/long.md`)

```bash
# Generate it deterministically — 250 lines × ~25 chars each ≈ 5500 chars
python3 -c '
import sys
print("## What’s New")
print()
for i in range(250):
    print(f"- Item {i:03d} with **bold** and `code`.")
' > .gitlab/ci/scripts/test/fixtures/long.md
wc -c .gitlab/ci/scripts/test/fixtures/long.md
```

Expected: byte count ≥ 4500.

- [ ] **Step 4: Write the markdown-features fixture** (`.gitlab/ci/scripts/test/fixtures/markdown_features.md`)

```markdown
# Heading One

## Heading Two

### Heading Three

A paragraph with **bold** and *italic* and `inline code` and a [link](https://example.com).

Special chars to escape: 5 < 10 & 10 > 5.

- list item one
- list item two
* list item three
```

- [ ] **Step 5: Write the test runner skeleton** (`.gitlab/ci/scripts/test/run_tests.sh`)

```sh
#!/usr/bin/env bash
# Bash test runner for CI notification helper scripts.
# Usage: .gitlab/ci/scripts/test/run_tests.sh
# Exits 0 on success, 1 on first failure.
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
    FAIL=$((FAIL + 1))
  else
    echo "  ok: $label"
    PASS=$((PASS + 1))
  fi
}

echo "== CI notification helper tests =="
echo "(no tests registered yet)"

echo
echo "Passed: $PASS  Failed: $FAIL"
[ "$FAIL" -eq 0 ]
```

- [ ] **Step 6: Make scripts executable and run the empty runner**

```bash
chmod +x .gitlab/ci/scripts/test/run_tests.sh
.gitlab/ci/scripts/test/run_tests.sh
```

Expected: exits 0, prints `Passed: 0  Failed: 0`.

- [ ] **Step 7: Commit**

```bash
git add .gitlab/ci/scripts/test/
git commit -m "ci: scaffold test runner and fixtures for chat notifications"
```

---

### Task 2: Truncation script (TDD)

Trims input to ≤ 3900 bytes and appends a tail line pointing at the GitLab Release. Both Discord and Telegram use this; both have a 4096-char message cap.

**Files:**

- Create: `.gitlab/ci/scripts/truncate_release_notes.sh`
- Modify: `.gitlab/ci/scripts/test/run_tests.sh` (register tests)

- [ ] **Step 1: Write the failing tests** (append to `.gitlab/ci/scripts/test/run_tests.sh` BEFORE the trailing summary block)

Replace the placeholder section:

```sh
echo "== CI notification helper tests =="
echo "(no tests registered yet)"
```

with:

```sh
echo "== CI notification helper tests =="

echo
echo "-- truncate_release_notes.sh --"

# Short input passes through unchanged (no tail).
out=$(RELEASE_URL="https://example.com/release" "$HELPERS_DIR/truncate_release_notes.sh" < "$FIXTURES_DIR/short.md")
assert_contains "short input keeps body" "small" "$out"
assert_not_contains "short input has no truncation tail" "Full notes" "$out"
short_len=${#out}
[ "$short_len" -lt 4096 ] || { echo "  FAIL: short output exceeded 4096 chars"; FAIL=$((FAIL + 1)); }

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
```

- [ ] **Step 2: Run tests, verify they fail**

```bash
.gitlab/ci/scripts/test/run_tests.sh || true
```

Expected: error from missing `truncate_release_notes.sh`, FAIL count > 0.

- [ ] **Step 3: Write the script** (`.gitlab/ci/scripts/truncate_release_notes.sh`)

```sh
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
```

- [ ] **Step 4: Make executable and run tests**

```bash
chmod +x .gitlab/ci/scripts/truncate_release_notes.sh
.gitlab/ci/scripts/test/run_tests.sh
```

Expected: all three truncation assertions pass.

- [ ] **Step 5: Commit**

```bash
git add .gitlab/ci/scripts/truncate_release_notes.sh .gitlab/ci/scripts/test/run_tests.sh
git commit -m "ci: add truncate_release_notes.sh helper with bash tests"
```

---

### Task 3: Markdown → Telegram HTML script (TDD)

Telegram's HTML parse mode supports a small fixed tag set. We only convert what RELEASE_NOTES.md actually uses: bold, italic, inline code, links, headings (downgraded to bold), and the three escapes for `<`, `>`, `&`. List bullets pass through as plain text.

**Files:**

- Create: `.gitlab/ci/scripts/md_to_telegram_html.sh`
- Modify: `.gitlab/ci/scripts/test/run_tests.sh`

- [ ] **Step 1: Write the failing tests** (append to `.gitlab/ci/scripts/test/run_tests.sh` BEFORE the summary block)

```sh
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
```

- [ ] **Step 2: Run tests, verify they fail**

```bash
.gitlab/ci/scripts/test/run_tests.sh || true
```

Expected: failures for the new `md_to_telegram_html.sh` block.

- [ ] **Step 3: Write the script** (`.gitlab/ci/scripts/md_to_telegram_html.sh`)

```sh
#!/usr/bin/env bash
# Convert a small subset of CommonMark markdown on stdin to the HTML subset
# Telegram's parse_mode=HTML accepts. Output on stdout.
#
# Conversions, applied in order:
#   1. HTML-escape & < > in the source (so user text can never inject tags).
#   2. Strip "### ", "## ", "# " heading prefixes and wrap the heading text in <b>.
#   3. **bold** -> <b>bold</b>
#   4. *italic* -> <i>italic</i>      (single-asterisk only; greedy on shortest match)
#   5. `code`  -> <code>code</code>
#   6. [text](url) -> <a href="url">text</a>
# List markers ("- " and "* " at line start) are preserved as plain text;
# Telegram doesn't render bullets.
set -euo pipefail

# All conversions chained through one sed -E invocation. Order matters:
# - HTML-escape FIRST so tags we generate later aren't escaped.
# - Headings BEFORE bold so a "# **foo**" becomes "<b>**foo**</b>" then "<b><b>foo</b></b>".
#   That nested-bold case shouldn't happen in our notes, but ordering is documented.
sed -E \
  -e 's/&/\&amp;/g' \
  -e 's/</\&lt;/g' \
  -e 's/>/\&gt;/g' \
  -e 's/^### (.*)$/<b>\1<\/b>/' \
  -e 's/^## (.*)$/<b>\1<\/b>/' \
  -e 's/^# (.*)$/<b>\1<\/b>/' \
  -e 's/\*\*([^*]+)\*\*/<b>\1<\/b>/g' \
  -e 's/(^|[^*])\*([^*]+)\*([^*]|$)/\1<i>\2<\/i>\3/g' \
  -e 's/`([^`]+)`/<code>\1<\/code>/g' \
  -e 's/\[([^]]+)\]\(([^)]+)\)/<a href="\2">\1<\/a>/g'
```

- [ ] **Step 4: Make executable and run tests**

```bash
chmod +x .gitlab/ci/scripts/md_to_telegram_html.sh
.gitlab/ci/scripts/test/run_tests.sh
```

Expected: all 13 markdown-conversion assertions pass.

- [ ] **Step 5: Manual smoke test against the real RELEASE_NOTES.md**

```bash
.gitlab/ci/scripts/md_to_telegram_html.sh < RELEASE_NOTES.md | head -20
```

Expected: the first lines of the real notes appear with `<b>`, `<code>`, `<a href>` tags inserted; no raw `**` or unescaped `<`/`>` visible.

- [ ] **Step 6: Commit**

```bash
git add .gitlab/ci/scripts/md_to_telegram_html.sh .gitlab/ci/scripts/test/run_tests.sh
git commit -m "ci: add md_to_telegram_html.sh helper with bash tests"
```

---

### Task 4: Discord notification script (TDD with dry-run)

Builds the embed payload and posts to `$DISCORD_WEBHOOK_URL`. Honours `DRY_RUN=1` (prints payload to stdout, no curl), skips cleanly if `$DISCORD_WEBHOOK_URL` is empty, soft-fails on transport errors.

**Files:**

- Create: `.gitlab/ci/scripts/notify_discord.sh`
- Modify: `.gitlab/ci/scripts/test/run_tests.sh`

- [ ] **Step 1: Write the failing tests** (append to `.gitlab/ci/scripts/test/run_tests.sh` BEFORE the summary block)

```sh
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
```

- [ ] **Step 2: Run tests, verify they fail**

```bash
.gitlab/ci/scripts/test/run_tests.sh || true
```

Expected: `notify_discord.sh: command not found` or similar; FAIL count > 0.

- [ ] **Step 3: Write the script** (`.gitlab/ci/scripts/notify_discord.sh`)

```sh
#!/usr/bin/env bash
# Post the contents of RELEASE_NOTES.md to a Discord channel via webhook.
# Required env: DISCORD_WEBHOOK_URL, CI_COMMIT_TAG, CI_PROJECT_URL.
# Optional env: RELEASE_NOTES_PATH (default: RELEASE_NOTES.md), DRY_RUN=1.
#
# Behaviour:
#   - DISCORD_WEBHOOK_URL empty -> log + exit 0 (so the first pipeline after
#     merging the change doesn't go red while secrets are being added).
#   - Transport / HTTP error from Discord -> log warning + exit 0 (the build
#     is already shipped; chat outages don't fail the pipeline).
#   - DRY_RUN=1 -> print the JSON payload to stdout instead of POSTing.
set -euo pipefail

if [ -z "${DISCORD_WEBHOOK_URL:-}" ]; then
  echo "Skipping Discord notification — DISCORD_WEBHOOK_URL not set in CI/CD variables."
  exit 0
fi

: "${CI_COMMIT_TAG:?CI_COMMIT_TAG must be set}"
: "${CI_PROJECT_URL:?CI_PROJECT_URL must be set}"

NOTES_PATH="${RELEASE_NOTES_PATH:-RELEASE_NOTES.md}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
RELEASE_URL="${CI_PROJECT_URL}/-/releases/${CI_COMMIT_TAG}"

# Title + colour vary by stable vs beta. The same beta-detection trick is
# already used by deploy and publish_version in .gitlab-ci.yml.
if echo "$CI_COMMIT_TAG" | grep -q '\-beta'; then
  TITLE="Athenaeum ${CI_COMMIT_TAG} (beta) released"
  COLOR=15976499  # #f39c12 amber
else
  TITLE="Athenaeum ${CI_COMMIT_TAG} released"
  COLOR=3066993   # #2ecc71 green
fi

# Body source: the file, truncated. Falls back to a one-liner if missing.
if [ -s "$NOTES_PATH" ]; then
  BODY=$(RELEASE_URL="$RELEASE_URL" "$SCRIPT_DIR/truncate_release_notes.sh" < "$NOTES_PATH")
else
  BODY="Athenaeum ${CI_COMMIT_TAG} is out — see the release page for details."
fi

# Use python3 for JSON escaping — already a dep of the existing release job.
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
)

if [ "${DRY_RUN:-}" = "1" ]; then
  echo "$PAYLOAD"
  exit 0
fi

# Post. Soft-fail: log the curl exit + any response body, but exit 0 either way.
http_code=$(
  curl --silent --show-error --max-time 30 \
    --output /tmp/discord_response.txt \
    --write-out '%{http_code}' \
    --request POST \
    --header 'Content-Type: application/json' \
    --data "$PAYLOAD" \
    "$DISCORD_WEBHOOK_URL" \
    || echo "000"
)

if [ "$http_code" -ge 200 ] && [ "$http_code" -lt 300 ]; then
  echo "Discord notification posted (HTTP $http_code)."
else
  echo "WARNING: Discord notification failed (HTTP $http_code). Pipeline continues."
  echo "Response body:"
  cat /tmp/discord_response.txt 2>/dev/null || true
fi
exit 0
```

- [ ] **Step 4: Make executable and run tests**

```bash
chmod +x .gitlab/ci/scripts/notify_discord.sh
.gitlab/ci/scripts/test/run_tests.sh
```

Expected: all 8 Discord assertions pass.

- [ ] **Step 5: Commit**

```bash
git add .gitlab/ci/scripts/notify_discord.sh .gitlab/ci/scripts/test/run_tests.sh
git commit -m "ci: add notify_discord.sh helper with dry-run tests"
```

---

### Task 5: Telegram notification script (TDD with dry-run)

Same shape as Task 4. Wraps the title in `<b>` (HTML mode), pipes the body through `md_to_telegram_html.sh` before truncation, posts to the Bot API.

**Files:**

- Create: `.gitlab/ci/scripts/notify_telegram.sh`
- Modify: `.gitlab/ci/scripts/test/run_tests.sh`

- [ ] **Step 1: Write the failing tests** (append to `.gitlab/ci/scripts/test/run_tests.sh` BEFORE the summary block)

```sh
echo
echo "-- notify_telegram.sh --"

# Skip cleanly when TELEGRAM_BOT_TOKEN is empty (chat id alone isn't enough).
out=$(
  TELEGRAM_BOT_TOKEN="" \
  TELEGRAM_CHAT_ID="123" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_telegram.sh" 2>&1
)
assert_contains "skip when bot token unset" "Skipping Telegram" "$out"

# Skip cleanly when TELEGRAM_CHAT_ID is empty.
out=$(
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
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
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_telegram.sh"
)
assert_contains "dry-run prints chat id" "@athenaeum_releases" "$out"
assert_contains "dry-run uses HTML parse mode" "HTML" "$out"
assert_contains "dry-run wraps title in bold" "<b>Athenaeum v0.2.0 released</b>" "$out"
assert_contains "dry-run converts inline code in body" "<code>inline code</code>" "$out"
assert_contains "dry-run includes release URL" "https://gitlab.com/eg013ra1n/athenaeum/-/releases/v0.2.0" "$out"
assert_not_contains "stable title omits beta marker" "(beta)" "$out"

# Beta tag.
out=$(
  DRY_RUN=1 \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/short.md" \
  "$HELPERS_DIR/notify_telegram.sh"
)
assert_contains "beta title labels beta" "<b>Athenaeum v0.2.0-beta.10 (beta) released</b>" "$out"

# Long input gets truncated AND HTML-converted (truncation runs after conversion).
out=$(
  DRY_RUN=1 \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/long.md" \
  "$HELPERS_DIR/notify_telegram.sh"
)
assert_contains "long telegram body has truncation tail" "Full notes: https://gitlab.com/eg013ra1n/athenaeum/-/releases/v0.2.0-beta.10" "$out"

# Missing RELEASE_NOTES.md falls back to a one-liner (does not fail).
out=$(
  DRY_RUN=1 \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0" \
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
  RELEASE_NOTES_PATH="/nonexistent/RELEASE_NOTES.md" \
  "$HELPERS_DIR/notify_telegram.sh"
)
assert_contains "fallback body when notes missing" "is out" "$out"
```

- [ ] **Step 2: Run tests, verify they fail**

```bash
.gitlab/ci/scripts/test/run_tests.sh || true
```

Expected: `notify_telegram.sh: command not found`; FAIL count > 0.

- [ ] **Step 3: Write the script** (`.gitlab/ci/scripts/notify_telegram.sh`)

```sh
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

if echo "$CI_COMMIT_TAG" | grep -q '\-beta'; then
  TITLE="Athenaeum ${CI_COMMIT_TAG} (beta) released"
else
  TITLE="Athenaeum ${CI_COMMIT_TAG} released"
fi

# Pipeline: raw markdown -> Telegram HTML -> truncate to <= 3900.
# Order matters: convert first, THEN truncate, so we never split a tag in half.
if [ -s "$NOTES_PATH" ]; then
  BODY=$(
    "$SCRIPT_DIR/md_to_telegram_html.sh" < "$NOTES_PATH" \
      | RELEASE_URL="$RELEASE_URL" "$SCRIPT_DIR/truncate_release_notes.sh"
  )
else
  BODY="Athenaeum ${CI_COMMIT_TAG} is out — see the release page for details."
fi

# Title is bold; body is HTML-formatted; trailing line links to the release page.
TEXT=$(printf '<b>%s</b>\n\n%s\n\nDownload: https://artfrom.space/releases/download/' "$TITLE" "$BODY")

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
)

if [ "${DRY_RUN:-}" = "1" ]; then
  echo "$PAYLOAD"
  exit 0
fi

URL="https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/sendMessage"
http_code=$(
  curl --silent --show-error --max-time 30 \
    --output /tmp/telegram_response.txt \
    --write-out '%{http_code}' \
    --request POST \
    --header 'Content-Type: application/json' \
    --data "$PAYLOAD" \
    "$URL" \
    || echo "000"
)

if [ "$http_code" -ge 200 ] && [ "$http_code" -lt 300 ]; then
  echo "Telegram notification posted (HTTP $http_code)."
else
  echo "WARNING: Telegram notification failed (HTTP $http_code). Pipeline continues."
  echo "Response body:"
  cat /tmp/telegram_response.txt 2>/dev/null || true
fi
exit 0
```

- [ ] **Step 4: Make executable and run tests**

```bash
chmod +x .gitlab/ci/scripts/notify_telegram.sh
.gitlab/ci/scripts/test/run_tests.sh
```

Expected: all Telegram assertions pass; final `Passed: <N>  Failed: 0`.

- [ ] **Step 5: Commit**

```bash
git add .gitlab/ci/scripts/notify_telegram.sh .gitlab/ci/scripts/test/run_tests.sh
git commit -m "ci: add notify_telegram.sh helper with dry-run tests"
```

---

### Task 6: Wire jobs into `.gitlab-ci.yml`

The two jobs are siblings under `needs: [release]` in the existing `release` stage. Each is a thin wrapper around its helper script — no inline shell logic that isn't covered by tests.

**Files:**

- Modify: `.gitlab-ci.yml` (append at end, after `publish_version`)

- [ ] **Step 1: Append the two jobs**

Add this block at the very end of `.gitlab-ci.yml` (after the `publish_version` job):

```yaml
# --- Chat Notifications (only on tags) ---
# After the GitLab Release exists, post the contents of RELEASE_NOTES.md to
# Discord and Telegram. Both are siblings under `needs: [release]` so a failure
# in one platform does not prevent the other from posting.
#
# Required CI/CD variables (Settings -> CI/CD -> Variables; Masked, Protected):
#   - DISCORD_WEBHOOK_URL  (full webhook URL)
#   - TELEGRAM_BOT_TOKEN   (BotFather token, e.g. 1234567:ABC-DEF...)
#   - TELEGRAM_CHAT_ID     (numeric chat id, or @channelusername)
# Each job skips cleanly with a log message if its required variables are unset,
# so the first pipeline after merging this change does not go red while you
# add the secrets.
#
# Spec: docs/superpowers/specs/2026-05-11-release-notes-chat-notifications-design.md

notify:discord:
  stage: release
  tags:
    - linux
  only:
    - tags
  needs:
    - job: release
      artifacts: false
  script:
    - .gitlab/ci/scripts/notify_discord.sh

notify:telegram:
  stage: release
  tags:
    - linux
  only:
    - tags
  needs:
    - job: release
      artifacts: false
  script:
    - .gitlab/ci/scripts/notify_telegram.sh
```

- [ ] **Step 2: Validate YAML syntax**

```bash
yamllint -d "{extends: relaxed, rules: {line-length: disable}}" .gitlab-ci.yml
```

Expected: no errors. Warnings about indentation style are OK if they match the rest of the file.

- [ ] **Step 3: Validate the file parses as a GitLab CI config (structural check)**

```bash
python3 -c '
import yaml, sys
doc = yaml.safe_load(open(".gitlab-ci.yml"))
assert "notify:discord" in doc, "notify:discord job missing"
assert "notify:telegram" in doc, "notify:telegram job missing"
for name in ("notify:discord", "notify:telegram"):
    job = doc[name]
    assert job["stage"] == "release", f"{name}: wrong stage"
    assert job["only"] == ["tags"], f"{name}: should be tags-only"
    assert any(n.get("job") == "release" for n in job["needs"]), f"{name}: should depend on release job"
print("OK: both notify jobs are wired correctly")
'
```

Expected: prints `OK: both notify jobs are wired correctly`.

- [ ] **Step 4: Re-run helper tests (sanity check)**

```bash
.gitlab/ci/scripts/test/run_tests.sh
```

Expected: still passing.

- [ ] **Step 5: Commit**

```bash
git add .gitlab-ci.yml
git commit -m "ci: post release notes to Discord and Telegram on tag push

Two new jobs in the release stage, both needs: [release], each calling
the corresponding helper script in .gitlab/ci/scripts/. Skip cleanly
when their secrets are unset; soft-fail on transport errors so chat
outages don't fail the pipeline."
```

---

### Task 7: Document the release flow update in auto-memory

The release flow note in `MEMORY.md` lists the things that auto-fire on a tag push. Add the chat-notifications step so future you doesn't think they need a manual post.

**Files:**

- Modify: `/Volumes/BigMac/Users/astrobureau/.claude/projects/-Volumes-BigMac-Users-astrobureau-Documents-Projects-athenaeum/memory/MEMORY.md` (the "Releasing / Tagging" section)

- [ ] **Step 1: Locate the release-flow section in MEMORY.md**

```bash
grep -n "publish_version" /Volumes/BigMac/Users/astrobureau/.claude/projects/-Volumes-BigMac-Users-astrobureau-Documents-Projects-athenaeum/memory/MEMORY.md
```

Expected: a line near the "Commit + push + tag" step that mentions `publish_version` auto-SCPing `version.json`.

- [ ] **Step 2: Edit step 5 of the Releasing / Tagging section** to mention the chat notifications

Find the line that starts with:

```text
5. **Commit + push + tag** — `git push origin main && git tag v<version> && git push origin v<version>`. The `publish_version` GitLab CI job
```

Append to the end of that paragraph (right before step 6):

```text
The `notify:discord` and `notify:telegram` jobs (`.gitlab-ci.yml`) post `RELEASE_NOTES.md` to chat after the `release` job creates the GitLab Release. Both skip cleanly if their CI/CD variables aren't set; both soft-fail on transport errors. Required vars: `DISCORD_WEBHOOK_URL`, `TELEGRAM_BOT_TOKEN`, `TELEGRAM_CHAT_ID`.
```

- [ ] **Step 3: Verify the edit**

```bash
grep -A 1 "notify:discord" /Volumes/BigMac/Users/astrobureau/.claude/projects/-Volumes-BigMac-Users-astrobureau-Documents-Projects-athenaeum/memory/MEMORY.md
```

Expected: returns the new line.

- [ ] **Step 4: No commit needed**

Auto-memory lives outside the repo — it's a personal note, not source. Skip the git step.

---

### Task 8: Final verification

A pre-flight checklist before any tag push exercises the new jobs.

- [ ] **Step 1: Run the full helper test suite once more**

```bash
.gitlab/ci/scripts/test/run_tests.sh
```

Expected: `Passed: 24  Failed: 0` (3 truncate + 13 markdown + 8 discord + ~9 telegram = ~33, exact count depends on which sub-asserts you collapsed).

- [ ] **Step 2: Manual end-to-end dry-run with the real RELEASE_NOTES.md**

```bash
DRY_RUN=1 \
  DISCORD_WEBHOOK_URL="https://discord.com/api/webhooks/dummy" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
  .gitlab/ci/scripts/notify_discord.sh | python3 -m json.tool
```

Expected: pretty-printed JSON. Inspect that:
- `embeds[0].title` reads `Athenaeum v0.2.0-beta.10 (beta) released`
- `embeds[0].color` is `15976499`
- `embeds[0].url` ends with `/-/releases/v0.2.0-beta.10`
- `embeds[0].description` contains a `…\n\nFull notes:` tail (the real notes are 4503 chars > 3900)
- `embeds[0].fields[0].name` is `Download`

```bash
DRY_RUN=1 \
  TELEGRAM_BOT_TOKEN="dummy:token" \
  TELEGRAM_CHAT_ID="@athenaeum_releases" \
  CI_COMMIT_TAG="v0.2.0-beta.10" \
  CI_PROJECT_URL="https://gitlab.com/eg013ra1n/athenaeum" \
  .gitlab/ci/scripts/notify_telegram.sh | python3 -m json.tool
```

Expected: pretty-printed JSON. Inspect that:
- `parse_mode` is `HTML`
- `text` starts with `<b>Athenaeum v0.2.0-beta.10 (beta) released</b>`
- `text` contains `<b>`, `<code>`, and `<a href=` tags from the body conversion
- `text` ends with the truncation tail and the `Download:` line
- Total `len(text)` is comfortably under 4096 (`python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d["text"]))'`)

- [ ] **Step 3: Acceptance test on the next real tag** (manual, post-merge)

After this branch is merged:

1. User adds `DISCORD_WEBHOOK_URL`, `TELEGRAM_BOT_TOKEN`, `TELEGRAM_CHAT_ID` in GitLab → Settings → CI/CD → Variables (all Masked, all Protected).
2. On the next `v*` tag push, watch the pipeline. The `release` stage should show `release`, `publish_version`, `notify:discord`, `notify:telegram` all green.
3. Confirm a single embed appears in the Discord channel within ~30 s of the `release` job finishing.
4. Confirm a single bold-titled message appears in the Telegram chat within ~30 s, with formatted body and the truncation tail.
5. If either fails, the GitLab job log shows the HTTP status and Discord/Telegram response body — no debugging guesswork.

There is no automated way to test step 3-5 from CI itself; that's the trade-off of soft-fail behaviour. The first tag push after merge is the live acceptance test.

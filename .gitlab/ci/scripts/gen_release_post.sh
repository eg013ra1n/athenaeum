#!/usr/bin/env bash
# RELEASE_NOTES.md (stdin) -> a Starlight blog post (stdout).
# Env: TAG (required unless --tagline), RELEASE_DATE (YYYY-MM-DD, default today UTC).
# `--tagline` prints only the bare tagline (first non-empty line, the italic
# *…* stripped) — the one sentence the Version History row and the GitLab
# Release description reuse.
set -euo pipefail

MODE="${1:-post}"
NOTES_FILE=$(mktemp -t release_notes.XXXXXX)
trap 'rm -f "$NOTES_FILE"' EXIT
cat > "$NOTES_FILE"

python3 - "$MODE" "${TAG:-}" "${RELEASE_DATE:-}" "$NOTES_FILE" <<'PY'
import datetime, json, sys
mode, tag, date, notes_file = sys.argv[1:5]

with open(notes_file, encoding="utf-8") as f:
    lines = f.read().splitlines()

i = 0
while i < len(lines) and not lines[i].strip():
    i += 1
if i == len(lines):
    sys.exit("ERROR: release notes are empty")
tagline = lines[i].strip()
if tagline.startswith("*") and tagline.endswith("*") and len(tagline) > 2:
    tagline = tagline[1:-1].strip()
body = lines[i + 1:]
while body and not body[0].strip():
    body.pop(0)

if mode == "--tagline":
    print(tagline)
    sys.exit(0)

if not tag:
    sys.exit("ERROR: TAG must be set")
if not date:
    date = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")

# json.dumps yields a double-quoted, backslash-escaped string — a valid YAML
# double-quoted scalar for every character a tagline can carry.
excerpt = json.dumps(tagline, ensure_ascii=False)
print("---")
print(f"title: Athenaeum {tag}")
print(f"date: {date}")
print("authors:")
print("  - vilen")
print("tags:")
print("  - release")
print(f"excerpt: {excerpt}")
print("---")
print()
print("\n".join(body).rstrip())
PY

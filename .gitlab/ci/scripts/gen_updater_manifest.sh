#!/usr/bin/env bash
# The in-app updater's manifest for one tag (spec §3.6), to stdout.
# Env: CI_COMMIT_TAG (required); STAGE_DIR (default release_stage) holding
# <subdir>/<filename>.sig for every updater_artifacts row; NOTES_PATH
# (default RELEASE_NOTES.md); BUILDS_BASE_URL; PUB_DATE (RFC 3339, default now).
# A missing or empty .sig is exit 1 — a platform is never omitted: every build
# job is required, so a missing signature is a pipeline defect, and a release
# the updater cannot serve on one OS is not a release.
set -euo pipefail

: "${CI_COMMIT_TAG:?CI_COMMIT_TAG must be set}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/artifact_names.sh"
VERSION="${CI_COMMIT_TAG#v}"; export VERSION
STAGE_DIR="${STAGE_DIR:-release_stage}"
NOTES_PATH="${NOTES_PATH:-RELEASE_NOTES.md}"
BUILDS_BASE_URL="${BUILDS_BASE_URL:-https://artfrom.space/builds}"
PUB_DATE="${PUB_DATE:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}"

[ -s "$NOTES_PATH" ] || { echo "ERROR: release notes not found at $NOTES_PATH" >&2; exit 1; }

# One line per platform: key<TAB>url<TAB>sig_path — python assembles the JSON
# (never printf-built JSON: notes carry quotes, backslashes and newlines).
ROWS=$(mktemp -t updater_rows.XXXXXX); trap 'rm -f "$ROWS"' EXIT
fail=0
while read -r product os arch ext variant subdir filename key; do
  sig="$STAGE_DIR/$subdir/$filename.sig"
  if [ ! -s "$sig" ]; then echo "ERROR: missing signature $sig" >&2; fail=1; continue; fi
  printf '%s\t%s\t%s\n' "$key" "${BUILDS_BASE_URL}/${CI_COMMIT_TAG}/${subdir}/${filename}" "$sig" >> "$ROWS"
done < <(updater_artifacts)
[ "$fail" -eq 0 ] || exit 1

python3 - "$VERSION" "$PUB_DATE" "$NOTES_PATH" "$ROWS" <<'PY'
import datetime, json, sys
version, pub_date, notes_path, rows_path = sys.argv[1:5]
try:
    datetime.datetime.strptime(pub_date, "%Y-%m-%dT%H:%M:%SZ")
except ValueError:
    sys.exit("ERROR: PUB_DATE must be RFC 3339 UTC like 2026-10-01T12:00:00Z, got %r" % pub_date)
platforms = {}
with open(rows_path, encoding="utf-8") as f:
    for line in f:
        key, url, sig_path = line.rstrip("\n").split("\t")
        with open(sig_path, encoding="utf-8") as s:
            signature = s.read().strip()
        if not signature:
            sys.exit("ERROR: missing signature %s" % sig_path)
        platforms[key] = {"url": url, "signature": signature}
with open(notes_path, encoding="utf-8") as f:
    notes = f.read()
json.dump({"version": version, "notes": notes, "pub_date": pub_date, "platforms": platforms},
          sys.stdout, indent=2, ensure_ascii=False)
sys.stdout.write("\n")
PY

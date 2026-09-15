#!/usr/bin/env bash
# Create the GitLab Release object for a tag: RELEASE_NOTES.md as the
# description, one asset link per published installer (from artifact_names.sh).
# Env: CI_COMMIT_TAG, CI_API_V4_URL, CI_PROJECT_ID, CI_JOB_TOKEN (GitLab-provided);
#      RELEASE_NOTES_PATH (RELEASE_NOTES.md), BUILDS_BASE_URL; DRY_RUN=1 prints the payload.
set -euo pipefail

: "${CI_COMMIT_TAG:?}"; : "${CI_API_V4_URL:?}"; : "${CI_PROJECT_ID:?}"; : "${CI_JOB_TOKEN:?}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/artifact_names.sh"
VERSION="${CI_COMMIT_TAG#v}"; export VERSION
NOTES="${RELEASE_NOTES_PATH:-RELEASE_NOTES.md}"
BUILDS_BASE_URL="${BUILDS_BASE_URL:-https://artfrom.space/builds}"

label() { # $1..: product os arch ext variant
  local p="$1" os="$2" arch="$3" ext="$4" variant="$5" product
  case "$p" in athenaeum) product="Athenaeum" ;; perseus) product="Perseus" ;; esac
  local osname; case "$os" in macos) osname="macOS" ;; windows) osname="Windows" ;; linux) osname="Linux" ;; esac
  local archname; case "$arch" in x64) archname="x64" ;; arm64) archname="arm64" ;; esac
  local fmt; case "$ext" in msi) fmt="MSI installer" ;; exe) fmt="EXE setup" ;; dmg) fmt="disk image" ;; AppImage) fmt="AppImage" ;; deb) fmt="Debian package" ;; tar.gz) fmt="tarball" ;; *) fmt="$ext" ;; esac
  printf '%s for %s (%s, %s)' "$product" "$osname" "$archname" "$fmt"
}

# The label/URL pairs travel to python through a temp file, not stdin — a
# heredoc already occupies stdin for the python script itself, so a
# `<<'PY' … PY < <(…)` construct on the same command would bind stdin twice.
LINKS_FILE=$(mktemp -t release_links.XXXXXX)
RESP=""
trap 'rm -f "$LINKS_FILE" "$RESP"' EXIT

all_release_artifacts | while read -r product os arch ext variant subdir filename alias; do
  printf '%s\t%s\n' "$(label "$product" "$os" "$arch" "$ext" "$variant")" "${BUILDS_BASE_URL}/${CI_COMMIT_TAG}/${subdir}/${filename}"
done > "$LINKS_FILE"

PAYLOAD=$(python3 - "$CI_COMMIT_TAG" "$NOTES" "$LINKS_FILE" <<'PY'
import json, sys
tag, notes, links_file = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    description = open(notes, encoding="utf-8").read()
except OSError:
    description = f"Release {tag}"
with open(links_file, encoding="utf-8") as f:
    links = [{"name": n, "url": u} for n, u in (l.rstrip("\n").split("\t") for l in f if l.strip())]
print(json.dumps({"tag_name": tag, "name": f"Athenaeum {tag}", "description": description, "assets": {"links": links}}, indent=2, ensure_ascii=False))
PY
)

if [ "${DRY_RUN:-0}" = "1" ]; then printf '%s\n' "$PAYLOAD"; exit 0; fi
RESP=$(mktemp -t release_resp.XXXXXX)
code=$(printf '%s' "$PAYLOAD" | curl --silent --show-error --request POST --output "$RESP" --write-out '%{http_code}' \
  --header "JOB-TOKEN: ${CI_JOB_TOKEN}" --header "Content-Type: application/json" --data @- \
  "${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/releases" || echo 000)
if [ "$code" != "201" ]; then echo "ERROR: GitLab release API answered HTTP $code: $(cat "$RESP")" >&2; exit 1; fi
echo "ok: GitLab Release ${CI_COMMIT_TAG} created with $(printf '%s' "$PAYLOAD" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["assets"]["links"]))') links"

#!/usr/bin/env bash
# Fetch back everything a release published and refuse to let the pipeline
# announce until all of it is really there. Blocking by design (review F1).
set -euo pipefail

: "${CI_COMMIT_TAG:?CI_COMMIT_TAG must be set}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/artifact_names.sh"
VERSION="${CI_COMMIT_TAG#v}"; export VERSION
BUILDS_BASE_URL="${BUILDS_BASE_URL:-https://artfrom.space/builds}"
DOCKERHUB_REPO="${DOCKERHUB_REPO:-vsharifov/athenaeum}"
MIN_BYTES="${MIN_ARTIFACT_BYTES:-1048576}"
BLOG_BASE_URL="${BLOG_BASE_URL:-https://artfrom.space/blog}"
BLOG_TIMEOUT="${VERIFY_BLOG_TIMEOUT_SECONDS:-900}"
POLL="${VERIFY_POLL_SECONDS:-30}"
fail=0

HDR=$(mktemp -t verify_hdr.XXXXXX); BODY=$(mktemp -t verify_body.XXXXXX); trap 'rm -f "$HDR" "$BODY"' EXIT

# --- 1. every installer answers HEAD 200 and is not a stub ---------------------
count=0
while read -r product os arch ext variant subdir filename alias; do
  url="${BUILDS_BASE_URL}/${CI_COMMIT_TAG}/${subdir}/${filename}"
  code=$(curl --silent --show-error --head --output "$HDR" --write-out '%{http_code}' --max-time 60 "$url" || echo 000)
  if [ "$code" != "200" ]; then echo "ERROR: HTTP $code for $url" >&2; fail=1; continue; fi
  length=$(tr -d '\r' < "$HDR" | awk 'tolower($1)=="content-length:"{print $2}' | tail -n1)
  if [ -z "$length" ] || [ "$length" -lt "$MIN_BYTES" ]; then echo "ERROR: ${length:-0} bytes (< $MIN_BYTES) at $url" >&2; fail=1; continue; fi
  count=$((count + 1))
done < <(all_release_artifacts)
[ "$fail" -eq 0 ] && echo "ok: $count artifacts present under ${BUILDS_BASE_URL}/${CI_COMMIT_TAG}/"

# --- 2. Docker Hub: the version tag exists, carries the arches, the channel tag moved
if [ "${SKIP_DOCKER_CHECK:-0}" = "1" ]; then
  echo "skipped: docker check (SKIP_DOCKER_CHECK=1)"
else
  case "$CI_COMMIT_TAG" in *-beta*) channel=beta ;; *) channel=latest ;; esac
  code=$(curl --silent --show-error --output "$BODY" --write-out '%{http_code}' --max-time 60 "https://hub.docker.com/v2/repositories/${DOCKERHUB_REPO}/tags/${VERSION}" || echo 000)
  if [ "$code" != "200" ]; then
    echo "ERROR: Docker Hub has no tag ${VERSION} for ${DOCKERHUB_REPO} (HTTP $code)" >&2; fail=1
  else
    arches=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(" ".join(sorted(i["architecture"] for i in d.get("images",[]))))' "$BODY")
    digest=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("digest",""))' "$BODY")
    case " $arches " in *" amd64 "*) ;; *) echo "ERROR: docker.io/${DOCKERHUB_REPO}:${VERSION} has no amd64 image" >&2; fail=1 ;; esac
    case " $arches " in
      *" arm64 "*) echo "ok: docker.io/${DOCKERHUB_REPO}:${VERSION} has amd64 + arm64" ;;
      *) if [ "${RELEASE_ALLOW_AMD64_ONLY:-0}" = "1" ]; then echo "WARNING: publishing amd64-only — RELEASE_ALLOW_AMD64_ONLY=1 is set; unset it when the arm64 runner is back"
         else echo "ERROR: docker.io/${DOCKERHUB_REPO}:${VERSION} has no arm64 image (set RELEASE_ALLOW_AMD64_ONLY=1 to accept)" >&2; fail=1; fi ;;
    esac
    code=$(curl --silent --show-error --output "$BODY" --write-out '%{http_code}' --max-time 60 "https://hub.docker.com/v2/repositories/${DOCKERHUB_REPO}/tags/${channel}" || echo 000)
    cdigest=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("digest",""))' "$BODY" 2>/dev/null || true)
    if [ "$code" = "200" ] && [ -n "$digest" ] && [ "$cdigest" = "$digest" ]; then echo "ok: :${channel} -> ${digest}"
    else echo "ERROR: :${channel} does not point at ${VERSION} (channel digest '${cdigest}', version digest '${digest}', HTTP $code)" >&2; fail=1; fi
  fi
fi

# --- 3. the blog post the announcements will link ------------------------------
if [ "${SKIP_BLOG_CHECK:-0}" = "1" ]; then
  echo "skipped: blog check (SKIP_BLOG_CHECK=1)"
else
  slug="$(printf '%s' "$CI_COMMIT_TAG" | tr -d '.')"
  blog="${BLOG_BASE_URL}/${slug}/"
  start=$(date +%s)
  while :; do
    code=$(curl --silent --show-error --head --output "$HDR" --write-out '%{http_code}' --max-time 30 "$blog" || echo 000)
    if [ "$code" = "200" ]; then echo "ok: blog $blog"; break; fi
    elapsed=$(( $(date +%s) - start ))
    if [ "$elapsed" -ge "$BLOG_TIMEOUT" ]; then echo "ERROR: blog $blog still HTTP $code after ${elapsed}s — did docs:publish push, did the docs site deploy?" >&2; fail=1; break; fi
    echo "waiting: blog $blog is HTTP $code (${elapsed}s)"; sleep "$POLL"
  done
fi

[ "$fail" -eq 0 ] || { echo "ERROR: release ${CI_COMMIT_TAG} is not fully published — nothing will be announced" >&2; exit 1; }
echo "ok: release ${CI_COMMIT_TAG} verified"

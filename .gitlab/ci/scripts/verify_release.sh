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

# --- 1b. the updater manifest: version, every platform, every URL, every signature
UPDATES_BASE_URL="${UPDATES_BASE_URL:-https://artfrom.space/updates}"
TAURI_CONF_PATH="${TAURI_CONF_PATH:-$SCRIPT_DIR/../../../crates/athenaeum-tauri/tauri.conf.json}"
if [ "${SKIP_UPDATER_CHECK:-0}" = "1" ]; then
  echo "skipped: updater manifest check (SKIP_UPDATER_CHECK=1)"
else
  murl="${UPDATES_BASE_URL}/${CI_COMMIT_TAG}.json"
  # DL is a directory, not a single file: each artifact downloads to
  # "$DL/<real filename>" so a downstream signature-verification failure can
  # be reported against the artifact it actually names, not a mktemp path.
  MAN=$(mktemp -t verify_manifest.XXXXXX); PUB=$(mktemp -t verify_pub.XXXXXX); DL=$(mktemp -d -t verify_dl.XXXXXX); SIG=$(mktemp -t verify_sig.XXXXXX)
  trap 'rm -rf "$HDR" "$BODY" "$MAN" "$PUB" "$DL" "$SIG"' EXIT
  code=$(curl --silent --show-error --output "$MAN" --write-out '%{http_code}' --max-time 60 "$murl" || echo 000)
  if [ "$code" != "200" ]; then
    echo "ERROR: HTTP $code for $murl" >&2; fail=1
  else
    mver=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$MAN" 2>/dev/null || true)
    if [ -z "$mver" ]; then echo "ERROR: updater manifest at $murl is not the expected JSON" >&2; fail=1
    elif [ "$mver" != "$VERSION" ]; then echo "ERROR: updater manifest version is $mver, tag $CI_COMMIT_TAG expects $VERSION" >&2; fail=1
    else
      # The committed public key (base64 of the minisign key file) → rsign's key file.
      python3 -c 'import base64,json,sys; sys.stdout.write(base64.b64decode(json.load(open(sys.argv[1]))["plugins"]["updater"]["pubkey"]).decode())' "$TAURI_CONF_PATH" > "$PUB"
      command -v rsign >/dev/null || { echo "ERROR: rsign (rsign2) is not installed on this runner — cargo install rsign2 --locked" >&2; exit 1; }
      # The inventory itself names how many platforms to expect — never a
      # literal, so a future platform added to updater_artifacts() doesn't
      # silently under-count here.
      expected_platforms=$(updater_artifacts | wc -l | tr -d ' ')
      mcount=0
      while read -r product os arch ext variant subdir filename key; do
        expected="${BUILDS_BASE_URL}/${CI_COMMIT_TAG}/${subdir}/${filename}"
        if ! entry=$(python3 -c 'import json,sys; p=json.load(open(sys.argv[1]))["platforms"].get(sys.argv[2]); print(p["url"]+"\t"+p["signature"] if p else "")' "$MAN" "$key" 2>/dev/null); then
          echo "ERROR: updater manifest entry for $key is malformed" >&2; fail=1; continue
        fi
        if [ -z "$entry" ]; then echo "ERROR: updater manifest has no entry for $key" >&2; fail=1; continue; fi
        IFS=$'\t' read -r url signature <<< "$entry"
        if [ "$url" != "$expected" ]; then echo "ERROR: updater manifest $key points at $url, expected $expected" >&2; fail=1; continue; fi
        dlfile="$DL/$filename"
        code=$(curl --silent --show-error --output "$dlfile" --write-out '%{http_code}' --max-time 600 "$url" || echo 000)
        if [ "$code" != "200" ] || [ ! -s "$dlfile" ]; then echo "ERROR: HTTP $code (or empty body) for $url" >&2; fail=1; continue; fi
        if ! printf '%s' "$signature" | python3 -c 'import base64,sys; sys.stdout.write(base64.b64decode(sys.stdin.read()).decode())' > "$SIG" 2>/dev/null; then
          echo "ERROR: updater signature for $url is not valid base64" >&2; fail=1; continue
        fi
        if rsout=$(rsign verify -p "$PUB" -x "$SIG" "$dlfile" 2>&1); then mcount=$((mcount + 1))
        else echo "ERROR: updater signature does not verify for $url — $rsout" >&2; fail=1; fi
      done < <(updater_artifacts)
      if [ "$fail" -eq 0 ] && [ "$mcount" -ne "$expected_platforms" ]; then
        echo "ERROR: updater manifest verified $mcount platforms, expected $expected_platforms" >&2; fail=1
      fi
      [ "$fail" -eq 0 ] && echo "ok: updater manifest v${VERSION} — ${mcount} platforms, every URL present, every signature verified"
    fi
  fi
fi

# --- 2. Docker Hub: the version tag exists, carries the arches, the channel tag moved
if [ "${SKIP_DOCKER_CHECK:-0}" = "1" ]; then
  echo "skipped: docker check (SKIP_DOCKER_CHECK=1)"
else
  case "$CI_COMMIT_TAG" in *-beta*) channel=beta ;; *) channel=latest ;; esac
  # The tag API is eventually consistent right after `imagetools create` —
  # retry a non-200 a few times before calling the tag missing.
  DOCKER_ATTEMPTS="${VERIFY_DOCKER_ATTEMPTS:-3}"
  attempt=1
  code=000
  while [ "$attempt" -le "$DOCKER_ATTEMPTS" ]; do
    code=$(curl --silent --show-error --output "$BODY" --write-out '%{http_code}' --max-time 60 "https://hub.docker.com/v2/repositories/${DOCKERHUB_REPO}/tags/${VERSION}" || echo 000)
    [ "$code" = "200" ] && break
    echo "waiting: Docker Hub tag ${VERSION} is HTTP ${code} (attempt ${attempt}/${DOCKER_ATTEMPTS})"
    attempt=$((attempt + 1))
    [ "$attempt" -le "$DOCKER_ATTEMPTS" ] && sleep "$POLL"
  done
  if [ "$code" != "200" ]; then
    echo "ERROR: Docker Hub has no tag ${VERSION} for ${DOCKERHUB_REPO} (HTTP $code)" >&2; fail=1
  else
    # One combined, guarded parse: a 200 with a non-JSON body (rate-limit
    # HTML, a CDN error page) must yield one ERROR line, never a Python
    # traceback aborting the script under `set -e`.
    parsed=$(python3 -c 'import json,sys
d = json.load(open(sys.argv[1]))
print(" ".join(sorted(i["architecture"] for i in d.get("images", []))) + "\t" + d.get("digest", ""))' "$BODY" 2>/dev/null || true)
    if [ -z "$parsed" ]; then
      echo "ERROR: Docker Hub answered 200 for tag ${VERSION} with a body that is not JSON" >&2; fail=1
    else
      IFS=$'\t' read -r arches digest <<< "$parsed"
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

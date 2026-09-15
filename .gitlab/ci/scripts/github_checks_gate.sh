#!/usr/bin/env bash
# Block a tag pipeline until the tagged commit's GitHub check-suite is green.
# The GitHub run of the release commit starts on the push of main, seconds
# before the tag; this job waits for it instead of racing it.
# Env: CI_COMMIT_SHA (required); GITHUB_REPO (eg013ra1n/athenaeum);
#      GITHUB_API_TOKEN (optional — raises the rate limit, shortens the poll);
#      GATE_REQUIRED_CHECKS (';'-separated check names, default = both ci.yml jobs);
#      GATE_TIMEOUT_SECONDS (3000); GATE_POLL_SECONDS (60, or 20 with a token);
#      GATE_INITIAL_WAIT_SECONDS (600 — tolerate "no runs yet" this long).
set -euo pipefail

: "${CI_COMMIT_SHA:?CI_COMMIT_SHA must be set}"
GITHUB_REPO="${GITHUB_REPO:-eg013ra1n/athenaeum}"
REQUIRED="${GATE_REQUIRED_CHECKS:-Build, test and typecheck;Build and test (Windows)}"
TIMEOUT="${GATE_TIMEOUT_SECONDS:-3000}"
INITIAL_WAIT="${GATE_INITIAL_WAIT_SECONDS:-600}"
if [ -n "${GITHUB_API_TOKEN:-}" ]; then POLL="${GATE_POLL_SECONDS:-20}"; else POLL="${GATE_POLL_SECONDS:-60}"; fi
URL="https://api.github.com/repos/${GITHUB_REPO}/commits/${CI_COMMIT_SHA}/check-runs?per_page=100"

RESP=$(mktemp -t gh_checks.XXXXXX); trap 'rm -f "$RESP"' EXIT
start=$(date +%s)
while :; do
  args=(--silent --show-error --output "$RESP" --write-out '%{http_code}' --header "Accept: application/vnd.github+json" --max-time 30)
  [ -n "${GITHUB_API_TOKEN:-}" ] && args+=(--header "Authorization: Bearer ${GITHUB_API_TOKEN}")
  code=$(curl "${args[@]}" "$URL" || echo 000)
  elapsed=$(( $(date +%s) - start ))
  if [ "$code" != "200" ]; then
    echo "warning: GitHub API answered HTTP $code (elapsed ${elapsed}s)"
    verdict=retry
  else
    # Guarded parse, mirroring verify_release.sh: a 200 with a body that is
    # not JSON (rate-limit HTML, a CDN error page) must yield one warning and
    # another poll, never a Python traceback aborting the script under `set -e`.
    py_out=$(python3 - "$RESP" "$REQUIRED" 2>/dev/null <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
required = [r for r in sys.argv[2].split(";") if r]
runs = {r["name"]: r for r in data.get("check_runs", [])}
if data.get("total_count", 0) == 0:
    print("none"); sys.exit(0)
verdict = "ok"
rank = {"ok": 0, "retry": 1, "fail": 2}
def worst(v1, v2):
    return v2 if rank.get(v2, -1) > rank.get(v1, -1) else v1
for name in required:
    r = runs.get(name)
    if r is None:
        print(f"waiting: {name} has not started"); verdict = worst(verdict, "retry"); continue
    if r["status"] != "completed":
        print(f"waiting: {name} is {r['status']}"); verdict = worst(verdict, "retry"); continue
    if r["conclusion"] in ("success", "skipped", "neutral"):
        print(f"ok: {name} — {r['conclusion']}")
    else:
        print(f"ERROR: {name} concluded {r['conclusion']}"); verdict = worst(verdict, "fail")
print(verdict)
PY
) || true
    if [ -z "$py_out" ]; then
      echo "warning: GitHub answered 200 with a body that is not JSON (elapsed ${elapsed}s)"
      verdict=retry
    else
      printf '%s\n' "$py_out" | sed '$d'
      verdict=$(printf '%s\n' "$py_out" | tail -n1)
    fi
  fi
  case "$verdict" in
    ok)   echo "ok: GitHub check-suite green for ${CI_COMMIT_SHA}"; exit 0 ;;
    fail) echo "ERROR: GitHub check-suite red for ${CI_COMMIT_SHA} — fix on main and re-tag" >&2; exit 1 ;;
    none) if [ "$elapsed" -ge "$INITIAL_WAIT" ]; then
            echo "ERROR: no GitHub check runs for ${CI_COMMIT_SHA} after ${elapsed}s — was main pushed to GitHub before the tag?" >&2; exit 1
          fi
          echo "waiting: no check runs yet (${elapsed}s)" ;;
  esac
  if [ "$elapsed" -ge "$TIMEOUT" ]; then
    echo "ERROR: GitHub check-suite for ${CI_COMMIT_SHA} still not complete after ${TIMEOUT}s" >&2; exit 1
  fi
  sleep "$POLL"
done

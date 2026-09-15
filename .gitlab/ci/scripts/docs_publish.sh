#!/usr/bin/env bash
# Publish a release to the docs site: blog post + download page, one commit on main.
# Env: TAG (required), RELEASE_NOTES_PATH (default RELEASE_NOTES.md),
#      RELEASE_DATE (default today UTC), DOCS_REPO_URL, DOCS_REPO_TOKEN
#      (required unless DRY_RUN=1), DOCS_GIT_NAME / DOCS_GIT_EMAIL,
#      DRY_RUN=1 + DOCS_REPO_LOCAL=<path> → clone that path, print the diff, never push.
# Idempotent: a re-run for the same tag rewrites the same post and row; no
# change → "docs: nothing to publish", exit 0.
set -euo pipefail

: "${TAG:?TAG must be set}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
NOTES="${RELEASE_NOTES_PATH:-RELEASE_NOTES.md}"
RELEASE_DATE="${RELEASE_DATE:-$(date -u +%Y-%m-%d)}"
DOCS_REPO_URL="${DOCS_REPO_URL:-http://192.168.31.208:9080/root/artfrom-space.git}"
DOCS_GIT_NAME="${DOCS_GIT_NAME:-eg013ra1n}"
DOCS_GIT_EMAIL="${DOCS_GIT_EMAIL:-vilen.sharifov@gmail.com}"
DRY_RUN="${DRY_RUN:-0}"

[ -s "$NOTES" ] || { echo "ERROR: release notes not found at $NOTES" >&2; exit 1; }

WORK=$(mktemp -d -t docs_publish.XXXXXX)
trap 'rm -rf "$WORK"' EXIT

if [ "$DRY_RUN" = "1" ]; then
  : "${DOCS_REPO_LOCAL:?DOCS_REPO_LOCAL must be set for DRY_RUN=1}"
  git clone -q "$DOCS_REPO_LOCAL" "$WORK/site"
else
  : "${DOCS_REPO_TOKEN:?DOCS_REPO_TOKEN must be set (artfrom-space project access token, write_repository)}"
  # Any non-empty username works with a GitLab project access token.
  AUTH_URL="$(printf '%s' "$DOCS_REPO_URL" | sed -E "s#^(https?://)#\1release-bot:${DOCS_REPO_TOKEN}@#")"
  git clone -q --depth 1 --branch main "$AUTH_URL" "$WORK/site"
fi

POST="$WORK/site/src/content/docs/blog/${TAG}.md"
PAGE="$WORK/site/src/content/docs/releases/download.md"
mkdir -p "$(dirname "$POST")"
TAG="$TAG" RELEASE_DATE="$RELEASE_DATE" "$SCRIPT_DIR/gen_release_post.sh" < "$NOTES" > "$POST"
TAGLINE="$("$SCRIPT_DIR/gen_release_post.sh" --tagline < "$NOTES")"
PAGE="$PAGE" TAG="$TAG" RELEASE_DATE="$RELEASE_DATE" TAGLINE="$TAGLINE" "$SCRIPT_DIR/update_download_page.sh"

cd "$WORK/site"
git add -A src/content/docs/blog src/content/docs/releases/download.md
if git diff --cached --quiet; then
  echo "docs: nothing to publish for $TAG (post and row already current)"
  exit 0
fi
git -c user.name="$DOCS_GIT_NAME" -c user.email="$DOCS_GIT_EMAIL" commit -q -m "docs: ${TAG} release post + download-page row"
git show --stat --oneline HEAD | sed 's/^/  /'
if [ "$DRY_RUN" = "1" ]; then
  echo "DRY_RUN=1: not pushing"
  exit 0
fi
git push -q origin HEAD:main
echo "docs: pushed ${TAG} post + download-page row to main"

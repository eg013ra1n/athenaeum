#!/usr/bin/env bash
#
# sync-github.sh — Mirror the repo to GitHub with accumulating history.
#
# Reads .github-export-ignore for files/dirs to exclude, clones the current
# GitHub repo, rsyncs filtered content into it, and pushes a new commit.
# Skips pushing if nothing changed. On first run (empty remote), initializes
# a fresh repo and force-pushes.
#
# Usage:
#   ./scripts/sync-github.sh [--force] [REMOTE_URL]
#
# Options:
#   --force     Wipe GitHub history and start fresh (use once for initial push)
#
# Arguments:
#   REMOTE_URL  Git remote URL (default: git@github.com:eg013ra1n/athenaeum.git)
#
# CI Setup:
#   Requires a GITHUB_DEPLOY_KEY CI variable (File type) containing an SSH
#   private key with write access to the GitHub repo. See .gitlab-ci.yml.

set -euo pipefail

FORCE=false
if [ "${1:-}" = "--force" ]; then
    FORCE=true
    shift
fi

REMOTE="${1:-git@github.com:eg013ra1n/athenaeum.git}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IGNORE_FILE="${REPO_ROOT}/.github-export-ignore"
TMPDIR=""

cleanup() {
    if [ -n "$TMPDIR" ] && [ -d "$TMPDIR" ]; then
        rm -rf "$TMPDIR"
    fi
}
trap cleanup EXIT

# --- Build rsync exclude list ---------------------------------------------------
RSYNC_EXCLUDES=(
    --exclude='.git/'
    --exclude='node_modules/'
    --exclude='dist/'
    --exclude='src-tauri/target/'
)

if [ -f "$IGNORE_FILE" ]; then
    while IFS= read -r line; do
        # Skip empty lines and comments
        [[ -z "$line" || "$line" == \#* ]] && continue
        RSYNC_EXCLUDES+=(--exclude="$line")
    done < "$IGNORE_FILE"
else
    echo "Warning: $IGNORE_FILE not found, only default excludes will be used." >&2
fi

# --- Determine source commit info -----------------------------------------------
SOURCE_SHA="unknown"
SOURCE_MSG="Mirror from GitLab"
if command -v git &>/dev/null && git -C "$REPO_ROOT" rev-parse HEAD &>/dev/null; then
    SOURCE_SHA="$(git -C "$REPO_ROOT" rev-parse --short HEAD)"
    SOURCE_MSG="$(git -C "$REPO_ROOT" log -1 --format=%B)"
fi

# --- Clone existing GitHub repo or init fresh -----------------------------------
TMPDIR="$(mktemp -d)"

if [ "$FORCE" = true ]; then
    echo "Force mode: starting fresh (will overwrite GitHub history)."
    git init -b main "$TMPDIR"
    git -C "$TMPDIR" remote add origin "$REMOTE"
elif git clone --depth=1 "$REMOTE" "$TMPDIR" 2>/dev/null; then
    echo "Cloned existing GitHub repo."
else
    echo "Remote is empty or unreachable — initializing fresh repo."
    rm -rf "$TMPDIR"
    TMPDIR="$(mktemp -d)"
    git init -b main "$TMPDIR"
    git -C "$TMPDIR" remote add origin "$REMOTE"
fi

# --- Rsync content (--delete removes files no longer in source) -----------------
echo "Syncing repo to mirror directory..."
rsync -a --delete "${RSYNC_EXCLUDES[@]}" --exclude='.git/' "${REPO_ROOT}/" "${TMPDIR}/"

# --- Set commit identity (ensures CI doesn't use unexpected defaults) -----------
cd "$TMPDIR"
git config user.name "eg013ra1n"
git config user.email "vilen.sharifov@gmail.com"

# --- Commit and push if there are changes ---------------------------------------
git add -A

if git diff --cached --quiet; then
    echo "No changes to mirror. Skipping push."
    exit 0
fi

git commit -m "${SOURCE_MSG}"

echo "Pushing to ${REMOTE}..."
if [ "$FORCE" = true ]; then
    git push --force origin main
else
    git push origin main
fi

echo "Done. GitHub mirror updated (source: ${SOURCE_SHA})."

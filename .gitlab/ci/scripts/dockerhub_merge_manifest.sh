#!/usr/bin/env bash
# Merge the per-arch Docker Hub image tags pushed separately by
# publish:dockerhub:amd64 (linux runner) and publish:dockerhub:arm64 (macOS
# runner, native builds each) into a single multi-arch manifest, published
# under the release version tag and the rolling channel tag (:latest /
# :beta). Extracted from the old single-job qemu-based publish:dockerhub so
# it's independently testable — see .gitlab/ci/scripts/test/run_tests.sh.
#
# Required env:
#   IMAGE              e.g. docker.io/vsharifov/athenaeum
#   VERSION             e.g. 0.2.3 (no leading v)
#   CHANNEL_TAG         latest|beta
#   DOCKERHUB_USERNAME  Docker Hub username
#   DOCKERHUB_TOKEN     Docker Hub access token
#   CI_COMMIT_TAG       e.g. v0.2.3 (forwarded to update_dockerhub_description.sh;
#                       a GitLab-predefined variable already present in the
#                       job's environment, so callers don't need to set it by
#                       hand outside tests)
#
# Behaviour:
#   - DOCKERHUB_USERNAME / DOCKERHUB_TOKEN empty -> log + exit 0 (same
#     skip-cleanly semantics as the arch build jobs and the other CI helpers
#     in this directory).
#   - ${IMAGE}:${VERSION}-amd64 missing from the registry (checked via
#     `docker buildx imagetools inspect`) -> hard error, exit 1. There is
#     nothing to publish without an amd64 image.
#   - ${IMAGE}:${VERSION}-arm64 missing -> LOUD warning, continue with an
#     amd64-only manifest (the macOS arm64 job is allow_failure and may
#     legitimately not have produced its tag, e.g. Docker Desktop was closed
#     on the runner host).
#   - `docker buildx imagetools create` merges whichever arch tag(s) are
#     present into ${IMAGE}:${VERSION} and ${IMAGE}:${CHANNEL_TAG} — an
#     index-only operation, no rebuild/re-push of image layers.
#   - On success, forwards to update_dockerhub_description.sh (moved here
#     from the old single job) so the Hub description table stays in sync.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ -z "${DOCKERHUB_USERNAME:-}" ] || [ -z "${DOCKERHUB_TOKEN:-}" ]; then
  echo "DOCKERHUB_USERNAME / DOCKERHUB_TOKEN not set; skipping Docker Hub manifest merge."
  exit 0
fi

: "${IMAGE:?IMAGE not set}"
: "${VERSION:?VERSION not set}"
: "${CHANNEL_TAG:?CHANNEL_TAG not set}"

AMD64_TAG="${IMAGE}:${VERSION}-amd64"
ARM64_TAG="${IMAGE}:${VERSION}-arm64"

echo "${DOCKERHUB_TOKEN}" | docker login -u "${DOCKERHUB_USERNAME}" --password-stdin docker.io

tag_exists() {
  docker buildx imagetools inspect "$1" >/dev/null 2>&1
}

if ! tag_exists "$AMD64_TAG"; then
  echo "ERROR: ${AMD64_TAG} not found on Docker Hub — amd64 build did not publish. Aborting manifest merge."
  exit 1
fi
echo "Found ${AMD64_TAG}."

SOURCE_TAGS=("$AMD64_TAG")
if tag_exists "$ARM64_TAG"; then
  echo "Found ${ARM64_TAG}."
  SOURCE_TAGS+=("$ARM64_TAG")
else
  echo "WARNING: ${ARM64_TAG} not found on Docker Hub — publishing ${IMAGE}:${VERSION} and ${IMAGE}:${CHANNEL_TAG} as amd64-only. Check the publish:dockerhub:arm64 job (macOS runner / Docker Desktop must be running)."
fi

echo "Creating manifest ${IMAGE}:${VERSION} + ${IMAGE}:${CHANNEL_TAG} from: ${SOURCE_TAGS[*]}"
docker buildx imagetools create \
  -t "${IMAGE}:${VERSION}" \
  -t "${IMAGE}:${CHANNEL_TAG}" \
  "${SOURCE_TAGS[@]}"

DOCKERHUB_REPO="${IMAGE#docker.io/}" VERSION="${VERSION}" CHANNEL_TAG="${CHANNEL_TAG}" \
  "${SCRIPT_DIR}/update_dockerhub_description.sh"

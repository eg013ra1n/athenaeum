# Release Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A tag pipeline that publishes everything before it announces anything, gates on the tests that already ran, writes the docs-site post itself, names every installer one way, and a one-command release procedure that owns all of it.

**Architecture:** The GitLab tag pipeline grows from `build → deploy → release` to `gate → build → deploy → publish → verify → announce`: the gate refuses a tag whose versions disagree or whose GitHub check-suite is not green; publish covers Docker Hub AND the docs site; a blocking verify fetches every published thing back (installers, Docker tags, the blog URL, Gatekeeper on the real DMGs) and only then do the GitLab Release, `version.json` and the chat posts fire. Every new behaviour lives in a small bash script under `.gitlab/ci/scripts/` with a test in the existing `run_tests.sh` harness and mocks for `curl`/`docker`, so the YAML stays a thin call layer. One artifact naming function is the single source of every filename in the deploy job, the release links, the verify list and the download page.

**Tech Stack:** GitLab CI (shell runners: linux, windows, macos), bash + python3 (stdlib only) helper scripts, the existing `.gitlab/ci/scripts/test/run_tests.sh` harness, GitHub Actions (`ci.yml`), Astro/Starlight docs site in `../artfrom-space`, Tauri 2 bundler, `notarytool`/`spctl`.

**Spec:** `docs/superpowers/research/2026-09-15-release-cycle-review.md` (the review; findings F1–F7 and §4's cycle order). Cycle 4 (in-app updater, §3) is NOT in this plan — it gets its own spec after this plan lands, because it depends on the final artifact names and on the verify stage.

## Global Constraints

- **Assumptions made where the review left owner decisions open** (stated here so a later reversal knows what to undo): ONE release text — `RELEASE_NOTES.md` is the blog post body and its italic tagline is the excerpt and the Version History row (F3); macOS x64 stays (F5.2) and the version-check query gains an `arch` parameter so telemetry can answer it later; the in-app updater is out of scope.
- Naming scheme (F4), verbatim: `<product>-<version>-<os>-<arch>[-<variant>].<ext>` with `product ∈ {athenaeum, perseus}` lower-case, `version` WITHOUT `v` (`0.6.4`, `0.6.4-beta.1`), `os ∈ {macos, windows, linux}`, `arch ∈ {x64, arm64}` and nothing else — `amd64`, `x86_64`, `aarch64` never appear in a filename. Stable aliases drop the version only: `athenaeum-macos-arm64.dmg`. The old alias spellings (`Athenaeum-macos-aarch64.dmg`, `athenaeum-linux-x86_64.AppImage`, `perseus-linux-amd64.deb`, …) stay as **legacy symlinks** until v0.7.0, in a marked block, so external links keep working.
- Every helper script: `#!/usr/bin/env bash`, `set -euo pipefail`, reads its inputs from environment variables, supports `DRY_RUN=1` where it would mutate anything outside the runner, exits non-zero with a one-line `ERROR:` naming the variable or URL on a hard failure, and has at least one test in `run_tests.sh`. Python is stdlib only (`json`, `sys`, `os`, `re`, `datetime`).
- Never name a third-party astronomy application in code or comments (CLAUDE.md rule).
- Release notes, blog posts, commit messages and every user-visible string stay English.
- The GitHub CI job names the gate looks for are exactly `Build, test and typecheck` and `Build and test (Windows)` (`.github/workflows/ci.yml`).
- Pushes to either remote and the acceptance tag happen only on the owner's word (memory rule). Everything else in this plan — local commits, docs-repo local commits — proceeds without asking.
- Commit as `eg013ra1n <vilen.sharifov@gmail.com>` with the `Co-Authored-By`/`Claude-Session` trailers from the session reminder.

---

## File map

Athenaeum repo (`/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum`):

| Path | Responsibility |
| ---- | ---- |
| `.gitlab/ci/scripts/artifact_names.sh` | **Sourced library.** `artifact_name PRODUCT OS ARCH EXT [VARIANT]`, `alias_name …`, `all_release_artifacts VERSION` (one line per `product os arch ext variant filename`). The ONLY place a release filename is spelled. |
| `.gitlab/ci/scripts/check_versions.sh` | Reads the six version files, prints them, fails unless all six equal `TAG` (with the tauri `-beta.N → -N` mapping). |
| `scripts/release/bump.sh` | `bump.sh X.Y.Z[-beta.N]` rewrites the six files, refreshes `Cargo.lock`, runs `check_versions.sh`. |
| `.gitlab/ci/scripts/github_checks_gate.sh` | Polls GitHub check-runs for `CI_COMMIT_SHA`; exit 0 only when every required check concluded `success`. |
| `.gitlab/ci/scripts/verify_release.sh` | HEADs every installer URL, checks Docker Hub tags, polls the blog URL. Blocking. |
| `.gitlab/ci/scripts/create_gitlab_release.sh` | Builds the GitLab Release payload (notes + asset links from `artifact_names.sh`) with python, POSTs it. Replaces the inline curl JSON. |
| `.gitlab/ci/scripts/gen_release_post.sh` | `RELEASE_NOTES.md` on stdin → Starlight blog post with frontmatter on stdout. |
| `.gitlab/ci/scripts/update_download_page.sh` | Rewrites the marked Latest Build block and inserts/replaces the Version History row in the docs site's `download.md`. |
| `.gitlab/ci/scripts/docs_publish.sh` | Clones `artfrom-space` with a token, runs the two generators, commits, pushes `main`. |
| `.gitlab/ci/scripts/test/run_tests.sh` + `fixtures/` | Existing harness; gains a section per new script and new mock cases in `fixtures/mock_bin/curl`. |
| `.gitlab-ci.yml` | Stage graph `gate → build → deploy → publish → verify → announce`; jobs call the scripts. |
| `.github/workflows/ci.yml` | nextest + PR path filter (Task 8, measured). |
| `.claude/skills/release/SKILL.md` | The one release procedure. |
| `CLAUDE.md` | "Release workflow" section → pointer to the skill. |
| `crates/athenaeum-tauri/src/commands/core.rs` | `arch=` query parameter on the version-check GET (one line). |

Docs repo (`/Volumes/BigMac/Users/astrobureau/Documents/Projects/artfrom-space`):

| Path | Responsibility |
| ---- | ---- |
| `src/content/docs/releases/download.md` | Gains the three HTML-comment markers the generator needs. One-time edit. |

CI/CD variables the owner creates in GitLab (Settings → CI/CD → Variables, Masked + Protected) — the plan's tasks say when each is needed:

| Variable | Used by | Notes |
| ---- | ---- | ---- |
| `DOCS_REPO_TOKEN` | `docs:publish` | Project access token on `root/artfrom-space`, role Maintainer (its `main` is protected to Maintainers), scope `write_repository`. REQUIRED — the job fails loudly without it. |
| `GITHUB_API_TOKEN` | `gate:tests` | Optional. A fine-grained read-only token raises the API limit from 60/h; without it the gate polls every 60 s and still works. |
| `SSH_KNOWN_HOSTS` | `deploy`, `publish_version` | Optional. Output of `ssh-keyscan -p 40022 <DEPLOY_HOST>`; pins the host key (fail-closed) like the docs site's CI already does. |
| `RELEASE_ALLOW_AMD64_ONLY` | `verify:release` | Set to `1` only while the arm64 Docker runner is genuinely down; lets verify pass with the amd64-only image and a loud warning. Unset again afterwards. |

---

### Task 1: One artifact naming library

**Files:**
- Create: `.gitlab/ci/scripts/artifact_names.sh`
- Modify: `.gitlab/ci/scripts/test/run_tests.sh` (append a section)

**Interfaces:**
- Produces (sourced, bash functions):
  - `artifact_name PRODUCT OS ARCH EXT [VARIANT]` → prints `<product>-<VERSION>-<os>-<arch>[-<variant>].<ext>`; reads `VERSION` from the environment (no `v`).
  - `alias_name PRODUCT OS ARCH EXT [VARIANT]` → prints `<product>-<os>-<arch>[-<variant>].<ext>`.
  - `all_release_artifacts` → prints one line per shipped file: `product os arch ext variant subdir filename alias` (variant `-` when none; `subdir` ∈ `windows macos linux perseus`). This list is THE inventory: deploy renames to it, release links it, verify fetches it, the download page renders it.
  - `legacy_alias_pairs` → prints `subdir legacy_alias current_filename` per legacy link to keep until v0.7.0.

- [ ] **Step 1: Write the failing tests**

Append to `.gitlab/ci/scripts/test/run_tests.sh` just before the final `echo` / `Passed:` lines:

```bash
echo
echo "-- artifact_names.sh --"

# shellcheck disable=SC1091
. "$HELPERS_DIR/artifact_names.sh"

out=$(VERSION=0.6.4 artifact_name athenaeum macos arm64 dmg)
assert_eq "athenaeum dmg name" "athenaeum-0.6.4-macos-arm64.dmg" "$out"
out=$(VERSION=0.6.4-beta.1 artifact_name athenaeum windows x64 exe setup)
assert_eq "windows setup name with variant and beta" "athenaeum-0.6.4-beta.1-windows-x64-setup.exe" "$out"
out=$(VERSION=0.6.4 artifact_name perseus linux arm64 tar.gz)
assert_eq "perseus tarball name" "perseus-0.6.4-linux-arm64.tar.gz" "$out"
out=$(alias_name athenaeum linux x64 AppImage)
assert_eq "alias drops the version only" "athenaeum-linux-x64.AppImage" "$out"

rc=0
out=$(VERSION=0.6.4 artifact_name athenaeum linux amd64 deb 2>&1) || rc=$?
assert_eq "amd64 is refused as an arch" "1" "$rc"
assert_contains "refusal names the vocabulary" "arch must be x64 or arm64" "$out"

inventory=$(VERSION=0.6.4 all_release_artifacts)
assert_eq "inventory has 13 shipped files" "13" "$(printf '%s\n' "$inventory" | wc -l | tr -d ' ')"
assert_contains "inventory: msi" "athenaeum windows x64 msi - windows athenaeum-0.6.4-windows-x64.msi athenaeum-windows-x64.msi" "$inventory"
assert_contains "inventory: perseus x64 deb" "perseus linux x64 deb - perseus perseus-0.6.4-linux-x64.deb perseus-linux-x64.deb" "$inventory"
assert_not_contains "inventory never says amd64" "amd64" "$inventory"
assert_not_contains "inventory never says aarch64" "aarch64" "$inventory"
assert_not_contains "inventory never says x86_64" "x86_64" "$inventory"

legacy=$(VERSION=0.6.4 legacy_alias_pairs)
assert_contains "legacy: old macOS alias maps to the new file" "macos Athenaeum-macos-aarch64.dmg athenaeum-0.6.4-macos-arm64.dmg" "$legacy"
assert_contains "legacy: old perseus deb alias" "perseus perseus-linux-amd64.deb perseus-0.6.4-linux-x64.deb" "$legacy"
```

- [ ] **Step 2: Run the harness to verify it fails**

Run: `.gitlab/ci/scripts/test/run_tests.sh 2>&1 | tail -20`
Expected: the new section errors with `No such file or directory` for `artifact_names.sh` (the `.` source line aborts under `set -e`), i.e. the run does not reach `Passed:`.

- [ ] **Step 3: Write the library**

Create `.gitlab/ci/scripts/artifact_names.sh`:

```bash
#!/usr/bin/env bash
# The ONE place a release filename is spelled. Source this file; do not run it.
#
#   <product>-<version>-<os>-<arch>[-<variant>].<ext>
#
# product: athenaeum | perseus (lower-case)      version: no leading "v"
# os:      macos | windows | linux               arch:    x64 | arm64 — nothing else
#
# `amd64`, `x86_64` and `aarch64` are Debian's, Linux's and Apple's words for
# the same two things; they never appear in a filename we publish (they may
# still appear INSIDE a .deb's control file — that is Debian's business).
# Reads VERSION from the environment. Used by the deploy job (rename + alias),
# the GitLab Release links, verify_release.sh and the download-page generator.

_check_vocab() {
  case "$1" in athenaeum|perseus) ;; *) echo "ERROR: product must be athenaeum or perseus, got '$1'" >&2; return 1 ;; esac
  case "$2" in macos|windows|linux) ;; *) echo "ERROR: os must be macos, windows or linux, got '$2'" >&2; return 1 ;; esac
  case "$3" in x64|arm64) ;; *) echo "ERROR: arch must be x64 or arm64, got '$3'" >&2; return 1 ;; esac
}

# artifact_name PRODUCT OS ARCH EXT [VARIANT]
artifact_name() {
  local product="$1" os="$2" arch="$3" ext="$4" variant="${5:-}"
  _check_vocab "$product" "$os" "$arch" || return 1
  : "${VERSION:?VERSION must be set (no leading v)}"
  if [ -n "$variant" ]; then
    printf '%s-%s-%s-%s-%s.%s\n' "$product" "$VERSION" "$os" "$arch" "$variant" "$ext"
  else
    printf '%s-%s-%s-%s.%s\n' "$product" "$VERSION" "$os" "$arch" "$ext"
  fi
}

# alias_name PRODUCT OS ARCH EXT [VARIANT] — the version-less stable link.
alias_name() {
  local product="$1" os="$2" arch="$3" ext="$4" variant="${5:-}"
  _check_vocab "$product" "$os" "$arch" || return 1
  if [ -n "$variant" ]; then
    printf '%s-%s-%s-%s.%s\n' "$product" "$os" "$arch" "$variant" "$ext"
  else
    printf '%s-%s-%s.%s\n' "$product" "$os" "$arch" "$ext"
  fi
}

# all_release_artifacts — the inventory of every file a release publishes.
# One line each: product os arch ext variant subdir filename alias
# (variant is "-" when there is none). Order = the download page's order.
all_release_artifacts() {
  : "${VERSION:?VERSION must be set (no leading v)}"
  local spec product os arch ext variant subdir
  # product os arch ext variant subdir
  for spec in \
    "athenaeum windows x64 msi - windows" \
    "athenaeum windows x64 exe setup windows" \
    "athenaeum macos arm64 dmg - macos" \
    "athenaeum macos x64 dmg - macos" \
    "athenaeum linux x64 AppImage - linux" \
    "athenaeum linux x64 deb - linux" \
    "perseus macos arm64 dmg - perseus" \
    "perseus macos x64 dmg - perseus" \
    "perseus windows x64 exe setup perseus" \
    "perseus linux x64 deb - perseus" \
    "perseus linux x64 tar.gz - perseus" \
    "perseus linux arm64 deb - perseus" \
    "perseus linux arm64 tar.gz - perseus"; do
    read -r product os arch ext variant subdir <<< "$spec"
    local v="" ; [ "$variant" != "-" ] && v="$variant"
    printf '%s %s %s %s %s %s %s %s\n' "$product" "$os" "$arch" "$ext" "$variant" "$subdir" \
      "$(artifact_name "$product" "$os" "$arch" "$ext" "$v")" \
      "$(alias_name "$product" "$os" "$arch" "$ext" "$v")"
  done
}

# legacy_alias_pairs — old alias spellings kept as extra symlinks until
# v0.7.0 so links published before the rename keep resolving.
# One line each: subdir legacy_alias current_filename
legacy_alias_pairs() {
  : "${VERSION:?VERSION must be set (no leading v)}"
  printf 'windows Athenaeum-windows-x64.msi %s\n'        "$(artifact_name athenaeum windows x64 msi)"
  printf 'windows Athenaeum-windows-x64-setup.exe %s\n'  "$(artifact_name athenaeum windows x64 exe setup)"
  printf 'macos Athenaeum-macos-aarch64.dmg %s\n'        "$(artifact_name athenaeum macos arm64 dmg)"
  printf 'macos Athenaeum-macos-x64.dmg %s\n'            "$(artifact_name athenaeum macos x64 dmg)"
  printf 'linux athenaeum-linux-x86_64.AppImage %s\n'    "$(artifact_name athenaeum linux x64 AppImage)"
  printf 'linux athenaeum-linux-amd64.deb %s\n'          "$(artifact_name athenaeum linux x64 deb)"
  printf 'perseus perseus-linux-amd64.deb %s\n'          "$(artifact_name perseus linux x64 deb)"
}
```

- [ ] **Step 4: Run the harness to verify it passes**

Run: `.gitlab/ci/scripts/test/run_tests.sh 2>&1 | tail -25`
Expected: every `artifact_names.sh` assertion prints `ok:`; the final line is `Passed: N  Failed: 0`.

- [ ] **Step 5: Commit**

```bash
git add .gitlab/ci/scripts/artifact_names.sh .gitlab/ci/scripts/test/run_tests.sh
git commit -m "ci: one artifact naming library — <product>-<version>-<os>-<arch>, x64/arm64 only"
```

---

### Task 2: Version consistency — `check_versions.sh` and `bump.sh`

**Files:**
- Create: `.gitlab/ci/scripts/check_versions.sh`
- Create: `scripts/release/bump.sh`
- Create: `.gitlab/ci/scripts/test/fixtures/version_tree/` (six minimal files, see Step 1)
- Modify: `.gitlab/ci/scripts/test/run_tests.sh` (append a section)

**Interfaces:**
- `check_versions.sh`: env `TAG` (e.g. `v0.6.4-beta.1`), optional `REPO_ROOT` (default: the repo the script lives in). Prints one `file: version` line per file, then `ok: all six versions match <version>` or `ERROR: … expected <x> got <y>` and exit 1. Later tasks: the `check:versions` job (Task 6) runs it with `TAG=$CI_COMMIT_TAG`; `bump.sh` runs it after rewriting.
- `bump.sh NEW_VERSION`: env `REPO_ROOT` (default: repo root), `BUMP_SKIP_CARGO=1` skips the `Cargo.lock` refresh (tests). Rewrites `package.json`, `crates/{athenaeum-core,athenaeum-tauri,athenaeum-web,perseus}/Cargo.toml`, `crates/athenaeum-tauri/tauri.conf.json` (tauri form: `-beta.N` → `-N`).

- [ ] **Step 1: Create the fixture tree and write the failing tests**

Create the six fixture files (minimal, real shapes):

`.gitlab/ci/scripts/test/fixtures/version_tree/package.json`
```json
{
  "name": "athenaeum",
  "private": true,
  "version": "0.6.3",
  "type": "module"
}
```

`.gitlab/ci/scripts/test/fixtures/version_tree/crates/athenaeum-core/Cargo.toml` (and the same shape for `athenaeum-tauri`, `athenaeum-web`, `perseus`, each with its own `name`):
```toml
[package]
name = "athenaeum-core"
version = "0.6.3"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
```

`.gitlab/ci/scripts/test/fixtures/version_tree/crates/athenaeum-tauri/tauri.conf.json`
```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Athenaeum",
  "version": "0.6.3",
  "identifier": "com.vsharifov.athenaeum"
}
```

Append to `run_tests.sh`:

```bash
echo
echo "-- check_versions.sh / bump.sh --"

VT_TMP=$(mktemp -d -t version_tree.XXXXXX)
cp -R "$FIXTURES_DIR/version_tree/." "$VT_TMP/"

# Matching tag passes and lists all six.
rc=0
out=$(REPO_ROOT="$VT_TMP" TAG="v0.6.3" "$HELPERS_DIR/check_versions.sh" 2>&1) || rc=$?
assert_eq "check: matching tag exits 0" "0" "$rc"
assert_contains "check: lists package.json" "package.json: 0.6.3" "$out"
assert_contains "check: lists perseus" "crates/perseus/Cargo.toml: 0.6.3" "$out"
assert_contains "check: success line" "ok: all six versions match 0.6.3" "$out"

# Mismatching tag fails and names the file.
rc=0
out=$(REPO_ROOT="$VT_TMP" TAG="v0.6.4" "$HELPERS_DIR/check_versions.sh" 2>&1) || rc=$?
assert_eq "check: mismatch exits 1" "1" "$rc"
assert_contains "check: mismatch names a file" "ERROR: package.json is 0.6.3, tag v0.6.4 expects 0.6.4" "$out"

# bump rewrites all six (tauri gets the -N form for a beta) and re-checks.
rc=0
out=$(REPO_ROOT="$VT_TMP" BUMP_SKIP_CARGO=1 "$REPO_DIR/scripts/release/bump.sh" 0.7.0-beta.2 2>&1) || rc=$?
assert_eq "bump: exits 0" "0" "$rc"
assert_contains "bump: package.json rewritten" '"version": "0.7.0-beta.2"' "$(cat "$VT_TMP/package.json")"
assert_contains "bump: perseus Cargo.toml rewritten" 'version = "0.7.0-beta.2"' "$(cat "$VT_TMP/crates/perseus/Cargo.toml")"
assert_not_contains "bump: dependency version lines untouched" 'serde = { version = "0.7.0-beta.2"' "$(cat "$VT_TMP/crates/perseus/Cargo.toml")"
assert_contains "bump: tauri.conf gets the -N form" '"version": "0.7.0-2"' "$(cat "$VT_TMP/crates/athenaeum-tauri/tauri.conf.json")"
assert_contains "bump: re-check passes" "ok: all six versions match 0.7.0-beta.2" "$out"

# A malformed version is refused before touching anything.
rc=0
out=$(REPO_ROOT="$VT_TMP" BUMP_SKIP_CARGO=1 "$REPO_DIR/scripts/release/bump.sh" 0.7 2>&1) || rc=$?
assert_eq "bump: malformed version exits 1" "1" "$rc"
assert_contains "bump: malformed version message" "ERROR: version must look like X.Y.Z or X.Y.Z-beta.N" "$out"
assert_contains "bump: nothing rewritten on refusal" '"version": "0.7.0-beta.2"' "$(cat "$VT_TMP/package.json")"

rm -rf "$VT_TMP"
```

Add near the top of `run_tests.sh`, after `HELPERS_DIR=…`:

```bash
REPO_DIR="$(cd "$HELPERS_DIR/../../.." && pwd)"
```

- [ ] **Step 2: Run the harness to verify it fails**

Run: `.gitlab/ci/scripts/test/run_tests.sh 2>&1 | grep -A3 "check_versions"`
Expected: `check_versions.sh: No such file or directory`, the section's assertions FAIL.

- [ ] **Step 3: Write `check_versions.sh`**

```bash
#!/usr/bin/env bash
# Assert that the six version files agree with a release tag.
# Env: TAG (required, e.g. v0.6.4 or v0.6.4-beta.1); REPO_ROOT (optional).
# tauri.conf.json carries X.Y.Z-N for a beta (its bundler rejects the dotted
# form), so the tag's -beta.N maps to -N there and nowhere else.
set -euo pipefail

: "${TAG:?TAG must be set (the release tag, with its leading v)}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../../.." && pwd)}"

VERSION="${TAG#v}"
TAURI_VERSION="$(printf '%s' "$VERSION" | sed -E 's/-beta\.([0-9]+)$/-\1/')"

read_json_version() {  # $1 = file — the top-level "version" key
  python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$1"
}
read_cargo_version() { # $1 = file — the first `version = "…"` line (the [package] one)
  sed -nE 's/^version = "([^"]+)".*/\1/p' "$1" | head -n1
}

fail=0
check() { # $1 = relative file, $2 = actual, $3 = expected
  echo "$1: $2"
  if [ "$2" != "$3" ]; then
    echo "ERROR: $1 is $2, tag $TAG expects $3" >&2
    fail=1
  fi
}

check "package.json" "$(read_json_version "$REPO_ROOT/package.json")" "$VERSION"
for crate in athenaeum-core athenaeum-tauri athenaeum-web perseus; do
  check "crates/$crate/Cargo.toml" "$(read_cargo_version "$REPO_ROOT/crates/$crate/Cargo.toml")" "$VERSION"
done
check "crates/athenaeum-tauri/tauri.conf.json" "$(read_json_version "$REPO_ROOT/crates/athenaeum-tauri/tauri.conf.json")" "$TAURI_VERSION"

if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "ok: all six versions match $VERSION"
```

- [ ] **Step 4: Write `bump.sh`**

```bash
#!/usr/bin/env bash
# Bump the release version in the six files that carry it, refresh Cargo.lock,
# and prove the six agree. Usage: scripts/release/bump.sh X.Y.Z[-beta.N]
# Env: REPO_ROOT (optional), BUMP_SKIP_CARGO=1 (skip the Cargo.lock refresh; tests).
set -euo pipefail

NEW="${1:-}"
if ! printf '%s' "$NEW" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$'; then
  echo "ERROR: version must look like X.Y.Z or X.Y.Z-beta.N (got '${NEW}')" >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TAURI_NEW="$(printf '%s' "$NEW" | sed -E 's/-beta\.([0-9]+)$/-\1/')"

# Line-level rewrites keep each file's formatting byte-for-byte except the
# version itself: the FIRST top-level "version" key in the JSON files (line 5
# in package.json, line 4 in tauri.conf.json) and the FIRST `version = "…"`
# line in each Cargo.toml (the [package] one; dependency tables come later).
rewrite_first() { # $1 = file, $2 = python regex with one group for the value, $3 = new value
  python3 - "$1" "$2" "$3" <<'PY'
import re, sys
path, pattern, new = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path, encoding="utf-8").read()
rx = re.compile(pattern, re.M)
m = rx.search(text)
if not m:
    sys.exit(f"ERROR: no version line matched {pattern!r} in {path}")
text = text[:m.start(1)] + new + text[m.end(1):]
open(path, "w", encoding="utf-8").write(text)
PY
}

rewrite_first "$REPO_ROOT/package.json"                              '^  "version": "([^"]+)"' "$NEW"
for crate in athenaeum-core athenaeum-tauri athenaeum-web perseus; do
  rewrite_first "$REPO_ROOT/crates/$crate/Cargo.toml"                '^version = "([^"]+)"'     "$NEW"
done
rewrite_first "$REPO_ROOT/crates/athenaeum-tauri/tauri.conf.json"    '^  "version": "([^"]+)"' "$TAURI_NEW"

if [ "${BUMP_SKIP_CARGO:-0}" != "1" ]; then
  # cargo metadata re-resolves the workspace and rewrites Cargo.lock's own
  # package entries — the same effect as `cargo check`, without compiling.
  (cd "$REPO_ROOT" && cargo metadata --format-version 1 --quiet > /dev/null)
fi

TAG="v$NEW" REPO_ROOT="$REPO_ROOT" "$REPO_ROOT/.gitlab/ci/scripts/check_versions.sh"
```

`chmod +x` both scripts.

- [ ] **Step 5: Run the harness to verify it passes**

Run: `.gitlab/ci/scripts/test/run_tests.sh 2>&1 | tail -20`
Expected: all `check:` and `bump:` assertions `ok:`, `Failed: 0`.

- [ ] **Step 6: Run `check_versions.sh` against the real tree**

Run: `TAG=v0.6.3 .gitlab/ci/scripts/check_versions.sh`
Expected: six lines ending `: 0.6.3` and `ok: all six versions match 0.6.3`.

- [ ] **Step 7: Commit**

```bash
git add .gitlab/ci/scripts/check_versions.sh scripts/release/bump.sh .gitlab/ci/scripts/test/
git commit -m "ci: check_versions.sh asserts tag == the six version files; scripts/release/bump.sh rewrites them"
```

---

### Task 3: Docs-site generators — blog post, download page, publish

**Files:**
- Create: `.gitlab/ci/scripts/gen_release_post.sh`
- Create: `.gitlab/ci/scripts/update_download_page.sh`
- Create: `.gitlab/ci/scripts/docs_publish.sh`
- Create: `.gitlab/ci/scripts/test/fixtures/release_notes_sample.md`, `.gitlab/ci/scripts/test/fixtures/download_page_sample.md`
- Modify: `.gitlab/ci/scripts/test/run_tests.sh`
- Modify (docs repo): `../artfrom-space/src/content/docs/releases/download.md` — add markers

**Interfaces:**
- `gen_release_post.sh`: stdin = `RELEASE_NOTES.md`; env `TAG`, `RELEASE_DATE` (YYYY-MM-DD; default today UTC). stdout = the blog post. The tagline (first non-empty line, `*…*`) becomes `excerpt`; the body is everything after it.
- `update_download_page.sh`: env `PAGE` (path), `TAG`, `RELEASE_DATE`, `TAGLINE`, `BUILDS_BASE_URL` (default `https://artfrom.space/builds`). Requires the markers `<!-- latest-build:start -->`, `<!-- latest-build:end -->`, `<!-- version-history:rows -->` in the page; fails naming the missing one. A beta tag updates only the Version History.
- `docs_publish.sh`: env `DOCS_REPO_URL` (default `http://192.168.31.208:9080/root/artfrom-space.git`), `DOCS_REPO_TOKEN` (required unless `DRY_RUN=1`), `TAG`, `RELEASE_NOTES_PATH` (default `RELEASE_NOTES.md`), `RELEASE_DATE`, `DOCS_GIT_NAME`/`DOCS_GIT_EMAIL` (defaults `eg013ra1n` / `vilen.sharifov@gmail.com`), `DRY_RUN=1` = clone from `DOCS_REPO_LOCAL` (a local path) instead and print the diff instead of pushing. Prints `docs: nothing to publish` and exits 0 when a re-run finds no change.
- The tagline extraction is shared: `gen_release_post.sh --tagline` prints only the tagline (used by `docs_publish.sh` for the Version History row and by Task 5's `create_gitlab_release.sh`).

- [ ] **Step 1: Fixtures and failing tests**

`.gitlab/ci/scripts/test/fixtures/release_notes_sample.md`:
```markdown
*Athenaeum v0.7.0: a "quoted" tagline — with a dash & an ampersand.*

## What's New

- **First thing.** It does this.

## Bug Fixes

- **Second thing.** It no longer does that.
```

`.gitlab/ci/scripts/test/fixtures/download_page_sample.md`:
```markdown
---
title: Download
---

## Latest Build

Direct downloads of the most recent stable release.

<!-- latest-build:start -->
old block that must be replaced
<!-- latest-build:end -->

## Version History

| Version | Date | Notes |
| ---- | ---- | ---- |
<!-- version-history:rows -->
| v0.6.3 | 2026-09-15 | old row |
```

Append to `run_tests.sh`:

```bash
echo
echo "-- gen_release_post.sh --"

out=$(TAG=v0.7.0 RELEASE_DATE=2026-10-01 "$HELPERS_DIR/gen_release_post.sh" < "$FIXTURES_DIR/release_notes_sample.md")
assert_contains "post: title" "title: Athenaeum v0.7.0" "$out"
assert_contains "post: date" "date: 2026-10-01" "$out"
assert_contains "post: author" "  - vilen" "$out"
assert_contains "post: tag" "  - release" "$out"
assert_contains "post: excerpt is the tagline without stars, YAML-quoted" 'excerpt: "Athenaeum v0.7.0: a \"quoted\" tagline — with a dash & an ampersand."' "$out"
assert_not_contains "post: body does not repeat the tagline" "*Athenaeum v0.7.0: a" "$out"
assert_contains "post: body keeps the sections" "## Bug Fixes" "$out"
first_body_line=$(printf '%s\n' "$out" | awk 'f&&NF{print;exit} /^---$/{c++; if(c==2)f=1}')
assert_eq "post: body starts at the first heading" "## What's New" "$first_body_line"
out=$("$HELPERS_DIR/gen_release_post.sh" --tagline < "$FIXTURES_DIR/release_notes_sample.md")
assert_eq "post: --tagline prints the bare tagline" 'Athenaeum v0.7.0: a "quoted" tagline — with a dash & an ampersand.' "$out"

echo
echo "-- update_download_page.sh --"

DP_TMP=$(mktemp -t download_page.XXXXXX)
cp "$FIXTURES_DIR/download_page_sample.md" "$DP_TMP"
rc=0
out=$(PAGE="$DP_TMP" TAG=v0.7.0 RELEASE_DATE=2026-10-01 TAGLINE="A tagline | with a pipe" "$HELPERS_DIR/update_download_page.sh" 2>&1) || rc=$?
assert_eq "page: stable update exits 0" "0" "$rc"
page=$(cat "$DP_TMP")
assert_not_contains "page: old block gone" "old block that must be replaced" "$page"
assert_contains "page: versioned msi link" "[athenaeum-0.7.0-windows-x64.msi](https://artfrom.space/builds/v0.7.0/windows/athenaeum-0.7.0-windows-x64.msi)" "$page"
assert_contains "page: versioned arm64 dmg link" "[athenaeum-0.7.0-macos-arm64.dmg](https://artfrom.space/builds/v0.7.0/macos/athenaeum-0.7.0-macos-arm64.dmg)" "$page"
assert_contains "page: AppImage link" "builds/v0.7.0/linux/athenaeum-0.7.0-linux-x64.AppImage" "$page"
assert_contains "page: markers survive" "<!-- latest-build:end -->" "$page"
assert_contains "page: history row inserted with the pipe escaped" "| v0.7.0 | 2026-10-01 | A tagline \| with a pipe |" "$page"
assert_contains "page: old row kept" "| v0.6.3 | 2026-09-15 | old row |" "$page"
# newest row directly under the marker
after_marker=$(grep -A1 -F '<!-- version-history:rows -->' "$DP_TMP" | tail -n1)
assert_contains "page: new row is first" "| v0.7.0 |" "$after_marker"

# Same tag again: the row is replaced, not duplicated.
PAGE="$DP_TMP" TAG=v0.7.0 RELEASE_DATE=2026-10-01 TAGLINE="Second wording" "$HELPERS_DIR/update_download_page.sh" >/dev/null 2>&1
assert_eq "page: re-run keeps one v0.7.0 row" "1" "$(grep -c '^| v0.7.0 |' "$DP_TMP")"
assert_contains "page: re-run row carries the new wording" "| v0.7.0 | 2026-10-01 | Second wording |" "$(cat "$DP_TMP")"

# Beta: history row only, Latest Build untouched.
before_block=$(sed -n '/latest-build:start/,/latest-build:end/p' "$DP_TMP")
PAGE="$DP_TMP" TAG=v0.7.1-beta.1 RELEASE_DATE=2026-10-02 TAGLINE="Beta" "$HELPERS_DIR/update_download_page.sh" >/dev/null 2>&1
after_block=$(sed -n '/latest-build:start/,/latest-build:end/p' "$DP_TMP")
assert_eq "page: beta leaves Latest Build alone" "$before_block" "$after_block"
assert_contains "page: beta row added" "| v0.7.1-beta.1 | 2026-10-02 | Beta |" "$(cat "$DP_TMP")"

# Missing marker is a loud failure.
printf '# no markers\n' > "$DP_TMP"
rc=0
out=$(PAGE="$DP_TMP" TAG=v0.7.0 RELEASE_DATE=2026-10-01 TAGLINE="x" "$HELPERS_DIR/update_download_page.sh" 2>&1) || rc=$?
assert_eq "page: missing marker exits 1" "1" "$rc"
assert_contains "page: names the marker" "ERROR: marker <!-- latest-build:start --> not found" "$out"
rm -f "$DP_TMP"

echo
echo "-- docs_publish.sh (dry run against a local clone) --"

DOCS_TMP=$(mktemp -d -t docs_repo.XXXXXX)
git -C "$DOCS_TMP" init -q -b main
mkdir -p "$DOCS_TMP/src/content/docs/blog" "$DOCS_TMP/src/content/docs/releases"
cp "$FIXTURES_DIR/download_page_sample.md" "$DOCS_TMP/src/content/docs/releases/download.md"
git -C "$DOCS_TMP" -c user.name=t -c user.email=t@t add -A
git -C "$DOCS_TMP" -c user.name=t -c user.email=t@t commit -q -m init
rc=0
out=$(DRY_RUN=1 DOCS_REPO_LOCAL="$DOCS_TMP" TAG=v0.7.0 RELEASE_DATE=2026-10-01 RELEASE_NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/docs_publish.sh" 2>&1) || rc=$?
assert_eq "publish: dry run exits 0" "0" "$rc"
assert_contains "publish: would commit the post" "src/content/docs/blog/v0.7.0.md" "$out"
assert_contains "publish: would commit the page" "src/content/docs/releases/download.md" "$out"
assert_contains "publish: commit message" "docs: v0.7.0 release post + download-page row" "$out"
assert_contains "publish: dry run does not push" "DRY_RUN=1: not pushing" "$out"
rm -rf "$DOCS_TMP"
```

- [ ] **Step 2: Run the harness to verify it fails**

Run: `.gitlab/ci/scripts/test/run_tests.sh 2>&1 | grep -E "gen_release_post|update_download|docs_publish" -A2 | head`
Expected: `No such file or directory` for each of the three scripts.

- [ ] **Step 3: Write `gen_release_post.sh`**

```bash
#!/usr/bin/env bash
# RELEASE_NOTES.md (stdin) -> a Starlight blog post (stdout).
# Env: TAG (required unless --tagline), RELEASE_DATE (YYYY-MM-DD, default today UTC).
# `--tagline` prints only the bare tagline (first non-empty line, the italic
# *…* stripped) — the one sentence the Version History row and the GitLab
# Release description reuse.
set -euo pipefail

MODE="${1:-post}"
python3 - "$MODE" "${TAG:-}" "${RELEASE_DATE:-}" <<'PY'
import datetime, json, sys
mode, tag, date = sys.argv[1], sys.argv[2], sys.argv[3]
lines = sys.stdin.read().splitlines()

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
```

- [ ] **Step 4: Write `update_download_page.sh`**

```bash
#!/usr/bin/env bash
# Rewrite the marked "Latest Build" block and insert/replace the Version
# History row of the docs site's download page.
# Env: PAGE (path), TAG, RELEASE_DATE, TAGLINE, BUILDS_BASE_URL (default
# https://artfrom.space/builds). A beta tag touches only the history row.
# Markers the page must carry:
#   <!-- latest-build:start -->  …  <!-- latest-build:end -->
#   <!-- version-history:rows -->   (the line right under the table header)
set -euo pipefail

: "${PAGE:?PAGE must be set}"; : "${TAG:?TAG must be set}"; : "${RELEASE_DATE:?RELEASE_DATE must be set}"; : "${TAGLINE:?TAGLINE must be set}"
BUILDS_BASE_URL="${BUILDS_BASE_URL:-https://artfrom.space/builds}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/artifact_names.sh"
VERSION="${TAG#v}"
export VERSION

for marker in '<!-- latest-build:start -->' '<!-- latest-build:end -->' '<!-- version-history:rows -->'; do
  if ! grep -qF -- "$marker" "$PAGE"; then
    echo "ERROR: marker $marker not found in $PAGE" >&2
    exit 1
  fi
done

link() { # $1 = subdir, $2 = filename
  printf '[%s](%s/%s/%s/%s)' "$2" "$BUILDS_BASE_URL" "$TAG" "$1" "$2"
}
n() { artifact_name "$@"; }

block_file=$(mktemp -t latest_build.XXXXXX)
trap 'rm -f "$block_file"' EXIT
cat > "$block_file" <<EOF
### Windows

| Download | Architecture | Format |
| ---- | ---- | ---- |
| $(link windows "$(n athenaeum windows x64 msi)") | x64 (Intel / AMD) | MSI installer (recommended) |
| $(link windows "$(n athenaeum windows x64 exe setup)") | x64 (Intel / AMD) | EXE setup |

### macOS

| Download | Architecture | Format |
| ---- | ---- | ---- |
| $(link macos "$(n athenaeum macos arm64 dmg)") | Apple Silicon (M1 / M2 / M3 / M4) | DMG |
| $(link macos "$(n athenaeum macos x64 dmg)") | Intel (x86_64) | DMG |

Not sure which Mac you have? Apple menu → **About This Mac**: "Chip: Apple M…" → Apple Silicon; "Processor: Intel…" → Intel.

:::tip
macOS builds are code-signed with a Developer ID certificate and notarized by Apple — open the DMG, drag **Athenaeum** to Applications, and launch. No security workaround needed.
:::

### Linux

| Download | Architecture | Format |
| ---- | ---- | ---- |
| $(link linux "$(n athenaeum linux x64 AppImage)") | x64 | AppImage (any distro) |
| $(link linux "$(n athenaeum linux x64 deb)") | x64 | Debian / Ubuntu package |

:::tip
Every release is also archived by tag — [browse all builds](${BUILDS_BASE_URL}/) for older versions or to verify a specific release. Version-less links such as \`${BUILDS_BASE_URL}/latest/macos/$(alias_name athenaeum macos arm64 dmg)\` always point at the newest stable build.
:::
EOF

python3 - "$PAGE" "$TAG" "$RELEASE_DATE" "$TAGLINE" "$block_file" <<'PY'
import re, sys
page, tag, date, tagline, block_path = sys.argv[1:6]
text = open(page, encoding="utf-8").read()
is_beta = "-beta" in tag

if not is_beta:
    block = open(block_path, encoding="utf-8").read().rstrip("\n")
    start, end = "<!-- latest-build:start -->", "<!-- latest-build:end -->"
    a, b = text.index(start) + len(start), text.index(end)
    text = text[:a] + "\n" + block + "\n" + text[b:]

row = f"| {tag} | {date} | {tagline.replace('|', chr(92) + '|')} |"
lines = text.split("\n")
lines = [l for l in lines if not l.startswith(f"| {tag} |")]   # replace an existing row for this tag
marker = "<!-- version-history:rows -->"
idx = lines.index(marker)
lines.insert(idx + 1, row)
open(page, "w", encoding="utf-8").write("\n".join(lines))
print(f"updated {page}: history row for {tag}" + ("" if is_beta else " + Latest Build block"))
PY
```

- [ ] **Step 5: Write `docs_publish.sh`**

```bash
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
```

`chmod +x` all three.

- [ ] **Step 6: Run the harness to verify it passes**

Run: `.gitlab/ci/scripts/test/run_tests.sh 2>&1 | tail -40`
Expected: every `post:`, `page:` and `publish:` assertion `ok:`, `Failed: 0`.

- [ ] **Step 7: Add the markers to the real download page (docs repo)**

In `../artfrom-space/src/content/docs/releases/download.md`:
- insert `<!-- latest-build:start -->` on its own line right after the paragraph `Direct downloads of the most recent stable release — these links always point to the newest version.` and `<!-- latest-build:end -->` on its own line right before `## Beta Builds`; rewrite that paragraph to `Direct downloads of the most recent stable release.`
- insert `<!-- version-history:rows -->` on its own line directly under `| ---- | ---- | ---- |` in the Version History table.

Then dry-run the real generator against it to see what a real release would produce:

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
DRY_RUN=1 DOCS_REPO_LOCAL=../artfrom-space TAG=v0.6.3 RELEASE_DATE=2026-09-15 .gitlab/ci/scripts/docs_publish.sh
```
Expected: a commit stat naming `blog/v0.6.3.md` and `download.md`, `DRY_RUN=1: not pushing` (the existing hand-written v0.6.3 post gets replaced in the throwaway clone — that is the "one text" decision made visible; nothing touches the real docs repo).

Commit the docs repo locally:
```bash
git -C ../artfrom-space add src/content/docs/releases/download.md
git -C ../artfrom-space commit -m "docs: markers for the release pipeline's download-page generator"
```
(Push on the owner's word — Task 9.)

- [ ] **Step 8: Commit (athenaeum)**

```bash
git add .gitlab/ci/scripts/gen_release_post.sh .gitlab/ci/scripts/update_download_page.sh .gitlab/ci/scripts/docs_publish.sh .gitlab/ci/scripts/test/
git commit -m "ci: the docs site is generated from RELEASE_NOTES.md — blog post, download page, one commit on main"
```

---

### Task 4: `gate:tests` — the tag waits for the GitHub check-suite

**Files:**
- Create: `.gitlab/ci/scripts/github_checks_gate.sh`
- Create: `.gitlab/ci/scripts/test/fixtures/github_checks_{success,failure,pending}.json`
- Modify: `.gitlab/ci/scripts/test/fixtures/mock_bin/curl` (new URL case)
- Modify: `.gitlab/ci/scripts/test/run_tests.sh`

**Interfaces:**
- `github_checks_gate.sh`: env `CI_COMMIT_SHA` (required), `GITHUB_REPO` (default `eg013ra1n/athenaeum`), `GITHUB_API_TOKEN` (optional), `GATE_REQUIRED_CHECKS` (default `Build, test and typecheck;Build and test (Windows)`, `;`-separated), `GATE_TIMEOUT_SECONDS` (default 3000), `GATE_POLL_SECONDS` (default 60; 20 with a token), `GATE_INITIAL_WAIT_SECONDS` (default 600 — how long "no check runs yet" is tolerated). Exit 0 when every required check has `status == completed` and `conclusion == success`; exit 1 naming the check on `failure|cancelled|timed_out|action_required`, or on timeout.
- Mock `curl` case `*api.github.com/repos/*/check-runs*`: `MOCK_CURL_GITHUB_HTTP` (default 200), `MOCK_CURL_GITHUB_BODY_FILE`, and `MOCK_CURL_GITHUB_SEQUENCE_DIR` — when set, the Nth call answers with `<dir>/<N>.json` (a counter in `MOCK_CURL_COUNTER_FILE`), the last file repeating.

- [ ] **Step 1: Fixtures, mock case, failing tests**

`github_checks_success.json`:
```json
{"total_count": 2, "check_runs": [
  {"name": "Build, test and typecheck", "status": "completed", "conclusion": "success"},
  {"name": "Build and test (Windows)", "status": "completed", "conclusion": "success"}
]}
```
`github_checks_failure.json`: same shape, the Windows run `"conclusion": "failure"`.
`github_checks_pending.json`: same shape, the Windows run `"status": "in_progress", "conclusion": null`.

Add to `fixtures/mock_bin/curl`, inside the `case "$url" in` before the `*)` arm:

```bash
  *api.github.com/repos/*/check-runs*)
    # github_checks_gate.sh. MOCK_CURL_GITHUB_HTTP (200), MOCK_CURL_GITHUB_BODY_FILE,
    # or MOCK_CURL_GITHUB_SEQUENCE_DIR + MOCK_CURL_COUNTER_FILE: call N answers <dir>/N.json,
    # the highest-numbered file repeating once the sequence is exhausted.
    http_code="${MOCK_CURL_GITHUB_HTTP:-200}"
    body_file="${MOCK_CURL_GITHUB_BODY_FILE:-}"
    if [ -n "${MOCK_CURL_GITHUB_SEQUENCE_DIR:-}" ]; then
      counter_file="${MOCK_CURL_COUNTER_FILE:?MOCK_CURL_COUNTER_FILE must be set with a sequence dir}"
      n=$(( $(cat "$counter_file" 2>/dev/null || echo 0) + 1 ))
      printf '%s' "$n" > "$counter_file"
      while [ "$n" -gt 1 ] && [ ! -f "$MOCK_CURL_GITHUB_SEQUENCE_DIR/$n.json" ]; do n=$((n - 1)); done
      body_file="$MOCK_CURL_GITHUB_SEQUENCE_DIR/$n.json"
    fi
    if [ -n "$body_file" ] && [ -f "$body_file" ]; then cp "$body_file" "$output_file"; else printf '{"total_count":0,"check_runs":[]}' > "$output_file"; fi
    ;;
```

Append to `run_tests.sh`:

```bash
echo
echo "-- github_checks_gate.sh --"

rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_GITHUB_BODY_FILE="$FIXTURES_DIR/github_checks_success.json" \
  CI_COMMIT_SHA=deadbeef GATE_POLL_SECONDS=0 "$HELPERS_DIR/github_checks_gate.sh" 2>&1) || rc=$?
assert_eq "gate: all green exits 0" "0" "$rc"
assert_contains "gate: reports both checks" "ok: Build and test (Windows) — success" "$out"

rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_GITHUB_BODY_FILE="$FIXTURES_DIR/github_checks_failure.json" \
  CI_COMMIT_SHA=deadbeef GATE_POLL_SECONDS=0 "$HELPERS_DIR/github_checks_gate.sh" 2>&1) || rc=$?
assert_eq "gate: a failed check exits 1" "1" "$rc"
assert_contains "gate: names the failed check" "ERROR: Build and test (Windows) concluded failure" "$out"

# pending, then success — the gate polls and passes.
SEQ_TMP=$(mktemp -d -t gate_seq.XXXXXX)
cp "$FIXTURES_DIR/github_checks_pending.json" "$SEQ_TMP/1.json"
cp "$FIXTURES_DIR/github_checks_success.json" "$SEQ_TMP/2.json"
rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_GITHUB_SEQUENCE_DIR="$SEQ_TMP" MOCK_CURL_COUNTER_FILE="$SEQ_TMP/count" \
  CI_COMMIT_SHA=deadbeef GATE_POLL_SECONDS=0 "$HELPERS_DIR/github_checks_gate.sh" 2>&1) || rc=$?
assert_eq "gate: pending then green exits 0" "0" "$rc"
assert_contains "gate: says it waited" "waiting: Build and test (Windows) is in_progress" "$out"
assert_eq "gate: polled exactly twice" "2" "$(cat "$SEQ_TMP/count")"
rm -rf "$SEQ_TMP"

# no check runs at all and the initial wait exhausted → fail, name the sha.
rc=0
out=$(PATH="$MOCK_BIN:$PATH" CI_COMMIT_SHA=deadbeef GATE_POLL_SECONDS=0 GATE_INITIAL_WAIT_SECONDS=0 \
  "$HELPERS_DIR/github_checks_gate.sh" 2>&1) || rc=$?
assert_eq "gate: no runs exits 1" "1" "$rc"
assert_contains "gate: no-runs message" "ERROR: no GitHub check runs for deadbeef" "$out"
```

- [ ] **Step 2: Run the harness to verify it fails**

Run: `.gitlab/ci/scripts/test/run_tests.sh 2>&1 | grep -A2 "github_checks_gate" | head`
Expected: `No such file or directory`.

- [ ] **Step 3: Write `github_checks_gate.sh`**

```bash
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
    verdict=$(python3 - "$RESP" "$REQUIRED" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
required = [r for r in sys.argv[2].split(";") if r]
runs = {r["name"]: r for r in data.get("check_runs", [])}
if data.get("total_count", 0) == 0:
    print("none"); sys.exit(0)
verdict = "ok"
for name in required:
    r = runs.get(name)
    if r is None:
        print(f"waiting: {name} has not started"); verdict = "retry"; continue
    if r["status"] != "completed":
        print(f"waiting: {name} is {r['status']}"); verdict = "retry"; continue
    if r["conclusion"] in ("success", "skipped", "neutral"):
        print(f"ok: {name} — {r['conclusion']}")
    else:
        print(f"ERROR: {name} concluded {r['conclusion']}"); verdict = "fail"
print(verdict)
PY
)
    printf '%s\n' "$verdict" | sed '$d'
    verdict=$(printf '%s\n' "$verdict" | tail -n1)
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
```

`chmod +x`.

- [ ] **Step 4: Run the harness to verify it passes**

Run: `.gitlab/ci/scripts/test/run_tests.sh 2>&1 | tail -15`
Expected: all `gate:` assertions `ok:`, `Failed: 0`.

- [ ] **Step 5: Run it for real against HEAD (unauthenticated, one call)**

Run: `CI_COMMIT_SHA=$(git rev-parse HEAD) GATE_POLL_SECONDS=5 .gitlab/ci/scripts/github_checks_gate.sh`
Expected: two `ok:` lines and `ok: GitHub check-suite green for …` (HEAD's `main` run was green when this plan was written; if HEAD moved, expect whichever verdict `gh run list --commit <sha>` shows).

- [ ] **Step 6: Commit**

```bash
git add .gitlab/ci/scripts/github_checks_gate.sh .gitlab/ci/scripts/test/
git commit -m "ci: gate:tests — the tag pipeline waits for the commit's GitHub check-suite instead of racing it"
```

---

### Task 5: `verify_release.sh` and `create_gitlab_release.sh`

**Files:**
- Create: `.gitlab/ci/scripts/verify_release.sh`
- Create: `.gitlab/ci/scripts/create_gitlab_release.sh`
- Create: `.gitlab/ci/scripts/test/fixtures/dockerhub_tag_{both,amd64only}.json`
- Modify: `.gitlab/ci/scripts/test/fixtures/mock_bin/curl` (three URL cases + `--head`)
- Modify: `.gitlab/ci/scripts/test/run_tests.sh`

**Interfaces:**
- `verify_release.sh`: env `CI_COMMIT_TAG` (required), `BUILDS_BASE_URL` (`https://artfrom.space/builds`), `DOCKERHUB_REPO` (`vsharifov/athenaeum`), `MIN_ARTIFACT_BYTES` (1048576), `RELEASE_ALLOW_AMD64_ONLY` (`1` = arm64 image may be missing), `BLOG_BASE_URL` (`https://artfrom.space/blog`), `SKIP_BLOG_CHECK` (`1` = don't poll the blog), `VERIFY_BLOG_TIMEOUT_SECONDS` (900), `VERIFY_POLL_SECONDS` (30), `SKIP_DOCKER_CHECK` (`1`). Exit 0 only when every artifact from `all_release_artifacts` answers HEAD 200 with `content-length ≥ MIN_ARTIFACT_BYTES`, the Docker Hub tag `<version>` exists with an `amd64` image (and `arm64` unless allowed) and the channel tag (`latest`/`beta`) has the same digest, and the blog URL answers 200 within the budget.
- `create_gitlab_release.sh`: env `CI_COMMIT_TAG`, `CI_API_V4_URL`, `CI_PROJECT_ID`, `CI_JOB_TOKEN` (all from GitLab), `RELEASE_NOTES_PATH`, `BUILDS_BASE_URL`, `DRY_RUN=1` prints the JSON payload. Links come from `all_release_artifacts`; the description is the whole `RELEASE_NOTES.md`.
- Mock `curl`: learns `--head` (writes `HTTP/2 200\ncontent-length: N\n` to the output file) and three URL cases: `*artfrom.space/builds/*` (`MOCK_CURL_ARTIFACT_HTTP` 200, `MOCK_CURL_ARTIFACT_LENGTH` 5000000, `MOCK_CURL_MISSING_ARTIFACTS` = space-separated basenames that answer 404), `*hub.docker.com/v2/repositories/*/tags/*` (`MOCK_CURL_HUB_BODY_FILE`, `MOCK_CURL_HUB_HTTP`), `*artfrom.space/blog/*` (`MOCK_CURL_BLOG_HTTP` 200), `*/releases` with `--request POST` (`MOCK_CURL_RELEASE_HTTP` 201, captures the body to `MOCK_CURL_CAPTURE_RELEASE_BODY`).

- [ ] **Step 1: Fixtures, mock cases, failing tests**

`dockerhub_tag_both.json`:
```json
{"name": "0.7.0", "digest": "sha256:aaaa", "images": [{"architecture": "amd64", "os": "linux"}, {"architecture": "arm64", "os": "linux"}]}
```
`dockerhub_tag_amd64only.json`:
```json
{"name": "0.7.0", "digest": "sha256:aaaa", "images": [{"architecture": "amd64", "os": "linux"}]}
```

Mock `curl` additions — in the arg parser add `--head) is_head=1 ;;` (initialise `is_head=0` at the top) and `--dump-header) i=$((i + 1)) ;;`; new URL arms:

```bash
  *artfrom.space/builds/*)
    http_code="${MOCK_CURL_ARTIFACT_HTTP:-200}"
    base=$(basename "$url")
    for missing in ${MOCK_CURL_MISSING_ARTIFACTS:-}; do [ "$base" = "$missing" ] && http_code=404; done
    if [ "$is_head" -eq 1 ]; then
      printf 'HTTP/2 %s\ncontent-length: %s\n' "$http_code" "${MOCK_CURL_ARTIFACT_LENGTH:-5000000}" > "$output_file"
    else
      : > "$output_file"
    fi
    ;;
  *hub.docker.com/v2/repositories/*/tags/*)
    http_code="${MOCK_CURL_HUB_HTTP:-200}"
    if [ -n "${MOCK_CURL_HUB_BODY_FILE:-}" ] && [ -f "${MOCK_CURL_HUB_BODY_FILE}" ]; then cp "$MOCK_CURL_HUB_BODY_FILE" "$output_file"; else printf '{}' > "$output_file"; fi
    ;;
  *artfrom.space/blog/*)
    http_code="${MOCK_CURL_BLOG_HTTP:-200}"
    printf 'HTTP/2 %s\n' "$http_code" > "$output_file"
    ;;
  */releases)
    http_code="${MOCK_CURL_RELEASE_HTTP:-201}"
    [ -n "${MOCK_CURL_CAPTURE_RELEASE_BODY:-}" ] && printf '%s' "$body" > "$MOCK_CURL_CAPTURE_RELEASE_BODY"
    printf '{}' > "$output_file"
    ;;
```

Note: `*repositories/*` (the Docker Hub description case) must stay BELOW the new `*hub.docker.com/v2/repositories/*/tags/*` arm, or it shadows it — order the arms tags-first.

Append to `run_tests.sh`:

```bash
echo
echo "-- verify_release.sh --"

common=(PATH="$MOCK_BIN:$PATH" CI_COMMIT_TAG=v0.7.0 VERIFY_POLL_SECONDS=0 VERIFY_BLOG_TIMEOUT_SECONDS=0)
rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: everything present exits 0" "0" "$rc"
assert_contains "verify: checks 13 artifacts" "ok: 13 artifacts present" "$out"
assert_contains "verify: docker both arches" "ok: docker.io/vsharifov/athenaeum:0.7.0 has amd64 + arm64" "$out"
assert_contains "verify: channel tag matches" "ok: :latest -> sha256:aaaa" "$out"
assert_contains "verify: blog reachable" "ok: blog https://artfrom.space/blog/v070/" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" \
  MOCK_CURL_MISSING_ARTIFACTS="athenaeum-0.7.0-macos-x64.dmg" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: a missing installer exits 1" "1" "$rc"
assert_contains "verify: names the missing URL" "ERROR: HTTP 404 for https://artfrom.space/builds/v0.7.0/macos/athenaeum-0.7.0-macos-x64.dmg" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" \
  MOCK_CURL_ARTIFACT_LENGTH=12 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: a tiny file exits 1" "1" "$rc"
assert_contains "verify: names the size" "ERROR: 12 bytes (< 1048576) at https://artfrom.space/builds/v0.7.0/windows/athenaeum-0.7.0-windows-x64.msi" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_amd64only.json" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: arm64 missing exits 1 by default" "1" "$rc"
assert_contains "verify: arm64 message names the escape hatch" "ERROR: docker.io/vsharifov/athenaeum:0.7.0 has no arm64 image (set RELEASE_ALLOW_AMD64_ONLY=1 to accept)" "$out"

rc=0
out=$(env "${common[@]}" RELEASE_ALLOW_AMD64_ONLY=1 MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_amd64only.json" "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: arm64 missing accepted with the variable" "0" "$rc"
assert_contains "verify: still warns" "WARNING: publishing amd64-only" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_HTTP=404 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: docker tag absent exits 1" "1" "$rc"
assert_contains "verify: docker absent message" "ERROR: Docker Hub has no tag 0.7.0" "$out"

rc=0
out=$(env "${common[@]}" MOCK_CURL_HUB_BODY_FILE="$FIXTURES_DIR/dockerhub_tag_both.json" MOCK_CURL_BLOG_HTTP=404 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: blog 404 past the budget exits 1" "1" "$rc"
assert_contains "verify: blog message" "ERROR: blog https://artfrom.space/blog/v070/ still HTTP 404 after" "$out"

rc=0
out=$(env "${common[@]}" SKIP_DOCKER_CHECK=1 SKIP_BLOG_CHECK=1 "$HELPERS_DIR/verify_release.sh" 2>&1) || rc=$?
assert_eq "verify: skips honoured" "0" "$rc"
assert_contains "verify: says what it skipped" "skipped: docker check (SKIP_DOCKER_CHECK=1)" "$out"

echo
echo "-- create_gitlab_release.sh --"

REL_TMP=$(mktemp -t release_body.XXXXXX)
rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_CAPTURE_RELEASE_BODY="$REL_TMP" CI_COMMIT_TAG=v0.7.0 CI_API_V4_URL=http://gitlab.local/api/v4 CI_PROJECT_ID=1 CI_JOB_TOKEN=t \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/create_gitlab_release.sh" 2>&1) || rc=$?
assert_eq "release: exits 0" "0" "$rc"
payload=$(cat "$REL_TMP")
assert_contains "release: tag" '"tag_name": "v0.7.0"' "$payload"
assert_contains "release: name" '"name": "Athenaeum v0.7.0"' "$payload"
assert_contains "release: description carries the notes" "## Bug Fixes" "$payload"
assert_contains "release: msi link" 'https://artfrom.space/builds/v0.7.0/windows/athenaeum-0.7.0-windows-x64.msi' "$payload"
assert_contains "release: perseus arm64 deb link" 'https://artfrom.space/builds/v0.7.0/perseus/perseus-0.7.0-linux-arm64.deb' "$payload"
assert_eq "release: 13 asset links" "13" "$(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["assets"]["links"]))' "$REL_TMP")"
rm -f "$REL_TMP"

rc=0
out=$(PATH="$MOCK_BIN:$PATH" MOCK_CURL_RELEASE_HTTP=409 CI_COMMIT_TAG=v0.7.0 CI_API_V4_URL=http://gitlab.local/api/v4 CI_PROJECT_ID=1 CI_JOB_TOKEN=t \
  RELEASE_NOTES_PATH="$FIXTURES_DIR/release_notes_sample.md" "$HELPERS_DIR/create_gitlab_release.sh" 2>&1) || rc=$?
assert_eq "release: API error exits 1" "1" "$rc"
assert_contains "release: API error message" "ERROR: GitLab release API answered HTTP 409" "$out"
```

- [ ] **Step 2: Run the harness to verify it fails**

Run: `.gitlab/ci/scripts/test/run_tests.sh 2>&1 | grep -E "verify_release|create_gitlab" -A2 | head`
Expected: `No such file or directory` for both scripts.

- [ ] **Step 3: Write `verify_release.sh`**

```bash
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
```

- [ ] **Step 4: Write `create_gitlab_release.sh`**

```bash
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

PAYLOAD=$(
  python3 - "$CI_COMMIT_TAG" "$NOTES" <<'PY' < <(all_release_artifacts | while read -r product os arch ext variant subdir filename alias; do printf '%s\t%s\n' "$(label "$product" "$os" "$arch" "$ext" "$variant")" "${BUILDS_BASE_URL}/${CI_COMMIT_TAG}/${subdir}/${filename}"; done)
import json, sys
tag, notes = sys.argv[1], sys.argv[2]
try:
    description = open(notes, encoding="utf-8").read()
except OSError:
    description = f"Release {tag}"
links = [{"name": n, "url": u} for n, u in (l.rstrip("\n").split("\t") for l in sys.stdin if l.strip())]
print(json.dumps({"tag_name": tag, "name": f"Athenaeum {tag}", "description": description, "assets": {"links": links}}, indent=2, ensure_ascii=False))
PY
)

if [ "${DRY_RUN:-0}" = "1" ]; then printf '%s\n' "$PAYLOAD"; exit 0; fi
RESP=$(mktemp -t release_resp.XXXXXX); trap 'rm -f "$RESP"' EXIT
code=$(printf '%s' "$PAYLOAD" | curl --silent --show-error --request POST --output "$RESP" --write-out '%{http_code}' \
  --header "JOB-TOKEN: ${CI_JOB_TOKEN}" --header "Content-Type: application/json" --data @- \
  "${CI_API_V4_URL}/projects/${CI_PROJECT_ID}/releases" || echo 000)
if [ "$code" != "201" ]; then echo "ERROR: GitLab release API answered HTTP $code: $(cat "$RESP")" >&2; exit 1; fi
echo "ok: GitLab Release ${CI_COMMIT_TAG} created with $(printf '%s' "$PAYLOAD" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["assets"]["links"]))') links"
```

`chmod +x` both. (The mock `curl` must treat `--data @-` as "read stdin" — it already does via `has_data`.)

- [ ] **Step 5: Run the harness to verify it passes**

Run: `.gitlab/ci/scripts/test/run_tests.sh 2>&1 | tail -30`
Expected: all `verify:` and `release:` assertions `ok:`, `Failed: 0`. Existing Docker Hub description tests still pass (arm order — check the `*repositories/*` arm still gets its calls).

- [ ] **Step 6: Run verify for real against v0.6.3 with the OLD names expected to fail**

Run: `CI_COMMIT_TAG=v0.6.3 SKIP_BLOG_CHECK=1 .gitlab/ci/scripts/verify_release.sh; echo "rc=$?"`
Expected: 13 `ERROR: HTTP 404 …` lines (v0.6.3 was published under the old names), Docker `ok:` lines for 0.6.3 (both arches, `:latest` digest), `rc=1`. This proves the script talks to the real endpoints; the names go live with the next tag.

- [ ] **Step 7: Commit**

```bash
git add .gitlab/ci/scripts/verify_release.sh .gitlab/ci/scripts/create_gitlab_release.sh .gitlab/ci/scripts/test/
git commit -m "ci: verify_release.sh fetches every published thing back before anything is announced; GitLab Release payload built by a script"
```

---

### Task 6: The pipeline graph — `gate → build → deploy → publish → verify → announce`

**Files:**
- Modify: `.gitlab-ci.yml` (whole-file restructure; every script body below is complete)
- Modify: `crates/athenaeum-tauri/src/commands/core.rs` (the version-check URL gains `&arch=`)

**Interfaces:**
- Consumes every script from Tasks 1–5.
- Produces the job set the release skill (Task 7) documents and the acceptance run (Task 9) exercises.

- [ ] **Step 1: Stages and the gate jobs**

Replace the `stages:` block:

```yaml
stages:
  - gate      # check:versions, gate:tests — refuse a tag that cannot be a release
  - build     # the seven installers + check:headless-core
  - deploy    # rename + upload to artfrom.space/builds/<tag>/
  - publish   # Docker Hub (3 jobs) + the docs site
  - verify    # fetch everything back; Gatekeeper on the real DMGs; BLOCKING
  - announce  # GitLab Release, version.json, Discord, Telegram — only after verify
```

Add after the `variables:` block:

```yaml
# --- Gate (tags only) ---
# Two refusals before a single build minute is spent:
#   check:versions — the six version files must equal the tag (bump.sh's job to make them so).
#   gate:tests     — the tagged commit's GitHub check-suite (the only place tests
#                    run) must be green. It polls up to 50 min; build jobs start in
#                    parallel (needs: []) and deploy waits for it, so it costs no
#                    wall clock on a normal release.
check:versions:
  stage: gate
  tags: [linux]
  only: [tags]
  needs: []
  script:
    - TAG="${CI_COMMIT_TAG}" .gitlab/ci/scripts/check_versions.sh

gate:tests:
  stage: gate
  tags: [linux]
  only: [tags]
  needs: []
  timeout: 55m
  script:
    - .gitlab/ci/scripts/github_checks_gate.sh
```

Every `build:*` job that has `only: [tags]` gets `needs: [check:versions]` (replace `needs: []` on `build:windows`; `build:perseus:windows` keeps its `build:windows` need — add `check:versions` to the list). `build:linux`, `build:macos`, `build:perseus:macos`, `build:perseus:linux`, `build:perseus:linux-arm64`: add `needs: [check:versions]`.

- [ ] **Step 2: `build:macos` — sign in Tauri, notarize each DMG once**

Replace the two `npm run tauri build` lines and the notarize loop:

```yaml
    # Tauri SIGNS the .app (APPLE_SIGNING_IDENTITY) but does NOT notarize it:
    # the three APPLE_API_* variables are withheld from this step, so the only
    # notarization per architecture is the DMG's below (ruling F5.1 — two
    # notary waits per release instead of four; the DMG ticket covers the app
    # inside, which verify:macos-gatekeeper proves on the published file).
    - env -u APPLE_API_ISSUER -u APPLE_API_KEY -u APPLE_API_KEY_PATH npm run tauri build -- --target aarch64-apple-darwin
    - env -u APPLE_API_ISSUER -u APPLE_API_KEY -u APPLE_API_KEY_PATH npm run tauri build -- --target x86_64-apple-darwin
    - |
      for DMG in target/aarch64-apple-darwin/release/bundle/dmg/*.dmg \
                 target/x86_64-apple-darwin/release/bundle/dmg/*.dmg; do
        echo "Notarizing $DMG ..."
        xcrun notarytool submit "$DMG" --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" --wait
        xcrun stapler staple "$DMG" && xcrun stapler validate "$DMG"
      done
```

- [ ] **Step 3: Delete the three `update-cache:*` jobs; add runner smoke jobs**

Remove `update-cache:windows`, `update-cache:linux`, `update-cache:macos` entirely (the build jobs are `pull-push` since `8baaaedb`; the warmers were dead weight). In their place:

```yaml
# --- Runner smoke (main, manual) ---
# A ten-second job per runner to prove it takes work — the thing to play after
# a runner restart or a machine reboot. It is NOT a build.
.runner_smoke:
  stage: build
  only: [main]
  when: manual
  script:
    - echo "runner ok on $(hostname)"; pwd; echo "PWD=$PWD"

smoke:macos:
  extends: .runner_smoke
  tags: [macos]
smoke:linux:
  extends: .runner_smoke
  tags: [linux]
smoke:windows:
  extends: .runner_smoke
  tags: [windows]
  script:
    - Write-Output "runner ok on $env:COMPUTERNAME"; Get-Location
```

- [ ] **Step 4: `deploy` — rename through the library, aliases + legacy aliases, pinned host key**

Replace the `deploy` job's `before_script` and `script`:

```yaml
  before_script:
    - eval $(ssh-agent -s)
    - chmod 400 "$DEPLOY_SSH_KEY"
    - ssh-add "$DEPLOY_SSH_KEY"
    - mkdir -p ~/.ssh && chmod 700 ~/.ssh
    # Pin the host key from SSH_KNOWN_HOSTS when set (ssh-keyscan -p 40022 <host>),
    # fall back to TOFU with a warning — the same contract as the docs site's CI.
    - |
      if [ -n "${SSH_KNOWN_HOSTS:-}" ]; then printf '%s\n' "$SSH_KNOWN_HOSTS" > ~/.ssh/known_hosts; chmod 600 ~/.ssh/known_hosts; echo "known_hosts pinned"
      else echo "WARNING: SSH_KNOWN_HOSTS unset — unpinned ssh-keyscan (TOFU)"; ssh-keyscan -p ${DEPLOY_PORT} -H "$DEPLOY_HOST" >> ~/.ssh/known_hosts 2>/dev/null; fi
    - export SSH_CMD="ssh -p ${DEPLOY_PORT} -o StrictHostKeyChecking=yes"
    - export SCP_CMD="scp -P ${DEPLOY_PORT} -o StrictHostKeyChecking=yes"
  script:
    - . .gitlab/ci/scripts/artifact_names.sh
    - export VERSION="${CI_COMMIT_TAG#v}"
    # One staging dir laid out exactly like the server: <subdir>/<final name>.
    - rm -rf release_stage && mkdir -p release_stage/windows release_stage/macos release_stage/linux release_stage/perseus
    - cp target/release/bundle/msi/*.msi                          "release_stage/windows/$(artifact_name athenaeum windows x64 msi)"
    - cp target/release/bundle/nsis/*.exe                         "release_stage/windows/$(artifact_name athenaeum windows x64 exe setup)"
    - cp target/aarch64-apple-darwin/release/bundle/dmg/*.dmg     "release_stage/macos/$(artifact_name athenaeum macos arm64 dmg)"
    - cp target/x86_64-apple-darwin/release/bundle/dmg/*.dmg      "release_stage/macos/$(artifact_name athenaeum macos x64 dmg)"
    - cp target/release/bundle/appimage/*.AppImage                "release_stage/linux/$(artifact_name athenaeum linux x64 AppImage)"
    - cp target/release/bundle/deb/*.deb                          "release_stage/linux/$(artifact_name athenaeum linux x64 deb)"
    - cp target/aarch64-apple-darwin/release/bundle/perseus-*-macos-arm64.dmg "release_stage/perseus/$(artifact_name perseus macos arm64 dmg)"
    - cp target/x86_64-apple-darwin/release/bundle/perseus-*-macos-x64.dmg    "release_stage/perseus/$(artifact_name perseus macos x64 dmg)"
    - cp crates/perseus/packaging/windows/perseus-*-setup.exe     "release_stage/perseus/$(artifact_name perseus windows x64 exe setup)"
    - cp target/debian/perseus_*_amd64.deb                        "release_stage/perseus/$(artifact_name perseus linux x64 deb)"
    - cp target/debian/perseus-*-linux-amd64.tar.gz               "release_stage/perseus/$(artifact_name perseus linux x64 tar.gz)"
    - cp target/debian/perseus-headless_*_arm64.deb               "release_stage/perseus/$(artifact_name perseus linux arm64 deb)"
    - cp target/debian/perseus-*-linux-arm64.tar.gz               "release_stage/perseus/$(artifact_name perseus linux arm64 tar.gz)"
    # Every inventory line must now exist — a glob that matched nothing above fails here, not on the server.
    - |
      while read -r product os arch ext variant subdir filename alias; do
        [ -s "release_stage/$subdir/$filename" ] || { echo "ERROR: missing release_stage/$subdir/$filename"; exit 1; }
      done < <(all_release_artifacts)
    - ${SSH_CMD} ${DEPLOY_USER}@${DEPLOY_HOST} "mkdir -p ${BUILDS_PATH}/${CI_COMMIT_TAG}/linux ${BUILDS_PATH}/${CI_COMMIT_TAG}/windows ${BUILDS_PATH}/${CI_COMMIT_TAG}/macos ${BUILDS_PATH}/${CI_COMMIT_TAG}/perseus"
    - for d in windows macos linux perseus; do ${SCP_CMD} release_stage/$d/* ${DEPLOY_USER}@${DEPLOY_HOST}:${BUILDS_PATH}/${CI_COMMIT_TAG}/$d/; done
    # Stable aliases (version-less) + legacy aliases (pre-rename spellings, until v0.7.0).
    - |
      { echo "set -e"
        all_release_artifacts | while read -r product os arch ext variant subdir filename alias; do
          echo "ln -sfn '$filename' '${BUILDS_PATH}/${CI_COMMIT_TAG}/$subdir/$alias'"; done
        legacy_alias_pairs | while read -r subdir legacy current; do
          echo "ln -sfn '$current' '${BUILDS_PATH}/${CI_COMMIT_TAG}/$subdir/$legacy'"; done
      } > alias_cmds.sh
      ${SSH_CMD} ${DEPLOY_USER}@${DEPLOY_HOST} "bash -s" < alias_cmds.sh
    - |
      if echo "${CI_COMMIT_TAG}" | grep -q '\-beta'; then ${SSH_CMD} ${DEPLOY_USER}@${DEPLOY_HOST} "ln -sfn ${BUILDS_PATH}/${CI_COMMIT_TAG} ${BUILDS_PATH}/beta"
      else ${SSH_CMD} ${DEPLOY_USER}@${DEPLOY_HOST} "ln -sfn ${BUILDS_PATH}/${CI_COMMIT_TAG} ${BUILDS_PATH}/latest"; fi
```

Add `- job: gate:tests` (with `artifacts: false`) to `deploy`'s `needs:` list, and drop the `SSH_CMD`/`SCP_CMD` entries from its `variables:` (they are exported in `before_script` now).

- [ ] **Step 5: `publish` stage — Docker jobs move, `docs:publish` is new**

Change `stage: release` → `stage: publish` on `publish:dockerhub:amd64`, `publish:dockerhub:arm64`, `publish:dockerhub:manifest`, and their `needs: [release]` → `needs: [{job: deploy, artifacts: false}]` (the manifest job keeps its two arch needs, arm64 `optional: true`). `allow_failure: true` stays on all three — verify is where a Docker failure becomes blocking.

Add:

```yaml
docs:publish:
  stage: publish
  tags: [linux]
  only: [tags]
  needs:
    - job: deploy
      artifacts: false
  script:
    # DOCS_REPO_TOKEN: artfrom-space project access token (Maintainer, write_repository).
    # This job is REQUIRED — the chat announcements link the post it writes.
    - TAG="${CI_COMMIT_TAG}" RELEASE_DATE="$(date -u -d "${CI_COMMIT_TIMESTAMP}" +%Y-%m-%d)" .gitlab/ci/scripts/docs_publish.sh
```

- [ ] **Step 6: `verify` stage**

```yaml
verify:release:
  stage: verify
  tags: [linux]
  only: [tags]
  needs:
    - job: deploy
      artifacts: false
    - job: docs:publish
      artifacts: false
    - job: publish:dockerhub:manifest
      artifacts: false
      optional: true
  timeout: 30m
  script:
    - .gitlab/ci/scripts/verify_release.sh

# Gatekeeper on the PUBLISHED DMGs, on a Mac, every release: the battery from
# memory project_macos_signing, automated. Proves ruling F5.1 (one notarization
# per DMG) on the real files users download.
verify:macos-gatekeeper:
  stage: verify
  tags: [macos]
  only: [tags]
  needs:
    - job: deploy
      artifacts: false
  script:
    - . .gitlab/ci/scripts/artifact_names.sh
    - export VERSION="${CI_COMMIT_TAG#v}"
    - rm -rf gk && mkdir gk
    - |
      set -e
      for arch in arm64 x64; do
        f="$(artifact_name athenaeum macos "$arch" dmg)"
        curl -fsSL -o "gk/$f" "https://artfrom.space/builds/${CI_COMMIT_TAG}/macos/$f"
        xcrun stapler validate "gk/$f"
        spctl -a -t open --context context:primary-signature -v "gk/$f"
        mnt="$PWD/gk/mnt-$arch"; mkdir -p "$mnt"
        hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "gk/$f" >/dev/null
        spctl -a -t exec -vv "$mnt/Athenaeum.app" 2>&1 | tee "gk/spctl-$arch.txt"
        grep -q "source=Notarized Developer ID" "gk/spctl-$arch.txt" || { echo "ERROR: $f: app inside is not accepted as Notarized Developer ID"; hdiutil detach "$mnt" >/dev/null || true; exit 1; }
        hdiutil detach "$mnt" >/dev/null
        echo "ok: $f accepted by Gatekeeper"
      done
  after_script:
    - for m in gk/mnt-*; do [ -d "$m" ] && hdiutil detach "$m" >/dev/null 2>&1 || true; done
```

- [ ] **Step 7: `announce` stage**

`release`, `publish_version`, `notify:discord`, `notify:telegram` become `stage: announce` with

```yaml
  needs:
    - job: verify:release
      artifacts: false
    - job: verify:macos-gatekeeper
      artifacts: false
```

(the two notify jobs keep `allow_failure: true` and drop their `needs: [release]` in favour of the verify pair; the GitLab Release and the chat posts are siblings now — a chat outage never blocks the Release object and vice versa). `release`'s script becomes:

```yaml
  script:
    - .gitlab/ci/scripts/create_gitlab_release.sh
```

`publish_version` gets the same pinned-host-key `before_script` as `deploy` (Step 4) and keeps its script.

- [ ] **Step 8: The version-check GET reports the architecture**

In `crates/athenaeum-tauri/src/commands/core.rs::fetch_version_json`, extend the URL: `"https://artfrom.space/{}?v={}&os={}&arch={}&commit={}&id={}"` with `std::env::consts::ARCH` as the new argument (values `aarch64` / `x86_64` — the Rust names; the nginx-log parser in astronet reads them, and that parser is a separate follow-up in the astronet repo: `athenaeum_version_parser.py` gains an `arch` label). Run `cargo check -p athenaeum-tauri` — expected clean.

- [ ] **Step 9: Lint the YAML and the whole graph**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.gitlab-ci.yml'))" 2>/dev/null || pip3 install --quiet pyyaml && python3 -c "import yaml; d=yaml.safe_load(open('.gitlab-ci.yml')); print(d['stages']); print(sorted(k for k in d if isinstance(d[k],dict) and 'stage' in d[k]))"`
Expected: the six stages in order and the job list: `build:linux build:macos build:perseus:linux build:perseus:linux-arm64 build:perseus:macos build:perseus:windows build:windows check:headless-core check:versions deploy docs:publish gate:tests notify:discord notify:telegram publish:dockerhub:amd64 publish:dockerhub:arm64 publish:dockerhub:manifest publish_version release smoke:linux smoke:macos smoke:windows verify:macos-gatekeeper verify:release`.

Then GitLab's own linter (catches `needs` on non-existent jobs, stage order):
```bash
PAT=$(cat ~/.config/athenaeum-ci/gitlab-pat); python3 -c 'import json,sys; print(json.dumps({"content": open(".gitlab-ci.yml").read()}))' | curl -s -H "PRIVATE-TOKEN: $PAT" -H "Content-Type: application/json" -d @- "http://192.168.31.208:9080/api/v4/projects/1/ci/lint" | python3 -c 'import sys,json; d=json.load(sys.stdin); print("valid" if d.get("valid") else d)'
```
Expected: `valid`.

- [ ] **Step 10: Commit**

```bash
git add .gitlab-ci.yml crates/athenaeum-tauri/src/commands/core.rs
git commit -m "ci: gate → build → deploy → publish → verify → announce — nothing is announced before everything is published; one notarization per DMG; runner smoke jobs replace the cache warmers"
```

---

### Task 7: The release skill, `CLAUDE.md`, memory

**Files:**
- Create: `.claude/skills/release/SKILL.md`
- Modify: `CLAUDE.md` (the `## Release workflow` section)
- Modify: memory `reference_release_workflow.md` (pointer to the skill; the ×6 line stays)

- [ ] **Step 1: Write the skill**

`.claude/skills/release/SKILL.md`:

```markdown
---
name: release
description: Cut an Athenaeum release (stable or beta) — notes, version bump, the green-check rule, tag, the pipeline's gate/publish/verify/announce stages and what to do when one goes red. Use when the owner says "release", "cut vX.Y.Z", "tag", or asks what the pipeline is doing.
---

# Release

One procedure for every Athenaeum release. The tag pipeline does the publishing,
the docs site and the announcements; this skill's job is to hand it a commit
that can be released and to read its verdicts.

## Before you start

- You are in the main checkout on `main`, clean, and `main` == `origin/main` on
  both remotes (`git fetch all && git status -sb`). A worktree session cannot
  release — see memory `reference-worktree-session-release`.
- The GitHub run of the commit you will tag must be green. The pipeline's
  `gate:tests` job enforces it; you check first so a red gate never surprises
  anyone: `gh run list --commit $(git rev-parse HEAD) --json name,conclusion`.
- Keep the dev Mac idle while `build:macos` and `build:perseus:macos` run — the
  macOS runner is this machine and a saturated box times the job out.

## Procedure

1. **Notes.** Rewrite `RELEASE_NOTES.md` (fully replaced each release): an
   italic tagline line (`*Athenaeum vX.Y.Z: …*` — it becomes the blog excerpt,
   the Version History row and the chat teaser), then `## What's New` /
   `## Changes` / `## Bug Fixes` in user-facing English. This file IS the blog
   post: write it at that quality, there is no second text.
2. **Bump.** `scripts/release/bump.sh X.Y.Z[-beta.N]` — rewrites the six version
   files, refreshes `Cargo.lock`, asserts they agree.
3. **The release commit contains ONLY** `RELEASE_NOTES.md`, the six version files
   and `Cargo.lock`. A code fix goes in its own commit BEFORE this one and gets
   its own green GitHub run first. `git status --short` must list exactly those
   eight paths. Commit: `chore(release): vX.Y.Z — <tagline without the product name>`.
4. **Push main, wait for green, tag.**
   `git push all main` → wait for the GitHub run of that commit
   (`gh run watch` or poll `gh run list --commit <sha>`) → `git tag vX.Y.Z && git push all vX.Y.Z`.
   Pushing the tag before the run is green is allowed (the gate waits) but
   pointless; a red run means fix-then-retag, never force.
5. **Read the pipeline** (`http://192.168.31.208:9080/root/athenaeum/-/pipelines`, or
   the API with the PAT in `~/.config/athenaeum-ci/gitlab-pat`):
   - `gate` — `check:versions` red: a version file disagrees with the tag → fix,
     delete the tag on both remotes, retag. `gate:tests` red: GitHub is red →
     same.
   - `build` — a runner problem: memory `reference-macos-runner-wedge` (Mac),
     `reference-linux-ci-runner`. A plain retry of the one job is the first move.
   - `publish` — `docs:publish` red without `DOCS_REPO_TOKEN`: create the token
     (artfrom-space, Maintainer, write_repository), set the variable, retry the
     job. Docker jobs are yellow-on-failure by design; the verify stage decides.
   - `verify` — `verify:release` red names exactly what is missing (an installer
     URL, a Docker arch, the blog URL). Fix the cause, retry the failed publish
     job, then retry `verify:release`; the announce jobs re-run by themselves.
     `RELEASE_ALLOW_AMD64_ONLY=1` (project variable) accepts an amd64-only image
     while the arm64 runner is down — unset it afterwards.
     `verify:macos-gatekeeper` red: the published DMG is not accepted — do NOT
     announce; the notarization step is the suspect.
   - `announce` — the GitLab Release, `version.json`, Discord and Telegram. Only
     these are user-visible; nothing before them is.
6. **Done when** the pipeline is green. Nothing else to do by hand: the blog post,
   the download page and the Version History row were written by `docs:publish`.

## Re-tagging

`git tag -d vX.Y.Z && git push all :refs/tags/vX.Y.Z && git tag vX.Y.Z && git push all vX.Y.Z`.
Every stage re-runs; the docs commit is idempotent (same post, same row).
NEVER retry a build job of a FINISHED release pipeline — it re-fires the announcements.

## Where things are

- Pipeline: `.gitlab-ci.yml`; scripts + their tests: `.gitlab/ci/scripts/`,
  `.gitlab/ci/scripts/test/run_tests.sh` (run it after touching any of them).
- Artifact names: `.gitlab/ci/scripts/artifact_names.sh` — the only place a
  filename is spelled. Scheme `<product>-<version>-<os>-<arch>[-variant].<ext>`,
  arch ∈ {x64, arm64}.
- Docs site: `../artfrom-space` — generated files only; hand-edit guides, never
  the blog post of a release or the marked blocks of `download.md`.
- Review that shaped this: `docs/superpowers/research/2026-09-15-release-cycle-review.md`.
```

- [ ] **Step 2: Collapse `CLAUDE.md`'s section**

Replace the four numbered items of `## Release workflow` (keep the **Branching** paragraph) with:

```markdown
## Release workflow

The procedure is the `release` skill (`.claude/skills/release/SKILL.md`): notes
(`RELEASE_NOTES.md` is the blog post), `scripts/release/bump.sh`, a release
commit that holds only notes + the six version files + `Cargo.lock`, push, green
GitHub run, tag. The tag pipeline (`.gitlab-ci.yml`) runs
`gate → build → deploy → publish → verify → announce`: it refuses a tag whose
versions or GitHub checks are not green, uploads to `artfrom.space/builds/<tag>/`
under the one naming scheme (`.gitlab/ci/scripts/artifact_names.sh`), publishes
Docker Hub and the docs site (post + download page generated from the notes),
fetches all of it back, and only then creates the GitLab Release, `version.json`
and the chat posts. Nothing is done by hand after the tag.
```

- [ ] **Step 3: Memory**

Edit memory `reference_release_workflow.md`: replace items 1–6 with one line — `Procedure: the repo's `.claude/skills/release/SKILL.md` (since 2026-09-15; pipeline `gate → build → deploy → publish → verify → announce`, docs site generated, one naming scheme).` — keep the Docker Hub, keychain, re-tagging and update-checker paragraphs. Update the `description:` line accordingly.

- [ ] **Step 4: Commit**

```bash
git add .claude/skills/release/SKILL.md CLAUDE.md
git commit -m "docs: the release skill is the one procedure; CLAUDE.md points at it"
```

---

### Task 8: GitHub CI — nextest and a PR path filter (measured, revertible)

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:** the two job NAMES must not change (`gate:tests` looks them up).

- [ ] **Step 1: Baseline**

Record from `gh run view <latest main run> --json jobs`: per job, the `Test workspace` step duration (2026-09-14 reference: Linux 3 min 55 s, Windows 6 min 41 s).

- [ ] **Step 2: Doctest count**

Run: `cargo test --workspace --doc 2>&1 | grep -E "^test result|Doc-tests" | head -20`
Expected: the number of doctests per crate. If the total is 0, the `--doc` step below is dropped; if > 0 it stays (nextest does not run doctests).

- [ ] **Step 3: Edit `ci.yml`**

In both jobs replace the `Test workspace` step with:

```yaml
      - uses: taiki-e/install-action@v2
        with:
          tool: nextest

      # nextest runs every test binary's tests in parallel processes (the
      # per-binary serial runner spent most of the step waiting on the slowest
      # binary). Same unit graph as the --no-run step above, so no recompile.
      # The two skips are load-bearing (see the comment above); nextest's
      # filter expression replaces the two --skip flags.
      - name: Test workspace
        run: cargo nextest run --workspace --no-fail-fast -E 'not (test(unclean_shutdown_mid_transfer_resumes_on_restart) | test(ingest_releases_conn_between_frames))'

      - name: Doctests
        run: cargo test --workspace --doc
```

Add a path filter for pull requests only (pushes to `main`/tags always run everything — the gate depends on it):

```yaml
jobs:
  changes:
    name: What changed
    runs-on: ubuntu-latest
    if: github.event_name == 'pull_request'
    outputs:
      rust: ${{ steps.filter.outputs.rust }}
      frontend: ${{ steps.filter.outputs.frontend }}
    steps:
      - uses: actions/checkout@v4
      - uses: dorny/paths-filter@v3
        id: filter
        with:
          filters: |
            rust:
              - 'crates/**'
              - 'rustafits/**'
              - 'solvemyastro/**'
              - 'Cargo.*'
              - 'rust-toolchain.toml'
              - '.github/workflows/ci.yml'
            frontend:
              - 'src/**'
              - 'package*.json'
              - 'tsconfig*.json'
              - 'vite.config.ts'
```

and on `build-and-test` / `build-and-test-windows`:

```yaml
    needs: [changes]
    if: always() && (github.event_name != 'pull_request' || needs.changes.outputs.rust == 'true')
```

plus a new `typecheck-only` job that runs `npm ci` + `npx tsc --noEmit` when `github.event_name == 'pull_request' && needs.changes.outputs.rust != 'true' && needs.changes.outputs.frontend == 'true'`.

- [ ] **Step 4: Measure on the SECOND run**

Push the change (owner's word — it is a `main` push), then `gh run rerun <id>` once the first run finishes (memory `reference_github_ci_tuning`: the first run after a command-line change is cold and lies). Compare the `Test workspace` step to the baseline.
Acceptance: ≤ 65 % of the baseline on both jobs and no test that passed before now fails on two consecutive runs. Otherwise revert the nextest step (keep the path filter, which is orthogonal) and record the numbers in the plan's research note.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci(github): nextest for the test step; PRs that touch no Rust skip the Rust jobs"
```

---

### Task 9: Acceptance — `v0.6.4-beta.1` through the skill

This is the one step that proves the graph; everything before it is unit-tested scripts and a linted YAML. A beta is the right vehicle: it exercises every job (beta channel: `builds/beta`, `version-beta.json`, Docker `:beta`, a real blog post, real chat posts marked "(beta)") without touching `latest`.

- [ ] **Step 1: Prerequisites (owner)**
  - `DOCS_REPO_TOKEN` created on `root/artfrom-space` and set in the athenaeum project's CI variables (Masked, Protected).
  - The docs repo's marker commit (Task 3, Step 7) pushed: `git -C ../artfrom-space push origin main`.
  - `main` pushed to both remotes with Tasks 1–8 (`git push all main`), GitHub run green.

- [ ] **Step 2: Follow the skill** for `0.6.4-beta.1` with a real `RELEASE_NOTES.md` describing this cycle (the pipeline, the names, the docs automation, the runner fix — all user-visible in the sense that download names change). Tag on the owner's word.

- [ ] **Step 3: Watch and record**

Per stage, in `docs/superpowers/research/2026-09-15-release-pipeline-acceptance.md`: job durations, `verify:release`'s output, `verify:macos-gatekeeper`'s `spctl` lines, the blog URL, the download-page diff the pipeline committed, the Discord/Telegram posts. Compare the macOS job's duration with the 35–53 min of v0.6.0–v0.6.3 (Task 6 Step 2's one-notarization change).

- [ ] **Step 4: Fix-forward**

Any red job → fix on `main` (its own commit, green run), delete + recreate the tag, repeat. When green: update memory `project_release_cycle_review_2026_09.md` (cycles 1–3 DONE, numbers), and move the in-app updater (review §3) to its own spec via the brainstorming skill.

---

## Self-review

- **Spec coverage.** F1 (announce after publish) → Task 6 Steps 5–7 + Task 5's verify. F2 (test gate) → Task 4 + skill rule 3/4 in Task 7. F3 (docs by hand) → Task 3 + Task 6 Step 5. F4 (names, versioned download links) → Task 1, Task 6 Step 4, Task 3's page block. F5.1 (notarize once) → Task 6 Step 2, proven by `verify:macos-gatekeeper`; F5.2 (x64) → the `arch` query parameter, decision deferred; F5.3 (dedicated runner) → owner's call, not a task. F6 (skill + bump + check:versions) → Tasks 2, 6 Step 1, 7. F7 (dead warmers, pinned host key, nextest, path filter) → Task 6 Steps 3–4, Task 8. §3 (updater) → explicitly out of scope, Task 9 Step 4 hands it to a spec.
- **Placeholders.** None: every script body is complete; every test asserts exact strings; the YAML fragments are complete jobs.
- **Type/name consistency.** `all_release_artifacts` columns (`product os arch ext variant subdir filename alias`) are read identically in Tasks 3, 5 and 6; `artifact_name`'s variant is the fifth positional argument everywhere (`exe setup`); `legacy_alias_pairs` columns (`subdir legacy current`) match Task 6's loop; `gen_release_post.sh --tagline` is used by `docs_publish.sh`; the mock `curl` env names (`MOCK_CURL_ARTIFACT_LENGTH`, `MOCK_CURL_MISSING_ARTIFACTS`, `MOCK_CURL_HUB_BODY_FILE`, `MOCK_CURL_HUB_HTTP`, `MOCK_CURL_BLOG_HTTP`, `MOCK_CURL_RELEASE_HTTP`, `MOCK_CURL_CAPTURE_RELEASE_BODY`, `MOCK_CURL_GITHUB_*`, `MOCK_CURL_COUNTER_FILE`) match between the mock additions and the tests; the GitHub job names in `GATE_REQUIRED_CHECKS` match `ci.yml` and Task 8 does not rename them.

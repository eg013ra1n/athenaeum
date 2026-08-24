# Open-sourcing Athenaeum on GitHub — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish Athenaeum at `github.com/eg013ra1n/athenaeum` as a buildable, contributable open-source project, with GitLab keeping the release pipeline and a contributor's merged PR reaching the local environment in one `git pull`.

**Architecture:** One git history, two remotes — the same commits and SHAs live on the private GitLab and on the public GitHub, so there is no mirror, no filter and no second lineage. `main` becomes the development trunk and releases become tags on it. The `solvemyastro` submodule is published alongside `rustafits` so a recursive clone builds with no access to the private network.

**Tech Stack:** git (submodules, remotes), `gh` CLI (authenticated as `eg013ra1n`), `gitleaks` (to be installed via Homebrew), GitHub Actions, Cargo workspace pinned to Rust 1.96.1, Node/npm.

**Spec:** `docs/superpowers/specs/2026-08-24-open-sourcing-github-design.md`

## Global Constraints

- **Publication is irreversible.** No push to GitHub happens before Task 2's secrets gate is clean on both repositories. Tasks 1 and 3-7 touch only the local repository and the private GitLab.
- Public owner is `github.com/eg013ra1n` — beside the already-public `eg013ra1n/rustafits`.
- Licence stays Apache-2.0. No CLA: under §5 contributions arrive under the same licence.
- Only `main` is pushed to GitHub. The 25 dead local branches, `feature/stacking-prep` and `wip/pure-rust-jpeg` stay GitLab-only.
- Rust toolchain is pinned at `1.96.1` by `rust-toolchain.toml`; do not add a second version anywhere.
- Every commit is authored by `eg013ra1n <vilen.sharifov@gmail.com>`. Never add Claude as author or co-author.
- Markdown tables use spaced separators (`| ---- |`) for MD060/Obsidian.
- The three project gates are `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`. Clippy is not a gate.

## Execution log — 2026-08-24

Executed in one session. Deviations from the plan as written, and why:

| # | Deviation | Reason |
| ---- | ---- | ---- |
| 1 | Task 8 does not create the repository — `eg013ra1n/athenaeum` already existed, public since 2025-10-27, carrying an unrelated line of history (tip `b6f52d4f`, 6 stars, 1 fork, 5 divergent tags). Force-pushed over it by decision. | Discovered while retiring the auto-memory rule that said the github remote was unmaintained. `gh repo create` would have failed. |
| 2 | Task 1 grew a step retiring the rule in auto-memory, and a second memory (`feedback_gitlab_only.md`, "never push to GitHub") had to be rewritten too. | Both would have reinstated the superseded model in later sessions. |
| 3 | Task 2 skipped the `gitleaks dir` working-tree scan and relies on `gitleaks git`, which covers `HEAD`. | `gitleaks dir .` descends into `target/` (hundreds of GB here), `node_modules/` and `builds/` — none of it tracked or published. It ran for over 10 minutes without finishing and was killed. |
| 4 | Task 5 dropped `dtolnay/rust-toolchain@stable` in favour of `rustup show active-toolchain`. | The action installs stable and would have shadowed the `1.96.1` pin, or forced the version to be written a second time — against this plan's own global constraint. |
| 5 | Task 4 acquired an extra step restoring solvemyastro's GitLab remote. | `git submodule sync` rewrites the submodule's own `origin`. It silently repointed it from GitLab to GitHub, which would have orphaned the private copy. It is now `origin` = GitHub, `gitlab` = GitLab, `all` = both. |
| 6 | Task 4's rebuild step was not re-run. | The successful `cargo build --workspace` already ran after the `solvemyastro/Cargo.toml` edit; the only later change was `.gitmodules`, which cargo never reads. |
| 7 | Task 8's `git gc` moved from before the push to after it. | A push transfers only reachable objects, so the repack bought nothing beforehand while locking a 4 GB repository for minutes. |
| 8 | Task 7 additionally removed the README's claim of a metadata token-template engine (`{OBJECT}`, `{DATE-OBS:%Y-%m-%d}`). | No such engine exists — `CLAUDE.md` records the tokens and the `export_templates` table as doc and schema leftovers. Publishing it would have advertised a feature the app does not have. |

Gate evidence at publication time: `cargo build --workspace` finished clean;
`cargo test -p athenaeum-core` — 1600 passed, 0 failed, 13 ignored across 11 test
binaries; `npx tsc --noEmit` clean; gitleaks clean on both repositories.

---

## File Structure

| Path | Status | Responsibility |
| ---- | ---- | ---- |
| `docs/superpowers/research/2026-08-24-pre-publication-secrets-audit.md` | Create | The recorded result of the publication gate — what was scanned, with what, and what was found. |
| `solvemyastro/Cargo.toml` | Modify | `repository` field points at the public GitHub repo instead of the LAN GitLab. |
| `.gitmodules` | Modify | `solvemyastro` URL points at GitHub so a recursive clone works off-network. |
| `.github/workflows/ci.yml` | Create | The PR gate: build, test and typecheck on Ubuntu. The only automation GitHub runs. |
| `CONTRIBUTING.md` | Create | The cardinal rules a contributor must follow, externalized from `CLAUDE.md`. |
| `SECURITY.md` | Create | Private vulnerability reporting channel. |
| `CODE_OF_CONDUCT.md` | Create | Contributor Covenant 2.1. |
| `.github/ISSUE_TEMPLATE/bug_report.yml` | Create | Bug form that collects version, OS and the JSONL log the `/bug-triage` flow consumes. |
| `.github/ISSUE_TEMPLATE/feature_request.yml` | Create | Feature form. |
| `.github/ISSUE_TEMPLATE/config.yml` | Create | Points general questions at Discussions rather than the issue tracker. |
| `.github/PULL_REQUEST_TEMPLATE.md` | Create | Checklist mirroring the cardinal rules. |
| `README.md` | Modify | The front door: accurate prerequisites, recursive clone, real crate layout, both build targets. |
| `.github-export-ignore` | Delete | Describes the abandoned filtered-mirror plan; already stale in two paths. |
| `CLAUDE.md` | Modify | Release workflow section switches from version branches to trunk + tags. |

---

### Task 1: Trunk transition — fast-forward `main` to `0.5.1`

Nothing else can be planned around `main` until `main` is current. `main` is 0 commits ahead of `0.5.1` and `0.5.1` is 700 ahead, so this is a pure fast-forward with no merge commit and no conflict. All 23 release tags are already ancestors of `0.5.1`, so no tag is orphaned by the move.

Everything after this task is committed on `main`.

**Files:**
- Modify: `CLAUDE.md` (the `## Release workflow` section, step 3)

**Interfaces:**
- Consumes: nothing.
- Produces: `main` at the same commit as `0.5.1`; every later task commits on `main`.

- [ ] **Step 1: Confirm the fast-forward is clean before moving**

```bash
git status --porcelain          # expect: empty
git rev-list --left-right --count main...0.5.1
```

Expected: `0	700` — `main` is behind by 700 and ahead by nothing.

- [ ] **Step 2: Fast-forward `main`**

```bash
git checkout main
git merge --ff-only 0.5.1
```

Expected: `Fast-forward` in the output, no merge commit, no editor.

- [ ] **Step 3: Verify the branches now agree**

```bash
git rev-list --left-right --count main...0.5.1
```

Expected: `0	0`.

- [ ] **Step 4: Rewrite the release workflow section in `CLAUDE.md`**

Replace step 3 of the `## Release workflow` section. The old text tells the reader to commit on a version branch and ff-merge to `main` at release. Replace it with:

```markdown
3. One commit `chore(release): vX.Y.Z — …` on `main`; gates (`cargo build --workspace`, core tests, `npx tsc --noEmit`); tag `vX.Y.Z`; push `main` and the tag to both remotes (`git push all main && git push all vX.Y.Z`). The tag pipeline then: builds ×3 platforms → uploads to `artfrom.space/builds/<tag>/` (+ `latest` symlink + stable-named aliases) → GitLab Release from RELEASE_NOTES.md → publishes `version.json` → Discord/Telegram notifications.
```

Then add this paragraph directly beneath the numbered list:

```markdown
**Branching.** `main` is the development trunk and releases are tags on it. This
replaces the older "develop on a branch named after the version, ff-merge at
release" rule, which left `main` hundreds of commits stale — unworkable once
`main` is the default branch outside contributors base their pull requests on.
A release branch is cut only if a backport is ever actually needed.
```

- [ ] **Step 5: Verify no other section still teaches the old rule**

```bash
grep -n "version branch" CLAUDE.md
```

Expected: no hit that instructs development on a version branch. If one remains, fix it in this step.

- [ ] **Step 6: Retire the superseded rule in auto-memory**

The rule also lives outside the repository, at
`~/.claude/projects/-Volumes-BigMac-Users-astrobureau-Documents-Projects-athenaeum/memory/feedback_version_branches.md`,
where it is loaded into every session. Leaving it there would keep reinstating
the model this task just replaced. Rewrite its body to state the trunk model —
`main` is the development trunk, releases are tags on it, pull requests target
`main` — keeping the frontmatter `name`, `description` and `metadata.type`
fields intact, and update the description line for it in `MEMORY.md`.

```bash
grep -n "version branch\|version_branches"   ~/.claude/projects/-Volumes-BigMac-Users-astrobureau-Documents-Projects-athenaeum/memory/MEMORY.md
```

Expected after the edit: the index line describes trunk-based development, not
version branches.

- [ ] **Step 7: Commit and push to GitLab**

```bash
git add CLAUDE.md
git commit -m "docs: main is the development trunk, releases are tags

Supersedes the version-branch rule. Under it main trailed the active branch by
700 commits, which is unworkable once main is the default branch that outside
contributors base pull requests on."
git push origin main
```

- [ ] **Step 8: Verify GitLab accepted it and the pipeline behaves**

```bash
git rev-list --left-right --count origin/main...main
```

Expected: `0	0`.

A push to `main` admits a pipeline (`workflow.rules`), but every build job is `only: tags`; the single job that runs is `check:headless-core`. Confirm in the GitLab UI that only that job started and that it passes. If the whole build matrix started, stop and report — `workflow.rules` has drifted from what this plan assumes.

---

### Task 2: Publication gate — secrets audit of both repositories

The one hard gate. It covers full history, not the working tree: a credential committed once and deleted later is still reachable in the pack. Nothing may be pushed to GitHub until this task is green.

**Files:**
- Create: `docs/superpowers/research/2026-08-24-pre-publication-secrets-audit.md`

**Interfaces:**
- Consumes: `main` from Task 1.
- Produces: a committed audit record; the go/no-go signal for Task 3 and Task 8.

- [ ] **Step 1: Install gitleaks**

```bash
brew install gitleaks
gitleaks version
```

Expected: a version string. If Homebrew is unavailable, stop and report rather than substituting a different scanner — the audit record names the tool and its version.

- [ ] **Step 2: Scan the athenaeum working tree**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
gitleaks detect --no-git --redact --verbose 2>&1 | tail -20
```

Expected: `no leaks found`. Any finding is triaged in Step 6 before continuing.

- [ ] **Step 3: Scan the athenaeum full history**

```bash
gitleaks detect --redact --verbose 2>&1 | tail -20
```

Expected: `no leaks found`. This walks every commit, so it takes noticeably longer than Step 2.

- [ ] **Step 4: Scan solvemyastro, tree and history**

```bash
cd solvemyastro
gitleaks detect --no-git --redact --verbose 2>&1 | tail -10
gitleaks detect --redact --verbose 2>&1 | tail -10
cd ..
```

Expected: `no leaks found` from both.

- [ ] **Step 5: Confirm the rustafits pin is already public**

rustafits is published, but the superproject pin must exist on the public remote or `clone --recursive` breaks for everyone.

```bash
git -C rustafits ls-remote origin | grep -c 72aca7cfbfc5f7824236cbff947efde0c488d36b
```

Expected: `1` or more — the pinned commit is advertised by the public remote.

- [ ] **Step 6: Triage any finding before writing the record**

If any scan reported something, do not proceed. For each finding decide, and write the decision down: a real credential means publication stops until it is rotated and the history is rewritten; a false positive (a test fixture, a public key, a sample token) is recorded as such with the file and line. Only an all-clear or a fully-triaged list of false positives lets Task 3 start.

- [ ] **Step 7: Write the audit record**

Create `docs/superpowers/research/2026-08-24-pre-publication-secrets-audit.md`:

```markdown
# Pre-publication secrets audit

Date: 2026-08-24
Scope: `athenaeum` and `solvemyastro`, working tree and full history.
Gate for: `docs/superpowers/specs/2026-08-24-open-sourcing-github-design.md`

Publication is irreversible — forks, archives and GitHub's caches keep whatever
is pushed. This is the record that the gate was actually run.

## Tooling

- `gitleaks` <version from Step 1>, default ruleset.
- Preliminary hand scans (recorded here because they cover a different axis than
  gitleaks' rules):
  - Filename scan over all 18 080 reachable objects for `.env`, `.p12`, `.pem`,
    `.p8`, `.key`, `.pfx`, `id_rsa`, `id_ed25519`, `.netrc`, `.kdbx`,
    `.keychain`. One hit, `crates/athenaeum-core/src/account/token_store.rs` —
    a source file, not a credential.
  - Content scan over all reachable blob content (~264 MB) for private-key
    headers, `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`, `glpat-`, `xox[baprs]-`,
    `AKIA…`, Telegram bot tokens, `sk-…` and Discord webhook URLs. Zero matches.

## Results

| Scan | Result |
| ---- | ---- |
| athenaeum, working tree | <fill in> |
| athenaeum, full history | <fill in> |
| solvemyastro, working tree | <fill in> |
| solvemyastro, full history | <fill in> |
| rustafits pin `72aca7c` advertised by the public remote | <fill in> |

## Accepted disclosures

Decided in the design, section 3. Do not re-flag these.

- `192.168.31.208:9080` — the LAN address of the private GitLab, in
  `.gitmodules`, `solvemyastro/Cargo.toml` and docs. Not routable from outside;
  the two real URLs are repointed to GitHub in Tasks 3 and 4.
- `.gitlab-ci.yml` and `.gitlab/` reveal the deploy host, artefact paths, the
  Docker Hub repository and the notification scripts. All credentials are CI
  variables; the layout is published on purpose.
- Author identities `sharifov.v@mail366.com` (78 commits) and
  `Administrator <gitlab_admin_ba4842@example.com>` (3 commits) become public.
- Client-side service endpoints: `test-hub.artfrom.space` (debug) and
  `projects.artfrom.space` (release) in `settings/mod.rs`, the relay hosts in
  `examples/relay_check.rs`, `artfrom.space/catalogs/` in
  `catalog/gaia_prebuilt.rs`. The server code behind them is not published.
```

Replace every `<fill in>` and `<version from Step 1>` with the real values from Steps 1-5.

- [ ] **Step 8: Verify no placeholder survived**

```bash
grep -n "fill in\|version from Step" docs/superpowers/research/2026-08-24-pre-publication-secrets-audit.md
```

Expected: no output.

- [ ] **Step 9: Commit**

```bash
git add docs/superpowers/research/2026-08-24-pre-publication-secrets-audit.md
git commit -m "docs: pre-publication secrets audit — gate for open-sourcing

gitleaks over tree and full history of athenaeum and solvemyastro, plus the
hand scans for credential-shaped filenames and blob content. Records the
accepted disclosures so they are not re-flagged."
git push origin main
```

---

### Task 3: Publish solvemyastro

`solvemyastro` is a workspace member and a path dependency of `athenaeum-tauri`, `athenaeum-web` and `catalog-builder`. Its submodule URL points at the LAN GitLab, so today no outside clone can build anything at all. This is the only real blocker.

The metadata edit comes first so that a single push publishes the final state and the superproject pin moves exactly once.

**Files:**
- Modify: `solvemyastro/Cargo.toml` (the `repository` field, line 8)

**Interfaces:**
- Consumes: a green Task 2.
- Produces: `github.com/eg013ra1n/solvemyastro` public, with a commit SHA that Task 4 pins.

- [ ] **Step 1: Confirm the submodule is clean and on `main`**

```bash
cd solvemyastro
git status --porcelain          # expect: empty
git rev-parse --abbrev-ref HEAD # expect: main
git rev-list --left-right --count origin/main...main
```

Expected: `0	0`.

- [ ] **Step 2: Repoint the crate's `repository` field**

In `solvemyastro/Cargo.toml`, replace:

```toml
repository = "http://192.168.31.208:9080/root/solvemyastro.git"
```

with:

```toml
repository = "https://github.com/eg013ra1n/solvemyastro"
```

- [ ] **Step 3: Verify the edit and that the crate still parses**

```bash
grep -n '^repository' Cargo.toml
cargo metadata --no-deps --format-version 1 > /dev/null && echo "manifest ok"
```

Expected: the GitHub URL, then `manifest ok`.

- [ ] **Step 4: Commit inside the submodule and push to GitLab**

```bash
git add Cargo.toml
git commit -m "chore: point repository metadata at the public GitHub repo"
git push origin main
```

- [ ] **Step 5: Create the public GitHub repository**

```bash
gh repo create eg013ra1n/solvemyastro \
  --public \
  --description "Plate solver for astrophotography — gnomonic projection, quad matching, SIP/affine fitting" \
  --disable-wiki
```

Expected: the new repository URL printed. `gh` is already authenticated as `eg013ra1n`.

- [ ] **Step 6: Add the GitHub remote and push `main` only**

`feature/stacking-prep` and `phase0-hygiene` stay private, matching the superproject rule that only `main` is published.

```bash
git remote add github https://github.com/eg013ra1n/solvemyastro.git
git push github main
```

- [ ] **Step 7: Verify the new HEAD is advertised publicly**

```bash
NEW_SHA=$(git rev-parse HEAD)
echo "local HEAD: $NEW_SHA"
git ls-remote https://github.com/eg013ra1n/solvemyastro.git refs/heads/main
```

Expected: the remote's `refs/heads/main` SHA equals `$NEW_SHA`. Record `$NEW_SHA` — Task 4 pins it.

- [ ] **Step 8: Verify an anonymous clone works**

```bash
cd "$(mktemp -d)"
git clone --depth 1 https://github.com/eg013ra1n/solvemyastro.git && echo "anonymous clone ok"
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
```

Expected: `anonymous clone ok`.

---

### Task 4: Repoint the submodule in the superproject

**Files:**
- Modify: `.gitmodules` (the `solvemyastro` URL)

**Interfaces:**
- Consumes: the public repository and `$NEW_SHA` from Task 3.
- Produces: a superproject whose recursive clone needs no LAN access.

- [ ] **Step 1: Repoint the URL**

In `.gitmodules`, replace:

```
	url = http://192.168.31.208:9080/root/solvemyastro.git
```

with:

```
	url = https://github.com/eg013ra1n/solvemyastro.git
```

- [ ] **Step 2: Propagate the URL into `.git/config`**

```bash
git submodule sync -- solvemyastro
git config --get submodule.solvemyastro.url
```

Expected: the GitHub URL. Without `sync`, `.git/config` keeps the LAN URL and later fetches silently keep using it.

- [ ] **Step 3: Verify both submodule pins are publicly reachable**

```bash
git submodule status
git ls-remote https://github.com/eg013ra1n/solvemyastro.git | grep "$(git -C solvemyastro rev-parse HEAD)"
git ls-remote https://github.com/eg013ra1n/rustafits.git    | grep "$(git -C rustafits rev-parse HEAD)"
```

Expected: each `ls-remote` prints a matching line. A pin that exists only locally breaks `clone --recursive` for every contributor.

- [ ] **Step 4: Verify the workspace still builds against the moved submodule**

```bash
cargo build --workspace 2>&1 | tail -5
```

Expected: `Finished` with no error. The submodule content did not change, only its metadata and URL, so this confirms the pin bump is inert.

- [ ] **Step 5: Commit the URL and the pin together**

```bash
git add .gitmodules solvemyastro
git commit -m "chore: point the solvemyastro submodule at its public GitHub repo

The LAN GitLab URL made a recursive clone impossible for anyone outside the
network, and solvemyastro is a hard workspace member — nothing built at all."
git push origin main
```

- [ ] **Step 6: Verify the GitLab runner can still fetch the submodule**

The pipeline checks out with `GIT_SUBMODULE_STRATEGY: recursive`. It previously reached the submodule over the LAN and must now reach github.com. Watch the `check:headless-core` job triggered by Step 5's push and confirm it does not fail at the checkout stage. If it does, the runner has no outbound access to github.com and that must be fixed before Task 8.

---

### Task 5: GitHub Actions PR gate

The gate a contributor sees on their pull request. Ubuntu only — the three-platform matrix stays on GitLab. It needs no secrets, so it runs on pull requests from forks.

**Files:**
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `rust-toolchain.toml` (pins `1.96.1`), `package-lock.json` (makes `npm ci` reproducible).
- Produces: the required-status-check surface for pull requests.

- [ ] **Step 1: Create the workflow**

```yaml
name: CI

on:
  pull_request:
  push:
    branches: [main]

concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: true

jobs:
  build-and-test:
    name: Build, test and typecheck
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive

      # Tauri 2 Linux prerequisites. The GitLab runners have these provisioned
      # on the host, so this list is the only place in the repository where the
      # dependency set is written down.
      - name: Install Tauri system dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends \
            libwebkit2gtk-4.1-dev \
            libgtk-3-dev \
            libayatana-appindicator3-dev \
            librsvg2-dev \
            libxdo-dev \
            libssl-dev \
            build-essential \
            curl wget file

      # No toolchain input: the action reads rust-toolchain.toml, so CI can
      # never drift from the pinned 1.96.1.
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: npm

      - name: Install npm dependencies
        run: npm ci

      - name: Build workspace
        run: cargo build --workspace

      - name: Test core
        run: cargo test -p athenaeum-core

      - name: Typecheck frontend
        run: npx tsc --noEmit
```

- [ ] **Step 2: Verify the YAML parses**

```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml')); print('yaml ok')"
```

Expected: `yaml ok`. If PyYAML is missing, `pip3 install pyyaml` first.

- [ ] **Step 3: Verify the three gates pass locally, exactly as the workflow runs them**

```bash
cargo build --workspace 2>&1 | tail -3
cargo test -p athenaeum-core 2>&1 | tail -5
npx tsc --noEmit && echo "tsc clean"
```

Expected: `Finished`, a passing test summary, and `tsc clean`. A gate that is red locally will be red on every contributor's first pull request — fix it here, not after publication.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: GitHub Actions pull-request gate

Build, core tests and frontend typecheck on Ubuntu. No secrets, so it runs on
fork pull requests. The release matrix stays on GitLab."
git push origin main
```

---

### Task 6: Contributor-facing scaffolding

What turns a readable repository into a contributable one. All of these are new files except the deletion.

**Files:**
- Create: `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`
- Create: `.github/ISSUE_TEMPLATE/bug_report.yml`, `.github/ISSUE_TEMPLATE/feature_request.yml`, `.github/ISSUE_TEMPLATE/config.yml`, `.github/PULL_REQUEST_TEMPLATE.md`
- Delete: `.github-export-ignore`

**Interfaces:**
- Consumes: the cardinal rules as stated in `CLAUDE.md`.
- Produces: the documents the PR template and README link to.

- [ ] **Step 1: Write `CONTRIBUTING.md`**

```markdown
# Contributing to Athenaeum

Thanks for wanting to help. Athenaeum manages astrophotography catalogs, so most
bugs are found with real data — a FITS file that parses wrong is worth more than
a synthetic test that passes.

## Getting set up

```bash
git clone --recursive https://github.com/eg013ra1n/athenaeum.git
cd athenaeum
npm install
npm run tauri dev
```

`--recursive` matters: `rustafits` (image rendering) and `solvemyastro` (plate
solving) are submodules and workspace members. Without them nothing builds. If
you already cloned without it, run `git submodule update --init --recursive`.

The Rust toolchain is pinned in `rust-toolchain.toml`; rustup will fetch the
right version for you.

## The rules that matter

These are not style preferences. A pull request that breaks one of them will be
asked to change.

**Two backends stay in sync.** There are two hosts for the same logic: the Tauri
desktop shell (`crates/athenaeum-tauri/src/commands/<domain>.rs`) and the Axum
web server (`crates/athenaeum-web/src/routes/<domain>.rs`). They mirror each
other one-for-one. Adding or changing a command in one requires the matching
change in the other, in the same pull request. Real logic belongs in
`athenaeum-core`; both hosts are thin wrappers over it.

**The frontend never imports Tauri directly.** No `@tauri-apps/*` import outside
`src/api/`. The frontend talks to whichever backend is active through the single
`api` object, selected by `VITE_TARGET`. Desktop-only code lives in
`src/api/desktop.ts`.

**Mind the serde boundary.** Rust is snake_case, TypeScript is camelCase. Use
`#[serde(rename_all = "camelCase")]` and check that the interface in
`src/types/models.ts` matches the Rust struct. Most "the value is undefined"
bugs are a casing mismatch, not a logic error.

**Never swallow an error.** Log to console or stderr before returning it.
Silent failures have repeatedly cost hours here. Inside `athenaeum-core` use
`anyhow::Result`; convert with `.map_err(|e| e.to_string())` at the command
boundary.

**Use design tokens, not raw colours.** `bg-surface`, `text-content-muted`,
`bg-accent`, `text-error` and friends — so both the dark and light themes keep
working.

**Log through `tracing`.** No `println!` or `eprintln!` in production code (CLI
binaries and tests are exempt). A message is a short stable phrase with the data
in snake_case fields: `info!(root_id, new = 12, "scan finished")`, never
`info!("scan finished — 12 new")`.

## Before you open a pull request

Run the three gates:

```bash
cargo build --workspace
cargo test -p athenaeum-core
npx tsc --noEmit
```

CI runs exactly these on Ubuntu. Clippy is not a gate.

## Commits and branches

`main` is the development trunk; releases are tags on it. Base your branch on
`main` and open the pull request against `main`.

Commit messages follow Conventional Commits — `feat:`, `fix:`, `docs:`,
`chore:`, `perf:`, `refactor:`, with an optional scope: `fix(calibration): …`.

## Repository layout

| Path | What lives there |
| ---- | ---- |
| `crates/athenaeum-core/` | All non-IPC logic: DB, FITS parsing, calibration, scanner, archive, export, plate solving, sync |
| `crates/athenaeum-tauri/` | Desktop shell; `commands/` thinly wraps core |
| `crates/athenaeum-web/` | Axum HTTP/SSE server; `routes/` mirrors the Tauri commands |
| `crates/perseus/` | Capture-agent CLI for observatory machines |
| `rustafits/` | Submodule — FITS/XISF image rendering |
| `solvemyastro/` | Submodule — plate solver |
| `src/` | React/TypeScript frontend |
| `docs/superpowers/specs/` | Design documents for the larger subsystems |

`CLAUDE.md` at the repository root is the long-form architecture reference —
subsystem by subsystem, with the invariants. It is written as guidance for an
agent working in the repository, but it is the most complete description of how
the system fits together and is worth reading before a substantial change.

## Reporting bugs

Use the bug report template. Attaching the JSONL log makes triage far faster —
Settings → Logging shows where the log directory is.
```

- [ ] **Step 2: Write `SECURITY.md`**

```markdown
# Security Policy

## Supported versions

Athenaeum is pre-1.0 and moves quickly. Only the latest release gets fixes.

## Reporting a vulnerability

Please do not open a public issue.

Use GitHub's private reporting — the **Security** tab → **Report a
vulnerability** — or email <vilen.sharifov@gmail.com>.

Include what you did, what happened, and what you expected. A proof of concept
helps but is not required.

You can expect an acknowledgement within a few days. Once a fix ships you are
credited in the release notes unless you would rather not be.

## Areas worth extra scrutiny

- `crates/athenaeum-core/src/sharing/` — the iroh peer-to-peer transport and its
  wire protocol.
- `crates/athenaeum-core/src/sync/` — device-to-device transfers, including what
  a peer is allowed to send and where it lands on disk.
- `crates/athenaeum-core/src/account/` — account tokens and their storage.
- `crates/athenaeum-web/` — the HTTP surface, when it is exposed beyond
  localhost.
- Path handling in `file_op/`, `archive/` and `scanner/` — these move and delete
  real files.
```

- [ ] **Step 3: Write `CODE_OF_CONDUCT.md`**

Use Contributor Covenant 2.1 verbatim from <https://www.contributor-covenant.org/version/2/1/code_of_conduct/>, with `vilen.sharifov@gmail.com` as the enforcement contact.

- [ ] **Step 4: Write `.github/ISSUE_TEMPLATE/bug_report.yml`**

```yaml
name: Bug report
description: Something behaves incorrectly
labels: ["bug"]
body:
  - type: markdown
    attributes:
      value: |
        Attaching the JSONL log makes triage far faster. Settings → Logging
        shows the log directory; the newest `athenaeum-desktop.*` or
        `athenaeum-web.*` file covers the session where it happened.

  - type: textarea
    id: what-happened
    attributes:
      label: What happened
      description: What you did, what happened, and what you expected instead.
    validations:
      required: true

  - type: input
    id: version
    attributes:
      label: Athenaeum version
      placeholder: "0.5.1"
    validations:
      required: true

  - type: dropdown
    id: platform
    attributes:
      label: Platform
      options:
        - macOS (Apple Silicon)
        - macOS (Intel)
        - Windows
        - Linux (AppImage)
        - Linux (DEB)
        - Docker / web
    validations:
      required: true

  - type: dropdown
    id: file-format
    attributes:
      label: Which file format is involved
      options:
        - "Not file-specific"
        - FITS
        - XISF
        - Both
    validations:
      required: true

  - type: textarea
    id: logs
    attributes:
      label: Log
      description: Paste the relevant JSONL lines, or attach the file.
      render: text
```

- [ ] **Step 5: Write `.github/ISSUE_TEMPLATE/feature_request.yml`**

```yaml
name: Feature request
description: Suggest a capability or an improvement
labels: ["enhancement"]
body:
  - type: textarea
    id: problem
    attributes:
      label: What problem are you trying to solve
      description: Describe the situation in your workflow, not the solution.
    validations:
      required: true

  - type: textarea
    id: proposal
    attributes:
      label: What you have in mind
    validations:
      required: false

  - type: textarea
    id: workaround
    attributes:
      label: How you handle it today
      description: Another tool, a manual step, or nothing at all.
    validations:
      required: false
```

- [ ] **Step 6: Write `.github/ISSUE_TEMPLATE/config.yml`**

```yaml
blank_issues_enabled: false
contact_links:
  - name: Documentation
    url: https://artfrom.space
    about: Guides, manuals and release downloads.
  - name: Questions and ideas
    url: https://github.com/eg013ra1n/athenaeum/discussions
    about: For anything that is not a bug report or a concrete feature request.
```

- [ ] **Step 7: Write `.github/PULL_REQUEST_TEMPLATE.md`**

```markdown
## What this changes

<!-- One or two sentences. Link the issue if there is one. -->

## Checklist

- [ ] If a Tauri command changed, the matching Axum route in `crates/athenaeum-web/src/routes/` changed too (and vice versa)
- [ ] Logic lives in `athenaeum-core`; the command and route layers stay thin wrappers
- [ ] No `@tauri-apps/*` import outside `src/api/`
- [ ] New or changed structs crossing the boundary use `#[serde(rename_all = "camelCase")]`, and `src/types/models.ts` matches
- [ ] No new colour literals — design tokens only
- [ ] Errors are logged before being returned; no `println!`/`eprintln!` in production code
- [ ] `cargo build --workspace` passes
- [ ] `cargo test -p athenaeum-core` passes
- [ ] `npx tsc --noEmit` passes

## How it was tested

<!-- Real FITS/XISF data if the change touches parsing, calibration or scanning. -->
```

- [ ] **Step 8: Delete the stale mirror config**

`.github-export-ignore` describes the abandoned filtered-mirror plan and is already stale in two paths: it names `scripts/sync-github.sh`, which does not exist, and `src-tauri/REFACTORING.md`, a pre-refactor path.

```bash
git rm .github-export-ignore
```

- [ ] **Step 9: Verify the templates parse and nothing dangles**

```bash
for f in .github/ISSUE_TEMPLATE/bug_report.yml \
         .github/ISSUE_TEMPLATE/feature_request.yml \
         .github/ISSUE_TEMPLATE/config.yml; do
  python3 -c "import yaml,sys; yaml.safe_load(open('$f')); print('ok $f')"
done
grep -rn "sync-github.sh\|github-export-ignore" --exclude-dir=.git . || echo "no dangling references"
```

Expected: three `ok` lines, then `no dangling references`.

- [ ] **Step 10: Commit**

```bash
git add CONTRIBUTING.md SECURITY.md CODE_OF_CONDUCT.md .github/
git commit -m "docs: contributor scaffolding for the public repository

CONTRIBUTING with the cardinal rules, SECURITY with a private reporting
channel, Contributor Covenant, issue and pull-request templates. Drops the
stale .github-export-ignore from the abandoned filtered-mirror plan."
git push origin main
```

---

### Task 7: Bring the README up to date

The README is the front door and it currently misdescribes the project. It documents a `src-tauri/` layout that no longer exists, claims "68+ commands" against an actual 232 across 23 modules, names Node 18 and Rust 1.70 against a pinned 1.96.1, says the export engine resolves path templates when there is no templating engine, and — the part that actually breaks people — never mentions that the clone must be recursive.

**Files:**
- Modify: `README.md` — the `## Building from Source`, `## Project Structure` and `## Architecture` sections, plus two `## Features` additions

**Interfaces:**
- Consumes: `CONTRIBUTING.md` from Task 6 (linked from the build section).
- Produces: the landing page for every visitor.

- [ ] **Step 1: Replace `## Building from Source` entirely**

```markdown
## Building from Source

### Clone

```bash
git clone --recursive https://github.com/eg013ra1n/athenaeum.git
```

`--recursive` is required: [rustafits](https://github.com/eg013ra1n/rustafits)
(image rendering) and
[solvemyastro](https://github.com/eg013ra1n/solvemyastro) (plate solving) are
submodules and Cargo workspace members. Without them the workspace does not
build. Already cloned flat? Run `git submodule update --init --recursive`.

### Prerequisites

- Node.js 22.12 or newer, and npm
- Rust — the toolchain is pinned in `rust-toolchain.toml`, so rustup installs
  the right version automatically
- Platform-specific Tauri prerequisites: <https://tauri.app/start/prerequisites/>

On Debian or Ubuntu that means:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev \
  build-essential curl wget file
```

### Desktop

```bash
npm install
npm run tauri dev      # hot-reload development build
npm run tauri build    # packaged application
```

### Web and Docker

The same core, served over HTTP with SSE instead of Tauri IPC:

```bash
npm run dev:web              # frontend, VITE_TARGET=web
cargo run -p athenaeum-web   # backend, in a second terminal
```

A multi-stage `docker/Dockerfile` builds the containerized version. The catalog
database lives in the OS application-data directory on desktop, and in `/data`
(or `$ATHENAEUM_DB_PATH`) under Docker.

### Checks

```bash
cargo build --workspace
cargo test -p athenaeum-core
npx tsc --noEmit
```

These three are what CI runs. See [CONTRIBUTING.md](./CONTRIBUTING.md) before
opening a pull request.
```

- [ ] **Step 2: Replace `## Project Structure` entirely**

```markdown
## Project Structure

```text
athenaeum/
├── crates/
│   ├── athenaeum-core/           # Shared library — all non-IPC logic
│   │   └── src/
│   │       ├── db/               # SQLite schema and operations
│   │       ├── fits_parser/      # FITS/XISF metadata extraction
│   │       ├── scanner/          # Multi-threaded directory traversal
│   │       ├── clustering/       # Sky-coordinate frame-set grouping
│   │       ├── calibration/      # Calibration matching engine and config
│   │       ├── calibration_library/  # Master creation and light calibration
│   │       ├── integration/      # Banded frame integration and combiners
│   │       ├── plate_solve/      # Plate-solving adapter
│   │       ├── archive/          # ZIP archive lifecycle
│   │       ├── file_op/          # Move pipeline with cross-volume verify
│   │       ├── export/           # WBPP folder/keyword export
│   │       ├── sync/             # Device-to-device transfers
│   │       ├── sharing/          # iroh transport and wire protocol
│   │       └── services/         # ServiceContext, queues, ProgressEmitter
│   ├── athenaeum-tauri/          # Desktop shell — commands/ wraps core
│   ├── athenaeum-web/            # Axum HTTP/SSE server — routes/ mirrors commands
│   ├── perseus/                  # Capture-agent CLI for observatory machines
│   ├── catalog-builder/          # Star-catalog build tool
│   └── log-mcp/                  # Log query server for development
├── rustafits/                    # Submodule — FITS/XISF image rendering
├── solvemyastro/                 # Submodule — plate solver
├── src/                          # React frontend
│   ├── api/                      # The only place Tauri IPC or HTTP is touched
│   ├── components/               # UI components
│   ├── pages/                    # One per view
│   ├── hooks/                    # Custom React hooks
│   └── types/                    # TypeScript mirrors of the Rust models
└── docs/                         # Design documents and references
```
```

- [ ] **Step 3: Replace the `## Architecture` paragraph**

```markdown
## Architecture

Athenaeum runs on two backends over one shared library. `athenaeum-core` holds
everything that is not transport: the SQLite catalog, FITS/XISF parsing, frame-set
clustering, calibration matching, master creation, archiving, export and
peer-to-peer transfers. The desktop shell is **Tauri 2**, exposing 232 commands
across 23 domain modules over Tauri's IPC; the web build is an **Axum** server
whose routes mirror those commands one-for-one and stream progress over SSE. The
React frontend reaches whichever is active through a single `api` object, so no
component knows which host it is running under.

Image rendering is [rustafits](https://github.com/eg013ra1n/rustafits), a pure
Rust FITS/XISF library — Bayer demosaicing, auto-stretch and multi-resolution
JPEG output with no C dependencies. Plate solving is
[solvemyastro](https://github.com/eg013ra1n/solvemyastro), using quad matching
against a Gaia-derived star catalog.
```

- [ ] **Step 4: Correct the stale claim in `### Export`**

The current text describes path-template resolution. There is no templating
engine — the export is WBPP folder and keyword organization. Replace the
`### Export` body with:

```markdown
Export for WBPP: files are organized into the folder hierarchy and keyword
layout PixInsight's WeightedBatchPreprocessing expects, with symlinks instead of
copies where the platform supports them.
```

- [ ] **Step 5: Add the two shipped features the list is missing**

After `### Calibration Library`, insert:

```markdown
### Master Calibration Library

Build master darks, flats, bias and dark-flats in-app from a matched raw
calibration set — no external stacker. Masters register into the catalog exactly
as a scanned file would, every consumer relinks onto them automatically, and the
originals can be archived in the same step. Calibrated lights are written as
32-bit float FITS that WBPP or Siril consume with their own calibration step
disabled.

### Plate Solving

Blind and hinted solving against a tiered Gaia-derived catalog, with the WCS
written back into the frame's metadata and used by the Sky Chart.
```

- [ ] **Step 6: Verify nothing stale survives**

```bash
grep -n "src-tauri\|68+\|Node.js 18\|Rust 1.70\|template resolution" README.md
```

Expected: no output.

- [ ] **Step 7: Verify every internal link resolves**

```bash
grep -oE '\]\(\./[^)]+\)' README.md | tr -d '](.)' | while read -r f; do
  [ -e "$f" ] && echo "ok $f" || echo "MISSING $f"
done
```

Expected: only `ok` lines.

- [ ] **Step 8: Commit**

```bash
git add README.md
git commit -m "docs: bring the README up to date for publication

Documents the recursive clone the submodules require, the real crates/ layout,
the actual toolchain floors and both build targets. Drops the src-tauri tree,
the 68-command figure and the path-template claim, and adds the master library
and plate solving to the feature list."
git push origin main
```

---

### Task 8: Publish

The irreversible step. Task 2 must be green before any of it runs.

**Files:** none — remote and repository configuration only.

**Interfaces:**
- Consumes: everything from Tasks 1-7.
- Produces: the public repository and the two-remote working copy.

- [ ] **Step 1: Re-confirm the gate and the working state**

```bash
git status --porcelain                                  # expect: empty
git rev-parse --abbrev-ref HEAD                         # expect: main
git rev-list --left-right --count origin/main...main    # expect: 0	0
grep -c "no leaks found\|<fill in>" docs/superpowers/research/2026-08-24-pre-publication-secrets-audit.md
```

The last command must not report any `<fill in>` remaining. If the audit record is incomplete, stop — the gate did not actually run.

- [ ] **Step 2: Repack the local repository**

`.git` is 4.0 GB, of which 3.91 GB is unreachable garbage: 42 727 objects in the pack against 18 080 reachable. A push transfers only reachable objects, but repacking first makes the initial upload predictable.

```bash
git gc --prune=now
du -sh .git
```

Expected: substantially smaller than 4.0 GB.

- [ ] **Step 3: Reuse the existing public repository**

`eg013ra1n/athenaeum` **already exists** and is public — created 2025-10-27,
last pushed 2026-05-20, carrying an independent line of history whose tip
(`b6f52d4f`) is unknown to this repository. It has 6 stars, 1 fork, no issues,
and 6 merged pull requests that are all the owner's own. It was abandoned when
GitLab became canonical.

Decision (2026-08-24): **keep the repository and force-push over it.** That
preserves the URL, the stars and the fork link. There were never any outside
contributions to lose. Do NOT run `gh repo create` — it would fail.

```bash
gh repo view eg013ra1n/athenaeum --json name,visibility,pushedAt,stargazerCount
```

Expected: the repository exists and is `PUBLIC`. Refresh its metadata to match
the current project:

```bash
gh repo edit eg013ra1n/athenaeum \
  --description "Catalog manager for astrophotographers — FITS/XISF metadata, frame-set clustering, calibration matching, master creation and export" \
  --homepage "https://artfrom.space"
```

- [ ] **Step 4: Add the remotes**

`origin` stays GitLab so nothing about the release pipeline changes. `github` is the public remote. `all` pushes to both at once.

```bash
git remote add github https://github.com/eg013ra1n/athenaeum.git

git remote add all http://192.168.31.208:9080/root/athenaeum.git
git remote set-url --add --push all http://192.168.31.208:9080/root/athenaeum.git
git remote set-url --add --push all https://github.com/eg013ra1n/athenaeum.git

git remote -v
```

Expected: `all` shows two push URLs and one fetch URL.

- [ ] **Step 5: Force-push `main` and the tags**

The remote's `main` is an unrelated line, so a plain push is rejected. All 23
local tags are ancestors of `main`, so this pushes no commit that `main` does
not already carry — but 5 remote tags (`v0.2.0-beta.1` … `v0.2.0-beta.5`) point
at different commits and must be overwritten, which a plain `--tags` will not do.

```bash
git push --force github main
git push --force --tags github
```

This discards the abandoned public line. It is the decision recorded in Step 3;
do not soften it into a merge — the two histories share no commit, so a merge
would graft an unrelated tree onto the project.

- [ ] **Step 6: Verify what actually landed**

```bash
git rev-parse main
git ls-remote --heads https://github.com/eg013ra1n/athenaeum.git
git ls-remote --tags  https://github.com/eg013ra1n/athenaeum.git | grep -c 'refs/tags'
```

Expected: exactly one head, `refs/heads/main`, at the same SHA as local `main`;
23 tags, up from the 5 that were there. The old tip `b6f52d4f` must no longer be
advertised. If any other branch appears, delete it — only `main` is published.

- [ ] **Step 7: Configure the repository surface**

```bash
gh repo edit eg013ra1n/athenaeum \
  --default-branch main \
  --enable-issues \
  --enable-discussions \
  --add-topic astrophotography \
  --add-topic fits \
  --add-topic astronomy \
  --add-topic tauri \
  --add-topic rust \
  --add-topic react \
  --add-topic xisf \
  --add-topic plate-solving
```

Leave branch protection off. A rule that requires a pull request would block the maintainer's own `git push all main`, which is the normal path under the trunk model. Required status checks can be added later without that side effect.

- [ ] **Step 8: Verify the Actions gate runs green on the published tree**

Open the Actions tab. The push to `main` triggers `CI`. Wait for it and confirm it passes. A red first run means the Ubuntu dependency list or the Node version in `.github/workflows/ci.yml` is wrong — fix and push before announcing anything.

---

### Task 9: Prove an outsider can build it

The claim this whole plan rests on is that a stranger can clone and build. Until that is executed on a tree that came from GitHub rather than from disk, it is an assumption.

**Files:** none — verification only.

**Interfaces:**
- Consumes: the published repository from Task 8.
- Produces: the go signal for announcing the project.

- [ ] **Step 1: Clone recursively into a scratch directory, from GitHub only**

```bash
SCRATCH=$(mktemp -d)
cd "$SCRATCH"
git clone --recursive https://github.com/eg013ra1n/athenaeum.git
cd athenaeum
```

Expected: both submodules check out. A prompt for credentials, or a failure mentioning `192.168.31.208`, means a pin or a URL is still private — stop and fix Task 3 or 4.

- [ ] **Step 2: Prove no remote in the fresh clone touches the private network**

```bash
git config --file .gitmodules --get-regexp url
git remote -v
grep -rn "192.168.31" .gitmodules
```

Expected: both submodule URLs are `github.com`, the only remote is `origin` pointing at GitHub, and the `grep` finds nothing.

- [ ] **Step 3: Build the workspace from the fresh clone**

```bash
cargo build --workspace 2>&1 | tail -5
```

Expected: `Finished`. This is a cold build with no shared target directory, so allow it real time.

- [ ] **Step 4: Run the other two gates from the fresh clone**

```bash
npm ci
cargo test -p athenaeum-core 2>&1 | tail -5
npx tsc --noEmit && echo "tsc clean"
```

Expected: a passing test summary and `tsc clean`.

- [ ] **Step 5: Clean up and record the result**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum
rm -rf "$SCRATCH"
```

Append the outcome to the audit record's Results table as a final row — `clean-machine recursive clone and workspace build`, with the date — and commit:

```bash
git add docs/superpowers/research/2026-08-24-pre-publication-secrets-audit.md
git commit -m "docs: record the clean-clone acceptance result"
git push all main
```

The `git push all main` here is the first use of the two-remote workflow — it verifies the alias configured in Task 8 works.

---

## Post-plan follow-ups

Not part of this plan; record them wherever open items are tracked.

- `docs/superpowers/open-items.md` still describes the version-branch rule that Task 1 supersedes (Task 1 covers `CLAUDE.md` and auto-memory, not this ledger).
- `CLAUDE.md`'s module map says "~157 functions across 16 modules"; the measured figure is 232 commands across 23 modules. Task 7 corrects the README but not `CLAUDE.md`.
- An optional `.mailmap` would collapse `sharifov.v@mail366.com` and the GitLab `Administrator` identity into one displayed author without rewriting history.
- The `RELEASE_NOTES.md` line for the next release should mention that the project is now open source.

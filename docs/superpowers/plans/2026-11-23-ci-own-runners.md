# CI on our own runners Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (the owner runs this natively). The `lean-execution` skill overrides it wherever the two conflict. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Athenaeum's tests run on our own hardware: Linux on the astrodb VM (GitLab runner), Windows on EVO-X2. A `test/<name>` branch runs both before `main`, a release tag runs both again and the release waits for them; GitHub Actions runs only on pull requests.

**Architecture:**
- **One source of commands.** `.github/workflows/ci.yml` stays the only place the test commands are written. GitHub runs it on `pull_request` only; the GitLab jobs read their commands from it, as `test:windows` already does (`.gitlab/ci/scripts/windows_tests.ps1`), so the runs cannot drift.
- **Multi-line steps become scripts** (`scripts/ci/*.sh`), called by `ci.yml` and so by the Linux reader.
- **The release gate is native:** on a tag, `deploy` `needs` `test:linux`, `test:windows` and `check:headless-core`; `gate:tests` (polling GitHub check-runs) goes.

**Tech Stack:** GitLab CI (shell executors: bash on the Linux VM, pwsh on EVO-X2), GitHub Actions, the GitLab lint API (`dry_run`, `include_jobs`).

**Why** (owner, 2026-11-23): GitHub's free runners take about 32 min for Linux, and Windows runs into its 60-min limit. Of the last 30 runs, 13 were green, 8 red (mostly network flakes) and 8 cancelled (mostly Windows). EVO-X2 ran the same Windows suite in 14 min cold and 5 min warm. The owner will grow the astrodb Linux VM for the Linux suite.

Plan review: fable, 2026-11-23, APPROVE WITH FIXES (2 critical, 10 important, 8 minor). All are folded in below; the two release-critical ones are `check:headless-core` in `deploy`'s needs (Task 4) and `limit = 1` per runner (owner task 2).

## Rulings (plan author, 2026-11-23; the owner can overturn any)

| ID | Ruling | Task |
| ---- | ---- | ---- |
| CI-R1 | `ci.yml` triggers on `pull_request` only (`paths-ignore` kept). It no longer runs on a `push` to `main` or on a tag. Its two jobs stay, so PRs and the GitLab readers share one command list. The `rust` paths filter gains `'starloom'`, `'rustafits'`, `'solvemyastro'` and `.gitmodules`, so a gitlink-only Dependabot PR runs the Rust jobs. | 2 |
| CI-R2 | A `test/<name>` branch pipeline is `test:linux` plus `test:windows`, in parallel on our runners, and nothing else (`check:headless-core` gains `except: /^test\//`; `test:linux` runs the same check). `wintest/<name>` stays a Windows-only alias. | 3 |
| CI-R3 | On a `v*` tag, the pipeline also runs `test:linux` and `test:windows`. `deploy` `needs` both, plus `check:headless-core` (today it gates only through `gate:tests`). `gate:tests`, `github_checks_gate.sh`, its `run_tests.sh` section and fixtures are deleted. The release green-check rule: the tagged commit's `test/<name>` pipeline was green before tagging, and the tag pipeline's own tests gate the deploy. A red test job on a tag is retried, never bypassed: no `allow_failure`, no `optional: true`. The `GATE_REQUIRED_CHECKS` override that v0.7.1 used disappears. Whether a deliberate bypass variable should exist is the owner's call; default: none. | 4, 5 |
| CI-R4 | `test:linux` runs on a second runner registration on the same VM, tag `linux-test`, so tests never queue behind the release builds. Both registrations carry `limit = 1`. The global `concurrent = 2` would otherwise let the `linux` runner take two release compiles at once (v0.7.1's `build:linux` already OOM'd). Each registration has its own builds dir, separated by its token, with its own warm `target/` (`GIT_CLEAN_FLAGS`). | owner, 3 |
| CI-R5 | The Linux reader runs every `run:` of `ci.yml`'s `build-and-test` job in file order. It skips an explicit list, the apt install and the toolchain-resolve steps (the VM has both). It exits 2 on any multi-line `run:` that is neither a script call nor on that list, so a step added later can never be skipped silently. It sets the job's and the file's `env:` first. | 3 |
| CI-R6 | Actions move to `actions/checkout@v5` and `actions/setup-node@v5` (the Node 20 deprecation notice). `ci.yml`'s comments about push and tag runs and the GitLab gate are rewritten to the new facts. | 2 |
| CI-R7 | Starloom's own GitHub CI stays as it is: about 11 min, free for a public repo, the check outside contributors see. | — |
| CI-R8 | A push to `main` is no longer tested after the fact. The standing "small change: push without the full gate" habit now means untested until a `test/*` branch or a tag. The skills say so. | 5 |

## Owner tasks (before Task 1)

1. **Grow the VM** (astrolinux-runner1 on astrodb). Recommended size, assuming tests and one release build run at the same time (depends on astrodb's host capacity):
   - **CPU:** 12 vCPU. EVO-X2 has 16 cores and takes 14 min cold; GitHub has 4 vCPU and takes about 32 min.
   - **RAM:** 32 GB.
   - **Disk:** 300 GB. Today 62 GB, filled three times by the release `target/`, buildx and rustup; a debug test `target/` is 40-50 GB more, plus a second checkout with submodules.
   - Then, as a user with sudo (the `gitlab-runner` user has none): `sudo growpart /dev/vda 2 && sudo resize2fs /dev/vda2`.
2. **A second runner** for project `root/athenaeum`.
   - **If you see** "New project runner" in GitLab (Settings → CI/CD → Runners): create it there with tag `linux-test`, "Run untagged jobs" off and "Protected" **off** (it serves unprotected `test/*` branches and protected tags, as EVO-X2's does). Then on the VM: `sudo gitlab-runner register --url http://192.168.31.208:9080 --token glrt-…`, executor `shell`.
   - **If you only see** a registration token (legacy flow): `sudo gitlab-runner register` with `--tag-list linux-test --run-untagged=false --access-level not_protected`, executor `shell`.
   - **`/etc/gitlab-runner/config.toml`:** the top-level line `concurrent = 2` (there is no `[global]` table), and in BOTH `[[runners]]` blocks `limit = 1`. In the new block also `output_limit = 65536` (EVO-X2 needed it to keep the `test result:` lines). Then `sudo gitlab-runner restart`.

If any of this is not possible (no sudo, a smaller VM), say so. CI-R4 then falls back to the one `linux` runner, and a tag pipeline gets longer by the Linux test time.

## Global Constraints

- **The release pipeline must keep working.** Check every `.gitlab-ci.yml` change with the lint API: `POST /projects/:id/ci/lint`, `content` = the new YAML, `dry_run=true`, `include_jobs=true`, `ref=<ref>`. Use only refs that already exist: `main`, the existing `wintest/starloom-p3` branch, the latest release tag (`v0.7.1` today). **Never push a new `v*` tag to test anything: any `v*` tag runs the real release and deploys.**
- **The lint is proven before it is trusted.** Task 1 lints today's unchanged YAML first. Against `v0.7.1` it must list `gate:tests`, `deploy` and the seven builds; against `main` only `check:headless-core` and the smokes. If that control fails, the dry-run does not simulate a tag: stop and bring it to the owner (the fallback is a throwaway project).
- **One source of commands:** no GitLab script spells out a `cargo test` command line. The scripts read `ci.yml`.
- **Commits** are authored by the owner, carry the session trailers, and land on a branch merged into `main` with `--no-ff`. A push to GitHub (a branch, a PR, `main`) and to Athenaeum `main` is on the owner's word.

## Review Focus

1. **A tag still deploys only after its tests and the headless check** (Task 4 step 3's lint).
2. **The Linux run reads the same commands as GitHub's job** (CI-R5; Task 3 step 4 compares the log with `ci.yml`).
3. **Ceilings on the VM:** `ulimit -u` ≥ 4 096 (the core suite peaks at about 1 650 threads), PyYAML present (Task 1 probe).
4. **Runner contention:** `test:linux` and a release build run side by side, and the `linux` runner never runs two jobs (Task 3 step 5).
5. **Dependabot and outside PRs** still get the GitHub checks, gitlink-only ones included (CI-R1, Task 2 step 3).

## File structure

| Path | Change | Task |
| ---- | ---- | ---- |
| `.gitlab-ci.yml` | `workflow:` rule `test/*`; `probe:linux-test` (Task 1 only); `test:linux`; `test:windows` rules; `check:headless-core` `except`; `gate:tests` deleted; `deploy` needs; comments at about 11, 46-48, 343-349, 533-544 | 1, 3, 4 |
| `.gitlab/ci/scripts/linux_tests.sh` | new: the Linux reader of `ci.yml` | 3 |
| `.gitlab/ci/scripts/windows_tests.ps1` | header comment only (GitHub runs on PRs) | 5 |
| `.gitlab/ci/scripts/github_checks_gate.sh`; its section in `.gitlab/ci/scripts/test/run_tests.sh` (about lines 765-815); `fixtures/github_checks_*` (5 files) and its `mock_bin/curl` cases | deleted | 4 |
| `scripts/ci/lock_guard.sh`, `scripts/ci/headless_check.sh` | new, mode 755 (`git update-index --chmod=+x`) | 2 |
| `.github/workflows/ci.yml` | CI-R1, CI-R6, the two steps call the scripts, comments | 2 |
| `.claude/skills/release/SKILL.md`, `.claude/skills/starloom-release/SKILL.md`, `docs/release/README.md` (about 10-13), `docs/superpowers/open-items.md` | Task 5 list | 5 |
| `claude-config/machines/EVO-X2.md`; memory `reference_ci.md`, `reference_linux_ci_runner.md`, `reference_lab_runners.md` | Task 5 list | 5 |

---

### Task 1: The VM is ready, the lint is proven

- [ ] **Step 1: Lint control.** Lint today's unchanged `.gitlab-ci.yml` (Global Constraints) against `v0.7.1` and `main`. The job lists must match the control, or STOP.
- [ ] **Step 2:** On branch `ci-own-runners`, add a `workflow:` rule `if: $CI_COMMIT_BRANCH =~ /^test\//` and the job `probe:linux-test`: tags `[linux-test]`, rule `test/*`, `before_script: source "$HOME/.cargo/env"`. Its script prints:
  - `nproc`, `free -g`, `df -h /home/gitlab-runner`, `ulimit -u`;
  - `rustup show active-toolchain`, `node --version`, `python3 -c "import yaml"`;
  - `dpkg -s` for every package of `ci.yml`'s apt step.

  Add `/^test\//` to `check:headless-core`'s `except`.
- [ ] **Step 3:** Push the commit to GitLab as `test/probe` (`git push origin <sha>:refs/heads/test/probe`). Expected: the pipeline is `probe:linux-test` alone, on the new runner, printing at least 12 CPUs, 32 GB, 250 GB free, `ulimit -u` ≥ 4 096, PyYAML importable and every package installed. A shortfall goes back to the owner before Task 3. Delete `test/probe` on GitLab.

### Task 2: GitHub runs on pull requests only

**Files:** `.github/workflows/ci.yml`, `scripts/ci/lock_guard.sh`, `scripts/ci/headless_check.sh`.

- [ ] **Step 1:** Move the bodies of the steps "One copy of rustafits and solvemyastro" and "Headless core builds without rustafits or solvemyastro" verbatim into the two scripts (`#!/usr/bin/env bash`, `set -euo pipefail`). The steps become `run: scripts/ci/lock_guard.sh` and `run: scripts/ci/headless_check.sh`. Apply CI-R1 and CI-R6, comments included.
- [ ] **Step 2: Verify locally.** Both scripts exit 0 on the dev Mac. The lock guard's negative check reproduces non-zero (starloom-p3 plan, Task 1 step 5). `yaml.safe_load` parses `ci.yml`.
- [ ] **Step 3: GitHub reading (owner's word: a branch and a draft PR on the public repo).** `gh pr create --draft` from a throwaway branch: both jobs start. Cancel the runs (`gh run cancel`), close the PR, delete the branch.

### Task 3: `test:linux` and the `test/*` pipeline

**Files:** `.gitlab-ci.yml`, `.gitlab/ci/scripts/linux_tests.sh`.

**Interfaces:** `linux_tests.sh` reads `.github/workflows/ci.yml` with `yaml.safe_load` (PyYAML, proven by the probe) and follows CI-R5. It prints `+ <command>` before each step, runs it with `bash -c`, and exits with the first failing step's code. Exit 2 means `ci.yml` has a step the reader does not know.

- [ ] **Step 1:** Write `linux_tests.sh`; mode 755.
- [ ] **Step 2:** `.gitlab-ci.yml`: replace `probe:linux-test` with `test:linux`:
  - `stage: build`, `tags: [linux-test]`, rules `test/*` and `$CI_COMMIT_TAG =~ /^v/`;
  - `before_script: source "$HOME/.cargo/env"`, `interruptible: true` (harmless on a tag: say so in the comment), `timeout: 90m`;
  - variables `GIT_CLEAN_FLAGS: -ffdx -e target/ -e .cargo/ -e .npm/` and `GIT_SUBMODULE_STRATEGY: recursive`.

  `test:windows` rules gain `test/*` and tags beside `wintest/*`.
- [ ] **Step 3: Lint.** `wintest/starloom-p3`: `test:windows` only. `main`: unchanged from the control. `v0.7.1`: the control's jobs plus `test:linux` and `test:windows`.
- [ ] **Step 4: First run.** Push the branch head as `test/ci-own-runners`. The pipeline has exactly `test:linux` and `test:windows`, both green. Every non-skipped `run:` of the `build-and-test` job appears in the Linux log after `+ `. Record cold durations in the ledger, re-run both jobs and record warm.
- [ ] **Step 5: Contention.** While `test:linux` re-runs, a one-line job on tag `linux`, pushed on a second `test/*` branch, starts at once. A second job on `linux` waits (`limit = 1`). Delete that branch.

### Task 4: The release gate is native

- [ ] **Step 1:** Delete `gate:tests`, `github_checks_gate.sh`, its `run_tests.sh` section, its fixtures and mock cases. `deploy` (and any other job that names `gate:tests`; `grep -n "gate:tests"`) needs `test:linux`, `test:windows` and `check:headless-core`, all `artifacts: false`.
- [ ] **Step 2:** `.gitlab/ci/scripts/test/run_tests.sh` passes (it also holds the `.gitlab-ci.yml` shape lint). `grep -rn "github_checks_gate\|GATE_REQUIRED_CHECKS\|gate:tests" .gitlab-ci.yml .gitlab docs .claude` prints only historical plan text.
- [ ] **Step 3: Lint** `v0.7.1`: no `gate:tests`, and `deploy`'s needs list `test:linux`, `test:windows` and `check:headless-core` besides the seven builds. `main` and `wintest/starloom-p3` are unchanged from Task 3.

### Task 5: Docs, skills, memory; merge

- [ ] **Step 1: `release/SKILL.md`.**
  - "Before you start": replace the `gh run list` line with "both test runners (`linux-test`, `windows-test`) online".
  - Delete the `GITHUB_API_TOKEN` paragraph, or mark it unused. The GitLab CI variable stays until the owner removes it.
  - Steps 3 and 4: push the commit as `test/<version>`, wait for that pipeline to go green, then tag. The tag's own tests gate `deploy`. Delete the sentence about the second GitHub run.
  - Step 5: `gate:tests` → the two test jobs. A red test job on a tag is retried (CI-R3).
  - CI-R8.
- [ ] **Step 2: The rest of the list.**
  - `starloom-release/SKILL.md`: "CI green" for an Athenaeum bump means its `test/*` pipeline.
  - `docs/release/README.md` (about lines 10-13).
  - The `.gitlab-ci.yml` comments of the file table.
  - The `windows_tests.ps1` header.
  - `claude-config/machines/EVO-X2.md`.
  - Memory `reference_ci.md` (our runners gate; GitHub runs PRs), `reference_linux_ci_runner.md` (the new size, runner `linux-test`, `limit = 1`) and `reference_lab_runners.md`, with the `MEMORY.md` lines.
- [ ] **Step 3: Open items.**
  - The runners set `CI=true`, so the `CI`-gated ceilings are logged, not asserted, as on GitHub.
  - A flake that shows only on a runner goes under "Known flaky test" with the runner's name.
  - At the first release, record the Windows lane time (`test:windows` shares EVO-X2 with the release builds, `concurrent 1`) and the size of `C:\gr\…\target`.
- [ ] **Step 4:** Merge `ci-own-runners` into `main` (`--no-ff`); push on the owner's word. The first release after this is the first real check of Task 4: watch its tag pipeline to the end.

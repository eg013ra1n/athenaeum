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
  release — see memory `reference_worktree_session_release`.
- The GitHub run of the commit you will tag must be green. The pipeline's
  `gate:tests` job enforces it; you check first so a red gate never surprises
  anyone: `gh run list --commit $(git rev-parse HEAD) --json name,conclusion`.
- Keep the dev Mac idle while `build:macos` and `build:perseus:macos` run — the
  macOS runner is this machine and a saturated box times the job out.
- `root/artfrom-space` `main` must carry the three `download.md` markers
  (`<!-- latest-build:start -->`, `<!-- latest-build:end -->`,
  `<!-- version-history:rows -->`) — `docs:publish` fails naming the missing
  one otherwise.
- Four CI/CD variables the pipeline needs: `DOCS_REPO_TOKEN` (required:
  artfrom-space project access token, Maintainer, write_repository),
  `GITHUB_API_TOKEN` (recommended — effectively required if a gate ever has
  to wait or a tag is redone within the hour: the unauthenticated GitHub API
  limit is 60 calls/hour and the gate polls once a minute for up to 50
  minutes), `SSH_KNOWN_HOSTS`
  (optional, MUST be the `[host]:40022` form that `ssh-keyscan -p 40022 <host>`
  prints, else `StrictHostKeyChecking=yes` refuses the upload), `RELEASE_ALLOW_AMD64_ONLY`
  (only while the arm64 Docker runner is down; unset afterwards).

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
   pointless; a red run means fix-then-retag, never force. The tag push
   starts a second GitHub run for the same commit; the gate reads whichever
   is the latest, so wait for the main-branch run to be green before tagging
   rather than relying on the tag's own run.
5. **Read the pipeline** (`http://192.168.31.208:9080/root/athenaeum/-/pipelines`, or
   the API with the PAT in `~/.config/athenaeum-ci/gitlab-pat`):
   - `gate` — `check:versions` (the only job in stage `gate`) red: a version file disagrees with the tag → fix,
     delete the tag on both remotes, retag.
   - `build` — three kinds of jobs run here. The seven platform-build jobs (`build:windows`,
     `build:linux`, `build:macos`, `build:perseus:*`) usually fail on runner problems (memory
     `reference_macos_runner_wedge` for Mac, `reference_linux_ci_runner` for Linux); a plain
     retry of the one job is the first move. `check:headless-core` red: a feature-gated compile
     break — a module reaching into `plate_solve`/`registration`/`catalog` without the matching
     `#[cfg]` (every `--workspace` command builds with `default = ["render", "solver"]`, so it
     compiles locally and fails only here and in Perseus jobs; this is exactly what happened to
     v0.5.5) → fix the code on `main`, get green, delete the tag on both remotes, retag; never
     retry the job. `gate:tests` (stage `build`, polls GitHub up to 50 min) red: GitHub is red →
     same fix-and-retag.
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
   - `announce` — `release` (GitLab Release), `publish_version` (`version.json`),
     `publish:channel-link` (version-less `builds/latest` or `builds/beta` links
     move ONLY here, after both verify jobs — a rejected build never becomes
     `latest`), `notify:discord`, `notify:telegram`. Only these are user-visible;
     nothing before them is.
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
- Runner smoke jobs `smoke:{macos,linux,windows}` (manual, on main) are the
  ten-second check that a runner takes work after a restart — play one instead
  of a build.
- Review that shaped this: `docs/superpowers/research/2026-09-15-release-cycle-review.md`.

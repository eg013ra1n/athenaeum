---
name: starloom-release
description: Cut a starloom release (the stacking kernel submodule at starloom/, its own repo github.com/eg013ra1n/starloom) and bump it in Athenaeum — changelog, version, tag, the release workflow, the tripwire and lab checkpoint before the bump merges, Dependabot bump PRs. Use when the owner says "release starloom", "bump starloom", or a starloom bump PR arrives.
---

# Starloom release

Starloom is a git submodule at `starloom/`, developed in place: `cargo check --workspace` sees a change at once. Its
own repository is `https://github.com/eg013ra1n/starloom` (GitHub only, no GitLab mirror). Athenaeum patches its
rustafits and solvemyastro back to the workspace submodules (root `Cargo.toml`), so a change there needs no starloom
release. Spec: `docs/superpowers/specs/2026-11-22-starloom-extraction-design.md` (SL-14, SL-18); plan
`docs/superpowers/plans/2026-11-23-starloom-p3.md`.

## A change in starloom

1. In `starloom/`, compare `main` with `origin/main` first (the submodule sits in detached HEAD; a stale local `main`
   silently moves the tree back), then branch from `origin/main`.
2. A change that moves a stage's output bits bumps that stage's version and records its golden row in the same commit
   (`tests/stage_goldens.rs`, `cargo test --test stage_goldens -- --nocapture`, copy the printed literal). Rows are
   recorded at opt-level 2 only (the dev profile; never from an opt-level 0 build). A change that moves no number says
   so in its commit message.
3. Merge into starloom `main` with a merge commit (`--no-ff`), never a squash: Athenaeum pins `main` commits only, and
   a squash leaves a pinned SHA unreachable.

## The release

1. `CHANGELOG.md`: a `## [X.Y.Z] — YYYY-MM-DD` section that covers EVERY commit since the last tag
   (`git log vLAST..main --oneline`). The release workflow takes the notes from it and fails without it.
2. `Cargo.toml` `version = "X.Y.Z"`; `Cargo.lock` follows (`cargo metadata --format-version 1 >/dev/null`, inside a
   standalone clone, not the workspace). Commit `release: vX.Y.Z` on `main`.
3. Push `main`; wait for CI green (Linux all layers, macOS, Windows; `publish-dry-run` passes on the expected
   git-dependency failure until solvemyastro is on crates.io).
4. `git tag vX.Y.Z` on that commit (lightweight, like rustafits), push it. The tag must equal the `Cargo.toml`
   version (the workflow checks).
5. Verify by fact: `gh run list -R eg013ra1n/starloom --workflow Release -L 1` green and
   `gh release view vX.Y.Z -R eg013ra1n/starloom` shows the changelog section. There is no crates.io leg and no binary
   (spec Q3; the CLI is phase 2).
6. The owner's word is needed for pushes to starloom `main` and tags, as for rustafits.

## The bump in Athenaeum

1. Release starloom BEFORE the superproject, and push the pin before the superproject merges: a pin on no remote
   breaks every fresh recursive clone.
2. A bump names a starloom `v*` tag. Dependabot (`.github/dependabot.yml`, `gitsubmodule`, `allow: starloom`)
   proposes the branch head: a PR whose SHA is not a tag waits or is closed.
3. Regenerate and commit Athenaeum's `Cargo.lock` with the bump (`cargo metadata --format-version 1 >/dev/null`). CI's
   "One copy of rustafits and solvemyastro" step fails when a `[patch]` stopped applying (for example a starloom that
   needs a rustafits the submodule no longer satisfies).
4. Athenaeum's CI never checks starloom's solvemyastro `rev` (the patch hides it); starloom's own CI does. A starloom
   that needs a newer solvemyastro moves both pins: the solvemyastro submodule here and the `rev` there.
5. Before the owner merges (SL-14): the tripwire `cargo test -p athenaeum-core --test starloom_versions`, a green
   `test/<name>` pipeline on GitLab (Athenaeum's tests run on our runners; GitHub runs nothing for Athenaeum by itself,
   so a Dependabot bump PR carries no check), and the lab checkpoint on real data (the measurement doctrine: a numeric
   change is judged on a whole group at master level). Starloom's own CI on GitHub is unchanged.
6. Merge a bump PR locally and push to `all` (GitLab and GitHub keep the same SHAs; GitHub closes the PR as merged).
   Commit message: `chore: bump starloom submodule to vX.Y.Z`.
7. `rust-toolchain.toml` exists in both repositories with the same channel; change them together.

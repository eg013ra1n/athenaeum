# Rustafits Release

How to cut a new rustafits release. rustafits lives as a git submodule at `athenaeum/rustafits/` and has its own GitHub repo at `github.com/eg013ra1n/rustafits`. Releases are driven by pushing a `vX.Y.Z` tag to that repo — most distribution steps are automated by CI, but macOS binaries are manual and publish-to-crates.io has `continue-on-error: true` so it must be verified.

**DO NOT** follow the stale `rustafits/.claude/skills/release/SKILL.md` at `Documents/Projects/rustafits/` — it incorrectly references `package.json`, `tauri.conf.json`, and a manual Homebrew bump. None of those apply.

## Files that need a manual version bump

Only one:
- `rustafits/Cargo.toml` → bump the top-level `version = "X.Y.Z"`

There is no `CHANGELOG.md`, no `package.json`, no `tauri.conf.json`, no "About page" in rustafits. `Cargo.lock` updates on the next `cargo build`/`check`; don't hand-edit it.

## What CI does automatically (on `v*.*.*` tag push)

All three workflows live in `rustafits/.github/workflows/`:

1. **`release.yml`** (triggers on tag push)
   - Creates a GitHub Release object
   - Builds Linux x86_64 binary + `.tar.gz` + `.sha256`, uploads to release
   - Builds a `.deb` package (`rustafits_X.Y.Z_amd64.deb`), uploads to release
   - Runs `cargo publish --token $CARGO_TOKEN` → **has `continue-on-error: true`**, so failures are silent. Verify manually after.
2. **`update-homebrew.yml`** (triggers on tag push)
   - Regenerates the Formula in the `homebrew-rustafits` tap repo with new version + SHA256 of the GitHub source tarball
   - Commits and pushes to the tap
3. **`update-aur.yml`** (triggers on `release: published`)
   - Updates `PKGBUILD` version + sha256, pushes to AUR

## What you (the human/agent) must do manually

macOS binaries — GitHub Actions charges for macOS runners, so `release.yml` only builds Linux. Follow `rustafits/BUILD_MACOS.md`:
- `cargo build --release` on Apple Silicon → `tar czf rustafits-macos-aarch64.tar.gz target/release/rustafits`
- Optional: cross-compile x86_64 via `rustup target add x86_64-apple-darwin && cargo build --release --target x86_64-apple-darwin`
- Upload both tarballs + `.sha256` files to the GitHub release page (or use `gh release upload vX.Y.Z ...`)

## Full release procedure

Run from **inside the athenaeum submodule** at `athenaeum/rustafits/` (or the standalone clone at `Documents/Projects/rustafits/` — both point at the same remote).

1. **Verify clean state**
   ```bash
   cd athenaeum/rustafits
   git status        # clean
   git pull origin main
   ```
2. **Pre-release check** — run the full test suite to avoid publishing a broken crate:
   ```bash
   cargo check
   cargo test
   cargo clippy -- -D warnings
   ```
3. **Bump version** in `rustafits/Cargo.toml` (`version = "X.Y.Z"`)
4. **Regenerate `Cargo.lock`**:
   ```bash
   cargo check
   ```
5. **Commit + push**:
   ```bash
   git add Cargo.toml Cargo.lock
   git commit -m "release: vX.Y.Z"
   git push origin main   # VERIFY push succeeds
   ```
6. **Tag + push tag**:
   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z # VERIFY push succeeds
   ```
7. **Watch CI** — `release.yml`, `update-homebrew.yml`, and `update-aur.yml` should all go green. Check:
   - https://github.com/eg013ra1n/rustafits/actions — all three workflows pass
   - https://crates.io/crates/rustafits — new version listed (because publish is `continue-on-error`, a green CI does NOT prove crates.io succeeded; confirm visually)
   - https://github.com/eg013ra1n/homebrew-rustafits/blob/main/Formula/rustafits.rb — bumped
   - https://aur.archlinux.org/packages/rustafits — bumped
8. **Build macOS binaries manually** (per `BUILD_MACOS.md`) and upload to the GitHub release page.
9. **If `cargo publish` silently failed** — fix the root cause (missing dep, licence issue, existing version), bump to `X.Y.(Z+1)`, retry from step 3. There's no "republish same version" path on crates.io.
10. **Update the submodule pointer in athenaeum** so athenaeum's workspace points at the released commit:
    ```bash
    cd ..                      # back to athenaeum root
    git add rustafits
    git commit -m "chore: bump rustafits submodule to vX.Y.Z"
    git push all main          # main is the trunk; `all` pushes GitLab + GitHub
    ```
    Because athenaeum uses `rustafits = { path = "../../rustafits" }` (not a crates.io version pin), no `Cargo.toml` edits are needed in athenaeum-core/athenaeum-tauri.

## Common failure modes

- **`cargo publish` silently fails** — always verify crates.io directly. If it fails due to a registry issue, a new patch version is the only recovery path.
- **Tag pushed but release.yml already ran on a previous tag** — GitHub Actions runs per-tag, so pushing `vX.Y.Z` fresh always triggers. If the tag was created wrong, delete both local and remote tag (`git tag -d vX.Y.Z && git push origin :refs/tags/vX.Y.Z`) and retag.
- **Homebrew tap push fails** — tap token (`HOMEBREW_TAP_TOKEN`) expired. Regenerate in GitHub settings → secrets of the rustafits repo.
- **AUR push fails** — `AUR_SSH_PRIVATE_KEY` secret is invalid. Regenerate and store.
- **Submodule URL is HTTPS but you need to push** — athenaeum currently uses `https://github.com/eg013ra1n/rustafits.git` for the submodule (set during `git submodule add`). For pushing you either need a GitHub Personal Access Token in the URL, or switch that submodule to SSH: `git config -f .gitmodules submodule.rustafits.url git@github.com:eg013ra1n/rustafits.git && git submodule sync rustafits`.

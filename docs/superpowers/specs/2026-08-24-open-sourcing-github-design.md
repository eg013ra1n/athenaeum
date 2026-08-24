# Open-sourcing Athenaeum on GitHub — design

Date: 2026-08-24
Status: approved for planning

## 1. Goal

Publish Athenaeum on GitHub as a collaborative open-source project so that
outside contributors can read the code, build it, file issues and send pull
requests — while GitLab keeps the private release pipeline, and while merging a
contributor's PR back into the local working environment costs one `git pull`.

### Non-goals

- Publishing the hub server or the `projects` site. They live in separate
  repositories, are not part of this workspace, and remain the paid service.
- Moving the release pipeline to GitHub Actions. macOS signing and
  notarization, the three-platform build matrix, `artfrom.space` deployment,
  Docker Hub and the Discord/Telegram notifications stay on GitLab CI with its
  self-hosted runners.
- Hiding the agent tooling, the development history, or the fact that the
  project is developed with agent assistance.

## 2. Decisions

| # | Decision | Rationale |
| ---- | ---- | ---- |
| D1 | Publish the repository as-is — nothing stripped | Agent config, planning docs and CI reveal no secrets. Nothing to hide means no history rewrite, no filter to maintain, no second lineage. |
| D2 | One history, two remotes | Identical commits and SHAs on GitLab and GitHub. Contribution flow becomes plain `git pull` / `git push`; no mirror script, no cherry-pick bridge, no broken `git blame`. |
| D3 | `main` becomes the development trunk; releases are tags on `main` | The default branch is what a visitor sees and what every PR is based on. Under the previous rule `main` trails the active version branch — today by 700 commits — so every external PR would conflict. |
| D4 | Owner is `github.com/eg013ra1n` | Sits beside the already-public `eg013ra1n/rustafits`. |
| D5 | `solvemyastro` is published as its own public repository | It is a hard workspace member; without it nobody can build. Same shape as `rustafits`: separate repo, submodule pointing at GitHub. |
| D6 | GitLab CI keeps the release pipeline; GitHub Actions gets a PR gate only | Contributors get a build/test signal without any access to signing material or deploy hosts. |
| D7 | Apache-2.0 stays, no CLA | Under Apache-2.0 §5 contributions arrive under the same licence by default. A CLA would only be needed to relicense later; not wanted. |

D1 supersedes the earlier mirror plan that `.github-export-ignore` was written
for. D3 supersedes the standing "develop on a branch named after the version"
rule.

## 3. Publication gate — secrets

The one hard gate. Publication is irreversible: once pushed, forks, archives
and GitHub's own caches keep the history forever.

The gate covers **full history**, not the working tree. A credential committed
once and deleted later is still reachable in the pack.

### Evidence already gathered

- **Filename scan**, all 18 080 reachable objects: no `.env`, `.p12`, `.pem`,
  `.p8`, `.key`, `.pfx`, `id_rsa`, `id_ed25519`, `.netrc`, `.kdbx` or
  `.keychain` path was ever committed. The single hit was
  `crates/athenaeum-core/src/account/token_store.rs`, a source file.
- **Content scan**, all reachable blob content (~264 MB): zero matches for
  private-key headers, `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`, `glpat-`,
  `xox[baprs]-`, `AKIA…`, Telegram bot tokens, `sk-…` and Discord webhook URLs.
- `.gitlab-ci.yml` consumes only CI variables — `APPLE_CERTIFICATE`,
  `APPLE_CERTIFICATE_PASSWORD`, `KEYCHAIN_PASSWORD`, `APPLE_API_KEY*`,
  `DEPLOY_SSH_KEY` and the notification tokens. No literal values.
- `.claude/settings.json` holds hook definitions; `.codex/config.toml` holds an
  MCP command. Neither carries credentials.
- Claude's memory lives at `~/.claude/projects/.../memory/`, outside the
  repository. It cannot leak through git.

### Formal gate

`gitleaks` (not currently installed) must run clean over the tree and the full
history of **both** `athenaeum` and `solvemyastro` before the first push. Zero
findings required.

### Accepted disclosures

Decided deliberately; do not re-flag.

- `192.168.31.208:9080` — the LAN address of the private GitLab. Appears in
  `.gitmodules`, `solvemyastro/Cargo.toml`'s `repository` field, `CLAUDE.md`
  and docs. Not routable from outside. The two entries that are real URLs get
  repointed to GitHub anyway.
- `.gitlab-ci.yml` and `.gitlab/` reveal the deploy host, artefact paths, the
  Docker Hub repository and the notification scripts. Published on purpose: it
  documents how a signed three-platform build is produced, and keeping it
  private would reintroduce exactly the divergent-tree machinery D2 avoids.
- Author identities `sharifov.v@mail366.com` (78 commits) and
  `Administrator <gitlab_admin_ba4842@example.com>` (3 commits) become public.
  An optional `.mailmap` normalizes how they display without rewriting history.
- Service endpoints in the client: `test-hub.artfrom.space` (debug default) and
  `projects.artfrom.space` (release default) in `settings/mod.rs`, the relay
  hosts in `examples/relay_check.rs`, and `artfrom.space/catalogs/` in
  `catalog/gaia_prebuilt.rs`. These are client-side endpoints of a service
  whose server code is not published.

## 4. Submodule reachability

The only thing that makes the public repository unbuildable today.

| Submodule | Current URL | Pinned commit | State |
| ---- | ---- | ---- | ---- |
| `rustafits` | `github.com/eg013ra1n/rustafits.git` | `72aca7c` (`v1.0.0-26-g72aca7c`) | Public; verified anonymously reachable |
| `solvemyastro` | `192.168.31.208:9080/root/solvemyastro.git` | `4a41a05` (`heads/logging`) | Private LAN only — blocks every outside clone |

`solvemyastro` is a workspace member and a path dependency of
`athenaeum-tauri`, `athenaeum-web` and `catalog-builder`; in `athenaeum-core`
it is optional behind the `solver` feature. Nothing builds without it.

Work required:

1. Run the section-3 secrets gate over `solvemyastro`'s own history.
2. Create `github.com/eg013ra1n/solvemyastro`, push its history there. It gets
   the same two-remote arrangement as the superproject: GitLab stays `origin`,
   GitHub is added alongside.
3. Confirm the pinned commit `4a41a05` is reachable on the public remote — a
   superproject pin that only exists locally breaks `clone --recursive`.
4. Repoint `.gitmodules` and `solvemyastro/Cargo.toml`'s `repository` field.
5. Confirm the same for `rustafits`'s pin `72aca7c`.

**Acceptance:** on a machine with no route to the LAN GitLab,
`git clone --recursive https://github.com/eg013ra1n/athenaeum.git` followed by
`cargo build --workspace` succeeds.

## 5. Branching model

`main` becomes the trunk. Releases are tags on `main`. A release branch is cut
only if a backport is ever actually needed.

**Transition.** `main` is 0 commits ahead of `0.5.1` and `0.5.1` is 700 ahead of
`main`, so the move is a pure fast-forward:

```
git checkout main && git merge --ff-only 0.5.1
```

This is a publication-time step and is independent of whether v0.5.1 ships —
it does not consume the pending release smoke checks.

**Branches on GitHub.** Only `main` is pushed initially. The 25 dead local
branches (`dreamy-golick`, `upbeat-bohr`, `calibration-2`, `charts`, `export_2`,
…) stay GitLab-only. `feature/stacking-prep` and `wip/pure-rust-jpeg` likewise
stay private until they are real work again.

**Downstream doc changes.** `CLAUDE.md`'s "Release workflow" section and the
auto-memory note `feedback_version_branches.md` both encode the superseded rule
and must be updated in the same cycle.

## 6. Remote topology and workflow

```
local working copy
 ├── origin  → GitLab 192.168.31.208:9080/root/athenaeum.git   (private: CI, releases, signing)
 └── github  → github.com/eg013ra1n/athenaeum.git              (public: collaboration)
```

A third `all` remote makes a single push reach both:

```bash
git remote add all http://192.168.31.208:9080/root/athenaeum.git
git remote set-url --add --push all http://192.168.31.208:9080/root/athenaeum.git
git remote set-url --add --push all git@github.com:eg013ra1n/athenaeum.git
git push all main
```

**Maintainer, day to day** — commit on `main`, `git push all main`.

**Contributor PR** — review on GitHub, merge there, then locally:

```bash
git fetch github
git merge --ff-only github/main
git push origin main
```

**Release** — unchanged in substance: tag `main`, push the tag to `origin`; the
GitLab pipeline builds, signs, uploads and announces. Push `main` and the tag to
`github` as well so the public repository shows the release point. Tags carry no
GitHub workflow, so nothing is triggered there.

**GitLab pipeline scope.** Unchanged in configuration, but see section 7 for
what D3 changes about how often it fires.

**Branch protection.** None on `main` initially. A rule that requires a pull
request would break the maintainer's own `git push all main`, which is the
normal path. Required status checks can be added later without that side effect.

## 7. GitHub Actions PR gate

One workflow, `.github/workflows/ci.yml`, on `pull_request` and on `push` to
`main`. Ubuntu only — the three-platform matrix stays on GitLab.

- `actions/checkout` with `submodules: recursive`.
- Rust toolchain from `rust-toolchain.toml` (pinned `1.96.1`).
- Tauri v2 Linux system dependencies: `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`,
  `libayatana-appindicator3-dev`, `librsvg2-dev`, `libxdo-dev`, `libssl-dev`,
  `patchelf`, `build-essential`, `curl`, `wget`, `file`. The GitLab runners have
  these pre-provisioned on the host, so the list is not currently expressed
  anywhere in the repository and has to be written down here.
- Node 22, `npm ci`.
- The three project gates: `cargo build --workspace`,
  `cargo test -p athenaeum-core`, `npx tsc --noEmit`.
- Cargo registry and target caching.

The gate needs no secrets, so it runs on pull requests from forks.

What exists today on GitLab is narrower: `workflow.rules` admit a pipeline only
for `v*` tags and for pushes to `main`, every build job is `only: tags`, and the
sole job that runs on a `main` push is `check:headless-core`
(`cargo check -p athenaeum-core --no-default-features`). So `cargo test` and
`npx tsc --noEmit` are run by hand at release time, and this workflow is the
first time they run automatically.

D3 has a second-order effect here: with `main` as the trunk, `check:headless-core`
goes from running roughly once per release to once per trunk push. That is what
the job was written for — its own comment says it runs on every `main` commit so
a regression is caught — and it is cheap, so this is a fit rather than a cost.

## 8. Repository scaffolding

- **`README.md`** — add a "Building from source" section: recursive clone,
  prerequisites per platform, `npm run tauri dev`, `npm run dev:web` +
  `cargo run -p athenaeum-web`, and where the catalog data comes from.
- **`CONTRIBUTING.md`** — the project's cardinal rules, externalized from
  `CLAUDE.md`: both backends change together (Tauri command ⇄ Axum route), no
  `@tauri-apps/*` imports outside `src/api/`, the snake_case ↔ camelCase serde
  boundary, design tokens instead of raw colours, never swallow errors, and the
  three gates a PR must pass.
- **`SECURITY.md`** — a private reporting channel. The `sync`, `sharing` (iroh)
  and `account` modules are security-relevant.
- **`CODE_OF_CONDUCT.md`** — Contributor Covenant.
- **`.github/ISSUE_TEMPLATE/`** — a bug report asking for version, OS and the
  JSONL log from the log directory (this feeds the existing `/bug-triage`
  workflow), and a feature request.
- **`.github/PULL_REQUEST_TEMPLATE.md`** — a checklist mirroring the cardinal
  rules.
- **Delete `.github-export-ignore`.** It describes the abandoned filtered-mirror
  plan and is already stale: it names `scripts/sync-github.sh`, which does not
  exist, and `src-tauri/REFACTORING.md`, a pre-refactor path.

## 9. Housekeeping

`.git` is 4.0 GB, of which 3.91 GB is unreachable garbage — 42 727 objects in
the pack against 18 080 reachable. A push transfers only reachable objects
(~264 MB raw, far less compressed), so `git gc --prune=now` is comfort rather
than a blocker, but it should run before the first push so the initial upload is
predictable.

## 10. Order of operations

1. Secrets gate on `athenaeum` and `solvemyastro`.
2. Publish `solvemyastro`; verify the pinned commit is reachable.
3. Repoint `.gitmodules` and `solvemyastro/Cargo.toml`; verify the `rustafits`
   pin.
4. Fast-forward `main` to `0.5.1`.
5. Scaffolding commits (section 8) and the CI workflow (section 7).
6. `git gc`; create the GitHub repository; add remotes; push `main` and tags.
7. Clean-machine acceptance test: recursive clone plus `cargo build
   --workspace` with no LAN access.
8. Update `CLAUDE.md` (release workflow, branching model) and the auto-memory
   notes that encode the superseded rules.

## 11. Risks

| Risk | Mitigation |
| ---- | ---- |
| Publication is irreversible | The section-3 gate runs before the first push, over full history, on both repositories. |
| A submodule pin exists only locally, breaking every recursive clone | Explicit reachability check per submodule plus the clean-machine acceptance test in step 7. |
| The branching change silently contradicts the documented release workflow | Step 8 updates `CLAUDE.md` and the memory notes in the same cycle. |
| The GitHub Linux dependency list drifts from what the GitLab runners actually have | The list is written down in section 7 and lives in the workflow file; a red PR gate surfaces drift immediately. |
| A contributor's PR touches only one of the two backends | `CONTRIBUTING.md` and the PR template state the rule; review enforces it. |

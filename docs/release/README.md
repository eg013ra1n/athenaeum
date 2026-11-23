# Release


## Detail moved from CLAUDE.md (2026-10-20) - Release workflow and branching

### Release workflow

The procedure is the `release` skill (`.claude/skills/release/SKILL.md`): notes
(`RELEASE_NOTES.md` is the blog post), `scripts/release/bump.sh`, a release
commit that holds only notes + the seven version files + `Cargo.lock`, a green
`test/<version>` pipeline on our own runners, push, tag. The tag pipeline
(`.gitlab-ci.yml`) runs `gate → build → deploy → publish → verify → announce`:
it refuses a tag whose versions disagree, runs the tests again (`test:linux` on
the Linux VM, `test:windows` on EVO-X2) and uploads nothing until they and
`check:headless-core` are green, uploads to `artfrom.space/builds/<tag>/`
under the one naming scheme (`.gitlab/ci/scripts/artifact_names.sh`), publishes
Docker Hub and the docs site (post + download page generated from the notes),
fetches all of it back, and only then creates the GitLab Release, `version.json`
and the chat posts. Nothing is done by hand after the tag.

**Branching.** `main` is the development trunk and releases are tags on it. This
replaces the older "develop on a branch named after the version, ff-merge at
release" rule, which left `main` hundreds of commits stale — unworkable once
`main` is the default branch outside contributors base their pull requests on.
A release branch is cut only if a backport is ever actually needed.

### The assistant bridge in the installers

Every desktop installer carries the `athenaeum-mcp` bridge as a Tauri sidecar (MCP plan, decision D2; reference
`docs/mcp/README.md`, "Packaging and the bridge copy"). The sidecar is listed only in the overlay
`crates/athenaeum-tauri/tauri.mcp-bridge.json` (`bundle.externalBin`), because tauri-build fails every `cargo build` / `cargo test
-p athenaeum-tauri` whose listed sidecar file is missing. So each `npx tauri build` in `.gitlab-ci.yml` is preceded by
`node scripts/release/build-mcp-bridge.mjs [--target <triple>]` (release profile; without `--target` it uses the host triple from
`rustc -vV`) and carries `--config crates/athenaeum-tauri/tauri.mcp-bridge.json`. The script copies the bridge to
`crates/athenaeum-tauri/binaries/athenaeum-mcp-<triple>[.exe]` (gitignored) and fails with a non-zero exit when the cargo build fails. On
macOS the two targets each get their own bridge, built just before their `tauri build`; Tauri signs the sidecar with the app, so no
signing line mentions it. The Linux job needs nothing extra for the AppImage: the app copies the bridge to `<app-data>/bin` itself.

A local installer build that should carry the bridge does the same, with the debug profile if it is only a check:
`node scripts/release/build-mcp-bridge.mjs --debug`, then `npx tauri build --debug --bundles app --config
crates/athenaeum-tauri/tauri.mcp-bridge.json`; the bridge is then at `Athenaeum.app/Contents/MacOS/athenaeum-mcp`. A plain
`tauri build` (no overlay) ships no bridge. The jobs call `npx tauri build`, never `npm run tauri build -- ...`: the Windows job is PowerShell, which can eat the bare `--` and drop the overlay. The macOS and Linux jobs also fail when the built `.app` / `.deb` lacks the bridge. The release procedure itself is unchanged: `bump.sh` already covers
`athenaeum-mcp`'s version.

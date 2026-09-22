# In-app updates

> Moved verbatim out of `CLAUDE.md` on 2026-09-22. This file is the reference for the subsystem; `CLAUDE.md` keeps only the rules, the file map and a pointer here. A cycle that changes this subsystem updates THIS file (acceptance paragraphs, rulings, measurements) and touches `CLAUDE.md` only if a rule or a path in its summary changed.


Spec `docs/superpowers/specs/2026-09-16-in-app-updates-design.md`, plan
`docs/superpowers/plans/2026-09-16-in-app-updates-plan.md`. Hybrid: **core
owns the check** (`athenaeum-core/src/updates/` — `check` fetches
`https://artfrom.space/updates/latest.json` (+ `latest-beta.json` under
`updates.check_beta`) with the telemetry query `v/os/arch/commit/id` the
retired `version.json` GET used to carry, compares with `semver`, exposes the
manifest's notes; `whats_new` is once per version via
`updates.last_seen_version`, from the notes EMBEDDED at build time
(`include_str!("RELEASE_NOTES.md")` — the release commit holds notes and
versions together, so they are this build's by construction);
`release_notes` is the same text any time) and **`tauri-plugin-updater` owns
download / minisign verify / install / relaunch** (`commands/updates.rs`:
`install_update(channel: Channel)` — the invoke payload is `{ channel }`
directly, not a wrapped args struct — runs the plugin with
`MANIFEST_BASE_URL/<channel file>` and a `version_comparator` that
normalizes the plugin's `X.Y.Z-N` current version to `X.Y.Z-beta.N` — the
ONE place that normalization happens; `restart_app` is `AppHandle::restart`,
no `tauri-plugin-process` dependency). The plugin is used Rust-side only:
`updater:default` is NOT granted in `capabilities/default.json`, so the
webview's own ACL never sees it — only the five commands above cross that
boundary. Web mirrors answer the check and the notes and `501` for
install/restart (`athenaeum-web` carries its own `build.rs` for
`ATHENAEUM_GIT_HASH`, same pattern as the Tauri crate's). **Three version
forms**: Cargo dotted `0.6.5-beta.1`, `tauri.conf.json` `0.6.5-1`, tag +
manifest dotted. `platform_supported` keys on `<os>-<arch>[-<installer>]`
with the plugin's lookup order and refuses `deb`/`rpm` installs by rule;
manifest keys are `darwin-aarch64`, `darwin-x86_64`, `windows-x86_64-nsis`,
`windows-x86_64-msi`, `linux-x86_64` — no plain Windows key. Early refusals
before any download: debug build, install in flight, translocated/unwritable
macOS bundle, non-AppImage Linux. Events `update-progress { downloaded,
total, finished? }` / `update-ready { version }`; frontend state lives in
`src/contexts/UpdatesContext.tsx` (dialog phases `idle | downloading |
installing | ready | failed | restartFailed`), rendered by
`src/components/updates/{UpdateDialog,ReleaseNotes}.tsx` in `available` /
`whatsNew` modes. **Pipeline**: `bundle.createUpdaterArtifacts` +
`TAURI_SIGNING_PRIVATE_KEY` (protected; the key CANNOT be rotated —
1Password); `build:macos` notarizes the `.app` again (F5.1 reversed);
`deploy` publishes `.app.tar.gz` + every `.sig` beside the installers
(`updater_artifacts()` in `artifact_names.sh`, the same five platform keys
above); `publish:updater-manifest` (stage `publish`) writes the frozen
`updates/<tag>.json`; `verify:release` (stage `verify`, macOS runner) fetches
it back and verifies every URL and signature with `rsign2` (`cargo install
rsign2 --locked`, auto-installed by the job when missing); `publish:updater-
channel` (stage `announce`) copies it to `latest.json`/`latest-beta.json`.
`publish_version` (`version.json`) stays until v0.7.0 for pre-updater
installs. `docker/Dockerfile` copies `RELEASE_NOTES.md` into the image
(`.dockerignore` re-includes it) so core's `include_str!` has something to
embed in a Docker build too.

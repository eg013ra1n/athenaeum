# Release cycle review — 2026-09-15

Owner's complaints, verbatim in spirit: the docs-site blog post is sometimes
written by hand and sometimes by the agent; one pipeline covers the whole
release, so a failed Docker publish still ends with release notes going out;
every test runs on every release even when the release touched nothing the
tests cover; installer files carry no version in their names and the
architecture vocabulary (`aarch64` / `x64` / `amd64` / `x86_64`) is mixed —
the Perseus naming is the one to keep; and a backlog analysis of in-app
updates is wanted.

Everything below was measured on the last four tag pipelines (349 v0.6.0,
353 v0.6.1, 356 v0.6.2, 359 v0.6.3), the GitHub CI runs of the same commits,
the files on `artfrom.space/builds/`, `.gitlab-ci.yml`, `.github/workflows/
ci.yml`, the docs site's own CI, CLAUDE.md's "Release workflow" and the
release memories.

## 1. The cycle as it runs today

### 1.1 The manual part

1. Rewrite `RELEASE_NOTES.md`.
2. Bump the version in six files (CLAUDE.md says six; the release memory
   still says five — the Perseus crate was added later and the memory never
   caught up).
3. Run the four local gates: `cargo build --workspace`, `cargo test
   --workspace`, `cargo check -p athenaeum-core --no-default-features`,
   `npx tsc --noEmit` — a full-workspace compile and test run on the dev Mac.
4. Commit, tag, push `main` and the tag to both remotes.
5. Docs site, **by hand, in a second repository**: `blog/vX.Y.Z.md` (the
   release notes re-told as prose with its own frontmatter), a Version
   History row on the download page, commit, push. Its CI deploys.
6. Verify: pipeline green, Docker Hub tags, `version.json`, the builds dir.
7. Keep the dev Mac idle until the two macOS build jobs finish (it is the
   macOS runner; the v0.6.3 pipeline wedged it for an hour when it wasn't).

There is no `release` skill for Athenaeum. The procedure lives in three
places that disagree: CLAUDE.md "Release workflow" (six files), memory
`reference_release_workflow` (five files, stale line references, a
qemu-user-static prerequisite that the split-arch Docker jobs retired), and
`.claude/skills/rustafits-release` — a different product. Step 5 has no
owner at all, which is exactly why it is sometimes done and sometimes not.

### 1.2 The GitLab tag pipeline

Stages `build → deploy → release`. Seven build jobs (three Athenaeum, four
Perseus), one `deploy` that renames + scp's everything, then a `release`
stage where `release` (GitLab Release object), `publish_version`
(`version.json`), `notify:discord`, `notify:telegram` and the three
`publish:dockerhub:*` jobs all fan out from `release` as siblings — the
Docker jobs `allow_failure: true`, the notify jobs `needs: [release]` only.

**There is no test stage.** The main-branch pipeline runs only
`check:headless-core` (3–4 min). A green tag pipeline means "compiled and
uploaded", never "tests pass".

Wall clock and critical path:

| Pipeline | Total | `build:macos` | `build:perseus:macos` (queued behind it) | Docker |
| ---- | ---- | ---- | ---- | ---- |
| 349 v0.6.0 | 73 min | 53 min | 14 min, 53 min in queue | all three FAILED in seconds; notify posted the same minute |
| 353 v0.6.1 | 79 min | 51 min | 14 min, 51 min in queue | ok |
| 356 v0.6.2 | 62 min | 35 min | 15 min, 35 min in queue | ok |
| 359 v0.6.3 | 84 min | 60 min timeout → retry 12 min | canceled → 5 min | arm64 killed by the runner wedge, retried |

The macOS leg is the critical path every time: one runner (the dev Mac),
`concurrent = 1`, two Tauri targets each signed and notarized as an `.app`
by Tauri AND then notarized again as a `.dmg` by the job (four notary
round-trips per release), and Perseus's own two-target build waits behind it.
Windows and Linux finish in 15 min and then idle for half an hour.

The v0.6.0 row is the owner's complaint in one line: the Docker image had
been broken since v0.5.6 (fixed in v0.6.1), all three Docker jobs failed at
23:47, and the Discord/Telegram announcements went out at 23:47. Nothing in
the graph waits for Docker.

### 1.3 GitHub CI

`ci.yml` runs the full workspace on Ubuntu and on Windows for every push to
`main`, every tag and every PR (docs-only paths excluded). It is the ONLY
place tests run. The tag run is concurrent with the GitLab build, so it can
name a bad release, not prevent one.

Measured on the v0.6.3 release commit's `main` run (34908768151):

| Job | Total | Compile | Tests | tsc |
| ---- | ---- | ---- | ---- | ---- |
| Linux | 23 min | 18 min | 4 min | 13 s |
| Windows | 20 min | 13 min | 7 min | — |

The last four release commits' `main` runs: v0.6.0 red, v0.6.1 red, v0.6.2
red, v0.6.3 red (the tag run of v0.6.3 green, the `main` run red on
scheduling jitter, fixed the next morning). Four releases in a row were
published over a red test gate, and the gate is the one the owner does not
wait for because it is not in the release path.

On the owner's "tests run everything every time": true, but the numbers say
the tests are 4–7 of the 20–23 minutes. Compile is the cost. The release
commit touches six manifests, so every workspace crate recompiles from the
dependency cache; that is inherent and the same for any code commit.
Selective test running would save at most the test minutes and costs the
one thing this suite has been burned by (the feature-unification trap:
`cargo test -p <subset>` changes the unit graph and rebuilds 77 crates —
`reference_github_ci_tuning`).

### 1.4 Artifact names as published (v0.6.3)

| Product | Published name | Stable alias the download page links | Vocabulary |
| ---- | ---- | ---- | ---- |
| Athenaeum macOS | `Athenaeum_v0.6.3_aarch64.dmg` / `Athenaeum_v0.6.3_x64.dmg` | `Athenaeum-macos-aarch64.dmg` / `-x64.dmg` | `aarch64`, `x64`, capital A, underscore, `v` |
| Athenaeum Windows | `Athenaeum_v0.6.3_x64_en-US.msi` / `Athenaeum_v0.6.3_x64-setup.exe` | `Athenaeum-windows-x64.msi` / `-x64-setup.exe` | `x64`, an `en-US` Tauri leftover |
| Athenaeum Linux | `athenaeum_v0.6.3_amd64.AppImage` / `athenaeum_v0.6.3_amd64.deb` | `athenaeum-linux-x86_64.AppImage` / `-amd64.deb` | lower-case a, `amd64` in the file, `x86_64` in the alias |
| Perseus | `perseus-0.6.3-macos-arm64.dmg`, `-macos-x64.dmg`, `-windows-x64-setup.exe`, `-linux-amd64.tar.gz`, `-linux-arm64.tar.gz` | `perseus-macos-arm64.dmg` … | `arm64`, `x64`, `amd64`, dash, no `v` — but the debs are `perseus_v0.6.3_amd64.deb` |

Two separate defects: (a) the versioned files exist on the server, but the
download page and `builds/latest/` hand out the alias, so what lands in
`~/Downloads` is `Athenaeum-macos-aarch64.dmg` for every release — the
"which version is this" problem is a download-page problem, not a naming
problem; (b) four spellings of two architectures and three spellings of the
product name. The Perseus scheme is the model except for its own debs.

## 2. Findings

Ranked by how much they cost the owner per release.

### F1 — Announcements do not wait for the publish to complete

`notify:*` and `publish_version` need only `release`; Docker is a sibling
with `allow_failure: true`. A Docker failure produces a yellow pipeline and a
finished announcement. `version.json` is also written before the Docker
image exists, so the in-app update check points Docker users at a version
whose image is not there.

**Fix.** Split the tail into three stages and make the graph honest:

```
build → deploy → publish → verify → announce
                 ├ publish_version        (moved: after verify, see below)
                 ├ publish:dockerhub:amd64
                 ├ publish:dockerhub:arm64
                 ├ publish:dockerhub:manifest
                 └ docs:publish            (new, F3)
verify:release   needs every publish job — curls every installer URL, the
                 Docker Hub tag manifest, the blog URL; fails red on any miss
announce         release (GitLab Release), publish_version, notify:discord,
                 notify:telegram — needs: [verify:release]
```

`allow_failure` stays on the Docker jobs (a Hub outage must not require a
re-tag), but `verify:release` is blocking, so a Docker failure now means:
yellow Docker job → red verify → no announcement, no `version.json`. The
owner retries the one Docker job; verify and announce re-run. GitLab
Release creation moves to `announce` too — it is an announcement, and today
its `curl` fallback silently posts `"Release vX"` when `RELEASE_NOTES.md`
fails to encode.

### F2 — The release path has no test gate

Neither the tag pipeline nor the manual procedure blocks on the workspace
tests; four releases went out over red. The local gates in step 3 of §1.1
are the same suite on the dev Mac (20+ min) and are what the owner is
paying for twice.

**Fix, two parts.**

- *Skill-level (zero CI cost).* The release skill (F6) refuses to push a tag
  unless the GitHub check-suite of the commit being tagged is green
  (`gh run list --commit <sha>`), and demands that the release commit
  contains only `RELEASE_NOTES.md`, the six version files and
  `Cargo.lock` — a code fix rides in its own commit and gets its own green
  run first. With that rule the release commit never needs a re-run of the
  suite for itself: "the tests ran on this code" is HEAD~1's run. This is
  the direct answer to "tests run every time even when nothing changed":
  stop re-running them locally at release time; wait for the run that
  already happened.
- *Pipeline-level (belt).* A `gate:tests` job at the head of the tag
  pipeline (`needs: []`, blocking) that asks the GitHub API for the tagged
  commit's check-suite conclusion (read-only token as a protected variable)
  and fails if it is not `success`. Cheap, no runner time, closes the
  "tag pushed by hand" hole.

The local gate list in CLAUDE.md shrinks to what CI cannot do: nothing. The
headless check and tsc already run in CI; `cargo build --workspace` is
subsumed by `cargo test --no-run`.

### F3 — The docs site is a manual second repository

The blog post and the Version History row are the only release artefacts
with no automation, and `notify:*` links `artfrom.space/blog/<slug>/`, so a
forgotten post is a public 404 in two chats.

**Fix.** `docs:publish` job in the `publish` stage: clone `artfrom-space`
with a project deploy token, generate `src/content/docs/blog/vX.Y.Z.md` from
`RELEASE_NOTES.md` (frontmatter: title `Athenaeum vX.Y.Z`, date, author,
tag `release`, `excerpt` = the italic tagline line that every
`RELEASE_NOTES.md` already opens with), insert the Version History row
(`| vX.Y.Z | date | <tagline> |`), commit, push. The docs CI deploys;
`verify:release` polls the blog URL until 200 (10 min budget).

This forces one decision the owner has been making implicitly: **one text
or two.** Today the blog post is richer prose than the notes (compare
`RELEASE_NOTES.md` and `blog/v0.6.3.md`). Automation means the notes ARE the
post. Recommendation: one text, written once in `RELEASE_NOTES.md` at blog
quality; the Version History row is the tagline, nothing hand-written. The
landing-page version already follows the newest `blog/v*.md` filename
(`sync-landing-version.mjs`), so nothing else on the site needs touching.

### F4 — Installer names

**One scheme, both products, Perseus style:**

```
<product>-<version>-<os>-<arch>[-<variant>].<ext>

athenaeum-0.6.4-macos-arm64.dmg      athenaeum-0.6.4-macos-x64.dmg
athenaeum-0.6.4-windows-x64.msi      athenaeum-0.6.4-windows-x64-setup.exe
athenaeum-0.6.4-linux-x64.AppImage   athenaeum-0.6.4-linux-x64.deb
perseus-0.6.4-macos-arm64.dmg        perseus-0.6.4-macos-x64.dmg
perseus-0.6.4-windows-x64-setup.exe
perseus-0.6.4-linux-x64.deb          perseus-0.6.4-linux-x64.tar.gz
perseus-0.6.4-linux-arm64.deb        perseus-0.6.4-linux-arm64.tar.gz
```

Rules: product lower-case, version without `v`, `os ∈ {macos, windows,
linux}`, `arch ∈ {x64, arm64}` and nothing else (`amd64`/`x86_64`/`aarch64`
never appear in a filename; Debian's own `amd64` stays inside the package
control file where it belongs), dashes only. The `en-US` MSI suffix goes.
Stable aliases keep existing for external links but drop the version, not
the vocabulary: `athenaeum-macos-arm64.dmg`, `athenaeum-linux-x64.AppImage`.

**The download page links the versioned files.** With `docs:publish` (F3)
the page is regenerated at every release anyway, so the "Latest Build"
tables can carry `builds/vX.Y.Z/<versioned name>` directly; the
`builds/latest/` aliases remain for scripts and old links. That is what
makes the file in `~/Downloads` say which version it is — no nginx
`Content-Disposition` gymnastics (the alias is a symlink; nginx cannot name
the target without a per-file rule the deploy job would have to install
with sudo).

Implementation is confined to the `deploy` job's `mv`/`ln` block, the
`release` job's asset links, the download page template and the Perseus
deploy lines. The old versioned names of past releases stay as they are.

### F5 — The macOS leg is the whole release's wall clock

35–53 min of every 62–84 min pipeline, then 14 min of Perseus behind it,
on the dev Mac, `concurrent = 1`. Three levers, in order of payoff over
risk:

1. **Notarize each DMG once, not the app and then the DMG.** Tauri
   notarizes the `.app` when `APPLE_API_*` are in its environment; the job
   then submits the `.dmg` (which contains the same app) and waits again.
   Pass only `APPLE_SIGNING_IDENTITY` to `tauri build` (signing, no
   notarization) and keep the single DMG `notarytool submit --wait` +
   staple. Two notary waits instead of four per release; Perseus's
   `build_app.sh` already works this way. Verify on one release that
   Gatekeeper accepts the result (a stapled DMG with a signed app inside is
   the documented layout).
2. **Drop the macOS x64 target if nobody uses it.** The adoption dashboard
   keys on `os`, not arch (`version.json?os=macos`) — one release with an
   `arch` query parameter (or the updater's `{{arch}}` path, §3) answers
   it. Halves the macOS job outright if the answer is zero.
3. **A runner that is not the dev Mac.** A Mac mini as a dedicated runner
   removes the "keep the machine idle for an hour" rule and the LaunchAgent
   wedge class of incident (`reference_macos_runner_wedge`). Hardware cost;
   owner's call, listed for completeness.

### F6 — The procedure is three documents and no script

Bump ×6 by hand, four local gates by hand, the docs repo by hand, and the
memory says ×5. **Fix:** a `.claude/skills/release/SKILL.md` for Athenaeum
(the counterpart of `rustafits-release`) that is the ONE procedure, and a
`scripts/release/bump.sh <version>` that edits the six files, runs `cargo
check` for `Cargo.lock`, and asserts the six agree; plus a
`check:versions` job at the head of the tag pipeline asserting tag ==
every crate/package version (today only the Perseus deb *warns* on a
mismatch). CLAUDE.md's "Release workflow" section becomes a pointer to the
skill; the memory is corrected (done in this review: ×6, qemu note
removed).

### F7 — Smaller things seen on the way

- `update-cache:*` manual jobs are dead weight since the build jobs went
  `pull-push` (the comment on `build:windows` says so). Delete.
- `deploy` and `publish_version` use unpinned `ssh-keyscan`; the docs
  site's CI pins via `SSH_KNOWN_HOSTS` with a fail-closed rsync. Same
  treatment here.
- `publish_version` writes `{version, download_url}` only. Once `latest.json`
  exists for the updater (§3) it carries `notes_url`, `pub_date` and
  per-platform URLs; `version.json` stays until the adoption log shows no
  pre-updater versions checking in.
- The Windows GitHub job runs on every PR. PRs from forks need approval
  anyway; on `main` and tags it earns its 20 min. Consider `main` + tags
  only for PRs that touch nothing under `crates/` — a path filter, not a
  crate exclusion (F2's trap).
- `cargo nextest` would cut the 4–7 min test phase roughly in half
  (per-test parallelism across the 41 binaries) and turn the two `--skip`s
  into a named filter set with per-test retries for the load-sensitive
  pair. Second-order; do it after F1–F4.

## 3. Backlog: in-app updates

**What it is.** Tauri 2's `tauri-plugin-updater` + `tauri-plugin-process`:
the app fetches a manifest, compares versions, downloads the platform
artifact, verifies a minisign signature against a public key baked into
`tauri.conf.json`, installs, relaunches. The download page stops being the
upgrade path for desktop users; it stays the first-install path.

**Manifest.** A static JSON at a fixed URL (no server code):

```json
{
  "version": "0.6.4",
  "notes": "…tagline…",
  "pub_date": "2026-09-20T00:00:00Z",
  "platforms": {
    "darwin-aarch64":  { "url": "https://artfrom.space/builds/v0.6.4/macos/athenaeum-0.6.4-macos-arm64.app.tar.gz", "signature": "…" },
    "darwin-x86_64":   { "url": "…macos-x64.app.tar.gz", "signature": "…" },
    "windows-x86_64":  { "url": "…windows-x64-setup.exe", "signature": "…" },
    "linux-x86_64":    { "url": "…linux-x64.AppImage",   "signature": "…" }
  }
}
```

Platform keys are the plugin's `OS-ARCH` vocabulary (`darwin-aarch64` etc.)
and are internal to the manifest; the file names follow F4. The plugin
validates the whole file before reading `version`, so the generator must
never emit a platform entry without both fields — omit a platform whose
build failed rather than leave it half-filled.

**Artifacts.** `bundle.createUpdaterArtifacts: true` makes Tauri emit, next
to the normal bundles: macOS `<app>.app.tar.gz` + `.sig`, Windows the NSIS
`-setup.exe` (and MSI) + `.sig`, Linux the `.AppImage` + `.sig`. Signing
needs `TAURI_SIGNING_PRIVATE_KEY` (+ password) as protected CI variables
from `tauri signer generate`. **The private key is the one secret that
cannot be rotated**: losing it means no installed copy can ever be updated
in-app again — it goes to 1Password the day it is generated.

**Coverage.** Desktop macOS (both arches), Windows, Linux AppImage. Not
covered, by construction: Linux `.deb` (package managers own it), Docker/web
(image pull), Perseus (headless; its own item — a `perseus update`
subcommand or an apt repo). The existing `isTauri` gate already hides the
update UI on web.

**Two decisions the design must settle before code.**

1. *One Windows installer.* Tauri ships MSI and NSIS; the plugin downloads
   whatever the manifest names and runs it. An MSI-installed copy updated
   through the NSIS exe ends with two Add/Remove entries. Recommendation:
   the updater serves the NSIS exe (`installMode: passive`, the documented
   default) and the download page stops offering the MSI for new installs,
   or the MSI becomes a "for deployment tools" secondary link. Verify the
   plugin's current behaviour for an MSI-installed app before deciding.
2. *The `-N` beta version form.* `tauri.conf.json` carries `0.6.4-1` where
   the tag says `0.6.4-beta.1` because Tauri's bundler rejects the dotted
   form. The plugin compares the manifest against the app's OWN version
   (tauri.conf), and SemVer ranks a numeric prerelease below an
   alphanumeric one: installed `0.6.4-2` vs manifest `0.6.4-beta.2` reads as
   "update available" for the very build that is running. Either betas
   stop going through the updater (stable channel only, `latest.json`), or
   the beta manifest carries the `-N` form the app really has. Decide at
   design time; it also touches the existing `version_is_newer` check.

**Telemetry.** The adoption dashboard is fed by the nginx access log of the
`version.json?v=&os=&commit=&id=` GET (`project_adoption_monitoring`). The
updater's own request lands in the same log; with the endpoint template
`…/{{target}}/{{arch}}/{{current_version}}` it even adds the arch the dashboard
lacks, but not `id`. Keep the legacy GET as the telemetry ping for one
cycle (the app can issue both), or add the `id` as a custom header via
`updater_builder().header()` and extend the astronet parser. Either way the
adoption pipeline must not go dark on the day the updater ships.

**UI.** The About page's manual check and the startup auto-check
(`useAutoUpdateCheck`) already exist; they gain a "Download and install"
action with progress (Tauri channel → `notify()` toast, kind `update`,
which already exists in the kind union) and a "Restart now" on completion.
Settings: the existing `updates.auto_check` opt-out; maybe "download
automatically, ask before restart".

**CI.** After `deploy`, a `publish:updater-manifest` job collects the four
`.sig` files from the build artifacts, writes `latest.json` (stable) or
`latest-beta.json` (beta), uploads to `/var/www/artfrom.space/updates/`, and
`verify:release` (F1) fetches it back and checks every URL it names
answers 200. It belongs in the `publish` stage — the announcement must not
go out before the updater can serve the release.

**Effort.** Roughly three working days plus owner smokes: plugin, config,
key generation and storage (½); bundler flag, CI artifacts, manifest job,
verify (1); UI, settings, notifications, both hosts' command mirrors (1);
a real update on each OS from the previous release (½ — needs the previous
build installed, which is exactly the "download a new installer every day"
state the owner is in now). Prerequisite: F4 (names) and F1 (verify stage)
first, so the manifest is written against the final names into a pipeline
that checks them.

**Risks.** macOS: the app must live where the user installed it, not in a
translocated DMG mount (normal after drag-to-Applications). Windows:
`installMode` and the MSI question above. All: a bad manifest bricks the
in-app path for every user until the next manifest, so the generator is
tested by a script in `.gitlab/ci/scripts/test/` like the notify helpers.

## 4. Verdict and proposed order

The cycle's defects are structural, not effort: the pipeline announces
before it has published, the only test gate is outside the release path,
one of the seven release artefacts is manual, and two products name their
files four different ways. None of it needs new product code.

Proposed as three small CI cycles and one product cycle, each its own
plan:

1. **Pipeline shape + names** (F1, F4, F7's dead jobs): stages
   `build → deploy → publish → verify → announce`, one naming scheme,
   `check:versions`. One release to prove it. No product change.
2. **Docs automation + release skill** (F3, F6, F2's skill-level gate):
   `docs:publish`, `bump.sh`, `.claude/skills/release`, the green-check
   rule, CLAUDE.md and memory collapsed to one pointer. The blog post
   becomes a by-product of `RELEASE_NOTES.md`.
3. **Time** (F5.1, F2's pipeline gate, nextest, path filters): halve the
   notary waits, gate the tag on the GitHub check-suite, faster tests.
   F5.2 (drop macOS x64) and F5.3 (a dedicated runner) are owner questions.
4. **In-app updater** (§3): its own spec after 1–2 land, because it
   depends on final names and on a verify stage.

Owner decisions needed before cycle 2: one release text or two (F3); and
before cycle 4: one Windows installer, and whether betas go through the
updater at all.

## 5. Measured: nextest (Task 8, 2026-09-15)

`cargo nextest` replaced `cargo test` in the `Test workspace` step of both
GitHub jobs and was measured against run 34908768151, the last plain run on
`main`. Run 34981151619 carried it over two attempts, cold cache and warm. Both
were green over an identical test set (3172 baseline passes plus 35 ignored
against nextest's 3171 run plus 35 skipped; the one difference is the
CI-self-skipping `ingest_releases_conn_between_frames`, a pass for the plain
runner and an exclusion for the filter expression).

| Job | Step, baseline | Step, cold | Step, warm | nextest's own run |
| ---- | ---- | ---- | ---- | ---- |
| Linux | 3:55 | 3:31 | 3:32 | 2:45 |
| Windows | 6:41 | 7:49 | 10:25 | 6:30 / 9:21 |

Linux is a real 30 % cut on the run itself; the step shows only 6 % because
installing nextest, listing the tests and relinking cost about 45 s the plain
runner does not pay. Windows is a loss, and the loss is one test.

### One test was 82 % of the Windows run

`db::schema::init_db_concurrency_tests::concurrent_init_db_on_shared_file_is_race_free`
took 4 min 24 s (cold) and 7 min 38 s (warm) under nextest against about 60 s
under the plain runner, and everything else finished long before it. The next
slowest, `db::repair::tests::init_db_repairs_a_catalog_that_already_holds_erased_rows`,
was just over a minute. Both are `init_db` tests on a file-backed database.

### Root cause: a hand-copied pragma list that had drifted

Neither test was slow because of nextest, and neither was slow because of what
it tests. Both opened their connection with SQLite's default durability instead
of production's. `SqliteConnectionManager::setup_connection` runs
`synchronous = NORMAL` under WAL; the concurrency test restated a shortened
pragma list (`busy_timeout` and WAL only, so `synchronous` stayed at the FULL
default) and the repair test opened a bare connection (rollback journal, FULL).
`init_db` is about 83 statements, each its own implicit transaction, and the
concurrency test runs it 400 times. That is roughly 33 000 fsyncs the
application itself never performs.

It stayed invisible because the whole team's development machines are macOS,
where `fsync` does not flush to media unless a connection asks for
`F_FULLFSYNC`. Forcing that locally reproduces the CI behaviour exactly:

| Configuration, same test, same machine | Time |
| ---- | ---- |
| as written, macOS default fsync | 1.3 s |
| as written, `PRAGMA fullfsync = ON` | 67.8 s |
| production pragmas, `PRAGMA fullfsync = ON` | 1.5 s |

67.8 s against the Windows runner's ~60 s is the same number, so the mechanism
is settled.

### The fix, and what was checked

Both tests now call `SqliteConnectionManager::setup_connection` instead of
restating its pragmas, which is also why it became `pub(crate)`: a copied list
is what drifted, so there is no longer a copy. `stacking::test_fixtures`'s
shared constructor got the same treatment, since most of its callers hand it a
file-backed connection too.

Durability is orthogonal to what the concurrency test proves, and that was
verified rather than assumed: with `init_db`'s mutex removed and the production
pragmas in place, the test still fails 5 runs out of 5, on the original
`CREATE TRIGGER ... already exists`. Its 50 iterations were left alone. Across
six such runs the race first landed on iterations 0, 4, 5, 10, 23 and 27, so
trimming the loop to make the test look fast would have quietly cost it the bug
it exists for. The whole crate is green: 2370 passed.

### Settled: the plain runner wins (2026-09-16)

The plan's bar was 65 % of a baseline, and that baseline turned out to be
worthless — it predated the fix above, which helps BOTH runners and on Windows
helps a great deal. So the pair was measured again on the fixed tests, doctests
counted on both sides (nextest cannot run them and needs a step of its own):

| Job | `cargo test` | nextest |
| ---- | ---- | ---- |
| Linux | 165 s | 187 s and 212 s |
| Windows | 357 s | 388 s and 390 s |

The plain runner is faster on both, so it stays and nextest is out. Its whole
advantage here had been overlapping those two slow tests with everything else;
once they stopped being slow there was nothing left to overlap, and
process-per-test scheduling is pure overhead on a 4-CPU runner with ~3 200
tests. Re-opening this needs new numbers, not a new opinion.

It was still worth doing. nextest is what made the slow tests findable: under
the plain runner that test was one minute lost among two thousand siblings;
given a process of its own it became seven minutes and 82 % of the job. The
pipeline is faster today because of a tool we then removed.

The path filter from the same task (`What changed`, PR-only) is orthogonal and
stays — both extra jobs skipped correctly on the push to `main`.

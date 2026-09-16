# In-app updates with release notes — design

Date: 2026-09-16
Status: approved for planning
Supersedes: the "Backlog: in-app updates" analysis, §3 of
`docs/superpowers/research/2026-09-15-release-cycle-review.md`

## 1. Goal

A desktop user of Athenaeum learns about a new version, reads its release
notes, downloads and installs it and restarts — all inside the application,
on macOS (both architectures), Windows and Linux AppImage. The download page
becomes the first-install path only. A Docker/web user learns about the new
version and reads the same notes inside the web UI, with the `docker pull`
line to run; the image itself is pulled by the operator as today. Every user,
on every host, sees "What's new" once after the first launch of a new
version.

What the user sees afterwards:

- Desktop, at launch: a toast *Update available: v0.6.5* → the bell → the
  **Update available** dialog with the full release notes → **Download and
  install** with a byte progress bar → **Restart now** (macOS/Linux) or the
  installer relaunching the app by itself (Windows).
- Any host, first launch after an update: a **What's new in v0.6.5** dialog
  with that build's own notes. Once.
- Docker/web, at launch: the same toast; the dialog shows the notes and
  `docker pull vsharifov/athenaeum:0.6.5` with a copy button instead of an
  install button.
- Settings: nothing new. The existing `updates.auto_check` (default on) and
  `updates.check_beta` (default off) keep their meaning; `auto_check` now
  applies to the web host too.

Owner decisions taken during the brainstorm (2026-09-16):

| # | Decision |
| - | -------- |
| D1 | Notes are shown both BEFORE an update (in the *available* dialog, from the manifest) and AFTER it (*What's new* on first launch, from the build's own embedded notes). |
| D2 | The web host checks for updates server-side (Axum → artfrom.space), opt-out via `updates.auto_check`; Docker installs thereby enter the adoption dashboard. |
| D3 | Nothing is downloaded without a click. No background download, no automatic install. |
| D4 | Ruling F5.1 of the release-cycle review is REVERSED: `tauri build` notarizes the `.app` again, so the updater's `.app.tar.gz` carries a stapled bundle. +2 notary waits per release. |
| D5 | One `UpdateDialog` component in two modes (*available* / *whatsNew*), shared by both hosts; About keeps a one-line "Check for updates". |
| D6 | Hybrid architecture (§2): core owns *is there an update and what is in it*; `tauri-plugin-updater` owns *download, verify, install, relaunch* on desktop only. |

## 2. Architecture

### 2.1 Responsibilities

| Concern | Owner | Hosts |
| ------- | ----- | ----- |
| Fetch the channel manifest, telemetry query, pick the channel, compare versions, expose notes | `athenaeum-core::updates` | desktop + web |
| "What's new" once per version (`updates.last_seen_version`) | `athenaeum-core::updates` | desktop + web |
| Download the platform artifact, verify its minisign signature, install, relaunch | `tauri-plugin-updater` + `tauri-plugin-process` | desktop only |
| Produce signed updater artifacts, the manifest, verify and move the channel file | GitLab tag pipeline | — |

The plugin re-reads the manifest when the user clicks install (its `check()`
is the only way to obtain an `Update` handle). That is one extra GET of a
static file per install and is accepted; the two "is it newer" answers are
computed by the same core function (§4), so they cannot disagree.

### 2.2 `athenaeum-core/src/updates/` (new, ungated)

`reqwest` and `uuid` are already core dependencies; `semver` moves from
`athenaeum-tauri` to core.

- `manifest.rs` — `UpdateManifest { version: String, notes: Option<String>,
  pub_date: Option<String>, platforms: HashMap<String, PlatformEntry> }`,
  `PlatformEntry { url: String, signature: String }` — a serde mirror of the
  plugin's static format. `fetch_manifest(client, channel, ping)` performs
  `GET https://artfrom.space/updates/<latest|latest-beta>.json?v=<cargo
  version>&os=<std::env::consts::OS>&arch=<ARCH>&commit=<git hash>&id=<installation id>`
  with a 10 s timeout. Any failure is an `Err` logged at `error!` with `url`
  and `error` — never `None`, never silent (the old `fetch_version_json`
  returned `Option` and swallowed the cause).
- `version.rs` — `is_newer(remote: &str, current: &str) -> bool` over the
  `semver` crate (SemVer 2.0 precedence, leading `v` stripped, unparseable →
  `false` with a `warn!`), and `normalize_tauri_version(&str) -> String`
  mapping `X.Y.Z-N` to `X.Y.Z-beta.N` and leaving every other form alone.
  The existing `version_is_newer` tests move here.
- `notes.rs` — `EMBEDDED_RELEASE_NOTES: &str = include_str!("../../../../RELEASE_NOTES.md")`.
  The release commit holds `RELEASE_NOTES.md` together with the six version
  files (release skill), so the embedded text is BY CONSTRUCTION the notes of
  the build that carries it.
- `mod.rs` —

  ```rust
  pub enum HostKind { Desktop, Web }
  pub enum Channel { Stable, Beta }

  #[derive(Serialize)] #[serde(rename_all = "camelCase")]
  pub struct UpdateCheck {
      pub current_version: String,     // CARGO_PKG_VERSION, dotted form
      pub latest_version: String,      // manifest version, dotted form
      pub is_update_available: bool,
      pub channel: Channel,            // which manifest won
      pub notes: Option<String>,       // manifest `notes` (Markdown)
      pub pub_date: Option<String>,    // RFC 3339, as published
      pub platform_supported: bool,    // an entry for this host's `<os>-<arch>` exists AND host == Desktop
      pub download_page_url: String,   // https://artfrom.space/releases/download
      pub blog_url: String,            // https://artfrom.space/blog/v<latest_version>
      pub docker_image: Option<String>,// Some("vsharifov/athenaeum:<latest>") when host == Web
  }

  pub struct WhatsNew { pub version: String, pub notes: String, pub blog_url: String }

  pub async fn check(ctx: &ServiceContext, host: HostKind, git_hash: &str) -> Result<UpdateCheck>;
  pub fn whats_new(ctx: &ServiceContext) -> Result<Option<WhatsNew>>;   // one-shot: marks the version seen
  pub fn release_notes() -> WhatsNew;                                    // the running build's notes, any time
  ```

  `check`: reads `updates.check_beta` and `installation.id` (the UUID
  generation moves here from the Tauri command, unchanged semantics: one
  per app DB, created on first use); fetches `Stable` always and `Beta` when
  opted in; the newer of the two wins (`is_newer`); `is_update_available =
  is_newer(latest, current)`. `platform_supported` is `false` on the web host
  by definition and `false` on desktop when the manifest has no
  `<os>-<arch>` entry (a `.deb` install is desktop but has none — the plugin
  target vocabulary is `darwin-aarch64`, `darwin-x86_64`, `windows-x86_64`,
  `linux-x86_64`).

  `whats_new`: compares `updates.last_seen_version` with the current
  (dotted) version. Missing key = fresh install: write the key, return
  `None` (a new user is not greeted with a change list). Different: write
  the key, return the embedded notes. Equal: `None`. The write happens
  before returning, so a crash in the dialog cannot show it twice.

### 2.3 Tauri host — `commands/updates.rs` (new; the update parts of
`commands/core.rs` move here)

| Command | Does |
| ------- | ---- |
| `check_for_updates` | `core::updates::check(ctx, Desktop, env!("ATHENAEUM_GIT_HASH"))` |
| `get_whats_new` | `core::updates::whats_new` (one-shot) |
| `get_release_notes` | `core::updates::release_notes` (About's **View release notes**, never touches the key) |
| `install_update { channel }` | early refusals (§6.2) → `app.updater_builder().endpoints([channel url]).version_comparator(cmp).build()?.check()` → `download_and_install(on_chunk, on_finish)` → emits `update-ready` |
| `restart_app` | `tauri_plugin_process::restart` (macOS/Linux; on Windows the NSIS installer relaunches, the command is never reached) |

`cmp` is `|current, release| is_newer(&release.version.to_string(),
&normalize_tauri_version(&current.to_string()))` — the plugin's "current"
is `tauri.conf.json`'s `X.Y.Z-N` (§4). `install_update` runs on the async
runtime; progress rides the Tauri event `update-progress { downloaded:
u64, total: Option<u64> }` throttled to ≥ 300 ms, and `update-ready {
version }` fires once on success. A running install is recorded on
`AppState` (`update_in_flight: AtomicBool`); a second call is refused.

Plugins: `tauri-plugin-updater = "2"`, `tauri-plugin-process = "2"` in
`Cargo.toml` and `lib.rs` (`app.handle().plugin(tauri_plugin_updater::Builder::new().build())`
in `setup`, `tauri_plugin_process::init()`); npm `@tauri-apps/plugin-updater`
and `@tauri-apps/plugin-process` are NOT added — the frontend never calls
the plugins directly, it goes through `api.invoke` (CLAUDE.md rule).
Capabilities: `updater:default`, `process:allow-restart` in
`capabilities/default.json` and the inline capability in `tauri.conf.json`.

### 2.4 Web host — `routes/updates.rs` (new; the stub in `routes/mod.rs` goes)

| Route | Does |
| ----- | ---- |
| `POST /api/check_for_updates` | `core::updates::check(ctx, Web, env!("ATHENAEUM_GIT_HASH"))` — the web crate gets the same `build.rs` the Tauri crate has |
| `POST /api/get_whats_new` | `core::updates::whats_new` |
| `POST /api/get_release_notes` | `core::updates::release_notes` |
| `POST /api/install_update`, `POST /api/restart_app` | `501 Not Implemented`, body `updates are installed by pulling the image` |

### 2.5 Frontend

- `src/types/helpers.ts`: `UpdateInfo` becomes the camelCase mirror of
  `UpdateCheck` (rename to `UpdateCheck`; the old snake_case fields go);
  `WhatsNew` added.
- `src/contexts/UpdatesContext.tsx` (new): the last `UpdateCheck`, dialog
  state (`closed | available | whatsNew`), install phase (`idle |
  downloading { downloaded, total } | ready | failed { message }`),
  `openDialog(mode)`, `install()`, `restart()`. Listens to `update-progress`
  and `update-ready` with the cancelled-flag pattern from CLAUDE.md.
- `src/hooks/useAutoUpdateCheck.ts`: the `isTauri` gate is removed (D2);
  on launch, `get_whats_new` runs first and opens the dialog when `Some`,
  then the update check runs and raises the toast (`kind: 'update'`,
  `link: '/about?update'`, `dedupeKey: 'update-<version>'`). When both
  happen the dialog wins and the toast waits in the bell.
- `src/pages/About.tsx`: the Updates section shrinks to one line —
  `Athenaeum v0.6.4 · Check for updates · View release notes` — plus the
  check result; `?update` in the query opens the *available* dialog on
  mount.
- `src/components/updates/UpdateDialog.tsx` (new): §5.
- `react-markdown` + `remark-gfm` are the only new frontend dependencies.

## 3. Manifest and pipeline

### 3.1 Signing key

Generated once by the owner with `tauri signer generate`. The private key
and its password live in 1Password and in the GitLab project variables
`TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
(protected + masked). The public key is committed in `tauri.conf.json`
(`plugins.updater.pubkey`). **There is no rotation**: a new public key
reaches installed copies only inside an update signed by the old key.
Losing the private key means every installed copy updates by hand from the
site, forever. The release skill records this.

Consequence for local builds: with `bundle.createUpdaterArtifacts: true`,
`npm run tauri build` refuses to bundle without the key in the environment.
`tauri dev` is unaffected. A local release-style build exports the two
variables by hand (documented in the release skill).

### 3.2 `tauri.conf.json`

```json
"bundle": { "createUpdaterArtifacts": true, ... },
"plugins": {
  "updater": {
    "pubkey": "<base64>",
    "endpoints": ["https://artfrom.space/updates/latest.json"],
    "windows": { "installMode": "passive" }
  }
}
```

The configured endpoint is the formal default; `install_update` sets the
endpoint for the channel that won the check.

### 3.3 Build jobs

- `build:macos`: `tauri build` runs WITH `APPLE_API_ISSUER/KEY/KEY_PATH`
  again (D4) — Tauri signs, notarizes and staples the `.app`, then packs
  `bundle/macos/Athenaeum.app.tar.gz` + `.sig`. The DMG loop below it stays
  (the DMG is still notarized and stapled separately, and
  `verify:macos-gatekeeper` still runs on the published DMG). Artifacts add
  `target/<triple>/release/bundle/macos/*.app.tar.gz{,.sig}`.
- `build:windows`: artifacts add `bundle/nsis/*.exe.sig` and
  `bundle/msi/*.msi.sig`.
- `build:linux`: artifacts add `bundle/appimage/*.AppImage.sig`.

### 3.4 Names — `artifact_names.sh`

A new `updater_artifacts()` inventory, same vocabulary, one line per file:
`product os arch ext variant subdir filename plugin_key`:

| filename | plugin key |
| -------- | ---------- |
| `athenaeum-<v>-macos-arm64.app.tar.gz` | `darwin-aarch64` |
| `athenaeum-<v>-macos-x64.app.tar.gz` | `darwin-x86_64` |
| `athenaeum-<v>-windows-x64-setup.exe` (already in `all_release_artifacts`) | `windows-x86_64` (or `windows-x86_64-nsis`, §3.8) |
| `athenaeum-<v>-windows-x64.msi` (already there) | `windows-x86_64-msi` (only if §3.8 confirms) |
| `athenaeum-<v>-linux-x64.AppImage` (already there) | `linux-x86_64` |

A signature file is `<filename>.sig`, always beside its file in
`builds/<tag>/<subdir>/`. No version-less aliases: the manifest names
frozen, tag-scoped paths only. The download page does not list updater
artifacts — they are not for people.

### 3.5 `deploy`

Copies the `.app.tar.gz` files and all `.sig` files into
`release_stage/<subdir>/` under the names above; the existing rsync
publishes them with everything else.

### 3.6 `publish:updater-manifest` (stage `publish`, linux runner)

`needs`: the three build jobs (artifacts) and `deploy` (no artifacts).
Script `.gitlab/ci/scripts/gen_updater_manifest.sh` (bash + python, the
`gen_release_post.sh` shape):

- Inputs: `VERSION`, `CI_COMMIT_TAG`, `RELEASE_NOTES.md`, the `.sig` files
  from the build artifacts, `BUILDS_BASE_URL`.
- Output: `{ "version": "<tag without v>", "notes": "<the whole
  RELEASE_NOTES.md>", "pub_date": "<job time, RFC 3339 UTC>", "platforms":
  { "<plugin key>": { "url": "<BUILDS_BASE_URL>/<tag>/<subdir>/<filename>",
  "signature": "<content of the .sig file, trimmed>" } } }`.
- **A missing or empty `.sig` for any inventory row is `exit 1`.** The
  generator never omits a platform: every build job is required, so a
  missing signature is a pipeline defect, and a release the updater cannot
  serve on one OS is not a release.
- Uploaded as the frozen copy `/var/www/artfrom.space/updates/<tag>.json`.
  The channel file is NOT touched here.

Tests in `.gitlab/ci/scripts/test/test_gen_updater_manifest.sh`, wired into
`run_tests.sh`: happy path (all keys present, URLs equal the inventory);
missing `.sig` → exit 1 and names the row; beta tag → `version` keeps the
dotted `-beta.N`; notes with quotes, backslashes, newlines and non-ASCII
round-trip through `json.loads`; `pub_date` parses as RFC 3339.

### 3.7 `verify:release` additions

After the installer loop: fetch `updates/<tag>.json`; `version` equals the
tag without `v`; for every platform entry, HEAD the URL (200, non-zero
length) and `curl` the file and its `.sig`, then verify the signature from
the manifest against the downloaded bytes with the committed public key
(`rsign2`, installed on the linux runner with `cargo install rsign2` — the
runner has cargo). A signature the manifest carries must equal the `.sig`
file beside the artifact, and both must verify. Any failure is a red verify
stage: the channel never moves (§3.8) and nothing is announced.

### 3.8 `announce`

- New `publish:updater-channel` (needs `verify:release` +
  `verify:macos-gatekeeper`): copies the frozen `updates/<tag>.json` to
  `updates/latest.json` (stable tag) or `updates/latest-beta.json`
  (`-beta` tag) on the host. Idempotent; safe to retry (it re-copies a
  frozen file — unlike build jobs, which must never be retried on a
  finished pipeline).
- `publish_version` (`version.json` / `version-beta.json`) **stays until
  v0.7.0**: installs ≤ 0.6.4 read only those files and would otherwise
  never learn that a version with the updater exists. It goes with the
  legacy filename aliases.

### 3.9 Windows installer question — settled by the plan's first task

The plugin knows the installed bundle type at runtime
(`tauri::utils::platform::bundle_type()`). Task 1 of the plan reads the
plugin source at the pinned version and answers: does the static-manifest
lookup try `windows-x86_64-<installer>` before `windows-x86_64`?

- Yes → the manifest carries `windows-x86_64-nsis` AND `windows-x86_64-msi`
  (both installers are already built and published); MSI installs update
  through the MSI, NSIS through the exe, no Add/Remove duplication.
- No → the manifest carries `windows-x86_64` → the NSIS exe only.
  `platform_supported` stays `true` for an MSI install (the plugin cannot
  tell us), so the plan adds a Windows-only early refusal in
  `install_update`: `bundle_type() == Msi` → "installed from the MSI —
  download the new installer from the site" (§6.2). MSI stays on the
  download page as a "for deployment tools" link.

Either way `installMode: passive` (a progress window, no questions), and
the NSIS installer is per-user (Tauri default), so no UAC prompt.

## 4. Versions and channels

Three spellings of one version, and where each lives:

| Where | Form | Example |
| ----- | ---- | ------- |
| `Cargo.toml` × 4, `package.json` → `CARGO_PKG_VERSION` in core/web | dotted | `0.6.5-beta.1` |
| `tauri.conf.json` → the plugin's `current_version` | numeric prerelease (the bundler rejects the dotted form) | `0.6.5-1` |
| git tag, manifest `version`, `builds/<tag>/` | dotted | `v0.6.5-beta.1` / `0.6.5-beta.1` |

- Core compares dotted against dotted: **no normalization** in
  `core::updates::check`.
- The plugin compares its `X.Y.Z-N` against the manifest's dotted form; by
  SemVer a numeric prerelease ranks BELOW an alphanumeric one, so without
  help `0.6.5-1` vs `0.6.5-beta.1` reads as "update available" for the
  build that is running. `install_update` therefore passes the comparator
  of §2.3, the only place `normalize_tauri_version` is applied.
- A parity test in `athenaeum-tauri` feeds one table of `(current tauri
  form, manifest version)` pairs through the closure and through
  `core::updates::version::is_newer(manifest, dotted current)` and requires
  equal answers.
- `check_versions.sh` (gate) already asserts the six files; it gains the
  assertion that `tauri.conf.json`'s form equals the `bump.sh` transform of
  the tag (`-beta.N` → `-N`, nothing else changed).

Channels: `latest.json` always; `latest-beta.json` when
`updates.check_beta`. Consequences under SemVer precedence, all intended:

- A beta user on `0.6.5-beta.2` is offered stable `0.6.5` (release >
  prerelease).
- A stable user never sees a beta.
- A user who turns `check_beta` off stays on their beta until the next
  stable overtakes it.
- Equal or older manifest version → "up to date"; a downgrade is never
  offered.

Manifests carry the version without a leading `v` (the plugin accepts both;
the generator emits none).

"What's new" uses the embedded notes only (§2.2): `updates.last_seen_version`
holds the dotted form.

Development builds: `install_update` refuses under `cfg(debug_assertions)`
with `updates are disabled in development builds` — the updater must never
replace a dev checkout's bundle. Check and notes work in dev.

## 5. UI

### 5.1 `UpdateDialog`

One modal, `src/components/updates/UpdateDialog.tsx`, design tokens
throughout, body scrolls inside the dialog, rendered at app root (like
`NotificationPanel`).

Header:

- *available*: **Athenaeum v0.6.5 is available** / `You have v0.6.4 ·
  released 2026-09-20` (from `pub_date`, `formatTimestamp`).
- *whatsNew*: **What's new in v0.6.5**.

Body: the notes Markdown through `react-markdown` + `remark-gfm`; every
link opens through `openUrl` (opener), never as webview navigation; a
trailing **Read the full post** link → `blog_url`.

Footer, *available*:

| Host / platform | Buttons |
| --------------- | ------- |
| Desktop, `platform_supported` | **Later** · **Download and install** → progress bar `42.1 MB / 118.4 MB · 36 %` → macOS/Linux: **Restart now** · **Later**; Windows: the app exits when the installer starts (plugin behaviour), text says so beforehand |
| Desktop, not supported (`.deb`) | **Open download page** |
| Web | `docker pull vsharifov/athenaeum:0.6.5` in a code line with **Copy** · **Open download page** |

Footer, *whatsNew*: **Close**.

No download cancel in v1 (the plugin offers none): **Later** is available
until the download starts; a started download runs to completion or
failure.

### 5.2 Work-in-progress guard

Before **Restart now** — and on Windows before **Download and install**,
since installing exits the app — the dialog reads what the sidebar already
reads: `get_compute_queue` (stacking runs, master builds, calibrations,
analysis) and the active transfers from `TransfersContext`. If anything is
running it shows a `text-warning` line — *A stacking run is in progress —
restarting now will cancel it* — and leaves the button enabled. Soft guard:
the user decides, the app never blocks. No new backend.

### 5.3 Entry points and notifications

1. Launch (`useAutoUpdateCheck`, both hosts, unless `updates.auto_check` is
   off): `get_whats_new` → dialog when `Some`; then `check_for_updates` →
   toast + bell entry, `kind: 'update'`, `link: '/about?update'`,
   `dedupeKey: 'update-<version>'`. Failures of the launch check are
   `console.error` only (offline at launch is normal).
2. About page: **Check for updates** (result inline, errors shown) and
   **View release notes** (`get_release_notes` → *whatsNew* for the running
   version — embedded notes, works offline, repeatable).
3. `/about?update` opens the *available* dialog on mount.
4. On `update-ready`: toast *v0.6.5 downloaded — restart to apply*, same
   `dedupeKey`.

### 5.4 Settings

`updates.auto_check` and `updates.check_beta` in `Settings.tsx` are
unchanged except for the auto-check description, which no longer says
"desktop". No new settings.

## 6. Errors and safety

### 6.1 Never swallow

Every failure logs at `error!` with fields and reaches the user as text,
except the launch check (log only, as today). Manual check → text in About.
Install failure → the dialog's *failed* phase with the message, a
notification with `hasErrors`, and **Open download page** always present as
the fallback.

New canonical log fields, added to the logging spec's dictionary in the
same change: `channel`, `current_version`, `latest_version`, `bytes_total`.

### 6.2 Early refusals in `install_update` (before any download)

| Condition | Message |
| --------- | ------- |
| `cfg(debug_assertions)` | `updates are disabled in development builds` |
| another install in flight | `an update is already being installed` |
| macOS: bundle path not writable, or the executable lives under an `AppTranslocation` path or a mounted DMG (`/Volumes/…` that is not the boot volume) | `Athenaeum can't replace itself at <path> — move it to Applications first` |
| Linux: `APPIMAGE` unset or the file not writable | `Athenaeum can't replace itself at <path> — …` |
| Windows, only if §3.8 answers "no": `bundle_type() == Msi` | `installed from the MSI — download the new installer from the site` |

Each refusal is `warn!`-logged with `path` and returned as the command's
`Err`.

### 6.3 Failures mid-way

- Signature mismatch: the plugin refuses BEFORE installing anything. Dialog:
  *The update package failed verification — nothing was changed.* `error!`
  with `url`.
- Network drop / HTTP error during download: dialog *failed* phase with the
  error and a **Retry** button (a fresh `install_update`; the plugin has no
  resume).
- Windows: if the user closes the NSIS window, the app is already gone; the
  next launch starts the old version, nothing is half-replaced (NSIS
  replaces files only after extraction succeeds).
- macOS/Linux: the plugin replaces the bundle/AppImage atomically enough for
  our purpose (extract to a temp dir, then move); a failure leaves the old
  copy running until restart.

### 6.4 A bad channel file

Both the plugin and core validate the whole document; a malformed
`latest.json` does not brick installs, it blinds them: every check fails
with a logged error until the file is replaced. Three layers keep it from
happening: the generator's tests (§3.6), `verify:release`'s URL + signature
check on the frozen file (§3.7), and the channel moving only after verify
(§3.8). Repair = re-run `publish:updater-channel`, or re-tag.

### 6.5 Rollback

None in v1. `builds/<tag>/` is permanent, so a manual downgrade from the
site is always possible; the dialog's **Open download page** is the pointer.

### 6.6 Telemetry and privacy

The manifest GET carries the same query the `version.json` GET carries
today (`v`, `os`, `arch`, `commit`, `id`); nothing new is sent. The
astronet log parser switches its path pattern from `version*.json` to
`updates/latest*.json` the day the first updater release ships; until
then it sees both, and `publish_version` keeps the legacy feed alive
(§3.8). Docker installs now appear in the dashboard (D2), opt-out via
`updates.auto_check`. The adoption caveats in the project memory still
hold.

## 7. Testing and acceptance

### 7.1 Automated

- `core::updates::version`: the moved `version_is_newer` table plus
  `normalize_tauri_version` cases (`0.6.5-1` → `0.6.5-beta.1`; `0.6.5`,
  `0.6.5-beta.1`, `0.6.5-rc.1`, `garbage` unchanged).
- `core::updates::manifest`: static format parses; leading `v` accepted;
  entry without `signature` → error; `platform_supported` false when the
  host key is absent; the telemetry URL is built with every parameter.
- `core::updates::whats_new` on a temp DB: fresh install → `None` and the
  key written; version changed → `Some` once, then `None`.
- `athenaeum-web`: one integration test serving a manifest from a local
  axum listener and asserting `check` end to end (the fetch path); route
  tests for the response shape and the two `501`s.
- `athenaeum-tauri`: the comparator parity test (§4).
- CI scripts: `test_gen_updater_manifest.sh` (§3.6) and the
  `verify_release.sh` additions, in `run_tests.sh`.
- Frontend: `tsc` + `npm run build:web` as the gate (no unit runner
  exists); the dialog is the owner's click-through.

### 7.2 Local rehearsal — the plan's real gate, before any tag

On the dev Mac: a release build signed with a THROWAWAY key, its
`.app.tar.gz` + `.sig` and a hand-assembled manifest served by
`python -m http.server`; a test build whose endpoint points at
`http://localhost` (`dangerousInsecureTransportProtocol` in that build only
— never committed). Walk: launch → toast → dialog → download with progress
→ bundle replaced → restart → *What's new*. Then the web host locally
(`cargo run -p athenaeum-web` against the same server) → *available* with
the docker line. Both must pass before the pipeline changes are exercised.

### 7.3 Acceptance on real releases — two tags

The first build carrying the updater cannot itself be received through the
updater, so acceptance takes two:

1. `v0.6.5-beta.1` — installed BY HAND on macOS arm64, macOS x64, Windows,
   Linux AppImage and Docker. Checks: pipeline green including the new
   verify (signatures verified); `updates/v0.6.5-beta.1.json` and
   `latest-beta.json` in place; the adoption dashboard shows the new pings
   (astronet parser switched).
2. `v0.6.5-beta.2` — any minimal change. On every OS: dialog → install →
   restart → *What's new in v0.6.5-beta.2*; `spctl --assess --type execute`
   passes on the UPDATED macOS bundle; the Docker container still on beta.1
   shows *available* with `docker pull vsharifov/athenaeum:0.6.5-beta.2`.

Written up as `docs/superpowers/research/<date>-updater-acceptance-run.md`.

## 8. Out of scope

- Perseus (headless; its own item — a `perseus update` subcommand or an apt
  repository).
- Linux `.deb` installs (the package manager owns them; they get the
  notification and the download page).
- Background/automatic download (D3), download cancel/resume, rollback,
  delta updates, key rotation.
- A web-side telemetry `id` beyond the app-DB UUID already in use.

## 9. Documentation

- `.claude/skills/release/SKILL.md`: the key custody rule, the local-build
  environment, the new jobs and what a red `verify:release` on the
  manifest means.
- `CLAUDE.md`: an **In-app updates** section (module, commands, the three
  version forms, the two channel files, F5.1 reversed).
- Docs site: the download page gets a line "Athenaeum updates itself from
  v0.6.5 on"; nothing else.

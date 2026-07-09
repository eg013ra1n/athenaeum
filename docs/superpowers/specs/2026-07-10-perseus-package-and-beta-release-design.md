# Perseus Installable Package + v0.4.0-beta.1 Release — Design

Date: 2026-07-10
Status: approved by owner (brainstorming session 2026-07-09/10)
Branch: `0.4.0`

## 1. Overview & Goals

Two coupled deliverables that unblock continued field testing of the Stage I/1.5
sync work:

1. **Perseus productization** — turn the headless CLI capture agent into a
   normally installable desktop package: system-tray mode with live status,
   account sign-in (email → OTP via the hub) from the embedded web page,
   platform installers with autostart, sane installed-app config/data paths.
2. **v0.4.0-beta.1 release** — production builds of every app (Athenaeum
   desktop ×3 platforms, web/Docker, Perseus ×6 artifacts) published on
   artfrom.space through the existing beta channel, with English release notes
   and download-page coverage.

Versioning decision: **`0.4.0-beta.1`**, not `0.4.0-prealpha1`. The CI channel
switch keys exclusively on the `-beta` suffix; any other suffix routes to the
stable path (`version.json`, `builds/latest`, docker `:latest`) and would
advertise the build to all stable users. The beta channel needs zero CI
changes.

## 2. Non-Goals

- No changes to sync engine semantics, retention policy, or pairing protocol.
- No hub-side changes (OTP issuance, device registration, relay map are used
  as deployed).
- No token-templating/branding work on the web page beyond the Account section.
- Windows code signing (no cert; Athenaeum Windows builds are unsigned too).
- This release does NOT replace the pending owner gates (Stage 1.5 live
  verification runbook, A9 observatory soak). Beta ships alongside them.

## 3. Perseus Runtime: Tray Mode & Supervisor

### 3.1 CLI surface

- New subcommand **`perseus tray`**. Launching with **no arguments** defaults
  to `tray` when the binary is compiled with the `tray` feature (covers
  double-click and autostart); headless builds keep clap's usage/help as the
  no-args behavior. Existing `login` / `run` / `status` / `enqueue-backlog`
  remain.
- `run` and `tray` share the same **supervisor**; `run` is the supervisor
  without the tray UI. A fully configured `run` behaves exactly as today.

### 3.2 Supervisor state machine

Today the web server spawns inside `Agent::start` and the agent requires a
resolved pairing. This inverts for installed use: **tray + web server always
start; the sync engine (watcher + engine + transport) starts only when ready.**

Engine-ready condition: (account signed in **or** dev `pairing_ticket` set)
**and** ≥ 1 capture dir configured.

States (single source of truth, shared by tray, `/api/status`, and logs):

| State | Meaning | Tray icon | Menu status line |
| ----- | ------- | --------- | ---------------- |
| `NeedsSetup` | no account or no capture dirs | gray | "Not signed in" / "No capture folders" |
| `Idle` | engine up, queue empty | normal | "Watching N folders" |
| `Syncing` | packages in flight | activity variant | "Syncing K package(s)" |
| `Error` | pairing failed, bind conflict, disk | red-dot variant | short error text |

The supervisor watches for readiness changes (login/logout via web, capture-dir
edits) and brings the engine up/down **in-process, without a restart**. The
web capture-dirs editor's "restart to apply" banner becomes "applies live" in
supervisor mode (the restart-to-apply path remains for plain `run` pre-1.5
configs only if in-process rewire proves risky at plan time — decide during
implementation, default is live apply).

### 3.3 Tray implementation

- Crates: `tray-icon` + `tao` (Tauri's own, **no webview**). OS event loop on
  the main thread (macOS requirement); the tokio runtime hosting the
  agent/web/supervisor runs beside it.
- Menu: status line (disabled item) → **Open Web UI** → **Start at login**
  (checkbox) → **Quit**.
- "Open Web UI" opens the system browser at the configured `web_bind`
  (loopback host substituted when bound to `0.0.0.0`).
- Status updates flow tray-ward over a `watch` channel from the supervisor
  (poll fallback every ~2 s is acceptable).
- Everything trays sits behind a cargo feature **`tray`** (default off).
  Desktop packages build with `--features tray`; the headless arm64 build
  compiles without it and pulls zero GTK/GUI deps.
- Icons: one base glyph, four state variants; macOS uses a monochrome template
  icon (menu-bar convention), Windows/Linux use the colored variants.
- Zero-print rule holds: tray mode logs via `tracing` only. The interactive
  CLI prompts in `account.rs` keep their documented exemption.

### 3.4 Installed-app config & data paths

- `--config` default stays `./perseus.toml` for compatibility; **if absent in
  cwd**, fall back to the platform config path:
  - macOS: `~/Library/Application Support/Perseus/perseus.toml`
  - Windows: `%APPDATA%\Perseus\perseus.toml`
  - Linux: `~/.config/perseus/perseus.toml` (XDG)
- Default `data_dir` moves to the matching platform data dir when the config
  is auto-created.
- **First run:** missing config is created with defaults and empty
  `capture_dirs`. Validation is relaxed: an empty list is a legal
  `NeedsSetup` state (engine simply does not start). Legacy exactly-one
  validation applies only when keys are present.
- First-run story: install → gray tray icon → Open Web UI → sign in
  (email + OTP) → add capture folders in the existing editor → supervisor
  starts the engine → icon goes normal.

## 4. Web Account Sign-In (email → OTP)

### 4.1 account.rs refactor

Split into a non-interactive core + wrappers:

- `request_code(hub_url, email)` — asks the hub to email an OTP.
- `verify_and_register(config, email, code)` — already exists (private);
  becomes the shared core: stores the device token (0600 file in `data_dir`,
  never in TOML), registers the node as a capture device, resolves pairing.
- CLI `perseus login` becomes a thin interactive wrapper over the same core.
- `PairingCache` and token storage unchanged.

### 4.2 New endpoints (same style/gate as existing `/api/*`)

| Endpoint | Behavior |
| -------- | -------- |
| `GET /api/account` | `signed_out`, or `signed_in` + email, device name, paired primary name, hub_url |
| `POST /api/account/request-code` `{email}` | hub OTP issuance; passes hub errors through honestly |
| `POST /api/account/verify` `{email, code}` | verify → store token → register device → resolve pairing → wake supervisor |
| `POST /api/account/logout` | delete token file, clear pairing cache, supervisor stops engine → `NeedsSetup`; idempotent |

### 4.3 UI

`index.html` gains an **Account** section at the top: signed-out shows a
two-step form (email → code, with re-send); signed-in shows who is signed in,
the paired primary, and Sign out. The rest of the page is untouched.

### 4.4 Security & errors

- No new auth surface: existing rule applies — loopback (default) needs no
  token; non-loopback binds require `web_token` on all `/api/*`, **including
  the account endpoints** (prevents a LAN stranger from re-pairing the node to
  their account). `GET /` stays auth-exempt.
- OTP rate limiting is the hub's job; not duplicated locally.
- Hub down / wrong code / rejected registration → honest page messages +
  `tracing::error!`; never swallowed.

## 5. Packaging Matrix

| Platform | Artifact | Contents |
| -------- | -------- | -------- |
| macOS arm64 (M-series) | `perseus-<ver>-macos-arm64.dmg` | `Perseus.app`, menu-bar-only (`LSUIElement=true`), bundle id `com.vsharifov.perseus`, codesigned + notarized (same cert/runner as Athenaeum) |
| macOS x86_64 (Intel) | `perseus-<ver>-macos-x64.dmg` | same, x64 |
| Windows x64 | `perseus-<ver>-windows-x64-setup.exe` | NSIS: Program Files, Start Menu shortcut, autostart checkbox (HKCU `Run`), launches tray post-install |
| Linux x86_64 | `perseus_<ver>_amd64.deb` | cargo-deb, desktop variant **with tray**: `/usr/bin/perseus`, `.desktop` file |
| Linux aarch64 | `perseus_<ver>_arm64.deb` | **headless variant, no `tray` feature** (zero GTK deps) for RPi: binary + sample systemd unit in `/usr/share/doc/perseus/` |
| Linux both arches | `perseus-<ver>-linux-{amd64,arm64}.tar.gz` | bare binary for non-Debian distros / headless x64 |

Two macOS DMGs (not universal) mirror the Athenaeum `build:macos` pattern and
reuse its signing flow unchanged.

### 5.1 Autostart ("Start at login" tray menu item, per-user, no admin)

| Platform | Mechanism |
| -------- | --------- |
| macOS | LaunchAgent plist in `~/Library/LaunchAgents/com.vsharifov.perseus.plist` |
| Windows | HKCU `Software\Microsoft\Windows\CurrentVersion\Run` (plus installer checkbox) |
| Linux | `~/.config/autostart/perseus.desktop` |

## 6. CI & Publishing

Four new jobs in the existing build stage (tags only, like the others):

- `build:perseus:macos` — `cargo build --release --features tray` per target,
  `.app` assembly script, codesign + notarytool (reuse the keychain
  before_script pattern; hand-codesign, so the `unset APPLE_CERTIFICATE` Tauri
  workaround is not needed here — verify at plan time).
- `build:perseus:windows` — cargo + makensis (already present on the runner).
- `build:perseus:linux` — cargo-deb amd64 on the linux runner (+ tar.gz).
- `build:perseus:linux-arm64` — **native arm64 build in Docker via OrbStack on
  the mac runner** (fast; ring/iroh under qemu on the x86 runner is the slow
  fallback), headless .deb + tar.gz.

Publishing piggybacks on the existing deploy/release jobs: Perseus artifacts
are added to `builds/<tag>/` uploads + stable-named aliases and to the GitLab
Release asset links. The `-beta` tag automatically yields `builds/beta`,
`version-beta.json` (beta-channel update notifications only), and docker
`:beta`. No channel logic changes.

## 7. Release v0.4.0-beta.1

- **Version bump ×6**: `package.json`, `crates/{athenaeum-core,athenaeum-tauri,
  athenaeum-web,perseus}/Cargo.toml`, `crates/athenaeum-tauri/tauri.conf.json`
  (as `0.4.0-1` — Tauri bundle naming rejects dotted prerelease). Refresh
  `Cargo.lock` via `cargo check`.
- Work stays on branch `0.4.0`; tag `v0.4.0-beta.1` on that branch. **ff-merge
  to `main` only at stable 0.4.0** (owner's version-branch rule). The tag must
  match the Protected-tag pattern or the mac job fails fast on signing vars.
- **`RELEASE_NOTES.md`** fully rewritten, English: What's New — Personal sync
  (Stage I + 1.5), Perseus capture agent debut (tray, web UI, OTP sign-in,
  installers ×4 platforms); Changes / Bug Fixes from `v0.3.0..HEAD`.
- **Docs site (artfrom-space repo):** Version History row on `download.md` +
  new **Perseus section** (6 artifacts, short per-platform install notes incl.
  headless RPi/systemd). **The landing page stays on stable** (owner rule
  2026-07-10): betas are mentioned only in passing on the download page. Add a
  prerelease filter to `scripts/sync-landing-version.mjs` so a beta blog post
  can never flip the hero button; publish a beta blog post only if that filter
  is in place.
- Iteration on CI packaging failures: re-tagging (established mechanism).

## 8. Testing

- **Unit/integration (cargo):**
  - Account endpoints against a wiremock hub (pattern exists in `account.rs`
    tests): request-code, verify success/failure, logout idempotence.
  - Supervisor transitions: empty `capture_dirs` legal; engine starts after
    simulated login without process restart; logout stops engine.
  - Platform-default config path resolution + first-run config creation.
  - Existing `web.rs` tower-oneshot handler tests and perseus tests unchanged.
- **Manual smoke (runbook in `scripts/`):** per platform — install → gray
  icon → Open Web UI → sign in → add dirs → icon normal → sync a real FITS →
  Quit. One pass each on macOS/Windows/Linux desktop + RPi headless.
- **Gates before tag:** `cargo build --workspace`, `cargo test -p perseus` +
  core tests, `npx tsc --noEmit`.

## 9. Risks & Open Items

- `tray-icon`/`tao` on Linux needs appindicator/GTK at runtime — declared as
  .deb dependencies of the desktop variant only.
- In-process engine rewire on capture-dir edit: default is live apply; if the
  watcher/engine teardown proves brittle mid-implementation, fall back to the
  existing restart-to-apply banner for that one path (login/logout stays
  live — it is the core UX of this feature).
- `sync-landing-version.mjs` prerelease behavior must be verified before any
  beta blog post exists (§7).
- macOS `.app` assembly for a non-Tauri binary is new scripting (Info.plist,
  iconset, dmg); notarization of a bare Rust binary bundle occasionally
  surfaces hardened-runtime entitlement needs — budget for one re-tag cycle.

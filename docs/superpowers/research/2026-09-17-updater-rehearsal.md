# In-app updater — local rehearsal (Task 11)

Date: 2026-09-17. Machine: this dev Mac (Apple Silicon, `aarch64-apple-darwin`).
Branch: `feature/in-app-updates` at `c69c03b3`. Everything below runs from
`/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum`.
`$SCRATCH` = `/private/tmp/claude-501/-Volumes-BigMac-Users-astrobureau-Documents-Projects-athenaeum/8c4a0323-14b8-4991-a14b-35a9f41144f8/scratchpad`.

This is the plan's real gate before any release tag touches the updater (spec
§7.2): two real builds, a real minisign-signed manifest built by the actual
generator script, a real `rsign2` verification (including a tamper check),
`verify_release.sh`'s manifest section run for real against a local server,
and the desktop + web hosts both checking a real HTTP endpoint and logging
the result. The GUI click-through (toast → About → Download and install →
Restart → What's new) is the owner's — this agent has no display access.

## tl;dr

- **Blocker found and worked around by omitting one build flag, not by
  editing code**: real Developer ID codesigning is impossible from this
  agent's shell — `codesign` fails immediately with `errSecInternalComponent`
  / `-60008 (errAuthorizationInternal)` even signing a throwaway text file,
  because the private key's authorization requires an interactive
  SecurityAgent prompt this session cannot receive. Both `.app` bundles were
  built **unsigned** (ad-hoc, linker-signed only — Tauri's normal fallback
  when `APPLE_SIGNING_IDENTITY` isn't set) instead. This matches the
  project's own documented constraint (`project_macos_signing.md`: "runner
  must be a GUI-session LaunchAgent") — it is an environment limitation, not
  a code defect, and it doesn't touch the actual release pipeline (which
  already ran green on a properly configured LaunchAgent runner).
- **DMG bundling also failed** in this same session for what looks like the
  same class of reason (Automation/Finder scripting needs GUI access too);
  worked around by building `--bundles app` only, which is all the rehearsal
  needs (no DMG is used — the app is copied straight to `/Applications`).
- Everything else — the dev-key signing, the manifest generator, `rsign2`
  verify + tamper-fail, `verify_release.sh` section 1b against a live local
  server, the desktop app's real update check (logged), and the web host's
  `check_for_updates` / `install_update` JSON — worked exactly as specified,
  first try, no code changes.

## Signing key and endpoint override

Dev key from Task 4, confirmed to be the one already baked into
`crates/athenaeum-tauri/tauri.conf.json`'s `plugins.updater.pubkey`:

```
$ cat ~/.tauri/athenaeum-dev.key.pub
untrusted comment: minisign public key: B9AD1928F1205024
RWQkUCDxKBmtuZ65e8xiPsRgQS1wHH9Nsyb48vTM6/Q2jlQEngkw5EPi
```

Base64 of that exact string equals `tauri.conf.json`'s `pubkey` field
byte-for-byte — confirmed by decoding both and diffing.

```bash
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/athenaeum-dev.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
```

No rejection from the bundler for the empty password (the key was generated
`--ci`, as noted in the brief) — both builds signed the updater artifact with
it without complaint.

`ATHENAEUM_UPDATES_BASE_URL=http://127.0.0.1:8765` was exported for every
cargo invocation (both tauri builds, the `npm run build:web` + `cargo run -p
athenaeum-web`). Cargo recompiled `athenaeum-core` each time the value's
presence changed across branches, as expected from `option_env!` tracking.

## The codesigning blocker (full evidence)

Attempted per controller instruction #2:

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: VLEN SHARIFOV (3WUWPBHUMK)"
npm run tauri build -- --target aarch64-apple-darwin --config '{...}'
```

Compiled clean (5m15s), then failed at the bundling stage:

```
Signing with identity "Developer ID Application: VLEN SHARIFOV (3WUWPBHUMK)"
Signing /…/target/aarch64-apple-darwin/release/bundle/macos/Athenaeum.app/Contents/MacOS/athenaeum-tauri
/…/athenaeum-tauri: replacing existing signature
/…/athenaeum-tauri: errSecInternalComponent
failed to bundle project: failed codesign application: failed to run command codesign: failed to sign app
```

Reproduced directly, outside Tauri, on a throwaway file:

```
$ codesign --sign "Developer ID Application: VLEN SHARIFOV (3WUWPBHUMK)" --verbose=4 /tmp/codesign_test2.txt
Developer ID Application: VLEN SHARIFOV (3WUWPBHUMK): found in both …athenaeum-signing-1575.keychain-db and …login.keychain-db (this is all right)
/tmp/codesign_test2.txt: errSecInternalComponent
exit=1
```

`security find-identity -v -p codesigning` found the identity fine (10
listings across the several keychains in the search list — login keychain
plus leftover `athenaeum-signing-*.keychain-db` temp keychains from earlier
CI signing runs). The keychain itself is unlocked (`security show-keychain-info`
reports `no-timeout`, and the unified log shows `KCdb … unlocking for
makeUnlocked()` succeeding). The failure is specifically about *using the
private key*, captured in the unified log at the exact moment of the failed
attempt:

```
securityd[150] […] new SecurityAgentConnection(0x16f89dec0)
securityd[150] […] new SecurityAgentXPCQuery(0x16f89dec0)
securityd[150] […] Retrieved UID 501 for this session
securityd[150] […] Creating a standard security agent
securityd[150] […] activating connection … name=com.apple.security.agent
securityd[150] […] SecurityAgentXPCQuery(0x16f89dec0) dying                    <- ~115ms later
securityd[150] […] SecurityAgentConnection(0x16f89dec0) dying
codesign[67463] […] CSSM Exception: -60008 Unable to obtain authorization for this operation.
codesign[67463] [com.apple.security:seckey] SecKeyCreateSignature failed: Error Domain=NSOSStatusErrorDomain Code=-60008
  "CSSM Exception: -60008 Unable to obtain authorization for this operation." (errAuthorizationInternal: something else went wrong)
```

securityd tries to stand up an interactive SecurityAgent to ask permission to
use the key, and that agent dies within ~115ms without ever presenting
anything — there is no window-server-backed interactive session available to
this agent's shell to answer an authorization prompt. This is exactly the
class of constraint already on record in
`project_macos_signing.md`: *"runner must be a GUI-session LaunchAgent"*. The
existing release pipeline signs successfully because it runs under that
specific LaunchAgent setup; this ad hoc agent shell does not. **This is an
environment limitation, not a code defect** — no code or CI file was touched
to work around it.

**Decision**: rather than block the entire rehearsal on a facility this
agent fundamentally cannot exercise (no display, no way to grant an
interactive keychain prompt, and altering the real signing key's ACL/partition
list would need the keychain password, which this agent does not have and
should not try to guess), both builds were made **without**
`APPLE_SIGNING_IDENTITY` — Tauri's normal behavior in that case is to leave
the `.app` ad-hoc/linker-signed only (no `codesign --sign <identity>` step
at all), which is sufficient to launch locally (no quarantine bit on a
locally-built, locally-copied app) and fully exercises the updater mechanism
that is the actual point of Task 11.

```
$ codesign -dv target/aarch64-apple-darwin/release/bundle/macos/Athenaeum.app
CodeDirectory v=20400 size=527432 flags=0x20002(adhoc,linker-signed) hashes=16478+0 location=embedded
Signature=adhoc
TeamIdentifier=not set
```

## The DMG bundling failure (same session, likely same root cause)

With no signing identity, the `.app` bundle succeeded, but `--bundles all`
(the default) then failed at `bundle_dmg.sh`:

```
Bundling Athenaeum.app (…/bundle/macos/Athenaeum.app)
Bundling Athenaeum_0.6.4-1_aarch64.dmg (…/bundle/dmg/Athenaeum_0.6.4-1_aarch64.dmg)
 Running bundle_dmg.sh
failed to bundle project: error running bundle_dmg.sh: `failed to run …/bundle/dmg/bundle_dmg.sh`
```

Tauri does not forward the script's own stderr on this failure path (only
the generic message above). `bundle_dmg.sh` mounts the writable DMG and
normally drives Finder via AppleScript/System Events to lay out the icon —
another operation that needs a real GUI/Automation-authorized session. A
96 MB `rw.68337.Athenaeum_0.6.4-1_aarch64.dmg` shadow file was left behind
under `target/aarch64-apple-darwin/release/bundle/dmg/` (the writable-stage
DMG create step did succeed; whatever runs after it — almost certainly the
Finder-scripting step — is what failed). **Not investigated further**: the
rehearsal doesn't need a DMG (the app is copied straight into
`/Applications`), and this is consistent with the same "needs a GUI-session
LaunchAgent" constraint as codesigning, not evidence of a new defect in the
real release pipeline (which builds and notarizes DMGs successfully under
that dedicated LaunchAgent). **Worked around by build flag, not code**: both
builds used `--bundles app`, which produces exactly `macos/Athenaeum.app` +
`macos/Athenaeum.app.tar.gz` + `.sig` and skips the DMG target entirely.

## Build A — current tree, 0.6.4-beta.1

```bash
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/athenaeum-dev.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
export ATHENAEUM_UPDATES_BASE_URL="http://127.0.0.1:8765"
npm run tauri build -- --target aarch64-apple-darwin --bundles app \
  --config '{"plugins":{"updater":{"dangerousInsecureTransportProtocol":true,"endpoints":["http://127.0.0.1:8765/latest.json"]}}}'
```

- First attempt (with `APPLE_SIGNING_IDENTITY` set): compiled clean in
  `5m 15s`, failed at codesign (above).
- Second attempt (identity unset, `--bundles all` default): compiled
  incrementally (`2m 28s`, only `athenaeum-tauri` — core was already built
  with the right `option_env!` value), `.app` bundle produced fine, DMG
  failed (above).
- Third attempt (`--bundles app`): instant (nothing to recompile),
  succeeded:

```
Bundling Athenaeum.app (…/bundle/macos/Athenaeum.app)
Bundling …/bundle/macos/Athenaeum.app.tar.gz (…) (updater)
Finished 1 bundle at: …/bundle/macos/Athenaeum.app , Athenaeum.app.tar.gz (updater)
Finished 1 updater signature at: …/bundle/macos/Athenaeum.app.tar.gz.sig
```

Installed as the rehearsal copy:

```bash
rm -rf /Applications/Athenaeum-rehearsal.app
cp -R target/aarch64-apple-darwin/release/bundle/macos/Athenaeum.app /Applications/Athenaeum-rehearsal.app
```

`/Applications/Athenaeum.app` (the owner's real install) was never touched.

## Build B — scratch bump, 0.6.5-beta.1

```bash
git switch -c scratch/rehearsal
scripts/release/bump.sh 0.6.5-beta.1
```

Output:

```
package.json: 0.6.5-beta.1
crates/athenaeum-core/Cargo.toml: 0.6.5-beta.1
crates/athenaeum-tauri/Cargo.toml: 0.6.5-beta.1
crates/athenaeum-web/Cargo.toml: 0.6.5-beta.1
crates/perseus/Cargo.toml: 0.6.5-beta.1
crates/athenaeum-tauri/tauri.conf.json: 0.6.5-1
ok: all six versions match 0.6.5-beta.1
```

The working tree carried two files modified by another session
(`docs/backlog-v0.5.6.md`, `docs/superpowers/open-items.md`) — staged and
committed **only** the seven files `bump.sh` actually rewrote:

```bash
git add package.json crates/athenaeum-core/Cargo.toml crates/athenaeum-tauri/Cargo.toml \
  crates/athenaeum-web/Cargo.toml crates/perseus/Cargo.toml crates/athenaeum-tauri/tauri.conf.json Cargo.lock
git commit -m "chore(rehearsal): bump to 0.6.5-beta.1 (scratch, never merged)…"
```

Commit `2dac1ffe` on `scratch/rehearsal` (never merged, never pushed — branch
deleted immediately after use).

Built with the same env + `--target aarch64-apple-darwin --bundles app`
(same `--config` override). Full recompile since the version bump touches
every crate's `Cargo.toml`: `athenaeum-tauri` + `athenaeum-core` in `3m 59s`,
then:

```
Bundling Athenaeum.app (…/bundle/macos/Athenaeum.app)
Bundling …/bundle/macos/Athenaeum.app.tar.gz (…) (updater)
Finished 1 bundle at: …/bundle/macos/Athenaeum.app , Athenaeum.app.tar.gz (updater)
Finished 1 updater signature at: …/bundle/macos/Athenaeum.app.tar.gz.sig
```

Artifacts copied out, then cleanup:

```bash
mkdir -p "$SCRATCH/serve" "$SCRATCH/b-app"
cp target/aarch64-apple-darwin/release/bundle/macos/Athenaeum.app.tar.gz "$SCRATCH/serve/b.app.tar.gz"
cp target/aarch64-apple-darwin/release/bundle/macos/Athenaeum.app.tar.gz.sig "$SCRATCH/serve/b.app.tar.gz.sig"
cp -R target/aarch64-apple-darwin/release/bundle/macos/Athenaeum.app "$SCRATCH/b-app/Athenaeum.app"
git switch feature/in-app-updates
git branch -D scratch/rehearsal
```

Confirmed clean return to base:

```
$ git status
On branch feature/in-app-updates
Changes not staged for commit:
        modified:   docs/backlog-v0.5.6.md
        modified:   docs/superpowers/open-items.md
$ git log -1 --oneline
c69c03b3 docs(updates): CLAUDE.md section, release skill, logging dictionary, open items
```

Exactly the two foreign files, exactly the expected HEAD — nothing from the
rehearsal survived on the branch.

## The manifest — built by the real generator

```bash
mkdir -p "$SCRATCH/stage/macos" "$SCRATCH/stage/windows" "$SCRATCH/stage/linux"
cp "$SCRATCH/serve/b.app.tar.gz.sig" "$SCRATCH/stage/macos/athenaeum-0.6.5-beta.1-macos-arm64.app.tar.gz.sig"
# + the 4 other updater_artifacts rows, same .sig content (they are never downloaded — see below)
cp "$SCRATCH/serve/b.app.tar.gz.sig" "$SCRATCH/stage/macos/athenaeum-0.6.5-beta.1-macos-x64.app.tar.gz.sig"
cp "$SCRATCH/serve/b.app.tar.gz.sig" "$SCRATCH/stage/windows/athenaeum-0.6.5-beta.1-windows-x64-setup.exe.sig"
cp "$SCRATCH/serve/b.app.tar.gz.sig" "$SCRATCH/stage/windows/athenaeum-0.6.5-beta.1-windows-x64.msi.sig"
cp "$SCRATCH/serve/b.app.tar.gz.sig" "$SCRATCH/stage/linux/athenaeum-0.6.5-beta.1-linux-x64.AppImage.sig"

CI_COMMIT_TAG=v0.6.5-beta.1 STAGE_DIR="$SCRATCH/stage" NOTES_PATH=RELEASE_NOTES.md \
  BUILDS_BASE_URL=http://127.0.0.1:8765/builds .gitlab/ci/scripts/gen_updater_manifest.sh > "$SCRATCH/serve/latest.json"
```

Ran clean, no `updater_artifacts`/inventory error. Verified:

```
$ python3 -c 'import json; d=json.load(open("latest.json")); print(d["version"], sorted(d["platforms"]))'
0.6.5-beta.1 ['darwin-aarch64', 'darwin-x86_64', 'linux-x86_64', 'windows-x86_64-msi', 'windows-x86_64-nsis']
```

Manifest body (notes field omitted here — it's the real `RELEASE_NOTES.md`
head, ~3 KB, unmodified):

```json
{
  "version": "0.6.5-beta.1",
  "pub_date": "2026-09-17T00:24:26Z",
  "platforms": {
    "darwin-aarch64": {
      "url": "http://127.0.0.1:8765/builds/v0.6.5-beta.1/macos/athenaeum-0.6.5-beta.1-macos-arm64.app.tar.gz",
      "signature": "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRa1VDRHhLQm10dVVoQ2JBMlp5dG1SYTcrKzVzK0YzU3Y2dHRKRGNLTGY0M01tczBLNkNCanp3cXcrMEZFVDhldGV2dmJNK3VlQkxySVNPUWsrdEVoVEorcy93WW9telFZPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzg5NjA0NjQyCWZpbGU6QXRoZW5hZXVtLmFwcC50YXIuZ3oKVDZCSE5RMVpwV0R1WTdKdkJXRHI4djY2bm5YcnU3d0xrZFMxaEgwelc3WFV0TlhadCtYdDYrdUpqZUVZREpzZE50blp5dThPWURkaVpBOUF4YWZSQmc9PQo="
    },
    "darwin-x86_64":       { "url": "…/macos/athenaeum-0.6.5-beta.1-macos-x64.app.tar.gz",             "signature": "<same B sig>" },
    "windows-x86_64-nsis": { "url": "…/windows/athenaeum-0.6.5-beta.1-windows-x64-setup.exe",           "signature": "<same B sig>" },
    "windows-x86_64-msi":  { "url": "…/windows/athenaeum-0.6.5-beta.1-windows-x64.msi",                 "signature": "<same B sig>" },
    "linux-x86_64":        { "url": "…/linux/athenaeum-0.6.5-beta.1-linux-x64.AppImage",                "signature": "<same B sig>" }
  }
}
```

**Note on the four non-macOS rows**: their `.sig` content is B's real
minisign signature over B's real `.app.tar.gz` — it is a valid minisig
signature, but it does not match any file actually served at those URLs
(no Windows/Linux binary was built in this rehearsal). Those four URLs 404
on GET, by design (see the `verify_release.sh` run below) — they exist only
so the generator's "every platform must have a signature or the manifest
build fails" invariant is satisfied, and they are never fetched by anything
that matters here (the desktop app on this Mac only ever asks for
`darwin-aarch64`).

Placed B's real artifact at the path the manifest names for `darwin-aarch64`:

```bash
mkdir -p "$SCRATCH/serve/builds/v0.6.5-beta.1/macos"
cp "$SCRATCH/serve/b.app.tar.gz"     "$SCRATCH/serve/builds/v0.6.5-beta.1/macos/athenaeum-0.6.5-beta.1-macos-arm64.app.tar.gz"
cp "$SCRATCH/serve/b.app.tar.gz.sig" "$SCRATCH/serve/builds/v0.6.5-beta.1/macos/athenaeum-0.6.5-beta.1-macos-arm64.app.tar.gz.sig"
```

## rsign2 — real verify + real tamper failure

```
$ command -v rsign || cargo install rsign2 --locked
   Installed package `rsign2 v0.6.6` (executable `rsign`)
```

Decoded the manifest's pubkey (from `tauri.conf.json`) and the
`darwin-aarch64` signature exactly the way `verify_release.sh` does:

```
$ rsign verify -p "$SCRATCH/pub.key" -x "$SCRATCH/b.minisig" \
    "$SCRATCH/serve/builds/v0.6.5-beta.1/macos/athenaeum-0.6.5-beta.1-macos-arm64.app.tar.gz"
Signature and comment signature verified
Trusted comment: timestamp:1789604642	file:Athenaeum.app.tar.gz
```
Exit code: `0`.

Tampered one byte of a **copy** of that same tar.gz (offset 1000, XOR 0xFF)
and re-ran:

```
$ rsign verify -p "$SCRATCH/pub.key" -x "$SCRATCH/b.minisig" "$SCRATCH/tampered.app.tar.gz"
Signature verification failed
```
Exit code: `1`.

Both outputs verbatim above, both as expected.

## `verify_release.sh` — real run against the local server

Server started first (see "Servers" below). Section 1 (`all_release_artifacts`
— the human-facing installers, DMGs, MSIs, etc.) 404s on everything, as
expected — none of those files were built in this rehearsal. Section 1b
(`updater_artifacts`, the actual thing Task 11 cares about) 404s on the four
platforms with no real file behind them, and passes clean for
`darwin-aarch64`:

```
$ cp "$SCRATCH/serve/latest.json" "$SCRATCH/serve/updates/v0.6.5-beta.1.json"
$ CI_COMMIT_TAG=v0.6.5-beta.1 UPDATES_BASE_URL=http://127.0.0.1:8765/updates \
    BUILDS_BASE_URL=http://127.0.0.1:8765/builds SKIP_DOCKER_CHECK=1 SKIP_BLOG_CHECK=1 \
    .gitlab/ci/scripts/verify_release.sh
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/windows/athenaeum-0.6.5-beta.1-windows-x64.msi
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/windows/athenaeum-0.6.5-beta.1-windows-x64-setup.exe
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/macos/athenaeum-0.6.5-beta.1-macos-arm64.dmg
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/macos/athenaeum-0.6.5-beta.1-macos-x64.dmg
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/linux/athenaeum-0.6.5-beta.1-linux-x64.AppImage
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/linux/athenaeum-0.6.5-beta.1-linux-x64.deb
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/perseus/perseus-0.6.5-beta.1-macos-arm64.dmg
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/perseus/perseus-0.6.5-beta.1-macos-x64.dmg
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/perseus/perseus-0.6.5-beta.1-windows-x64-setup.exe
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/perseus/perseus-0.6.5-beta.1-linux-x64.deb
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/perseus/perseus-0.6.5-beta.1-linux-x64.tar.gz
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/perseus/perseus-0.6.5-beta.1-linux-arm64.deb
ERROR: HTTP 404 for http://127.0.0.1:8765/builds/v0.6.5-beta.1/perseus/perseus-0.6.5-beta.1-linux-arm64.tar.gz
ERROR: HTTP 404 (or empty body) for http://127.0.0.1:8765/builds/v0.6.5-beta.1/macos/athenaeum-0.6.5-beta.1-macos-x64.app.tar.gz
ERROR: HTTP 404 (or empty body) for http://127.0.0.1:8765/builds/v0.6.5-beta.1/windows/athenaeum-0.6.5-beta.1-windows-x64-setup.exe
ERROR: HTTP 404 (or empty body) for http://127.0.0.1:8765/builds/v0.6.5-beta.1/windows/athenaeum-0.6.5-beta.1-windows-x64.msi
ERROR: HTTP 404 (or empty body) for http://127.0.0.1:8765/builds/v0.6.5-beta.1/linux/athenaeum-0.6.5-beta.1-linux-x64.AppImage
skipped: docker check (SKIP_DOCKER_CHECK=1)
skipped: blog check (SKIP_BLOG_CHECK=1)
ERROR: release v0.6.5-beta.1 is not fully published — nothing will be announced
```
Exit code: `1` (expected — most of the inventory genuinely isn't there).

**The line that matters**: no line above mentions `darwin-aarch64` or
`macos-arm64.app.tar.gz` — the two `macos-arm64.dmg` 404s are section 1
(installer) noise for a file this rehearsal never built, not the updater
check. `darwin-aarch64`'s signature download, base64-decode and `rsign
verify` all passed silently inside the script (it only prints on failure)
— confirmed directly by the standalone `rsign verify` run above, which is
byte-for-byte what this script does internally for that one platform.

## Servers

```bash
cd "$SCRATCH/serve"
nohup python3 -m http.server 8765 > "$SCRATCH/serve.log" 2>&1 &   # PID 71585
```

```
$ curl -s http://127.0.0.1:8765/latest.json | head -c 200
{
  "version": "0.6.5-beta.1",
  "notes": "*Athenaeum v0.6.4-beta.1: nothing inside the application changed…
```

## Desktop launch — verified by logs

```bash
open /Applications/Athenaeum-rehearsal.app
```

Log dir (shared with the owner's real install — same bundle id
`com.vsharifov.athenaeum`, same app-data dir and DB; the prod DB has no
`updates.*` settings row, confirmed by `SELECT key,value FROM settings WHERE
key LIKE 'updates%'` returning nothing before launch):

```
~/Library/Application Support/com.vsharifov.athenaeum/logs/athenaeum-desktop.2026-09-17.jsonl
```

The two key lines, verbatim:

```json
{"timestamp":"2026-09-17T00:26:03.729070Z","level":"INFO","fields":{"message":"first launch — release notes not shown","current_version":"0.6.4-beta.1"},"target":"athenaeum_core::updates","span":{"name":"get_whats_new"},"spans":[{"name":"get_whats_new"}]}
{"timestamp":"2026-09-17T00:26:03.740525Z","level":"INFO","fields":{"message":"update check finished","channel":"Stable","current_version":"0.6.4-beta.1","latest_version":"0.6.5-beta.1","is_update_available":true,"platform_supported":true},"target":"athenaeum_core::updates","span":{"name":"check_for_updates"},"spans":[{"name":"check_for_updates"}]}
```

It logged **"first launch — release notes not shown"**, not "version
changed" — this prod DB has never stored `updates.last_seen_version`
before (this is the first time this exact machine has run a build carrying
the What's-new/update-check feature at all).

`serve.log` shows the exact request the brief predicted:

```
::ffff:127.0.0.1 - - [17/Sep/2026 03:26:03] "GET /latest.json?v=0.6.4-beta.1&os=macos&arch=aarch64&commit=c69c03b3&id=f7c48d06-2e6d-4ea2-8a4a-9b7c5d9b5bcb HTTP/1.1" 200 -
```

App quit afterward (`osascript … quit app "Athenaeum-rehearsal"` did not
actually terminate it — not scriptable that way; `pkill -f
Athenaeum-rehearsal` did, confirmed with `pgrep` returning nothing after).

**What was not done, on purpose**: no click on the toast, no opening of
About, no "Download and install", no "Restart now". Task 11 is explicit that
this leg is BUILD/SERVE/VERIFY-BY-LOGS only; the click-through is the
owner's.

### codesign / spctl on the rehearsal copy — actual state (unsigned, not notarized)

```
$ codesign --verify --deep --strict /Applications/Athenaeum-rehearsal.app
/Applications/Athenaeum-rehearsal.app: code has no resources but signature indicates they must be present
(exit 1)

$ spctl --assess --type execute -v /Applications/Athenaeum-rehearsal.app
/Applications/Athenaeum-rehearsal.app: code has no resources but signature indicates they must be present
(exit 1)
```

This is the expected shape for an **ad-hoc-only** build (the compiler
ad-hoc-signs the arm64 executable itself; no `codesign --sign <identity>
--deep` pass ran over the whole bundle because no identity was available —
see the blocker above). Under a build actually signed with the real
Developer ID (as the brief originally anticipated), `codesign --verify
--deep --strict` would pass and `spctl --assess` would say **rejected**
(signed but not notarized — notarization is the release pipeline's job, out
of scope for this rehearsal). Neither check was meaningful to run against an
intentionally-unsigned build beyond confirming it's unsigned, which is
recorded here for completeness.

## Web host

```bash
npm run build:web    # VITE_TARGET=web tsc && vite build — clean, dist/ is gitignored
ATHENAEUM_UPDATES_BASE_URL=http://127.0.0.1:8765 \
  ATHENAEUM_DB_PATH="$SCRATCH/webdb/web.db" ATHENAEUM_PORT=8790 \
  ATHENAEUM_STATIC_DIR="$PWD/dist" \
  nohup cargo run --release -p athenaeum-web > "$SCRATCH/web.log" 2>&1 &   # PID 71901
```

Compiled `athenaeum-web` + `athenaeum-core` fresh (1m50s — first build of
the web binary on this branch/env combination), then:

```json
{"message":"Athenaeum web server started","addr":"0.0.0.0:8790"}
```

```
$ curl -s -X POST localhost:8790/api/check_for_updates -H 'content-type: application/json' -d '{}'
{"currentVersion":"0.6.4-beta.1","latestVersion":"0.6.5-beta.1","isUpdateAvailable":true,
 "channel":"stable","notes":"…","pubDate":"2026-09-17T00:24:26Z","platformSupported":false,
 "downloadPageUrl":"https://artfrom.space/releases/download","blogUrl":"https://artfrom.space/blog/v065-beta1/",
 "dockerImage":"vsharifov/athenaeum:0.6.5-beta.1"}

$ curl -s -o /dev/null -w '%{http_code}' -X POST localhost:8790/api/install_update -H 'content-type: application/json' -d '{}'
501
(body: "updates are installed by pulling the image")
```

Both exactly as specified: `latestVersion` = `0.6.5-beta.1`,
`platformSupported` = `false` (web has no local install path), `dockerImage`
= `vsharifov/athenaeum:0.6.5-beta.1`, `install_update` → `501`.

**Left running** for the owner's browser walk-through, per instruction.

## Running servers — PIDs, ports, how to stop

| Server | PID | Port | Log |
| ------ | --- | ---- | --- |
| `python3 -m http.server` (manifest + builds) | `71585` (`$SCRATCH/serve.pid`) | 8765 | `$SCRATCH/serve.log` |
| `athenaeum-web` | `71901` (`$SCRATCH/web.pid`) | 8790 | `$SCRATCH/web.log` |

Both confirmed alive as of the end of this rehearsal
(`ps -p 71585`/`ps -p 71901`). Stop both with:

```bash
kill $(cat "$SCRATCH/serve.pid") $(cat "$SCRATCH/web.pid")
```

## What remains for the owner

1. Open `/Applications/Athenaeum-rehearsal.app` (already quit after the log
   verification above — a fresh launch will log the check again and should
   show the toast this time since the app is now actually running with a UI
   visible).
2. Expect, in order: toast *"Update available: v0.6.5-beta.1"* → bell → About
   → dialog with the real `RELEASE_NOTES.md` head and
   `You have v0.6.4-beta.1 · released …` → **Download and install** →
   progress bar to the tar.gz's real size (~27 MB) → *"v0.6.5-beta.1 is
   installed — restart to apply"* → **Restart now** → app relaunches →
   **What's new in v0.6.5-beta.1** opens once → About says `v0.6.5-beta.1` →
   **Check for updates** says up to date (server still has to be running for
   that last check — don't kill port 8765 until this is done).
3. After that: `codesign --verify --deep --strict /Applications/Athenaeum-rehearsal.app`
   and `spctl --assess --type execute -v /Applications/Athenaeum-rehearsal.app`
   — both will report **unsigned** (see the ad-hoc note above), *not* the
   originally-anticipated "signed but not notarized" shape, because real
   Developer ID signing could not be exercised in this session. If the owner
   wants that leg actually covered, it needs to run from a session that can
   satisfy the SecurityAgent authorization for that key (a real GUI-session
   terminal at the physical console, or a fresh interactive-once grant), or
   from the same LaunchAgent context the release pipeline already uses.
4. Negative-path checks from the brief (kill the server mid-check, tamper
   the served tar.gz then click Download and install, a second `install_update`
   invoke while one is in flight) were **not** exercised — they require the
   GUI click sequence above to reach the download step, which is the owner's
   leg.
5. Web UI walk-through: open `http://127.0.0.1:8790` in a browser — the same
   toast/dialog flow, notes + `docker pull vsharifov/athenaeum:0.6.5-beta.1`
   with Copy, **Open download page**. Toggle `updates.auto_check` off in
   Settings and reload to confirm no check fires.

## Cleanup list (not yet done — left for after the owner's walk-through)

```bash
kill $(cat "$SCRATCH/serve.pid") $(cat "$SCRATCH/web.pid")   # stop both servers
rm -rf /Applications/Athenaeum-rehearsal.app                  # the rehearsal copy (NOT /Applications/Athenaeum.app)
rm -rf target/aarch64-apple-darwin/release/bundle/dmg/rw.68337.Athenaeum_0.6.4-1_aarch64.dmg  # partial DMG leftover from the failed bundle_dmg.sh run
# $SCRATCH/serve, $SCRATCH/stage, $SCRATCH/b-app, $SCRATCH/webdb are scratchpad-scoped and expire with the session
```

`/Applications/Athenaeum.app` (the owner's real installed copy) was never
touched at any point.

## Summary of deviations from the brief/controller instructions

1. **No real Developer ID codesigning** — environment-blocked
   (`errSecInternalComponent` / `-60008`, full evidence above). Both builds
   are ad-hoc/linker-signed only. This is the single biggest gap versus what
   was asked; everything downstream of it (the updater mechanism itself, the
   manifest, the signature verification, the check-log, the web JSON) is
   unaffected by it and was fully exercised.
2. **`--bundles app` instead of the default `all`** — the DMG target failed
   in this same session (very likely the same class of GUI-session
   restriction). The rehearsal doesn't consume the DMG, so this cost
   nothing observable; flagged for the owner in case it's worth a quick
   sanity check that the *real* runner still builds DMGs fine (it should —
   nothing here suggests otherwise).

No production code, CI script, or config file was edited to work around
either finding.

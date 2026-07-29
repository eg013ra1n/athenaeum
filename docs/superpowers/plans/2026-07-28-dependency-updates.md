# Workspace-Wide Dependency Update Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring every Rust and npm dependency of the Athenaeum workspace (including the rustafits and solvemyastro submodules) to its latest stable version, on branch `0.5.1`.

**Architecture:** Phased by risk — one lockfile-wide semver-compatible refresh first, then one task per breaking (major-version) bump so each lands green and individually revertable. Submodule bumps are their own tasks (separate repos, pointer commits). Every task ends with the project gates: `cargo build --workspace` + `cargo test --workspace` + `npx tsc --noEmit`.

**Tech Stack:** Cargo workspace (8 crates + 2 submodules), npm/Vite/React frontend, Tauri 2 desktop, Axum web.

## Global Constraints

- Branch: `0.5.1` (already checked out) — never commit to `main`.
- Commit author: `eg013ra1n <vilen.sharifov@gmail.com>` — never Claude as author or co-author. Push to `origin` only (GitLab); the `github` remote is unmaintained.
- Gates per task (repo rule — clippy is NOT a gate): `cargo build --workspace`, `cargo test --workspace` (or the crate-scoped subset named in the task), `npx tsc --noEmit` when frontend/types are touched.
- Owner rule — one version of each crate across the whole workspace ("consistent dep versions"). Final task audits with `cargo tree -d`.
- Owner rule — protocol libs (iroh family) verified against published semantics before bumping. Verified 2026-07-28: iroh 1.0.3 is a patch release; iroh-blobs 0.103.0 (latest stable) accepts `iroh ^1.0`; iroh 1.0.3 requires `ed25519-dalek >=3.0.0-rc.0, <4.0.0` so stable 3.0.0 is in-range. `iroh-blobs = "=0.103.0"` and `iroh-tickets = "=1.0.0"` are ALREADY the latest stables — do not touch.
- MSRV ceiling of the new set: rustc 1.91 (iroh). Local toolchain 1.96.1 and Docker `rust:1.96-bookworm` both satisfy it.
- Scope exclusions (owner decisions 2026-07-28): Tailwind CSS stays on 3.4.x (v4 migration = its own future cycle); TypeScript stays on latest 5.x (7.0 native compiler = future cycle). Solver RNG bump IS in scope, gated by corpus_bench (revert on precision regression, re-baseline on sampling-order-only shift).
- `crates/jpeg-encode` is workspace-EXCLUDED on purpose (dev-profile opt trick) — edit its manifest directly; it has no workspace lock entry of its own.

## Audit Snapshot (2026-07-28, source: crates.io + npm)

Already at latest stable — no action: `anyhow` `axum` `byteorder` `cdshealpix 0.9.1` `chrono` `chrono-tz` `clap` `dirs 6` `fd-lock 4.0.4` `flate2` `futures` `hostname 0.4.2` `image 0.25.x` `jpeg-decoder 0.3.2` `libc` `memmap2` `n0-future 0.3.2` `percent-encoding` `postcard 1.1.3` `r2d2 0.8.10` `rayon` `semver` `serde` `serde_json` `tempfile` `tokio` `tokio-stream` `tower 0.5` `tracing` `tracing-appender 0.2.5` `tracing-subscriber 0.3.23` `uuid` `walkdir 2.5` `wiremock 0.6.5` `xxhash-rust` — these all move to their newest compatible point release via the Task 1 `cargo update`.

Major/breaking bumps (each gets a task):

| Dep | From (lock) | To | Where |
| ---- | ---- | ---- | ---- |
| rusqlite | 0.32.1 | 0.40.1 | core, tauri, web, perseus |
| zip | 2.4.2 | 8.6.0 | core, catalog-builder |
| reqwest | 0.12.24 (+0.13.4 dup) | 0.13.4 | core, tauri |
| quick-xml | 0.36.2 / 0.37.5 | 0.41.0 | core, rustafits |
| ts-rs | 10.1.0 | 12.0.1 | core |
| keyring | 3.6.3 | 4.1.5 (conditional) | core |
| notify | 6.1.1 | 8.2.0 | perseus |
| toml / toml_edit | 0.8.2 / 0.23.7 | 1.1.x / 0.25.x | perseus |
| md-5 / sha2 | 0.10.x | 0.11.x | core, catalog-builder |
| base64 | 0.22.1 | 0.23.0 | core, tauri, web, rustafits |
| tower-http | 0.6.11 | 0.7.0 | web |
| tray-icon / auto-launch / open / windows-sys | 0.21 / 0.5 / 5.3 / 0.59 | 0.24 / 0.6 / 5.4 / 0.61 | perseus |
| imageproc (dev) | 0.25.0 | 0.27.0 | core |
| nalgebra | 0.33.3 (+0.32.6 dup) | 0.35.0 | rustafits, solvemyastro |
| lz4_flex / ruzstd / turbojpeg / criterion | 0.11 / 0.7 / 1.4 / 0.5 | 0.14 / 0.9 / 1.5.1 / 0.8 | rustafits (+jpeg-encode) |
| rand / rand_xoshiro | 0.8.5 / 0.6.0 | 0.10.2 / 0.8.1 | solvemyastro |
| ed25519-dalek | 3.0.0-rc.0 | 3.0.0 | core |
| iroh (pin) | =1.0.2 | =1.0.3 | core, perseus |
| vite / @vitejs/plugin-react | 7.1.12 / 4.7.0 | 8.1.5 / 6.0.4 | frontend |
| typescript | 5.8.3 | 5.9.3 (NOT 7.0) | frontend |
| lucide-react | 0.469.0 | 1.27.0 | frontend |
| tailwindcss | 3.4.18 | 3.4.19 (NOT 4.x) | frontend |

npm in-range (land via `npm update`): @tauri-apps/api 2.11.1, @tauri-apps/cli 2.11.4, plugin-dialog 2.7.2, plugin-fs 2.5.1, plugin-opener 2.5.4, @types/react 19.2.17, @types/react-dom 19.2.3, autoprefixer 10.5.4, date-fns 4.4.0, postcss 8.5.24, react/react-dom 19.2.8, react-router-dom 7.18.1, recharts 3.10.1.

---

### Task 0: Record the green baseline

**Files:** none modified.

- [ ] **Step 1: Confirm clean tree on `0.5.1` and submodule pointers**

Run: `git status --short && git branch --show-current && git submodule status`
Expected: clean, `0.5.1`, rustafits at `dbde720`, solvemyastro at `e72193b` (heads/logging).

- [ ] **Step 2: Run all gates once, before touching anything**

Run: `cargo build --workspace 2>&1 | tail -3 && cargo test --workspace 2>&1 | tail -15 && npx tsc --noEmit`
Expected: build OK, all tests pass, tsc silent. If anything is already red, STOP and report — do not start the update on a red baseline.

---

### Task 1: Semver-compatible refresh + iroh pin bump + ed25519-dalek stable

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml` (iroh pin, ed25519-dalek)
- Modify: `crates/perseus/Cargo.toml` (iroh pin, tray-icon, tao-if-needed)
- Modify: `Cargo.lock` (via cargo update), `package-lock.json` (via npm update)

**Interfaces:** none — behavior-preserving version refresh. Later tasks assume the lock already resolves tauri 2.11.5.

- [ ] **Step 1: Bump the iroh pins and ed25519-dalek in `crates/athenaeum-core/Cargo.toml`**

```toml
iroh = "=1.0.3"
```
(replace `iroh = "=1.0.2"`; leave `iroh-blobs = "=0.103.0"` and `iroh-tickets = "=1.0.0"` — already latest), and
```toml
ed25519-dalek = "3.0.0"
```
(replace `ed25519-dalek = "3.0.0-rc.0"`). Update the adjacent comment: iroh 1.0.3 carries `ed25519-dalek >=3.0.0-rc.0, <4.0.0`, resolving to 3.0.0 stable.

- [ ] **Step 2: Same iroh pin in `crates/perseus/Cargo.toml`**

```toml
iroh = "=1.0.3"
```

- [ ] **Step 3: Align Perseus tray pins with tauri 2.11 (single-version rule)**

tauri 2.11.5 requires `tray-icon ^0.24`. In `crates/perseus/Cargo.toml`:
```toml
tray-icon = { version = "0.24", optional = true }
```
Leave `tao = "0.34"` for now — verify after Step 4 with `grep -A1 'name = "tao"' Cargo.lock`; if the lock resolves tao 0.35.x via tauri-runtime-wry, change perseus to `tao = { version = "0.35", optional = true }` in the same commit.

- [ ] **Step 4: Refresh the whole Cargo lock**

Run: `cargo update 2>&1 | tail -30`
Expected: tauri → 2.11.5, tokio → 1.53.x, uuid → 1.24.x, iroh → 1.0.3, ed25519-dalek → 3.0.0, plus dozens of point bumps. Verify single ed25519-dalek: `grep -c 'name = "ed25519-dalek"' Cargo.lock` → `1`.

- [ ] **Step 5: Refresh npm in-range**

Run: `npm update && npm outdated || true`
Expected: only the deliberate majors remain listed (vite, @vitejs/plugin-react, lucide-react, tailwindcss→4, typescript→7).

- [ ] **Step 6: Gates**

Run: `cargo build --workspace 2>&1 | tail -3 && cargo test --workspace 2>&1 | tail -15 && npx tsc --noEmit && npm run build 2>&1 | tail -3`
Expected: all green. tray feature isn't in defaults — additionally `cargo check -p perseus --features tray 2>&1 | tail -3`.

- [ ] **Step 7: Commit**

```bash
git add Cargo.lock package-lock.json crates/athenaeum-core/Cargo.toml crates/perseus/Cargo.toml
git commit -m "chore(deps): semver-compatible refresh; iroh 1.0.3, ed25519-dalek 3.0.0 stable, tray-icon 0.24"
```

---

### Task 2: rusqlite 0.32 → 0.40

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml`, `crates/athenaeum-tauri/Cargo.toml`, `crates/athenaeum-web/Cargo.toml`, `crates/perseus/Cargo.toml`
- Possibly modify: any of the ~96 files using rusqlite (compile-driven; core `db/`, `archive/db.rs`, `file_op/db.rs`, `sync/store.rs`, perseus `seen.rs` are the dense ones)

**Interfaces:** none exported change — `Connection`, `params!`, pool manager behavior identical.

- [ ] **Step 1: Bump all four manifests in one pass** (multi-file-edit rule)

core + tauri: `rusqlite = { version = "0.40", features = ["bundled", "functions"] }`
web + perseus: `rusqlite = { version = "0.40", features = ["bundled"] }`

- [ ] **Step 2: Build and fix compile errors**

Run: `cargo build --workspace 2>&1 | grep -E "^error" | head -30`
Expected breakage class (0.33–0.40 changelog): fallible `column_name()`, `Rows`/`MappedRows` lifetime tightening, `ToSql`/`FromSql` blanket-impl adjustments, bundled SQLite version jump (newer than 3.45 — WAL multi-connection pattern unaffected). Fix mechanically; NO behavior rewrites.

- [ ] **Step 3: Full test suite — DB layer is the most-tested code in the repo**

Run: `cargo test --workspace 2>&1 | tail -15`
Expected: all pass, including schema-init, scanner reparse-in-place, archive db, sync store, perseus seen-store tests.

- [ ] **Step 4: Real-data smoke (repo rule "real data first")**

Run: `sqlite3 "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db" "PRAGMA integrity_check;"` then launch `npm run tauri dev`, open Files page, confirm catalog lists frames, quit. Use the `athenaeum-logs` MCP (`query_logs`, level=error) to confirm zero new DB errors.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "chore(deps): rusqlite 0.32 -> 0.40 (bundled SQLite bump)"
```

---

### Task 3: zip 2 → 8

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml`, `crates/catalog-builder/Cargo.toml`
- Possibly modify: `crates/athenaeum-core/src/archive/{zip_writer,zip_reader,restore}.rs`, `crates/athenaeum-core/src/catalog/gaia_prebuilt.rs`, `crates/catalog-builder/src/publish.rs`

**Interfaces:** archive on-disk format is plain ZIP/deflate — produced archives must stay readable by zip 2-era readers and by Finder/Explorer (no format change, only API).

- [ ] **Step 1: Bump both manifests**

Both: `zip = { version = "8", default-features = false, features = ["deflate", "time"] }`
(verified: zip 8.6.0 still exposes `deflate` and `time` features.)

- [ ] **Step 2: Build and migrate the API**

Run: `cargo build -p athenaeum-core -p catalog-builder 2>&1 | grep -E "^error" | head -30`
Expected breakage: `FileOptions` → `SimpleFileOptions` (type + method-chain changes on `ZipWriter::start_file`), `zip::result::ZipError` variant renames, extended-timestamp API. Read side (`ZipArchive::by_index/by_name`) is mostly stable.

- [ ] **Step 3: Archive tests — the round-trip suite is the correctness oracle**

Run: `cargo test -p athenaeum-core archive 2>&1 | tail -10 && cargo test -p athenaeum-core zip 2>&1 | tail -10`
Expected: all pass (zip round-trip, hash-verify restore, resume/rollback step-log tests).

- [ ] **Step 4: Cross-version read check** — one manual verification that an archive written pre-bump still restores: run `cargo test -p athenaeum-core restore 2>&1 | tail -5`; if no fixture-based test covers an old zip, unzip any existing archive from the dev archive root with the new code path via the app once during Task 13's smoke.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "chore(deps): zip 2 -> 8 (archive + catalog publish)"
```

---

### Task 4: reqwest 0.12 → 0.13 (kills a duplicate)

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml` (`reqwest = { version = "0.13", features = ["blocking", "json"] }`)
- Modify: `crates/athenaeum-tauri/Cargo.toml` (`reqwest = { version = "0.13", features = ["json"] }`)
- Possibly modify: `crates/athenaeum-core/src/account/hub_client.rs`, `catalog/gaia_download` call-sites, tauri update-check

**Interfaces:** none — HTTP client internals only. Verified: 0.13.4 keeps `blocking` and `json` features.

- [ ] **Step 1: Bump both manifests** (as above).

- [ ] **Step 2: Build + fix** — Run: `cargo build --workspace 2>&1 | grep -E "^error" | head -20`. Expected: small (client builder/TLS default renames).

- [ ] **Step 3: Verify the duplicate is gone**

Run: `grep -c 'name = "reqwest"' Cargo.lock`
Expected: `1` (0.13.4 was already in the lock transitively; unifying removes 0.12.24).

- [ ] **Step 4: Hub-client tests (wiremock-backed)**

Run: `cargo test -p athenaeum-core account 2>&1 | tail -10 && cargo test -p perseus 2>&1 | tail -5`
Expected: pass — login/pairing flows exercised against the mock hub.

- [ ] **Step 5: Commit** — `git add -A && git commit -m "chore(deps): reqwest 0.12 -> 0.13, unify single version"`

---

### Task 5: quick-xml 0.36 → 0.41 (core side)

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml` (`quick-xml = "0.41"`)
- Possibly modify: `crates/athenaeum-core/src/fits_parser/mod.rs`, `crates/athenaeum-core/src/fits_parser/stored_header.rs` (XISF XML header parsing)

**Interfaces:** XISF parse results unchanged — same `ImageMetadata` out. rustafits's own quick-xml 0.37 → 0.41 happens in Task 10; a temporary version dup in the lock between these tasks is expected and resolved there.

- [ ] **Step 1: Bump manifest** (as above).
- [ ] **Step 2: Build + migrate** — Run: `cargo build -p athenaeum-core 2>&1 | grep -E "^error" | head -20`. Expected breakage: `Reader` event API renames (`read_event_into`, attribute unescape signatures) accumulated over 0.37–0.41.
- [ ] **Step 3: XISF tests with real data** — Run: `cargo test -p athenaeum-core xisf 2>&1 | tail -10` and `cargo test -p athenaeum-core fits_parser 2>&1 | tail -10`. Expected: pass.
- [ ] **Step 4: Commit** — `git add -A && git commit -m "chore(deps): quick-xml 0.36 -> 0.41 (core XISF header parsing)"`

---

### Task 6: ts-rs 10 → 12 + regenerate TS models

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml` (`ts-rs = { version = "12", features = ["chrono-impl", "no-serde-warnings"] }`)
- Regenerate: `src/types/models.ts` (via the ts_export mechanism)
- Test: `crates/athenaeum-core/tests/ts_contract.rs` (or wherever the generated-output diff guard lives — locate with `grep -rn "ts_contract" crates/athenaeum-core`)

**Interfaces:** generated TS types consumed by the whole frontend — `npx tsc --noEmit` is the real gate.

- [ ] **Step 1: Verify features exist in v12** — Run: `curl -s -A "dep-audit" https://crates.io/api/v1/crates/ts-rs/12.0.1 | python3 -c "import json,sys; print(sorted(json.load(sys.stdin)['version']['features']))"`. If `chrono-impl`/`no-serde-warnings` were renamed, use the v12 names (check the ts-rs CHANGELOG via context7/docs.rs) — do NOT drop chrono support silently.
- [ ] **Step 2: Bump manifest, build** — Run: `cargo build -p athenaeum-core 2>&1 | grep -E "^error" | head -20`. Expected: derive-macro attribute changes across 10→12 (e.g. `#[ts(export_to)]` semantics).
- [ ] **Step 3: Regenerate + diff** — run the export test/binary (`cargo test -p athenaeum-core ts_ 2>&1 | tail -10`), then `git diff --stat src/types/`. Review: only formatting/import-style changes allowed; any TYPE change (field renamed, optionality flipped) must be traced to a deliberate ts-rs semantic change and reconciled with the serde attrs — never hand-edit the generated file.
- [ ] **Step 4: Frontend gate** — Run: `npx tsc --noEmit`. Expected: silent.
- [ ] **Step 5: Commit** — `git add -A && git commit -m "chore(deps): ts-rs 10 -> 12, regenerate TS models"`

---

### Task 7: keyring 3 → 4 (research-gated, explicit fallback)

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml`
- Modify: `crates/athenaeum-core/src/account/token_store.rs` (sole usage site)

**Interfaces:** MUST-HOLD invariants: (a) existing stored device tokens remain readable after the bump (same macOS Keychain item — service `KEYRING_SERVICE` / account); (b) Linux/headless builds still compile with NO dbus/secret-service dependency and fall through to the 0600-file path; (c) token never logged.

- [ ] **Step 1: Research first — v4 restructured stores.** Verified: keyring 4.1.5 has features `[cli, default, v1]` — the v3 `apple-native`/`windows-native` features are GONE (stores moved out of the crate). Read the v4 migration notes (context7 or docs.rs/keyring/4) and determine: which companion store crate(s) provide the macOS Keychain + Windows Credential Manager backends, and whether the default store maps to the same underlying keychain items as v3's `apple-native`.
- [ ] **Step 2: Decision gate.** If v4 + store crates can hold all three invariants → migrate `token_store.rs` (small file, `Entry::new`/`set_password`/`get_password`/`delete`, `Error::NoEntry` arms) and add the store crates with `default-features = false` as needed. If ANY invariant fails (e.g. v4 default store reads a different keychain item → users get logged out) → STAY on `keyring = "3"`, add a manifest comment `# keyring 4 rejected 2026-07-28: <concrete reason>`, and record the decision in the final report.
- [ ] **Step 3 (migrate path only): Token round-trip verification on THIS machine** — `cargo test -p athenaeum-core token_store 2>&1 | tail -5`, then the real check: launch the desktop app, confirm the hub account is STILL signed in (existing v3-written token read by v4 code). A forced re-login is a failed invariant → revert.
- [ ] **Step 4: Commit** — `git add -A && git commit -m "chore(deps): keyring 3 -> 4 (native stores)"` (or the rejection-comment commit).

---

### Task 8: Perseus stack — notify 8, toml 1.x, toml_edit 0.25, windows-sys 0.61, auto-launch 0.6, open 5.4

**Files:**
- Modify: `crates/perseus/Cargo.toml`
- Possibly modify: `crates/perseus/src/watcher.rs` (notify), `crates/perseus/src/config.rs` + `config_edit.rs` (toml/toml_edit)

**Interfaces:** watcher semantics MUST be preserved — stability tracker + WatcherForget seam (T9b) depend on event delivery per path; TOML config files written by 0.5.0 installs must parse identically.

- [ ] **Step 1: Bump the manifest in one pass**

```toml
toml = "1"
toml_edit = "0.25"
notify = "8"
open = { version = "5.4", optional = true }
auto-launch = { version = "0.6", optional = true }
# [target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = ["Win32_Storage_FileSystem", "Win32_Foundation"] }
```

- [ ] **Step 2: Build + migrate** — Run: `cargo build -p perseus 2>&1 | grep -E "^error" | head -30` and `cargo check -p perseus --features tray 2>&1 | tail -3`. Expected breakage: notify 7/8 event/serialization API (do NOT enable `serialization-compat-6` — no serialized notify events cross a boundary here), toml 1.0 `Value`/error-type moves, toml_edit 0.24/0.25 `DocumentMut` method renames, windows-sys feature-path check (Windows compile verified by CI, not locally — note it).
- [ ] **Step 3: Perseus test suite incl. watcher + scheduler + config round-trip** — Run: `cargo test -p perseus 2>&1 | tail -15`. Expected: all pass, notably watcher stability tests, schedule.rs DST arms, config_edit comment-preservation tests.
- [ ] **Step 4: Live watcher smoke** — run a local perseus against a temp dir (`cargo run -p perseus -- --help` first for wiring), copy a FITS file in, confirm the batcher enqueues it (log line). Skip if the existing integration test in `crates/perseus/tests/` already covers watch-enqueue end-to-end (check first — it does cover an in-process transport flow).
- [ ] **Step 5: Commit** — `git add -A && git commit -m "chore(deps): perseus stack — notify 8, toml 1.0, toml_edit 0.25, windows-sys 0.61"`

---

### Task 9: Small unifiers — digest family, base64, tower-http, imageproc

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml` (`sha2 = "0.11"`, `md-5 = "0.11"`, `base64 = "0.23"`, `imageproc = { version = "0.27", default-features = false }` dev-dep)
- Modify: `crates/catalog-builder/Cargo.toml` (`sha2 = "0.11"`)
- Modify: `crates/athenaeum-tauri/Cargo.toml` (`base64 = "0.23"`)
- Modify: `crates/athenaeum-web/Cargo.toml` (`base64 = "0.23"`, `tower-http = { version = "0.7", features = ["cors", "fs"] }`)

**Interfaces:** hash OUTPUTS must not change (sha2/md5 are algorithms — only the `digest` trait API moves); base64 encoded artifacts unchanged (standard alphabet).

- [ ] **Step 1: Bump all manifests in one pass** (as listed). Note: sha2 0.11 is ALREADY in the lock transitively — this unifies; imageproc 0.27 pairs with `image ^0.25.8` (verified) so the existing `image = "0.25"` dev-dep stands.
- [ ] **Step 2: Build + migrate** — Run: `cargo build --workspace 2>&1 | grep -E "^error" | head -20`. Expected: digest 0.11 trait-method renames (`Digest::new/update/finalize` largely stable; `Output` type paths move), base64 0.23 engine API tweaks.
- [ ] **Step 3: Duplicate audit for this slice** — Run: `for c in sha2 base64 md-5; do echo "$c: $(grep -c "name = \"$c\"" Cargo.lock)"; done`. Expected: `sha2: 1`, `md-5: 1`; base64 may still show 2 (rustafits at 0.22 until Task 10, plus a possible 0.21 transitive — record, don't chase transitives).
- [ ] **Step 4: Gates** — `cargo test --workspace 2>&1 | tail -15` (covers xxhash/sha checksums in archive + catalog + web auth routes).
- [ ] **Step 5: Commit** — `git add -A && git commit -m "chore(deps): sha2/md-5 0.11, base64 0.23, tower-http 0.7, imageproc 0.27"`

---

### Task 10: rustafits submodule — quick-xml 0.41, nalgebra 0.35, lz4_flex 0.14, ruzstd 0.9, turbojpeg 1.5, base64 0.23, criterion 0.8 (+ jpeg-encode turbojpeg 1.5)

**Files (submodule repo `rustafits/`):**
- Modify: `rustafits/Cargo.toml`
- Possibly modify: `rustafits/src/formats/xisf.rs` (quick-xml + ruzstd + lz4_flex), SIP/geometry files using nalgebra
- Also (parent repo): `crates/jpeg-encode/Cargo.toml` (`turbojpeg = "1.5"`)

**Interfaces:** `astroimage` public API unchanged; JPEG/PNG bytes may differ (turbojpeg 1.5 encoder) — visual equivalence is the bar, not byte equality. solvemyastro (Task 11) needs nalgebra 0.35 here FIRST since both submodules must agree.

- [ ] **Step 1: Put the submodule on a branch** — `cd rustafits && git fetch && git checkout main && git merge --ff-only dbde720` (or checkout whatever branch contains `dbde720` — verify with `git branch -a --contains dbde720`). The merge-gate pointer `dbde720` must remain an ancestor.
- [ ] **Step 2: Bump `rustafits/Cargo.toml`**

```toml
lz4_flex = { version = "0.14", default-features = false, features = ["safe-decode"] }
ruzstd = "0.9"
turbojpeg = "1.5"
quick-xml = "0.41"
nalgebra = "0.35"
# [dev-dependencies]
criterion = { version = "0.8", features = ["html_reports"] }
```

- [ ] **Step 3: Build + migrate inside the submodule** — Run: `cd rustafits && cargo build 2>&1 | grep -E "^error" | head -30`. Expected breakage: ruzstd 0.8 moved `StreamingDecoder` under `ruzstd::decoding::`; lz4_flex 0.12+ error-type changes; quick-xml as in Task 5; nalgebra 0.34/0.35 renames are minor for the types used; criterion 0.8 bench macro API (`criterion_group!` config changes).
- [ ] **Step 4: The four golden files (rustafits CLAUDE.md rule)** — Run: `cd rustafits && cargo test 2>&1 | tail -10`. Expected: pass against `tests/cocoon.fits`, `tests/mono.fits`, `tests/osc.fits`, `tests/test.xisf` — the XISF one exercises all three decompressors (zlib/LZ4/Zstd).
- [ ] **Step 5: jpeg-encode alignment** — set `turbojpeg = "1.5"` in `crates/jpeg-encode/Cargo.toml` (parent repo) so the native lib unifies; `cargo build -p athenaeum-core 2>&1 | tail -3`.
- [ ] **Step 6: Commit + push the submodule** — `cd rustafits && git add Cargo.toml Cargo.lock src && git commit -m "chore(deps): quick-xml 0.41, nalgebra 0.35, lz4_flex 0.14, ruzstd 0.9, turbojpeg 1.5, criterion 0.8" && git push origin HEAD`. (Author = eg013ra1n, its own repo.)
- [ ] **Step 7: Do NOT bump the parent pointer yet** — Task 11 bumps both pointers together so nalgebra lands atomically in the workspace lock.

---

### Task 11: solvemyastro submodule — nalgebra 0.35, rand 0.10 + rand_xoshiro 0.8 (corpus-gated) + both submodule pointers

**Files (submodule repo `solvemyastro/`):**
- Modify: `solvemyastro/Cargo.toml`
- Possibly modify: `solvemyastro/src/orchestrate.rs` (rand API), lsq/SIP files (nalgebra)
- Parent repo: submodule pointer bumps for BOTH `rustafits` and `solvemyastro`, workspace `Cargo.lock`

**Interfaces:** solver precision is the contract — the corpus_bench baseline (run QUIET; `cone_calls` deterministic) is the gate. Decision matrix pre-agreed with owner: precision regression → revert rand family; sampling-order-only shift with equal-or-better precision → re-baseline.

- [ ] **Step 1: Branch** — `cd solvemyastro && git checkout logging` (pointer `e72193b` is heads/logging).
- [ ] **Step 2: Bump `solvemyastro/Cargo.toml`**

```toml
nalgebra = "0.35"
rand = "0.10"
rand_xoshiro = "0.8"
```

- [ ] **Step 3: Build + migrate** — Run: `cd solvemyastro && cargo build 2>&1 | grep -E "^error" | head -20`. Expected: rand 0.9+ renames in `orchestrate.rs` — `Rng::gen_range` → `Rng::random_range`, `thread_rng` → `rng` (only if used; the PROSAC path is `Xoshiro256StarStar::seed_from_u64` which is unchanged). nalgebra minor.
- [ ] **Step 4: Unit tests** — `cd solvemyastro && cargo test 2>&1 | tail -10`. Expected: pass.
- [ ] **Step 5: THE GATE — corpus bench against baseline** — run the corpus_bench regression gate exactly as documented in the solvemyastro repo (QUIET mode). Compare precision + `cone_calls` against the committed baseline:
  - identical → done;
  - shifted sampling, precision equal/better → update the baseline JSON in the same commit, note "rand 0.10 uniform-sampling mapping change" in the commit body;
  - precision worse → `git checkout -- Cargo.toml src` for the rand family only, keep nalgebra 0.35, add manifest comment `# rand kept at 0.8: 0.10 mapping regressed corpus precision (2026-07-28)`.
- [ ] **Step 6: Commit + push submodule** — `cd solvemyastro && git add -A && git commit -m "chore(deps): nalgebra 0.35, rand 0.10 + rand_xoshiro 0.8 (corpus gate: <outcome>)" && git push origin logging`.
- [ ] **Step 7: Bump both pointers in the parent + relock** — `cd .. && git add rustafits solvemyastro && cargo update -p nalgebra 2>/dev/null; cargo build --workspace 2>&1 | tail -3`. Verify single nalgebra: `grep -c 'name = "nalgebra"' Cargo.lock` → ideally `1` (a leftover 0.32.6 transitive is acceptable — record it). Verify single quick-xml major at 0.41 + any transitive: `grep -A1 'name = "quick-xml"' Cargo.lock`.
- [ ] **Step 8: Workspace gates + commit** — `cargo test --workspace 2>&1 | tail -15`, then `git add Cargo.lock rustafits solvemyastro crates/jpeg-encode/Cargo.toml && git commit -m "chore(deps): bump rustafits + solvemyastro submodule pointers (dep refresh)"`.

---

### Task 12: Frontend majors — vite 8, @vitejs/plugin-react 6, typescript 5.9, lucide-react 1.x

**Files:**
- Modify: `package.json`, `package-lock.json`
- Possibly modify: `vite.config.ts`, any of the ~102 files importing from `lucide-react` (icon renames), `tsconfig.json`

**Interfaces:** `npm run build` and `npm run build:web` must produce a working bundle; design tokens untouched (Tailwind stays 3.4.x).

- [ ] **Step 1: Edit `package.json` ranges**

```json
"lucide-react": "^1.27.0",
"@vitejs/plugin-react": "^6.0.4",
"tailwindcss": "^3.4.19",
"typescript": "~5.9.3",
"vite": "^8.1.5"
```

- [ ] **Step 2: Install** — Run: `npm install 2>&1 | tail -5`. Expected: no peer-dep errors (plugin-react 6 supports vite 8; node v26 local / node:22 Docker both satisfy vite 8 engines).
- [ ] **Step 3: Type-check — this catches every lucide rename** — Run: `npx tsc --noEmit 2>&1 | head -40`. Fix icon imports mechanically (lucide 1.x renamed/retired some icons; pick the 1.x name closest in meaning, verify visually in Step 5). NO other frontend refactors.
- [ ] **Step 4: Build both targets** — Run: `npm run build 2>&1 | tail -5 && npm run build:web 2>&1 | tail -5`. Expected: both green. If vite 8 flags config options in `vite.config.ts` (removed/renamed options), fix per the vite 8 migration guide (context7).
- [ ] **Step 5: Visual smoke** — `npm run tauri dev`, click through: Files, Objects, Equipment, Transfers, Settings. Confirm icons render (no blank squares), charts render (recharts 3.10), no console errors.
- [ ] **Step 6: Commit** — `git add package.json package-lock.json vite.config.ts src && git commit -m "chore(deps): vite 8, plugin-react 6, typescript 5.9, lucide-react 1.x"`

---

### Task 13: Final sweep — duplicate audit, Docker/CI, docs, report

**Files:**
- Possibly modify: `CLAUDE.md` (stale line: "Docker … rust:1.85-bookworm" — Dockerfile is already 1.96), `docker/Dockerfile` (only if a new dep needs a system package — turbojpeg 1.5 still needs nasm+cmake, both present)

- [ ] **Step 1: Full gates one last time** — Run: `cargo build --workspace 2>&1 | tail -3 && cargo test --workspace 2>&1 | tail -15 && npx tsc --noEmit && npm run build 2>&1 | tail -3 && npm run build:web 2>&1 | tail -3`. All green.
- [ ] **Step 2: Duplicate-version audit (owner rule)** — Run: `cargo tree -d 2>&1 | head -60`. Direct deps must be single-version; list remaining transitive dups in the report (windows-sys multi-version is normal/unavoidable).
- [ ] **Step 3: Docker build check** — Run: `docker build -f docker/Dockerfile . 2>&1 | tail -5` (needs OrbStack running; skip with a note if unavailable — CI covers it on the next pipeline).
- [ ] **Step 4: CI sanity** — after pushing, watch the `0.5.1` branch pipeline via the GitLab API (PAT at `~/.config/athenaeum-ci/gitlab-pat`); the Linux runner + Windows job compile-check what local macOS cannot (windows-sys 0.61, tray deps).
- [ ] **Step 5: Docs touch-up** — fix the stale rust-image line in `CLAUDE.md` Docker section; nothing else (release notes happen at release time per the release workflow).
- [ ] **Step 6: Final commit + push** — `git add -A && git commit -m "chore(deps): finish dependency refresh — docs + lock tidy" && git push origin 0.5.1`.
- [ ] **Step 7: Report to owner** — summary table of what moved, what was deliberately held back (Tailwind 4, TS 7, keyring-if-rejected, rand-if-reverted), corpus-gate outcome, and remaining transitive dups.

---

## Deliberately NOT updated (and why)

| Dep | Held at | Reason |
| ---- | ---- | ---- |
| tailwindcss | 3.4.19 | Owner decision 2026-07-28 — v4 CSS-first migration is its own cycle with a full visual smoke |
| typescript | 5.9.3 | Owner decision — 7.0 native compiler evaluated separately |
| iroh-blobs / iroh-tickets | 0.103.0 / 1.0.0 | Already latest stable; protocol pins |
| keyring | 4.x conditional | Task 7 invariant gate — token continuity beats freshness |
| rand family | 0.10 conditional | Task 11 corpus gate — precision beats freshness |

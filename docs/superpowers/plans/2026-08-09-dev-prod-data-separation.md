# Dev/Prod Data Separation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Debug desktop builds resolve a `.dev` sibling app-data directory (`com.vsharifov.athenaeum.dev`) so `npm run tauri dev` can never touch the production catalog; a refresh script snapshots the prod DB into the dev tree.

**Architecture:** One name-helper in `athenaeum-core` (`paths::app_data_dir_name()`, `.dev` suffix under `debug_assertions`) consumed by the core logging fallback and a new tauri-side resolver that swaps the leaf of Tauri's `app_data_dir()`. Env `ATHENAEUM_APP_DATA_DIR` overrides everything. `log-mcp` scans both flavor dirs. `scripts/dev-db-refresh.mjs` snapshots the prod DB (WAL-safe), wipes identity-bound transfer tables, symlinks `catalogs/`.

**Tech Stack:** Rust (workspace crates `athenaeum-core`, `athenaeum-tauri`, `log-mcp`), Node ≥22 script (no new deps), sqlite3 CLI.

**Spec:** `docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md`

## Global Constraints

- Branch: `0.5.1`. Commit as the repo user (`eg013ra1n`) — never Claude as author/co-author.
- Release builds must be byte-for-byte unaffected: every behavior change is gated on `cfg!(debug_assertions)` or the additive `ATHENAEUM_APP_DATA_DIR` env read.
- Web backend (`athenaeum-web`) is NOT touched — its DB path already comes from `ATHENAEUM_DB_PATH`/`/data`.
- Never string-mangle the dotted identifier with extension APIs (`set_extension` would truncate `com.vsharifov.athenaeum`); swap whole path components only.
- The snapshot script must never copy `sync/`, `account/`, or `logs/` (identity must not be cloned).
- Zero-print rule exemptions: the Node script prints to stdout (dev CLI tool, same category as `catalog-builder`); no `println!` in any Rust production code.
- Gates per repo convention: `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`. clippy is NOT a gate. A `cargo check` hook fires automatically after Rust edits.

---

### Task 1: Core `paths` module + logging fallback

**Files:**
- Create: `crates/athenaeum-core/src/paths.rs`
- Modify: `crates/athenaeum-core/src/lib.rs` (module list, after `pub mod coordinates;`)
- Modify: `crates/athenaeum-core/src/logging/mod.rs:39-62` (`resolve_app_data_dir`)

**Interfaces:**
- Produces: `athenaeum_core::paths::app_data_dir_name() -> &'static str` — used by Task 2's tauri resolver.
- Behavior: core logging fallback now honors `ATHENAEUM_APP_DATA_DIR` and lands debug-build logs in the `.dev` tree.

- [ ] **Step 1: Write `paths.rs` with its test**

```rust
//! App-data directory identity, shared by every consumer that must locate
//! the desktop data tree without a Tauri handle (the logging fallback in
//! `crate::logging`, the desktop resolver in `athenaeum-tauri/src/paths.rs`).
//!
//! Debug builds resolve a `.dev` SIBLING directory so `npm run tauri dev`
//! can never touch the production catalog on the same machine — the same
//! debug/release split as the test-hub default in
//! `settings::defaults::ACCOUNT_HUB_URL`. Release builds are unaffected.
//! The `ATHENAEUM_APP_DATA_DIR` env override (honored by the resolvers
//! that consume this name, not here) wins over both.
//! Spec: docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md

/// Directory name under the platform app-data root
/// (`~/Library/Application Support`, `%APPDATA%`, `~/.local/share`).
pub fn app_data_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "com.vsharifov.athenaeum.dev"
    } else {
        "com.vsharifov.athenaeum"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_suffix_tracks_build_profile() {
        if cfg!(debug_assertions) {
            assert_eq!(app_data_dir_name(), "com.vsharifov.athenaeum.dev");
        } else {
            assert_eq!(app_data_dir_name(), "com.vsharifov.athenaeum");
        }
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/athenaeum-core/src/lib.rs`, after `pub mod coordinates;` add:

```rust
pub mod paths;
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p athenaeum-core --lib paths -- --nocapture`
Expected: PASS (`dev_suffix_tracks_build_profile`)

- [ ] **Step 4: Rewrite `logging::resolve_app_data_dir`**

Replace the whole function at `crates/athenaeum-core/src/logging/mod.rs:39-62` (keep `resolve_log_dir` and everything else untouched):

```rust
/// Resolve the app data directory from environment variables (no Tauri
/// needed). `ATHENAEUM_APP_DATA_DIR` wins verbatim; otherwise mirrors
/// Tauri's default app_data_dir resolution per platform, using
/// `crate::paths::app_data_dir_name()` for the leaf — so debug builds land
/// in the `.dev` sibling, consistent with the desktop's own resolver
/// (`athenaeum-tauri/src/paths.rs`).
fn resolve_app_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ATHENAEUM_APP_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA")
            .map(|d| PathBuf::from(d).join(crate::paths::app_data_dir_name()));
    }
    #[cfg(target_os = "macos")]
    {
        return std::env::var_os("HOME").map(|d| {
            PathBuf::from(d)
                .join("Library/Application Support")
                .join(crate::paths::app_data_dir_name())
        });
    }
    #[cfg(target_os = "linux")]
    {
        return std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|d| d.join(crate::paths::app_data_dir_name()));
    }
    #[allow(unreachable_code)]
    None
}
```

- [ ] **Step 5: Compile check**

Run: `cargo check -p athenaeum-core`
Expected: clean (warnings-free for the touched files)

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-core/src/paths.rs crates/athenaeum-core/src/lib.rs crates/athenaeum-core/src/logging/mod.rs
git commit -m "feat(core): .dev app-data identity for debug builds + ATHENAEUM_APP_DATA_DIR override"
```

---

### Task 2: Tauri-side resolver + three call sites

**Files:**
- Create: `crates/athenaeum-tauri/src/paths.rs`
- Modify: `crates/athenaeum-tauri/src/lib.rs` (module decl next to `mod commands;`; call site ~line 155)
- Modify: `crates/athenaeum-tauri/src/commands/core.rs:54-57` (`initialize_database`)
- Modify: `crates/athenaeum-tauri/src/commands/files.rs:190-193` (`get_database_path`)

**Interfaces:**
- Consumes: `athenaeum_core::paths::app_data_dir_name()` (Task 1).
- Produces: `crate::paths::resolve_app_data_dir(&tauri::AppHandle) -> Result<PathBuf, String>` — the ONLY way the desktop host resolves its app-data dir from now on; `app_handle.path().app_data_dir()` must not be called anywhere else.

- [ ] **Step 1: Write `crates/athenaeum-tauri/src/paths.rs`**

```rust
//! Desktop app-data resolution — the single place the Tauri host converts
//! `app_data_dir()` into THIS build flavor's data tree.
//!
//! `ATHENAEUM_APP_DATA_DIR` wins verbatim (bug-triage / deliberate
//! debug-against-prod escape hatch). Otherwise Tauri's platform dir has its
//! final component swapped for `athenaeum_core::paths::app_data_dir_name()`
//! — identical in release, the `.dev` sibling in debug.
//! Spec: docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md

use std::path::PathBuf;
use tauri::Manager;

pub(crate) fn resolve_app_data_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("ATHENAEUM_APP_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let platform_dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    Ok(rename_leaf(platform_dir))
}

/// Swap the final path component for the build-flavor identifier. Component
/// replacement, never `set_extension` — the identifier is dotted, an
/// extension API would truncate it.
fn rename_leaf(platform_dir: PathBuf) -> PathBuf {
    match platform_dir.parent() {
        Some(parent) => parent.join(athenaeum_core::paths::app_data_dir_name()),
        None => platform_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_leaf_swaps_only_the_final_component() {
        let out = rename_leaf(PathBuf::from(
            "/x/Application Support/com.vsharifov.athenaeum",
        ));
        assert_eq!(
            out,
            PathBuf::from("/x/Application Support")
                .join(athenaeum_core::paths::app_data_dir_name())
        );
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/athenaeum-tauri/src/lib.rs`, next to `mod commands;` add:

```rust
mod paths;
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p athenaeum-tauri --lib paths -- --nocapture`
Expected: PASS (`rename_leaf_swaps_only_the_final_component` — asserts the `.dev` leaf, since tests build with `debug_assertions`)

- [ ] **Step 4: Convert the three call sites**

`crates/athenaeum-tauri/src/commands/core.rs` (`initialize_database`) — replace lines 54-57:

```rust
    let app_dir = crate::paths::resolve_app_data_dir(&app_handle)?;
```

`crates/athenaeum-tauri/src/commands/files.rs` (`get_database_path`) — replace lines 190-193:

```rust
    let app_dir = crate::paths::resolve_app_data_dir(&app_handle)?;
```

(the following `Ok(app_dir.join("athenaeum.db")…)` line stays). Extend the function's doc comment ("NOT single-sourced…") with one line: `/// Resolution goes through `crate::paths::resolve_app_data_dir` (build-flavor aware).`

`crates/athenaeum-tauri/src/lib.rs` legacy-cache cleanup (~line 155) — replace:

```rust
            if let Ok(app_dir) = crate::paths::resolve_app_data_dir(&app_handle) {
```

- [ ] **Step 5: Compile check + sweep**

Run: `cargo check -p athenaeum-tauri`
Expected: clean. If a `use tauri::Manager;` import in `core.rs`/`files.rs` becomes unused, remove it (only if the check flags it — both files may use `Manager` elsewhere).

Run: `grep -rn "app_data_dir()" crates/athenaeum-tauri/src --include="*.rs"`
Expected: exactly one hit — inside `paths.rs`.

- [ ] **Step 6: Commit**

```bash
git add crates/athenaeum-tauri/src/paths.rs crates/athenaeum-tauri/src/lib.rs crates/athenaeum-tauri/src/commands/core.rs crates/athenaeum-tauri/src/commands/files.rs
git commit -m "feat(tauri): debug builds resolve the .dev app-data sibling (single resolver)"
```

---

### Task 3: log-mcp scans both flavor dirs

**Files:**
- Modify: `crates/log-mcp/src/query.rs` (`log_files`:79-90, `scan`:176-178, `scan_with`:184-212, `list_operations`, `get_operation`, `tail`, resolvers:309-351)
- Modify: `crates/log-mcp/src/rpc.rs` (`handle`:61, `dispatch_tool_call`:223)
- Modify: `crates/log-mcp/src/main.rs`
- Test: `crates/log-mcp/tests/query_fixture.rs`

**Interfaces:**
- Consumes: nothing from other tasks (log-mcp stays dependency-free of `athenaeum-core` — both identifier names are duplicated literals, per the existing comment at `query.rs:309-313`).
- Produces: `query::scan/tail/list_operations/get_operation(dirs: &[PathBuf], …)`, `query::default_log_dirs() -> Vec<PathBuf>`, `rpc::handle(req, log_dirs: &[PathBuf])`. `default_log_dir` (singular) and `resolve_app_data_dir` are deleted.

- [ ] **Step 1: Write the failing multi-dir test**

Append to `crates/log-mcp/tests/query_fixture.rs` (adjust imports at top if `PathBuf` is not yet imported: `use std::path::PathBuf;`):

```rust
#[test]
fn scan_merges_files_across_multiple_dirs_and_skips_missing() {
    let prod = tempfile::tempdir().expect("tempdir");
    let dev = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        prod.path().join("athenaeum-desktop.2026-08-08.jsonl"),
        r#"{"timestamp":"2026-08-08T10:00:00.000000Z","level":"INFO","target":"athenaeum_core::scanner","fields":{"message":"prod event"}}"#,
    )
    .expect("write prod fixture");
    std::fs::write(
        dev.path().join("athenaeum-desktop.2026-08-09.jsonl"),
        r#"{"timestamp":"2026-08-09T10:00:00.000000Z","level":"INFO","target":"athenaeum_core::scanner","fields":{"message":"dev event"}}"#,
    )
    .expect("write dev fixture");

    let both = vec![prod.path().to_path_buf(), dev.path().to_path_buf()];
    let results = log_mcp::query::tail(&both, 10).expect("tail");
    assert_eq!(results.len(), 2, "events from both dirs: {results:?}");
    // filename sort ⇒ the older file streams first regardless of which dir owns it
    assert_eq!(results[0]["fields"]["message"], "prod event");
    assert_eq!(results[1]["fields"]["message"], "dev event");

    // a missing dir (the .dev tree before the first dev run) is skipped, never an error
    let with_missing = vec![
        prod.path().to_path_buf(),
        PathBuf::from("/nonexistent-athenaeum-dev-tree"),
    ];
    assert_eq!(log_mcp::query::tail(&with_missing, 10).expect("tail").len(), 1);
}
```

Match the import style already used in the file (it may use `use log_mcp::query;` — then call `query::tail(…)`).

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p log-mcp --test query_fixture scan_merges -- --nocapture`
Expected: COMPILE FAIL — `tail` takes `&Path`, not `&[PathBuf]`.

- [ ] **Step 3: Convert `query.rs` to multi-dir**

Replace `log_files` (lines 79-90):

```rust
/// `*.jsonl` files across `dirs`, sorted by filename — chronological because
/// `tracing-appender`'s daily rolling prefix format sorts that way
/// (`<prefix>.<date>.jsonl`) — with the full path as tie-break. A dir that
/// cannot be read (typically the `.dev` sibling before the first dev run)
/// is skipped: one missing tree must not hide the other's logs.
fn log_files(dirs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        files.extend(
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl")),
        );
    }
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()).then_with(|| a.cmp(b)));
    Ok(files)
}
```

Change the four public signatures + `scan_with` mechanically — `dir: &Path` → `dirs: &[PathBuf]`, and pass `dirs` through (bodies otherwise unchanged):

```rust
pub fn scan(dirs: &[PathBuf], f: &Filter) -> Result<Vec<Value>> { scan_with(dirs, f.limit, |v| event_matches(v, f)) }
fn scan_with(dirs: &[PathBuf], limit: usize, mut predicate: impl FnMut(&Value) -> bool) -> Result<Vec<Value>> { /* body unchanged; `for path in log_files(dirs)?` */ }
pub fn tail(dirs: &[PathBuf], n: usize) -> Result<Vec<Value>> { /* scan(dirs, …) */ }
pub fn list_operations(dirs: &[PathBuf], kind: Option<&str>, since: Option<&str>, limit: usize) -> Result<Vec<OperationSummary>> { /* scan_with(dirs, …) */ }
pub fn get_operation(dirs: &[PathBuf], id: &str, limit: usize) -> Result<Vec<Value>> { /* scan(dirs, …) */ }
```

If `use std::path::Path;` becomes unused in `query.rs`, drop `Path` from the import.

Replace `resolve_app_data_dir` + `default_log_dir` (lines 309-351) with:

```rust
/// Both build flavors' app-data dirs (production + the debug `.dev`
/// sibling), existing or not — enumeration skips missing dirs. Duplicated
/// (not imported) from `athenaeum_core` so this crate stays dependency-free
/// of it; log-mcp is an observer and always watches BOTH trees regardless
/// of its own build profile.
fn resolve_app_data_dirs() -> Vec<PathBuf> {
    const IDENTS: [&str; 2] = ["com.vsharifov.athenaeum", "com.vsharifov.athenaeum.dev"];
    fn root_dir() -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            return std::env::var_os("APPDATA").map(PathBuf::from);
        }
        #[cfg(target_os = "macos")]
        {
            return std::env::var_os("HOME")
                .map(|d| PathBuf::from(d).join("Library/Application Support"));
        }
        #[cfg(target_os = "linux")]
        {
            return std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
        }
        #[allow(unreachable_code)]
        None
    }
    root_dir()
        .map(|r| IDENTS.iter().map(|i| r.join(i)).collect())
        .unwrap_or_default()
}

/// Default log dirs, mirroring `athenaeum_core::logging::resolve_log_dir`'s
/// precedence: `ATHENAEUM_LOG_DIR` > `ATHENAEUM_DB_PATH`'s parent + `logs/`
/// > BOTH platform app-data dirs (production and `.dev`) + `logs/`.
pub fn default_log_dirs() -> Vec<PathBuf> {
    if let Ok(dir) = std::env::var("ATHENAEUM_LOG_DIR") {
        return vec![PathBuf::from(dir)];
    }
    if let Ok(db_path) = std::env::var("ATHENAEUM_DB_PATH") {
        let parent = PathBuf::from(&db_path)
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        return vec![parent.join("logs")];
    }
    resolve_app_data_dirs()
        .into_iter()
        .map(|d| d.join("logs"))
        .collect()
}
```

- [ ] **Step 4: Convert `rpc.rs` and `main.rs`**

`rpc.rs`: `pub fn handle(req: &Request, log_dirs: &[PathBuf]) -> Option<Response>` and `fn dispatch_tool_call(params: &Value, log_dirs: &[PathBuf]) -> Result<Value>`; pass `log_dirs` to the four `query::` calls (lines 233, 242, 252, 265). Fix the import: `use std::path::PathBuf;` (drop `Path` if now unused).

`main.rs` — replace the `log_dir` resolution block and the `handle` call:

```rust
    let log_dirs: Vec<PathBuf> = match std::env::args().nth(1) {
        Some(dir) => vec![PathBuf::from(dir)],
        None => query::default_log_dirs(),
    };
    assert!(
        !log_dirs.is_empty(),
        "log dirs: pass one as argv[1] or have the platform app-data root resolvable"
    );
```

```rust
        if let Some(resp) = rpc::handle(&req, &log_dirs) {
```

Update the module doc header sentence: `The log directory is argv[1] if given, else BOTH build flavors' dirs (production + .dev) are scanned (see query::default_log_dirs).`

- [ ] **Step 5: Update existing fixture tests**

In `crates/log-mcp/tests/query_fixture.rs` add a helper near the top and convert the 8 existing call sites mechanically:

```rust
fn dirs(dir: &tempfile::TempDir) -> Vec<PathBuf> {
    vec![dir.path().to_path_buf()]
}
```

`query::scan(dir.path(), &f)` → `query::scan(&dirs(&dir), &f)` — same pattern for `tail`, `list_operations`, `get_operation` (lines 43, 60, 80, 88, 101, 120, 140, 175; local tempdir variable names may differ per test — keep each test's own name).

- [ ] **Step 6: Run the full log-mcp test suite**

Run: `cargo test -p log-mcp`
Expected: PASS, including `scan_merges_files_across_multiple_dirs_and_skips_missing`

- [ ] **Step 7: Commit**

```bash
git add crates/log-mcp/src crates/log-mcp/tests
git commit -m "feat(log-mcp): scan both prod and .dev app-data log dirs"
```

---

### Task 4: `dev:db-refresh` snapshot script

**Files:**
- Create: `scripts/dev-db-refresh.mjs`
- Modify: `package.json` (scripts block, after `"dev:web"`)
- Modify: `docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md` (§5 step 2 — see Step 3 below)

**Interfaces:**
- Consumes: the `.dev` naming convention from Task 1 (duplicated literal — the script is a Node dev tool, it cannot import Rust).
- Produces: `npm run dev:db-refresh`.

- [ ] **Step 1: Write `scripts/dev-db-refresh.mjs`**

```js
#!/usr/bin/env node
// Refresh the dev catalog from the production one:
//   npm run dev:db-refresh
//
// Copies <prod>/athenaeum.db into the .dev sibling app-data dir via
// `sqlite3 .backup` (WAL-safe even while the production app runs), wipes the
// identity-bound transfer tables in the copy (the dev tree has its own sync
// identity — inherited outbound rows would be resurrected at startup pointing
// at payload dirs that don't exist there), and symlinks the multi-GB
// catalogs/ dir instead of duplicating it. Never touches sync/, account/,
// or logs/. Requires the sqlite3 CLI (ships with macOS; `apt install sqlite3`
// on Debian).
// Spec: docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const IDENT = 'com.vsharifov.athenaeum';

// The same 8 tables the batch-model upgrade reset wipes (see schema.rs test
// `batch_upgrade_wipes_transfer_tables_once_and_spares_catalog`), children
// before parents for FK safety.
const TRANSFER_TABLES = [
  'sync_outbound_files',
  'sync_inbound_files',
  'sync_events',
  'sync_receipts',
  'sync_sources',
  'sync_history',
  'sync_outbound',
  'sync_inbound',
];

function appDataRoot() {
  if (process.platform === 'darwin') return path.join(os.homedir(), 'Library', 'Application Support');
  if (process.platform === 'win32') return process.env.APPDATA;
  return process.env.XDG_DATA_HOME || path.join(os.homedir(), '.local', 'share');
}

function sqlite3(...args) {
  const r = spawnSync('sqlite3', args, { encoding: 'utf8' });
  if (r.error?.code === 'ENOENT') {
    console.error('sqlite3 CLI not found — install it (ships with macOS; `apt install sqlite3` on Debian) and re-run.');
    process.exit(1);
  }
  if (r.status !== 0) {
    console.error(`sqlite3 failed (args: ${args.join(' ')}):\n${r.stderr}`);
    process.exit(1);
  }
  return r.stdout;
}

const root = appDataRoot();
const prodDir = path.join(root, IDENT);
const devDir = path.join(root, `${IDENT}.dev`);
const prodDb = path.join(prodDir, 'athenaeum.db');
const devDb = path.join(devDir, 'athenaeum.db');

if (!fs.existsSync(prodDb)) {
  console.error(`no production DB at ${prodDb} — nothing to snapshot`);
  process.exit(1);
}
fs.mkdirSync(devDir, { recursive: true });

// A stale WAL/SHM pair from a previous dev run must not shadow the fresh copy.
for (const suffix of ['', '-wal', '-shm']) fs.rmSync(devDb + suffix, { force: true });

sqlite3(prodDb, `.backup "${devDb}"`);
console.log(`snapshot: ${prodDb} -> ${devDb}`);

const wipe = ['BEGIN;', ...TRANSFER_TABLES.map((t) => `DELETE FROM ${t};`), 'COMMIT;'].join(' ');
sqlite3(devDb, wipe);
console.log(`wiped transfer state: ${TRANSFER_TABLES.join(', ')}`);

const prodCatalogs = path.join(prodDir, 'catalogs');
const devCatalogs = path.join(devDir, 'catalogs');
if (!fs.existsSync(prodCatalogs)) {
  console.log('no prod catalogs/ dir — skipping link');
} else if (fs.lstatSync(devCatalogs, { throwIfNoEntry: false })) {
  console.log('dev catalogs/ already present — leaving as is');
} else if (process.platform === 'win32') {
  console.warn('catalogs symlink skipped on Windows — copy the dir manually if plate-solving is needed in dev');
} else {
  fs.symlinkSync(prodCatalogs, devCatalogs);
  console.log(`linked catalogs: ${devCatalogs} -> ${prodCatalogs}`);
}
console.log('done — dev catalog refreshed (fresh sync identity, signed-out account)');
```

- [ ] **Step 2: Add the npm script**

In `package.json` scripts, after `"dev:web"`:

```json
    "dev:db-refresh": "node scripts/dev-db-refresh.mjs",
```

- [ ] **Step 3: Spec touch-up (deviation made explicit)**

The spec's §5 step 2 promised a no-CLI file-copy fallback; the mandatory table wipe (step 3) needs the sqlite3 CLI anyway, so the fallback is dropped. In `docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md` replace the step-2 sentence

> Copy `athenaeum.db` — prefer `sqlite3 <prod> ".backup <dev>"` (WAL-safe); if the `sqlite3` CLI is unavailable, fall back to copying `athenaeum.db` + `-wal` + `-shm` and print a warning to close the production app first.

with:

> Copy `athenaeum.db` via `sqlite3 <prod> ".backup <dev>"` (WAL-safe). The sqlite3 CLI is required — the transfer-table wipe (next step) needs it regardless, so there is no CLI-less fallback; the script exits with an instructive error when it is missing.

- [ ] **Step 4: Run it live and verify**

Run: `npm run dev:db-refresh`
Expected: the four progress lines, exit 0.

Then verify the copy is real and clean:

```bash
DEV_DB="$HOME/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db"
sqlite3 "$DEV_DB" "SELECT COUNT(*) FROM files; SELECT COUNT(*) FROM sync_outbound; SELECT COUNT(*) FROM sync_history;"
ls -la "$HOME/Library/Application Support/com.vsharifov.athenaeum.dev/"
```

Expected: `files` count > 0 (catalog survived), both sync counts = 0 (wipe ran), `catalogs` is a symlink to the prod dir, and NO `sync/` / `account/` entries.

Re-run `npm run dev:db-refresh` once more — expected: still exit 0, `dev catalogs/ already present — leaving as is` (idempotent).

- [ ] **Step 5: Commit**

```bash
git add scripts/dev-db-refresh.mjs package.json docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md
git commit -m "feat(scripts): dev:db-refresh — snapshot prod catalog into the .dev tree"
```

---

### Task 5: Full gates + live verification

**Files:** none new — verification only (fix-forward anything the gates flag, amend into the responsible commit or a small `fix:` commit).

- [ ] **Step 1: Rust gates**

Run: `cargo build --workspace`
Expected: clean build.

Run: `cargo test -p athenaeum-core && cargo test -p athenaeum-tauri --lib && cargo test -p log-mcp`
Expected: all PASS.

- [ ] **Step 2: Frontend gate**

Run: `npx tsc --noEmit`
Expected: clean (no frontend files were touched; this guards against accidental drift).

- [ ] **Step 3: log-mcp live check against the real dirs**

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tail_logs","arguments":{"n":3}}}' | cargo run -q -p log-mcp
```

Expected: a JSON-RPC response with up to 3 events (from the prod tree; the `.dev/logs` dir may not exist yet — must NOT error).

- [ ] **Step 4: Owner smoke handoff (manual, GUI)**

Not executable headless — listed for the owner:
1. `npm run tauri dev` → Settings/About: DB path shows `…/com.vsharifov.athenaeum.dev/athenaeum.db`; the snapshot catalog is visible; app is signed out (fresh device).
2. The installed production app still opens its own catalog untouched.
3. In a dev session, `athenaeum-logs` MCP shows events from BOTH the prod app and the dev run.
4. `ATHENAEUM_APP_DATA_DIR="$HOME/Library/Application Support/com.vsharifov.athenaeum" npm run tauri dev` → dev build opens the PROD catalog (escape hatch works). Use deliberately.

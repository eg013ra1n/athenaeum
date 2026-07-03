# Logging Overhaul Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace all ad-hoc printing with one leveled, structured `tracing` pipeline (JSONL files + live level control + MCP query tool) per `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`.

**Architecture:** `tracing` facade everywhere; subscriber (JSONL rolling file + console + reloadable `EnvFilter`) built once in `athenaeum_core::logging` and initialized by both hosts. Commands/routes get `#[tracing::instrument(skip_all, err)]`; span-close events (`FmtSpan::CLOSE`) provide per-command duration/outcome. Print sites are swept per module against a fixed rubric. MCP crate reads the JSONL dir.

**Tech Stack:** `tracing 0.1`, `tracing-subscriber 0.3` (features `env-filter`, `json`), `tracing-appender 0.2`; serde_json (already present); hand-rolled stdio JSON-RPC for the MCP crate (no SDK dependency).

## Global Constraints

- Spec: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` — binding for schema, levels, cleanup.
- **Two backends in sync**: every Tauri command change mirrors in `crates/athenaeum-web/src/routes/` in the same task.
- **Canonical field dictionary (verbatim, snake_case only):** `frame_id`, `file_id`, `frame_set_id`, `set_id`, `root_id`, `operation_id`, `command`, `path`, `src`, `dest`, `duration_ms`, `count`, `error`, `outcome`, `stage`. New names require a spec PR — never invent inline.
- **Message style rule:** message = short stable phrase; ALL data in fields. `info!(root_id, new = 12, "scan finished")`, never `info!("scan finished — 12 new")`.
- **Level rubric (every sweep task applies this decision tree to every site):**
  1. Reports a failed operation the user will notice → `error!` (include `error` field).
  2. Fallback/assumption/recovery taken → `warn!`.
  3. Operation lifecycle (start/finish/outcome, counts, durations) → `info!`.
  4. Stage-level internals (per-file decision, per-set score, stage timing) → `debug!`.
  5. Per-item math (per star/quad/inlier) → `trace!`.
  6. Progress output superseded by `ProgressEmitter`, or debugging leftovers with no diagnostic value → **delete**.
- **Zero-print gate (final):** `println!`/`eprintln!` = 0 in production code of all 5 codebases. Exempt: `#[cfg(test)]`/`tests/`/`benches/`/`examples/`/`build.rs`, and solvemyastro CLI binaries' intentional stdout (exact whitelist in Task 8).
- **`trace` is env-only** — never exposed in the Settings UI.
- Default filter when nothing configured: `info`. `ATHENAEUM_LOG` (EnvFilter syntax) overrides settings entirely.
- Logging must never panic or abort the app; all its own failures degrade with at most one `warn`/stderr line.
- Submodules get the `tracing` facade only (no subscriber in library code).
- `corpus_bench` precision+speed gate must pass after submodule tasks (run QUIET; `cone_calls` deterministic).
- Version branch per house rule (e.g. `0.2.3` or next); submodule work on `logging` branches + one bump commit each.
- Markdown tables in reports/docs: spaces around dashes (`| ---- |`).

## File Structure (locked decomposition)

- `crates/athenaeum-core/src/logging/mod.rs` — public API: `init`, `LoggingHandle`, `apply_config`, `get_path`. (Directory replaces `logging.rs`.)
- `crates/athenaeum-core/src/logging/config.rs` — `LoggingConfig` (serde) + directive building + module-key mapping.
- `crates/athenaeum-core/src/logging/panic_hook.rs` — panic hook + `crash.log` (moved, behavior-preserved).
- `crates/log-mcp/src/main.rs` (+ `rpc.rs`, `query.rs`) — MCP server.
- `src/components/settings/LoggingSettings.tsx` + `src/types/models.ts` addition — UI.
- Everything else is modification-in-place of existing modules.

---

### Task 1: Core logging infrastructure (subscriber, files, reload, config)

**Files:**
- Delete: `crates/athenaeum-core/src/logging.rs`
- Create: `crates/athenaeum-core/src/logging/mod.rs`, `crates/athenaeum-core/src/logging/config.rs`, `crates/athenaeum-core/src/logging/panic_hook.rs`
- Modify: `crates/athenaeum-core/Cargo.toml` (add the three tracing crates)
- Test: unit tests inside `logging/config.rs` + `logging/mod.rs`

**Interfaces:**
- Produces: `logging::init(process: Process) -> LoggingHandle`; `enum Process { Desktop, Web }`; `LoggingHandle::apply_config(&self, cfg: &LoggingConfig)`; `LoggingHandle::env_override_active(&self) -> bool`; `logging::get_path() -> Option<PathBuf>` (current log file); `LoggingConfig { level: String, modules: BTreeMap<String, String> }` with `serde` and `Default` (`level: "info"`, empty modules).
- Consumes: nothing (foundation).

- [ ] **Step 1: Add dependencies**

```toml
# crates/athenaeum-core/Cargo.toml [dependencies]
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
tracing-appender = "0.2"
```

- [ ] **Step 2: Write failing unit tests for `LoggingConfig` directive building** (in `logging/config.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_config_is_info() {
        assert_eq!(LoggingConfig::default().to_directives(), "info");
    }
    #[test]
    fn module_overrides_map_to_targets() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "warn".into();
        cfg.modules.insert("scanner".into(), "debug".into());
        cfg.modules.insert("solver".into(), "debug".into());
        // solver expands to BOTH the core plate_solve target and the solvemyastro crate
        assert_eq!(
            cfg.to_directives(),
            "warn,athenaeum_core::scanner=debug,athenaeum_core::plate_solve=debug,solvemyastro=debug"
        );
    }
    #[test]
    fn unknown_module_key_is_skipped_not_fatal() {
        let mut cfg = LoggingConfig::default();
        cfg.modules.insert("bogus".into(), "debug".into());
        assert_eq!(cfg.to_directives(), "info");
    }
    #[test]
    fn invalid_level_falls_back_to_info() {
        let mut cfg = LoggingConfig::default();
        cfg.level = "chatty".into();
        assert_eq!(cfg.to_directives(), "info");
    }
}
```

- [ ] **Step 3: Implement `config.rs`**

```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SETTINGS_KEY: &str = "logging.config";
const LEVELS: [&str; 4] = ["error", "warn", "info", "debug"]; // trace is env-only by spec

/// UI module key -> tracing filter targets.
const MODULE_TARGETS: [(&str, &[&str]); 4] = [
    ("scanner", &["athenaeum_core::scanner"]),
    ("solver", &["athenaeum_core::plate_solve", "solvemyastro"]),
    ("calibration", &["athenaeum_core::calibration"]),
    ("archive", &["athenaeum_core::archive", "athenaeum_core::file_op"]),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LoggingConfig {
    pub level: String,
    pub modules: BTreeMap<String, String>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self { level: "info".into(), modules: BTreeMap::new() }
    }
}

impl LoggingConfig {
    pub fn to_directives(&self) -> String {
        let base = if LEVELS.contains(&self.level.as_str()) { self.level.as_str() } else { "info" };
        let mut out = base.to_string();
        for (key, level) in &self.modules {
            if !LEVELS.contains(&level.as_str()) { continue; }
            if let Some((_, targets)) = MODULE_TARGETS.iter().find(|(k, _)| k == key) {
                for t in *targets {
                    out.push_str(&format!(",{t}={level}"));
                }
            }
        }
        out
    }
}
```

- [ ] **Step 4: Run the tests — expect the 4 new tests green** (`cargo test -p athenaeum-core logging::`)

- [ ] **Step 5: Implement `mod.rs`** — subscriber init. Complete code:

```rust
pub mod config;
mod panic_hook;
pub use config::LoggingConfig;

use std::path::PathBuf;
use std::sync::OnceLock;
use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    reload,
    util::SubscriberInitExt,
    EnvFilter, Registry,
};

#[derive(Clone, Copy)]
pub enum Process { Desktop, Web }

impl Process {
    fn prefix(self) -> &'static str {
        match self { Process::Desktop => "athenaeum-desktop", Process::Web => "athenaeum-web" }
    }
}

pub struct LoggingHandle {
    reload: reload::Handle<EnvFilter, Registry>,
    env_override: bool,
    // keep the appender guard alive for the process lifetime
    _guard: tracing_appender::non_blocking::WorkerGuard,
}

static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Resolve app-data dir (same env-based logic as before — moved verbatim
/// from the old logging.rs `resolve_app_data_dir`), then `logs/` under it.
/// Web/Docker: `ATHENAEUM_DB_PATH`'s parent joined with `logs/` wins when set.
fn resolve_log_dir() -> Option<PathBuf> { /* moved code + ATHENAEUM_DB_PATH branch */ }

pub fn init(process: Process) -> Option<LoggingHandle> {
    let dir = resolve_log_dir()?;
    let _ = std::fs::create_dir_all(&dir);
    // Legacy cleanup (spec: "Legacy cleanup"): best-effort delete of the old files
    for old in ["athenaeum.log", "athenaeum.log.1"] {
        let _ = std::fs::remove_file(dir.parent().unwrap_or(&dir).join(old));
    }

    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(process.prefix())
        .filename_suffix("jsonl")
        .max_log_files(14)
        .build(&dir)
        .ok()?;
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_override = std::env::var("ATHENAEUM_LOG").is_ok();
    let initial = if env_override {
        EnvFilter::try_from_env("ATHENAEUM_LOG").unwrap_or_else(|_| EnvFilter::new("info"))
    } else {
        EnvFilter::new(LoggingConfig::default().to_directives())
    };
    let (filter, handle) = reload::Layer::new(initial);

    let file_layer = fmt::layer()
        .json()
        .with_span_events(FmtSpan::CLOSE)
        .with_writer(non_blocking);
    let console_layer = match process {
        // human-pretty on stderr for desktop terminal launches
        Process::Desktop => fmt::layer().with_span_events(FmtSpan::CLOSE).with_writer(std::io::stderr).boxed(),
        // container convention: JSON to stdout
        Process::Web => fmt::layer().json().with_span_events(FmtSpan::CLOSE).with_writer(std::io::stdout).boxed(),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(console_layer)
        .try_init()
        .ok()?;

    let _ = LOG_DIR.set(dir);
    panic_hook::install(); // preserved crash.log behavior + emits tracing::error!
    Some(LoggingHandle { reload: handle, env_override, _guard: guard })
}

impl LoggingHandle {
    pub fn env_override_active(&self) -> bool { self.env_override }
    /// Live-apply a config. No-op (with one warn) while ATHENAEUM_LOG is set.
    pub fn apply_config(&self, cfg: &LoggingConfig) {
        if self.env_override {
            tracing::warn!("logging config change ignored: ATHENAEUM_LOG override active");
            return;
        }
        match cfg.to_directives().parse::<EnvFilter>() {
            Ok(f) => { let _ = self.reload.reload(f); tracing::info!(directives = %cfg.to_directives(), "logging filter applied"); }
            Err(error) => tracing::warn!(%error, "invalid logging directives; keeping previous filter"),
        }
    }
}

pub fn get_path() -> Option<PathBuf> {
    LOG_DIR.get().cloned() // directory; current file is <prefix>.<date>.jsonl inside it
}
```

`panic_hook.rs`: move the existing hook body verbatim, plus `tracing::error!(%info, "panic")` before the crash.log write. Note `init` returns `Option` — a `None` leaves the app running unlogged (spec: never take the app down).

- [ ] **Step 6: Integration test — JSONL line parses** (in `logging/mod.rs` tests): init with `Process::Web` pointed at a `tempfile::TempDir` via `ATHENEUM`-independent injection: refactor `resolve_log_dir` to check a test-only `ATHENAEUM_LOG_DIR` env var first (also useful for Docker). Emit `tracing::info!(count = 3, "test event")`, flush (drop handle), read the file, `serde_json::from_str` each line, assert one object has `"fields":{"count":3,...}` and `"level":"INFO"`.

- [ ] **Step 7: Build + full core suite** — `cargo build -p athenaeum-core && cargo test -p athenaeum-core` green. Old `logging::log(...)` call sites will fail to compile — fix them NOW by converting each to the rubric-appropriate `tracing` macro (there are few; enumerate with `rg -n 'logging::log' crates/`).

- [ ] **Step 8: Commit** — `feat(logging): tracing subscriber, rolling JSONL, reloadable filter, config model`

### Task 2: Host wiring (both backends) + log-path command + uninstall scripts

**Files:**
- Modify: `crates/athenaeum-tauri/src/lib.rs:45` (init call), the `initialize_database` command in `crates/athenaeum-tauri/src/commands/core.rs` (apply stored config once DB is up), `crates/athenaeum-web/src/main.rs:78` (init + apply after DB open), `crates/athenaeum-tauri/src/commands/files.rs:284` (`get_log_path` → new dir), `crates/athenaeum-tauri/scripts/uninstall-macos.sh`, `crates/athenaeum-tauri/scripts/uninstall-linux.sh` (add `logs/`)
- Test: manual smoke + existing suites

**Interfaces:**
- Consumes: `logging::init(Process)`, `LoggingHandle`, `LoggingConfig`, `config::SETTINGS_KEY` from Task 1.
- Produces: `LoggingHandle` stored in each backend's state (`AppState.logging: Arc<LoggingHandle>` — tauri managed state; web `AppState` field), consumed by Task 3.

- [ ] **Step 1:** Tauri: replace `logging::init();` with `let logging = logging::init(logging::Process::Desktop);` and manage `Arc<Option<LoggingHandle>>` in state. In `initialize_database` (DB init is frontend-driven — house memory), after the DB opens: read `logging.config` via `get_setting`, `serde_json::from_str::<LoggingConfig>`, `handle.apply_config(&cfg)` (default on parse failure with a `warn!`).
- [ ] **Step 2:** Web: same at `main.rs` — init `Process::Web` before config parsing, apply stored config right after the DB is opened.
- [ ] **Step 3:** `get_log_path` returns `logging::get_path()` (directory) — keep the command name; adjust the one frontend usage (`rg -n 'get_log_path' src/`) to show/open the directory.
- [ ] **Step 4:** Add `"$APP_DATA/logs"` removal to both uninstall scripts (house rule).
- [ ] **Step 5:** Smoke: `cargo run -p athenaeum-web` with `ATHENAEUM_LOG=debug` → JSONL file appears under the DB dir's `logs/`, stdout shows JSON events; then without the env var → level is info. Desktop: `npm run tauri dev`, initialize DB, confirm `athenaeum-desktop.*.jsonl` created.
- [ ] **Step 6:** Full gate: 3-crate build + `cargo test -p athenaeum-core -p athenaeum-tauri -p athenaeum-web`. Commit — `feat(logging): host wiring, stored-config apply, log dir command, uninstall entries`.

### Task 3: `get_logging_config` / `set_logging_config` commands (both backends)

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/settings.rs` (+ register in `lib.rs` `invoke_handler`), `crates/athenaeum-web/src/routes/settings.rs` (+ `routes/mod.rs`), `src/types/models.ts`
- Test: web handler tests (follow `routes/settings.rs` existing test style if present; else `crates/athenaeum-web/src/main.rs` test mod pattern from T13/T14)

**Interfaces:**
- Consumes: Task 2's state-held `LoggingHandle`.
- Produces: commands `get_logging_config() -> LoggingConfig` (stored or default; plus `envOverrideActive: bool` in a wrapper `LoggingConfigResponse`) and `set_logging_config(config: LoggingConfig) -> ()` (validates by `to_directives().parse::<EnvFilter>()`, persists JSON under `logging.config` via `set_setting` plumbing, calls `apply_config`). TS: `interface LoggingConfig { level: string; modules: Record<string, string>; }`, `interface LoggingConfigResponse { config: LoggingConfig; envOverrideActive: boolean; }` in `src/types/models.ts`.

- [ ] **Step 1:** Write the two web handler tests first: GET returns default config when unset; POST with `{"level":"debug","modules":{"scanner":"debug"}}` → 200 and subsequent GET returns it; POST with `{"level":"chatty"}` → 400.
- [ ] **Step 2:** Implement both backends (serde `rename_all = "camelCase"` — verify TS parity per house rule). Reject invalid configs with a `String` error (tauri) / 400 (web).
- [ ] **Step 3:** Run web tests + 3-crate build; register both commands; commit — `feat(logging): get/set logging config commands, live filter apply`.

### Task 4: Command-boundary instrumentation (all commands + routes)

**Files:**
- Modify: all 16 modules under `crates/athenaeum-tauri/src/commands/` and their mirrors under `crates/athenaeum-web/src/routes/`
- Test: one integration assertion + compile gate

**Interfaces:** none new — one attribute per fn.

- [ ] **Step 1:** Enumerate: `rg -n '#\[tauri::command' crates/athenaeum-tauri/src/commands/` (157 fns) and `rg -n 'pub async fn' crates/athenaeum-web/src/routes/` (156 fns).
- [ ] **Step 2:** Add to every command fn, directly under the `#[tauri::command…]` attribute:

```rust
#[tracing::instrument(skip_all, err)]
```

Same attribute on every web route handler fn. `skip_all` prevents argument capture (paths/keys may be large or sensitive); `err` emits the `Err` value at error level inside the span — this is the structural never-swallow. Span-close events (already configured, `FmtSpan::CLOSE`) carry `time.busy`/`time.idle` = the duration record. Do NOT add per-fn `fields(...)` — the fn name is the span name and equals the command name.
- [ ] **Step 3:** Exception check: `get_setting`/`set_setting` fire constantly from the UI — instrument them at `level = "debug"` (`#[tracing::instrument(skip_all, err, level = "debug")]`) so info logs aren't spammed. Apply the same to any command the enumerate step shows is called per-frame in a hot UI loop (candidates: preview/cache image fetch commands in `commands/cache.rs`-adjacent modules — decide per rubric, record in report).
- [ ] **Step 4:** Verify: run web server, hit `get_scan_roots` once, confirm the JSONL contains a span-close event with `"span":{"name":"get_scan_roots"}` and an error event for a forced-fail call (e.g. `relink_scan_root` with unknown id → the `err` event).
- [ ] **Step 5:** 3-crate build + suites + `tsc --noEmit` (no TS change expected) → commit — `feat(logging): instrument all command/route boundaries (span + err)`.

### Tasks 5–7: The core print-site sweep (three tasks, module-grouped)

Shared procedure (each task applies it to its module list; the **level rubric and field dictionary live in Global Constraints**):

1. Enumerate exactly: `rg -n 'println!|eprintln!' crates/athenaeum-core/src/<module>/` — the count at plan time is noted per task below; re-run at execution (drift from other work is fine, sweep whatever exists).
2. For each site, write one row into the task report table: `file:line | old text (truncated) | disposition (level+fields | delete) | rationale (rubric rule #)`.
3. Transform per the message style rule — data into canonical fields, message to a stable phrase. Example transformation (real site pattern from scanner):

```rust
// BEFORE
eprintln!("[scanner] Failed to update file {}: {}", path.display(), e);
// AFTER
tracing::error!(path = %path.display(), error = %e, "file update failed");
```

```rust
// BEFORE
println!("Scan complete: {} files, {} new", total, new_count);
// AFTER (lifecycle → info, counts as fields)
tracing::info!(count = total, new = new_count, "scan finished");
```

4. Wrap long-running entry points in this module group with operation spans where missing:

```rust
let span = tracing::info_span!("scan", root_id, operation_id = %op_id);
let _g = span.enter(); // or .instrument(span) for async
```

5. Gate: `rg -n 'println!|eprintln!' crates/athenaeum-core/src/<module>/ --glob '!*test*'` → 0 (test code exempt); `cargo test -p athenaeum-core` green; commit per task.

**Task 5 — scanner + fits_parser** (plan-time counts: scanner/mod.rs 20 + rest of scanner dir; fits_parser 16+; run enumerate for exact list). Required domain fields: scan events per catalog (`root_id`, `path`, counts). Operation span: `run_registered_scan` entry.
**Task 6 — db, calibration, clustering, duplicates, relinking (9), sessions (3), fingerprint (1), auto_merge, coordinates, settings, monitor.** Domain fields: db-maintenance (table, `count`, `duration_ms`); calibration (`set_id`, `frame_set_id`, `score`).
**Task 7 — archive, file_op, export, analysis, plate_solve, catalog, cache, services, rustafits_processor + anything the final enumerate still shows.** Domain fields: file-op/archive (`operation_id`, `src`, `dest`, `stage`); solve (`frame_id`, `stage`, `outcome`). Operation spans: archive `run_operation`, file_op executor entry, plate-solve queue item, export run.

Each of Tasks 5–7 ends: enumerate-gate 0 for its modules, suite green, commit `refactor(logging): sweep <modules> to tracing (N sites: E error / W warn / I info / D debug / X deleted)`.

### Task 8: solvemyastro — facade + sweep + math instrumentation

**Files:**
- Modify: `solvemyastro/Cargo.toml` (add `tracing = "0.1"`), library sources per enumerate, superproject `Cargo.lock` + submodule pointer (bump commit)
- Test: `cargo test` in submodule + `corpus_bench` gate

**Interfaces:** none new to athenaeum — events flow through the host subscriber automatically (workspace path dep).

- [ ] **Step 1:** Branch `logging` in the submodule. Enumerate prints: `rg -n 'println!|eprintln!' solvemyastro/src/`. **CLI stdout whitelist:** binaries under `src/bin/` and `examples/` keep user-facing `println!` — list the kept files explicitly in the report; library code goes to 0.
- [ ] **Step 2:** Sweep library sites per the Global rubric. Math instrumentation minimum (spec "solve" domain): solve pipeline stages (`select`, quad build, match, refine, verify) each get a `debug!` stage-timing event with `stage`, `duration_ms`, counts; per-quad/per-star detail → `trace!` with coarse-grained summaries preferred (rubric 5). The orchestrator's existing per-cell diagnostics (`CellTrace`) stay untouched.
- [ ] **Step 3:** Gates in submodule: `cargo test` green; `corpus_bench` QUIET run — precision unchanged (truth counts identical), `cone_calls` identical, wall-clock within noise. Library print gate: `rg -n 'println!|eprintln!' src/ --glob '!bin/**' --glob '!*test*'` → 0.
- [ ] **Step 4:** Commit in submodule; bump pointer + lockfile in superproject; workspace build green. Commit — `chore(solvemyastro): bump — tracing facade + leveled solve logging`.

### Task 9: rustafits — facade + sweep

Same procedure as Task 8 for `rustafits/` (branch `logging`, facade dep, library sweep per rubric, analysis/detection stage `debug!` timings, per-star `trace!`). Known pre-existing `fast_detect_real` failure is NOT a gate (deferred S7 — house memory). Gates: suite green minus S7, print gate 0 for `src/`, bump + lockfile + workspace build. Commit.

### Task 10: Settings UI — Logging section

**Files:**
- Create: `src/components/settings/LoggingSettings.tsx`
- Modify: the Settings page registry (locate via `rg -n 'CalibrationMatching' src/pages src/components/settings` and follow the existing section pattern), `src/types/models.ts` (done in Task 3 — verify)
- Test: `tsc --noEmit` + manual smoke

**Interfaces:**
- Consumes: `api.invoke('get_logging_config')` → `LoggingConfigResponse`; `api.invoke('set_logging_config', { config })`.

- [ ] **Step 1:** Component: global level select (`error|warn|info|debug` — NO trace, spec), four module-override selects (`scanner`, `solver`, `calibration`, `archive` — each `inherit|error|warn|info|debug`; `inherit` = absent from `modules`), Save button using the existing settings-section save pattern, and a banner when `envOverrideActive`: "Log level is overridden by ATHENAEUM_LOG on this server — UI changes are saved but inactive." Design tokens only (`bg-surface`, `text-content-muted`, …); notify() on save success/failure per notification rules.
- [ ] **Step 2:** Wire into the Settings page beside Calibration Matching. `tsc --noEmit` + `npm run build:web` green.
- [ ] **Step 3:** Smoke in web dev: flip scanner→debug, run a scan, confirm per-file debug events appear in the JSONL without restart; flip back. Commit — `feat(settings-ui): logging level section with live apply`.

### Task 11: Legacy cleanup verification + docs

**Files:**
- Modify: `CLAUDE.md` (Module Map `logging` entry + a short Logging section: levels, field dictionary pointer, ATHENAEUM_LOG, log dir), `docs/` if export README-style logging README desired (NO — YAGNI, spec has it)
- Test: grep gates

- [ ] **Step 1:** Zero-print gate across all five codebases with the documented exemptions:

```bash
rg -n 'println!|eprintln!' crates/*/src --glob '!*test*'                       # expect 0
rg -n 'println!|eprintln!' solvemyastro/src --glob '!bin/**' --glob '!*test*'  # expect 0
rg -n 'println!|eprintln!' rustafits/src --glob '!*test*'                      # expect 0
rg -n 'logging::log\(' crates/                                                  # expect 0 (old API dead)
```
- [ ] **Step 2:** Confirm first-run cleanup removed old `athenaeum.log*` (manual check on the dev machine's app-data dir).
- [ ] **Step 3:** Update CLAUDE.md; commit — `docs: logging conventions in CLAUDE.md + zero-print gate record`.

### Task 12: `log-mcp` crate

**Files:**
- Create: `crates/log-mcp/Cargo.toml`, `crates/log-mcp/src/main.rs`, `crates/log-mcp/src/rpc.rs`, `crates/log-mcp/src/query.rs`, `.mcp.json` (repo root)
- Modify: workspace `Cargo.toml` members
- Test: `crates/log-mcp/tests/query_fixture.rs` against a fixture log dir

**Interfaces:**
- Consumes: the JSONL schema (envelope from tracing-subscriber's json layer: `timestamp`, `level`, `target`, `fields{message,…}`, `span{name,…}`, `spans[…]`).
- Produces: MCP stdio server exposing `query_logs`, `tail_logs`, `list_operations`, `get_operation`.

- [ ] **Step 1:** Crate skeleton — deps only `serde`, `serde_json`, `anyhow`. `main.rs`: stdio JSON-RPC loop:

```rust
// Reads Content-Length-free line-delimited JSON-RPC (MCP stdio framing: one JSON object per line).
// Handles: initialize (returns protocolVersion "2024-11-05", capabilities.tools),
// notifications/initialized (ignore), tools/list, tools/call. Everything else -> method_not_found.
fn main() -> anyhow::Result<()> {
    let log_dir = std::env::args().nth(1)
        .map(Into::into)
        .or_else(query::default_log_dir)   // same app-data resolution as athenaeum-core
        .expect("log dir: pass as arg or have the app-data dir present");
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lines() {
        let req: rpc::Request = match serde_json::from_str(&line?) { Ok(r) => r, Err(_) => continue };
        if let Some(resp) = rpc::handle(&req, &log_dir) {
            serde_json::to_writer(&mut out, &resp)?;
            use std::io::Write; writeln!(out)?; out.flush()?;
        }
    }
    Ok(())
}
```

`rpc.rs`: request/response serde types + `tools/list` returning the 4 tool JSON Schemas + dispatch to `query.rs`.
- [ ] **Step 2:** `query.rs` — the real logic, all tools funnel through one scan:

```rust
pub struct Filter { pub level: Option<String>, pub module: Option<String>,
    pub contains: Option<String>, pub since: Option<String>, pub until: Option<String>,
    pub operation_id: Option<String>, pub limit: usize /* default 200, cap 1000 */ }

/// Stream all *.jsonl files in dir (sorted by name = chronological), parse each
/// line lazily, apply filter, return the last `limit` matches as serde_json::Value.
/// level matches at-or-above severity; module = prefix match on `target`;
/// operation_id matches any span in `spans[]` carrying that field.
pub fn scan(dir: &Path, f: &Filter) -> anyhow::Result<Vec<serde_json::Value>>
```

`tail_logs(n)` = `scan` with empty filter, limit n. `list_operations(kind?, since?)` = scan for span-close events whose `span.name` ∈ operation kinds, project `{kind, operation_id, timestamp, duration}`. `get_operation(id)` = `scan` with `operation_id`.
- [ ] **Step 3:** Fixture test: write 3 hand-authored JSONL lines (one info scan event inside a scan span with `operation_id:"op1"`, one error, one unrelated debug) to a temp dir; assert `query_logs(level=error)` → 1, `get_operation("op1")` → 1, `tail_logs(10)` → 3.
- [ ] **Step 4:** `.mcp.json` at repo root:

```json
{
  "mcpServers": {
    "athenaeum-logs": {
      "command": "cargo",
      "args": ["run", "-q", "-p", "log-mcp"]
    }
  }
}
```
- [ ] **Step 5:** Live check: with a real log dir present, `tools/call query_logs {"level":"info","limit":5}` over stdin returns events. Workspace build + test green (this crate joins `cargo test --workspace` for the athenaeum crates gate). Commit — `feat(log-mcp): stdio MCP server over the JSONL log dir`.

### Task 13: Final gates (exit checklist)

- [ ] `cargo build --workspace` + `cargo test -p athenaeum-core -p athenaeum-tauri -p athenaeum-web -p log-mcp` green (submodule suites per Tasks 8–9 records; S7 exception stands).
- [ ] `corpus_bench` gate unchanged at default level (re-run if any solver-adjacent commit landed after Task 8).
- [ ] Zero-print gates from Task 11 re-run → 0.
- [ ] `tsc --noEmit` + `npm run build:web` green.
- [ ] Live smoke: desktop scan at info → lifecycle lines only; Settings→scanner=debug → per-file lines appear live; `ATHENAEUM_LOG=trace cargo run -p athenaeum-web` → trace events flow; UI shows override banner.
- [ ] Docker: `docker build` succeeds (logs to `/data/logs` + stdout JSON — verify with one container run).
- [ ] Ledger + memory updated; both submodule branches pushed + pointers bumped.

## Self-Review (done at authoring)

1. **Spec coverage:** levels+schema (T1, GC), files/rotation (T1), runtime control (T1–T3, T10), boundary (T4), sweep (T5–T7), submodules (T8–T9), legacy cleanup (T1 step 5, T11), MCP (T12), error-handling/perf/testing (T1 steps 6–7, T8 step 3, T13). Non-goals untouched. ✓
2. **Placeholders:** `resolve_log_dir` marked "moved verbatim + documented branch" — the source exists at `logging.rs:13-33`; acceptable move-instruction, not a TBD. ✓
3. **Type consistency:** `LoggingConfig`/`LoggingHandle`/`SETTINGS_KEY`/`LoggingConfigResponse` names match across T1/T2/T3/T10; `Process::{Desktop,Web}` across T1/T2. ✓

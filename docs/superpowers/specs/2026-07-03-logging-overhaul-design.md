# Logging Overhaul — Design

**Date:** 2026-07-03 · **Status:** approved-pending-review · **Owner decisions baked in:** scope = backend + submodules (frontend console out of scope); level control = Settings UI + env override; approach = `tracing` backbone + structured JSONL files + MCP query server (cut-able last phase).

## Problem

Athenaeum has no real logging. The de-facto mechanism is ~516 `println!`/`eprintln!` sites across the backend (scanner and fits_parser densest), which vanish in a normal desktop launch; the hand-rolled `logging.rs` (single 5 MB file, no levels, no filtering) is called from almost nowhere; the submodules (solvemyastro, rustafits) print to stdout; the frontend logs to a DevTools console nobody has open. Debugging a user report means asking them to relaunch from a terminal. There is no tiering (info/warn/error/debug), no per-module verbosity, no machine-readable format, and no way to pull "everything that happened during solve X".

## Goals

1. Every command and every math-heavy operation logs through one leveled, structured API.
2. Four user-facing tiers — `error` / `warn` / `info` / `debug` — plus `trace` for per-item math detail (env-only).
3. **Unified event style**: same envelope everywhere; per-domain required fields from a canonical catalog; data in fields, never interpolated into message prose.
4. Runtime level control: Settings UI (global + per-module, live, no restart) with `ATHENAEUM_LOG` env override; works in both backends.
5. Logs stored as rotating JSONL files — the single source of truth for dev sessions, Docker, and beta-user support bundles alike.
6. A small MCP server for dev-time querying of those files (filter by level/module/time/operation).
7. Logging can never take the app down or measurably slow disabled-level hot paths.

## Non-goals (explicit)

- Frontend console forwarding into the backend log (own design questions — later cycle).
- Log shipping / telemetry of any kind (nothing leaves the machine).
- Replacing `ProgressEmitter` (progress is UI data, not logs) or the notification system.
- An in-app log-viewer UI.
- Changing `CellTrace`/diag structures in solvemyastro (solve-result data, not logging) — only its stdout prints migrate.

## Architecture

`tracing` is the sole logging API in all five Rust codebases:

- **core / tauri / web**: full `tracing` usage; subscriber initialization lives in a rewritten `athenaeum_core::logging` and is invoked from both hosts' `main`.
- **solvemyastro / rustafits**: the lightweight `tracing` facade only (macros + spans, no subscriber). Standalone binaries/benches in those repos may init a simple fmt subscriber behind their own `main`s; as workspace path-deps their events flow into the host subscriber automatically.

Subscriber stack (both hosts, one code path in core):

1. **JSONL file layer** — one JSON event per line, non-blocking writer (`tracing-appender` dedicated thread), daily rotation with max-files cap (default: 14 files). Location: `<app-data>/logs/` (desktop), `/data/logs/` (Docker).
2. **Console layer** — pretty human format; always on for the web server (Docker convention: JSON to stdout as well), dev-visible for desktop terminal launches.
3. **Reloadable filter** — `EnvFilter` behind a `reload::Layer` handle. Filter directives come from settings (below) or `ATHENAEUM_LOG` when set.

The existing panic hook + `crash.log` behavior is preserved (panic also emitted as an `error` event). The old single-file `athenaeum.log` writer is deleted. `get_log_path` returns the live log directory/current file. Uninstall scripts gain the `logs/` location (house rule).

Multi-process safety: desktop and web pointed at the same app-data dir use per-process file prefixes (`athenaeum-desktop.*`, `athenaeum-web.*`) so rotation never races.

## Levels

| Level | Meaning | Examples |
| ---- | ---- | ---- |
| `error` | Operation failed; user-visible consequence. Always logged; the command boundary guarantees no swallowed Err. | command returned Err; file op failed; restore conflict |
| `warn` | Recoverable / suspicious; a fallback or assumption was taken. | NULL binning treated as 1×1; poisoned lock recovered; retry taken; filter parse error kept previous filter |
| `info` | Operation lifecycle. The level a beta user runs at. | command invoked/completed + duration; scan finished with counts; solve outcome + confidence |
| `debug` | Stage-level internals. | per-file scanner decision; per-set calibration score; solver stage timings |
| `trace` | Per-item math detail. Env-only (not in the Settings UI), off by default, negligible disabled cost. | per-star, per-quad, per-inlier iteration |

## Unified event schema

**Base envelope** (every event; mostly free from the JSONL layer): `timestamp`, `level`, `target` (module path), `message`, span context (operation kind + id + host span fields).

**Canonical field dictionary** — never ad-hoc synonyms. Core names: `frame_id`, `file_id`, `frame_set_id`, `set_id` (calibration set), `root_id`, `operation_id`, `command`, `path`, `src`, `dest`, `duration_ms`, `count`, `error`, `outcome`, `stage`. snake_case only. New fields extend the dictionary in this spec via PR — not invented inline.

**Dictionary extensions (added during implementation, T1):** scan-summary counts `found`, `processed`, `skipped`, `modified` (siblings of the scan-domain counts `seen`/`new`/`updated`/`errors` and the example's `unchanged`; the Task 5 scanner sweep table reconciles final scan-count naming); `monitor_enabled` (bool, scan-root monitor toggle); `addr` (server bind address); `directives` (EnvFilter directive string on logging-filter events); `frame` (string; source frame filename in solver-submodule contexts, which have no DB row id — deliberately distinct from core's numeric `frame_id` so MCP queries never mix the two); T8.

**Message style rule**: the message is a short, stable human phrase; all data lives in fields. `info!(root_id, new = 12, unchanged = 4219, "scan finished")` — never `info!("scan finished — 12 new of 4231")`. This is what makes events aggregatable and MCP-queryable.

**Per-domain required fields** (the catalog; enforced by the sweep tables and code review). "Required" means: present wherever the value exists at that point in the operation — an event before a value is computed omits it; the sweep tables specify exact fields per event:

| Domain | Required fields on every event |
| ---- | ---- |
| command boundary | span name = command name; duration = span-close `time.busy`/`time.idle` (FmtSpan::CLOSE built-ins — as-built, T4); failure = the `err`-emitted error event inside the span. The literal `duration_ms`/`outcome` names apply to hand-written events, not boundary spans. |
| scan | `root_id`, `path` (where per-file), counts (`seen`/`new`/`updated`/`errors` on summary) |
| solve | `frame_id`, `stage`, `scale_arcsec_px`, `inliers`, `rms_px`, `outcome`, confidence fields |
| registration | `frame_set_id`, `frame_id`, `flipped`, tolerance fields, group counts (gate) |
| calibration matching | `set_id`, `frame_set_id`, per-parameter mode, `score` |
| file op / archive | `operation_id`, `src`, `dest`, `strategy`/`stage`, hash-verify outcome |
| db maintenance | affected table, `count`, `duration_ms` |

**Spans**: every long-running operation (scan, solve, archive op, file op, registration run) opens a span named for the operation kind carrying its `operation_id`; all nested events inherit it. This is the correlation mechanism for "give me everything about operation X".

## Runtime level control

- Settings gains a **Logging** section: global level (`error|warn|info|debug`) + per-module overrides for `scanner`, `plate_solve`/solver, `calibration`, `archive`+`file_op`. Stored as one JSON value in the settings table (`logging.config`), mirrored by a TS type; applied live via the reload handle.
- Commands `get_logging_config` / `set_logging_config` in both backends.
- `ATHENAEUM_LOG` (full `EnvFilter` syntax, e.g. `info,athenaeum_core::scanner=trace`) overrides settings entirely while set; the UI shows an "overridden by environment" notice.
- Default when nothing configured: `info`.

## Command-boundary instrumentation

A small wrapper/macro in each backend's shared layer runs every command/route inside a span (`command`, `duration_ms`, `outcome`) and logs boundary errors at `error` level — structurally enforcing never-swallow for all ~157 commands in one place. Applied mechanically across the 16 command modules and their web mirrors.

## The audit sweep

Every existing print site (~516 backend + submodule prints) is individually dispositioned in per-module tables (produced during planning, executed module-by-module): **level + domain + required fields**, or **delete** (dead progress prints superseded by `ProgressEmitter`). `ProgressEmitter` events remain events — "notify on outcomes, log everything" keeps the two systems separate.

## MCP log-query server (last phase; cut-able to a follow-up without redesign)

New workspace crate `crates/log-mcp`: a stdio MCP server over the JSONL directory (path via arg/env). Tools:

- `query_logs(level?, module?, contains?, since?, until?, limit?)`
- `tail_logs(n)`
- `list_operations(kind?, since?)` — from span-open/close events
- `get_operation(operation_id)` — every event under that operation across modules

Registered in the repo's `.mcp.json` for dev sessions. Reads the same files a beta user zips up — one code path to understand any log.

## Legacy cleanup (explicit)

The old logging style is removed entirely, not left beside the new one:

- `logging.rs`'s single-file writer and its `log(level, msg)` free-function API are deleted in the rewrite; no call sites may remain.
- First init of the new subscriber deletes the obsolete `<app-data>/athenaeum.log` and `athenaeum.log.1` files (crash.log stays — same mechanism, still written on panic).
- **Zero-print exit gate**: after the sweep, `println!`/`eprintln!` in production code of all five codebases = 0 (tests, benches, examples, and build scripts exempt; CLI binaries' intentional user-facing stdout in solvemyastro exempt and listed by file in the plan). Enforced as a checklist grep like Phase 0's naming gate.
- Frontend `console.*` sites stay (out of scope this cycle) — but the api layer's error paths touched by other work must not *add* new bare `console.log` for backend-reportable errors.

## Error handling

Logging never takes the app down: file-open failure → console-only + one `warn`; writer is non-blocking (full disk stalls the log thread, not operations); filter parse errors keep the previous filter and `warn`; subscriber init failure leaves the app running unlogged rather than dead.

## Performance

Disabled levels cost ~an atomic load per call site (tracing callsite caching); field evaluation and formatting are lazy. Hot-loop math logging sits at `trace` and is additionally coarse-grained (per-iteration summaries preferred over per-element where element counts are huge). Acceptance: `corpus_bench` regression gate unchanged at default (`info`) level.

## Testing

- Unit: settings-JSON → filter-directive parsing; canonical-dictionary helpers; rotation config.
- Integration: init produces parseable JSONL; live level flip via `set_logging_config` changes an emitted module's output; a real command invocation emits its boundary span with outcome; panic hook still writes `crash.log`.
- Submodules: suites green + `corpus_bench` gate (no measurable cost at `info`).
- MCP: tools against a fixture log directory.

## Rollout shape

Phased (detail in the implementation plan): (1) infra — deps, subscriber, files, reload, settings plumbing; (2) command boundary both backends; (3) core sweep module-by-module; (4) submodules (usual branch + bump); (5) Settings UI; (6) MCP crate. Next version branch per the version-branch rule. Docker image and uninstall scripts updated where touched.

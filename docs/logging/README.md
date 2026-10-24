# Logging — developer guide

How to use Athenaeum's structured logging to debug, trace, and write
log-asserting tests. The binding rules (levels, schema, zero-print) live in
`CLAUDE.md → Logging` and the design spec
(`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`); this is the
practical how-to.

## Where the logs are

| Deployment | Location |
| ---- | ---- |
| Desktop (macOS) | `~/Library/Application Support/com.vsharifov.athenaeum/logs/athenaeum-desktop.<date>.jsonl` |
| Desktop (Linux) | `<XDG data dir>/com.vsharifov.athenaeum/logs/…` |
| Docker / web | `/data/logs/athenaeum-web.<date>.jsonl` + JSON on stdout (`docker logs`) |
| Anywhere | Settings → "Open log folder", or the `get_log_path` command |

Daily rotation, 14 files kept, one JSON object per line. Test/dev override:
`ATHENAEUM_LOG_DIR=<dir>` redirects everything.

## Turning verbosity up

**In the app**: Settings → Logging — global level (error/warn/info/debug) plus
per-module overrides (`scanner`, `solver`, `calibration`, `archive`, `transport`). Applies
live, persists in the DB. `trace` is deliberately not offered here.

**Environment** (wins over the UI while set; full
[`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
syntax):

```bash
ATHENAEUM_LOG=debug                                    # everything at debug
ATHENAEUM_LOG=info,athenaeum_core::scanner=trace       # per-item scanner detail
ATHENAEUM_LOG=warn,solvemyastro=debug                  # quiet app, chatty solver
```

The UI shows a banner ("Log level is overridden by ATHENAEUM_LOG on this
server") while `ATHENAEUM_LOG` is set; UI changes still persist but stay
inactive.

## Reading an event

```json
{"timestamp":"2026-07-03T17:19:36Z","level":"DEBUG",
 "fields":{"message":"building existing files map from DB","root_id":1},
 "target":"athenaeum_core::scanner",
 "span":{"root_id":1,"name":"scan"},
 "spans":[{"name":"start_scan"},{"root_id":1,"name":"scan"}]}
```

- `target` = module that emitted it; `fields` = the structured data
  (message is a stable phrase — grep fields, not prose).
- `spans` = the enclosing context chain: here a `start_scan` command invoked a
  `scan` operation. **Every event inside a long-running operation carries that
  operation's span**, which is how you pull "everything about scan X".
- Command boundary spans close with `fields.message = "close"` +
  `time.busy`/`time.idle` — that's the per-command duration record; a failed
  command additionally emits an error-level event inside its span.

Operation kinds and their id fields: `scan` (`root_id`), `archive_op`
(`operation_id`), `file_op` (`operation_id`), `solve` (`frame_id`), `export`
(`frame_set_id`), `registration` (`frame_set_id`).

## Querying with log-mcp

The repo's `.mcp.json` registers `athenaeum-logs` (crate `crates/log-mcp`) in
every Claude Code session automatically. Tools:

- `query_logs(level?, module?, contains?, since?, until?, limit?)` — filtered
  events, last-N semantics (default 200, cap 1000).
- `tail_logs(n)` — last n events (default 50).
- `list_operations(kind?, since?)` — completed operations with durations.
- `get_operation(operation_id)` — every event of one operation, cross-module.

By default the server scans BOTH flavor trees — production
(`com.vsharifov.athenaeum`) and the debug `.dev` sibling — merging events
chronologically, so a dev session sees the production app's logs alongside
its own; `ATHENAEUM_LOG_DIR` / `ATHENAEUM_DB_PATH` / `ATHENAEUM_APP_DATA_DIR`
narrow it to one tree. Operation ids can collide across the two trees — the dev
DB is a snapshot of the production one, so both apps mint ids from the same
sequence — so disambiguate a merged `get_operation` result with `since`, or pin
one tree via `ATHENAEUM_LOG_DIR`.

Manual use without MCP: it's a stdio JSON-RPC binary —
`cargo run -q -p log-mcp -- <log-dir>` — or just grep the JSONL
(`grep '"root_id":3' …/logs/*.jsonl`).

## Debugging recipes

- **"Why did this scan do X?"** — Settings → Logging → scanner=debug (or
  `ATHENAEUM_LOG=info,athenaeum_core::scanner=debug`), rescan, then
  `get_operation` / grep the `scan` span's `root_id`. Per-file decisions are
  debug; per-item detail is trace.
- **"Why didn't this frame get calibration?"** — calibration=debug; the
  matcher logs per-set scores and per-frame skip reasons under
  `find_calibration_for_frame_set` spans (warn = a fallback/assumption you
  should read).
- **Solver internals** — `solvemyastro=debug` gives per-stage timings
  (`stage` + `duration_ms`); `=trace` adds per-candidate detail. Solve outcome
  events carry `outcome`, `inliers`, `rms_px`, `scale_arcsec_px`.
- **A user's support bundle** — it's the same JSONL: point log-mcp at the
  unzipped folder (`cargo run -q -p log-mcp -- /path/to/their/logs`).

## Writing tests that assert on logs

The pattern used by the suite (see `logging/mod.rs`'s integration test and the
T13 live smokes):

1. Point the app/server at scratch dirs: `ATHENAEUM_LOG_DIR=$(mktemp -d)`,
   `ATHENAEUM_DB_PATH=/tmp/x.db`.
2. Drive the real flow (curl against `athenaeum-web`, or call the core fn).
3. Assert on the JSONL: parse each line with `serde_json`, filter by
   `target`/`fields`/`span.name`. Field names are stable API — that's the
   point of the dictionary.

In-process unit tests that need a subscriber: only ONE global subscriber can
exist per test binary (`try_init`), and env-var mutation must hold the test
module's `ENV_LOCK` — see `logging/mod.rs::tests` for both patterns.

## Rules when adding code (short form — CLAUDE.md governs)

- New command/route → `#[tracing::instrument(skip_all, err)]` (+ web mirror;
  `level = "debug"` if UI-loop-hot).
- New long-running operation → `info_span!("<kind>", <id_field>)` at the entry
  point + add the kind to `log-mcp`'s `OPERATION_KINDS` + the spec's Spans list.
- Events: level per the rubric (lifecycle=info, stage=debug, per-item=trace,
  fallback=warn, user-visible failure=error); message = stable phrase; data in
  snake_case dictionary fields; new field names go through the spec.
- Never `println!`/`eprintln!` in production code (grep-gated); never log
  secrets or full header dumps.

## Memory diagnostics (stacking, dev only)

Every stacking stage start logs a debug `memory relief` line (footprint before / after the allocator's free-page release) and the run logs debug `memory checkpoint` lines at named points. `ATH_VMMAP_DIAG=1` (macOS only, default off) additionally writes `vmmap --summary <own pid>` at each stage start to `<log dir>/vmmap/<run_id>-<stage>-<group>.txt`; a spawn failure logs a `warn!` and the stage carries on.

## Other environment switches you will see in the logs

| Variable | Effect | Logged as |
| ---- | ---- | ---- |
| `RAYON_NUM_THREADS=<n>` | Sizes the image pool on both hosts (and rayon's global pool) and overrides machine discovery. Read once at start. | `pool_threads`, `pool_why = "env"` on `compute budget resolved`; a value that is not a positive integer `warn!`s and the pool is sized automatically |
| `ATHENAEUM_TOKEN_STORE=file` | A release build keeps the hub token in the 0600 file only, never the OS keychain. Debug builds always do. The lab servers set it, so a rebuilt ad-hoc-signed binary never raises a keychain prompt. | one info `token store: file only` at the first account call; any other value `warn!`s once and is ignored |
| `ATHENAEUM_SCALAR_SELECTION=1` | Forces the scalar oracle for Integrate's selection kernels (the before arm of a timing or an identity proof on one binary). Anything but "" or "0" counts. | one info `integration selection kernel resolved` with `kind` (`"scalar"` / the vector kernel's name) and `forced` (bool, `true` when this variable forced the scalar kernel) |
| `ATHENAEUM_REQUIRE_VECTOR_KERNEL=1` | Tests only: the selection stress and identity tests fail instead of passing green when no vector kernel ran (an x86 CPU without AVX2 would otherwise skip silently). | no log line; the failure message names the kernels seen |

The first two are described in `docs/stacking/README.md` R-OSP-13 (Task 13c), the last two in R-OSP-14 (fast-measurement cycle).

## Detail moved from CLAUDE.md (2026-10-20) - Logging rules

### Logging

`tracing` is the sole logging API across all five Rust codebases (core/tauri/web + solvemyastro/rustafits submodules, facade-only in the latter two — no subscriber in library code). Design: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`. **Developer how-to (debugging recipes, log-mcp queries, log-asserting test patterns): `docs/logging/README.md`.** The `athenaeum-logs` MCP server (`.mcp.json`) exposes `query_logs`/`tail_logs`/`list_operations`/`get_operation` in every session — use it to inspect app behavior during development instead of asking for terminal relaunches.

- **Five levels**: `error` (failed op, user-visible consequence — every command boundary's `Err` logs here, never swallowed), `warn` (fallback/assumption taken), `info` (operation lifecycle — the level a beta user runs at), `debug` (stage-level internals — per-file/per-set decisions), `trace` (per-item math — **env-only, never exposed in the Settings UI**).
- **Message style**: message = short stable phrase, all data in snake_case fields — `info!(root_id, new = 12, "scan finished")`, never `info!("scan finished — 12 new")`. Canonical field dictionary (`frame_id`, `file_id`, `operation_id`, `command`, `path`, `src`, `dest`, `duration_ms`, `count`, `error`, `outcome`, `stage`, …) lives in the spec's "Unified event schema" section — new field names require a spec update, never invent inline.
- **Files**: rotating JSONL at `<app-data>/logs/` (desktop; debug builds: the `.dev` sibling app-data dir) / `/data/logs/` (Docker/web), daily rotation, max 14 files, per-process filename prefix (`athenaeum-desktop.*`, `athenaeum-web.*`) so both hosts can point at the same dir without racing. `get_log_path` returns the directory (not a single file).
- **Runtime control**: Settings → Logging (global level + per-module overrides for `scanner`/solver/`calibration`/`archive`+`file_op`/`transport` (iroh), live via a reload handle, no restart). `ATHENAEUM_LOG` (full `EnvFilter` syntax) overrides settings entirely while set — UI shows an "overridden by environment" banner. Default when nothing is configured: `info`.
- **Command boundary**: every Tauri command / Axum route wears `#[tracing::instrument(skip_all, err)]` (or `err(Debug)` per return type). The span-close event (`FmtSpan::CLOSE`) carries the duration as `time.busy`/`time.idle`; failure shows up as the `err`-emitted error event inside the span (there is no literal `duration_ms`/`outcome` field on boundary spans — those canonical names apply to hand-written events). Hot-path commands (fired per-frame/per-index-change, e.g. `get_setting`, `get_frame_preview`, `get_frame_star_metrics`) are instrumented at `level = "debug"` instead of the default `info` to avoid flooding.
- **Zero-print rule**: `println!`/`eprintln!` = 0 in production code of all five codebases. Exempt: `#[cfg(test)]`/`tests/`/`benches/`/`examples/`/`build.rs`, the CLI binaries of solvemyastro (`main.rs`) and rustafits (`src/bin/rustafits.rs`, `src/bin/debug.rs`) — intentional user-facing stdout — `crates/catalog-builder` (dev-facing CLI build tool; stdout is its UI, same category as the CLI-binary exemption), and the Perseus capture-agent CLI (`crates/perseus/src/main.rs` — `status` human output — and the interactive login prompts/`println!` sign-in confirmations in `crates/perseus/src/account.rs`; same CLI-binary category).
- **`ProgressEmitter` events stay events** — SSE/Tauri progress payloads are UI data for the frontend, not logs; don't fold one into the other. Rule of thumb: notify on outcomes (via `notify()`, see below), log everything (every level, every stage) to `tracing`.

- **Analysis config decode (Plan C C5)**: `analysis::config::load_config` logs `warn!(key = "analysis.config", error, "stored analysis config does not decode; using the default")` before it takes the default; both fields are already in the dictionary (`key`: the `settings` row key, `error`). No new field names.

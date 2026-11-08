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

## Zip archive events

Zip work logs under `athenaeum_core::zip_store` (scan, extraction, cache, catalog moves) and `athenaeum_core::archive`; turn the `scanner` and `archive` module levels up to `debug` to see per-entry decisions. Every event about one entry carries `zip_path` + `zip_entry`. The stable phrases to grep:

| Phrase | Level | Fields | Use |
| ---- | ---- | ---- | ---- |
| `zip scanned` | info | `root_id`, `zip_path`, `count` (frames cataloged), `new`, `updated`, `bytes_read` | The metadata-scan budget check: `bytes_read` is at most 64 KiB per entry plus the directory and under 0.5% of the zip. A rescan of an unchanged zip logs nothing (one `stat`). |
| `zip entry materialized` | debug | `zip_path`, `zip_entry`, `cache_hit`, `bytes_read` | One extraction request; `cache_hit = true` means no read from the zip. |
| `zip entry extraction failed` / `extracted zip entry does not match the hash recorded when it was archived` | warn | `zip_path`, `zip_entry`, `error` / `expected_hash`, `actual_hash` | A frame that cannot be read ("inside X.zip, which is not reachable"). |
| `zip cache evicted` / `zip cache cleared` | debug / info | `bytes`, `limit_bytes` | The cache shrinking back to its limit after an operation released its pins. |
| `zip frames unpacked` | info | `unpacked`, `adopted`, `conflicts` | A permanent unpack. |
| `frames already in zip files left out of the archive plan` | info | `frames_set_id`, `skipped`, `remaining` | Move and ZIP skipping zip-resident members. |
| `rollback refused: ...` / `keeping the zip after restore: ...` | error / warn | `operation_id`, `lost`, `first` / `zip_path`, `count` | The archive safety refusals. |
| `zip renamed in the catalog` / `zip sent to void` / `relink zips` | info | `src`, `dest`, `rows`, `virtual_paths` / `zip_path`, `rows` / `moved`, `present`, `missing` | Whole-zip rename, Void and relink. |

The new field names (`zip_path`, `zip_entry`, `zip_id`, `zip_cache_key`, `cache_hit`, `bytes_read`, `limit_bytes`, `unpacked`, `adopted`, `conflicts`, `zips`, `moved`, `present`, `missing`, `virtual_paths`, `rows`, `cataloged_frames`, `frames_set_id`, `calibration_set_id`, `skipped`, `remaining`, `expected_hash`, `actual_hash`, `owner_file_id`, `stored_size`, `actual`, `lost`, `first`, `swept`, `old_path`, `offline_root`, `prefix`, `file_a`, `file_b`) are defined in the logging spec's "Dictionary extension (zip archives in the catalog)" paragraph; `bytes_read` keeps its integration meaning (band reads) elsewhere. Query with `athenaeum-logs` `query_logs(contains="zip_path")`, or `get_operation` on an `archive_op` id for a Move and ZIP / Unarchive.

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
- **Analysis input offset (Plan C C5 r2)**: rustafits' `star_measure::raw::read_raw_frame` (target `astroimage::star_measure::raw`; it was `analysis::input` until port phase 4, R-OSP-19) logs `warn!(path, plane_max, "BITPIX 16 / BZERO 0 file read with the 32768 offset")` when the plane maximum exceeds 32767, else a `debug!` with `plane_max` and `full_scale`; `star_measure` logs `warn!(count, given_flags, …)` when `given_extended` does not match the given seeds. `plane_max`, `full_scale`, `given_flags` are dictionary extensions in the logging spec. Since `star_measure` moved to rustafits (R-OSP-19 in `docs/stacking/README.md`), its events carry the target `astroimage::star_measure::…`, not `athenaeum_core::…`. The rest of the Analysis input moved with `raw` and kept its fields: `error!(path, error, "raw frame read failed")` and `warn!(path, finite_max, "float file above 65535 ADU, full scale follows its maximum")` (target `astroimage::star_measure::raw`), and the mosaic's `error!(width, height, "cfa green sites: no noise estimate on the green phase")`, now `star_measure::green`'s (target `astroimage::star_measure::green`; was `analysis::cfa`). `athenaeum_core::analysis` emits nothing of the Analysis input's own (`analysis::config::load_config`'s decode warn above stays) and the filter has no `analysis` module key, so the base level shows these as before.
- **XISF header parse abort (port phase 4, R-OSP-19)**: the ported header rules log `error!(path, position, error, "error parsing XISF XML header; using partial result")` under the target `astroimage::formats::stored_keys` (rustafits `xisf_keyword_text`) when an XISF header's XML stops parsing, then carry on with the partial result; `fits_parser`'s own copy of the loop (target `athenaeum_core::fits_parser`, same message) still fires for the catalog. `path` is on the line only when it comes from `read_xisf_stored_keys`, the header keys `star_measure::raw` reads; the XISF reader's Bayer-offset fold passes none. One `read_raw_frame` of a malformed CFA XISF can therefore log the line up to three times. The fields are those of `fits_parser`'s line; the port adds none.
- **Cached-file identity (frame-level cache, R10 M1)**: `stacking::plan::file_matches_identity` logs `debug!(path, recorded, current, "cached file has the recorded size but another modification time; counted as changed")` when a cached file's size equals its row's and its modification time does not (the one case where a file looks right and is refused: a copy tool that drops mtimes, a filesystem reporting another time after close), and `warn!(path, error, "cached file cannot be stat-ed; counted as changed")` for a stat failure other than not-found. A missing file is the ordinary stale and logs nothing. `recorded` and `current` (RFC 3339 strings) are dictionary extensions in the logging spec; `path` and `error` are existing names.
- **Registration seed refusal (Task RL, R4-33 (3))**: `stacking::register::frame` (`register_detected`) logs `warn!(path, seed = "wcs", coverage, cells, pairs, reason, "plate-solve seed refused; quad seed tried")` when a leading plate-solve seed's map is refused or a hint is rejected at the confirmation radius (`coverage`/`cells` only for a coverage refusal, `pairs` always), before the quad seed's turn and whether or not it then carries the frame, and `warn!(path, error, "plate-solve seed retry failed; quad verdict stands")` when the plate-solve retry after a failed quad seed fails too. Each line precedes its frame's `"frame registration failed"` / `"frame registered"` line and carries the same `path`, so a refusal joins to its frame by `path` under the fan-out. `align` logs nothing: `align_traced` returns the refusal in a `SeedTrail`. `seed`, `cells`, `pairs` are dictionary extensions in the logging spec; `path`, `coverage`, `reason`, `error` are existing names.

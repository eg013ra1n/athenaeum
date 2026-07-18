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

**Dictionary extensions (sync connection-path diagnostics, `sharing::iroh`):** `peer` (short hex id of the remote endpoint on a connection-path event); `direction` (`"outgoing"`/`"incoming"` — which side established the QUIC connection); `conn_type` (`"direct"`/`"relay"`/`"mixed"`/`"pending"` — a connection's transport-path classification, or `"direct"`/`"relay"`/`"other"` on a path-change event); `relay_url` (the home relay URL an endpoint connected to); `relay_mode` (`"disabled"`/`"default"`/`"staging"`/`"custom"` — what the endpoint was actually built with); `relay_count` (number of relays in the endpoint's relay map); `timeout_ms` (a configured wait bound, e.g. the home-relay `online()` timeout — distinct from `duration_ms`, which is elapsed time); `role` (`"recv"`/`"out"`/`"collab"` — the `SharedIrohNode` role whose blob-tag prefix a startup sweep scoped to, per the 2026-07-15 iroh hardening); `kind` (`"announce"`/`"ack"`/`"project_announce"`/`"project_request"`/`"fetch_progress"` — the inbound transport-event variant an orphan-drop warn names when the `SharedIrohNode` event demux finds no `(peer, package)` claim or Recv consumer, per the 2026-07-15 iroh hardening; on this event `peer` also carries the source endpoint's hex id). Reuses `addr` for a connection's selected remote transport address (its `ip:<socket>` / `relay:<url>` Display), extending the T1 "server bind address" sense to any transport address.

**Dictionary extensions (sync retry backoff, `sync::engine`, TEST-11 2026-07-16):** `delay_ms` (u64; the backoff window a package will now wait before its next re-announce — the just-computed `retry_backoff(ack_timeout, rung)` in ms, distinct from `duration_ms`/`timeout_ms`: it is a scheduled future wait, not elapsed time or a configured bound); `next_retry_at` (RFC3339-UTC-millis wall-clock instant the retry is scheduled for — the same stamp persisted to `OutboundRow::next_retry_at`, reused for the log so a one-line `query_logs` on the `"ack timeout, backing off"` event answers "when does it retry" without watching the UI countdown). `forced` (bool) rides the `sharing::iroh` `"relay map node rebuild complete"` event (TEST-12) alongside the existing `relay_count`: whether the deferred rebuild fired past the idle gate (max-defer) or on a quiet instant.

**Dictionary extension (FITS writer sanitize, `fits_writer::card`, 2026-07-18):** `keyword` (the FITS header keyword whose string value/comment/text contained non-printable-ASCII chars and was lossily degraded to `?` placeholders on the `"non-ASCII characters in header value sanitized"` warn — the ATH_REJ master-build bug-report fix).

**Dictionary extension (send+receive concurrency instrumentation, `sync` + `sharing::iroh`, Problem 4 Task 4.1 2026-07-19):** `op` (a stable snake_case name for the store write being timed on the `"sync store write slow"` warn — `"receiver_ingest"` for the receiver's per-frame ingest transaction, `"confirm"` for the sender's ack-confirm write). Reuses `duration_ms` (elapsed time) on that warn, on the `"sync receiver announce handled"` info (inline announce-handling time, candidate (a)), and on the `"inbound event delivery delayed"` warn (the `tx.send().await` wait to hand a decoded inbound event to its consumer — candidate (a): a blocked send means the receiver loop was busy). Reuses `kind` (the inbound transport-event variant — same values as the orphan-drop warn) and `peer` on that delivery-delayed warn, and reuses `from` (short hex id of the announcing peer) + `package_id` on the announce-handled info. All three are behavior-neutral timing events for discriminating the smoke-reported "can't receive while sending".

**Dictionary extension (sync send-path dial classification, `sync::engine`/`sync::diagnostics`, 2026-07-19):** `class` (stable snake_case dial-outcome class of a failed serve/announce attempt — `no_route`/`relay_unreachable`/`refused`/`timeout`/`not_started`/`other` — derived string-only from the `anyhow`-chain error text on the `"sync serve/announce failed; will retry"` event, and mirrored as the machine-readable prefix of the outbound row's `last_error`; a best-effort diagnostic hint, never an authorization signal).

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

**Spans**: every long-running operation opens a span named for the operation kind, carrying that kind's id field; all nested events inherit it. This is the correlation mechanism for "give me everything about operation X" (`log-mcp`'s `list_operations`/`get_operation`). As-built, six kinds: `scan` (`root_id`), `archive_op` (`operation_id`), `file_op` (`operation_id`), `solve` (`frame_id`), `export` (`frame_set_id`), `registration` (`frame_set_id`).

## Runtime level control

- Settings gains a **Logging** section: global level (`error|warn|info|debug`) + per-module overrides for `scanner`, `plate_solve`/solver, `calibration`, `archive`+`file_op`. Stored as one JSON value in the settings table (`logging.config`), mirrored by a TS type; applied live via the reload handle.
- Commands `get_logging_config` / `set_logging_config` in both backends.
- `ATHENAEUM_LOG` (full `EnvFilter` syntax, e.g. `info,athenaeum_core::scanner=trace`) overrides settings entirely while set; the UI shows an "overridden by environment" notice.
- Default when nothing configured: `info`.

## Command-boundary instrumentation

As-built (T4): each command/route function carries `#[tracing::instrument(skip_all, err)]` (Tauri) or `#[tracing::instrument(skip_all, err(Debug))]` (Axum, whose error type isn't `Display`) directly on the function — no separate wrapper/macro layer. The span is named for the function; its close event (`FmtSpan::CLOSE`, built into the subscriber stack) carries `time.busy`/`time.idle` as the duration record, superseding the literal `duration_ms`/`outcome` fields floated at design time. A failing command surfaces as the `err`-emitted `ERROR`-level event inside the span, not as an attribute on the close event — this is what structurally enforces never-swallow. Applied mechanically across all ~157 commands in both backends' command/route modules (323 annotated functions total: 160 Tauri + 163 web); a handful of high-frequency, low-value commands (`get_setting`/`set_setting`, per-frame preview/metrics fetches driven by UI index changes) are additionally marked `level = "debug"` so they don't spam the default `info` tier.

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

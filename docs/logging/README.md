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
per-module overrides (`scanner`, `solver`, `calibration`, `archive`). Applies
live, persists in the DB. `trace` is deliberately not offered here.

**Environment** (wins over the UI while set; full
[`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
syntax):

```bash
ATHENAEUM_LOG=debug                                    # everything at debug
ATHENAEUM_LOG=info,athenaeum_core::scanner=trace       # per-item scanner detail
ATHENAEUM_LOG=warn,solvemyastro=debug                  # quiet app, chatty solver
```

The UI shows an "overridden by environment" banner while `ATHENAEUM_LOG` is
set; UI changes still persist but stay inactive.

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

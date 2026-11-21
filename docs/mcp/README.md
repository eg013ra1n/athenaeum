# MCP (assistant access)

Reference for the MCP server in `athenaeum-core` (`crates/athenaeum-core/src/mcp/`). Design:
`docs/superpowers/specs/2026-10-27-athenaeum-mcp-design.md`; plan: `docs/superpowers/plans/2026-11-19-athenaeum-mcp-v1-plan.md`;
spike facts (what the real clients do): `docs/superpowers/research/2026-11-19-mcp-spike.md`.

## Purpose and rules

An assistant (Claude desktop, Claude Code, ...) asks Athenaeum about one astrophotography catalog. Three rules bind every tool:

- **Questions change nothing.** They read through the host's one `SQLITE_OPEN_READ_ONLY` connection (`McpHost::reader`, plan R11),
  never a PRAGMA on a pooled connection; the exceptions are `file_details`, the calibration summary and `find_duplicates_in_scope`, which reuse the app's
  context-taking functions that only read (below). `questions_change_nothing` digests the catalog before and after every tool.
- **No tool applies a plan.** Changes are plans that open in Athenaeum's window, where only the user can apply them; there is
  no `apply` tool, no in-chat card, no elicitation in v1.
- **Off by default.** `mcp.enabled` is `"false"`; see "Assistant access: the local channel" below.

## Layout

| Item | Where |
| ---- | ---- |
| `McpHost` (context, identity, the reader, `on_catalog_open`, `source`) | `mcp/mod.rs` |
| The local channel: endpoint, handshake, the channel files | `mcp/channel.rs` |
| On/off lifecycle, the key, `McpStatus`, the client config text | `mcp/lifecycle.rs`; commands' logic in `api/mcp.rs` |
| `Answer`, `to_tool_result` (line 1 + both halves) | `mcp/answer.rs` |
| `AthenaeumMcp` (the rmcp handler, `INSTRUCTIONS`) | `mcp/server.rs` |
| Tool bodies, argument types, the scope step | `mcp/tools/{mod,catalog,plans}.rs` |
| The Black Hole plan: planner, applier, fixtures | `mcp/plans/black_hole.rs`, `black_hole_tests.rs` |
| Tests (a client over `tokio::io::duplex`, both protocol revisions) | `mcp/tests/server_tests.rs` |

`McpHost::new` opens nothing; `on_catalog_open` opens the reader and reconciles the local channel to the stored setting. Before it, `reader()` and `source()` are `Conflict("Athenaeum is starting ...")` and every tool answers that in
words with `catalog not open` on line 1.

## The answer shape

Every answer is two halves that each read on their own.

- **Text, line 1:** `[<server name> <version> · catalog <db path>]`. The server name is `athenaeum` (release) or `athenaeum-dev`
  (debug build, `mcp_server_name()`), so one assistant can hold both.
- **Structured half:** `{"source": {"build", "version", "catalog"}, ...}` then the tool's own fields, camelCase.
- **Files are named by file name and folder.** The text never shows a handle (`file:<id>`). Every structured object with a
  `handle` also has `fileName` and `folder`; a duplicate group's `handle` (`dup:<key>:<hash>`) sits beside the file name and
  folder of its first copy for that reason. No table or column name, hash or set id appears in a description.
- **A failure is an error result in words** (`isError`, same line 1, `{"source", "error"}`), never a protocol error: a client
  shows a protocol error's message to nobody. A protocol error is only for an unknown tool or a malformed request.
- **A scope that names nothing the catalog holds is an answer**, not an error: it says `no camera 'CamB'; the catalog has 'CamA'`
  (`scope::hints_text`) and runs nothing else.
- **The default state says what it leaves out:** the last line is `Black Hole files are left out (N)`, structured
  `blackHoleLeftOut`.

## Tools (the catalog questions, the plan tools, and `plan_status`)

| Tool | Answers |
| ---- | ------- |
| `catalog_overview` | The default scope: totals by frame type, how many objects, cameras, telescopes and filters, first and last night, storage, duplicate groups, missing-metadata counts per category |
| `query_frames` | Totals and integration time over a scope, optional `groupBy`, a page of files (`filesPage`), a stored-measurement summary (`quality`), calibration (`calibration`), storage (`storage`) |
| `file_details` | One file: header values, stored measurements and whether they are current, transparency, calibration links, copies, plate solve |
| `find_duplicates` | Duplicate groups with a copy in the scope, each copy with file name, folder, scan root kind and a processing-folder note |
| `find_missing_metadata` | The Missing Metadata view's rows inside the scope: counts per category, folders, a page of files |
| `find_rebuilt_masters { scope, page }` | Masters of the calibration library built more than once (the `_2` / `_3` copy a rebuild wrote beside the first): groups of copies with how many calibration links point at each (`linkCount`: light frames, and master sets that use it as a dark or bias); not duplicates by any hash, so `find_duplicates` does not list them (`rebuilt:<lowest file id>` handles). Reads through the reader (`catalog_query` helpers only); like `find_duplicates`, a group is in the scope when one of its copies is, and it is then listed whole (every copy, its handle the lowest file id of the whole group, so `plan_black_hole` finds it) |
| `plan_black_hole { source, groups, keepInFolder?, keepFiles? }` | Prepares a Black Hole plan from named groups (`source` `duplicates` or `rebuilt_masters`); see "Black Hole plans" below. Changes nothing until the user applies the plan in Athenaeum's window. A plan with copies to byte-check answers at once with state `preparing` (the checks run on a thread of their own); the answer of a published plan lists every copy and its fate |
| `plan_metadata_fill { scope, fields?, statement? }` | Prepares a metadata plan: fills the object (lights only), camera, date or frame type that frames in the scope miss, from evidence or from `statement {field, value}` (the user's own word); see "Metadata plans" below. Refused in words, leaving the waiting plan alone, when the scope names nothing, nothing can be proposed or the plan would hold more than 2 000 items. The plan is published before the tool answers, and the answer lists every group and frame |
| `plan_plate_solve { scope, onlyUnsolved? }` | Prepares a plate-solve plan over the lights of the scope (those that miss coordinates unless `onlyUnsolved` is false); see "Plate-solve plans" below. Refused in words, leaving the waiting plan alone, when the scope names nothing, no light is left to solve or the plan would hold more than 2 000 items, and refused while any plate-solve batch runs (the running run's own words). Solver builds only |
| `solve_status { runId }` | A run's progress while it runs (frames done of total, failed so far, the file being solved, each failure by file name and folder; no solved count, the store can still reject a frame or roll the batch back) and its result after it (solved, failed with every failed file and its error, frames a cancel left unattempted). Changes nothing; knows the runs the assistant started since Athenaeum opened (the last 20). Solver builds only |
| `cancel_solve { runId }` | Sets the cancel flag of the assistant's own run, and only while slot 0 holds that exact `runId`; a run started in Athenaeum's window, an ended run or an unknown id is answered in words and nothing is stopped. It stops a run, it never decides a plan (the one tool name that says "cancel"; `no_tool_applies_a_plan` allows it by name). Solver builds only |
| `plan_status {planId}` | Where a plan stands, for a plan made on any connection (`mcp/tools/plans.rs`): `waiting for you in Athenaeum's window (expires <time>)`, `applied` with the report (applied, skipped, failed, each by file name and folder), cancelled, replaced, expired, failed; an unknown id says `No plan <id>: Athenaeum restarted since, or the plan is older than a day.` (an answer, not an error). Only an `applied` plan's text says "applied" (`only_an_applied_plan_says_applied`): a plan that ended `failed` reads `Failed: <reason>.` and, when it got something done, `Done before it stopped: N done, N skipped, N failed.` (and never "applied"); before a line was written the words follow the kind (Black Hole `No file was recorded as moved before it stopped.`, metadata `No value was written before it stopped.`, plate solve `Nothing was started before it stopped.`), and a plate-solve plan that was refused its start says the reason once, `Nothing was started: <reason>.`, with no `Failed:` line above it |

Each tool is a core fn `mcp::tools::<name>(host, args) -> Result<Answer, ApiError>` wearing
`#[tracing::instrument(skip_all, fields(tool = "<name>"), err)]`; `AthenaeumMcp::all_tools()` (`server.rs`) joins the tool routers (`catalog_tools() + plan_tools() + fill_tools()`, and `tools.json` is written from it); the `#[tool]` method is a two-line adapter that runs it with
`spawn_blocking` (it reads SQLite) and wraps it with `to_tool_result`. No test keeps a list of tool names: core's tests read `all_tools()`, the bridge's read `McpHost::tool_list_json()`, so a new tool cannot leave a stale list.

**One frame set.** A `query_frames` scope that is exactly one frame set (`frameSet` and nothing else, default state) answers the
frame set's own page figures (`catalog_query::frame_set_totals`): its frame count and integration time are the page's (the
page sums the members' live exposure and leaves Black Hole files out), never the stored `total_exp_time` column. When members
are in the Black Hole the text adds `N frames of this set are in the Black Hole and not counted (<time>)` (`1 frame ... is`).

## The scope

One `Scope` (`scope::Scope`) filters every question and plan; every field narrows, an absent one does not. Text values match a
value the catalog holds, ignoring case and runs of spaces, never a part of one. User-supplied paths enter in two places only,
`file_details.file` and `Scope.folder` / `scanRoot`; both are matched against the catalog's stored paths (equality,
separator-strict prefix) and never opened or `canonicalize`d.

### Limits worth knowing

- **R1 night limit.** A night is the UTC date of `date_obs` minus 12 h, a fixed 12:00 UTC cut, not local noon. An observer whose
  night straddles 12:00 UTC (far from Greenwich) sees one night cut in two. Recorded, not fixed.
- **`DATE-OBS` forms SQLite reads as a date that the night rule does not.** The SQL twin (`DATE(col, '-12 hours')`) reads a bare time
  (`22:15:03`, as today's date), a Julian-day number and `now`; the Rust twin (`scope::night::night_of_date_obs`) gives `None`
  for them. The scanner never writes one of these into a frame's `date_obs`, so the twins agree on every catalog row.
- **A content-key duplicate group whose copies have no catalog frame row is never in a scope.** A scope is a filter on frames;
  such a group is visible only in the Duplicates page.
- **Three reads use the pool, not the reader:** `file_details`, the calibration summary (`query_frames` with `calibration`) and
  `find_duplicates_in_scope` (`find_duplicates`, `catalog_overview`) reuse the app's own ctx-taking functions, which only read
  (R11's named exceptions); the reader covers `query_frames`' frames, quality and storage, `find_missing_metadata` and the overview's counts.
- **The reader guard is never held across a call that takes `ctx`:** do every reader read, `drop` the guard, then call the app
  function (`McpHost::reader` says so; `a_slow_app_call_does_not_hold_the_reader` pins it).

## Sizes (D3)

Per answer: 50 groups, 3 example files each (the text lists examples only when an answer has 10 groups or fewer); 50 files per
page; 25 frame sets per calibration page; 5 outliers per side; 50 duplicate groups per page; at most 50 folders in a
missing-metadata or storage answer (the rest said as a count; the text lists the 10 largest storage folders). Plan lifetime and
plan size limits belong to the planning tools.

## Assistant access: the local channel

The desktop serves the assistant over a local channel; an assistant starts the `athenaeum-mcp` bridge, which connects to it. The
web build serves `/mcp` over HTTP instead (below) and binds nothing here.

**Setting.** `mcp.enabled` (`"false"` default, in `defaults::all()`). Only `set_mcp_enabled` writes it; the generic
`set_setting` refuses the key with `Invalid("use set_mcp_enabled")`. No other path writes settings in bulk (the Settings page's
"reset" is `set_setting` per key), so nothing else needed the same refusal. `get_mcp_status` / `set_mcp_enabled` /
`regenerate_mcp_token` are the three commands (`api/mcp.rs`); `McpStatus` is in `src/types/mcp.ts`.

**Channel files**, in `<app-data>/mcp/`:

| File | Content |
| ---- | ------- |
| `token` | The key: 64 hex characters, mode 0600. Kept across off/on; only Regenerate changes it. The web build's key is this file |
| `endpoint` | `unix:<path>` or `pipe:<name>` (one line) |
| `enabled` | Marker: the desktop binds early when it exists |
| `tools.json` | `{"tools":[...]}`, the server's own `tools/list` result, written through a temp file and a rename; a bridge serves it while the app is not running |

**Endpoint.** Unix socket `<app-data>/mcp/mcp.sock` when that path is at most 100 bytes, else
`<temp dir>/athenaeum-mcp-<8 hex>/mcp.sock` (the directory is made mode 0700; the 8 hex are the first 4 bytes of the SHA-256 of
the app-data dir's path). On a dev Mac the first path is 102 bytes, so the temp-dir one is the normal one there. Windows: the pipe
`\\.\pipe\athenaeum-mcp-<app-data dir name>-<8 hex>-<8 random hex>` (the 8 hex as above; the random part is new at every bind, from the
same source as the key, so a pipe instance left over from an earlier bind never blocks a new one and the name cannot be derived from the
path), opened as the first instance. The `endpoint` file is how a bridge learns the name.

**One instance per build.** Whoever holds an exclusive lock on `<app-data>/mcp/serve.lock` (`File::try_lock`, every OS) owns the
endpoint. The lock lives in the channel's state and is released when the channel closes (off, failure, quit); the OS also releases it
when a process dies, so a crash never leaves a false `elsewhere`. The file itself is never removed. Holding the lock proves nobody else
serves, so on Unix a leftover socket file is removed before bind without a probe.

**Handshake (frozen, append-only).** The bridge writes `ATHENAEUM-MCP/1 <token>\n` (one line, at most 256 bytes, within 5 s); the
host answers `OK <server name> <version>\n` and the same stream then carries MCP (newline-delimited JSON-RPC, rmcp's
`serve_server_with_ct`), or `ERR <starting|off|token|version> <message>\n` and closes. `/n` with n above 1 is `ERR version`; a line
that is not a handshake, too long, or with a wrong key is `ERR token`; the key is compared as SHA-256 digests and never logged or
echoed. A later host still serves `/1`.

**Lifecycle.**

- *On:* store `"true"`, key (kept if the file exists), `enabled` marker, bind, `endpoint`, `tools.json`: `serving`.
- *Off:* store `"false"`, cancel every connection (each holds a child of the run's `CancellationToken`), close the listener, delete
  `endpoint`, `enabled` and `tools.json`, keep `token`: `off`.
- *Regenerate:* new key, every live connection cancelled (the stream ends), the listener stays.
- *`start_early`* (desktop `.setup`; the host needs `set_app_data_dir` first, because the database is opened later): binds in `starting`
  iff the `enabled` marker exists; a connection gets `ERR starting`. *`on_catalog_open`* then reconciles to the stored setting, which
  wins over the marker (`starting` becomes `serving`, or the channel is closed and its files removed).
- *`shutdown`* (app exit): closes everything and removes the socket and `endpoint`; the marker, `tools.json` and the key stay.
- A leftover socket file (a crash) is removed before bind, under the lock. A 30 s check (`recheck_endpoint`) rebinds when the
  socket file has vanished (`$TMPDIR` cleaners); live connections are untouched.
- The serve lock held by another instance of this build: `elsewhere`, `lastError` "another Athenaeum of this build serves the
  assistant". That instance's files are never removed or rewritten by this one, including by its off switch. Any other lock error is
  `failed` with the OS error. The same 30 s check runs while `elsewhere` with the setting on: it retries the lock and, once it has it,
  binds and becomes `serving` (`elsewhere_becomes_serving_when_the_other_instance_stops`).
  The check reads the stored `mcp.enabled` first, in every state (the catalog is shared, so the setting is the one truth): stored off,
  a serving or `elsewhere` instance disengages (`off_in_the_serving_instance_is_not_undone_by_the_other`,
  `off_in_the_other_instance_reaches_the_server`); only stored on lets an `elsewhere` instance engage. `endpoint` is removed only by the
  instance that holds the serve lock (`a_quitting_instance_that_does_not_serve_leaves_the_files_alone`).
- *How a connection closes.* `Run::close` drops the listener, the handshake-phase tasks and every session task inside the call. A
  session that has completed `initialize` has been handed to rmcp's own task, which is only cancelled: its stream closes when that
  task next runs (`regenerate_cuts_off_connected_clients`: end of stream within 2 s). A Windows pipe name is fresh per bind, so a
  late-closing instance never blocks the next bind. The Windows instance that waits for the next client also outlives the call
  by a moment: dropping it cancels its pending connect, and mio keeps the handle until the runtime processes the cancellation.
  A client that opens the pipe in that window gets no handshake reply, and the bridge says "not running" and tries again at
  the next request. The channel tests therefore give the runtime up to 2 s on Windows before a closed endpoint must refuse
  (`refused_soon`); the first CI run of these tests on Windows (2026-11-21) failed without it.
- Channel tasks run on the caller's tokio runtime, or on a small fallback runtime (`mcp-channel`, 2 workers) when the caller is not
  inside one (a window's setup closure).

**Web.** Turning it on is refused with `NEEDS_API_KEY` (`Invalid`) when `ATHENAEUM_API_KEY` is not set; `McpStatus.blockedReason`
carries the same sentence, and a stored `"true"` without a key at boot stays `off` with `lastError` = the sentence. With a key, on
means `serving` (the key is in `McpStatus.token`; `check_token` and `is_serving` are what the `/mcp` route asks). Nothing binds.

**Web `/mcp`.** The route (`athenaeum-web/src/routes/mcp.rs`, `require_mcp_access`) is gated in this order: access off answers 403
`Assistant access (MCP) is off in Athenaeum's Settings.`; no catalog open yet answers 503; a missing or wrong `Authorization: Bearer
<key>` answers 401 (the `X-API-Key` header is not this key). An SSE `GET` stream that is already open on `/mcp` outlives turning
access off or regenerating the key, while every new request is refused at once. Accepted for v1: the questions are `POST`s, and
the stream carries only server notices.

**Client config.** `desktop_client_config(name, bridge, build, app_data_override)` returns
`{"mcpServers":{"<name>":{"command":"<bridge>","args":["--build","<build>"(,"--app-data-dir","<dir>")]}}}` and
`claude mcp add <name> -- "<bridge>" --build <build>` (plus ` --app-data-dir "<dir>"`). `McpStatus` fills them with the bridge's path
(`bridge_path_for`, see "Packaging and the bridge copy") and the app-data dir always passed, so the bridge never guesses.

## The bridge (`athenaeum-mcp`)

`crates/athenaeum-mcp` is a small program an assistant starts over stdio (`athenaeum-mcp --build release|dev [--app-data-dir <dir>]`, the
arguments the app writes into the config it copies, ruling R8/P9). It links no `athenaeum-core` and opens no catalog: it pipes the
client's newline-delimited JSON-RPC to the app's channel after the handshake above, and says why in words whenever the app cannot
answer. The handshake and the channel files are a wire contract it copies (`Endpoint::parse`, the `ATHENAEUM-MCP/1` line); the tests in
`crates/athenaeum-mcp/tests/bridge_e2e.rs` run it against real core hosts, so a change of either side fails there. Folder names:
`Build::dir_name` copies core's `paths::APP_DATA_DIR_NAME_RELEASE` / `_DEV` (a test pins them). Without `--app-data-dir` it uses
`dirs::data_dir()/<dir name>` and never guesses beyond that.

**Every connect attempt re-reads the files** (`enabled`, `token`, `endpoint`): a Windows pipe name is new at every bind and the key can be
regenerated. No `enabled` marker means `off`; no key or `endpoint`, or a connection that fails, means `not_running`; then the handshake:
`ERR starting|off|token|version` are `starting`, `off`, `token_rejected`, `outdated`; `OK <name>` with a name other than the build's own
is `wrong_build` (a dev bridge at the release folder says it reached `athenaeum`).

**Words.** `Unavailable` (`not_running`, `off`, `starting`, `token_rejected`, `outdated`, `wrong_build`) has one sentence per state
(`unavailable_text`, build-aware: ` (dev build)`), carried by `FallbackServer`, a tiny rmcp server over `tokio::io::duplex`: its
instructions are the sentence, `tools/list` is the app's own list from `<app-data>/mcp/tools.json` when it parses (else one tool,
`athenaeum_status`), and a `tools/call` of ANY name is an error result with the sentence as text and
`structuredContent {state, build, message}`. A client that starts before the app (Claude desktop starts its bridges at its own launch)
therefore sees the real tools at once and gets the reason when it calls one.

**Loop.** Live: bytes both ways, requests tracked by id until answered. The bridge keeps the client's `initialize` and
`notifications/initialized`. When the live stream ends it looks again at once (unless the connection lived under half a second) and
otherwise starts a fresh fallback; either way it replays `initialize` (id `athenaeum-bridge-replay`, the reply dropped) and
`initialized` into the new session and resends the unanswered requests, so a question asked while the app quits is answered in words,
never lost. While down, a request arriving two seconds or more after the last attempt tries to connect again; on success it switches,
replays, and writes `notifications/tools/list_changed` to the client. A stateless client (revision 2026-07-28) has nothing to replay.
Client end of input: exit 0.

**Packaging and the bridge copy.** The installers carry the bridge as a Tauri sidecar. `bundle.externalBin` is listed only in the
overlay `crates/athenaeum-tauri/tauri.mcp-bridge.json`, which the release pipeline passes with `--config`: tauri-build fails every
`cargo build` / `cargo test -p athenaeum-tauri` whose listed sidecar file is missing, so the committed `tauri.conf.json` does not
list it. `scripts/release/build-mcp-bridge.mjs [--target <triple>] [--debug]` builds the bridge and copies it to
`crates/athenaeum-tauri/binaries/athenaeum-mcp-<triple>[.exe]` (gitignored), the name Tauri wants; the pipeline runs it with each
build's own target before each `npx tauri build` (`docs/release/README.md`). The bridge lands at
`Athenaeum.app/Contents/MacOS/athenaeum-mcp` (signed and notarized with the app), beside the exe in the MSI and setup exe, and
`/usr/bin/athenaeum-mcp` in the `.deb`. In `tauri dev` it is `target/debug/athenaeum-mcp` (build it with
`cargo build -p athenaeum-mcp`). The configuration names the file next to the app's executable, except on two platforms:

- **Bridge copy.** An AppImage mounts at a new path at every start, and on Windows a client holding the installed
  `athenaeum-mcp.exe` locks it against the installer, so both run a copy at `<app-data>/bin/athenaeum-mcp[.exe]` and the
  configuration names the copy (`mcp_bridge.rs`: `bridge_path_for`, `copies_to_app_data`, `refresh_bridge_copy`). At a start with
  assistant access on, once the catalog is open, the copy is refreshed when `<app-data>/bin/athenaeum-mcp.version` differs from the
  app's version: copied to a temporary name, then renamed over the copy. A copy a client has locked stays as it is (and its version file
  with it) and is refreshed at a later start; the handshake is versioned, so a bridge one version behind still works.

The web image has no bridge (the Dockerfile drops the crate from the workspace). The bridge is versioned with the app (`bump.sh`,
`check_versions.sh`).

## Plans (the store and the apply framework)

`mcp/plans/` (`McpHost.plans`). A plan is what an assistant prepares and only the user applies, in Athenaeum's window. Nothing an
MCP client can call applies or cancels a plan: `PlanStore::answer` is reached only from the window's two commands, `get_mcp_plan {planId?}` (the plan by id, the newest when absent, `null` when there is none: a page that opens later finds the waiting plan) and `answer_mcp_plan {planId, answer}` (`api/mcp.rs`, both hosts, `commands/mcp.rs` / `routes/mcp.rs`); `no_tool_applies_a_plan` pins that no tool name or input schema can decide a plan, and is re-run with every planning tool.

| Item | Where |
| ---- | ---- |
| The store: `begin`, `progress`, `publish`, `fail`, `view`, `answer`, `plan_opened_text`, `PLAN_TTL`, `PLAN_ITEM_CAP` | `plans/mod.rs` |
| The DTOs the window reads (`McpPlanView`, `PlanState`, `PlanAnswer`, ...), `DiskStamp`, `ItemSnapshot`, `PlanContent` | `plans/model.rs` (TS in `src/types/mcp.ts`) |
| The generic selection check and the re-check at apply | `plans/apply.rs` |

- **One undecided plan.** `begin` makes the old `preparing` / `waiting` plan `replaced` (`replacedBy` = the new id, its cancel flag set). A
  replaced, cancelled, expired or applied plan stays readable by its id for 24 h (at most 100 finished plans). `view(None)` is the newest plan
  that is `preparing`, `waiting` or `applying` (a page that reloads during an apply finds it, even when a newer plan failed), else the newest plan.
  A failed apply always carries a report (an empty one says nothing was done; `plan_status` words it by the plan's kind, see the `plan_status` row); only a plan that failed while it was being prepared has none. Turning assistant access off does not end a waiting or preparing
  plan: the user still decides it in the window.
- **Expiry is on tokio time.** `publish` stores a `tokio::time::Instant` deadline (`PLAN_TTL`, one hour); the sleeper and the apply check
  both read it, so a test pauses time. `expiresAt` is display text (local time, `YYYY-MM-DD HH:MM:SS`).
- **Apply.** `Apply` only from `waiting`, unexpired. The generic check (known groups and items, no locked item included, `chosen` a
  candidate, something selected), the kind's `PlanApplier::validate` and its `PlanApplier::ready` run first (`ready` is the kind's resource: an `Err` refuses the Apply in words as a `Conflict` and the plan stays `waiting`, so it is never burnt; `apply_while_a_batch_runs_keeps_the_plan_waiting`); then the plan is `applying` and a thread re-stamps every
  included item and each group's kept copy (`disk_stamp`; a zip-resident row stamps its zip, `files.archive_zip_path`; paths are never
  `canonicalize`d) and re-reads the catalog values captured at planning (`catalog_values`). A changed item is skipped with its reason
  (`changed on disk since the plan (size A → B)`, `missing`, `catalog changed: OBJECT is now 'X'`); a changed or missing kept copy skips its
  whole group. The applier acts on what is fresh; the report merges both sets of skips.
- **Events.** `mcp-plan-updated {"planId"}` on begin (and for the plan it replaced), publish, fail, apply start, applied, cancel and expiry;
  no other name, and `bring_to_front` once per `begin`.
- **Logging.** Span `mcp_plan_apply` (`plan_id`, `kind`), ending `info!(via = "mcp", plan_id, kind, applied, skipped, failed, "mcp plan applied")`.
  `log-mcp` lists it as an operation keyed by `plan_id`.

## Black Hole plans (`plan_black_hole`)

`mcp/plans/black_hole.rs`. An assistant names groups (`dup:<key>:<hash>` from `find_duplicates`, `rebuilt:<id>` from `find_rebuilt_masters`);
only those groups enter the plan. The handles are read before the plan begins, so a mistyped or stale handle (or the other source's) is an
`Invalid` in words and never replaces the plan the user is looking at. So does a request that is too large: `resolve` sums the distinct copies
of the named groups and refuses more than `PLAN_ITEM_CAP` (2 000) in words before `begin`, so no file is stamped and the waiting plan stays.

- **One keeper per group** ("Copy that stays", `chooseOne`): `chosen` is the one copy under `keepInFolder` (separator-strict: the folder or
  anything below it, never a folder that merely starts like it) or among `keepFiles` (`file:<n>` or the stored path). None or several: `chosen`
  is null, nothing is selected and the group's note says `no instruction picks the copy that stays here (2 copies under <folder>)`. The other
  copies are include/exclude items, selected iff a keeper is set. The kept copy is a choice, not a lock: it is shown by `chosen` alone, the
  planned keeper is an ordinary candidate item (not selected, not locked), and the window may keep another copy and include the planned one
  (the apply byte-checks that pair first). A file sits in one group only: a file already placed in an earlier group is left out of later ones,
  with a note; `validate` refuses a selection that includes any group's `chosen`.
- **Locked, with the reason (only real reasons):** a master with calibration links (`calibration links point at it; it stays in v1`, counted by
  `rebuilt_copies::master_link_count`, the one link count: light frames plus master sets that use it, such as a master dark behind a master
  flat with no light yet); a copy whose master set superseded raw frames (`raw frames were superseded by it; it stays in v1`: moving it would
  un-supersede them, `master_unregister`); a zip-resident copy (`inside a zip; a zip moves to the Black Hole only as a whole`); a copy whose
  file cannot be stamped (`not reachable ...`); a byte-checked copy whose bytes differ from the keeper's, whichever key matched it (`differs in bytes from the copy that
  stays`, with a group note). A group whose every copy sits in an earlier group of the plan is titled `group whose copies are in another group of this plan`.
- **The answer is the plan, as data and in words** (`describe_plan`, one function for the planning answer and for `plan_status` of a waiting
  plan): per group its title, the copy that stays (`chosen`: file name and folder), and `items` with `fileName`, `folder`, `values` (the
  columns), `selected`, `locked` (the reason or null) and `candidate`; the text lists, under each group, the copy that stays and every other
  copy as `moves`, `not selected` or `locked: <reason>`. At most 50 groups are shown with their items (the rest summed) and at most 20 items
  per group (the rest summed).
- **Byte checks** (`Header`, `Content` and `Master` groups; a rebuilt master is not a copy of its keeper's bytes and is not checked): a stored
  full-file hash may predate an in-place overwrite, so the disk decides, and `files.content_hash` samples 3 x 512 KB. Each unlocked copy is
  compared with the keeper by `verify_duplicate_pair`, which answers from the stored hashes when both files still match their rows (about a
  stat) and reads the bytes otherwise. The evidence says which: `byte-identical to the copy that stays (compared just now)` for a byte compare,
  `same full-file hash as the copy that stays, files unchanged since` for a stored-hash answer (`VerifyFn` returns the pair's method). The
  tool answers right after the plan begins, with the handle and state `preparing` (`Athenaeum is checking N copies byte for byte; ... Ask
  plan_status.`), because a client cancels a tool call after about a minute and a cancelled call gives the assistant no handle. The checks run
  on a thread of their own, progress per pair, the plan's cancel flag read between pairs (a plan the user cancels or a newer plan replaces
  stops there); when they end the plan is published (`waiting`). A plan with nothing to byte-check is published before the tool answers. The
  worker enters the caller's tokio runtime, so the published plan gets its expiry timer; an error or a panic while preparing (on the tool's thread or
  the worker) ends the plan `failed` with a sentence, because `sweep` never expires a preparing plan. Both halves of the answer report the state taken
  before the worker starts (`preparing`, 0 of N). No
  reader guard and no pooled connection is held across a comparison. A byte compare that finds a pair identical banks the pair's
  full-file hash (`files.strong_hash`, `updated_at`), as the Duplicates page's verify does, so planning writes that cache and nothing else
  (M2 smoke, 2026-11-20: 11 rows).
- **A master moved to the Black Hole leaves the master library.** The move is the app's own (`bulk_move_to_black_hole`), which unregisters
  the master's calibration set first (`master_unregister`), as the Black Hole flow does from the file browser. Only a master that no
  calibration link points at reaches a plan's ticks, so no light or master loses its calibration (M2 smoke: 2 sets and their frame rows went).
- **Apply** (after the store's stamp re-check of the included items and the kept copy): a byte-checked group whose `chosen` is not the copy the
  plan compared against is compared again first (a differing pair is skipped); then, immediately before the move, a group whose keeper is in
  the Black Hole now is skipped as a whole, every master item is re-checked for links and for superseding (skipped with the lock's reason) and
  an item already in the Black Hole is skipped. The rest goes through `api::black_hole::bulk_move_to_black_hole(ctx, ids, "assistant", None,
  emitter)`: one `blackhole-changed` with `count` = moved, no per-file progress events; `BulkMoveResult.failed` fills the report's `failed`.
- **`validate`** is in memory (no SQLite, so the hosts' `answer_mcp_plan` wrappers need no `spawn_blocking` for it): an included item needs a
  `chosen` (`pick the copy that stays ...`) and `chosen` is never included, in its own group or any other.
- Columns: IMAGETYP, OBJECT, FILTER, EXPTIME, DATE-OBS, INSTRUME, size, modified.

## Metadata plans (`plan_metadata_fill`)

`mcp/infer.rs` (pure: what the evidence says), `mcp/plans/metadata.rs` (the plan and its applier), `mcp/tools/fill.rs` (the tool). An
assistant asks Athenaeum to fill the object, camera, date or frame type frames miss (`fields`: `object`, `camera`, `date`, `frame_type`;
empty means all four, or only the statement's field when a statement is given; coordinates are the plate-solve plan's). Only a frame of
the scope that misses the field is proposed for, by the Missing Metadata view's own test (`missing_flags`: the object only for lights);
a frame whose stored value is not empty is left alone even where the parser could not read it.

- **Evidence, each item says which.** Frame type: walk up from the file; the nearest folder whose name contains any kind word (`light`, `dark`,
  `flat`, `bias`, `offset`) decides. A name that is exactly `light(s)`, `dark(s)`, `flat(s)`, `bias`/`biases`/`offset(s)` or
  `darkflat(s)`/`flatdark(s)` (case-insensitive) gives that kind, `folder 'Flats' in its path`; an inexact one (`DARK_300s`) gives no proposal,
  `folder 'DARK_300s' names a frame type only in part`, whatever a folder further up is named (`Lights/2024/DARK_300s`;
  `the_nearest_folder_decides_the_frame_type`).
  Date: the file name holds `YYYY-MM-DD[_T ]HH[-_:]MM[-_:]SS` or `YYYYMMDD[-_]HHMMSS` (hand-parsed, year 1990-2100, a real date, not part of
  a longer run of digits), `from the file name '<name>'; the camera clock's time zone is unknown, taken as UTC`. Camera and object (D9):
  the other frames of the same folder (the folder itself, not below it; Black Hole files do not count) that have the field set: at
  least 3 and all equal gives `40 frames in this folder have INSTRUME = 'ZWO ASI2600MM Pro'`; disagreeing frames give no proposal,
  `frames in this folder disagree: 30 'A', 10 'B'`; fewer than 3 give `fewer than 3 frames in this folder have it`. Neighbours are read
  for the folders of the missing frames only, outside the scope too. The object is counted from lights only (a flat's `OBJECT` is not the
  target; `flats_never_name_a_lights_object`); a neighbour with no frame type (`imagetyp` NULL) is not a light and does not count toward an object. A frame that is a master (`is_master`, set by the parser from the file name) keeps its master
  type: the proposal is `MasterDark` / `MasterFlat` / `MasterBias` / `MasterDarkFlat` / `MasterLight`, evidence `folder 'Darks' in its path; the
  file is a master`, because the catalog derives `is_master` from the type text and `Dark` would turn the master into a raw frame
  (`a_master_in_a_darks_folder_stays_a_master`).
- **The user's word.** `statement {field, value}` covers the frames in the scope that miss that field; evidence `by your instruction`.
  The value is normalized before any plan begins (a frame type to `Light` / `Dark` / `Flat` / `Bias` / `DarkFlat`, or its master kind for a
  master; a date `YYYY-MM-DD HH:MM:SS` or `YYYY-MM-DDTHH:MM:SS`, UTC) and a bad one is an `Invalid` in words. A date without a time is
  refused (`give the time too (YYYY-MM-DD HH:MM:SS): the night a frame belongs to is decided by its time`, as the Set Date modal does: a
  midnight UTC would land on the previous night; `a_date_without_a_time_is_refused`). **Reach:** with no `scope`, a statement (and each
  inferred field) covers every frame of the whole catalog that misses the field, up to the 2 000-item cap; the tool's description says so. With a statement and no `fields`, only the statement's field is
  planned; `fields` are inferred in addition, except the statement's own.
- **The plan.** One group per (field, value), largest first within a field, its items selected (`item:<frame id>:<field>`; columns
  Field / Now / Proposed). A frame that misses a field and has no evidence is a locked item in a group `<Field>: nothing proposed`, with the
  reason. A file that cannot be stamped is locked `not reachable ...`. In the answer's text a group's line counts the frames that can be filled when some are locked (`- Object: LDN 1272: 0 of 5 frames can be filled now`; every item fillable keeps `N frames`; the `nothing proposed` group keeps its plain count; `a_group_with_every_item_locked_says_none_can_be_filled`). Notes: `these changes take N frames out of their sessions and M out
  of calibration links` (`count_frame_metadata_relations`, as the metadata editor's confirm warns; only when a link goes) and that only
  the catalog changes. A date is shown `YYYY-MM-DD HH:MM:SS` in the item, its action and the group title (`Proposal.value` keeps the `T` form), and its group is
  per (field, folder), titled `Date: <folder> (<first> to <last>)`, each item with its own time (`dates_are_grouped_by_folder`); the apply
  still writes equal values as one batch.
- **Refused in words before the plan begins** (the waiting plan is never replaced): a scope that names nothing (the scope's own hints), a
  request with nothing to propose (what was missing and why, and that the user's value goes in `statement`), a plan of more than
  `PLAN_ITEM_CAP` (2 000) items, a bad statement.
- **The answer is the plan** (`metadata::describe_plan`, also what `plan_status` says of a waiting metadata plan): `N values proposed in M
  groups`, per group its title and how many frames, each frame by name and folder with its evidence or `locked: <why>`; the data carries the
  same as `groups[].items[]` (`fileName`, `folder`, `values` current and proposed, `selected`, `locked`, `evidence`), at most 50 groups and 20
  items per group, the rest summed (`otherGroups`, `itemsNotShown`).
- **Apply** (after the store's stamp and catalog re-check: an item whose field was filled meanwhile is skipped `catalog changed: INSTRUME is now
  'X'`): the applier re-checks the field is still empty (the object: still a light) on a pooled connection it releases, groups equal edits
  and writes each group with `api::files::bulk_update_frame_metadata` (the Missing Metadata page's own write: catalog only, `frames.override =
  1`, so the scanner's re-parse keeps the value - `fill_sets_override_and_survives_rescan` - and the links and session seats of the frame go).
  A date is stored `<value>Z`, as the Set Date modal stores it. One `frames-metadata-changed {"count": <frames>}` follows when anything
  was written; the closing line is `info!(via = "mcp", plan_id, count, "mcp metadata fill applied")`. A write that reached fewer rows than it
  named (a frame deleted between the re-check and the write) is not reported applied: the applier looks the frames up again and reports those no
  longer in the catalog `failed`, `not in the catalog any more` (`a_frame_deleted_before_the_write_is_failed`).

## Plate-solve plans (`plan_plate_solve`, `solve_status`, `cancel_solve`)

`mcp/plans/plate_solve.rs` (the plan, its applier, the host's run log), `mcp/tools/fill.rs` (the three tools), core's `api::plate_solve` (the
slot, the run, the cancel). `cfg(all(feature = "render", feature = "solver"))`, as `api::plate_solve` is: `--no-default-features` builds without
the module, the three tools and `McpHost::solves`, and its tool list simply lacks them.

- **Items** are the raw lights of the scope (a master or another type is left out and counted: `N frames are not a light frame and are left out`);
  with `onlyUnsolved` (the default) only lights that miss coordinates by the Missing Metadata view's own test (`missing_flags`), the others counted
  (`N light frames already have coordinates and are left out (onlyUnsolved)`). One group per folder, each item with Object / Filter / Exposure /
  Coordinates. An item is locked (never ticked) with the reason: no star catalog (`the star catalog is not installed (Settings → Plate Solving)`,
  every item), the input gate (`service::input_gate_reason`, the stored `trailed` verdict at the current `MEASURE_VERSION` while
  `input_gate_enabled`), or a file that cannot be stamped (`not reachable ...`).
- **Notes**: `N of M frames can be solved`, the refusals by reason, `star catalog: installed|not installed`, `quad index packs: K installed`,
  `runs X frames at a time` (the configured `batch_concurrency`, else `(cores / 3).clamp(2, 8)`), and the left-out counts. The last note says what a solve writes besides the coordinates (`NAMES_THE_OBJECT`, also in the `plan_plate_solve` tool description,
  `the_solve_plan_says_it_may_name_the_object`): `A solved frame gets its coordinates; a frame with no object name also gets the name of the catalogued
  object the solve finds in its field, as a solve started in Athenaeum does.` (`store_result` fills `frames.object` through `update_frame_object_if_missing`.) The answer is the plan
  as data and words (`describe_plan`, also what `plan_status` says of a waiting plate-solve plan), at most 50 groups and 20 items per group.
- **Refused in words before the plan begins** (the waiting plan is never replaced): a scope that names nothing, no light to solve, a plan of more
  than 2 000 items (counted from the scope's rows before any frame is read, `an_unscoped_request_over_the_cap_reads_no_frame`), and a plate-solve batch already running: `BUSY_ASSISTANT` when the running run is the assistant's, `BUSY_UI` when the user
  started it in Athenaeum (`planning_refused_while_a_ui_batch_runs`).
- **Applied means started.** Apply re-checks each file and the frame's type and coordinates (the store), resolves the catalogs again, claims the slot
  with `prepare_plate_solve_batch(.., Assistant)` and starts `run_plate_solve_batch` on a thread named `mcp-plate-solve` (never the store's apply
  thread). The plan ends `applied` with `report.runId = solve-<uuid>` and `plan_status` says `started a plate solve of N frames (run solve-...);
  ask solve_status for its progress`, never that frames were solved; its report counts `started`, `skipped` and `could not start`. A batch already
  running when Apply is pressed refuses the Apply itself (`PlateSolveApplier::ready`, a `Conflict` with the filled `BUSY_*` text) and the plan stays
  `waiting`. A batch that takes the slot between `ready` and the start (or a catalog removed meanwhile) starts nothing: every selected item is a
  `failed` line with the reason, and the plan ends `failed` (`PlanApplier::start_failure`, the first failure's reason), so the window shows a
  failure and `plan_status` says `Nothing was started: <reason>.` once, never "Some files may have changed". Log: `info!(via = "mcp", plan_id, run_id, count, "plate solve run started from the assistant")`.
- **`solve_status`** reads the handle's progress while the slot holds the run, and the snapshot the host keeps (`SolveRuns`, the last 20 runs) after.
  The snapshot counts from the run's own result: the solved and failed counts are the `plate-solve-complete` event's, never the progress events'
  (a failed store or COMMIT sends no event; a run that returns an error solved nothing; the handle's progress holds no solved count, so a running status never promises one). A failed frame is
  named with its file name and folder (the run's emitter records each failed frame), `failedWithoutFile` counts failures no event named, and
  `objectNamed` counts the frames of the run that had no object name when it started and have one when it ends (read from the catalog by frame id in
  batches, not per frame; `null`, and no words, when the catalog could not tell; `the_final_status_counts_the_frames_that_got_an_object_name`), and
  `notAttempted` is what a cancel left (`status_reports_progress_and_result`, `the_final_snapshot_counts_from_the_runs_own_result`). A run whose
  cancel flag was set when it ended, without `cancel_solve` having set it, was cancelled in Athenaeum's window: the status says `The run was cancelled
  in Athenaeum; N frames were not attempted.` and carries `cancelledInApp` (`a_cancel_from_the_app_is_named_to_the_assistant`).
- **`cancel_solve`** goes through `cancel_plate_solve_run` (check and set under the slot's lock) and only for a run in the host's own log, so
  the window's run is never touched whatever id is named (`cancel_solve_stops_only_its_run`).
- **The window's events name their run.** `plate-solve-progress` and `plate-solve-complete` carry `run_id` (the `solve-<uuid>` that
  `plate-solve-started` and the activity carry; snake_case like the rest of those payloads; `progress_and_complete_name_their_run`), and the
  `cancel_plate_solve` command (both hosts) takes an optional `runId`: with one it is `cancel_plate_solve_run` (acts only while slot 0 holds that run,
  `cancel_with_a_run_id_spares_another_run`), without it the old cancel of whatever holds the slot. The object autofind's events are unchanged.

## The plan window

`src/components/mcp/{PlanWindow,PlanView,planSelection}`, state in `src/contexts/McpPlanContext.tsx` (provider and window sit in `Layout.tsx`, once).
The window is the one place a plan is looked over and decided: ticks, the copy that stays, **Apply** or **Cancel**. A `DialogShell size="lg"`.

- **Reads.** On mount `get_mcp_plan({ planId: null })` (a page opened later finds the plan that waits; the key is always sent); on every
  `mcp-plan-updated {planId}` `get_mcp_plan({ planId })` (required listener pattern; progress events come about every 250 ms). Only an undecided
  plan (`preparing`, `waiting`, `applying`) opens a closed window; an event for a finished plan does nothing.
- **Never swapped under the cursor.** An event for another plan while one is shown only records that a newer one waits; the shown plan stays
  as it was. When the shown plan becomes `replaced` it shows the banner "The assistant prepared a newer plan" and **Open the new plan**
  (Apply disabled); while a plan applies, a newer one waits unseen. Close on a finished plan opens a waiting newer plan, if there is one.
- **No keypress or stray click answers a plan.** No button has the initial focus (the title line does, `data-autofocus` + `tabIndex={-1}`):
  the window opens by itself over any page. A scrim click never dismisses the window in any state (`DialogShell closeOnScrim={false}`;
  the report is the only place the user sees what moved); Escape and the cross send `cancel` while a plan is `preparing` or `waiting`.
  Apply arms one second after the window shows a waiting plan.
- **An applying plan never locks the app.** While it applies, Escape, the cross and a **Close** button hide the window and the apply carries
  on; the footer says "Applying… you can close this window; the report opens when it is done." The hidden plan stays tracked, and when it
  reaches its end state (`applied`, or `failed` with its report) the window opens again with the report. That report is the one outcome of
  the user's own click, so there is no `notify()`. A newer plan waits unseen meanwhile and opens when the report is closed.
- **Events are not trusted alone.** The web build drops events (an EventSource reconnect, a lagged broadcast). While the tracked plan is
  `preparing`, `waiting` or `applying` (shown or hidden) the context re-reads it with `get_mcp_plan({ planId })` every 3 s and once when the
  tab becomes visible; the timer ends with the plan and on unmount. The re-read never swaps the shown plan for another id.
- **States.** `preparing`: `done of total` and Cancel; `waiting`: groups, totals ("N files · X GB"), Cancel, Apply (disabled while
  `selectionProblems` is non-empty, the problems are listed beside the totals); `applying`: read-only, only Close (it hides the window; above); `applied`: the report
  (applied / skipped / failed with each reason) until dismissed, also for a plan that `failed` while applying; `replaced`, `expired`, `failed`: state text and Close.
- **Selection rules** (`planSelection.ts`, pure): the initial selection is the plan's `selected` items and `chosen` copies; a locked item can never be
  ticked, but it may be the copy that stays (a lock forbids moving a copy, not keeping it; only a non-candidate cannot be chosen); the kept copy (the group's current `chosen`) is never included, and choosing a ticked copy unticks it; a group with
  ticked items and no `chosen` is "pick the copy that stays in <group title>". The selection starts over once, when a `preparing` plan turns
  `waiting`. Outside `waiting` the window shows the plan's own selection (`initialSelection(plan)`, what the backend recorded), never its local
  ticks, so a second page sees what another page applied. A group with some copies ticked shows half-ticked; one click unticks them all.
- A failed command is logged (`console.error`) and raises one `notify()`; the window stays. No notification for progress.

## Logging

New log fields: `via`, `plan_id`, `kind`, `applied`, `skipped`, `failed` (plans, above) and `tool` (the tool's name on its span: `catalog_overview`, `query_frames`, `file_details`, `find_duplicates`,
`find_missing_metadata`, `plan_status`, `find_rebuilt_masters`, `plan_black_hole`, `plan_metadata_fill`, `plan_plate_solve`, `solve_status`, `cancel_solve`). A failed tool is logged by its span's `err`, then returned to the assistant in words. The channel (`mcp::lifecycle`) adds no field
name: it logs with `state` (`off`/`starting`/`serving`/`elsewhere`/`failed`), `addr` (the endpoint's text), `reason`
(`timeout`, `too_long`, or a handshake refusal's word), `error` and `path`; never the key.

## v1 rulings and deferred items (2026-11-20/21)

v1 was built by plan `docs/superpowers/plans/2026-11-19-athenaeum-mcp-v1-plan.md` (Tasks 1-19, three milestones), each
task reviewed, each milestone reviewed whole and smoked headless on a copy of the dev catalog. The decisions taken on
the owner's behalf during the build, and the findings left for later, are kept here verbatim from the build ledger
(task numbers T1-T19 are the plan's; P1-P10 are pre-flight rulings). Each ruling ends with what it costs if wrong.

### Rulings

- P1 Ruling: `night_of_date_obs` widens as T1 says, and collab_exchange uses the widened fn — a frame from another member with a space-separated or date-only DATE-OBS now gets the same night the calendar's SQL gives it; collab tests must stay green, and a collab golden/wire pin that moves is a STOP for that task — cost if wrong: one collab frame per such form lands on a night instead of none.
- P2 Ruling: each of T2/T3/T4 derives, in its own task, `Serialize` (+ `Deserialize`, `rmcp::schemars::JsonSchema` with `#[schemars(crate = "rmcp::schemars")]` on enums used as tool arguments: `GroupKey`, `QualityMetric`, `MissingCategory`) and `#[serde(rename_all = "camelCase")]` on output structs, so T5 only adapts — cost if wrong: T5 adds the derives itself.
- P3 Ruling: T2 may enable rusqlite's `trace` feature through `[dev-dependencies]` only (same version 0.40); if that drags a second rusqlite or libsqlite3-sys, count statements with a `#[cfg(test)]` counter inside `fetch_scoped_frames` — cost if wrong: a weaker statement-count test.
- P4 Ruling: `find_missing_in_scope(conn: &Connection, scope: &ResolvedScope, cats: &[MissingCategory], page: u32)` takes the reader connection (R11); the other ctx-taking fns stay as planned — cost if wrong: one signature change.
- P5 Ruling: T5 adds `McpHost::on_catalog_open(self: &Arc<Self>) -> Result<(), ApiError>` that opens the read-only reader (and nothing else); T6 extends that same fn with the KV reconcile and the bind — cost if wrong: none (T6 owns the full body).
- Ruling: a second `darling` (0.24 via rmcp-macros beside 0.23 via serde_with ← tauri-utils) is accepted — both are transitive proc-macro helpers neither of which we pin, and unifying needs a tauri or rmcp change; the plan's "new crates are only rmcp's own" was wrong (also schemars_derive, serde_derive_internals, unicode-*) — cost if wrong: a few seconds of compile, no runtime type crosses.
- Ruling: implementers use their own harness's trailer lines (brief.md fixed); abcfe1e5 carries the Opus line, left as is — cost if wrong: one commit's model credit.
- T1 review (opus): Needs fixes — 2 Important (fixture drops the TempDir before the pool; `frame_type` ignores `frames.is_master`). Rulings on the minors that touch the plan text or the constraints (they join round 1): soft_archived follows the brief (used copy only) — cost if wrong: a non-used copy in an archived set reads `catalog`; scan root by exact equality (Global Constraint); malformed night/date → error in words; `Scope` gets `deny_unknown_fields` (a misspelled field must never widen a plan's scope); `biases`; test hygiene.
- Ruling P6 (for T2 onward): a frame with `frames.is_master = 1` is a master everywhere the scope says so — T2's frame-type grouping, `FileLine.imagetyp` reading, `Totals.lights` and `integration_s` (Σ raw LIGHT exptime) treat it as the master kind, never as a raw light — cost if wrong: a filename-detected master light counted as integration.
- Ruling P7: `frame_set_totals.integration_s` = the page's live figure (equals the page; the owner's «как на странице»); `black_hole_in_total` is replaced by `black_hole_left_out: Option<(usize, f64)>` — the set's members now in the Black Hole (count, exposure) — so the answer says "N frames of this set are in the Black Hole and not counted (X)", the honest form of the owner's «пометка про Black Hole» — cost if wrong: one field and one sentence.
- T2 review (opus): Needs fixes — 1 Important (P7 field swap; the old `black_hole_in_total` could also misfire for a non-light member with exposure). Ruling: minors 2-4 join round 1 (one spelling for a missing filter is user copy; the Windows path branch must run on every OS — cross-platform rule; a hand-written state list fails at run time) — cost if wrong: a few lines in a round that runs anyway.
- Ruling P8: `FileDetails.calibration` becomes objects `{kind, handle, fileName, folder}` (the master as a file the model can ask about by handle), not the brief's tuple — the Global Constraint wants files named by file name and folder in the structured half too — cost if wrong: one struct.
- T3 review (opus): Needs fixes — 2 Important (a fixture path fails on Windows; `QualityScore` is never written by any production path, so its "run Analysis" reason is false — controller confirmed analyzer.rs:136 stores None). Ruling: drop `QualityScore` from `QualityMetric` — cost if wrong: re-add when Analysis writes it. Ruling: minors 3 (transparency reasons are the app's own; the action sentence only where an action cures it), 4 (file details keep the transparency reason), 5 (`analysis_current`), 7 (calibration sets by the used copy) and 8 (worst first pinned) join round 1 with P8 — they decide whether an answer is true — cost if wrong: a few lines.
- T4 review (opus): Needs fixes — 1 Important (findings test fixture drops the TempDir before the pool). Ruling: the processing-folder note reads singular for one file (copy), and a Black Hole test for it joins round 1 (its doc claims it, nothing pins it) — cost if wrong: two lines. Round 1 dispatched (fresh sonnet; the first ended at ~197k).
- Ruling: group objects with a `handle` carry their first copy's `fileName` and `folder` (the every-handle rule) — cost if wrong: one field pair per group.
- T5 review (opus): Needs fixes — 1 Important (reader guard held across ctx calls in `query_frames` / overview). Ruling: minors 1 (docs claim every read uses the reader — false), 2 (test hosts outlive their temp dir — Windows), 4 (a copy's scan root only in the structured half — the both-halves rule) and 3-in-part (pin the 50-folder cap) join round 1 — cost if wrong: small. Round 1 dispatched (fresh sonnet; the first ended at ~270k).
- Ruling P9: the copied client config always passes `--app-data-dir <dir>` (R8: the bridge never guesses; the app knows its own dir) — cost if wrong: a longer config line.
- T6 review (opus): Needs fixes — 1 Important (Windows: the waiting pipe instance drops asynchronously, so off→on reads `elsewhere` and three cross-OS tests would fail on Windows CI). Ruling: minors 2 (regenerate in `elsewhere` rewrites the other instance's key), 3 (a test host outlives its temp dir), 4 (`fail()` leaves a stale endpoint) and 5 (mcp dir 0700) join round 1 — cost if wrong: small. Found: the Mac has `x86_64-pc-windows-gnu` + mingw, so round 1 runs a Windows cross-check of core (first on the unchanged code). Round 1 dispatched (fresh sonnet; first ~235k).
- Ruling P10: "another instance of this build serves" is decided by an exclusive lock on `<mcp>/serve.lock` (`std::fs::File::try_lock`, released by the OS at process death) on every OS, not by probing the socket/pipe; Unix removes a leftover socket under the lock; Windows binds a fresh pipe name per bind (`…-<8 hex>-<8 random hex>`, written to `endpoint`), so leftover instances never block a re-bind (also closes the squat note); the 30 s check recovers from `elsewhere`; docs say an initialized session closes when rmcp's task runs — replaces the plan's D1 first-instance detection — cost if wrong: one more lock file per build dir and a T7 bridge that re-reads `endpoint` on every connect (it does by design).
- T7 review (opus): Approved with 1 Important (the release skill still counts eight paths; nine now). Ruling: minors M1 (a raw-line test — the only proof of "exactly one answer" and the Review Focus "app quits mid-conversation"), M2 (two silent failures — CLAUDE.md), M3 (non-UTF-8 line ends the bridge — the plan's "byte pipe") and M4 (`last_try` set before connect stalls every request behind a hung app) join round 1 — cost if wrong: small. M5 (unbounded queues) accepted. Round 1 dispatched (fresh sonnet; the first ended at ~239k).
- Ruling: an open SSE GET stream outliving off/regenerate is accepted for v1 — every request (each question is a POST) is refused at once, and the GET stream carries only server notifications, no catalog data; docs/mcp/README.md must say so (T8 fix round or T10) — cost if wrong: a revoked web client keeps receiving `list_changed` notices.
- T9 review (sonnet): Needs fixes — 1 Important (the web-blocked how-to is said twice: the section appends its own sentence to `blockedReason`, which already carries it; the test fed a made-up reason). Ruling: the two inline copies of the tab rule in Objects.tsx (filter switch stage/archive, Purge counts) call `frameSetTab` (R5's pin must cover what the page runs), and the `fs` shim gets its delete-me comment — join round 1. Round 1 dispatched (fresh sonnet frontend-dev; a fresh small context is cheaper than resuming at ~124k).
- Ruling (T10): the local packaging proof is `npm run tauri build -- --debug --bundles app --config crates/athenaeum-tauri/tauri.mcp-bridge.json` into `target-agent` with `APPLE_SIGNING_IDENTITY=-` (ad-hoc; no signing identity in tauri.conf, no APPLE_* env here, so no keychain prompt, no notarization); the release build is CI's — cost if wrong: the release-profile path of the overlay is first proved by the tag pipeline.
- T10 review (sonnet): Needs fixes — 1 Important (Windows PowerShell may eat `--` → installers silently without the bridge). Ruling: `npx tauri build --config …` at all four sites (one form); cheap bundle guards on macOS (.app MacOS dir) and Linux (deb lists usr/bin/athenaeum-mcp), Windows relies on the npx form; the script's log line and the release skill sentence join round 1 — cost if wrong: four CI lines. Round 1 dispatched (fresh sonnet).
- Ruling: M1 merges to the trunk on its own, so until M2 its text says it only reads (INSTRUCTIONS, Settings description and help — exact strings in `m1-fixwave-handoff.md`); T13's dispatch must restore the plan's INSTRUCTIONS and the Settings copy (§6.4) — cost if wrong: two string changes back in M2.
- M1: parked — a `failed` instance (I/O error in engage) turning off removes the serving instance's `enabled` marker and `tools.json`, so bridges say "off" until A is toggled or restarted — Ruling: real, rare (needs a lock-file or token I/O error while another instance serves), the old rule had it too; fix at M2's first lifecycle touch (remove them only after acquiring the serve lock, or the serving recheck rewrites a missing marker).
- M1: parked — `stored_enabled` uses `Database::conn()`, which panics on a pool timeout and would end the 30 s watch task — Ruling: use `try_conn` at M2's first lifecycle touch; unlikely with 8 connections and a 30 s timeout.
- M1: parked — an instance switched off by the other has no watch, so it keeps showing off after the other turns access on and quits, until restarted — Ruling: LATER (Settings re-read on focus would cover it, T9 deferred minor).
- M1 real-data smoke (sonnet, copy of the dev catalog: 40 535 files/frames, 47 BH, 82 sets): 11 of 11 cases equal the app or SQL value for value; digest unchanged across all five tools, the error cases and off. Findings: (1) whole-catalog `calibration:true` 38 s cold, 4.2 s warm, 100+ "flat-dark exposure mismatch" WARNs per call — the app's own hierarchy + readiness for the page's 25 sets (the code pages BEFORE computing; checked) — Ruling: accepted for M1, told to the owner; revisit if a client times out; (2) quality success path not exercised (the dev catalog predates MEASURE_VERSION 8: every analysed frame answers "older version", counts match SQL); (3) the web host's own startup migration (`stacking.migration.frame_level_keys`) changed the copy before MCP was on — app behaviour, not MCP; (4) two frame sets named "LDN 1272" → plain totals (372) instead of each page's figures, no hint — FIX before merge (`m1-smoke-fix-handoff.md`); (5) `get_mcp_status` still returns the web token after off (behind the API key; the UI hides it when off) — LATER; (6) the agent's first build wrote a 3.5 GB `target/` in the worktree, removed (checked: no `target/` there now).
- Q1 Ruling: the two M1 lifecycle parks go into T13's dispatch as two small extra items (T13 is the host/commands task; both are a few lines in `lifecycle.rs` with one test each) — cost if wrong: T13 a little larger.
- Ruling: T15 also restores the plan's Settings copy (description "Let an AI assistant on this computer read your catalog and prepare changes that you approve in Athenaeum."; help "…Every change opens here for your approval.") once plans exist — cost if wrong: two strings.
- T11 review (opus): Needs fixes — 1 Important (the module gate keeps the move out of the `--no-default-features` build the ungated MCP module and T14 compile in). Ruling: minors 2 (a public `master_link_count` for T14 + a sub-calibration-chain test), 3 (docs: only the desktop emitter logs), 4 (dictionary wording) join round 1. Round 1 dispatched (fresh sonnet; first ended ~187k).
- T12 review (opus): Needs fixes — 2 Important: a pooled connection held across `applier.apply` (Global Constraint; T14's applier would nest a checkout); `progress` emits no event (T15 would show 0 of 0 while preparing). Ruling: `progress` emits `mcp-plan-updated`, throttled (done == total, else ≤ 1 per 250 ms) — the window stays event-driven — cost if wrong: a few more events. Minors 3 (panics keep the report; a re-check panic never strands `applying`), 4 (omitted groups cleared), 5 (one log line per unreachable volume), 6 (Windows test host lifetime), 7 (race tests) join round 1. Carry to T14: `validate` refuses `chosen ∈ included` and `chosen: None` with choose_one. Round 1 dispatched (fresh sonnet; the first ended ~186k).
- Task 13: ⚠️ Ruling: Tauri 2 deserializes a missing key for an `Option<String>` command argument as `None` — documented behaviour; T15 sends `planId: null` explicitly anyway — cost if wrong: the window's first call errors on desktop, seen at once in the owner smoke.
- Task 13: ⚠️ Ruling: `a_failed_instance_turning_off_leaves_the_serving_instances_files` relies on reading a folder as the key file failing with an error other than NotFound; on Windows std returns PermissionDenied (ERROR_ACCESS_DENIED) — accepted; the GitHub Windows job confirms at push — cost if wrong: one red Windows test.
- Ruling: a planning tool returns right after `begin` when it has byte checks; the checks run detached with progress and cancel; `plan_status` renders the plan's content once waiting; `planId` stays required — §6.2 "a long, cancellable operation" and the measured ~60 s desktop timeout — cost if wrong: one more plan_status round-trip per plan.
- Ruling: the plan's answer lists every copy (≤ 50 groups, ≤ 20 items per group, the rest summed) — §6.4 l.138 — cost if wrong: longer answers.
- Ruling: the kept copy is `chosen` only, never a locked item; locks stay for real reasons — spec l.147/156 "picks one candidate per group" (the brief's "stays" lock was a plan defect) — cost if wrong: T15 draws the keeper from `chosen`, which it must anyway.
- Ruling: a scoped `find_rebuilt_masters` returns whole groups with a copy in scope, handle = lowest id of the whole group — mirrors `find_duplicates_in_scope` — cost if wrong: larger answers for scoped asks.
- Ruling: content-key groups are byte-checked like header groups; only full-file `strong_hash` groups are not — the sampled hash does not decide on bytes, and the spec's rule is that only proven copies move — cost if wrong: a longer preparing for opt-in content-hash catalogs.
- Ruling: masters that another master's links point at stay (check `master_link_count`; count them if it does not) — spec "a copy that calibration links point at always stays" — cost if wrong: one more join.
- Ruling: round 2 also takes the re-review's Minors that change what the assistant or the window reads (the halves' disagreeing state, `linkedLights` → `linkCount` and three stale tool descriptions, a wrong group title) and the outside-scope panic that strands `preparing` — they set the data shape T15 and the smoke read, and are a few lines each — cost if wrong: a slightly larger round.
- Ruling: no button has initial focus (a `tabIndex=-1` header line) — the brief's outcome wins over its mechanism; the window opens by itself and is raised — cost if wrong: one more Tab to reach Cancel.
- Ruling: Escape and the cross stay Cancel (deliberate); a scrim click does nothing while preparing / waiting / applying — the reviewer's weighing: a hidden waiting plan is worse than a rerun — cost if wrong: a reflexive Escape loses a prepared plan and the assistant plans again.
- Ruling: notes ruling 1 amended — a locked copy may be the copy that stays (a lock forbids moving, not keeping; the backend accepts any candidate) — cost if wrong: none on disk; the backend re-compares bytes on a keeper change.
- Ruling: Apply arms 1 s after the window shows a waiting plan — closes the last stray-click path — cost if wrong: one second's wait.
- Ruling: while applying the window can be hidden and the apply carries on; the report reopens the window when that plan ends (no notify) — I1 — cost if wrong: a modal that reopens by itself once.
- Ruling: the window re-reads the shown undecided or applying plan every 3 s and on visibilitychange — the web build drops events on reconnect or lag — cost if wrong: a cheap in-memory read every 3 s per open page.
- Ruling: `Master`-key groups are byte-checked too (`verify_duplicate_pair` answers from stored hashes when the rows still match) — a hash may predate an in-place overwrite — cost if wrong: near zero.
- Ruling: a failed apply always stores a report (empty if nothing ran); `view(None)` prefers an undecided or applying plan; a request over the cap or with unknown groups is refused before `begin` — Minors M1, M3, M4 — cost if wrong: small.
- M2: parked — a re-read that returns null (web server restarted mid-apply) polls every 3 s forever and keeps the tab's window hidden for newer plans until a reload (McpPlanContext.tsx:90-103, :130) — Ruling: real, rare (needs a server restart during an apply), no file at risk; fix with the first M3 frontend touch or a smoke finding's fix — cost if wrong: one stuck tab until reload.
- M2: parked — a failed apply with an empty report shows an empty block, not "No file was recorded as moved" (PlanView.tsx:44-47, 250-258) — Ruling: wording only, plan_status says it — fix with the next frontend touch.
- M2: parked — B1's fixture never reaches the stored-hash branch (modified_at = now), so only api/files.rs tests pin it; a tool dropped from the router by accident fails no bridge test (names now read from the router) — Ruling: LATER.
- Ruling: planning that banks a verified pair's full-file hash (`strong_hash`, `updated_at`) is accepted — the Duplicates page's verify does the same; documented (fbafca66) — cost if wrong: an `updated_at` bump on verified rows before Apply.
- Ruling: a master moved to the Black Hole unregistering its calibration set is the app's own Black Hole flow (only unlinked masters reach a plan's ticks); documented — cost if wrong: a plan's master item does not say so in the window (wording LATER).
- Ruling: planning a plate solve while a batch runs refuses with the running run's own words (`BUSY_ASSISTANT` or `BUSY_UI`, as `prepare` does) — T18's text names BUSY_UI only, which misnames the assistant's own run — cost if wrong: none.
- Ruling: T19 takes the two parked M2 window items — they were parked for "the first M3 frontend touch" — cost if wrong: T19 slightly larger.
- Ruling: the controller fixed that one doc comment itself (a docs-only line; a fix round would cost more than the line) — cost if wrong: none.
- Ruling: an `is_master` frame is proposed its master kind (`MasterDark`, …) — ruling P6 — cost if wrong: none.
- Ruling: a date statement without a time is refused in words, as the UI's Set Date asks for a time — the night is decided by the time — cost if wrong: the user types a time.
- Ruling: Date proposals are grouped by (field, folder), not one group per timestamp — the brief's rule is degenerate for dates and the 50-group cap would hide most — cost if wrong: a group mixes values (each item keeps its own).
- Ruling: `PlanApplier` gains a default `start_failure(&report) -> Option<String>` (None); the plate-solve applier overrides it; `apply::run` returns `Err(Failure { error, report })` so the plan ends `failed` with its report, the panic path's shape — the smallest framework change; the other impls unchanged — cost if wrong: one trait method.
- Ruling: the running `solve_status` drops "N solved so far" (store can still reject or roll back); done / total / current file / failures stay (§6.5) — cost if wrong: one figure fewer mid-run.
- Ruling: the busy notify is keyed per holder run (`plate-solve-busy-<runId>`) — `NotificationContext` keeps seen keys forever; the brief's global key was a plan defect — cost if wrong: one notify per waiting episode.
- Ruling: the assistant's line follows every activity read (3 s re-read while shown); its Cancel re-reads and invokes only when slot 0 holds that run — residual: a run that ends between the re-read and the invoke (milliseconds) could let the cancel reach a batch started in that gap; a run-scoped cancel command would close it (+1 command) — LATER.
- Ruling: an assistant run's end raises one notify (keyed per run) and Missing Metadata reloads after it — spec §6.5 "as if started there", §7 — cost if wrong: one more notification.
- Ruling: the complete event gains its run id in the M3 fix wave (core `api/plate_solve.rs:453` adds `runId` to the payload, the TS type follows, the queue ignores a complete for another run) — a late complete of the holder lands on the held batch that just started (false "finished", the real result lost on web) — cost if wrong: one more field on an append-only event.
- Ruling: `plate-solve-progress` and `plate-solve-complete` gain `run_id` (additive); the queue ignores known foreign ids — the queue cannot otherwise tell its batch from the holder's — cost if wrong: two fields.
- Ruling: `cancel_plate_solve` takes an optional run id (same command, count stays 329) → `cancel_plate_solve_run` — closes the LATER residual of T19 — cost if wrong: one optional argument in both hosts.
- Ruling: `PlanApplier::ready(&host)` (default Ok) is checked before `applying`; a plate-solve Apply while the slot is held is refused in words and the plan keeps waiting — the review's M3: a terminal `failed` the user cannot recover — cost if wrong: one trait method.
- Ruling: frame type comes from the nearest folder that names a kind; an inexact name (`DARK_300s`) makes no proposal — the review's M7 (a far ancestor wrote `Light` with override) — cost if wrong: fewer frame-type proposals.
- Ruling: the assistant line becomes "a run this page did not start" (any origin), with a run-scoped Cancel — the hold notify says "wait for it or cancel it" — cost if wrong: one more line in the panel.
- M3: parked — inside the sub-second window while the queue's start command is in flight: My Frames' progress can land on the queue's batch, and a queue Cancel there may send My Frames' id or no id (which could stop an assistant run) — Ruling: real, needs a start in that window; the next touch makes `plate_solve_batch` return its run id so the queue never adopts — LATER, open-items.
- M3: parked — a batch cancelled while its start command was in flight and then refused stays "Cancelling…" until reload (`usePlateSolveQueue.ts:378`, one `dropIfCancelled` missing) — Ruling: LATER with a test, open-items.
- M3: parked — an assistant cancel racing the run's end can read "cancelled in Athenaeum"; "0 frames were not attempted" wording (`plans/plate_solve.rs:1032, 1100`) — Ruling: wording, LATER.
- Ruling: the smoke's two untruths are fixed before the merge by one small core round (`m3-smoke-fix-handoff.md`): the solve plan says it may name the object (the app's own `update_frame_object_if_missing` wrote "RCW 145" on 5 M8 frames), and a metadata group title counts what can be filled — the smoke's bar is "what a plan says is true" — cost if wrong: one more short round.
- M3: parked — a metadata plan locks an item "not reachable" when its volume is unmounted although only the catalog is written (the M2 stamp rule; the UI's Set Object works offline) — Ruling: LATER, open-items (relax the stamp to catalog values for catalog-only plans).
- M3: parked — `note_named_frames` / `frames_without_object` use `Database::conn()` (panics on pool exhaustion) outside `catch_unwind`, which would leave `solve_status` "saving its results" for good; the named count also counts an object set by the user during the run — Ruling: LATER (try_conn; word it "have an object name now").

### Deferred (LATER, parked, or minor)

- Task 1: minor (deferred): night.rs:9 links docs/mcp/README.md before T5 writes it.
- Task 1: minor (deferred): the `Scope` doc comment still says text values match "ignoring case"; not true for `scan_root` any more.
- Task 2: minor (deferred): fixtures seed `scan_roots` / `black_hole` with raw INSERTs, not `upsert_scan_root`.
- Task 3: minor (deferred): no test with a measured TransparencyLoss; "one transparency call" unpinned; the no-render / no-solver branches compiled, never run.
- Task 3: minor (deferred): stale comment "two low ones" in `quality_outliers_are_worst_first`.
- Task 4: minor (deferred): `find_missing_in_scope` fetches every frame of the scope then keeps the missing ones (a chunked `fr.id IN` on the missing ids would read fewer rows); content-key groups whose copies have no frame row are never in scope (opt-in content mode only; T5's answer could say so); full `cargo test -p athenaeum-core` owed at the M1 gate.
- Task 5: minor (deferred): `path_arguments_are_looked_up_not_opened` proves only that nothing was created; line-1 test skips `file_details` and errors after the catalog opens; storage / `rest` / 40-keyword header caps unpinned.
- Task 5: minor (deferred): the slow-call test hard-codes the pool's 8 connections.
- Task 6: minor (deferred): a Windows pipe name derivable from the app-data path (squat after a crash); `elsewhere` never recovers when the owner quits (note for T8/T9).
- Task 6: minor (deferred, FINAL REVIEW MUST TRIAGE): (1) shutdown while `elsewhere` can race a 30 s tick already past its select into a bind after quit — retry only while `ch.watch.is_some()` (lifecycle.rs:441); (2) `Channel`'s doc comment sits on `WatchGuard` (lifecycle.rs:187-190); (3) `fail` / `close_channel` decide ownership by `state != Elsewhere`, so a host that went `failed` without the lock can remove the serving instance's `endpoint` / `enabled` / `tools.json` — use `ch.lock.is_some()` (lifecycle.rs:516, :671).
- Task 7: minor (deferred): CRLF now passes through unstripped (harmless; untested); the two new warns and the pending-initialize `list_changed` rule have no dedicated test (no tracing-capture harness).
- Task 8: minor (deferred, FINAL REVIEW MUST TRIAGE): no real-router test that the three MCP `/api` routes need the API key; web MCP tests leak temp dirs on Windows (session tasks hold the host until the runtime drops — use a manual runtime dropped before the TempDir); `bridge_found` false before the first enable on Windows/AppImage (the copy does not exist yet; Settings shows it only when on); the early bind runs on core's fallback runtime.
- Task 9: minor (deferred): status read only on mount/write (no refresh of `starting`/`elsewhere`; a re-read on focus would do — owner follow-up); `CopyButton` timer without cleanup; `>= 14` row guard.
- Task 10: minor (deferred): SKILL.md:135 still names `npm run tauri build` for the local signed rehearsal (no `--`, harmless); docs/release/README.md:40 one very long line.
- M1 whole review (fable, ~287k): FIX FIRST — 1 Important (two instances of one build: off undone by the other's 30 s check; off in the other ignored; a quitting non-server deletes the server's `endpoint` — includes T6 deferred (3)). Minors: INSTRUCTIONS and Settings copy promise M2's plans; the cargo hint shown to release users; the `WatchGuard` doc comment. Deferred-minor triage: all others LATER (listed in the review; T8's API-key route test goes into T13, which adds two routes). Declined-to-judge list recorded in the review.
- Task 12: minor (deferred): a log writer that panics at mod.rs:451/:472 leaves the plan in `applying` for good (no cancel, `finished_at` None); hardening = set the end state, then log, or a drop guard.
- Task 12: minor (deferred): tests.rs `POOL_SIZE = 8` repeats `Database::new` (db/mod.rs:128).
- T14 implemented: ecce7ff7 (DONE_WITH_CONCERNS: tests not seen red; keeper locked, a swap needs a new plan; byte checks inline in the tool call; `validate` in memory, so no host change). The agent again parked on a background build (bounded by `timeout 900`) and resumed by itself; it ended ~292k, so any T14 fix round goes to a fresh agent. Controller re-gate: `mcp:: black_hole rebuilt` 172 passed; no-default-features check ok. Task review (opus) on `task-14-review.diff`.
- Task 14: minor (deferred): a cancel waits for a zip extraction inside `verify_duplicate_pair` (fresh `AtomicBool`).
- Task 14: minor (deferred): `files_in_scope` (tools/plans.rs:220-229) copies catalog_query/findings.rs:115-121.
- Task 14: minor (deferred, FINAL REVIEW MUST TRIAGE — a flaky test): `a_byte_checked_plan_gets_its_timer` (black_hole_tests.rs:359-371) reads the event count right after `settle`, but `publish` emits after releasing the lock, so `after == before + 1` can flake.
- Task 14: minor (deferred): no test pins the `guard` around `load_groups`/`assemble` in `prepare` (black_hole.rs:653); the panic `error!` carries no payload.
- Task 15: minor (deferred): Shift+Tab from the title reaches an armed Apply; a scrim click dismisses an applied/failed report (consider closeOnScrim false while a report shows); no focus ring on the title; no window test that a non-candidate radio stays disabled; arming-timer clearing on unmount has no direct test.
- M2 whole review (fable, ~215k): With fixes — 0 Critical, 4 Important (I1 an applying plan locks the app; I2 the window trusts events alone, the web build drops them; I3 full-hash groups never checked against the disk; I4 the flaky timer test), 7 Minors. No path loses a last copy. Deferred-minor triage: FIX NOW the flaky test, scrim on a report, progress while applying (via I1); the rest LATER.
- M3: LATER (open-items at the close): autofind (key 1) has no guard against a second run; pre-existing.
- Task 16: minor (deferred): `a_refusal_while_preparing_names_the_batch_size` calls `claim_batch_slot` directly, so it does not pin that `prepare` passes `frame_ids.len()`.
- Round 1 also takes the Minors: dates displayed `YYYY-MM-DD HH:MM:SS`; object neighbours lights only; a log line on a db failure; the applier's own re-check pinned; the statement's reach documented. Deferred LATER: the apply does not re-check the evidence's neighbours; an all-unreachable request replaces the waiting plan with an empty one (Black Hole too).
- Task 17: minor (deferred): untyped neighbours (imagetyp NULL) no longer count toward the object (not in the docs); a 300-frame date plan makes up to 300 bulk updates at apply.
- Task 18: minor (deferred): the failed text says the reason twice ("Failed: X." then "Nothing was started: X."); a failed apply logged twice (warn + error); README plan_status row lacks "Nothing was started".
- M3: minor (deferred, FINAL REVIEW MUST TRIAGE): `runNext` can drop the next queued batch when a batch is cancelled during its activity read or catalog precheck (`setQueue(q => q.slice(1))`); raw violet classes remain in PlateSolveBatchPanel.tsx :158, :168, :195, :222 (pre-existing).
- Task 19: minor (deferred): a read begun before the complete event can re-show the assistant line for up to 3 s (flicker, nothing told twice); a cancelled unstarted batch keeps 'pending' rows; the `?? 'unknown'` key fallback.
- M3 whole review (fable): With fixes — 0 Critical, 1 Important (a batch cancelled while `runNext` awaits still runs, and the next queued batch is dropped), 11 Minors; the slot, run and plan machinery sound. Triage: FIX NOW I1, the complete-event run id (with progress too), run-scoped cancel, the failed words; LATER the rest.
- Wave also takes: the cap counted first; a cancel from the window named to the assistant; a short write reported failed; the words (solve_status, failed text by kind, window footer and tick text, the re-read's ending). LATER: other lists listening to `frames-metadata-changed`; the rest of the triage's LATER lines.
- M3: parked — `solve_status` "3 of 5 done" while the activity read 2 (two sources a moment apart) — LATER.

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
| `plan_status {planId}` | Where a plan stands, for a plan made on any connection (`mcp/tools/plans.rs`): `waiting for you in Athenaeum's window (expires <time>)`, `applied` with the report (applied, skipped, failed, each by file name and folder), cancelled, replaced, expired, failed; an unknown id says `No plan <id>: Athenaeum restarted since, or the plan is older than a day.` (an answer, not an error). Only an `applied` plan's text says "applied" (`only_an_applied_plan_says_applied`): a plan that ended `failed` reads `Failed: <reason>.` and, when it got something done, `Done before it stopped: N done, N skipped, N failed.` (and never "applied") |

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
  late-closing instance never blocks the next bind.
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
  A failed apply always carries a report (an empty one says nothing was recorded as moved; `plan_status` reads `No file was recorded as moved before
  it stopped.`); only a plan that failed while it was being prepared has none. Turning assistant access off does not end a waiting or preparing
  plan: the user still decides it in the window.
- **Expiry is on tokio time.** `publish` stores a `tokio::time::Instant` deadline (`PLAN_TTL`, one hour); the sleeper and the apply check
  both read it, so a test pauses time. `expiresAt` is display text (local time, `YYYY-MM-DD HH:MM:SS`).
- **Apply.** `Apply` only from `waiting`, unexpired. The generic check (known groups and items, no locked item included, `chosen` a
  candidate, something selected) and the kind's `PlanApplier::validate` run first; then the plan is `applying` and a thread re-stamps every
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

- **Evidence, each item says which.** Frame type: the nearest folder of the path named `light(s)`, `dark(s)`, `flat(s)`,
  `bias`/`biases`/`offset(s)` or `darkflat(s)`/`flatdark(s)` (case-insensitive, the whole folder name), `folder 'Flats' in its path`.
  Date: the file name holds `YYYY-MM-DD[_T ]HH[-_:]MM[-_:]SS` or `YYYYMMDD[-_]HHMMSS` (hand-parsed, year 1990-2100, a real date, not part of
  a longer run of digits), `from the file name '<name>'; the camera clock's time zone is unknown, taken as UTC`. Camera and object (D9):
  the other frames of the same folder (the folder itself, not below it; Black Hole files do not count) that have the field set: at
  least 3 and all equal gives `40 frames in this folder have INSTRUME = 'ZWO ASI2600MM Pro'`; disagreeing frames give no proposal,
  `frames in this folder disagree: 30 'A', 10 'B'`; fewer than 3 give `fewer than 3 frames in this folder have it`. Neighbours are read
  for the folders of the missing frames only, outside the scope too. The object is counted from lights only (a flat's `OBJECT` is not the
  target; `flats_never_name_a_lights_object`). A frame that is a master (`is_master`, set by the parser from the file name) keeps its master
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
  reason. A file that cannot be stamped is locked `not reachable ...`. Notes: `these changes take N frames out of their sessions and M out
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
  was written; the closing line is `info!(via = "mcp", plan_id, count, "mcp metadata fill applied")`.

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
`find_missing_metadata`, `plan_status`, `find_rebuilt_masters`, `plan_black_hole`, `plan_metadata_fill`). A failed tool is logged by its span's `err`, then returned to the assistant in words. The channel (`mcp::lifecycle`) adds no field
name: it logs with `state` (`off`/`starting`/`serving`/`elsewhere`/`failed`), `addr` (the endpoint's text), `reason`
(`timeout`, `too_long`, or a handshake refusal's word), `error` and `path`; never the key.

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
| Tool bodies, argument types, the scope step | `mcp/tools/{mod,catalog}.rs` |
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

## Tools (M1: the catalog questions)

| Tool | Answers |
| ---- | ------- |
| `catalog_overview` | The default scope: totals by frame type, how many objects, cameras, telescopes and filters, first and last night, storage, duplicate groups, missing-metadata counts per category |
| `query_frames` | Totals and integration time over a scope, optional `groupBy`, a page of files (`filesPage`), a stored-measurement summary (`quality`), calibration (`calibration`), storage (`storage`) |
| `file_details` | One file: header values, stored measurements and whether they are current, transparency, calibration links, copies, plate solve |
| `find_duplicates` | Duplicate groups with a copy in the scope, each copy with file name, folder, scan root kind and a processing-folder note |
| `find_missing_metadata` | The Missing Metadata view's rows inside the scope: counts per category, folders, a page of files |

Each tool is a core fn `mcp::tools::<name>(host, args) -> Result<Answer, ApiError>` wearing
`#[tracing::instrument(skip_all, fields(tool = "<name>"), err)]`; the `#[tool]` method is a two-line adapter that runs it with
`spawn_blocking` (it reads SQLite) and wraps it with `to_tool_result`.

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
web build serves `/mcp` over HTTP instead (Task 8) and binds nothing here.

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
- *How a connection closes.* `Run::close` drops the listener, the handshake-phase tasks and every session task inside the call. A
  session that has completed `initialize` has been handed to rmcp's own task, which is only cancelled: its stream closes when that
  task next runs (`regenerate_cuts_off_connected_clients`: end of stream within 2 s). A Windows pipe name is fresh per bind, so a
  late-closing instance never blocks the next bind.
- Channel tasks run on the caller's tokio runtime, or on a small fallback runtime (`mcp-channel`, 2 workers) when the caller is not
  inside one (a window's setup closure).

**Web.** Turning it on is refused with `NEEDS_API_KEY` (`Invalid`) when `ATHENAEUM_API_KEY` is not set; `McpStatus.blockedReason`
carries the same sentence, and a stored `"true"` without a key at boot stays `off` with `lastError` = the sentence. With a key, on
means `serving` (the key is in `McpStatus.token`; `check_token` and `is_serving` are what the `/mcp` route asks). Nothing binds.

**Client config.** `desktop_client_config(name, bridge, build, app_data_override)` returns
`{"mcpServers":{"<name>":{"command":"<bridge>","args":["--build","<build>"(,"--app-data-dir","<dir>")]}}}` and
`claude mcp add <name> -- "<bridge>" --build <build>` (plus ` --app-data-dir "<dir>"`). `McpStatus` fills them with the bridge expected
next to the app's executable and the app-data dir always passed, so the bridge never guesses.

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

**Not covered here.** Packaging the bridge next to the app and the Settings page's copy button are Task 8's; the web image has no
bridge (the Dockerfile drops the crate from the workspace). The bridge is versioned with the app (`bump.sh`, `check_versions.sh`).

## Logging

One new log field: `tool` (the tool's name on its span: `catalog_overview`, `query_frames`, `file_details`, `find_duplicates`,
`find_missing_metadata`). A failed tool is logged by its span's `err`, then returned to the assistant in words. The channel (`mcp::lifecycle`) adds no field
name: it logs with `state` (`off`/`starting`/`serving`/`elsewhere`/`failed`), `addr` (the endpoint's text), `reason`
(`timeout`, `too_long`, or a handshake refusal's word), `error` and `path`; never the key.

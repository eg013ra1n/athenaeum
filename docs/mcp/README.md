# MCP (assistant access)

Reference for the MCP server in `athenaeum-core` (`crates/athenaeum-core/src/mcp/`). Design:
`docs/superpowers/specs/2026-10-27-athenaeum-mcp-design.md`; plan: `docs/superpowers/plans/2026-11-19-athenaeum-mcp-v1-plan.md`;
spike facts (what the real clients do): `docs/superpowers/research/2026-11-19-mcp-spike.md`.

## Purpose and rules

An assistant (Claude desktop, Claude Code, ...) asks Athenaeum about one astrophotography catalog. Three rules bind every tool:

- **Questions change nothing.** They read through the host's one `SQLITE_OPEN_READ_ONLY` connection (`McpHost::reader`, plan R11),
  never a PRAGMA on a pooled connection. `questions_change_nothing` digests the catalog before and after every tool.
- **No tool applies a plan.** Changes are plans that open in Athenaeum's window, where only the user can apply them; there is
  no `apply` tool, no in-chat card, no elicitation in v1.
- **Off by default.** `mcp.enabled` is `"false"` (Task 6 adds the setting, the local channel and the on/off lifecycle).

## Layout

| Item | Where |
| ---- | ---- |
| `McpHost` (context, identity, the reader, `on_catalog_open`, `source`) | `mcp/mod.rs` |
| `Answer`, `to_tool_result` (line 1 + both halves) | `mcp/answer.rs` |
| `AthenaeumMcp` (the rmcp handler, `INSTRUCTIONS`) | `mcp/server.rs` |
| Tool bodies, argument types, the scope step | `mcp/tools/{mod,catalog}.rs` |
| Tests (a client over `tokio::io::duplex`, both protocol revisions) | `mcp/tests/server_tests.rs` |

`McpHost::new` opens nothing; `on_catalog_open` opens the reader (Task 6 extends the same function with the setting reconcile
and the bind). Before it, `reader()` and `source()` are `Conflict("Athenaeum is starting ...")` and every tool answers that in
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
- **`find_duplicates` and `catalog_overview` read through the app's own duplicate query** (`get_duplicates`), a pooled connection
  that only reads; the reader connection covers every other read.

## Sizes (D3)

Per answer: 50 groups, 3 example files each (the text lists examples only when an answer has 10 groups or fewer); 50 files per
page; 25 frame sets per calibration page; 5 outliers per side; 50 duplicate groups per page; at most 50 folders in a
missing-metadata or storage answer (the rest said as a count; the text lists the 10 largest storage folders). Plan lifetime and
plan size limits belong to the planning tools.

## Logging

One new log field: `tool` (the tool's name on its span: `catalog_overview`, `query_frames`, `file_details`, `find_duplicates`,
`find_missing_metadata`). A failed tool is logged by its span's `err`, then returned to the assistant in words.

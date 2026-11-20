# Athenaeum MCP v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task, under the `lean-execution` skill (fresh agent per
> task, one cargo build at a time). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** An MCP server inside the running app (desktop and web) that answers catalog questions over one common scope
and prepares Black Hole, metadata and plate-solve plans that only the user applies, in Athenaeum's own window.

**Architecture:** `athenaeum-core` gains a `scope` type, read-side `catalog_query` functions and an `mcp` module (an
`rmcp` 3.11 `ServerHandler`, an app-wide plan store, a local channel with a token). The desktop host serves it to the
`athenaeum-mcp` bridge (stdio ↔ Unix socket / named pipe); the web host mounts rmcp's streamable-HTTP service at
`/mcp`. Plans open in one generic plan view (`DialogShell`) fed by two commands and one event in both hosts.

**Tech Stack:** Rust 2021 (toolchain 1.96.1), `rmcp` 3.11.0, tokio 1.53, axum 0.8, rusqlite, Tauri 2, React/TS, vitest.

**Spec:** `docs/superpowers/specs/2026-10-27-athenaeum-mcp-design.md` (working tree with the 2026-11-19 corrections).
Inputs: `.superpowers/sdd/2026-11-19-mcp-v1/audit-r0.md`, `docs/superpowers/research/2026-11-19-mcp-spike.md`.

## At a glance

The dev Mac takes one cargo build at a time: every **cargo** task runs alone. A frontend task runs no cargo command
(its TS types come from the task it consumes) and may run beside one cargo task.

| # | M | Task | Owner | Needs | Cargo |
| ---- | ---- | ---- | ---- | ---- | ---- |
| 1 | M1 | Night rule, scope type, scope resolution | rust-engineer | — | cargo |
| 2 | M1 | Frame query: totals, groups, pages, frame-set parity | rust-engineer | 1 | cargo |
| 3 | M1 | Quality, calibration, storage, file details | rust-engineer | 2 | cargo |
| 4 | M1 | Duplicates tagged by key; scoped findings; frontend-truth fns | rust-engineer | 2 | cargo |
| 5 | M1 | MCP server in core: host, answers, M1 tools | rust-engineer | 3, 4 | cargo |
| 6 | M1 | Local channel, token, on/off lifecycle, `mcp.enabled` | rust-engineer | 5 | cargo |
| 7 | M1 | Bridge crate `athenaeum-mcp` | rust-engineer | 6 | cargo |
| 8 | M1 | Wire both hosts: `/mcp`, three commands | rust-engineer | 7 | cargo |
| 9 | M1 | Settings section + frontend-truth parity tests | frontend-dev | 4, 6 | no |
| 10 | M1 | Packaging the bridge | rust-engineer | 8 | cargo |
| 11 | M2 | Black Hole move into core; rebuilt master copies | rust-engineer | 10 | cargo |
| 12 | M2 | Plan store, model, apply framework | rust-engineer | 11 | cargo |
| 13 | M2 | Plan status tool, window commands, both hosts | rust-engineer | 12 | cargo |
| 14 | M2 | Black Hole plan | rust-engineer | 13 | cargo |
| 15 | M2 | Plan window | frontend-dev + `frontend-design` | 13 | no |
| 16 | M3 | Plate solving into core; one slot both ways | rust-engineer | 14 | cargo |
| 17 | M3 | Metadata plan: inference, the user's word | rust-engineer | 16 | cargo |
| 18 | M3 | Plate-solve plan, status, cancel | rust-engineer | 17 | cargo |
| 19 | M3 | UI: the assistant's solve, own batch refused, views refresh | frontend-dev | 16, 17 | no |

## Global Constraints

- One interface: plans are chosen and applied only in Athenaeum's window; **no MCP tool applies a plan**; no in-chat
  card, no elicitation in v1 (§6.4, §11). `send_to_void` is never a tool; no file on disk is rewritten (§4, §5).
- **Off by default**: `mcp.enabled` = `"false"`; off means the bridge answers that it is off (§5, §8).
- Every answer starts with its build and catalog (§7): text line 1 `[<server name> <version> · catalog <db path>]`;
  structured half `"source": {"build","version","catalog"}`; server names `athenaeum` / `athenaeum-dev` (§2).
- Both halves read on their own (§6.1): text names files, folders, header values; every structured object with a
  `handle` also has `fileName` and `folder`. No table/column names, hashes or set ids in a description.
- Dates `YYYY-MM-DD HH:MM:SS`; nights `YYYY-MM-DD` (R1).
- A tool failure is `Ok(CallToolResult::error(..))` in words (a protocol error's message never reaches the user);
  `McpError` only for an unknown tool or a malformed request.
- A tool body is a core fn `mcp::tools::<x>(host, args) -> Result<Answer, ApiError>` wearing
  `#[tracing::instrument(skip_all, fields(tool = "<name>"), err)]`; the `#[tool]` method is a two-line adapter.
- Never hold a `db.conn()` guard across an `api::*` call that takes `ctx`: the pool has 8 connections and a nested
  checkout can exhaust it (the Black Hole wrapper's `drop(conn)` is the precedent). Questions read through the host's
  one read-only connection (R11), not through a PRAGMA on a pooled one.
- User-supplied paths enter in two places only: `file_details.file` and `Scope.folder` / `scan_root`. Both are matched
  against the catalog's stored paths (equality, separator-strict prefix) and never opened or `canonicalize`d, so the
  `PathPolicy` has nothing to gate; a plan's files are catalog rows, never request paths.
- The UI's gates apply: path policy, the plate-solve input gate, the Black Hole's checks (§5).
- CLAUDE.md rules hold (two backends in one task, `api` object, camelCase DTOs in `ts_export.rs` — regenerate with
  `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` —, tokens, `notify`, listener pattern, Settings
  contract, disk rule, no other program named). New log field names go into the dictionary of
  `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` ("Unified event schema") in the task that logs them.
- Tests pass on Windows, Linux, macOS: `Path::join`; paths in JSON/logs compared by file name or `{:?}`; handles
  closed before a temp dir drops; one channel abstraction, same assertions everywhere; test file names `[A-Za-z0-9_-]`.
- Core fixtures: `ServiceContext::new_for_tests(dir.join("catalog.db"))`, `canonical_tempdir()`, `db::insert_file`,
  `insert_frame`, `upsert_scan_root`, `set_setting`, `CapturingEmitter` + `collab_autopublish::test_lock()`; sweeps
  green (`fixtures_never_seed_a_raw_canonicalized_path`, `every_key_with_a_default_is_listed`, ts_contract,
  parity_tests, `cargo check -p athenaeum-core --no-default-features`). Commits carry the session's trailers.

## Rulings (controller, binding)

- **R1 Night** = UTC date of `date_obs − 12 h`: `night_of_date_obs` moves from `api/collab_exchange.rs:751` to
  `scope::night` (`pub`), SQL twin `night_sql(col)` = `DATE(col, '-12 hours')`. Stored `imaging_nights` are not used.
  The 12:00-UTC limit is recorded in `docs/mcp/README.md`, not fixed.
- **R2 Black Hole**: findings and questions leave Black Hole files out unless the scope's `state` asks. Checked: the
  Missing Metadata view does **not** hide them (no exclusion in its SQL) → O1; acceptance 13 compares non-BH frames.
- **R3** `DuplicateGroup.key`, set inside `get_duplicates`' loop; ts_export + ts_contract.
- **R4** Calibration reason only where the app states one (export readiness sentences).
- **R5** `missing_flags` and `frame_set_tab` in core, pinned by one JSON truth table read by a Rust and a vitest
  test. The Missing Metadata view keeps its code (flags drive its chips, row tags and five toolbar counts,
  `MissingMetadataView.tsx:191-360`; switching adds wire fields for no behaviour change). The Objects page changes
  only where the three-line tab rule (`Objects.tsx:739-743`) becomes a call of `frameSetTab`, same rule, so the pin
  covers the code the page runs.
- **R6** The desktop channel binds in `.setup` when the `enabled` marker file exists and answers `ERR starting` until
  `initialize_database` opened the catalog; then the setting decides.
- **R7** An MCP plate solve shows in the UI as started by the assistant (progress, cancel); the UI's own batch is
  refused with a `notify()` naming it; core gets the busy query, the three resolvers and cancel.
- **R8** `athenaeum-mcp --build release|dev [--app-data-dir <dir>]`, written into the copied config; the bridge reads
  that build's `<app-data>/mcp/{token,endpoint}`; it never guesses.
- **R9** `tool` (T5), `via`, `plan_id`, `kind`, `applied`, `skipped`, `failed` (T12) and `run_id`, `count` (T18) enter
  the logging dictionary in the task that logs them.
- **R10** `/mcp` is a `nest_service` outside `/api/`, own token check + toggle; not a `.route("/api/…")`, so no
  `WEB_ONLY` entry (a comment in the parity test says why); the auth tests gain `/mcp`.

- **R11 Questions read on one dedicated connection:** `McpHost` owns a `SQLITE_OPEN_READ_ONLY` connection (same
  process, opened in `on_catalog_open`) and every question fn takes `&Connection`. No PRAGMA toggle on a pooled
  connection: `Database::conn()` is a pool of 8, `query_only` would cover one of them, and a missed restore would leave
  a pooled connection read-only app-wide. The few reused `ctx` fns (`get_duplicates`, calibration readiness,
  `get_links_for_frame`) only read; `questions_change_nothing` pins it.
- **O5 (owner, 2026-11-19) Web needs a key:** on the web build the MCP cannot be enabled unless `ATHENAEUM_API_KEY` is
  set. The web Settings section says why and how; `set_mcp_enabled` refuses with that sentence (T6, tested at T6, T8, T9).
- **O6 (owner, 2026-11-19) Integration time:** a whole frame set's integration time is the stored `total_exp_time`
  (equals the page); the answer says when Black Hole members are counted in it (how many, how much time). The page's
  own figure is fixed outside this plan (O3).

## Decisions this plan makes (owner should know)

- **D1 Windows channel: named pipe** `\\.\pipe\athenaeum-mcp-<app-data dir name>-<8 hex>`, `first_pipe_instance(true)`,
  same token handshake. It is a byte stream like the measured Unix socket (one code path), tokio ships it, no port
  and no firewall prompt. The address is in `<app-data>/mcp/endpoint` on every platform.
- **D2 Packaging:** `bundle.externalBin` only in a release overlay `tauri.mcp-bridge.json` passed with `--config` in
  CI, because tauri-build fails every `cargo build/test -p athenaeum-tauri` whose listed sidecar file is missing.
  Installed at `Athenaeum.app/Contents/MacOS/athenaeum-mcp` (signed and notarized with the app), beside the exe in the
  MSI / setup exe, `/usr/bin/athenaeum-mcp` in the deb. The AppImage mounts at a new path each start, and a running
  `athenaeum-mcp.exe` held by a client is locked against the installer, so on those two the client config points at a
  copy the app keeps at `<app-data>/bin/athenaeum-mcp[.exe]`: refreshed at start when its version differs, a locked
  copy kept and refreshed on a later start (the bridge is a dumb pipe; the handshake is versioned, T6). Settings finds it with `bridge_path_for` (Task 8); in `tauri
  dev` that is `target/debug/athenaeum-mcp`. Docker needs no bridge (the crate joins the Dockerfile's `sed` list).
- **D3 Sizes:** plan lifetime 60 min once ready; ≤ 2 000 items per plan (beyond: "narrow the scope"); per answer 50
  groups × 3 example files; 50 files per page; 25 duplicate groups or frame sets per page; 5 outliers per side;
  finished plans kept 24 h, at most 100.
- **D4 Bridge fallback:** with the app unreachable, the bridge serves its own tiny rmcp server, so every request is
  answered in words; it retries at most every 2 s and replays the client's `initialize` to the app. Its `tools/list`
  is the app's own, read from `<app-data>/mcp/tools.json` (written at each bind), so a client that starts first
  (Claude desktop starts the bridge at its launch) still sees the real tools; `tools/list_changed` is a bonus.
- **D5 Web `/mcp`:** rmcp's Host allow-list disabled (token on every request; Docker is reached by LAN name). Off →
  403, no catalog → 503, bad token → 401. Enabling needs `ATHENAEUM_API_KEY` (O5), so `answer_mcp_plan` is never open.
- **D6** Inferred proposals are pre-selected (asking to fill is the instruction); file-name dates are taken as UTC
  and say so.
- **D7 Token:** 64 hex (two v4 UUIDs) in `<app-data>/mcp/token` (dir 0700, file 0600 on Unix), kept across off/on:
  only Regenerate changes it. Desktop bridges re-read it on connect, so regenerating ends sessions and they reconnect;
  revocation matters for the web key. An `enabled` marker file (not the token) tells `start_early` and the bridge.
- **D8** The bridge is versioned with the app; Settings section `general.assistantAccess` on the General tab.
- **D9 Beyond the spec's categories, kept:** an OBJECT or INSTRUME proposal from same-folder neighbours (≥ 3 frames,
  all equal; a disagreement is no proposal), always with its evidence sentence naming the count and the value. Dropped
  as beyond the spec: `cancel_plan` as a model tool (the user cancels in the window, a new plan replaces the old one)
  and filter / telescope fills.

## Review Focus

1. **A 40k-frame catalog** → summaries, pages, capped plans: `forty_thousand_frames_come_back_as_a_summary`,
   `fetch_scoped_frames_is_three_statements` (T2), `plan_item_cap_asks_for_a_narrower_scope` (T12).
2. **Dev and release at once** → `dev_and_release_never_cross` (T7), `every_answer_names_its_build_and_catalog` (T5).
3. **App quits / MCP off mid-conversation, client before app** → `app_quits_mid_conversation`,
   `app_returns_and_the_bridge_reconnects`, `client_started_before_the_app_lists_the_real_tools` (T7),
   `regenerate_cuts_off_connected_clients` (T6).
4. **Files changed between plan and apply** → `changed_on_disk_is_skipped`, `changed_kept_copy_skips_its_group` (T12),
   `header_pair_rechecked_after_keeper_change`, `keeper_gone_skips_the_group`, `master_linked_meanwhile_is_skipped`
   (T14), `apply_skips_a_frame_filled_meanwhile` (T17).
5. **Two connections from one client** → `status_on_another_connection` (T13), `two_bridges_one_host` (T7).
6. **A plan replaced under the user's cursor** → `replaced_plan_stays_until_the_user_opens_the_new_one`,
   `apply_has_no_initial_focus` (T15).

## Reuse (from the audit)

- **KEEP** `get_duplicates`, `verify_duplicate_pair`, `bulk_update_frame_metadata`, `count_frame_metadata_relations` (api/files.rs:107-190,
  :1500); `scan_root_prefix_predicate`, `file_not_in_black_hole_sql`, `FRAME_NOT_IN_BLACK_HOLE_SQL`, `frame_is_used_copy_sql`,
  `FILE_FRAME_SELECT`/`map_file_frame_row` (db/operations.rs:114, :1248, :2709; operations_blackhole.rs:43); `get_frames_sets`,
  `get_calibration_hierarchy_for_frame_set`, `get_export_readiness`, `get_links_for_frame`, `get_calibration_set_consumers`
  (api/frame_sets.rs:1154; api/calibration.rs:96, :209; api/lights.rs:1204); `get_frame_analyses_by_ids`,
  `transparency_for_frames` + `installed_catalog_identity` (db/analysis.rs:221; analysis/transparency.rs:817);
  `prepare/run_plate_solve_batch`, `service::input_gate_reason`, `store_result` (api/plate_solve.rs:259, :385; plate_solve/service.rs:645).
- **ADAPT** `night_of_date_obs` → `scope::night`; `DuplicateGroup` + `key`; the missing-metadata clause (collab_exchange.rs:751;
  models.rs:217; operations.rs:1662); Black Hole bulk-move side effects → `api::black_hole`; plate-solve resolvers + cancel →
  `api::plate_solve`, `PlateSolveHandle` + origin (tauri commands/duplicates.rs:27-52, plate_solve.rs:15-70 (+ web);
  services/mod.rs:47); `usePlateSolveQueue`, `PlateSolveProgressContext`, `MissingMetadataView`, `DialogShell` (+ `lg`).
- **NEW** `scope`, `catalog_query`, `mcp/*`, `api::mcp`, `api::black_hole`, `calibration_library::rebuilt_copies`; crate
  `athenaeum-mcp`; `commands/mcp.rs`, `routes/mcp.rs`; plan window; Settings section.

## Seams (named once)

| Seam | Value | Producer → consumers |
| ---- | ---- | ---- |
| Handshake | bridge sends `ATHENAEUM-MCP/1 <token>\n` (`/1` = channel protocol version, frozen and append-only: a later host still serves `/1`); host replies `OK <server_name> <version>\n` or `ERR <starting\|off\|token\|version> <message>\n` | T6 → T7 |
| Channel files | `<app-data>/mcp/{token (kept across off/on), endpoint, enabled (marker), tools.json}` | T6 → T7, T8 |
| Bridge args | `--build release\|dev`, `--app-data-dir <dir>` | T7 → T6/T8 config text |
| Setting | `mcp.enabled`, default `"false"` | T6 → T8, T9 |
| Handles | `file:<file_id>`; `dup:<header\|master\|content>:<content_hash>`; `rebuilt:<lowest file_id>`; `plan-<uuid>`; `solve-<uuid>`; plan items `item:<file_id>` (metadata `item:<frame_id>:<field>`); plan groups `group:<n>` | T2, T4, T11, T12, T16 → tools, T15 |
| Enums (JSON) | `DuplicateKey` `header\|master\|content`; `FrameState` `catalog\|soft_archived\|zipped\|black_hole`; origin `ui\|assistant` | T4, T1, T16 |
| Plan DTOs casing | every plan struct (`McpPlanView`, `PlanGroup`, `PlanItem`, `PlanReport`, `GroupSelection`, `PlanSelection`, …) `#[serde(rename_all = "camelCase")]`; every plan enum value `snake_case`; TS in `src/types/mcp.ts` via `ts_export.rs` | T12 → T13, T15 |
| `PlanState` (`tag = "kind"`) | on the wire `{"kind":"preparing","done":3,"total":9}`, `{"kind":"waiting"}`, `applying`, `applied`, `cancelled`, `replaced`, `expired`, `{"kind":"failed","error":…}`; so `McpPlanView.state.kind`, never `state.state` | T12 → T14, T15, T17, T18 |
| `PlanAnswer` (`tag = "action"`) | `{"action":"apply","selection":{…}}` / `{"action":"cancel"}`; `PlanKind` values `black_hole\|metadata\|plate_solve` | T12 → T15 |
| Events | `mcp-plan-updated {planId}`; `plate-solve-started {origin, runId, total}`; `frames-metadata-changed {count}`; existing `blackhole-changed {action:"bulk-blackholed", count}` | T12, T16, T17, T11 → T15, T19 |
| Commands | `get_mcp_status`, `set_mcp_enabled {enabled}`, `regenerate_mcp_token` (T8); `get_mcp_plan {planId?}`, `answer_mcp_plan {planId, answer}` (T13); `get_plate_solve_activity` (T16) | → T9, T15, T19 |
| Strings | Black Hole `from_where = "assistant"`; `BUSY_ASSISTANT` / `BUSY_UI` (T16; display only, the test is `get_plate_solve_activity`); operation span `mcp_plan_apply` keyed by `plan_id` | T14, T16 → T18, T19 |

## Dependencies

- Core: `rmcp = { version = "3.11", default-features = false, features = ["server", "macros"] }` — brings
  `transport-async-rw`, schemars 1, uuid at the lock's single versions; new crates are only rmcp's own (`rmcp-macros`,
  `pastey`). Dev-dep: rmcp `client`. tokio gains `net`. Never `request-state` (hmac 0.13 against the lock's 0.12.1).
- Web: rmcp feature `transport-streamable-http-server` (the lock's http 1, bytes, sse-stream, rand 0.10); mounted with
  `nest_service`, rmcp has no axum dependency.
- Bridge: tokio (`rt`, `io-std`, `io-util`, `net`, `macros`, `time`), serde_json 1, `dirs = "6"`, rmcp `server`, tracing +
  tracing-subscriber 0.3; dev: `athenaeum-core` (`default-features = false`), rmcp `client`, tempfile 3.
- No `[workspace.dependencies]`; caret ranges per crate. After each Cargo change `cargo tree -d -p athenaeum-core
  -p athenaeum-web -p athenaeum-mcp` shows one tokio, axum, hyper and schemars 1. Packaging: D2.

---

## M1 — the host and the questions

### Task 1: Night rule, scope type, scope resolution (§6.6, R1, R2) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/scope/{mod,night,nearest,tests}.rs`. Modify `src/lib.rs`;
`src/api/collab_exchange.rs:751-765` (fn and its test move; callers import `crate::scope::night::night_of_date_obs`);
`crates/athenaeum-core/Cargo.toml` (rmcp, see Dependencies).

**Interfaces:**
- Consumes: `scan_root_prefix_predicate`, `FRAME_NOT_IN_BLACK_HOLE_SQL`, `frame_is_used_copy_sql`, `ImageType::from_str`.
- Produces:
```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "camelCase")] #[schemars(crate = "rmcp::schemars")]
pub struct Scope { pub object, frame_set, folder, scan_root, camera, telescope, filter, frame_type: Option<String>,
    pub exposure_s: Option<f64>, pub night, date_from, date_to: Option<String>, pub year: Option<i32>,
    pub state: Option<Vec<FrameState>> }
#[serde(rename_all = "snake_case")] pub enum FrameState { Catalog, SoftArchived, Zipped, BlackHole }
pub struct ScopeFilter { pub sql: String, pub params: Vec<rusqlite::types::Value> } // one predicate on `fr.id`, see below
pub struct ResolvedScope { pub filter: Option<ScopeFilter> /* None = nothing matches */, pub description: String,
    pub hints: Vec<NearestHint>, pub black_hole_left_out: bool }
pub struct NearestHint { pub condition: &'static str, pub given: String, pub existing: Vec<String> }
pub fn resolve_scope(conn: &Connection, scope: &Scope) -> anyhow::Result<ResolvedScope>;
pub fn describe_scope(scope: &Scope) -> String;
pub fn hints_text(hints: &[NearestHint]) -> String;
pub fn frame_state_sql(state: FrameState) -> String;
pub fn normalize_value(s: &str) -> String; // lowercase, whitespace runs → one space, trim (Objects.tsx:258)
pub mod night { pub fn night_of_date_obs(date_obs: &str) -> Option<String>; pub fn night_sql(column: &str) -> String; }
```
Resolution (not fixed by the signatures). `ScopeFilter.sql` is `fr.id IN (SELECT f.id FROM frames f JOIN files fl ON
fl.id = f.file_id WHERE <conditions>)`, so inside it `f` = **frames** (the alias `frame_is_used_copy_sql` and
`FRAME_NOT_IN_BLACK_HOLE_SQL` hard-code) and `fl` = files; a caller's `FILE_FRAME_SELECT … WHERE` (`f` files, `fr`
frames) only sees `fr.id`:
```text
text conditions: camera→f.instrume, telescope→f.telescop, filter→f.filter, object→f.object ∪ frames_set.name,
                 frame_set→frames_set.name
  distinct = SELECT DISTINCT <col> WHERE <col> IS NOT NULL AND <col> <> ''
  hits = distinct where normalize_value(v) == normalize_value(given)          -- equality, never substring
  none → hint{condition, given, existing: nearest(distinct, given)}; else "<col> IN (?,…)"
  object → "(f.object IN (…) OR <member of a set named IN (…)>)"; frame_set → <member of a set named IN (…)>
  member = EXISTS(session_members→sessions→imaging_nights→frames_set fs WHERE sm.frame_id = f.id
                  AND frame_is_used_copy_sql("fs.id") AND …)
frame_type: ImageType::from_str(given minus a trailing "s"), unknown → hint over DISTINCT imagetyp
exposure_s: ABS(f.exptime - ?) < 0.005
night: night_sql("f.date_obs") = ?; year → 'YYYY-01-01'…'YYYY-12-31'; range → night_sql(..) BETWEEN (open end unbounded)
folder: scan_root_prefix_predicate("fl.path", &[folder]); scan_root: same on the matching scan_roots.path (else hint)
state (default [Catalog, SoftArchived, Zipped] → black_hole_left_out): OR of frame_state_sql —
  black_hole = NOT(FRAME_NOT_IN_BLACK_HOLE_SQL); zipped = fl.archive_zip_path NOT NULL ∧ ¬BH;
  soft_archived = fl.archive_zip_path NULL ∧ ¬BH ∧ member of a set with is_archived = 1; catalog = none of these
night parity: `night_of_date_obs` accepts exactly the forms SQLite's DATE() parses — `T` or space separator,
date-only, fractional seconds, `Z`/offset — and returns None where DATE() gives NULL (pinned below).
any hint → filter None. nearest: normalized containment either way, then Levenshtein ≤ 3; ≤ 5, by distance, value.
```
`describe_scope` order: object, frame set, camera, telescope, filter, exposure `300 s`, nights, frame type plural
(`lights`), `in <folder>`, `under scan root <path>`, state (`including the Black Hole`); empty → `the whole catalog`.
- [ ] **Step 1: Write the failing tests** (`scope/tests.rs`, fixtures under `canonical_tempdir()`):
  - `night_sql_equals_night_of_date_obs` (table test, one row per form): `2026-09-27T11:59:59Z`, `…T12:00:00Z`,
    `…T03:10:00`, `…T03:10:00.250`, `…T23:30:00+02:00`, `2024-10-05 22:15:03` (space), `2024-10-05` (date-only),
    `garbage`: `SELECT DATE(?, '-12 hours')` == `night_of_date_obs` (NULL ↔ None), scope (SQL) and grouping (Rust) agree.
  - `every_scope_statement_prepares_and_runs` (all conditions at once, on an empty and a populated catalog: no "no such
    column f.file_id"; the first run of each helper under the `f` = frames alias).
  - `camera_matches_normalized_not_substring`: catalog `ZWO ASI2600MM Pro`, `ZWO ASI2600MC Duo`; `zwo  asi2600mm pro`
    resolves; `ASI2600` → `filter None` and `hints_text == "no camera 'ASI2600'; the catalog has 'ZWO ASI2600MC Duo'
    and 'ZWO ASI2600MM Pro'"`.
  - `describe_reads_like_the_spec`: camera `ZWO ASI2600MM Pro`, year 2024, frame type `lights` →
    `"ZWO ASI2600MM Pro, nights 2024-01-01 … 2024-12-31, lights"`.
  - `black_hole_left_out_unless_asked`; `folder_includes_subfolders_separator_strict` (`M31/H/a.fits` in,
    `M31b/a.fits` out); `object_matches_frame_set_name` (empty OBJECT in set `IC 5146`, scope `ic 5146`);
    `year_is_the_night_range` (`2025-01-01T05:00:00Z` in 2024, not 2025); `states_partition_the_catalog` (each
    fixture frame in exactly one state); the moved `night_of_date_obs_shifts_twelve_hours_in_utc`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core scope::` → FAIL (module missing).
- [ ] **Step 3: Implement** the Interfaces.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core scope:: collab_exchange` and `cargo check -p athenaeum-core
  --no-default-features` → PASS.
- [ ] **Step 5: Commit** `feat(core): one scope over the catalog, nights as the calendar counts them`

### Task 2: Frame query — totals, groups, pages, frame-set parity (§6.1, §6.7) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/catalog_query/{mod,tests}.rs`; Modify `lib.rs`.

**Interfaces:**
- Consumes: T1 `ScopeFilter`, `FrameState`, `night_of_date_obs`; `FILE_FRAME_SELECT`, `map_file_frame_row`,
  `FRAME_NOT_IN_BLACK_HOLE_SQL`, `frame_is_used_copy_sql` (both under the `f` = frames alias, so a frame-set query
  wraps them in a subquery that names frames `f`).
- Produces:
```rust
pub struct ScopedFrame { pub file: FileWithFrame, pub folder: String /* parent, as stored */, pub night: Option<String>,
    pub state: FrameState, pub frame_sets: Vec<String>, pub scan_root: Option<(String, String) /* path, kind */> }
pub fn fetch_scoped_frames(conn: &Connection, filter: &ScopeFilter) -> anyhow::Result<Vec<ScopedFrame>>;
#[serde(rename_all = "snake_case")] pub enum GroupKey { Object, FrameSet, Filter, Camera, Telescope, Night, Month,
    Year, FrameType, Exposure, Folder, ScanRoot, State }
pub struct Totals { pub frames: usize, pub lights: usize, pub integration_s: f64 /* Σ LIGHT exptime */, pub bytes: i64,
    pub nights: usize, pub first_night: Option<String>, pub last_night: Option<String> }
pub struct FileLine { pub handle, file_name, folder: String, pub imagetyp, object, filter: Option<String>,
    pub exptime_s, gain, offset, ccd_temp: Option<f64>, pub binning: Option<String>, pub width, height: Option<i64>,
    pub date_obs, night, instrume, telescop: Option<String>, pub state: FrameState }  // §6.1 header values
pub struct FrameGroup { pub key: BTreeMap<GroupKey, String>, pub label: String, pub totals: Totals, pub examples: Vec<FileLine> }
pub struct FilePage { pub page: u32, pub page_count: u32, pub items: Vec<FileLine> }
pub struct FrameQueryResult { pub totals: Totals, pub groups: Vec<FrameGroup>, pub groups_total: usize,
    pub rest: Option<Totals>, pub files: Option<FilePage> }
pub fn query_frames(frames: &[ScopedFrame], group_by: &[GroupKey], files_page: Option<u32>) -> FrameQueryResult;
pub fn group_label(key: &BTreeMap<GroupKey, String>, totals: &Totals) -> String;
pub fn file_line(f: &ScopedFrame) -> FileLine;
pub struct FrameSetTotals { pub frames: usize, pub integration_s: f64 /* the stored total_exp_time (O6) */,
    pub black_hole_in_total: Option<(usize, f64)> /* Black Hole members' (lights, seconds) when the stored total counts them */,
    pub by_filter: Vec<(String, usize, f64)> }
pub fn frame_set_totals(conn: &Connection, frames_set_id: i64) -> anyhow::Result<FrameSetTotals>;
pub fn format_duration(seconds: f64) -> String; // "45 s", "12 min", "2 h 05 min"
pub const GROUPS_SHOWN: usize = 50; pub const EXAMPLES_PER_GROUP: usize = 3; pub const FILES_PAGE: u32 = 50;
```
`fetch_scoped_frames` is three statements however many rows (frames; set names for those frames, grouped; scan roots
read once), never a query per row. Grouping runs in Rust over the fetched rows (night by R1; month `YYYY-MM` of the
night; folder = parent by the path's native separator); groups by frames desc, then label. `frame_set_totals`: `frames`
= the members `get_frames_sets_by_project` counts (`FRAME_NOT_IN_BLACK_HOLE_SQL` + `frame_is_used_copy_sql`);
`integration_s` = the set's stored `total_exp_time` (O6: equals the page); `black_hole_in_total` = `Some` when that
figure exceeds the Σ of the non-Black-Hole LIGHT exposures by more than 0.5 s, carrying the Black Hole members' count
and light time (the answer: "includes N frames now in the Black Hole (X)"). `by_filter` follows the same members.
- [ ] **Step 1: Write the failing tests** (names and assertions):
  - `group_label_reads_like_the_spec`: Object "IC 5146", Night "2024-10-05", Filter "H", Exposure "300", 24 frames →
    `"IC 5146, night 2024-10-05, filter H, 300 s, 24 frames"`.
  - `forty_thousand_frames_come_back_as_a_summary` (40 000 in-memory frames, 120 objects): 50 groups of `groups_total`
    120, `files` None, `rest.frames` + Σ shown = 40 000, ≤ 3 examples per group; `files_page = Some(0)` → 50 items,
    `page_count` 800.
  - `fetch_scoped_frames_is_three_statements` (real connection, 2 000 rows; a `conn.profile` counter): ≤ 3 statements,
    the same count for 20 rows.
  - `frame_set_totals_equal_the_page` (3 H + 3 O lights of 300 s): `frames` == `member_count`, `integration_s` ==
    `total_exp_time`, `by_filter` == `[("H",3,900.0),("O",3,900.0)]`; `black_hole_member_left_out` (frames drop by one
    as `member_count` does); `black_hole_members_counted_in_the_stored_total_are_said` (stored total left counting a
    Black Hole light → `black_hole_in_total == Some((1, 300.0))`); `no_black_hole_note_when_the_total_leaves_them_out`.
  - `integration_counts_lights_only`, `month_and_year_follow_the_night`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core catalog_query::` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same → PASS.
- [ ] **Step 5: Commit** `feat(core): frame query over a scope — totals, groups, pages`

### Task 3: Quality, calibration, storage, file details (§6.7, R4) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/catalog_query/{quality,calibration,storage,details}.rs` (+ tests).

**Interfaces:**
- Consumes: T2; `get_frame_analyses_by_ids`, `MEASURE_VERSION`, `transparency_for_frames`, `installed_catalog_identity`,
  `get_calibration_hierarchy_for_frame_set`, `get_export_readiness`, `get_links_for_frame`, `get_duplicates`.
- Produces:
```rust
#[serde(rename_all = "snake_case")] pub enum QualityMetric { Fwhm, Hfr, Eccentricity, Stars, Snr, FrameSnr,
    Background, Noise, QualityScore, TransparencyLoss }
pub struct QualitySummary { pub metric: QualityMetric, pub measured, not_measured, older_version: usize,
    pub refused: Vec<(String, usize)>, pub min, median, max: Option<f64>,
    pub low_outliers, high_outliers: Vec<(FileLine, f64)>, pub not_measured_reason: Option<String> }
pub fn quality_summary(conn: &Connection, catalog: Option<&str> /* installed_catalog_identity */, frames: &[ScopedFrame],
    metric: QualityMetric) -> Result<QualitySummary, ApiError>;  // reader connection (R11)
pub struct CalibrationLine { pub frame_set, night, camera, filter: String, pub lights: usize,
    pub darks, flats, bias: bool, pub reason: Option<String> }
pub fn calibration_summary(ctx: &ServiceContext, frames: &[ScopedFrame], page: u32) -> Result<(Vec<CalibrationLine>, usize), ApiError>;
pub struct StorageLine { pub label: String, pub files: usize, pub bytes: i64 }
pub struct StorageSummary { pub by_scan_root, by_folder: Vec<StorageLine>, pub by_state: Vec<(FrameState, usize, i64)>,
    pub black_hole: (usize, i64) }
pub fn storage_summary(conn: &Connection, frames: &[ScopedFrame]) -> anyhow::Result<StorageSummary>;
pub struct FileDetails { pub line: FileLine, pub path: String, pub size: i64, pub modified_at: String,
    pub header: Vec<(String, String)>, pub analysis: Option<FrameAnalysis>, pub transparency_loss: Option<f64>,
    pub calibration: Vec<(String, String, String) /* type, master file name, folder */>, pub copies: Vec<FileLine>, pub solved: bool }
pub fn file_details(ctx: &ServiceContext, file: &str /* "file:<id>" or a stored path */) -> Result<FileDetails, ApiError>;
pub const OUTLIERS_SHOWN: usize = 5; pub const FRAME_SETS_PAGE: u32 = 25;
```
`get_frame_analyses_by_ids` binds one `?` per id with no chunking, so `quality_summary` calls it in chunks of 900.
Measured = an analysis row at `MEASURE_VERSION` with the value; an older row counts in `older_version` ("measured
by an earlier version — run Analysis again"), never in the figures; a set `frame_verdict` counts in `refused`.
Outliers `|x − median| > 3 × 1.4826 × MAD` (MAD > 0), worst first. One `transparency_for_frames` call per summary
(it chunks itself); without `solver`: `not_measured_reason = "this build measures no transparency"`. A reason always
names the app action that would produce the value (§6.7): "run Analysis on these frames in Athenaeum" for the
quality metrics, "plate-solve these frames in Athenaeum, then run Analysis" for `TransparencyLoss`. Calibration: one line per
date/camera/filter group of each touched set's hierarchy; `reason` = the set's readiness sentences only when not
ready (R4). Storage leaves Black Hole rows out of every line and reports them in `black_hole` (O4).
- [ ] **Step 1: Write the failing tests:** `unmeasured_is_said_not_estimated` (3 rows at `MEASURE_VERSION`, 1 none, 1 at
  `MEASURE_VERSION - 1` → `(3, 1, 1)`, median of the 3); `quality_outliers_named_by_file` (FWHM 2.0 2.1 2.0 2.2 2.1 5.0
  → `high_outliers[0]` is `f6.fits`, 5.0; no low); `calibration_reason_only_where_the_app_states_one` (no flats →
  `flats false`, `reason` == the readiness sentence; a calibrated set → `None`); `storage_black_hole_apart` (a 10-byte
  BH file only in `black_hole == (1, 10)`); `file_details_by_path_and_handle` (equal; unknown path → `NotFound`
  naming the file; the path is looked up in the catalog, never opened);
  `quality_over_forty_thousand_frames_chunks_its_ids` (real connection, 40 000 analysed frames → `measured == 40 000`,
  no variable-limit error); `not_measured_names_the_app_action` (each reason contains "Analysis" or "plate-solve").
- [ ] **Step 2: Run** `cargo test -p athenaeum-core catalog_query::` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same + `cargo check -p athenaeum-core --no-default-features` → PASS.
- [ ] **Step 5: Commit** `feat(core): quality, calibration and storage summaries over a scope`

### Task 4: Duplicates tagged by key, scoped findings, frontend-truth fns (§6.2, §6.3, R2, R3, R5) — rust-engineer (cargo)

**Files:** Modify `db/operations.rs:166` (`DuplicateKey` + `Serialize, Deserialize, ts_rs::TS`,
`rename_all = "lowercase"`), `models.rs:217` (`#[serde(default)] pub key: Option<DuplicateKey>`), `api/files.rs:209-217`
(tag each group in the loop), `db/operations.rs:1662` (extract `pub(crate) fn missing_metadata_clause(category) ->
(&'static str, &'static str)`, add the scoped variant), `ts_export.rs` (regenerate `src/types/models.ts`). Create
`catalog_query/{findings,frontend_truth}.rs`, `crates/athenaeum-core/tests/fixtures/frontend_truth_tables.json`.

**Interfaces:**
- Consumes: T1, T2; `get_duplicates`, `get_frames_with_missing_metadata`.
- Produces:
```rust
pub fn get_frames_with_missing_metadata_in(conn: &Connection, category: &str, filter: &ScopeFilter) -> Result<Vec<MissingMetadataRow>>;
pub struct CopyLine { pub line: FileLine, pub scan_root: Option<String>, pub root_kind: Option<String>, pub folder_note: Option<String> }
pub struct DuplicateFinding { pub handle: String, pub key: DuplicateKey, pub matched_by: String, pub size: i64,
    pub copies: Vec<CopyLine>, pub extra_copies: usize }
pub fn find_duplicates_in_scope(ctx: &ServiceContext, scope: &ResolvedScope, page: u32) -> Result<(Vec<DuplicateFinding>, usize, i64), ApiError>;
#[serde(rename_all = "snake_case")] pub enum MissingCategory { Coordinates, Object, Date, Camera, FrameType }
impl MissingCategory { pub fn db_name(self) -> &'static str } // coordinates, object, datetime, instrument, frametype
pub struct MissingFinding { pub counts: Vec<(MissingCategory, usize)>, pub by_folder: Vec<(String, usize, Vec<FileLine>)>, pub page: FilePage }
pub fn find_missing_in_scope(ctx: &ServiceContext, scope: &ResolvedScope, cats: &[MissingCategory], page: u32) -> Result<MissingFinding, ApiError>;
pub struct MissingFlags { pub coordinates, object, date, camera, frame_type: bool }
pub fn missing_flags(frame: &Frame) -> MissingFlags;                     // = computeMissingFlags (MissingMetadataTable.tsx:29)
pub enum FrameSetTab { Stage, Wip, Archive }
pub fn frame_set_tab(is_custom: bool, is_archived: bool) -> FrameSetTab; // = Objects.tsx:739-743
pub fn processing_folder_note(conn: &Connection, folder: &str) -> anyhow::Result<Option<String>>;
```
`matched_by`: header "same header, size and name (raw sub-frames)"; master "same full-file hash (masters and
processed files)"; content "same content". A group is in scope when one copy is; all copies are shown with scan root
and kind (the role). `processing_folder_note`: ≥ 1 master/processed file (`is_master = 1` or IMAGETYP `Master*`) and
no raw light in the folder → "this folder holds N masters or processed files and no raw lights; it looks like a
processing folder". The MCP masks flags to the categories that apply (coordinates/object: lights only), the view
does not (O2). Fixture rows: `{frame:{ra,dec,objctra,objctdec,object,date_obs,instrume,imagetyp}, flags:{coordinates,
object,date,camera,type}}` and `{isCustom, isArchived, tab}`.
- [ ] **Step 1: Write the failing tests:** `duplicate_groups_carry_their_key` (setting off → `header|master`, on → all
  `content`; order unchanged); `scoped_duplicates_equal_the_page` (empty scope → handle set == `get_duplicates`'
  groups, acceptance 2); `missing_in_scope_equals_view_for_every_category` (five categories + `all`: all-states filter
  ids == `get_frames_with_missing_metadata` ids; default scope == those minus BH ids); `missing_for_one_camera_and_year`
  (acceptance 13, core half); `missing_flags_match_the_frontend_truth_table` (incl. a DARK with empty OBJECT →
  `object: true`); `frame_set_tab_matches_objects_page` (4 rows); `processing_folder_is_said_as_an_inference`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core catalog_query:: duplicate` → FAIL.
- [ ] **Step 3: Implement**, then regenerate TS types.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core` → PASS.
- [ ] **Step 5: Commit** `feat(core): duplicate groups say what matched; scoped duplicate and missing-metadata findings`

### Task 5: The MCP server in core — host, answers, M1 tools (§6.1, §6.7, §7, §9) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/mcp/{mod,answer,server}.rs`, `mcp/tools/{mod,catalog}.rs`,
`mcp/tests/server_tests.rs`, `docs/mcp/README.md`. Modify `lib.rs`; `Cargo.toml` (dev-dep rmcp `client`); `CLAUDE.md`
(Docs map row "MCP (assistant access)" → `docs/mcp/README.md`; `scope`, `catalog_query`, `mcp` in the Module Map; a
short **MCP** Subsystems block: off by default, no tool applies, both halves, one scope); logging dictionary +
`docs/logging/README.md` (field `tool`).

**Interfaces:**
- Consumes: T1-T4.
- Produces:
```rust
pub enum HostKind { Desktop, Web }
pub struct McpIdentity { pub server_name: &'static str, pub version: &'static str, pub kind: HostKind,
    pub web_api_key_set: bool /* Web: ATHENAEUM_API_KEY is set; ignored on Desktop (O5) */ }
pub fn mcp_server_name() -> &'static str;  // debug_assertions → "athenaeum-dev", else "athenaeum"
#[serde(rename_all = "camelCase")] pub struct McpSource { pub build: String, pub version: String, pub catalog: String }
impl McpHost {
    pub fn new(ctx: Arc<ServiceContext>, identity: McpIdentity, emitter: Arc<dyn ProgressEmitter>,
               bring_to_front: Arc<dyn Fn() + Send + Sync>,
               has_viewer: Arc<dyn Fn() -> bool + Send + Sync> /* desktop: always true; web: event_tx.receiver_count() > 0 */) -> Arc<Self>;
    pub fn ctx(&self) -> &Arc<ServiceContext>;
    pub fn reader(&self) -> Result<MutexGuard<'_, Connection>, ApiError>; // the one SQLITE_OPEN_READ_ONLY connection (R11)
    pub fn has_viewer(&self) -> bool;
    pub fn source(&self) -> Result<McpSource, ApiError>; // Conflict("Athenaeum is starting …") before the catalog
    pub fn server(self: &Arc<Self>) -> AthenaeumMcp; }
pub struct Answer { pub text: String, pub data: serde_json::Value }
pub fn to_tool_result(host: &McpHost, answer: Result<Answer, ApiError>) -> rmcp::model::CallToolResult;
#[derive(Clone)] pub struct AthenaeumMcp { host: Arc<McpHost>, tool_router: rmcp::handler::server::router::tool::ToolRouter<Self> }
// tools/catalog.rs: #[tool_router(router = catalog_tools, vis = "pub(crate)")]; server.rs: #[tool_handler(router = self.tool_router)]
pub struct QueryFramesArgs { pub scope: Scope, pub group_by: Vec<GroupKey>, pub quality: Option<QualityMetric>,
    pub calibration: bool, pub storage: bool, pub files_page: Option<u32> }
pub struct ScopeArgs { pub scope: Scope, pub page: Option<u32> }
pub struct MissingArgs { pub scope: Scope, pub categories: Vec<MissingCategory>, pub page: Option<u32> }
pub struct FileArgs { pub file: String }
// tools: catalog_overview {}, query_frames, file_details, find_duplicates (ScopeArgs), find_missing_metadata (MissingArgs)
// each #[tool] method takes `Parameters(args): rmcp::handler::server::wrapper::Parameters<…Args>`; every type reachable
// from an argument (Scope, FrameState, GroupKey, QualityMetric, MissingCategory, …) derives rmcp::schemars::JsonSchema
// with #[schemars(crate = "rmcp::schemars")]
```
`to_tool_result`: `CallToolResult::success(vec![ContentBlock::text("[{build} {version} · catalog {catalog}]\n{text}")])`
with `structured_content = Some({"source", ..data})`; `Err` → `CallToolResult::error(..)` with the same line 1 (`catalog not open` before the catalog) and
`{"source","error"}`. `get_info` = `ServerConfig::new(ServerCapabilities::builder().enable_tools()
.enable_tool_list_changed().build()).with_server_info(Implementation::new(name, version)).with_instructions(INSTRUCTIONS)`;
`supported_protocol_versions` stays the default (2025-11-25 and 2026-07-28 included). `catalog_overview` = default
scope totals grouped by frame type, counts of objects, cameras, telescopes and filters, first/last night,
`storage_summary`, duplicate groups (`get_duplicates`) and missing-metadata counts per category. A scope with hints answers
`hints_text` and runs nothing; the default state adds `Black Hole files are left out (N)`. A `query_frames` scope that is
exactly one frame set answers `frame_set_totals` (the stored integration time, the Black Hole sentence, O6). Question
tools read through `host.reader()` and T2-T4 fns only; `McpHost::new` opens nothing until `on_catalog_open` (before it,
`reader()` is `Conflict("Athenaeum is starting …")`).
`INSTRUCTIONS` (exact): "Athenaeum answers about one astrophotography catalog; every answer starts with the build and
catalog it came from. Questions change nothing. Changes are plans: a planning tool opens the plan in Athenaeum's
window, where only the user can apply it — no tool applies a plan. Ask plan_status for the outcome and never report a
plan as applied before plan_status says so. Name frames by file name and folder, not by handles."
- [ ] **Step 1: Write the failing tests** (server + clients over `tokio::io::duplex`, `rmcp::serve_server`,
  `rmcp::serve_client_with_lifecycle((), …)`):
  - `both_revisions_list_the_same_tools`: `ClientLifecycleMode::Initialize` and `ClientLifecycleMode::Discover {
    preferred_versions: vec![ProtocolVersion::V_2026_07_28] }` → equal names `catalog_overview, file_details,
    find_duplicates, find_missing_metadata, query_frames`; none contains `apply`.
  - `every_answer_names_its_build_and_catalog`: text starts `[athenaeum-dev `; `structured.source.catalog` ends with
    `catalog.db`.
  - `both_halves_read_on_their_own`: `find_duplicates` (2 copies) → text holds each copy's file name and folder; every
    structured object with `handle` has `fileName` and `folder`.
  - `questions_change_nothing`: digest of `files`, `frames`, `settings`, `black_hole`, `frame_analysis` (rows by id)
    equal before/after every M1 tool over three scopes (acceptance 14); `reader_refuses_a_write` (an `INSERT` on
    `host.reader()` fails; the pool's connections still write afterwards);
    `path_arguments_are_looked_up_not_opened` (`file_details` and `scope.folder` with a path outside the catalog → in
    words, nothing opened).
  - `scope_that_matches_nothing_names_nearest_values`; `a_failure_is_an_error_result_in_words` (no catalog →
    `is_error`, text has "starting"); `two_connections_share_one_host`; `frame_set_question_equals_its_page`
    (`frame_set` scope, `group_by [filter]` == `frame_set_totals`, integration == the page's `total_exp_time`,
    acceptance 14).
- [ ] **Step 2: Run** `cargo test -p athenaeum-core mcp::` → FAIL.
- [ ] **Step 3: Implement**; write `docs/mcp/README.md` (purpose, tools, scope, answer shape, R1 limit, D3 sizes).
- [ ] **Step 4: Run** `cargo test -p athenaeum-core mcp::` + `--no-default-features` check → PASS.
- [ ] **Step 5: Commit** `feat(core): the MCP server — catalog questions over one scope`

### Task 6: Local channel, token, on/off lifecycle, `mcp.enabled` (§7, §8, R6, D1, D7) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/mcp/{channel,lifecycle}.rs`, `src/api/mcp.rs` (+ tests). Modify
`settings/mod.rs` (`keys::MCP_ENABLED = "mcp.enabled"`, `defaults::MCP_ENABLED = "false"`, pair in `defaults::all()`),
`api/settings.rs:50` (`set_setting` refuses `mcp.enabled`: `Invalid("use set_mcp_enabled")`), `api/mod.rs`,
`Cargo.toml` (tokio `net`), `ts_export.rs` (new `src/types/mcp.ts`), `docs/mcp/README.md`.

**Interfaces:**
- Consumes: T5 `McpHost`, `AthenaeumMcp`.
- Produces:
```rust
pub const HANDSHAKE_PREFIX: &str = "ATHENAEUM-MCP/1";
pub enum HandshakeError { Starting, Off, Token, Version }         // "starting" | "off" | "token" | "version" (protocol > ours)
pub fn mcp_dir(app_data_dir: &Path) -> PathBuf;                    // <app-data>/mcp ; files "token", "endpoint", "enabled", "tools.json"
pub enum Endpoint { Unix(PathBuf), Pipe(String) }
impl Endpoint { pub fn parse(text: &str) -> Option<Self>; pub fn to_text(&self) -> String; }
pub fn bind(dir: &Path) -> std::io::Result<(Endpoint, Listener)>; pub async fn connect(e: &Endpoint) -> std::io::Result<Stream>;
#[serde(rename_all = "camelCase")] pub struct McpStatus { pub enabled: bool,
    pub state: String /* off | starting | serving | elsewhere | failed */, pub server_name: String,
    pub endpoint: Option<String>, pub token: Option<String> /* Web only */, pub bridge_path: Option<String>,
    pub bridge_found: bool, pub client_config: Option<String>, pub claude_code_command: Option<String>,
    pub last_error: Option<String>,
    pub blocked_reason: Option<String> /* web without ATHENAEUM_API_KEY: why enabling is refused (O5) */ }
impl McpHost { pub fn start_early(self: &Arc<Self>); pub fn on_catalog_open(self: &Arc<Self>) -> Result<(), ApiError>;
    pub fn set_enabled(self: &Arc<Self>, on: bool) -> Result<McpStatus, ApiError>;
    pub fn regenerate_token(self: &Arc<Self>) -> Result<McpStatus, ApiError>; pub fn status(&self) -> McpStatus;
    pub fn check_token(&self, presented: &str) -> bool; pub fn is_serving(&self) -> bool; pub fn shutdown(&self);
    pub fn tool_list_json(&self) -> String; /* the server's tools/list result, as written to tools.json */ }
pub const NEEDS_API_KEY: &str = "Assistant access cannot be turned on here: this Athenaeum is not protected by an API key, so anyone who can reach it could read your catalog. Set the ATHENAEUM_API_KEY environment variable of the container, restart it, then turn this on.";
pub fn desktop_client_config(server_name: &str, bridge: &Path, build: &str, app_data_override: Option<&Path>) -> (String, String);
// api::mcp — get_mcp_status(&McpHost) -> McpStatus; set_mcp_enabled(&Arc<McpHost>, bool) / regenerate_mcp_token(&Arc<McpHost>) -> Result<McpStatus, ApiError>
```
Rules: enable → refused with `NEEDS_API_KEY` (`Invalid`) when `kind == Web && !web_api_key_set` (`status().blocked_reason`
carries it; a stored `"true"` without a key at boot stays `off`, `last_error` = the sentence), else KV `"true"`, token file
if absent, `enabled` marker, bind, `endpoint` + `tools.json` (tmp file + rename), `serving`. Disable → KV `"false"`,
cancel every connection (each holds a child of the host's `CancellationToken`), close the listener, delete `endpoint`,
`enabled` and `tools.json`, **keep `token`** (only Regenerate changes it, so off/on never changes the web key), `off`.
Regenerate → new token, cancel live connections. `start_early` binds in `starting` iff the `enabled` marker exists;
`on_catalog_open` reconciles to the KV (the DB wins) and opens the reader. Only `HostKind::Desktop` binds. Unix socket:
`<mcp dir>/mcp.sock` when the path is ≤ 100 bytes, else `std::env::temp_dir().join("athenaeum-mcp-<8 hex>/mcp.sock")`
(dir 0700; 8 hex = the first 4 bytes of SHA-256 of the app-data dir path) — on a dev Mac the first path is 102 bytes, so
the temp-dir path is the normal one there. A leftover socket file with nobody listening (crash) is removed before
`bind`; a 30 s check rebinds when the socket file has vanished (`$TMPDIR` cleaners). An endpoint that accepts a
connection belongs to another instance of this build → `elsewhere`, `last_error` "another Athenaeum of this build
serves the assistant". Per connection: one line ≤ 256 bytes within 5 s, protocol `/n` with n > 1 → `ERR version`,
token compared by SHA-256 digest, reply, then `rmcp::service::serve_server_with_ct(host.server(), (reader, writer),
child)`. `desktop_client_config` → `{"mcpServers":{"<name>":{"command":"<bridge>","args":["--build","<build>"(,"--app-data-dir","<dir>")]}}}`
and `claude mcp add <name> -- "<bridge>" --build <build>`.
- [ ] **Step 1: Write the failing tests:** `off_by_default` (`enabled false`, `state "off"`, no files);
  `web_refuses_enable_without_api_key` (`set_enabled(true)` → `Invalid(NEEDS_API_KEY)`, KV stays `"false"`, status
  `blocked_reason` set) and `web_enables_with_api_key` (O5); `disable_keeps_the_token_and_on_again_gives_the_same_key`;
  `tools_json_is_written_on_enable` (equals the server's tool list); `frozen_v1_handshake_is_still_served` (a raw client
  writes the literal `ATHENAEUM-MCP/1 <token>` line, as an old bridge would → `OK`, then a working rmcp session; a
  `/9` line → `ERR version`); `stale_socket_is_removed_after_a_crash` (a leftover file, no listener → bind works);
  `vanished_socket_is_rebound`;
  `handshake_before_catalog_says_starting`; `enable_then_handshake_ok_then_mcp_flows` (`OK athenaeum-dev <version>`,
  then an rmcp client on that stream lists five tools); `wrong_token_is_refused` (`ERR token`);
  `regenerate_cuts_off_connected_clients` (old stream EOF within 2 s; old token → `ERR token`);
  `disable_removes_endpoint_marker_and_tool_list_keeps_the_token` (and stops); `second_instance_of_one_build_does_not_steal_the_endpoint`;
  `long_app_data_dir_uses_short_endpoint` (Unix endpoint ≤ 100 bytes, the 8 hex derive from the dir; Windows a pipe name — one test);
  `set_setting_refuses_mcp_enabled`; `client_config_names_build_and_server`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core mcp:: settings::` → FAIL.
- [ ] **Step 3: Implement**; regenerate TS types.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core mcp:: settings:: --test ts_contract` → PASS.
- [ ] **Step 5: Commit** `feat(core): MCP channel with a per-build token, off by default`

### Task 7: The bridge `athenaeum-mcp` (§7, R8, D4) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-mcp/{Cargo.toml,src/lib.rs,src/main.rs,src/fallback.rs,src/paths.rs,tests/bridge_e2e.rs}`.
Modify root `Cargo.toml` (member); `docker/Dockerfile:54` (`sed` drops the member); `scripts/release/bump.sh:36` and
`.gitlab/ci/scripts/check_versions.sh:33` (versioned with the app); `crates/athenaeum-core/src/paths.rs`
(`pub const APP_DATA_DIR_NAME_RELEASE`, `APP_DATA_DIR_NAME_DEV`, used by `app_data_dir_name()`); `docs/mcp/README.md`;
`docs/logging/README.md` (the bridge logs `tracing` to stderr, which the client keeps; stdout is the protocol).

**Interfaces:**
- Consumes: the handshake and channel-file seams; T6 `Endpoint::parse` semantics (copied, the bridge links no core).
- Produces:
```rust
pub enum Build { Release, Dev }  impl Build { pub fn dir_name(self) -> &'static str; pub fn server_name(self) -> &'static str; }
pub struct BridgeOptions { pub build: Build, pub app_data_dir: Option<PathBuf> }
pub fn parse_args(args: impl Iterator<Item = String>) -> Result<BridgeOptions, String>;
pub fn app_data_dir(opts: &BridgeOptions) -> Option<PathBuf>; // override, else dirs::data_dir()/<dir_name>
pub enum Unavailable { NotRunning, Off, Starting, TokenRejected, Outdated, WrongBuild { reached: String } }
pub fn unavailable_text(u: &Unavailable, build: Build) -> String;
pub async fn run_bridge<R, W>(opts: BridgeOptions, client_in: R, client_out: W) -> std::io::Result<()>
    where R: AsyncRead + Unpin + Send + 'static, W: AsyncWrite + Unpin + Send + 'static;
```
Exact copy (`{b}` = "" release, " (dev build)" dev; `structuredContent.state` in brackets):
- NotRunning [`not_running`]: `Athenaeum{b} is not running. Start it, then ask again.`
- Off [`off`]: `Assistant access is off in Athenaeum{b}: Settings → General → Assistant access (MCP). Turn it on, then ask again.`
- Starting [`starting`]: `Athenaeum{b} is starting and has not opened its catalog yet. Ask again in a moment.`
- TokenRejected [`token_rejected`]: `Athenaeum{b} refused this connection: its assistant access key changed. Ask again; if this repeats, restart this server in your client.`
- Outdated [`outdated`]: `Athenaeum{b} no longer accepts this assistant bridge (version mismatch). Restart Athenaeum so it refreshes its bridge copy, then restart this server in your client.`
- WrongBuild [`wrong_build`]: `This connection is set up for Athenaeum{b} but reached {reached}. Copy the client configuration again from that build's Settings.`

Bridge loop (newline-delimited JSON-RPC both ways):
```text
connect(): no `enabled` marker → Off; no token or endpoint file, or connect fails → NotRunning; send the handshake;
           ERR starting|off|token|version → Starting|Off|TokenRejected|Outdated; OK <name> with name ≠ build.server_name() → WrongBuild
peer = Live(stream) | Down(FallbackServer{reason} served by rmcp::serve_server over tokio::io::duplex)
client line: parse {id, method} (unparsable → forwarded as is); keep the `initialize` request and
   `notifications/initialized`; a request enters inflight[id];
   Down ∧ request ∧ ≥ 2 s since the last try → connect(); if Live: replay initialize with id "athenaeum-bridge-replay"
   (reply dropped) + initialized, switch, write {"jsonrpc":"2.0","method":"notifications/tools/list_changed"} to the
   client; then send the line to the peer
peer line: a response clears inflight[id]; the replay's reply is dropped; the rest goes to the client
Live EOF/error: reason = connect(); a fresh fallback peer; replay initialize/initialized into it; resend every inflight line
client EOF → exit 0
```
`FallbackServer` (`ServerHandler`; tools + `tool_list_changed`; instructions = the text): `list_tools` → the app's own
list from `<app-data>/mcp/tools.json` when it parses (the app wrote it at its last bind), else one tool
`athenaeum_status` ("Why Athenaeum's assistant access is not answering"); `call_tool` for **any** name → `is_error`,
the text, `{"state","build","message"}`. A client that starts before the app (Claude desktop starts the bridge at its
own launch) therefore sees the real tools at once and gets the words when it calls one.
- [ ] **Step 1: Write the failing tests** (`tests/bridge_e2e.rs`: hosts from the core dev-dep in temp app-data dirs,
  `run_bridge` over duplex pipes, an rmcp client in front):
  - `missing_build_argument_is_an_error` (message contains `--build release|dev`); `dirs_match_core`
    (`Build::Dev.dir_name() == APP_DATA_DIR_NAME_DEV`, same for release).
  - `off_answers_in_words` (no `enabled` marker, no `tools.json`: tools == `["athenaeum_status"]`; `query_frames` →
    `is_error`, text == `unavailable_text(&Off, Dev)`, acceptance 1); `not_running_answers_in_words`;
    `starting_answers_in_words`; `outdated_answers_in_words` (a host replying `ERR version`);
    `client_started_before_the_app_lists_the_real_tools` (marker + `tools.json` + no endpoint: tools == the file's list,
    a call → the NotRunning text; then the host starts and the next call answers).
  - `app_quits_mid_conversation` (live host, tools listed, `host.shutdown()`; next `query_frames` → NotRunning text
    within 5 s); `app_returns_and_the_bridge_reconnects` (next answer starts `[athenaeum-dev`; client saw
    `tools/list_changed`); `stateless_client_needs_no_replay` (Discover client across a host restart).
  - `dev_and_release_never_cross` (hosts `athenaeum` / `athenaeum-dev` in two dirs: each bridge's `source.catalog` is
    its own; a dev bridge at the release dir → WrongBuild, acceptance 11); `two_bridges_one_host`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-mcp` → FAIL.
- [ ] **Step 3: Implement**; `cargo tree -d` per Dependencies.
- [ ] **Step 4: Run** `cargo test -p athenaeum-mcp` → PASS.
- [ ] **Step 5: Commit** `feat(mcp): athenaeum-mcp bridge — answers in words when Athenaeum cannot`

### Task 8: Wire both hosts — `/mcp`, three commands (§7, §8, R6, R10, D5) — rust-engineer (cargo)

**Files:** Tauri: create `src/commands/mcp.rs`, `src/mcp_bridge.rs`; modify `src/lib.rs` (`.setup`: `McpHost::new` with
`TauriProgressEmitter`, the bring-to-front closure and `has_viewer = || true`, `start_early`, `app.manage(McpHostState(host))`; three names in
`invoke_handler!`; `RunEvent::Exit` → `shutdown()`), `commands/mod.rs`, `commands/core.rs:12` (`initialize_database`
takes `State<'_, McpHostState>`, calls `on_catalog_open` once the catalog is set). Web: create `routes/mcp.rs`; modify
`main.rs` (`WebAppState.mcp`; `McpHost::new` with `web_api_key_set = api_key.is_some()` and `has_viewer = ||
event_tx.receiver_count() > 0`; `on_catalog_open` at boot), `routes/mod.rs` (three `/api/…` routes; `.nest_service("/mcp",
…)` under `require_mcp_access`; a `test_state_with_catalog(api_key)` helper — `test_state` leaves `ctx.db` unset, yet serving
needs the catalog and the KV write), `routes/parity_tests.rs` (comment), `Cargo.toml`. `WebAppState` gains `mcp: McpSlot`
(a `OnceLock<Arc<McpHost>>` newtype with `Default`), so each of the 25 `WebAppState {` literals (`grep -rn 'WebAppState {'
crates/athenaeum-web/src`: `main.rs` and `routes/{analysis,calibration,collab,export,images,mod,plate_solve,scan_roots,
settings,stacking,zip}.rs`) adds `mcp: Default::default()`. Docs:
`docs/architecture/commands.md` (323 → 326, module `mcp`), `CLAUDE.md` count line.

**Interfaces:**
- Consumes: T6 `api::mcp::*`, `McpStatus`, `desktop_client_config`; T5 `McpHost::server`.
- Produces: commands `get_mcp_status() -> McpStatus`, `set_mcp_enabled(enabled: bool) -> McpStatus`,
  `regenerate_mcp_token() -> McpStatus`, each `#[tracing::instrument(skip_all, err)]`; the Tauri wrapper fills
  `bridge_path`, `bridge_found`, `client_config`, `claude_code_command` using
  `pub(crate) fn bridge_path_for(exe: &Path, copy_to_app_data: bool, app_data: &Path) -> PathBuf` (`copy_to_app_data` =
  AppImage **or Windows** (D2): the config then points at `<app-data>/bin/athenaeum-mcp[.exe]`; macOS and .deb use the
  installed path). `refresh_bridge_copy(src, dst_dir, app_version, rename: impl Fn(&Path, &Path) -> io::Result<()>) ->
  CopyOutcome { Current, Refreshed, Kept(reason) }` runs on enable and at each start while enabled: it compares
  `athenaeum-mcp.version` beside the copy with the app's version, copies to a temp name and renames over; a failed
  rename (a client holds the exe, Windows) keeps the old copy with a `warn!` and tries again at the next start — the
  channel handshake is versioned (T6), so a one-version gap is harmless. `/mcp` =
  `StreamableHttpService::new(move || Ok(host.server()), Arc::new(LocalSessionManager::default()) /*
  rmcp::transport::streamable_http_server::session::local */,
  StreamableHttpServerConfig::default().disable_allowed_hosts())` — no cancellation token: a cancelled one would
  leave `/mcp` dead after a re-enable; turning off or regenerating works per request through the middleware, so a
  live session's next request is refused. `require_mcp_access`: not serving → 403 "Assistant access (MCP) is off in Athenaeum's Settings."; no catalog → 503
  "Athenaeum is starting and has not opened its catalog yet."; missing or wrong `Authorization: Bearer` → 401 "Wrong or
  missing assistant access key." Bring to front: `get_webview_window("main")` → `unminimize()`, `show()`,
  `set_focus()`, each failure a `warn!`.
- [ ] **Step 1: Write the failing tests** (web ones on `test_state_with_catalog`, enabled through the real command):
  `enable_command_refuses_without_api_key` (`test_state_with_catalog(None)`, `POST /api/set_mcp_enabled {enabled:true}` →
  an error body equal to `NEEDS_API_KEY`, status stays off, `/mcp` → 403; O5); `mcp_requires_its_token` (key set, on:
  no Bearer → 401; Bearer → an `initialize` POST answers 200); `mcp_off_answers_403_even_with_token`;
  `api_key_does_not_open_mcp` (`X-API-Key: k`, no MCP token → 401); `off_then_on_serves_again` (same token) and
  `regenerated_key_refuses_the_old_one` (same router, no restart); parity green with the three names; tauri
  `bridge_path_next_to_the_exe` (`exe.parent()/athenaeum-mcp{EXE_SUFFIX}`; with `copy_to_app_data` →
  `app_data/bin/athenaeum-mcp{EXE_SUFFIX}`); `bridge_copy_refreshes_when_the_version_differs`,
  `locked_copy_is_kept_and_refreshed_on_a_later_start` (the injected `rename` fails once: outcome `Kept`, old copy and
  `.version` intact; next call `Refreshed`).
- [ ] **Step 2: Run** `cargo test -p athenaeum-web routes:: && cargo test -p athenaeum-tauri mcp_bridge` → FAIL.
- [ ] **Step 3: Implement**; recount (`grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l` → 326).
- [ ] **Step 4: Run** `cargo test -p athenaeum-web && cargo test -p athenaeum-tauri` → PASS.
- [ ] **Step 5: Commit** `feat(hosts): serve the MCP from the desktop app and at /mcp on the web`

### Task 9: Settings section "Assistant access (MCP)" + truth parity (§8, R5) — frontend-dev (no cargo; types from T6, fixture from T4)

**Files:** Create `src/components/settings/sections/AssistantAccessSection.tsx` (+ test),
`src/components/missing-metadata/missingFlags.truth.test.ts`, `src/pages/objectsTabs.ts` (+ `.truth.test.ts`).
Modify `src/settings/registry.ts` (`general.assistantAccess`), `src/components/settings/tabs/GeneralTab.tsx`
(`GENERAL_SECTIONS`, after `general.dataLocations`), `src/pages/Objects.tsx:739-743` (call `frameSetTab`, same rule),
`docs/settings/README.md` (General row + paragraph).

**Interfaces:**
- Consumes: `get_mcp_status`, `set_mcp_enabled {enabled}`, `regenerate_mcp_token` → `McpStatus`; the JSON fixture.
- Produces: `export function frameSetTab(fs: FramesSetWithCount): 'stage' | 'wip' | 'archive'`.

Copy (exact): title "Assistant access (MCP)"; description "Let an AI assistant on this computer read your catalog and
prepare changes that you approve in Athenaeum."; field `enabled` "Allow assistant access" — help "Off by default.
When on, an MCP client such as Claude desktop or Claude Code can ask about your catalog through the bridge below.
Every change opens here for your approval.", keywords `['mcp.enabled', 'mcp', 'assistant', 'claude']`; field
`regenerate` "Regenerate access key" — help "Connected assistants are cut off and must reconnect with the new key."
Desktop (on): bridge path (or "not found — build it with `cargo build -p athenaeum-mcp`"), client configuration and
Claude Code command with copy buttons; a path containing `/AppTranslocation/` adds the warning "Athenaeum is running
from a temporary location (macOS App Translocation), so this path changes at every launch. Move Athenaeum to the
Applications folder, restart it, then copy the configuration again." State copy: `elsewhere` → "Another Athenaeum of
this build is already serving the assistant on this computer; this one is not."; `failed` → "Assistant access could
not start: <lastError>". Web without `ATHENAEUM_API_KEY` (`blockedReason` set, O5): the toggle is disabled and the
section shows `blockedReason` with the how ("set the ATHENAEUM_API_KEY environment variable of the container, restart
it"); no key, no config. Web (on): `${window.location.origin}/mcp`, the key, and
`{"mcpServers":{"<name>":{"type":"http","url":"<url>","headers":{"Authorization":"Bearer <key>"}}}}` with copy. Off:
the toggle only. `SettingToggle` with a `write` override to `set_mcp_enabled`, never `set_setting`.
- [ ] **Step 1: Write the failing tests:** `off_shows_only_the_toggle`; `web_without_api_key_says_why_and_disables_the_toggle`;
  `translocated_path_warns`; `elsewhere_and_failed_have_their_copy`; `turning_on_calls_set_mcp_enabled_and_shows_the_
  config` (`routeInvoke` sees `{enabled: true}`; text has `athenaeum-dev` and `--build`); `a_failure_notifies_once`
  (`tone: 'warning'`, `dedupeKey: 'mcp-settings'`, no banner); `regenerate_calls_its_command`;
  `missing_flags_match_the_truth_table`; `frame_set_tab_matches_the_truth_table`; `registry-coverage.test.tsx` green.
- [ ] **Step 2: Run** `npx vitest run src/components/settings src/components/missing-metadata src/pages` → FAIL.
- [ ] **Step 3: Implement** (tokens; `SettingToggle`, `Field`, `Button`). The truth tests read the fixture with
  `readFileSync(path.resolve(__dirname, …/crates/athenaeum-core/tests/fixtures/frontend_truth_tables.json))` and
  `JSON.parse`, not an import, so neither `tsconfig` `include` nor vite's `server.fs` is involved.
- [ ] **Step 4: Run** the same + `npx tsc --noEmit` → PASS.
- [ ] **Step 5: Commit** `feat(settings): Assistant access (MCP) section, off by default`

### Task 10: Packaging the bridge (§7, §11, D2) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-tauri/tauri.mcp-bridge.json` (`{"bundle":{"externalBin":["binaries/athenaeum-mcp"]}}`),
`scripts/release/build-mcp-bridge.mjs`. Modify `.gitlab-ci.yml` (before each `npm run tauri build`, :95, :132,
:269-270: run the script with the job's `--target`, append `--config crates/athenaeum-tauri/tauri.mcp-bridge.json`),
`.gitignore` (`crates/athenaeum-tauri/binaries/`), `docs/release/README.md`, `docs/mcp/README.md`.

**Interfaces:** Produces `node scripts/release/build-mcp-bridge.mjs [--target <triple>] [--debug]`: `cargo build
--release -p athenaeum-mcp --target <triple>` (host triple from `rustc -vV` by default), copied to
`crates/athenaeum-tauri/binaries/athenaeum-mcp-<triple>[.exe]`.
- [ ] **Step 1: Failing check:** `node scripts/release/build-mcp-bridge.mjs && test -x
  crates/athenaeum-tauri/binaries/athenaeum-mcp-$(rustc -vV | sed -n 's/host: //p')` → FAIL (no script).
- [ ] **Step 2: Implement** the script, overlay and CI lines.
- [ ] **Step 3: Run** the check → PASS; `cargo test -p athenaeum-tauri` still PASS with no `binaries/`; `npm run tauri
  build -- --config crates/athenaeum-tauri/tauri.mcp-bridge.json --bundles app` → `…/MacOS/athenaeum-mcp` exists.
- [ ] **Step 4: Commit** `build: ship athenaeum-mcp in every desktop installer`

**M1 acceptance:** the tests above; `cargo test --workspace`; `npx vitest run`; owner smokes S1, S2, S11, S13, S14.

---

## M2 — plans, the window, duplicates and rebuilt master copies

### Task 11: Black Hole move into core; rebuilt master copies (§6.2, §10) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/api/black_hole.rs`, `calibration_library/rebuilt_copies.rs` (+ tests).
Modify `crates/athenaeum-tauri/src/commands/duplicates.rs:27-52` and `crates/athenaeum-web/src/routes/duplicates.rs:120-147`
(one call each), `api/mod.rs`, `calibration_library/mod.rs`, `docs/archive/README.md`.

**Interfaces:**
- Consumes: `db::bulk_move_to_black_hole`, `collab_autopublish::note_black_hole_change`, `get_calibration_set_consumers`,
  T1 `normalize_value`, `night_of_date_obs`, T2 `FileLine`.
- Produces:
```rust
pub fn bulk_move_to_black_hole(ctx: &ServiceContext, file_ids: &[i64], from_where: &str,
    progress: Option<&dyn ProgressEmitter> /* the UI passes its emitter; the assistant None */,
    events: &dyn ProgressEmitter) -> Result<BulkMoveResult, ApiError>;
pub struct RebuiltCopy { pub file_id: i64, pub line: FileLine, pub calibration_set_id: Option<i64>, pub link_count: usize }
pub struct RebuiltMasterGroup { pub handle: String, pub params_label: String, pub copies: Vec<RebuiltCopy> }
pub fn find_rebuilt_master_copies(conn: &Connection, filter: Option<&ScopeFilter>) -> anyhow::Result<Vec<RebuiltMasterGroup>>;
```
Inside `bulk_move_to_black_hole`, the wrappers' order: core move with `progress` → `drop(conn)` →
`note_black_hole_change` → `events.emit_json("blackhole-changed", {"action":"bulk-blackholed","count": moved})`. The
core move's progress events drive an open Duplicates page's bar, so the assistant's apply (T14) passes `progress: None`:
only `blackhole-changed` reaches the pages. Masters =
`frames.is_master = 1` under scan roots of kind `calibration_library`, not in the Black Hole. Same master = camera,
IMAGETYP, gain, offset, binning, width, height exactly; exposure to 1 ms; CCD temperature to 0.1 °C; filter by
`normalize_value`; shooting date = night (R1).
- [ ] **Step 1: Write the failing tests:** `bulk_move_notes_and_emits_like_the_ui` (exactly one `blackhole-changed`
  `{"action":"bulk-blackholed","count":2}`; collab dirty state set, under `test_lock()`);
  `assistant_move_emits_no_progress_events` (`progress: None` → the capture holds only `blackhole-changed`);
  `rebuilt_copies_found_with_suffixes` (`MasterDark_300s.xisf`, `_2`, `_3`, same parameters, one linked → one group of
  3, `link_count > 0` only on the linked one); `different_temperature_is_another_master` (−10.0 vs −15.0).
- [ ] **Step 2: Run** `cargo test -p athenaeum-core black_hole rebuilt_copies` → FAIL.
- [ ] **Step 3: Implement**; thin the two wrappers.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core && cargo test -p athenaeum-web duplicates` → PASS.
- [ ] **Step 5: Commit** `refactor(core): Black Hole bulk move with its side effects in core; find rebuilt master copies`

### Task 12: Plan store, model, apply framework (§6.4, §9) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/mcp/plans/{mod,model,apply}.rs` (+ tests). Modify `mcp/mod.rs` (`McpHost.plans:
PlanStore`), `ts_export.rs` (new `src/types/mcp.ts` entries); `crates/log-mcp/src/query.rs:37` (`("mcp_plan_apply",
"plan_id")`); logging dictionary (`via`, `plan_id`, `kind`, `applied`, `skipped`, `failed` if absent) + its Spans list;
`docs/mcp/README.md`.

**Interfaces:**
- Consumes: T5 `McpHost` (emitter, bring_to_front, has_viewer, source).
- Produces (casing and tags: Seams; every struct `rename_all = "camelCase"`, every enum value `snake_case`):
```rust
#[serde(tag = "kind", rename_all = "snake_case")] pub enum PlanState { Preparing { done: usize, total: usize }, Waiting, Applying, Applied, Cancelled,
    Replaced, Expired, Failed { error: String } }
#[serde(rename_all = "snake_case")] pub enum PlanKind { BlackHole, Metadata, PlateSolve }
pub struct PlanColumn { pub key: String, pub label: String }
pub struct PlanItem { pub id, file_name, folder: String, pub values: BTreeMap<String, String>, pub action: String,
    pub evidence: Option<String>, pub selected: bool, pub locked: Option<String>, pub candidate: bool, pub size_bytes: Option<i64> }
pub struct PlanGroup { pub id, title: String, pub note: Option<String>, pub choose_one: Option<String>,
    pub chosen: Option<String>, pub items: Vec<PlanItem> }
pub struct ReportLine { pub item_id, file_name, folder, reason: String }
pub struct PlanReport { pub applied, skipped, failed: Vec<ReportLine>, pub run_id: Option<String> }
pub struct McpPlanView { pub plan_id: String, pub kind: PlanKind, pub title: String, pub source: McpSource,
    pub state: PlanState, pub replaced_by: Option<String>, pub expires_at: Option<String>, pub columns: Vec<PlanColumn>,
    pub groups: Vec<PlanGroup>, pub notes: Vec<String>, pub report: Option<PlanReport> }
pub struct GroupSelection { pub group_id: String, pub chosen: Option<String>, pub included: Vec<String> }
pub struct PlanSelection { pub groups: Vec<GroupSelection> }
#[serde(tag = "action", rename_all = "snake_case")] pub enum PlanAnswer { Apply { selection: PlanSelection }, Cancel }
pub struct DiskStamp { pub path: PathBuf, pub size: u64, pub modified_ms: i64 }
pub fn disk_stamp(path: &Path) -> std::io::Result<DiskStamp>;  // a zip-resident row stamps its zip
pub fn stamp_for_plan(path: &Path) -> Result<DiskStamp, String>; // Err = the lock reason "not reachable (its volume may be unmounted)"
pub struct ItemSnapshot { pub file_id: i64, pub frame_id: Option<i64>, pub stamp: DiskStamp, pub catalog: BTreeMap<String, Option<String>> }
pub trait PlanApplier: Send + Sync {
    fn validate(&self, view: &McpPlanView, sel: &PlanSelection) -> Result<(), String>;
    fn apply(&self, host: &McpHost, plan_id: &str, sel: &PlanSelection, fresh: &[String]) -> PlanReport; } // report holds the applier's own skips
pub struct PlanContent { pub title: String, pub columns: Vec<PlanColumn>, pub groups: Vec<PlanGroup>, pub notes: Vec<String>,
    pub snapshots: HashMap<String, ItemSnapshot> /* every item, candidates and the kept copy included */, pub applier: Box<dyn PlanApplier> }
impl PlanStore { pub fn begin(&self, host: &McpHost, kind: PlanKind, title: &str) -> (String, Arc<AtomicBool>);
    pub fn progress(&self, id: &str, done: usize, total: usize); pub fn publish(&self, id: &str, c: PlanContent) -> Result<(), String>;
    pub fn fail(&self, id: &str, error: String); pub fn view(&self, id: Option<&str>) -> Option<McpPlanView>; // None = the newest
    pub fn answer(&self, host: &Arc<McpHost>, id: &str, a: PlanAnswer) -> Result<McpPlanView, ApiError>; }
pub fn plan_opened_text(host: &McpHost) -> String;
pub const PLAN_TTL: Duration = Duration::from_secs(3600); pub const PLAN_ITEM_CAP: usize = 2_000;
```
Rules: one undecided plan (`preparing`/`waiting`); `begin` replaces it: the old one becomes `replaced` with `replaced_by`
= the new id, its cancel flag set, **but stays readable by its id** (finished plans are kept 24 h, at most 100), and
`view(None)` is the newest; `begin` emits `mcp-plan-updated {planId}` for both and calls `bring_to_front`. `publish`
refuses > `PLAN_ITEM_CAP` items ("this plan would hold N items; narrow the scope to at most 2 000"), else `waiting`,
`expires_at` = now + `PLAN_TTL` (app date format) and a sleeper marks `expired` + event. A planning tool whose
`stamp_for_plan` fails locks that item with the reason. `Apply` only from `waiting`, unexpired, generic check (ids
exist, no locked item included, `chosen` a candidate) and `validate` pass → `applying` + event → `spawn_blocking`:
every included item **and each group's selected `chosen` copy** is re-stamped (`disk_stamp` and the catalog values read at
planning); a changed included item is `skipped` ("changed on disk since the plan (size A → B)", "missing", "catalog
changed: OBJECT is now 'X'"); a changed or missing kept copy skips every included item of its group ("the copy that
stays changed or is gone: <reason>") → `applier.apply` on the rest → `applied` + merged report + event, all in span
`mcp_plan_apply` (`plan_id`, `kind`) ending `info!(via = "mcp", plan_id, kind, applied, skipped, failed, "mcp plan
applied")`. Only the user ends a plan (`Cancel` from `preparing`/`waiting` → `cancelled`); no plan event drives another
page's progress bar. `plan_opened_text`: "the plan is open in Athenaeum's window" when `host.has_viewer()`, else "no
Athenaeum page is open; open Athenaeum to decide".
- [ ] **Step 1: Write the failing tests** (a `#[cfg(test)]` `TestApplier` + a test-only planning fn):
  `new_plan_replaces_an_undecided_one` (old `Replaced` with `replaced_by`, its cancel flag set; acceptance 5);
  `replaced_plan_stays_readable_by_id` (`view(Some(old))` → `Replaced`; `view(None)` → the new one);
  `expired_plan_cannot_be_applied` (`tokio::time::pause`, +61 min); `selection_must_be_a_subset` (unknown / locked /
  non-candidate → `Invalid` naming it, nothing applied); `changed_on_disk_is_skipped` (rewritten with another
  length → skipped "changed on disk", the other applied, acceptance 7); `changed_kept_copy_skips_its_group`;
  `unreachable_file_is_locked_not_reachable`; `plan_item_cap_asks_for_a_narrower_scope` (2 001 items);
  `cancel_ends_a_waiting_plan` (`cancelled`, then Apply → `Invalid`); `events_and_front` (`mcp-plan-updated` on begin,
  publish, apply, cancel, replace; `bring_to_front` once per `begin`; no progress event of any other name);
  `the_apply_is_logged_with_its_plan` (captured event has `via = "mcp"`, `plan_id`, `kind`, acceptance 12);
  `plan_opened_text_follows_the_viewer` (`has_viewer` false → "open Athenaeum to decide").
- [ ] **Step 2: Run** `cargo test -p athenaeum-core mcp::plans` → FAIL.
- [ ] **Step 3: Implement**; regenerate TS types.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core mcp::plans --test ts_contract` → PASS.
- [ ] **Step 5: Commit** `feat(mcp): app-wide plan store and apply framework`

### Task 13: Plan status tool, window commands, both hosts (§6.4, §9) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/mcp/tools/plans.rs` (+ tests). Modify `api/mcp.rs`, `mcp/server.rs` (router
`catalog_tools() + plan_tools()`); Tauri `commands/mcp.rs`, `lib.rs`; web `routes/mcp.rs`, `routes/mod.rs`;
`docs/architecture/commands.md` (326 → 328); `CLAUDE.md` count line (326 → 328); `docs/mcp/README.md`.

**Interfaces:**
- Consumes: T12 `PlanStore`, `PlanAnswer`, `McpPlanView`.
- Produces: commands `get_mcp_plan(plan_id: Option<String>) -> Option<McpPlanView>`, `answer_mcp_plan(plan_id, answer:
  PlanAnswer) -> McpPlanView` (both hosts, `#[tracing::instrument(skip_all, err)]`); `api::mcp::get_mcp_plan(&McpHost,
  Option<String>)`, `api::mcp::answer_mcp_plan(&Arc<McpHost>, String, PlanAnswer)`; tool `plan_status { plan_id }`.
`plan_status` text for `waiting`: "waiting for you in Athenaeum's window (expires <time>)"; an unknown id: "No plan <id>:
Athenaeum restarted since, or the plan is older than a day." No tool cancels or applies a plan.
- [ ] **Step 1: Write the failing tests:** `no_tool_applies_a_plan` (no tool name has `apply`, none cancels or replaces a plan,
  no input schema has a `selection` property, acceptance 4; re-run in T14/T18 with their tools; the only cancel is
  T18's `cancel_solve` of a run); `status_waits_for_the_user`;
  `status_on_another_connection` (made on client A, asked on B → `waiting`, acceptance 6); `unknown_plan_is_said_in_words`;
  `a_page_that_opens_later_sees_the_waiting_plan` (`get_mcp_plan(None)`); `answer_cancel_over_the_command`; parity green
  with the two commands.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core mcp::tools::plans` → FAIL.
- [ ] **Step 3: Implement**; recount (`grep -rE '^\s*#\[tauri::command(\]|\()' crates/athenaeum-tauri/src/commands/*.rs | wc -l` → 328).
- [ ] **Step 4: Run** `cargo test -p athenaeum-core mcp:: && cargo test -p athenaeum-web routes:: && cargo test -p athenaeum-tauri` → PASS.
- [ ] **Step 5: Commit** `feat(mcp): plan status tool and the window's two commands in both hosts`

### Task 14: Black Hole plan (§6.2, §6.4) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/mcp/plans/black_hole.rs` (+ tests); modify `mcp/tools/plans.rs`,
`docs/mcp/README.md`.

**Interfaces:**
- Consumes: T12 store + `PlanApplier`, T13 `plan_tools`; T11 `bulk_move_to_black_hole`, `find_rebuilt_master_copies`; T4
  `find_duplicates_in_scope`; `verify_duplicate_pair`.
- Produces: tools `find_rebuilt_masters { scope, page }`, `plan_black_hole(PlanBlackHoleArgs)`:
```rust
#[serde(rename_all = "snake_case")] pub enum BlackHoleSource { Duplicates, RebuiltMasters }
pub struct PlanBlackHoleArgs { pub source: BlackHoleSource, pub groups: Vec<String>, pub keep_in_folder: Option<String>,
    pub keep_files: Vec<String> }
```
Rules: only named groups enter. Each group is a choose-one "Copy that stays": `chosen` = the one copy under
`keep_in_folder` (separator-strict) or in `keep_files`; none or several → `chosen None`, nothing selected, note "no
instruction picks the copy that stays here (2 copies under <folder>)". Other copies are include/exclude items,
selected iff `chosen` is set. The copy that stays is the group's `chosen`, which the user may change; it is never an
included item. Locked with reason: a master with calibration links
("calibration links point at it; it stays in v1"); a zip-resident copy ("inside a zip; a zip moves to the Black Hole
only as a whole"). `validate`: an included item needs a `chosen`; `chosen` is never included. Header-key items are
byte-checked while `preparing` against `chosen` (`verify_duplicate_pair` on a blocking thread, progress per pair,
cancel flag between pairs); a differing pair → locked "differs in bytes from the copy that stays" + a note. Apply
(after the store's stamp re-check of the included items and the kept copy) re-verifies header pairs whose `chosen`
changed in the window; **requires each group's kept copy to be out of the Black Hole** and **re-reads the calibration
links of every master item**: a group whose keeper went to the Black Hole, and a master item that gained links since
planning, are skipped with that reason; then `bulk_move_to_black_hole(ctx, ids, "assistant", None, host.emitter)` (no
progress events, T11); `BulkMoveResult.failed` → `report.failed`. An item whose file cannot be stamped at planning
(unmounted volume) is locked "not reachable". Columns IMAGETYP, OBJECT, FILTER, EXPTIME, DATE-OBS, INSTRUME,
size, modified. Rebuilt groups: linked copies locked, same keep rules; the report names the linked copy.
- [ ] **Step 1: Write the failing tests:** `keep_in_folder_picks_the_keeper_and_selects_the_rest`;
  `no_instruction_selects_nothing`; `last_copy_always_stays` (`chosen None` + an included item → `Invalid` "pick the
  copy that stays"); `linked_master_is_locked_with_reason` (acceptance 8, core half);
  `header_pair_that_differs_in_bytes_is_dropped`; `header_pair_rechecked_after_keeper_change`;
  `keeper_gone_skips_the_group` (the kept copy moved to the Black Hole or deleted after planning → its group's items
  `skipped`, nothing moved; the other group applied); `master_linked_meanwhile_is_skipped` (a link added after
  planning → that master skipped, the rest of its group moved);
  `apply_moves_exactly_the_ticked_files` (3 groups, one unticked → `black_hole` rows == the ticked ids, `from_where ==
  "assistant"`, one `blackhole-changed` with `count` == moved, acceptance 3); `verification_is_cancellable`;
  `zip_copy_is_locked`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core mcp::plans::black_hole` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core mcp::` → PASS.
- [ ] **Step 5: Commit** `feat(mcp): Black Hole plans for duplicates and rebuilt master copies`

### Task 15: The plan window (§6.4, §7) — frontend-dev; load the `frontend-design` skill first (no cargo; types from T12, commands from T13)

**Files:** Create `src/components/mcp/{PlanWindow,PlanView}.tsx`, `src/components/mcp/planSelection.ts` (+ `.test.ts`),
`src/components/mcp/PlanWindow.test.tsx`, `src/contexts/McpPlanContext.tsx`. Modify `src/components/Layout.tsx`
(provider beside `DeviceReplaceProvider`, window rendered once), `src/components/ui/DialogShell.tsx` (`size: 'sm' |
'md' | 'lg'`, `lg` = `w-[960px]` within `max-w-[92vw]`, scrolling body), `docs/mcp/README.md`.

**Interfaces:**
- Consumes: `get_mcp_plan {planId?}`, `answer_mcp_plan {planId, answer}`, `mcp-plan-updated {planId}`, `src/types/mcp.ts`.
- Produces:
```ts
export function initialSelection(plan: McpPlanView): PlanSelection;
export function toggleItem(plan: McpPlanView, sel: PlanSelection, groupId: string, itemId: string): PlanSelection;
export function toggleGroup(plan: McpPlanView, sel: PlanSelection, groupId: string): PlanSelection;
export function chooseOne(plan: McpPlanView, sel: PlanSelection, groupId: string, itemId: string): PlanSelection;
export function totals(plan: McpPlanView, sel: PlanSelection): { items: number; bytes: number };
export function selectionProblems(plan: McpPlanView, sel: PlanSelection): string[];
```
Behaviour: the provider listens to `mcp-plan-updated` (required listener pattern) and reads `get_mcp_plan({planId})`;
on mount `get_mcp_plan({})` (a page opened later shows the waiting plan). **The plan on screen is never swapped under
the cursor:** when the shown plan becomes `replaced` (`state.kind`, `replacedBy`), it stays as it was with a banner
"The assistant prepared a newer plan" and a button **Open the new plan**; Apply is disabled for it. A plan opened for
the first time (window closed) is not a swap. **Apply never has initial focus:** the dialog's `data-autofocus` goes to
Cancel (Close when there is no Cancel), and Enter on a freshly opened plan therefore cancels nothing and applies
nothing. Shown for `preparing` (`done of total`, Cancel), `waiting`, `applying`, `applied` (report until dismissed),
`replaced`/`expired`/`failed` (state text, Close).
Header: title, `athenaeum-dev 0.7.1 · catalog <path>`, expiry. Body: collapsible groups (title, note); columns from
`plan.columns`; per item a checkbox, file name + folder, values, action, evidence; locked items greyed with their
reason; a radio per candidate where `choose_one` is set. Footer: `totals` ("N files · X GB"), **Cancel**, **Apply**
(disabled while `selectionProblems` is non-empty, problems listed). Report: applied / skipped / failed with reasons.
- [ ] **Step 1: Write the failing tests:** `planSelection.test.ts` — `initial_selection_follows_the_plan`,
  `unticking_a_group_updates_the_totals` (bytes drop by the group's sum, acceptance 3), `locked_item_cannot_be_ticked`,
  `chosen_copy_is_never_included`, `problems_when_no_copy_stays`; `PlanWindow.test.tsx` — `opens_on_event_and_on_mount`
  (StrictMode: one listener), `cancel_sends_cancel` (`answer: {action: 'cancel'}`), `apply_sends_the_selection`,
  `shows_the_report_with_reasons`, `expired_plan_offers_no_apply`, `replaced_plan_stays_until_the_user_opens_the_new_one`
  (event for a newer plan while one is shown → still the old one, banner, Apply disabled; the button opens the new one),
  `apply_has_no_initial_focus` (`document.activeElement` is Cancel on open and after a plan switch).
- [ ] **Step 2: Run** `npx vitest run src/components/mcp` → FAIL.
- [ ] **Step 3: Implement** in the app's look (`bg-surface-elevated`, `text-content-muted`, `Chip`, `Button`, lucide icons).
- [ ] **Step 4: Run** `npx vitest run src/components/mcp src/components/ui` + `npx tsc --noEmit` → PASS.
- [ ] **Step 5: Commit** `feat(ui): Athenaeum's plan window — choose and apply what the assistant prepared`

**M2 acceptance:** owner smokes S3-S8, S12; `cargo test --workspace`; `npx vitest run`.

---

## M3 — missing metadata and plate solving

### Task 16: Plate solving into core; one slot both ways (§6.5, §10, R7) — rust-engineer (cargo)

**Files:** Modify `crates/athenaeum-core/src/api/plate_solve.rs`, `services/mod.rs:47`; Tauri `commands/plate_solve.rs`
(resolvers deleted, thin calls, `get_plate_solve_activity`), `lib.rs`; web `routes/plate_solve.rs`, `routes/mod.rs`;
`ts_export.rs`; `docs/architecture/commands.md` (328 → 329); `CLAUDE.md` count line (328 → 329); `docs/platesolving/README.md` (one
slot both ways). The second `PlateSolveHandle` literal (`api/plate_solve.rs:758`, the autofind run, key 1) gets the new
fields too (`origin: Ui`, a fresh `run_id`, an empty `progress`).

**Interfaces:**
- Consumes: `prepare/run_plate_solve_batch`, `open_catalog_layers`, `open_index_packs`, `DsoCatalog::load`.
- Produces:
```rust
#[serde(rename_all = "snake_case")] pub enum PlateSolveOrigin { Ui, Assistant }
pub struct PlateSolveHandle { pub cancel_flag: Arc<AtomicBool>, pub origin: PlateSolveOrigin, pub run_id: String, pub progress: Arc<Mutex<SolveProgress>> }
pub struct SolveProgress { pub done, total, solved: usize, pub current_file: Option<String>, pub failures: Vec<(String, String)> }
#[serde(rename_all = "camelCase")] pub struct PlateSolveActivity { pub running: bool, pub origin: Option<PlateSolveOrigin>,
    pub run_id: Option<String>, pub done: usize, pub total: usize, pub current_file: Option<String> }
pub fn resolve_layer_caches(ctx: &ServiceContext) -> Result<Vec<Arc<solvemyastro::StarCache>>, ApiError>; // texts of plate_solve.rs:19-36
pub fn resolve_index_packs(ctx: &ServiceContext) -> Vec<Arc<solvemyastro::QuadIndex>>;
pub fn dso_catalog(ctx: &ServiceContext) -> Option<Arc<DsoCatalog>>;
pub fn prepare_plate_solve_batch(ctx, frame_ids, layer_caches, index_packs, dso, origin: PlateSolveOrigin) -> Result<PreparedPlateSolveBatch, ApiError>;
pub fn cancel_plate_solve(ctx: &ServiceContext) -> bool;
pub fn plate_solve_activity(ctx: &ServiceContext) -> PlateSolveActivity;
pub const BUSY_ASSISTANT: &str = "A plate solve started from the assistant is running ({done} of {total} frames). Wait for it or cancel it.";
pub const BUSY_UI: &str = "A plate solve batch started in Athenaeum is running ({done} of {total} frames); the assistant's run can start when it ends.";
// command get_plate_solve_activity() -> PlateSolveActivity; event "plate-solve-started" {origin, runId, total}
```
`prepare` checks slot 0 and inserts the handle **under one lock** (no window between the two) and returns
`Conflict(<BUSY_ASSISTANT | BUSY_UI filled>)` while slot 0 is held (today it silently replaces the cancel handle). The
slot is released by a `Drop` guard that `PreparedPlateSolveBatch` owns and `run` takes over, so a batch dropped before
`run` (an error between the two, a cancelled plan) frees it and no later solve is blocked until restart. `run_plate_solve_batch` wraps its emitter in a recorder that copies each `plate-solve-progress`
(`current`, `total`, `status`, `filename`, `error`) into the handle's `progress` before forwarding, and emits
`plate-solve-started` once. All `cfg(feature = "solver")`.
- [ ] **Step 1: Write the failing tests:** `second_batch_is_refused_not_replaced` (assistant run holds slot 0 →
  `prepare(.., Ui)` `Conflict` starting "A plate solve started from the assistant is running"; `run_id` unchanged);
  `assistant_refused_while_ui_runs` (BUSY_UI); `activity_reports_the_running_run`; `idle_activity`;
  `cancel_sets_the_flag_of_slot_zero`; `resolvers_report_a_missing_catalog_in_the_same_words`;
  `started_event_names_its_origin`; `dropped_batch_frees_the_slot` (prepare, drop without `run`, prepare again →
  `Ok`); `check_and_insert_are_atomic` (two threads prepare at once → exactly one `Ok`); the autofind run still holds
  and frees key 1; existing plate-solve tests green.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core plate_solve` → FAIL.
- [ ] **Step 3: Implement**; recount → 329.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core plate_solve && cargo test -p athenaeum-web && cargo test -p
  athenaeum-tauri && cargo check -p athenaeum-core --no-default-features` → PASS.
- [ ] **Step 5: Commit** `refactor(core): plate-solve resolvers, cancel and the one slot in core; a second run is refused`

### Task 17: Metadata plan — inference and the user's word (§6.3) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/mcp/infer.rs`, `mcp/plans/metadata.rs`, `mcp/tools/fill.rs` (+ tests);
modify `mcp/server.rs` (`+ fill_tools()`), `docs/mcp/README.md`.

**Interfaces:**
- Consumes: T4 missing semantics, T2 `ScopedFrame`, T12 store, `bulk_update_frame_metadata`,
  `count_frame_metadata_relations`, `FrameMetadataEdits`.
- Produces:
```rust
#[serde(rename_all = "snake_case")] pub enum FillField { Object, Camera, Date, FrameType }  // the spec's categories; coordinates are the plate-solve plan (T18)
pub struct Proposal { pub frame_id: i64, pub field: FillField, pub value: String, pub evidence: String }
pub struct NoProposal { pub frame_id: i64, pub field: FillField, pub why: String }
pub fn infer(missing: &[ScopedFrame], neighbours: &[ScopedFrame], fields: &[FillField]) -> (Vec<Proposal>, Vec<NoProposal>);
pub fn by_instruction(frames: &[ScopedFrame], field: FillField, value: &str) -> Vec<Proposal>; // evidence "by your instruction"
pub struct UserStatement { pub field: FillField, pub value: String }
pub struct PlanMetadataArgs { pub scope: Scope, pub fields: Vec<FillField>, pub statement: Option<UserStatement> }
// tool plan_metadata_fill(PlanMetadataArgs); event "frames-metadata-changed" {count}
```
Rules (exact): only frames missing the field (object: lights only). Frame type from the nearest path component,
case-insensitive: `light(s)` → Light, `dark(s)` → Dark, `flat(s)` → Flat, `bias`/`biases`/`offset(s)` → Bias,
`darkflat(s)`/`flatdark(s)` → DarkFlat; evidence "folder 'Flats' in its path". Date: the file name holds
`YYYY-MM-DD[_T ]HH[-_:]MM[-_:]SS` or `YYYYMMDD[-_]HHMMSS` (hand-parsed, year 1990-2100, valid date); evidence "from
the file name '<name>'; the camera clock's time zone is unknown, taken as UTC". Camera and object (D9)
from same-folder frames with the field set: ≥ 3 and all equal → "40 frames in this folder have INSTRUME = 'ZWO
ASI2600MM Pro'"; disagreeing → NoProposal "frames in this folder disagree: 30 'A', 10 'B'"; < 3 → NoProposal "fewer
than 3 frames in this folder have it". A statement covers the frames in scope only. Plan: one group per (field,
value), items pre-selected (D6), NoProposal frames locked with `why`; notes from `count_frame_metadata_relations`
("these changes take N frames out of their sessions and M out of calibration links", as the UI's confirm warns).
Apply: re-check the field is still missing, group equal edits → `bulk_update_frame_metadata` → report →
`frames-metadata-changed {count}`; logged `via = "mcp"`, `plan_id`.
- [ ] **Step 1: Write the failing tests:** `frame_type_from_folder`; `date_from_file_name_with_evidence`
  (`M31_2024-10-05_22-15-03_0001.fits` → `2024-10-05T22:15:03`, evidence has "taken as UTC");
  `camera_from_agreeing_neighbours`; `object_from_agreeing_neighbours_names_count_and_value`; `disagreeing_neighbours_make_no_proposal`; `two_neighbours_are_not_enough`;
  `statement_says_by_your_instruction` (acceptance 9); `fill_sets_override_and_survives_rescan` (`frames.override ==
  1`; the scanner's re-parse of the file keeps the value, acceptance 9); `apply_skips_a_frame_filled_meanwhile`;
  `frames_metadata_changed_emitted`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core mcp::infer mcp::plans::metadata` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core mcp::` → PASS.
- [ ] **Step 5: Commit** `feat(mcp): fill missing metadata from evidence or the user's word, through a plan`

### Task 18: Plate-solve plan, status, cancel (§6.3, §6.5) — rust-engineer (cargo)

**Files:** Create `crates/athenaeum-core/src/mcp/plans/plate_solve.rs` (+ tests, `cfg(feature = "solver")`); modify
`mcp/tools/fill.rs`, `docs/mcp/README.md`.

**Interfaces:**
- Consumes: T16 (resolvers, `prepare(.., Assistant)`, `run_plate_solve_batch`, `PlateSolveHandle.progress`, `BUSY_UI`),
  `service::input_gate_reason`, `PlateSolveConfig`, T12 store.
- Produces: tools `plan_plate_solve { scope, only_unsolved: bool /* default true */ }`, `solve_status { run_id }`,
  `cancel_solve { run_id }`; `PlanReport.run_id = Some("solve-<uuid>")`.

Rules: items = lights in scope (missing coordinates when `only_unsolved`); `input_gate_reason` → locked with its
reason; `stamp_for_plan` failing → locked "not reachable"; non-lights left out with a count note. Notes: "N of M frames can be solved", refusals by reason, "star
catalog: installed / not installed", "quad index packs: K installed", "runs X frames at a time" (`(cores / 3).clamp(2,
8)`). No catalog → every item locked "the star catalog is not installed (Settings → Plate Solving)". Planning while
any batch runs → no plan, BUSY_UI. Apply: `prepare(.., Assistant)` (a UI batch started meanwhile → `failed` with
BUSY_UI), then the run on a thread named `mcp-plate-solve`; `info!(via = "mcp", plan_id, run_id, count, "plate solve
run started from the assistant")`. `solve_status`: while running, the handle's progress (done, current file by name,
failures so far); after, the final snapshot kept in the host (solved, failed with files and errors). `cancel_solve`
acts only when slot 0 holds that `run_id`.
- [ ] **Step 1: Write the failing tests:** `eligible_subset_is_n_of_m` (a Trailed frame at `MEASURE_VERSION`,
  `input_gate_enabled` → locked with the gate's reason; note "2 of 3 frames can be solved");
  `planning_refused_while_a_ui_batch_runs` (acceptance 10, core half); `missing_catalog_locks_every_item`;
  `status_reports_progress_and_result` (items that fail to load finish fast, as at api/plate_solve.rs:905; status
  names each failed file); `cancel_solve_stops_only_its_run`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core mcp::plans::plate_solve` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core mcp:: plate_solve` + `--no-default-features` check → PASS.
- [ ] **Step 5: Commit** `feat(mcp): plate-solve plans; the run's status and cancel`

### Task 19: UI — the assistant's solve, own batch refused, views refresh (§6.5, §7, R7) — frontend-dev (no cargo; types from T16-T17)

**Files:** Modify `src/hooks/usePlateSolveQueue.ts`, `src/contexts/PlateSolveProgressContext.tsx` (+ the component
drawing plate-solve progress), `src/components/missing-metadata/MissingMetadataView.tsx` (+ tests).

**Interfaces:** Consumes `get_plate_solve_activity` → `PlateSolveActivity`; events `plate-solve-started`,
`plate-solve-progress`, `plate-solve-complete`, `frames-metadata-changed`; `cancel_plate_solve`.

Behaviour: read the activity on mount; an `assistant` run shows as "Plate solve started from the assistant — done of
total, current file" with **Cancel** (`cancel_plate_solve`), updated by progress events, cleared on complete. Busy is
decided by `get_plate_solve_activity` (`running` and `origin`), never by matching error text: the queue's own batch is
not started while the activity says an assistant run is running, and when an invoke is rejected the queue re-reads the
activity and treats `running` as the reason. Either way → `notify({ title: 'Plate solve not started', detail: <the
`BUSY_ASSISTANT` text, display only>, tone: 'warning', dedupeKey: 'plate-solve-busy' })`, and the batch stays queued. `MissingMetadataView` reloads on `frames-metadata-changed`.
- [ ] **Step 1: Write the failing tests:** `assistant_run_shows_progress`; `own_batch_refused_with_notify_naming_the_
  assistant_run` (still queued); `busy_is_decided_by_the_activity_not_the_error_text` (a rejection with different
  wording, activity `running` → the same refusal; a rejection while the activity is idle → no "busy" notify); `cancel_from_the_ui_cancels_the_assistant_run`; `missing_metadata_reloads_on_event`.
- [ ] **Step 2: Run** `npx vitest run src/hooks src/contexts src/components/missing-metadata` → FAIL.
- [ ] **Step 3: Implement** (listener pattern).
- [ ] **Step 4: Run** the same + `npx tsc --noEmit` → PASS.
- [ ] **Step 5: Commit** `feat(ui): show a plate solve started from the assistant; one solve at a time`

**M3 acceptance:** owner smokes S9, S10, S12, S13 (fill side); `cargo test --workspace`; `npx vitest run`.

---

## Acceptance map (spec §13) and owner smokes

Agent tests cover each item's core half. The owner runs the smokes on the real dev and release catalogs (`npm run
tauri dev` and the installed release; Claude desktop and Claude Code):

| §13 | M | Agent tests | Owner smoke |
| ---- | ---- | ---- | ---- |
| 1 off | M1 | T7 `off_answers_in_words`, T6 `off_by_default` | S1: MCP off in both builds → the off sentence only; then client first, app second (Claude desktop started before Athenaeum: the real tools listed, a call answers in words, then answers once the app is up) |
| 2 duplicates | M1 | T4 `scoped_duplicates_equal_the_page` | S2: groups equal the Duplicates page, setting on and off |
| 3 Black Hole plan | M2 | T14, T15 | S3: both clients: window opens, app to front, untick → totals, Cancel = nothing, Apply = exactly the ticked files, Black Hole tab, restore, file views refresh |
| 4 model cannot apply | M2 | T13 `no_tool_applies_a_plan` | S4: "apply it yourself" fails; status "waiting" until the click |
| 5 web, no page | M2 | T12, T13, T15 | S5: Docker without `ATHENAEUM_API_KEY`: the toggle is off with the why; with it: no tab → "open Athenaeum"; a tab shows the plan; a replaced plan stays on screen as "replaced"; expired cannot apply |
| 6 other connection | M2 | T13 `status_on_another_connection` | S6: Claude desktop status after a plan |
| 7 changed on disk | M2 | T12, T14 | S7: touch a file between plan and apply → skipped with reason |
| 8 rebuilt copies | M2 | T11, T14 | S8: `/Volumes/bigbase3/Calibration` → 108 masters, 81 distinct, 27 extra (ATR2600M 12, ZWO_ASI2600MC_Duo 9, ZWO_ASI6200MM_Pro 6); no linked copy in a plan |
| 9 override | M3 | T17 | S9: fill by inference and by instruction, rescan, values stay; "by your instruction" shown |
| 10 plate solve | M3 | T16, T18 | S10: MCP and UI solves of the same frames agree; UI shows and refuses meanwhile; MCP refuses during a UI batch |
| 11 dev + release | M1 | T7 `dev_and_release_never_cross` | S11: both on at once; each names itself and its catalog |
| 12 writes logged | M2/M3 | T12 `the_apply_is_logged_with_its_plan` | S12: `list_operations(kind = "mcp_plan_apply")` lists each apply with `plan_id` |
| 13 scope | M1/M3 | T1, T4 `missing_for_one_camera_and_year` | S13: one camera + one year equals the Missing Metadata view, by eye, on non-BH rows (O1); a no-match names the nearest values |
| 14 questions | M1 | T2, T3, T5 | S14: a frame set's counts and integration per filter equal its page; an outlier named by file; a never-measured value said so |

Also owner-run: one Windows session (named pipe; the client config points at `<app-data>\bin\athenaeum-mcp.exe`; update
the app while Claude holds the bridge: the copy is kept, refreshed at the next start, and the old bridge still answers
against the new app) and one Linux AppImage session (copied bridge).

## Owner-visible items (found while planning; neither copied nor fixed here)

- **O1** The Missing Metadata view lists Black Hole files (no exclusion in `get_frames_with_missing_metadata`); the
  MCP leaves them out unless asked (R2).
- **O2** The view's chips and tags (`computeMissingFlags`) mark a non-light "Coordinates"/"Object" when it is listed
  for another gap; the MCP's categories keep the lights-only rule.
- **O3** A frame set's stored `total_exp_time` may count Black Hole members that `member_count` leaves out. The MCP
  answers the stored figure (O6) and says when Black Hole members are in it; the page's own figure is **fixed outside
  the MCP plan**.
- **O4** `get_folder_overview` counts Black Hole rows per scan root; the MCP's storage figures report them apart.

## Spec items not placed

None without a task. Out of v1 as the spec says: phase 2 (§12), reveal/preview links in the window (§6.4 "v1 needs
none"), the log MCP's build labelling (§9 "v1 does not change that").

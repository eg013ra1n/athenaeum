# MCP smoke fixes (0.7.2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task, run under the `lean-execution` skill (it overrides SDD where they conflict). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the gaps the owner's 2026-11-21 smoke of assistant access (MCP v1) found: the assistant cannot name exact
files, cannot tell two frame sets of one name apart, hides measurements of an earlier version, treats a frame with mount
coordinates as solved, and the Claude desktop configuration cannot be pasted into an existing file.

**Architecture:** Every fix lives in `athenaeum-core` behind the existing tools; no new MCP tool, no new plan kind. The
scope (`scope::Scope`) gains exact files and a frame-set handle, so every question and plan gets both at once. The Claude
desktop entry is merged into the user's `claude_desktop_config.json` by one core function behind one new command
(desktop; the web route answers 501 "not available in web mode").

**Tech Stack:** Rust (rusqlite, serde_json, rmcp schemars), Tauri 2, Axum, React/TS (vitest).

**Spec:** `docs/superpowers/specs/2026-10-27-athenaeum-mcp-design.md` (§6.6 one scope, §6.7 questions) and the build
rulings in `docs/mcp/README.md`. Two rulings change, both by the owner on 2026-11-21 and recorded in Task 7:
(a) the text half may show a frame set's handle `set:<id>` (`docs/mcp/README.md:41-43` said the text never shows a
handle; `file:<n>` stays out of the text); (b) `onlyUnsolved` means "no stored plate solve", no longer the Missing
Metadata view's coordinates test (`docs/mcp/README.md:380-381`).

## Findings (owner smoke 2026-11-21, checklist artifact `9QJKiWZeQkgu1un9Akbsj6`; causes verified by a fable review)

| Check | What the owner saw | Cause (code) | Task |
| ---- | ---- | ---- | ---- |
| П2 | The desktop configuration "is hard to paste into the existing file and looks invalid" | `lifecycle::desktop_client_config` (`mcp/lifecycle.rs:78-92`) emits a whole `{"mcpServers":{…}}` document; the user's file already holds `mcpServers` and `preferences`. `CopyBlock` renders it `whitespace-pre-wrap break-all` (`AssistantAccessSection.tsx:55`), cutting keys and the path mid-word | 5, 6 |
| S10 | "The assistant cannot solve just 2 files, it adds a folder" | `Scope` (`scope/mod.rs:63-92`) has no file list. `onlyUnsolved` is `missing_flags(frame).coordinates` (`plans/plate_solve.rs:471-475`): set 231's lights have mount RA/DEC and no `plate_solves` row, set 109's are solved, so the default planned nothing (log: "already has coordinates (372)") and the assistant re-asked with `onlyUnsolved: false` over the folder | 1, 4 |
| S14 | "MCP does not return the Stage object" | Two sets are named `LDN 1272`: 109 (WIP, 368 frames, 66 240 s) and 231 (Stage, 4 frames, 720 s, another centre). Set-by-set answers run only for a scope that is a frame set and nothing else (`mcp/tools/catalog.rs:355-363`); entries carry name, nights, totals (`:437-462`); `fetch_set_names` dedups names and `groupBy frame_set` joins them (`catalog_query/mod.rs:252-253, 523-549`); Claude desktop's model reads the text half only (`docs/superpowers/research/2026-11-19-mcp-spike.md:27`) | 1, 2 |
| S14 | "Analysis cannot be started, the data is not returned" | `catalog_query/quality.rs:234-237` skips a row of another `MEASURE_VERSION` before reading its value; set 109's 368 rows are all legacy (`measure_version` NULL). Starting Analysis from the assistant is not in this plan (owner, 2026-11-21) | 3 |
| S12 | "`list_operations` is not there" | Not an MCP gap: the check is for Claude Code in the repo (`athenaeum-logs`), asked of a client without it. `log-mcp` keys `mcp_plan_apply` by `plan_id` already (`crates/log-mcp/src/query.rs:45`); its two descriptions (`rpc.rs:150, 156`) still omit the kind | 7 |

## Global Constraints

- Two backends in sync: a new Tauri command gets its Axum route in the same task; `routes/parity_tests.rs` must pass. A
  desktop-only command's route answers 501 `NOT_IMPLEMENTED` "<name> is not available in web mode"
  (`athenaeum-web/src/routes/mod.rs:526`).
- No `@tauri-apps/*` outside `src/api/`; Serde camelCase at the boundary; new model types go in `ts_export.rs`.
- Errors are never swallowed: `tracing::error!`/`warn!` before returning, with existing field names only (`path`,
  `reason`, `error`); every command wears `#[tracing::instrument(skip_all, err)]`.
- MCP answers: build and catalog first; text half and structured half each read on their own (Claude desktop reads only
  the text, Claude Code only the structured half). Files are named by file name and folder in text; a frame set is
  named with its handle `set:<id>` in both halves.
- Questions change nothing; the Claude desktop merge is a Settings action of the user, never an MCP tool.
- Settings page: no Save button, no banner; success is quiet (`SavedTick`), `notify()` on failure only
  (`docs/settings/README.md:127-131`).
- Never enable serde_json `preserve_order`: it unifies across the workspace and would reorder every `serde_json::Value`
  map, changing the stacking cache hashes computed over a `Value` (`stacking/config.rs:895-902, 990`). The merged
  desktop file is rewritten with sorted keys.
- Tests pass on macOS, Windows and Linux; test names use letters, digits, `_` and `-`; no bare path compared with log text.
- Cargo target for agents: `target-agent`; every cargo run is wrapped in `timeout 900`. One implementer at a time (16 GB Mac).
- English for UI strings, docs, commit messages; commits carry the harness trailers, author `eg013ra1n`.

## Review Focus

1. A frame-set name plus another field (`{frameSet: "LDN 1272", frameType: "lights"}`): both sets are still named, with
   handles, in the text — test in Task 2.
2. A file named the way a text answer names it (folder + separator + file name), including a zip-resident row — test in
   Task 1.
3. One good and one mistyped entry in `files`: nothing matches, the hint names the mistyped one (same as every other
   unknown value), never a silent plan of one file — test in Task 1.
4. A `claude_desktop_config.json` that is not valid JSON, has a UTF-8 BOM, or whose `mcpServers` is not an object: the
   BOM is accepted; the other two are refused in words, the file byte-identical, no `.bak` — tests in Task 5.
5. An unscoped default `plan_plate_solve` on a real catalog (dev: 19 258 lights with no stored solve): refused by the
   2 000-item cap with the narrowing advice, not a hang — test in Task 4.

---

### Task 1: Exact files and a frame-set handle in the scope

**Files:**
- Modify: `crates/athenaeum-core/src/scope/mod.rs` (`Scope` 63-92, `describe_scope` 208, `resolve_scope` 321-470 and its
  frame-set condition)
- Modify: `crates/athenaeum-core/src/catalog_query/details.rs:63-75` (`file_id_of` becomes `pub(crate)`, the one lookup)
- Test: `crates/athenaeum-core/src/scope/tests.rs`

**Interfaces:**
- Produces: `Scope.files: Option<Vec<String>>` (serde `files`); `Scope.tab: Option<FrameSetTab>` (serde `tab`, values
  `stage` / `wip` / `archive`; `FrameSetTab` gains `Deserialize` + `JsonSchema`); `Scope.frame_set` accepts `set:<id>`;
  `pub fn frame_set_handle(id: i64) -> String` and `pub fn parse_frame_set_handle(s: &str) -> Option<i64>` in
  `scope/mod.rs`; `NearestHint.condition` gains `"file"`.

Rules:
- `files` doc (the desktop model reads only text, so it never sees `file:<n>`): "Exact files: each the folder and file
  name joined, as an answer names them (a full path), or a `file:<number>` handle an earlier answer gave. Narrows like
  every other field." An empty list is absent.
- Lookup per entry: `file_id_of` (handle, or `files.path`); when that finds nothing, split the entry at its last `/` or
  `\` and match the row an answer would print with that folder and file name (`catalog_query::file_line` decides how an
  answer names a file — a zip entry's printed folder may differ from `files.path`; resolve it the same way).
- More than 2 000 entries → error "at most 2 000 files; narrow the scope". The condition is `fl.id IN (…)` over the ids.
- An entry that names no file adds a hint `{condition: "file", given, existing}`, `existing` = up to 3 catalog files
  (printed as folder + name) with the same file name, case-insensitive. As for every field, any hint empties the whole
  scope (`scope/mod.rs:457-459`): one bad entry among several plans nothing and says which.
- `frameSet: "set:<id>"`: that set only (same used-copy rule as a name); no set of that id → hint
  `{condition: "frame set", given, existing: []}`, nothing matches.
- `tab` (owner, 2026-11-21): frames that belong to at least one frame set listed under that Objects tab, by the same
  rule as `frame_set_tab(is_custom, is_archived)` (`catalog_query/frontend_truth.rs:59`) written as SQL on `frames_set`,
  with the used-copy rule of a frame-set name. Doc: "The Objects page tab of the frame set the frames belong to: stage,
  wip or archive."
- `describe_scope`: `2 named files` / `1 named file`; a handle reads `frame set set:231`; a tab reads `in Stage`.

- [ ] **Step 1: Write the failing tests:** `files_by_path_and_handle_select_exactly_those`,
  `files_named_as_an_answer_prints_them` (entry built from a `FileLine`'s `folder` + `/` + `file_name`, for a loose row
  and a zip-resident row → both selected), `files_narrow_with_other_fields` (a light + a dark, `frameType: lights` → 1),
  `files_one_bad_entry_matches_nothing_and_hints` (hint `condition == "file"` lists the real file, `filter.is_none()`),
  `files_over_cap_refused`, `frame_set_handle_selects_one_of_two_same_named`, `frame_set_handle_unknown_hints`,
  `tab_stage_selects_frames_of_non_custom_unarchived_sets` (a WIP, a Stage and an archived set → only the Stage set's
  frames), `tab_archive_wins_over_custom`, `describe_scope_names_files_and_handle`.
- [ ] **Step 2: Run** `timeout 900 cargo test -p athenaeum-core --target-dir target-agent scope::` — new tests FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same plus `… mcp::` — all PASS (the MCP tests read the schema from `all_tools()`).
- [ ] **Step 5: Commit** `feat(mcp): a scope names exact files and one frame set by its handle`.

### Task 2: Frame sets of one name are told apart

**Files:**
- Modify: `crates/athenaeum-core/src/catalog_query/mod.rs` (`ScopedFrame.frame_sets` 63, `GroupKey::FrameSet` 252-253 and
  297, `fetch_set_names` 523-549)
- Modify: `crates/athenaeum-core/src/mcp/tools/catalog.rs` (`frame_sets_named` 352-374 incl. `only_a_set` 355, the block
  426-464, `frame_set_json`, `frame_set_lines`, the `query_frames` description)
- Test: `crates/athenaeum-core/src/catalog_query/tests.rs` (update 334, 374 which assert `Vec<String>`),
  `crates/athenaeum-core/src/mcp/tests/server_tests.rs` (537-568 assert `frameSet`)

**Interfaces:**
- Consumes: `frame_set_handle`, `parse_frame_set_handle` (Task 1); `catalog_query::frame_set_tab(is_custom, is_archived)`.
- Produces: `pub struct SetRef { pub id: i64, pub name: String, pub tab: FrameSetTab }`;
  `ScopedFrame.frame_sets: Vec<SetRef>` (no name dedup).

Rules:
- **Group by set.** `GroupKey::FrameSet` keys on the sorted set ids; `FrameGroup.key["frame_set"]` holds the label,
  each set as `'<name>' (<tab>, set:<id>)` joined by `, ` (unique per group); the group's data adds
  `frameSetHandles: ["set:109", …]`.
- **Every set entry** (`frameSet`, each `frameSets[]`) gains `handle`, `tab` (`stage`/`wip`/`archive`), `cameras`,
  `telescopes` (distinct non-empty INSTRUME / TELESCOP of members, sorted) and `centre` (`objctra`/`objctdec` as stored,
  or null). Text heading: `Frame set 'LDN 1272' (Stage, set:231), nights 2025-09-13 … 2025-09-13, camera ATR2600M`. The
  "N frame sets are named …" line ends "; ask again with frameSet set:<id> for one of them".
- `frame_sets_named` answers a `set:<id>` scope; `files` joins the `only_a_set` list.
- **Sets in scope.** Whenever the scope names `object`, `frameSet` or `tab` and the set-by-set block did not run, add one text
  line and `frameSetsInScope`: each set the scope's frames belong to, `'<name>' (<tab>, set:<id>): <n> frames`, largest
  first, at most 10 and a count of the rest. Built in memory from `ScopedFrame.frame_sets`, not from `frame_set_totals`.

- [ ] **Step 1: Write the failing tests:** `group_by_frame_set_splits_same_named_sets` (labels contain `(WIP, set:` and
  `(Stage, set:`), `frame_sets_named_entries_carry_handle_tab_cameras` (both halves), `object_scope_lists_its_frame_sets`,
  `frame_set_name_plus_other_field_lists_both_sets` (`{frameSet: "LDN 1272", frameType: "lights"}` → text names both
  handles), `frame_set_handle_scope_answers_one_set`.
- [ ] **Step 2: Run** `timeout 900 cargo test -p athenaeum-core --target-dir target-agent catalog_query:: mcp::` — FAIL.
- [ ] **Step 3: Implement**, updating the tests at `catalog_query/tests.rs:334, 374` to `SetRef`.
- [ ] **Step 4: Run** the same — PASS.
- [ ] **Step 5: Commit** `feat(mcp): frame sets of one name are told apart`.

### Task 3: Earlier measurements are shown apart

**Files:**
- Modify: `crates/athenaeum-core/src/catalog_query/quality.rs` (`QualitySummary`, the loop 222-302, the sentence 293)
- Modify: `crates/athenaeum-core/src/mcp/tools/catalog.rs:312-316` (the quality sentences)
- Test: `crates/athenaeum-core/src/catalog_query/summary_tests.rs`

**Interfaces:**
- Consumes: `frame_set_handle` (Task 1) only through scopes in tests.
- Produces: `QualitySummary.earlier: Vec<EarlierFigures>`, newest version first; `pub struct EarlierFigures {
  pub version: Option<u32> /* None = legacy row */, pub frames: usize /* rows with a value */, pub min, median, max:
  Option<f64>, pub high_outliers, low_outliers: Vec<FileLine>, pub refused: HashMap<&'static str, usize> }`.
  `older_version` stays (the count of all such rows).

Rules:
- Rows of another `MEASURE_VERSION` never enter the current figures (the v1 rule stands); they form one
  `EarlierFigures` per version, each with the current outlier rule over its own rows; a row with a `frame_verdict` goes
  to that version's `refused`.
- Text replaces the sentence at `quality.rs:293` and the ones at `catalog.rs:312-316` with one per version: `368
  measured before versions were recorded (legacy) — their figures, apart from the current ones: median 3.12, range
  2.41 … 5.90; outliers …; run Analysis again for current figures` (`measured by version 7` for a numbered one).
  "earlier version" appears once per version, not three times.
- `transparency_loss` keeps today's behaviour (`earlier` empty; its currency is `ZP_VERSION`).

- [ ] **Step 1: Write the failing tests:** `earlier_rows_form_figures_per_version` (2 current + 3 legacy + 2 v7 → current
  median over 2, two `earlier` entries, v7 first), `earlier_refused_rows_not_in_figures`,
  `earlier_text_names_each_version_once`, `transparency_loss_has_no_earlier_figures`.
- [ ] **Step 2: Run** `timeout 900 cargo test -p athenaeum-core --target-dir target-agent catalog_query:: mcp::` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same, then the core lib once: `… --lib` — PASS.
- [ ] **Step 5: Commit** `feat(mcp): earlier measurements are shown apart, per version`.

### Task 4: "Unsolved" means no stored plate solve

**Files:**
- Modify: `crates/athenaeum-core/src/mcp/plans/plate_solve.rs` (args doc 70-73, `keys_of` 429-431, `read_plan` 443-506,
  `Read.located` doc, `nothing_to_solve` 509-…, evidence text 601, note 635-641, the applier's hand-off to the solver)
- Modify: `crates/athenaeum-core/src/mcp/tools/fill.rs:45` (tool description)
- Test: `crates/athenaeum-core/src/mcp/plans/plate_solve_tests.rs`

**Interfaces:**
- Consumes: `crate::plate_solve::storage::frames_with_solve(conn, &[i64]) -> Result<HashSet<i64>>` (chunks by 500).
- Produces: nothing new; the field name `onlyUnsolved` stays.

Rules:
- Default: a light is planned when it has no `plate_solves` row (the app's own "solved", `db/calibration_links.rs:1316`),
  whatever its header says. One `frames_with_solve` call over the scope's light ids.
- Sentences: arg doc "Plan only the lights with no plate solve stored (the default). False plans every light of the
  scope, solved or not."; refusal `every light frame in the scope ({d}) already has a plate solve ({n}); pass
  onlyUnsolved false to solve them again; nothing was planned`; evidence `no plate solve is stored for this frame`; the
  note at 635-641 and the tool description say the same. The description adds: "a plan holds at most 2 000 frames;
  narrow by folder, night or frame set".
- The stamp keeps `IMAGETYP` only. At apply, one `frames_with_solve` over the plan's selected frames: a frame solved
  since the plan is skipped, reported `skipped: solved since the plan was made`.
- Header coordinates still go to the solver as the position hint (`Candidate.coordinates`), unchanged.

- [ ] **Step 1: Write the failing tests:** `default_plans_lights_with_header_coordinates_but_no_solve`,
  `default_skips_lights_with_a_stored_solve`, `all_solved_refusal_names_plate_solve`,
  `only_unsolved_false_plans_solved_lights`, `apply_skips_frame_solved_since_plan`, `unscoped_default_over_cap_refused`
  (2 001 unsolved lights → the cap sentence).
- [ ] **Step 2: Run** `timeout 900 cargo test -p athenaeum-core --target-dir target-agent mcp::plans::` — FAIL.
- [ ] **Step 3: Implement.** Give a `plate_solves` row to any existing fixture that relied on header RA/DEC meaning
  "solved"; name them in the report.
- [ ] **Step 4: Run** the same — PASS.
- [ ] **Step 5: Commit** `fix(mcp): a light with mount coordinates but no plate solve is unsolved`.

### Task 5: Add Athenaeum to Claude desktop's configuration (backend)

**Files:**
- Modify: `crates/athenaeum-core/src/mcp/lifecycle.rs` (entry split from `desktop_client_config` 78-92; path and merge;
  `McpStatus` gains `claude_desktop_config: Option<String>`; `status_of` 513-527 fills it)
- Modify: `crates/athenaeum-core/src/api/mcp.rs` (handler; a new `#[cfg(test)] mod tests`), `ts_export.rs`
- Modify: `crates/athenaeum-tauri/src/commands/mcp.rs`, `crates/athenaeum-tauri/src/lib.rs`
- Modify: `crates/athenaeum-web/src/routes/mcp.rs`, `crates/athenaeum-web/src/routes/mod.rs`
- Modify: `src/types/mcp.ts` (`McpStatus.claudeDesktopConfig`, `ClaudeDesktopAdded`)
- Test: `crates/athenaeum-core/src/mcp/tests/channel_tests.rs` (beside the pin at 831), `api/mcp.rs` tests

**Interfaces:**
- Produces:
  - `pub fn desktop_server_entry(bridge: &Path, build: &str, app_data_override: Option<&Path>) -> serde_json::Value`
    (`desktop_client_config` builds from it; its output and the pin at 831 unchanged).
  - `pub fn claude_desktop_config_dir() -> Option<PathBuf>`: macOS `$HOME/Library/Application Support/Claude`;
    Windows: the first existing of `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude` (the Store/MSIX build
    reads only this one) and `%APPDATA%\Claude`; other OS `None`; `None` too when no candidate folder exists. The file
    is `claude_desktop_config.json` inside it. A pure `fn pick_config_dir(candidates: &[PathBuf]) -> Option<PathBuf>`
    carries the choice so it is tested on every OS.
  - `pub fn merge_desktop_entry(existing: Option<&str>, server_name: &str, entry: Value) -> Result<(String, bool), String>`
    — pure: new text (pretty, trailing newline), whether that name was replaced; `Err` = the refusal sentence. A leading
    UTF-8 BOM is stripped before parsing.
  - `pub fn add_to_claude_desktop(host: &McpHost) -> Result<ClaudeDesktopAdded, ApiError>` (bridge, build and app-data
    override from `status_of`, as the copy block gets them); inner `fn add_to_claude_desktop_at(host, dir: &Path)` for
    tests. `#[serde(rename_all = "camelCase")] pub struct ClaudeDesktopAdded { pub path: String, pub replaced: bool,
    pub backup: Option<String> }`.
  - Tauri `add_mcp_to_claude_desktop` (no args); web `POST /api/mcp/add-to-claude-desktop` → 501.

Rules:
- Refusals, each `tracing::warn!(path, reason)` and `ApiError::Invalid` in words, the file untouched: no Claude desktop
  folder ("Claude desktop was not found for this user"); bridge not found; bridge under `/AppTranslocation/`; not valid
  JSON ("… is not valid JSON (line L, column C); it was left as it is"); top level or `mcpServers` not an object.
- Absent file → `{"mcpServers": {<name>: entry}}`. Present: every other key and server kept; `mcpServers.<name>` set.
- A present file is copied to `claude_desktop_config.json.bak` first (an older `.bak` replaced); the new text goes to a
  temp file in the same folder, renamed over the original.
- `McpStatus.claude_desktop_config` = the file's display path when `claude_desktop_config_dir()` is `Some`.

- [ ] **Step 1: Write the failing tests:** `merge_into_absent_file`, `merge_keeps_other_keys_and_servers` (`preferences`,
  `coworkUserFilesPath`, server `other` kept), `merge_replaces_same_name`, `merge_accepts_bom`,
  `merge_refuses_invalid_json` (line number in the sentence), `merge_refuses_non_object_servers`,
  `pick_config_dir_prefers_store_package` (both faked → the package one; only `%APPDATA%` → it; none → `None`),
  `desktop_client_config_unchanged_by_refactor`, `add_writes_backup_then_file` (`.bak` = old bytes),
  `add_refused_leaves_file_byte_identical`.
- [ ] **Step 2: Run** `timeout 900 cargo test -p athenaeum-core --target-dir target-agent mcp:: api::mcp` — FAIL.
- [ ] **Step 3: Implement**, register both hosts, export the types.
- [ ] **Step 4: Run** the core tests, `timeout 900 cargo test -p athenaeum-web --target-dir target-agent parity`,
  `timeout 900 cargo check -p athenaeum-tauri --target-dir target-agent` — PASS.
- [ ] **Step 5: Commit** `feat(mcp): add Athenaeum to Claude desktop's configuration from Settings`.

### Task 6: The Settings section uses it (frontend)

**Files:**
- Modify: `src/components/settings/sections/AssistantAccessSection.tsx` (`CopyBlock` 48-58, desktop block 175-205)
- Modify: `src/settings/registry.ts` (field meta `addToClaudeDesktop`: label "Add to Claude desktop", help "Writes
  Athenaeum's entry into Claude desktop's configuration and keeps a copy of the old file. Restart Claude desktop
  afterwards.")
- Test: `src/components/settings/sections/AssistantAccessSection.test.tsx`

**Interfaces:**
- Consumes: `api.invoke<ClaudeDesktopAdded>('add_mcp_to_claude_desktop')`, `McpStatus.claudeDesktopConfig` (Task 5).

Rules:
- The button shows on the desktop when `bridgeFound`, not translocated, and `claudeDesktopConfig` is set, above the
  "Client configuration" block. Success: `SavedTick` beside it and a muted line `Added to <path>. Restart Claude desktop
  to load it.` (+ `Replaced the earlier entry.` when `replaced`). Failure: the existing `fail()` (console + `notify()`).
- `CopyBlock` renders `whitespace-pre overflow-x-auto` (no `break-all`). Design tokens only; `Button` from `ui/`.

- [ ] **Step 1: Write the failing tests:** `shows_add_button_when_config_path_known`,
  `hides_add_button_on_web_and_when_path_unknown`, `add_success_shows_restart_line`, `add_failure_notifies`,
  `copy_block_does_not_break_words`.
- [ ] **Step 2: Run** `npx vitest run src/components/settings/sections/AssistantAccessSection.test.tsx` — FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the same, `npx tsc --noEmit`, `npm run lint -- src/components/settings` — PASS.
- [ ] **Step 5: Commit** `feat(settings): an Add to Claude desktop button; the configuration reads as it is copied`.

### Task 7: Docs, rulings, the log tool's description, open items (controller)

**Files:**
- Modify: `docs/mcp/README.md`: tool rows of `query_frames`, `plan_plate_solve`; the scope paragraph (`files`,
  `set:<id>`); the text-handle paragraph 41-43 (set handles in text, `file:` still not); the `onlyUnsolved` rule 380-381;
  "the three commands" 113-114 → four; a dated entry in "v1 rulings and deferred items" with both owner rulings of
  2026-11-21 and this plan's name.
- Modify: `crates/log-mcp/src/rpc.rs:150, 156` (both kind lists add `mcp_plan_apply`)
- Modify: `CLAUDE.md:83` and `docs/architecture/commands.md` (329 → 330 commands; recount with the doc's command)
- Modify: `docs/superpowers/open-items.md` (owner smokes below)

Owner smokes (they replace П2, S10a, S12 and S14 in the checklist, which the controller updates):
- П2': Settings → Assistant access → Add to Claude desktop; restart Claude desktop → Athenaeum (dev) in its tools, the
  file's old keys kept, a `.bak` beside it. On OBS-PC1 (Windows): the same, and say whether Claude is the Store build.
- S10': ask the assistant to solve two named files → the plan window holds exactly those two.
- S14': ask about LDN 1272 → both sets named, the Stage one with its 4 frames, camera and `set:231`; ask for FWHM
  outliers → legacy figures shown apart, outliers by file name.
- S12': ask Claude Code in the repo for `list_operations(kind = "mcp_plan_apply")` → each applied plan with its id.

- [ ] **Step 1:** Edit the files.
- [ ] **Step 2:** `timeout 900 cargo check -p log-mcp --target-dir target-agent` — PASS.
- [ ] **Step 3: Commit** `docs(mcp): the smoke fixes, two owner rulings, owner smokes owed`.

## Not in this plan (owner's call)

- Starting Analysis from the assistant (a new plan kind).

## Execution notes (controller)

- Worktree `.claude/worktrees/mcp-smoke-fixes` (branch `mcp/smoke-fixes`), removed with `git worktree remove --force`
  after the merge (submodules; never `submodule deinit` there). Builds: `--target-dir <main checkout>/target-agent`;
  its `debug/` is deleted at the job's close.
- Agents: Tasks 1-5 `rust-engineer` (sonnet), one at a time; Task 6 `frontend-dev`; Task 7 the controller. One review
  per task sized to the diff; a fix round is re-gated by the controller; one fable review of the whole branch before the
  merge into local `main`. No push.
- Before the merge: the controller re-runs `cargo test -p athenaeum-core --lib`, the web parity test and the Task 6
  vitest file on the merged tree, launches `npm run tauri dev` against the dev catalog and repeats S14 and S10 through
  this session's MCP tools (set 231 with `tab: stage` and `set:231` in the text; a two-file plan).

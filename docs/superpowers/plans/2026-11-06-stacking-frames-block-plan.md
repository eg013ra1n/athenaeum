# Stacking Frames Block Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this repo the `lean-execution` skill overrides subagent-driven-development where they conflict.

**Goal:** Rebuild the Stacking tab's Frames block with the Analysis toolbar style, row selection, grouping, filters, outliers, visible references, reasons beside every status, and Blink over the selection with an Aligned / Original switch.

**Architecture:** Phase 1 is frontend plus one small command: the row model and the grouping/filter logic move into pure modules, `FramesTable` is rebuilt on a shared labelled-toolbar module extracted from the Analysis tab, and a `StackingBlink` component opens `BlinkViewer` in project mode with stacking actions. Phase 2 adds the plan's group members (grouping before the first run) and two core-backed commands that resolve and render a run's registered frame, which drive Blink's Aligned / Original switch.

**Tech Stack:** React 19 + TypeScript + Tailwind (Nord tokens in `tailwind.config.js`), vitest + Testing Library; Rust (`athenaeum-core`, Tauri 2 commands, Axum routes), `ts_rs` bindings.

**Spec:** `docs/superpowers/specs/2026-11-06-stacking-frames-block-design.md` (approved 2026-11-06). Executors read both; section numbers below (§6.4 …) are the spec's.

## Global Constraints

- Two backends in sync: every new Tauri command has its Axum route in the same commit; logic lives in `athenaeum-core`.
- No `@tauri-apps/*` outside `src/api/`; the frontend calls `api.invoke(...)`.
- Serde `#[serde(rename_all = "camelCase")]`; new model types go in `crates/athenaeum-core/src/ts_export.rs`; regenerate with `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`.
- Every command wears `#[tracing::instrument(skip_all, err)]` (Axum `err(Debug)`; `level = "debug"` for the per-frame image command).
- Never swallow errors: `console.error('[FramesTable] …', err)` / `notify({... tone: 'warning', hasErrors: true})` on a failed write; never `notify` on success-only noise.
- Design tokens only (`bg-surface-hover`, `text-content-muted`, `bg-accent/20`, `bg-error/10`, `bg-table-group`, …); no raw colours.
- UI strings in English, sentence case; dates `YYYY-MM-DD HH:MM:SS` via `formatTimestamp` (`src/utils/dateFormatting.ts`).
- Actions on the eligible subset: a button acting on a selection is enabled when its eligible set is non-empty and its label reads `Exclude (3 of 5)` when the eligible set is smaller than the selection.
- No Black Hole, no file deletion anywhere in the Stacking tab or its Blink.
- Test file names use letters, digits, `_` and `-` only.
- Disk rule: any new stacking run made for verification puts its working folder under `~/.athenaeum-acc/` with a `REGISTRY.tsv` row.
- Commit messages end with `Co-Authored-By: Claude <model> <noreply@anthropic.com>`; author is the repo user.

## Review Focus

1. **Hidden rows never take an action.** Selecting rows, then changing the search, the status chip, the Outliers toggle, the grouping, or collapsing a group, drops the hidden rows from the selection; Exclude / Include / Blink then count only visible rows. Pinned by Task 6 `selection_prunes_hidden_rows`.
2. **Switching the run in Results keeps the selection.** Row keys are uuids / frame ids, not indexes; a new `runDetail` keeps every selected key that still has a row. Pinned by Task 6 `selection_survives_run_switch`.
3. **A run in flight.** `summary` is `null`: the strip names the reference from the row with `regStatus = 'reference'`; Exclude, Include and the star are disabled; Blink stays enabled. Pinned by Task 5 `strip_in_flight_uses_reference_row` and Task 6 `run_active_disables_writes_not_blink`.
4. **Rows that left the project plan** (`editable: false`, key `h:<handle>` or an orphan uuid): never eligible for Exclude / Include / pin; Blink leaves a row with no uuid out and the notice counts it. Pinned by Task 6 `non_editable_rows_not_eligible` and Task 7 `blink_skips_rows_without_uuid`.
5. **Units with a group that has no pixel scale.** With `"` on, FWHM of a row whose group has no `pixelScaleArcsec` stays in px with a `px` suffix; it is never shown as arcsec. Pinned by Task 6 `units_fall_back_to_px_without_scale`.

---

## File Structure

| File | Responsibility |
| ---- | ---- |
| `src/components/toolbar/LabeledToolbar.tsx` (new) | The Analysis toolbar pieces: `ToolButton` (moved), `ToolSplit`, `ToolToggle`, `ToolDivider`, `BlinkToolButton` |
| `src/components/analysis/outlierLimit.ts` (new) | `autoOutlierLimit(values)`: the Analysis auto-threshold rule, shared |
| `src/components/stacking/frameRows.ts` (new) | Row model: `FrameRow`, `Outcome`, `buildFrameRows`, `buildProjectFrameRows`, `sortRows` (moved out of `FramesTable.tsx`) |
| `src/components/stacking/frameView.ts` (new) | Pure grouping / filtering / outliers / status counts / group headers |
| `src/components/stacking/FramesReferenceStrip.tsx` (new) | The two-line reference strip (§6.5) |
| `src/components/stacking/FramesTable.tsx` (rebuilt) | Header line, strip, toolbar, chips, grouped table, selection |
| `src/components/stacking/StackingBlink.tsx` (new) | Opens `BlinkViewer` over chosen rows, both subjects, stacking actions, aligned availability |
| `src/components/stacking/StackingTab.tsx` | Wiring: bulk exclusion, frame-set star, Blink, plan groups |
| `src/pages/FrameSetDetail.tsx` | `LightFrameRef` gains `night`, `exptimeSec`; imports `ToolButton` from the new module |
| `src/components/LightsAnalysisView.tsx` | Uses the shared toolbar module and `autoOutlierLimit`, no visible change |
| `src/components/BlinkViewer.tsx`, `src/components/blink/{types.ts,ToolBar.tsx}` | Aligned / Original switch |
| `crates/athenaeum-core/src/registration/db.rs` (+ `mod.rs` re-export) | `clear_frame_set_reference` |
| `crates/athenaeum-core/src/stacking/plan.rs` | `PlanGroup.member_frame_ids` |
| `crates/athenaeum-core/src/api/stacking_aligned.rs` (new, + `api/mod.rs`) | Aligned availability and path resolution |
| `crates/athenaeum-tauri/src/commands/{registration,stacking}.rs`, `src/lib.rs` | Command wrappers + registration |
| `crates/athenaeum-web/src/routes/{registration,stacking}.rs`, `routes/mod.rs` | Route mirrors + registration |
| `tailwind.config.js` | `blink` colour token |

---

# Phase 1

### Task 1: Shared labelled toolbar module

**Files:**
- Create: `src/components/toolbar/LabeledToolbar.tsx`, `src/components/toolbar/LabeledToolbar.test.tsx`
- Modify: `src/components/LightsAnalysisView.tsx:50-74` (remove `ToolButton`, re-export it from the new module so existing imports keep working), `:693-869` (Selection split, Blink, Units, View use the new pieces), `src/pages/FrameSetDetail.tsx:10`, `tailwind.config.js`

**Interfaces:**
- Produces:
  - `ToolButton({ label, title, onClick, disabled?, className?, children })`: moved verbatim.
  - `ToolSplit<T extends string>({ label, options: { value: T; title: string; content: ReactNode }[], value: T | null, onChange: (v: T) => void, disabled?: boolean })`: segmented split button. First segment `rounded-l-lg border-r-0`, last `rounded-r-lg`. Active segment `bg-accent/20 text-accent border-accent/40`, others `bg-surface-hover hover:bg-surface-elevated text-content-secondary border-border`. Segment width `min-w-10 px-2 h-7`. The label goes under, as in `ToolButton`.
  - `ToolToggle({ label, title, on, onClick, disabled?, children })`: one `w-10 h-7` toggle with the Units button's on/off classes (`bg-accent/20 border-accent text-accent` / `bg-surface-hover border-border text-content-secondary hover:text-content`).
  - `ToolSelection({ count, onClear, onInvert })`: the Analysis Selection split button (count clears, `ArrowLeftRight` inverts, label `Selection`).
  - `BlinkToolButton({ selectedCount, visibleCount, onClick })`: `bg-blink hover:bg-blink-hover text-white`, `Play` icon, label `Blink` when `selectedCount > 0`, else `Blink all`. Title `Blink N selected frame(s)` / `Blink all N frame(s)`. Disabled when `visibleCount === 0`.
  - `ToolDivider()`: `<span className="text-border h-7 flex items-center">|</span>`.
  - Token: `tailwind.config.js` `colors.blink = { DEFAULT: '#0891b2', hover: '#0e7490' }` (Tailwind's cyan-600 / cyan-700, so nothing changes visibly).

- [ ] **Step 1: Write the failing tests** in `LabeledToolbar.test.tsx`:
  - `blink_label_follows_selection`: `selectedCount=0, visibleCount=4` renders the label `Blink all` and the title `Blink all 4 frames`; `selectedCount=2` renders `Blink` and `Blink 2 selected frames`; `visibleCount=0` makes the button disabled.
  - `blink_uses_token_classes`: the button's class contains `bg-blink` and not `cyan`.
  - `split_marks_active_segment`: `ToolSplit` with value `night` gives that segment `aria-pressed="true"` and the class `bg-accent/20`; clicking another calls `onChange` with its value.
  - `selection_count_clears`: `ToolSelection count=3` shows `3`; clicking it calls `onClear`; the invert button calls `onInvert`.
- [ ] **Step 2: Run** `npx vitest run src/components/toolbar/LabeledToolbar.test.tsx`. Expected: FAIL, module not found.
- [ ] **Step 3: Implement the module and the token.** Copy the class strings from `LightsAnalysisView.tsx:693-869` exactly. Then switch the Analysis toolbar's Selection, Blink, Units and View markup to the new pieces, re-export `ToolButton` from `LightsAnalysisView.tsx` and change `FrameSetDetail.tsx:10` to import it from the new module.
- [ ] **Step 4: Run** `npx vitest run src/components/toolbar src/components/calibration src/pages` and `npx tsc --noEmit`. Expected: PASS, no type errors. The existing Analysis tests must pass unchanged.
- [ ] **Step 5: Commit** `refactor(ui): the Analysis toolbar pieces as a shared labelled-toolbar module`.

### Task 2: `clear_frame_set_reference` in both hosts

The core function already exists: `crates/athenaeum-core/src/registration/db.rs:296-303`, re-exported at `registration/mod.rs:23`, with a test at `db.rs:601`. Only the two host wrappers and their registration are missing (`docs/platesolving/precise-registration.md:355` already says they exist; this task makes that true).

**Files:**
- Modify: `crates/athenaeum-tauri/src/commands/registration.rs` (import `clear_frame_set_reference as core_clear_frame_set_reference` beside the pair); `crates/athenaeum-tauri/src/lib.rs:470-471` (register beside the pair); `crates/athenaeum-web/src/routes/registration.rs`; `crates/athenaeum-web/src/routes/mod.rs:254-255`; `docs/architecture/commands.md` (count 328 → 329, with a history clause)

**Interfaces:**
- Consumes: core `clear_frame_set_reference(conn: &Connection, frames_set_id: i64) -> Result<()>` (exists).
- Produces: Tauri `clear_frame_set_reference(frames_set_id: i64) -> Result<(), String>`; web `POST /api/clear_frame_set_reference` taking `FrameSetIdArgs` and returning `Json(())` (errors `INTERNAL_SERVER_ERROR`, like `get_`). Frontend: `api.invoke<void>('clear_frame_set_reference', { framesSetId })`.

- [ ] **Step 1: Write the failing web test** `clear_frame_set_reference_route_is_registered`, a router-level test in a new `#[cfg(test)] mod tests` in `routes/registration.rs`, shaped like `get_stacking_presets_route` (`routes/stacking.rs:733`): `test_state(None)` from `routes::tests` has no DB, so POST `/api/clear_frame_set_reference` with `{"framesSetId": 1}` must answer `500` with `DB not initialized`, not `404`. The core behaviour is already pinned by `db.rs:601`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-web clear_frame_set_reference`. Expected: FAIL with status `404`.
- [ ] **Step 3: Implement** the two wrappers, shaped like their `set_` siblings, and register both.
- [ ] **Step 4: Run** `cargo test -p athenaeum-web clear_frame_set_reference`, `cargo test -p athenaeum-core registration::db` and `cargo check -p athenaeum-tauri`. Expected: PASS. Recount with the `grep` in `docs/architecture/commands.md`; it should print 329.
- [ ] **Step 5: Commit** `feat(registration): clear_frame_set_reference in both hosts`.

### Task 3: Row model module

**Files:**
- Create: `src/components/stacking/frameRows.ts`, `src/components/stacking/frameRows.test.ts`
- Modify: `src/components/stacking/FramesTable.tsx` (remove the moved code and import it), `src/components/stacking/FramesTable.test.tsx` (the row-builder cases move to `frameRows.test.ts`), `src/components/stacking/subject.ts:1` (import `LightFrameRef` from `frameRows`), `src/pages/FrameSetDetail.tsx:381-399` (fill `night`, `exptimeSec`)

**Interfaces:**
- Produces:
  ```ts
  export interface LightFrameRef { frameId: number; filename: string; night: string | null; exptimeSec: number | null }
  export type OutcomeKind = 'reference' | 'included' | 'regFailed' | 'excluded' | 'notStackable';
  export interface Outcome { kind: OutcomeKind; label: string; tone: 'accent' | 'success' | 'warning' | 'error'; reason: string | null }
  export type RunFrameData = Pick<StackingRunDetail, 'groups' | 'frames'> & { run: Pick<StackingRunRow, 'id' | 'referenceMode'> };
  export interface RowContext { planGroupByFrameId: Map<number, string>; pinnedFrameId: number | null }
  export interface FrameRow {
    key: string; frameId: number | null; filename: string; publisher: string | null;
    groupKey: string | null; night: string | null; exptimeSec: number | null;
    weight: number | null; fwhmPx: number | null; eccentricity: number | null; stars: number | null;
    regRmsPx: number | null; lnScale: number | null; regInliers: number | null; regModel: string | null;
    outcome: Outcome | null; outOfNextRun: boolean;
    editable: boolean; unavailable: string | null; pinnable: boolean; pinned: boolean; pinNote: string | null;
  }
  export function buildFrameRows(lightFrames: LightFrameRef[], detail: RunFrameData | null, summary: RunSummary | null, excludedFrameIds: number[], ctx: RowContext): FrameRow[]
  export function buildProjectFrameRows(projectFrames: PlanProjectFrame[], detail: RunFrameData | null, summary: RunSummary | null, excluded: Set<string>, referenceFrameUuid: string | null, ctx: RowContext): FrameRow[]
  export function sortRows(rows: FrameRow[], column: SortKey, dir: 'asc' | 'desc'): FrameRow[]
  ```
  - `outcome`, per §6.4, decided in this order: `regStatus === 'failed'` → `{kind:'regFailed', label:'Reg failed', tone:'error', reason: exclusionReason with a leading 'registration failed: ' removed}`; `regStatus === 'reference'` → `{kind:'reference', label:'Reference', tone:'accent', reason:`run #${run.id} · ${run.referenceMode}`}`; `!included` → `{kind:'excluded', label:'Excluded', tone:'warning', reason: exclusionReason}`; included → `{kind:'included', label:'Included', tone:'success', reason:null}`. With no run row: a project row with a plan `reason` → `{kind:'notStackable', label:'Not stackable here', tone:'warning', reason}`, otherwise `null`.
  - `outOfNextRun` is exclusion-list membership, independent of the outcome.
  - `groupKey`: the run group's `groupKey` when a run row exists, else `ctx.planGroupByFrameId.get(frameId)` (the handle for a project), else `null`. Phase 1 passes an empty map.
  - A set row is `pinnable` when its frame is in `lightFrames`, and `pinned` when `frameId === ctx.pinnedFrameId`. A project row keeps today's pin rules.
  - `night` is the `YYYY-MM-DD` of `imaging_night.start_time`; `exptimeSec` is `frame.exptime`. A project row takes `exptimeSec` from `PlanProjectFrame.exptimeSec` and has `night: null`.

- [ ] **Step 1: Write the failing tests** in `frameRows.test.ts`. Keep every case moved from `FramesTable.test.tsx`, adjusted to `outcome`, and add:
  - `failed_reason_drops_prefix`: `exclusionReason: 'registration failed: quad seed failed (0 quad matches)'` gives reason `quad seed failed (0 quad matches)` and label `Reg failed`.
  - `reference_row_names_run_and_mode`: `regStatus: 'reference'`, run `{id: 8, referenceMode: 'auto'}` gives label `Reference` and reason `run #8 · auto`.
  - `weight_exclusion_keeps_full_reason`: `regStatus: 'skipped', included: false, exclusionReason: 'weight 0.006 below 0.05 of the group maximum'` gives kind `excluded` with the full reason.
  - `manual_exclusion_is_a_second_signal`: the reference row in the exclusion list keeps outcome `reference` and has `outOfNextRun: true`.
  - `no_run_row_no_outcome`: a set row with no run row has `outcome: null` and `groupKey: null`.
  - `project_unusable_row_is_not_stackable`: plan `reason: 'not on this device'` and no run row give kind `notStackable`.
  - `set_pin_follows_ctx`: `pinnedFrameId: 7` marks row 7 `pinned`. A run-only row (not in `lightFrames`) is not `pinnable`.
- [ ] **Step 2: Run** `npx vitest run src/components/stacking/frameRows.test.ts`. Expected: FAIL, module not found.
- [ ] **Step 3: Implement**: move the builders, add the fields, and populate `night` / `exptimeSec` in `FrameSetDetail.tsx`.
- [ ] **Step 4: Run** `npx vitest run src/components/stacking` and `npx tsc --noEmit`. Expected: PASS.
- [ ] **Step 5: Commit** `refactor(stacking-ui): the Frames row model as its own module, outcome + next-run exclusion`.

### Task 4: Grouping, filters and outliers

**Files:**
- Create: `src/components/analysis/outlierLimit.ts` (+ `.test.ts`), `src/components/stacking/frameView.ts` (+ `frameView.test.ts`)
- Modify: `src/components/LightsAnalysisView.tsx:354-369` (`autoFwhmPx` calls `autoOutlierLimit`)

**Interfaces:**
- Consumes: `FrameRow` (Task 3).
- Produces:
  ```ts
  export function autoOutlierLimit(values: number[]): number | null // median = sorted[floor(n/2)], MAD likewise, limit = median + 3 * max(MAD, 0.1 * median); null for []
  export type GroupMode = 'group' | 'night' | 'publisher' | 'status' | 'none';
  export type StatusFilter = 'all' | 'included' | 'regFailed' | 'excludedByRun' | 'outOfNextRun' | 'notStackable';
  export interface OutlierFlags { fwhm: boolean; ecc: boolean }
  export function computeOutliers(rows: FrameRow[], subjectKind: 'frameSet' | 'project'): Map<string, OutlierFlags>
  export function matchesStatus(row: FrameRow, f: StatusFilter): boolean
  export function statusCounts(rows: FrameRow[]): Record<StatusFilter, number>
  export interface FilterState { query: string; status: StatusFilter; outliersOnly: boolean }
  export function filterRows(rows: FrameRow[], f: FilterState, outliers: Map<string, OutlierFlags>): FrameRow[]
  export interface GroupHeaderInfo { total: number; included: number; integrationSec: number; medianFwhmPx: number | null; failed: number; excluded: number; topReason: string | null }
  export interface FrameGroupView { key: string; title: string; rows: FrameRow[]; info: GroupHeaderInfo }
  export interface GroupContext { groupOrder: string[]; groupTitle: (key: string) => string }
  export function groupRows(visible: FrameRow[], all: FrameRow[], mode: GroupMode, ctx: GroupContext): FrameGroupView[]
  ```
  - `computeOutliers` builds the limit from the `included`/`reference` rows of each bucket and flags every row of the bucket. A frame set buckets by `groupKey`; a project buckets by `groupKey + publisher`. FWHM and Ecc each get their own limit.
  - `matchesStatus`: `included` matches outcome kinds `included` and `reference`; `regFailed` matches `regFailed`; `excludedByRun` matches `excluded`; `notStackable` matches `notStackable`; `outOfNextRun` matches `row.outOfNextRun`; `all` matches everything.
  - `filterRows`: a case-insensitive substring match on `filename`, plus `publisher` for a project.
  - `groupRows` takes header totals from `all` rows with the same bucket key. A group with no `visible` rows is dropped. Mode `group` orders buckets by `ctx.groupOrder` (the plan's group order), with `null` keys last under `Not grouped yet`. `night` orders by date ascending, with `null` under `No night`. `publisher` orders alphabetically. `status` uses the order `Reference, Included, Reg failed, Excluded, Not stackable here, Not run yet`, keyed by outcome. `none` returns one bucket with key `all`.
  - `integrationSec` sums `exptimeSec` over the included rows. `medianFwhmPx` is the upper median over the included rows. `topReason` is the most frequent `outcome.reason` among `regFailed` rows, with ties broken by the first occurrence in `all` order.

- [ ] **Step 1: Write the failing tests:**
  - `autoOutlierLimit_matches_analysis`: `[2.0, 2.1, 2.2, 2.3, 9.0]` gives median `2.2`, MAD `0.1`, and an effective MAD of `max(0.1, 0.22) = 0.22`, so the limit is `2.86` (within `1e-9`). `[]` gives `null`.
  - `lost_group_header`: a fixture of group `B` with 25 `regFailed` rows (22 with reason `quad seed failed (0 quad matches)`, 3 with `too few stars (subject 0, reference 196)`) and 3 `excluded` rows gives `info = {total: 28, included: 0, failed: 25, excluded: 3, topReason: 'quad seed failed (0 quad matches)'}`.
  - `header_totals_ignore_filter`: filtering `L` to one visible row still reports `total` over all `L` rows.
  - `project_outliers_per_publisher`: in one group, Alice has six rows at FWHM `2.0` and Bob two at `4.0`. Bucketed per publisher, the limits are `2.6` (Alice) and `5.2` (Bob), so nobody is flagged. Under a single publisher the median is `2`, MAD `0`, the floor `0.2` and the limit `2.6`, so Bob's two rows are flagged.
  - `out_of_next_run_counts_every_listed_row`: a `reference` row and a `regFailed` row, both listed, count 2 under `outOfNextRun` and each still counts under its own outcome.
  - `status_mode_order`: the buckets come out in the fixed order above, with no empty buckets.
- [ ] **Step 2: Run** `npx vitest run src/components/stacking/frameView.test.ts src/components/analysis/outlierLimit.test.ts`. Expected: FAIL.
- [ ] **Step 3: Implement** both modules. Switch `autoFwhmPx` to `autoOutlierLimit(values)`.
- [ ] **Step 4: Run** `npx vitest run src/components/analysis src/components/stacking`. Expected: PASS, and the Analysis tests are unchanged.
- [ ] **Step 5: Commit** `feat(stacking-ui): Frames grouping, filters, outliers and group headers (pure)`.

### Task 5: Reference strip

**Files:**
- Create: `src/components/stacking/FramesReferenceStrip.tsx`, `FramesReferenceStrip.test.tsx`

**Interfaces:**
- Produces: `FramesReferenceStrip({ runDetail: StackingRunDetail | null, planReference: PlanReference | null, groupLabel: (groupKey: string) => string, filenameOf: (runFrameId: number) => string | null })`.
  - Line 1, with a run selected: `Registration reference, run #<id>: <file> · <group> · weight <w.toFixed(3)> · <mode>`, plus ` · two-pass moved it from <file of switchedFrom>` when `switchedFrom != null`. The file, weight and mode come from `summary.reference`. With `summary === null`, the file comes from the row with `regStatus = 'reference'` through `filenameOf`, and the weight is left out. In native mode (`summary.config.registration.geometry === 'native'`, the test `ResultsPanel.tsx:637` uses), the line reads `Registration reference, run #<id>: per group`. With no run, line 1 is left out.
  - Line 2: `Next run: auto, picked at run time`, or `Next run: <planReference.filename> · manual`, with an error `XCircle` and title `The reference file is not on this device` when `onDisk` is false.

- [ ] **Step 1: Write the failing tests:** `strip_auto_with_weight`, `strip_two_pass_names_previous`, `strip_native_per_group`, `strip_in_flight_uses_reference_row` (summary `null`, row 7 `regStatus: 'reference'`, `filenameOf(7) = 'L_0037.fits'` gives `Registration reference, run #8: L_0037.fits`), `strip_next_run_manual_off_disk`.
- [ ] **Step 2: Run** `npx vitest run src/components/stacking/FramesReferenceStrip.test.tsx`. Expected: FAIL.
- [ ] **Step 3: Implement.** Use `bg-surface-hover/40 rounded-lg px-3 py-2 text-xs`, the `Star` icon in `text-accent`, file names in `font-mono`.
- [ ] **Step 4: Run** the same command. Expected: PASS.
- [ ] **Step 5: Commit** `feat(stacking-ui): the Frames reference strip`.

### Task 6: Rebuild `FramesTable`

**Files:**
- Modify: `src/components/stacking/FramesTable.tsx` (rebuilt), `src/components/stacking/FramesTable.test.tsx`, `src/components/stacking/stackingPrefs.ts` (+ test)

**Interfaces:**
- Consumes: Tasks 1, 3, 4, 5.
- Produces the new props (they replace `onToggleExclude`):
  ```ts
  export interface FramesTableProps {
    subjectKind: StackSubject['kind'];
    lightFrames: LightFrameRef[]; projectFrames: PlanProjectFrame[];
    runDetail: StackingRunDetail | null;
    planGroups: PlanGroup[]; planReference: PlanReference | null;
    excludedKeys: string[]; referenceFrameUuid: string | null; pinnedFrameId: number | null;
    planGroupByFrameId: Map<number, string>;
    onSetExcluded: (keys: string[], excluded: boolean) => void; // bulk; the caller writes the list
    onPin: (key: string) => void;                                // both subjects
    onBlink: (rows: FrameRow[]) => void;                         // already in table order
    collapsed: boolean; onToggleCollapsed: () => void;
    disabled?: boolean;                                          // a run is active
  }
  ```
  - Layout, per §6.1. The header line reads `Frames (N)` and `metrics from run #<id> · <formatTimestamp(run.startedAt)>`, or `no run yet`. Then the strip, then the toolbar: `ToolSelection`, `BlinkToolButton`, `Exclude`, `Include` (`ToolButton`), `ToolDivider`, `ToolSplit` labelled `Group by` (set: Group / Night / Status / None; project: Group / Publisher / Status / None), `ToolToggle` Units (`px` / `"`, shown when any `planGroups[].pixelScaleArcsec != null`), `ToolToggle` Outliers, `ToolToggle` Columns, `ToolDivider`, and a search field (`h-7 text-xs`, label `Search` under it). The chip row comes next, using `FilterChip` shapes without the dot.
  - Table: the first column is the selection checkbox, with `LightsAnalysisTable.handleRowSelect` semantics (click toggles; Shift+click applies the clicked row's direction to the range in displayed order). Default columns: File, Publisher (project), Weight, FWHM, Ecc, Seeds, Status, Reference. Columns adds Reg RMS, Inliers, Model (keep `splitRegModel` + `WCS` chip), Scale. Group header rows use `bg-table-group` and are clickable to collapse; their text follows §6.6. A lost group's summary is `text-error`: `0 / 28 included — 25 registration failed (<topReason>) · 3 excluded`. In Group mode the header also names, from the selected run's summary group (`summary.groups.find(s => s.key === groupKey)`), `Reference: <file>` when `summary.config.registration.geometry === 'native'` and `referenceFrameId` is set, and `LN ref: <file>` when `normalizationReferenceFrameId` is set. Both resolve through the summary frames' `filename`. Rows: a `regFailed` outcome adds `bg-error/10`, `outOfNextRun` adds `bg-warning-muted/40`, and an outlier cell adds `bg-warning-muted text-warning`. Status cell: the outcome chip (`STATUS_CHIP` tones plus accent `bg-accent/20 text-accent`), the `Out of next run` chip when listed, and the muted reason (`truncate`, full text in `title`). Reference cell: `Star`, `fill-current text-accent` when `pinned`, disabled unless `pinnable || pinned`. An empty result shows `No frames match these filters`. The `<thead>` is `sticky top-0`, inside a `max-h-[70vh] overflow-y-auto` wrapper.
  - Eligibility: Exclude = selected ∧ visible ∧ `editable` ∧ ¬`outOfNextRun`; Include = selected ∧ visible ∧ `outOfNextRun`. Exclude, Include and the star are disabled when `disabled`; Blink is not.
  - Units: arcsec = `fwhmPx × planGroups.find(g => g.key === row.groupKey)?.pixelScaleArcsec`. With no scale, the cell keeps px.
  - `stackingPrefs.ts` gains `readFramesView(): { groupMode: GroupMode; arcsec: boolean; columns: boolean }` and `writeFramesView(v)` (local storage, wrapped in try/catch with `console.warn`, like `readFramesCollapsed`). Group collapse state is per render only.

- [ ] **Step 1: Write the failing tests** in `FramesTable.test.tsx`, on a run #8-like fixture (groups L, R, B) plus the project fixture already there:
  - `groups_by_stacking_group_by_default`: header rows `L`, `R`, `B` appear in plan order. The `R` header reads `0 / 4 included — 4 registration failed (0 quad matches)`.
  - `exclude_counts_eligible_subset`: select 5 rows, 2 of them already listed. The Exclude label reads `Exclude (3 of 5)`; clicking it calls `onSetExcluded` with the 3 keys and `true`.
  - `shift_click_selects_range`: click row 1, Shift+click row 4, and rows 1–4 are selected.
  - `selection_prunes_hidden_rows` (Review Focus 1): select 3 rows, type a query that hides 2, and Blink calls `onBlink` with 1 row.
  - `selection_survives_run_switch` (Review Focus 2): rerender with another `runDetail` that has the same keys, and the selection count stays the same.
  - `run_active_disables_writes_not_blink` (Review Focus 3): with `disabled`, Exclude, Include and the star are disabled and Blink is enabled.
  - `non_editable_rows_not_eligible` (Review Focus 4): an orphan project row in the selection is not counted by Exclude and has no star.
  - `units_fall_back_to_px_without_scale` (Review Focus 5): with `"` on, a row in a group with scale `1.2` and FWHM `2.0` shows `2.40"`, and a row in a group with no scale shows `2.00 px`.
  - `reference_star_pins_set_row`: with a set subject, clicking a row's star calls `onPin(String(frameId))`.
  - `no_match_message`: the query `zzz` shows `No frames match these filters`.
  - `group_header_names_ln_and_native_references`: a native-mode summary whose group `L` has `referenceFrameId: 7` and `normalizationReferenceFrameId: 9` gives an `L` header containing `Reference: L_0007.fits` and `LN ref: L_0009.fits`. The same group in co-registered mode shows only `LN ref: …`.
  - `out_of_next_run_chip_counts_listed`: the chip count equals `excludedKeys.length`.
- [ ] **Step 2: Run** `npx vitest run src/components/stacking/FramesTable.test.tsx`. Expected: FAIL.
- [ ] **Step 3: Implement** the component on the Task 1/3/4/5 modules, and the prefs pair.
- [ ] **Step 4: Run** `npx vitest run src/components/stacking` and `npx tsc --noEmit`. Expected: PASS. `StackingTab` will not compile until Task 7, so run `tsc` after Task 7 if it fails only on `StackingTab` props.
- [ ] **Step 5: Commit** `feat(stacking-ui): the Frames block on the Analysis toolbar — selection, grouping, filters, reasons`.

### Task 7: Wire the tab and Blink of originals

**Files:**
- Create: `src/components/stacking/StackingBlink.tsx`, `StackingBlink.test.tsx`
- Modify: `src/components/stacking/StackingTab.tsx:233-243` (bulk exclusion), `:530-543` (pin for both subjects), `:870-880` (`pinnedReference`), `:1274-1286` (new props), `StackingTab.test.tsx`, `src/pages/FrameSetDetail.tsx:1098` (pass `onReferenceChanged`)

**Interfaces:**
- Consumes: Task 2 (`clear_frame_set_reference`) and Task 6 (`FramesTableProps`).
- Produces:
  - `handleSetExcluded(keys: string[], excluded: boolean)`: one `setUserExcluded` call that adds or removes every key.
  - Frame-set `handlePinReference(key)`: pinning a non-pinned row calls `api.invoke('set_frame_set_reference', { framesSetId, frameId: Number(key) })`, then sets `reference.mode = 'manual'` through `setUserConfig`. Clicking the pinned row calls `api.invoke('clear_frame_set_reference', { framesSetId })`, then sets `mode = 'auto'`. Both re-plan through the tab's existing re-plan path, so `plan.reference` updates. A failure gets `console.error('[StackingTab] frame-set reference write failed:', err)` and `notify({ title: 'Could not set the reference frame', detail, kind: 'stacking', tone: 'warning', hasErrors: true })`. The project branch is today's code.
  - `StackingTab` gains the prop `onReferenceChanged?: (ref: FrameSetReference | null) => void`, called after a successful set (with the new `FrameSetReference`) or clear (with `null`). `FrameSetDetail` passes the same setter its Analysis tab uses (`(ref) => setReferenceFrameId(ref?.referenceFrameId ?? null)`, `FrameSetDetail.tsx:1144`), so the Analysis star follows without a reload (spec §6.5: after a pin or an unpin both tabs agree).
  - `pinnedFrameId` for a set: `draftConfig.reference.mode === 'manual' ? plan.reference.frameId : null`.
  - `StackingBlink({ subject: StackSubject, rows: FrameRow[], runDetail: StackingRunDetail | null, excludedKeys: string[], disabled: boolean, onSetExcluded, onPin, onClose })`. It is mounted keyed by an open counter, like `ProjectBlink`. Every entry carries `key: row.key`, because `useBlinkProjectMode` snapshots the entries and reads live badges and eligibility by `key` (`useBlinkProjectMode.ts:31-48`). A set loads `get_files_with_frames_by_ids { frameIds }`, which returns `FileWithFrame`s ordered by `date_obs` (`db/operations.rs:3342`) with no key; the set branch re-orders them by the rows' `frameId` and attaches each row's `key`. A project loads `get_collab_blink_frames { projectId, refs: rows.filter(r => uuid).map(r => ({ frameId: null, frameUuid: r.key })) }`. If entries come back for fewer rows than were asked for, it calls `notify({ title: 'Some frames are not on this device', detail: '<n> of <m> frames are not on this device.', kind: 'stacking', tone: 'warning' })`; with none it closes after that notice. It renders `BlinkViewer` in project mode: `actions` = `Exclude from next run (n)` (eligible: editable, not listed), `Include (n)` (eligible: listed), and `Use as reference` (eligible only while the Blink selection holds exactly one pinnable entry; it runs `onPin`). Each `badge` is the outcome label, plus `· out of next run` when listed. `contextLabel` = `<group title> · <outcome reason>`. All actions are hidden while `disabled`. Entries keep the table's row order.

- [ ] **Step 1: Write the failing tests:**
  - `StackingTab.test.tsx`: `bulk_exclude_writes_one_list`, where Exclude on 3 rows writes the list once with all 3 keys; `set_star_sets_then_clears`, where the first click invokes `set_frame_set_reference` then puts `mode: 'manual'` in the persisted config, and the second invokes `clear_frame_set_reference` then puts `mode: 'auto'`; and `set_star_tells_the_page`, where `onReferenceChanged` is called with `{ framesSetId, referenceFrameId: 7, … }` after the set and with `null` after the clear.
  - `StackingBlink.test.tsx`: `blink_skips_rows_without_uuid` (Review Focus 4), where project rows `u1`, `u2` and `h:9` ask for 2 refs, and with 1 entry back the notice reads `1 of 3 frames are not on this device.`; `blink_actions_follow_eligibility`, where with 2 entries selected, one of them listed, the toolbar shows `Exclude from next run (1)` and `Include (1)`, and `Use as reference` is hidden; `blink_keeps_table_order`, where a set whose `get_files_with_frames_by_ids` mock returns frames 3, 1, 2 (date order) for rows 1, 2, 3 still blinks 1, 2, 3; and `badge_flips_after_exclude`, where after `Exclude from next run` the parent rerenders with the key listed and the entry's badge gains `· out of next run`.
- [ ] **Step 2: Run** `npx vitest run src/components/stacking`. Expected: FAIL.
- [ ] **Step 3: Implement** the wiring and `StackingBlink`, and mount it from `StackingTab` through `FramesTable.onBlink`.
- [ ] **Step 4: Run** `npx vitest run` (whole frontend) and `npx tsc --noEmit`. Expected: PASS.
- [ ] **Step 5: Verify in the app**: `npm run tauri dev` on the dev DB. On frame set 182 / run #8, check that the R, G and B headers read lost, that the strip names the L reference, that bulk exclude works, and that Blink opens originals with the stacking actions. On project `af136ec9` / run #9, check the Publisher grouping, the pin and Blink. Ask the owner to look at the window (screen capture is blocked). Note the result in the ledger.
- [ ] **Step 6: Commit** `feat(stacking-ui): bulk exclusion, the frame-set star and Blink over the selection`.

# Phase 2

### Task 8: The plan names each group's members

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/plan.rs:157` (`PlanGroup`) and `:3759` (push), the plan test module, `src/types/stacking.ts` (regenerated), `src/components/stacking/StackingTab.tsx` (build `planGroupByFrameId`), `docs/superpowers/open-items.md` (delete the `"—" in the frames table's Group column` clause)

**Interfaces:**
- Produces: `PlanGroup.member_frame_ids: Vec<i64>`, which is `g.frames.iter().map(|f| f.frame_id)` in group order: every member, manually excluded ones included, in the run's namespace (a catalog frame id for a set, a project-frame handle for a project). TS: `memberFrameIds: Array<number>`. Frontend: `planGroupByFrameId = new Map(plan.groups.flatMap(g => g.memberFrameIds.map(id => [id, g.key])))`.

- [ ] **Step 1: Write the failing core test** `plan_groups_name_their_members`, in the plan tests next to the existing group-count tests. On the existing set fixture, every `PlanGroup` has `member_frame_ids.len() == frame_count`, the ids across groups are disjoint, and their union is the set's light frames.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core stacking::plan -- plan_groups_name_their_members`. Expected: FAIL, no field.
- [ ] **Step 3: Implement** the field. Then run `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` and wire `planGroupByFrameId` in `StackingTab`.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core stacking::plan`, `cargo test -p athenaeum-core --test ts_contract`, and `npx vitest run src/components/stacking` with a new `FramesTable` case `groups_before_first_run` (no run, plan groups → headers `L`, `R`). Expected: PASS.
- [ ] **Step 5: Commit** `feat(stacking): the plan names each group's members; Frames groups before the first run`.

### Task 9: Aligned-frame core API and commands

**Files:**
- Create: `crates/athenaeum-core/src/api/stacking_aligned.rs` (+ `pub mod` in `api/mod.rs`)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register both types in `stacking.ts`), `crates/athenaeum-tauri/src/commands/stacking.rs` + `src/lib.rs:490`, `crates/athenaeum-web/src/routes/stacking.rs` + `routes/mod.rs:274`, `docs/architecture/commands.md` (329 → 331)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
  #[serde(rename_all = "camelCase")]
  pub enum AlignedState { Available, RegistrationFailed, NotRegistered, Missing, Replaced }

  #[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
  #[serde(rename_all = "camelCase")]
  pub struct AlignedFrameStatus { pub frame_id: i64, pub state: AlignedState }

  pub fn get_stacking_aligned_frames(ctx: &ServiceContext, run_id: i64, frame_ids: &[i64]) -> Result<Vec<AlignedFrameStatus>, ApiError>
  pub fn resolve_stacking_aligned_path(ctx: &ServiceContext, run_id: i64, frame_id: i64) -> Result<PathBuf, ApiError> // Err unless Available
  ```
  - State per frame, decided in this order:
    1. No run row → `NotRegistered`.
    2. The run row's `reg_status = 'failed'` → `RegistrationFailed`.
    3. A later run of the same subject (`stacking_runs.id > run_id`, same `frames_set_id` / `project_id`) has a `stacking_run_groups` row with the frame's group key → `Replaced`.
    4. A `stacking_artifacts` row with `kind = 'registered'` for (subject, group key, `frame_id` for a set / `project_frame` for a project) exists, and its `path` exists on disk → `Available`, whatever else `reg_status` says. A frame cut by the weight floor after Register (`weights::weight_gate`, Plan D D1w) still has its aligned file, and it is worth blinking. Look the row up with `db::stacking::find_artifact(conn, subject, key, "registered", Some(frame_id))`. The path is the row's `path`, never rebuilt from the subject's name.
    5. `reg_status` is `aligned` or `reference` (the run registered it) → `Missing`.
    6. Otherwise → `NotRegistered`: a frame cut before Register, e.g. by weight. This is a fourth reason the spec did not name; its Blink text is `not registered in run #N`.
  - Tauri: `get_stacking_aligned_frames(run_id, frame_ids) -> Result<Vec<AlignedFrameStatus>, String>` and `get_stacking_aligned_image(run_id, frame_id, resolution: Option<String>) -> Result<tauri::ipc::Response, String>`, the latter via `crate::commands_rustafits::read_fits_image_bytes` (the `get_collab_frame_image` shape). Web: `POST /api/get_stacking_aligned_frames` and `POST /api/get_stacking_aligned_image`, the latter via `crate::routes::images::render_path_jpeg`.

- [ ] **Step 0: Make the end-to-end run helpers reachable.** `stacking::test_fixtures` (`pub(crate)`) holds set, light and FITS builders only. The run driver lives privately in `run.rs`'s `mod tests` (`:15897`): `seed_star_group` (`:17564`), `star_light_spec` (`:16250`), `wait_for_run` / `wait_for_run_within` (`:16275`, `:16283`) and `date_obs_at` (`:17551`). Move them to `stacking/test_fixtures.rs` as `pub(crate)` and re-import them in `run.rs`'s tests. Run `cargo test -p athenaeum-core stacking::run` and expect every existing test to pass unchanged. Commit `test(stacking): the end-to-end run helpers as shared fixtures`.
- [ ] **Step 1: Write the failing core tests** in `stacking_aligned.rs`, driving real runs with the Step 0 helpers. Per-state fixtures:
  - `RegistrationFailed`: the starless flat-field frame trick used at `run.rs:21459-21465`.
  - `NotRegistered`: a group with one much noisier frame, with `config.selection.min_weight_fraction` raised so that frame is cut at the Measure-time weight gate, before Register. Check in the run row that its `reg_status` is `skipped` and that no registered artifact exists.
  - `Replaced`: a second `start_stacking` on the same set after `wait_for_run`.
  - `Missing`: delete the artifact's file after the run.
  - A post-Register weight cut that stays `Available`: a frame the end-of-Register reweigh cuts (`run_reweigh_tests.rs` builds one). Its run row is not included, its registered artifact exists, and the test `aligned_available_after_post_register_weight_cut` expects `Available`.

  Tests: `aligned_available_after_run`, `aligned_failed_frame_reports_registration_failed`, `aligned_weight_cut_reports_not_registered`, `aligned_missing_when_file_deleted`, `aligned_replaced_by_later_run_of_group`, `aligned_older_run_kept_when_later_run_lacks_group`, and `aligned_path_survives_set_rename` (rename the set after the run; the state is still `Available` and the path is the artifact row's).
- [ ] **Step 2: Run** `cargo test -p athenaeum-core api::stacking_aligned`. Expected: FAIL.
- [ ] **Step 3: Implement** the core module, the four wrappers and their registration, and the `ts_export` entries. Then regenerate the bindings.
- [ ] **Step 4: Run** `cargo test -p athenaeum-core api::stacking_aligned`, `cargo test -p athenaeum-core --test ts_contract`, `cargo check -p athenaeum-tauri -p athenaeum-web`, and recount the commands (expect 331). Expected: PASS.
- [ ] **Step 5: Commit** `feat(stacking): aligned run frames for Blink — availability and image, both hosts`.

### Task 10: Blink's Aligned / Original switch

**Files:**
- Modify: `src/components/blink/types.ts` (`BlinkFrame`, `ToolBarProps`), `src/components/blink/ToolBar.tsx`, `src/components/BlinkViewer.tsx` (image load at `:276` and `:601`, caches, overlays), `src/components/stacking/StackingBlink.tsx`, tests `BlinkViewer.test.tsx`, `StackingBlink.test.tsx`

**Interfaces:**
- Consumes: Task 9 commands; Task 7 `StackingBlink`.
- Produces:
  - `BlinkFrame.aligned?: { runId: number; frameId: number } | { runId: number; reason: string }`. The first form loads through `get_stacking_aligned_image`; the second is an entry shown as original, and `reason` goes into the context strip as `original — <reason>`.
  - `ToolBarProps.imageVariant?: { aligned: boolean; onToggle: () => void }`. It renders as a toggle styled like the full-resolution button (`p-2 rounded`, `bg-accent text-surface` when on), titled `Show aligned frames (run #N)` / `Show original frames`, with an icon of the builder's choice. It is shown only when some entry has the first `aligned` form.
  - `BlinkViewer`: `alignedMode` state, default `true` when shown. `loadImage` and the full-res load call `get_stacking_aligned_image { runId, frameId, resolution }` for an entry with the first form while `alignedMode`, and otherwise the existing paths. Toggling `alignedMode` revokes and clears `loadedImages` and the full-res blob, the way a full-res toggle resets its blob. While `alignedMode` is on, the star annotations and the flat contour are off and their buttons are disabled with the title `Not available on aligned frames`.
  - `StackingBlink`: when `runDetail` is set, it calls `get_stacking_aligned_frames { runId, frameIds }` once at open, using each row's run `frameId`, and maps the states to `aligned` like this: `available` → first form; `registrationFailed` → `registration failed in run #N`; `notRegistered` → `not registered in run #N`; `missing` → `not in the working folder any more`; `replaced` → `a later run replaced it`. If the call fails it logs, opens with originals, and notifies `Aligned frames are unavailable`.

- [ ] **Step 1: Write the failing tests:**
  - `BlinkViewer.test.tsx`: `aligned_mode_loads_run_frame`, where an entry with `aligned: {runId: 8, frameId: 7}` invokes `get_stacking_aligned_image` with `{runId: 8, frameId: 7}`; `toggle_reloads_original`, where after a toggle the same index invokes the original path; `aligned_disables_annotations`; and `no_switch_without_aligned_entries`.
  - `StackingBlink.test.tsx`: `maps_aligned_states_to_reasons`, covering all five states, and `aligned_call_failure_opens_originals`.
- [ ] **Step 2: Run** `npx vitest run src/components/BlinkViewer.test.tsx src/components/stacking/StackingBlink.test.tsx`. Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `npx vitest run` and `npx tsc --noEmit`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(blink): Aligned / Original switch over a stacking run's registered frames`.

### Task 11: Docs, full gate and real-data acceptance

**Files:**
- Modify: `docs/stacking/README.md` (the frames table paragraph at ~line 447 and the project Frames table section at ~1644: selection, toolbar, outcome + next-run chips, strip, star for both subjects, grouping, outliers, Blink and the aligned rule), `docs/superpowers/open-items.md` (new smokes: the strip on a two-pass run; the frame-set star across Analysis and Stacking; aligned Blink on a frame-set run; Blink on a project run with a member's frame missing), `docs/architecture/commands.md` (history clause for 331)

- [ ] **Step 1: Run the full gate:** `cargo test -p athenaeum-core` (all targets), `cargo test -p athenaeum-web`, `cargo check -p athenaeum-tauri`, `npx vitest run`, `npx tsc --noEmit`. Expected: PASS. Known timing flakes are listed in `reference_collab_live_timing_test_flake` and are not regressions.
- [ ] **Step 2: Real-data acceptance.** Launch `npm run tauri dev` on the dev DB. On project `af136ec9` run #9, Aligned Blink should show stars standing still across frames and the context strip should read `original — …` for frames without one. For a frame set, make a fresh small run on set 182 with its working folder under `~/.athenaeum-acc/<date>-frames-block/` (add a `REGISTRY.tsv` row). Aligned Blink must show the L frames aligned and the R frames as `original — registration failed in run #N`. Then delete the folder and its registry row once the result is recorded.
- [ ] **Step 3: Write the docs.**
- [ ] **Step 4: Commit** `docs(stacking): the Frames block — reference, toolbar, grouping, Blink`.

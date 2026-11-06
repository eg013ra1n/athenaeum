# Collab v3 wave 6c — the project's Stacking tab — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task, run under the user skill `lean-execution` (it overrides SDD where they conflict). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A processor stacks a collaboration project from the app: a Stacking tab on the project page, the canonical grid card, the Prepare stage, the Frames table by frame uuid, the coverage and crop outputs, and run progress and notifications keyed by subject.

**Architecture:** `StackingTab` stops taking a frame-set id and takes a `StackSubject` (`frameSet | project`). A small adapter hook maps every command to the subject's variant, and `useStackingRuns` keys its maps by a subject key (`set:<id>` / `project:<projectId>`). The board, inspector, Frames table and Results panel read project-specific facts from the plan the backend already returns (`StackingPlan.subject` fields, `grid`, `projectFrames`). One backend change: the plan reports what Prepare will do (`StackingPlan.prepare`).

**Tech Stack:** React 19 + TS, Tailwind design tokens, `src/components/ui/` primitives, vitest + Testing Library; Rust (`athenaeum-core`, ts-rs) for Tasks 1, 1b and 1c only.

**Spec:** `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md` — §12 (the UI), §4.3, §5.0–§5.1, §5.4, §6.3, §7, §8, §9.1–§9.2, §13, §18 (6c). The backend is merged: plans 6a, 6b-1, 6b-2, 6b-3 (`docs/superpowers/plans/2026-10-0{2,3}-collab-v3-wave6*`). Reference: `docs/stacking/README.md` ("The project path, wave 6b-2" and "The project path on the grid, wave 6b-3").

## What exists (verified 2026-11-05 on `main` be695679)

- Commands, both hosts, registered: `get_project_stacking_plan {projectId, config?}`, `start_project_stacking {projectId, config?, rerunFrom?}`, `get_project_stacking_runs {projectId, limit}`, `get_project_stacking_config {projectId}`, `set_project_stacking_config {projectId, config, excludedFrameUuids, referenceFrameUuid?}`, `get_project_stacking_work_usage {projectId}`, `cleanup_project_stacking_work {projectId, what}`, `get_collab_grid_proposal {projectId}`, `set_collab_project_grid {projectId, grid}`. The run-id commands (`cancel_stacking`, `get_stacking_run`, `get_master_light_preview`) serve both subjects.
- Types in `src/types/stacking.ts`: `StackingPlan.{setId|null, projectId|null, grid: PlanGrid|null, projectFrames: PlanProjectFrame[]}`, `PlanGrid {state: 'unset'|'proposed'|'confirmed', current, proposal, framesInside, canEdit}`, `PlanProjectFrame {handle|null, frameUuid, fileName, publisher, filterCanonical, exptimeSec, colorMode, hasWcs, onDisk, reason|null}`, `PlanGroup.exposureClass`, `GroupingConfig.{exposureMode, stopsWidth}`, `OutputConfig.{coverageMap, cropToCoverage}`, `StackingProjectConfig`, `StackingProgressEvent`/`StackingCompleteEvent` with `setId|null` + `projectId|null`. `src/types/models.ts`: `CanonicalGrid`, `GridProposal`, `CollabGridChanged {projectId}`, `CollabFramesChange {projectId, kind, count}`, `ExportReadiness.attested`.
- Blocker codes a project plan can carry: `grid | folders | space | frames | unsupported` (`stacking/plan.rs`).
- The UI uses none of it. `useStackingRuns` drops every event without a `setId` (comment: "wave 6c"). No grouping or coverage control exists in the UI for any subject. Frame sets choose a manual reference in the Analysis tab.
- Already shipped by 6a, so not in this plan: the project export dialog's linked / copied / rewritten / failed counts (`exportCountsText`), the Moderation → Filters remap and its notifications.
- `scripts/ui-harness/` renders the project page on fixture data (scenarios `default|long|filters|empty|offline|review`), commands over HTTP only, no events.

## Decisions taken in this plan (spec silent or a choice of placement)

| # | Decision | Why |
| ---- | ---- | ---- |
| D1 | **Exposure grouping control** sits in the Groups table header, for both subjects: the four modes of §8 and each mode's threshold (`exposureToleranceSec` for `seconds`, `stopsWidth` for `stops` and `equivalent`). A frame set's default stays `seconds` (owner 2026-11-05: «Оставляй по умолчанию секунды»); a project's stays `stops` 1.0 (W1). The owner's two wants — a settable threshold and combining exposures by the telescope's parameters — are the thresholds and `equivalent` (t/N², N = FOCALLEN / APTDIA). The measured rung of `equivalent` (the zero point) is sub-wave 6.5, not this plan. | §8 makes the modes a config field for both; groups are where the result shows. |
| D2 | **Manual reference of a project** = a pin column in the project Frames table (§12). Enabled on every usable row (`onDisk ∧ reason == null`). A row without `hasWcs` (no manifest WCS) is pinnable with the tooltip `No WCS in the manifest — plate-solved locally first`. Pinning sets `reference.mode = 'manual'` and `referenceFrameUuid`; "Choose in Analysis" is hidden for a project. Frame sets are unchanged. | §12 names the pin; a project has no Analysis tab. `hasWcs` is the manifest WCS only (`plan.rs:~2603`) and §5.4 admits locally solved frames, so a manifest-only pin would leave a project of older publishers with no manual reference (review I4). |
| D3 | **Prepare counts** come from the plan (`StackingPlan.prepare`, Task 1): a prediction made from the same `probe_file` + `prepare_action` the plan already runs. What the run actually did stays in its exclusions and warnings. | §12 asks for the counts; the run summary has none, and the plan already probes every file. |
| D8 | **An attested set's plan drops an unreadable light as a project's plan does** (`unsupported_ids`, the `in_scope` filter and the "cannot be read" warning, `plan.rs:~2334–2365, ~2585`): out of the groups, the included counts and the ≥ 3 rule. Cost: an attested-set plan opens every included light's header (zip-resident lights skipped, as the project loop does); a calibrated set's plan is untouched. | The new path mirrors the proven one; a count that says "left out" while the groups still hold the frame would be false (review I2). |
| D4 | **The coverage map** gets a thumbnail beside its group's master in the Results panel (`get_master_light_preview` already serves kind `coverage`, `api/stacking.rs:~1156`), with its path line; the drizzle coverage map gets a path line. Owner 2026-11-05. | The real overlap of the stack, after the run; no backend change. |
| D9 | **A master's name states its exposure range** when its frames have more than one exposure: `<min>-<max>s` (e.g. `…_L_mono_120-180s_57x.fits`), over the included frames. One exposure, and every `seconds`-mode class, keep today's `<t>s`, so frame-set names in the default mode do not change. Owner 2026-11-05. | Today the name carries the shortest member's seconds (`groups.rs`, `ExposureClass.exposure_s`), so a `stops` class of 120 s and 180 s frames reads `120s`. |
| D10 | **A project's masters folder may not lie inside the Collaboration folder**: the plan's `folders` blocker covers `<output>/<project title>/` against the Collaboration root (the working root check stays). Inside an ordinary scan root it keeps the frame-set rule (the Save-time overlap warning only). Owner 2026-11-05. | 6b-3 left this to 6c (`docs/stacking/README.md`, "Output on the grid (D11)"); files there would be strays in the project's folder. |
| D11 | **The grid card draws the grid** before any run: the grid rectangle, the target circle and the outline of every accepted frame with a manifest WCS, coloured by publisher (`useMemberColor`, the project page's member colours); the dialog redraws it as the values change; frames without a manifest WCS are counted, not drawn. Owner 2026-11-05. The sky-chart overlay (the grid and the outlines over the survey background) is out of this plan: a follow-up, possibly with the portal's grid editor (wave 7). | The grid is chosen from the frames' spread; numbers alone do not show it. |
| D5 | **An attested frame set** (R3, §13) gets the same two board changes as a project: Masters "not needed — the lights are calibrated externally", row 1 labelled Prepare with the counts. | §13's stacking half shipped in 6b-2 with no UI; the switch is the same one. |
| D6 | **Grid centre** shown and edited in degrees (RA 4 decimals, Dec 4 decimals), scale in ″/px (2 decimals), size in px and as a field in degrees. | No frontend sexagesimal formatter exists; the hub stores degrees. |
| D7 | **Plan refresh for a project:** besides today's triggers, `collab-grid-changed` and `collab-frames-changed` for this `projectId`, coalesced to one refetch per 1 s. | Frames land and remaps arrive while the tab is open (§6.1, §9.2). |

## Global Constraints

- Backend only through the `api` object (`src/api/`); no `@tauri-apps/*` import elsewhere.
- Every `api.listen` uses the StrictMode-safe pattern from `CLAUDE.md` (cancelled flag, unlisten on resolve-after-cancel).
- Design tokens only (`bg-surface`, `text-content-muted`, `text-warning`, …); primitives from `src/components/ui/`; the one new modal uses `DialogShell`.
- `notify()` from `useNotifications()` on discrete outcomes only, never on progress; every caught error is `console.error`ed with a `[Component]` prefix before it is shown.
- No new Tauri command and no new route: Tasks 1, 1b and 1c change the return type of existing commands or the run's own output; both hosts share `StackingPlan`. The command count stays 328.
- Serde `camelCase`; after a Rust model change run `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` and commit the regenerated `src/types/*.ts`.
- Frame sets must behave exactly as before: every existing stacking test stays green with only prop-shape edits, and the selected-run localStorage key of a frame set keeps its current spelling (`<prefix><setId>`), so stored selections survive.
- UI strings in English; platform-neutral wording.
- The dev Mac has 16 GB: one implementer at a time, one compile at a time.

## Review Focus

1. **Progress of project A shown on project B or on a frame set.** Two subjects with runs in flight, the user switches pages: each tab shows only its own subject's progress and outcome. Pinned in Task 2 (hook) and Task 4 (tab).
2. **A pending config write lands on the wrong subject.** The user edits a project's config and navigates to a frame set within the debounce window: the write goes to `set_project_stacking_config` for the old project with its own uuids, never to the set. Pinned in Task 4.
3. **A run row whose frame left the plan.** A frame excluded by the coordinator, remapped or no longer on disk after the run: its row still renders from the run's own data (file name from the summary, or `frame <handle>`), never crashes and never takes another row's uuid. Pinned in Task 6.
4. **Grid save refused or overtaken.** The hub answers 403 (capability lost) or 400 (bounds), or another member saves first: the dialog stays open with the error text, `notify` fires once, and the next `collab-grid-changed` refetches the plan. Pinned in Task 7.
5. **Listener leaks on the project page.** StrictMode double-mounts the Stacking tab: exactly one `collab-grid-changed` and one `collab-frames-changed` listener remain, and unmount releases both. Pinned in Task 9.

---

## Summary

| Task | Deliverable | Agent | Depends on |
| ---- | ---- | ---- | ---- |
| 1 | `StackingPlan.prepare` counts (core) | rust-engineer (sonnet) | — |
| 1b | The plan's grid carries the frames' outlines and the target; the projection golden file (core) | rust-engineer (sonnet) | 1 |
| 1c | Masters: the exposure range in the name; the Collaboration-folder blocker (core) | rust-engineer (sonnet) | 1b |
| 2 | Subject model; `useStackingRuns` keyed by subject | frontend-dev | — |
| 3 | `useStackingSubjectApi` adapter | frontend-dev | 2 |
| 4 | `StackingTab` + `ResultsPanel` take a subject (frame sets unchanged) | frontend-dev | 3 |
| 5 | Board and inspector for a project / attested set: Masters, Prepare, hidden navigations | frontend-dev | 1, 4 |
| 6 | Frames table by uuid: Publisher, exclusion, reference pin | frontend-dev | 4 |
| 7 | Grid card, grid dialog and the grid schematic | frontend-dev | 1b, 4 |
| 8 | Output (coverage, crop), Groups (class token, grouping control), Results coverage thumbnail | frontend-dev | 4 |
| 9 | Project page Stacking tab, refresh triggers, harness scenario | frontend-dev | 5–8 |
| 10 | Docs, full gates, real-data acceptance on current `main` | controller + sonnet | 1–9, 1b, 1c |

---

### Task 1: The plan says what Prepare will do

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/plan.rs` (`StackingPlan` ~:290; `project_inputs` ~:2300–2360; `frame_set_inputs` ~:2157 for an attested set; `SubjectInputs`)
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register `PlanPrepare`)
- Regenerate: `src/types/stacking.ts`
- Test: `crates/athenaeum-core/src/stacking/plan.rs` test module (beside the existing project-plan tests, e.g. the `unsupported` test ~:8556)

**Interfaces:**
- Produces: `pub struct PlanPrepare { pub in_place: u32, pub debayered: u32, pub converted: u32, pub unsupported: u32, pub at_run: u32 }` (`camelCase`, ts-rs), and `StackingPlan.prepare: Option<PlanPrepare>` — `Some` for a project and for an attested set, `None` for every other frame set.
- Counting rule, per included frame (not manually excluded): `PrepareAction::InPlace` → `inPlace`; `Write { read: InPlaceFits, debayer: Some(_) }` → `debayered`; `Write { read: ScaledFits | Xisf, .. }` → `converted` (with or without a debayer); `Unsupported` → `unsupported`; a zip-resident file the plan does not probe → `atRun`.

- [ ] **Step 1: Write the failing tests**
  - `project_plan_counts_prepare_actions`: a project fixture with one mono FITS (BITPIX −32), one OSC mosaic FITS (−32) with `debayer_osc = true`, one integer FITS (BITPIX 16), one XISF and one tile-compressed FITS → `prepare == Some(PlanPrepare { in_place: 1, debayered: 1, converted: 2, unsupported: 1, at_run: 0 })`.
  - `project_plan_prepare_with_debayer_off`: the same fixture with `debayer_osc = false` → `in_place: 2, debayered: 0, converted: 2, unsupported: 1`.
  - `project_plan_prepare_skips_excluded_frames`: exclude the integer FITS by uuid → `converted: 1`.
  - `attested_set_plan_counts_prepare_actions`: an attested catalog set of two float lights (the fixture at `test_fixtures.rs:~359`) and one `write_fits_i16` light → `Some` with `in_place: 2, converted: 1`.
  - `attested_set_plan_drops_an_unreadable_light` (D8): an attested set of four lights, one an empty-primary FITS (`write_empty_primary_fits`) → `unsupported: 1`, `(frameCount, includedCount) = (4, 3)` as the project arm pins (3, 2), and the plan carries the project path's "cannot be read" warning.
  - `calibrated_set_plan_has_no_prepare`: an ordinary set → `prepare == None`, and its groups and warnings equal the plan before the change (no probe ran).
  - Fixtures: `ProjectFile::{Mono32, Rgb32, Mosaic32, Mono16, Xisf32}` (`test_fixtures.rs:~1000`); the unsupported proxy is `write_empty_primary_fits` (`prepare.rs` tests).
- [ ] **Step 2: Run them, expect FAIL** — `cargo test -p athenaeum-core --lib stacking::plan::tests::project_plan_counts_prepare_actions` → compile error (`prepare` missing).
- [ ] **Step 3: Implement.** Count inside the existing probe loop of `project_inputs`. For an attested set, run the same probe loop over the included lights before anything is counted, with the project loop's drop, warning and zip skip (`at_run`) — the proven project path, mirrored (D8). Carry the counts through `SubjectInputs` into `StackingPlan`. Log nothing new at info; the per-frame debug and warn lines are the project loop's.
- [ ] **Step 4: Regenerate TS and run** — `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`, then `cargo test -p athenaeum-core --lib stacking::plan` → PASS; `cargo test -p athenaeum-core --test ts_contract` → PASS; `npx tsc --noEmit` → PASS.
- [ ] **Step 5: Commit** — `feat(stacking): the plan counts what Prepare will do — in place, debayered, converted, unsupported (wave 6c)`.

---

### Task 1b: The plan's grid carries the frames' outlines and the target

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/plan.rs` (`PlanGrid`; the grid block of `project_inputs` ~:2367–2400, where `all_inputs` and `GridTarget` are already built)
- Modify: `crates/athenaeum-core/src/collab/grid_geometry.rs` (a corner helper beside `FrameFootprint`, `grid_wcs`)
- Modify: `crates/athenaeum-core/src/ts_export.rs`; regenerate `src/types/stacking.ts`
- Create: `src/components/stacking/gridProjection.golden.json` (written and checked by a Rust test)

**Interfaces:**
- Produces: `PlanGrid` gains `target: PlanGridTarget { raDeg, decDeg, radiusDeg }`, `footprints: Vec<PlanGridFootprint>` and `framesNotDrawn: u32` (an accepted frame with no manifest WCS, no pixel size or unparseable meta — every frame `grid_input_of` gives no footprint). `PlanGridFootprint { frameUuid, publisherAccountId, publisher, filterCanonical, outline: Vec<[f64; 2]> }` — RA/Dec in degrees of `grid_geometry::boundary_points(w, h)` (`~:74`: the pixel EDGES −0.5 … w−0.5, eight points per edge, so SIP curvature shows) through the frame's `FrameFootprint` (SIP included), the same points `fraction_inside` uses, so an outline agrees with `framesInside`. The set is `source.frames`: every published, accepted frame of the manifest, on this device or not (§6.2, `project_source.rs:~312–330`), so every member sees the same picture. `SourceFrame` gains `publisher_account_id` (the row has it, `db/collab_frames.rs:~170`).
- Produces: the golden file `{ grids: [{ grid: CanonicalGrid, points: [{ raDeg, decDeg, x, y }] }] }` — four sky points in front of the tangent plane (the grid centre, one 0.5° east, one 0.5° north, one near a corner) projected to 0-based grid pixels with `grid_wcs`, for a grid at rotation 0 and one at 30°. Task 7's TS projection asserts the same numbers to 1e-6 px.

- [ ] **Step 1: Write the failing tests:** `project_plan_grid_carries_footprints`: a project of three frames, two with a manifest WCS and one without → two footprints with the right uuids, account ids and publishers, `framesNotDrawn: 1`, the target equal to the project row's; an outline point inverts back to its pixel edge point within 1e-6 px through the same WCS. `grid_projection_golden_file_is_current`: computes the golden points; with `GOLDEN_WRITE=1` it writes `src/components/stacking/gridProjection.golden.json`, otherwise it asserts the file equals the computation (the `ts_contract` pattern).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** `boundary_points` becomes `pub(crate)`. No new log lines; a frame with no footprint is counted in `framesNotDrawn`.
- [ ] **Step 4: Regenerate and run** — `GOLDEN_WRITE=1 cargo test -p athenaeum-core --lib grid_projection_golden`, `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`, then both tests without the env vars and `cargo test -p athenaeum-core --lib stacking::plan` → PASS; `npx tsc --noEmit`.
- [ ] **Step 5: Commit** — `feat(stacking): the project plan's grid carries every accepted frame's outline and the target (wave 6c)`.

---

### Task 1c: Masters — the exposure range in the name, and never inside the Collaboration folder

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/master_cards.rs` (`master_file_name` ~:456 and its tests ~:1298–1360)
- Modify: `crates/athenaeum-core/src/stacking/run.rs` (the call ~:12439) and `crates/athenaeum-core/examples/integrate_probe.rs` (the call ~:1059; `cargo test -p athenaeum-core` builds examples)
- Modify: `crates/athenaeum-core/src/stacking/plan.rs` (the `folders` gate ~:2938–2990), `crates/athenaeum-core/src/stacking/paths.rs` (`canonical_or_nearest`, `subject_output_dir` ~:1161), `crates/athenaeum-core/src/api/scan_roots.rs` (`resolve_special_root_dir` ~:764)

**Interfaces:**
- Produces: `master_file_name(set_name, filter, color_mode, binning, exposure: ExposureLabel, n)` where `ExposureLabel = Single(Option<f64>) | Range { min: f64, max: f64 }` (D9). `Range` prints `<min>-<max>s`, both numbers through `calibration_library::paths::fmt_num` (`pub(crate)`, `~:49`) as today's single value. At the call the included frames are `output.included` (indices into `members`, `integrate.rs:~647`) × `GroupMember.exposure_s` (`run.rs:~10546`), not `group.frames`; the mode is `rc.config.grouping.exposure_mode`. `Range` when the mode is `stops`, `equivalent` or `none` and the included frames carry more than one distinct exposure; otherwise `Single(group.exposure_s)` — in `seconds` mode the cluster's own label (`groups.rs:~553–557`), so today's names hold. No reader parses the exposure token (review 2026-11-05: `resolve_collision` only appends `_2`, the masters library names through `master_relative_path`, the scanner keys on cards).
- Produces: `pub(crate) fn collaboration_dir(conn) -> Result<Option<PathBuf>>` in `api/scan_roots.rs`, a wrapper over the private `resolve_special_root_dir(conn, "collaboration")`; and `pub fn project_output_overlap(conn, output_dir: &Path) -> Result<Option<String>, ApiError>` in `stacking/paths.rs` — `canonical_or_nearest(output_dir)` against the canonicalized, `normalize_path`'d Collaboration root by `starts_with` (equality included). `Some(sentence)` becomes a `folders` blocker: `The project's masters folder <path> is inside the Collaboration folder; choose an output folder outside it`. No Collaboration folder set → `None`. The plan passes `subject_output_dir(&validated.output, subject, &set_name)` (`set_name` is in scope, `plan.rs:~2730`; the run uses the same call, `run.rs:~988`). Never `check_scan_root_overlap` or `project_root_overlap`: they flag every scan root, and D10 keeps the frame-set warning for ordinary roots (`validate_transfer_dir`, `paths.rs:~513`).

- [ ] **Step 1: Write the failing tests:**
  - `master_name_carries_an_exposure_range`: `Range { min: 120.0, max: 180.0 }` → `LDN_1272_L_mono_120-180s_57x.fits`; `Range { min: 0.5, max: 2.0 }` prints both numbers as `fmt_num` does; every existing `master_name_*` test unchanged with `Single(…)`.
  - A run test (`stacking_project_e2e.rs` or the run's group test): a `stops` group of 120 s and 180 s frames writes a master named with `120-180s`; a `seconds` group of 120 s frames keeps `120s`.
  - `project_plan_blocks_a_masters_folder_inside_the_collaboration_folder`: output folder `<collab root>/masters` → a `folders` blocker with the sentence above; the output folder equal to the Collaboration root → the same blocker; an output folder outside → none; no Collaboration folder → none; an output folder inside an ordinary scan root → no blocker (the frame-set rule).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p athenaeum-core --lib stacking::master_cards stacking::plan` and the e2e test → PASS.
- [ ] **Step 5: Commit** — `feat(stacking): a master's name states its exposure range; a project's masters never land in the Collaboration folder (wave 6c)`.

---

### Task 2: The subject model, and run tracking by subject

**Files:**
- Create: `src/components/stacking/subject.ts`, `src/components/stacking/subject.test.ts`
- Modify: `src/hooks/useStackingRuns.ts`, `src/hooks/useStackingRuns.test.tsx`
- Modify: `src/components/stacking/stackingPrefs.ts` (selected-run key)

**Interfaces:**
- Produces (`subject.ts`):
  - `type StackSubject = { kind: 'frameSet'; framesSetId: number; lightFrames: LightFrameRef[] } | { kind: 'project'; projectId: string; title: string }`
  - `type SubjectKey = string`; `subjectKey(s: StackSubject): SubjectKey` → `set:<id>` / `project:<projectId>`
  - `eventSubjectKey(e: { setId: number | null; projectId: string | null }): SubjectKey | null` — `null` when neither or both are set (logged `console.error`)
  - `subjectLink(s: StackSubject): string` → `/objects/<id>?tab=stacking` / `/projects/<projectId>?tab=stacking`
- Produces (`useStackingRuns`): `progress: Map<SubjectKey, RunProgress>`, `lastOutcome: Map<SubjectKey, RunOutcome>`, `startRun(subject: StackSubject, config?: StackingConfig, rerunFrom?: Stage): Promise<number>`, `cancelRun(runId)`, `isRunning(key: SubjectKey): boolean`. `RunProgress` and `RunOutcome` replace `setId: number` with `subjectKey: SubjectKey`.
- Produces (`stackingPrefs.ts`): `readSelectedRunId(key: SubjectKey)` / `writeSelectedRunId(key, runId)`. Storage spelling: a frame set keeps `<prefix><setId>` (the number alone); a project uses `<prefix>project:<projectId>`.

- [ ] **Step 1: Write the failing tests**
  - `subject.test.ts`: `subjectKey` both forms; `eventSubjectKey({setId: 7, projectId: null}) === 'set:7'`; `({setId: null, projectId: 'p1'}) === 'project:p1'`; both null → `null`; `subjectLink` both forms.
  - `useStackingRuns.test.tsx`: the existing case "ignores a project run until wave 6c" (`:~24`) flips — it now asserts the project run is tracked under `project:p1`. Added: a progress event with `projectId: 'p1'` lands under `project:p1` and not under any set key; a completion for `p1` clears only `p1`'s progress and notifies with `link: '/projects/p1?tab=stacking'`; `startRun({kind:'project', projectId:'p1', title:'T'}, cfg, 'register')` invokes `start_project_stacking` with `{ projectId: 'p1', config: cfg, rerunFrom: 'register' }`; `startRun` for a set still invokes `start_stacking` with `{ setId, config, rerunFrom }`. Existing set tests keep their assertions, only re-keyed to `set:<id>`.
  - `stackingPrefs`: `writeSelectedRunId('set:12', 5)` writes the key `<prefix>12`; a value stored under `<prefix>12` before the change reads back through `readSelectedRunId('set:12')`.
- [ ] **Step 2: Run, expect FAIL** — `npx vitest run src/components/stacking/subject.test.ts src/hooks/useStackingRuns.test.tsx`.
- [ ] **Step 3: Implement** the module and the re-keying. The notification title stays `Stacking finished — N master(s)` (the event has no title); for a project the `detail` is the master file names as today.
- [ ] **Step 4: Run, expect PASS**; `npx tsc --noEmit` lists only the call sites Task 4 fixes (StackingTab, ResultsPanel). To keep `main` compiling between commits, adapt those two call sites mechanically in this task (`progress.get(subjectKey({kind:'frameSet', …}))`) — no behaviour change.
- [ ] **Step 5: Commit** — `feat(stacking-ui): runs, outcomes and the selected run are keyed by subject (wave 6c)`.

---

### Task 3: The subject adapter

**Files:**
- Create: `src/components/stacking/useStackingSubjectApi.ts`, `src/components/stacking/useStackingSubjectApi.test.ts`

**Interfaces:**
- Consumes: `StackSubject` (Task 2).
- Produces: `useStackingSubjectApi(subject: StackSubject): StackingSubjectApi`, stable per subject key, with
  - `getPlan(config?: StackingConfig): Promise<StackingPlan>` → `get_stacking_plan {setId, config}` / `get_project_stacking_plan {projectId, config}`
  - `getConfig(): Promise<SubjectConfig>` / `setConfig(c: SubjectConfig): Promise<SubjectConfig>` → `get/set_stacking_config` / `get/set_project_stacking_config`
  - `getRuns(limit: number): Promise<StackingRunSummary[]>`, `getWorkUsage(): Promise<WorkUsage>`, `cleanup(what: CleanupWhat): Promise<number>` → the set or project variants
- `SubjectConfig = { config: StackingConfig; excluded: string[]; referenceFrameUuid: string | null; isDefault: boolean; updatedAt: string | null }`. Exclusion keys are strings: `String(frameId)` for a set (converted back to numbers for `excludedFrameIds`), the frame uuid for a project. `referenceFrameUuid` is always `null` for a set and is never sent for one.
- Read the current `StackingSetConfig` fields before writing the set mapping; carry every field the set command takes today through unchanged.

- [ ] **Step 1: Write the failing tests** (mock `api.invoke`): for each subject and each of the six calls, the exact command name and argument object; `getConfig` of a set maps `excludedFrameIds: [3, 9]` to `excluded: ['3', '9']` and `setConfig` maps back to numbers; a project round-trips `excludedFrameUuids` and `referenceFrameUuid`; a rejected invoke is `console.error`ed with `[useStackingSubjectApi]` and rethrown.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** — `feat(stacking-ui): one adapter maps every stacking command to the subject (wave 6c)`.

---

### Task 4: `StackingTab` and `ResultsPanel` take a subject

Behaviour-preserving for frame sets. Characterize first, then refactor.

**Files:**
- Create: `src/components/stacking/StackingTab.test.tsx`
- Modify: `src/components/stacking/StackingTab.tsx` (props ~:33; `framesSetId`/`framesSetIdRef`/`draftForSetRef`/`pendingWriteRef` ~:127–422; `startRun` calls ~:619, :635)
- Modify: `src/components/stacking/ResultsPanel.tsx` (`setId` prop ~:380, the three invokes ~:416–488)
- Modify: `src/pages/FrameSetDetail.tsx` (~:1091: pass `subject={{ kind: 'frameSet', framesSetId, lightFrames }}`, keep `key={id}`)

**Interfaces:**
- Consumes: Tasks 2 and 3.
- Produces: `StackingTabProps = { subject: StackSubject }`; `ResultsPanelProps.subject: StackSubject` in place of `setId`. Inside the tab, `excluded` is `string[]` (Task 3's keys); the Frames table still receives the set's numeric ids until Task 6.

- [ ] **Step 1: Characterization test on the current code** (commit it green before refactoring): render the current `StackingTab` for a set with a mocked api that also answers `get_stacking_presets` and `list_stacking_presets` (the mount effect calls them between the config and the plan, `StackingTab.tsx:~258–290`); assert the mount calls `get_stacking_config {setId}`, then `get_stacking_plan {setId, config: <the stored config>}` (not `undefined`); toggling a frame's exclusion writes `set_stacking_config` with `excludedFrameIds` after the debounce; Run calls `start_stacking {setId, config}`; the Results panel calls `get_stacking_runs {setId, limit: 20}`.
- [ ] **Step 2: Add the failing subject tests:**
  - `a project subject reads and writes through the project commands` (plan, config, runs, work usage).
  - `progress of another subject is not shown`: context holds progress for `project:p2`; a tab for `project:p1` shows no running state (Review Focus 1).
  - `a pending write goes to the subject it was made for`: edit the config of `project:p1`, rerender with a set subject before the debounce fires → exactly one `set_project_stacking_config` call with `projectId: 'p1'`, no `set_stacking_config` call (Review Focus 2).
- [ ] **Step 3: Run, expect the new tests FAIL and the characterization test PASS.**
- [ ] **Step 4: Refactor** to the subject: replace every set-id ref and guard with the subject key (same race guards: the plan sequence number plus a subject-key ref), route every command through Task 3, pass the subject to `startRun` and `ResultsPanel`. The zip scope and `ZipExtractionLine` (`StackingTab.tsx:~342, ~934`) stay for `frameSet` only; a project's zip-resident own frames are counted by Task 1's `atRun`, and a project extraction estimate is out of scope. Update the characterization test's render call only (prop shape); its assertions do not change.
- [ ] **Step 5: Run** `npx vitest run src/components/stacking src/hooks` and `npx tsc --noEmit` → PASS.
- [ ] **Step 6: Commit** — `refactor(stacking-ui): StackingTab and the Results panel take a stacking subject; frame sets unchanged (wave 6c)`.

---

### Task 5: The board and inspector for a project and an attested set

**Files:**
- Modify: `src/components/stacking/stageSummary.ts` (+ `stageSummary.test.ts`): row label and summary by subject
- Modify: `src/components/stacking/PipelineBoard.tsx`, `src/components/stacking/StageInspector.tsx`
- Create: `src/components/stacking/panels/PreparePanel.tsx` (+ `PreparePanel.test.tsx`)
- Modify: `src/components/stacking/panels/MastersPanel.tsx`, `DebayerPanel.tsx`, `ReferencePanel.tsx`
- Modify: `src/components/stacking/StackingTab.tsx` (the Coverage navigation ~:646–663, :1025)

**Interfaces:**
- Consumes: `plan.prepare` (Task 1), `plan.projectId`, `plan.readiness.attested`, `StackSubject`.
- Produces: `boardMode(plan: StackingPlan | null): 'calibrate' | 'prepare'` — `'prepare'` when `plan.projectId != null` or `plan.readiness.attested`; `stageLabel(stage: BoardStage, mode)`; `PreparePanel({ config, plan, disabled, onPatchCalibration })`.

Copy (exact):
- Masters row and panel, project: `Not needed — project frames are calibrated by their publishers`.
- Masters row and panel, attested set: `Not needed — the lights are calibrated externally`.
- Row 1 label in `prepare` mode: `Prepare`. Its board summary: `<inPlace> in place · <debayered> debayered · <converted> converted` (zero terms omitted; `+ <atRun> checked at run` when `atRun > 0`).
- Prepare panel: the counts as a `KV` list (In place, Debayered, Converted to float FITS, Unsupported — left out of the run, Checked when the run starts), the `debayerOsc` checkbox with its current label from `CalibratePanel`, and the line `Other calibration settings do not apply: these frames are already calibrated.` No other calibration control.
- Debayer panel in `prepare` mode: `Debayering is on — set on the Prepare panel.` / `… off — … Turn it on from the Prepare panel.`
- Reference panel, project, Manual mode: replace `Choose in Analysis` with `Pin a frame in the Frames table below. Only frames with a WCS can be the reference.`
- The `→ Coverage` link is not rendered for a project.

- [ ] **Step 1: Write the failing tests:** `stageSummary.test.ts` — `boardMode` for a set, an attested set, a project; the Masters row state for a project and for an attested set is `'off'` in every run state, never `blocked`; the Prepare summary string for `{inPlace: 3, debayered: 0, converted: 2, unsupported: 1, atRun: 0}` is `3 in place · 2 converted`. `PreparePanel.test.tsx` — the five counts, the debayer checkbox calls `onPatchCalibration({ debayerOsc: false })`, no other calibration control is rendered. A `StackingTab` case: a project plan renders no `→ Coverage` link and no `Choose in Analysis` button.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** Use the frontend-design skill for the Prepare panel layout against the Calibrate panel it replaces.
- [ ] **Step 4: Run, expect PASS** (`npx vitest run src/components/stacking`), `npx tsc --noEmit`.
- [ ] **Step 5: Commit** — `feat(stacking-ui): a project and an attested set show Prepare and need no masters (wave 6c)`.

---

### Task 6: The Frames table by frame uuid

**Files:**
- Modify: `src/components/stacking/FramesTable.tsx` (+ its test if present, else create `FramesTable.test.tsx`)
- Modify: `src/components/stacking/StackingTab.tsx` (wiring the project rows, exclusion and pin)

**Interfaces:**
- Consumes: `plan.projectFrames`, `StackingRunDetail.frames` (keyed by handle as `frameId`), `RunSummary` frames, `SubjectConfig.excluded` / `referenceFrameUuid` (Task 3).
- Produces: `FrameRow` gains `key: string` (the exclusion key), `publisher: string | null`, `pinnable: boolean`, `pinned: boolean`, `unavailable: string | null` (= `reason`; `onDisk` is false exactly when `reason` is set, `plan.rs:~2604`, so there is no separate fallback). `buildProjectFrameRows(projectFrames, detail, summary, excluded: Set<string>, referenceFrameUuid: string | null): FrameRow[]`. `FramesTableProps` gains `subjectKind`, `onToggleExclude(key: string)`, `onPin(key: string)`.
- Join rule: a run row (`StackingRunFrameRow`) already carries `frameUuid` (`db/stacking.rs:~291–295`): key it by that uuid, and by `h:<frameId>` only when `frameUuid` is null. A `SummaryFrame` carries the handle as `frameId` and no uuid: map it through the run rows' `frameId → frameUuid`, then through `projectFrames[].handle`. A run row whose uuid has no plan row keeps its uuid key, the summary's filename (or `frame <handle>`), no toggle and no pin (Review Focus 3).
- Pin (D2): `onPin(uuid)` sets `config.reference.mode = 'manual'` and `referenceFrameUuid = uuid`; pinning the pinned row clears `referenceFrameUuid` (`null` clears the stored value: the upsert writes `reference_frame_uuid = excluded.reference_frame_uuid`, `db/stacking.rs:~1227`) and returns to `auto`. Every usable row (`onDisk ∧ reason == null`) is `pinnable`; one without `hasWcs` carries the tooltip `No WCS in the manifest — plate-solved locally first`; an unusable row carries its `reason`.

- [ ] **Step 1: Write the failing tests:** `buildProjectFrameRows` — rows from a plan of 4 frames (one with `reason: 'not on this device'`, one with `reason: 'unsupported file: …'`, one without WCS); the Publisher column; exclusion state from the uuid set; a run row joined by its `frameUuid`; a run row with a uuid but no plan row keeps that uuid as its key and the summary's filename, with no toggle and no pin; one with `frameUuid: null` and no summary renders as `frame 41` (Review Focus 3); the no-WCS row is pinnable with its tooltip, the two unusable rows are not; clicking a pin calls `onPin` with the uuid, clicking the pinned row clears it; set rows are unchanged (existing assertions, keys are `String(frameId)`).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.** The set path keeps its columns; the project path adds Publisher and Pin and drops nothing the set shows that the summary also has.
- [ ] **Step 4: Run, expect PASS**; `npx tsc --noEmit`.
- [ ] **Step 5: Commit** — `feat(stacking-ui): the project Frames table by frame uuid — publisher, exclusion and the reference pin (wave 6c)`.

---

### Task 7: The grid card and the grid dialog

**Files:**
- Create: `src/components/stacking/GridCard.tsx`, `src/components/stacking/GridDialog.tsx`, `src/components/stacking/GridCard.test.tsx`
- Create: `src/components/stacking/GridSchematic.tsx`, `src/components/stacking/gridProjection.ts`, `src/components/stacking/gridProjection.test.ts` (reads Task 1b's `gridProjection.golden.json`)
- Modify: `src/components/stacking/StackingTab.tsx` (render the card above the board for a project)
- Modify: `src/components/stacking/stageSummary.ts` (+ test): `BLOCKER_STAGE` (`~:437–445`) gains `grid: 'register'` — registration lands on the grid (§5.5), so the Register row reads `blocked` while the grid is unconfirmed (review I3)

**Interfaces:**
- Consumes: `plan.grid: PlanGrid`, the `grid` blocker (`plan.rs:~2405–2415`: message `Confirm the project's grid in the Stacking tab` for an editor, `Waiting for the coordinator to confirm the grid` otherwise), `CanonicalGrid` (`src/types/models.ts`), `api.invoke('set_collab_project_grid', { projectId, grid })` → the stored `CanonicalGrid`.
- Produces: `skyToGridPixel(raDeg, decDeg, grid: CanonicalGrid): { x: number; y: number } | null` — the gnomonic projection of `grid_wcs` (0-based pixels), equal to the golden file to 1e-6 px. It returns `null` when cos c ≤ 0 (the point is 90° or more from the grid centre): a TS-side rule, because the Rust tangent projection returns finite, mirrored values there (`solvemyastro` `wcs.rs:~36–45`), and a mirrored outline is what the rule prevents. `GridSchematic({ grid: CanonicalGrid, target: PlanGridTarget, footprints: PlanGridFootprint[], framesNotDrawn: number })` (D11). `GridCard({ projectId, grid: PlanGrid, onSaved: () => void })`; `GridDialog({ initial: CanonicalGrid, warnRewrite: boolean, schematic: { target, footprints, framesNotDrawn }, onSave(g): Promise<void>, onClose })`; `validateGrid(g: CanonicalGrid): string | null` mirroring `collab::grid::CanonicalGrid::validate` and the hub: `scaleArcsec ∈ (0, 100]`, `widthPx, heightPx` integers in `[64, 65536]`, `crval1 ∈ [0, 360)`, `crval2 ∈ [-90, 90]`, `rotationDeg` finite.

States and copy (exact):
- `unset`/`proposed`, `canEdit`: title `Canonical grid — not confirmed`; the proposal's values; `<framesInside> accepted frames lie at least half inside` (`1 accepted frame lies at least half inside` for one); buttons `Use proposal` (saves `proposal` as is) and `Edit…` (dialog prefilled with the proposal). No proposal: `No proposal yet.` and only `Edit…` prefilled from the project target, scale 1.00, 4096 × 4096, rotation 0.
- same, not `canEdit`: `Waiting for the coordinator to confirm the grid` (the blocker's own string, no period) plus the proposal's values (read-only).
- `confirmed`: title `Canonical grid`; the values; `Change…` for `canEdit` only. The dialog opened from `Change…` shows, above Save: `Changing the grid makes every processor's next run rewrite its registered frames. Star matching is kept.`
- Values line: `<scale>″/px · <w> × <h> px (<w·scale/3600>° × <h·scale/3600>°) · centre RA <crval1>° Dec <crval2>° · rotation <rotationDeg>°`.
- Schematic (D11), in the card for the grid shown (the confirmed one, else the proposal) and in the dialog for the draft as it is edited: an SVG drawn grid-up, as the master will look — SVG x = grid x, SVG y = `heightPx − y` (the grid is `BOTTOM-UP`; east is left because `grid_wcs` has CDELT1 = −s). North is up only at rotation 0, so a small `N` arrow in the top-right corner points along the projection of a point 0.1° north of the centre. The grid rectangle spans the pixel edges −0.5 … W−0.5 × −0.5 … H−0.5. The view box is the union of the grid rectangle and every outline, plus 5 %, so frames reaching past the grid show. The grid rectangle in `stroke-accent`; the target circle dashed, muted; each outline a thin polygon through its `outline` points in its publisher's colour (`useMemberColor(publisherAccountId)`; without one, `className="text-content-muted" stroke="currentColor"`), no fill. An outline is drawn only when every point projects. Height at most 220 px, width the card's. Under it, when `framesNotDrawn > 0`: `<n> frames are not drawn: no WCS in the manifest` (`1 frame is not drawn: no WCS in the manifest` for one). With no grid shown (no proposal) the schematic draws the target circle and the outlines only, on a tangent plane at the target.
- Save in flight disables the buttons; a failure keeps the dialog open with the backend's message under the fields, `console.error('[GridDialog] set_collab_project_grid failed:', err)` and one `notify({ title: 'Grid not saved', detail, tone: 'warning', hasErrors: true })` (Review Focus 4). Success closes the dialog and calls `onSaved` (the tab refetches the plan; the feed's `collab-grid-changed` refetches again, harmlessly).

- [ ] **Step 1: Write the failing tests:** `gridProjection.test.ts` — every golden point to 1e-6 px for both grids; at rotation 0 a point 0.5° north of the centre is above it on the schematic and one 0.5° east is left of it; at rotation 30° the `N` arrow is tilted from vertical by 30°; a point 100° away is `null`. `GridSchematic` — three footprints render three outlines in their publishers' colours (and the fallback class without a provider), a footprint with a point behind the plane is not drawn, the `framesNotDrawn` line, and in the dialog changing `scaleArcsec` changes the outlines' coordinates. `rowState('register', …)` is `blocked` for a plan carrying the `grid` blocker and `ready` once it is gone; each state's copy and buttons; `Use proposal` invokes `set_collab_project_grid` with `{ projectId, grid: proposal }` exactly; `validateGrid` boundaries (`scaleArcsec` 0 refused, 100 allowed, `widthPx` 63 refused, 65536 allowed, `crval1` 360 refused, `crval2` −90 allowed, `rotationDeg` NaN refused) and Save disabled while invalid; a rejected save keeps the dialog open, shows the message and notifies once; the rewrite warning shows only from `Change…`.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement** with `Card`, `KV`, `Button`, `DialogShell` and the stacking `NumericField`. Use the frontend-design skill for the card against the board's width.
- [ ] **Step 4: Run, expect PASS**; `npx tsc --noEmit`.
- [ ] **Step 5: Commit** — `feat(stacking-ui): the canonical grid card — propose, confirm and change the project's grid (wave 6c)`.

---

### Task 8: Output, Groups and the coverage map line

**Files:**
- Modify: `src/components/stacking/panels/OutputPanel.tsx` (+ test), `src/components/stacking/GroupsTable.tsx` (+ create `GroupsTable.test.tsx`), `src/components/stacking/ResultsPanel.tsx` (the drizzle weight-map line ~:355 is the model)
- Modify: `src/components/stacking/stageSummary.ts` (Output summary mentions `coverage map` and `crop ≥ K` when on)

**Interfaces:**
- Consumes: `OutputConfig.{coverageMap, cropToCoverage}`, `GroupingConfig.{exposureMode, stopsWidth}`, `PlanGroup.exposureClass`, `SummaryGroup.{coveragePath, drizzleCoveragePath}`.
- Produces: `GroupsTable({ groups, grouping, disabled, onPatchGrouping })`.

Copy and rules (exact):
- Output panel, both subjects: checkbox `Write a coverage map (frames per pixel) beside each master`; integer field `Crop to coverage of at least` … `frames (0 = no crop)`, min 0, step 1. Both are config fields; the default comes from the stored config (project on / 0, set off / 0).
- Groups table: the `Exp` column shows `g.exposureClass` (for a frame set it equals today's token; assert it on the existing fixtures before switching). Header control `Exposure grouping`: a `Seg` or select with `Seconds` (`seconds`), `Stops` (`stops`), `Equivalent t/N²` (`equivalent`), `None` (`none`), and the mode's threshold beside it:
  - `seconds`: `Tolerance` in s (`exposureToleranceSec`), min 0.01 (`MIN_EXPOSURE_TOLERANCE_SEC`), step 0.5. For a frame set only, the hint `The tolerance is part of the calibration hash: changing it re-calibrates this set's lights on the next run.` (`calibration_subtree`, `stacking/config.rs:~997`; a project's Prepare hash has no grouping in it).
  - `stops` and `equivalent`: `Width` in stops (`stopsWidth`), min 0.1 (`MIN_STOPS_WIDTH`), step 0.1.
  - `equivalent` adds the hint `Classes by t / N², N = FOCALLEN / APTDIA from each frame's header. Frames without both are grouped by seconds; the plan names how many.`
  - `none`: no threshold.
  A change patches `grouping` and the plan refetches as for any config edit. (D1: rendered for both subjects.)
- Results panel (D4): a group whose `SummaryGroup` (through `sg`, as `weightMapPath` comes) has `coveragePath` shows a coverage thumbnail beside its master thumbnail — `useMasterPreview(runId, groupKey, 'coverage', 512, enabled)`, chained to keep one render at a time: `enabled` after the drizzle thumbnail resolved, or after the master's when there is no drizzle (the `masterResolved` sequencing, `ResultsPanel.tsx:~254–259`) — labelled `Coverage map — frames per pixel`, with its `RevealOrPath` line under it. `drizzleCoveragePath` gets a `Drizzle coverage map` path line only.

- [ ] **Step 1: Write the failing tests:** Output panel patches `{ coverageMap: true }` and `{ cropToCoverage: 3 }`, refuses −1; Groups table renders `st120` and `eq0.24` tokens, the grouping control patches `{ exposureMode: 'stops' }`, `{ stopsWidth: 0.5 }` and `{ exposureToleranceSec: 5 }`; the field shown per mode is Tolerance / Width / Width / none, a tolerance of 0 and a width of 0.05 are refused, the re-calibration hint shows for a set in `seconds` mode and never for a project; the Results panel requests the `coverage` preview only after the master preview resolved (and after the drizzle preview when the group has one), shows the thumbnail and both path lines when the paths are set, and requests nothing and shows nothing when they are null; the Output board summary.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run, expect PASS**; `npx tsc --noEmit`.
- [ ] **Step 5: Commit** — `feat(stacking-ui): coverage map and crop with a coverage thumbnail, the exposure-class token and the exposure grouping control (wave 6c)`.

---

### Task 9: The project page's Stacking tab

**Files:**
- Modify: `src/pages/ProjectDetail.tsx` (`Tab` ~:43, `TAB_LABEL`, `TAB_ALIASES` ~:59, `tabs` ~:493, the tab body switch), `src/pages/ProjectDetail.test.tsx`
- Modify: `src/components/stacking/StackingTab.tsx` (project refresh triggers, empty state)
- Modify: `scripts/ui-harness/fixtures.mjs`, `scripts/ui-harness/README.md`

**Interfaces:**
- Consumes: everything above; `ProjectCard.{projectId, title}`.
- Produces: tab id `stacking`, label `Stacking`, visible to every member, placed right after `Library` — or after `My frames` when Library is hidden (Library shows only for `canReceive`, `ProjectDetail.tsx:~483, ~496`); `?tab=stacking` deep link (the notification link of Task 2).

Rules and copy:
- Body: `<StackingTab key={projectId} subject={{ kind: 'project', projectId, title }} />`.
- Empty state instead of the board when the plan's blockers include `frames`. `EmptyState` (`src/components/ui/EmptyState.tsx`) takes children only, so its children are two lines: `Nothing to stack on this device` (`text-content`) and a muted detail — `No frame of this project is usable on this device.` when no `projectFrames` row has `onDisk` (the four `reason`s of `db/collab_frames.rs:~277–280`), else `Every usable frame is excluded.`. The Frames table stays visible under it when rows exist, so exclusions can be undone.
- Refresh (D7): in `StackingTab`, for a project only, listen to `collab-grid-changed` and `collab-frames-changed`; ignore other `projectId`s; coalesce into one plan refetch per 1 s (a trailing timer, cleared on unmount). StrictMode-safe pattern (Review Focus 5).
- Harness scenario `stacking`: handlers for `get_project_stacking_plan` (grid `proposed`, `canEdit: true`, a target and ten footprints from three publishers with two frames reaching past the proposal, `framesNotDrawn: 2`; three groups `mono__L__st120`, `mono__Ha__st300`, `osc__None__st180`; twelve `projectFrames` with one `onDisk: false`, one `reason: 'unsupported file: tile-compressed FITS'`, two without WCS; `prepare {inPlace: 9, debayered: 0, converted: 1, unsupported: 1, atRun: 0}`), `get_project_stacking_config`, `get_project_stacking_runs` (`[]`), `get_project_stacking_work_usage`, `get_stacking_presets`, `list_stacking_presets`, `get_stacking_paths`. Shape every value from `src/types/stacking.ts`.

- [ ] **Step 1: Write the failing tests:** `ProjectDetail.test.tsx` — the `Stacking` tab follows `Library` for a `send_receive` member without any capability, and follows `My frames` for a send-only contributor (beside the existing contributor case, `~:1143`); `?tab=stacking` opens it. `StackingTab.test.tsx` — the empty-state copy for both cases; a `collab-frames-changed` for another project does not refetch; three events for this project inside 1 s cause one `get_project_stacking_plan` call; after a StrictMode double mount and an unmount, both unlisten functions were called and no listener is left (Review Focus 5).
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3: Implement**, then the harness scenario.
- [ ] **Step 4: Run** `npx vitest run` (whole suite) and `npx tsc --noEmit` → PASS. Run `HARNESS_SCENARIO=stacking npm run ui:harness`, open `/projects/p-m31?tab=stacking` in the built-in browser at 1440 × 900: the grid card with its schematic (north up, the two overhanging outlines visible), the board with Prepare, the Groups table with the three tokens, the Frames table with the publisher and pin columns render with no console error. Save a screenshot in the job folder.
- [ ] **Step 5: Commit** — `feat(collab-ui): the project page gets a Stacking tab (wave 6c)`.

---

### Task 10: Docs, gates and the first project run on current `main`

The project path's numerics moved after its last acceptance (6b-3 run B, 2026-10-04 at 9f9624ce): the LN model fixes, Plans B–D, R-SC-1, the robust quad seed and the star_measure port all landed since, and their lab runs used `runB` at master level but not the grid-level checks. This task re-runs those checks once, through the commands the new tab calls.

**Files:**
- Modify: `docs/stacking/README.md` (a "The project's Stacking tab, wave 6c" section: subject, grid card and schematic, Prepare panel, pin, grouping control, coverage thumbnail, refresh triggers; the master name's exposure range (D9) and the Collaboration-folder blocker (D10), replacing the "a blocker can be added in 6c" line), `docs/transfers/README.md` (the grid card as the coordinator's confirm path), `docs/frontend/conventions.md` only if it lists the stacking components
- Modify: `docs/superpowers/open-items.md`: remove "There is no project UI before 6c" from the 6b-3 entry; add a "Collab wave 6c" entry with the owner smokes below; record the acceptance result
- `CLAUDE.md`: no change (no command, rule or path changed)

Steps:
- [ ] **Step 1: Gates** — `cargo test -p athenaeum-core` (all targets; Tasks 1, 1b and 1c touched core), `cargo test -p athenaeum-web`, `cargo test -p athenaeum-core --test ts_contract`, `npx tsc --noEmit`, `npx vitest run`. Any collab-live timing failure is judged by `reference_collab_live_timing_test_flake` (fails only on a hang), never re-run until green without a reason.
- [ ] **Step 2: Real-data run** (sonnet, a judgment run), under `~/.athenaeum-acc/wave6c-runb/` with a `REGISTRY.tsv` row (owner `wave6c`, end date the merge day, budget 80 GB). The athenaeum-lab harness (`~/Documents/Projects/athenaeum-lab/harness/acceptance/`, its `README.md` items 7–8 are the procedure) on a copy of the `.dev` catalog, project `ldn1272-test-project`:
  1. `prepare-catalog.sh` makes the copy; set its `account.hub_url` to `http://127.0.0.1:9` before the server first starts (signed out, so the feed cannot replace the local grid; never `account_sign_out` on a copy); start `server.sh` once so the schema migrates, stop it.
  2. `run-a-grid.py <copy.db> ldn1272-test-project --grid-json <J> --apply` with run B's grid: 4480 × 5760 px at 1.89″/px, rotation 0, centred on the project target (the 2026-10-04 record).
  3. `server.sh`, then `api.sh get_project_stacking_plan` — record `prepare`, `blockers`, `warnings`; then `tier1/project-run.sh` with run B's configuration (LN on, `cropToCoverage` 1, drizzle off), then `grid-report.py`.
  
  Bars, each printed beside the 2026-10-04 value (open-items, "Collab wave 6b-3"): the run completes; the WCS probe ≤ 0.5 px on both groups (was 0.157 OSC, 0.465 mono) — a value above it sends the run to the outlier-frame look (stretched JPG of the worst frame, its refusal breakdown) before any verdict; no LN overlap under 25 % (minimum was 0.973 mono, 0.987 OSC); the crop rectangle from the event equals CRPIX (was 4402 × 5760 at x0 73); statistics rectangles cover ≥ ⌈n/2⌉ frames; no memory refusal; plate-solve-seed fallbacks ≤ 5 and no new warning kind without a named cause (RQ-1 / RQ-10 change which frames fall back, and later cycles added warning kinds). Masters' pixel values are not a bar: Plans B–D and R-SC-1 changed them on purpose. Report ≤ 80 lines; delete the work folder after the report is committed.
- [ ] **Step 3: Launch the app** — `npm run tauri dev` on the dev DB, open the LDN1272 test project's Stacking tab: the plan loads with no error in the log (`athenaeum-logs` MCP), the grid card shows. Screen capture is blocked from the terminal: ask the owner to look.
- [ ] **Step 4: Final whole-branch review** (fable): hunt plan-verbatim seams (copy strings matched in tests across tasks, the exclusion-key conversion, the handle→uuid join, the listener patterns).
- [ ] **Step 5: Commit the docs** — `docs(stacking): the project's Stacking tab (wave 6c) — reference, open items, the run B re-check`.

**Owner smokes for open-items (on real data, not done by an agent):**
- The grid schematic on a real project: every member's frames where the sky chart puts them, north up and east left; edit the scale in the dialog and watch the outlines move.
- After a run, the coverage thumbnail: deep centre, thin edges, the overhanging frames visible; a `stops` master named `…_<min>-<max>s_…`.
- Confirm a grid as the coordinator with `Use proposal`; a second member's tab leaves "Waiting for the coordinator" without reopening the page.
- Stack the project from the tab; the progress shows on the project page only; the notification opens `?tab=stacking`.
- The first grid master against a sky chart: the grid is sky-standard (det CD < 0) and every solve in the owner's catalog has det CD > 0, so the master is the mirror image of a raw frame (6b-3 item).
- Pin a reference, run, and see the run's reference is that frame; exclude a frame and see it out of the run.
- Change the grid after a run: the warning shows; the next run rewrites the registered frames and does not re-match stars (the log shows the cached star map).
- An attested frame set: Masters "not needed", Prepare counts, a run without masters.
- Real-window (Tauri/WebKit) look at the tab against the harness.

---

## Owner questions

- **Q1 (D1) — answered 2026-11-05:** the control shows for both subjects with each mode's threshold; a frame set keeps `seconds` by default, a project `stops` 1.0 (W1, unchanged unless the owner says otherwise at plan review).

## Review record (fable, 2026-11-05)

Verdict "ready after fixes", no critical finding; every "What exists" claim checked on `main`. Folded: run rows keyed by their own `frameUuid` (I1); an attested set's plan drops an unreadable light as a project's does, D8 (I2); the `grid` blocker gets the Register row and one string with the card (I3); any usable row can be pinned, D2 (I4); Task 10's harness steps — signed-out copy, `run-a-grid.py --grid-json`, `tier1/project-run.sh` (I5); bars that survive the numeric cycles since 2026-10-04, old values printed beside (I6); the flipped "wave 6c" hook test, the presets calls in the characterization test, the zip line for sets only, `unavailable = reason`, `EmptyState` children, the tab placement without Library, an integer attested light, clearing a pin (minors).

**Owner additions after the review (2026-11-05):** D4 becomes a coverage thumbnail, D9 (the exposure range in a master's name), D10 (masters never in the Collaboration folder), D11 (the grid schematic); Tasks 1b and 1c added. The sky-chart overlay of the grid stays a follow-up. Fable re-reviewed the additions ("ready after fixes", no critical): folded — the Collaboration root read through `conn`, never the every-scan-root helpers; the probe example's call and the named name-readers; the TS-side `cos c ≤ 0` rule; grid-up with a north arrow; pixel edges via `boundary_points`; `framesNotDrawn`; colour by account id; one preview render at a time.

## Execution notes (lean-execution)

- One implementer at a time; a fresh agent per task and per fix round; reports ≤ 80 lines in `.superpowers/sdd/2026-11-05-collab-v3-wave6c/`.
- Tasks 1, 1b, 1c: rust-engineer (sonnet), reviewer opus. Tasks 2–9: frontend-dev, reviewer opus sized to the diff; Tasks 5 and 7 invoke the frontend-design skill. Task 10 step 2: sonnet. Final review: fable.
- Estimated spend: Tasks 1, 1b and 1c small each (plan fields, a name rule, a blocker); Tasks 2–4 medium (the refactor is the riskiest UI change); Tasks 5–9 small to medium each; Task 10's real-data run about 15 min of wall time on the dev Mac (6b-3 run B took 810 s).
- Merge to local `main` after the final review; push only on the owner's word.

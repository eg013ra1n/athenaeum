# Stacking tab: the Frames block — design

Date: 2026-11-06. Status: approved by the owner 2026-11-06.

## 1. Intent

The Stacking tab's Frames block (`src/components/stacking/FramesTable.tsx`, the same component for a frame set and for a
collab project) is one flat, sortable table. It has no filtering and no grouping, it does not say which frame is the
reference, and it hides why a frame was left out. On real runs that hides the most important fact a run can report. Run
#8 of the dev catalog (frame set 182, 164 frames, four filters) stacked 73 frames, all of them L. None of the 88 R, G
and B frames was stacked: R lost 30 of 30 and G 30 of 30 to registration failures, B 25 of 28, mostly with `quad seed
failed (0 quad matches)`, and the remaining 3 B frames fell below the weight floor. In the table that is 88 warning and
error chips mixed in among the L rows, and the only way to see that the whole colour data is lost is to read the
Results panel below.

The block's job is to let an astrophotographer decide, per frame and per group, what goes into the next stack, and to
show what the selected run did with each frame. This design gives it the same working style as the Analysis tab
(toolbar of labelled icon buttons over a selectable table), groups the frames, shows the reference, says why a frame
is out, and adds Blink over the selection, showing either the original frames or the run's aligned frames.

Owner decisions (2026-11-06): variant A of the mockup (a grouped table with a reference strip, not a side tree and not
per-group charts); add Blink for the selected frames; Blink shows both the aligned and the original frames, with a
switch; buttons follow the Analysis tab's toolbar rules so the app keeps one style.

## 2. Goals

- The reference is visible: the frame the selected run registered onto (and, in native mode, each group's), whether the
  run picked it automatically or it was pinned, whether the two-pass pick moved it, and what the next run will use.
- Frames are grouped: by stacking group (default), by night (frame set), by publisher (project), by status, or not at
  all. A group's header says how the group did, so a group that lost every frame reads as lost at a glance.
- Frames can be filtered: by file name, by status, and to outliers only.
- Every left-out frame says why, in short form beside its status.
- Several frames can be selected (click, Shift+click range) and excluded from or included in the next run at once.
- Blink opens over the selection (or over every visible row when nothing is selected), with a switch between the
  run's aligned frames and the originals.
- The block's buttons look and behave like the Analysis tab's toolbar.
- The block says which run its metrics come from.

## 3. Non-goals

- No Black Hole and no other file deletion from the Stacking tab, in the table or in Blink.
- No threshold bar that auto-rejects frames (the Analysis tab's `RejectionThresholdBar`): outliers are highlighted and
  filterable, excluding them stays the user's click.
- No left tree of groups (variant B) and no per-group charts (variant C).
- No change to what a run does, to how exclusions and the reference are stored, or to the reference rules
  (`docs/stacking/README.md`, §4.4 rulings).
- No restyling of the Analysis tab.

## 4. Anti-goals

- A second visual language for toolbar buttons in the app.
- A frame's group computed in the frontend: the grouping rules (exposure seconds / stops / equivalent / none,
  tolerances, colour mode) live in core and are not reimplemented.
- An action that touches a row the user cannot see.
- A frame shown as plain "Included" when the run used it as the reference, or a status chip that hides the reason.

## 5. What exists today (facts the design builds on)

- `FramesTable.tsx` builds rows from the plan (`plan.projectFrames` for a project, `subject.lightFrames` from
  `FrameSetDetail.tsx`'s `detail.nights` for a set) joined to the selected run (`StackingRunDetail`: `groups`,
  `frames`, `summary`). The run is selected in `ResultsPanel` below the table and lifted through `StackingTab`.
- Columns: Filename, (Publisher), Group, Weight, FWHM, Ecc, Seeds, Reg RMS, Scale, Inliers, Model, Status, an Include
  checkbox (a write of the exclusion list, Ruling 7), and for a project a Reference pin (D2).
- The run records the reference per row: `stacking_run_frames.reg_status = 'reference'` (5 such rows in the dev DB).
  `RunSummary.reference` carries `frameId`, `filename`, `mode`, `weight`, `switchedFrom` (two-pass). Each
  `SummaryGroup` carries `referenceFrameId` (its registration reference, its own in native mode) and
  `normalizationReferenceFrameId` (LN reference). `StackingPlan.reference` carries the next run's `mode`, `frameId`,
  `filename`, `onDisk`.
- A frame set's manual reference is stored by `set_frame_set_reference` / `get_frame_set_reference` and is chosen today
  on the Analysis tab (a `Star` per row in `LightsAnalysisTable.tsx`, filled for the stored frame whatever
  `reference.mode` says; Analysis has no way to clear it, and no clear command exists). A project's is
  `referenceFrameUuid`, pinned in the Frames table: `StackingTab.handlePinReference` sets `reference.mode = 'manual'`
  with the uuid, and clicking the pinned frame again clears the uuid and returns to `auto`. The uuid survives only an
  Auto switch made in the Reference panel, and the pin shows only in Manual mode.
- Exclusion reasons in real runs: `reg_status = 'failed'` with `registration failed: …` (86 rows), `reg_status =
  'skipped'` with `weight 0.019 below 0.05 of the group maximum` (from `stacking/weights.rs`). Other reasons exist in
  code; the design does not parse reason strings (§6.4).
- The plan knows each group only by its counts (`PlanGroup.frameCount`), not its members, so before the first run the
  Group column shows "—" (logged in `docs/superpowers/open-items.md`).
- Registered frames are written by stage 5 to `<working_dir>/<subject>/registered/<group_key>/r_<stem>[_d].fits`
  (`registered_dir` in `stacking/paths.rs`, `registered_file_name` and `calibrated_file_stem` in `stacking/run.rs`,
  the writer in `stacking/register/writer.rs`): resampled into the reference geometry, NaN outside coverage,
  calibrated, debayered for colour. Each has a `stacking_artifacts` row with its absolute `path`. They stay on disk
  under the default `output.cleanup = KeepAll`; `DeleteRegistered` and `DeleteIntermediates` remove them, and so does
  deleting the working folder.
- Registered frames are per subject, group and frame, not per run: `stacking_artifacts` has no run id, and every later
  run of the group overwrites the file and its row. The subject folder name is built from the set's current name, so
  rebuilding a path for an old run after a rename gives the wrong folder; the artifact row's `path` does not.
- The preview stretch already leaves NaN out of its statistics (`rustafits/src/processing/stretch.rs`), so a registered
  frame renders with no rustafits change.
- Blink: `BlinkViewer` takes `BlinkFrame[]`. A frame set opens it with `get_files_with_frames_by_ids`
  (`FrameSetDetail.handleBlink`). A project uses `ProjectBlink` with `get_collab_blink_frames` and the viewer's project
  mode (`actions`, `contextLabel`, badges in place of the Black Hole). Desktop images load with
  `read_fits_image_rustafits { path }`, web with `get_frame_preview { frameId }`, a project's calibrated/replica entries
  with `get_collab_frame_image`.
- The Analysis toolbar (`LightsAnalysisView.tsx`, right panel) is built inline, with its own exported `ToolButton`
  (`LightsAnalysisView.tsx`, "a 40×28 button with its label under it"). It does not use `src/components/Toolbar.tsx`,
  whose `ToolbarButton` has no label under the button and no split buttons (only `Objects.tsx` uses it). Its rules:
  `h-7` buttons, icon-only `w-10`, `rounded-lg`, a 10 px muted label under each button, `|` dividers between groups,
  split buttons for Selection (count that clears + invert) and for View (two-segment toggle, active segment
  `bg-accent/20 text-accent border-accent/40`), a Units toggle (`px` / `"`), Blink as a cyan `Play` button labelled
  "Blink" with a selection and "Blink all" without one.
- `LightsAnalysisTable.tsx` selection: a selection checkbox in the first column, a row click toggles one row,
  Shift+click selects or clears the range from the anchor in displayed order.
- The Analysis auto FWHM threshold: median + 3 × max(MAD, 0.1 × median) over the displayed frames
  (`LightsAnalysisView.tsx`, `autoFwhmPx`).

## 6. Design

The interactive mockup shown to the owner on 2026-11-06 is the reference for the layout. It used a status chip row and
per-row Include checkboxes; this document replaces both with the Analysis toolbar pattern (§6.2, §6.3).

### 6.1 Layout, top to bottom

1. **Header line**: the collapse chevron, `Frames (N)`, and the run context: `metrics from run #8 · 2026-11-05
   22:14:03` (the selected run's id and start time, format `YYYY-MM-DD HH:MM:SS`), or `no run yet`.
2. **Reference strip** (§6.5).
3. **Toolbar** (§6.2), then one row of status filter chips under it.
4. **The table** (§6.3), with a sticky header; the table scrolls inside the block once it is taller than a fixed
   maximum (runs have 100–368 frames in the dev catalog).

### 6.2 Toolbar (Analysis rules)

Built from the Analysis toolbar's own pieces: its `ToolButton` and the same inline split-button, toggle and divider
markup, moved to a shared module so both tabs render the same components (the Analysis tab switches to the shared
module with no visible change). Same sizes, radii, label style and active-toggle style. Groups, left to right:

| Group | Control | Behaviour |
| ---- | ---- | ---- |
| Selection | split button: count (clears) + invert | as in Analysis; invert works over the visible rows |
| Actions | **Blink** (cyan `Play`) | label `Blink` with a selection, `Blink all` without; opens §6.6 |
| | **Exclude** (default style, an exclusion icon) | removes the eligible selected rows from the next run |
| | **Include** (default style, an inclusion icon) | puts the eligible selected rows back |
| `\|` | | |
| View | **Group by** split toggle | Group · Night · Status · None for a set; Group · Publisher · Status · None for a project |
| | **Units** `px` / `"` | as in Analysis; shown when at least one group has a pixel scale |
| | **Outliers** toggle | shows outlier rows only (§6.4) |
| | **Columns** toggle | shows the registration columns (§6.3) |
| `\|` | | |
| Search | a small `h-7` text field labelled `Search` | matches the file name (and the publisher for a project) |

The status filter chips sit in one row under the toolbar, pill-shaped like the app's `FilterChip` (a variant without
the filter dot), each with its count: `All`, `Included`, `Reg failed`, `Excluded by the run`, `Out of next run`, and
for a project `Not stackable here` (§6.4 says what each counts). One chip is active at a time.

Exclude and Include act on the eligible part of the selection (owner rule: actions on the eligible subset). Exclude's
eligible rows are selected editable rows not already in the exclusion list; Include's are selected rows in it. A button
is enabled when its eligible set is not empty, and its label reads `Exclude (3 of 5)` when the eligible set is smaller
than the selection. Both write through the existing gated persist path (`setUserExcluded`). Both, and the reference
star, are disabled while a run is active, as today. Blink stays available during a run.

### 6.3 The table

- **First column: a selection checkbox**, with the row click and Shift+click range behaviour of `LightsAnalysisTable`.
  The per-row Include checkbox goes: inclusion is changed with Exclude / Include and shown by the status chip and the
  row tint. When a filter or the grouping hides selected rows, they leave the selection, so an action never touches a
  hidden row.
- **Default columns**: File, Publisher (project), Weight, FWHM, Ecc, Seeds, Status, Reference.
  **Registration columns** (the Columns toggle): Reg RMS, Inliers, Model (with its `WCS` chip), Scale (LN). The Group
  column goes: the group is the header row in Group mode; with any other grouping the group is shown in the row's
  context (a muted suffix or tooltip, builder's choice).
- **Reference column**: the Analysis `Star`, sortable. A filled accent star marks the next run's manual reference
  (§6.5). Clicking a row's star pins it, clicking the pinned star returns to Auto.
- **Row tint**: rows the selected run failed to register take the Analysis rejected-row tint (`bg-error/10`); rows in
  the exclusion list take a muted warning tint.
- **Outlier cells**: an FWHM or Ecc value beyond its group's outlier limit (§6.4) gets a warning tint, whether or not
  the Outliers filter is on.
- Sorting by a column header works within each group, as today across the flat table.

### 6.4 Status and outliers

A row carries up to two signals: **the selected run's outcome** (one chip) and **the next run's exclusion** (a second
chip). They never compete for one chip. The outcome comes from structured fields only, never from parsing
`exclusionReason`:

| Outcome chip | When | Text beside the chips |
| ---- | ---- | ---- |
| `Reference` (accent) | the run row's `regStatus = 'reference'` | `run #N · auto` or `· manual` |
| `Included` (success) | included by the run | none |
| `Reg failed` (error) | `regStatus = 'failed'` | `exclusionReason` without a leading `registration failed: ` |
| `Excluded` (warning) | not included for any other reason | `exclusionReason` |
| `Not stackable here` (warning, project) | no run row and the plan's `reason` | the reason |

With no run row (a frame added after the selected run, or no run yet) and no plan reason there is no outcome chip.
The second chip, `Out of next run` (warning), is shown on every row in the exclusion list, whatever its outcome,
including the run's reference. The reason text is muted, truncated, with the full text in the tooltip.

Filter chips and Status grouping: `Reg failed`, `Excluded by the run` and `Not stackable here` count rows by outcome;
`Included` counts `Reference` and `Included` outcomes; `Out of next run` counts every row in the exclusion list, so the
user can always find the rows Include acts on. A row can match more than one chip; Status grouping puts it under its
outcome, and rows with no outcome under `Not run yet`.

Outlier limit: median + 3 × max(MAD, 0.1 × median) over the included rows, for FWHM and for Ecc (the Analysis auto
FWHM rule). It is computed per group for a frame set and per group and publisher for a project, since each member
shoots with their own rig. A row is an outlier when either value is above its limit. Known limit: a frame-set group
that mixes focal lengths (camera and geometry are not group keys) flags the longer focal length's frames; no per-frame
pixel scale exists to correct for it, and this design does not add one.

### 6.5 The reference

- **Strip** above the toolbar, two lines:
  - `Registration reference, run #8: <file> · <group> · weight 1.000 · auto` (or `manual`), plus `· two-pass moved it
    from <file>` when `switchedFrom` is set. In native mode it reads `Registration reference, run #8: per group` and each
    group header names its own. While a run is in flight and the summary is not written, the strip uses the row with
    `regStatus = 'reference'`.
  - `Next run: auto, picked at run time`, or `Next run: <file> · manual` from `plan.reference`, with an error mark when
    `onDisk` is false.
- **Rows**: the run's reference row carries the `Reference` status chip; the next run's manual reference carries the
  filled star. The star means "you chose this" (as on the Analysis tab), the chip means "the run used this".
- **Group headers** (Group mode) add `Reference: <file>` in native mode and `LN ref: <file>` when the summary group has
  `normalizationReferenceFrameId`.
- **Pinning in a frame set's table** (new), mirroring the project pin: the star calls `set_frame_set_reference` and
  sets `reference.mode = 'manual'` in the draft config; clicking the pinned star clears the stored reference through a
  new `clear_frame_set_reference` command (§7.3) and sets the mode back to `auto`. The star shows only in Manual mode,
  like the project pin. The Analysis tab reads the same stored reference, so after a pin or an unpin both tabs agree.
  One state still differs, as it does for a project today: Manual switched to Auto in the Reference panel keeps the
  stored frame, so Analysis shows its star and Stacking does not; the strip then reads `Next run: auto`. A failed write
  is logged and notified. A project's star keeps today's `handlePinReference` behaviour.

### 6.6 Grouping

- **Group** (default): a row's group key is the selected run's group for that row when it has a run row, otherwise
  the plan's group for the frame (§7.1), otherwise `Not grouped yet`. Groups are ordered as the plan orders them.
- **Night** (frame set): the night from `detail.nights`. `LightFrameRef` gains the night date and the frame's exposure,
  read from the same tree (frontend only).
- **Publisher** (project): `PlanProjectFrame.publisher`.
- **Status**: the chips of §6.4, in that table's order.
- **None**: today's flat table.

A **header row** is clickable to collapse or expand its group. It shows the filter dot (`FilterDot`), the group's name
(filter, exposure, colour mode, from the plan or run group), then `N / M included · <integration> · median FWHM <x>`,
where integration is the sum of the included frames' exposures. A group with no included frame reads in error text,
with its outcome counts and its most frequent reason: `0 / 28 included — 25 registration failed (quad seed failed (0
quad matches)) · 3 excluded`. The most frequent reason is the most frequent full `exclusionReason` among the group's
failed rows with the leading `registration failed: ` removed, nothing else parsed. Night, Publisher and Status headers
show `M frames · N included`.

### 6.7 Blink over the selection

- **Which frames**: the selected rows, or every visible row when nothing is selected, in the table's current order
  (group order, then the sort).
- **Opening**: a frame set's entries come from `get_files_with_frames_by_ids`, a project's from
  `get_collab_blink_frames` by frame uuid, as the two callers do today. Rows whose file is not on this device are left
  out, with a notification saying how many.
- **Aligned / Original switch** in Blink's toolbar, styled like Blink's own toggles. Default: Aligned for every entry
  that has an aligned frame of the selected run; an entry without one shows its original, and the context strip says
  why: `registration failed in run #8`, `not in the working folder any more`, or `a later run replaced it`. With no
  run selected the switch is hidden and Blink shows originals.
- **Which aligned frame belongs to the selected run**: registered files are overwritten by every later run of the
  group (§5), so an aligned frame is offered only when the selected run is the subject's latest run that holds the
  frame's group, and the frame's `stacking_artifacts` registered row and its file exist. For an older run every entry
  shows its original with `a later run replaced it`.
- **Aligned images**: a new core API takes a run id and the run's frame id, finds the frame's registered artifact row
  (subject, group key, frame), applies the rule above, and renders the file at the row's absolute `path` to JPEG with
  the same pipeline and auto-stretch as the original preview (NaN outside coverage renders black). It never rebuilds
  the path from the subject's current name. It serves preview and full resolution, like `read_fits_image_rustafits`.
  At open Blink needs, per entry, whether an aligned frame is available and, if not, which of the three reasons applies:
  one batch call that answers this, and one image call. Each is a Tauri command and its Axum mirror, thin wrappers over
  `athenaeum-core/src/api/`.
- **Overlays**: the star overlay and the flat contour are drawn on the original's pixel grid, so they are off in
  Aligned mode.
- **Actions in Blink** (project mode of the viewer, for both subjects): `Exclude from next run (N)`, `Include (N)`, and
  `Use as reference` (eligible when exactly one pinnable entry is selected). No Black Hole, no withhold, no exclude from
  the project. Each entry's badge is its §6.4 status chip label; the context label is the group and the reason. The
  actions write through the same paths as the table, so the table shows the change when Blink closes.

## 7. Backend changes

### 7.1 The plan names each group's members

The plan returns, for each group, the frames it holds: frame ids for a set, project-frame handles for a project (the
ids the run's frame rows carry). They are filled by the same grouping code that builds the groups, from the existing plan
command, so the plan and the run never disagree about membership. The new field goes through `ts_export.rs`; no new
command. This closes the open item about "—" in the Group column before any run.

### 7.2 Aligned frame images for Blink

§6.7: two new commands in the stacking domain (the availability batch and the image), each in
`crates/athenaeum-tauri/src/commands/` and `crates/athenaeum-web/src/routes/` in the same change, with
`#[tracing::instrument(skip_all, err)]` (`level = "debug"` for the per-frame image call), the logic in core.

### 7.3 Clearing a frame set's reference

`clear_frame_set_reference { framesSetId }` deletes the stored row, beside the existing set/get pair in the registration
domain, in both hosts. With §7.2 the command count in `docs/architecture/commands.md` goes up by three.

## 8. Constraints

- The repository's critical rules (`CLAUDE.md`): two backends in sync, `api` object only, camelCase serde with
  `models.ts` / `stacking.ts` matching, errors always logged, design tokens, `anyhow` in core.
- The Analysis Blink button uses `bg-cyan-600`, a raw colour. The Stacking Blink button must look the same. In the
  shared toolbar module (§6.2) the colour becomes a design token with the same value in both themes, so neither tab
  changes visibly.
- Settings: none added.
- Real data first: verify the table on frame set 182 (run #8, the lost RGB groups) and project `af136ec9` (run #9,
  two groups) of the dev catalog, launching the app (`npm run tauri dev`). Only project run #9 still has registered
  frames on disk (`/Volumes/bigbase3/athenaeum-acc-geom-2026-10-25/stacking-work/_projects/ldn1272-test-project/registered/`):
  run #8's working folder is gone and runs 1–4 have no `registered/` folder. Aligned Blink for a frame set needs a
  fresh run, with its working folder under `~/.athenaeum-acc/` and a `REGISTRY.tsv` row (the disk rule).

## 9. Phases

1. **Frontend, plus the one small command it needs**: header line, reference strip and stars (the frame-set star with
   `clear_frame_set_reference`, §7.3), the shared toolbar module, selection, grouping (run group / night / publisher /
   status / none), filters, statuses, outliers, columns toggle, Blink of originals with the stacking actions.
   Shippable on its own.
2. **Backend + the rest**: the plan's group members (§7.1, grouping before the first run), the aligned-frame commands
   and the Aligned / Original switch (§7.2, §6.7).

## 10. Docs

`docs/stacking/README.md` (the Frames table section and the project Frames table section), `docs/architecture/commands.md`
(count), and `docs/superpowers/open-items.md` (the "—" item resolved, new smokes for the reference strip, the frame-set
star and aligned Blink).

## 11. Left to the builder

Payload shapes of the new plan field and the new commands; component split (`FramesTable` will need a toolbar, a header
row and row-builder modules); the table's maximum height; icons for Exclude, Include, Outliers and Columns; whether a
group's collapse state and the toolbar toggles persist (per subject, in local storage, like the block's own collapse);
how a row shows its group outside Group mode; where the shared toolbar module lives and the token's name (§6.2, §8);
how "the subject's latest run that holds the group" is queried (§6.7).

## 12. Changes during implementation

Controller rulings made while the plan was built that changed this document's letter:

- **Night labels (R6, R7).** A night is labelled as the Analysis tab labels it (`formatNightLabel`, the TS twin of
  `format_night_display`: UTC dates, `September 13–14, 2025`), not by a bare date as §6.6 says. A plain `YYYY-MM-DD`
  start keeps the Rust fallback spelling (`October 08, 2025`), so both tabs agree.
- **Row tint (R9).** A row in the exclusion list takes `bg-warning/10` (§6.3's "muted warning tint"), not
  `bg-warning-muted/40`, which rendered four times as strong as the `bg-error/10` tint beside it.
- **Blink's notice (R11).** `N of M frames are not on this device.` counts every row Blink does not open, including a row
  that cannot be asked for (a project run row with no uuid), not only the requested frames that did not come back.
- **Registered statuses (R12).** An aligned frame needs the run row's `reg_status` to be `aligned`, `aligned_flipped` or
  `reference` (`stacking::plan::registered_status`, shared with the plan's freshness check). §6.7's rule (the artifact row
  and its file exist) alone would show another run's file for a frame this run skipped, since artifacts have no run id.
- **A fourth reason, `notRegistered` (§6.7).** The batch call answers `available`, `registrationFailed`, `notRegistered`,
  `missing` or `replaced`. `notRegistered` (the run holds no row for the frame, or cut it before Register) reads `not
  registered in run #N` in the context strip, beside §6.7's three reasons.
- **Include needs `editable` (§6.2).** Include's eligible rows are selected rows in the exclusion list that are also
  `editable`, like Exclude's: a project run row whose frame left the plan is never eligible.

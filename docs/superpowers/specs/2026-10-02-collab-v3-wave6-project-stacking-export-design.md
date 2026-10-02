# Collaboration v3, wave 6 — stacking a project, and the project export

Status: design approved section by section in conversation (2026-10-02);
this document awaits the owner's review of the written text.

Parent spec: `2026-09-23-collab-v3-per-frame-model-design.md` (cited as
"v3"); wave numbering per its amendment A4 (6 = stacking a project). This
spec is amendment **A8** of v3 (§3 below lists what it changes there).
Related: `2026-09-08-stacking-pipeline-design.md` (the stacking spec),
`docs/stacking/README.md` (every stacking ruling — all of them stay in
force), `2026-09-28-collab-v3-contributor-path-design.md` (R3 attestation,
filter mapping), `docs/export/README.md`.

## 1. Why

A processor holds a project's frames on disk (its own and replicas) but
cannot stack them: the stacking pipeline accepts only a frame set, every
table and key in it is a catalog `frames_set_id` / `frame_id`, and project
frames never become catalog rows (v3 R1, A1). The project's WBPP export
exists but drops the filter level of the R1 hierarchy and groups by the raw
`FILTER`, so a filter mapping or a coordinator remap never reaches WBPP. The
coordinator has no tool to remap at all, although the hub accepts it.

Wave 6 delivers:

1. Stacking a project on the processor's machine: already-calibrated
   project frames, every frame registered onto the project's canonical grid,
   per-pixel coverage, groups by colour × canonical filter × exposure class.
2. The project WBPP export in the R1 hierarchy, with the canonical filter
   written into the `FILTER` card of every exported file that needs it, and
   a mixed symlink/copy mode.
3. The coordinator's filter remap in the app.
4. The second half of R3: an attested ("calibrated externally") frame set is
   never calibrated again by personal stacking or by the calibrated-lights
   export.

## 2. Owner rulings (2026-10-02)

Numbered so plans and reviews can cite them.

- **W1 — Zero point is not in this wave.** Photometric ZP (v3 R9 top rung)
  becomes sub-wave 6.5. Wave 6 ships the exposure modes `seconds`, `stops`,
  `equivalent` (ladder `t/N²` → seconds only) and `none` (§8). ZP later plugs
  in as the top rung of `equivalent` without changing any group key format.
- **W2 — The coordinator remaps filters in the app** — Moderation tab,
  section *Filters* — through the hub's existing per-frame
  `PATCH /projects/{id}/frames/{uuid}` (`filterCanonical`, capability
  `thresholds.edit`). No hub change in wave 6.
- **W3 — Stacking solves frames without WCS itself.** A project frame whose
  manifest carries no `meta.wcs` is plate-solved locally by the stacking run
  and the solve is cached. A frame that fails to solve still registers by
  quads when its scale is inside the quad window, else it is excluded with a
  reason. v3 R10's "plate-solve is a publish precondition" stays in the gate
  backlog, not in this wave.
- **W4 — Every exported project file carries the canonical filter.** The
  `FILTER` card of each exported file equals the frame's canonical filter.
  When the file's own card differs (or is absent), the export writes a COPY
  with the card rewritten. The landed replica and an own original are never
  modified.
- **W5 — Mixed symlink export.** With "use symbolic links" on, a frame that
  needs the rewrite is copied with the rewrite; every other frame is a
  symlink to its landed file.
- **W6 — A re-export never deletes.** Files of a previous export that are no
  longer part of the project (a remap moved them to another filter folder,
  an exclusion) are named in a warning, never deleted.
- **W7 — Binning is not a group key for a project.** The canonical grid
  resamples every scale. When frames of different binning or pixel scale
  may be stacked together is its own later study (§17); until then no frame
  is dropped for it, and the existing M4b scale warning reports the spread
  against the grid's scale.
- **W8 — One pipeline.** A project is a second *stacking subject* of the
  existing pipeline (`StackSubject::{FrameSet, Project}`), not a second
  pipeline and not shadow catalog rows.
- **W9 — Filters and colour mode never mix.** A group is one colour mode
  (mono / OSC) and one canonical filter; OSC frames with different filters
  (a dual narrowband, a UV/IR cut, none) are different groups, and OSC never
  joins mono (until the channel-split spec, v3 R21).

Restated from v3 and unchanged: R10's grid (blocker `grid` until the
coordinator confirms; the proposal is computed in the app); R12 (an
excluded frame is never stacked or exported); R1's export hierarchy; R3
("never calibrated again by export or stacking").

## 3. Amendments to v3 (A8)

- **§6.2, last bullet** ("the file's `FILTER` header is never rewritten")
  now reads: the *landed* file's header is never rewritten; an *exported
  copy* always carries the canonical name (W4).
- **§7 groups**: `grouping.exposureMode = strict | equivalent | none`
  becomes `seconds | stops | equivalent | none` — `stops` is v3's `strict`
  (class width in stops, default 1), `seconds` keeps today's tolerance
  clustering as the frame-set default; binning is not a key for a project
  (W7).
- **§7 reference**: the run's reference must have a WCS (manifest or the
  local solve, W3); the grid is applied by composition (§6.4), never by a
  second resampling.
- **§7 weights**: the ZP-derived `t_eq` exposure term waits for 6.5 (W1).
- v3 §16 gets a one-line A8 entry pointing here.

## 4. Data model

### 4.1 The project frame handle

```
stack_project_frames(
  id          INTEGER PRIMARY KEY,
  project_id  TEXT NOT NULL,
  frame_uuid  TEXT NOT NULL,
  UNIQUE (project_id, frame_uuid),
  FOREIGN KEY (project_id, frame_uuid)
    REFERENCES project_frames_local(project_id, frame_uuid) ON DELETE CASCADE)
```

A row is minted the first time a plan includes the frame and is never
re-used for another frame. Inside the pipeline a project frame is this
`id` — an `i64` like a catalog `frame_id` — so `GroupFrame`, `RunContext`,
`HashMemo`, `SolveCache` and every map keyed by a frame id keep their type.
A run belongs to exactly one subject, so a handle and a catalog id never
meet in one map. A new content version keeps the uuid and therefore the
handle; its artifacts go stale through the content hash (§5.1).

### 4.2 Stacking tables (rebuilt once, in `init_db`)

| Table | Change |
| ---- | ---- |
| `stacking_runs` | `frames_set_id INTEGER NULL → frames_set CASCADE`, new `project_id TEXT NULL → collab_projects CASCADE`, `CHECK ((frames_set_id IS NULL) <> (project_id IS NULL))`. `reference_frame_id` keeps holding a frame id (a handle for a project run). |
| `stacking_run_frames` | `frame_id INTEGER NULL → frames CASCADE`, new `project_frame INTEGER NULL → stack_project_frames CASCADE`, `CHECK` exactly one; two partial unique indexes replace `UNIQUE(run_id, frame_id)`. |
| `stacking_artifacts` | the subject pair as on `stacking_runs`; the frame pair as on `stacking_run_frames` but both may be NULL (group-level rows); the unique index becomes `(COALESCE(frames_set_id,0), COALESCE(project_id,''), group_key, kind, COALESCE(frame_id,0), COALESCE(project_frame,0))`. |
| `master_lights` | the subject pair. |
| `stacking_run_groups` | unchanged (FK to the run). |
| `stacking_set_config` | unchanged (frame sets only). |
| **new** `stacking_project_config` | `project_id TEXT PRIMARY KEY → collab_projects CASCADE, config_json, excluded_frame_uuids_json DEFAULT '[]', reference_frame_uuid TEXT NULL, updated_at`. |
| `collab_projects` | `ALTER ADD canonical_grid_json TEXT NULL` (the hub's grid, §6). |

The rebuild is the standard SQLite table rebuild (new table, copy, drop,
rename, recreate indexes) inside one transaction under `INIT_DB_LOCK`, with
foreign keys off for the copy and `PRAGMA foreign_key_check` before commit.
It is guarded by a schema probe (`pragma table_info(stacking_runs)` lacks
`project_id`) and is therefore idempotent. Existing rows copy verbatim, the
new columns NULL. Registration rows of a project run do not go to
`registration_results` (catalog-keyed); the registered artifact's
`payload_json` holds the star-based map and its quality (§5.5).

### 4.3 The subject in code

`stacking::StackSubject { FrameSet(i64), Project(String) }` replaces
`RunContext.set_id` and the `set_id` arguments of `build_plan`,
`start_stacking`, `db::stacking::*`, `WorkingLayout` and the run-id lookups
(preview, provenance). `db::stacking` writes the subject's column and the
frame's column from the subject. A frame source returns the plan's
`GroupFrame`s plus a per-frame origin:
`FrameOrigin::Catalog { file_id }` or
`FrameOrigin::Project { frame_uuid, publisher, landed_path, blake3, size_mtime, manifest }`.
`ActiveStacks` admits one active run per subject.

## 5. The pipeline for a project subject

The board keeps its ten rows. What changes per stage:

### 5.0 Masters

Not applicable. The row reads "not needed — project frames are calibrated
by their publishers"; the panel says the same. The blockers `masters`,
`links`, `masterFiles` never apply to a project.

### 5.1 Prepare (the Calibrate slot)

Input: the frame's `landed_path` from `project_frames_local`.

| The file is | Prepare does |
| ---- | ---- |
| uncompressed float32 FITS, one plane (mono, or an OSC CFA mosaic with `debayer_osc` off) | nothing — the file is used **in place**; the `calibrated` artifact points at `landed_path` |
| an OSC CFA mosaic with `debayer_osc` on | VNG debayer into `prepared/<group>/p_<stem>_d.fits`; for Bayer drizzle the mosaic is the landed file itself (no `calibrated_mosaic` copy) |
| uncompressed float32 FITS, three planes | used in place |
| XISF, an integer or compressed FITS, another layout | converted to float32 FITS in `prepared/<group>/p_<stem>.fits` (then debayered if OSC and `debayer_osc` on) |
| distributed XISF, unreadable | excluded, reason "unsupported file" |

- Only `calibration.debayerOsc` (and the debayer method) of the
  `calibration` subtree applies to a project; the rest is hidden in the
  panel and kept out of the Prepare hash.
- Stage hash: `blake3` and byte size of the row + the prepare subtree. The
  file identity (`size:mtime`) is re-checked against the row's
  `size_mtime_seen` before every read of a landed file in any stage; a
  mismatch excludes the frame with reason "changed during the run" (the live
  exchange may land a new version mid-run). Reads never hold the file open
  across stages.
- BAYERPAT/ROWORDER come from the file's header; the manifest's `bayerpat`
  only decides the colour mode at plan time.

### 5.2 Solve (a sub-step at the start of Measure)

- A frame whose manifest has `meta.wcs` uses it (TAN, 1-based CRPIX, CD,
  optional SIP — `collab/frame_meta.rs`). The manifest WCS refers to the
  published file's own geometry, which Prepare does not change.
- A frame without it is solved on its prepared file with
  `plate_solve::solve_frame_with_hints`, the `Frame` built transiently from
  the manifest (no catalog row), hints = the manifest's `pixelScaleArcsec`
  and the project target's RA/Dec. Result: a `solve` artifact
  (`payload_json` = the `PlateSolveRecord`), hash = `blake3` + the solver
  config hash + the catalog identity. Never re-solved while the hash holds.
- A failed solve is not an error: the frame continues without WCS (W3).

### 5.3 Measure

Unchanged: re-measured from the prepared pixels. The manifest's metrics are
display-only (the publisher's analysis ran on another pipeline).

### 5.4 Reference

Unchanged selection (Auto: best weight in the largest group with the
two-pass re-pick; Manual: `stacking_project_config.reference_frame_uuid`),
with one new rule: **candidates must have a WCS** (manifest or §5.2).

- `registration.geometry = coRegistered` (the project default): one run
  reference for every group, so cross-filter alignment stays star-based.
- `native`: each group its own reference; cross-group alignment then rests
  on the WCS of the group references.
- No candidate with a WCS after §5.2 → the run fails the stage with "no
  frame of this project could be plate-solved; the grid needs one".

### 5.5 Register onto the grid

- The star-based subject→reference `PixelMap` is fitted exactly as today
  (quad seed, WCS seed from §5.2 across scales, polynomial or TPS
  distortion, the per-frame scale gate). It is cached in the `registered`
  artifact's payload together with `reg_*` quality and the reference handle.
- **The output geometry is the grid.** The resampler's per-pixel chain is
  grid pixel → sky (the grid's TAN) → reference pixel (the reference's WCS,
  SIP inverse included) → subject pixel (`PixelMap::inverse_eval`). One
  interpolation, as today. The grid→reference part is sampled once per
  reference onto a displacement grid (every 32 px, bilinear; pinned by a
  test at < 0.01 px against the exact chain).
- The registered file has the grid's width × height, NaN outside the
  frame's footprint (`register/writer.rs` already writes NaN outside).
- The registration hash folds in the grid hash, so a grid change
  re-registers on the next run.
- The two-pass dry pass and the scale statistics work in reference space,
  unchanged.

### 5.6 Normalize (LN)

- The LN reference is built on the grid from the group's sky-penalized best
  members with a per-pixel count; a pixel no member covers is NaN.
- Every member's LN models are fitted on its overlap with the LN reference
  (both finite). A member whose overlap is under 25 % of its own footprint
  falls back to the global normalization for that member, with one warning
  naming the count.
- This is the wave's numeric risk (§16): partial coverage is new to LN.

### 5.7 Integrate and the coverage map

- The engine counts, per output pixel, the included frames whose registered
  sample is finite (geometric coverage, before rejection). One plane.
- With `output.coverageMap` on, the count is written as a float32 FITS
  `…_coverage.fits` beside the master (`master_lights.kind = 'coverage'`,
  `BUNIT = 'frames'`, the master's WCS).
- A pixel with no data stays 0.0 in the master, as today: the count is a
  side output and the master's pixels do not change (pinned bit-identical on
  a frame set with the map on and off).
- Available to frame sets too; default on for a project, off for a set.

### 5.8 Drizzle

The output grid is the canonical grid × the drizzle scale. The forward map
is subject → reference (`forward_eval`) → sky (the reference's WCS) → grid.
With `output.coverageMap` on, drizzle writes its own coverage (frames whose
drop touched the pixel) beside its weight map.

### 5.9 Output

- The master's WCS is the grid's TAN (a synthetic `PlateSolveRecord`:
  CRVAL = grid centre, CRPIX = grid centre pixel, CD from scale and
  rotation, no SIP); drizzle scales it with `scale_plate_solve`.
- `output.cropToCoverage = K` (default 0 = off): every master, rejection
  map and coverage map of the run is cropped to ONE run-wide rectangle — the
  union over groups of the bounding boxes of `coverage ≥ K` — so the run's
  masters stay pixel-aligned; CRPIX shifts by the crop origin. In `native`
  mode the rectangle is per group.
- Cards: `ATH_SPRJ` (project id), `ATH_GRID` (grid hash, 16 hex),
  `ATH_RGEO = 'grid'`. **Never `ATH_PRJ`**: the scanner diverts any file
  carrying it to the project reconcile branch.
- Masters go to `<output_dir>/<sanitized project title>/`, named as
  personal masters are.

## 6. The canonical grid

### 6.1 Storage and feed

The hub's `projects.canonical_grid` (`{scaleArcsec, widthPx, heightPx,
crval1, crval2, rotationDeg}`, validated by `put_grid`) is the source of
truth. The app keeps a copy in `collab_projects.canonical_grid_json`,
refreshed with the project meta; the feed's `ChangeKind::Grid` (today logged
"no consumer in this wave") triggers that refresh and emits
`collab-grid-changed { projectId }`. Grid hash = XXH3 of the canonical JSON
(fixed key order, numbers as stored).

### 6.2 The proposal

`get_collab_grid_proposal` computes from the WHOLE manifest
(`project_frames_local` rows `published ∧ accepted`), so every device
proposes the same grid for the same manifest version:

- scale = the median `meta.pixelScaleArcsec`, rounded to 0.01″;
- centre = the project target (RA/Dec);
- rotation = 0 (north up);
- size = the bounding box, in the proposed TAN projection, of the
  footprints of the frames with `meta.wcs`, clipped to the bounding box of
  the target circle, rounded up to a multiple of 64 px; no frame with WCS →
  a square of 2 × target radius.

It also reports how many accepted frames fall at least half inside it.

### 6.3 Confirming

- A member with `project.edit` sees, in the Stacking tab, the grid card
  with the proposal and editable scale / size / centre / rotation, and saves
  through `set_collab_project_grid` → `PUT /projects/{id}/grid`. The local
  copy is written after the hub answers 2xx; the feed confirms it.
- Everyone else, while the grid is unset, gets the blocker `grid`: "waiting
  for the coordinator to confirm the grid", with the proposal shown.
- Changing a confirmed grid warns that every processor's next run
  re-registers.
- A confirmed grid that leaves accepted frames outside raises a plan
  warning ("N frames lie outside the grid").

## 7. Frame selection and the plan

- **Stackable**: `state = 'published' ∧ accepted ∧ on_disk ∧ ¬awaiting_gc ∧
  landed_path IS NOT NULL ∧ local_state ∈ {held, own_held}` — own frames
  included, own pending ones not (as the export, R31).
- **Manual exclusions**: per device, per project, by `frame_uuid`
  (`stacking_project_config.excluded_frame_uuids_json`).
- **Warnings**:
  - frames of the project not on this device, counted per canonical filter;
  - frames that will be solved locally;
  - the M4b scale spread, measured against the grid's scale;
  - unknown exposure equivalence (§8);
  - frames outside the grid.
- **Blockers**: `grid | folders | space | frames | unsupported`. `space`
  estimates the registered artifacts at grid size × planes × frames, plus
  the prepared conversions. `frames` = nothing stackable.
- **No re-gating.** Thresholds are prospective (v3 §3.3): the stack takes
  every accepted frame; a coordinator exclusion drops a frame on the next
  plan.
- **The project plan** is a `StackingPlan` with `subject`, the grid state
  (`unset | proposed | confirmed`, with the proposal) and `projectFrames`:
  `[{handle, frameUuid, fileName, publisher, filterCanonical, exptimeSec,
  colorMode, hasWcs, onDisk}]` — the Frames table's rows.

## 8. Groups and exposure classes

**Project group key**: colour mode × canonical filter × exposure class →
`"<mono|osc>__<canonical>__<class token>"`. Camera and binning are not keys
(W7). The canonical filter is `project_frames_local.filter_canonical` (the
current value, remaps included); the dictionary's `None` is its own filter.
Frame-set keys are unchanged.

`grouping.exposureMode` (new, `#[serde(default)]`), with
`grouping.stopsWidth` (default 1.0):

| Mode | Classes | Token |
| ---- | ---- | ---- |
| `seconds` | today's clustering with `exposureToleranceSec` — the frame-set default, so existing stacks do not change | `<t>s` (today's) |
| `stops` | frames sorted by exposure; a class starts at the shortest remaining `t0` and takes every exposure `≤ t0 · 2^W` — the project default | `st<t0>` |
| `equivalent` | as `stops`, but on `e = t / N²` with `N = FOCALLEN / APTDIA` when both are known; frames with an unknown `N` are classed by seconds in classes of their own, with the warning "exposure equivalence unknown for N frames — grouped by seconds" | `eq<e0>` / `<t>s` |
| `none` | one class per colour × filter | `all` |

- APTDIA: publication starts putting `meta.aperture` (from the source's
  APTDIA) into the manifest; older frames lack it and fall to seconds. A
  frame set reads APTDIA from its stored header (`fits_header`).
- The new grouping and output fields sit outside the per-frame stage
  subtrees (calibration / measurement / registration / LN), so adding them
  invalidates no cached artifact of any existing frame set. A test pins
  this.

## 9. Commands, events, paths, config

### 9.1 Commands

Eleven new commands, on both hosts, each a 3–5-line wrapper over
`athenaeum-core/src/api/`. Each wears `#[tracing::instrument(skip_all, err)]`
(the web mirror `err(Debug)` per its return type). The count goes 294 → 305.

| Command | Args | Returns |
| ---- | ---- | ---- |
| `get_project_stacking_plan` | `projectId, config?` | `StackingPlan` (project form) |
| `start_project_stacking` | `projectId, config, rerunFrom?` | `{runId}` |
| `get_project_stacking_runs` | `projectId, limit` | `StackingRunSummary[]` |
| `get_project_stacking_config` | `projectId` | `{config, excludedFrameUuids, referenceFrameUuid, isDefault, updatedAt}` |
| `set_project_stacking_config` | `projectId, config, excludedFrameUuids, referenceFrameUuid?` | same |
| `get_project_stacking_work_usage` | `projectId` | as for a set |
| `cleanup_project_stacking_work` | `projectId, what` | as for a set |
| `get_collab_grid_proposal` | `projectId` | `{current, proposal, framesInside, canEdit}` |
| `set_collab_project_grid` | `projectId, grid` | the stored grid |
| `remap_collab_filter` | `projectId, frameUuids, canonical` | `{started: true}` (runs in the background, §11.2) |
| `cancel_collab_remap` | `projectId` | `()` |

The run-id commands (`cancel_stacking`, `get_stacking_run`,
`get_master_light_preview`) serve both subjects; the preview resolves the
working folder from the run row's subject. Frame-set commands do not
change.

### 9.2 Events

- `stacking-progress`, `stacking-complete`: `setId` becomes optional and
  `projectId` (optional) is added; exactly one is set. `frameId` of a project
  run is the handle; the plan's `projectFrames` maps it to the uuid. The
  frontend keys its maps by `set:<id>` / `project:<uuid>`.
- `collab-grid-changed { projectId }` (§6.1).
- `collab-frames-changed` gains the kind `remapped` (a row's
  `filterCanonical` changed at the same content version).
- `collab-remap-progress { projectId, done, total }` (throttled 300 ms) and
  one `collab-remap-finished { projectId, done, failed, cancelled, error? }`.

### 9.3 Paths

`<working_dir>/projects/<project-slug>/{prepared,registered,ln,fits,rej,runs,previews}/`
(project slug from `collab_projects.slug`). Work usage and cleanup operate
on that folder. Masters in `<output_dir>/<sanitized project title>/`.

### 9.4 Config

Whole-config precedence as for a set: the stored per-project document IS
the run's config. The first plan of a project with no document seeds it
from the global default with `grouping.exposureMode = stops` and
`output.coverageMap = true`, and stores it. New fields:
`grouping.exposureMode`, `grouping.stopsWidth`, `output.coverageMap`,
`output.cropToCoverage`, all `#[serde(default)]`, no
`STACKING_CONFIG_VERSION` bump. Presets stay subject-free.

## 10. The project export

### 10.1 Layout and selection

`<out>/<title>/<publisher>/camera_<instrume>/<canonical filter>/<file>` —
title and publisher through `sanitize_display_folder_name`, the camera
through `sanitize_folder_name` (as today), the filter folder through
`sanitize_display_folder_name` of the canonical name. The `lights/` level of
today's project layout goes. Selection is unchanged:
`published ∧ accepted ∧ on_disk ∧ ¬awaiting_gc ∧ landed_path` (own pending
excluded, R31). Grouping and the folder use `filter_canonical`, never
`filterRaw`.

### 10.2 The FILTER rule (W4)

For each exported frame, read the file's own `FILTER`: FITS — the header of
the HDU that holds the image (the primary, or the first tile-compressed
image extension), XISF — the `FITSKeyword` of the first `<Image>`. Trimmed;
absent = empty. Compare exactly (case-sensitive) with the canonical name.

- Equal → the file is exported as is (copy, or symlink in symlink mode).
- Different → a copy is written with:
  - `FILTER = '<canonical>' / was '<raw>'` (`/ was unset` when absent);
  - `ATH_FILT` updated when present;
  - FITS `CHECKSUM` removed when present (`DATASUM` stays valid; the data
    unit is unchanged).

### 10.3 The FITS patcher (`fits_writer::header_patch`)

`patch_cards(src, dest, edits)` — `CardEdit::Set { key, value, comment }` /
`CardEdit::Remove { key }`.

- It walks the HDUs. In each target header it replaces the first card of
  the key in place, drops its duplicates, or inserts it before `END`.
- The header grows by one 2880-byte block only when the `END` block has no
  free card.
- Every byte after each header is streamed verbatim (no pixel decode). HDUs
  carry no absolute offsets, so a grown header simply shifts what follows.
- Writes `<dest>.tmp-patch`, then `rename_replace` onto `dest`. It refuses
  `src == dest` and never opens `src` for writing.
- It supersedes `stamp_extra_card` (insert-only, unused), which goes.

### 10.4 The XISF patcher (`fits_writer::xisf_patch`)

`patch_fits_keywords(src, dest, edits)`, monolithic XISF only.

1. Parse the signature, the header length and the XML.
2. In each `<Image>`, set or insert the `FITSKeyword`. Also update a
   `Property id="Instrument:Filter:Name"` when one exists.
3. Re-serialize the XML.
4. If `16 + new length ≤` the first attachment's position, write the header,
   zero-pad to that position and copy the rest verbatim.
5. Otherwise shift every attachment by the smallest multiple of 4096 that
   fits. Rewrite every `location="attachment:<pos>:<size>"`, iterating to a
   fixed point because the digit counts change. Then copy every data block
   verbatim from its old position.
6. Compression, checksums of data blocks and inline/embedded data are
   untouched.

A distributed XISF (data in external files) is not exported: the frame is
skipped with the warning "distributed XISF is not supported by the project
export".

### 10.5 Symlinks, re-export, results

- **Symlink mode (W5)**: a frame that needs the rewrite → patched copy; any
  other → symlink to `landed_path`. The option stays where it is today
  (desktop macOS/Linux).
- **Re-export into an existing tree.** An existing destination is kept when
  it is already what this export would write. That means a symlink to the
  same target, an as-is copy of the same size, or a patched copy whose
  `FILTER` is the canonical name. Otherwise it is replaced (temp + rename).
  - The export writes `<out>/<title>/.athenaeum-export.json` listing the
    relative paths it placed with their frame uuids.
  - On a re-export, paths listed before and not placed now are named in one
    warning (first 20 + count), **never deleted** (W6).
- A copy is checked against the row's `byte_size` after writing. A mismatch
  warns "changed during export" and the frame counts as failed.
- `ExportResult` gains `linked`, `copied`, `rewritten`, `failed`. The dialog
  shows them on completion. The completion notification carries the counts
  and the warnings.

## 11. Filter remap

### 11.1 UI

- The Moderation tab is visible when the caller can moderate OR holds
  `thresholds.edit`. The card gains `canRemap` (`thresholds.edit`) and
  `canEditGrid` (`project.edit`), derived from the caller's governance caps
  as `canModerate` is.
- Section **Filters** is shown with `canRemap`. It is a table of buckets,
  one per publisher × camera (`instrume`) × raw filter, over the manifest's
  rows of every state. Columns:
  - Publisher, Camera, Raw filter ("(no FILTER)" for empty);
  - Canonical (the current value, "mixed" when the bucket disagrees);
  - Frames;
  - a dictionary `Select` (name + kind) and Apply.
- Apply opens a `DialogShell` confirm: "Remap N frames from '<raw>' to
  <canonical>? Stacking groups and exports follow on the next plan; no file
  changes."

### 11.2 Core

- `remap_collab_filter` validates that the canonical name is in the
  project's dictionary, and admits one remap per project at a time (a second
  is refused).
- It runs in the background: per frame
  `PATCH /projects/{id}/frames/{uuid} {filterCanonical}`, four in flight,
  cooperative cancel between requests.
- A 403 stops the task with "this account lacks thresholds.edit". Other
  per-frame failures are collected.
- It emits `collab-remap-progress` / one `collab-remap-finished` and logs
  the outcome.
- The app writes no local row. The manifest change arrives by the feed,
  which updates `filter_canonical` and `manifest_json` (as today) and now
  classifies the edit as `FramesChangeKind::Remapped`.
- Notification on `collab-remap-finished` (discrete outcome). For
  `remapped` frames of the caller's own, a history-only entry ("N of your
  frames were remapped to R"); none for others' frames.

## 12. UI — the project's Stacking tab

- A **Stacking** tab after Library, visible to every member. With nothing
  stackable on this device it shows an empty state naming why (no frames on
  disk / all excluded).
- `StackingTab` takes
  `subject: {kind:'frameSet', framesSetId, lightFrames} | {kind:'project', projectId}`.
  A small adapter hook maps every command to the subject's variant.
  `StackingContext` keys by subject key; the selected-run localStorage key
  gains the subject.
- For a project:
  - row 0 is muted ("not needed"); row 1 is labelled **Prepare**, and its
    panel shows in place / debayered / converted counts and the debayer
    switch;
  - the Coverage and Analysis navigations are hidden;
  - `FramesTable` rows come from `plan.projectFrames` (adds Publisher; the
    Manual reference pin and the exclusion toggle work by uuid);
  - the notification link is `/projects/<id>?tab=stacking`.
- **Grid card** above the board: unset (proposal + "frames inside" count +
  `Use proposal` / `Edit…` for `canEditGrid`, a waiting line for others) or
  confirmed (values + `Change…` for `canEditGrid`, with the re-register
  warning).
- `OutputPanel` gains the coverage-map switch and the crop field.
  `GroupsTable` shows the exposure-class token.
- The export dialog shows linked / copied / rewritten / failed.
- `npm run ui:harness` gains a project-stacking scenario. Design tokens and
  `src/components/ui/` primitives throughout.

## 13. R3 — attested frame sets

- **Personal stacking**: a frame set with `calibrated_externally = 1` runs
  Prepare (§5.1, on the catalog path) instead of Calibrate. Masters are not
  needed; the blockers `masters | links | masterFiles` do not apply.
- **Calibrated-lights export**: such a set's lights are copied as is (no
  generation, original file name). `check_mode_ready(CalibratedLights)`
  treats its lights as ready.
- One readiness answer for export, send and stacking, as today.

## 14. Errors and logging

- Every command boundary logs its `Err` (instrument `err`).
- Lifecycle at info: plan built, run started/finished, grid saved, remap
  finished, export finished with counts.
- Per-frame decisions at debug: in place / converted / solved / excluded.
- New field names go into the logging spec's dictionary in the same change:
  `grid_hash`, `prepared` (`in_place | debayered | converted`), `linked`,
  `rewritten`. Existing ones are reused: `project_id`, `frame_uuid`, `run_id`,
  `group_key`, `count`, `outcome`, `duration_ms`, `error`.
- No `println!`. Never a swallowed error: an excluded frame always carries
  its reason into `stacking_run_frames.exclusion_reason` and the summary.

## 15. Testing

### 15.1 Unit and integration

- **Patchers.** `header_patch` cases: replace, insert, grow by a block, a
  duplicate dropped, multi-HDU, tile-compressed, `CHECKSUM` removed. In
  every case the data bytes are identical. `xisf_patch` cases: fits in
  place, shifted attachments, `location` fixed point, the property updated,
  data blocks byte-identical, distributed refused. Both refuse
  `src == dest`.
- **Export.**
  - Layout; the FILTER rule (equal / different / absent / `None`).
  - Mixed symlink.
  - Re-export: kept vs replaced; stale paths warned, never deleted.
  - Counts; a mid-export size change.
- **Migration.** A DB with frame-set runs, artifacts and masters rebuilt:
  rows intact, cascades intact (deleting a set or a frame removes its rows),
  idempotent second run.
- **Groups.** The four exposure modes, the token formats, `None`. No cache
  invalidation for an existing set's stages (pin).
- **Grid.**
  - The proposal is deterministic for a manifest; hash stability.
  - The grid→reference displacement grid is < 0.01 px from the exact chain.
  - The forward and inverse chains are consistent.
- **Coverage.** Counts match the footprints. A frame set's master is
  bit-identical with `coverageMap` on and off. Crop rectangle and CRPIX.
- **Project plan.**
  - Stackable selection; own pending out; excluded out.
  - Blockers and warnings; the handle is stable across plans.
- **Remap.** Dictionary validation; the 403 stop; cancel. A fake-hub remap
  arrives as `remapped` and re-buckets the plan.
- **R3.** An attested set stacks without masters; the calibrated-lights
  export copies it as is.

### 15.2 End to end (fake hub)

A synthetic project: two publishers, two pixel scales, two fields of view,
mono R/G and OSC with two filters, a coordinator exclusion, one frame
without WCS. The plan shows the right groups and blockers. The grid is
confirmed. The run writes masters on the grid with a coverage map whose N
matches the footprints. LN takes the global fallback for the small-overlap
frames. The excluded frame is absent. The unsolved frame is solved locally
and cached (a second run does not solve it again).

### 15.3 Acceptance (the stacking method rules)

- **Real data.** A real project on the test hub with frames from two rigs.
  Each group's master on the grid is compared with a personal stack of the
  same frames, over the reference's footprint, on the rows of the Tier C §8
  tolerance table (median, MAD, noise, FWHM;
  `2026-09-20-stacking-compute-tierC-design.md`). It is a full group at
  master level, never a few-frame probe. A missed row is reported with its
  numbers for an owner ruling, never tuned toward the tolerance.
- **WCS.** An independent plate solve of a grid master agrees with the grid
  within 0.5 px.
- **Owner smoke.** An export with a remapped filter opens in WBPP grouped by
  the canonical filter, symlinks and copies mixed.
- **v3 §15's wave-6 lines.**
  - An excluded frame is absent from the processor's plan.
  - The processor stacks onto the grid with two pixel scales and reads the
    coverage map.
  - The export lands `<title>/<publisher>/<camera>/<filter>/`.

## 16. Risks

- **LN on partial overlap** (§5.6) is new numerics. It is judged only at
  master level on the acceptance set; the 25 % fallback threshold is
  re-measured there.
- **Disk.** Registered artifacts at grid size can exceed a frame's own size
  many times over for a wide grid. The `space` blocker and the estimate make
  this visible; nothing is silently skipped.
- **Reading replicas in place.** The exchange may land a new version under
  a running stack. The identity re-check excludes such a frame. On Windows a
  rename over an open file fails and the exchange retries later (its
  existing behaviour).
- **The table rebuild** touches the user's run history. It runs once,
  guarded, in one transaction with `foreign_key_check`, and is tested on a
  copy of a real database before merge.
- **`run.rs` breadth.** The subject seam touches many call sites. Plan 6b
  lands it first as a behaviour-preserving refactor (every existing stacking
  test unchanged) before any project code.
- **Absolute astrometry** of a grid master rests on the reference's WCS.
  Masters of two processors agree to within the two references' WCS error,
  not star-matched precision; within one run, groups are star-matched.

## 17. Out of scope and open

- **Out of scope:**
  - ZP and the `t_eq` weight term (sub-wave 6.5, W1).
  - The bin/scale admission study (W7).
  - The portal's grid editor and per-frame list (wave 7).
  - OSC channel split / super luminance (R21).
  - Ask to exclude; per-frame Download / Stop keeping.
  - R10's publish precondition (W3).
  - A hub batch-remap route (W2).
  - Deleting stale export files (W6).
  - A separate project Export tab.
- **Open, recorded in open-items:** the remap in-use count including
  rejected frames (a pending owner ruling, unchanged by this wave).

## 18. Plans

Each plan gets per-task and whole-branch review; merge and push wait for
the owner's word.

- **6a — export and remap.** Patchers, layout, the FILTER rule, mixed
  symlink, re-export, result counts, the dialog. Plus the remap commands,
  the Filters section, `canRemap`/`canEditGrid`, and the `remapped` feed
  kind. Independent of 6b; goes first.
- **6b — core.** In order:
  1. the subject seam (behaviour-preserving);
  2. the table rebuild and handles;
  3. the project frame source and plan;
  4. Prepare, Solve;
  5. the grid (storage, proposal, commands, feed);
  6. groups and exposure modes;
  7. registration onto the grid;
  8. LN on overlap;
  9. coverage and crop;
  10. drizzle and output on the grid;
  11. the project commands on both hosts;
  12. R3;
  13. the fake-hub e2e.
- **6c — frontend.** The `StackingTab` subject refactor, the project tab,
  the grid card, Prepare, `FramesTable` by uuid, the context keys,
  notifications, the harness scenario.
- **Docs, in the last plan.** `docs/stacking/README.md` (project subject,
  grid, coverage, exposure modes); `docs/export/README.md` (the project
  export, the patchers); `docs/transfers/README.md` (remap, grid);
  `CLAUDE.md` (command count 294 → 305, the Stacking and Transfers
  summaries, and the stale "collab publish of a device's own lights is
  deferred" line); the same stale claim in
  `docs/export/calibrated-lights.md`; v3 §16 A8; the logging dictionary;
  open-items (owner smokes, the acceptance run).

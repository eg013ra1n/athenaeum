# Athenaeum Export (WBPP folder export)

## Overview

The export feature organizes a frame set's light frames and their **already-linked**
calibration frames (flat/dark/bias/darkflat) into a nested folder tree on disk, sized
for the downstream processing program's WBPP (Weighted Batch Preprocessing) process to consume via its
"Grouping Keywords with Pre" option. Athenaeum copies or symlinks files into the
right folders and stops there — it does **not** create master calibration frames,
does not run the downstream processing program, and does not generate any external tool's scripts. All of
that (master creation, registration, stacking) is done in the downstream processing program after export.

This is the whole of the *frame-set* export. There is no script generator, no CLI
runner, no execution-mode selector, and no template-token engine (`{OBJECT}`,
`{FRAME_FOLDER}`, `:slug`, etc. do not exist in this module — see "What this doc
replaces" below). A second, project-scoped entry point (collab) reuses the same
organizer idea over a different collector — see "Project-scoped export (collab)" below.

Module: `crates/athenaeum-core/src/export/` — `models.rs`, `data_collector.rs`,
`file_organizer.rs`, and `project_collector.rs` + `project_organizer.rs` +
`filter_header.rs` (re-exported by `mod.rs`). The first three are the frame-set path
described here; the last three are the collab project export.

## What this doc replaces

The previous version of this file described a script pipeline for a second open-source stacker (branch
folders, `00_create_masters.ssf` / `01_calibrate_lights.ssf` / `02_register_and_stack.ssf`,
`generate_scripts` / `direct_execution` modes, an `AtheneumScoring` reference-frame
mode, a CLI runner for that stacker with a 30-second timeout workaround, exposure-time
clustering with absolute/relative tolerance, etc.). None of that exists in the
codebase — zero hits for `script_generator`, `cli_runner`, `folder_structures`,
or any invocation of that stacker anywhere in `crates/`. It described a feature that was
planned but never built. This rewrite documents only what `crates/athenaeum-core/src/export/`,
`commands/export.rs`, `routes/export.rs`, and `ExportTab.tsx` actually do.

## How it works (data flow)

1. **`collect_export_data`** (`data_collector.rs`) takes a `frame_set_id` and:
   - Loads every LIGHT frame reachable through
     `session_members → sessions → imaging_nights → frames_set` (`get_light_frames_for_frame_set`, `data_collector.rs`),
     in capture order. **A light whose file is in the Black Hole is not exported**, sent, or counted by readiness:
     the query splices `db::FRAME_NOT_IN_BLACK_HOLE_SQL`, the same fragment as the stacking plan's light source
     (2026-10-25; set 182 exported its 25 rejected lights before).
   - Groups them by `(filter, camera_type)` into `ExportGroup`s (`build_export_groups`, `data_collector.rs`).
     `camera_type` is `Osc` if `BAYERPAT` is a non-empty string, else `Mono`
     (`CameraType::from_bayerpat`, `models.rs`).
   - Within each group, further splits frames into `CalibrationSubgroup`s by the
     **combination of calibration-set IDs already linked to each frame**
     (`build_calibration_subgroups`, `data_collector.rs`).
   - Builds a `MasterCreationPlan` — a topologically-sorted, informational list of
     which master calibration files *would* need to be created and in what order
     (`build_master_creation_plan`, `data_collector.rs`).
2. **Calibration links are read, not computed.** `get_frame_calibration_links`
   (`data_collector.rs`) and friends query the `calibration_set_to_frames`
   table directly. Export does **not** call `calibration/configurable_matcher.rs`
   or `calibration/hierarchy.rs` — those run earlier, when the user runs
   calibration matching from the Calibration page, and persist rows into
   `calibration_set_to_frames`. If a frame set has no calibration links yet,
   export will show it with missing-calibration warnings but will not try to
   find or create matches itself.
3. **`organize_files_wbpp`** (`file_organizer.rs`) walks the same groups/subgroups
   and copies or symlinks files into the folder tree described below.

## Camera type detection

```
OSC (One-Shot Color): BAYERPAT is a non-empty string (e.g. RGGB, BGGR)
Mono: BAYERPAT is NULL or empty
```
Source: `CameraType::from_bayerpat`, `crates/athenaeum-core/src/export/models.rs`.
OSC and Mono frames land in different `ExportGroup`s (different `group_key`,
`models.rs`) because they can't be stacked together in the downstream processing program.

## Folder structure it produces

`organize_files_wbpp` and `build_folder_preview` (used for the UI's folder-tree
preview) both build the same shape — verified they use identical set-ID-keyed
logic (`file_organizer.rs` vs. `data_collector.rs`):

```
<output_dir>/
└── <frame set name, sanitized>/
    └── camera_<instrume, sanitized>/
        └── BIAS_<set_id>/           # bias frames — omitted if none linked
            └── DARKS_<set_id>/      # dark + darkflat frames — omitted if none linked
                └── FLAT_<set_id>/   # flat frames — omitted if none linked
                    └── lights/      # light frames — always present
```

This is the shape for `lightsOnly` / `rawWithCalibrationSets` / `rawWithMasters`.
The **Calibrated lights** mode (see "Export modes" below) uses the same
`<output_dir>/<frame set name>/camera_<x>/` root but skips the `BIAS_`/`DARKS_`/`FLAT_`
levels entirely — calibration is already applied, not linked, so only `lights/` exists.

- The frame-set folder name uses `sanitize_display_folder_name` (`models.rs`) —
  keeps spaces/case, replaces `: / \ * ? " < > |` with `_`, collapses repeats.
- The `camera_` folder name uses `sanitize_folder_name` (`models.rs`) — lowercases
  and strips everything but alphanumerics (e.g. `"ZWO ASI2600MM Pro"` → `zwoasi2600mmpro`).
- **Missing calibration levels collapse.** If a subgroup has no dark and no
  darkflat linked, there is no `DARKS_*` folder at all — the tree goes straight
  from `BIAS_*` (or `camera_*` if no bias either) to `FLAT_*`/`lights/`
  (`file_organizer.rs`).
- A flat's *own* dark/darkflat/bias (`flat.dark`, `flat.dark_flat`, `flat.bias` on
  `CalibrationSetInfo`) are folded into the same `BIAS_*`/`DARKS_*` folders as the
  light's own bias/dark, deduplicated by set ID via a shared `HashSet<i64>`
  (`organized_set_ids` in `file_organizer.rs`, `counted_sets` in `data_collector.rs`)
  so a calibration set already placed once isn't copied twice.
- `copy_or_link` skips a file if the destination path already exists
  (`file_organizer.rs`) — re-running an export into the same output
  directory does not overwrite or duplicate files already placed there.

## Copy vs. symlink, and the platform caveat

`export_to_wbpp` / `organize_files_wbpp` take a `use_symlinks: bool` supplied by
the caller — there is no config default; the frontend decides it per platform:

| Platform (in `ExportTab.tsx`) | Symlink toggle shown? | What actually happens |
| ---- | ---- | ---- |
| Tauri desktop, macOS/Linux | Yes | User's choice; unchecked = copy |
| Tauri desktop, Windows | No (hidden) | Always copies — `useSymlinks` state stays `false`, there's no UI path to set it `true` |
| Web/Docker | No (hidden) | Always copies — output dir is also constrained to `ATHENAEUM_EXPORT_DIR` (`crates/athenaeum-web/src/main.rs`) |

Source: `symlinksAvailable = isTauri && !isWindows` and the two
`symlinkUnavailableReason` strings, `src/components/export/ExportTab.tsx`.

**The Rust side does have a Windows symlink branch** — `copy_or_link`
(`file_organizer.rs`) has both `#[cfg(unix)]` (`std::os::unix::fs::symlink`)
and `#[cfg(windows)]` (`std::os::windows::fs::symlink_file`) arms. But because the
frontend never surfaces the toggle on Windows, that branch is currently
unreachable except by calling `export_to_wbpp` (Tauri) / `POST /api/export_to_wbpp`
(web) directly with `use_symlinks: true`. If it were exercised, note the standard OS caveat: creating
a file symlink on Windows requires either Developer Mode enabled or admin
privileges (`SeCreateSymbolicLinkPrivilege`) — not an Athenaeum-specific
restriction, just how `symlink_file` behaves.

**Known caveat with symlinked exports (documented, not yet solved):** because a
symlinked export is a set of links back into the catalog's original file
locations, moving the export root to a different volume, or sharing it via a
sync tool (e.g. Syncthing) or a Docker bind mount, breaks the links — the target
paths travel with the machine that created them, not with the export folder. A
"materialize copies" option (turning a symlinked export into real files after the
fact) is **planned** for a later pillar (pillar C, per the 2026-07-02 platform
parity audit); it does not exist today. Until then, use copy mode (the default)
for anything that will be moved or shared, and only use symlinks when the export
stays in place next to the catalog's original files.

## WBPP keyword-order config — stored, but not applied to the layout

`WbppExportConfig` (`models.rs`) has two fields: `export_mode` and `keyword_order: Vec<String>`, the latter
defaulting to `["CAMERA", "BIAS", "DARKS", "FLAT"]`. It's persisted under the
`export.wbpp_config` settings key via `get_wbpp_export_config` /
`set_wbpp_export_config` / `reset_wbpp_export_config`.

**It does not currently change what `organize_files_wbpp` does.** Both consumers
of `WbppExportConfig` take it as `_config` (`file_organizer.rs`) or
`_config` (`data_collector.rs`) — the parameter is accepted but unused; the
nesting order (`CAMERA` → `BIAS` → `DARKS` → `FLAT` → `lights`) is hardcoded.
The only place `keywordOrder` is actually read is the frontend's "WBPP Setup
Guide" (`ExportTab.tsx`), which uses it to render the setup instructions
and the example-structure text shown to the user. There's also no UI to edit
`keyword_order` today — `useWbppConfig`'s `save`/`reset` (`useExportData.ts`)
are exported but unused; `ExportTab.tsx` only reads the config, it never calls
`save`/`reset`. In practice this means: the setting exists and round-trips
through the DB, but changing it (via a raw API call) would make the on-screen
setup guide describe an order the folder organizer doesn't actually build.

## Master creation plan — informational only

`MasterCreationPlan` / `MasterInfo` (`models.rs`) are built by
`build_master_creation_plan` (`data_collector.rs`): a topological sort over
the unique calibration sets referenced by the export, with a suggested
`output_name` (e.g. `master_flat_38.fit`) and which sub-calibrations to apply
(`apply_bias` / `apply_dark` / `apply_darkflat`). For flats, `apply_dark` is only
set if the dark's average exposure time is within 30% of the flat's
(`FLAT_DARK_EXPOSURE_TOLERANCE = 0.30`, `data_collector.rs`); otherwise the
flat falls back to bias-only, matching the calibration hierarchy's normal
Flat → DarkFlat → Dark(±30%) → Bias fallback chain.

Athenaeum never acts on this plan — no code creates master files. The plan is
exposed through `ExportData.master_plan` (from `get_export_preview` /
`get_calibration_route`) purely as data; the only field the shipped UI currently
renders from it is a count (`masters_to_create` in `CalibrationRouteSummary`,
`commands/export.rs`). The downstream processing program's WBPP creates the actual masters after the
files are organized on disk.

## Export modes

`WbppExportConfig.export_mode` (`ExportMode`, `models.rs`) picks what a run puts
on disk for the lights + calibration side; the same enum gates the frame-set
*send* feature (Transfers), not only folder export — `check_mode_ready`
(`api::lights`) is the single readiness gate for both. Four modes:

- **`lightsOnly`** — raw lights, no calibration frames at all.
- **`rawWithCalibrationSets`** (default) — the behavior described above:
  raw lights plus the raw calibration sets behind their links. Where a link
  names an Athenaeum-built master (a build repoints every consumer link of the
  raw set onto the master), the raw set that master superseded is exported
  instead — the master file itself never lands in this mode; that is
  `rawWithMasters`. The substitution follows the raw set's own links, so a
  raw flat comes with its raw darks. An imported master (nothing raw behind
  it) is kept and reported in the run's warnings. Raw originals that are not
  on disk — archived after the build, or moved — block the mode up front
  ("N raw calibration file(s) missing on disk — restore from archive first",
  `ExportReadiness.missingRawCalibrationFiles`), the same way a missing
  master file blocks `calibratedLights`.
- **`rawWithMasters`** — raw lights, calibration side restricted to already-built
  master files (`calibration_set.is_master_library = 1`). Strict: a linked set
  that still has raw frames is a blocking error, not a silent omission.
- **`calibratedLights`** — described in full below; the lights themselves are
  generated, not copied, and there is no calibration folder at all.

## Calibrated lights export mode

Spec: `docs/superpowers/specs/2026-08-31-calibrated-export-v2-design.md`
(supersedes the standalone "Calibrate Lights" flow described in
`docs/superpowers/specs/2026-07-05-light-calibration-design.md`, which no
longer exists — no dialog, no `light_calibrations` table, no per-frame
calibration badge).

Selecting **Calibrated lights** does not copy pre-existing artifacts — it
**calibrates every LIGHT frame at export time** from its currently linked
masters (and, for OSC, debayers it) and writes the result straight into this
export's own destination tree. There is still no token-templating engine here
— output naming is a fixed rule (below), not `{OBJECT}`-style tokens.

- **Gate (masters-built strictness).** Every calibration set linked anywhere
  in the frame set's tree must already be a built master — a raw linked set
  blocks with "Build masters first — N sets without a master" (`→ Coverage`
  deep-link to the first blocking set). A light with **zero** calibration
  links blocks with "N lights have no calibration links". A *partially*
  linked light (e.g. dark only) does **not** block — it calibrates
  best-effort with an honest `CALSTAT`. Athenaeum never auto-builds masters
  during export; the blocker routes the user to Coverage instead.
- **Run-level options**, shown when the mode is selected and persisted the
  same way as the mode itself (`src/components/export/lightCalPrefs.ts`).
  Every option the retired standalone dialog had lives here now, under the
  same storage keys and with the same defaults, so an existing install's
  choices carry over:
  - **Normalize master flat** (default ON) + its **statistic** — central-third
    mean (Athenaeum) or full-frame trimmed mean — and, nested under the
    statistic it refines, **per-channel CFA flat scaling** (default ON,
    unavailable in the trimmed mode, which is measured whole-frame). The trimmed
    mode is `FlatNormMode::TrimmedMeanWhole` in Rust, with the trim fraction
    `TRIMMED_MEAN_TAIL_FRACTION`; its stored / wire string is the pre-rename one
    (`#[serde(rename)]` in `models.rs`, the new spelling accepted as an alias) so
    saved configs, the DB column, the frontend preference and an older build all
    keep working — the frontend compares against `FLAT_NORM_TRIMMED_MEAN_WHOLE`
    (`src/types/flat-norm.ts`), never a literal.
  - **Hot-pixel correction** (default ON) — replaces hot pixels detected from
    the applied master dark (median + 10·1.4826·MAD threshold) with a
    neighbourhood median before debayering; honestly skipped when no dark
    applies. Stamps `ATH_CHPX` (replaced-pixel count) on the output.
  - **Debayer OSC lights (VNG)** (default ON) — full-resolution VNG debayer
    for OSC lights, planar RGB output. OFF exports the CFA frame as-is
    (`c_*.fits`) for stackers that debayer themselves.
  - **Advanced** (collapsed by default) — the flat trim fraction (only in the
    trimmed statistic, default 0.05), the output pedestal in DN (default 0 =
    negatives preserved, added after the scale divide), and the no-dark-master
    policy: subtract the linked bias best-effort (default) or skip the frame.
  The same five values travel with a **Send to device…** of this mode, so a
  transfer generates the bytes an export of the set would.
- **Every export regenerates.** No cache and no skip-if-exists for generated
  files — a re-run overwrites its own previous output in place (tmp + atomic
  rename). The exists-skip described above under "Folder structure it
  produces" applies only to *copied* files in the other three modes.
- **Output layout**: lands in the *same* destination tree as every other
  mode — `<output_dir>/<frame set name, sanitized>/camera_<x>/lights/` — with
  no `BIAS_*`/`DARKS_*`/`FLAT_*` folders at all (calibration is already
  applied, not linked). This replaces the old standalone flow's separate
  `<CalibrationLibraryRoot>/<OBJECT>/<INSTRUME>/<date>/` artifact store, which
  no longer exists; any `c_*` trees still sitting under the Calibration
  Library root from that old flow are uncataloged leftovers — Athenaeum does
  not migrate or delete them automatically.
- **Output naming**: `c_<original stem>.fits` for a mono light, or an OSC
  light with the VNG toggle off; `c_<original stem>_d.fits` for a debayered
  OSC light (3-plane; the Bayer cards are stripped since the mosaic is gone,
  `ROWORDER` stays). The output container is the export's own choice (FITS
  default, XISF optional); an XISF source does not decide it.
- **Progress**: `export-progress` gains phase `"calibrating"` (per-frame
  `current`/`total`/`current_file`) alongside the existing `"copying"` phase
  used by the other modes; `cancel_export` and the existing progress/complete
  events are otherwise unchanged.
- Runs inside one `ComputeQueue` admission slot (job kind `LightCalibration`),
  so it serializes with master builds and analysis rather than running
  concurrently with them.
- The same generation logic runs during a frame-set **send** in this mode
  (Transfers) instead of at folder-export time — see
  `docs/export/calibrated-lights.md` for the send-side and receiver behavior,
  which is out of scope for this file.

Engine internals (the calibration formula itself, CFA flat handling, header
whitelist, BITPIX-aware scaling) live in
`crates/athenaeum-core/src/calibration_library/` and are documented in
`docs/export/calibrated-lights.md`, not repeated here — this file covers only how the *export* feature
drives them.

## Project-scoped export (collab)

A second entry point exports a *collaboration project* instead of a single frame
set. It no longer goes through `organize_files_wbpp`: the collector
(`collect_project_export_data`, `project_collector.rs`) gathers the project's
**received contributions ∪ this device's own calibrated outputs** — the frames the
project actually holds on disk — as `ProjectExportFrame`s carrying the canonical
filter, bucketed by publisher, and `organize_project_export`
(`export/project_organizer.rs`) places them. Spec
`docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md` §10.

- **Runner:** `export_project_for_wbpp` (`api/collab_exchange.rs`), wrapped by the
  `export_collab_project` command (Tauri `commands/collab.rs` + Axum
  `routes/collab.rs`). UI: "Export for WBPP" on a project's **Receive** tab opening
  `ProjectExportDialog.tsx`; the dialog lists the result's warnings (first 20 plus
  "and N more") and the completion notification carries the four counts and the
  first 3 warnings.
- **Layout:** `<output_dir>/<project title>/<publisher>/camera_<instrume>/<canonical filter>/<file>`
  (every segment sanitized; an empty INSTRUME or filter is `unknown`). WBPP groups by
  the filter folder, so a filter the coordinator remapped lands in one folder.
  No calibration folders — project frames are already-calibrated lights.
- **The FILTER rule (W4):** a file's own `FILTER` card is compared with the frame's
  canonical filter — exact, case-sensitive, after trimming; an absent card differs
  (`export::filter_header::needs_rewrite`). Equal: the file is exported as it is.
  Different: it is exported as a **patched copy** whose `FILTER` is the canonical
  name with the comment `/ was '<raw>'` (`/ was unset` when the card was absent;
  the comment is shortened, never the value, to fit 80 columns). The same copy sets
  `ATH_FILT` to the canonical name **only when the file already has it** (never
  inserted), and a FITS `CHECKSUM` card is dropped (`DATASUM` stays valid because
  the data is streamed verbatim). In an XISF copy the `FILTER` and `ATH_FILT`
  keywords get the same treatment and an existing `Instrument:Filter:Name`
  property is rewritten (never inserted).
- **Patchers:** `fits_writer::header_patch` (replace or insert cards, multi-HDU,
  data blocks copied verbatim, tmp + fsync + rename) and `fits_writer::xisf_patch`
  (splices the XML keywords and shifts every attachment offset when the header
  grows). The source is only ever opened for reading; `src == dest` is refused. A
  distributed XISF (`path(…)`/`url(…)` data locations) and an unparseable
  attachment location are refused — that frame counts as failed, with a warning.
- **Symlink mode:** a file that needs no rewrite becomes a symlink to the replica;
  a rewrite is a real copy (a link cannot carry a patched header). So in symlink
  mode only the rewritten frames are files.
- **Re-export:** into the same folder an existing destination is **kept** only when
  the previous export record maps THIS frame uuid to THIS path AND the destination's
  current size + mtime still match the recorded identity (plus the published size
  and, for a rewrite, the destination's FILTER and a source-mtime check);
  otherwise it is **replaced** through a temp file + rename. A copy never writes
  through a link: an old symlink at the destination is removed first, so the
  replica is never modified. Every as-is copy is stamped with its own placement
  time (so a recorded identity is never the source's), a destination that resolves
  to its own source is refused, and read-only sources export fine in copy mode.
- **Export record:** `<title>/.athenaeum-export.json` — `{version, projectId, frames:
  {<frame uuid>: {path, size, mtimeNs}}}` of the file actually placed. A re-export
  names the files of the previous export that are no longer placed (a remapped
  filter moves a frame to another folder) in ONE warning (first 20 listed) and
  **never deletes them**. A frame that failed keeps its previous record entry (it
  is not reported stale); a cancelled run keeps the previous entries of frames not
  reached. An unreadable record is a warning and is treated as empty.
  A record written by **another project** (its `projectId` differs from the running
  one — two projects whose titles sanitize to the same folder, exported into the same
  output) is ONE warning ("This folder already holds the export of another project
  (…); its files were left in place.") and is treated as empty: no stale list from it,
  nothing kept; this project's record then replaces it.
- **Output-root guard:** an output whose `<output>/<sanitized title>` folder is
  inside, equal to, or contains a scan root or the Collaboration root (both
  canonicalized, case and symlinks resolved) is refused, as is a path with `.` or
  `..` components, so an export can never overwrite catalogued files. The web
  mirror also validates `output_dir` against the configured export dir.
- **One at a time:** the cancel flag is registered under the sentinel
  `frame_set_id = -1`; a second project export is refused with `Conflict`
  ("Another project export is running."). The slot is an RAII guard, freed on
  success, error, panic, or when the request future is dropped (it also raises the
  cancel flag so an orphaned organizer stops at its next frame).
- **Counts:** `ExportResult` / `ExportCompleteEvent` carry `linked`, `copied`,
  `rewritten`, `failed` beside `filesOrganized` (= linked + copied + rewritten). A
  per-frame failure is a warning and the batch goes on; `success` is false only for
  a cancel (`error = "Export cancelled"`).
- **Events / cancel:** standard `export-progress` (phase `"collecting"` then
  `"copying"`, throttled to 100 ms; the percent is one monotonic run over all
  publishers) / `export-complete` with the sentinel `frame_set_id = -1`, so
  `api.invoke('cancel_export', { frameSetId: -1 })` cancels a running project
  export. Cancel stops between frames.

## Commands

Mirrored 1:1 in `crates/athenaeum-tauri/src/commands/export.rs` and
`crates/athenaeum-web/src/routes/export.rs`. "Used by current UI" reflects what
`ExportTab.tsx` actually calls today (via `useExportSummary` / `useWbppConfig` /
`useExportProgress`) — a couple of commands have hooks (`useExportData.ts`) that
aren't imported by any page.

| Command | Purpose | Used by current UI |
| ---- | ---- | ---- |
| `get_wbpp_export_config` | Read `WbppExportConfig` (default if unset) | Yes — setup-guide text |
| `set_wbpp_export_config` | Persist a `WbppExportConfig` | No (hook exists, unused) |
| `reset_wbpp_export_config` | Delete the stored config, revert to default | No (hook exists, unused) |
| `get_export_preview` | Full `ExportData` (groups/subgroups/master plan) for a frame set | No (hook exists, unused) |
| `get_exportable_frame_sets` | List frame sets with light-frame counts, for a picker | No (hook exists, unused) — `ExportTab` gets `frameSetId` as a prop from the frame set detail page, it doesn't pick from a list |
| `get_calibration_route` | `ExportData` reshaped into a UI calibration tree | No (hook exists, unused) |
| `get_export_summary` | The enhanced summary `ExportTab` actually renders (equipment, filter groups, folder preview, warnings) | Yes |
| `get_export_readiness` | The mode-less `ExportReadiness` counts behind the one readiness gate (`check_mode_ready`) for export and send | Yes |
| `export_to_wbpp` | Run the organizer: copy/symlink files into the WBPP tree, emit progress | Yes |
| `cancel_export` | Set the cooperative cancel flag for a running export | Yes (via `useExportProgress`) |
| `get_export_dir` (web only, no Tauri equivalent) | Return the server-configured `ATHENAEUM_EXPORT_DIR`, or null | Yes, web mode only — desktop uses a native folder picker instead |

Progress/completion events (both backends emit the same names — Tauri via
`app_handle.emit`, web via `SseProgressEmitter` over SSE): `export-progress`
(`phase: "collecting" | "copying" | "calibrating" | "complete"`, with `current`/`total`/`percent`/`current_file`)
and `export-complete` (`ExportCompleteEvent`, final outcome). `useExportProgress`
(`src/hooks/useExportProgress.ts`) listens for both and calls `notify()` on
`export-complete` with `kind: 'export'`.

## Frontend

- `src/components/export/ExportTab.tsx` — the live export UI, embedded as a tab
  on the frame set detail page (`src/pages/FrameSetDetail.tsx`, reached from the
  Objects list; frame set is passed in as a prop, not picked from a list). Shows
  warnings, the export summary (equipment, filter groups, folder
  preview), the output-directory picker, the symlink toggle (platform-gated, see
  above), the collapsible WBPP Setup Guide, and the Export button.
- `src/components/export/ExportSummary.tsx` — renders the `ExportSummary` payload
  (`get_export_summary`): cameras/telescopes/date range and the night count
  (`nightCount`: DISTINCT `imaging_nights` of the exported lights, the catalog's
  night — never the days in the span; the "Sessions" line read "380 nights" for
  a year-long set before 2026-10-25), per-filter-group
  breakdown (exposure groups from `build_exposure_groups`, `data_collector.rs`
  — an exact/rounded-to-0.1s tally for display, not a configurable clustering
  tolerance), calibration detail, and the folder-structure preview tree.
- `src/components/export/WarningsPanel.tsx` — renders `DetailedWarning`s
  (temperature mismatch >2°C/>5°C, missing flat/dark, calibration age) with a
  clickable set-ID chip that jumps to the Calibration Coverage tab.
- `src/hooks/useExportData.ts` — `useExportData`, `useExportableFrameSets`,
  `useCalibrationRoute`, `useExportSummary`, `useWbppConfig`.
- `src/hooks/useExportProgress.ts` + `src/contexts/ExportProgressContext.tsx` +
  `src/components/ExportProgressIndicator.tsx` — progress/cancel state and the
  global progress banner.

## Known limitations (as of this writing)

- Symlinked exports break if the export root is moved cross-device, or shared
  via a sync tool or container volume — see "Copy vs. symlink" above. No
  materialize-copies fallback exists yet (planned).
- `keyword_order` in `WbppExportConfig` is stored and round-trips through the
  settings table, but the folder organizer ignores it — the nesting order is
  hardcoded to CAMERA → BIAS → DARKS → FLAT → lights. There's also no UI to
  edit it, so this only matters if something calls the config commands directly.
- Export depends entirely on calibration links already existing in
  `calibration_set_to_frames`. It does not run calibration matching itself —
  if a frame set hasn't been matched yet, export runs with missing-calibration
  warnings instead of finding matches.
- `get_export_preview` and `get_calibration_route` (and their React hooks) are
  live commands with no current caller in the shipped UI — they were used by an
  earlier `ExportWizard` (see `ExportTab.tsx`'s own doc comment) that has since
  been replaced by the current tab.

## Key files

| File | Purpose |
| ---- | ---- |
| `crates/athenaeum-core/src/export/mod.rs` | Declares `calibrated_generator` (render-gated), `data_collector`, `file_organizer`, `filter_header`, `frame_set_queries`, `models`, `project_collector`, `project_organizer` |
| `crates/athenaeum-core/src/export/models.rs` | All export data structures, incl. `WbppExportConfig` |
| `crates/athenaeum-core/src/export/data_collector.rs` | Reads the catalog + existing calibration links, builds `ExportData`/`ExportSummary` |
| `crates/athenaeum-core/src/export/file_organizer.rs` | Builds the folder tree, copies or symlinks files, emits progress |
| `crates/athenaeum-core/src/export/project_organizer.rs` | Collab project export: layout, mixed symlink/copy, export record |
| `crates/athenaeum-core/src/export/filter_header.rs` | The project FILTER rule and the patched copy |
| `crates/athenaeum-core/src/fits_writer/{header_patch,xisf_patch}.rs` | Header patchers for a copy (FITS, XISF) |
| `crates/athenaeum-tauri/src/commands/export.rs` | Tauri commands (desktop) |
| `crates/athenaeum-web/src/routes/export.rs` | Axum routes (web/Docker), same surface + `get_export_dir` |
| `src/components/export/ExportTab.tsx` | The live export UI |
| `src/components/export/ExportSummary.tsx` | Renders the export summary payload |
| `src/components/export/WarningsPanel.tsx` | Renders detailed warnings |
| `src/hooks/useExportData.ts`, `src/hooks/useExportProgress.ts` | Data-fetching + progress hooks |
| `src/types/export.ts` | TS mirror of the Rust export models |

Calibration matching itself (which populates the links this feature reads) lives
in `crates/athenaeum-core/src/calibration/configurable_matcher.rs` and
`crates/athenaeum-core/src/calibration/hierarchy.rs` — not part of this module,
but the data this feature depends on.

# Athenaeum Export (WBPP folder export)

## Overview

The export feature organizes a frame set's light frames and their **already-linked**
calibration frames (flat/dark/bias/darkflat) into a nested folder tree on disk, sized
for PixInsight's WBPP (Weighted Batch Preprocessing) process to consume via its
"Grouping Keywords with Pre" option. Athenaeum copies or symlinks files into the
right folders and stops there — it does **not** create master calibration frames,
does not run PixInsight, and does not generate any external tool's scripts. All of
that (master creation, registration, stacking) is done in PixInsight after export.

This is the whole of the *frame-set* export. There is no script generator, no CLI
runner, no execution-mode selector, and no template-token engine (`{OBJECT}`,
`{FRAME_FOLDER}`, `:slug`, etc. do not exist in this module — see "What this doc
replaces" below). A second, project-scoped entry point (collab) reuses the same
organizer over a different collector — see "Project-scoped export (collab)" below.

Module: `crates/athenaeum-core/src/export/` — `models.rs`, `data_collector.rs`,
`file_organizer.rs`, and `project_collector.rs` (re-exported by `mod.rs`). The
first three are the frame-set path described here; `project_collector.rs` feeds
the collab project export.

## What this doc replaces

The previous version of this file described a Siril script pipeline (branch
folders, `00_create_masters.ssf` / `01_calibrate_lights.ssf` / `02_register_and_stack.ssf`,
`generate_scripts` / `direct_execution` modes, an `AtheneumScoring` reference-frame
mode, a Siril CLI runner with a 30-second timeout workaround, exposure-time
clustering with absolute/relative tolerance, etc.). None of that exists in the
codebase — zero hits for `script_generator`, `cli_runner`, `folder_structures`,
or any Siril invocation anywhere in `crates/`. It described a feature that was
planned but never built. This rewrite documents only what `crates/athenaeum-core/src/export/`,
`commands/export.rs`, `routes/export.rs`, and `ExportTab.tsx` actually do.

## How it works (data flow)

1. **`collect_export_data`** (`data_collector.rs:21`) takes a `frame_set_id` and:
   - Loads every LIGHT frame reachable through
     `session_members → sessions → imaging_nights → frames_set` (`get_light_frames_for_frame_set`, `data_collector.rs:192`).
   - Groups them by `(filter, camera_type)` into `ExportGroup`s (`build_export_groups`, `data_collector.rs:734`).
     `camera_type` is `Osc` if `BAYERPAT` is a non-empty string, else `Mono`
     (`CameraType::from_bayerpat`, `models.rs:22`).
   - Within each group, further splits frames into `CalibrationSubgroup`s by the
     **combination of calibration-set IDs already linked to each frame**
     (`build_calibration_subgroups`, `data_collector.rs:791`).
   - Builds a `MasterCreationPlan` — a topologically-sorted, informational list of
     which master calibration files *would* need to be created and in what order
     (`build_master_creation_plan`, `data_collector.rs:881`).
2. **Calibration links are read, not computed.** `get_frame_calibration_links`
   (`data_collector.rs:485`) and friends query the `calibration_set_to_frames`
   table directly. Export does **not** call `calibration/configurable_matcher.rs`
   or `calibration/hierarchy.rs` — those run earlier, when the user runs
   calibration matching from the Calibration page, and persist rows into
   `calibration_set_to_frames`. If a frame set has no calibration links yet,
   export will show it with missing-calibration warnings but will not try to
   find or create matches itself.
3. **`organize_files_wbpp`** (`file_organizer.rs:112`) walks the same groups/subgroups
   and copies or symlinks files into the folder tree described below.

## Camera type detection

```
OSC (One-Shot Color): BAYERPAT is a non-empty string (e.g. RGGB, BGGR)
Mono: BAYERPAT is NULL or empty
```
Source: `CameraType::from_bayerpat`, `crates/athenaeum-core/src/export/models.rs:22`.
OSC and Mono frames land in different `ExportGroup`s (different `group_key`,
`models.rs:165`) because they can't be stacked together in PixInsight.

## Folder structure it produces

`organize_files_wbpp` and `build_folder_preview` (used for the UI's folder-tree
preview) both build the same shape — verified they use identical set-ID-keyed
logic (`file_organizer.rs:186-328` vs. `data_collector.rs:1503-1747`):

```
<output_dir>/
└── <frame set name, sanitized>/
    └── camera_<instrume, sanitized>/
        └── BIAS_<set_id>/           # bias frames — omitted if none linked
            └── DARKS_<set_id>/      # dark + darkflat frames — omitted if none linked
                └── FLAT_<set_id>/   # flat frames — omitted if none linked
                    └── lights/      # light frames — always present
```

- The frame-set folder name uses `sanitize_display_folder_name` (`models.rs:242`) —
  keeps spaces/case, replaces `: / \ * ? " < > |` with `_`, collapses repeats.
- The `camera_` folder name uses `sanitize_folder_name` (`models.rs:229`) — lowercases
  and strips everything but alphanumerics (e.g. `"ZWO ASI2600MM Pro"` → `zwoasi2600mmpro`).
- **Missing calibration levels collapse.** If a subgroup has no dark and no
  darkflat linked, there is no `DARKS_*` folder at all — the tree goes straight
  from `BIAS_*` (or `camera_*` if no bias either) to `FLAT_*`/`lights/`
  (`file_organizer.rs:253-288`).
- A flat's *own* dark/darkflat/bias (`flat.dark`, `flat.dark_flat`, `flat.bias` on
  `CalibrationSetInfo`) are folded into the same `BIAS_*`/`DARKS_*` folders as the
  light's own bias/dark, deduplicated by set ID via a shared `HashSet<i64>`
  (`organized_set_ids` in `file_organizer.rs`, `counted_sets` in `data_collector.rs`)
  so a calibration set already placed once isn't copied twice.
- `copy_or_link` skips a file if the destination path already exists
  (`file_organizer.rs:356-359`) — re-running an export into the same output
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
`symlinkUnavailableReason` strings, `src/components/export/ExportTab.tsx:176-185`.

**The Rust side does have a Windows symlink branch** — `copy_or_link`
(`file_organizer.rs:356-379`) has both `#[cfg(unix)]` (`std::os::unix::fs::symlink`)
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

`WbppExportConfig` (`models.rs:701`) has one field, `keyword_order: Vec<String>`,
default `["CAMERA", "BIAS", "DARKS", "FLAT"]`. It's persisted under the
`export.wbpp_config` settings key via `get_wbpp_export_config` /
`set_wbpp_export_config` / `reset_wbpp_export_config`.

**It does not currently change what `organize_files_wbpp` does.** Both consumers
of `WbppExportConfig` take it as `_config` (`file_organizer.rs:116`) or
`_config` (`data_collector.rs:1503`) — the parameter is accepted but unused; the
nesting order (`CAMERA` → `BIAS` → `DARKS` → `FLAT` → `lights`) is hardcoded.
The only place `keywordOrder` is actually read is the frontend's "WBPP Setup
Guide" (`ExportTab.tsx:66-110`), which uses it to render the setup instructions
and the example-structure text shown to the user. There's also no UI to edit
`keyword_order` today — `useWbppConfig`'s `save`/`reset` (`useExportData.ts:212-230`)
are exported but unused; `ExportTab.tsx` only reads the config, it never calls
`save`/`reset`. In practice this means: the setting exists and round-trips
through the DB, but changing it (via a raw API call) would make the on-screen
setup guide describe an order the folder organizer doesn't actually build.

## Master creation plan — informational only

`MasterCreationPlan` / `MasterInfo` (`models.rs:184-224`) are built by
`build_master_creation_plan` (`data_collector.rs:881`): a topological sort over
the unique calibration sets referenced by the export, with a suggested
`output_name` (e.g. `master_flat_38.fit`) and which sub-calibrations to apply
(`apply_bias` / `apply_dark` / `apply_darkflat`). For flats, `apply_dark` is only
set if the dark's average exposure time is within 30% of the flat's
(`FLAT_DARK_EXPOSURE_TOLERANCE = 0.30`, `data_collector.rs:1047`); otherwise the
flat falls back to bias-only, matching the calibration hierarchy's normal
Flat → DarkFlat → Dark(±30%) → Bias fallback chain.

Athenaeum never acts on this plan — no code creates master files. The plan is
exposed through `ExportData.master_plan` (from `get_export_preview` /
`get_calibration_route`) purely as data; the only field the shipped UI currently
renders from it is a count (`masters_to_create` in `CalibrationRouteSummary`,
`commands/export.rs:394`). PixInsight/WBPP creates the actual masters after the
files are organized on disk.

## Project-scoped export (collab)

A second entry point exports a *collaboration project* instead of a single frame
set. It reuses `organize_files_wbpp` untouched but swaps the collector:
`collect_project_export_data` (`project_collector.rs`) gathers the project's
**received contributions ∪ this device's own calibrated outputs** — the frames the
project actually holds on disk, not a catalog frame set — and buckets them by
publisher (Д2).

- **Runner:** `export_project_for_wbpp` (`api/collab_exchange.rs`), wrapped by the
  `export_collab_project` command (Tauri `commands/collab.rs` + Axum
  `routes/collab.rs`, the web mirror validating `output_dir` is within the
  configured export dir like `export_to_wbpp`). UI: an "Export for WBPP" button on
  a project's **Receive** tab opening `ProjectExportDialog.tsx`.
- **Tree shape** — one subtree per publisher under the sanitized project title:

  ```
  <output_dir>/
  └── <project title, sanitized>/
      └── <publisher display, sanitized>/     # one per contributor (own = your display)
          └── camera_<instrume, sanitized>/
              └── lights/                      # calibrated light frames
  ```

  Each publisher subtree is a separate `organize_files_wbpp` call whose dataset
  `frame_set_name` is that publisher's display name.
- **No calibration folders (calibration-off by design).** Project contributions are
  already-calibrated lights, so per-publisher datasets carry no linked calibration
  sets — there are no `BIAS_*` / `DARKS_*` / `FLAT_*` levels, only `lights/`. WBPP
  runs with its own calibration step disabled.
- **Events / cancel:** rides the standard `export-progress` / `export-complete`
  events with the Д3 sentinel `frame_set_id = -1`, and registers its cancel flag
  under that key — so the existing `cancel_export` command
  (`api.invoke('cancel_export', { frameSetId: -1 })`) cancels a running project
  export. Because each publisher is organized in its own pass, the emitted percent
  **restarts per publisher**; the dialog shows a per-publisher counter alongside
  the bar rather than one monotonic total. An empty project surfaces the
  collector's "nothing to export" error inline (no separate pre-flight check).

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
| `export_to_wbpp` | Run the organizer: copy/symlink files into the WBPP tree, emit progress | Yes |
| `cancel_export` | Set the cooperative cancel flag for a running export | Yes (via `useExportProgress`) |
| `get_export_dir` (web only, no Tauri equivalent) | Return the server-configured `ATHENAEUM_EXPORT_DIR`, or null | Yes, web mode only — desktop uses a native folder picker instead |

Progress/completion events (both backends emit the same names — Tauri via
`app_handle.emit`, web via `SseProgressEmitter` over SSE): `export-progress`
(`phase: "collecting" | "copying"`, with `current`/`total`/`percent`/`current_file`)
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
  (`get_export_summary`): cameras/telescopes/date range, per-filter-group
  breakdown (exposure groups from `build_exposure_groups`, `data_collector.rs:1323`
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
| `crates/athenaeum-core/src/export/mod.rs` | Re-exports `data_collector` and `file_organizer` |
| `crates/athenaeum-core/src/export/models.rs` | All export data structures, incl. `WbppExportConfig` |
| `crates/athenaeum-core/src/export/data_collector.rs` | Reads the catalog + existing calibration links, builds `ExportData`/`ExportSummary` |
| `crates/athenaeum-core/src/export/file_organizer.rs` | Builds the folder tree, copies or symlinks files, emits progress |
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

# Frame Sets Documentation

## Overview

Frame sets are collections of LIGHT frames grouped by sky position, with each set holding its frames in a derived hierarchy of imaging nights and sessions. Frame sets are global (not per project). This page is the entry point: the commands, the grouping and night rules, the UI flow and where the code lives.

Code map:

| Concern | Where |
| ---- | ---- |
| Workflows shared by both hosts | `crates/athenaeum-core/src/api/frame_sets.rs` (`auto_generate_frame_sets`, `merge_frame_sets`, `recalculate_frame_set_nights`, `reconcile_frame_set_nights`, `set_frame_set_attestation`) |
| Sky clustering | `crates/athenaeum-core/src/clustering/` |
| Nights and sessions | `crates/athenaeum-core/src/sessions/` (`detect_sessions`, `rederive_for_frame_set`, `reconcile_for_frame_set`) |
| Set metadata | `crates/athenaeum-core/src/frames_set_metadata.rs` |
| Tauri commands | `crates/athenaeum-tauri/src/commands/frame_sets.rs`, registered in `invoke_handler![]` in `crates/athenaeum-tauri/src/lib.rs` |
| Axum mirror | `crates/athenaeum-web/src/routes/frame_sets.rs`, registered in `build_router` |
| Frontend | `src/pages/Objects.tsx` (set list, route `/objects`), `src/pages/FrameSetDetail.tsx` (route `objects/:id`), `src/pages/ExcludedFrames.tsx` (route `/excluded`), `src/components/SelectionDialog.tsx`, `src/components/FrameSetHistoryTab.tsx` |

## Documentation Structure

The four pages below are the original 2025-01 write-up of this subsystem. They have not been re-reviewed against the code in the 2026-10-24 pass that rewrote this page, so where one disagrees with this page or the code, this page and the code win.

- [architecture.md](./architecture.md) - database schema and structural design.
- [algorithms.md](./algorithms.md) - merge, split, metadata and session-detection algorithms.
- [commands.md](./commands.md) - API reference for the frame-set commands.
- [edge_cases.md](./edge_cases.md) - edge cases and boundary conditions.

---

## Quick Start

The frontend never imports Tauri directly; every call goes through `api.invoke` (`src/api/`). Argument casing follows the command: most take camelCase keys (`framesSetId`, `sourceId`); `create_frame_set_from_selection` is declared `rename_all = "snake_case"` and takes `frame_ids`.

### Creating Frame Sets

**Auto-generate from LIGHT frames:**
```typescript
const result = await api.invoke<AutoGenerateResult>('auto_generate_frame_sets', {
  projectId: 1,          // required by the command, ignored (frame sets are global)
  thresholdDeg: null,    // optional override; null = use the persisted grouping threshold
});
// result.sets_created, result.frames_clustered, result.frames_excluded,
// result.frames_already_in_sets, result.exclusion_reasons
```
The result is snake_case on the wire (the struct deliberately has no camelCase rename; `AutoGenerateResult` in `src/types/helpers.ts` mirrors it).

**Create a custom set from a selection of frames:**
```typescript
const setId = await api.invoke<number>('create_frame_set_from_selection', {
  name: 'M31 Core',
  frame_ids: [101, 102, 103],
  description: null,     // accepted, not stored
});
```
The new set is custom (`is_custom = 1`). It needs at least one frame.

### Merging Frame Sets

```typescript
const merged = await api.invoke<FrameSetDetail>('merge_frame_sets', {
  sourceId: 42,  // dragged set: deleted after the merge
  targetId: 43,  // drop target: receives the nights, marked custom
});
```

### Splitting Frame Sets

```typescript
const newSet = await api.invoke<FrameSetDetail>('split_frame_set', {
  sourceSetId: 42,
  selection: { type: 'nights', ids: [10, 11] },   // or 'sessions' / 'frames'
  newName: 'M31 Widefield - Split 1',
});
```
There is no separate pre-check command. `split_frame_set` itself refuses a selection that would leave the source empty (all nights, all sessions or all frames) and a selection that resolves to no frames.

### Other Commands

| Command | Arguments | Notes |
| ---- | ---- | ---- |
| `get_frames_sets` | `projectId` | Every set with `member_count`; the project id is ignored |
| `get_frame_set_detail` | `framesSetId` | Set plus its nights and sessions |
| `rename_frames_set` | `framesSetId`, `newName` | |
| `delete_frames_set` | `framesSetId` | |
| `delete_auto_generated_frame_sets` | none | Deletes every set with `is_custom = 0`; returns the count |
| `mark_frame_set_custom` | `framesSetId` | One-way auto to custom |
| `archive_frame_set` | `framesSetId` | Moves the set to the Archive tab (see `docs/archive/README.md`) |
| `recalculate_frame_set_nights` | `framesSetId` | Force re-derive of nights and sessions; returns the detail |
| `reconcile_frame_set_nights` | `framesSetId` | Cheap, idempotent check run on every detail-page open; rewrites only on drift; returns `{ changed, nights, sessions }`; skips archived sets |
| `set_frame_set_attestation` | `framesSetId`, `attested` | The user's word that the lights are calibrated externally |
| `find_new_frames_for_set` | `framesSetId`, `scanFirst` | Unassigned LIGHT frames within the grouping threshold of the set |
| `auto_merge_new_frames_for_set` | `framesSetId`, `frameIds`, `source` | Attaches candidates and writes a merge-log entry |
| `get_frame_set_merge_log` | see `commands/frame_sets.rs` | Feeds the History tab |
| `get_excluded_frames_count`, `get_excluded_frames_with_metadata`, `remove_files_from_excluded` | | The Excluded Frames page |

## Key Concepts

### Hierarchy

```
frames_set
  |- date_obs_start, date_obs_end
  |- objctra, objctdec (spherical-mean centre)
  |- total_exp_time (sum)
  |
  '-> imaging_nights (1 to N)       frames_set_id, start_time, end_time
       |
       '-> sessions (1 to N)        imaging_night_id, instrume, frame_count, total_exp_time
            |
            '-> session_members     (session_id, frame_id) primary key
                 '-> frames (1)
```

`frames_set` is created with `id`, `name`, `is_custom`, `date_obs_start`, `date_obs_end`, `objctra`, `objctdec`, `total_exp_time`; later migrations in `db/schema.rs` add `flat_pattern`, `avg_rotation`, `min_rotation`, `max_rotation`, `is_archived`, `archived_at`, `archive_operation_id`, `calibrated_externally`, `attested_at` (and sync columns). `imaging_nights`, `sessions` and `session_members` cascade on delete from the set.

### Grouping by sky position (auto-generate)

`auto_generate_frame_sets` takes every LIGHT frame that is not yet a member of any set and clusters it (`clustering::cluster_frames`):

1. Frames without a usable position, or that are not LIGHT, are excluded (not clustered) with a reason; they are stored in `excluded_frames` (cleared and rewritten on each run) and surface on the Excluded Frames page. Numeric `ra`/`dec` are preferred; the sexagesimal `objctra`/`objctdec` are used when the numeric pair is missing or has a zero leg. A position that is all zeros is treated as missing.
2. Frames are sorted by RA, then Dec, then DATE-OBS.
3. Seed-and-grow, single link: the first unassigned frame seeds a cluster; every unassigned frame within the threshold (great-circle distance) of the cluster's running centre joins, the centre is recomputed as the spherical mean after each join, and the scan repeats until no frame is added. The next unassigned frame seeds the next cluster.
4. Each cluster is named by a strict-majority `OBJECT` value, else `Unknown @ RA=..., Dec=...`.

The pass is single-threaded and O(n^2) typical, O(n^3) worst case (documented in the function's header comment, which names a spatial index as the next step if that is ever hit).

The threshold comes from settings `grouping.threshold.value` (default `3.0`) and `grouping.threshold.unit` (default `deg`; `arcsec`, `arcmin`, `deg` accepted). The Objects page passes `thresholdDeg` only when the value typed there differs from the persisted one. Changing the setting does not regroup existing sets. Details: `docs/catalog/README.md` and the Settings registry (`src/settings/registry.ts`).

After a set is created, nights and sessions are detected for it, and a `project-set-match` event is emitted when the set's centre falls inside one of the user's collaboration projects (a suggestion, never an auto-link).

### Nights and sessions are derived

A night is derived data with one definition, the gap rule: `sessions::detect_sessions` sorts the frames that have a `DATE-OBS` by time and starts a new night whenever the gap between consecutive frames exceeds `session_gap_threshold_hours` (default `6.0`; whole hours are used). Within a night, frames are grouped into sessions by `INSTRUME` (`Unknown` when absent).

Nights are never stitched from stored rows by calendar date or time overlap; that stored one night as two rows after a merge. Instead `sessions::rederive_for_frame_set` deletes the set's night rows (sessions and members cascade), runs the gap rule over the whole current membership and writes the rows back. A member with no `DATE-OBS` cannot be placed by the gap rule and is kept on a stable fallback night so a recalculation never loses a frame. `reconcile_frame_set_nights` compares the stored rows with a fresh derivation and writes only on a difference. Rule and history: `docs/catalog/README.md`.

### Merge

`merge_frame_sets` (one transaction): every night of the source is reassigned to the target, the target's nights and sessions are re-derived from the union of both memberships, the target's metadata is refreshed and it is marked custom, and the source set is deleted. A set cannot be merged into itself.

The UI offers this as a mouse-driven drag of one set card onto another on the Objects page, with a confirmation dialog; it also offers suggested merges after an auto-generate run.

### Split

`split_frame_set` moves the selected nights, sessions or frames into a new custom set (nights and sessions of the new set come from `detect_sessions` over the moved frames), deletes the selected rows from the source, refreshes the source's metadata and marks the source custom. In the UI the split dialog on the detail page sends a `frames` selection.

### Metadata Calculation

Set metadata is calculated from member frames (`frames_set_metadata`):

- **date_obs_start / date_obs_end:** earliest and latest frame DATE-OBS
- **objctra, objctdec:** spherical mean of the member coordinates
- **total_exp_time:** sum of exposure times
- rotation: circular mean plus min/max of the member rotations

**Why spherical mean?** Simple averaging fails near the RA wrap: RA 359 and RA 1 average to 180 degrees, while the spherical mean is 0.

### Custom vs auto-generated

`is_custom = 0` marks a set that auto-generate created. Merge, split, `create_frame_set_from_selection` and `mark_frame_set_custom` make a set custom. `delete_auto_generated_frame_sets` (the Objects page's "delete auto-generated sets" action) removes only non-custom sets, so user-shaped sets survive. Auto-generate itself only ever considers frames that are in no set.

## Troubleshooting

### Frame Set Appears Empty

1. Does it have nights? `SELECT * FROM imaging_nights WHERE frames_set_id = ?`
2. Do nights have sessions? `SELECT * FROM sessions WHERE imaging_night_id = ?`
3. Do sessions have members? `SELECT * FROM session_members WHERE session_id = ?`

Try `recalculate_frame_set_nights` first: it rebuilds the night, session and member rows from the set's membership and refreshes the metadata. Rows are removed by cascade when a parent night, session or frame is deleted.

### Coordinates Are Wrong

1. Do frames have coordinates? `SELECT ra, dec, objctra, objctdec FROM frames`
2. Frames with missing or all-zero coordinates are excluded from clustering and listed on the Excluded Frames page.
3. The set centre is the spherical mean of the members, so one mislocated frame pulls it; check with the detail page.

### Nights Look Split or Merged Wrongly

Nights follow `session_gap_threshold_hours` (Settings, General). Run `recalculate_frame_set_nights` after changing it; the reconcile on page open also repairs drift on its own.

### Frames Missing from Every Set

Open the Excluded Frames page. Frames that are not LIGHT, or that lack usable coordinates, are listed with the reason; fix the metadata there and run auto-generate again.

## Contributing

### Adding or Changing an Operation

Follow "Adding a Tauri Command" in the repo `CLAUDE.md`:

1. Logic in `crates/athenaeum-core/src/api/frame_sets.rs` (reusing `clustering/`, `sessions/`, `frames_set_metadata.rs`).
2. Tauri wrapper in `crates/athenaeum-tauri/src/commands/frame_sets.rs` with `#[tracing::instrument(skip_all, err)]`, registered in `invoke_handler![]`.
3. Axum mirror in `crates/athenaeum-web/src/routes/frame_sets.rs`, registered in `build_router`.
4. Frontend call through `api.invoke('command_name', { ... })`; model types through the `ts_export.rs` registry.
5. Update this page.

## License

See repository root for license information.

---

Last reviewed 2026-10-24 against the code

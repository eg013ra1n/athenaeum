# Frame-set send from the Export tab — design

Date: 2026-08-28
Status: approved for planning

## 1. Goal

Let the operator send an object (a frame set) to another Athenaeum node from
the Export tab, in one of four compositions, using the WBPP folder layout the
export already produces:

| Mode | What travels | Ready when | When not ready |
| ---- | ------------ | ---------- | -------------- |
| `lightsOnly` *(new)* | the set's LIGHT frames | always | — |
| `rawWithCalibrationSets` | lights + every linked raw calibration set | always (a missing link is a warning, as today) | — |
| `rawWithMasters` | lights + the master file of every linked set | **every** linked set that has frames is a master | mode disabled — "Build masters first — N sets without a master" |
| `calibratedLights` | the `c_*.fits` artifact of every light | every light has a fresh calibrated output (none stale, none missing) | mode disabled — "Calibrate lights first — N stale, M missing" |

The same four modes and the same readiness rule drive **both** actions on the
tab: *Export to WBPP* (folder) and *Send to node…* (transfer). Owner decision
D1: the tab never starts a master build or a light calibration itself — it
only shows what is ready and lets the operator export or send the ready
material; masters and calibrated lights are prepared beforehand on the
Coverage tab (Create All Masters / Calibrate Lights), and the tab links there.

### Non-goals

- Chaining "build → calibrate → send" behind one button (D1). No
  prepare-then-send job, in the frontend or the backend.
- A download cap, a new wire message, or a new manifest schema. The manifest
  already carries `PayloadKind::{RawFrame, CalibratedLight, Master}`; the
  wire (`Announce3`/`Announce4`) is untouched.
- Registering calibrated lights in the receiver's `files`/`frames`. They are
  artifacts outside the catalog on the sender (light-calibration spec §3) and
  stay that way on the receiver.
- Making the folder-structure preview in the export summary mode-aware. It
  shows the full tree today for every mode (including `calibratedLights`);
  known limitation, §11.

## 2. What exists and is reused

- **Export pipeline** — `export::collect_export_data(conn, fsid)` builds the
  tree (lights per filter/camera subgroup + linked calibration sets, masters
  included); `export::apply_export_mode(conn, &mut data, mode)` rewrites it for
  `rawWithMasters` (drops non-master sets) and `calibratedLights` (swaps each
  light's path for its `light_calibrations.output_path`, drops calibration
  nodes); `export::file_organizer::compute_wbpp_placements(&data)` yields one
  `(frame_id, file_path, filename, rel_dir)` per file with the WBPP directory
  (`camera_<INSTRUME>/BIAS_<id>/DARKS_<id>/FLAT_<id>/lights`).
- **Object send** — `api::sync::enqueue_sync_selection(frame_ids, dest,
  batch_name, frame_set_id)` → `build_selection_package`, which resolves
  catalog files, hashes them (`package::xxh3_full_file`, banks
  `files.strong_hash`), assigns `rel_path` from the WBPP placement map when a
  `frame_set_id` is given, writes the package, and enqueues on the per-peer
  sender engine. Every record today is
  `PayloadKind::RawFrame` with `frame_meta` = the `models::Frame` snapshot.
- **Readiness** — `api::lights::get_export_readiness(set_id, mode, flat_norm,
  flat_norm_mode, params)` tallies `total/calibrated/stale/missing` for the
  set's lights (only meaningful for `calibratedLights` today);
  `get_light_calibration_readiness` already classifies per-frame links as
  `master` / `rawSet` / `missing`.
- **Receiver ingest** — `sync::ingest::ingest_package` → `process_frame` per
  manifest record: hash check, dedup by `frames.uuid` then by full hash via
  `sync_receipts`, `land_payload`, then `insert_ingested_rows` (`files` +
  `fits_header` + `frames` from the snapshot + analysis). `payload_kind` is
  never consulted. No calibration-set integration runs after ingest — the
  scanner's `calibration::scan_integration::create_calibration_sets_from_scan_with_masters`
  only runs over frames inserted *by that scan pass*, and a later scan skips
  already-cataloged paths, so received calibration frames and masters stay
  bare `frames` rows with no `calibration_set` (pre-existing gap, closed here).
- **Calibrated-light adoption** — the scanner recognizes a `c_*.fits` by its
  header (`fits_parser::calibrated_light::calibrated_light_identity`) and
  routes it to `scanner::reconcile_calibrated_light` (known / moved /
  duplicate / adopt; adoption deferred with a `warn!` when the source light is
  not cataloged). Never registered as a frame.
- **Desktop retention** — removed 2026-08-29 (owner ruling: retention is
  Perseus-only; the app never deletes a sent source).

## 3. Sender: composing the package

One new command, `enqueue_frame_set_send`, Tauri + Axum mirror, logic in
`api::sync`:

```
enqueue_frame_set_send(
    frame_set_id: i64,
    mode: ExportMode,
    destination_device_id: String,
    batch_name: Option<String>,
    flat_norm: Option<bool>,            // readiness prefs, same three the
    flat_norm_mode: Option<FlatNormMode>, // export command already takes
    params: Option<LightCalParams>,
) -> EnqueueSelectionResult
```

Steps, in order — nothing is written before step 3 passes:

1. **Collect** — `collect_export_data(conn, frame_set_id)`.
2. **Apply the mode** — `apply_export_mode(conn, &mut data, mode)`, with the
   new `ExportMode::LightsOnly` arm: clear `flat`/`dark`/`bias` on every
   subgroup, leave light paths untouched (the `calibratedLights` arm minus the
   path swap).
3. **Gate** — compute readiness (§5) and run the shared `check_mode_ready`
   (§5); a not-ready mode returns `ApiError::Invalid` with the same sentence
   the UI shows, and no package directory is created. The mode transforms are
   made strict as a backstop: `apply_raw_with_masters` now `bail!`s on a raw
   set with frames instead of dropping it with a warning (mirrors
   `apply_calibrated_lights`, which already bails on a missing artifact).
4. **Place** — `compute_wbpp_placements(&data)`; for `calibratedLights` the
   placements already carry the `c_*.fits` path and filename because step 2
   swapped them in the tree.
5. **Build** — `build_selection_package` is generalized to take payload
   entries instead of frame ids:

   ```rust
   struct PayloadEntry {
       frame_id: i64,          // the catalog frame this file is / derives from
       source_path: PathBuf,   // file to copy into the package
       rel_path: String,       // WBPP dir + filename, forward slashes
       kind: PayloadKind,      // RawFrame | Master | CalibratedLight
   }
   ```

   `enqueue_sync_selection` (the frame-table "Send to…" button) is unchanged
   in behavior: it maps its `frame_ids` to `RawFrame` entries with the
   existing layout logic (WBPP map or source-relative) and calls the same
   builder. Per entry the builder does what it does today — existence/stat
   checks feeding `ineligible`, full hash, `strong_hash` banking under
   `disk_matches_row` (catalog file only), manifest record, per-directory
   filename dedup — with these kind-specific rules:
   - **`RawFrame`** (lights, raw calibration frames): unchanged. `frame_meta` =
     the frame's snapshot; `frame_uuid` = `frames.uuid`.
   - **`Master`**: same as `RawFrame` for the receiver's purposes (a master is a
     catalog frame); the kind is an honest label.
   - **`CalibratedLight`**: `frame_meta` = the **source light's** snapshot
     (`frames` row of `frame_id`), `frame_uuid` = a fresh v4 uuid (identity on
     the receiver comes from the file's own header, §4), `analysis` = `None`,
     no `strong_hash` banking (not a catalog file).
6. **Enqueue** — `engine.enqueue_package(dir, display_name, files,
   PackageLayout::Batch)` exactly as `enqueue_sync_selection` does; the batch
   auto-name falls back to the frame-set name. The frontend fans the command
   out per destination (`useSyncSend`), one package per node.

Result: the existing `EnqueueSelectionResult { enqueued_count, eligible_count,
total_count, ineligible }`, counted over payload entries (files), not lights.

## 4. Receiver

Landing location is unchanged: `<incoming>/<sender_slug>/<batch_slug>/<rel_path>`
(`PackageLayout::Batch`), so the batch folder is WBPP-ready as received.

### 4.1 Per record — `process_frame` branches on `payload_kind`

- **`RawFrame` / `Master` / `Other`, and every record from a sender older than this change**:
  the existing path, byte-for-byte — uuid dedup → full-hash dedup →
  `land_payload` → `insert_ingested_rows` → receipt + history. A re-sent
  master is a `Duplicate` by uuid; nothing is overwritten.
- **`CalibratedLight`** — new function `process_calibrated_light`, ~40 lines
  around existing helpers:
  1. dedup by full hash against `sync_receipts` alone (`xxh3` + outcome
     `Ingested`; the existing `full_hash_already_ingested` joins `frames`,
     which this record never has) → `Duplicate` receipt, no second file;
  2. `land_payload` into its WBPP directory (collision → `unique_path`
     `_2`, never overwrite);
  3. **no** `files` / `frames` / `fits_header` rows;
  4. read the landed file's header → `parse_stored_header_keys` →
     `calibrated_light_identity` → `scanner::reconcile_calibrated_light`
     (visibility `pub(crate)`; `root_id` = the `sync_incoming` scan root's id
     when designated, else `0` — it is a log field only). Outcome: source
     light cataloged on the receiver → a `light_calibrations` row (the light
     reads *calibrated* there too); not cataloged → file on disk only, `info!`
     "calibrated light source not in catalog — deferred". A later scan of the
     incoming root re-runs the same adopt because the file is not in `files`
     (idempotent by construction). A file that fails
     `calibrated_light_identity` (not actually a calibrated light) is
     `Rejected("payload is not a calibrated light")`, logged at `error!`, the
     landed file removed — a sender bug must not leave an untracked file;
  5. `Ingested` receipt + `sync_history` row, so the Transfers UI counts it.

  A re-sent artifact whose identity is already tracked at another path is
  dropped as a duplicate — the receiver's existing artifact wins; re-sending a
  re-calibrated light does not replace it.

### 4.2 Per package — calibration-set integration

After the record loop, once per package, under one connection acquisition:
collect the `frames.id`s inserted in this ingest (returned by
`insert_ingested_rows`) bucketed by the snapshot's `imagetyp` — flat / dark /
bias / darkflat and master-dark / master-flat / master-bias / master-darkflat
(`ImageType::is_master`) — and call
`create_calibration_sets_from_scan_with_masters(conn, flats, darks, bias,
darkflats, MasterFrameIds{…})`. Only frames whose verdict was `ingested`
count (duplicates and rejects have no new row). Empty buckets → no call.

Effect: raw calibration frames cluster into `calibration_set`s by parameters,
each master becomes one `is_master_library = 1` set — as if the receiver had
scanned them. A received master has no `master_provenance` row, so it is an
*imported* master on the receiver (Rebuild unavailable), the same as a foreign
master dropped into the library by hand.

Failure → `error!` with `package_id` + the bucket counts, one entry in the
batch's `sync_events` journal (where per-batch noise lives, never the status
string — Transfers Batch Model §journal), and the package still finishes —
the files and frame rows are already durable. This also closes the
pre-existing gap for browser-selection sends that included calibration frames.

### 4.3 Old receivers

A receiver older than this change ignores `payload_kind` and ingests a
`CalibratedLight` record as a raw frame (a `frames` row with `CALSTAT` in its
header, entering clustering and duplicates). Stance as for `Announce4`
(mirror hierarchy): **upgrade the receiver**; no wire-level guard. The send
dialog does not know peer versions and does not try to.

## 5. Readiness and the gate

`get_export_readiness(set_id, flat_norm, flat_norm_mode, params)` loses its
`mode` argument and returns everything the tab needs in one call:

```rust
pub struct ExportReadiness {
    // lights (unchanged fields)
    pub total: i64,
    pub calibrated: i64,
    pub stale: i64,      // Stale + Partial, as today
    pub missing: i64,
    // linked calibration sets that have frames but are not masters
    pub raw_sets_without_master: i64,
    pub raw_set_ids_without_master: Vec<i64>,   // for the → Coverage link
    // files each mode would place, = placements.len() after apply_export_mode
    pub file_counts: ExportFileCounts { lights_only, raw_with_calibration_sets,
                                        raw_with_masters, calibrated_lights },
}
```

`raw_sets_without_master` walks the collected tree the same way
`filter_masters_recursive` does (subgroup flat/dark/bias and their sub-cals),
counting distinct set ids with `frames` non-empty and `is_master_library = 0`.
The ids come back in **ascending** order, not tree-walk order: the tab's
`→ Coverage` link deep-links to `[0]`, and accumulation order depends on which
subgroup the walk reaches first.
`file_counts` come from a count-only walk of the collected tree that never
bails, so a not-ready mode still shows a number: `lights_only` = lights;
`raw_with_calibration_sets` = lights + every calibration frame, each set
counted once (the same cross-subgroup dedup `compute_wbpp_placements`
applies); `raw_with_masters` = lights + one file per master set;
`calibrated_lights` = lights (one artifact per light). Informational only —
the gate is `check_mode_ready`.

One shared function is the gate for export and send:

```rust
pub fn check_mode_ready(r: &ExportReadiness, mode: ExportMode) -> Result<(), String>
// calibratedLights: stale + missing == 0
//   else "N of T lights lack a fresh calibrated output — run Calibrate Lights first"
// rawWithMasters:   raw_sets_without_master == 0
//   else "N calibration sets have no master — build masters first"
// lightsOnly / rawWithCalibrationSets: Ok
```

`export_to_wbpp` (both hosts) calls it for every mode (today it gates only
`calibratedLights`); `enqueue_frame_set_send` calls it before writing anything.

## 6. UI — Export tab

```
┌ Export Mode ──────────────────────────────────────────────────┐
│ ○ Lights only                                   60 files      │
│ ● Lights + calibration sets                     84 files      │
│ ○ Lights + masters                              63 files      │
│     ⚠ Build masters first — 2 sets without a master   → Coverage │
│ ○ Calibrated lights                             60 files      │
│     ⚠ Calibrate lights first — 3 stale, 5 missing     → Coverage │
└───────────────────────────────────────────────────────────────┘
  … warnings, summary, Export Options (output dir, symlinks) — unchanged …

  [ ▶ Export to WBPP ]        [ ✈ Send to node… ]
```

- Four radios in the order above; the hint under each is the existing one,
  plus the new "Lights only — raw light frames, no calibration frames".
- Readiness is fetched once when the tab mounts and re-fetched on the
  existing `light-cal-updated` window event and on `master-build-complete`,
  so returning from the Coverage tab shows the new state without a reload.
- A not-ready mode's radio is `disabled`; under it the `check_mode_ready`
  sentence and a `→ Coverage` link (the existing `?tab=calibration`
  navigation; with `raw_set_ids_without_master` non-empty the link highlights
  the first set via `highlightSet=`).
- If the persisted mode (`athenaeum.export.mode`) is not ready it stays
  selected; both action buttons are disabled with the sentence as tooltip
  (the current `calibratedLights` behavior, extended to `rawWithMasters`).
  `readExportModePref` accepts `lightsOnly`.
- **Export to WBPP** — unchanged apart from the stricter gate.
- **Send to node…** — enabled by `check_mode_ready` alone (no output dir, no
  symlink option). Opens `SendToNodeDialog` in a new *frame-set* variant:

  ```ts
  type SendToNodeTarget =
    | { kind: 'frames'; frameIds: number[] }                       // existing
    | { kind: 'frameSet'; frameSetId: number; mode: ExportMode; fileCount: number };
  ```

  Header "Send 63 files — Lights + masters"; transfer-name field pre-filled
  with the frame-set name; device list, per-node fan-out and the aggregated
  outcome notification are the existing ones. `useSyncSend` gains
  `sendFrameSet(frameSetId, mode, deviceIds, opts)` calling
  `enqueue_frame_set_send` per device with the same readiness prefs the tab
  used (`readFlatNormPref` etc.), so the backend gate agrees with the UI.
- The frame-table "Send to…" (selected frames) is untouched.

## 7. Data and compatibility

- **No schema change.** No new table or column; `light_calibrations`,
  `sync_receipts`, `sync_history` are used as they are.
- **Manifest**: `PayloadKind::Master` and `CalibratedLight` are emitted for
  the first time by personal sync (both variants have existed since manifest
  v1; `Other` remains the forward-compatible catch-all). Record shape
  unchanged.
- **Wire**: untouched — `Announce3`/`Announce4`, Offer/Want dedup, receipts.
  The Offer's sampling hash of a `c_*.fits` never matches a receiver `files`
  row, so calibrated lights are always wanted; re-sends dedup at ingest by
  receipt (§4.1).
- **Retention**: the app writes no source linkage and never deletes a sent
  source; retention is a Perseus-only concern.
- **TS types**: `ExportMode` (new variant), `ExportReadiness` (new fields),
  `ExportFileCounts` (new) regenerate from the `ts_export.rs` registry;
  `src/types/models.ts` / `src/types/export.ts` updated in the same change.

## 8. Error handling

Every failure is logged before it is returned (project rule).

| Where | Condition | Behavior |
| ----- | --------- | -------- |
| sender, gate | mode not ready | `Err` with the `check_mode_ready` sentence; no package dir created; `warn!` with `frame_set_id`, `mode` |
| sender, build | a placed file missing/unreadable on disk | `ineligible` entry with the reason (existing shape); the rest ships; notification says `(N of M)` |
| sender, build | a `calibratedLights` artifact path vanished between gate and build | same as above — `ineligible: "file missing on disk"`; the readiness the UI showed was true at gate time |
| receiver, calibrated light | adopt fails (I/O, DB) | `Rejected` receipt, `error!`, landed file removed |
| receiver, calibrated light | source light not cataloged | `Ingested` receipt, file kept, `info!` deferred |
| receiver, package | calibration-set integration fails | `error!` + a `sync_events` journal entry; package finishes |

## 9. Testing

Core (`cargo test -p athenaeum-core`), fixtures = the existing synthetic FITS
writers in `export::data_collector` tests and `sync::ingest_tests`.

Sender:
- `apply_export_mode(LightsOnly)` clears every calibration node and leaves
  light paths untouched; `compute_wbpp_placements` then yields
  `camera_<x>/lights` for every light.
- `apply_raw_with_masters` bails when a raw set with frames remains; the two
  existing tests that asserted omission warnings are updated to assert the
  error.
- `ExportReadiness`: `raw_sets_without_master` counts distinct raw sets with
  frames only (a master set, an empty set, a dangling link count 0);
  `file_counts.<mode>` equals `placements.len()` for that mode.
- `check_mode_ready` truth table (four modes × ready / not ready).
- Package build from payload entries, per mode: `rel_path`s are the WBPP
  directories; `payload_kind` per file is `RawFrame` for lights and raw
  calibration, `Master` for master files, `CalibratedLight` for `c_*.fits`; a
  `CalibratedLight` record's `frame_meta` is the source light's snapshot.
- `enqueue_sync_selection` regression: the existing selection tests pass
  unchanged through the generalized builder.

Receiver:
- `CalibratedLight` record: file landed at `rel_path`; `files`/`frames`
  count unchanged; with the source light pre-seeded (matching `ATH_CSRC`
  uuid) a `light_calibrations` row exists with `output_path` = landed path;
  without it no row, receipt `Ingested`, `info!` line present (log-asserting
  pattern from `docs/logging/README.md`).
- Re-sending the same `CalibratedLight` → `Duplicate` receipt, one file on
  disk.
- A `CalibratedLight` record whose payload lacks the header cards →
  `Rejected`, no file left behind.
- Post-ingest integration: a package of raw flats + one master dark → one
  flat `calibration_set` with the flats as members, one dark set with
  `is_master_library = 1`; the master's `files`/`frames`/`calibration_set`
  rows column-diff equal to a scanner ingestion of the same file (the
  `direct_registration_matches_scanner_ingestion` technique).
- Existing `RawFrame` ingest tests pass without modification.

Frontend: `npx tsc --noEmit`; the readiness/radio rendering is covered by
the two-instance real-data smoke recorded in `docs/open-items.md` (all four
modes; on the receiver: batch folder opens in WBPP as-is, the master shows
on Equipment as imported, a calibrated light shows *calibrated* when its
source is cataloged).

## 10. Key files (expected)

- `crates/athenaeum-core/src/export/models.rs` — `ExportMode::LightsOnly`.
- `crates/athenaeum-core/src/export/data_collector.rs` — `LightsOnly` arm;
  strict `apply_raw_with_masters`.
- `crates/athenaeum-core/src/api/lights.rs` — `ExportReadiness` fields,
  `ExportFileCounts`, `check_mode_ready`, mode-less `get_export_readiness`.
- `crates/athenaeum-core/src/api/sync.rs` — `PayloadEntry`, generalized
  `build_selection_package`, `enqueue_frame_set_send`; `enqueue_sync_selection`
  re-expressed over entries.
- `crates/athenaeum-core/src/sync/ingest.rs` — `payload_kind` branch,
  `process_calibrated_light`, per-package calibration integration.
- `crates/athenaeum-core/src/scanner/mod.rs` — `reconcile_calibrated_light`
  → `pub(crate)`.
- `crates/athenaeum-tauri/src/commands/{export,sync}.rs`,
  `crates/athenaeum-web/src/routes/{export,sync}.rs`, `lib.rs` /
  `routes/mod.rs` registration — `enqueue_frame_set_send`, readiness
  signature, stricter export gate.
- `crates/athenaeum-core/src/ts_export.rs` — `ExportFileCounts`.
- `src/components/export/ExportTab.tsx`, `src/components/transfers/SendToNodeDialog.tsx`,
  `src/hooks/useSyncSend.ts`, `src/types/{models,export}.ts`.
- `docs/open-items.md` — smoke list; `CLAUDE.md` — Transfers section note on
  frame-set sends and receiver-side calibration integration.

## 11. Decisions and known limitations

- **D1** — no "offer to build/calibrate" from the Export tab; ready-only, the
  operator prepares material on Coverage (owner, 2026-08-28).
- **D2** — `rawWithMasters` is strict for export as well as send: what the
  summary shows is what lands. Replaces the omit-with-warning behavior
  (owner accepted the consistency argument, 2026-08-28).
- **D3** — receiver integrates received calibration frames and masters into
  `calibration_set`s by reusing the scanner's integration tail at ingest, not
  by relying on a later scan (a scan skips cataloged paths and only integrates
  its own pass's inserts) (owner, 2026-08-28).
- **D4** — calibrated lights are never cataloged on the receiver; adoption
  into `light_calibrations` is best-effort and deferred when the source light
  is absent (same contract as the scanner).
- **D5** — the app writes no source linkage and never deletes a sent source;
  retention is a Perseus-only concern (owner ruling, 2026-08-29 — supersedes
  the per-entry `reclaimable` opt-in this section originally specified).
- **Limitation** — the export summary's folder preview is not mode-aware.
- **Limitation** — a receiver older than this change ingests `CalibratedLight` records as
  raw frames; documented "upgrade the receiver" stance, no wire guard.
- **Limitation** — a received master is *imported* on the receiver (no
  provenance, no Rebuild); provenance transfer is out of scope.

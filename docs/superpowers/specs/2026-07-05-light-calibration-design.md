# B5 In-App Light Calibration — Design — 2026-07-05

Roadmap item B5 ("calibration stage 2", core deliverable): apply master
darks/biases and flats to LIGHT frames, producing calibrated f32 FITS that
WBPP/Siril consume with their own calibration step disabled. Builds directly
on Phase 2 (master library, `integration/` engine, ComputeQueue) and the
calibration math research (`../research/2026-07-04-calibration-math-research.md`).

Decisions in this spec were settled with the owner on 2026-07-05.

## 1. Scope and user model

- Standalone stage, triggered per frame set: a **Calibrate Lights** button on
  the frame-set toolbar opens a readiness dialog, the operation runs in the
  background, and the WBPP export later just references the produced files.
  Calibration is NOT performed inside the export flow.
- Unit of work: the LIGHT frames of one frame set. Scope selector in the
  dialog: **all lights** or **only uncalibrated + stale**.
- Re-calibration is a first-class operation: re-running overwrites outputs
  in place (tmp + atomic rename) and refreshes the tracking row.

Out of scope for v1: WBPP-export integration (export referencing calibrated
files — follow-up ticket), cosmetic correction / hot-pixel maps, dark
scaling/optimization (harmful on modern CMOS, same stance as Phase 2),
output pedestal, XISF output.

## 2. Math

Per the research report (all HIGH-confidence findings):

```
L_c = (L − MasterDark) / F_norm        F_norm = MasterFlat / ATH_FNRM
```

- **Raw-master-dark convention** (Phase 2 invariant): `(L − D)` removes bias
  and dark in one subtraction; no separate bias term when a dark is applied.
- Master flats are stored illumination-only, normalized to the central-third
  mean stamped as `ATH_FNRM`. Imported masters missing the card get the
  constant recomputed on the fly (same helper Phase 2 uses).
- **Flat normalization is switchable per run** (owner request): a dialog
  toggle "Normalize master flat", default ON. OFF → `F_norm = MasterFlat`
  as stored (plain division, output scale changes by ~`1/mean(F)`). The
  divisor actually applied is recorded as `ATH_CFNM` (the `ATH_FNRM` value,
  or `1.0` when disabled) and as `flat_norm_applied` in the tracking row;
  a frame calibrated with the other setting counts as *stale* for a run
  started with the current one. Note ON is scale-invariant math
  (`F/mean(F)` has mean ≈ 1 for any input scale), so it is safe for
  imported already-normalized flats too.
- **Fallbacks (owner policy: best-effort, honestly labeled):**
  - dark + flat → `CALSTAT='BDF'`
  - dark only → `(L − D)`, `CALSTAT='BD'`
  - no dark, bias linked → `(L − B) / F_norm`, `CALSTAT='BF'` (`'B'` when no flat either)
  - flat only → `L / F_norm`, `CALSTAT='F'`
  - nothing linked → frame is reported "not calibrated", no output written.
- Output: 32-bit float, **negatives preserved** (no clamping — f32 carries
  them losslessly; PI/Siril tolerate negative floats), **no pedestal** in v1.
- Numeric scale: normalized to [0,1] by dividing by the source bit-depth
  maximum (65535 for 16-bit sources), recorded as `ATH_CSCL`. Verifying that
  WBPP consumes this correctly with calibration disabled is an acceptance
  test (research open question #2); the divisor lives in one constant.
- OSC/CFA: calibrated **un-debayered** (CFA mosaic preserved), `BAYERPAT` /
  `XBAYROFF`/`YBAYROFF` copied through. Global flat normalization (the single
  `ATH_FNRM`) preserves channel ratios; per-CFA-plane normalization is
  explicitly not done (research open question #3 — global matches PI default
  behavior for our purposes).

## 3. Output location and layout

```
<CalibrationLibraryRoot>/<OBJECT sanitized>/<INSTRUME sanitized>/<DATE-OBS date>/c_<original filename>.fits
```

- Same sanitizer as the master path layout (`calibration_library/paths.rs`);
  name collisions get `_2`, `_3`… suffixes.
- Calibrated lights are **never registered in `files`/`frames`**. They are
  artifacts tracked in `light_calibrations` (§5). This keeps clustering,
  duplicates, sessions, and the matcher untouched.

## 4. Scanner behavior: recognize, repair, adopt

Calibrated lights are self-describing via header cards (§7). The scanner
already parses every header; on seeing `CALSTAT` + `ATH_CSRC` it takes the
**reconcile-adopt** path instead of frame registration:

1. **Known path** — a `light_calibrations` row has `output_path` equal to the
   scanned path → no-op.
2. **Moved file** — a row matches the file's identity (source uuid, §7) but
   its `output_path` no longer exists on disk → UPDATE `output_path` to the
   new location (same philosophy as file relinking). `info!` log.
3. **Duplicate copy** — identity matches a row whose `output_path` ALSO still
   exists → user-visible duplicate signal: the pair (kept path, duplicate
   path) is added to the scan result's `calibrated_duplicates` list, surfaced
   in the scan-finished notification (warning tone, count + first paths in
   detail), and logged `warn!` with both paths. The tracking row is not
   changed.

   Relationship to the existing duplicates feature: the FileManager →
   Duplicates screen (`DuplicatesView`, xxHash groups from `duplicates/`)
   operates on **cataloged `files` rows**. Calibrated lights are artifacts
   outside the catalog (§3), so they can never appear there — the
   scan-time signal above is deliberately the sole detection point, not an
   oversight to be "fixed" by registering the files. The notification
   panel's persistent history keeps the signal reviewable after the toast
   expires. If practice shows copies accumulate faster than the
   notification handles, the follow-up is a calibrated-artifacts section
   inside `DuplicatesView` fed by persisted sightings — explicitly out of
   scope for v1.
4. **Unknown file (broken/rebuilt DB)** — no row matches → **adopt**: resolve
   the source frame by `ATH_CSRC` uuid, falling back to `ATH_CSRN` (source
   filename, `frames.filename` is indexed) since uuids are DB-generated and
   do not survive a catalog rebuild. Resolved → INSERT a tracking row
   (calstat from header; master references resolved by uuid/path where
   possible, else NULL), `info!` "adopted calibrated light". Source not in
   the catalog yet → `warn!` and skip; scans are idempotent, so adoption
   succeeds on a later scan once the originals are cataloged.

In every branch the file is excluded from normal light ingestion.

## 5. Database

New table `light_calibrations`:

| column | type | notes |
| ------ | ---- | ----- |
| `id` | INTEGER PK | |
| `frame_id` | INTEGER NULL UNIQUE, FK→frames ON DELETE CASCADE | NULL only for adopted rows whose source is not cataloged yet; backfilled on a later scan |
| `source_uuid` | TEXT | identity anchor for adoption/repair |
| `source_filename` | TEXT | fallback identity key |
| `output_path` | TEXT NOT NULL UNIQUE | |
| `dark_set_id` / `flat_set_id` / `bias_set_id` | INTEGER NULL, FK→calibration_set (no action) | what was actually applied |
| `calstat` | TEXT NOT NULL | honest applied-state flags |
| `flat_norm_applied` | INTEGER NOT NULL | 1 = divided by `ATH_FNRM`, 0 = plain flat division |
| `output_hash` | TEXT NOT NULL | xxh3 of the written file |
| `engine_version` | INTEGER NOT NULL | bump on math changes → everything becomes stale |
| `created_at` | TEXT NOT NULL | |

Frame status is **derived**, not stored:

- no row → *not calibrated*
- row's `*_set_id`s differ from the frame's current matcher links, or a
  referenced master's `master_provenance.created_at` is newer than
  `created_at`, or `engine_version` is older → *stale*
- `calstat` lacks a type the frame now has a link for → *partial* (a special
  case of stale; the readiness dialog offers re-calibration)
- otherwise → *calibrated*

## 6. Orchestration and engine

Composition of existing queues (approach B, owner-approved):

1. `start_light_calibration(set_id, scope)` runs a **preflight**: read each
   light's Dark/Flat/Bias links (`calibration_set_to_frames`); links that
   point at raw (non-master, non-superseded) sets are grouped and submitted
   as ordinary master-build jobs via the existing batch machinery
   (dependency-ordered, `start_master_builds_batch` path). The
   `LightCalibration` job is submitted immediately after.
2. The light job's worker thread then runs an explicit
   **wait-for-preflight-builds handshake**: it blocks until every preflight
   build has completed and dropped its `active_master_builds` handle, and this
   wait runs **before** the job acquires its compute slot (at
   `compute.max_concurrent = 1` a still-running build holds the only slot, so
   waiting after admission would deadlock). This handshake — **not** FIFO queue
   admission — is what guarantees the masters are built before the light job
   runs, at any `max_concurrent`. Independently, the job re-resolves every
   light's links **at execution time** — Phase 2's supersede repoints links
   onto the master automatically when a build lands — so it always calibrates
   with whatever is ready, labeling honestly (§2 fallbacks); a preflight build
   that was skipped/failed is `warn!`-logged and its lights calibrate
   best-effort.
3. The job itself follows the master-build execution pattern: runs on the
   caller's `spawn_blocking` thread, `ComputeQueue::acquire(LightCalibration)`
   for admission, cooperative cancellation between frames.

Per light frame:

1. Re-resolve links; pick the master file paths (dark, flat, bias).
2. Open `BandSource` over `[light, dark?, flat?/bias?]` — geometry must
   match (dimensions); mismatch = per-frame error, batch continues.
3. Stream bands: apply §2 formula per pixel, accumulate nothing (O(band)
   memory, same budget policy as integration).
4. Write via `write_fits_f32` to a temp name, fsync, atomic rename.
5. UPSERT the `light_calibrations` row; emit `calibration-progress`
   (frame index / total / filename).

Batch end: `calibration-finished` event `{ set_id, outcome, ok_count,
failed: [{frame_id, reason}] }`; per-frame errors never abort the batch. A
failed master build does not abort either — affected lights calibrate
best-effort per policy.

## 7. Output headers

Copied from the source light: WCS, optics, `DATE-OBS`, session cards,
`BAYERPAT`/`XBAYROFF`/`YBAYROFF`. Added:

| card | value |
| ---- | ----- |
| `CALSTAT` | applied-state flags (`'BDF'`, `'BD'`, `'BF'`, `'F'`, …) — MaxIm interop convention |
| `ATH_CSRC` | uuid of the source frame |
| `ATH_CSRN` | source filename (adoption fallback key) |
| `ATH_CDRK` / `ATH_CFLT` / `ATH_CBIA` | uuid + path of each master actually applied |
| `ATH_CSCL` | numeric-scale divisor (e.g. 65535.0) |
| `ATH_CFNM` | flat-normalization divisor actually applied (`ATH_FNRM` value, or 1.0 when normalization is off) |
| `ATH_CVER` | engine version |

All `ATH_*` names respect the 8-char FITS keyword limit and extend the
Phase 1 vocabulary module.

## 8. Commands and UI

Commands (Tauri + Axum mirrors, thin wrappers over `api::lights`):

- `get_light_calibration_readiness(set_id)` → per-type summary + per-frame
  status (drives the dialog and the frame-table badges).
- `start_light_calibration(set_id, scope)` → operation id (runs preflight,
  submits builds + job).
- `cancel_light_calibration(operation_id)`.

UI:

- **Calibrate Lights** button on the frame-set toolbar → dialog (patterned
  after `CreateMasterDialog`): readiness summary — N lights fully ready, M
  linked to raw sets ("masters will be built automatically", listed), K with
  missing links (which type is missing); scope selector; "Normalize master
  flat" toggle (default ON, last choice remembered); start.
- Progress via the existing sidebar ComputeQueue indicator; completion via
  `notify()` with a new `calibration` NotificationKind (union + icon map).
- Frame table: status badge per light (calibrated / partial / stale / —)
  with an applied-masters tooltip.

## 9. Error handling

- Per-frame failures (geometry mismatch, read/write errors) collect into the
  finish summary; `error!` at the command boundary, never swallowed.
- Preflight/master-build failures degrade to best-effort calibration with
  honest labeling; the notification lists what fell back and why.
- Duplicate calibrated copies: user-visible via the scan-finished
  notification (§4.3).

## 10. Testing

- Unit (synthetic frames, known values): formula correctness incl. every
  fallback branch (dark=100 flat-gradient fixtures → exact f32 expectations);
  staleness derivation (link change, master rebuild, engine bump); path
  layout + collision suffixes; scanner branch matrix (known / moved /
  duplicate / adopt / defer).
- Integration: real FITS from the owner's archive end-to-end — build masters
  → calibrate → verify headers + values (real-data-first rule).
- Acceptance: WBPP consumes a calibrated set with its calibration step
  disabled; verifies the [0,1] scale decision (research open question #2).

## 11. Key files (expected)

- `crates/athenaeum-core/src/calibration_library/light_cal.rs` — engine
  (band streaming, formula, fallbacks, header assembly).
- `crates/athenaeum-core/src/api/lights.rs` — readiness / start / cancel
  handlers, preflight, job submission.
- `crates/athenaeum-core/src/db/light_calibrations.rs` — table + status
  derivation queries.
- `crates/athenaeum-core/src/scanner/…` — reconcile-adopt branch.
- `src/components/calibration/CalibrateLightsDialog.tsx`, frame-table badge,
  `calibration` notification kind.
- Mirrors: `commands/…` + `routes/…` per the two-backend rule.

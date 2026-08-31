# Calibrated-Lights Export v2 — Design — 2026-08-31

Calibration becomes a stage of the WBPP export instead of a standalone
operation: choosing the **Calibrated lights** export mode calibrates every
LIGHT frame on the fly from its linked masters — with hot-pixel cosmetic
correction and (for OSC) VNG debayering — and writes the results straight into
the export destination, `<dest>/<frame-set name>/camera_<x>/lights/`. The
standalone **Calibrate Lights** flow (dialog, commands, `light_calibrations`
table, artifact store under the Calibration Library root) is removed.

Supersedes the orchestration/tracking/output-layout sections of
`2026-07-05-light-calibration-design.md` (B5). B5's **engine** — math (§2),
CFA flat handling, header builder (§7), BITPIX-aware scaling — carries over
unchanged and stays the compute core.

Decisions in this spec were settled with the owner on 2026-08-31.

## 1. Background: verification against WBPP

Pixel-level comparison against a WBPP reference run (LDN 1272, OSC
ZWO ASI2600MC Duo + mono ATR2600M, `~/Pictures/LDN1272`, log
`20260830105156.log`) established:

- **The math is identical.** Feeding WBPP's own masters and per-channel flat
  factors through Athenaeum's formula reproduces WBPP's calibrated output to
  float32 rounding (median |diff| ≈ 1e-9) on 99.7 % of pixels, both mono and
  OSC. WBPP uses the same raw-master-dark convention (`masterBiasEnabled =
  false`, dark retains bias).
- **Masters are equivalent.** Master-dark medians agree to 5e-6 DN; flats
  correlate at 0.999995.
- **The one real gap is cosmetic correction.** WBPP runs
  `cosmeticCorrectionHigh = true` (σ = 10, kernel radius 1): ~0.24 % of pixels
  per frame are hot-pixel replacements, 72 % directly over the master-dark hot
  map. Athenaeum has no equivalent. This spec adds it (§5).
- Benign differences: flat-scale estimator convention (WBPP 5 %-trimmed global
  mean vs our central-third mean — a constant per-channel factor ×1.02–1.03,
  absorbed by integration normalization) and negatives (we preserve, WBPP
  clamps to 0). Both stay as they are.

WBPP's debayer stage is `Debayer.VNG`, output 3-channel float, suffix `_d`.
The reference `debayered/` folder is the validation oracle for §6.

## 2. Scope and user model

- **One entry point: the Export tab.** Mode **Calibrated lights** now means
  "calibrate (and debayer) at export time from linked masters". No
  pre-existing artifacts are required or consulted. The Coverage tab loses the
  Calibrate Lights button; the per-frame calibration badge disappears.
- **Options** (shown when the mode is selected; persisted like the mode):
  - Flat normalization toggle + mode (moved from the old dialog, defaults
    unchanged: ON, CentralThird, per-CFA-channel for colour lights).
  - Advanced `LightCalParams` (output pedestal), unchanged.
  - **Hot-pixel correction** toggle, default ON (§5).
  - **Debayer OSC lights (VNG)** toggle, default ON (§6). OFF exports CFA
    `c_*.fits` for stackers that debayer themselves.
- **Every export regenerates.** No cache, no skip-if-exists for generated
  files — outputs are overwritten in place (tmp + atomic rename). The
  copy-path's exists-skip applies only to copied files in the other modes.
- **Destination layout** is the export's existing one:
  `<dest>/<sanitized frame-set name>/camera_<x>/lights/…` — this replaces the
  old `<LibraryRoot>/<OBJECT>/<INSTRUME>/<date>/` store and fixes the
  wrong-OBJECT scattering by construction. Masters keep their library layout;
  nothing else under the Calibration Library root changes.
- Old `c_*` trees under the library root are not migrated or deleted — the
  owner cleans them up manually (they are not cataloged).

## 3. Architecture: generation inside the export executor

The export pipeline keeps its shape — collect → mode transform → placements →
organize — with generation replacing the artifact-path swap:

- **Mode transform** (`export/data_collector.rs`): `apply_calibrated_lights`
  no longer swaps light paths for `light_calibrations.output_path`. It drops
  calibration nodes (as today) and leaves light paths pointing at the RAW
  files. It keeps returning per-set warnings.
- **Placement source**: `WbppPlacement` gains a source discriminant —
  `PlacementSource::Copy` (default, all other modes) vs
  `PlacementSource::CalibrateLight { frame_id }`. In CalibratedLights mode
  every light placement is `CalibrateLight`; there are no calibration
  placements at all.
- **Executor** (`export/file_organizer.rs::organize_files_wbpp`): dispatches
  per placement — `Copy` → existing `copy_or_link`; `CalibrateLight` → the new
  generator. Symlink setting is irrelevant for generated files.
- **Generator** (new `export/calibrated_generator.rs`): for one frame,
  1. resolve inputs against the catalog — the B5 resolution
     (`ResolvedFrameInputs`: links re-resolved at execution time, master
     member file, source cards, CFA geometry, flat-norm divisor) moves from
     `api::lights` into `calibration_library` so the api layer and the export
     executor share it without an export→api dependency;
  2. run the B5 engine `calibration_library::light_cal::calibrate_light`
     (band-streamed formula, honest CALSTAT fallbacks, BITPIX-aware scale) —
     split into compute-then-write so the calibrated buffer is returned to
     the caller instead of written immediately (today the engine writes
     `output_path` itself); the formula core is unchanged;
  3. apply hot-pixel correction on the materialized calibrated buffer (§5);
  4. VNG-debayer if OSC and enabled (§6);
  5. build cards (`light_headers` whitelist + §7 additions) and write
     float32 FITS via tmp + atomic rename into the destination.
  A per-frame failure (geometry mismatch, unreadable master) is recorded as a
  warning and the batch continues — same policy as B5. A frame with **zero**
  usable calibration terms is unreachable here (blocked by the gate, §4).
- **Compute admission**: the export in this mode acquires ONE `ComputeQueue`
  slot (job kind `LightCalibration`) around the whole generation phase, so it
  serializes with master builds and analysis. Cancellation stays cooperative
  via the existing export cancel flag, checked per frame.
- Both hosts share all of this: the Tauri command and Axum route keep their
  thin-wrapper shape and pass the new options through.

## 4. Readiness gate

`check_mode_ready(CalibratedLights)` changes from "fresh artifacts exist" to
**masters-built strictness**, mirroring `rawWithMasters` (D2 stance):

- Every linked calibration set in the frame set's tree must be a built master
  (`is_master_library = 1`; supersede already repoints links onto masters, so
  a raw linked set means "not built yet"). Raw sets block with the existing
  "Build masters first — N sets without a master" + `→ Coverage` deep-link
  (`raw_sets_without_master` already powers both).
- A light with **no calibration links at all** blocks: "N lights have no
  calibration links". Partially-linked lights (e.g. dark only) do NOT block —
  they calibrate best-effort with an honest CALSTAT, as the engine always has.
- No auto-building of masters during export — the blocker routes the user to
  Coverage. (The B5 preflight-build handshake is removed with the old flow.)

`ExportReadiness` drops the artifact fields (`calibrated`/`stale`/`missing`)
and gains `unlinkedLights`; with the `light_calibrations` table gone,
`get_export_readiness` no longer needs flat-norm/params arguments and slims to
`(ctx, frame_set_id)`. The Export tab, both backends' commands, ts types and
`frame_set_entries` follow the new signature. `fileCounts.calibratedLights`
counts the lights themselves (all generated).

## 5. Hot-pixel cosmetic correction

New module `crates/athenaeum-core/src/calibration_library/cosmetic.rs`:

- **Map from the master dark.** For each distinct resolved dark file in the
  run, compute once (cache keyed by path): hot = `value > median + k·1.4826·MAD`
  with k = 10 (WBPP's σ-high default; constant in v1, no UI beyond the
  toggle). No dark linked (CALSTAT F/B) → no map → correction honestly
  skipped for that frame.
- **Replacement on the calibrated frame**, before debayering (WBPP's order):
  mono → median of the 8 neighbors in the 3×3 window; CFA → median of the 8
  same-channel neighbors (stride-2, i.e. the 5×5 window's same-phase cells).
  Border pixels use the available subset. Applied to the full output buffer
  the engine already materializes before `write_fits_f32` — no streaming
  change.
- **Header**: `ATH_CHPX = <replaced count>` on every corrected output.
- Runs for mono and OSC alike — which is why it lives in core, not rustafits.

## 6. VNG debayer (rustafits)

- New `rustafits/src/processing/vng.rs` (re-exported beside the existing
  super-pixel kernels): `vng_debayer_f32(data, width, height, pattern) →
  planar RGB (3 × w·h f32)` at **native resolution**, classic 8-gradient VNG.
  The 2-pixel border falls back to bilinear. Scalar first; row-parallel/SIMD
  only if profiling demands it (26 MP scalar Rust is expected at ~1–2 s).
- The generator maps `frames.bayerpat`/offsets (already validated by the
  engine's `CfaGeometry`) to rustafits' `BayerPattern`; XBAYROFF/YBAYROFF
  shift the pattern phase exactly as the engine's flat-norm path does.
- **Output**: NAXIS3 = 3 float32 FITS (`write_fits_f32` already takes a plane
  count). Filename `c_<stem>_d.fits` (§7).
- **Validation protocol** (before wiring into the export): a throwaway
  harness runs our VNG over WBPP's calibrated `_c.xisf` frames and diffs
  against the reference `_c_d.xisf` — this isolates debayer math from
  calibration. Acceptance: no pattern/channel-assignment errors (these show
  as checkerboard/color swaps), median |diff| ≈ 0, small p99.9 residual on
  interior pixels; bitwise equality with PI is NOT expected (implementation
  freedom in gradient thresholds). Synthetic fixtures (constant channels,
  ramps) pin the kernel in unit tests. Per the repo rule, neither code nor
  comments name the reference implementation.

## 7. Output naming and headers

- Mono, or OSC with debayer off: `c_<original stem>.fits` — unchanged B5 §7
  card set (whitelist copy-through, CALSTAT, ATH_CSRC/CSRN/CDRK/CFLT/CBIA,
  ATH_CSCL, ATH_CFNM + per-channel ATH_C* cards, ATH_CVER) plus `ATH_CHPX`.
- OSC debayered: `c_<original stem>_d.fits`, 3-plane RGB; same card set MINUS
  the Bayer cards (`BAYERPAT`/`XBAYROFF`/`YBAYROFF` removed — the mosaic is
  gone; `ROWORDER` stays), plus `ATH_CDBM = 'VNG'`. Full-res debayer keeps
  geometry, so copied WCS stays valid.
- `ATH_CVER` bumps (engine output surface changed).
- Collision suffixing inside the export tree follows the export's existing
  claim rules; a re-export overwrites its own previous output.

## 8. Frame-set send: generation during prepare

The send path (`calibratedLights` mode) generates the same outputs into the
package instead of copying pre-built artifacts:

- `PayloadEntry` (UNGATED, shared with headless Perseus) gains
  `generate: bool` — the entry's own `frame_id` names what to calibrate, and
  the generation options travel at package level; `false` for every existing
  producer, so Perseus and the selection path are untouched.
  `frame_set_entries` sets it for lights in CalibratedLights mode and leaves
  `source_path` at the RAW light (the existence pre-flight stats it; its size
  is the progress estimate).
- `api::sync_prepare::spawn_prepare` staging loop: an entry with `generate`
  runs the §3 generator writing directly to the entry's package destination
  (tmp + rename inside the package dir), then hashes the written file for the
  manifest with the same xxh3 helper. No `files.strong_hash` banking — the
  output is not a cataloged file. `sync_outbound_files.size` is updated to
  the real size after generation. The prepare cancel flag threads into the
  generator's per-frame checks.
- The generation portion of a prepare acquires one `ComputeQueue` slot
  (`LightCalibration`) for its duration — prepare's own `Semaphore(1)` is
  held first, compute second; nothing acquires in the reverse order, so no
  cycle.
- Gate at enqueue: `frame_set_entries` already runs `check_mode_ready`, which
  now enforces §4. Links re-resolve at prepare time (supersede-safe), same as
  the export executor.
- **Receiver**: `PayloadKind::CalibratedLight` keeps its wire value; landing
  no longer goes through reconcile-adopt — the file lands and the scanner
  skip (§9) keeps it out of the catalog.

## 8a. Collab: deferred (owner decision C, 2026-08-31)

Collab publishing is built on pre-existing artifacts: the project gate's
layer-1 "calibrated" precondition reads `light_calibrations`
(`collab/gate.rs::LightCalStatus`), and `publish_collab_frames` /
`project_collector` package `light_calibrations.output_path` files. The owner
chose to DEFER the collab rework to its own cycle rather than extend this one:

- The gate's calibration status resolves to **NotCalibrated unconditionally**
  (one caller-side constant in `api::collab`, with a comment naming this spec)
  — no frame passes layer 1, so `publish_collab_frames` fails early with its
  existing "no publishable frames" error. Publishing own lights is therefore
  temporarily non-functional, honestly blocked rather than silently empty.
- `LightCalStatus` moves into `collab/gate.rs` (the gate is DB-free by
  design); every other `db::light_calibrations` import in collab code is
  excised — artifact lookups become unconditional skip-with-warn naming the
  pending rework.
- **Receiving contributions keeps working**: the scanner's `ATH_PRJ` routing
  and `reconcile_project_contribution` run on `db::collab_exchange` tables and
  are untouched.
- Collab tests that seeded `light_calibrations` rows to make frames
  publishable are rewritten to assert the blocked behavior (or `#[ignore]`d
  with a reason naming the follow-up) — never deleted silently.
- The rework itself (generate-at-publish, gate = masters-built) is a named
  follow-up cycle, recorded in `docs/open-items.md`.

## 9. Demolition of the standalone flow

- **Commands removed** (both backends + registrations):
  `get_light_calibration_readiness`, `get_light_calibration_details`,
  `start_light_calibration`, `cancel_light_calibration`.
- **api::lights**: the worker thread, preflight master-build handshake,
  `active_light_cal` map, scope/staleness machinery go. What the generator
  reuses (resolution, readiness computation, `check_mode_ready`) stays, slimmed
  per §4.
- **Frontend removed**: `CalibrateLightsDialog.tsx`, `LightCalStatusBadge`,
  the `onCalibrateLights` prop chain and readiness/details loading in
  `FrameSetDetail`, `calibration-progress`/`calibration-finished` listeners.
  The `calibration` NotificationKind stays (used by master builds). Flat-norm
  prefs readers move to the Export tab options.
- **DB**: `DROP TABLE IF EXISTS light_calibrations` in `init_db` (idempotent,
  catalog untouched); `db/light_calibrations.rs` (incl. `derive_status`)
  deleted.
- **Scanner**: `reconcile_calibrated_light`'s four branches collapse to one
  rule — a file carrying `CALSTAT` + `ATH_CSRC` is a calibrated artifact and
  is **never cataloged**: skip with a `debug!`. The `calibrated_duplicates`
  scan-result field and its notification surface are removed.
- **Docs**: CLAUDE.md's B5 section rewritten to describe export-time
  generation; `docs/export/README.md` updated.

## 10. Progress, cancel, events

- `export-progress` gains phase `"calibrating"` (per-frame `current/total`,
  `current_file`); `"copying"` remains for the other modes. Existing
  `export-complete` + export notification cover completion — the old
  `calibration-finished` event vocabulary disappears with the flow.
- `cancel_export` keeps working: the flag is polled per generated frame; a
  cancelled export reports the partial count honestly, exactly as the copy
  path does today. Prepare cancellation follows the transfer-prepare spec's
  existing contract.

## 11. Testing

- **rustafits**: VNG unit tests on synthetic CFA fixtures (constant channels
  → constant planes, ramps, all four patterns + offsets); `#[ignore]`d
  real-data reference comparison per §6's protocol.
- **core**: generator tests on tiny synthetic FITS (reuse B5 test helpers) —
  formula + cosmetic + debayer composition, per-frame failure isolation;
  cosmetic map/replacement tests (mono 3×3 vs CFA stride-2, threshold);
  §4 gate tests (raw set blocks, zero-link light blocks, partial link
  passes); scanner-skip test (CALSTAT+ATH_CSRC never ingested); migration
  test (table dropped, re-init idempotent); send-prepare generation test
  (entry with `generate` produces a hashed manifest row, cancel honored).
  The B5 `#[ignore]`d real-data e2e is repointed at the export path.
- **TS**: `npx tsc --noEmit`; Export tab option wiring.
- Gates: `cargo build --workspace`, `cargo test -p athenaeum-core`, tsc.

## 12. Out of scope

- Caching/incremental regeneration of calibrated outputs.
- Hot-pixel detection from the frame itself (dark-map only in v1) or a
  configurable σ.
- XISF output; dark scaling/optimization (unchanged stance); auto master
  builds during export; migrating or deleting old artifact trees; Perseus
  changes.

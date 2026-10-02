# 6b-2 research — LN on overlap (step 9), coverage + crop (step 10), acceptance runs A/B

Branch `collab-wave6b2` @ e03bc48f. Read-only research; no cargo/npm run. Line numbers are
from this HEAD. "Spec" = `docs/superpowers/specs/2026-10-02-collab-v3-wave6-project-stacking-export-design.md`.

---

## 0. Headline facts (what contradicts or sharpens the spec)

1. **Seam 1 cites the wrong function.** `engine.rs:~536-538` is `run_banded` — the unweighted
   *calibration-master* path (`integrate_bias_like` / `integrate_flat`). The LN reference and every
   light master go through `integrate_stack` (`build_reference` → `integrate_planes` →
   `integrate_stack`, `ln/reference.rs:181`, `stacking/integrate.rs:570-768`). The real site is
   `engine.rs:1336-1337`:
   ```rust
   if work.is_empty() {
       *out_px = 0.0;
       row_all_bad += 1;
   ```
   Two traps there:
   - `work.is_empty()` is ALSO true when every finite sample was range-rejected
     (`raw <= range_low`, and `range_low` defaults to `Some(0.0)`, `stacking/integrate.rs:62-72`),
     or forced out by the large-scale pass. So "empty → NaN" must key on "no FINITE sample"
     (a per-pixel finite count, i.e. the coverage count), not on `work.is_empty()`.
   - The post-loop guard rejects any non-finite output with a hard error
     (`engine.rs:1576-1580`, `"internal: non-finite value {bad} survived input filtering"`). The
     NaN option has to relax it to "non-finite only where count == 0".
2. **Seam 1 alone makes reference-side detection WORSE.** Today a pixel no member covers is
   `0.0`. rustafits' fast detector ignores exact zeros in its global background and noise
   (`rustafits/src/analysis/adaptive_detection.rs:66` skips `v <= 0.0` in the histogram/mean; the
   3σ-clipped RMS skips `v != 0.0` false, around line 125). `ln/background.rs::clean_plane` also
   drops `v < low_clip (4.5e-5)` to NaN. So the 0.0 fill is ALREADY mask-aware for detection and
   for the background model. Once seam 1 makes those pixels NaN, `LnReferenceForDetection::build`
   (`ln/mod.rs:453-463`) replaces them with the plane's finite MEDIAN — and a median fill is NOT
   ignored by the detector. It sits in the histogram's mode bin and adds zero-deviation samples to
   the RMS, so σ is under-estimated and the threshold drops. **Seams 1 and 2 must land together, or
   seam 2's detection fix first.** The concrete detection fix: in the project path, fill masked
   pixels with **0.0** for detection (the detector then excludes them exactly), never the location.
   Then drop stars and seeds whose fit stamp touches a masked pixel.
3. **The acceptance set cannot exercise partial LN overlap or the 25 % threshold.** The real
   test-hub project is "LDN1272-test Project" (dev DB, linked to frame set 204 = the Tier A/C
   reduced set). Its two rigs are two GROUPS (W9): mono ATR2600M `L` (87 published, 83
   accepted) and OSC ZWO ASI2600MC Duo `None` (101). Within each group every member has the
   same framing, so its overlap with the group's LN reference is ≈ 100 %. Nothing there falls
   under 25 %, so "re-measured on the acceptance set" (spec §5.6, §16) is not achievable. The
   pixel scales are 1.866″ and 1.884″ (ratio 1.009), so "two pixel scales" (§15.3, v3 §15 line) is
   barely exercised too.
   - A real partial-overlap set exists: set 195 "Ghost Nebula QHY", one camera at 352 mm and
     448 mm within one narrowband group (×1.286, M4b acceptance). That is ~60 % overlap, not
     < 25 %, and it would have to be published into a test-hub project by the owner.
   - The < 25 % fallback is reachable only in synthetic fixtures and the fake-hub e2e.
4. **Grid parity.** Both rigs' WCS have det(CD) > 0:
   - OSC landed frame: `CD1_1=-3.70e-6, CD1_2=+5.242e-4, CD2_1=-5.242e-4, CD2_2=-3.70e-6`.
   - Mono: `CD1_1=+3.20e-6, CD1_2=-5.206e-4, CD2_1=+5.203e-4, CD2_2=+3.27e-6`.
   - The rigs are also rotated ~180° from each other.

   The grid tuple `{scaleArcsec, widthPx, heightPx, crval1, crval2, rotationDeg}` has no parity
   flag. If step 6 builds CD from (scale, rotation) with the standard det < 0 parity, then:
   - every grid master is mirrored relative to these frames;
   - run A's "grid = the reference's own geometry" can only be the reference FLIPPED (θ ≈ −89.6°
     gives the OSC reference flipped top-to-bottom).

   Whole-plane stats (median, MAD, noise, FWHM) are mirror-invariant, so run A's rows still work.
   Any pixel-level imgcmp needs `np.flipud`. The ROWORDER card that the master copies from the
   reference is meaningless on a grid master (a step 11 concern).
5. **The coverage filter breaks on a wide grid.** `reference_coverage()` (`weights.rs:655-688`)
   measures the fraction of the GRID rectangle a member covers, against
   `MIN_REFERENCE_COVERAGE = 0.97` (`weights.rs:640`). It gates two choices: the normalization
   anchor (`run.rs:6940-6980`) and the LN-reference member list (`run.rs:8517-8560`).
   - On a proposed grid (the union of both rigs' footprints) almost no member reaches 0.97. The
     filter is then "dropped" for every group on every run, with a warning, and the LN members
     become pure sky-penalized.
   - Grid mode needs its own rule, for example coverage relative to the group's own footprint
     union or the anchor's footprint. The spec does not say.
6. **coRegistered crop cannot be done inline.** Masters are written per group inside
   `process_group_output` (`run.rs:7741-7845`), drizzle right after (`run.rs:7883-8320`). The
   run-wide rectangle (the union over groups) is known only after the last group.
   - Re-reading written masters fails for XISF. `PlaneReader` cannot read `.xisf` (README M4d
     Task 3 finding). rustafits returns XISF Float32 in the ADU domain (the `measure_probe` doc
     notes the ×65535 trap).
   - So crop needs a deferred write when `cropToCoverage > 0`: scratch FITS, crop at the end of
     `stage_output`, then the final format. `native` can crop per group inline.
7. **Disk.**
   - `/Volumes/BigMac` has **43 GiB free** (99 % full; `~/.athenaeum-acc` holds 344 GB of old
     checkpoints). `checkpoint.sh` needs about 135 GB and hard-codes `ACC_ROOT=$HOME/.athenaeum-acc`.
   - `/Volumes/bigbase3` has 6.8 TiB free and is already in `server.sh`'s allowed paths. It is
     the place for the 6b-2 acceptance work dir, outside its scan roots `Astrolib`,
     `Calibration`, `Sync` and `Projects`.
   - The `tierA-template` (2026-09-20) predates the project link (2026-09-27), so run A needs a
     fresh copy of the current dev DB.

---

## 1. The LN reference build

### Where and how

- `stacking/run.rs:8484` `run_group_normalization`:
  - **Member selection** (`run.rs:8510-8580`). Sky-penalized order (`sky_penalized_order`) over
    members passing the coverage filter: `reference_coverage(&m.map, m.measurement.width,
    m.measurement.height, ref_width, ref_height) >= MIN_REFERENCE_COVERAGE`. The filter is
    dropped (warn + run warning) when fewer than 3 pass. `n = referenceFrames.min(ranked.len())`,
    and `n < 3` takes a group-level global fallback. `reference_members = ranked[..n]`.
  - **Reference hash** `normalization_hash_for(cfg, combined_registration_hash,
    combined_measurement_hash, &reference_member_ids, "", None, group_beta)` (`run.rs:8614`;
    formula `plan.rs:601-655`).
  - **Cache**: the `find_artifact(.., "ln_reference", None)` + `is_fresh` row is read back with
    `read_reference` (FITS through `PlaneReader`; NaN round-trips). A miss calls
    `build_and_write_ln_reference` (`run.rs:9547-9675`).
  - **Backgrounds**: `ref_backgrounds = background_grid(plane, …, DEFAULT_PARAMS{scale})` once
    per channel, per GROUP (`run.rs:8716-8735`). A fully invalid reference background is a
    group-level global fallback (`run.rs:8747-8770`).
  - **Detection prep**: `LnReferenceForDetection::build(&ln_reference, reference_psf,
    max_stars, pool)` once per group (`run.rs:8797-8802`; `ln/mod.rs:444-485`).
  - **Seeds calibration** `k` (`run.rs:8880-8990`), then the fan-out `normalize_frame` over
    stale members (`run.rs:9180-9240`).
- `ln/reference.rs:112` `build_reference(input, included, n, pool, cancel, io, on_progress)`:
  - re-sorts `included` by weight (a no-op on which set is used);
  - takes `frames_used = ranked[..n]`;
  - calls `integrate_planes(input, &frames_used, &weights(=1.0), AdditiveWithScaling,
    ScaleZeroOffset, local_for_output=false, ln=None, rej=None, forced=None,
    LinearFitClip{5.0,3.5}, write_maps=false, …)` (`ln/reference.rs:181-205`).

  `range_low/high` still come from `input.integration` (default `range_low = Some(0.0)`). The
  recipe is pinned by `reference_recipe_is_pinned_to_the_fixed_choices` (`ln/reference.rs:565`).
- The output is `LnReference { width, height, planes, frames_used }`. There is no count plane
  today.

### Seam 1 — threading "empty → NaN" without touching the master path

Today the only zero-write that matters is `engine.rs:1336-1337` (above).
`StackParams` (`engine.rs:752-813`) is the per-call option struct. Recommended shape:

- `StackParams` gains `coverage: bool` (accumulate the per-pixel finite count) and
  `empty_as_nan: bool`, with `empty_as_nan ⇒ coverage` enforced inside.
- Count next to the two `row_samples[i] += 1` sites (`engine.rs:1215` in the no-local hot loop,
  `engine.rs:1283` in the mixed loop) with a per-pixel `u16` local. Store it like the rejection
  maps: a full-image `Mutex<Vec<f32>>`, the same per-band lock pattern as `map_low`/`map_high`
  (`engine.rs:1010-1053`, store at `1452`).
- At the empty branch: `if empty_as_nan && finite_here == 0 { NaN } else { 0.0 }`.
- The guard at `1576` checks only `count > 0` pixels when `empty_as_nan`.
- Return `StackOutput.coverage: Option<Vec<f32>>`.
- `integrate_planes` (`stacking/integrate.rs:570`, already 15 params) gets one extra options
  argument (`coverage`, `empty_as_nan`).
  - `integrate_group` passes `(GroupInput.coverage_map && p == 0, false)`: one plane; the master
    keeps `0.0`.
  - `build_reference` passes `(true, true)` only in grid/project mode (a new parameter or
    options struct), so the frame-set reference stays bit-identical.
  - `LnReference` gains `coverage: Option<Vec<f32>>`. `read_reference` recovers `count > 0` from
    NaN; the count itself needs its own plane or file if a min-count rule is ever added.
- Churn:
  - 21 `StackParams {` literals: 20 in `engine.rs` tests, 1 in `integrate.rs:730`.
  - 17 `GroupInput {` literals: 12 in `integrate.rs` tests, 3 in `ln/reference.rs`, 2 in `run.rs`.
  - Plus `examples/integrate_probe.rs` and `examples/ln_probe.rs` (`build_reference`,
    `normalize_frame`, `LnReferenceForDetection::build` at `ln_probe.rs:531/563/749`).
  - `cargo check --all-targets` is mandatory (README: `--lib` hides example breakage).
    `integration/` is `render`-gated, so the headless check is no coverage.

### Tests that pin the master path bit-for-bit today

- `engine.rs:2737` `stack_path_with_unit_inputs_equals_the_master_path`: `run_banded` ≡
  `integrate_stack`, `to_bits`.
- `engine.rs:2115` `non_finite_samples_are_excluded_not_propagated`: the master never contains
  non-finite values.
- Other engine pins (all `to_bits` / exact):
  - `engine.rs:2889` `local_grids_replace_the_output_pair_for_a_frame_that_has_one`;
  - `engine.rs:3187` `rejection_bit_sink_records_exactly_the_rejected_samples_and_does_not_change_the_output`;
  - `engine.rs:3866` `band_prefetch_does_not_change_the_output`;
  - the forced-rejection tests at `3489-3790`.
- `stacking/integrate.rs:2613` `large_scale_rejection_removes_a_bands_shoulder…` (`to_bits`
  outside the forced band).
- Run-level:
  - `run.rs:18182` `drizzle_off_leaves_the_master_byte_identical_to_drizzle_on`, which reads
    master planes with `PlaneReader` and compares `to_bits` (THE template for the coverage pin);
  - `run.rs:11038` `a_registered_frame_deleted_mid_run_falls_back_and_the_master_is_unchanged`;
  - `run.rs:16049` `local_rejection_normalization_group_fallback_matches_global_scale_zero_offset`;
  - `run.rs:16221` `local_normalization_rejection_only_frame_failure_keeps_global_normalization`.
- LN: `ln/background.rs:788` `background_grid_is_bit_identical_with_and_without_a_pool`;
  `ln/mod.rs:1218` `one_source_per_frame_warps_each_channel_identically`; the
  `a_grid_*` / sidecar tests at `ln/mod.rs:1126-1382`.
- Cache keys: `plan.rs:6278` `stage_hashes_are_pinned_to_golden_literals` (calib / meas / reg /
  ln_ref / ln / ln_cal for four configs).

---

## 2. Per-member LN fitting

`normalize_frame` (`ln/mod.rs:571-958`) runs per channel:

1. **Warp/read** the member into the reference geometry — the grid in project mode — through the
   materialized registered artifact or the on-the-fly fallback (`ln/mod.rs:637-668`). It reads
   into `target`, NaN outside the footprint. `RegisteredSource` fills NaN
   (`registered_source.rs:409`); `resample/warp.rs:295-337` returns NaN where the kernel support
   leaves the source.
2. `target_bg = background_grid(&target, W, H, target_params(TARGET_DEVIATION_SIGMA 3.2), pool)`
   (`ln/mod.rs:699`) on the NaN-preserving plane. A fully invalid grid is `LnError::Other`.
3. `location_tgt = median_of_finite(&target)` (`ln/mod.rs:731`). This is only diagnostic (stored
   in `LnGrid.location_tgt`, no pixel math reads it) and the fill value.
4. **The sanitize** (`ln/mod.rs:748-754`), verbatim:
   ```rust
   if !target.iter().all(|v| v.is_finite()) {
       for v in target.iter_mut() {
           if !v.is_finite() {
               *v = location_tgt as f32;
           }
       }
   }
   ```
5. **Scale**:
   - With usable Measure fits (`LnScaleSeeds::Measured`, ≥ `MIN_MATCHES=20`):
     `scale::relative_scale_from_seeds` (`scale.rs:1069-1218`). Seeds are mapped through
     `frame.map.forward_exact` and dropped outside `[0,W)×[0,H)` (`map_seed_position`
     `scale.rs:803-816`) or when saturated (`warped_pixel_is_saturated` `:828`). They are
     pre-selected against `prepared.tree` (reference fits within `match_radius_px` = 4.0), then
     re-fitted with `fit_stars_with_beta` on the sanitized `target`.
   - Otherwise the fallback is `relative_scale_against` (`scale.rs:641` → `_core` `:699-800`):
     `detect_seeds(target…)` → `register::detect::detect_stars` → rustafits
     `detect_fast_data`, then fit at the reference β and pair against the reference tree.
   - The RCR location gives `s`; `k` (seeds calibration) applies only on the seeds path.
6. **Grid**: `A = a_grid(...)`, `B = B_ref − A·B_tgt` per node (`ln/mod.rs:880-886`), written as
   the `.athln` sidecar (`ln/grid.rs`, `LnFrameGrids::write/read`).

Reference side, once per group:

- `LnReferenceForDetection::build` sanitizes the same way (`ln/mod.rs:453-463`: NaN → plane
  median, `Cow::Owned`). All-finite planes are borrowed.
- `PreparedReferenceChannel::build` (`scale.rs:573-603`) runs `detect_seeds` + `fit_stars` on
  that sanitized plane and builds the match trees.

### What the detector and the background do with masks today

- **Background** (`ln/background.rs`). `clean_plane` (`:289-345`) maps non-finite and
  `< low_clip` / `> high_clip` to NaN. `gather_cell` (`:369-395`) keeps finite pixels only.
  - **A cell is "valid" with ANY ≥ 1 finite pixel.** `robust_cell_level` returns invalid only
    for an empty set or a `> rejection_limit (0.3)` clip fraction (`:433-473`). So an edge cell
    straddling the footprint is measured from a sliver.
  - Invalid cells are filled by iterated 8-neighbour means (`fill_invalid_cells` `:475+`).
  - **The background is already NaN-aware; it lacks a minimum-coverage rule per cell.**
- **Detection** (rustafits). `background_and_noise` (`adaptive_detection.rs:38-140`) builds a
  histogram mode and a 3σ-clipped RMS. It skips `v <= 0.0` (and NaN, by comparison semantics) in
  the histogram/mean, and `v == 0.0` in the RMS. Thresholds are global `bg + k·noise`
  (`detect_stars_adaptive`, `:692-760`). The ln/mod.rs comment says the per-star HFD/median math
  panics on NaN, hence the sanitize.

### What "detection and background on member ∩ reference, masked" takes

Do this under a project/grid-only switch (e.g. `LnOverlap::{Legacy, Masked}` on
`normalize_frame` and `LnReferenceForDetection::build`), so every frame-set LN pin holds.

1. **Mask** `M = target finite ∧ reference finite` per channel. The reference is finite iff its
   count > 0 once seam 1 is in.
2. **Target background**: set `target[!ref_finite] = NaN` before `background_grid`.
3. **Reference background** on the same overlap. This makes `ref_backgrounds` PER MEMBER, not
   per group: `background_grid(ref_plane with NaN where !target_finite)`.
   - That doubles background cost per frame (Tier C measured `background_ms` ≈ 73-83 ms per mono
     frame on the full plane) and adds one grid-size plane per worker.
   - `LN_PLANES_RESIDENT = 8` (`run.rs:2511`) and `ln_working_set_bytes` must be revisited; the
     admission is at grid size.
   - **Do NOT propose computing it on a binned plane: measured out (ruling C-29,
     `background.rs` doc).**
4. **Optionally a minimum finite fraction per cell** (project path only), so a sliver cell is
   invalid and filled.
5. **Detection fill**: masked pixels = **0.0** (not `location`), for both the target fallback
   path and the reference prep. The detector's global stats then skip them exactly (fact 2 above).
6. **Stamp filter**: drop seeds/stars within the fit-stamp radius of `!M`. One erosion of `M`
   (separable box or distance transform) per frame/channel, before `fit_stars_with_beta`. This
   matters on the seeds path too: it never detects on the target, but fits there.

   In the seeds path, pairing already lands inside the overlap: target seeds come from the frame
   itself, and `prepared.tree` has no stars where the reference has no data.
7. **Overlap fraction** `|M| / |target finite|`, computed in `normalize_frame` right after the
   read (cheap, the plane is in RAM).

### The "< 25 % → global normalization for that member" fallback

What exists:

- Global normalization IS the engine's default. `integrate_planes` computes per frame
  `rejection_pair(ref_ls, frame_ls, rejection_mode)` and
  `output_pair(ref_ls, frame_ls, output_mode)` from Measure's whole-frame location/scale
  (`stacking/integrate.rs:700-706`).
  - Config `NormalizationConfig { output: OutputNormalization (default AdditiveWithScaling),
    rejection: RejectionNormalization (default ScaleZeroOffset; None/EqualizeFluxes/Local),
    scale_estimator, local: LocalNormalizationConfig{ enabled (default false), scale 1024,
    referenceFrames 20, psfModel, localScale } }` (`stacking/integrate.rs:126-176`;
    `integration/stats.rs:224-243`).
- A frame whose `GroupInput.ln[i]` is `None` already uses its global pair in the engine
  (`frames_without_local`, `engine.rs:970-976`; mixed loop `1304-1316`). Drizzle does the same
  via `GroupOutput.output_pairs` (ruling R-M3-5, `run.rs:8040-8075`). **The integration side of
  the fallback exists.**
- **The run side does not.** Today any LN failure while LN drives OUTPUT EXCLUDES the frame
  (`fail_ln_frame`, `run.rs:9255-9300`, persisted via `exclude_frame_and_persist`).

What is new:

- An outcome like `LnError::LowOverlap { fraction }` (or an Ok variant) that
  `run_group_normalization` maps to "no sidecar, no exclusion, kept with its global pair". One
  aggregated `warn!` + run warning naming the count.
- **A cache marker.** `is_fresh` requires `path` + `size` (`plan.rs:715-722`). A member with no
  `ln` row reads as stale on every run, and the plan gate shows Normalize stale forever
  (`plan.rs:2240-2298`). The precedent for "known, not owed" is `ln_runtime_excluded_by_frame`
  (`plan.rs:2016-2055`, gated on the exclusion-reason prefix `"local normalization:"` and
  `last_run.config_hash == hash`). Use a payload-only row (e.g. kind `ln_global`, hash-checked by
  equality, like `metrics`) or extend that set.
- Decide whether `TooFewMatches` on a low-overlap member (overlap 25-40 % can drop below 20
  matches) also falls back instead of excluding.
- Fold a project-only LN token (e.g. `LN_OVERLAP_VERSION`) into the project LN hashes, never into
  the frame-set formula. The golden literals at `plan.rs:6278` must not move.
- Plan-gate note: `ln_registration_by_frame` reads `stored_registrations(conn, subject)`
  (`plan.rs:2005-2011`), whose Project arm returns nothing (6b-1 open item 2). Until step 8
  resolves the project `RegistrationRecord` shape, a project's Normalize reads stale every plan.

---

## 3. Coverage

### Where to count

- The per-pixel frame loop is in `integrate_stack`'s `process_row` (`engine.rs:1140-1475`).
  Finite raw samples are counted per FRAME today (`row_samples[i] += 1` at `1215` / `1283`,
  flushed to `samples_per_frame`). That count is "geometric coverage before rejection": it comes
  before range rejection, normalization and the algorithm.
- A per-pixel `finite_here: u16` incremented at those two sites is the coverage count, at
  essentially zero extra I/O.
- The no-local loop (`1196-1255`) is documented as "textually untouched … the Plan 4 `to_bits()`
  pin's byte-identity comes from this" (outputs, not instructions, are what the pins check). The
  plan should require an interleaved `integrate_probe` bracket (R-TA-9) to show no regression with
  coverage off.

### The band model

- `band_loop` (`engine.rs:233`) hands each band `BandJob { planes, out_band, y0, rows, width }`.
  Bands are combined serially (reads prefetched).
- The rejection maps already use the exact pattern a count plane needs: full `w*h` buffers under
  a `Mutex` locked once per band, sliced `[y0*width..(y0+rows)*width]` (`1020-1053`), written per
  pixel (`1452-1455`). The coverage buffer is a third such map.
- Memory is W×H×4 at grid size, so reuse the "maps memory check" (`run.rs:7090-7113`), which
  turns maps off with a warning when `> total_ram/4`.
- **One plane**: compute on plane 0 only (`integrate_planes` sets `coverage` for `p == 0`). OSC
  debayered planes share the footprint. With large-scale rejection the second pass sees the
  same geometry; keep pass 1's or recompute, identical.

### Writing `…_coverage.fits`

- `master_cards.rs::write_master_light` (`:680-741`) writes the master, then FITS-always
  rejection maps via `write_fits_f32` with `rejection_map_cards` (`:496-510`: IMAGETYP,
  `ATH_STK`/`ATH_STKV`, `BUNIT 'count'`, copied ROWORDER/ATH_STKI/ATH_STKG, **no WCS**).
- Add a coverage writer:
  - `<resolved master stem>_coverage.fits`, `write_fits_f32(path, w, h, 1, &cov, cards)`;
  - cards: IMAGETYP e.g. `'Coverage Map'`, `ATH_STK`/`ATH_STKV` (scanner skip), `BUNIT 'frames'`,
    ROWORDER/ATH_STKI/ATH_STKG/`ATH_RGEO`;
  - the master's WCS block copied with the `is_wcs_keyword` filter (`master_cards.rs:250`), the
    same move `weight_map_cards` makes (`:519-552`);
  - never `ATH_PRJ`.
- `WrittenMaster` gains `coverage: Option<PathBuf>`. `GroupOutput` gains
  `coverage: Option<Vec<f32>>` (`stacking/integrate.rs:436-463`). `GroupInput` gains
  `coverage_map: bool`.
- **`master_lights.kind`**: a free TEXT column with `UNIQUE(run_id, group_key, kind)`
  (`db/schema.rs:386-403`). Kinds come only through `api::stacking::MasterLightKind`
  (`api/stacking.rs:897-923`: `Master|Drizzle|WeightMap` ↔ `master|drizzle|weight_map`; ts-rs
  exported, `ts_export.rs:445`; round-trip test `api/stacking.rs:1838`).
  - Add `Coverage` → `'coverage'` and a separate kind for drizzle coverage (UNIQUE forbids
    reuse), e.g. `'drizzle_coverage'`; the spec does not name it.
  - Rows are inserted at `run.rs:7823-7838` (master) and `8250-8288` (drizzle, weight map),
    `channels = 1` for coverage.
  - `get_master_light_preview` then serves coverage too. This is a TS union change (frontend 6c).
- `SummaryGroup` (`push_summary_group`, `run.rs:8322`) gains `coverage_path` /
  `drizzle_coverage_path` with `#[serde(default)]`, so `src/types` regenerates.

### Drizzle's own coverage

- `drizzle_group` (`drizzle/mod.rs:602-1060`) has, per plane, a `w_buf` (`W`). Its
  `DrizzleStats.coverage` = fraction of `W > 0` (`:999`). The weight map = `W/max(W)` per plane
  when `write_weight_map`.
- A per-pixel "frames whose drop touched the pixel" count needs per-frame dedup inside
  `deposit_band` (`:1183+`):
  - a `u32` stamp plane (`tag = plane·n + k + 1`) plus a count plane;
  - `if stamp[idx] != tag { stamp[idx] = tag; count[idx] += 1 }` at each deposit site;
  - passed as an optional per-band slice, so the off path is unchanged.
- Add the terms to `estimate_memory_bytes` (`:277-305`, the R-M3-7 refusal).
- Per plane, like the weight map: Bayer drizzle R/B/G cover different sites.
- "Touched" should mean "deposited weight > 0". Zero-weight frames and rejected samples deposit
  nothing.
- Pin: the drizzle output is bit-identical with coverage on/off. Template:
  `drizzle/mod.rs:2190` `drizzle_group_output_is_bit_identical_across_rotations`.

### OutputConfig

- `stacking/config.rs:310-315`:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
  #[serde(rename_all = "camelCase", default)]
  pub struct OutputConfig {
      pub format: OutputFormat,
      pub cleanup: CleanupPolicy,
  }
  ```
- `coverage_map: bool` and `crop_to_coverage: u32` fit the derives.
- **They enter no stage hash.** The four subtrees are `calibration_subtree` (`:654`, writes
  `calibration` minus `format` + the fixed grouping shape), `measurement_subtree` (`:684`),
  `registration_subtree` (`:714`) and `normalization_subtree` (`:747`). None touches `output`.
  The golden literals (`plan.rs:6278`) stay.
- **They DO move `config_hash`** (`config.rs:589`, the whole config with defaults serialized).
  So `config_hash_is_canonical_and_pinned` (`config.rs:1478`) changes its literal, with a comment
  as previous bumps did. Side effect (documented precedent, `config.rs:568-577`): the plan gate's
  `ln_runtime_excluded_by_frame` empties once, so LN-excluded frames are re-owed one Normalize
  pass.
- Project seed config: `coverageMap = true` (spec §9.4). The frame-set default stays off, which
  keeps the Tier ruler untouched.

---

## 4. Crop

- **There is no crop today**: zero `crop` hits under `stacking/`, `integration/`, `fits_writer/`.
- **The rectangle**: `bbox(coverage ≥ K)` per group from the engine's count plane (pure helper,
  `None` when empty). `coRegistered` = the union over groups that wrote a master. `native` = per
  group.
- **CRPIX**:
  - `crop_plate_solve(solve, x0, y0)` sets `crpix1 -= x0; crpix2 -= y0`. SIP terms are in
    `(u,v) = p − CRPIX`, so they are unchanged. Exact for frame-set masters with SIP and for the
    grid's TAN.
  - Drizzle: crop rect × `s` in output pixels, applied after `scale_plate_solve`
    (`fits_writer/wcs.rs:151-175`, `scaled_crpix(u,s) = s·u + (s−1)/2`, `:85`), then
    CRPIX −= `x0·s`.
  - The WCS refers to array coordinates, so ROWORDER needs no special case.
- **Every output that must crop**:
  - the master (any format);
  - `_rejlow` / `_rejhigh` (FITS, no WCS);
  - `_coverage` (FITS, WCS shifted);
  - the drizzled master, `_weight` and the drizzle coverage (each rect×s, scaled WCS shifted).

  Also the `master_lights` rows' `width/height` and the `stacking_run_groups` paths (the paths are
  unchanged if written once at the final location).

  `GroupStats` master stats are measured before the write on the uncropped plane
  (`stacking/integrate.rs:1290-1300`). Whole-plane stats on a sparse grid master include the
  uncovered `0.0`; the spec is silent on re-measuring them.
- **Orchestration**:
  - `native`: crop inline in `process_group_output` before `write_master_light`. Drizzle uses
    rect×s.
  - `coRegistered`: defer, because the rectangle is known only after the group loop in
    `stage_output` (`run.rs:9760-9800`), the natural place for a crop pass before cleanup.
  - Because XISF masters cannot be re-read, a crop-on run should write per-group outputs as
    scratch FITS (e.g. `runs/run-<id>/uncropped/`), then in the crop pass:
    1. read back with `PlaneReader`;
    2. crop;
    3. write the final format to `output_dir`;
    4. insert `master_lights`, update the group paths and the summary;
    5. remove the scratch.
  - Today's per-group `insert_master_light` / `update_group(master_path)` (`run.rs:7805-7838`)
    move behind that pass for crop-on runs only.
  - A crop-pass failure must degrade to "write uncropped + warning", never lose a master (the
    spirit of R-M3-7).
  - `cropToCoverage = 0` keeps today's path literally (bit-identical).

---

## 5. Bit-identical pins

- **Existing master pins**: the list in §1. The run-level template is `run.rs:18182`
  `drizzle_off_leaves_the_master_byte_identical_to_drizzle_on`:
  - two `start_stacking` runs on `seed_star_group` + `add_master_dark_and_flat`;
  - `PlaneReader` on both `master_path`s;
  - `assert_eq!(a.to_bits(), b.to_bits())` per pixel.
- **Pin "a frame set's master is bit-identical with `coverageMap` on and off"**: clone that test
  with `cfg_on.output.coverage_map = true`. The fixture's shifts give real NaN margins. Assert:
  - planes `to_bits`-equal;
  - the `_coverage.fits` exists with `BUNIT='frames'` and `NAXIS=2`;
  - its counts equal the shift geometry (e.g. 4 in the interior, fewer at the margins);
  - a `master_lights` row of kind `coverage` with `channels = 1`;
  - rejection maps (when on) equal as well.

  Add the engine-level twin (`integrate_stack` with `coverage` on/off: `base.data`, maps,
  `rejected_per_frame`, `samples_per_frame` identical), and the same for drizzle (§3).
- **For seam 1**: an engine test where `empty_as_nan` on vs off differ ONLY at count-0 pixels
  (NaN vs 0.0) and a pixel whose finite samples are all range-rejected stays `0.0`. Plus a
  `build_reference` frame-set call that is unchanged.
- **For seam 2**: every LN test in Legacy mode unchanged (`ln/mod.rs` tests, `run.rs:15982-16380`
  LN run tests, `plan.rs` LN-staleness tests `5140-5525`). Plus a synthetic half-covered member
  (60 % overlap, a background gradient, a known `s`/offset) where Masked recovers truth and Legacy
  is biased, and a 20 %-overlap member that falls back without exclusion.

---

## 6. Acceptance harness

### Scripts (`docs/superpowers/research/scripts/acceptance/`)

- `README.md` — the recipe: reuse, never rebuild.
- `prepare-catalog.sh <work> [fits|xisf] [set-id]`
  - Makes an SQLite `.backup` of the dev DB (or `$ATH_ACC_DB`) into `<work>/athenaeum.db`.
  - Creates `library/export/stackwork/stackout` and redirects the settings to them.
  - For `set-id`: copies set 109's stacking config if absent, then forces
    `output.format=fits` and `cleanup=keepAll`, and removes `paths`.
- `server.sh <work> [port]`
  - Builds release `athenaeum-web` into `.superpowers/target-acc` and serves the copy.
  - `ATHENAEUM_ALLOWED_PATHS` includes `/Volumes/bigbase*`, `/Volumes/Universe`, `$WORK`, plus
    `ATH_ACC_EXTRA_PATHS`.
- `api.sh <command> '<json>' [port]` — POSTs `/api/<command>`.
- `tier1/tier1-run.sh REPO W PORT SET_ID`
  - Starts the server and the SSE capture, then `POST start_stacking {setId, rerunFrom:"calibrate"}`.
  - Fails loudly if the answer has no `runId`.
  - Waits for `stacking-complete` and writes `run.txt` (`wall_s`).
- `tier1/checkpoint.sh <name> [base] [--numeric]`
  - Copies `$HOME/.athenaeum-acc/tierA-template` to `tierA-<name>` and rewrites the paths.
  - Refuses on a set-204 `paths` override, on Time Machine or a running cargo, and when
    `name == base`. It is zsh-only and re-execs itself.
  - Runs set 204, then `tier1-extract.py`, then `tier1-compare.py` (identity or `--numeric`).
- `tier1/tier1-extract.py <W>` — stage table and per-stage medians from `logs/*.jsonl` + `run.txt`.
- `tier1/tier1-compare.py`
  - Identity mode compares masters (raw, card set, data blake2), `registration_results`,
    `.athln`, calibrated frames.
  - `--numeric <head> <base> [--set-id]` applies the §8 rows:
    - masters from `measure_probe` (whole plane) on `<W>/stackout/*.fits|xisf`, paired BY
      FILENAME (`classify_masters`);
    - rejected fraction from `stats_json`;
    - weights from `stacking_run_frames.weight` joined by `frame_id`;
    - LN `s` from the `.athln` A-grid means;
    - drizzle level and coverage from the `_weight` file.
  - Tolerance constants at the module top: `MEDIAN_TOL 0.001`, `MAD_TOL 0.01`,
    `NOISE_TOL 0.02`, `FWHM_TOL 0.01`, `REJECTED_FRACTION_TOL_PP 0.3`, `LN_MEDIAN_TOL 0.005`,
    `LN_SCATTER_TOL 0.01`.
- `tier1/rerun-build.sh <commit> <name>` — the same run on an older commit's build (bracket,
  R-TA-9).
- `tierC-external.sh <checkpoint> [external-dir]` — the external-masters gate (N-independent rows:
  FWHM ±2 %, drizzle FWHM ratio, weights ρ).
- `imgcmp.py <a> <b> [--scale-b F]` — pixel diff of FITS/XISF float32 (numpy).
- `xisf-acceptance-run.sh` — a documented sequence, not one-shot.
- `tier1/*-top.txt` — profiler tops.

### Tier C §8 table

`docs/superpowers/specs/2026-09-20-stacking-compute-tierC-design.md` lines 443-463; repeated
verbatim in the `tier1-compare.py` docstring.

| Metric | Tolerance |
| ---- | ---- |
| Master pixel median / MAD vs baseline | ± 0.1 % / ± 1 % |
| Master noise (MRS σ) | ± 2 % |
| FWHM (mono, OSC per plane) | ± 1 % |
| Rejected fraction (linear fit) | ± 0.3 pp |
| Per-frame weights | Spearman ρ ≥ 0.99; top-20 overlap ≥ 18/20 |
| LN relative scale `s` per frame | median ratio 1 ± 0.5 %, scatter ≤ 1 % |
| Drizzle level vs undrizzled | 0.998-1.002 (absolute; C-20) |
| Drizzle coverage | 1.0 on every plane (a C-23 data fact: mono 0.9935) |
| Wall | reported |

Run A names only the first four rows.

### What run A needs

1. **A fresh template** from the CURRENT dev DB. `tierA-template` is from 2026-09-20 and has
   no LDN1272-test Project rows; the link was made 2026-09-27.
   - Run `prepare-catalog.sh <work> fits 204` with a work dir on `/Volumes/bigbase3` (e.g.
     `/Volumes/bigbase3/acc-6b2/…`, outside its scan roots `Astrolib`, `Calibration`, `Sync`,
     `Projects`).
   - BigMac has 43 GiB free. `checkpoint.sh`'s `$HOME/.athenaeum-acc` root does not fit, and
     the release build into `.superpowers/target-acc` on BigMac is itself at risk.
2. **The grid set LOCALLY in the copy**: `UPDATE collab_projects SET canonical_grid_json = …`.
   Never PUT to the real hub project. Run the acceptance server without hub connectivity
   (sign-out, or an unreachable hub), otherwise the feed refresh overwrites the grid. This
   requires step 6 to read the local copy.
3. **The same reference in both runs.**
   - Personal: Manual reference = catalog frame X (set 204's config `reference.mode`).
   - Project: `stacking_project_config.reference_frame_uuid` = the uuid whose
     `project_frames_local.source_frame_id = X`.
   - Tier A/C auto-picked an OSC frame (6248×4176; README C-23 "the OSC reference geometry").
4. **The grid from X's WCS**: the manifest `meta.wcs` = the publisher's solve (TAN-SIP).
   - `crval` = the sky position of X's centre pixel `((W+1)/2, (H+1)/2)` through X's WCS
     including SIP.
   - `scale` = `sqrt(|det CD|)·3600`.
   - `size` = W×H. The hub accepts any `64..=65536` sizes and any finite scale
     (`athenaeum-hub/src/routes/projects.rs:1478-1497`), so it can be exact.
   - `rotation` follows step 6's CD convention. With standard parity the master comes out
     mirrored (fact 4); the stats rows are unaffected.
5. **Same frames.** The project stacks 83 accepted mono + 101 OSC (the 4 not-accepted mono are
   not stackable). Set 204 holds 92 mono + 105 OSC (README Tier A). So the personal run excludes
   9 mono + 4 OSC through `stacking_set_config.excluded_frame_ids_json`, mapped via
   `project_frames_local.source_frame_id`.
6. **Same config**: set 109's acceptance config, as in the template:
   - normalization: `local.enabled = true`, `rejection = "local"`;
   - drizzle 2× square with `bayer = true`, `useLocalNormalization` and `writeWeightMap`;
   - `coRegistered`, `distortion = off`, `reference.twoPass`, `calibration.debayerOsc = true`,
     `hotPixelCorrection = true`.

   The project's document is the same with `reference` set to manual and `coverageMap` on. The
   exposure mode is irrelevant (every frame is 180 s, one class).
7. **Calibration-equivalence pre-check.** The project's inputs are the PUBLISHED files, not the
   run's own Calibrate output:
   - the published files carry `CALSTAT='BDF'`, `ATH_CVER=3`, `ATH_CHPX` (hot pixels replaced)
     and `ATH_CCFA=T`;
   - their masters are under `/Volumes/bigbase3/Calibration/…`;
   - the OSC files are the CFA mosaic (`NAXIS=2`), which Prepare debayers.

   The personal run calibrates from raw with ITS links. Before trusting run A, `imgcmp.py` a
   sample of personal calibrated artifacts against the landed files (mono) and the personal
   `c_*_d.fits` against the project's prepared `p_*_d.fits` (OSC). Otherwise a calibration
   difference masquerades as a grid/LN difference.
8. **A project-run driver**: a `tier1-run.sh` variant posting
   `start_project_stacking {projectId, rerunFrom}` and waiting on `stacking-complete` with
   `projectId`.
9. **A pairs mode in `tier1-compare.py`** (e.g. `--numeric-pairs head=base …`) reusing
   `measure_master` and the four row checks.
   - Project masters land in `<output>/<sanitized project title>/`, but `classify_masters` globs
     `stackout/*.fits` flat and pairs by filename.
   - Group keys and names differ (`mono__L__st180` vs the frame-set key).
   - A new `_coverage.fits` would be classified as a "plain" master. In numeric mode it only shows
     as "MISSING in base (new in head)", not a FAIL.

   Optionally also report the rejected-fraction, weights (join handle ↔ frame id via the
   project-frame map) and LN-scale rows for context.

The reference frame is resampled once in grid mode. The grid master is also "SIP-undistorted"
relative to the personal master, which keeps the reference's SIP.

### What run B reports

The same project on the proposed grid (median scale, north up, target centre, union bbox clipped
to the target circle, multiple of 64). Per group:

- FWHM in px × grid scale = arcsec;
- eccentricity;
- background noise per arcsec²;
- the coverage map (min/max, histogram, crop rectangle).

There is no tolerance (§15.3).

- **"Noise per arcsec²" needs a written definition** in the plan: e.g. σ_px·scale for
  area-normalized white noise, or σ_px/scale² for surface brightness.
- `measure_probe` has no mask or rect option and measures the whole plane, including uncovered
  `0.0`. So measure on a crop to the `coverage ≥ K` region: `cropToCoverage` itself, or a small
  numpy crop helper writing FITS before `measure_probe`.
- The WCS row (independent solve within 0.5 px) can use the `solvemyastro` CLI on the master.

### Suggested extra bracket for seam 2

Run A barely exercises it (near-full footprints). Run B on the union grid has large uncovered
regions per group — the detection-fill case. A same-grid run-B pair Legacy vs Masked on the §8
rows would give seam 2 a master-level verdict. That needs a way to force Legacy in the project
path (e.g. a hidden config or probe flag).

---

## 7. The test-hub project

Dev DB `~/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db` (read-only
queries). The dev build defaults to the test hub.

- `collab_projects`: "LDN1272-test Project", `project_id = af136ec9-1885-4087-b684-9136ad655e9b`,
  slug `ldn1272-test-project`.
  - This device is NOT coordinator (that is account "EgoBrain"), but its `gov_caps_json`
    includes `project.edit`, so it may set the grid.
  - `project_links`: → `frames_set_id = 204` "LDN1272-test".
- `project_frames_local`: all `origin = own`, publisher "Vilen", `local_state = own_held`,
  on disk. Every frame has a `meta.wcs` (TAN-SIP).

  | Group | Camera | Size | Scale | Frames |
  | ---- | ---- | ---- | ---- | ---- |
  | mono `L` | ATR2600M | 6224×4168 | 1.866-1.874″ | 87 published, 83 accepted |
  | OSC `None` (RGGB, CFA mosaic) | ZWO ASI2600MC Duo | 6248×4176 | 1.881-1.888″ | 101, all accepted |

  All frames are 180 s. Landed under `/Volumes/bigbase3/Projects/ldn1272-test-project/vilen/c_*.fits`.
- Other dev projects (M 33, M 45, Tadpoles smoke) have no frames. The prod DB has "M 33
  Project" and "LDN 1272 Project" on the prod hub.
- No memory note or open-items entry names this project for the 6b-2 acceptance. `open-items.md`
  has the wave-6a owner smokes only.

---

## 8. Recommended decomposition of steps 9 and 10

Risk order: numerics first, plumbing after, structure last. Each commit stands alone and is
testable.

**C1 — Engine coverage count + empty→NaN (bit-identical; prerequisite of both steps)**
- `StackParams.coverage` / `empty_as_nan`; `StackOutput.coverage`; the guard relaxed for count-0
  pixels only.
- Files: `integration/engine.rs` plus all 21 `StackParams` literals.
- Tests: coverage on/off bit-identical (data, maps, per-frame counts); count = finite samples
  on a NaN-margin fixture; empty-NaN only at count 0; an all-range-rejected pixel stays 0.0.
  The existing engine pins are unchanged.
- An `integrate_probe` interleaved bracket (R-TA-9) for the hot-loop change.

**C2 — `integrate_planes` / `GroupInput` / `GroupOutput` / `build_reference` plumbing**
- Plane-0 coverage; `LnReference.coverage`; NaN reference in grid mode only.
- Files: `stacking/integrate.rs`, `ln/reference.rs`, 17 `GroupInput` literals, both examples.
- Gate: `cargo check -p athenaeum-core --all-targets`. Frame-set `build_reference` is
  bit-identical (pin).

**C3 — Seam 2, masked LN (project/grid switch only)** — the numeric risk
- Detection fill 0.0 + the stamp-distance filter (target and reference prep).
- Overlap-masked target and per-member reference backgrounds. Optionally a per-cell min-fraction.
- A project-only LN hash token.
- Files: `ln/mod.rs`, `ln/scale.rs`, `ln/background.rs` (if the min-fraction is added), `run.rs`
  `run_group_normalization`, `LN_PLANES_RESIDENT` / admission.
- Tests: Legacy unchanged (every LN pin); a synthetic 60 %-overlap member where Masked recovers
  the true `s` and B and Legacy is biased.
- **Numeric gate = full group at master level** (run A, plus the Legacy/Masked run-B bracket).
  It cannot run before steps 8 and 11 work end to end, so C3's review closes only after the
  acceptance checkpoint (method rule).

**C4 — The < 25 % global fallback**
- `LowOverlap` outcome → kept with its global pair; one aggregated warning; the cache marker
  (payload-only row) and plan-gate "not owed"; the `TooFewMatches` policy decision.
- Tests: synthetic 20 % member (no sidecar, not excluded, its global pair used in integrate and
  drizzle, a second run is cached); the plan does not show Normalize stale forever.
- The threshold cannot be re-measured on LDN1272-test (single-rig groups). Say so in the plan;
  owner option: set 195 as a project (~60 % overlap, exercises C3 not C4).

**C5 — Grid-mode coverage-filter semantics**
- `reference_coverage` vs `MIN_REFERENCE_COVERAGE` for the anchor and the LN members on a wide
  grid. Needs a ruling. It may belong with step 8's reference rule.
- A test for a wide-grid group that does not drop the filter.

**C6 — Coverage map output**
- `OutputConfig.{coverage_map, crop_to_coverage}`, with `#[serde(default)]` from the `default`
  struct attribute.
- `config_hash` literal update; stage golden literals unchanged (pin).
- The FITS writer + cards; `MasterLightKind::Coverage`; `SummaryGroup.coverage_path`; the memory
  check; project seed `coverageMap = true`; ts regen.
- Pin: the clone of `run.rs:18182` with coverage on.

**C7 — Drizzle coverage**
- Stamp/count planes in `deposit_band`, the memory-estimate terms,
  `<drizzle stem>_coverage.fits`, the kind `drizzle_coverage`.
- Pins: drizzle output bit-identical on/off; counts match a synthetic geometry (per plane for
  Bayer).

**C8 — Crop**
- Pure helpers: rect, union, `crop_planar`, `crop_plate_solve`, rect×s.
- `native` inline; `coRegistered` deferred scratch-FITS + end-of-`stage_output` crop pass, with
  the uncropped fallback.
- Every output, the `master_lights` dims, the summary paths.
- Tests:
  - rect/union math;
  - CRPIX exactness (the same sky through the WCS before and after crop, SIP and TAN);
  - K = 0 identical;
  - `coRegistered` masters share one rect, `native` rects per group;
  - the XISF path;
  - the drizzle rect×s;
  - the rejection maps (no WCS) cropped;
  - the coverage map WCS shifted;
  - a crop failure keeps the masters.

Numeric gates: only C3/C4 change numbers, and only for project subjects. Every other commit is
pinned bit-identical for frame sets. Run A (Tier C rows 1-4, per group, full group) and run B
(reported) are the master-level gates at the end of 6b-2. A missed row is reported with its
numbers for an owner ruling, never tuned (Tier C rule).

Do not re-propose, from the reference's measured-out list:
- LN background on a binned plane (C-29);
- a wider seeds pre-select radius (C-13);
- `with_max_len(4)` and the integer accumulators in the engine (Task 9/10).

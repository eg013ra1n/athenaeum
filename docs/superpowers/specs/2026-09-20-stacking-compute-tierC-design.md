# Stacking compute Tier C — reuse and algorithm (design)

**Status:** owner decision 2026-09-20 — write the spec and execute it at once, no approval gate
("закончи tier a и напиши спек tier c и делай сразу c, не дожидаясь моего апрува"). Push stays on
the owner's word.

**Audit:** `docs/superpowers/research/2026-09-19-stacking-compute-audit.md` §3.1 (D1, D2, D6, D9),
§3.3 (I7, I8), §3.4 (Z3), §3.5 (L3, L4), §7. **Tier A:** plan
`docs/superpowers/plans/2026-09-19-stacking-compute-tierA-plan.md` — bit-identical, closed at
≈ 22–23 min on the reduced set (`LDN1272-test`, prod set 204: 92 mono + 105 OSC 180 s lights) from
26.6, with the registered frame materialized once (owner decision 2026-09-19).

## 0. Verdict this spec argues from

Tier A removed every duplicate that could be removed without moving a bit, and the kernel
rewrites the audit estimated at −25–40 % of the combine measured as regressions on this toolchain
(Task 9: a cached product loses to a register multiply, the std stable sort beats pdqsort at
n ≈ 200). What is left is **work that produces the same physical answer by a different route**:
LN re-detects and re-fits stars Measure already fitted (3.9 + 1.0 s per frame of its 5.2 s
`scale_ms`), drizzle clips a polygon with ≈ 70 f64 divisions per source pixel for a drop whose
shape is constant across a linear map, and the fitter pays three transcendentals per
sample-iteration. Tier C changes those routes. **Outputs move**, so its gate is not byte identity
but the M-run acceptance: masters against the external reference and against `tierA-baseline`
within stated tolerances.

Expected on the reduced set (g2/g3 numbers as the base): Normalize 5.8 → ≈ 2.5 min, Drizzle
4.3 → ≈ 1.2 min, Measure 4.4 → ≈ 3.5 min, Integrate 4.0 → ≈ 3.3 min — **≈ 22.3 → ≈ 15 min**;
on the full LDN 1272 set (368 frames) ≈ 49 → ≈ 28–30 min.

## 1. Scope

| # | Item | Class | Stage | Expected (reduced set) |
| - | ---- | ----- | ----- | ---------------------- |
| C1 | **LN takes Measure's fits** (D1) with β resolved once per group (D9) | numeric | Normalize | −3 min |
| C2 | **Drizzle phase table** (Z3): tabulated overlap areas, 0 divisions per pixel | numeric | Drizzle | −3 min |
| C3 | **Moffat fitter arithmetic** (D6): one `powf` per sample-iteration, no `ln` at fixed β, SoA samples, scratch reuse, residual reuse | numeric (last-bit) | Measure, LN, Register refine | −0.8 min |
| C4 | **`medfit_line` early exit + warm bracket** (I7, I8) | numeric (last-bit) | Integrate | −0.7 min |
| C5 | **LN background on a 4×4-binned plane** (L3) + `median_of_finite` from the stratified sample (L4) | numeric | Normalize | −0.3 min |
| C6 | **Register reuses Measure's fits for mono frames** (D2) | numeric | Register | −0.2 min |

Not in Tier C: I10 (IRLS line — recalibrates `LINEAR_FIT_SIGMA_SCALE`, R-M4a-17), Z4 (an
axis-aligned kernel as a user option), W2/D10/Z2 (Tier B leftovers worth ≈ 1 min together, not
worth a second acceptance), GPU, and Measure-fused-into-Calibrate (the measured `read_ms` is
38 ms per plane — the round trip through `calibrated/` costs nothing; the only gain would be
stage overlap, which the admission model does not support without its own design).

## 2. C1 — LN takes Measure's fits

### 2.1 Today
`ln/scale.rs::detect_seeds` runs the full `RankBudget` detector with centroid refine on the
frame warped into the reference geometry, then `psf_signal` fits every seed at the reference's β,
matches against the reference tree and takes the relative scale by RCR (`relative_scale_against`).
Measure (`stacking/measure.rs`) has already fitted the same stars on the calibrated, unwarped plane
with `NoiseRelative` seeds and Auto β — and discards `outcome.fits` at `measure.rs:469`, keeping
aggregates only. Per frame (g3 medians): `ln_detect_ms` 3 930, `ln_fit_ms` 1 021, `ln_match_ms` 26.

### 2.2 Design
1. **Persist the fits.** `measure_plane_with_seeds` returns its accepted `StarFit`s; the Measure
   stage writes them as a per-frame, per-plane artifact `kind = "fits"` beside `metrics`
   (`stacking_artifacts` row, payload file `<working_dir>/<set_slug>/fits/<group>/<stem>.p<plane>.bin`,
   a fixed-width little-endian record per fit: `x, y, flux, peak, sigma_x, sigma_y, theta, beta,
   snr, background, rms` as `f32` + flags `u8`; ≈ 200–400 KB per plane). Its hash = the
   measurement hash (`measurement_hash_for`) — the fits are a product of the same computation
   as `metrics`, so they share its staleness and its cleanup class (`CleanupWhat::All` only,
   like `metrics`).
2. **One β per group (D9).** `PsfModel::Auto` today resolves β per frame (the 64 brightest seeds
   fitted at four candidates). Stage 3 resolves β ONCE per group from the reference candidate's
   Auto pick — the group's best-weighted frame is not known until Measure finishes, so Measure
   runs as today (Auto per frame, cheap relative to the fits), records each frame's chosen β in
   `metrics`, and the group β is the MEDIAN of the members' Auto β's, written to the group's run
   row and the LN reference artifact. LN fits (the reference build's own detection, §2.2.4) use
   the group β; Measure's persisted fits carry their own β — the flux of a Moffat fit depends on
   β weakly (the audit measured < 0.5 % between adjacent candidates), and the ratio target/
   reference uses the same reference either way. Ruling C-1: the group β replaces the reference
   frame's β as `normalization.local.psfModel = auto`'s resolved value; `moffat4`/`gaussian`
   unchanged.
3. **Map, don't re-detect.** `normalize_frame` loads the target's `fits` artifact, maps each fit's
   position through `frame.map` (`PixelMap::forward_exact`, ruling R-T4-3 — the O(nodes) path, a
   few thousand calls per frame) into the reference geometry, corrects the flux by `|det J|` of
   the map's linear part at that point (a non-unit determinant appears under M4b native/
   cross-scale registration; for a same-rig Similarity it is `s²`), and hands the mapped list to
   the existing matcher → RCR path with NO change to `relative_scale_against`'s math. Fits whose
   mapped position falls outside the reference's coverage, or which fail Register's own
   saturation/eccentricity/SNR cuts (`register/detect.rs` constants, applied to the reused fits
   exactly as the detector applied them), are dropped before matching.
4. **The LN reference** (an integration of the group's best `referenceFrames`) has no Measure
   fits — it is not a frame. It keeps its own detection + fit (once per group, `NoiseRelative`
   seeds, the group β) — that is one detection per group instead of one per frame.
5. **The barycentre second pass** (R-T5-2) has no detection barycentres to fall back on; it is
   replaced by a second matching pass at a wider radius (`2 × LN_MATCH_RADIUS_PX`) on the same
   mapped fits when pass 1 covers < `LN_BARYCENTRE_PASS_THRESHOLD` of the target's fits. Ruling
   C-2: the wider-radius pass replaces the barycentre pass; the constant keeps its name and value.
6. **Fallback.** A frame with no `fits` artifact (an old catalog, a cleanup) or fewer than
   `LN_MIN_MATCHED_STARS` mapped fits inside coverage falls back to today's warped detection with
   one `warn!(frame_id, path, "ln: no measured fits, detecting on the warped frame")` — never a
   failure, and the master is still an honest LN master.
7. **Hashes.** `ln` artifacts already fold in `PSF_FIT_VERSION`; C1 bumps it (2 → 3), which
   invalidates every cached `metrics` (they must now carry the fits) and every `.athln` — the
   first Tier C run re-measures and re-normalizes every set once, as M4a did.

### 2.3 What moves
The relative scale `s` per frame: Measure's fit flux (native geometry, its own β) vs today's
warped-frame fit flux (reference geometry, the reference's β). The audit's estimate: the fluxes
differ by the interpolation kernel's flux non-conservation (< 0.3 % at bicubic B-spline) and the
β difference (< 0.5 %); `s` is a RATIO against the same reference so the systematic part cancels
across the group; the per-frame scatter is what RCR already absorbs. Acceptance §7 measures it.

## 3. C2 — drizzle phase table

### 3.1 Today
`deposit_band` (`stacking/drizzle/mod.rs`) per source pixel: five `fwd.at`, `map_drop` (8 finite
checks + a shoelace area), then 4–9 Sutherland–Hodgman clips of ≈ 100–120 f64 ops with ≈ 8
divisions each — 422 ns ≈ 1 700 cycles per pixel; `deposit_ms` ≈ 42–44 s per plane on the reduced
set. Tier A's Z1 (Task 11) skips the pixels whose drop cannot touch the band; the remaining pixels
still pay the clip.

### 3.2 Design
For a **linear** map (Similarity/Affine/Homography without distortion) the mapped drop is one
parallelogram for the whole frame; only its sub-pixel phase `(fx, fy) = frac(to_output(centre))`
varies. Tabulate once per frame: for a `PHASES × PHASES` grid of phases (`PHASES = 32`), the
overlap area of the parallelogram at that phase with each of the ≤ 9 (3×3 at `scale = 2`, ≤ 16 at
`scale = 3`) output pixels its bounding box touches — `32 × 32 × 9 × f32 = 37 KB` per frame,
computed with the SAME `clip_area` the per-pixel path uses (so the table's entries are exactly
what the exact path would compute at those phases). Per source pixel: `fwd.at` once → floor +
phase index (2 multiplies, 2 floors) → up to 9 table reads + multiply-adds into the accumulators.
No division. With **distortion** (polynomial/TPS) the parallelogram varies slowly: the table is
rebuilt per `TILE = 256` px source tile from the map's local Jacobian at the tile centre
(`PixelMap::forward_exact` at the four corners → the affine part), a 37 KB build per tile
(≈ 400 tiles on 26 Mpx ⇒ 15 MB of arithmetic per frame, negligible).

**Level preservation** (R-M3-2) holds by construction: each source pixel's table row sums to the
drop's area (the clips partition the drop), so `Σ w = area` exactly as before up to the phase
quantization. **What moves:** a pixel's phase is rounded to `1/32` of an output pixel, so its
area split among the neighbours differs from the exact clip by at most the area swept by a
`1/32`-pixel shift — the audit measured the per-frame weight change at ≤ 1.6 % (nearest) /
< 0.1 % (bilinear) and the master-level change at 0.1 % / 0.007 % over 208 frames. Ruling C-3:
`PHASES = 32`, one table per frame for linear maps and per 256-px tile under distortion; the exact
clip stays as the reference path under `#[cfg(test)]` and as the runtime path for `scale = 1` with
`dropShrink = 1.0` (there the table is a 1×1 identity — not worth a build).

Bayer drizzle (`cfa_plane_of`) is unchanged: the phase table is per plane-agnostic geometry; the
colour routing selects which accumulator receives the deposit.

## 4. C3 — Moffat fitter arithmetic (D6)

`rustafits/src/analysis/fitting.rs::fit_moffat_2d_impl` evaluates per sample per iteration
`base^(-β)`, `base^(-β-1)` and `ln(base)` — three transcendentals. C3: `p = base.powf(-β)`,
`base^(-β-1) = p / base` (one `powf`, one division), `ln(base)` only when β is a free parameter
(with `PsfModel::Moffat4`/the group β it is fixed → no `ln`), `powi` when β is integral; samples
stored as SoA `f32` with implicit grid coordinates (the stamp is a rectangle — `x`/`y` come from
the index); `cholesky_solve` on a thread-local scratch; the accepted step's residuals reused by
the next Jacobian pass instead of recomputed. Every one of these is the same real-valued function
evaluated with different rounding: the fit's fixed point moves by ≈ 1e-6 relative in position and
flux (the audit's estimate; measured in §7 as the metrics' drift). Ruling C-4: no change to the LM
control flow (damping, acceptance, iteration cap) — those are what M4a calibrated.

## 5. C4 — `medfit_line` early exit and warm bracket (I7, I8)

`integration/combine.rs::medfit_line`: the bracket is re-derived as `b ± 3σ_b` on every call and
bisected 12 times; the outer rejection loop runs a confirming iteration that rejects < 0.1 % of
samples. C4: (I8) warm-start the bracket from the previous iteration's `(b, width/4)` and bisect
until the width is below the same tolerance (≈ 4 halvings instead of 12 on a warm start), with
the cold bracket as the fallback when the warm one does not contain a sign change; (I7) stop the
outer loop when an iteration rejected nothing new (today it runs one more to confirm). The
tolerance `1e-3 σ_b` is NOT loosened (I9 dropped: its −1.2 min is not worth widening the
dispersion the R-M4a-17 calibration rests on). What moves: the slope's last bits (the bisection
converges to the same root within tolerance from a different start) and, for a stack whose
confirming iteration would have rejected one more sample at the boundary, that sample. The
rejected fraction is an acceptance metric (§7).

## 6. C5 — LN background on a binned plane (L3, L4)

`background_grid` models the background on a `scale/8 = 128` px node mesh; it reads the full
26 Mpx plane. C5: clip-then-bin the plane 4×4 (mean of the 16 finite pixels, NaN if fewer than 8
are finite) before `clean_plane`'s passes and the cell loop — the model lives on a 128-px stride,
so a 32-px-stride input carries it; `median_of_finite` (the placeholder value) from the existing
1/16 stratified sample. Both change the node values at the 1e-3 relative level (the binned mean
is a smoother estimator than the per-pixel median inside a 128-px cell); the `.athln` sidecars
move accordingly. Ruling C-5: `LN_BIN = 4`, applied to the reference model and every target
identically.

## 7. C6 — Register reuses Measure's fits for mono frames (D2)

For a single-plane frame Register's luminance IS the measured plane (`luminance(&[p]) == p`),
so Register's `RankBudget` detection + centroid refine can be replaced by a projection of the
`fits` artifact (`StarFit → Star`: position, flux, size; the fitted centroid is better than the
refine's). The quad seed, RANSAC and distortion fits are unchanged; the star LIST differs (Measure's
`NoiseRelative` seeds at the group cap instead of `RankBudget` at 2 000). Ruling C-6: the reused
list is truncated to the same `max_stars` by flux, and OSC frames keep the luminance detection
(Measure fits per channel; a luminance fit does not exist). Fallback: no `fits` artifact ⇒
today's detection with a `warn!`.

## 8. Acceptance (the gate)

One run of the reduced set (`checkpoint.sh tierC`), compared against `tierA-baseline` AND against
the external reference's masters of the same frames (the M-run harness:
`docs/superpowers/research/scripts/`):

| Metric | Tolerance |
| ------ | --------- |
| Master pixel median / MAD vs tierA-baseline | ± 0.1 % / ± 1 % |
| Master noise (MRS σ) vs baseline | ± 2 % |
| FWHM (mono, OSC per plane) vs baseline | ± 1 % |
| Rejected fraction (linear fit) vs baseline | ± 0.3 pp (baseline 2.985 / 2.733 %) |
| Per-frame weights: Spearman ρ vs baseline | ≥ 0.99; top-20 overlap ≥ 18/20 |
| LN relative scale `s` per frame vs baseline | median ratio 1 ± 0.5 %, scatter ≤ 1 % |
| Drizzled master level vs undrizzled (R-M3-2) | 0.998–1.002 (baseline 0.9987–0.99999) |
| Drizzle coverage | 1.0 on every plane |
| Wall (interleaved with a fresh baseline re-run, R-TA-9) | reported, target ≤ 16 min |

Each item lands behind its own config-hash bump (the stage whose artifacts it moves) and its
own pin against the pre-change code recording the DELTA (not identity) on the fixtures, so a later
regression is caught at the fixture level, not only at the acceptance run.

## 9. Rulings (proposed, decided at plan time)

C-1 group β = median of the members' Auto β; C-2 wider-radius pass replaces the barycentre pass;
C-3 `PHASES = 32`, per frame for linear maps, per 256-px tile under distortion; C-4 LM control
flow untouched; C-5 `LN_BIN = 4`; C-6 mono-only fit reuse in Register, OSC keeps luminance
detection; C-7 (process) one implementer at a time, interleaved before/after measurement for
every timing, the product-build checkpoint as the arbiter, an honest revert with numbers when an
item measures ≤ 0 (Tier A's rulings R-TA-8/9 carried over).

**C-1a** (Task 1, plan ledger): the group β is the LOWER median of the members' `Auto` β's
(`psf_signal::group_beta`, sorted, index `(n-1)/2`), not their mean — a plain average of two
adjacent `AUTO_BETAS` candidates (e.g. `4.0`/`6.0` → `5.0`) is not a value `Auto`'s own search
ever produces, and Task 2's `PsfModel::Fixed(beta)` fits at exactly this number. "Members" means
every frame stage 3 actually measured (fresh or reused from a cached `metrics` row) — every
channel of every such frame contributes one β observation, not narrowed to `included` (weighing/
selection happens after Auto has already resolved a β for each channel). A group that measured
no frame at all records no β (`None`).

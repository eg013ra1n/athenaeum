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
4.3 → ≈ 1.2 min, Measure 4.4 → ≈ 3.5 min, Integrate 4.0 → **4.0 min (unchanged — C4 measured out, §5)** —
**≈ 22.3 → ≈ 15.7 min**;
on the full LDN 1272 set (368 frames) ≈ 49 → ≈ 28–30 min.

## 1. Scope

| # | Item | Class | Stage | Expected (reduced set) |
| - | ---- | ----- | ----- | ---------------------- |
| C1 | **LN takes Measure's fits** (D1) with β resolved once per group (D9) | numeric | Normalize | −3 min |
| C2 | **Drizzle phase table** (Z3): tabulated overlap areas, 0 divisions per pixel | numeric | Drizzle | −3 min |
| C3 | **Moffat fitter arithmetic** (D6): one `powf` per sample-iteration, no `ln` at fixed β, SoA samples, scratch reuse, residual reuse | numeric (last-bit) | Measure, LN, Register refine | −0.8 min |
| C4 | **`medfit_line` early exit + warm bracket** (I7, I8) | numeric (last-bit) | Integrate | ~~−0.7 min~~ **0 — measured out, see §5** |
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
3. **Map positions, refit on the warped plane — do NOT compare fitted flux (fix round 2, ruling
   C-12).** The first implementation of this item mapped each fit's position through `frame.map`
   and corrected the flux by `|det J|` of the map's linear part, comparing that corrected flux
   directly against the reference's own fit. A diagnostics round (ruling C-11) measured that
   design at up to ~20% bias on real undersampled frames and traced it to §2.3's own H2 finding
   below — the fix drops flux comparison entirely. `relative_scale_from_seeds` instead: maps each
   fit's centroid through `frame.map` (`PixelMap::forward_exact`, ruling R-T4-3 — position only,
   no `|det J|` correction, since there is no flux to correct any more), drops one whose mapped,
   rounded position falls outside the reference's coverage or whose value on the WARPED target
   plane fails a saturation guard (native `[0, 1]` units, `register::detect::SATURATION` — the one
   piece of the old cuts this path still needs, since Measure's own acceptance already covers
   eccentricity/SNR on the native frame but says nothing about the warped pixel a fresh fit is
   about to sample), pre-selects against the reference's own match tree (a seed with no nearby
   reference star can never produce a matched pair), then RE-FITS every survivor on the WARPED
   target plane at the reference's DEFAULT β (`psf_signal::fit_stars_with_beta`) — the SAME fit
   call `relative_scale_against` makes, just fed a pre-selected seed list instead of a full-frame
   `detect_seeds` one. The existing matcher → RCR path runs with NO change to its own math, fed
   this fresh fit outcome instead of a detected one.
4. **The LN reference** (an integration of the group's best `referenceFrames`) has no Measure
   fits — it is not a frame. It keeps its own detection + fit (once per group, `NoiseRelative`
   seeds, the group β) — that is one detection per group instead of one per frame.
5. **The barycentre second pass** (R-T5-2) has no detection barycentres to fall back on — the
   seeds are Measure's own mapped positions, not a fresh detection's, whether or not the fit that
   follows is on the target's own flux (the original design) or a fresh warped-plane fit (ruling
   C-12's own fix); it is replaced by a second matching pass at a wider radius
   (`2 × LN_MATCH_RADIUS_PX`) on the same mapped fits when pass 1 covers <
   `LN_BARYCENTRE_PASS_THRESHOLD` of the target's fits. Ruling C-2: the wider-radius pass replaces
   the barycentre pass; the constant keeps its name and value.
6. **Fallback.** A frame with no `fits` artifact (an old catalog, a cleanup) or fewer than
   `LN_MIN_MATCHED_STARS` mapped fits inside coverage falls back to today's warped detection with
   one `warn!(frame_id, path, "ln: no measured fits, detecting on the warped frame")` — never a
   failure, and the master is still an honest LN master.
7. **Hashes.** `ln` artifacts already fold in `PSF_FIT_VERSION`; C1 bumps it (2 → 3), which
   invalidates every cached `metrics` (they must now carry the fits) and every `.athln` — the
   first Tier C run re-measures and re-normalizes every set once, as M4a did.
8. **Per-group seeds calibration (fix round 3, ruling C-13, LEVER 2; re-specified by fix round 4,
   ruling C-14).** Fix round 2's own 13-frame
   table (§2.3 below) found the seeds path's scale reading SAME-SIGNED higher than today's
   detection path on every real frame, median +0.666 %, max +1.401 % — small against the 1.5 %
   ceiling but a same-signed bias moves a master's LEVEL (spec §8's median ± 0.1 % target), so it
   had to go. LEVER 1 (relax the seeds path's own pre-select/saturation filters to recover more of
   the population) was measured directly (`SeedFilterBreakdown`, `--diag`) and found NOT the cause:
   on a dense mono frame the saturation guard drops 0 seeds, the pre-select-against-the-reference-
   tree step is the dominant nominal loss (16502 → 9198, ≈ 44 %), but widening it to `2 ×
   match_radius_px` or removing it entirely left `matched`/`scale` unchanged to three significant
   figures while roughly doubling `scale_ms` — the lost stars have no reference counterpart at
   either radius, so the filter was kept as shipped in fix round 2 and LEVER 1 landed nothing.
   LEVER 2 ships instead — `stacking::ln::calibration`, driven once per group by
   `stacking::run::measure_ln_seeds_calibration` before stage 6's fan-out.

   **The sample** (ruling C-15) is the group's registration/geometry reference frame
   (`RunContext::geometry_of(&group.key).reference_frame_id`) plus ONE member per weight SEXTILE
   of the OTHER included members — sort the others by `FrameWeight::normalized_mean` descending
   (ties on the frame id, so two runs over the same catalog pick the same frames), split into six
   contiguous bins as evenly as the count allows (the remainder goes to the leading bins), and
   take each bin's median-weight member: `SEEDS_CALIBRATION_FRAMES = 7` either way. A group with
   no more others than slots measures all of them.

   Ruling C-14 originally said "the six best-weighted", and fix round 4's hold-out measured why
   that is wrong: `k` is a median, so it centres on the SAMPLE's own median bias, and on the
   acceptance catalog's mono group the six best-weighted frames were one contiguous 30-minute
   window at FWHM 1.99–2.29 px against a group reaching 5.08 px — their median bias (+0.554 %)
   sat below the group's own (+0.992 %), leaving a **+0.435 % systematic** on every frame outside
   the window, which is exactly the quantity §8's ±0.1 % master-median row cannot absorb.
   Stratifying puts the median on the group's middle for the same seven `normalize_frame` pairs.
   (Fix round 3 used three frames, which C-14 raised to seven after the review observed that a
   3-sample median of a bias spanning +0.1…+1.4 % carries roughly ±0.3 % of common-mode sampling
   error; seven made `k` STABLE, C-15's stratification makes it REPRESENTATIVE.)

   **The honest cost**: each calibration frame runs BOTH arms of `normalize_frame`, and the
   detection arm is the ≈ 6 s per-frame path the seeds design replaces, so the measurement is
   ≈ 7 × 6 s ≈ 45 s per group (measured: 40.7 s mono / 49.8 s OSC on the reduced set). Against
   it: the seeds path saves ≈ 2.5 s on every one of a group's 90–160 frames, i.e. ≈ 4–7 min.
   **Break-even is ≈ 10 members** — below that a group pays more to calibrate than the seeds path
   saves it, and nothing scales the sample down between the floor (a group with no more others
   than slots measures all of them) and there; a 10-frame group is the worst case and is accepted
   as such. The run emits
   a `stacking-progress` message (`"calibrating the seeds path · frame i/7"`) while it happens, so
   the Normalize row says what it is doing instead of appearing stalled. A group whose members are
   ALL cache hits pays nothing: the measurement runs only when a fresh stored calibration is
   missing AND at least one member would be re-normalized anyway.

   **`k` is per CHANNEL** (`SeedsCalibration { frame_ids, k }`): for each calibration frame the
   measurement collects `s_detected / s_seeds` per channel — only from channels whose SEEDS arm
   genuinely took the seeds path, since a channel that fell back compared detection against
   detection — and takes the MEDIAN per channel (odd `n` the natural middle, even `n` the LOWER of
   the two middles: an interpolated mid-point is not one of the measured ratios). A channel with
   fewer than `SEEDS_CALIBRATION_MIN_RATIOS = 3` usable ratios runs uncalibrated (`k = 1`), with
   ONE `warn!` for the whole group. A median outside `1 ± SEEDS_CALIBRATION_BAND` (`0.03`, about
   TWICE the largest per-frame bias measured for this path — ≈ 2.1 × the +1.449 % of frame 29047
   in fix round 5's hold-out; C-14 said "four times", which never matched the measurement, and
   C-17 corrects the claim while leaving the value) is REFUSED outright — `warn!`,
   `k = 1` for that channel — rather than applied or clamped: a factor beyond that band is not a
   seeds-path bias, and applying it would move the master's level by more than the defect it
   claims to correct. Channels are independent: an OSC group's blue plane being refused says
   nothing about its red.

   **Determinism.** The measurement's inputs are hashed as the group's own `ln_calibration`
   artifact (`seeds_calibration_hash_for`: the normalization subtree, the group's LN reference
   hash, and the calibration frames' ids + registration AND measurement hashes), and its RESULT —
   the frame ids and
   the per-channel `k` rounded to 1e-6 — is folded into every member's `normalization_hash_for`.
   **Both LN hashes fold the frame's MEASUREMENT hash** (`measurement_hash_for`, the value its
   `fits` artifact is keyed on) since C-17: `normalization_subtree` carries only
   `measurement.psfModel`/`maxStars`, so without it a changed `detectionSigma`, `seedDetector`,
   `seedPrefilter` or `structure` rewrote every `.athf` while the plan gate reported Normalize
   cached and the run reused both the sidecars and the stored `k`. The gate and the run pass the
   SAME per-frame value (empty on both sides when it cannot be resolved), and the group's LN
   reference hash folds the sorted join of its reference members' measurement hashes the same way
   it already folds their registration hashes.
   A changed `k` therefore invalidates the whole group's sidecars rather than leaving a group
   mixing frames normalized at two different factors; a re-registered calibration frame
   re-measures `k` and, through the same fold, re-normalizes the group. The plan gate reads the
   stored `ln_calibration` payload and hashes with it, so it and the run agree about what "fresh"
   means without the gate ever measuring anything. **Documented residual**, beside the stored LN
   reference member list's own: the gate does NOT re-derive whether that stored calibration is
   itself stale — doing so would need the stage-3 weight ranking that picks the sample, which a
   DB-only gate does not have — so a run whose calibration IS stale re-measures and re-normalizes
   while the gate had reported the group fresh. One run's optimism, not a wrong master. The value
   is also recorded per frame
   (`LnArtifactPayload::seeds_calibration`, beside `ln_scale_source`) and per group in the run
   summary (`SummaryGroup::seedsCalibration`).

   **Application.** `normalize_frame` takes `seeds_calibration: Option<&[f64]>` (one entry per
   channel; a short slice or `None` reads as `k = 1`). A channel that actually took the seeds path
   is calibrated; a channel that fell back to detection is not, by definition — LEVER 1's own
   diagnostics found no bias on that path to correct. A channel carrying a fitted local-scale
   spline (`normalization.local.localScale`) IS calibrated, by scaling the SAMPLED `A` surface —
   `A(x, y) = k · (s + spline(x, y))`, with `B = B_ref − A·B_tgt` following from the scaled `A` —
   rather than being skipped as fix round 3 did: the spline is a residual AROUND `s`, so scaling
   the whole sampled value keeps the two consistent by construction, and leaving such a channel
   uncalibrated was the larger inconsistency. `a_grid`'s safety band is measured on the UNSCALED
   deviation, which for `k > 0` is the identical test, so no surface's accept/refuse verdict moves
   because `k` exists.

   **Which path a channel took is recorded**, and the calibration's own detection arm is told
   apart from a genuine fallback: `normalize_frame`'s seed input is
   `LnScaleSeeds::{Measured, NoFits, ForcedDetection}`, and only the first two make a detection run
   warn ("no measured fits, detecting on the warped frame") or count as a fallback. The
   calibration's seven detection runs are the POINT of the call, not a defect to report.

### 2.3 What moves
The relative scale `s` per frame: Measure's fit flux (native geometry, its own β) vs today's
warped-frame fit flux (reference geometry, the reference's β). The audit's estimate: the fluxes
differ by the interpolation kernel's flux non-conservation (< 0.3 % at bicubic B-spline) and the
β difference (< 0.5 %); `s` is a RATIO against the same reference so the systematic part cancels
across the group; the per-frame scatter is what RCR already absorbs. Acceptance §7 measures it.

**Fix round 2 finding (ruling C-11/C-12): the audit's estimate above was wrong by roughly an
order of magnitude on real frames.** A diagnostics round measured that a Moffat fit's own `signal`
is NOT warp-invariant — fitting the IDENTICAL star on the native calibrated frame versus on the
same frame warped into the reference geometry (bicubic B-spline) integrates measurably different
flux, growing with how undersampled the star's native PSF is relative to the resampling kernel:
~0.5% on well-sampled real frames, up to ~19–20% on the sharpest ones in the acceptance catalog.
An aperture SUM over the same pixels — no fit model involved — conserves flux through the warp to
≤ 0.6% on the SAME frames, so the divergence sits entirely in what the FIT extracts, not in the
pixels; a same-star-set control (restricting a full-detection oracle to exactly the population
`relative_scale_from_seeds`'s own pre-select would keep) moved the RCR location by only 0.3–1.3%,
ruling out sample selection as the dominant cause. Today's `relative_scale_against` was never
exposed to this: it fits the reference (itself an integration of already-warped frames) and the
target (also warped) on planes of the SAME KIND, which is why that comparison stays self-
consistent and is kept as the baseline the seeds design (§2.2 item 3) is checked against, rather
than being replaced.

**Why a residual bias exists at all — a plausible mechanism (fix round 4, the review's M5).** The
seeds path and the detection path fit the SAME warped plane at the SAME β; what differs is where
each fit's `initial_sigma` comes from. `detect_seeds` hands the fitter a detection's own
`peak`/`flux` measured ON THE WARPED PLANE, while the seeds path copies Measure's `amplitude`/
`signal`, measured on the NATIVE one — and the field-level `initial_sigma` heuristic those two
numbers feed sizes the fit stamp. A stamp sized from native photometry is systematically a little
different from one sized from warped photometry on exactly the undersampled frames where the warp
changes a fit's integrated flux most (the H2 finding above), which is the shape of the residual
`k` absorbs. This is stated as a mechanism, not a measurement: nothing in fix round 3 or 4 isolated
it, and the per-group factor removes the bias whatever its cause.

**Fix round 3 finding (ruling C-13, LEVER 2): the per-group calibration factor closes the
remaining bias.** Re-measuring the same 13-frame table with `k` applied (§2.2 item 8): median
|Δ| 0.666 % → 0.182 %, max |Δ| 1.401 % → 0.722 % — both comfortably inside the ruling's own
"median ≈ 0, max ≈ 0.7 %" expectation — and, more importantly, the residual is no longer
SAME-SIGNED: 6 of 13 frames now read below the detection baseline, 7 above (mono: −0.29, +0.27,
−0.26, +0.01, +0.72, −0.01, −0.59, +0.38 %; OSC: +0.16, 0.00, −0.15, +0.18, −0.12 %), consistent
with genuine per-frame scatter around zero rather than a systematic offset a master's level would
inherit. `matches` is unaffected by design (LEVER 2 only rescales the already-matched sample, it
does not touch pairing) — fix round 2's own shortfall against the "≥ 0.8×" bar on mono frames
(0.70–0.77×, LEVER 1's own unsuccessful target) is unchanged by this round.

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

## 5. C4 — `medfit_line` early exit and warm bracket (I7, I8) — **measured out**

**Outcome (ruling C-26): I7 was already implemented, I8 was implemented, measured and reverted.
C4 contributes 0 to Tier C.** The section below keeps the original proposal for the record,
followed by what the measurement found.

### The proposal, as designed

`integration/combine.rs::medfit_line`: the bracket is re-derived as `b ± 3σ_b` on every call and
bisected 12 times; the outer rejection loop runs a confirming iteration that rejects < 0.1 % of
samples. C4: (I8) warm-start the bracket from the previous iteration's `(b, width/4)` and bisect
until the width is below the same tolerance (≈ 4 halvings instead of 12 on a warm start), with
the cold bracket as the fallback when the warm one does not contain a sign change; (I7) stop the
outer loop when an iteration rejected nothing new (today it runs one more to confirm). The
tolerance `1e-3 σ_b` is NOT loosened (I9 dropped: its −1.2 min is not worth widening the
dispersion the R-M4a-17 calibration rests on).

### What the measurement found (Task 6)

**I7 — the exit already exists**, and has since the original linear-fit clipper (`e8512317`):
`if w == kept { break; }` ends the loop on the first pass that rejects nothing. That pass IS the
confirmation — it runs a full `medfit_line` and filter sweep to learn the survivor set has
stopped moving — and skipping it cannot keep the same survivors. Measured: the final iteration
rejected 0 samples on 1 000 of 1 000 seeded stacks; `rejection_iters_mean` on the real probe is
1.588, and no change to the loop can lower it. Commit `fdab9ba1` pins the exit (with teeth:
deleting it makes the loop run all 20 passes) and changes no production code. The premise "today
it runs one more to confirm" was simply not true.

**I8 — built, measured, reverted.** Implemented as `3·σ_b/8` with three extra widenings (which
walk the bracket back up to the cold one exactly, so no fallback branch and a warm call can never
bracket less than a cold one), pinned against a verbatim pre-change reference, and measured with
`integrate_probe --limit 60 --rejection linearFit` on 60 real mono frames, interleaved B/A x5:

| metric | before | after | ratio |
| ------ | ------ | ----- | ----- |
| `medfit_evals_mean` | 22.690 | 21.466 | **0.946x** |
| `combine_cpu_ms` (median of arms) | 286 312 | 276 650 | **0.966x** (≈ 9 s per run) |
| `rejected_fraction` | 0.012 475 337 | 0.012 474 010 | −0.000 13 pp |
| `rejection_iters_mean` | 1.588 034 | 1.587 973 | −0.004 % |

Reverted under **ruling C-26**: a ≈ 3 % combine gain does not buy a numeric change to the
rejection kernel, and the item's own bar (`medfit_evals_mean` ≤ 0.5x) was missed rather than met
— re-sizing the bar to the 0.95x achieved would be tolerance-tuning. Four findings stand, and are
recorded in `medfit_line`'s doc comment so the shape is not re-proposed:

- **≤ 0.5x is unreachable by ANY change confined to the warm bracket.** The outer loop runs ~1.6
  iterations per pixel stack on a real plane, so the COLD first call — which has no prior slope
  and must not move — is about half of all evaluations; free warm calls would still land near
  0.63x.
- **The "≈ 4 halvings instead of 12" premise is false.** The slope moves between iterations as the
  extreme samples leave the survivor set: `|Δb|` p50 = **216 tolerances**, p90 = 929 (4 000 seeded
  stacks). A bracket narrow enough for 4 halvings misses the root almost always and pays two
  evaluations per widening to find it again.
- **A carried half-width is worth nothing** — simulated at 1.00x. `σ_b` collapses between
  iterations, so a width in absolute slope units is stale when used; only a multiple of the
  current call's `σ_b` is scale-free.
- **Any narrower bracket is a real numeric change, not a last-bit one.** 40 of 1 000 warm calls
  converge to a genuinely different root — inherent to the estimator (`f` is an integer-valued
  step function whose zero set is a plateau, so the minimum-absolute-deviation line is not unique
  and both roots are equally valid; the warm line's objective was on average 2.5e-6 BELOW the
  cold line's and never 0.151 % above it) — which moved ~2 000 of 26 M rejection decisions per
  plane. Every master built at n ≥ 20 would have differed.

Full measurement: `.superpowers/sdd/2026-09-20-stacking-compute-tierC-plan/task-6-report.md`.

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

**C-26** (Task 6, coordinator): C4 is reverted. `medfit_line`'s warm bracket measured 0.946x
evaluations and 0.966x `combine_cpu_ms` on real data — a ≈ 3 % combine gain does not buy a
numeric change to the rejection kernel, and the item's ≤ 0.5x bar was missed, not met (re-sizing
it to the achieved 0.95x would be tolerance-tuning). The pin of the already-existing
exit-on-zero-rejection (I7) stands; C4's expected −0.7 min becomes 0. See §5.

**C-1a** (Task 1, plan ledger): the group β is the LOWER median of the members' `Auto` β's
(`psf_signal::group_beta`, sorted, index `(n-1)/2`), not their mean — a plain average of two
adjacent `AUTO_BETAS` candidates (e.g. `4.0`/`6.0` → `5.0`) is not a value `Auto`'s own search
ever produces, and Task 2's `PsfModel::Fixed(beta)` fits at exactly this number. "Members" means
every frame stage 3 actually measured (fresh or reused from a cached `metrics` row) — every
channel of every such frame contributes one β observation, not narrowed to `included` (weighing/
selection happens after Auto has already resolved a β for each channel). A group that measured
no frame at all records no β (`None`).

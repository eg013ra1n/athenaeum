# Local normalization model fixes and a calibration pedestal warning — design

**Status:** owner decision 2026-10-04 — one cycle, five items ("делаем все пункты одним циклом").
Written spec APPROVED by the owner 2026-10-04 (fable review "ready after fixes", fix round 1 folded,
§15 points 1–17 accepted — see §15). Push stays on the owner's word.

**No migration (owner, 2026-10-04).** Existing runs, run rows, provenance and `master_lights` are
left exactly as they are. Nothing rewrites stored history. The new model reaches existing sets only
through the normal cache invalidation of §7 (the version bump makes the Normalize stage stale; the
next run re-normalizes). The legacy config rule of L-9 is a read-time resolution in
`resolve_config`, not a stored-data rewrite. No migration code, no one-off scripts.

**Branch:** `ln-model-fixes`, cut from `main` 60df59c4.

**Research basis** (read before this spec):

- `docs/superpowers/research/2026-10-04-ln-scale-psf-width-audit.md` — the scale estimator's
  PSF-width bias (F1 is its §6 item 1).
- `docs/superpowers/research/2026-10-04-ln-math-review.md` — the LN model beside the external
  reference pipeline (ERP); §5 is the Horse C11 halo, §8 the ranked fixes and their bars.
- `docs/stacking/README.md` (M2, M4c, Tier C), the stacking spec
  `2026-09-08-stacking-pipeline-design.md` (§5.2, §9.2, §9.3), the Tier C spec
  `2026-09-20-stacking-compute-tierC-design.md` (§8 tolerance table, rulings C-1…C-30) and the math
  reference `2026-09-08-stacking-math-reference.md` §4.

Rulings this spec makes are numbered **L-1 … L-22** and collected in §14. §15 lists the points put to
the owner; all were accepted on 2026-10-04.

## 0. Verdict and scope

Two defects are measured on real data.

1. **The background model is a cell model, not a model at the scale.** Each node of the `scale/8`
   mesh is the robust level of its own 128-px cell. A cell that loses more than 30 % of its pixels
   to the clip is refilled from its neighbours. A star's wings take 24 % of a cell in a noisy frame
   and 29–31 % in the low-noise LN reference, so the gate flips between the two. On Horse C11 G
   this removes one node's worth of halo (−30 % of background) from the master: the 80–90 px ring
   reads +4.1 % with LN against +12.2 % without it.
2. **The scale estimator reads a partial-aperture flux.** The fitted tenth-maximum ellipse holds
   0.78–0.93 of a real star's light, depending on profile shape. The measured gap to photometric
   truth runs at −3.7 … −4.6 % per px² of PSF-variance difference. Corrected, set 204's mono master
   gains +5.53 % star flux.

The cycle ships five items:

| # | Item | Changes output | Section |
| ---- | ---- | ---- | ---- |
| 1 | Background model at the scale | every LN master's sky | §1 |
| 2 | Scale derived from the image size | the scale on large sensors | §2 |
| 3 | Star selection for the scale (peak SNR, brightest, relative saturation) | every LN scale, a little | §3 |
| 4 | F1: a common-aperture photometric scale estimator, beside the PSF-flux one | signal level, if it becomes the default | §4 |
| 5 | Calibration warning: a flat without a dark or bias | nothing (advisory) | §5 |

**Out of scope** (stated so a reviewer does not expect them):

- The registration interpolation default (B-spline vs Lanczos-3). It is a later cycle, after this
  one; with F1 the kernel stops biasing LN (audit H6).
- Dark-free hot-pixel detection.
- The LN reference-frame count rule (math review §8 item 5).
- Rejection clipping sharp cores in a mixed-seeing group (audit §9 item 4).
- The unexplained ≈ 4.5 % on run B mono (audit §9 item 3).
- The calibration matcher's missing geometry check (open-items, "Calibration matching ignores
  frame geometry"). §11.4 works around it on the acceptance copy.
- A dark-brighter-than-lights guard.
- Frontend beyond the LN fields of the Normalize panel (both places it renders), the plan's
  derived-scale line, and the warning text.

No command is added or removed. Every new field rides an existing payload on both hosts.

## 1. Background model at the scale (item 1)

### 1.1 What goes and what stays

**Retired** (`stacking/ln/background.rs`):

- the per-cell node (`robust_cell_level` over the node's own stride×stride cell, with 2×2
  subsampling above 4096 px);
- the cell validity gate (`BackgroundParams::rejection_limit = 0.3`);
- the neighbour fill of invalid cells (`fill_invalid_cells`);
- the asymmetric deviation thresholds (`DEFAULT_PARAMS.deviation_sigma = 3.0` for the reference,
  `TARGET_DEVIATION_SIGMA = 3.2` for a target);
- the full-plane cleaned copy (`clean_plane`).

**Kept unchanged:**

- the hot-pixel pass (radius 2, a pixel above 0.85 of full scale and above median + 5σ of its
  5×5 window is replaced by that median) and the clip `[4.5e-5, 0.85 of full scale]`, with the
  2026-09-10 controller ruling that 0.85 means 0.85 of full scale;
- the mesh: node `(i, j)` at pixel `(i·stride, j·stride)`, `stride = scale/8`, `(gw, gh)` from
  `LnGrid::grid_dims`, the trailing node clamped into the plane;
- the bicubic B-spline evaluator (`grid.rs`), its ruling on the smoothing kernel, and its pins;
- the `.athln` binary format (`VERSION = 1`);
- `B = B_R − A·B_T` on the mesh and `v′ = A·v + B` in integration and drizzle.

### 1.2 The algorithm

One procedure, run on the LN reference plane and on every target plane with the same constants
(L-1). Steps 1–6 run on each image alone; step 7 runs on each reference–target pair; step 8 writes
the nodes. All coordinates are in the reference geometry.

**Step 1 — tiles.** The plane is cut into non-overlapping tiles of `D = 32` px anchored at
`(0, 0)` (L-2). Tile `(u, v)` covers `[32u, 32u + 32) × [32v, 32v + 32)`, clipped at the plane
edge. A 9576 × 6388 plane has 300 × 200 = 60 000 tiles.

**Step 2 — cleaning.** The hot-pixel pass and the clip run tile by tile. A tile reads a 2-px apron
from the original plane for the hot-pixel window, so the result equals the full-plane pass pixel
for pixel. A pixel is *present* when it is finite and inside the clip range after the pass.

**Step 3 — tile statistic.** A tile with at least 256 present pixels (a quarter of a full tile) is
a *sample*. On its present pixels:

- iterate `median ± k_t · 1.4826 · MAD`, `k_t = 3.0`, at most 5 rounds, stopping when a round keeps
  every pixel or when `MAD = 0` (the existing loop, without its rejected-fraction test);
- sample value `y` = the median of the kept pixels;
- sample noise `σ_t` = `1.4826 · MAD` of the kept pixels;
- sample position = the centre of the tile's pixel extent, `((x0 + x1 − 1)/2, (y0 + y1 − 1)/2)`.

Every tile, sample or not, also records the maximum of its cleaned finite pixels taken before the
clip. The plane maximum for §3 is the maximum over tiles (L-14).

**Step 4 — smooth surface.** A tensor-product uniform cubic B-spline with knot spacing
`h = scale` (L-3):

- along x, `n_x = ceil((W − 1)/h) + 3` basis functions on knots `t_k = (k − 3)·h`; along y the
  same with `H`;
- coefficients `c` minimise `Σ_samples (y_s − S(x_s))² + λ · Σ_adjacent (c_a − c_b)²`, where the
  penalty runs over every pair of coefficients adjacent along x or along y (a first-difference,
  "membrane" penalty);
- `λ = 1e-4 ×` the median of the non-zero diagonal entries of `BᵀB` for the current sample set;
- samples are unweighted; the normal matrix and right-hand side are accumulated in `f64`, in tile
  order (row-major), so the fit is bit-identical with and without a pool and from run to run;
- the system is solved by dense Cholesky (at most 1148 coefficients, §1.5).

The scale is the surface's correlation length: at 1536 px a 9576 × 6388 plane has 10 × 8
coefficients, about four to six degrees of freedom across the short side, as in the ERP.

**Step 5 — sample rejection, two rounds.** Fit on every sample; compute residuals
`e_s = y_s − S(x_s)`; `σ_e = 1.4826 · MAD(e)` over the samples in that fit; reject every sample
with `|e_s| > k_s · σ_e`, `k_s = 3.0`, two-sided; refit on the survivors. Repeat once. The third
fit is final. Each round re-judges every sample against the latest fit, so a sample may return.
When `σ_e ≤ 0` no sample is rejected.

**Step 6 — validity: a sample-count floor.** A knot cell (`h × h`) is *supported* when it holds at
least 8 accepted samples of the final fit (L-4). Unsupported cells are counted and logged; they are
not an error. A plane is refused only when no knot cell is supported. Then the old consequences
apply: a refused reference turns LN off for the group (global normalization, one warning); a
refused target fails that frame (excluded when LN drives output, global pair with a warning when
LN drives rejection only).

There is no rejected-fraction gate anywhere (owner decision).

**Step 7 — the symmetric shared-acceptance refit, per reference–target pair** (L-6). After both
own-threshold fits (steps 4–6), the reference surface and the target surface are fitted once more,
each on the **intersection** of the two accepted sample sets: the tiles both final fits kept.

- Each side keeps its own sample values `y_s` from its own tile table. No tile is re-measured.
- The two refits share their positions, hence one design matrix, one `λ` (step 4's rule on the
  intersection) and one Cholesky factorization with two right-hand sides. That is one extra
  ≈ 80-coefficient solve per member.
- There is no rejection round in this step.
- The floor of step 6 applies to the intersection. When no knot cell is supported on it, the pair
  keeps its two own-threshold surfaces (steps 4–6, both already valid) and the run records one
  `warn!` per such frame ("no background sample in common with the LN reference; own-threshold
  backgrounds kept"). The frame is never refused for this: the refit is a refinement, and both
  inputs to `B` are sound without it (controller ruling on the spec writer's proposal).
- The reference's own-threshold fit is built once per group (once per member on a project grid,
  §1.3); the refit is built per member.

**This is not the rejected "one shared mask from the reference."** That proposal took the
reference's mask alone and imposed it on every target. This step is symmetric: the accepted set is
derived from both sides, for each reference–target pair, and it runs on top of the model at the
scale, which remains the fix. Its job is to remove the last difference the two own-threshold fits
can have: each image rejects in units of its own noise (§1.4), so a low-noise reference drops more
of a faint extended feature than a noisy target does, and the two surfaces then differ under that
feature.

Measured (controller, 2026-10-04, IC 434 on Horse C11 G): the surface difference
`S(one frame) − S(reference)` is 1.66 % of background against a 28-frame reference and 1.31 %
against a 5-frame one with the own-threshold fits alone, and 0.23 % / 0.20 % with this step. The
halo star's node is unchanged by it.

**Step 8 — nodes.** Each mesh node takes the step-7 surface's value at its own (clamped) pixel. These
values are what `BackgroundGrid.cells` held before, in the same row-major layout, and
`B = B_R − A·B_T` is formed from the two refitted surfaces of the pair.

**Why this removes the halo class.** A star with a 220-px halo is about 6 % of one 1536² patch,
and the part above 3σ is about 1 %. Whether its samples are rejected on one side, the other or
both, the surface moves by ≲ 0.1–0.5 % of background (math review §5.3). No statistic can swap one
estimator for another (the old gate did), and a sample that flips between reference and target
moves the surface by about one part in the ≈ 2300 samples of its knot cell. Step 7 then removes the
flip itself: a sample enters both surfaces or neither.

### 1.3 Uncovered areas and the project grid's masked overlap

There is no fill step. Tiles with fewer than 256 present pixels are not samples. The membrane
penalty continues the surface over them: between data it interpolates, beyond the last data it
continues at the edge level. In a gap the coefficients are a discrete harmonic continuation of the
fitted ones around it, so the surface never overshoots them there. This covers three cases:

- **A frame set's off-canvas strip.** A warped target is NaN outside its footprint. The strip's
  nodes hold the continuation, as the neighbour fill did before.
- **A non-finite reference region.** Same rule.
- **A project member on the grid (6b-3 D5–D8, `LnOverlap::Masked`).** The target is masked to its
  overlap with the LN reference, and the member's reference model is fitted on that overlap only.
  Nodes outside the overlap hold the continuation of both surfaces and are still read, as 6b-3
  requires: the evaluator reaches two strides in, and member pixels the reference does not cover
  are normalized too.

**Per-member reference model on the grid** (L-5). Today the masked path rebuilds the reference
background per member. The new model reuses the group's reference tiles: a tile wholly inside the
member's overlap takes the group reference's tile statistic bit for bit; a tile cut by the overlap
edge is recomputed on its overlap pixels; a tile outside is absent. The fits (§1.2 steps 4–7,
the shared refit included) run per member. The result is identical to recomputing every tile.

`LN_MASK_MARGIN_PX = 48` (seed erosion) and `LN_MIN_OVERLAP = 25 %` are unchanged.

### 1.4 One procedure, each image's own noise, one shared acceptance

The owner ruled the same procedure and the same thresholds for reference and target, so
`k_t = k_s = 3.0` on both sides, and the procedure ends with the shared-acceptance refit of step 7
(L-1). Each threshold is in units of that image's own noise (its tiles' MAD and its residuals' MAD),
as the ERP does with 3.0 σ_R / 3.2 σ_T. The reference's own-threshold model is therefore built once
per group, as today.

Two noise-dependent effects remain, and each has its answer:

- **Which samples each side rejects.** A faint extended feature is rejected more in the low-noise
  reference than in a noisy target. Step 7 removes this: both surfaces are refitted on the samples
  both kept (§1.2 step 7 has the measurement).
- **The tile statistic itself.** The in-tile clip's bound scales with the tile's noise, so a tile
  median under star wings still depends a little on the image's noise (math review §5.2 measured
  −6 % of background at one 128-px node). The fit dilutes it by the sample count of a knot cell.

The acceptance watches both on the IC 434 boxes and on the B-move read (`ln_probe --diag`), §11.3.
The master median does not watch them: a sky that is right on average can still be wrong under the
nebula.

A second lever is recorded and not built: a plain tile median without the in-tile clip, which takes
the noise-dependent bound out of the tile statistic. It is the next step if the IC 434 rows miss.

### 1.5 Cost on a 9576 × 6388 plane

| Quantity | Today | New | Note |
| ---- | ---- | ---- | ---- |
| Pixels read for statistics | 61.2 M (2×2-subsampled in cells: 15.3 M) | 61.2 M | 60 000 tiles of ≤ 1024 px |
| Full-plane transient copy | 245 MB (`clean_plane`) | none | tile-local cleaning |
| Tile table | — | ≈ 1.4 MB | position, value, σ, max, flags |
| Fits | — | 80 coefficients at scale 1536; at most 41 × 28 = 1148 at scale 256 | three own-threshold fits per image, one shared refit (two right-hand sides) per member; dense Cholesky, milliseconds |
| `background_ms` per plane, 10-worker pool | ≈ 0.17 s (73 ms at 26 Mpx, scaled) | the same order (estimate) | more pixels per statistic (no subsampling), no full-plane copy |

On set 204 (26 Mpx planes, 92 mono + 105 OSC frames) the Normalize stage is expected to grow by
about +10 s on ≈ 2 min. The figure is an estimate, measured at checkpoint A. Time is reported, not
gated: the change is a correctness fix (owner rule: quality over speed). `LN_PLANES_RESIDENT` stays
8; the peak drops by one plane.

### 1.6 What must stay bit-exact or a no-op

- **LN off** (`local.enabled = false` and `rejection ≠ local`): Normalize stays a no-op and every
  output is byte-identical to `main`. The M1 pins hold.
- **The `.athln` format and `grid.rs`**: unchanged code; every pin holds.
- **At the checkpoint-A head** (items 1–2 only), on a set whose resolved scale is unchanged (set 204
  and run B both resolve to 1024): every `A` grid and every `global_scale` is bit-identical to
  `main`. The scale path does not read the background model until item 3 lands.
- **Upstream stages**: calibrated, registered, `metrics`, `fits` and registration artifacts are not
  touched. Their cache rows stay valid (565/565 registration rows, 302/302 calibrated frames on set
  204).
- **The background model itself**: bit-identical with and without a pool, and run to run.
- **Unit pins**:
  - a constant plane gives constant nodes (to 1e-7 relative);
  - a linear gradient is reproduced at every node to 1e-5 of its amplitude;
  - a plane with one planted 200-px halo star gives reference and target nodes within 0.5 % of
    background of each other at three noise ratios (1×, 5×, 28× deeper reference — the math
    review's §5.2 proxies);
  - beside it, a broad extended feature (a 2000-px Gaussian at +30 % of background) at the same
    three noise ratios, with the same 0.5 % bar. It fails without the step-7 refit and passes with
    it; the pin asserts both.

### 1.7 Relation to measured-out work

C-29 (LN background on a 4×4-binned plane) is not re-proposed. It replaced pixels by a mean of 16,
which smears star light under the clip and moved the median node by 2.4e-3 of sky. The tiles here
take a robust median of full-resolution pixels; nothing is averaged before the clip. C-29's lesson
still binds: a background change is judged on the whole group at master level, with the B-move read
(`ln_probe --diag`) reported beside the §8 rows (§11.3).

## 2. Scale derived from the image size (item 2)

### 2.1 The rule

`scale = snap(min(W_ln, H_ln) / gridSize)`, `gridSize` default 4 (owner decision).

- `W_ln × H_ln` is the geometry the LN models live in: the run's registration reference in
  co-registered mode, the group's own reference in native mode, the grid for a project.
- The supported list for LN is `256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144, 8192`
  (the ERP list from 256 up, L-7). Below 256 a knot interval would hold fewer than 8 tiles and the
  stride would be smaller than a tile.
- `snap` takes the nearest list value; a tie goes to the smaller; values below 256 give 256, above
  8192 give 8192 (L-8). The ERP's own snapping is closed; nearest and floor both reproduce its one
  logged case (1597 → 1536).

| Geometry | Short side | ÷ 4 | Scale | Stride |
| ---- | ---- | ---- | ---- | ---- |
| Horse C11, 9576 × 6388 | 6388 | 1597 | **1536** | 192 |
| Set 204 LN reference, 6248 × 4176 | 4176 | 1044 | 1024 | 128 |
| Run B grid, 4480 × 5760 | 4480 | 1120 | 1024 | 128 |
| 4656 × 3520 sensor | 3520 | 880 | 768 | 96 |
| 3008 × 3008 sensor | 3008 | 752 | 768 | 96 |

So item 2 is a no-op on set 204 and run B, and only Horse C11 exercises 1536. The math review's
§8 item 2 says "1536 on the 6224×4168 set"; under the nearest rule that set resolves to 1024. This
spec follows the rule. Stride 192 is a non-power-of-two stride; R-M4a-19 already covers its
evaluator rounding.

### 2.2 Configuration and stored documents

`normalization.local` gains `scaleMode: "auto" | "fixed"` and `gridSize`; `scale` stays as the
fixed value (§6). In fixed mode `scale` is snapped to the list by `resolve_config`, with a `warn!`
when it moves.

**An existing stored document** (per-set config, global default, user preset, project config) has
no `scaleMode`. Every such document carries `scale` explicitly, because serialization writes every
field, so a user who chose 1024 cannot be told from one who never touched it. The rule (L-9):

- `scaleMode` absent and `scale == 1024` (the old default) → **auto**;
- `scaleMode` absent and any other `scale` → **fixed** at that value, snapped.

Why: the owner made the derived scale the default, and 1024 was the default. A non-default number
was a deliberate choice, and it keeps its meaning (a larger number follows less structure). The
number's effect does change — it was a 128-px cell model and is now a correlation length — but that
change is item 1's, for every document.

How the absence is seen: the field decodes as `Option<LnScaleMode>` (`None` when the document has
no `scaleMode`). `resolve_config` applies the rule above and sets `Some(..)`, so every in-memory
config carries a mode, every consumer reads a resolved one, and the next save stores it.

### 2.3 The plan/run twin

One function decides the scale: `ln::resolve_scale(local, w, h) -> u32` (L-10). The run calls it
once per group, at the start of Normalize, with `rc.geometry_of(group)` (the grid on a project).
Every consumer reads the resolved value; `local.scale` is read by `resolve_scale` alone.

The plan calls the same function. Its geometry:

1. **A project**: the grid. Exact.
2. **A frame set with a stored `ln_reference` artifact for the group**: that reference's own width
   and height, now recorded in `LnReferencePayload` (`width`, `height`, `scale_px`). Exact: it is
   the geometry the sidecars on disk were written in, which is the question the cache check asks.
   A payload written before this cycle has no dims and falls to case 3; its hash cannot match
   anyway (§7).
3. **Otherwise**: the plan's resolved reference frame (a manual pin, or the last run's reference in
   Auto) in co-registered mode; the group's anchor member in native mode; with neither, the largest
   group's anchor (the fallback order `compute_register_stale` and the scale-ratio column use).
   This value is marked *estimated*. Normalize is stale in this case anyway (there is no reference
   to compare against), so an estimate never backs a cache claim.

If the run's resolved scale differs from the plan's estimate, the run logs it and adds one run
warning. This is the documented "one run's optimism" class, as for the group β (C-30).

## 3. Star selection for the scale estimator (item 3)

The selection runs after pairing and before the flux ratio, in both scale paths (seeds and
detection), on both estimators, in both arms of the seeds calibration, and it feeds the local-scale
spline (L-11). Both estimators see the **same pairs**, so the F1 decision compares flux definitions
only.

**Step 1 — relative saturation.** On each side, the largest pixel within
`max(2, FWHM_fit)` px of the fitted centroid is compared with `saturation × plane maximum`
(default 0.75). The plane maximum is the largest hot-pixel-corrected finite pixel (§1.2 step 3), so
one hot pixel or cosmic ray does not set it. A pair fails if either side exceeds it. The existing
absolute guard on the warped seed pixel (`SATURATION`, 0.95 native) stays. A warped target's peaks
are lowered by the interpolation kernel, so a star saturated in its native frame can pass the
relative test on the warped plane; the absolute guard covers that case.

**Step 2 — peak SNR.** On each side, `(peak − b) / σ_local ≥ minPeakSnr` (default 40) (L-12):

- `peak` is the mean of the five brightest finite pixels inside the fit's sampling region (the
  adaptive square `fit_one` grew for that star). This follows the external reference pipeline's
  detector definition of a peak, and it is the value recorded for the star. `fit_one` computes it
  from the pixels it already gathered. It is not persisted in the `fits` artifact, so neither
  `PSF_FIT_VERSION` nor the artifact's version moves.
- `b` is the fit's own background.
- `σ_local` is the median `σ_t` of the accepted samples in the 3 × 3 tile block around the
  centroid's tile, or the plane's median `σ_t` when that block has none.

A five-pixel mean carries a fraction of one pixel σ of noise, so selecting on the target's SNR
adds no measurable selection bias.

**Step 3 — the brightest.** Pairs passing steps 1–2 are ranked by the reference fit's `signal`
(descending; ties by reference fit index). The deep reference sets the order, so the rank does not
depend on the member's noise. The first `maxStars` (default 500) pairs whose flux measurement is
valid are kept (§4.1 for F1; both signals > 0 for PSF signal).

The cap counts **pairs**. The ERP caps each image's own detections at 500 before matching, which
can leave a star in one image's 500 and out of the other's. Capping the matched pairs keeps up to
500 usable ratios. This is a deliberate deviation (L-11).

**Step 4 — fallback.** When fewer than `MIN_MATCHES` (20) pairs pass steps 1–2, the brightest
`max(⌈n/4⌉, 20)` of the `n` pairs passing step 1 are taken, capped at `maxStars`, and logged as
`brightQuarter`. When `n < 20`, the call returns `TooFewMatches` as today (the seeds path falls back
to detection; the detection path excludes the frame or keeps its global pair).

**Expected effect on the PSF-signal estimator.** The faint-star background term goes (+1 … +2.5 %
on broad frames; run B mono median gap −8.9 → about −7 %). The aperture term stays. On Horse C11 G
the slope against the ERP's scales should fall from −0.49 to about −0.1 %/px².

**A risk the acceptance must show.** On run B member h7 the 500 brightest read 1.9 points below
"all SNR ≥ 40" with a common aperture (math review §4.2). One explanation is clipped cores in the
sharp frames the reference integrates. §11 reports the truth gap by brightness tercile of the
selected set; a trend above 1 % goes to the owner (§15).

## 4. F1: the common-aperture estimator (item 4)

### 4.1 The measurement

A second estimator beside PSF signal, selected by `normalization.local.fluxEstimator`. Per plane,
per frame:

- `F_R` = the median `fwhm()` of the reference fits over the pairs that passed selection steps 1–2
  (or the fallback set); `F_T` the same for the target; `F = max(F_R, F_T)`.
- The circle has radius `r = a·F`, `a` from `aperture` (2 or 3). It is centred on each frame's own
  fitted centroid. Pixels whose centre lies within `r` count whole.
- The sky is the median of the present pixels whose centre distance is in `[6F, 8F]`, each frame
  about its own centroid. Inner and outer radii are the same on both sides.
- Photometry reads the un-sanitized plane: a non-finite pixel is absent. The target plane is
  sanitized in place for detection today, so `normalize_frame` keeps a finite-mask bitset before it
  sanitizes (1 bit per pixel, 7.6 MB at 61 Mpx).
- A pair is **valid** when its circle is entirely present on both frames, at least half of each
  annulus is present, and both fluxes are positive.
- `flux = Σ_circle − N_circle · sky`; `z = flux_R / flux_T`; `s` = the RCR location at limit 0.3,
  as now.

The Moffat fit stays the finder and the centroider. β no longer enters the flux.

**Growth-curve variant** (`aperture = growthCurve`). The ratio sample uses `a = 2`. A correction
`c` is the median, over the first 100 valid selected pairs (rank order) whose 5F circles are also
entirely present on both frames, of `(flux_R(5F)/flux_R(2F)) / (flux_T(5F)/flux_T(2F))`. Then
`s = c · RCR_loc(z(2F))`. The ratio keeps the small aperture's precision and recovers the light
beyond 2F from the bright stars only (L-13).

The three geometries are config values, so one build runs every variant (§4.3).

### 4.2 Interactions

- **Seeds path and `k` (C-12, C-14, C-15).** Unchanged machinery. An aperture sum is warp-invariant
  to ≤ 0.6 % (C-11), so `k` should come out at about 1 under F1. It is measured and recorded per
  channel; retiring it is a later cycle's call.
- **Pairing** (R-M4c-9, R-T5-2, C-2): unchanged.
- **Masked overlap.** The validity rule replaces the needed "clamp or skip": a circle cut by the
  overlap edge is invalid, and an annulus at least half present is used. With `F > 6` px the
  annulus reaches past the 48-px seed margin; the rule covers that.
- **Local scale** (R-M4c-8): unchanged code. Its input is the selected pairs at their reference
  positions; `LN_LOCAL_SCALE_MIN_STARS` (40) still applies; it stays off by default.
- **Cost.** About 6.5 M pixel reads per plane at F = 7.5 px for 500 pairs: milliseconds, less than
  the fit it follows.

### 4.3 Choosing the aperture (2F, 3F or growth curve)

The owner rule is "whole group, master level". `--stack-sim` (the audit's window simulation)
reproduces a master's pixels exactly over the whole group and is linear in each member's `A`, so
its no-rejection arm measures what a scale change does to the master. It is the discriminator; the
full checkpoint confirms (L-15).

1. Run the probe (§11.2) with the production selection on every member of run B (mono and three
   OSC planes), set 204 (mono and OSC) and Horse C11 G. Keep the variants that pass the probe bars
   on all of them: median |gap| ≤ 0.5 %, |slope| ≤ 0.5 %/px², no instrument offset (mono and OSC
   medians within 0.5 % of each other).
2. Run `--stack-sim` on set 204 mono with each surviving variant's per-member scales. Pick the one
   whose no-rejection master star flux is closest to the same run's truth-corrected no-rejection arm
   (every member scaled by `truth / applied`, the audit's "corrected, no rejection" arm).
3. Variants within 0.3 % of each other on step 2 are tied. A tie goes to the smaller median
   per-member RCR dispersion `σ_z` (precision), then to the smaller aperture.
4. Checkpoint B (§11.1) runs the chosen variant in full. A runner-up within 0.3 % also runs in full
   on set 204 mono only.

### 4.4 The default decision, stated in advance

The shipped default is `psfSignal` until checkpoint B rules. F1 at the chosen aperture becomes the
default when **all** of these hold (L-16):

1. **Truth, per member.** The §4.3 step-1 bars pass for F1 on all four audited groups and on
   Horse C11 G, and PSF signal with selection fails at least one of them on a mono group.
2. **Master level, scale only.** On `--stack-sim` set 204 mono, no-rejection arm: F1's master star
   flux is within ±0.5 % of the truth-corrected no-rejection arm of the same `--stack-sim` run
   (L-22). Its ratio to the LN reference is reported beside it. (The ratio to the LN reference is
   not the bar: in the audit's simulation even the truth-corrected arm reads 0.9918 of it without
   rejection and 1.0213 with it.)
3. **Master level, full runs** (set 204, run B):
   - F1's master SNR (§11.3's definition) is not below PSF signal's by more than 1 %;
   - rejected fraction within ±0.3 pp of the PSF-signal arm;
   - MAD within ±1 % of the PSF-signal arm, judged on the scale-normalized read (L-21), with the
     raw read reported beside it;
   - weights unchanged (ρ ≥ 0.99).
4. **Margin.** F1 improves on PSF signal with selection by at least 1 point of median |gap| on a
   mono group, and is closer to the truth-corrected `--stack-sim` arm by at least 1 % of master star
   flux.

When 1, 2 and 4 hold and 3 does not, the numbers go to the owner and PSF signal stays meanwhile.
When 4 fails (the gain is within noise), PSF signal stays: it is the ERP-faithful estimator. Either
way F1 ships as a choice.

If F1 becomes the default, math reference §4.3 step 3 (PSF flux) is overturned for the default
path. The owner accepted (2026-10-04) that masters may then differ from the ERP by a few percent in
level when F1 is measurably closer to the truth.

## 5. Calibration pedestal warning (item 5)

### 5.1 The condition

A light, in a frame set that is not attested, whose calibration links include a **Flat** link and
**neither a Dark nor a Bias** link (L-17). The links are `get_links_for_frame`, which stage 1 and
the export generator both resolve, so the warning names exactly the frames that will get
`CALSTAT = F`.

- One core function in `api::lights` (`flat_only_lights(conn, light_ids) -> FlatOnlyTally {
  frame_ids, offsets, gains, flat_set_ids }`) serves all three callers.
- The stacking plan evaluates it per group over the included lights.
- Export/send readiness evaluates it over the set's lights.
- The run evaluates it per group over the included lights at stage 1, so cache hits are covered.

Why it matters: dividing `P + V·S` by a flat `V` gives `P/V + S`. The camera's offset pedestal `P`
grows by `1/V` where the optics vignette. On Horse C11 (about 10 DN at OFFSET 1) that is
+10 … 30 % of sky in the corners.

### 5.2 Where it shows, and the text

`{offset_gain}` reads `OFFSET 1, GAIN 100`. Several values read `OFFSET 1–5`. A missing card reads
`OFFSET not recorded` or `GAIN not recorded`.

**Stacking plan** (`StackingPlan.warnings`, one per affected group; the tab's existing warning list
shows it). The plan's own `readiness` also carries the set-level sentence below; the plan does not
copy it into `warnings`, so the tab never shows both:

> group {key}: {n} of {m} lights have a flat but no dark or bias ({offset_gain}). Dividing by the
> flat also divides the camera's offset pedestal, which brightens vignetted corners. Link a master
> dark or bias with the same gain and offset.

**Export and send readiness** (new `ExportReadiness.calibrationWarnings: Vec<String>`,
`#[serde(default)]`; the Export tab lists it under the mode selector while the mode is
**Calibrated lights**, which covers both export and frame-set send):

> {n} of {m} lights have a flat but no dark or bias ({offset_gain}). Dividing by the flat also
> divides the camera's offset pedestal, which brightens vignetted corners. Link a master dark or
> bias with the same gain and offset.

**Run summary** (`RunSummary.warnings`, one per affected group, with the corner ratio of §5.3):

> group {key}: {n} lights were calibrated with a flat but no dark or bias ({offset_gain}). The
> flat's darkest corner reads {pct} % of its centre, so the offset pedestal is {amp}× larger there.
> Link a master dark or bias with the same gain and offset.

`pct = round(100·V)`; `amp = 1/V` to two significant figures. When `V` cannot be read, the second
sentence is left out and one `warn!` names the flat.

### 5.3 The corner ratio (run only)

The plan and readiness stay DB plus cheap file probes, so only the run measures `V`. It does so at
stage 1, once per distinct flat master linked to an affected light (L-18):

- read the top and bottom `c` rows of the master flat, `c = max(16, round(0.05 · min(W, H)))`
  (319 px on 9576 × 6388);
- take the median of each `c × c` corner patch;
- divide by the flat's normalization divisor, the same global value the generator uses
  (`FlatNormDivisor::global_value`);
- report the smallest of the four. With several flats in a group, report the smallest over them.

### 5.4 What it never does

- It never blocks (owner rule: calibration warnings are not gates).
- It never writes, and never changes an auto-link pick or a manual link. A test runs auto-link on a
  seeded catalog with and without the warning code path and compares the links.
- It enters no hash and moves no pixel.
- Attested sets and project frames (calibrated by their publisher) are out of its reach. The collab
  publish surface receives the field through `readiness_from_data` but does not display it this
  cycle.

## 6. Configuration

New fields under `normalization.local` are `#[serde(default)]`. There is no
`STACKING_CONFIG_VERSION` bump. Whole-config precedence is unchanged (spec §9.2, `resolve_config`).

| Field | Type | Default | Range (clamped by `resolve_config`, `warn!` on a move) | Read when |
| ---- | ---- | ---- | ---- | ---- |
| `scaleMode` | `"auto" \| "fixed"`, decoded as `Option` | `"auto"` (legacy rule §2.2) | — | always |
| `gridSize` | u32 | 4 | 2–8 | `auto` |
| `scale` | u32 | 1024 (unchanged) | snapped to §2.1's list | `fixed` |
| `fluxEstimator` | `"psfSignal" \| "commonAperture"` | `"psfSignal"` until §4.4 rules | — | always |
| `aperture` | `"fwhm2" \| "fwhm3" \| "growthCurve"` | the §4.3 winner (`"fwhm3"` until then) | — | `commonAperture` |
| `starSelection.minPeakSnr` | f64 | 40 | 5–200 | always |
| `starSelection.maxStars` | u32 | 500 | 20–5000 | always |
| `starSelection.saturation` | f64 | 0.75 | 0.5–1.0 | always |

`enabled`, `referenceFrames`, `psfModel` and `localScale` are unchanged. The global
`normalization.scaleEstimator` (BWMV/MAD/avgDev) is a different thing and keeps its name; the new
field is `fluxEstimator` to keep the two apart.

**Presets.** Default and Fast preview keep LN off. Maximum quality keeps LN on. None overrides the
new fields, so every preset carries the defaults above.

**Types.** `LnScaleMode`, `LnFluxEstimator`, `LnAperture` and `LnStarSelection` join the
`ts_export.rs` registry. `src/types/stacking.ts` and `src/types/models.ts` are regenerated
(`TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`).

## 7. Hashes, versions and the one-time recompute

- **`LN_MODEL_VERSION`** (L-19): a new constant folded into `normalization_subtree` as
  `"lnModelVersion"`. The background-model task sets it to 2; the star-selection task sets it to
  3; any later task that changes what an existing config produces bumps it again. Through the
  subtree it moves the `ln`, `ln_reference` and `ln_calibration` hashes together.
- **The resolved scale** is folded as the upstream token `ln_scale_px:<n>` into the per-frame `ln`
  hash (`normalization_hash_for`) and the `ln_calibration` hash (`seeds_calibration_hash_for`).
  Plan and run pass the same value (§2.3). The `ln_reference` hash call passes **no** scale token
  (the token argument is empty there): the reference image does not depend on the scale, so a
  `gridSize`, `scaleMode` or `scale` change never rebuilds it.
- **The subtree is canonicalized:**
  - `scale`, `scaleMode` and `gridSize` leave `normalization_subtree`; the token carries their
    effect where it matters, so switching auto ↔ fixed at the same resolved value invalidates
    nothing;
  - `aperture` is in the subtree only while `fluxEstimator = commonAperture`;
  - `fluxEstimator` and `starSelection` are always in it.
- **`PSF_FIT_VERSION` stays 3.** The fitter does not change.
- **The one-time recompute.** On the first LN run after the merge, every set rebuilds its LN
  reference, re-measures its seeds calibration (7 frames × 2 arms per group) and rewrites every
  `.athln`. The plan reports Normalize stale until then. Rebuilding the reference image this once
  is not strictly needed (it does not depend on the model); it is accepted so that the version
  lives in one place, the subtree (L-19). Later scale changes do not rebuild it (above).
- **`LnArtifactPayload`**: `cells_rejected` is replaced by `samples`, `samples_rejected`,
  `samples_shared` and `cells_unsupported` (`#[serde(default)]`; an old payload is stale anyway).

## 8. Plan, run and provenance

- `PlanGroup` gains `lnScalePx: Option<u32>` (`None` when LN is inactive) and
  `lnScaleEstimated: bool` (§2.3 case 3).
- `SummaryGroup` gains `lnScalePx`, `lnFluxEstimator` and `lnAperture` (`#[serde(default)]`), so
  provenance records what ran.
- `LnReferencePayload` gains `width`, `height` and `scale_px` (`#[serde(default)]`).
- The run's per-group LN line (`"ln scale resolved"`, §10) states scale, mode, geometry and
  estimator.
- No schema change. No command change. Both hosts return the same structs, so the web mirror needs
  no code beyond the shared types.

## 9. UI

**One panel, two places.** `NormalizePanel` renders inside the frame set's Stacking tab and inside
Settings → Stacking (`StackingSection`, `mode = 'global'`). Its LN block changes; the global
normalization controls above it do not. The panel gains a `plan` prop (`null` in Settings). The
panel's existing "default X" help lines stay, as every stage panel does.

| Control | Label | Help (one sentence) | Shown |
| ---- | ---- | ---- | ---- |
| select: `Auto`, 256 … 8192 px | Scale | The background model follows structures larger than this and treats smaller ones as signal. | always |
| number 2–8 | Grid size | Auto scale is the frames' short side divided by this, snapped to a supported size. | `Auto` |
| read-only line | Resolved scale | e.g. "1536 px (9576 × 6388)"; per group when they differ; "estimated — the run confirms it" when `lnScaleEstimated`. In Settings: "A frame set's Stacking tab shows the resolved value." | `Auto` |
| select | Star flux | PSF signal measures each star inside its fitted profile; common aperture measures both frames in one circle sized to the broader star, independent of seeing. | always |
| select: 2 × FWHM, 3 × FWHM, 2 × FWHM + growth curve | Aperture | The circle radius in units of the broader frame's FWHM; the growth curve adds the light beyond it from the brightest stars. | `commonAperture` |
| number pair | Minimum peak SNR / Brightest stars | Only stars this far above the local noise, and at most this many of the brightest, set the scale. | always |
| number 0.5–1.0 | Saturation limit (× plane maximum) | Stars with a pixel above this fraction of the brightest pixel in the frame are left out. | always |

PSF model, Reference frames and Local scale are unchanged. Labels are in sentence case. No help
text names another program.

- **Board summary** (`stageSummary.ts`, still a pure function of the config):
  `Local · scale auto (÷4) · ref 20 · common aperture 3×FWHM`, or `Local · scale 2048 · …` when
  fixed.
- **Settings registry** (`src/settings/registry.ts`, `stacking.pipeline` → `normalize`): keywords
  `normalization.local`, `scale`, `grid size`, `star flux`, `common aperture`, `psf signal`,
  `peak snr`, `saturation`. Metadata only, per the Settings contract (`docs/settings/README.md`).
  There is no Save button and no banner. `notify()` fires on a failed save only.
- **Warnings.**
  - Plan warnings use the Stacking tab's existing list.
  - Run warnings use `ResultsPanel` / `ProvenanceModal`'s existing list.
  - The Export tab gains one list for `readiness.calibrationWarnings` (`text-warning`, design
    tokens only), shown while the mode is Calibrated lights.

## 10. Logging

A new "Dictionary extension (LN model fixes, 2026-10-04)" paragraph in
`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` registers these fields. The `ln_`
prefix rule applies.

| Field | Type | Meaning | Events |
| ---- | ---- | ---- | ---- |
| `ln_scale_px` | u32 | the resolved LN scale | `"ln scale resolved"` (info, per group), `"ln reference built"` |
| `ln_scale_mode` | string `auto` \| `fixed` | how it was resolved | `"ln scale resolved"` |
| `ln_flux_estimator` | string `psfSignal` \| `commonAperture` | the estimator | `"ln scale resolved"`, `"ln relative scale"` |
| `ln_samples` | usize | background samples in the own-threshold final fit, summed over channels | `"ln frame normalized"`, `"ln reference background modelled"` (debug, per channel) |
| `ln_samples_rejected` | usize | samples the two rejection rounds left out | same |
| `ln_samples_shared` | usize | samples in the shared-acceptance refit (§1.2 step 7), summed over channels | `"ln frame normalized"` |
| `ln_cells_unsupported` | usize | knot cells under the sample floor | same as `ln_samples` |
| `ln_selected` | usize | pairs after the star selection, before RCR (`ln_matches` keeps "pairs before selection") | `"ln relative scale"` |
| `ln_selection` | string `snr` \| `brightQuarter` | which selection rule held | `"ln relative scale"` |
| `ln_aperture_px` | f64 | the common circle radius `a·F` | `"ln relative scale"` (common aperture) |
| `ln_common_fwhm_px` | f64 | `F`, the larger median FWHM | same |
| `ln_growth_factor` | f64 | the growth-curve correction `c` | same (growth curve) |
| `light_offset` | string | distinct OFFSET values of the affected lights | pedestal warn |
| `light_gain` | string | distinct GAIN values | pedestal warn |
| `flat_set_ids` | string | comma-joined flat set ids | pedestal warn |
| `flat_corner_ratio` | f64 | `V`, the flat's darkest corner over its centre | pedestal warn (run) |

- `ln_cells_rejected` is **retired**; the dictionary entry says so.
- The pedestal event is `warn!("flat without a dark or bias: the offset pedestal is amplified in
  vignetted corners")`. It carries `run_id` and `group_key` (run) or `set_id` (readiness), plus
  `count`.
- The mismatch between the plan's estimate and the run's scale (§2.3) is a `warn!` carrying
  `ln_scale_px` and `expected`.
- Every refusal (a plane with no supported cell, a pair with no shared supported cell) is logged
  before it returns. Never swallowed.

## 11. Acceptance

### 11.1 Datasets and runs

| Dataset | What it is | Baseline |
| ---- | ---- | ---- |
| **Set 204** (`LDN1272-test`, 92 mono + 105 OSC 180 s) | frame set, LN at 1024 | the `tierA-baseline` ruler plus a same-evening `main` re-run (R-TA-8/9) |
| **Run B** | the 6b-3 project on the 4480 × 5760 grid, LN on | a same-evening `main` re-run of `project-run.sh` |
| **Horse C11** | dev catalog `frames_set` 182, 9576 × 6388 ASI6200MM, prepared per §11.4: the L and G groups (R and B excluded), registration reference pinned to L frame 10666; the rows read the G master (30 lights, 28 included in run 8) | branch with LN off (the no-LN control), and `main` with LN on (reproduces run 8's ring deficit) |

**Checkpoint A** (after tasks 1–2, PSF signal unchanged):

- set 204: `tier1/checkpoint.sh lnA <main re-run> --numeric`, then `tier1-compare.py --numeric`;
- run B: `project-run.sh`, then `grid-report.py`;
- Horse C11: L + G three ways — branch LN on, branch LN off, `main` LN on;
- optionally, Horse C11 branch at a fixed 1024, to separate item 1 from item 2.

**Pre-checks for items 3–4:** the probe (§11.2) on every member, then `--stack-sim`, then the
aperture choice (§4.3).

**Checkpoint B** (after tasks 3–4): two arms each — PSF signal with selection, and F1 at the
chosen aperture — on set 204, run B and Horse C11 (L + G, rows on G). Checkpoint A's tree is the
baseline that isolates the selection's effect.

Each checkpoint tree needs about 135 GB free. Expect about 15–27 min per set-204 run and about
13.5 min per run-B run. One compile at a time, nothing else heavy beside a run (memory note
2026-09-26).

### 11.2 Harness changes (reuse, never rebuild)

- `examples/ln_scale_probe.rs` calls the production selection and both production estimators
  (library functions, not re-implementations). It writes per-member scales for each estimator and
  aperture variant, and the gap by brightness tercile of the selected set.
- `--stack-sim` takes those per-member scales as one arm per estimator and variant. It keeps the
  truth-corrected arm (every member scaled by `truth / applied`) and the no-rejection variant of
  every arm.
- `ln_probe --diag` reports the B move between two trees: median and p99 node change in units of
  sky, and nodes past 1 %.
- `docs/superpowers/research/scripts/ln-math/halo_profile.py` finds the star by a local centroid
  search around run 8's pixel (7556, 3212) in each master, then reads its ring medians. Run 8 has no
  WCS; the pinned reference (§11.4) keeps its geometry.
- `master_phot.py` gives each master's star flux in LN-reference units (`k`) and its MRS noise, for
  the scale-normalized read and the SNR row (§11.3), one script for every arm.
- A new `nebula_boxes.py` measures fixed boxes (below).
- `scale_pairs.py` pairs the new runs' per-frame scales with the ERP log's pass-2 G block (the
  2026-10-04 log under the external run folder on `/Volumes/Universe/Development/`, lines
  28516–29100).

### 11.3 The rows (re-ruled, truth-based)

**Two definitions used by the rows.**

- **The scale-normalized read** (L-21). For a master `X`, `k_X` is its star flux over the LN
  reference's star flux on the same stars (`master_phot.py`; the LN reference of the tree's LN-on
  run). The read is taken on `X / k_X`:
  - MAD is the MAD of `X / k_X`;
  - a halo ring or IC 434 box excess is `(median(X / k_X) − background(X / k_X)) / background_0`,
    where `background_0` is the no-LN control's raw background in the same annulus or boxes, one
    fixed denominator for every master.

  The raw read is the same with `k_X = 1` and each master's own background as the denominator (the
  math review's definition). A pure signal-scale change moves the raw read but not the normalized
  one. The normalized read decides; the raw read is reported beside it.
- **Master SNR** = `k_X / σ_X`: the master's star flux in LN-reference units over its MRS noise σ.
  One script computes both for every arm, on the same stars and the same rectangle.

| Row | Checkpoint A (items 1–2) | Checkpoint B (items 3–4) |
| ---- | ---- | ---- |
| Master median (sky truth) | within ±0.1 % of the LN reference's own median, same rectangle; the baseline's value reported beside it | same |
| Master MAD | ±1 % vs baseline, scale-normalized read (raw beside it) | ±1 % vs the PSF-signal arm, scale-normalized read (raw beside it) |
| Master noise (MRS σ) vs baseline | ±2 % | replaced: master SNR not below the baseline's by more than 1 % |
| FWHM vs baseline | ±1 % | within ±1 % of the signal-weighted variance-mixture prediction from the new per-member scales (audit §5.3 estimate +1.1 % on set 204 mono); baseline reported |
| Rejected fraction vs baseline | ±0.3 pp | ±0.3 pp |
| Per-frame weights | ρ ≥ 0.99, top-20 ≥ 18/20 | same |
| `A` grids and `global_scale` | bit-identical to `main` on set 204 and run B | — |
| Per-member scale vs truth (`--ln-truth`, probe) | reported | F1: median \|gap\| ≤ 0.5 %, \|slope\| ≤ 0.5 %/px², no instrument offset; PSF signal reported |
| `--stack-sim` set 204 mono, no rejection | reported | F1: master star flux within ±0.5 % of the same run's truth-corrected no-rejection arm; the ratio to the LN reference reported beside it |
| Seeds calibration `k` per channel | reported | reported (expected ≈ 1 under F1) |
| Drizzle level and coverage | as before (C-20, C-23 data facts) | same |
| B move vs baseline (`ln_probe --diag`) | reported (C-29 read) | — |
| Wall | reported, against the same-evening re-run | reported |

**Horse C11 rows** (the G master; branch LN on vs branch LN off, same registration):

- **Halo.** The star found near run 8's (7556, 3212) (§11.2). Ring excesses at 60–70, 80–90 and
  100–120 px must be within ±1 % of background of the no-LN control, on the scale-normalized read
  (raw beside it). Run 8 read +15.5 / +4.1 / +1.1 % against +23.9 / +12.2 / +6.7 %; the ERP reads
  +24.9 / +12.9 / +7.0 %.
- **B nodes.** No B node within 600 px of the star differs from the mean of its 8 neighbours by
  more than 2 % of background, on any member's sidecar.
- **IC 434.** Three 100 × 100 px boxes on the bright rim of IC 434 and three background boxes off
  the nebula within 1500 px. They are chosen on the no-LN control and recorded in the acceptance
  doc **before** the LN master is read. Each box's excess on the LN master must be within ±1 point
  of background of the no-LN control's, on the scale-normalized read (raw beside it). This row and
  the B-move read are what watch the noise-dependent effects of §1.4.
- **ERP slope.** Athenaeum's per-frame scale over the ERP's, against PSF variance: PSF signal with
  selection must reach within ±0.2 %/px² (from −0.49). The mean offset is explained by the
  reference choice (5 frames vs 10). F1 is reported. It is expected to show the ERP's own aperture
  term, so the bar does not apply to it (L-20).
- **`main` LN on reproduces run 8's ring deficit** (≈ −8 % of background at 60–90 px against the
  no-LN control). That shows the acceptance copy is a fair stand-in for run 8.

A miss that the raw read shows and the scale-normalized read does not is the estimator's, not the
background model's. Rows are never re-sized and nothing is tuned toward a bar (owner rule): a miss
stops the task and goes to the owner with its numbers.

### 11.4 Horse C11 preparation

**Run 8** is the owner's record `/Volumes/Universe/Development/ath/runs/run-8.json` (app 0.6.4,
2026-10-04 10:24):

- LN on, scale 1024, `referenceFrames` 5;
- output normalization `multiplicativeWithScaling`, rejection normalization `scaleZeroOffset`,
  scale estimator `mad`;
- G: 30 frames, 28 included.

The dev catalog has no stacking rows for set 182. The prod catalog's own "run 8" is a different,
LN-off run: never use it.

1. `sqlite3 -readonly <dev app-data>/athenaeum.db ".backup <acc>/horse-c11/athenaeum.db"`.
2. Write run-8.json's `config` into the copy's `stacking_set_config` for set 182 through the API
   (`set_stacking_config`), never by hand in SQL. Its stored `scale: 1024` reads as auto under L-9
   and resolves to 1536. The optional fixed-1024 run (§11.1) sets `scaleMode: "fixed"`.
3. On the copy only, delete the `calibration_set_to_frames` rows with `calibration_type = 'Dark'`
   for set 182's lights (dark sets 1242 and 1315). In the dev catalog they point at offset-50 darks;
   run 8 had flats only. Do not touch the flat links.
4. The G lights link to raw Flat sets 786, 849, 1174 and 1205, so the plan builds master flats
   (stage 0.5), and the L group's flats likewise. If the plan names an ROI pre-calibration master
   for one of them, re-link it per the open-items workaround (1.0 s → 1269, 0.5 s → 1297,
   bias → 1328). The geometry bug must not decide the acceptance.
5. Keep run 8's geometry:
   - keep the L group and exclude only the R and B lights;
   - pin the registration reference manually to L frame 10666 (run 8's two-pass pick).
6. Raw data: `/Volumes/bigbase2/Astrobase/2024/horsehead11/`.

### 11.5 Item 5

- **Unit tests.** Link combinations: F only → warn; F+D, F+B, D, B and none → no warn. Attested →
  no warn. Excluded lights are not counted. The plan counts per group. Text formatting covers one
  value, a range and not recorded. Auto-link is unchanged (§5.4).
- **Real data.** The Horse C11 copy (flats only) shows the warning in the plan, the Export tab and
  the run summary. The run's corner ratio is computed from the L and G master flats. Set 204 shows
  none.

## 12. Tasks and order (guidance for the plan)

Background model first, then the scale rule, then the selection, then F1. The warning is
independent and can land at any point. Each numeric task lands behind its own hash change (an
`LN_MODEL_VERSION` bump, or the subtree change of §7) and fixture pins that record the delta.

1. **Background model** (`ln/background.rs` rewrite: the own-threshold fit and the shared-acceptance
   refit; wiring in `normalize_frame` and `run_group_normalization`; masked per-member tile reuse;
   `LnArtifactPayload` fields; logging; the unit pins of §1.6, the extended-feature pin included). Afterwards run `cargo check -p athenaeum-core --all-targets`, because the
   probes use the old public API (`background_grid`, `DEFAULT_PARAMS`, `TARGET_DEVIATION_SIGMA`).
2. **Scale rule** (`scaleMode`, `gridSize`, legacy decode, `resolve_scale`, hash token, plan twin
   and `PlanGroup` fields, `LnReferencePayload` dims, `SummaryGroup`, TS, the scale controls and
   resolved line in `NormalizePanel`, `stageSummary`, registry keywords).
3. **Checkpoint A** (§11.1). Stop on a miss.
4. **Star selection** (`σ_local` and the plane maximum from the model, the five-pixel peak from
   `fit_one`, the selection in both paths and both calibration arms, `starSelection` config,
   logging, UI fields, the probe calling it).
5. **F1** (`fluxEstimator`, `aperture`, the finite mask, photometry with its validity rules, the
   growth curve, logging, UI fields, probe arms).
6. **Pre-checks and the aperture choice** (§4.3), then **checkpoint B**.
7. **The default** per §4.4 (a one-line default change if F1 wins; otherwise none), recorded as a
   ruling with its numbers.
8. **The warning** (core function, readiness field, plan per group, the run's corner ratio and
   summary line, the Export tab list, tests).
9. **Docs:**
   - `docs/stacking/README.md` (LN paragraph, the rulings, the acceptance);
   - math reference §4.2 (and §4.3 if F1 wins);
   - a pointer from stacking spec §5.2/§9.2 to this spec;
   - the logging dictionary;
   - `open-items.md` (unverified smokes, the release-note lines owed);
   - `CLAUDE.md`'s Stacking summary only if a rule changed (none is expected).

The full core suite (`cargo test -p athenaeum-core`, all targets) runs before any push.

## 13. What changes for existing users

- **Every set with LN on re-normalizes once** (reference, seeds calibration, every sidecar). The
  plan shows one "Normalize stale" pass after the upgrade, then reads cached again. A set with LN
  off sees nothing but the warning.
- **Normalize takes a little longer** (estimate ≈ +10 s on set 204's ≈ 2 min; reported, not
  gated).
- **The Normalize panel shows a "Resolved scale" line** in Auto mode, per group, marked as an
  estimate until a run confirms it.
- **The sky of every LN master changes.**
  - Halo and nebula "holes" near bright stars go.
  - Structures smaller than the scale are no longer followed by the background model: they are
    signal and scale with `A`.
  - Sky levels near bright structures move by up to the old error (tens of percent of background at
    single nodes on Horse C11).
- **Large sensors get a larger default scale.** A short side above 5120 px resolves above 1024;
  one between 3584 and 5120 px stays at 1024.
- **Stored configs:** `scale: 1024` becomes auto; any other value stays fixed, snapped to the list.
- **The selection** raises PSF-signal scales on broad frames by about 1–2.5 %. Every LN master's
  signal level moves by the group's weighted share of that.
- **If F1 becomes the default:**
  - an LN master's signal level rises by the group's weighted-mean gap (about +5–7 % mono on a
    mixed-seeing night; up to about 1.3 % per OSC plane, so colour balance moves);
  - noise in reference units rises by about 4 % while SNR rises by about 1.7 %;
  - FWHM moves by about +1 %.
- **Sets calibrated with a flat but no dark or bias** show the pedestal warning in the plan, the
  Export tab and the run summary. Nothing else changes for them.

## 14. Rulings

**New (this spec):**

- **L-1** One background procedure and one threshold for reference and target: `k_t = k_s = 3.0`,
  in each image's own noise units, ending with the shared-acceptance refit of L-6.
  `TARGET_DEVIATION_SIGMA` retires.
- **L-2** Background samples are 32-px non-overlapping tiles, every pixel used, a tile needing 256
  present pixels.
- **L-3** The surface is a penalized least-squares tensor cubic B-spline. The knot spacing is the
  scale, the penalty is a first-difference membrane, and `λ = 1e-4 ×` the median non-zero diagonal
  of `BᵀB`. It is accumulated in `f64` in tile order and solved by dense Cholesky, so it is
  deterministic.
- **L-4** Validity is a sample-count floor: a knot cell is supported at ≥ 8 accepted samples, and a
  plane is refused only with no supported cell. There is no rejected-fraction gate.
- **L-5** A project member's reference model reuses the group reference's whole-overlap tiles and
  recomputes the edge tiles.
- **L-6** The symmetric shared-acceptance refit (§1.2 step 7). After both own-threshold fits, the
  reference and target surfaces are refitted on the intersection of their accepted samples, per
  reference–target pair, with one factorization and no rejection round. It is not the rejected "one
  shared mask from the reference": it is derived from both sides and sits on top of the model at
  the scale. Measured on IC 434: 1.66 / 1.31 % → 0.23 / 0.20 % of background. It replaces the
  earlier "consistency guard as a fallback". The plain tile median (no in-tile clip) is recorded as
  a second lever, not built.
- **L-7** The LN scale list is 256 … 8192.
- **L-8** Snapping is to the nearest list value, ties to the smaller.
- **L-9** A legacy document without `scaleMode` reads as auto at `scale == 1024`, fixed otherwise.
- **L-10** `ln::resolve_scale` is the one function for plan and run. The plan's geometry is the
  grid, the stored LN reference's dims, or a marked estimate.
- **L-11** The star selection applies to both estimators, both scale paths, both calibration arms
  and the local-scale input, and runs on pairs. The `maxStars` cap counts matched pairs, a
  deliberate deviation from the ERP's per-image cap.
- **L-12** Peak SNR is `(peak − b) / σ_local`. `peak` is the mean of the five brightest finite pixels
  in the fit's sampling region (the external reference pipeline's detector definition), recorded by
  `fit_one` and not persisted. `b` is the fit background. `σ_local` is the median tile σ of the
  accepted samples in the 3 × 3 tile block, else the plane median.
- **L-13** F1's geometry: `F = max` of the two median fitted FWHMs; a whole-pixel circle at each
  frame's own centroid; an annulus `[6F, 8F]` median; validity rules as §4.1. The growth-curve
  variant is as defined there.
- **L-14** The plane maximum for relative saturation is the hot-pixel-corrected maximum. The
  saturation test reads the largest pixel within `max(2, FWHM)` px on both sides.
- **L-15** The aperture is chosen by the probe bars plus `--stack-sim` (whole group, master level);
  the full checkpoint confirms.
- **L-16** The F1 default rule is §4.4, fixed before the data are read.
- **L-17** The pedestal warning's condition is link-based (Flat, no Dark, no Bias), per group, not
  for attested sets.
- **L-18** The corner ratio is measured in the run only, from the flat's corner patches.
- **L-19** `LN_MODEL_VERSION` lives in `normalization_subtree`, so the LN reference image is rebuilt
  once with the rest. The resolved scale is a hash token on the per-frame `ln` and the
  `ln_calibration` hashes only; the `ln_reference` hash carries none. `scale`/`scaleMode`/`gridSize`
  leave the subtree, so a later scale change never rebuilds the reference image.
- **L-20** The ERP-slope bar of math review §8 item 3 applies to PSF signal with selection, not to
  F1.
- **L-21** MAD, the halo rings and the IC 434 boxes are judged on the scale-normalized read (the
  master divided by its own star-flux ratio to the LN reference, §11.3), with the raw read reported
  beside it.
- **L-22** The `--stack-sim` bar for F1 is ±0.5 % of the same run's truth-corrected no-rejection
  arm. The ratio to the LN reference is reported, not gated.

**Overturned:**

- Math reference §4.2 and stacking spec §5.2's background description ("deviation thresholds
  3.0σ / 3.2σ, rejection limit 0.3 per cell"): by item 1, owner decision 2026-10-04.
- `normalization.local.scale` as a fixed 1024 default: by item 2.
- Math reference §4.3 step 3 (the PSF-flux ratio) as the default: only if §4.4 makes F1 the
  default.

**Kept, and how they interact:**

- C-29 (binned background, measured out): not re-proposed (§1.7).
- C-11/C-12 (fit signal is not warp-invariant): F1 is warp-invariant.
- C-14/C-15 (seeds calibration): kept and re-measured.
- C-1/C-1a (group β): kept for fitting; moot for F1's flux.
- M2's same-β rule: kept for PSF signal.
- R-M4c-8 (local scale): unchanged; its input is the selected pairs.
- R-M4c-9, R-T5-2, C-2 (pairing passes): unchanged.
- R-M4a-19 (non-power-of-two stride): covers stride 192.
- R-M3-17 v2 (LN reference members): unchanged.
- The 2026-09-10 ruling on the 0.85 high clip: kept.
- The `grid.rs` smoothing-kernel ruling: kept.
- 6b-3 D5–D8 (masked overlap): kept; the model works on the overlap.
- C-20, C-23 (drizzle data facts): kept.
- R-TA-8/9 (interleaved brackets, same-evening re-run): followed.
- Owner rules: a numeric change is judged on the whole group at master level; quality over speed;
  calibration warnings are not gates.

## 15. Open points for the owner

Seventeen points. Each was decided above, by this spec or by the controller's fix round.

**Owner ruling 2026-10-04: all accepted.** Explicitly confirmed: point 2 (the symmetric
shared-acceptance refit), point 8 (only `scale: 1024` becomes auto), point 7 (`gridSize` visible,
2–8, default 4), point 14 (the F1 default rule as written, decided in advance) and point 11 resolved:
if the brightest stars show a trend above 1 % across terciles, raise `maxStars`; never cap the top.
The rest (technical points 1, 3–6, 9, 10, 12, 13, 15–17) were accepted as presented. Also ruled:
no migration of existing runs (see the header).

1. Thresholds `k_t = k_s = 3.0` (the old reference value), in each image's own noise (L-1).
2. The symmetric shared-acceptance refit as the background model's final step (L-6), with the plain
   tile median recorded as a second lever.
3. Tile size 32 px with every pixel, not 64 px or decimated (L-2, §1.5).
4. The surface method and its constants: P-spline with knot spacing = scale, membrane penalty,
   `λ = 1e-4` relative, `f64`, dense Cholesky (L-3).
5. The validity floor of 8 samples per knot cell (L-4).
6. The LN scale floor of 256 and nearest-value snapping; the math review's "1536 on 6224×4168" is
   1024 under this rule (L-7, L-8).
7. `gridSize` as a visible setting (2–8, default 4).
8. The legacy rule: only `scale: 1024` becomes auto (L-9).
9. The selection mechanics: pairs ranked by the reference's flux; SNR on both sides; saturation
   radius `max(2, FWHM)`; the fallback `max(⌈n/4⌉, 20)`; the 500 cap on pairs, not per image
   (L-11, L-14).
10. The peak definition for peak SNR: the mean of the five brightest pixels in the fit region minus
    the fit background (L-12).
11. The brightness trend of the 500 brightest (§3): if the acceptance shows more than 1 % across
    terciles, whether to cap the top or raise `maxStars`.
12. F1's annulus `[6F, 8F]` and the growth-curve definition (L-13).
13. Choosing the aperture on `--stack-sim` plus the probe rather than three full checkpoints (L-15).
14. The F1 default rule's margins, with the `--stack-sim` bar against the truth-corrected arm and
    the scale-normalized reads (L-16, L-21, L-22).
15. The ERP-slope bar moved from F1 to PSF signal with selection (L-20).
16. The warning's numbers: counts, OFFSET/GAIN and flat ids in the plan and export; the measured
    corner ratio in the run only (L-17, L-18). Collab publish surfaces do not show it this cycle.
17. Rebuilding the LN reference image once with the version bump, and never for a scale change
    (L-19).

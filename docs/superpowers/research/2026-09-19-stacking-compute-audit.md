# Stacking pipeline — compute audit (2026-09-19)

Follow-up to `2026-09-19-stacking-performance-audit.md` (the I/O-and-parallelism audit) after its
tier-1 acceptance run showed no radical gain. This document is about the ARITHMETIC: where the CPU
time goes per kernel, what is duplicated, what open-source pipelines do differently, and what
each fix is worth. Sources: the base acceptance run's per-frame sub-stage events (Task 0
instrumentation, run 37 on LDN 1272, 208 mono + 160 OSC frames of 6248×4176, 10-core M4 / 16 GB),
three `sample` profiles of the release probes (`measure_probe`, `ln_probe`, `integrate_probe`),
three code audits of the kernels, and three literature briefs on how published source-extraction, fitting, resampling,
drizzle and stacking implementations are built. Read-only: nothing in this document changed code.

## 0. Verdict

**The run is CPU-bound on three things, in this order: (1) measuring the same stars three
times, (2) a robust-line fit that does ~63 passes over every pixel stack to find two numbers,
(3) resampling and depositing pixels through generic, guard-heavy inner loops.** The tier-1
premise ("18–22 min of pure reading") was wrong on a local SSD — what the engine calls a band
"read" is a bicubic warp on the CPU — which is why the band prefetch and the single pool bought
nothing and why the per-frame times went UP when everything was squeezed into one pool.

Measured CPU budget of the base run (sum of the per-event phase times, frame-minutes):

| Kernel | CPU-min | Share | What it is |
| ------ | ------- | ----- | ---------- |
| LN `scale_ms` (re-detect + refine + Moffat fit + RCR on the warped plane) | **55.9** | 34 % | 100 % duplicate of Measure's work, at 12× Register's star cap |
| LN `warp_ms` (full-frame bicubic warp) | 26.7 | 16 % | the same warp Integrate redoes band by band |
| Measure (background 5.5, noise 3.3, detect 2.4, fit 4.9) | 19.8 | 12 % | the one measurement that is needed |
| Register detect (incl. the dry pass) | 13.7 | 8 % | mono frames: a bit-identical re-detection of Measure's plane |
| Integrate (read/warp 3.1, combine 7.6 — wall-ish) | 10.7 | 7 % | `medfit_line` = 6.0 of the 10.7 min (M4c runs 22 vs 25) |
| Drizzle deposit | 7.4 | 5 % | ≈ 1 000 f64 ops and ≈ 70 divisions per source pixel |
| Calibrate | 5.4 | 3 % | I/O-shaped, already fanned out |
| Register warp/read, LN background, drizzle read | ≈ 8 | 5 % | |

Wall time ≈ 49 min; roughly 165 frame-minutes of work; **44–58 % of the sampled thread time in
every probe is idle** (`__psynch_cvwait`), i.e. inside a frame the pipeline is serial-heavy and
the fan-out is what keeps the cores busy.

**Recommendation** (§7): a compute cycle in three tiers — (A) bit-identical hygiene worth ≈ 8–10
min, (B) same-math-faster-code that moves the last bits and needs one acceptance run, ≈ 7–9 min,
(C) reuse-and-algorithm changes — above all "LN takes Measure's star fits" (−46 CPU-min, ≈ −8 wall
min) and the drizzle phase table (−7.5 min) — ≈ 15–20 min. Together **≈ 49 → 20–25 min** on the
reference set. SIMD is real but third-order until the duplicated work is gone (§6). GPU stays out.

## 1. What the tier-1 run established

The tier-1 branch (`perf/stacking-tier1`, 11 commits) is byte-identical to the baseline on every
output (6 masters raw-identical, 620 registration rows, 368 `.athln`, 528 calibrated frames) and
it shipped the instrumentation this audit is built on. Its measured effect was small and partly
negative:

| Stage | base | head run 1, swapped in Normalize (one pool, prefetch, admission fixes) |
| ----- | ---- | ---- |
| Calibrate | 5.5 | 3.6–4.4 |
| Measure | 10.1 | 8.0–10.3 (per-frame time ×2–2.7 at the same admission) |
| Register | 1.8 | 1.2–1.9 |
| Normalize | 12.5 | 15.4 (swap at 10 workers → fixed to 4), then ≈ 12 |
| Integrate | 10.8 | 12.6 (prefetch overlaps two CPU phases on the same cores: read 44 → 185 s, combine 103 → 143 s, wall unchanged) |
| Drizzle | 8.6 | 10.2 |

Head run 1 is the source of every "head" number above. Two more head runs were attempted on the
same branch and are not: run 2 overlapped a Time Machine backup partway through, so its numbers
are not trustworthy; run 3 was cancelled by the owner before it finished.

Two lessons carry into this document. First, **on a local SSD every stage is compute**: the
measured "read" of a 208-frame band is 5.4 Gpx of bicubic warp, and the disk delivers 21 GB in
50 s while the warp takes 185 s. The prefetch and the one-pool change are only right on a network
mount and should be gated on `StorageClass::Network`. Second, the sub-stage medians now tell the
truth per frame — every number below comes from them or from a profile.

## 2. Profiles — what the CPU actually executes

`sample` (1 ms) of the release probes on real frames (`/Volumes/BigMac/…/.athenaeum-acc/profiles/`):

| Probe | Busy leaf samples | Top leaves (share of busy) | Idle |
| ----- | ---- | ---- | ---- |
| `measure_probe`, one OSC frame | 16 k | `fit_moffat_2d_impl` 25 % + `pow` 17 % + `find_median` 13 % + rayon overhead 28 % + `rcr` 2 % + `scan_region` 1 % + `estimate_noise_mrs` 1 % | 49 % |
| `ln_probe`, 3-frame reference + one OSC normalize | 220 k | `fit_moffat_2d_impl` 38 % + `pow` 28 % (called from the Moffat model, `fitting.rs:541/549/566`) + warp (`Plane::at` 6 %, `weights` 6 %, `sample_at` 2 %) + integrate (`partition_at_index` 4 %, `combine_pixel_weighted` 3 %, `medfit` 3 %) | 58 % |
| `integrate_probe`, 60 mono frames, linear fit | 916 k | `partition_at_index` (select_nth inside `medfit_line`) 16 % + `medfit` closure 11 % + `fit_moffat_2d_impl` 13 % + `pow` 10 % + `find_median` 7 % + warp (`Plane::at` 6 %, `weights` 6 %, `sample_at` 2 %) + `combine_pixel_weighted` 2 % + `BandPlanes::sample` 2 % | 44 % |

Three facts fall out. **`powf` is a first-class hot spot** — the Moffat model evaluates
`base.powf(-β)` and `base.powf(-β-1)` per sample per LM iteration (`rustafits/src/analysis/
fitting.rs:541,549`) plus `ln(base)` for the β-Jacobian even with β fixed (`:566`): three libm
transcendentals ≈ 60 of the ≈ 105 cycles per sample-iteration. **The robust line's `select_nth`
is the Integrate hot spot**, not the sort or the gather. **Half the thread time is idle**:
`adaptive_detection.rs` contains no rayon at all, `background_grid` is serial, the histogram
passes are serial — the inner work of a frame runs on one core while the pool waits.

## 3. Kernel by kernel

Every item is tagged **(a)** bit-identical, **(b)** same math / bits move (needs the hash-bump +
one acceptance re-run), **(c)** different algorithm or reuse (needs acceptance). Gains are
CPU-minutes on the reference set unless marked "wall".

### 3.1 Star detection and PSF fitting — 68 CPU-min, 80 % of it duplicated

The three call sites run the SAME detector (`rustafits/src/analysis/adaptive_detection.rs`,
`detect_stars_adaptive`) on the same pixels and keep nothing in common:

| Site | plane | levels | centroid refine (25-iter Moffat LM per detection) | PSF fit | star cap | kept |
| ---- | ----- | ------ | ---- | ---- | ---- | ---- |
| Measure (`measure.rs`) | each channel, native | `NoiseRelative` (R-M4a-1) | no | yes, Auto β | 24 576 | aggregates only — `outcome.fits` discarded at `measure.rs:469` |
| Register (`register/detect.rs`) | luminance, native | `RankBudget` (sky-blind, the thing R-M4a-1 removed from Measure) | **yes** | no | 2 000 | transform |
| LN (`ln/scale.rs::detect_seeds`) | each channel, **warped** | `RankBudget` | **yes** | yes, reference β | **24 576** (`ln/mod.rs:532` hands Measure's cap to a consumer that uses ≤ 600 nodes) | `.athln` grid |

- LN's 50 CPU-min of `scale_ms` is a second full detection (4 ladder arms incl. the 12-tile
  `7σ` pass) + a 25-iteration refine LM per detection (whose only consumer is a median over the
  100 brightest seeds, `psf_signal.rs:155`) + a second Moffat LM per seed — on the warped plane,
  for stars Measure already fitted in native geometry. For the 208 mono frames Register's plane is
  bit-identical to Measure's (`luminance(&[p]) == p`, `detect.rs:35`).
- Waste inside the detector (`adaptive_detection.rs`): `background_and_noise` computed twice per
  call (`analysis/mod.rs:1277` and `adaptive_detection.rs:489`); `hfd_at` (49 % of Register's
  detect) visits the same 35×35 window five times with a `sqrt` per visit and two full `sort_by`
  medians per candidate (`:222-228`), for ≈ 34 000 candidates per frame; the whole file is
  single-threaded; a 26 Mpx `fold(max)` whose only consumer is a `warn!` (`detect.rs:70-79`); a
  104 MB `to_vec` for `channels == 1` (`mod.rs:1266`) — four resident copies of the plane before the
  first candidate.
- Waste inside the fit (`psf_signal.rs`, `fitting.rs`): `sampling_radius` rescans the whole
  `(2r+1)²` square at every 1-px growth step (3 489 reads for a 729-px final region — O(r³));
  `PsfModel::Auto` fits the 64 brightest seeds 5× (4 β candidates + final); `PixelSample` is a 24-B
  AoS with integer coordinates stored as f64; `cholesky_solve` heap-allocates three `Vec`s per call,
  up to four calls per iteration; two f64 divisions per pixel in `aperture` (`:442`);
  `residual_cost_moffat` recomputes the accepted step's residuals that the next Jacobian pass
  computes again.
- Waste in Measure's own estimators: `estimate_background_mesh` builds a full-resolution bicubic
  NOISE map (`background.rs:164`) that `background_residual` never reads; `estimate_noise_mrs` = 1.04 G
  multiply-adds per plane through a SCALAR `b3_spline_smooth` with a reflected-boundary branch on
  every tap of every pixel (`convolution.rs:318-333`), allocating 12 fresh 104 MB buffers per plane
  — while the vectorised `convolve_row_neon`/AVX2 sibling in the same file goes unused by this path.

**What the field does** (§4): published source extractors are one-pass mesh-background →
matched-filter → label → peak pipelines at ≈ 50 Mpx/s; one photometry library won 10–400× by separable
box filters instead of footprint filters; a batched-fitting library's CPU baseline is ≈ 1.8×10⁴ fits/s/core on
fixed 5×5 stamps with analytic Jacobians; a common open stacker warm-starts its PSF fit (rotation off, then on) and
caps iterations 1–3. Our fitter is in the right family (analytic Jacobian, LM) but pays 3
transcendentals per sample-iteration and an O(r³) region search.

**Accelerations:**

| # | Item | Class | Gain |
| - | ---- | ----- | ---- |
| D1 | **LN takes Measure's fits.** Persist `outcome.fits` (≈ 200–400 KB/plane) beside the `metrics` artifact; `normalize_frame` maps each fit forward through `frame.map` (`forward_exact`, R-T4-3) and matches against the reference tree; RCR unchanged. Three constraints: β resolved once per group (D9) so target and reference fit at one β; flux corrected by `|det J|` under a non-unit map (M4b native/cross-scale); Register's saturation/ecc/SNR cuts applied to the reused fits. | c | **−46 to −48** |
| D1′ | *Alternative if D1 is too large:* LN `max_stars` 24 576 → 2 000, `centroid_refine` only on the 100 brightest seeds, `NoiseRelative` levels. | c | −30 to −40 |
| D2 | Register reuses Measure's fits for mono frames (`StarFit → Star` is a projection; the centroid is better than refine's) | c | −5.9 |
| D3 | Dedupe `background_and_noise` (thread the pair into `detect_stars_adaptive`) | a | −1.5 |
| D4 | Skip the dead noise map in `estimate_background_mesh` (flag) | a | −0.4 |
| D5 | Kill the copy chain (`Cow` for one channel, in-place ADU scale, fold the max) — also drops the measured residency 8 → ≈ 5 planes ⇒ **admission 4 → 6–7 on 16 GB**, the biggest WALL lever for Measure/LN short of D1 | a | −1 + wall |
| D6 | Moffat LM: `powf(-β-1) = power / base`; `powi` for integer β; skip `ln(base)` when β is fixed; SoA samples with implicit grid coordinates; thread-local `cholesky_solve` scratch; reuse the accepted step's residuals | b | −10 to −16 (−1.5 to −2.5 after D1) |
| D7 | `hfd_at`: copy the window once, constant `r²` table, `select_nth` medians | a (medians identical) | −10 to −14 (−2 after D1) |
| D8 | `sampling_radius` incremental ring; `fit_one` scratch reuse | a | −1 to −1.5 |
| D9 | Auto β once per group (prerequisite for D1) | c | −0.6 |
| D10 | `b3_spline_smooth`: border/interior split, the existing NEON/AVX2 row kernel, ping-pong buffers | b | −1.3 to −2 |
| D11 | Parallelise the two histogram passes and the MRS significance sweeps (per-row partial histograms, integer reduce) | a | wall −1 to −2 |

### 3.2 Resampling — 11 min for 18 Gpx, 660 cycles per pixel for 20 FMA

Every channel-frame is warped twice with identical kernel, map and geometry: once whole in LN
(`ln/mod.rs:437-468`), once band by band in Integrate (`registered_source.rs:178-249`), plus a
forward map per source pixel in drizzle. `warp_rows` / `sample_at` (`resample/warp.rs:34-124`)
per output pixel: one **boxed `dyn Fn`** inverse-map call even for an affine map, two `taps_for`
returning 48-byte `Taps` by value with **8 kernel-weight polynomials recomputed per pixel**, a
data-dependent renormalisation branch with ≈ 4 f32 divides, 16 gathers each behind three `clamp`s
and a bounds check (≈ 96 compare/selects), and two nested loops with runtime trip counts — ≈ 160
guard operations per 20 useful FMAs. Measured 167 ns/px/core (≈ 660 cycles); a monomorphised
4×4 separable resampler with hoisted row pointers is 100–150 cycles scalar, 30–60 with NEON.
Also: every band is encoded f32 → LE bytes → f32 (`store_f32_le`, `decode_*`) — 143 GB of pure
copy per pass, twice.

**What the field does:** a mainstream image library precomputes a 32-phase fixed-point weight table and tiles the
output (≈ 2× documented); the classic three-shear decomposition for affine maps; the
in-tree `LnScratch::wx_table` (`ln/grid.rs:299-325`, R-M4a-19) is the same LUT idea applied one
module over.

| # | Item | Class | Gain |
| - | ---- | ----- | ---- |
| W1 | Specialise `sample_at` per kernel; interior fast path (no clamps, no bounds checks, const 4×4) + border path; hoist row base pointers | a | 2.5–3.5× on the warp |
| W2 | Incremental inverse map along a row for affine/homography (2 adds per pixel instead of the boxed call + 2 divides); phase-indexed weight table (the `wx_table` trick) | b | ×1.3–1.6 on top of W1; **W1+W2 ≈ −7.5 to −9 min** on both warps |
| W3 | Drop the f32 → bytes → f32 round trip for `F32Le` sources (a `&[f32]` view per frame per band) | a | −0.5 to −1.2 |
| W4 | `Linear::apply` divides by `w` unconditionally (`linear.rs:51-58`) even for affine where `w == 1` | a | −0.3 to −0.6 |
| W5 | Warp once: Integrate reads LN's warped plane (the writer exists, `register/writer.rs`; 72 GB scratch) — only worth it on a local folder and only if W1/W2 do not land first | a | −4.7 (−1 to −1.5 after W1/W2) |
| W6 | Separable two-pass warp for affine maps | c (PSF changes) | −2 to −4, overlaps W1/W2 |

### 3.3 Integrate combine — `medfit_line` is 6.0 of the 10.7 min

The M4c acceptance runs are the measurement: the same four planes took 10.7 min with the Auto
linear fit and 4.7 min with min/max (`2026-09-12-m4c-acceptance-run.md:15-19`), so the robust
line costs 6.0 min and the floor (warp + gather + sort + mean) is 4.7. Per pixel at n = 208
(`combine.rs:771-979`): a stable `sort_by` that mallocs 1.6 KB per pixel stack (driftsort), then
per outer iteration (I ≈ 3) a `medfit_line` whose bisection always runs **12 halvings** — the
bracket is re-derived as `b ± 3σ_b` on every call (`:846`) so the documented warm start saves
only the 0–2 widenings — and each of the ≈ 15 `rofunc` evaluations is 4.5 passes over the stack
(residual rebuild, `select_nth`, an even-n half-fold, a single-accumulator sign sum). ≈ 39 000
element-visits and 2 heap allocations per output pixel; `rofunc` is 91 %, `select_nth` alone 44 %.
The side-attribution sort after rejection (`engine.rs:1147-1151`) is provably redundant —
`work[..kept]` is already ascending. `init_row_state` allocates 10.4 MB per rayon LEAF (208 LN
row buffers), ≈ 0.5 M page faults per plane. The gather, the per-row allocations and the
frame-major layout are NOT the problem (0.5 % each) — a pixel-major transpose is a negative result
unless the algorithm changes to a vectorisable IRLS line.

"≈ 3 effective cores" was a measurement artefact: `combine_ms` is a wall window that now also
contains the prefetch's warp on the same pool, and 6 of the M4's 10 cores are E-cores.

**What the field does:** a common open stacker fits an ordinary least-squares line to the sorted stack in closed
form with pixel-independent rank moments and rejects by mean absolute deviation; a widely used
sigma-clip routine got 6–14× from vectorising across pixels and stopping on convergence; a Python stacking suite's
Winsorized/ESD/biweight reducers run as fixed-iteration tensor ops with no per-pixel sort; a robust-regression
coadd method uses Huber IRLS weights `ψ(z)=ρ'(z)/z` — no order statistics at all.

| # | Item | Class | Gain |
| - | ---- | ----- | ---- |
| I1 | `rofunc`: fuse the even-n median into one selection, warm-start the pivot from the previous evaluation (a median is a deterministic order statistic) | a | −0.6 to −1.35 |
| I2 | Four accumulators in the sign sum (integer addends ≤ 207 — every partial sum exact) | a | −0.8 |
| I3 | Reuse `t = b·i` between the residual build and the sign test; thread-local `i as f64` table | a | −0.75 |
| I4 | `sort_unstable_by` with an explicit frame-index tiebreak at the `(f32, u16)` call site (pre-sort order is ascending frame index ⇒ identical permutation) | a | −0.15 + 26 M allocs/plane |
| I5 | Drop the side-attribution sort (median = `work[kept/2].0`) | a | −0.1 + 26 M allocs/plane |
| I6 | Per-thread LN row buffers instead of per-leaf | a | −0.1 |
| I7 | Break the confirming outer iteration (last iteration rejected < 0.1 %) | b | −2.2 |
| I8 | Warm-start the bracket WIDTH from the previous iteration (12 → ≈ 4 halvings) | b | −2.0 |
| I9 | Bisection tolerance 1e-3 → 1e-2 σ_b | b | −1.2 |
| I10 | IRLS/Huber or repeated-median line (4–6 passes instead of ≈ 63; vectorisable) | c (recalibrates `LINEAR_FIT_SIGMA_SCALE`, R-M4a-17) | −5 to −5.4 |
| I11 | `combine_cpu_ms` (thread CPU time) beside the wall `combine_ms`; histograms of I and E per plane | instrumentation | — |

I1–I6 keep every M1–M4d pin: ≈ 10.7 → 6.5–8.2 min. I7+I8 on top: ≈ 5.5 min. The floor is 4.7.

### 3.4 Drizzle deposit — ≈ 1 000 f64 ops and ≈ 70 divisions per source pixel

`deposit_band` (`drizzle/mod.rs:856-1016`) per source pixel with the square kernel at 2×/0.9:
five `fwd.at` (each 6 mul, 6 add, 2 div — `Linear::apply` divides by `w` even when it is 1), a
`map_drop` with 8 finite checks and a shoelace, then 4–9 Sutherland–Hodgman `clip_area` calls of
≈ 100–120 f64 ops with ≈ 8 divisions each. Measured 422 ns/px ≈ 1 700 cycles. For a LINEAR map the
mapped drop is the same parallelogram for every pixel — only the sub-pixel phase varies —
so the clipping can be tabulated. `band_source_window` returns an axis-aligned bbox, so at a 1°
rotation 43 % (5°: 213 %) of the scanned pixels pay the whole `map_drop` before the row loop finds
nothing to deposit. The `I/W` and weight passes are < 0.5 %.

**What the field does:** the reference drizzle implementation's `turbo` kernel replaces the polygon clip with an axis-aligned
`dx·dy` overlap (O(1) per candidate pixel, "significant speed increase"); the pixmap is
precomputed once per frame outside the per-pixel loop; a Python stacking suite partitions the OUTPUT into
thread bands and lets every thread scan the whole source — lock-free, bit-identical to serial.

| # | Item | Class | Gain |
| - | ---- | ----- | ---- |
| Z1 | Skip `map_drop` for a source pixel whose drop centre cannot reach this band (one compare on the already-computed `oy`) | a | −1 to −3 |
| Z2 | Hoist the constant drop parallelogram for linear maps (4 of 5 `fwd.at` and 8 of 10 divisions go) — Tier B, not Tier A: the hoisted corner arithmetic rounds differently from the per-pixel version, so this is same-math-bits-move, not bit-identical | b | −0.9 to −1.3 |
| Z3 | Phase-indexed overlap-area table (32×32 phases × ≤ 9 entries, 111 KB; per frame for linear maps, per 256-px tile with distortion): ≈ 60–80 ops and 0 divisions per source pixel | c | **8–13× on the deposit: 8.6 → ≈ 1 min**; level preservation (R-M3-2) exact by construction; per-frame weights move ≤ 1.6 % nearest / < 0.1 % bilinear, averaging to 0.1 % / 0.007 % over 208 frames |
| Z4 | A `turbo`-style axis-aligned kernel as a user option (the reference implementation's semantics) | c | ≈ Z3's gain with a documented approximation |
| Z5 | Frame-major loop with three accumulator pairs for OSC (or a cached mosaic) to read each frame once | a | −0.3 (−2.7 to −5 min under TPS, whose grid is rebuilt per plane) |

### 3.5 LN background model and the plane copies

`background_grid` (`ln/background.rs`) is serial and allocation-bound: `clean_plane` clones the
104 MB plane and makes two full passes; 1 700 cells × (`gather_cell` growing a `Vec` from empty,
up to 5 rounds each allocating three `Vec`s and running two `select_nth`) ≈ 17 allocations and 11
selections per cell ⇒ ≈ 20 M allocations per run; `median_of_finite` (`ln/mod.rs:173-179`)
copies the plane twice more (104 MB `collect`, then `median_of`'s `to_vec`) for a placeholder value.

| # | Item | Class | Gain |
| - | ---- | ----- | ---- |
| L1 | `median_in_place` on the already-owned `finite` buffer | a | −0.3 |
| L2 | Parallelise `clean_plane` and the cell loop; per-worker scratch; `retain` instead of `filter().collect()` | a | −1.1 to −2.3 |
| L3 | Clip-then-bin 4×4 before the model (the model lives on a 128-px stride) | c | −1 to −2 |
| L4 | `median_of_finite` from the existing 1/16 stratified sample | c (0.08 % precision) | −0.3 |

### 3.6 Idle cores — the cross-cutting wall lever

Every profile shows ≈ half the thread time waiting. The serial parts are: the whole detector
(`adaptive_detection.rs` has no rayon), the two 26 Mpx histograms, the MRS significance sweeps,
`background_grid`, the nearest-valid fill, `dedupe`. With admission 4 (Measure/LN on 16 GB) the
machine runs ≈ 4 cores through these. D5 (residency 8 → 5 planes ⇒ admission 6–7) and D11
(parallel histograms) recover most of it without touching any number; D1 removes the largest
serial phase outright.

## 4. Literature cross-check (one line each)

- **Source extractors**: one pass, 64² mesh background, matched filter, label, peak;
  ≈ 50 Mpx/s and 10 k sources/s on one 3 GHz core; a flexible C++ rewrite cost 10×.
- **Photometry library**: 10–400× from separable box filters; cutout/moment work vectorised across stars.
- **Batched GPU fitter**: fixed 5×5 stamps, analytic Jacobian, one block per fit; ≈ 1.8×10⁴ fits/s per CPU
  core baseline; GPU only pays above tens of thousands of fits per batch.
- **Open stacker**: PSF fit warm-started (rotation off → on), iteration cap 1–3; rejection over a
  contiguous per-pixel `stack[N]`, quickselect medians, OpenMP over row blocks; linear-fit
  clipping = closed-form OLS on the sorted stack with pixel-independent rank moments + MAD.
- **Vectorised sigma clip**: 6–14× from vectorising across pixels and stopping on convergence.
- **Mainstream image library (affine warp / remap)**: 32-phase fixed-point weight LUT (≈ 2×), 128×64 tiles.
- **Reference drizzle implementation**: `square` = per-pixel `boxer` clip; `turbo` = axis-aligned `dx·dy`; pixmap
  precomputed per frame; parallelism per exposure, not per pixel. No GPU drizzle exists.
- **Python stacking suite**: every rejection a fixed-iteration tensor op (JIT/tensor kernels), no per-pixel
  sort; own projective drizzle with output-row-banded lock-free deposit; global (not local)
  normalisation; memory-mapped tile-by-tile stacking.
- **Robust-regression coadd method**: joint multi-frame deconvolution with Huber-IRLS weights via multiplicative updates;
  0.12 s/iteration on a V100 at 1 k² — not a coadd benchmark; its transferable idea is the
  closed-form robust weight `ψ(z)=ρ'(z)/z` (I10).

## 5. SIMD — the current state, plainly

rustafits carries hand-written NEON/AVX2 for `convolution.rs` (the classic peak-finder's 5-tap kernel),
`binning`, `stretch`, `color`, `debayer` and the FITS byte swap — the blink/preview paths. **None
of the stacking hot kernels is vectorised**: `fitting.rs`, `background.rs` (`estimate_noise_mrs`
runs the scalar `b3_spline_smooth`, not the NEON row kernel beside it), `adaptive_detection.rs`,
and in `athenaeum-core` the warp, `medfit_line`, the drizzle deposit, the LN background — zero
intrinsics. Auto-vectorisation is blocked by shape: a `dyn Fn` per pixel, runtime trip counts,
≈ 96 clamps per pixel, AoS f64 samples, three `powf` per sample. `target-cpu` is not set (and
cannot be for a distributed binary; runtime dispatch is the rustafits pattern). So the honest
order is: remove the duplicated work (D1, D2), fix the algorithmic shapes (D6–D8, W1, I1–I8, Z1–Z3),
THEN vectorise the loops that remain dense (the warp's 4×4, the B3 spline, the LM Jacobian, an
IRLS line) — where 2–4× per loop is realistic and the existing rustafits dispatch is the model.

## 6. GPU, revisited in one paragraph

Unchanged from the first audit: the GPU-shaped kernels (warp, deposit, the separable convolutions)
are ≈ 30 % of the run today and shrink to ≈ 15 % after tiers A–B; the two CPU-bound leaders after
that (the LM fit's transcendentals and the robust line's order statistics) are the SIMT-hostile
ones. the batched fitter's own threshold — batches of tens of thousands of fits — is met per frame, so a
batched-LM spike is the one GPU experiment with a plausible payoff, and only after D1 removes the
duplicate fits. Not in this cycle.

## 7. Plan — three tiers

**Tier A — bit-identical (≈ 1 week).** Instrumentation first: split LN's `scale_ms` into
detect/refine/fit/match (the same `MeasureTimings` shape), add `combine_cpu_ms` and the I/E
histograms (I11). Then D3, D4, D5, D7, D8, D11, W1, W3, W4, I1–I6, Z1, Z5, L1, L2; gate the
tier-1 prefetch and pool routing on `StorageClass::Network`. Every pin stays green.
Expected: **−8 to −10 wall min** (Measure/LN admission 4 → 6–7, Integrate 10.7 → ≈ 7, drizzle −2,
warp −3), i.e. ≈ 49 → 39–41.

**Tier A — MEASURED (2026-09-20, `docs/superpowers/research/2026-09-20-stacking-compute-tierA-acceptance.md`).**
On the reduced set (`LDN1272-test`, 197 frames): 27.00 → 23.44 min against a same-evening baseline
re-run (−13 %; 26.6 → 22.3 against the cool-machine morning baseline), byte-identical on every artifact. Per item: D3 −18 ms/plane detect; D4 −40 ms/plane background; D5 −4 % detect,
residency UNCHANGED (the peak is `noise_mrs`'s three live planes — R-TA-3, admission gain
withdrawn); D7+D11 Register detect −45 %, LN detect −30 % in the pipeline; D8 fit −1…−7 %; W1
warp 2.0×; W3 read −8.5 % (≈ 3× per-sample sweep); W4 write −22 % (probe); I1/I2/I4 measured
1.05–1.7× SLOWER and reverted, I3 already minimal, I5/I6 neutral; Z1 −5.8 % on a 2.45°-tilted
sample, neutral axis-aligned; Z5 refused by the R-M3-7 ceiling; L1/L2 background −45 %. Not in
this tier's list but the largest gain: W5 (write the warp once; owner decision 2026-09-19) —
LN warp −96 %, Integrate read −75 %, Register +1.1 min, 41 GB. The combine estimate
(10.7 → ≈ 7) did not materialize: kernel micro-rewrites lose on this toolchain.

**Tier C — MEASURED (2026-09-20, `docs/superpowers/research/2026-09-20-stacking-compute-tierC-acceptance.md`).**
Executed straight after Tier A on the owner's word (Tier B skipped — its items were either
absorbed here or measured out). On the reduced set: 21.73 → 14.47 min against a Tier A build re-run back to back (−33 %; 23.44 →
14.47, −38 %, against Tier A's own acceptance run on a hotter evening; 27.0 → 14.5 against the
pre-audit baseline, −46 %); every gated §8 row PASS against the Tier A
ruler (masters within 0.1 % median / 1 % MAD / 2 % noise / 1 % FWHM, rejected fraction
+0.002 pp, per-frame weights ρ = 1.0, LN scale −0.035 %), the external gate PASS. Per item: D1
(LN takes Measure's fits) — not as FLUXES: a Moffat fit's integrated signal is not
warp-invariant (up to 20 %, FWHM-dependent), so the fits are SEEDS re-fitted on the warped plane
at the group β, with a per-channel calibration `k` from a weight-stratified 7-frame sample
(rulings C-12…C-17); LN scale 6.5 → 0.64 s per frame, Normalize 5.70 → 2.01 min. D2 (mono
Register from the fits) — detect 730 → 0 ms, the frame is not even read (header only),
Register 2.36 → 1.52. Z3 (drizzle phase table) — 64 phases, not 32 (the plan's 2 % per-pixel
bound is unreachable at 32); deposit −85 % per plane, Drizzle 4.26 → 1.18. D6 (Moffat LM
arithmetic, rustafits) — one transcendental per sample-iteration, residual reuse, Cholesky
scratch: fit −30…−49 %, iteration counts identical, ≤ 7e-15 px, Measure 4.48 → 3.67. I7/I8
(`medfit_line` early exit / warm bracket) — the early exit ALREADY EXISTED; the warm bracket
measured 0.966× and was reverted (C-26): Integrate unchanged at 4.0. L3 (LN background on a
4×4-binned plane) — background −80 % on the probe, but on the full group the `B` grid moved
2.4e-3 of sky at the median node and the masters failed §8 (mono MAD +1.9 %, OSC blue FWHM
+2.7 %); reverted (C-29). The two absolute drizzle rows (level 0.998–1.002, coverage 1.0) read
FAIL on this set for data reasons the baseline shares (rulings C-20/C-23). Lessons: a
per-frame numeric change is judged on the whole group at master level, never on a few-frame
probe (Task 7's 3-frame probe understated the move 2.5×); "small gain + any §8 miss" is a
revert with the numbers in the doc comment (C-26/C-29).

**Tier B — same math, bits move (≈ 1 week + one acceptance run).** D6, D10, W2, I7–I9 (+ D9), Z2
(§3.4 — its hoisted corner arithmetic rounds differently from the per-pixel version, so it needs
the hash-bump + acceptance re-run every other Tier B item does).
Expected another **−7 to −9 min** → ≈ 30–33.

**Tier C — reuse and algorithm (≈ 2 weeks + acceptance).** D1 (LN takes Measure's fits, with D2
for mono Register), Z3 (drizzle phase table), L3; I10 only if Tier B leaves Integrate above
5 min. Expected **−12 to −18 min** → **≈ 20–25 min** for the reference set, with drizzle at ≈ 1 min
and LN at ≈ 4–5.

Acceptance for B and C is the existing harness (`docs/superpowers/research/scripts/acceptance/`,
`.athenaeum-acc/tier1-*` catalog copies, `tier1-run.sh`/`tier1-extract.py`/`tier1-compare.py` in
the session scratchpad — worth moving beside the harness) against the LDN 1272 baseline masters
already on disk, on a verified-quiet machine (Time Machine and Spotlight excluded from the
acceptance folder; both were found running during the tier-1 head runs).

## 8. Rulings this audit proposes (owner's call)

1. The tier-1 branch merges as the safe base (byte-identical, instrumentation, honest admission)
   with the prefetch/one-pool changes gated on network storage rather than reverted.
2. Tier A starts without a further design round — every item is local and pin-guarded.
3. D1 is the one architectural item (a persisted per-plane star list, a group-wide β); it gets its
   own short design note before Tier C.

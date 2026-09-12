# Stacking pipeline — design

**Date:** 2026-09-08
**Status:** approved in dialogue (§1–§10 on 2026-09-08, screen layout A the
same day); spec under owner review.
**Math reference:** `docs/superpowers/research/2026-09-08-stacking-math-reference.md`
(every formula, constant and default cited below lives there with its
provenance; this document only names them).
**Baseline data:** `~/Pictures/Calibration Test/LDN1272-Output/` — a WBPP 3.0.1
run over the same 368 lights (2 h 37 min, ~170 GB of intermediates, every
process parameter block in `logs/20260908121336.log`). Its two master lights
and two drizzled masters are the acceptance comparison.
**Supersedes:** the pixel half of
`docs/superpowers/plans/2026-06-10-stacking-engine-roadmap.md` (Phases B–E).
Phase A of that roadmap is done: the calibrated-export generator
(`docs/superpowers/specs/2026-08-31-calibrated-export-v2-design.md`) is the
pipeline's first stage.

---

## 0. Owner decisions (2026-09-08)

1. **Hybrid materialization.** Calibrated frames are written to a working
   folder; registration stores transforms, not pixels; integration, local
   normalization and drizzle resample calibrated frames on the fly, band by
   band. Writing registered frames is opt-in.
2. **Incremental milestones**, each its own spec review + implementation plan:
   M1 first master light; M2 local normalization; M3 drizzle; M4 polish.
   This document is the program design; the M1 plan is written from it.
3. **One reference frame per frame set.** Every integration group is
   registered onto the same reference geometry, so the masters are
   co-registered (what WBPP did on the owner's data: the OSC group was
   registered onto the mono reference and written at its 6224×4168 geometry).
4. **Screen layout A** — pipeline board + inspector (§11).

## 1. Scope and principles

**What it is.** A frame set becomes one master light per *integration group*
(camera × colour mode × filter × binning × geometry, optionally × exposure).
The pipeline starts from raw lights and built masters; calibration and VNG
debayer are its first stage and reuse the calibrated-export generator
unchanged.

**What it is not.** Not a mosaic tool (the output geometry is the reference
frame's, never a union), not a comet stacker, not a multi-set stacker, no GPU.
XISF output, cataloging of masters and Bayer drizzle are M4.

**Quality target.** The same algorithm family as the WBPP run, measured on
LDN 1272 against the WBPP masters (§13). Where a vendor parameter could not be
verified, the value is ours and is labelled so in the math reference.

**Rules carried over from `CLAUDE.md`.** Two backends in sync; logic in
`athenaeum-core`; `tracing` only, canonical field names; never name the
reference implementation in code or comments; the headless
`--no-default-features` build must pass (the whole stacking module is gated on
`render` + `solver` exactly like `registration` and `plate_solve`); engine
identity for existing master builds is pinned (the master-build path keeps its
fingerprint test).

## 2. Stages and grouping

| # | Stage | Input → output | Reused when |
| ---- | ---- | ---- | ---- |
| 0 | Plan | set + config → groups, gate blockers, disk estimate, the masters the run will build | never (cheap, pure DB) |
| 0.5 | Masters | (owner requirement 2026-09-09) every linked raw calibration set without a built master is built, and every built master whose file is missing from disk is rebuilt from its provenance, inside the run, in dependency order (bias/darkflat → dark → flat), through the master library's own builder under the run's queue permit; the plan lists them (`mastersToBuild`) instead of blocking; a set that cannot be built (raw frames not on disk, no provenance) stays a blocker with the export wording; the gate also lists the pre-calibration master(s) a missing flat master's rebuild would read (`select_flat_precal`'s choice), so they build first | a built master whose file is on disk |
| 1 | Calibrate | raw light + linked masters → `calibrated/<group>/c_<stem>.fits` (mono: 1 plane; OSC: VNG-debayered, 3 planes) | artifact row exists, spec hash matches, file present |
| 2 | Debayer | folded into stage 1 for OSC groups (the generator debayers before writing); shown as its own row, whose status and progress mirror stage 1 for the OSC groups, so the user sees it | with stage 1 |
| 3 | Measure & select | calibrated frame → metrics, weight, included/excluded | metrics cached for (artifact, measurement config hash) |
| 4 | Reference | `frame_set_reference` if the user set one, else max weight over included frames of all groups | never |
| 5 | Register | detections vs reference detections → transform + QA | `registration_results` row for (reference, config hash, artifact unchanged) |
| 6 | Normalize | global: per frame per channel location/scale vs reference; M2: local normalization grids | global: recomputed (cheap); LN sidecars cached |
| 7 | Integrate | lazy registered band source → master, rejection maps, stats | never |
| 8 | Drizzle (M3) | calibrated + transform + rejection bitmaps + weights + LN → `_drizzle<s>x` | drizzle off |
| 9 | Output & finish | masters written, provenance rows, notification, cleanup policy | never |

**Grouping keys (revised 2026-09-10, owner decision — "different cameras
can be integrated together, as long as exposure (within the exposure
threshold set in the integration settings), camera type and filter
match").** Colour mode (mono / CFA, from the Bayer cards), `FILTER`
(sanitized, `NoFilter` when absent), `XBINNING`, and an exposure cluster —
exposure is now ALWAYS a key, there is no opt-out toggle. `INSTRUME` and
native geometry (`NAXIS1`/`NAXIS2`) are **not** keys any more: a group may
mix frames from several cameras and sensor sizes; registration warps every
included frame onto the ONE run-wide reference regardless (§3.4 — no new
gate). The exposure cluster: frames sorted by `EXPTIME`, greedy clustering —
a frame joins the current cluster when its exposure is within
`exposureToleranceSec` (default 2 s) of the cluster's own first value, not
the previous frame's; a frame with no `EXPTIME` never joins a numeric
cluster (a missing value is not "0 s") — it gets its own cluster, labelled
`unknown`, and the plan carries a warning naming those frames (never a
blocker). The group key is the stable string
`<mono|osc>__<filter>__bin<n>__<exposure cluster>` used in paths and rows
(`180s`, `0.39s`, or `unknown`). `IntegrationGroup`/`PlanGroup` carry
`cameras: string[]` (every distinct camera actually present, sorted) and
`instrume` as a DISPLAY-only value (the reference-anchor member's own
camera — the best-weighted included member once weights exist, the first
member by `(date_obs, id)` at plan time). M4b (mixed pixel scales): every
`GroupFrame` also carries `pixelScaleArcsec`/`scaleSource` (a stored plate
solve when the frame is solved, else the header's `FOCALLEN`/`XPIXSZ`), the
group's own value is the median of its members', and the plan gate warns —
never blocks — when a group's scale sits outside `[1 / SCALE_TOLERANCE,
SCALE_TOLERANCE]` of the resolved reference's own, or when a single group's
members themselves span that range.

**Gate** (stage 0, the one gate for the Run button and for `start_stacking`):

- `check_mode_ready(calibratedLights)` from the export gate, **reinterpreted
  since 2026-09-09 (owner requirement — the pipeline builds its own
  masters):** every light has at least one link (else the `links` blocker,
  export wording, `→ Coverage`); a linked raw calibration set without a
  built master becomes planned work (`mastersToBuild`, kind `build`) when
  its frames are on disk, and a blocker (`masters`, "restore from archive
  first" / "N raw frames missing") when they are not; a built master whose
  file is missing becomes planned work (kind `rebuild`) when its
  `master_provenance` row exists and its source frames are on disk, and the
  `masterFiles` blocker otherwise. Stage 0.5 executes the list.
- The reference frame (manual or auto) is on disk.
- Working and output folders validate (§9.4) and free space ≥ the estimate.
- At least 3 included frames in at least one group.

## 3. Registration

Registration runs on **calibrated** frames (flat-corrected, hot pixels
removed — cleaner detections, and what WBPP registers). Geometry equals the
raw frame's; the transform maps calibrated-frame pixels to reference-frame
pixels, 0-based pixel-centre convention throughout (FITS 1-based only at the
WCS card boundary).

### 3.1 Detection

`detect_fast` with Moffat centroid refinement (~0.05 px on well-sampled stars),
per-star σ from the fit; the detector is threshold-free (its adaptive ladder
targets `maxStars`), so there is no detection sigma. Cuts: saturation (peak ≥ `upperLimit` of the
plane's range), eccentricity > 0.8 (the existing `select` rule, moments-based),
SNR < `minSnr` (10). Keep the **2000 brightest** by flux (`maxStars`,
configurable). For RGB frames detection runs on the luminance
`0.25R + 0.5G + 0.25B`.

### 3.2 Matching

1. **Seed**: the existing scale-invariant quad matcher (distance ratios;
   mirror-invariant, which is why flipped frames register today) → seed
   similarity/affine from the quad centres.
2. **Correspondence**: project every subject detection through the seed,
   **KD-tree** nearest neighbour in the reference list within
   `ransacTolerancePx` × 2 (replaces the O(N·M) scan).
3. **RANSAC** on the configured model: minimal samples (2 pairs similarity, 3
   affine, 4 homography), inlier tolerance `ransacTolerancePx` (1.9),
   `ransacMaxIterations` (2000) with the adaptive stop
   `N = log(1 − 0.9999)/log(1 − w^k)`, early exit above 98 % inliers,
   deterministic seed (the solver's `ransac_seed` convention). Model score =
   inliers × overlap × regularity / (1 + RMS) with the three quality indexes
   defined as in the math reference §5.2.
4. **Refit** on the inliers by σ-weighted least squares
   (`w = 1/(σx² + σy² + ε)`, uniform when unrefined) with iterative 3σ
   clipping (≤ 5 rounds, stop on Jaccard > 0.97 of the inlier set).
5. Optional **distortion** on the residuals (§3.3).

### 3.3 Transformation models

| Model | Params | Use |
| ---- | ---- | ---- |
| `similarity` | 4 | same rig, few stars (auto below 12 inliers) |
| `affine` | 6 | auto for 12–29 inliers; the legacy `registration_results` shape |
| `homography` | 8 | **default**; normalized DLT (Hartley); flips are a negative determinant, nothing special |
| `polynomial2..4` | + (order+1)(order+2)−6 per axis, per direction | fitted on the residuals of the linear model; forward and inverse fitted independently (the plate solver's SIP convention); auto-enabled for a subject whose geometry differs from the reference's (the `INSTRUME` half of the cross-camera rule waits for the orchestrator to pass it — Plan 5) with ≥ 200 inliers whose inliers are consistent (overlap index ≥ 0.6 — inlier hull over the matched pairs' hull) and cover the frame (regularity index ≥ 0.6 — fraction of a 4×4 grid holding an inlier); an explicit order is always honoured |
| `tps` | ≤ 600 nodes | regularized thin-plate spline over the refit inliers; smoothing λ (`registration.tpsSmoothing`); nodes chosen grid-stratified, never the first N; evaluated through a cached 8 px displacement grid — M4c, rulings R-M4c-5/6 |

`model: auto` resolves per frame as above; the resolved model is recorded.

**Thin-plate-spline distortion (M4c, rulings R-M4c-5/6/7).** The polynomial
layer is a GLOBAL surface: a degree-3 fit cannot follow a residual field that
changes sign twice across the frame, however many stars it is fitted on
(measured on a 1.5 px checkerboard field: cubic 0.93 px RMS at the inliers and
1.19 px off them, spline 0.001 px and 0.108 px). The spline is local by
construction. `geometry/tps.rs` fits the classic order-2 RBF `φ(r) = r² ln r`
with an affine part — two independent scalar splines (the x and the y
displacement) over one node set, Bookstein's bordered system, coordinates
normalized to the node cloud's own bounding-box diagonal so `λ` is
sensor-independent. The bordered system is INDEFINITE (`φ(0) = 0`, so the
kernel block has zero trace and eigenvalues of both signs; `r² ln r` is only
conditionally positive definite), so it is solved by dense Gaussian
elimination with partial pivoting, `O(n³/3)` — there is no Cholesky of that
block to Schur-complement against, and a diagonal ridge cannot create one.
Forward and inverse are fitted independently, each at its OWN evaluation
points in reference space (`L(sub)` and `ref`), exactly as the polynomial arm
does. Nodes are capped at `TPS_MAX_NODES = 600` and chosen grid-stratified
over a 30×20 cell grid (best-σ pair per occupied cell first, then round-robin);
a frame with fewer than `4 · MIN_INLIERS = 32` refit inliers keeps the linear
model with a warning, since a local model fitted on a handful of stars says
nothing about the rest of the frame. Coincident nodes are dropped first
(`TPS_MIN_NODE_SEPARATION_PX = 0.05` px in either direction's node positions):
the correspondence search gives every subject star its own nearest reference
star independently, so two subject stars can pair to ONE reference star, and
the inverse spline's node set would then carry that position twice — an
exactly singular system that sinks the whole fit. `distortion: auto` never
resolves to the spline — it is always a deliberate choice.

**Pixel work and everything else are separate paths** (ruling R-T4-3).
Evaluating a spline costs one logarithm per node per query, so a 26 Mpx frame
cannot go through it: `PixelMap`'s distortion is an explicitly tagged enum
(`{"kind": "polynomial" | "tps", …}`, an untagged `transform_json` — every row
written before M4c — decoding as polynomial), and its `tps` arm samples the
spline onto a `TPS_GRID_PX = 8` px grid over the fitted domain, **one grid per
direction, each built lazily only when that direction is first asked for
through the pixel path** (the resampler only ever asks `inverse`, drizzle only
`forward`), **shared behind an `Arc` so the several clones of a registered
frame's map that run state holds are one allocation, not several**. Everything
that evaluates a map a few hundred times instead of a few million —
registration's residual statistics, star re-pairing, a coverage probe, a band's
source window in the other direction — calls `forward_exact` / `inverse_exact`
and builds nothing.

**A grid never outlives the stage's per-frame work** (ruling R-T4-6). Every
stage that resamples a frame releases its grid when that frame is done — the
registration writer after it writes, `RegisteredSource`'s `Drop` for
integration and both of local normalization's warps, drizzle after each frame's
deposit — and because the clones share ONE cache, releasing through any of them
frees it for all. Pixel loops take a HANDLE once per band or per frame
(`PixelMap::forward_eval` / `inverse_eval`, `InverseMap::inverse_burst`) and
sample through it: no lock and no atomic per pixel, and a release during a
running band cannot pull the data out from under it. The cost of that is honest
and bounded: a `tps` frame rebuilds its grid once per stage that resamples it
(≈ 1 s each on a 26 Mpx frame, so three to four builds per frame more than a
polynomial run), in exchange for never holding more than a few grids at once.
Without it, acceptance run 27 (208 mono frames at 6224×4168, LN on, drizzle 2×)
integrated in 11 minutes and then sat at 0 % CPU for 2.7 hours with 11.3 of
12 GB of swap in use — every frame's inverse grid alive from registration
onward, plus a forward grid each in drizzle.

**A spline's reported RMS is a hold-out measurement** (ruling R-T4-4). At the
default `λ = 0` the spline interpolates its own nodes, so measuring
`registration_results.rms_residual_px` there would report a solver artefact and
leave `maxRmsPx` / `failOnMaxRms` structurally unable to refuse a TPS frame.
Instead: when the node cap left inliers out, those inliers are the hold-out and
the shipped model is measured on them; when every inlier is a node, one in five
is held back, a second spline is fitted on the rest, and ITS error on the
held-out ones is reported — while the shipped model is still the one fitted on
all of them. The number is therefore not comparable with the polynomial arm's
in-sample RMS, and nothing in the pipeline compares them.

`λ` is the weight of the `λ · wᵀw` penalty in px² of the normalized frame; its
useful range grows with the node count (≈ 0.01 for a few dozen nodes, roughly an
order of magnitude higher at the cap), it is clamped to `[0, 10]` by
`resolve_config` like every other numeric config field, and the shipped default
is `0.0` — the interpolating spline — until Task 7's acceptance run picks one
from real frames.

**Local distortion correction loop (M4c, ruling R-M4c-7).** With
`registration.localDistortion` on and any distortion model selected, up to
`LOCAL_DISTORTION_ROUNDS = 3` rounds of: re-pair every subject star THROUGH
the current map (linear part and distortion together) at tolerance
`ransacTolerancePx · (1 + round)`; RANSAC a corrector homography `H_c` on what
the map still gets wrong (predicted reference position → actual reference
position); stop once `‖H_c − I‖_F < LOCAL_DISTORTION_STOP = 1e-3`; otherwise
compose `H_c` into the linear part and refit the distortion around it. A round
is KEPT only when it does not raise the RMS **measured on ONE COMMON pair set —
the incumbent's own inliers** (ruling R-T4-5; comparing each model on its own
inlier set would let a round whose corrector kept an easier subset look better
while being worse on the population the incumbent was judged on). A round that
cannot deliver the distortion the frame asked for is refused rather than
shipped as a silent downgrade. `Alignment.local_rounds` counts the rounds whose
corrector was actually fitted — a converged round included (R-T4-1/2);
`refit_rounds` keeps its own meaning, the σ-clip rounds inside one
`refit_weighted` call. After a kept round `pairs` and `inlier_ratio` are
reported against that round's own widened pairing.

The loop earns its keep exactly where the first pairing was partial. Measured
on the synthetic 1.5 px checkerboard field: the first map is fitted on 415 of
450 pairs — the σ-clip refit drops the legitimate stars in the deepest lobes —
re-pairing through that map recovers all 450, the corrector on the 35 it was
never fitted on is far from the identity, and the round is kept. Where the first
map already saw the whole field the corrector converges on the FIRST round, for
a structural reason worth recording: the refit has already least-squares-fitted
the linear part over those pairs and `Distortion::fit_joint` has already folded
the residual's affine term back into it, so what is left is orthogonal to what
a homography can represent. Implementation: `register/local_loop.rs`.

### 3.4 Output geometry and coverage

The reference frame's W×H, always. A source pixel that maps outside its frame
is **NaN** in the resampled band; the combiner already drops non-finite
samples per frame with accounting, so edges and rotated corners simply have
fewer samples. Frames of a different geometry than the reference (the OSC
camera here) are resampled into the reference geometry like any other.

### 3.5 Interpolation

`nearest`, `bilinear`, `bicubicSpline` (Keys, 4×4), **`bicubicBSpline`
(default — WBPP 3.0.1 sets it explicitly; smoother, better noise behaviour
for the rejection pass)**, `lanczos3`, `lanczos4`, `mitchellNetravali`.
`clampingThreshold` (0.30) applies the math reference's two clamping rules:
the per-row/column linear replacement for the bicubic spline, the
negative-lobe attenuation for Lanczos — both applied **separably**, once per
axis (a 2-D split of every product weight by sign over-counts the negative
lobes on smooth star flanks; measured on the M1 Plan 1 synthetic field it
inflated Lanczos-4 star flux by +1.5 %, the separable form leaves
+0.1–0.2 %, Lanczos-3 is unbiased). Two measured kernel properties the
acceptance run must keep in mind: windowed-sinc kernels carry a
phase-dependent first-moment error of ~0.02 px (uniform over a frame at a
given sub-pixel phase; the cubic kernels have linear precision and sit at
1e-4 px), and Lanczos-4 with the 0.3 clamp keeps the residual one-signed
+0.1–0.2 % flux inflation just named. The resampler is a gather: for every
output pixel, inverse-map to source coordinates, evaluate the kernel. The
inverse map is the stored inverse (homography inverse; polynomial inverse
coefficients), evaluated per pixel with Horner-form polynomials.

### 3.6 Per-frame QA and failure

Recorded per frame: inliers, inlier ratio, RMS, σ_RMS, peak error x/y,
scale, rotation, translation, flipped, quality score, model resolved, time.
A frame **fails** registration when RANSAC yields < 8 inliers, when the
linear fit's scale is outside the frame's own acceptance window — `[r /
1.25, r · 1.25]` around its implied ratio `r` to its reference, which is
`1.0`, i.e. M1's fixed `[0.8, 1.25]`, whenever either scale is unknown
(M4b, ruling R-M4b-2, §3.8) — or when RMS > `maxRmsPx` (2.0)
with `failOnMaxRms` on (default off: warn, keep). A failed frame is excluded
from the run with reason `registration failed: …` when
`excludeOnRegistrationFailure` is on (default on), else the run fails.

### 3.7 Optional registered frames

`writeRegisteredFrames` (default off) writes `registered/<group>/r_<stem>.fits`
(float32, its group's reference geometry — §3.8, the run-wide one in
co-registered mode — NaN coverage, copy-through cards + `ATH_REG`
cards with the transform) after registration, resampling once with the
configured kernel. They are artifacts (§9.3), never cataloged, and are not
read by later stages — the lazy source stays the single code path.

### 3.8 Mixed pixel scales (M4b)

One frame set may hold groups — or members of one group — shot at different
pixel scales: a bin-2 group, a second telescope, another camera. The
pipeline integrates all of them, in one of two modes the owner picks per set
through `registration.geometry` (§9.2): **co-registered** (every group
resampled into the set reference's geometry, one master per group in one
geometry) or **native** (a per-group reference, one master per group in its
own geometry, no cross-group registration at all). Nothing about group keys
changes — camera and native geometry have not been keys since 2026-09-10.

Three mechanisms carry it: every `GroupFrame` learns its pixel scale (§2)
and the plan gate turns a scale spread into a named WARNING, never a
blocker; registration's scale gate becomes per frame, centred on the frame's
own implied ratio to its reference (§3.6); and, when both frames carry a
stored plate solve, a seed built from the two WCS solutions (subject pixel →
sky → reference pixel over a grid, an affine fit) is available to the
aligner.

**Which seed leads is decided per frame, and it is not the WCS one by
default.** Quad matching — scale-invariant by construction — LEADS unless
the frame's implied scale ratio to its reference differs from 1 by more than
`WCS_SEED_RATIO_EPS` (5 %; within-rig solve-to-solve jitter reaches 1.6 % on
real data, so a tighter trigger would put ordinary same-scale frames on the
WCS path — R-T2-1/R-T6-4). When both frames are solved the plate-solve seed
is built regardless of the ratio and serves as the FALLBACK: a quad-seed
failure (`NoSeed`, too few correspondences, RANSAC/refit below
`MIN_INLIERS`) gets one turn through it rather than costing the frame
(R-T6-9). Whichever seed ships the alignment is recorded on the row — `+wcs`
in `registration_results.model` when it was the plate-solve one.

Rulings (M4b plan header, `docs/superpowers/plans/2026-09-10-stacking-m4b-plan-mixed-pixel-scales.md`):

- **R-M4b-1 Scale per frame, two sources, one field.**
  `GroupFrame.pixel_scale_arcsec: Option<f64>` and `GroupFrame.scale_source:
  ScaleSource { Solve, Header }` (`None` when neither exists). Solve wins
  over header (a solve measures, a header assumes). Header formula:
  `206.2648 · xpixsz_um / focallen_mm` — NO binning factor: `XPIXSZ` is
  written by every capture program in this catalog as the EFFECTIVE pixel
  size after binning (verified 2026-09-11 on the owner's catalog: the
  ASI294MM bin-2 lights carry `XPIXSZ = 4.63 = 2 × 2.315`, the ASI6200MM
  bin-2 lights `7.52 = 2 × 3.76`, both with `XBINNING = 2`; multiplying by
  the binning would double-count and report ×2 the true scale). `None` when
  `xpixsz` or `focallen` is missing, non-finite or ≤ 0. `xbinning` is still
  read (the group key and the display need it) but never enters the scale.

- **R-M4b-2 The gate is per frame and centred on the implied ratio.** `r =
  scale_frame / scale_reference` when both are known, else `1.0`; the
  accepted refit scale is `[r / 1.25, r · 1.25]` (`SCALE_TOLERANCE = 1.25`,
  the existing constant's half-width kept). A frame whose refit scale falls
  outside is refused with `ScaleOutOfRange { scale, expected: r }` (the
  message names both). Nothing else about registration's success criteria
  changes.

- **R-M4b-3 WCS seed when both frames are solved.** `seed_from_solves` maps
  a 5×5 grid of subject pixel centres (inset 5 % from each edge) through the
  subject's `WcsSolution::pixel_to_sky` and the reference's `sky_to_pixel`,
  least-squares-fits an affine, and returns it when every mapped point is
  finite and the affine's scale is within `[0.05, 20]`. `align` pairs
  through the hint with radius `WCS_SEED_RADIUS_PX = max(4 ·
  ransac_tolerance_px, 8.0)` (a stored solve's own residual is ≤ 1 px; a bad
  solve pairs nothing); fewer than `MIN_INLIERS` pairs → the hint is
  discarded with a warning `wcs seed rejected (<n> pairs); quad seed used`
  and the quad seed runs as today. `Alignment.seed: SeedKind { Wcs, Quads }`
  records which one shipped; `registration_results` does not change shape —
  the seed kind rides `transform_json`'s sibling `model` string as a `+wcs`
  suffix (e.g. `homography+polynomial3+wcs`) so the frames table can show it
  without a column. [Superseded in part by R-T2-1/R-T6-4/R-T6-9 below: the
  hint is first only above the 5 % ratio; otherwise it is the fallback.]

- **R-M4b-4 Two modes, one config field.** `registration.geometry:
  "coRegistered" | "native"` (`RegistrationConfig.geometry:
  RegistrationGeometry`, default `CoRegistered`). Co-registered = the
  run-wide reference (spec §4.4, two-pass per M4a); native = per group: the
  group's `best_by_weight` member (a manual reference applies to its own
  group only; every other group picks auto), two-pass re-pick per group. The
  run-level `stacking_runs.reference_frame_id` stays the largest group's
  reference in both modes (it is what the plan gate and the results header
  show); `SummaryGroup.reference_frame_id: Option<i64>`
  (`#[serde(default)]`) carries each group's own.

- **R-M4b-5 Geometry follows the reference.** `RunContext` grows
  `group_geometry: HashMap<String, GroupGeometry { reference_frame_id,
  width, height, calibrated: PathBuf, hash: String }>`; every consumer of
  `rc.reference_width/height` (the register admission bound, the LN driver,
  `process_group_output`'s integration and drizzle geometry, the coverage
  filter, the master's WCS lookup) reads `rc.geometry_of(&group.key)`. In
  co-registered mode every entry equals the run-wide one — the M1–M4a
  byte-identical pins therefore keep passing with the default config.

- **R-M4b-6 Level and resolution across scales are the resampler's business,
  not new code.** A coarse frame registered onto a finer reference is
  up-sampled by `RegisteredSource`'s existing inverse-mapped gather (bicubic
  B-spline by default) at the frame's OWN level (normalization runs after
  resampling, per pixel, as today); drizzle's forward-mapped drops map a
  coarse source pixel onto a `r × r` output-pixel quad through the same
  `PixelMap` — the exact clipping already handles any quad, and `I / W`
  stays level-preserving (Task 4 pins both). A finer frame onto a coarser
  reference is down-sampled by the same gather (aliasing accepted — the spec
  says "the finer scale loses resolution"; a pre-filter is M4c's
  interpolating-prefilter item).

- **R-M4b-7 Warnings, never blockers.** The plan gate emits at most one
  warning per group: a group whose scale is outside `[0.8, 1.25]` of the
  reference's, and a group whose members spread beyond ×1.25. Wording in
  Task 1. A group with NO known scale on any member gets no warning (nothing
  to compare).

- **R-M4b-8 Master header.** `ATH_RGEO = 'coRegistered' | 'native'` on every
  master (and drizzled master); in native mode the master's WCS is the GROUP
  reference's solve; `bin<n>` in the filename stays the group's own binning
  in both modes (M11) — the WCS says the delivered scale.

**Clarification (ruling R-T3-2, M4b Task 3).** R-M4b-4's "the run-level
`stacking_runs.reference_frame_id` stays the largest group's reference" is
the `Auto` rule. A **Manual** pin overrides it in both modes: the run-level
id IS the pin, whatever the size of the group it sits in — the results
header must show the frame the owner chose, and `plan.rs` keys its own
staleness check on it. In native mode the pin is additionally that group's
own registration reference and never moves (no two-pass re-pick for it),
while every OTHER group still auto-picks its best-weighted member and may
refine it two-pass. `SummaryGroup.reference_frame_id` is filled only for a
group that stage 5 actually registered — a group below the 3-frame
viability floor never warped anything onto its resolved reference, so it
reports `None`.

**Ruling R-T2-1/R-T6-4 (the WCS-seed trigger is a ratio, with a measured
tolerance).** Whether the plate-solve seed LEADS is decided by the frame's
implied scale ratio `r` to its reference, never by comparing the resulting
gate window against a constant: `r` is a quotient of two MEASURED scales
(`pixel_scale_arcsec` prefers the stored solve), so an exact comparison puts
every same-rig frame on the seed path. The seed leads when `|r − 1| >
WCS_SEED_RATIO_EPS = 0.05` — ≈ 3× above the within-rig jitter measured on
the owner's catalog (p50 1e-4–3e-4, tails to 1.6 %, nothing beyond 5e-2 in
1 781 solved lights) and 5× below the `SCALE_TOLERANCE` step (1.25) that
defines a foreign scale at all. `scale_gate_for` and the trigger read one
`scale_ratio_for`, so they cannot disagree about `r`.

**Ruling R-T6-9 (the plate-solve seed is also the quad seed's fallback).**
Below that 5 % ratio the M1 order runs untouched — quad seed, same pairing,
same RANSAC, same error messages — but when both frames are solved the seed
is built anyway and gets ONE turn if the quad path fails (`NoSeed`, too few
correspondences, RANSAC/refit below `MIN_INLIERS`), with the warning `quad
seed failed (<why>); plate-solve seed used` and `+wcs` on the row. A retry
that also fails leaves the quad path's verdict standing, so a frame that
fails both ways reports the message it always did; a frame with no solve on
either side keeps the M1 path with no fallback at all. The acceptance case:
an H-alpha field against an O-filter reference of the SAME rig shares too
few stars for the quad matcher — set 195 run 16 lost 14 of 30 448-mm H
frames to "only 0 inliers", each carrying 456–465 matched stars in its own
solve; run 20 on the fallback build aligned 30 of 30, the 14 through `+wcs`.

## 4. Measurement, weights, selection

Today's `frame_analysis.psf_signal` is `median(peak)/noise`, not the PSF
Signal Weight, and it measures raw frames. Stage 3 measures **calibrated**
frames and stores its own metrics on the run.

### 4.1 Metrics per calibrated frame, per channel

Star detection (noise-relative levels at `background + detectionSigma·noise`
and half that — spec §9.2 `measurement.detectionSigma`; the rank-budget
levels this replaced were blind to sky brightness and handed a sharp,
bright-sky night several times the seed population a soft one got, which
inverted the frame ranking — M4a Task 2), optionally on a 3×3 median of the
plane instead of the plane itself (`measurement.seedPrefilter`, math
reference §5.1's "hot-pixel filter radius 1" — it suppresses an undersampled
star's peak ~2.5× harder than a well-sampled one's, which no single
threshold can express; everything downstream of detection always measures
the untouched plane, and when the filter is on the detection LEVELS are
computed from the unfiltered plane's own noise and handed to the detector in
ADU — a median attenuates the noise as well as the stars, so a level derived
from the filtered copy would move with the peaks and the filter would cancel
itself. The shipped default is `none`; see the M4a Task 2 fix rounds 1-2 for
the two grids that left it off) — or, with `measurement.seedDetector =
"structure"`, from math reference §5.1's structure map instead of any peak
threshold at all (`stacking::structure`, M4c Task 0: connected groups of
pixels that survive a 33-px high-pass and a `median + 3σ` binarization, then
the reference's per-candidate rules, plus an automatic minimum structure
size derived per frame from the accepted candidates' own size distribution —
the reference's, and the one stage of it with no peak-detector equivalent;
it reproduces a structure detector's sharpness behaviour, which a peak
threshold cannot, but it ships off — see §9.2), PSF fitting with the
`psfModel` (`auto` = Moffat β ∈ {2.5, 4, 6, 10} best MAD, or `moffat4`), the
hybrid PSF/aperture flux at FWTM, RCR-cleaned and Winsorized mean fluxes,
`M*`/`N*` from the large-scale background residual (MMT residual, scale 256),
MRS noise σ_N, FWHM (weighted by fit residual), eccentricity, star count,
median, MAD, `MedianMeanDev`, classic `SNRWeight`. Formulas: math reference
§1–§2.

### 4.2 Weight modes

| Mode | Weight | Default |
| ---- | ---- | ---- |
| `psfSignalWeight` | `PSFSW = 5.326e-6·TFlux·TMeanFlux / (9.0e6·σ_N·M*)` | **yes** |
| `psfSnr` | `1.316e-7·TFlux² / (4.987e6·σ_N²)` | |
| `noise` | `(noiseScale/σ_N)²` | |
| `formula` | the classic WBPP formula with user weights A/B/C/D and pedestal P (defaults 15/15/20/0 + 50) | |
| `exposure` | `EXPTIME` | |
| `keyword` | a FITS keyword (default `SSWEIGHT`) | |
| `none` | 1 | |

Weights are per channel for RGB (the combiner runs per plane) and are
normalized by the maximum per channel inside a group. The frame-level weight
shown in the UI and used for ranking is the mean over channels.

### 4.3 Selection

In order: manual exclusions (per frame, persisted on the set config); frames
with normalized weight < `minWeightFraction` (0.05) of the group maximum; the
optional hard filters `maxFwhmPx`, `maxEccentricity`, `minStars`; registration
failures (§3.6). Every exclusion carries a reason string shown in the Frames
table. A group with < 3 included frames is skipped with a warning; a run with
no viable group fails at plan time.

### 4.4 Reference selection

`reference.mode = manual` uses `frame_set_reference` (set in the Analysis
tab, unchanged). `auto` picks the included frame with the highest frame-level
weight **inside the largest group** (most included frames; ties by total
exposure), ties inside the group broken by star count. Weights are not
compared across cameras — the PSF Signal Weight scale differs between a mono
and an OSC sensor — and the largest group is the one whose geometry the
other masters should adopt. On the owner's data this picks a frame of the
208-frame mono group, as WBPP did. Auto never writes `frame_set_reference`;
the run records `reference_frame_id` and `reference_mode`.

**Amendment (2026-09-10, ruling R-M3-17 v2, owner-driven).** The frame this
section picks — manual pin or auto best-by-weight — is the REGISTRATION
reference only: every subject frame's `PixelMap` is fitted against it, and
it names the master's copy-through header cards / WCS anchor. It no longer
anchors normalization. On LDN 1272 the OSC group's normalization reference
was `2025-09-14_02-19-02_0019`, the group's top-PSF-weight member — a
bright-sky night (G background 0.0083) — so the master inherited that
night's sky level and large-scale background shape (R-plane corners 0.85 of
centre vs. a reference tool's 0.69 when it normalized to a dark-sky
2025-10-18 frame, G ≈ 0.0028). Calibration, registration, LN and
integration were verified identical frame-for-frame; the ONLY difference
was which frame anchored normalization.

Every group now picks its own NORMALIZATION anchor — `GroupInput.reference`,
the frame §5.1's global normalization and §5.2's LN reference integration
normalize to, and the `normalization_reference_frame_id` the run summary
records — sky-penalized: per admissible included member (the same
weight-floor/star-count/coverage admissibility this section's own
best-by-weight uses, see the coverage note below), score

```
s_i = weight.normalized_mean / sqrt(background_i)
```

(`background_i` = mean over planes of the frame's measured background
median; a member whose background is non-finite or `<= 0` is not a
candidate). The anchor is `argmax s_i` (ties → the higher weight, then star
count); on a single-night set where every member shares roughly one sky
level, `s_i` is monotonic in weight and this coincides with plain
best-by-weight — the two rules only diverge on a mixed set, exactly where
Ruling 7 (superseded) got LDN 1272 wrong. Rationale: faint-signal SNR scales
as `1/sqrt(background)`, so this both ranks by weight and penalizes a
bright sky, without needing a fixed top-K or weight-fraction band (an
initial "good half by weight ≥ 0.5·max" draft still failed to reach LDN
1272's dark-sky night, since that set's own top-8-by-weight frames were ALL
from the bright night).

A candidate must also cover the reference geometry: `reference_coverage`
maps a 32×32 grid of reference-pixel cell centres through the member's own
`PixelMap::inverse` and counts the fraction landing inside its native
`[0, width) × [0, height)` extent; `>= 0.97` is required (a rotated frame, a
mismatched camera angle, or a badly offset one loses coverage at the
corners — normalizing to it, or modelling the LN reference's background on
it, would carry its uncovered corners/edges into every other frame). The
coverage filter is dropped for a group where fewer than 3 members would
pass it (a warning, not a hard failure) — ranking among too few candidates
to mean anything is worse than no filter. **Amendment (2026-09-10, fix
round 1, Important 6):** the 0.97 threshold was chosen from the estimator's
own measured values at 6224×4168 (a pure rotation about the rectangle's
centre): 1°→1.0, 2°→0.988, 3°→0.980, **4°→0.969 (the first crossing below
0.97)**, 5°→0.957; a 30-px translation dither loses far less, ≈0.995 — a
small rotation only clips the four corners' nearest grid cell(s)
(tangential-to-the-boundary near each corner, not radial), so it grows
slower than a first guess of "a couple of degrees" suggests. `0.97` sits
just past the measured 4° crossing.
Fewer than 3 admissible candidates for the anchor pick alone falls back to
plain `best_by_weight` with a warning too (fix round 1, Important 2/3): a
candidate's background is floored to 1% of the admissible candidates' own
median before the square root (a near-zero-but-positive background — an
over-subtracted master dark, not a broken frame — would otherwise dominate
the score by a landslide), and if EVERY candidate's background is still
non-finite/non-positive after that (every member's calibration is
genuinely suspect), the anchor falls back to `best_by_weight` over the
same admissible set rather than failing the group — a group with `>= 3`
members has always been guaranteed an anchor, and this ruling does not
lift that guarantee.

The LN reference's member list (§5.2) is ranked the same sky-penalized way,
not by raw weight — see the note there.

**Amendment (2026-09-11, ruling R-M4a-5, M4a Task 4) — the two-pass
reference.** Two-pass reference is Auto-only and dry-first. With
`reference.mode = auto` and `reference.twoPass = true` (default true),
stage 5 registers the reference's OWN group once WITHOUT writing
`registration_results` rows or registered artifacts (pass 1), takes the
median rotation and translation of the successful alignments, scores the
top `TWO_PASS_CANDIDATES = 10` frames by normalized weight (the group's
`best_by_weight` order — the registration reference is a geometry/quality
choice, the sky-penalized order is for normalization only, ruling R-M3-17)
by their corner displacement from the median transform
`d = sqrt((Δθ_rad · D/2)² + |Δt|²)` (`D` the reference frame's diagonal in
px), and switches the reference to the argmin when the current reference's
own `d` exceeds the best candidate's by at least
`TWO_PASS_MIN_GAIN_PX = 4.0` px; pass 2 is the existing persisting loop
over every group with the final reference. Manual references never move. A
switch updates `stacking_runs.reference_frame_id`, the summary's reference
block, and adds one run warning `reference switched by the two-pass pick:
<old> → <new> (corner displacement <d_old> → <d_new> px)`. Pass 1 is
bounded by the reference group's size (≈ 1–2 min on the acceptance set);
pass-1 star lists are NOT cached across passes (simplicity; the cost is
bounded).

Why it exists: stage 4 picks on weight alone, so on a set where the mount
was nudged (or a meridian flip left one night a fraction of a degree off),
the best-weighted frame can be the one frame whose pointing the rest of the
set does NOT share — and every master then adopts that frame's geometry and
loses its corners. The dry pass is the only way to know a frame's rotation
and offset: they are measured by registration, which stage 4 runs before.

The stale check honours a switched reference (ruling R-M4a-6):
`plan.rs::compute_register_stale` in `Auto` mode already takes the expected
reference from the LAST run's own `stacking_runs.reference_frame_id`, which
is exactly what the switch writes — so a plan after a switch does not call
Register stale and no run pays two passes twice for the same answer.
`reference.twoPass` is deliberately NOT part of the registration stage hash
(`registration_subtree` is `cfg.registration` plus the resolved
`reference_frame_id`): the toggle says HOW the reference is chosen, and a
stored row already records WHICH frame it was.

## 5. Normalization

### 5.1 Global (M1)

Per frame per channel, against the reference frame: location `m_i` = median,
scale `s_i` from the `scaleEstimator` (`bwmv` default, `mad`, `avgDev`),
two-sided and collapsed by the mean of the sides (math reference §2.4).
Both are measured on the **calibrated** (un-resampled) frame, on a 1/16
stratified pixel sample, during stage 3 — so no extra pass is needed and
every frame, the reference included, is measured the same way. The
resampling kernel changes the noise scale by a factor that is common to the
whole group (the reference is resampled through the same kernel with its
identity transform, so a smoothing kernel such as the bicubic B-spline
smooths it too); the ratios `s_ref/s_i` are therefore unaffected to first
order, and the residual second-order difference for interpolating kernels at
fractional phases is accepted. Output normalization modes: `none`, `additive`, **`additiveWithScaling`** (default,
`v′ = (v − m_i)·(s_ref/s_i) + m_ref`), `multiplicative`,
`multiplicativeWithScaling`. Rejection normalization: `none`,
**`scaleZeroOffset`** (default), `equalizeFluxes`, `local` (M2). The engine
represents both as per-frame `(offset, scale)` pairs; local normalization adds
per-frame grids.

### 5.2 Local (M2)

- **Reference** per group: integration of the best `referenceFrames` (20)
  included frames, linear-fit rejection, global normalization, resampled
  lazily, kept in RAM for the LN pass and written to
  `ln/<group>/reference.fits` as an artifact — normalized to the group's
  sky-penalized normalization anchor (§4.4 amendment, ruling R-M3-17 v2),
  not the set's registration reference. **Amendment (2026-09-10, ruling
  R-M3-17 v2):** "best `referenceFrames`" is by the SAME sky-penalized score
  `s_i = weight.normalized_mean / sqrt(background_i)` §4.4 defines for the
  anchor, not raw weight — the run ranks candidates once
  (`sky_penalized_order`, admissible by weight-floor/star-count/coverage the
  same way) and hands the reference builder exactly that top-N set; the
  reference builder itself still integrates however it always did (linear-
  fit rejection, global normalization, equal per-frame weighting) — only
  WHICH `referenceFrames` frames make up that set changed.
- **Per frame**: reference and target background models (MMT residual at
  `scale` 1024 after hot-pixel median filter radius 2, low clip 4.5e-5, high
  clip 0.85 relative, deviation thresholds 3.0σ / 3.2σ, rejection limit 0.3
  per cell); global scale `s` from matched-star PSF-flux ratios cleaned by
  RCR (limit 0.3), matched on the PSF-fit centroids and — when that
  pairing covered less than 80 % of the TARGET's own accepted fits — a
  second time on the DETECTION barycentres, the larger pairing winning
  (M4c, ruling R-M4c-9; a tie keeps the first pass, so a frame the first
  pass already handled never changes). The denominator is the target's own
  fit count, not the reference's (review finding R-T5-2): the LN reference
  is an integration of the group's best `referenceFrames` frames and is
  deeper than any single target, so measured against IT "matched under
  80 %" would be the ordinary case and the second pass would run on nearly
  every frame for nothing — the shortfall it repairs is fits that WALKED,
  which is a property of the target. `A(x,y) = s` (or a local
  scale spline when `localScale` is on: M4c, ruling R-M4c-8 — the
  residuals `z_k − s` of the pairs RCR kept, at their reference positions,
  fitted with an approximating thin-plate spline, smoothing `5·σ_z`, nodes
  grid-stratified and capped at 600, then `A(x,y) = s + spline(x,y)`
  sampled at every grid node; `s` stands alone when fewer than 40 DISTINCT
  reference stars survive RCR (counted after the reference-index dedupe a
  one-way match makes necessary), when the spline cannot be fitted, or when
  the sampled
  surface leaves a ±25 % band around `s` — each of those says so at
  `warn`, and none of them produces a partially-clamped grid),
  `B(x,y) = B_ref − A·B_tgt`, sampled on the
  `scale/8` grid (49×33 for the reference geometry) and interpolated with a
  bicubic B-spline; applied `v′ = A·v + B`. Written per frame as
  `ln/<group>/<stem>.athln` (binary: header, dims, stride, two f32 grids per
  channel, global scale/locations, relative scale factors).
- Used as output normalization and as rejection normalization when selected;
  drizzle reads the same grids at reference coordinates.

## 6. Integration

### 6.1 Engine generalization

`run_banded` gains a `FrameSource` trait:

```rust
pub trait FrameSource: Sync {
    fn frame_count(&self) -> usize;
    fn width(&self) -> usize; fn height(&self) -> usize; fn channels(&self) -> usize;
    fn bytes_per_row(&self, frame: usize) -> usize;              // for the band budget
    fn read_band(&self, plane: usize, y0: usize, rows: usize,
                 out: &mut BandPlanes, concurrency: usize,
                 on_bytes: &(dyn Fn(u64) + Sync), cancel: &AtomicBool) -> Result<()>;
}
```

- `FileSource` — today's `BandSource` (raw FITS by position), unchanged:
  1-plane, master builds keep using it and keep their fingerprint. Three-plane
  positional reads live in a small sibling, `PlaneReader` (one file, one
  plane at a time, the same `PlaneKind` decode), which is what
  `RegisteredSource`, the measurement stage and drizzle read calibrated
  frames through — no stage reads a multi-plane file banded across frames.
- `RegisteredSource` — one entry per included frame: calibrated file (f32,
  1 or 3 planes), inverse transform, kernel, clamping. `read_band` maps the
  band boundary densely through the inverse (every 32 px along the four
  edges), takes the source row span, expands by kernel radius + 1, clamps;
  if the span exceeds 60 % of the frame height it reads the whole plane. Reads
  are positional per plane; resampling fills f32 band rows, NaN outside.
  Parallelism: across frames with the storage-class read concurrency (a
  worker reads one frame's window, resamples into that frame's band slot,
  drops the window), rows within a frame by rayon. The band budget policy is
  unchanged and sees f32 sample widths.

Cost on this Mac for the 208-frame mono group: ≈ 5.4 Gpx × 16 taps ≈ 87 G
multiply-adds per pass ≈ 5–30 s of compute, against ≈ 21 GB of calibrated
reads ≈ 100–120 s at the measured disk rate — the pass is disk-bound, which
is the point of not writing registered frames.

`run_banded` further gains: per-frame `(offset, scale)` normalization (the
existing `scales` slot plus a per-frame offset), per-frame weights, an
optional per-frame LN grid, a channel loop (one pass per plane), and the
survivor masks described next.

### 6.2 Combiner v2

`combine_pixel_weighted(values: &mut [f32], weights: &[f32], recipe) →
(value, SurvivorMask)` where `SurvivorMask` is a bit set over frame index
(u64 words). Rejection runs on the (rejection-)normalized working copy and
returns the mask; the average is the weighted mean of the survivors' output-
normalized raw values (`x ≠ 0`, `w > 0`); median ignores weights. All-rejected
falls back to the median of all raw values. From the masks the engine
accumulates:

- **rejection maps** (`rejection_low`, `rejection_high`: count per pixel,
  written as float32 FITS when `writeRejectionMaps` is on),
- **per-frame rejection bitmaps** (`rej/<run>/<group>/<stem>.rej`, one bit
  per pixel per channel, LZ4-free plain bits — 3.2 MB per plane) only when
  drizzle is enabled; deleted at the end of the run unless intermediates are
  kept,
- per-frame rejected fraction (Frames table, group stats).

### 6.3 Rejection menu and the Auto rule

Existing: `none`, `percentileClip`, `sigmaClip`, `winsorizedSigma`,
`linearFitClip`, now weight- and mask-aware. Parity with the reference
semantics (math reference §3.4) is handled deliberately, because the same
functions build calibration masters and their output is fingerprint-pinned:

- `linearFitClip` adopts the reference dispersion
  `s = 2·adev` in M1 — otherwise the Auto thresholds 5.0/3.5
  would reject about twice as hard as the WBPP run they are copied from.
  The reference's slope term sqrt(1 + b²) is omitted: it is inert on [0, 1]
  input and dimensionally wrong on the ADU-scale stacks the master builder
  feeds (it silenced the rejection there). The robust minimum-absolute-deviation
  line lands in M4a (`integration/combine.rs::medfit_line`, still omitting
  the slope term for the same reason); the dispersion constant
  (`LINEAR_FIT_SIGMA_SCALE`) was calibrated by the M4a acceptance run
  (2026-09-11: 2.985 % / 2.733 % rejected at 5.0/3.5 with the constant at
  `1.0`, inside the 2.3–3.3 % target — it stays `1.0`).
  Master builds never select linear fit automatically (their Auto is
  Winsorized / percentile / median), so only a master built with an explicit
  linear-fit recipe changes; that test pin is re-measured in the same task.
- `winsorizedSigma` kept today's semantics through M1–M3 (it is what every
  master with n ≥ 15 is built with) and adopted the reference loop in M4c
  Task 2 (ruling R-M4c-3): `μ = median`, initial `σ = 1.4826·MAD`, then the
  1.5σ Winsorization with a first-pass cutoff of 5 (a sample beyond
  `μ ± 5σ` becomes `μ`, not the neighbouring threshold), `σ = 1.134·stddev`,
  `μ = mean`, to `|Δσ|/σ < 0.0005` after ≥ 2 passes and at most 20, then the
  sigma clip about `(μ_w, σ_w)` repeated until stable. Two documented
  deviations, both in the initial scale: `1.4826·MAD` instead of the
  reference's `1.1926·Sn`, which is O(n²) per pixel stack — the first-pass
  cutoff makes the start point nearly irrelevant and the loop reaches the
  same fixed point from either — and, when that MAD is exactly **0**, the
  sample standard deviation about the median instead (ruling R-T2-1). The
  MAD of any MAJORITY-TIED stack is 0, which integer-ADU calibration stacks
  routinely are: `15 × 500 ADU + one cosmic ray` would otherwise seed
  `σ = 0` and switch the rejection off on the default master recipe
  (Winsorized 3/3 for n ≥ 15). The reference's `Sn` seed degenerates on the
  same stacks; the fallback is ours, it restores the retired estimator's
  answer there, and a stack with a non-zero MAD can never reach it.
  No fixture fingerprint pin moved (the two Winsorized
  fixtures are tolerance- and survivor-set-based, and both fixed points
  reject the same planted outlier there); the real move was measured on 21
  calibrated LDN 1272 mono frames at 4.0/3.0 — rejected fraction
  0.380 % → 0.963 %, master median −0.024 %, master MAD +0.50 %, master
  noise +2.0 %, combine time +20 %.

Three more shipped in M4c Task 1 (math reference §3.4, rulings R-M4c-1/2),
all three USER choices the Auto rule below never selects:

- `minMax { low, high }` (defaults 1/1) — drop the `low` smallest and `high`
  largest samples outright; the counts are clamped so at least one sample
  always survives.
- `esd { outliersFraction, alpha, lowRelaxation }` (defaults 0.3/0.05/1.5) —
  the generalized extreme studentized deviate test: up to
  `clamp(trunc(f·n), 1, n−2)` sequential tests of the most extreme
  studentized residual against Rosner's critical value `λ_i`, with
  `lowRelaxation` inflating the scale used below the centre so the faint side
  is rejected less eagerly. `λ_i` needs a Student's t quantile, computed
  dependency-free from the regularized incomplete beta
  (`integration/student_t.rs`) and memoised per `(n, alpha)` in a
  thread-local (R-M4c-2).
- `rcr { limit }` (default 0.5, Chauvenet's criterion) — Robust Chauvenet
  Rejection: three phases of decreasing robustness, each rejecting the single
  most extreme sample while `n·Q(|x − μ|/σ)` stays below `limit`. Implemented
  a second time in `integration/combine.rs` for pixel stacks, because
  `stacking::robust`'s copy is gated behind `render + solver` and
  `integration` is not; a cross-check test in `stacking::robust` holds the
  two to the same answers.

Parameters keep the
two-axis `IntegrationRecipe` shape (combination × rejection) the master
builder uses; the three new ones are APPENDED to the persisted snake_case
`Rejection` JSON (`min_max`, `esd`, `rcr`), which never renames or reorders
what is already there.

**Large-scale (structure-aware) rejection** shipped in M4c Task 3 (math
reference §3.5, ruling R-M4c-4). The per-pixel algorithms above judge each
pixel stack alone, so a satellite trail survives in the master as the
speckle they leave of it — and its faint edges, which no per-pixel test
reaches at all, survive whole. `integration.largeScale { enabled,
protectedLayers, growth }` (defaults `false`, 2, 2) turns integration into
TWO passes:

1. Pass 1 integrates as usual and writes every included frame's per-frame
   rejection bitmap (`stacking::rej`, the M3 `.rej` format — one bit per
   pixel per plane). The M3 sink condition widens accordingly: the bitmaps
   are produced when `drizzle.enabled && drizzle.useRejection` **or**
   `integration.largeScale.enabled`.
2. Each bitmap is filtered to the structures big enough to be real and
   grown by `growth` px, then written as a `.rejl` sibling
   (`process_large_scale`): a cascade of binary median filters of windows
   3, 5, …, `2^protectedLayers + 1`, each keeping a pixel when more than
   half of its window is rejected, followed by a dilation with a disc of
   diameter `2·growth + 1`. This is OUR formulation of the reference's
   "MMT keeping only the residual erases rejected blobs smaller than
   ~2^layers px" — the cascade's combined support is the
   `2^(protectedLayers+1)+1` window the ruling names, to within a pixel,
   and it is run as the cascade rather than as one median of that whole
   window because a single wide median's majority rule only keeps a
   structure at least half the window THICK, i.e. it would erase exactly
   the thin trails this stage exists to keep. What survives, exactly: a
   band at least `2^layers / 2 + 1` px thick (3 px at the default 2, 5 px
   at 3, 9 px at 4) and a compact blob larger than the widest window.
   `protectedLayers` is a scale selector, not a strength knob.
3. Pass 2 re-integrates with those bits as FORCED rejections
   (`StackParams.forced_rejection`): a set bit is dropped before any
   algorithm runs, the algorithm decides among what is left, and the forced
   samples count as rejections in the low/high maps and the per-frame
   counts (never in `rejected_fraction`, which stays algorithm-only —
   same convention range rejection already has). Pass 2 writes its OWN
   bitmap set, a freshly created one under
   `rej/run-<id>/<group>/pass2/` (ruling R-T3-1) — never a rewrite of pass
   1's files, whose band-level "nothing to write" skip trusts `create`'s
   zero-fill and would leave pass 1's bits standing. Those bits are the
   master's real rejected set: the forced structures plus whatever pass 2's
   own tests rejected. `GroupStats.largeScaleRejectedFraction` reports what
   the processed bitmaps forced; it is `None` when the pass did not run.

`low`/`high` collapse into the single `enabled` (ruling R-M4c-4): the
bitmap is one bit per pixel and does not carry which side a rejection fell
on, so the side split is not a distinction this data can express, and a
format bump to carry it would buy a difference no acceptance test can see.
Cost: one more integration pass, plus one `.rejl` and one second-pass
`.rej` per included frame among the same per-run temporaries the first
pass's bitmaps live in (all under `rej/run-<id>`, removed at the run's exit
unless `output.cleanup = keepAll`). Drizzle, when both are on, reads the
SECOND pass's set — the same rejected set the master was built with; if
that set is missing or incomplete, drizzle is skipped for the group rather
than handed pass 1's bits, which describe the integration the second pass
replaced.

Auto (WBPP 3.0.1 as observed on the owner's data): n < 8 → percentile
0.2/0.1; 8 ≤ n < 20 → Winsorized 4.0/3.0; n ≥ 20 → linear fit 5.0/3.5 —
unchanged by M4c (ruling R-M4c-1), so no existing group's rejection moves
because the three algorithms above exist. Range
rejection: `rangeLow` 0.0 on, `rangeHigh` off (0.98 when on).
`minWeight` 0.005 as the engine floor below the UI's selection threshold.

### 6.4 Output

- Master: float32 FITS, 1 or 3 planes (`write_fits_f32`), path
  `<output>/<master name>` (§9.5). Header: copy-through cards (object,
  instrument, filter, dates, Bayer-free, `ROWORDER`) and `ATH_STKF` (below)
  come from the group's **normalization anchor** (§4.4, ruling R-M3-17 v2 —
  the sky-penalized pick, not necessarily the set's registration reference);
  the **WCS** is the **registration reference**'s plate solve rewritten by
  the new WCS card writer (CRPIX unchanged — the master is in reference
  geometry; SIP cards when the solve has them) — these two frames can
  differ, and each header field comes from whichever one it always has.
  Also `IMAGETYP = 'Master Light'`, `NCOMBINE`, `EXPTIME` = weighted total,
  `DATE-OBS`/`DATE-END` = earliest/latest,
  and the provenance cards `ATH_STK = 1`, `ATH_STKV` (format version),
  `ATH_STKN` (frames), `ATH_STKR` (recipe string), `ATH_STKW` (weight mode),
  `ATH_STKO` (normalization), `ATH_STKF` (the normalization anchor's
  identity — the registration reference is a run-level fact instead,
  `stacking_runs.reference_frame_id` / `RunSummary.reference`, not a
  per-master card), `ATH_STKG`
  (group key), `ATH_STKC` (2026-09-10 — groups are camera-agnostic: every
  distinct camera in the group, comma-joined, sorted, e.g.
  `'ATR2600M,ZWO ASI2600MC Duo'`, truncated with `…` past 68 chars),
  `ATH_STKI` (run id; FITS keywords are eight characters).
- Rejection maps: `<master stem>_rejlow.fits` / `_rejhigh.fits` (optional).
- **Scanner rule**: a file carrying `ATH_STK` or `ATH_REG` is an Athenaeum
  artifact and is never cataloged, the same one-rule skip as
  `CALSTAT + ATH_CSRC` (calibrated intermediates already carry those).
  Cataloging masters is an M4 decision.
- Stats per group (`stats_json`): frames, included, rejected low/high
  fraction, MRS noise of the master, noise of the best sub, `snrGain` =
  ratio of PSF SNR of the master to the best sub, FWHM and eccentricity of
  the master (same measurement as stage 3), per-stage durations, bytes read.

## 7. Drizzle (M3)

Inverse-mapping drizzle (math reference §6.3): output scale 1, **2** or 3;
`dropShrink` 0.9; kernels `square` (default), `circle`, `gaussian`;
`kernelGridSize` 16. For each output tile (512×512 output pixels) the tile's
source window is the inverse image of its bounds plus one pixel; source
pixels are iterated, their drops mapped forward through the frame's
transform (homography/polynomial; spline in M4) and clipped exactly against
the output pixels they touch; contributions `a·w·N(d)` with `N` the frame's
normalization (LN grid at reference coordinates when available, else
scale + zero offset) accumulate into `I` and `W`; rejected pixels (the run's
bitmaps) and zero samples are skipped; final `I /= W / dropShrink²`, weight
map kept and written as `<stem>_drizzle<s>x_weight.fits` when asked.
Parallel by tile, one `I`/`W` pair per plane. Output `<master
stem>_drizzle<s>x.fits` with the WCS scaled (`CRPIX·s`, `CD/s`) and
`ATH_DRZ`, `ATH_DRZP`, `ATH_DRZK` cards. Bayer drizzle (CFA source → RGB
planes without demosaic) is M4 and needs stage 1 to also keep the calibrated
CFA mosaic.

### Implementation notes (M3)

Rulings made while building the plan (`docs/superpowers/plans/2026-09-10-stacking-m3-plan-drizzle.md`), one line each:

- **R-M3-1 Deposition, not gathering.** The per-pixel work is forward: map the drop's four corners, clip against the output pixels the mapped quad touches — the spec's "inverse image" text above only bounds each tile's source window.
- **R-M3-2 Units and level.** `a` is measured in OUTPUT-pixel area units; `I += a·w·N(d)`, `W += a·w`; the result is `I / W` where `W > 0`, else `0` — no `s²` and no `dropShrink²` factor anywhere, so a uniform input field comes out at exactly the input level for every scale/dropShrink.
- **R-M3-3 Kernels.** `square` is exact polygon clipping (Sutherland–Hodgman, shoelace area). `circle`/`gaussian` use the `kernelGridSize² = 16×16` tabulated micro-drop table: 256 sub-drops, weights normalized to sum `dropShrink²`, each deposited as a point at the output pixel containing its mapped centre — not exact clipping.
- **R-M3-4 Rejection lookup per source pixel.** A frame's `.rej` bit is read at the rounded reference coordinate of the drop's CENTRE; a set bit skips the whole drop.
- **R-M3-5 LN lookup per source pixel.** With local normalization and a frame that has grids, `N(d) = a·d + b` read from its grid at `(round(u), round(v))`; otherwise `N(d) = os·d + oo`, the frame's global OUTPUT pair `integrate_group` hands back.
- **R-M3-6 Whole source plane in RAM, bands of 512 output rows.** One `PlaneReader::read_plane` per included frame per plane; bands processed in parallel, each band's source window the inverse image of its rectangle grown by `dropShrink/2 + 1` source px.
- **R-M3-7 Memory refusal, not swapping** (amended by R-M3-16 below). Refused BEFORE any output-geometry allocation when the estimated peak exceeds half of probed total RAM (unknown total → refuse above 4 GiB) — the master is already written, the group's `drizzle_path` stays `NULL`, a run warning, the run itself is not failed.
- **R-M3-8 `.rej` files are per-run temporaries.** Written only when `drizzle.enabled && drizzle.useRejection`; removed at the run's single exit path — success, failure or cancel — unless `output.cleanup = keepAll`; opened per write, never held open across frames.
- **R-M3-9 Re-run.** Drizzle has no cache of its own: `rerunFrom: "drizzle"` is clamped to `integrate` (`api::stacking::start_stacking`), with a `debug!`; `stale_stages` never lists `drizzle`.
- **R-M3-10 Ranges.** `scale ∈ {1, 2, 3}` (else the plan's `unsupported` blocker "drizzle scale must be 1, 2 or 3"); `dropShrink ∈ [0.5, 1.0]` (outside → clamped at run time with a `warn!` and a run warning). 1× drizzle is allowed (shift-and-add with sub-pixel drops).
- **R-M3-11 Normalize timing.** Drizzle pushes its own `StageTiming` once after the group loop, between `integrate` and `output`, only when drizzle is on and at least one group wrote a master.
- **R-M3-12 Injectable RAM total.** `DrizzleInput.ram_total_bytes: Option<u64>` — `None` probes `band_budget::total_ram_bytes()` (the run always passes `None`); tests inject a small value to exercise the refusal without a huge geometry.
- **R-M3-13 0-based CRPIX.** `PlateSolveRecord.crpix1`/`crpix2` are 0-based (the card writer adds 1), so `scale_plate_solve` computes `crpix' = s·crpix + (s − 1)/2` (the same formula as `geom::to_output`) — the 1-based card then reads `s·CRPIX − (s − 1)/2`.
- **R-M3-14 The sharpening test thresholds.** The drizzle-sharpens-a-star assertion compares `fwhm_drz(dropShrink=0.6)` against `fwhm_drz(dropShrink=1.0)`, both in OUTPUT pixels, on a σ = 0.7 px star (ratio < 0.985) and a σ = 0.45 px undersampled star (ratio < 0.93) — a naive cross-unit comparison (reference px vs output px) is trivially true and proves nothing.
- **R-M3-15 The memory formula, fix round 1** (amends R-M3-7; superseded by R-M3-16 below). `estimate_memory_bytes = (channels + 2)·out_w·out_h·4` (the output planes plus one `I`/`W` accumulator pair) `+ (write_weight_map ? channels·out_w·out_h·4 : 0)` (the weight-map planes) `+ out_w·out_h·4` (`measure_plane`'s own scaled copy) `+ width·height·4` (one full-resolution source plane) `+ (ln ? 2·width·height·4 : 0)` (two reference-geometry LN grid planes).
- **R-M3-16 The memory formula, final fix wave** (amends R-M3-15; whole-branch review found it still under-counted two live allocations). Adds `+ out_w·out_h·4` (`detect_fast_data`'s own `lum` working copy inside `measure_plane` — always made, mono `channels == 1` call, regardless of seed source) and, when `use_rejection` is set, `+ channels·height·ceil(width / 64)·8` (one frame's `RejBitmap`, reference geometry, held in RAM while `deposit_band` reads it — at most one is ever live at a time). `estimate_memory_bytes` gained a seventh parameter (`use_rejection: bool`) for the second term.

## 8. Execution model

- **Job**: `ComputeJobKind::Stacking`, one job per run, label
  `Stacking · <set name>`; `active_stacks: HashMap<run_id, StackHandle>` on
  `ServiceContext`; the master-build thread pattern (`std::thread::Builder`,
  `catch_unwind`, handle removed and the completion event emitted exactly
  once). The permit is acquired first, so a queued run waits behind an
  analysis or a master build like everything else. Registration stops being
  a queue-less side path: it runs inside the stacking job.
- **Parallelism inside a run**: per-frame stages (calibrate, measure,
  register) fan out over frames on the image pool with an admission of
  `min(cores, memory budget / per-frame working set)` frames in flight —
  `RegisteredSource`'s read concurrency rules for reads, rayon for pixels.
  Banded stages use the band budget.
- **Progress**: `stacking-progress` (§10.2), throttled 300 ms, one monotonic
  `percent` per stage; the sidebar `ComputeQueueIndicator` shows the job with
  cancel. **Cancel** is cooperative: checked per frame in fan-out stages and
  per band (and per frame inside a band read) in banded stages; a cancelled
  run keeps its finished artifacts and writes no master.
- **Checkpointing**: every artifact is keyed by a config hash of the stage's
  inputs (§9.3). A re-run reuses valid artifacts; "Re-run from" lists the
  stages whose cached outputs would be reused above it. "Change rejection and
  re-integrate" costs one integration pass.
- **Cleanup policy** (`output.cleanup`): `keepAll` (default), `deleteRegistered`
  (registered frames and rejection bitmaps), `deleteIntermediates` (also
  calibrated frames and LN data). The Results panel shows the working
  folder's size per set with a "Delete intermediates" action.
- **Logging**: `info!` at run start (`run_id`, `set_id`, `count`, `groups`,
  `config_hash`) and end (`duration_ms`, `outcome`, per-stage `duration_ms`
  in fields `stage`), `debug!` per frame in registration/measurement
  (`frame_id`, `rms_px`, `inliers`, `weight`), `warn!` for every exclusion
  and every non-fatal fallback. New field names (`run_id`, `group_key`,
  `stage`, `inliers`, `rms_px`, `weight`) go into the logging spec's
  dictionary in the same change.

## 9. Data model, configuration, paths

### 9.1 Tables (all `CREATE TABLE IF NOT EXISTS`, indexes on the FKs)

```
stacking_runs(id PK, frames_set_id FK→frames_set ON DELETE CASCADE,
  status TEXT NOT NULL,          -- planning|running|done|failed|cancelled
  started_at TEXT NOT NULL, finished_at TEXT,
  config_json TEXT NOT NULL, config_hash TEXT NOT NULL,
  reference_frame_id INTEGER, reference_mode TEXT NOT NULL,   -- auto|manual
  working_dir TEXT NOT NULL, output_dir TEXT NOT NULL,
  summary_json TEXT, error TEXT)

stacking_run_groups(id PK, run_id FK ON DELETE CASCADE, group_key TEXT NOT NULL,
  instrume TEXT, color_mode TEXT NOT NULL, filter TEXT, binning INTEGER,
  width INTEGER, height INTEGER, exposure REAL,
  frame_count INTEGER NOT NULL, included_count INTEGER NOT NULL,
  master_path TEXT, drizzle_path TEXT, rejection_low_path TEXT, rejection_high_path TEXT,
  stats_json TEXT, status TEXT NOT NULL, error TEXT,
  UNIQUE(run_id, group_key))

stacking_run_frames(id PK, run_id FK ON DELETE CASCADE, group_id FK ON DELETE CASCADE,
  frame_id FK→frames ON DELETE CASCADE,
  included INTEGER NOT NULL, exclusion_reason TEXT,
  weight REAL, weight_channels_json TEXT, metrics_json TEXT,
  reg_status TEXT, reg_model TEXT, reg_rms_px REAL, reg_inliers INTEGER,
  reg_inlier_ratio REAL, reg_flipped INTEGER, rejected_fraction REAL,
  UNIQUE(run_id, frame_id))

stacking_artifacts(id PK, frames_set_id FK ON DELETE CASCADE,
  frame_id INTEGER FK→frames ON DELETE CASCADE,   -- NULL for group-level artifacts (ln_reference)
  group_key TEXT NOT NULL, kind TEXT NOT NULL,     -- calibrated|registered|ln|ln_reference|metrics
  path TEXT, config_hash TEXT NOT NULL, size INTEGER, modified_at TEXT,
  payload_json TEXT,                               -- metrics rows keep their values here
  created_at TEXT NOT NULL)
CREATE UNIQUE INDEX stacking_artifacts_key
  ON stacking_artifacts(frames_set_id, group_key, kind, COALESCE(frame_id, 0))
  -- an expression index, because a plain UNIQUE treats NULL frame_ids as distinct

stacking_set_config(frames_set_id PK FK ON DELETE CASCADE,
  config_json TEXT NOT NULL, excluded_frame_ids_json TEXT NOT NULL DEFAULT '[]',
  updated_at TEXT NOT NULL)
```

`registration_results` gains, via the guarded `ALTER TABLE` pattern:
`model TEXT`, `transform_json TEXT`, `inlier_ratio REAL`,
`peak_error_px REAL`, `scale REAL`, `rotation_deg REAL`,
`flipped INTEGER NOT NULL DEFAULT 0`, `config_hash TEXT`,
`source_kind TEXT` (`calibrated`). The `affine_*` columns keep the linear
part. Rows from the retired flow are simply overwritten by the
`UNIQUE(frames_set_id, frame_id)` upsert.

`transform_json` is `PixelMap::to_json()` verbatim — `{ "linear": { "kind":
"homography", "m": [[..],[..],[..]] }, "linearInv": { … }, "distortion": null |
{ "order": 3, "center": [cx, cy], "scale": s, "domain": [u0, v0, u1, v1],
"forward": { "order": 3, "ax": [..], "ay": [..] }, "inverse": { … } } }`. The
polynomial acts on coordinates normalized as `u = (x − cx)/s`, `v = (y − cy)/s`
(reference centre and half the longer side) for conditioning. `domain` is the
normalized box the polynomials were fitted over (the inliers' bounding box,
each side inflated by 10 %); evaluation clamps `(u, v)` into it so a far
corner gets the nearest fitted edge's displacement, never a polynomial
extrapolation (absent = unbounded, for rows written before the field).
`linearInv` is recomputed from `linear` on load, so a stored inverse can
never disagree with its forward matrix.
`stacking_runs.summary_json` and the per-run `runs/run-<id>.json` file (same
content: config, reference, groups, per-frame rows, stats) are the
provenance, modelled on `master_provenance`.

### 9.2 Configuration

One `StackingConfig` JSON (camelCase, `version: 1`, every field optional on
the wire with the defaults below; the same struct is exported to TS):

```
grouping:      { exposureToleranceSec: 2.0 }   -- exposure ALWAYS splits a group now (2026-09-10);
                                                -- the old `splitByExposure` toggle is gone — a
                                                -- stored document that still carries it decodes
                                                -- fine, the field is just silently ignored
calibration:   CalibratedLightOptions (the export's: flat norm, hot pixels on, debayer on)
measurement:   { weightMode: "psfSignalWeight", psfModel: "auto", maxStars: 24576,
                 detectionSigma: 20.0, seedPrefilter: "none", seedDetector: "peak",
                 formula: { fwhm: 15, eccentricity: 15, snr: 20, stars: 0, pedestal: 50 },
                 keyword: "SSWEIGHT" }
                -- detectionSigma: star-detection threshold for the quality measurement in σ
                -- above the local background (noise-relative). The measurement detector's two
                -- ladder levels are `background + k·noise` and `background + (k/2)·noise`
                -- (M4a Task 2, ruling R-M4a-1) instead of the rank budget that preceded it.
                -- The number is high because it is compared against a star's PEAK pixel in
                -- units of the per-pixel noise, not against an aggregated structure response:
                -- 20 is where our population and, more importantly, our frame RANKING match
                -- the external reference's own on 368 real frames. Changing it changes the
                -- measurement stage hash, so every cached measure artifact is recomputed
                -- (R-M4a-9).
                -- seedPrefilter: "none" | "median3" — the image seed DETECTION runs on
                -- (math reference §5.1). With "median3" the levels come from the
                -- UNFILTERED plane's noise, in ADU (ruling R-M4a-14) — otherwise the
                -- filter cancels itself. It measurably separates sharp from soft
                -- frames the way the reference does (the OSC red channel's
                -- bright/dark fits-ratio spread 2.60 -> 1.95, the mono weight-rank
                -- correlation 0.919 -> 0.97), but it also cuts the mono fit count to
                -- 0.5-0.8 of the reference's and leaves the OSC blue channel's weight
                -- correlation at ~0.4, so it ships off (M4a Task 2 fix rounds 1-2).
                -- It rides the same stage hash.
                -- seedDetector: "peak" | "structure" — WHICH detector finds the seeds
                -- (math reference §5.1, M4c Task 0, ruling R-M4c-11). "peak" is the
                -- threshold detector the two settings above steer; "structure" builds
                -- the reference's structure map (median, 33-px high-pass, dilate,
                -- binarize at `median + 3σ` of the UNFILTERED plane's noise, erode,
                -- connected components, per-candidate rules) and ignores both of them.
                -- On the 368-frame acceptance set the structure map is better on mono
                -- (per-night fit ratios 1.02/0.81/1.00 vs 1.01/1.05/0.82, PSFSW ρ 0.977
                -- vs 0.918) and halves the OSC bright-night excess (2.6-3.9× the
                -- reference's fits vs 3.8-6.9×), but it leaves the OSC blue channel's
                -- weight correlation at 0.42 and the OSC top-20 overlap at 11/20, so it
                -- ships as an option and "peak" stays the default (12 of 22 R-M4a-2
                -- targets vs 10 — the ruling's bar is all 22). It rides the same stage
                -- hash.
selection:     { minWeightFraction: 0.05, maxFwhmPx: null, maxEccentricity: null,
                 minStars: null, excludeOnRegistrationFailure: true }
reference:     { mode: "auto", twoPass: true }
                -- twoPass (M4a Task 4, ruling R-M4a-5): re-pick the reference
                -- among the top-weighted frames closest to the reference group's
                -- median transform, after a dry first registration pass over that
                -- group (§4.4). Auto only — a manual pin never moves. It is NOT
                -- part of any stage hash, so flipping it never invalidates a
                -- cached artifact.
registration:  { geometry: "coRegistered",
                 model: "auto", distortion: "off",
                 tpsSmoothing: 0.0, localDistortion: false,
                 interpolation: "bicubicBSpline",
                 clampingThreshold: 0.30, maxStars: 2000, ransacTolerancePx: 1.9,
                 ransacMaxIterations: 2000, maxRmsPx: 2.0, failOnMaxRms: false,
                 detection: { minSnr: 10, maxEccentricity: 0.8 },
                 writeRegisteredFrames: false }
                -- distortion (M4c, ruling R-M4c-5): "tps" joins the polynomial
                -- orders and "auto" (§3.3). `auto` never resolves to it.
                -- tpsSmoothing (M4c, ruling R-M4c-5): the spline's λ, in px² of
                -- the normalized frame. 0.0 = interpolating, which lands every
                -- inlier exactly (and makes the reported RMS a hold-out
                -- measurement — §3.3). Read only by distortion: "tps". Clamped
                -- to [0, 10] by resolve_config. The useful range grows with the
                -- node count (≈ 0.01 for a few dozen nodes, roughly 10× that at
                -- the 600-node cap); Task 7's acceptance run picks the shipped
                -- default from real frames.
                -- localDistortion (M4c, ruling R-M4c-7): the local distortion
                -- loop (§3.3). A no-op with distortion: "off" — there is nothing
                -- to refit. Both fields ride `registration_subtree`, so either
                -- one re-registers the set, exactly as `geometry` does.
                -- geometry (M4b, ruling R-M4b-4): "coRegistered" | "native" (§3.8).
                -- Co-registered is M1-M4a's behaviour: ONE reference for the set,
                -- every group resampled into its geometry. Native gives each group
                -- its own reference (its best-weighted member, two-pass re-picked
                -- per group) and its own geometry for LN, integration, drizzle, the
                -- master's WCS and the rejection bitmaps. It rides
                -- `registration_subtree`, so flipping it re-registers every set —
                -- deliberately: a stored row records WHICH reference a frame was
                -- warped onto.
normalization: { output: "additiveWithScaling", rejection: "scaleZeroOffset",
                 scaleEstimator: "bwmv",
                 local: { enabled: false, scale: 1024, referenceFrames: 20,
                          psfModel: "auto", localScale: false } }
                -- localScale went live in M4c (ruling R-M4c-8, §5.2): the local
                -- SCALE spline on top of local BACKGROUND normalization. It
                -- stays OFF by default and only means anything while
                -- `local.enabled` is on; flipping it moves the normalization
                -- stage hash, so every cached `.athln` sidecar is recomputed.
integration:   { combination: "average", rejection: { method: "auto" },
                 minWeight: 0.005, rangeLow: 0.0, rangeHigh: null,
                 writeRejectionMaps: false,
                 largeScale: { enabled: false, protectedLayers: 2, growth: 2 } }
                -- largeScale (M4c Task 3, ruling R-M4c-4, §6.2): structure-aware
                -- rejection. ONE `enabled`, not the low/high pair the ruling started
                -- from — the rejection bitmap is one bit per pixel and cannot carry
                -- the side. `protectedLayers` 1-6 is a scale selector (a band
                -- survives from ~2^layers / 2 px thick), `growth` 0-4 the disc radius
                -- every survivor is grown by. On, integration runs TWICE.
drizzle:       { enabled: false, scale: 2, dropShrink: 0.9, kernel: "square",
                 useRejection: true, useWeights: true, useLocalNormalization: true,
                 writeWeightMap: false }
output:        { format: "fits", cleanup: "keepAll" }
paths:         { workingDir: null, outputDir: null }     -- null = the global default
```

Precedence: set config (`stacking_set_config`) > global defaults (settings key
`stacking.defaults`, JSON) > built-in defaults. Presets are built-in
transforms of the config: **Default** (above), **Fast preview** (bilinear,
sigma clip 4.0/3.0, LN off, drizzle off, `twoPass` off — ruling R-M4a-18: a
preset whose promise is "fast" does not pay for the dry registration pass,
§4.4 — `deleteIntermediates`), **Maximum
quality** (bicubic B-spline, polynomial-3 distortion, LN on, drizzle 2×,
rejection maps written). Editing any field makes the preset **Custom**. Settings → Stacking edits the
global defaults with the same inspector forms and holds the default folders
(`stacking.working_dir`, `stacking.output_dir`).

### 9.3 Artifacts and config hashes

Per stage, `config_hash = xxh3(canonical JSON of the stage's config
subtree + the upstream hashes it depends on + the source file identity
(`files.id`, size, `modified_at`))`. Stage 1 depends on the resolved master
paths and their identity; stage 3 on stage 1; stage 5 on stage 1 + the
reference id; LN on stage 5 + the reference set. An artifact is reused only
when its row's hash matches and the file exists with the recorded size. A
stale artifact is overwritten in place; orphan files in the working folder
are reported by the cleanup action, never deleted silently.

### 9.4 Paths

- Global defaults: `stacking.working_dir`, `stacking.output_dir` (empty =
  unset; the tab blocks the Run button with "Choose a working folder" until
  one exists). Per-set overrides in `paths`.
- Validation = `validate_transfer_dir`'s gate reused: absolute,
  `PathPolicy::check`, create-if-missing + write probe, the two folders may
  not be equal, the working folder may not sit inside the output folder.
  Overlap with a scan root is **allowed with a warning** — every artifact
  and every master carries a scanner-skip card.
- Web build: the folder browser gets a `stacking` scope backed by
  `browse_directories`; the same validation runs server-side.
- Free-space estimate before a run: calibrated `Σ frames × planes × W×H×4`,
  registered (if on) the same, LN reference per group, masters, drizzle
  `× s²`; compared with `statvfs` of the working folder's volume (the
  `diskspace` probe).

### 9.5 Working folder layout and names

```
<working>/<set slug>/
  calibrated/<group key>/c_<stem>.fits
  registered/<group key>/r_<stem>.fits          (optional)
  ln/<group key>/reference.fits, <stem>.athln    (M2)
  rej/run-<id>/<group key>/<stem>.rej            (drizzle runs only, temporary)
  runs/run-<id>.json
<output>/
  <set slug>_<filter>_<mono|osc>[_bin<n>]_<exp>s_<n>x.fits (2026-09-10, fix round 1 ruling: no
                                                    camera token — a group can mix cameras — but
                                                    the colour-mode token stays ALWAYS present, so
                                                    a mono and an OSC group of the same filter and
                                                    exposure never collide down to a bare `_2`;
                                                    <exp> is the group's own exposure-cluster
                                                    label, or `unknown`. M2 final fix wave, ruling
                                                    M11: `_bin<n>` appears ONLY when the group's
                                                    binning is >= 2 — bin-1 names are unchanged,
                                                    so a bin-1/bin-2 pair of the same filter/
                                                    colour-mode/exposure never collides either)
  …_drizzle<s>x.fits, …_rejlow.fits, …_rejhigh.fits, …_drizzle<s>x_weight.fits
```

When two frames of a group share a source file name, each of them gets
`_f<frame id>` before the extension (`c_<stem>_f<id>.fits`), so a capture
counter that restarts on another night can never overwrite a sibling.

Slugs use the calibration library's sanitizer. A name collision in the output
folder gets `_2`, `_3`… (never overwrite; the run's rows point at the file
actually written).

## 10. Commands, events, types

### 10.1 Commands (Tauri `commands/stacking.rs` ↔ Axum `routes/stacking.rs`, logic in `api/stacking.rs`)

| Command | Args → result |
| ---- | ---- |
| `get_stacking_plan` | `{ setId, config? }` → `StackingPlan { groups[], blockers[], reference, frameCount, includedCount, estimateBytes, freeBytes, staleStages[] }` |
| `start_stacking` | `{ setId, config, rerunFrom? }` → `{ runId, jobId }` |
| `cancel_stacking` | `{ runId }` |
| `get_stacking_runs` | `{ setId, limit? }` → `StackingRunSummary[]` |
| `get_stacking_run` | `{ runId }` → `StackingRunDetail { run, groups[], frames[] }` |
| `get_stacking_config` / `set_stacking_config` | `{ setId }` → `StackingConfig` + excluded frame ids / `{ setId, config, excludedFrameIds }` |
| `get_stacking_defaults` / `set_stacking_defaults` / `reset_stacking_defaults` | global `StackingConfig` |
| `get_stacking_paths` / `set_stacking_paths` | `{ working: PathSetting, output: PathSetting }` / `{ working?, output? }` (`null` = reset) |
| `get_stacking_work_usage` / `cleanup_stacking_work` | `{ setId }` → bytes per artifact kind / `{ setId, what: "registered" | "intermediates" | "all" }` |
| `get_stacking_presets` (plan 5b Task 1, the 15th command) | `{}` → `StackingPresets { default, fastPreview, maximumQuality }` — pure, no ctx; the one Rust source of truth the preset selector diffs the current config against, so the tab never re-implements the transforms |

**Retirement DONE (plan 5b Task 6, 2026-09-09):** `register_frame_set`,
`cancel_frame_set_registration`, `get_frame_set_registration` are retired —
removed from both backends (`commands/registration.rs` /
`routes/registration.rs`, `generate_handler!` / `build_router`), from
`ts_export.rs` (`StackingPrepProgressEvent`/`StackingPrepCompleteEvent`, and
by the same "no command returns it any more" logic, `RegistrationRecord` —
its Rust type stays, reached directly from `registration::db`), and from the
frontend (`StackingPrepTab`, `useRegistrationProgress`,
`RegistrationProgressContext`, `RegistrationQueueIndicator`, the
`registration` tab entry in `FrameSetDetail.tsx`). `registration::service`
and `registration::reference` (orphaned once `register_frame_set` was gone)
are deleted; `registration::db`, `registration_results`, and
`set_frame_set_reference`/`get_frame_set_reference` stay — the stacking run
writes/reads the table, the Analysis tab's "Set as reference" star still
calls the two kept commands. **No `StackingQueueIndicator` was ever added**
(ruling 2, §11.2 below): the sidebar's `ComputeQueueIndicator` already lists
a running stack (label `"Stacking · <set name>"`) with cancel, so a second
widget for the same job was never built — the retired
`RegistrationQueueIndicator` has no stacking-side replacement.
Every command wears `#[tracing::instrument(skip_all, err)]`; new model types
go into `ts_export.rs`.

### 10.2 Events

```
stacking-progress { runId, setId, stage, groupKey: string|null, current, total,
                    percent, bytesDone, bytesTotal, frameId: number|null, message: string|null }
stacking-complete { runId, setId, success, cancelled, error: string|null,
                    warnings: string[], masters: [{ groupKey, path, drizzlePath: string|null }] }
```

`stage` ∈ `masters | calibrate | measure | reference | register | normalize |
integrate | drizzle | output` (`masters` since 2026-09-09, stage 0.5). Web
mirrors via `SseProgressEmitter`. The frontend
listens with the cancelled-flag pattern and notifies once per run
(`kind: 'stacking'`, `dedupeKey: 'stack-<runId>'`).

### 10.3 Feature gating

`athenaeum_core::stacking` is `#[cfg(all(feature = "render", feature =
"solver"))]` like `registration`; the Tauri commands and Axum routes follow
the same cfg pattern the registration commands use today, so the headless
check keeps passing.

## 11. The Stacking tab (layout A)

Replaces the Registration tab in `FrameSetDetail.tsx` (tab key `stacking`,
label **Stacking**, icon `SquareStack` — `Layers` is taken by Export;
`?tab=stacking` deep link).
Gated only on "the set has lights"; blockers are shown inside the tab, not by
greying the tab. The Analysis tab's "Set as reference" star stays and is the
manual reference path.

### 11.1 Structure

```
┌ toolbar ───────────────────────────────────────────────────────────────────┐
│ Preset ▾ · 📁 Working … · 📁 Output … · Free 1.9 TB · estimate 84 GB       │
│                                   ▶ Run stacking   Cancel   Re-run from ▾  │
├ Pipeline board (62 %) ───────────────────┬ Inspector (38 %) ───────────────┤
│ ● 1 · Calibrate      summary     Ready ▸ │ 5 · Register                    │
│ ● 2 · Debayer        summary     Ready ▸ │ Model            [Homography ▾] │
│ ● 3 · Measure & select …         Ready ▸ │ Distortion       [Off ▾]        │
│ ● 4 · Reference      …           Ready ▸ │ Interpolation    [Bicubic B-… ▾]│
│ ◉ 5 · Register  ▓▓▓▓▓▓░░ 142/208 Running │ Clamping 0.30    Max stars 2000 │
│ ○ 6 · Local normalization [on]   Queued  │ RANSAC 1.9 px    Max RMS 2.0 px │
│ ○ 7 · Integrate      …           Queued  │ ☐ Exclude frame on failure      │
│ ○ 8 · Drizzle [off]  …           Off     │ ☐ Write registered frames       │
│ ○ 9 · Output         …           Queued  │ ▸ Advanced                      │
│ Groups (2): key · camera · colour · …    │ Defaults = WBPP-equivalent      │
├ ▾ Frames (368) ──────────────────────────┴─────────────────────────────────┤
│ filename · group · weight · FWHM · ecc · stars · reg RMS · inliers · status · ☑ │
├ Results (run #12, 2026-09-08 14:07) ───────────────────────────────────────┤
│ [thumb] master name · 208 frames · 2.4 % rejected · noise · SNR gain · Reveal │
└────────────────────────────────────────────────────────────────────────────┘
```

Below 1200 px the inspector drops under the board as an accordion.

### 11.2 Components (`src/components/stacking/`)

- `StackingTab.tsx` — owns the plan fetch (`get_stacking_plan` on mount, on
  config change debounced 300 ms, and on the `library-updated` DOM event),
  the config state (`get/set_stacking_config`, submit state, never a
  re-read), the run state from `useStackingRuns`, and the layout.
- `PipelineBoard.tsx` + `StageRow.tsx` — nine rows; row state ∈ `ready |
  blocked | stale | queued | running | done | skipped | failed`; a running
  row shows the accent bar with `current / total · percent`; optional stages
  (LN, drizzle, registered frames) carry a toggle on the row; the summary
  line is a pure function of the config (`stageSummary(stage, config)`), so
  it never disagrees with the inspector; a blocked row shows the blocker
  inline with the `→ Coverage` link, like the export mode card.
- `StageInspector.tsx` — switch over the selected stage → one panel each:
  `CalibratePanel` (read-only resolved masters per group + the export's
  light-cal options), `DebayerPanel` (info), `MeasurePanel` (weight mode,
  formula sliders when `formula`, PSF model, max stars, the four filters,
  "Re-measure" which invalidates stage 3 artifacts), `ReferencePanel`
  (auto/manual, the chosen frame with its weight, "Choose in Analysis"),
  `RegisterPanel` (as drawn), `NormalizePanel` (output and rejection
  normalization, scale estimator; the LN block with enabled/scale/reference
  frames/PSF model/local scale), `IntegratePanel` (combination, rejection
  method with `ParamPair` inputs and the Auto note stating the resolved
  algorithm per group, min weight, range clipping, rejection maps),
  `DrizzlePanel` (enabled, scale, drop shrink, kernel, the three "use"
  toggles, weight map, disk/time estimate), `OutputPanel` (two `FolderCard`s
  with per-set override vs default, cleanup policy, format). Every numeric
  field uses the two-state numeric discipline from the export tab; every help
  line states the default.
- `GroupsTable.tsx`, `FramesTable.tsx` (sortable; the include checkbox writes
  the manual exclusion list through `set_stacking_config`; status chips
  reuse `getSeverityColor`), `ResultsPanel.tsx` (runs dropdown from
  `get_stacking_runs`, master cards with a static glyph in place of a
  thumbnail until M4 catalogs masters and the existing preview route can
  render them, a stats line, Reveal / Open / Provenance (the run JSON in a
  modal), working-folder usage with "Delete intermediates").
- `stackingPrefs.ts` — only UI conveniences (collapsed panels, selected
  stage) in `localStorage`; the config itself is server-side.
- Hook `src/hooks/useStackingRuns.ts` + `StackingContext.tsx` — modelled on
  `useMasterBuilds` (backend owns admission, events carry `runId`/`setId`,
  one completion per start, `notify()` on completion, `library-updated`
  dispatch); `ComputeJobKind` gains `stacking` on both sides. **No
  `StackingQueueIndicator.tsx`** (plan 5b ruling 2, superseding this
  section's original text): the run rides the shared `ComputeQueue`
  (`ComputeJobKind::Stacking`, label `"Stacking · <set name>"`) that the
  sidebar's existing `ComputeQueueIndicator` already lists with a cancel
  button — a second widget for the same job would duplicate it. The retired
  `RegistrationQueueIndicator` is not replaced.
- Settings → **Stacking** section: the same inspector panels bound to the
  global defaults, the two default `FolderCard`s, "Reset to built-in
  defaults".
- Notifications: `NotificationKind` gains `stacking` (+ icon); the retired
  `registration` kind stays for stored history.

### 11.3 Removed

**DONE (plan 5b Task 6, 2026-09-09):** `StackingPrepTab.tsx`,
`useRegistrationProgress.ts`, `RegistrationProgressContext.tsx`,
`RegistrationQueueIndicator.tsx`, the `REGISTRATION_ENABLED` flag and the
`registration` tab entry in `FrameSetDetail.tsx` are gone. `STACKING_ENABLED
= import.meta.env.DEV` is also gone — enabled for every build since the
2026-09-10 acceptance run
(`docs/superpowers/research/2026-09-09-m1-acceptance-run.md`).

## 12. Web / Docker

Same commands over HTTP, progress over SSE, folder picking through the
`stacking` browser scope, paths checked by `PathPolicy` (`ATHENAEUM_ALLOWED_PATHS`).
Memory: the band budget already reads the cgroup limit; the fan-out admission
in §8 uses the same `total_ram_bytes()`. The image pool is shared with
analysis and blink as today.

## 13. Testing and acceptance

**Unit (every milestone):** kernels (each kernel reproduces a known analytic
sample; clamping rules on a step edge), transforms (each model round-trips
synthetic points with noise and 30 % outliers; RANSAC deterministic; inverse
polynomial error < 0.01 px over the frame), resampler (synthetic Gaussian
stars shifted by known fractions and rotated: recovered centroid error
< 0.02 px, flux within 0.5 %, NaN coverage exact), KD-tree vs brute force,
BWMV and MRS against tabulated values, weighted rejection with masks
(existing recipes byte-identical when all weights are 1 and masks unused —
the master-build fingerprint), rejection maps, PSF Signal Weight on the
synthetic calibration field (median ≈ 1), WCS card writer round-trip through
the parser, config hash stability, artifact reuse and invalidation, cancel
at every stage, headless build.

**Real data (LDN 1272, both groups):**

| Metric | Target |
| ---- | ---- |
| Registration RMS per frame | ≤ WBPP's `delta_RMS` for the same frame (0.28–0.72 px observed) |
| Frames registered | 208/208 and 160/160 |
| Master MRS noise | within 5 % of the WBPP master (same frames, same weights mode) |
| Rejected fraction | 1–4 % (WBPP: 1.4–3.8 %) |
| Artifacts | no residual satellite/plane trail visible at 400 % where WBPP shows none |
| Drizzle 2× (M3) | FWHM ratio drizzled/undrizzled within ±5 % of WBPP's |
| Wall time, whole set, this Mac | ≤ 60 min (WBPP 2 h 37 min) |
| Disk | ≤ 85 GB working folder with registered frames off (WBPP 170 GB) |

**Performance breakdown targets (this Mac, 208 mono + 160 OSC):** calibrate
≤ 8 min (already measured by the export), measure ≤ 5 min, register ≤ 5 min,
integrate ≤ 8 min per group, LN ≤ 15 min per group, drizzle ≤ 15 min per
group.

## 14. Milestones and task list

### M1 — first master light

1. **`FrameSource` trait + 3-plane reads** — `integration/source.rs`,
   `banded.rs` plane offsets; master builds unchanged (fingerprint test).
2. **Resampler** — `resample/{kernels,warp,window}.rs`: 7 kernels, clamping,
   inverse-mapped gather with NaN coverage, band-window computation, tests.
3. **Transform models** — `registration/{models,ransac,kdtree}.rs`:
   similarity/affine/homography/polynomial, forward + inverse, DLT, RANSAC
   with the quality score, σ-weighted refit; `Affine::inverse` upstream.
4. **Registration v2 service** — detection on calibrated frames, reference
   handling, per-frame QA, `registration_results` extension, optional
   registered-frame writer with `ATH_REG` cards; retire the old commands.
5. **Measurement** — `stacking/measure.rs`: PSF flux/FWTM aperture, RCR +
   Winsorization, `M*`/`N*`, MRS wrapper over rustafits, PSF Signal Weight,
   PSF SNR, classic formula, weight modes, selection filters, metrics
   artifacts.
6. **Statistics** — `integration/stats.rs`: BWMV, two-sided scales,
   global normalization pairs.
7. **Combiner v2 + engine** — weights, offsets, survivor masks, rejection
   maps, per-frame rejected fraction, channel loop, `RegisteredSource`.
8. **Headers** — `fits_writer/wcs.rs` (linear + SIP cards from a
   `WcsSolution`/`plate_solves` row), master card set, scanner skip rule.
9. **Run orchestration** — `stacking/{config,groups,plan,run,paths,naming,
   provenance}.rs`, artifacts + hashes, cleanup, events, cancel, logging.
10. **Data model** — the five tables, the `registration_results` columns,
    settings keys, `PathSetting` reuse.
11. **Commands** — `api/stacking.rs`, Tauri + Axum wrappers, `ts_export`,
    route tests, headless gating.
12. **Frontend** — the tab, board, inspector panels for stages 1–5, 7, 9
    (6 and 8 render with their toggles disabled and a "coming in M2/M3"
    note), tables, results, hook/context, queue indicator, notifications,
    Settings → Stacking, removal of the old tab.
13. **Acceptance run** — LDN 1272 both groups against the WBPP masters; the
    metrics table above filled in `docs/superpowers/research/` and the
    open-items ledger updated.
14. **Docs** — `CLAUDE.md` module map + a "Stacking" section, the logging
    dictionary additions, release-note lines.

### M2 — local normalization

Reference build per group, MMT background models, PSF-flux scale with RCR,
`.athln` sidecars, engine hook (output + rejection normalization),
`NormalizePanel` LN block live, acceptance re-run (noise target re-measured
with LN on both sides).

**Executed 2026-09-10**, plan
`docs/superpowers/plans/2026-09-10-stacking-m2-plan-local-normalization.md`
(Tasks 1–7: `LnGrid`/the bicubic B-spline evaluator/`.athln` sidecar, the
background model, PSF-flux relative scale with RCR, the per-group reference,
the stage-6 driver + artifact caching, the engine hook, and the wiring that
reads cached sidecars back into `GroupInput.ln` + `examples/ln_probe.rs`).
Tasks 8 (`NormalizePanel` LN block, results/frames-table surfacing) and 9
(the LDN 1272 acceptance re-run) are tracked separately in the same plan.
Two M1-era guards are DELIBERATELY still in place after Task 7 — `build_plan`
refuses any plan with `normalization.local.enabled = true` (Gate 6,
`plan.rs`), and `integrate_group` refuses `normalization.rejection ==
"local"` — so `start_stacking` does not yet run an LN-enabled plan
end-to-end; lifting both is scoped to whichever task next needs a real run
(Task 8's UI or Task 9's acceptance run).

### M3 — drizzle

Kernels and exact clipping, forward mapping, tile scheduler, rejection
bitmaps from M1 turned on, weights and LN application, weight map, scaled
WCS, `DrizzlePanel` live, acceptance (FWHM ratio vs WBPP drizzle).

### M4 — polish

Split into four plans, each with its own acceptance re-run:

- **M4a — quality** (`docs/superpowers/plans/2026-09-10-stacking-m4a-plan-quality.md`):
  the OSC PSF Signal Weight ranking that inverts bright-sky/dark-sky night
  order, the linear-fit rejection dispersion constant, a two-pass
  registration-reference pick, the XISF reader fix, two local-normalization
  hot spots, and the LDN 1272 re-acceptance.
- **M4b — mixed pixel scales** (`docs/superpowers/plans/2026-09-10-stacking-m4b-plan-mixed-pixel-scales.md`):
  the plan-gate scale-spread warning, a per-frame registration scale gate,
  WCS-seeded alignment across scales, and the co-registered / native modes
  (§15 — owner requirement). Tasks 1–5 (per-frame scale + plan-time
  warning, per-frame gate + WCS seed, native mode, cross-scale pins, docs
  and the frames-table `WCS` chip) landed 2026-09-11; the acceptance run on
  real mixed-scale sets is Task 6. **Acceptance run 2026-09-11** (`docs/superpowers/research/2026-09-11-m4b-acceptance-run.md`):
  sets 166 (×7.0 / ×4.8), 108 (×2 binning) and 195 (×1.286 inside one group) at 30 best frames per
  group, both modes — cross-scale groups register through the plate-solve seed at the expected scale
  (rms ≤ 0.64 px), native mode keeps each group's own reference and geometry, the reference's own
  master is bit-identical between the modes; rulings R-T6-4 (seed trigger 5 %), R-T6-6/7 (the plan
  gate over the included frames) and R-T6-9 (the plate-solve seed as the quad seed's fallback) landed
  from it.
- **M4c — rejection and registration algorithms** (`docs/superpowers/plans/2026-09-10-stacking-m4c-plan-algorithms.md`,
  incl. Task 0, the structure-map seed detector): ESD, RCR, min/max and
  large-scale rejection, the Winsorized-sigma reference re-pin, thin-plate-spline
  distortion with the local distortion loop, and LN's local-scale spline.
  Tasks 0–5 landed 2026-09-12 — the structure-map seed detector
  (`measurement.seedDetector`, default `peak`; rulings R-M4c-11, R-T0-1/2),
  `Rejection::{MinMax, Esd, Rcr}` as user choices with the Auto ladder left
  byte-for-byte unchanged and a dependency-free `integration/student_t.rs`
  (R-M4c-1/2), the Winsorized loop of §6.3 with its zero-MAD fallback
  (R-M4c-3, R-T2-1), large-scale rejection through processed `.rejl` bitmaps
  and a second integration pass writing its own `pass2/` bitmap set that
  drizzle reads (R-M4c-4, R-T3-1), `geometry/tps.rs` +
  `stacking/register/local_loop.rs` (R-M4c-5/6/7, R-T4-1…5), and LN's
  local-scale spline + barycentre second matching pass (R-M4c-8/9,
  R-T5-1/2) — **SHIPPED pending acceptance**: the LDN 1272 re-run (variants
  A–E over the real trail found for R-M4c-10) is Task 7.
- **M4d — outputs and product** (`docs/superpowers/plans/2026-09-10-stacking-m4d-plan-outputs.md`):
  Bayer drizzle, XISF output, cataloging masters (a `master_lights` entity
  linked to the set and a preview in the Results cards), and preset
  management.

## 15. Deferred and open

- Union/mosaic geometry, comet mode, multi-set stacking: not planned.
- `PSFScaleSNR` weights (needs LN relative scale factors): M2 follow-up.
- Adaptive normalization: not planned (LN covers the use case).
- Registration distortion `tps` smoothing and outlier defaults: our own
  values, to be set from the M4 acceptance run.
- Whether masters should be cataloged and how they appear in the Objects
  page: M4 decision, after the owner has used M1–M3 output for a while.
- **Mixed pixel scales in one set — required (owner, 2026-09-09), DONE in
  M4b** (`docs/superpowers/plans/2026-09-10-stacking-m4b-plan-mixed-pixel-scales.md`;
  the shipped design is §3.8 above, the config field §9.2's
  `registration.geometry`). Everything below describes the state BEFORE that
  plan and is kept for the reasoning: M1
  registered every group onto the set's one reference and refused a frame
  whose fitted scale is outside `[0.8, 1.25]` (§3.6), so a bin-2 group, a
  second telescope with another focal length, or a camera with another
  pixel size could not join the set's masters — the group key's
  binning × geometry split keeps them apart, and registration then drops
  every frame of the foreign-scale group, one at a time, with a visible
  per-frame exclusion reason in the frames table. **Correction (M2 final
  fix wave, ruling I4):** the paragraph below used to promise the plan-time
  blocker as "an M2 quick win" — Task 10 (2026-09-10, camera-agnostic
  grouping) explicitly ruled "no new gate" for that cycle, so it was never
  built; the per-frame registration-gate drop above remains the ONLY
  defence today. Planned as an M4 item (after drizzle, which changes the
  output scale for every group alike), with two modes the owner picks per
  set: (a) **co-registered** — widen the scale gate, resample every group
  into the reference geometry (the finer scale loses resolution, the
  coarser is upsampled) and keep one master per group in one geometry; (b)
  **native** — a per-group reference (the group's best-weighted frame) and
  a master in the group's own geometry, no cross-group registration. Both
  keep the group key. **First item of this M4 work** (moved from the M2
  promise above): the plan gate reports a group whose members' implied
  pixel scales differ beyond the registration gate's tolerance as a named
  WARNING, never a blocker (matching the missing-`EXPTIME` cluster's own
  precedent) — "group `<key>` spans ×2.0 the reference's pixel scale —
  some frames may be dropped at registration" — decided from the header's
  binning and, when both frames have a plate solve, the ratio of their
  pixel scales (`f.focallen` and a new pixel-size column in
  `load_group_members`, `stacking/groups.rs`).

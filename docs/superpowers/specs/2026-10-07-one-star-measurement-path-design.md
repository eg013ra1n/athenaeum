# One star-measurement path — design

Status: draft for the owner's review, 2026-10-07; revised after the independent spec review (Critical 3,
Important 12, Minor 14 — all folded in). Program: star measurement (part 1 was the truth lab; this spec replaces
the former parts 2 and 3). Baseline: `docs/superpowers/research/2026-10-07-star-measurement-baseline.md` (lab
athenaeum-lab e65af49, probes from f5321979); the path comparison below is
`star-lab/results/baseline-2026-10-07/path-compare.txt` (lab 5fc780c, computed from the committed summary and
the existing probe outputs). A second independent re-review (N-1…N-9) is folded in as well. Revised 2026-10-09:
§2.1's saturation test is replaced (owner rulings 2026-10-09) after the flat-top test failed on lab truth.
Amended 2026-10-11 (Plan A Task 10b, owner-approved scope; the design rulings are the controller's): §3 row F-10 "one
background", its bars and the owned-quantities table; §2.1's `seeds`, `fit`, `beta`, `photometry` and `diagnostics`
rows and the β-sample sentence follow it. F-8 and F-9 keep their meaning (Plan B), so the new row is F-10.
Amended again 2026-10-11 after the plan review (controller rulings P2/P3, R1–R3): the M42 frame β is reported in
F-10 and its bar moves to a new owner task (Plan A Task 10c, the saturation flag's near-clip class, F-3); `extended`
leaves `c`'s qualifiers and the frame statistic as well as the β sample; frame classes `trailed` / `elongated`; every
F-10 bar is derived from data or from the PSF and the noise, or reported with its reason; one owner per quantity.
Amended 2026-10-12 (Plan A Task 10b as built, after its joint lab cycle and review): §3 row F-10 records what was built
and how its bars were judged (every ruling with its numbers in `docs/stacking/README.md` R-OSP-7b); §2.1's `seeds` and
`fit` rows, its field-width sentence, the owned-quantities table and §4's frame statistic follow it.
Amended 2026-10-12 (Plan A Task 10c as built, after its lab cycle `t10c-floor` and review): §2.1's flag gains the
near-clip clause with the plane's clip floor and its limits; §3 row F-3 records the class and its bar as judged
(rulings R10c-0…R10c-11 with their numbers in `docs/stacking/README.md` R-OSP-7c); the owned-quantities table and §5
follow it; the M42 bar's B2 is a known miss owned by Plan A Task 11b (F-4b, a frame-level β) — resolved there by
definition: B2 is not a bar (Task 11b as built, below).
Amended 2026-10-12 (Plan A Task 11 as built, after its lab cycle `t11-f6` and review): §3 row F-6 records the SNR and
HFR as built and its bar as amended by ruling R11-4 (50 cells, the three nebula cells reported, the noise-pull row;
rulings R11-1…R11-10 with their numbers in `docs/stacking/README.md` R-OSP-8); the owned-quantities table and §4.2's
HFR line follow it.
Amended 2026-10-13 (Plan A Task 11b as built, owner decision R11b-12, after its lab read L5 and review): §3 row F-4
records the stacking GROUP β (`psf_model: Auto` in stacking = `psf_signal::group_beta` of the members' own βs, Measure
two-pass, the frame's own β the diagnostic `beta_own`) and its bars as judged (rulings R11b-0…R11b-13 with their
numbers in `docs/stacking/README.md` R-OSP-9); the M42 bar's B2 is resolved by that definition and is not a bar (3c and
3b's FWHM_m2 row are recorded as the composite-PSF definition limit); §2.1's `beta` row and β sentence, §2.2's
`psf_model` line, the owned-quantities table (frame F, frame β, a new group-β row, `betaAtCap`), §5 (step 4 and the
Plan C default PSF, Moffat 4) and §7's first risk follow it.
Amended 2026-10-20 (Plan B scope, from the Plan B design read and the controller's rulings R-B1…R-B10 on it, under the
owner's standing rulings: the ERP is a column in every LN table, synthetic and real, never the referee; the referee is
synthetic truth and the harness photometric truth; a cell where ours and the ERP both miss the truth by more than 3 %
the same way flags that truth as suspect; the ERP is re-run only when synthetic frames change): §3 rows F-8 (the LN
estimator and its referee) and F-9 (BOTH `c` = 1 floors: Measure's fit-call floor and LN F1's growth floor, R-B1) get
their defects, changes and bars; three rows are added, F-11 (`c` kept per plane, additive, R-B2), F-12 (`c`'s
qualifiers and sample, `MEASURE_VERSION` 5, R-B8) and F-13 (LN's own seeds, saturation definitions and nebula sky,
R-B9); every Plan B bar is fixed in the new "Plan B bars" block before any Plan B measurement; the owned-quantities
table (`c`, seeds, the `saturated` flag) and the clip-screen sentence below it follow; §5's Plan B paragraph becomes
stages B0–B6. Open with the owner: new ERP LN runs where none exist (set 204 full mono and OSC, run B mono and OSC;
the nebula-sky and the injected pairs); until then those cells read "not run". Plan B's rulings and acceptance runs go
to `docs/stacking/README.md` as R-OSP-16.

## 1. Goal and owner decisions

Every star number the app shows or uses — the Analysis tab, Blink, the plate-solve input gate, the stacking weights,
LN, the master FWHM — comes from ONE measurement path, and that path is correct against synthetic truth and fast.

Owner decisions (2026-10-07):

- **D-1 One path.** The stacking measure path is the base ("Да согласен, и надо сделать этот путь хорошим"). It
  is fixed, extracted into its own module, and the Analysis tab is switched onto it. The rustafits full analysis
  is no longer called by the app.
- **D-2 FWHM is the fit width.** The "correct" FWHM for every consumer is the width a model fit to the pixel values
  gives — the lab's `pointfit` truth column. Prepixel (optical) and pixbox widths stay lab columns only.
- **D-3 Analysis star budget.** The Analysis tab and Blink measure at most 500 good unsaturated stars spread over
  the field, configurable in Settings → Analysis. Stacking keeps measuring every star.

Why the stacking path (`path-compare.txt` part 1: medians over the (family, image) rows where both paths print the
metric, measure vs analysis):

| Metric | Rows | Measure | Analysis | Measure nearer the ideal |
| ---- | ---- | ---- | ---- | ---- |
| per-star FWHM abs(bias) vs pointfit | 137 | 0.1 % | 0.69 % | 108 of 137 |
| per-star FWHM scatter (each path's own star sample) | 137 | 0.56 % | 0.87 % | 79 of 137 |
| per-star FWHM efficiency vs its own CRLB (1 = ideal) | 136 | 1.165 | 1.245 | 80 of 136 |
| frame FWHM abs(error) vs pointfit median | 137 | 0.5417 % | 0.6388 % | 77 of 137 |
| frame ecc abs(error), all rows (round rows read the floor) | 137 | 0.0838 | 0.0955 | 90 of 137 |
| frame ecc abs(error), elongated truth only | 27 | 0.0032 | 0.0015 | 9 of 27 |
| completeness, raw % of all truth stars (one denominator) | 137 | 64.5 | 29.1 | 137 of 137 |
| bright coverage, % (saturated stars counted) | 121 | 99.3 | 38.3 | 118 of 121 |
| angle rms | 26 | 26.35° | 17.63° | 5 of 26 |

Seventeen of the 137 rows are resampled replicas of five fields (correlated, not duplicated). The analysis path is
better on two rows (frame ecc of elongated frames, angle); both are defects of the measure path this spec fixes
(F-2). Cost (`probe-runs-2026-10-07/batches.txt`, m42, 77 frames): measure 223.1 s, analysis 25.2 s
— about 2.9 s vs 0.33 s per frame (derived); at 5417 fits per frame the measure path spends about 0.53 ms per star
(derived). The analysis path is faster per frame only through its 500-star cap.

## 2. Architecture

### 2.1 The module

A new module `crates/athenaeum-core/src/star_measure/`, compiled under `feature = "render"` (as `analysis`;
`stacking` needs `render + solver`, so the Analysis tab must not depend on `stacking`). Dependencies the extraction
moves or leaves: `stacking::robust` (`rcr`, `winsorize`, used by TMeanFlux) moves into the module;
`stacking::prefilter` and `stacking::structure` stay in stacking as adapter-side seed sources (§2.2);
`integration::{plane_reader, stats}` and `astroimage` are already `render`.

| Unit | Responsibility |
| ---- | ---- |
| `domain` | input planes: calibrated float (unit domain) or raw ADU, CFA layout; the plane's noise (MRS) and background model |
| `seeds` | star detection: a matched filter at the field width on the plane minus its 128-px mesh (the plane's one background map), threshold K = 6 in units of σ_f, the PROPAGATED local noise (the mesh's per-cell clipped σ × the plane's filtered-noise gain: 1 on a plane the caller states white, read on a mixed or unknown one: debayered, resampled, integrated; F-10); the two knot tests (a structure drop, an `extended` flag; F-10); the field width (owned by F-10: the half-sample mode of every readable half-maximum read); selection by integrated signal; the default seed source of both consumers |
| `fit` | per-star region, background start (the fit-scale sky: a FREE local b, F-7), width start, the Moffat fit (rustafits `fit_moffat_2d` / `fit_moffat_2d_fixed_beta` as library functions), canonical axes, acceptance (the centre test on the fit's own half-maximum contour, F-10), the per-star outcome; the plane-level trailed verdict (F-10; there is no per-seed streak refusal) |
| `beta` | the per-frame β (§3 F-4) on unsaturated stars whose verdict fit passes the centre and region tests and that the knot test does not mark extended (F-10); in stacking the members' own βs feed the group β every member is fitted at, and the frame's own β is the diagnostic `beta_own` (F-4 as built, Task 11b) |
| `photometry` | the per-star aperture-scale sky B_i and noise σ_i (F-10: R6's ring, computed once per accepted star, stored on the fit; every aperture consumer reads them), the profile-independent star flux (F-5: model flux in the FWTM ellipse × `c`, `c` from 5F growth sums over (v − B_i), the amplitude screen, the footprint, the caller's full scale, 64 stars / at least 5, none `saturated` or `extended` (F-10), `aperture_stars`), the saturation test, the per-star SNR (F-6, on B_i and σ_i) |
| `frame` | frame aggregates: FWHM, ecc (over fits flagged neither `saturated` nor `extended`, F-10), SNR, the weight inputs, HFR, the trail metric and the trailed verdict's readers (F-10) |
| `budget` | the Analysis-mode selection: at most N stars spread over the field. *As built (Plan A Task 12, README R-OSP-10): `Mode::Budget`, water-filled 4 x 4 candidates and an unflagged-first SNR trim, no own-beta work at Moffat 4.* |
| `diagnostics` | `FitDiagnostics`: per-star outcome and refusal reason (F-10 adds `trailed`), region radius, background start, growth steps, iterations |

Public surface: `measure_plane(plane, opts) -> PlaneMeasurement` (stars, frame aggregates, diagnostics, refusal
counts) and the refit entry point LN needs, `fit_stars_at(plane, seeds, beta, opts)`. The module exports
`MEASURE_VERSION` (§4.4).

**The saturation test (one definition, used by every module caller, raw and calibrated planes, frames and integrated
masters alike; revised 2026-10-09).** The method an established stacking program uses, adopted as is (owner ruling 2026-10-09)
after the earlier flat-top test failed on lab truth. Three of its inputs are the module's own substitutes (bg, σ,
pixel0), named below; there is no raw-clip clause and no `saturation_fraction`.

Per plane, in the module's working units:
- **bg** = `SkyLevel.median` scaled to working units: the module's sampled, clipped sky median (PA-21), the
  substitute for the reference method's full-plane median (the module computes no other); **σ** = `NoiseModel.sigma`
  (MRS; `N*` when MRS is refused; 0 under `NoiseSource::None`, with one `warn!` per plane when seeds are given), the
  substitute for the reference method's background-noise estimate; **threshold** = bg + 5σ;
- **norm** = the plane's full scale in working units: the image of 1.0, `ADU_SCALE`, for a calibrated plane; the
  stored integer type's full scale for raw ADU (2^bits − 1 from BITPIX/BZERO: 65535 for unsigned 16-bit, i.e.
  BITPIX 16 with BZERO 32768, and 255 for 8-bit; never DATAMAX). The float raw plane (BITPIX −32) has no integer
  full scale; Plan C settles it. **range** = min(plane max, norm) − bg, the plane max read over the same working copy.

Per seed (every seed gets the whole test, as the reference method fits every candidate):
- **The plateau test at the seed pixel.** pixel0 = the pixel at the rounded seed centre (`fit_one`'s own `(cx, cy)`),
  the substitute for the reference method's own pick: for the matched filter that is the rounded centroid of the box
  about its local maximum, at most a pixel from that maximum on a plateau; for given seeds the caller's position.
  Cost: FN/FP move from the lab port's numbers; Phase C measures it.
  Of pixel0's 8 neighbours, those ≥ threshold count: a mono plane needs ≥ 3 of them, a colour plane all 8; meanhigh
  and minhigh are their mean and minimum. The test passes when meanhigh − bg ≥ 0.7·range AND
  pixel0 − minhigh ≤ 0.1·range.
- **The level** = min(pixel0, norm) − 0.1·range when the plateau test passes, else norm.
- **The verdict fit**: a free-β Moffat on the seed's own region (F-1) with every pixel ≥ the level excluded (left
  out of the fit, never modelled as clipped). The iteration limit is ×3 when anything is excluded. When no more
  pixels remain than the fit has parameters (n ≤ 8), the seed is refused `MostlySaturated`. A verdict fit that fails
  for any other reason (bounds, divergence) gives the seed no flag and no refusal: it is counted in
  `FitDiagnostics.verdict`, stays out of the β sample, and the frame-β fit decides acceptance as now.
- **The flag**: saturated when the verdict fit's amplitude A > range, OR (plateau-clip clause, owner 2026-10-09,
  after research R-S) when the plateau test passed and `pixel0 − bg ≥ 0.9·range`, whatever the fitted amplitude and
  even when the fit failed: a seed inside a large saturated area, whose fit sees only the rim, is flagged too. Known
  limit: the plane's brightest flat-topped star is flagged by construction (it sets the range). OR (near-clip clause,
  Plan A Task 10c, 2026-10-12) when the plateau test did NOT pass and the 3×3 maximum about pixel0 (the region's
  present pixels) is ≥ the plane's clip floor (below): a star clipped on one or two pixels, whose neighbours' mean is
  under 0.7·range, keeps the level at norm, so its fit follows the clipped top (A/range 0.92 on M42 0000) and β rises
  instead of the amplitude passing the range; the flag holds when the fit failed, the level and the fit are unchanged.
  Otherwise the plateau test only sets the level and the flag comes from the fit.
- **A fit that never leaves its start is refused** (fix rounds A3 / A4, 2026-10-09): a bounded or conditioned fit
  whose solve accepted no step and did not converge (a guard-refused first step on a censored star used to spend the
  reject budget and return the start as a fit) is `None`: the verdict reads `fit = false`, a sample star is counted out
  of the median, a fit at β is refused `fitFailed`; every refusal is counted and logged at `debug!`.

**The clip floor (Plan A Task 10c, ruling R10c-10; derived per plane; owner: ideally the threshold adapts to the plane).** Once per
plane, after every seed's verdict and before the multi-plane rule, the plane's own clipped evidence sets the floor
clause 3 reads (`SaturationPlane::clip_floor`): E = {the plane max, when ≥ 2 present pixels equal it — an exact clip;
the tie count rides on the plane-max reduce} ∪ {pixel0 of every seed that clause 1 (A > range) AND clause 2 (plateau,
pixel0 − bg ≥ 0.9·range) both flag — a certified clipped core}; floor = min E, no margin (a 5σ margin changes no lab or
real number). **E empty → no near-clip class on that plane**: a clip-free plane has no evidence, so its brightest
clean star (which sets the range) is not flagged by clause 3. An exact clip collapses the floor to the clip value; a
flat-divided clip takes its lowest certified clipped value (M42 0.921–0.946 of range, its clip running from 0.964;
hc11B L 0.900–0.910; LDN 1272 mono 0.900; its OSC planes 0.936 / 0.994 / 0.912). pixel0, not the 3×3 maximum, is the
evidence: on a debayered plane a certified core's 3×3 maximum is debayer overshoot. The floor is never below
bg + 0.9·range (the tie member is the plane max; clause 2's gate holds pixel0 above the band), so class B is a subset
of the knot exemption's near-clip set (F-10), which keeps the FIXED 0.9·range band because it runs before any verdict
exists — a documented superset; detection is unchanged. The fixed band as clause 3's own level (9f2cafc7) was
measured and replaced: on a clip-free or exact-clip plane it flagged the brightest unsaturated stars (lab FP mono
0.24 → 1.05 %, 871 newly flagged on 68 planes, 0 of them truth-saturated) and the frame ecc rose toward the faint stars'
noise-biased value (truthcheck +0.03…+0.07, `neb3` +0.025; R10c-8). Limits (recorded, not bars): (a) a clipped plane
whose clipped stars are all one-pixel clips (no tie, no certified core) gets no class B — resampled `f2.0` keeps its 4
false negatives, as before; no real frame tested qualifies (every one has 7–321 certified cores per plane); (b) the
floor never goes below the plateau band, so LDN 1272 mono's clip at 0.86–0.90 of range stays unflagged (its β is the
clamp, R10c-2); (c) the floor is the frame-wide lowest clipped value, so an unclipped star between it and a vignetted
corner's clip (about 1.03 on M42) is flagged; (d) a single certified star can set the floor; (e) clauses 1 and 2 keep
their own clip-free false positives (lab 0.23 %, the brightest star by construction, above); (f) a `MostlySaturated`
refusal (the heaviest clipped core) has no verdict, so its pixel0 is never evidence — on a flat-divided clip the floor
can only sit too high (the false-negative side) when the lowest-clipped stars are all refused; (g) on an integer plane
a chance tie at a clip-free maximum sets a floor at the plane max — class B then adds only the star(s) holding that
pixel, which clause 1 flags anyway (e).

Flagged stars are kept: an accepted fit carries the flag in the `.athf` flags byte. The β sample and the frame
statistic use unflagged stars only; the field width reads every candidate (F25), the per-star width start is the
star's own (F-3); the field width still reads every candidate (F-10 kept F25: it is now the half-sample mode of the
readable half-maximum reads, which a clipped core or any other contaminant wider than the PSF does not move). The
unflagged bright seeds whose verdict fit passes the fit's own centre and region tests and that the
knot test does not mark extended (F-10) are the β sample (F-4): each is RE-FITTED pixel-integrated, on its verdict fit's own
pixels and starts (owner E5, 2026-10-09; its median is the frame β), and every star is then fitted at the frame β with
the point model (in stacking at the group β: F-4 as built, Task 11b). A FLAGGED star (own or cross-plane flag, below) excludes the pixels ≥ its own level
(norm when its own plateau test failed), ×3 iterations; an unflagged star excludes nothing, and its fit is
byte-identical to a plain fit (revised 2026-10-09: excluding the top of unflagged plateau stars cost 218
truth-unsaturated bright stars their width in the lab). Registration's `passes_register_cuts` (peak ≥ 0.95) still holds, since censoring raises the fitted peak;
the parity gate checks it.

**Multi-plane frames (owner ruling 2026-10-09).** On a frame or master with more than one plane (the debayered
OSC planes), a star flagged on ANY plane — or refused `MostlySaturated` there — is flagged on EVERY plane at that
position (seed positions matched within 1 px; only a plane's own flags spread, so the union is not transitive). The
union is formed after every plane's verdict pass and before any plane's β sample, frame statistic or at-β fits; one
multi-plane entry serves frames and master statistics alike. Reason: the debayer mixes channels, so a star clipped
in one channel corrupts the others around it, and a debayered R or B plane shows no plateau at the seed pixel. The
reference method measures one plane of a colour image and has no such rule; this is our extension. Lab: OSC false
negatives 52 of 1229 → 0; the 90 added flags are all saturated in another channel.

**Known limit (owner ruling 2026-10-09).** A star with a small clipped core (3–5 px) fails the full-3×3 plateau
test, so its verdict fit runs uncensored. It is still flagged (its fitted A exceeds the range), but its own FWHM is
not trusted (+31 % median in the lab). No consumer reads it:
- the frame statistics exclude flagged stars;
- the Analysis tab (Plan C) takes unsaturated stars;
- registration takes centroids.

Where the plateau test passes, the censored fit reads the width within 2.2 % median (9.9 % p90) against 41 %
uncensored (lab research 2026-10-09, exact ties).

The range is plane-relative and capped at norm. A calibrated plane's clip level varies per pixel after the flat
division ("saturated cores above 1.0 are legal", `docs/stacking/README.md`); the cap is what keeps the test working
there (lab, 30 % vignetting: 99.95 % of the clipped stars flagged).

- Bars that say "unsaturated" mean the truth's `saturated` flag (the star reaches the generator's clip,
  `families.py`); one self-consistency row checks the module's flag against it: false negatives ≤ 0.5 % among the
  truth-saturated stars with at least 3 clipped core pixels in the noisy image; false positives are reported as a
  count and a share of the truth-unsaturated stars, with no bar. The test flags some bright unsaturated stars by
  design: a plane's brightest star when its own peak sets the range, a sharp star whose model peak tops its
  brightest pixel, and, through the cap, a star the flat division lifts above full scale (lab: 0.6–0.8 %, 7.5 %
  under 30 % vignetting).
  The truth flag also marks stars a neighbour clips and stars that barely reach the clip (in the v2 i0 truth: 61 saturated
  only through a neighbour and 90 within 2 % of the clip, two-sided, among the saturated); no per-star test sees those, and they are
  reported as counts, not judged.

**Exemptions (unchanged consumers of the numbers, not callers of the module's tests):** LN keeps its L-14 relative
rule, its L-23 reference-frames mask and its peak-SNR cut on warped planes, where a core is interpolated
(`ln/selection.rs:129-157`); registration keeps `passes_register_cuts` (0.95 × 65535 and
1/residual ≥ 10, `register/detect.rs:182-195`); LN's reference fit (`fit_stars` on the integrated reference,
`ln/scale.rs`) runs no verdict. Routing them through the module is a later question, not this spec.

### 2.2 Consumers

- **Stacking.** `stacking/measure.rs` becomes a thin adapter that calls the module on every star and keeps today's
  artifacts (`.athf` per plane, the `metrics` payload `FrameMeasurement`, `ChannelMeasurement`). `psf_signal.rs`
  keeps the weight formulas (PSFSW, PSFSNR) and the LN-specific pieces and takes fits, flux and the saturation test
  from the module. LN (`ln/scale.rs`) refits through `fit_stars_at`. The master statistics (`integrate.rs`) and mono
  registration (`register/frame.rs` reading the `.athf`) consume the same numbers. `stars_fitted` keeps its meaning
  (every accepted fit; the Formula weight's `stars` term and `min_stars` read it); the frame statistic uses the
  unsaturated subset (F-3) and `ChannelMeasurement` gains `stars_refused` with per-reason counts.
- **The stacking Measure config** (`MeasurementConfig`, presets, `MeasurePanel.tsx`, the TS types, the measurement
  stage hash), changed together:
  - the seed source becomes one field, `seed_source`, whose default is the module's matched filter (F-1's seed
    fixes reach every set that never chose otherwise); Peak (with its own `detection_sigma`, a peak threshold),
    Median3 and Structure stay explicit choices that feed `fit_stars_at` from the adapter;
  - every stored stacking document — a set's own config, the stored global config `resolve_config` falls back to,
    and the user presets — is read so that an explicit non-default choice survives (Structure; Peak with the Median3
    prefilter or a `detection_sigma` other than 20) and the old default (Peak, σ 20, no prefilter) becomes the
    matched filter. Median3 is Peak's prefilter only, so a stored (Structure, Median3) becomes Structure.
    Whole-config precedence is kept, and a stored value is never reinterpreted in other units; ruling R-M4c-11 in
    `docs/stacking/README.md` is updated;
  - `psf_model`: `Auto` becomes the continuous per-frame β (F-4), `Moffat4` stays a fixed choice; from Task 11b `Auto`
    in stacking is the GROUP β of the members' continuous βs, every member fitted at it (F-4 as built);
  - `STACKING_CONFIG_VERSION` is unchanged (new fields are serde defaults).
- **Analysis and Blink.** `analysis/analyzer.rs` (`map_result` is today's single writer of `FrameAnalysis` and
  `StarMetric`) calls the module in Analysis mode (§4). Commands, events, the batch job and the cache keep their
  shape. `get_frame_star_metrics` re-analyses on demand without a ComputeQueue slot; the 0.6 s bar (§3) covers it.
- **Not touched.** The fast detector used by plate solving (solvemyastro) and by the registration detector
  (`stacking/register/detect.rs`). The rustafits analysis code stays in rustafits, unused by the app.
- **The lab.** `star_probe` runs the module in both modes (stacking: all stars; Analysis: the budget) and keeps the
  rustafits analysis as a `legacy-analysis` arm, so the scorecard shows before and after side by side.

## 3. What is fixed, and the bars

Every bar is fixed here, before anything is measured, and judged by the truth lab against synthetic truth
(athenaeum-lab `star-lab/`). The ERP stays a comparison column, never the referee. A missed bar goes to the owner with
its numbers; a bar is never moved, a row never re-sized, a generator never tuned toward a bar.

| # | Defect (baseline) | Change | Bar |
| ---- | ---- | ---- | ---- |
| F-0 | No refusal reasons (`sharp/m4/f8.0`: 82.7 % fitted of 92.0 % seeded) | `FitDiagnostics` per seed; `stars_refused` + per-reason counts in `ChannelMeasurement` and `FrameAnalysis` (§4.2); the `.athf` flags byte carries per-accepted-fit flags (saturated, β at its cap), the file holds accepted fits only | accepted + refused-by-reason = seeds on every image; numbers unchanged against the extraction step |
| F-1 | Broad / defocused stars collapse (M51 Hα 6.53 px vs analysis 13.39, ERP 14.02; m82 11.43 vs 16.31 / 17.76; `_DSC5767` zero seeds) | region growth judged on (median − sky), not the absolute median; the region cap scales with the star's own width start (≥ 6 FWHM, cap 96 px; today `min(2·nominal, 48)`); background start from the region's rim, not the plain median of a star-filled square; width start from the star's own moments, not one field σ0; matched-filter seeds (§2.1); new lab families FWHM 12 and 16 px and defocused annular ("donut") stars, isolated (`min_sep` ≥ 6 FWHM) | fitted / seeded at FWHM 8: 82.7 % → ≥ 99 %; frame FWHM error vs `pointfit` ≤ 1 % at every FWHM up to 16 px; a donut star is refused with its reason, never reported as a narrow fit |
| F-2 | Angle: measure rms 44.564° / 32.339° on `shape/f2.6/e0.3|e0.5/t30` vs ERP 7.849° / 2.129° (summary row 4); 71–145 of 346–427 unsaturated fits per `e0.3` family have fwhm_x < fwhm_y (θ on the minor axis; `path-compare.txt` part 2) | canonical axes (fwhm_x ≥ fwhm_y, θ on the major axis); the fitter sees stamp-centred coordinates and amplitude-normalised values so its relative convergence test acts on O(1) parameters (the convergence test itself changes in rustafits only if the lab shows it is needed) | structural: every accepted fit has fwhm_x ≥ fwhm_y and θ on fwhm_x's axis, pinned by a test; the row-4 angle `eff` cell (prepixel CRLB) ≤ 1.5 on the `shape` families with e ≥ 0.3 |
| F-3 | β and frame FWHM move with the saturated set (2× flux twins: 9.99 %, 15.93 % with a β flip; 2.93 % with β held) | the β sample and the frame statistic use unsaturated stars only (§2.1's test); the field width reads every candidate (F25), the per-star width start is the star's own. **As built (Task 10c, 2026-10-12; athenaeum 9f2cafc7 the class at a fixed band, 7933f582 the floor derived per plane; every ruling with its numbers in `docs/stacking/README.md` R-OSP-7c):** the near-clip clause (§2.1: plateau failed ∧ 3×3 maximum ≥ the plane's clip floor; no evidence, no class B). **As judged** (lab cycle `t10c-floor` at 7933f582, athenaeum-lab 7eb48fb): the M42 frame-β bar R10c-0 (robust form, fixed before code; M = the 11 other M42 mono2 L calibrated frames, C = the hc11B L replica 0000–0009) — M42 0000's β 31.777 (SE 15.89) → 7.568 (SE 1.173), F 2.342 → 2.267; B1 z₀ 3.60 ≤ k 5.39, B3 0.051 ≤ 0.207 pass; **B2 0.155 > 0.046 a known miss** owned by Task 11b (F-4b: the frame β estimator on a one-sided mean-profile shoulder, R10c-6/7; 0000 is a clean frame) — resolved there by definition (Task 11b, 2026-10-13: B2 is not a bar; F-4 as built); row 10 FN 4 of 11 778 (= base; dev Mac 4 of 12 169) and 0 of 1 229, FP 0.244 % / 0.410 % (base 0.242 / 0.397); twins 48 / 48 (F worst 0.69 %, β worst 6.8 %, 24 void); 0 of 333 lab planes moved (β > 1 %, F > 0.1 %) vs base; C-11 frame ecc vs the base max +0.00145, 0 planes over +0.002 (one marginal new C-11 WORSE plane, `osc_pairs/f3.5` t3 p0 at 1× the line, accepted, R10c-11); seed sets identical on 391 of 391 outputs; every held bar (F-4 item 1 58 / 58, C1.1–C1.4, D.1, K.1, S.1 0.81 %, M.1 76.8 s) passes. The fixed-band run (`t10c-nearclip`, lab 2971210) had FP 1.047 % / 0.543 % and C-11 ecc up to +0.071 → the floor (R10c-8) | base vs 2× twins: frame FWHM change ≤ 1 %; β change ≤ 10 % relative (while β is still discrete at this step, that means zero flips; the twin rows are judged again after F-4, and the β part is void where both twins sit at the cap) |
| F-4 | Model-dependent width (Gaussian / blur pointfit −2.07…−3.04 %; β alone 9.60 %; `mixture/b` +4.88 %) | β becomes continuous per frame: a free-β Moffat fit on the unsaturated bright sample gives the frame's β (median), clamped to [1.5, 100]; every star is then fitted at it. **As built (owner E5, 2026-10-09):** the sample's fit is PIXEL-INTEGRATED (the model the mean over k × k sub-samples per pixel, k = clamp(⌈10/F_start⌉, 3, 9)), because a point model fitted to integrated pixels reads β high on undersampled stars (the lab step stopped on it); the verdict fit and every fit at β stay the point model. "Fits nothing twice" no longer holds: each plane runs up to 64 extra fits. A named rustafits change: `fit_moffat_2d` refuses β > 20 today (`rf/analysis/fitting.rs:545-551`), so its bound is lifted to the cap and the sample fit runs in a conditioned parametrization (ln β), because the Gaussian limit is a flat ridge in (α, β). At β 100 a Gaussian star reads −0.32 % (β 30: −1.04 %; spec re-review N-1). The Gaussian is the large-β limit; there is no separate Gaussian model, so β stays a finite number everywhere (`.athf` f32, `metrics` JSON, `stacking_run_groups.beta`, `group_beta`, `PsfModel::Fixed`, `star_metrics.beta`, `median_beta`; `fit_method` is `moffat`; `FITS_ARTIFACT_VERSION` unchanged). **As built (Task 11b, F-4b, 2026-10-13; athenaeum 0f70e3c0, 3af1fd8d the review round; owner decision R11b-12; every ruling with its numbers in `docs/stacking/README.md` R-OSP-9):** in stacking, `psf_model: Auto` is the GROUP β — `psf_signal::group_beta`, the lower median of the members' own βs, the same value LN fits its reference and every member at — and every at-β fit, F, `.athf`, `c` and F-6 in Measure is made at it. Measure is two-pass: pass 1 per frame (read, seeds, verdict, this row's E5 sample → the frame's own β), the group β resolved once, pass 2 per frame at `PsfModel::Fixed(group β)`; under `Auto` the measurement hash folds the group β (a cache hit while it stands, the group re-fitted when it moves). The per-frame β (E5, unchanged) is `beta_own` (+ `beta_own_at_cap`, the own β at a bound): a diagnostic, reported, never consumed. A group of one sits at its own β (an OSC frame at the lower median of its planes' own βs). The single-frame path (`star_probe`'s default, Analysis / Plan C) keeps its own β; `Moffat4` / `Fixed` stay one pass on the old hash. Why: on a composite PSF (M42 0000: 5 % wider than 0001 with a weak frame-wide +x skew) a single Moffat's per-star β is bimodal and the frame's χ²-best β sits at a mismatch value (7.568 against 2.6–4.8 on its siblings); F rides the (α, β) ridge at +1.8 % per unit β, and the consumers compare F across the group, so one β per compared set takes the ridge out of every comparison. **As judged** (lab read L5 at 0f70e3c0, athenaeum-lab 1e52339): the single-frame path is unchanged in value (487 probe outputs, every line identical but for the additive `betaOwn` == `beta`); M42 group β 3.3066 (0003's own β); **3c** F(0000) / F(0001) 1.0431 ± 0.0013 against [1.044, 1.062], missed by 0.0009 (0.7 SE; own β 1.0883); **3b** ours / HFR passes (z₀ 1.01 ≤ k 2.66), ours / FWHM_m2 misses (z₀ 5.16 > k 2.16, its sign flipped: the second moments weigh the skew shoulder a Moffat core does not). Both misses are recorded as the composite-PSF definition limit (the independent referees themselves span 1.043–1.062 on 0000), never a bar moved. **The M42 bar's B2 is not a bar** — resolved by this definition; B1 / B3 read `beta_own` = Task 10c's numbers. Task 13 judges the group-β Measure at master level; F-4's lab bar is unchanged | frame FWHM error vs `pointfit` ≤ 1 % at FWHM ≥ 2 px, ≤ 3 % below, on every family whose stars share one profile (`gauss`, `m*`, `blur*`, `mix*`: the lab's pointfit truth there is a free-β fit, `families.py:113-134`) |
| F-5 | Captured fraction depends on the profile (spread 18.73 % at F 2.6, 22.16 % at F 8.0) → PSFSW / PSFSNR / psfSel | star flux = fitted-model flux in the FWTM ellipse × a per-plane aperture correction `c` (as built, README R-OSP-7): the median over the 64 brightest qualifying fits (at least 5, else `c` = 1) of the 5F growth sum over the model flux, the background the plain mean of the [10F, 12F] far ring outside every seed's 3F disc; a candidate whose fitted amplitude tops the plane's range does not qualify (the F-3 amplitude screen, full scale from the caller); its 12F disc must lie inside the plane and the caller's footprint (LN's `allowed`); `aperture_stars` counts the qualifiers; psfSel reads this flux | spread across profile families ≤ 2 % at F 2.6 and F 8.0; psfSel: `pairs/iso/blur25mix` t60 −13.03 % and `pairs/iso/blur4` t60 −7.28 % → within ±2 %, the Moffat and Gaussian pairs stay within ±1.2 % |
| F-6 | No per-star SNR (stacking); the analysis SNR treats ADU as electrons (gain 1, `rf/analysis/snr.rs`) | **As built (Task 11, 2026-10-12; athenaeum 1224718a, 12706d6c the review round; every ruling with its numbers in `docs/stacking/README.md` R-OSP-8).** **SNR.** One definition: S / σ_S over the present pixels whose centres lie within max(1.5·FWHM, 3 px) of the fitted centre (the fit's own FWHM), S = Σ(v − B_i) on the star's stored aperture-scale sky (F-10; the [3F, 4F) annulus is retired for SNR); σ_S² = s·max(S, 0)/g + κ(A)·N_ap·σ_i²·(1 + N_ap/n_i), σ_i and n_i the star's own ring noise and pixel count (F-10): F-6 estimates no sky and no noise. κ(A) = Σ_{i,j∈A} ρ(i − j) / N_ap over the aperture's present pixels, ρ = C/C(0) from the plane's ONE noise read (F-10's detection-noise read on a `Mixed` / `Unknown` plane, reach 2; its owner is unchanged, F-6 only reads it; a given-seed plane runs the same read once, off the plane's one mesh); κ ≡ 1 and no read on a `White` plane (C-7); LN's refits and the probe's `betas` arm pass no correlation (κ ≡ 1, no shot term). s = 1 / the plane's CFA site share on a debayered plane (R 4, G 2, B 4: a flux-preserving interpolation spreads each site's shot noise over 1/share pixels; derived, no number chosen), else 1. A negative S (the star reads below its ring sky) gives a negative finite SNR, which enters the medians. g = EGAIN × `ATH_CSCL` e⁻ per calibrated unit from the header (PA-22; `ATH_CFNM` ignored; raw domain: EGAIN), no camera table; no gain → the shot term is omitted and the frame line's `snrGain` is null. **HFR** = the linear half-signal radius on (v − B_i): the present pixels within 5 × the ring width (the F the star's ring was read at), taken by distance until their sum reaches half the star's F-5 flux, interpolated linearly by the last pixel's share; NaN without a ring or when that circle holds less than half (so HFR ≤ 5F). **Populations (R11-5).** Every accepted fit carries SNR and HFR, saturated and extended ones included (finite: a saturated star's S is its clipped pixels' sum and its HFR reads against the unclipped model flux); NaN only without a ring (B_i missing, counted in `snr_missing`) or, HFR, under half the signal inside 5F; `snr_median` / `hfr_median` are medians over the fits in the frame statistic (neither `saturated` nor `extended`) with finite values (the flagged stars would lift the frame SNR +10…+24 %). Additive (R11-7): `StarFit.snr` / `.hfr` are not persisted (like `peak5`); `.athf`, `ChannelMeasurement`, the DB, the weights, LN and `MEASURE_VERSION` are unchanged. Lab: the generator writes EGAIN and `ATH_CSCL` from its own gain; the truth SNR (`snr_ap`) is the noiseless stamp summed in the same aperture over σ from the generator's gain, read noise and sky, with N_sky the star's ring (centres in [10F_t, 12F_t] outside every other truth star's 3F_t disc; 299 of the 301 v2 truth files moved, only `snr_ap`). **As judged** (lab cycle `t11-f6` at 1224718a, athenaeum-lab 8532f01; 12706d6c's probe output is byte-identical on the five Phase A inputs): row 6 worst 0.97 % on the 50 bar cells (`neb0.1`; `sharp/*` −0.90…+0.51 %), `n_no_gain` 0; the three nebula cells −16.54 / −75.51 / −60.65 % (= predicted); noise pull 0.92…1.25 on 50 planes (`resampled` f2.0 1.07); every held bar equals t10c-floor's. Recorded limits (README R-OSP-8): the R/B κ under-read (R11-2, the detection owner's), the nebula flux error 3.6–6× σ_S (R11-3, Plan B), the header gain (R11-6), a flagged-only plane (every SNR / HFR NaN), `Unread` (κ 1 and a warn) | median bias vs the truth SNR (on the ring) ≤ 3 % on 50 cells: the 46 `sharp/*` and `background/` flat, grad10, neb0.1, vig20; `n_no_gain` 0. `neb0.5` / `neb3` / `neb3_4096` are REPORTED with their predicted values (−16.5 / −75.5 / −60.7 % against the white-noise truth: there the white truth is not the star's real error, and any noise SNR misses it; R11-4, owner informed 2026-10-12). **Star SNR noise pull** (new row) on `osc/*`, `osc_pairs/*`, `resampled/*`: the robust std of (S_meas − S0)/σ_S, S0 = the generator's noiseless frame through the same debayer / warp, clipped at sat like the noisy frame (R11-8), within [0.85, 1.30] per plane; row 6 on these Mixed families reported ("white truth on a Mixed plane") |
| F-7 | Fit background under nebulosity +0.998σ (`background/neb3_4096`) | F-1's rim background | ≤ 0.3σ under nebulosity; within ±0.1σ on flat fields |
| F-8 | The F1 contradiction (amended 2026-10-20 with Plan A's numbers). F1 (`f1_gc`, the default since the owner's L-24) over-reads on synthetic pairs, more the wider the target: `pairs/main/m4w10` t60 +11.97 % (baseline +11.84 %), slopes +0.1418…+0.4072 %/px² on the 19 `pairs` families, `density_pairs/d600` t3 +4.38 % (after-Plan-A note `docs/superpowers/research/2026-10-20-star-measurement-after-plan-a.md` §5, lab scorecard `t14-final`); the mechanism is the [6F, 8F] median's non-linear response to neighbour light (lab `star-lab/f1v2-findings.txt`). On real data F1 sits on the harness truth (Plan A: pooled median abs(gap) 0.339 %, n 130, lab `t13-real/ln-truth/gap-table.txt`), while psfSel read the other way before Plan A (median gap set 204 mono −6.009 %, run B mono −7.467 %, Horse C11 G −2.758 %; lab `probe-runs-2026-10-07/ln-real-summary.txt`) and has not been read on real data with Plan A's fits. The referee is itself untested where it judges most members: the harness truth's crowded fallback (annulus [5F, 7F], aperture 5F, no growth correction: an F1-like estimator) is validated to FWHM 5 only (`density_pairs/d600` gaps −0.045 / +0.154 / +0.594 %, its t3 soft, 5 of 60 realizations read), yet it judges 50 of 91 set 204 mono and 49 of 82 run B mono members; on the 20 high-ratio members (FWHM ratio ≥ 1.5, all above 5 px, 17 of them more than 0.5 px beyond the validated range) F1 reads LOW (median gap −1.076 % set 204 mono, −1.836 % run B mono) where the synthetic rows read high (baseline research `2026-10-07-star-measurement-baseline.md`, the referee pair); on the set 204 30-member subset the ERP (−3.09 %) and our psfSel at β 4 (−6.51 %) both miss it the same way by more than 3 % (lab `findings.md` §4), so the owner's rule flags that truth. The run B and Horse C11 G truth CSVs are gone (their values survive in `ln-real-members.csv`) | Plan B stages B1–B3 (§5): re-baseline every LN arm on Plan A's fits, synthetic and real, with the ERP a column beside every arm (B1); validate the referee before any real-data verdict: the harness truth in both modes against s_true including FWHM > 5, a growth-corrected crowded mode built in the lab if the fallback over-reads there (R-B3), the synthetic fields' realism against the real members (R-B5), injected pairs on a seeing lever (B2); then the default estimator among F1 as is, psfSel on Plan A's fits and the F1 v2 sky (a candidate only under R-B6), judged at master level (R-B7), the owner's choice with the numbers (B3). No F1 or LN change before B2 closes | the Plan B bars below: the ERP column and the 3 % rule; F-8 referee; field realism; F-8 estimator |
| F-9 | Two `c` = 1 floors, no measured case at either (amended 2026-10-20, R-B1: F-9 names both). (i) **Measure's fit-call floor**: under 5 qualifying ratios (`CORRECTION_MIN_RATIOS`, `star_measure/photometry.rs`) a plane's `c` is None (= 1) with one `warn!` per plane (built and ruled, `docs/stacking/README.md` R-OSP-7). A plane's weight ∝ c² (PSFSW and PSFSNR both sum `signal`, `stacking/psf_signal.rs`; derived), so a floored plane among undersampled-OSC peers at `c` 1.9–2.7 (after-Plan-A note §4.1) gets 0.14–0.28 of the weight its own `c` would give (1/c², derived), and psfSel's s moves 1 : 1 with its `c`. (ii) **LN F1's growth floor**: 1–4 qualifying pairs (the 5F circle present on both frames) keep their growth factor with a `warn!` (`GROWTH_PAIRS_WARN_BELOW` 5, `stacking/ln/scale.rs`), 0 pairs keep the 2F ratio (factor 1) with a `warn!`: the LN model fixes cycle's owed question "fall back to 1 below 5 pairs?". A member under 20 matched pairs (`MIN_MATCHES`) is excluded first, so (ii) needs ≥ 20 pairs at 2F and < 5 at 5F; set 204 centre crops keep 100 growth pairs at 1500 × 1000 and 18–42 at 600 × 400 (`docs/superpowers/open-items.md`, F1 on small frames); real small sensors, binning and large dithers are unmeasured | B4 (§5): each floor is decided on lab cases that actually reach it. (i) sparse and undersampled planes (crops of set 204 OSC raw planes cut below 5 qualifiers; sparse lab fields); arms: today's `c` = 1, the plane's own median over its 1–4 ratios, the median kept `c` (F-11) of the same channel over the group's other members (a candidate, read from the owner's kept values, not a second estimate). (ii) crops, binned frames, masked-overlap corners; arms: today's factor kept, factor 1. The numbers and the verdict go to the owner (R-B1), which closes the LN cycle's owed question; the product changes only on the owner's word | the Plan B bars below: F-9 |
| F-10 | One background (2026-10-11; research RB1/RB2, design studies D1/D2, the ERP comparison on the trailed frames). One star met five estimates of sky and noise, none owned. On synthetic nebulae the filtered plane's measured σ_f is the nebula's own (σ_f / noise 1.75 / 6.7 / 32.5 / 30.4 on `neb0.1/0.5/3/3_4096`), so the seeds lose stars (recall vs flat 0.92 / 0.69 / 0.40; every `neb3` star below Fisher 100); a sky sample's error grows with its distance from the star (`neb3` p90: fit b 2.6σ < [6, 8]F 4.4σ < [10, 12]F 5.5σ < mesh 10.1σ < plane median 15.3σ); `c` couples 1:1 to its far ring and ≈ 0.1 to the fit's b, so an SNR built on the fit's b would inherit its 0.48σ p90 noise (47 % of the median star over 5F); the field width reads 8.2–8.8 px on M42 against a fitted 1.7–2.4 px (injected stars found 42/60, 43/62, 38/62); the trailed L 0010 / L 0011 hit the seed cap and keep 947 / 786 trail fragments (clean median 2458), `c` 4.48 / 6.89, weight 0.024 / 0.036 of the group max (the ERP: 2e-9 PSF signal weight) | **As built (Task 10b, 2026-10-12; every ruling with its numbers in `docs/stacking/README.md` R-OSP-7b).** **Detection.** σ_f is PROPAGATED: the 128-px mesh's per-cell clipped σ (its noise map) × the plane's filtered-noise gain, K = 6 unchanged; the mesh is the plane's one map, computed once and shared with N* (so on a masked plane N* reads the masked mesh: an absent pixel is not sky; D-1). The gain is 1 on a plane whose caller states its pixels' noise `White` (sensor pixels, raw or calibrated: shot, read and dark noise are per pixel and calibration is per-pixel arithmetic, so every lag-≠0 covariance is 0; C-7), and it is READ once per plane, from the noise's own autocovariance on the 5×5 high-pass residual, on a `Mixed` (debayered, resampled, warped, integrated) or `Unknown` plane (`star_measure::PixelNoise`, default `Unknown`: gain 1 on mixed pixels floods false seeds, a read on white pixels only costs nebula recall; C-10). The stacking Measure stage states `White` for a calibrated mono plane and `Mixed` for a debayered one; integrate and drizzle state `Mixed`. Two knot tests, thresholds from the PSF and the noise: the bounds are a pixel-centred Moffat β 1.5 through the same kernels (plain 0.972, zero-sum 0.935 at F 2.6–8; 0.911 / 0.885 at F 1.2), read at the field's star-width ENVELOPE F_env = F_field + 3.09·s, s the TRUE spread of the stars' widths (E4n: the mirrored lower-half spread of the readable reads in [0.715·F_field, F_field] with the reads' own error e(ν) = √((0.899/ν)² + 0.0208²) removed in quadrature; A2-1…A2-3: half the real stars are broader than the mode); σ_r = √(1 + r² − 2rρ)/ν, z = 3.09; a seed whose plain f(2F)/f(F) exceeds its bound + 3.09σ_r AND whose zero-sum F response is below K is structure and dropped; a seed whose zero-sum ratio exceeds its bound + 3.09σ_r is `extended` (kept as a seed; out of the β sample, `c`'s qualifiers AND the frame statistic — the saturation flag's treatment, P3); a seed whose peak reaches the plateau-clip level (0.9 of Task 8's `SaturationPlane` range, the owned quantity — no third maximum) is exempt, and whether that near-clip class is right is Task 10c's (F-3). The β sample takes only seeds whose verdict fit passes the fit's centre and region tests. **The field width** is the HALF-SAMPLE MODE of the half-maximum reads of every readable candidate (its brightest pixel ≥ 10 σ_rim above the read's own box rim), clamped to [1.5, 24] px, with no saturation test (F25 stands): every contaminant of a read — a clipped core, a blend, a knot, a sky bump — is wider than the PSF while the stars share one width (A5; M42 8.2–8.8 → 1.7–1.95 px). **Trails.** NO per-star streak refusal (a fitted-ecc line at 0.8 refuses 2–32 % of clean real fits; an elongated star is a real star, measured as such; Q1). The fit's centre test is its own half-maximum contour: a fit is the seed's star when the seed lies inside (u/α_x)² + (v/α_y)² ≤ 2^(1/β) − 1 in the fit's axes (a round FWHM-3 star keeps the former 1.5 px; a fixed tolerance refused elongated stars by the back door; A4-2). The trailed verdict is frame-level only, from the refusal structure — a fit cannot contain a star longer than its region: the trail share = (centreMoved + regionTooSmall + centreAtRim) / judged seeds, and a plane with at least 100 judged seeds and a share > 0.5 is trailed — the MAJORITY rule, a physical criterion (Q5, A4-2; the line derived from the clean frames' distribution, median + 5·1.4826·MAD, did not separate the trailed frames from the elongated G 0003, so it was not built). Like F-1's defocused verdict, every fit the plane would accept is refused `trailed`, `c` is None, the weight inputs are empty; measure path only (LN's refits never see it). ecc is reported beside it, never a gate. Frame classes (rendered before any reading): `trailed` (streaks, no measurable PSF: L 0010, L 0011) is the verdict's target; `elongated` (measurable short dashes: G 0003, ecc ≈ 0.65) gets no verdict and no per-star refusal, is measured as is with its ecc reported, and leaves every clean pooled row — rejecting it is frame selection's job (an ecc gate), not measurement's. Recorded miss: hook-shaped trails whose dwell cores fit (`trails/` H 0029, share 0.445) get no verdict (Q6). **Per-star sky (one definition per scale).** The fit keeps its free local b (fixing it at any ring value triples the nebula FWHM scatter and fails 2–4 F-5 cells: rejected). ONE aperture-scale sky B_i per accepted star on R6's geometry (the plain mean over [10F, 12F] outside every seed's 3F disc, F the frame FWHM), computed once and stored with σ_i (the RMS of (v − B_i) over the same pixels) and their count, not persisted; `c`'s growth sums (v − B_i), byte-identical to R6's by construction; Task 11's SNR and HFR read B_i and σ_i, none re-estimates; b − B_i is stored as a diagnostic. LN keeps each side's own ring width: the pair-common width max(F_R, F_T) failed the nebula-pair bar as the per-side width did and added misses of its own (P.3 = C-9). Recorded limits (with SE, not bars): the nebula `c` bias on R6 (`neb3` 0.880, `neb3_4096` 1.129) and its crowded form (density × `neb3` d200 / d600 / d1000: 1.058 / 1.133 / 1.087 as built, 1.058 / 1.135 / 1.069 before); the real halo `c` carries beyond the fitted model on injected stars (+4–6 % on clean frames, 10b-L); LN's psfSel on nebula-sky pairs and warped targets, unchanged by this row (13 of 34 cells over; Plan B's before, C-9); the mesh noise map reads ≈ 6× the true noise on `neb3` (structure inside a cell counts as noise), so heavy-nebula recall stays ≈ 0.54 until a later owner takes it (C-7); the gain read keeps its nebula-texture leak on debayered colour planes (Task 11, C-7) | the F-10 bars below |
| F-11 | `c` cannot be checked on a real run (2026-10-20, after-Plan-A note §7 (a)): per plane it is neither persisted (`stacking/measure.rs`, `ChannelMeasurement`) nor logged at a level a run keeps (`star_measure`'s `"aperture correction measured"` and LN's `"ln scale: per-side ring"` are `debug`; Measure's `"frame plane measured"` carries no `c`); the one real read (set 204 run 7, LN's own `c`: OSC p0 1.00, p1 1.13, p2 1.12, mono 1.13) came from a debug log | B0 (R-B2): per plane `c`, its SE and its qualifier count as additive `Option`s on `ChannelMeasurement` in the cached `metrics` payload, skipped when None like `beta_own` (camelCase in the payload, from the existing names: `apertureCorrection`, `apertureSe`, `apertureStars`); `MEASURE_VERSION` stays 4, no stage hash moves, an old payload decodes unchanged (the fields None until the set is re-measured); one `info!` per (group, channel) at the end of Measure (planes, `c` min / median / max, planes at floor (i)), the per-plane line staying `debug` (the logging rule); LN's per-side `c`, F1's growth factor and `ln_growth_pairs` go to the cheapest kept place the plan finds, with no new log field name | the Plan B bars below: F-11 |
| F-12 | `c`'s sample (2026-10-20, after-Plan-A note §7 (b)–(d)): the 64 brightest qualifiers (`CORRECTION_STARS`) with no point-source test. Hot pixels qualify on an uncorrected R plane: raw 0013 p0 `c` 1.334 against 2.097 / 2.074 on 0004 / 0007, 36 of its 64 `c` stars hot pixels, the same stars' 0004 → 0013 ratio 0.617 on p0 against 1.007 / 0.970 on p1 / p2 (R weight × 0.40); stacking meets it on a refused hot-pixel map (no `ATH_CHPX`) or a raw-mode measurement. The brightest are not representative on undersampled R / B: raw 0004 p0 aperture / model 1.57 (top 64) → about 1.2 (rank 500–2000), G flat (1.23 → 1.18); Task 10's RF7: the LN-like ratio over all pairs −4.7 % ± 1.5 %, bright pairs +0.2 %, which `docs/stacking/README.md` R-OSP-7 reads as likely the far ring's additive bias (an estimator artefact, not a profile property, until shown otherwise). The sharp-R ratio on the probe's own β (mosaic 0176 → 0206 p0 0.863 after `c` against about 0.985 on G / B) is unverified at the group β until F-11 | B4 (R-B8): `c`'s qualifiers gain a point-source screen (its design is the controller's), and its sample is stratified across brightness instead of the top 64 (bright stars are not representative); a measured value changes, so `MEASURE_VERSION` 4 → 5, one bump for Plan B, the once-only re-measure of every set named in the release note (the kind of §7's first risk) | the Plan B bars below: F-12 |
| F-13 | LN still estimates three of Measure's quantities a second time (2026-10-20; the owned-quantities table below). **Seeds**: the reference side always runs `register::detect::detect_stars`, the target side reads Measure's fits or detection, the mismatch corrected by a per-group, per-channel `k` (C-14: the seeds path +0.666 % median, +1.401 % max against detection; `stacking/ln/calibration.rs`). **Saturation**: four definitions beside Measure's flag: Register's 0.95 (`register/detect.rs`, also LN's warped guard), L-14's 0.75 × the side's maximum, L-23's reference-frames map and `unverdicted_clip_range`. **Nebula sky** (C-9): psfSel on the nebula-sky pairs is 13 of 34 cells over P.1 (worst `neb3` g t45 −15.88 pp, `neb3` b25mix t25 −12.28 pp) and shifted / warped targets at t25 read +1.91…+1.96 pp against ±1.2 (lab `t10b-onebg/a1c/p1-table.txt`, at 4c95b6b4, before Tasks 10c–14b), the stated causes LN's own tile sky on nebula (`ln/background.rs`) and structure inside 10F. **R11-3**: the nebula flux error at 3.6–6 × σ_S (pull `neb0.5` 3.63, `neb3` 6.04), "the annulus method's own limit" (R-OSP-8) | B5 (R-B9): if B3 keeps a module-fed estimator (psfSel, or F1 v2 through the module's ring), LN's seeds on both sides come from the module, which retires `k` (C-14) and `unverdicted_clip_range` and leaves LN's stars one saturation definition, the module's `saturated` flag (the plan maps each of the other three to it or names why it stays a selection screen); C-9 in scope on P.1 / P.2; R11-3 stays a recorded limit unless B5 finds its owner | the Plan B bars below: F-13 |

**F-10 bars (fixed 2026-10-11, before any 10b code; revised the same day after the plan review, still before any code;
one joint lab cycle; baselines in plan Task 10b).** Every bar is derived from data or from the PSF and the noise
before the cycle; a bar that cannot be is REPORTED with its reason, never kept as an underived number (R2).
- F-0…F-5 and F-7 re-read unchanged in the same cycle; F-5's four psfSel/spread bars verbatim.
- Detection: recall on the `seeds` path `neb0.5` and `neb3` ≥ the design arm's (D2: 0.777, 0.539) minus 2 SE of the
  recall at the cell's n truth stars (0.743, 0.499 at v2 i0's n = 629; recomputed at the scorecard cell's n before the
  run) with ≤ 3 false seeds per Mpx; `halo` ≤ 5 false seeds (the arm's 2 + 2 Poisson SE); flat, gradient, sharp,
  mixture, twins, artefacts, coma, fieldpsf, shape and saturated families within −0.005 of the baseline recall with
  false seeds ≤ the baseline; twins and artefacts ≤ 0.5 `nothing` per Mpx (R-OSP-2); doubles and `d1000` ≥ baseline
  − 0.01, density pairs ≥ baseline − 0.03; propagated / measured σ_f 1.00 ± 0.04 on flat and twins. A flat-family
  no-regression move re-sweeps K by PA-2. The `extended` flag moves the frame FWHM, ecc and `c` on the clean families
  (flat, sharp, density, twins) within SE only.
- Knots (new family): accepted unflagged fits on truth knots, per (size, amplitude) bin of FWHM ≥ 1.5F, at most the
  knot tests' own expected pass rate for a Gaussian knot of that size and SNR (from the bounds and the noise) + 2
  binomial SE; bins below 1.5F reported (the tests are blind there); truth-star recall ≥ baseline − 0.01. Density ×
  nebula (new): recall ≥ baseline − 0.03 with ≤ 3 false seeds per Mpx, `c` from ≥ 5 stars on every plane.
- Field width: on each M42 frame, injected stars found ≥ 99 % of those above the detection threshold at the frame's
  noise (the raw-mode truth rule below), the plain found share reported; on the other clean injected frames (G 0001,
  G 0002, L 0000, L 0008 — not the elongated G 0003) ≥ the baseline; sharp and blur families' recall within SE of the
  baseline.
- Trails: on L 0010 and L 0011 (class `trailed`) accepted stars ≤ 1 % of the clean siblings' median, PSF signal
  weight ≤ 0.01 × the clean median, `c` = None (< 5 genuine qualifiers); the trailed verdict on each wind-shaken
  `trails/` frame equals its rendered class; no verdict on every clean frame and on the `elongated` G 0003 (the verdict
  never fires there, so it changes no clean fit); no star lost on `shape` (e ≤ 0.7), `coma` and `fieldpsf` (recall ≥
  baseline − 0.005; no per-star refusal exists). The `elongated` G 0003: refusal share and ecc reported. Before any
  verdict code, the refusal share's line is derived from the clean frames' distribution (A4 step 2a) and must
  separate the trailed frames from G 0003 and every clean frame; failing that the step stops with the table.
- Per-star sky: σ_i / truth noise median 1.00 ± 0.05 on smooth sky (flat, gradients, `neb0.1`, sharp, density); `c`
  byte-identical where the seeds and F are unchanged (B_i is R6's ring, read once); B_i vs `sky_local` and b − B_i
  reported.
- LN: psfSel on nebula-sky pairs, as the change against the flat control on the same star field, within ±1.2 %
  (single-profile targets) and ±2 % (blur targets) on `neb0.5`, the same plus 2 SE on `neb3`; a cell refused for
  < 20 matched stars is n/a, not a miss; warped targets within their unwarped twin's tolerance. Injected stars with
  the frame's own PSF: row 6 within 2 % (+ 2 SE of `c`) of the F-5 cycle's value per clean frame; the halo excess
  is a recorded limit.
- Real sky (no truth; the F-5 cycle's numbers, re-measured on the cycle's own machine before it): `c`'s
  frame-to-frame sd ≤ before × (1 + 2/√(2(n − 1))) on a dataset of n ≥ 12 clean frames (1.43 × at n = 12, M42's
  calibrated set; the sd's own relative SE is 1/√(2(n − 1))); a dataset with fewer clean frames is reported; seeds per
  Mpx per clean frame reported (no derivation before the cycle; the seed flood it would catch is the trails bars'); the
  M42 frames' β REPORTED with its mechanism split (knot / extended seeds vs near-clip class-B stars); the bar is Task
  10c's.
- Cost: the m42 corpus Measure batch ≤ 1.2 × the same machine's wall at the F-5 cycle's binary, re-measured before the
  cycle (§3 cost; the dev Mac's 223.1 s is not comparable on another machine; read here first, confirmed at the
  checkpoint).

**F-10 as judged (2026-10-12; joint cycle `t10b-onebg`, third run at athenaeum 4c95b6b4, athenaeum-lab 4f9e21e).
Every bar is met or reported, no STOP is open; the cell-by-cell table is `docs/stacking/README.md` R-OSP-7b.** The
cycle ran three times: at 6a1addc6 (STOPs: `neb3` recall, LN's nebula pairs, the frame statistic), at 7fd911ab (gain 1
on unmixed planes; STOP: resampled mono planes flooded with 1 147 / 1 047 false seeds) and at 4c95b6b4 (the caller's
`PixelNoise`: 0 / 0). Judging rules ruled during the cycle, none of them moving a bar: D.1 is read on D2's own quantity,
found over all 629 truth stars (C-1); a zero-tolerance "false ≤ base" at base 0 is read at R-OSP-2's 0.5 per Mpx (C-3);
a population move is set aside only where the same truth stars measure better or unchanged (C-2), "unchanged" being
within max(2 SE, 1 % of the base mean) (C-5), with the WORSE count printed beside its binomial allowance (C-6c) and the
truth, not the base, as the referee on a lab family (C-11); the trails line is Q5's majority rule (above).

| Bars | As judged |
| ---- | ---- |
| F-0…F-5, F-7 | F-0 identity 885 of 885 rows; F-5 spread 0.76 % / 1.41 %, psfSel blur pairs +1.72 % / +1.19 %, pure pairs 0 of 70 over, mixtures 0 of 20; no-regression against the F-5 cycle and cumulative against t01c: 0 moved, 0 missed, 0 lost; legacy control 0 mismatches |
| Detection | recall `neb0.5` 489 / 629 = 0.777 and `neb3` 339 / 629 = 0.539 (floors 0.743 / 0.499; before 0.591 / 0.343), false 1.67 / 2.38 per Mpx; `halo` false seeds 2 (before 256); every flat-family line held, `resampled/*` included; twins and artefacts 0 `nothing` per Mpx; doubles, `d1000` and the density pairs within their lines; flat σ_f propagated / measured 0.973; `neb3_4096` completeness 0.719 (before 0.447), reported |
| Knots; density × nebula | accepted unflagged knots of FWHM ≥ 2F pooled 1 / 1 / 2 of 216 on flat / `neb0.5` / `neb3` (allowance 4 / 3 / 4), no bin over its allowance; truth-star completeness 0.987 / 0.928 / 0.655 (before 0.978 / 0.714 / 0.424); density × nebula completeness 0.607–0.862 on the six planes (before 0.367–0.681), `c` from 64 stars on each |
| Field width | M42 injected stars above the detection threshold found 45 of 45, 41 of 41, 45 of 45; the clean G / L inject frames at or above their before (L 0000 51 → 60 of 61, L 0008 51 → 59 of 60) |
| Trails | L 0010 / L 0011 trailed (share 0.916 / 0.933): 0 accepted fits, `c` None; six of the seven `trails/` frames match their class (H 0029, 0.445, the recorded miss); no verdict on the ten clean siblings (0.005–0.111) or on G 0003 (0.176); the synthetic long-trail family crosses 0.5 at a streak of 5.5–5.7 F, about the fit region's radius (reported) |
| Per-star sky | σ_i / truth noise median within 1.00 ± 0.05 on all 55 smooth-sky rows |
| LN | per-side kept (P.3): nebula-sky pairs 13 of 34 cells over the bar (pair-common 15), shifted / warped targets 3 cells at t25 (+1.91…+1.96 pp), both equal to the before cell for cell; reported as Plan B's before (C-9) |
| Real sky, cost | injected-star row 6 within 2 % + 2 SE_c on every clean frame (Δ −0.025…+0.009); M42's 12-frame `c` sd 0.67 % (bar 1.46 %, before 1.02 %); M42 frame β 31.8 / 2.81 / 3.38 reported (Task 10c's bar); Measure batch 74.7 s (bar 159.8 s, before 133.5 s) |

Same-star (1.31 M paired truth stars): 645 cells, 46 better, 580 unchanged, 19 worse (allowance 23). Accepted with
their numbers, never as a bar move: the seed box and the faint-bin noise at the corrected field width (C-5 (b), C-6,
C-6c), the frame statistic's population and weights (C-8, C-8b) and the corrected width's cost on a heavy-wing shape
(C-12d: +0.003 ecc on bright round β 2.5 stars at the true width — a later fit-design task's first datum).

**Plan B bars (fixed 2026-10-20, before any Plan B measurement, from the Plan B design read and rulings
R-B1…R-B10).** "The after note" is `docs/superpowers/research/2026-10-20-star-measurement-after-plan-a.md`; lab paths
are athenaeum-lab's; the scorecard is `t14-final` (sha256 `ac2c6914…`). §3's rules hold: a missed bar goes to the owner
with its numbers and is never moved; a bar that cannot be derived here is reported with its reason.
- **The ERP column (the owner's rule; R-B4).** Every LN table, synthetic and real, prints the ERP beside EVERY arm
  (`psfOff`, `psfSel`, `f1_2`, `f1_3`, `f1_gc` and the F1 v2 lab port), not on `sLib` rows only as today
  (`starlab/scorecard.py` `rows_ln`; a lab change). `erp_audit.py` gains an LN freshness audit: each `ln.csv` linked by
  sha256 to the frames it read, as the star units are (the 24 synthetic `ln.csv` are unaudited today and predate the
  2026-10-08 header augment of their frames); an `ln.csv` re-runs only where its frames changed. On real data the
  existing ERP LN is always printed (set 204 30-member subset −3.09 % / −1.92 %/px², Horse C11 G −2.44 % / slope
  −0.06; lab `findings.md` §4); where none exists (set 204 full mono and OSC, run B mono and OSC, the nebula-sky and
  the injected pairs) the column reads "not run" until the owner's OK. The realistic-field family below is new frames,
  so the ERP runs on it.
- **The 3 % rule (the owner's).** A cell where an arm of ours and the ERP both miss the truth by more than 3 % in the
  same direction marks that truth cell suspect: it is reported, its truth re-checked (on synthetic frames by B2's
  harness-truth read of the same cell; on real data by B2's validation of the mode that judges it), and until then it
  is not the referee for that cell.

*F-8 referee (B2; R-B3).*
- The harness truth (`harness/acceptance/grid-report.py`: F from the half-maximum area of about 100 isolated stars,
  aperture max(3F, 5 px), sky the 3σ-clipped mean over [10F, 12F], growth median flux(3F) / flux(8F) per frame,
  isolation 14F; under 10 such stars the crowded fallback: annulus [5F, 7F], aperture 5F, no growth, isolation 9F) is
  read in BOTH modes, the mode forced, on every cell of `pairs/main`, `pairs/iso`, `density_pairs` and `truthcheck`
  where the mode finds its stars, against s_true, the targets beyond FWHM 5 included (`pairs` t60 at 6.0 px,
  `truthcheck` t3 / t4 at 6 / 7.5 px). Bars: the normal mode within ±0.3 % per cell, the crowded within ±0.8 % per cell
  (validated so far: normal max abs(gap) 0.287 %; crowded −0.045 / +0.154 / +0.594 % at FWHM 3–5 only, its stated
  systematic +0.2 % at β 3 and +0.6…+0.8 % at β 2.5).
- A crowded cell beyond FWHM 5 outside ±0.8 % is the F1-like over-read R-B3 names: the lab builds a growth-corrected
  crowded mode and re-reads the same cells on the same bars, and only then does B3 read real data. A normal-mode cell
  outside ±0.3 % has its systematic reported with the cells, and every real member that mode judges is flagged. A real
  member that only a failing mode can judge is reported, never a referee cell.
- Injected pairs on a seeing lever: pairs of real frames of different seeing carrying stars injected at a known s_inj
  with the frame's own PSF stamp (`inject.py`'s `_fwhm_pass` stamp; a lab change), spanning the real members' FWHM
  range (set 204 mono 2.64–5.92 px against a 3.385 px reference): truth by construction, no bar of their own; the
  injected stars' own aperture sum over the injection printed as the check (today 0.95–1.03 on all 18 inject planes).

*Field realism (B2; R-B5).*
- `annulusCrowding` (L-26: per member, the median over its pairs of the larger side's fraction of annulus pixels above
  the annulus median + 3σ) and the halo `c` excess (`c` − 1, F-5's 5F growth over the model flux) of every synthetic
  pair family, against the real members. Today (baseline research): `annulusCrowding` set 204 mono 0.0757–0.0916
  (median 0.0847), run B mono median 0.0928, against `pairs/main/m4w10` t45 / t60 0.0618 / 0.0707 and `truthcheck`
  0.0016–0.0024; the real `c` comes from F-11 (B0) and B1. Medians are compared, the per-target-FWHM values printed
  (the synthetic wing is a β-2 Moffat at 4 × the core FWHM, so it scales with the seeing, `synth.py`; a real halo does
  not). A family differs when its median lies outside the real members' range (min…max over every real member B1
  reads).
- If no pair family lies inside that range on both quantities, ONE realistic-field pair family is built: a halo of
  fixed width in pixels sized from the real `c` excess, the real annulus crowding, the real seeing range, a reference
  stacked from the best frames (as the `*mix` families, `ref_nframes` 20), faint stars on the `main` luminosity
  function (fmin 0.05, slope 0.8), cosmic rays and hot pixels. The ERP runs on it; the harness truth is validated on it
  on the referee bars above.

*F-8 estimator (B3; R-B6, R-B7).*
- Every candidate on the same rows: synthetic s / s_true per cell and the slope on F_t² − F_r² (the 19 `pairs`,
  `density_pairs`, `osc_pairs`, `truthcheck`, the injected pairs, the realistic family if built), the ERP beside; real
  median gap and slope against the validated truth per dataset (set 204 mono and OSC, run B mono and OSC, Horse C11 G),
  the ERP beside where run.
- The synthetic bar a new default meets is F-5's psfSel bar at every target: within ±1.2 % on the Moffat and Gaussian
  pairs, ±2 % on the blur pairs. Keeping F1 as is misses it (+11.97 % at `pairs/main/m4w10` t60); that is the owner's
  choice, the miss recorded beside B2's field-realism verdict.
- The F1 v2 sky is a candidate only if (1) the lab port's pooled median abs(gap) against the validated truth over the
  real members is below F1's (before, as median gaps, lab `f1v2-findings.txt`: set 204 −1.01 % against F1's
  −0.64 %, Horse C11 G +0.08 % against −0.13 %; with the L-23 map −0.79 % against −0.22 %) and (2) its sky is robust
  to cosmic rays: on each of three cosmic-ray seeds its `pairs/main` t60 cell moves by ≤ 0.53 pp (the median sky's
  worst; the plain mean moved +3.22 pp on one seed of three). Only then is the module ring API change in scope
  (`ring_sky` taking a caller F, max(F_R, F_T), and the union mask of both fit lists).
- Master level (R-B7), for any change of the default: whole groups set 204 mono and OSC, run B mono and OSC, Horse C11
  L and G, stacked before and after with the reference pinned (`ReferenceMode::Manual`, the before run's pick): master
  median / MAD / noise / FWHM within 0.1 / 1 / 2 / 1 %; the LN row re-based (R13-4: (s_B / s_A) ÷ (phot_B / phot_A) − 1)
  within ±0.5 %; weights ρ ≥ 0.9. An estimator change moves s by design, so a miss of the LN row goes to the owner with
  each member's gap against the validated truth beside it, before and after; the bar is not moved.

*F-9 (B4; R-B1).*
- A case counts only when the product reaches the floor on it: for (ii) `ln_growth_pairs` 1–4 or the 0-pair warn, with
  ≥ 20 matched pairs; for (i) the "fewer than 5 isolated unsaturated stars" warn. The reference for a crop or a binned
  copy of a real frame is the same plane or pair uncropped (its `c` from ≥ 5 qualifiers, kept by F-11; F1's factor
  from its 100 growth pairs); for a synthetic case, the truth (row 6's captured fraction for (i), s_true for (ii)).
- Verdict per floor: the arm with the smaller median abs(error) against the reference over the counted cases, when
  the paired difference exceeds 2 SE; within 2 SE the verdict is "indistinguishable" and today's behaviour stays. A
  floor no case reaches is reported "not reached", with the lowest count the cases reached. For (i) the floored
  plane's weight relative to its group's other planes and its psfSel s are printed per case. Each verdict goes to the
  owner.

*F-11 (B0; R-B2).*
- Identity: probe outputs identical (`run_probe --verify`, `probe_parity`), every `.athf` byte-identical, every stage
  hash unchanged, an old payload decodes unchanged (the new fields None). The kept values equal the debug `"aperture
  correction measured"` values plane by plane on one set 204 run and one Horse C11 run. The per-(group, channel)
  `info!` uses the dictionary's names (`planes`, `count`, `aperture_correction`) and adds only a min / max pair in the
  `beta_own_min` / `beta_own_max` form, entered in the logging spec's dictionary in the same change.

*F-12 (B4; R-B8).*
- The × 0.40 R case: on raw 0013 p0 the 36 hot-pixel `c` stars are refused, and the same stars' 0004 → 0013 ratio on
  p0 lies within 3 % of the mean of p1's and p2's (today 0.617 against 1.007 / 0.970; 3 % is the agreement the same
  real stars reach on G / B after `c`, the after note §4.1).
- Inject row 6 (`manifests/inject.json`, the frame's own `c`): on the clean mono frames Δ against Plan A's value per
  frame ≤ 2 % + 2 SE_c (I.1's stability row); on the undersampled OSC planes, where an injected wingless Moffat-4 star
  reads × `c` (1.81–2.34 today, I.1's recorded limit extended to OSC), row 6 is printed beside its `c`-free ratio and
  `c` (a lab change) and reported.
- Set 204 OSC as a whole group (the reference pinned): weights ρ ≥ 0.9 against Plan A's run, each larger mover tied to
  its cause; the master rows reported.
- Lab truth: F-5's bars hold (the spread across profile families ≤ 2 % at F 2.6 and F 8.0; psfSel within ±1.2 % /
  ±2 %), and every row 6 cell on the lab families, whose true captured fraction does not depend on brightness and where
  Plan A's `c` already reads 0.9865–1.0010 (the after note, row 6), stays within 2 SE of its Plan A median: a drop
  there is the far ring's additive bias on faint stars (R-OSP-7, RF7) entering the sample, an estimator artefact, and
  a STOP. The qualifiers' growth / model ratios per brightness tercile are printed against truth beside it.
- Reported: the screen's refusals on the clean lab families counted against truth, per family (a truth star refused is
  the screen's defect); the planes at floor (i) before and after; the sharp-R ratio (mosaic 0176 → 0206 p0) at the
  group β, from F-11.
- `MEASURE_VERSION` 5 moves every stage hash, so B6 re-reads F-8's master rows on top of B3's after-run.

*F-13 (B5; R-B9).*
- P.1 (Plan A plan, row P.1): psfSel, and the default estimator if it is another, on the nebula-sky pairs (`iso/m4`,
  `iso/g`, `iso/blur25mix` × t25 / t45 / t60 × `neb0.5`, `neb3`; `manifests/t10b-l.json`, on the runner): Δ against the
  flat control on the same star field within ±1.2 % (m4, g) / ±2 % (blur25mix) on `neb0.5`, the same plus 2 SE on
  `neb3`; a cell refused for < 20 matched stars is n/a; before 13 of 34 cells over. P.2: warped targets within their
  unwarped twin's tolerance; before 3 cells at t25 over.
- Module seeds (if in scope): no synthetic LN cell worse against truth than B3's value beyond 2 SE; F-8's master rows
  on the real groups; structural pins that LN's stars carry the module's `saturated` flag and that `k` and
  `unverdicted_clip_range` are gone.
- R11-3: reported with its pulls, or its owner named with a bar of its own declared before it is measured.

**Owned quantities (F-10, 2026-10-11).** Every quantity more than one consumer reads has ONE owner and a truth bar
at its consumers' scale; a consumer reads it, never re-estimates it; a fix round that adds a second estimate is a
STOP and a re-audit of that quantity. One current owner per row; earlier tasks are provenance, in parentheses.

| Quantity | Owner (task) | Consumers | Truth bar |
| ---- | ---- | ---- | ---- |
| plane sky median (`SkyLevel`) | 8 (from 2, PA-21) | the saturation test's bg; Analysis `background` (Plan C) | F-3 row 10 |
| plane noise σ (`NoiseModel`: MRS, N*) | 8 (from 6a) | the saturation threshold, the hot-pixel rule, the weights' σ_n, Analysis `noise` (Plan C) | F-3 row 10 |
| the 128-px mesh map M (the plane's one background map, computed ONCE per plane and shared) | 6a (shared by 10b, Q4) | the matched filter's background and σ_f's per-cell noise map (detection), the knot metrics (through the filtered plane), the plane noise's N* (`domain::background_residual`; on a masked plane the masked mesh, D-1); the saturation test reads `SkyLevel.median`, not the mesh | F-1 fitted/seeded; F-10 detection |
| detection noise σ_f(x) (its gain from the caller's `PixelNoise` statement: 1 on `White`, read on `Mixed` / `Unknown`) | 10b | the matched filter's threshold, the knot tests' z | F-10 detection |
| seeds (structure drop, `extended`) | 10b (from 6a) | the field width, every fit, the β sample, `c`'s isolation and the ring's seed mask; LN's seeds on both sides if Plan B B5 moves them (F-13: `k`, C-14, retired) | F-1 fitted/seeded; F-10 detection, knots, trails; F-13 |
| field width F_field | 10b (from 6a, PA-17) | the kernel, the width-start box ⌈2·F_field⌉, `c`'s isolation 6·F_field, the knot kernels and (through the envelope F_env = F_field + 3.09·s) the knot bounds, `field_width_of` for given seeds | F-10 field width; F-1 |
| frame FWHM F (the unsaturated, unextended statistic; in stacking fitted at the group β, on the single-frame path at the frame's own β) | 11b in stacking (from 9, F-4; 8) | B_i's ring [10F, 12F], `c`'s 5F growth radius, LN's `ring_fwhm`, the weights' FWHM term (compared across the group) | F-4; 11b M42 3c / 3b (recorded limits, R-OSP-9) |
| fit-scale sky b (free) | 6b (F-7) | the model only (A, widths, β) | F-7 |
| aperture-scale sky B_i | 10b | `c`'s growth sums, F-6's S and HFR, the b − B_i diagnostic | F-5; F-10 per-star sky |
| per-star noise σ_i (with its pixel count) | 10b | F-6's σ_sky and N_sky | F-10 per-star sky |
| plane noise correlation ρ (`seeds::NoiseCorrelation`: C/C(0) at lags up to 2 from the plane's ONE noise read on a `Mixed` / `Unknown` plane; none on `White`) | 10b (the read; Task 11 exposed it and runs the same read once for given seeds, off the plane's one mesh) | the detection noise σ_f's gain; F-6's κ(A) | F-10 detection; F-6 noise pull (R/B under-read 12–21 %, R11-2) |
| flag `saturated` | 8 (F-3) | the β sample, the frame statistic, `c`'s qualifiers, `.athf`, the weights; LN's stars if Plan B B5 moves LN's seeds (F-13: their one saturation definition) | F-3 row 10; F-13 |
| flag `saturated`, its near-clip class (clause 3) | 10c (from 8) | the β sample, the frame statistic, `c`'s qualifiers, the flagged fit's exclusion (level norm), the multi-plane rule, `stars_unsaturated`, `.athf`, F-6's frame medians (`snr_median` / `hfr_median`, R11-5) | F-3 row 10, C-11; 10c M42 β B1 / B3 (B2 not a bar: 11b) |
| the plane's clip floor (`SaturationPlane::clip_floor`, with its tie count and evidence count; not persisted) | 10c | clause 3 only (the knot exemption keeps the fixed 0.9·range band, a superset); `PlaneMeasurement.clip_floor` / `clip_evidence` and the probe's `clipFloor` / `clipEvidence` report it | F-3 row 10 FP ≤ base + 0.02 pp; C-11 ≤ +0.002 |
| flag `betaAtCap` | 9 (F-4) | `.athf` and the probe (a diagnostic; no statistic excludes it); never set at the group β (a fixed β): stacking reports `beta_own_at_cap` beside `beta_own` (11b) | F-4 |
| flag `extended`; refusal `trailed` | 10b | the β sample, the frame statistic, `c`'s qualifiers, `.athf`, `stars_refused`, the weights | F-10 knots, trails |
| frame β (the frame's own: E5's sample median; in stacking `beta_own`, with `beta_own_at_cap`) | 9 (F-4) | on the single-frame path every fit at β and the model flux (`star_probe`'s default; Plan C's Analysis when Auto is chosen — its default PSF is Moffat 4 with this β shown, R11b-12); in stacking the group β's input only, reported, never consumed | F-4; 10c M42 β B1 / B3 (B2 not a bar: 11b) |
| group β (`psf_signal::group_beta`: the lower median of the members' own βs, always one of them; a group of one at its own) | 11b (LN's since Tier C Task 1) | in stacking Measure every fit at β, F, the model flux (`c`'s denominator), `.athf`, F-6; LN's reference β and its refits; `stacking_run_groups.beta`; the measurement hash under `Auto` (and through it Register's and LN's) | F-4 as built; 11b M42 3c / 3b (recorded limits), the structural pins (every member's `.athf` β == the group row == LN's β) |
| `c` (with its SE and qualifier count) | 10 (F-5); Plan B B4 takes its qualifiers and sample (F-12) | `signal` → PSFSW / PSFSNR, psfSel, Register's and LN's ordering; kept per plane in `ChannelMeasurement` and summarized per (group, channel) at `info` (F-11, B0) | F-5; F-10 LN, real sky, trails; F-11 identity; F-12 |

LN's clip screen on paths without a verdict (`unverdicted_clip_range`) is a declared second saturation definition,
kept until Plan B B5 (F-13): retired with LN's module seeds if B3 keeps a module-fed estimator, else kept and recorded
beside LN's other screens (Register's 0.95, L-14's 0.75 × the side's maximum, L-23's reference-frames map).

**Raw-mode truth.** Raw-mode truth exists only for injected stars (`manifests/inject.json`: 7 raw entries, 3
mosaics). In raw mode the bars are completeness ≥ 99 % of the injected stars above the detection threshold and
F-1 / F-4's FWHM bar on the injected stars; F-6 and F-7 have no raw truth.

**The budget (D-3).** The 500 stars cover ≥ 90 % of the 4 × 4 field cells that hold measurable stars.
*As built and judged (Task 12):* coverage worst 100 % over 141 planes (R12-6's cell definition); the cost bar for the Analysis mode, 0.6 s per m42 frame, is a recorded MISS at 0.70 s (R12-8: the three whole-plane passes, a Measure-performance item beside Task 13's M.2); the stacking Measure batch 111.9 s, B / A 1.028.

**No regressions (every fix step).** Every scorecard row a fix does not change by design stays within its own
standard error of the baseline; each fix names the rows it changes by design and judges them by its own bar. Cost
(`probe-runs-2026-10-07/batches.txt`): the stacking Measure stage at most +20 % wall time; the Analysis mode at most
0.6 s per 26 Mpx frame (today 0.33 s on m42), the on-demand Blink path included.

**Out of scope (reported, not fixed):** the VNG per-plane width change (G +12.63 %, B −6.71 %) — a property of the
debayered planes the stack integrates; the resampling broadening (FWHM² +2.596 px² vs 1.848 nominal).

## 4. The Analysis switch-over

### 4.1 Input

The Analysis tab measures raw lights (uncalibrated ADU, pedestal, hot pixels, no flat; OSC as a Bayer mosaic).

- **Reader.** Analysis mode reads through `ImageConverter::read_raw` → `PixelData` (FITS and XISF, `flip_vertical`,
  `bayer_pattern` — the Blink overlay and the OSC rule need both); stacking keeps `PlaneReader` (calibrated FITS).
  XISF closes the probe's gap. BITPIX 16 with BZERO 0 is signed (the FITS standard; `PlaneReader` reads it so):
  `_DSC5767.fit` (BITPIX 16, BZERO 0, NAXIS3 3) is read signed, pinned by a test; the rustafits reader's unsigned
  reinterpretation (`rf/formats/fits.rs:191-199`) is reviewed against the corpus in Plan C, and a file whose data
  makes sense only unsigned is reported, not guessed.
- The pedestal and vignetting are absorbed by the local background (F-1, F-7).
- A hot pixel is rejected by the detector (neighbours above the threshold) and, if seeded, refused by the fit with
  the reason "narrower than the pixel".
- **OSC.** Detection runs on a 2×2 super-pixel plane; the fit uses only the green CFA sites at their true native
  coordinates (the fitter takes a pixel list, so a checkerboard sample needs no interpolation); FWHM is in native
  pixels. This replaces today's green interpolation (`rf/analysis/mod.rs:661-665`) — an OSC width change in its own
  right, named in the release note. Stacking keeps measuring the debayered planes it integrates: the definition is
  the same, the input is not, so OSC numbers agree between the two consumers only to the VNG effect (§3).

### 4.2 The numbers written

All NOT NULL columns of `frame_analysis` and the `star_metrics` row (`db/schema.rs:1917-1981`):

- From the module, shared with stacking: frame FWHM (fit width, D-2), frame ecc, per-star x, y, peak, flux, fwhm,
  fwhm_x, fwhm_y, ecc, θ (radians, image pixel frame, canonical per F-2 — the Blink overlay already draws fwhm_x on θ
  and negates θ under `flip_vertical`, so it is unchanged by design), β, fit residual, `fit_method` (`moffat`).
- **One frame statistic for both consumers:** the residual-weighted mean over accepted fits flagged neither
  `saturated` (F-3) nor `extended` (F-10). The
  columns and fields keep their names (`median_fwhm`, `median_eccentricity`; no rename — `FrameAnalysis` stays
  wire- and sync-compatible).
- New: per-star SNR (F-6) and its median (`median_snr`); HFR (F-6) = the radius about the fitted centre inside which
  Σ(v − B_i) over the present pixels, taken by distance, reaches half of the star's F-5 flux, interpolated linearly
  (NaN when the circle of 5 × the ring width holds less than half); both frame medians read the fits in the frame
  statistic with finite values (R11-5); `stars_refused` (nullable INTEGER, beside `measure_version`).
  `quality_score` stays NULL (no producer today or after).
- **Formulas kept, values shift:** `frame_snr` (background / noise — a ratio, not dB; the "dB" labels in
  `LightsAnalysisTable.tsx` and `FrameInfoPanel.tsx` are wrong today and are fixed in Plan C), `snr_weight`
  (median flux² / (noise² · background)), `psf_signal` (median peak / noise); background, noise, detection threshold
  (now in the matched filter's units), stars detected, width, height, source channels, median β.
- **Stored thresholds the switch invalidates** (named in the release note): `analysis.rejection_defaults` (fwhm,
  ecc, frameSnr, snrWeight); each frame set's stacking selection `max_fwhm_px` / `max_eccentricity`; collab project
  rules on `fwhm_arcsec`, `eccentricity`, `median_snr`, `snr_weight`, `frame_snr` (mirrored in the hub's
  `collab_rules.rs`). The round-star ecc floor moves from 0.27 to 0.12 at F 2.6 (report row 4) — an F 2.6 figure; pooled over the 111
  round rows the two floors are equal (0.1187 vs 0.1144), and the release note does not generalize it.

### 4.3 Trails and the plate-solve gate

- The trail metric is ported unchanged in definition: the Rayleigh test on 2θ of the measured stars (R², p =
  exp(−nR²)), the `possibly_trailed` rule (R² > threshold and p < 0.01, or R² > 0.15 with median ecc > 0.7 and
  p < 0.05; ≥ 20 stars; field FWHM ≥ 2 px) and the optical-aberration suppression (radial-angle coherence), today in
  `rustafits/src/analysis/mod.rs` from line 1049. It now reads the canonical θ (F-2).
- The plate-solve input gate (`plate_solve/service.rs:622-634`; `input_max_eccentricity` 0.85 AND
  `input_min_trail_r2` 0.65; calibrated on the old numbers: solvable frames ecc 0.62–0.72 with R² 0.30–0.57,
  hopeless 0.90–0.96 with 0.73–0.93) is recalibrated on the star-test frames: the seven wind-shaken frames of
  `trails/` and the frames that solve today (m42, m101, singles), plus the lab's `trails` family. New defaults are
  written only from those numbers; `docs/platesolving/input-and-acceptance-gates.md` records both calibrations.

### 4.4 Version and compatibility

- **One constant.** `star_measure::MEASURE_VERSION` replaces `psf_signal::PSF_FIT_VERSION` (re-exported where
  `stacking/config.rs` uses it) and is folded into the Measure, Register and Normalize stage hashes
  (`stacking/config.rs:812-878`) AND `AnalysisConfig::config_hash`. It is bumped once per merged plan whose code
  changes what a fit accepts or measures (A, C). `config_hash_is_canonical_and_pinned` and the Measure-only
  staleness test are updated with it.
- `frame_analysis` gains nullable `measure_version INTEGER` and `stars_refused INTEGER` (migration; NULL = written
  by the rustafits analysis). Stale rows are recomputed the next time their set is analysed (today's behaviour on a
  settings change); Blink's on-demand path recomputes a stale frame.
- The plate-solve input gate trusts only rows with the current `measure_version`; a stale row counts as "no
  analysis" (no refusal). The collab gate keeps reading whatever row exists (a stale legacy row still gates;
  `measureVersion` makes it visible).
- `FrameAnalysis` gains `measure_version` and `stars_refused` with `#[serde(default)]` (it derives without
  `deny_unknown_fields`): the sync package and the receiver's ingest stay compatible both ways. The collab frame
  meta gains a `measureVersion` key (additive; older apps ignore it). The hub and the portal are not changed here.

### 4.5 Settings → Analysis

- Kept: the star budget (`maxStars`, default 500; `measureCap` folds into it), the trail threshold, batch
  concurrency, and the frontend rejection defaults.
- The saturation fraction has no reader once §2.1's test lost its clip clause (2026-10-09); whether it is removed
  with the fields below is decided before Plan C.
- The detection threshold becomes a new field in the matched filter's units (its default set from the lab); the
  stored `detectionSigma` measured the retired detector's peak threshold and is ignored (named in the release
  note).
- Removed: `fitMaxIter`, `fitTolerance`, `fitMaxRejects`, `mrsLayers`, `minStarArea`, `maxStarArea`. The
  `analysis.psf` section is removed with them (`src/settings/registry.ts`, `docs/settings/README.md`). A stored
  document with those keys still decodes (serde ignores them). The TS types (`ts_export.rs`) and
  `get_settings_defaults` change together.
- No command is added or removed; both hosts keep their surfaces. `stars_refused` is shown in
  `LightsAnalysisTable` and `FrameInfoPanel` (models.ts updated).

## 5. Order of work

One spec, three plans, executed in order (subagent-driven, each merged to local `main` when its final review is
clean; push only on the owner's word).

**Plan A — the module and the fixes.**
1. Lab first, no product change: families FWHM 12 and 16 px and donut stars (isolated); EGAIN / `ATH_CSCL` on the
   generated frames and the SNR truth row (F-6); raw-mode inputs from the existing injections. The bars above are
   already fixed.
2. Extraction into `star_measure/` with no behaviour change. Gate: the probe output is bit-identical after the
   artifact's f32 narrowing (`probe_parity`) and every scorecard summary cell equals the baseline's.
3. `FitDiagnostics` and `stars_refused` (F-0). Numbers unchanged.
4. One fix per step, each judged on its own bar plus the no-regression table: F-2 (axes, convergence) → F-1 with F-7
   (region, background, width start, seeds) → F-3 (the saturation test and its self-consistency row) → F-4
   (continuous β, with the rustafits free-β bound and parametrization) → F-5 (flux, psfSel) → F-10 (one background:
   detection noise, knots, the field width, trails, the per-star sky and noise; its one lab cycle re-judges F-0…F-5
   and F-7 jointly, and it reads the Measure wall first) → F-3's near-clip class (Plan A Task 10c: the saturation flag's
   near-clip class-B stars, the M42 β bar derived from the set's own spread) → F-6 (SNR, reading F-10's B_i and σ_i)
   → F-4b (Plan A Task 11b, built as the stacking group β with the frame's own β a diagnostic; the M42 bar's B2
   resolved by that definition, not a bar) → Task 11c (the Measure first pass spilled to the work dir);
   the F-3 twin rows are judged again after F-4. The stacking Measure config changes (§2.2) land with F-1 and F-4.
5. **Real-data checkpoint** under the owner's rule (a numeric change is judged on the whole group at master level),
   with the athenaeum-lab acceptance harness (`harness/acceptance/`, reused): set 204 mono and OSC, run B, Horse C11
   stacked before and after, with the reference frame pinned (`ReferenceMode::Manual`, the before run's pick) so a
   weight change cannot move the geometry. Both masters are measured by the new path. Pass: master
   median / MAD / noise / FWHM within the master tolerances recorded in `docs/stacking/README.md` (0.1 % / 1 % / 2 % /
   1 %) or better;
   weight rank correlation ρ ≥ 0.9, every larger change tied to a scorecard row; LN scale within 0.5 %; Measure wall
   ≤ +20 %.

**Plan B — F1 and `c` (amended 2026-10-20; rows F-8, F-9, F-11…F-13, bars in §3's Plan B block).** Seven stages in
order. In every LN table the ERP is a column beside every arm, never the referee; where no ERP LN exists or the owner
has not cleared a new run, the column reads "not run". Lab tools are committed in athenaeum-lab only.
- **B0. `c` kept** (product; F-11). Additive `Option`s on `ChannelMeasurement`, the per-(group, channel) `info!`, LN's
  per-side `c` and growth factor at a kept place. Data: one set 204 run and one Horse C11 (`hc11B-f1`) run on
  acceptance working copies, re-measured on a fresh work dir (a cached payload carries no `c`); no ERP. Bar: F-11's
  identity. Owner: informed (R-B2 rules the placement).
- **B1. LN re-baseline on Plan A's fits** (lab only). Synthetic: every LN row taken from the `t14-final` scorecard per
  family × β × arm (no new run), the ERP printed beside every arm, the LN freshness audit; the injected pairs; the
  nebula-sky pairs (`manifests/t10b-l.json`) re-run on the runner at the current binary (their data lives there,
  R-B10). Real: `ln_real.py run` / `table` on the three trees (`tierA-lnB-f1` run 5, `lnB-runB-f1` run 6, `hc11B-f1`
  run 5; the 2026-10-07 wall 2690 s), every arm plus the F1 v2 lab port, after the lab rebuilds `runB-truth.csv` and
  `hc11G-truth.csv` from `ln-real-members.csv` with a script (sha256 recorded) and checks every reference md5 against
  `ln-real-plan.json` before use (R-B10); `annulusCrowding` per member printed for B2. ERP: the existing real ERP LN
  printed; new real runs only on the owner's OK. Bar: none (a baseline); the `f1_gc` ↔ `.athln` parity printed
  (2026-10-07: median +0.0097 %). Owner: the new real-set ERP runs.
- **B2. The referee** (lab only; F-8 referee, field realism). The harness truth in both modes against s_true,
  FWHM > 5 included, and the growth-corrected crowded mode if the fallback fails there (R-B3); the field-realism
  comparison and, if it differs, the one realistic-field family with its one ERP run (R-B5); the injected pairs on a
  seeing lever with the frame's own PSF stamp. Data: the lab grid on this Mac (`pairs`, `density_pairs`, `truthcheck`)
  and B1's real members. Bar: F-8 referee and field realism, closed before any real-data verdict. Owner: informed with
  the table (the referee, synthetic truth and the harness truth, is the owner's; a lab fix to the harness truth is
  reported before B3 uses it). Controller: families and seeds.
- **B3. The LN estimator** (product, or none). Candidates: F1 as is, psfSel on Plan A's fits, the F1 v2 sky under its
  entry bar (with the module ring API change only then; R-B6). B1 / B2's tables first, then whole groups at master
  level (R-B7): set 204 mono and OSC, run B, Horse C11, reference pinned, on acceptance clones; no ERP run (the real
  inputs are unchanged). Bar: F-8 estimator. Owner: the default estimator (L-24 was the owner's), with the numbers;
  keeping F1 changes nothing in the product.
- **B4. The `c` floors and `c`'s sample** (product; F-9, F-12). The floor cases (crops, binned frames and
  masked-overlap corners for (ii); sparse and undersampled planes, crops of set 204 OSC raw planes, for (i)); the
  point-source screen and the brightness-stratified sample, `MEASURE_VERSION` 5. Data: the lab scorecard, inject OSC
  raw (`mono2`), the raw frames 0004 / 0007 / 0013, set 204 OSC as a whole group; an ERP LN run only on new synthetic
  floor-(ii) frames (new frames, the owner's rule). Bar: F-9, F-12. Owner: each floor's verdict (F-9, open since the LN
  model fixes cycle). Controller: the screen's design.
- **B5. LN's own estimates** (product if in scope; F-13). Module seeds for LN if B3 kept a module-fed estimator
  (retiring `k` and `unverdicted_clip_range`); C-9's nebula sky; R11-3. Data: the runner's nebula-sky pairs; no ERP
  (none exists there; a first run waits for the owner's OK). Bar: F-13. Owner: R11-3's scope, if B5 names an owner.
- **B6. Checkpoint and the "after" report.** The full uniform lab run (`lab_cycle`, inject, gates), `ln_real` on the
  three trees, `grid-report --ln-truth` on stacking runs at master level (B3's groups, reference pinned), B1's baseline
  beside every after number; the rulings and runs recorded in `docs/stacking/README.md` R-OSP-16; the full
  `cargo test -p athenaeum-core --all-targets`. Bar: every Plan B bar as judged plus the held bars (F-0…F-7, F-10, the
  no-regression rule). Owner: the push and the close.

**Plan C — the Analysis switch-over (§4).** `analyzer.rs` onto the module; `read_raw` input and OSC; the trail
metric; the plate-solve gate recalibration; `measure_version` / `stars_refused`; Settings; the "dB" labels. Checks:
the star-test corpus measured by the legacy and the new path frame by frame (a comparison; the corpus has no truth);
the Analysis tab, Blink overlay and the calibration panels checked by launching the app. **Default PSF (owner R11b-12,
2026-10-13):** the Analysis tab and Blink fit at Moffat 4 by default (as the reference frame analyser does), the
frame's own β shown as a diagnostic beside FWHM, Auto selectable.

At the end: one full uniform lab run (all gates, the full scorecard) and an "after" report beside the baseline.

## 6. Global constraints

- No third-party program name anywhere in this repository (code, comments, docs, commit messages); the comparison
  program is "the ERP". Lab automation lives in athenaeum-lab only.
- Never read or write the production catalog; acceptance copies only as `sqlite3 "file:…?immutable=1"`, working copies
  via `prepare-catalog.sh`.
- Never `rustfmt` `stacking/run.rs`, `plan.rs`, `ln/mod.rs`, `ln/scale.rs`, `calibration_library/light_cal.rs`,
  `stacking/measure.rs`, `stacking/register/detect.rs`; no `cargo fmt`.
- Cargo with `CARGO_TARGET_DIR=.superpowers/target-acc`, `CARGO_BUILD_JOBS=4`, `--test-threads=4`; one heavy job at a
  time with `memory_pressure` ≥ 30 % free; stop below 100 GiB free disk.
- The full `cargo test -p athenaeum-core --all-targets` before any push.
- `tracing` only; never swallow an error; a refused star is a counted, reasoned outcome, not a silent drop.
- Two backends in sync if any command surface changes (none planned); serde snake_case ↔ camelCase verified against
  `src/types/models.ts`.
- Every ruling and acceptance run is recorded in `docs/stacking/README.md`; `docs/settings/README.md` and
  `docs/platesolving/input-and-acceptance-gates.md` change with their sections.

## 7. Risks

- **The first run after Plan A** re-measures, re-registers (mono) and re-normalizes every set (`MEASURE_VERSION` in
  the stage hashes) — intended, and named in the release note. Task 11b adds a once-only ripple of its own for every
  `Auto` set (the measurement hash folds the group β: Measure, Register and Normalize re-run once; README R-OSP-9).
- **Stored thresholds shift** (§4.2): frame-set selection filters, Analysis rejection defaults and collab project
  rules may select different frames until reviewed.
- **Collab mixed versions.** Members on older app versions publish legacy numbers into the same project until they
  update; `measureVersion` makes the mix visible, the hub-side handling is a later cycle.
- **Gate recalibration data.** The star-test frames are on this Mac; if they do not separate solvable from hopeless
  frames on the new numbers, the gate is reported to the owner with those numbers before any default changes.
- **F-1 on real defocus.** A heavily defocused (annular) star is not a Moffat; such frames get refusals with reasons
  and a frame FWHM only from the stars that fit, possibly none. The Analysis tab shows the refusal count.
- **Speed.** Matched-filter detection, per-star width starts and the free-β verdict fit on every seed (§2.1) add
  cost; the 0.6 s and +20 % bars hold them.

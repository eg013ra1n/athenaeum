# Star measurement part 1, the truth lab and scorecard: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a referee for star measurement (synthetic truth, injected stars, a probe through the production entry
points, a scorecard against truth and the CRLB), and the baseline numbers of today's code against it.

**Architecture:**
- **athenaeum-lab** (`star-lab/starlab/`, Python 3 with numpy only) holds the generator, the truth widths, the CRLB,
  the injection, the matcher and the scorecard.
- **athenaeum** (`crates/athenaeum-core/examples/star_probe.rs`) holds the probe, which calls only production code.
  It needs one behaviour-preserving extraction in `stacking/measure.rs` and one visibility change in
  `stacking/register/detect.rs`.
- **Data** goes to `~/.athenaeum-acc/star-lab-v2/`. The lab finds athenaeum through `ATHENAEUM_REPO`.

**Tech stack:**
- Python 3.14 with numpy 2.4. No scipy, no pytest: tests run with `python3 -m unittest`.
- Rust (athenaeum-core example, serde_json).
- `sqlite3` with `immutable=1` for reading the acceptance copies.

**Spec:** `docs/superpowers/specs/2026-10-06-star-truth-lab-design.md` (with its 2026-10-06 review amendment). The
independent reviews are in `.superpowers/sdd/star-truth-lab-spec-review/`: `review-fable.md` for the spec,
`plan-review-fable.md` for this plan.

**Plan amendment (2026-10-06, after the plan review).** 3 critical, 11 important and 11 minor findings are folded
in. The critical ones:
- the `analysis` path takes a 16-bit sibling of every unit-domain image (P-20);
- P-6 is reversed: the analyzer's coordinates are already in file rows;
- injection's gain comes from `EGAIN`, then the owner's per-camera table, then the raw light's tiles (P-21), and
  its placement measures distances to the real stars.

Three of the plan's lab tests failed when the review ran them verbatim. Their fixes are in Tasks 1, 2 and 7.

## Global Constraints

- **Never name a third-party program in athenaeum:** not in code, comments, docs or commit messages. Say "the ERP".
  The athenaeum-lab ERP drivers may name the binary path they launch, but their docs still say "the ERP".
- **Never read or write the production catalog** (`~/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db`).
  Acceptance copies are read only with `sqlite3 "file:<path>?immutable=1"`.
- **No product behaviour change.** The athenaeum production diff is exactly two items, each listed in Task 5:
  - the `measure_plane_detailed` extraction (P-2);
  - `stars_from_fits` made `pub`.

  Every existing pinned test passes untouched.
- **No `cargo fmt`, ever.** No rustfmt of `stacking/run.rs`, `stacking/plan.rs`, `stacking/ln/mod.rs`,
  `stacking/ln/scale.rs` or `calibration_library/light_cal.rs`. `rustfmt crates/athenaeum-core/examples/star_probe.rs`
  is allowed.
- **Machine: 16 GB RAM, swap nearly full.**
  - One compile or heavy job at a time.
  - `CARGO_BUILD_JOBS=4`, `--test-threads=4`.
  - `memory_pressure | tail -1` must show ≥ 30 % free before any batch.
  - Build the probe with `CARGO_TARGET_DIR=.superpowers/target-acc`.
- **Python dependencies:** numpy and the stdlib only. Tests use `unittest`. Every Python file starts with a docstring
  saying what it is.
- **Data:** under `~/.athenaeum-acc/star-lab-v2/`, at most 10 GB, never committed. Manifests are committed.
- **Commits and pushes:**
  - Commit as `eg013ra1n <vilen.sharifov@gmail.com>`, every message ending with:
    ```
    Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
    Claude-Session: https://claude.ai/code/session_01AkvYzN6darz7u9XC2jYPTg
    ```
  - athenaeum-lab: commit to `main` and `git push origin main` at the end of each task (the owner's standing
    authorization for that repository).
  - athenaeum: commit to branch `star-truth-lab`, never push.
- **ERP runs:** one instance at a time, through `star-lab/erp/run_job.sh` (it has a memory guard), and only after
  the owner has closed any interactive instance. The controller asks the owner; an implementer never starts a long
  ERP batch on its own.
- **A lab gate that fails stops the task,** and the failure goes into its report with numbers. Fix the generator.
  Never move the bar.

## Review Focus

1. **Non-finite pixels** (registered frames, crops, the edge set) entering `star_probe`. Every path must run, NaNs
   are skipped the way production skips them, and the frame line records `nonFinite`. Pinned in Task 5,
   test `nan_border_runs_every_path`.
2. **A starless plane.** Every path writes its frame line with zero counts and no star lines, and the scorecard
   prints `n/a` instead of crashing. Pinned in Task 5 (`starless_plane_writes_frame_lines_only`) and Task 8
   (`test_empty_path_rows`).
3. **A truth star whose cutout is clipped by the frame border.** `flux_img` is the clipped sum. The star is flagged
   `edge` and kept in the detection rows, but left out of the width and CRLB rows. Pinned in Task 1
   (`test_border_star_flux_img_is_the_clipped_sum`) and Task 8 (`test_edge_stars_skip_width_rows`).
4. **A saturated flag on blurred and mixture stars.** The noiseless peak must be taken after the blur; the old
   generator re-searched a 5×5 box. Pinned in Task 4 (`test_saturated_flag_after_blur`).
6. **A unit-domain float into the `analysis` path.** The production detector bins its background in integer ADU,
   so a [0, 1] frame silently measures nothing. Pinned in Task 5 (`analysis_refuses_a_unit_domain_file`); every
   synthetic and injected image gets a 16-bit sibling for this path (P-20).
5. **OSC colour extremes:** a star bright in R and faint in B. Each plane's SNR bin uses that plane's own Fisher
   SNR. Pinned in Task 3 (`test_channel_snr_per_plane`).

## Plan rulings (where the plan has to be more precise than the spec)

- **P-1.** The exact erf pixel integral applies to round or axis-aligned Gaussians (θ a multiple of 90°). A rotated
  elliptical Gaussian goes through the sub-grid like every other profile. The sub-grid gate (§9) uses a round
  Gaussian forced through the sub-grid against the exact erf.
- **P-2.** The probe gets the Measure stage's seeds through a behaviour-preserving extraction:
  `measure_plane_detailed(...) -> (PlaneMeasurement, Vec<Seed>)`. Its body is `measure_plane_with_seeds`'s body,
  moved, and `measure_plane_with_seeds` returns `.0` of it. The existing pins guard it:
  - `the_shipped_none_path_measures_exactly_these_numbers`;
  - `measure_plane_with_seeds_pinned_before_task_3b_rewrite`;
  - `seed_source_fast_is_bit_identical_to_measure_plane`.

  This goes beyond the spec's "visibility-only" wording, so it is listed for the owner.
- **P-3.** Seed lines carry the production `Seed` fields `x, y, peak, flux`. `hfd` and `snr` do not survive past the
  detector's filter in production, so they are not dumped. Spec §5 listed them.
- **P-4.** Input conversion uses the harness's established rule: when any finite value of any plane exceeds 1.5,
  every plane is divided by 65535. The frame line records `conversion`. Prepare's attested path is not a reachable
  public function.
- **P-5.** Units. Production fits carry `background`, `amplitude`, `signal` and `peak5`, and seeds carry `peak` and
  `flux`, all in ADU-scaled units (×65535). The probe writes them divided by 65535, so every lab number is in image
  units [0, 1]. FWHM is in pixels.
- **P-6 (reversed by the plan review).** `flipVertical` is recorded on the `analysis` frame line and never applied.
  rustafits reads the pixel rows as stored, and the flag only tells the display overlay to flip (`annotate.rs`).
  The analyzer's `x`, `y` and `θ` are therefore in file-row coordinates, like every other path's. `probe_io` asserts
  that the flag is present and leaves the coordinates alone.
- **P-7.** The optional pre-warp injection mode (spec §4, `warp --map`) is not in this plan. The baseline report
  records "not done", and the warp-parity gate does not apply.
- **P-8.** All lab tests use stdlib `unittest`.
- **P-9.** The point-sampled-fit equivalent (truth column 3) is the lab's own Levenberg–Marquardt fit, point-sampled,
  to the noiseless pixel-integrated stamp:
  - an elliptical Moffat at the generator's β for Moffat families;
  - a Gaussian for Gaussian families;
  - a free-β elliptical Moffat for every other family (blur, wing, mixture, coma).

  It is computed at sub-pixel phase (0.3, −0.2) once per distinct shape. For a round generator (FWHM minor ==
  major), `fit_ecc = 0` and `fit_theta_deg = nan`. The LM's ≈ 0.02 there is optimizer wander on a flat direction,
  not a property of the profile.
- **P-10.** Truth widths are cached per distinct shape (profile, FWHMs, e, θ, blur). Position-dependent families
  (`fieldpsf`, `coma`, `trails`) are computed per star.
- **P-11.** The CRLB is computed for a seeded, stratified sample of 50 truth stars per SNR bin (at most 300 per
  image). `twins` reuse their base image's truth CSV verbatim, because only the noise differs. Measured cost:
  27–42 ms per star. Derivatives are central differences on the family's own renderer. B is dropped when the sky is 0. θ is
  dropped when e < 0.05, and then the bound of `q = 1 − minor/major` replaces the eccentricity bound.
- **P-12.** Injection's real stars come from `inject.py`'s own detection: local maxima of the 3 × 3 box-smoothed
  residual more than 5 σ (of that smoothed residual) above a 64-px median background. Placement keeps `12 × FWHM_inj`
  from each of them. A 3 σ rule on raw pixels took noise peaks for stars, and its exclusion swallowed a test frame
  (checked when the plan's code was run). The harness's candidate list keeps only
  isolated bright stars and would miss faint real ones.
- **P-13.** `star_probe` gets two more subcommands:
  - `debayer`, which writes the production VNG planes, for `ln_scale_probe`'s OSC pairs;
  - `dump-fits`, the production decoder of a persisted `fits` artifact, for the parity gate.
- **P-14.** The 14-bit saturation variant clips at 16383/65535 = 0.25 (a 14-bit ADC in a 16-bit container). The
  review's "0.5 of full scale" was a slip. This is a correction of the spec's number, listed for the owner with
  P-2.
- **P-15.** `pairs` reuses the Task 16b `iso` and `main` grids in place (`~/.athenaeum-acc/star-lab/grid/`, their
  existing manifests). The new OSC and density pairs are rendered by v2.
- **P-16.** Clipped-core mixture: at each pixel, member values above the 70th percentile across members are replaced
  by that percentile, then averaged (a winsorized mean, the model of a high-rejection master).
- **P-18.** The model bound (spec §6) is computed by the scorecard, not stored in the truth. It is the CRLB of an
  elliptical Moffat at the Auto β the `measure` path reported for that image and plane (its frame line's `beta`),
  at the star's point-fit-equivalent widths, for families whose generator is not an elliptical Moffat (Gaussian,
  blur, wing, mixture, coma). This is the spec's "the fit's own model at its best-fit parameters".
- **P-19.** `StarMetric` carries no per-star saturation flag, and `possibly_trailed` is frame-level. The `analysis`
  star line therefore dumps `fitMethod` only, and the trail flags come from the frame line's `frame` object. That
  object is production's `FrameAnalysis` serialized as is, so its keys are snake_case (`possibly_trailed`,
  `trail_r_squared`, `median_eccentricity`, …); the rest of the JSONL is camelCase. This corrects spec §5 (Task 5
  review, I-2).
- **P-20.** The `analysis` path reads raw-domain files only, as the Analysis tab does. Its detector bins its
  background in integer ADU, so a [0, 1] float measures nothing; an earlier probe saw 0/20 solves on unit-domain
  files. Every synthetic image and every calibrated injection therefore also gets a 16-bit sibling
  `<name>.u16.fits` (BITPIX 16, `BZERO 32768`, `round(clip(img, 0, 1) × 65535)`, the same cards). The probe
  refuses `analysis` on a file whose maximum is ≤ 1.5.
- **P-21.** Gain per unit of a calibrated frame (from `light_cal.rs`: `L_c = ((L − S)/(F/ATH_CFNM))/ATH_CSCL`):
  `g(x, y) = e_adu × ATH_CSCL × F(x, y) / ATH_CFNM`.
  - `F` is the master flat's own pixel (the file named in `ATH_CFLT`).
  - On a mosaic with `ATH_CCFA = T`, the channel's `ATH_CFNR/G/B` replaces `ATH_CFNM`.
  - `e_adu` (e⁻/ADU) comes from, in this order:
    1. `EGAIN` on the frame;
    2. the owner's per-camera table `star-lab/manifests/cameras.json` (`{INSTRUME: {GAIN: e_adu}}`);
    3. the tile estimate on the RAW source light named by `ATH_CSRN`, whose vignetting gives the level span.

    It never comes from the calibrated frame's own tiles: a flat-fielded sky does not span 20 %. A frame with none
    of the three is refused and named.
  - The gain gate checks the gain only. The read noise is reported, not gated, because on a dark-subtracted frame
    the intercept is rn² plus the removed pedestal.
- **P-22.** Real frames and the 4096² image run one path per `star_probe` invocation; a 61 Mpx frame reached
  5.6 GB in one LN probe. 2048² synthetics run the stacking paths in one invocation and `analysis` in a second one
  (on the `.u16.fits`). Row 9's peak RSS comes from the one-path invocations only.
- **P-23.** Base gain is 50 000 e⁻/unit (Task 16b's value), not 12 500. The SNR-bin check exempts bins above an
  image's saturation ceiling `snr_sat`, the Fisher SNR at 0.9 × the saturating flux (F 1.2: 293). Such a bin is
  reported `n/a (ceiling snr_sat)`. At gain 12 500 the `> 300` bin was empty at every FWHM ≤ 2.6 whatever the
  image count. Every other bin still needs at least 50 stars.
- **P-24 (execution, Task 1–3 review).** The truth half-max ellipse comes from a sub-sample CONTOUR, not the
  thresholded lattice region. Rays from the centre (720 angles) are bisected to the half-maximum, analytically for the
  pre-pixel profile and bilinear on the fine grid for the pixel-box and blur columns. The ellipse is taken from the
  exact second moments of the contour polygon. The lattice region read e 0.049 for a true 0.1 and 0 below 0.05,
  which would have broken the `fieldpsf` truth. Round generators report nan for every θ column. A sweep test pins
  the 5e-3 bar over e ∈ {0.02 … 0.3}.
- **P-25 (execution, Task 4).** Blur images and their truth use `render.render_star_blurred`: the core is sampled on
  an os sub-grid (os = max(25 / total FWHM, 9 / core FWHM), clamped to [3, 21]), blurred there and summed per pixel.
  The pixel-grid `blur_image` read the peak 14.5 % high at F 1.2 blur 2.5 (5.6 % at F 2.0), so it stays only for the
  fine grid and must not render an image below F ≈ 3. This moved Task 2's blur fit-truth column by up to +1.7 %. A
  generator fix under the doctrine, like P-24; no bar moved.
- **P-26 (execution, Task 4 review I-1, I-2, M-1, M-2).** Every family's `crlb_*` bounds its own truth columns.
  `crlb.star_bounds(..., jac=None)` takes J = ∂(truth minor, major)/∂(fitted width parameters) and transforms the
  covariance, C_T = J C Jᵀ, before the width, ecc/q and the new `crlb_fwhm_gm` (the bound of √(minor·major), which
  Task 8's `rows_fwhm` compares) are derived. J is computed per shape for blur and per star for coma; it is the
  identity for Moffat, Gaussian and mixtures, pinned within 1 %. Doubles with separation ≤ 3 px carry no CRLB (n/a):
  the isolated bound understates the blend 1.3–2.9×.
- **P-27 (execution, Task 4 review I-3).** `saturated` = the star's own background plus stamp reaches `sat` anywhere
  in the stamp, OR the full noiseless plane reaches `sat` on the star's own footprint (pixels where its own stamp is
  ≥ 1 % of its own peak), never on the whole cutout box. A faint star on a saturated neighbour's clipped core is
  therefore saturated: its own light is clipped on its footprint (d1000: 10 core-clipped + 24 such stars).
- **P-17.** The `register` path is chosen by plane count, as production does:
  - one plane: `stars_from_fits` on that plane's Measure fits;
  - three planes: `detect_stars` on `luminance` of the three.

  The spec's explicit `--plane lum` is therefore not needed.

## File structure

athenaeum-lab (`/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab`):

| File | Responsibility |
| ---- | ---- |
| `star-lab/starlab/__init__.py` | package marker, `DATA_ROOT`, `ATHENAEUM_REPO`, `PROBE` paths |
| `star-lab/starlab/fitsio.py` | FITS read/write (2-D, 3-plane, header cards) |
| `star-lab/starlab/render.py` | profiles, the `os` rule, `render_star`, blur, tracks |
| `star-lab/starlab/shape.py` | truth widths: half-max ellipse, moment ellipse, point-sampled-fit equivalent |
| `star-lab/starlab/crlb.py` | Fisher SNR and CRLB |
| `star-lab/starlab/field.py` | star lists: positions, SNR-ladder fluxes, saturated tails |
| `star-lab/starlab/background.py` | gradient, vignetting, fractal nebula, the halo star |
| `star-lab/starlab/artefacts.py` | hot pixels, cosmic-ray worms |
| `star-lab/starlab/osc.py` | channel weights, RGGB mosaic |
| `star-lab/starlab/noise.py` | Poisson + read noise + clip |
| `star-lab/starlab/families.py` | the family table and the image builders |
| `star-lab/starlab/manifest.py` | manifests, hashes, regeneration check |
| `star-lab/starlab/inject.py` | injection into real frames |
| `star-lab/starlab/match.py` | truth ↔ measurement matching |
| `star-lab/starlab/probe_io.py` | readers for `star_probe` JSONL, `ln_scale_probe` outputs, ERP CSVs |
| `star-lab/starlab/scorecard.py` | scorecard rows 1–11 |
| `star-lab/bin/make_v2.py` | generate families → images + truth + manifest |
| `star-lab/bin/inject.py` | CLI over `starlab.inject` |
| `star-lab/bin/run_probe.py` | run `star_probe` and `ln_scale_probe` over a manifest |
| `star-lab/bin/scorecard.py` | CLI over `starlab.scorecard` |
| `star-lab/bin/gates.py` | the lab's own gates (spec §9) |
| `star-lab/erp/erp_v2.py` | ERP drivers over v2 manifests (adapts `erp_stars.js`, `run_ln.js`) |
| `star-lab/tests/test_*.py` | unit tests per module |
| `star-lab/manifests/` | committed manifests |

athenaeum:

| File | Responsibility |
| ---- | ---- |
| `crates/athenaeum-core/src/stacking/measure.rs` | `measure_plane_detailed` extraction (P-2) |
| `crates/athenaeum-core/src/stacking/register/detect.rs` | `stars_from_fits` → `pub` |
| `crates/athenaeum-core/examples/star_probe.rs` | the probe: `measure`, `debayer`, `warp`, `dump-fits` |
| `docs/superpowers/research/<date>-star-measurement-baseline.md` | the baseline report (Task 11) |

Run the lab tests from `star-lab/`: `python3 -m unittest discover -s tests -v`.

---

### Task 1: Lab package, FITS I/O and the renderer

**Files:**
- Create: `star-lab/starlab/__init__.py`, `star-lab/starlab/fitsio.py`, `star-lab/starlab/render.py`
- Test: `star-lab/tests/__init__.py` (empty), `star-lab/tests/test_render.py`, `star-lab/tests/test_fitsio.py`

**Interfaces:**
- Produces:
  - `fitsio.write_fits(path, a, cards=None)` (a: 2-D `[h, w]` or 3-D `[n, h, w]`; cards: dict);
  - `fitsio.write_fits_u16(path, a, cards=None)`, the raw-light container for the `analysis` path (P-20);
  - `fitsio.read_fits(path) -> (np.ndarray float64, dict)`;
  - `render.FWHM_PER_SIGMA`, `render.erf(a)`, `render.moffat_alpha(fwhm, beta)`, `render.os_for(fwhm_minor)`;
  - `render.Component(w, kind, a, b, beta)` and `render.components(profile, fwhm_minor, fwhm_major, beta=4.0,
    wing_frac=0.0, wing_mult=4.0, wing_beta=2.0) -> list[Component]`;
  - `render.evaluate(comps, u, v)`, `render.cutout_radius(comps, fwhm_major, cap=150)`;
  - `render.render_star(img, x0, y0, flux, comps, theta_rad, fwhm_minor, fwhm_major, cap=150, force_subgrid=False)
    -> (flux_img, peak, (ya, yb, xa, xb))`;
  - `render.blur_image(img, blur_fwhms)`;
  - `render.render_track(img, points, flux, comps, fwhm_minor, fwhm_major) -> (flux_img, peak, box)`.

- [ ] **Step 1: Package marker with paths**

```python
"""starlab: the synthetic star lab v2 (star measurement part 1). numpy + stdlib only."""
import os

DATA_ROOT = os.path.expanduser(os.environ.get("STARLAB_DATA", "~/.athenaeum-acc/star-lab-v2"))
ATHENAEUM_REPO = os.path.expanduser(os.environ.get("ATHENAEUM_REPO", "~/Documents/Projects/athenaeum"))
PROBE = os.path.join(ATHENAEUM_REPO, ".superpowers", "target-acc", "release", "examples", "star_probe")
LN_SCALE_PROBE = os.path.join(ATHENAEUM_REPO, ".superpowers", "target-acc", "release", "examples", "ln_scale_probe")
```

- [ ] **Step 2: Write the failing tests** (`tests/test_render.py`)

```python
"""Renderer gates (spec §3.1, §9 pixel integration; plan P-1)."""
import math, unittest
import numpy as np
from starlab import render


def exact_gauss_stamp(x0, y0, fwhm, size):
    s = fwhm / render.FWHM_PER_SIGMA
    e = np.arange(size + 1) - 0.5
    cx = render.erf((e - x0) / (math.sqrt(2) * s)); cy = render.erf((e - y0) / (math.sqrt(2) * s))
    return np.outer(0.5 * np.diff(cy), 0.5 * np.diff(cx))


class TestRender(unittest.TestCase):
    def test_subgrid_matches_exact_erf_within_1e3_of_peak(self):
        for fwhm in (1.2, 2.0, 2.6, 3.5):
            size = 41; x0, y0 = 20.3, 19.8
            img = np.zeros((size, size))
            comps = render.components("gauss", fwhm, fwhm)
            render.render_star(img, x0, y0, 1.0, comps, 0.0, fwhm, fwhm, force_subgrid=True)
            ref = exact_gauss_stamp(x0, y0, fwhm, size)
            err = np.abs(img - ref).max() / ref.max()
            self.assertLess(err, 1e-3, f"FWHM {fwhm}: {err:.2e}")

    def test_axis_aligned_gauss_takes_the_exact_path(self):
        size = 41; img = np.zeros((size, size))
        comps = render.components("gauss", 1.2, 1.2)
        render.render_star(img, 20.3, 19.8, 1.0, comps, 0.0, 1.2, 1.2)
        np.testing.assert_allclose(img, exact_gauss_stamp(20.3, 19.8, 1.2, size), rtol=0, atol=1e-15)

    def test_moffat_matches_os45_within_1e3_of_peak(self):
        size = 41; x0, y0 = 20.3, 19.8; fwhm = 2.6
        comps = render.components("moffat", fwhm, fwhm, beta=2.5)
        a = np.zeros((size, size)); render.render_star(a, x0, y0, 1.0, comps, 0.0, fwhm, fwhm)
        b = np.zeros((size, size)); render._render_subgrid(b, x0, y0, 1.0, comps, 0.0, fwhm, 45)
        self.assertLess(np.abs(a - b).max() / b.max(), 1e-3)

    def test_flux_img_is_the_sum_and_the_remainder_is_bounded(self):
        for prof, beta, wing, bound in (("gauss", 0, 0.0, 1e-4), ("moffat", 2.5, 0.0, 1e-4),
                                        ("moffat", 4.0, 0.0, 1e-4), ("moffat", 10.0, 0.0, 1e-4),
                                        ("moffat", 4.0, 0.1, 2e-3)):
            img = np.zeros((401, 401))
            comps = render.components(prof, 3.0, 3.0, beta=beta or 4.0, wing_frac=wing)
            s, _, _ = render.render_star(img, 200.3, 199.8, 1.0, comps, 0.0, 3.0, 3.0)
            self.assertAlmostEqual(s, img.sum(), places=12)
            self.assertLessEqual(1.0 - s, bound + 1e-12, f"{prof} beta {beta} wing {wing}: {1 - s:.2e}")

    def test_border_star_flux_img_is_the_clipped_sum(self):
        img = np.zeros((64, 64))
        comps = render.components("moffat", 3.0, 3.0, beta=4.0)
        s, _, (ya, yb, xa, xb) = render.render_star(img, 1.2, 30.0, 1.0, comps, 0.0, 3.0, 3.0)
        self.assertEqual(xa, 0)
        self.assertAlmostEqual(s, img.sum(), places=12)
        self.assertLess(s, 0.9)

    def test_os_rule(self):
        self.assertEqual(render.os_for(1.2), 15); self.assertEqual(render.os_for(3.99), 15)
        self.assertEqual(render.os_for(4.0), 9); self.assertEqual(render.os_for(8.0), 9)

    def test_blur_conserves_flux(self):
        img = np.zeros((256, 256)); comps = render.components("moffat", 2.4, 2.4, beta=4.0)
        render.render_star(img, 128.0, 128.0, 1.0, comps, 0.0, 2.4, 2.4)
        out = render.blur_image(img, [2.4])
        self.assertAlmostEqual(out.sum(), img.sum(), places=9)

    def test_track_flux_and_extent(self):
        img = np.zeros((128, 128)); comps = render.components("moffat", 2.6, 2.6, beta=4.0)
        pts = [(50.0 + t, 64.0) for t in np.linspace(0, 12, 49)]
        s, _, _ = render.render_track(img, pts, 1.0, comps, 2.6, 2.6)
        self.assertAlmostEqual(s, 1.0, delta=2e-4)
        cols = img.sum(axis=0); half = cols.max() / 2
        width = np.count_nonzero(cols > half)
        self.assertGreaterEqual(width, 12)
```

`tests/test_fitsio.py`:

```python
"""FITS round trips for the lab."""
import os, tempfile, unittest
import numpy as np
from starlab import fitsio


class TestFits(unittest.TestCase):
    def test_round_trip_2d_and_3d_with_cards(self):
        with tempfile.TemporaryDirectory() as d:
            a = np.random.default_rng(1).random((5, 7)).astype(np.float32)
            p = os.path.join(d, "a.fits"); fitsio.write_fits(p, a, {"BAYERPAT": "RGGB", "SYNFWHM": 2.5, "NSTAR": 3})
            b, h = fitsio.read_fits(p)
            np.testing.assert_array_equal(b, a.astype(np.float64))
            self.assertEqual(h["BAYERPAT"].strip("' "), "RGGB"); self.assertEqual(int(h["NSTAR"]), 3)
            c = np.random.default_rng(2).random((3, 5, 7)).astype(np.float32)
            q = os.path.join(d, "c.fits"); fitsio.write_fits(q, c)
            e, _ = fitsio.read_fits(q)
            self.assertEqual(e.shape, (3, 5, 7)); np.testing.assert_array_equal(e, c.astype(np.float64))

    def test_u16_sibling_round_trips_to_adu(self):
        with tempfile.TemporaryDirectory() as d:
            a = np.array([[0.0, 0.5], [1.0, 0.25]])
            p = os.path.join(d, "u.fits"); fitsio.write_fits_u16(p, a, {"BAYERPAT": "RGGB"})
            b, h = fitsio.read_fits(p)
            np.testing.assert_array_equal(b, np.round(a * 65535.0))
            self.assertEqual(int(h["BITPIX"]), 16)

    def test_float_cards_carry_a_decimal_point(self):
        self.assertIn("2.0", fitsio._card("X", 2.0)); self.assertIn("1.0E-07", fitsio._card("X", 1e-7))
```

- [ ] **Step 3: Run them and see them fail**

Run: `cd star-lab && python3 -m unittest discover -s tests -v`
Expected: `ModuleNotFoundError: No module named 'starlab.render'` (and `fitsio`).

- [ ] **Step 4: Implement `fitsio.py`**

```python
"""FITS I/O for the lab: float32 images, 2-D [h, w] or 3-plane [n, h, w], plain header cards.
Rows: array row 0 = FITS row 1; x = column. Integer data honours BZERO/BSCALE."""
import numpy as np


def _card(key, value):
    if isinstance(value, bool):
        return f"{key:<8}= {'T' if value else 'F':>20}"
    if isinstance(value, int):
        return f"{key:<8}= {value:>20d}"
    if isinstance(value, float):
        t = f"{value:.12G}"                      # FITS fixed-format reals need a decimal point
        if "E" in t and "." not in t.split("E")[0]:
            t = t.replace("E", ".0E")
        elif "." not in t and "E" not in t and t not in ("NAN", "INF", "-INF"):
            t += ".0"
        return f"{key:<8}= {t:>20}"
    v = "'" + str(value).replace("'", "''") + "'"
    return f"{key:<8}= {v:<20}"


def write_fits(path, a, cards=None):
    a = np.ascontiguousarray(a, dtype=">f4")
    if a.ndim == 2:
        axes = [a.shape[1], a.shape[0]]
    elif a.ndim == 3:
        axes = [a.shape[2], a.shape[1], a.shape[0]]
    else:
        raise ValueError(f"write_fits: a {a.ndim}-D array")
    hdr = ["SIMPLE  =                    T", "BITPIX  =                  -32", f"NAXIS   = {len(axes):20d}"]
    hdr += [f"NAXIS{i + 1:<3}= {n:20d}" for i, n in enumerate(axes)]
    hdr += [_card(k, v) for k, v in (cards or {}).items()]
    hdr.append("END")
    blob = "".join(c[:80].ljust(80) for c in hdr)
    blob += " " * ((2880 - len(blob) % 2880) % 2880)
    body = a.tobytes()
    with open(path, "wb") as f:
        f.write(blob.encode("ascii")); f.write(body); f.write(b"\0" * ((2880 - len(body) % 2880) % 2880))


def write_fits_u16(path, a, cards=None):
    """The raw-light container (P-20): BITPIX 16, BZERO 32768, values round(clip(a, 0, 1) x 65535)."""
    v = np.round(np.clip(np.nan_to_num(np.asarray(a, dtype=np.float64), nan=0.0), 0.0, 1.0) * 65535.0)
    raw = (v - 32768.0).astype(">i2")
    axes = [raw.shape[-1], raw.shape[-2]] + ([raw.shape[0]] if raw.ndim == 3 else [])
    hdr = ["SIMPLE  =                    T", "BITPIX  =                   16", f"NAXIS   = {len(axes):20d}"]
    hdr += [f"NAXIS{i + 1:<3}= {n:20d}" for i, n in enumerate(axes)]
    hdr += ["BZERO   =              32768.0", "BSCALE  =                  1.0"]
    hdr += [_card(k, v2) for k, v2 in (cards or {}).items()]
    hdr.append("END")
    blob = "".join(c[:80].ljust(80) for c in hdr)
    blob += " " * ((2880 - len(blob) % 2880) % 2880)
    body = np.ascontiguousarray(raw).tobytes()
    with open(path, "wb") as f:
        f.write(blob.encode("ascii")); f.write(body); f.write(b"\0" * ((2880 - len(body) % 2880) % 2880))


def read_fits(path):
    with open(path, "rb") as f:
        raw = f.read()
    hdr, off = {}, 0
    while True:
        blk = raw[off:off + 2880].decode("ascii", "replace"); off += 2880
        done = False
        for i in range(36):
            c = blk[i * 80:(i + 1) * 80]
            k = c[:8].strip()
            if k == "END":
                done = True; break
            if c[8:10] == "= ":
                hdr[k] = c[10:].split(" /")[0].strip()
        if done:
            break
    n = int(hdr["NAXIS"]); dims = [int(hdr[f"NAXIS{i + 1}"]) for i in range(n)]
    bp = int(hdr["BITPIX"])
    dt = {-32: ">f4", -64: ">f8", 16: ">i2", 32: ">i4", 8: ">u1"}[bp]
    count = int(np.prod(dims))
    a = np.frombuffer(raw, dtype=dt, count=count, offset=off).astype(np.float64)
    if bp > 0:
        a = a * float(hdr.get("BSCALE", 1)) + float(hdr.get("BZERO", 0))
    return a.reshape(list(reversed(dims))), hdr
```

- [ ] **Step 5: Implement `render.py`**

```python
"""Star profiles with exact truth, integrated over each pixel's area (spec §3.1, plan P-1).

Geometry: the MINOR-axis FWHM is the seeing; the major axis grows with eccentricity,
FWHM_major = FWHM_minor / sqrt(1 - e^2). theta = angle of the major axis from +x, counter-
clockwise in (x, y) array coordinates. Coordinates: 0-based pixel centres, x = column."""
import math
import numpy as np

FWHM_PER_SIGMA = 2.0 * math.sqrt(2.0 * math.log(2.0))
_ERF = np.frompyfunc(math.erf, 1, 1)


def erf(a):
    return _ERF(np.asarray(a, dtype=np.float64)).astype(np.float64)


def moffat_alpha(fwhm, beta):
    return fwhm / (2.0 * math.sqrt(2.0 ** (1.0 / beta) - 1.0))


def os_for(fwhm_minor):
    """Sub-grid per pixel side: 15 below 4 px, 9 at or above (os 5 misses 1e-3 of the peak up to FWHM 3.5)."""
    return 15 if fwhm_minor < 4.0 else 9


class Component:
    """One profile component: weight w; kind 'gauss' (a, b = sigma major/minor) or 'moffat' (a, b = alpha)."""
    __slots__ = ("w", "kind", "a", "b", "beta")

    def __init__(self, w, kind, a, b, beta=0.0):
        self.w, self.kind, self.a, self.b, self.beta = w, kind, a, b, beta


def components(profile, fwhm_minor, fwhm_major, beta=4.0, wing_frac=0.0, wing_mult=4.0, wing_beta=2.0):
    if profile == "gauss":
        comps = [Component(1.0 - wing_frac, "gauss", fwhm_major / FWHM_PER_SIGMA, fwhm_minor / FWHM_PER_SIGMA)]
    elif profile == "moffat":
        comps = [Component(1.0 - wing_frac, "moffat", moffat_alpha(fwhm_major, beta), moffat_alpha(fwhm_minor, beta), beta)]
    else:
        raise ValueError(f"unknown profile {profile!r}")
    if wing_frac > 0:
        comps.append(Component(wing_frac, "moffat", moffat_alpha(fwhm_major * wing_mult, wing_beta),
                               moffat_alpha(fwhm_minor * wing_mult, wing_beta), wing_beta))
    return comps


def evaluate(comps, u, v):
    """Unit-flux profile at rotated offsets (u along the major axis, v along the minor)."""
    out = np.zeros(np.broadcast(np.asarray(u), np.asarray(v)).shape)
    for c in comps:
        if c.kind == "gauss":
            out += c.w * np.exp(-0.5 * ((u / c.a) ** 2 + (v / c.b) ** 2)) / (2 * math.pi * c.a * c.b)
        else:
            out += c.w * (c.beta - 1.0) / (math.pi * c.a * c.b) * (1.0 + (u / c.a) ** 2 + (v / c.b) ** 2) ** (-c.beta)
    return out


def cutout_radius(comps, fwhm_major, cap=150):
    r = 3.0 * fwhm_major
    for c in comps:
        if c.kind == "gauss":
            r = max(r, 6.0 * c.a)
        else:
            eps = 5e-5 if c.w > 0.5 else 2e-3     # core 5e-5: the tail sub-grid below leaves the 1e-4 bar room
            r = max(r, c.a * math.sqrt(eps ** (1.0 / (1.0 - c.beta)) - 1.0))
    return int(min(math.ceil(r), cap))


def _box(shape, x0, y0, R):
    h, w = shape
    ix, iy = int(round(x0)), int(round(y0))
    return max(0, iy - R), min(h - 1, iy + R), max(0, ix - R), min(w - 1, ix + R)


def _exact_gauss(comps, theta):
    if len(comps) != 1 or comps[0].kind != "gauss":
        return None
    c = comps[0]
    if abs(c.a - c.b) < 1e-15 * c.a:
        return c.a, c.b
    t = math.degrees(theta) % 180.0
    if min(t, abs(t - 90.0), 180.0 - t) < 1e-9:
        return (c.a, c.b) if (t < 45.0 or t > 135.0) else (c.b, c.a)
    return None


def _render_subgrid(img, x0, y0, flux, comps, theta, fwhm_major, os_, cap=150):
    """A 3 x 3 midpoint sub-grid over the whole cutout (a convex tail is under-counted by centre sampling,
    plan review I-2), and an os x os midpoint sub-grid inside +-3 FWHM_major."""
    R = cutout_radius(comps, fwhm_major, cap)
    ya, yb, xa, xb = _box(img.shape, x0, y0, R)
    ct, st = math.cos(theta), math.sin(theta)

    def ev(dx, dy):
        return evaluate(comps, dx * ct + dy * st, -dx * st + dy * ct)

    X, Y = np.meshgrid(np.arange(xa, xb + 1, dtype=np.float64), np.arange(ya, yb + 1, dtype=np.float64))
    off3 = (np.arange(3) + 0.5) / 3 - 0.5
    val = np.zeros_like(X)
    for oy in off3:
        for ox in off3:
            val += ev(X + ox - x0, Y + oy - y0)
    val /= 9.0
    core = int(math.ceil(3.0 * fwhm_major)); ix, iy = int(round(x0)), int(round(y0))
    cya, cyb, cxa, cxb = max(ya, iy - core), min(yb, iy + core), max(xa, ix - core), min(xb, ix + core)
    if cxb >= cxa and cyb >= cya:
        CX = X[cya - ya:cyb - ya + 1, cxa - xa:cxb - xa + 1]; CY = Y[cya - ya:cyb - ya + 1, cxa - xa:cxb - xa + 1]
        off = (np.arange(os_) + 0.5) / os_ - 0.5
        acc = np.zeros_like(CX)
        for oy in off:
            for ox in off:
                acc += ev(CX + ox - x0, CY + oy - y0)
        val[cya - ya:cyb - ya + 1, cxa - xa:cxb - xa + 1] = acc / (os_ * os_)
    stamp = flux * val
    img[ya:yb + 1, xa:xb + 1] += stamp
    return float(stamp.sum()), float(stamp.max()), (ya, yb, xa, xb)


def render_star(img, x0, y0, flux, comps, theta, fwhm_minor, fwhm_major, cap=150, force_subgrid=False):
    """Adds one star to img in place. Returns (flux_img, peak, (ya, yb, xa, xb))."""
    g = None if force_subgrid else _exact_gauss(comps, theta)
    if g is None:
        return _render_subgrid(img, x0, y0, flux, comps, theta, fwhm_major, os_for(fwhm_minor), cap)
    sx, sy = g
    R = cutout_radius(comps, fwhm_major, cap)
    ya, yb, xa, xb = _box(img.shape, x0, y0, R)
    ex = np.arange(xa, xb + 2, dtype=np.float64) - 0.5
    ey = np.arange(ya, yb + 2, dtype=np.float64) - 0.5
    gx = 0.5 * np.diff(erf((ex - x0) / (math.sqrt(2) * sx)))
    gy = 0.5 * np.diff(erf((ey - y0) / (math.sqrt(2) * sy)))
    stamp = flux * comps[0].w * np.outer(gy, gx)
    img[ya:yb + 1, xa:xb + 1] += stamp
    return float(stamp.sum()), float(stamp.max()), (ya, yb, xa, xb)


def blur_image(img, blur_fwhms):
    """Equal-weight mixture of Gaussian blurs (optics x seeing); flux is conserved."""
    if not blur_fwhms:
        return img
    F = np.fft.rfft2(img)
    fy = np.fft.fftfreq(img.shape[0])[:, None]; fx = np.fft.rfftfreq(img.shape[1])[None, :]
    H = sum(np.exp(-2 * math.pi ** 2 * (b / FWHM_PER_SIGMA) ** 2 * (fx * fx + fy * fy)) for b in blur_fwhms)
    return np.fft.irfft2(F * H / len(blur_fwhms), s=img.shape)


def render_track(img, points, flux, comps, fwhm_minor, fwhm_major):
    """A trail: the profile convolved with a polyline, sampled at <= 0.25 px steps."""
    pts = np.asarray(points, dtype=np.float64)
    seg = np.hypot(*np.diff(pts, axis=0).T)
    n = max(2, int(math.ceil(seg.sum() / 0.25)) + 1)
    s = np.concatenate([[0.0], np.cumsum(seg)]); t = np.linspace(0.0, s[-1], n)
    xs = np.interp(t, s, pts[:, 0]); ys = np.interp(t, s, pts[:, 1])
    tot, pk, boxes = 0.0, 0.0, []
    for x, y in zip(xs, ys):
        f, _, b = render_star(img, x, y, flux / n, comps, 0.0, fwhm_minor, fwhm_major)
        tot += f; boxes.append(b)
    ya = min(b[0] for b in boxes); yb = max(b[1] for b in boxes); xa = min(b[2] for b in boxes); xb = max(b[3] for b in boxes)
    pk = float(img[ya:yb + 1, xa:xb + 1].max())
    return tot, pk, (ya, yb, xa, xb)
```

Note: `render_track`'s returned peak is the image maximum inside the track's box. Callers render each track into its
own zero image (Task 4 does).

- [ ] **Step 6: Run the tests and see them pass**

Run: `cd star-lab && python3 -m unittest discover -s tests -v`
Expected: all of `TestRender` and `TestFits` PASS. If `test_subgrid_matches_exact_erf_within_1e3_of_peak` fails at
any FWHM, report the number and stop (Global Constraints).

- [ ] **Step 7: Commit and push (athenaeum-lab)**

```bash
cd /Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab
git add star-lab/starlab star-lab/tests
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "starlab: package, FITS I/O and the v2 renderer (exact erf Gaussians, os 15/9 sub-grid, tracks)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01AkvYzN6darz7u9XC2jYPTg"
git push origin main
```

---

### Task 2: Truth widths and the CRLB

**Files:**
- Create: `star-lab/starlab/shape.py`, `star-lab/starlab/crlb.py`
- Test: `star-lab/tests/test_shape.py`, `star-lab/tests/test_crlb.py`

**Interfaces:**
- Consumes: `render.components`, `render.evaluate`, `render.render_star`, `render.FWHM_PER_SIGMA`,
  `render.moffat_alpha`.
- Produces:
  - `shape.fine_profile(comps, theta, fwhm_minor, fwhm_major, pixel_box=False, blur_fwhms=()) -> (img, step)`.
  - `shape.halfmax_ellipse(img, step) -> (fwhm_minor, fwhm_major, ecc, theta_deg)`.
  - `shape.moment_ellipse(img, step, radius) -> (fwhm_minor_eq, fwhm_major_eq, ecc, theta_deg, mx, my)`.
  - `shape.point_fit_equivalent(comps, theta, fwhm_minor, fwhm_major, model, beta=None, blur_fwhms=()) ->
    (fwhm_minor, fwhm_major, ecc, theta_deg)`, where `model` is `'gauss' | 'moffat_fixed' | 'moffat_free'`.
  - `shape.truth_widths(comps, theta, fwhm_minor, fwhm_major, model, beta=None, blur_fwhms=()) -> dict`, with keys
    `fwhm_minor, fwhm_major, ecc, theta_deg, pix_fwhm_minor, pix_fwhm_major, pix_ecc, pix_theta_deg,
    fit_fwhm_minor, fit_fwhm_major, fit_ecc, fit_theta_deg`.
  - `crlb.fisher_snr(stamp_star, background, gain, rn) -> float`.
  - `crlb.bounds(render_fn, params, names, background, gain, rn, steps) -> dict[name -> sigma]`.
  - `crlb.star_bounds(comps_fn, x, y, flux, sky, gain, rn, fwhm_minor, fwhm_major, theta, ecc) -> dict`, with keys
    `x, y, flux, fwhm_minor, fwhm_major, ecc_or_q, theta, fisher_snr`.

- [ ] **Step 1: Write the failing tests**

`tests/test_shape.py`:

```python
"""Truth widths (spec §3.1, plan P-9)."""
import math, unittest
from starlab import render, shape


class TestShape(unittest.TestCase):
    def test_halfmax_ellipse_recovers_a_moffat_ellipse(self):
        fm, fM = 2.0, 2.0 / math.sqrt(1 - 0.5 ** 2)
        comps = render.components("moffat", fm, fM, beta=4.0)
        img, step = shape.fine_profile(comps, math.radians(30.0), fm, fM)
        mi, ma, e, th = shape.halfmax_ellipse(img, step)
        self.assertAlmostEqual(mi / fm, 1.0, delta=5e-3)
        self.assertAlmostEqual(ma / fM, 1.0, delta=5e-3)
        self.assertAlmostEqual(e, 0.5, delta=5e-3)
        self.assertAlmostEqual(th, 30.0, delta=0.5)

    def test_pixel_box_widens_a_gaussian(self):
        comps = render.components("gauss", 2.0, 2.0)
        img, step = shape.fine_profile(comps, 0.0, 2.0, 2.0, pixel_box=True)
        mi, ma, _, _ = shape.halfmax_ellipse(img, step)
        self.assertGreater(mi, 2.0)
        self.assertLess(mi, math.sqrt(4.0 + render.FWHM_PER_SIGMA ** 2 / 12.0) * 1.02)

    def test_point_fit_equivalent_of_a_gaussian(self):
        comps = render.components("gauss", 3.0, 3.0)
        mi, ma, e, _ = shape.point_fit_equivalent(comps, 0.0, 3.0, 3.0, "gauss")
        expect = math.sqrt(9.0 + render.FWHM_PER_SIGMA ** 2 / 12.0)
        self.assertAlmostEqual(mi / expect, 1.0, delta=0.01)
        self.assertAlmostEqual(ma / expect, 1.0, delta=0.01)
        self.assertLess(e, 0.05)

    def test_moment_ellipse_of_a_round_gaussian(self):
        comps = render.components("gauss", 3.0, 3.0)
        img, step = shape.fine_profile(comps, 0.0, 3.0, 3.0)
        mi, ma, e, _, mx, my = shape.moment_ellipse(img, step, 4.0 * 3.0)
        self.assertAlmostEqual(mi / 3.0, 1.0, delta=0.01); self.assertLess(e, 0.02)
        self.assertLess(abs(mx) + abs(my), 1e-6)
```

`tests/test_crlb.py`:

```python
"""CRLB gates (spec §6, §9; plan P-11)."""
import math, unittest
import numpy as np
from starlab import render, crlb


def erf_fisher_x(fwhm, flux, half=40):
    """Independent 1-D Fisher information of x for a pixel-integrated round Gaussian, Poisson only,
    centred on a pixel centre: I = F * sum_i g'_i^2 / g_i."""
    s = fwhm / render.FWHM_PER_SIGMA
    e = np.arange(-half, half + 2) - 0.5
    g = 0.5 * np.diff(render.erf(e / (math.sqrt(2) * s)))
    pdf = np.exp(-0.5 * (e / s) ** 2) / (math.sqrt(2 * math.pi) * s)
    dg = -(pdf[1:] - pdf[:-1])   # d/dx0 of the integral over [e_i, e_i+1] of N(x0, s), at x0 = 0
    m = g > 0
    return flux * np.sum(dg[m] ** 2 / g[m])


class TestCrlb(unittest.TestCase):
    def test_wide_gaussian_centroid_bound_is_sigma_over_sqrt_n(self):
        fwhm, n = 10.0, 1e5
        b = crlb.star_bounds(lambda fm, fM: render.components("gauss", fm, fM), 100.0, 100.0, n, 0.0, 1.0, 0.0,
                             fwhm, fwhm, 0.0, 0.0)
        ideal = fwhm / render.FWHM_PER_SIGMA / math.sqrt(n)
        self.assertAlmostEqual(b["x"] / ideal, 1.0, delta=0.005)

    def test_fwhm3_centroid_bound_matches_the_independent_erf_sum(self):
        fwhm, n = 3.0, 1e5
        b = crlb.star_bounds(lambda fm, fM: render.components("gauss", fm, fM), 100.0, 100.0, n, 0.0, 1.0, 0.0,
                             fwhm, fwhm, 0.0, 0.0)
        self.assertAlmostEqual(b["x"] * math.sqrt(erf_fisher_x(fwhm, n)), 1.0, delta=0.005)

    def test_fisher_snr_poisson_limit(self):
        stamp = np.full((10, 10), 100.0)   # 1e4 e- spread flat, no background
        self.assertAlmostEqual(crlb.fisher_snr(stamp, 0.0, 1.0, 0.0), 100.0, delta=1e-9)
```

- [ ] **Step 2: Run them and see them fail**

Run: `cd star-lab && python3 -m unittest tests.test_shape tests.test_crlb -v`
Expected: ImportError on `starlab.shape` / `starlab.crlb`.

- [ ] **Step 3: Implement `shape.py`**

```python
"""Truth widths per star (spec §3.1, plan P-9/P-10): the PSF before pixel integration, the PSF convolved
with the pixel box, and the point-sampled-fit equivalent. All widths in pixels, theta in degrees [0, 180)."""
import math
from functools import lru_cache
import numpy as np
from . import render


def fine_profile(comps, theta, fwhm_minor, fwhm_major, pixel_box=False, blur_fwhms=()):
    k = max(8, int(math.ceil(80.0 / fwhm_minor)))           # >= 40 samples per pixel of the minor FWHM (review I-3)
    R = 4.0 * max(fwhm_major, *(blur_fwhms or (0.0,)))
    n = 2 * int(math.ceil(R * k)) + 1
    c = (np.arange(n) - n // 2) / k
    X, Y = np.meshgrid(c, c)
    ct, st = math.cos(theta), math.sin(theta)
    img = render.evaluate(comps, X * ct + Y * st, -X * st + Y * ct)
    if pixel_box or blur_fwhms:
        F = np.fft.rfft2(np.fft.ifftshift(img))
        fy = np.fft.fftfreq(n)[:, None]; fx = np.fft.rfftfreq(n)[None, :]
        H = np.ones_like(F, dtype=np.float64)
        if blur_fwhms:
            H = sum(np.exp(-2 * math.pi ** 2 * (b * k / render.FWHM_PER_SIGMA) ** 2 * (fx * fx + fy * fy))
                    for b in blur_fwhms) / len(blur_fwhms)
        if pixel_box:
            H = H * np.sinc(fx * k) * np.sinc(fy * k)
        img = np.fft.fftshift(np.fft.irfft2(F * H, s=img.shape))
    return img, 1.0 / k


def _ellipse_from_cov(cov, scale):
    ev, evec = np.linalg.eigh(cov)
    ev = np.clip(ev, 0.0, None)
    minor, major = scale * math.sqrt(ev[0]), scale * math.sqrt(ev[1])
    th = math.degrees(math.atan2(evec[1, 1], evec[0, 1])) % 180.0
    ecc = math.sqrt(max(0.0, 1.0 - (minor / major) ** 2)) if major > 0 else 0.0
    return minor, major, ecc, th


def halfmax_ellipse(img, step):
    """The half-maximum region's ellipse: a filled ellipse of semi-axes A has covariance A^2/4, so FWHM = 4 sqrt(lambda)."""
    ys, xs = np.nonzero(img >= 0.5 * img.max())
    dx, dy = (xs - xs.mean()) * step, (ys - ys.mean()) * step
    return _ellipse_from_cov(np.cov(np.vstack([dx, dy]), bias=True), 4.0)


def moment_ellipse(img, step, radius):
    """Intensity second moments inside `radius`, reported as Gaussian-equivalent FWHMs (2.3548 sqrt(lambda))."""
    n = img.shape[0]; c = (np.arange(n) - n // 2) * step
    X, Y = np.meshgrid(c, c); m = X * X + Y * Y <= radius * radius
    w = img[m]; x = X[m]; y = Y[m]; s = w.sum()
    mx, my = (w * x).sum() / s, (w * y).sum() / s
    cxx = (w * (x - mx) ** 2).sum() / s; cyy = (w * (y - my) ** 2).sum() / s; cxy = (w * (x - mx) * (y - my)).sum() / s
    mi, ma, e, th = _ellipse_from_cov(np.array([[cxx, cxy], [cxy, cyy]]), render.FWHM_PER_SIGMA)
    return mi, ma, e, th, mx, my


def _lm(resid, p0, max_iter=200):
    p = np.array(p0, dtype=np.float64); r = resid(p); cost = r @ r; lam = 1e-3
    for _ in range(max_iter):
        J = np.empty((r.size, p.size))
        for i in range(p.size):
            h = 1e-6 * max(1.0, abs(p[i])); q = p.copy(); q[i] += h
            J[:, i] = (resid(q) - r) / h
        A = J.T @ J; g = J.T @ r
        improved = False
        while lam < 1e12:
            step = np.linalg.solve(A + lam * np.diag(np.diag(A) + 1e-30), -g)
            pn = p + step; rn = resid(pn); cn = rn @ rn
            if cn < cost:
                p, r, cost, lam, improved = pn, rn, cn, lam * 0.3, True
                break
            lam *= 10.0
        if not improved or np.abs(step).max() < 1e-10 * (1.0 + np.abs(p).max()):
            break
    return p


def point_fit_equivalent(comps, theta, fwhm_minor, fwhm_major, model, beta=None, blur_fwhms=()):
    """Fit a point-sampled elliptical model to the noiseless pixel-integrated star at phase (0.3, -0.2)."""
    R = int(math.ceil(3.0 * max(fwhm_major, *(blur_fwhms or (0.0,))))) + 2
    size = 2 * R + 1; x0, y0 = R + 0.3, R - 0.2
    img = np.zeros((size, size))
    render.render_star(img, x0, y0, 1.0, comps, theta, fwhm_minor, fwhm_major)
    if blur_fwhms:
        big = np.zeros((size + 64, size + 64)); big[32:-32, 32:-32] = img
        img = render.blur_image(big, list(blur_fwhms))[32:-32, 32:-32]
    Y, X = np.mgrid[0:size, 0:size].astype(np.float64)
    data = img.ravel()

    def model_eval(p):
        xc, yc, A, la, lb, th = p[:6]
        a, b = math.exp(la), math.exp(lb)
        ct, st = math.cos(th), math.sin(th)
        u = (X - xc) * ct + (Y - yc) * st; v = -(X - xc) * st + (Y - yc) * ct
        if model == "gauss":
            return A * np.exp(-0.5 * ((u / a) ** 2 + (v / b) ** 2))
        bb = beta if model == "moffat_fixed" else 1.0 + math.exp(p[6])
        return A * (1.0 + (u / a) ** 2 + (v / b) ** 2) ** (-bb)

    s0 = fwhm_minor / render.FWHM_PER_SIGMA
    if model == "gauss":
        p0 = [x0, y0, img.max(), math.log(s0 * fwhm_major / fwhm_minor), math.log(s0), theta]
    else:
        b0 = beta if model == "moffat_fixed" else 4.0
        p0 = [x0, y0, img.max(), math.log(render.moffat_alpha(fwhm_major, b0)),
              math.log(render.moffat_alpha(fwhm_minor, b0)), theta]
        if model == "moffat_free":
            p0.append(math.log(b0 - 1.0))
    p = _lm(lambda q: model_eval(q).ravel() - data, p0)
    a, b = math.exp(p[3]), math.exp(p[4])
    if model == "gauss":
        fa, fb = render.FWHM_PER_SIGMA * a, render.FWHM_PER_SIGMA * b
    else:
        bb = beta if model == "moffat_fixed" else 1.0 + math.exp(p[6])
        k = 2.0 * math.sqrt(2.0 ** (1.0 / bb) - 1.0); fa, fb = k * a, k * b
    mi, ma = min(fa, fb), max(fa, fb)
    th = math.degrees(p[5] if fa >= fb else p[5] + math.pi / 2) % 180.0
    return mi, ma, math.sqrt(max(0.0, 1.0 - (mi / ma) ** 2)), th


def truth_widths(comps, theta, fwhm_minor, fwhm_major, model, beta=None, blur_fwhms=()):
    img, step = fine_profile(comps, theta, fwhm_minor, fwhm_major, blur_fwhms=tuple(blur_fwhms))
    a = halfmax_ellipse(img, step)
    pimg, pstep = fine_profile(comps, theta, fwhm_minor, fwhm_major, pixel_box=True, blur_fwhms=tuple(blur_fwhms))
    b = halfmax_ellipse(pimg, pstep)
    c = point_fit_equivalent(comps, theta, fwhm_minor, fwhm_major, model, beta, tuple(blur_fwhms))
    if abs(fwhm_major - fwhm_minor) <= 1e-12 * fwhm_major:   # round generator (P-9): no ellipse to report
        c = (c[0], c[1], 0.0, float("nan"))
    keys = ("fwhm_minor", "fwhm_major", "ecc", "theta_deg")
    out = dict(zip(keys, a))
    out.update({"pix_" + k: v for k, v in zip(keys, b)})
    out.update({"fit_" + k: v for k, v in zip(keys, c)})
    return out
```

- [ ] **Step 4: Implement `crlb.py`**

```python
"""Fisher SNR and the Cramér-Rao lower bound per star (spec §6, plan P-11). Noise: white, per pixel,
var = (background + star) / gain + rn^2 in image units. The bound is a floor, never a target."""
import math
import numpy as np
from . import render


def fisher_snr(stamp_star, background, gain, rn):
    var = (np.asarray(background) + stamp_star) / gain + rn * rn
    var = np.maximum(var, 1e-300)
    return float(math.sqrt(np.sum(stamp_star * stamp_star / var)))


def _stamp(comps_fn, size, cx, cy, p):
    x, y, flux, B, fm, fM, th = p
    img = np.zeros((size, size))
    render.render_star(img, cx + x, cy + y, flux, comps_fn(fm, fM), th, fm, fM)
    return img + B


def star_bounds(comps_fn, x, y, flux, sky, gain, rn, fwhm_minor, fwhm_major, theta, ecc):
    """CRLB of (x, y, flux, B, fwhm_minor, fwhm_major, theta) on a 6 x FWHM square, by central differences.
    B is dropped when sky == 0; theta is dropped when ecc < 0.05 (then the bound of q = 1 - minor/major)."""
    half = int(math.ceil(3.0 * fwhm_major)); size = 2 * half + 1
    cx, cy = half + (x - round(x)), half + (y - round(y))
    p = np.array([0.0, 0.0, flux, sky, fwhm_minor, fwhm_major, theta])
    names = ["x", "y", "flux", "B", "fwhm_minor", "fwhm_major", "theta"]
    free = [0, 1, 2, 4, 5] + ([3] if sky > 0 else []) + ([6] if ecc >= 0.05 else [])
    steps = {0: 1e-3, 1: 1e-3, 2: 1e-4 * flux, 3: 1e-6 + 1e-4 * sky, 4: 1e-4 * fwhm_minor, 5: 1e-4 * fwhm_major, 6: 1e-4}
    mu = _stamp(comps_fn, size, cx, cy, p)
    var = np.maximum(mu / gain + rn * rn, 1e-300)
    D = []
    for i in free:
        dp = np.zeros_like(p); dp[i] = steps[i]
        D.append(((_stamp(comps_fn, size, cx, cy, p + dp) - _stamp(comps_fn, size, cx, cy, p - dp)) / (2 * steps[i])).ravel())
    D = np.array(D); w = 1.0 / var.ravel()
    Fm = (D * w) @ D.T
    C = np.linalg.inv(Fm)
    sig = {names[i]: math.sqrt(max(C[k, k], 0.0)) for k, i in enumerate(free)}
    im, iM = free.index(4), free.index(5)
    if ecc >= 0.05:
        r = fwhm_minor / fwhm_major
        g = np.zeros(len(free)); g[im] = -r / (fwhm_major * ecc); g[iM] = r * r / (fwhm_major * ecc)
        sig["ecc_or_q"] = math.sqrt(max(g @ C @ g, 0.0))
    else:
        g = np.zeros(len(free)); g[im] = -1.0 / fwhm_major; g[iM] = fwhm_minor / fwhm_major ** 2
        sig["ecc_or_q"] = math.sqrt(max(g @ C @ g, 0.0))
    star = mu - sky
    sig["fisher_snr"] = fisher_snr(star, sky, gain, rn)
    return sig
```

For a round star, `fwhm_minor` and `fwhm_major` are separate parameters perturbed independently. The generator's
`components(profile, fm, fM)` puts `fM` on the major axis at θ, so both directions are identifiable. If the Fisher
matrix is singular for a round star (check `np.linalg.cond(Fm) < 1e12`), report that in the task report instead of
regularizing.

- [ ] **Step 5: Run the tests and see them pass**

Run: `cd star-lab && python3 -m unittest tests.test_shape tests.test_crlb -v`
Expected: PASS. The two CRLB gates must hold:
- FWHM 10: `b["x"] / ideal` within 0.5 % (expected ≈ 1.0024);
- FWHM 3: within 0.5 % of the erf sum.

A miss stops the task with its numbers.

- [ ] **Step 6: Commit and push (athenaeum-lab)**

```bash
git add star-lab/starlab/shape.py star-lab/starlab/crlb.py star-lab/tests/test_shape.py star-lab/tests/test_crlb.py
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "starlab: truth widths (pre-pixel, pixel-box, point-fit equivalent) and the CRLB with its gates

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01AkvYzN6darz7u9XC2jYPTg"
git push origin main
```

---

### Task 3: Fields, noise, backgrounds, artefacts and OSC

**Files:**
- Create: `star-lab/starlab/field.py`, `noise.py`, `background.py`, `artefacts.py`, `osc.py`
- Test: `star-lab/tests/test_field.py`, `test_background.py`, `test_osc.py`

**Interfaces:**
- Consumes: `render.*`, `crlb.fisher_snr`.
- Produces:
  - `field.positions(n, size, margin, min_sep, rng) -> (x, y)`;
  - `field.flux_for_snr(snr, fwhm, sky, gain, rn) -> float`;
  - `field.flux_for_peak(peak, comps, theta, fwhm_minor, fwhm_major) -> float`;
  - `field.ladder_fluxes(n, fwhm, sky, gain, rn, sat_flux, rng, snr_lo=3.0, snr_hi=2000.0, sat_share=0.05)`;
  - `noise.apply(noiseless, gain, rn, sat, rng, nframes=1) -> img`;
  - `background.gradient(size, sky, frac)`, `background.vignetting(size, sky, frac)`;
  - `background.fractal(size, slope, rng) -> [0, 1] array`, `background.nebula(size, sky, amp, rng)`;
  - `background.local_sky_and_grad(bg, x, y) -> (sky_local, grad_per_px)`;
  - `artefacts.hot_pixels(img, n, rng) -> rows`, `artefacts.cosmic_rays(img, n, rng) -> rows`,
    `artefacts.hot_in_fit_region(img, stars, share, rng) -> rows`, where rows is `list[dict(type, x, y, value)]`;
  - `osc.channel_fluxes(flux, colour) -> (fR, fG, fB)`, `osc.FWHM_FACTOR = (1.00, 0.95, 1.10)`,
    `osc.SKY = (0.05, 0.06, 0.04)`;
  - `osc.mosaic(planes, pattern="RGGB") -> 2-D`, `osc.site_channel(pattern) -> 2×2 array of channel indices`.

- [ ] **Step 1: Write the failing tests**

`tests/test_field.py`:

```python
"""Star lists and noise (spec §3.2)."""
import math, unittest
import numpy as np
from starlab import field, noise, render, crlb


class TestField(unittest.TestCase):
    def test_min_sep_holds(self):
        x, y = field.positions(600, 2048, 40, 25.0, np.random.default_rng(1))
        self.assertEqual(len(x), 600)
        d = np.hypot(x[:, None] - x[None, :], y[:, None] - y[None, :]); np.fill_diagonal(d, 1e9)
        self.assertGreaterEqual(d.min(), 25.0)

    def test_flux_for_snr_round_trips_through_fisher_snr(self):
        fwhm, sky, gain, rn = 3.0, 0.05, 12500.0, 0.0005
        for snr in (5.0, 40.0, 500.0):
            f = field.flux_for_snr(snr, fwhm, sky, gain, rn)
            img = np.zeros((41, 41)); render.render_star(img, 20.0, 20.0, f, render.components("gauss", fwhm, fwhm), 0.0, fwhm, fwhm)
            got = crlb.fisher_snr(img, sky, gain, rn)
            self.assertAlmostEqual(got / snr, 1.0, delta=0.15)

    def test_ladder_fills_every_snr_bin(self):
        """approx_snr is the Gaussian n_eff estimate; Moffat 2.5 lands ~5 % lower. The per-family check in Task 4
        uses the Fisher SNR and the saturation-ceiling exemption (P-23)."""
        rng = np.random.default_rng(3)
        fwhm, sky, gain, rn = 3.0, 0.05, 12500.0, 0.0005
        f = field.ladder_fluxes(600, fwhm, sky, gain, rn, sat_flux=field.flux_for_peak(0.9, render.components("moffat", 3, 3), 0.0, 3, 3), rng=rng)
        snr = np.array([field.approx_snr(v, fwhm, sky, gain, rn) for v in f])
        edges = [0, 10, 20, 40, 100, 300, np.inf]
        counts = np.histogram(snr, bins=edges)[0]
        self.assertTrue((counts >= 50).all(), counts)

    def test_noise_clips_at_saturation(self):
        img = noise.apply(np.full((8, 8), 2.0), 12500.0, 0.0005, 0.95, np.random.default_rng(1))
        self.assertTrue((img <= 0.95).all())
```

`tests/test_background.py`:

```python
"""Backgrounds (spec §3.2, review I-10)."""
import unittest
import numpy as np
from starlab import background


class TestBackground(unittest.TestCase):
    def test_fractal_is_normalised_and_red(self):
        z = background.fractal(512, -3.0, np.random.default_rng(1))
        self.assertAlmostEqual(z.min(), 0.0); self.assertAlmostEqual(z.max(), 1.0)
        # red spectrum: neighbouring pixels are strongly correlated
        self.assertGreater(np.corrcoef(z[:, :-1].ravel(), z[:, 1:].ravel())[0, 1], 0.95)

    def test_nebula_amplitude_in_sky_units(self):
        n = background.nebula(256, 0.05, 3.0, np.random.default_rng(2))
        self.assertAlmostEqual(n.max(), 0.15, places=9); self.assertAlmostEqual(n.min(), 0.0, places=12)

    def test_local_sky_and_gradient(self):
        bg = background.gradient(128, 0.05, 0.10)
        s, g = background.local_sky_and_grad(bg, 64.0, 64.0)
        self.assertAlmostEqual(s, 0.05, delta=1e-4)
        self.assertAlmostEqual(g, 0.05 * 0.10 / 127.0, delta=1e-7)
```

`tests/test_osc.py`:

```python
"""OSC mosaic (spec §3.3, §9 mosaic gate; review focus 5)."""
import unittest
import numpy as np
from starlab import osc, crlb, render


class TestOsc(unittest.TestCase):
    def test_every_cfa_pixel_equals_its_channel(self):
        rng = np.random.default_rng(1)
        planes = rng.random((3, 6, 8))
        m = osc.mosaic(planes, "RGGB")
        sc = osc.site_channel("RGGB")
        for y in range(6):
            for x in range(8):
                self.assertEqual(m[y, x], planes[sc[y & 1, x & 1], y, x])
        self.assertEqual(sc.tolist(), [[0, 1], [1, 2]])

    def test_channel_snr_per_plane(self):
        fR, fG, fB = osc.channel_fluxes(1.0, 1.0)   # the reddest star
        self.assertGreater(fR, fB)
        snr = []
        for f, k in zip((fR, fG, fB), range(3)):
            fw = 3.0 * osc.FWHM_FACTOR[k]
            img = np.zeros((41, 41)); render.render_star(img, 20, 20, 1e-3 * f, render.components("moffat", fw, fw), 0.0, fw, fw)
            snr.append(crlb.fisher_snr(img, osc.SKY[k], 12500.0, 0.0005))
        self.assertGreater(snr[0], snr[2])
```

- [ ] **Step 2: Run them and see them fail**

Run: `cd star-lab && python3 -m unittest tests.test_field tests.test_background tests.test_osc -v`
Expected: ImportErrors.

- [ ] **Step 3: Implement the five modules**

`field.py`:

```python
"""Star lists: positions with a minimum separation, fluxes on an SNR ladder (spec §3.2)."""
import math
import numpy as np
from . import render


def positions(n, size, margin, min_sep, rng, max_tries=200):
    xs, ys = [], []
    cell = max(min_sep, 1.0); grid = {}
    tries = 0
    while len(xs) < n and tries < n * max_tries:
        tries += 1
        x = rng.uniform(margin, size - 1 - margin); y = rng.uniform(margin, size - 1 - margin)
        if min_sep > 0:
            cx, cy = int(x // cell), int(y // cell); ok = True
            for gx in (cx - 1, cx, cx + 1):
                for gy in (cy - 1, cy, cy + 1):
                    for j in grid.get((gx, gy), ()):
                        if math.hypot(xs[j] - x, ys[j] - y) < min_sep:
                            ok = False; break
                    if not ok: break
                if not ok: break
            if not ok:
                continue
            grid.setdefault((cx, cy), []).append(len(xs))
        xs.append(x); ys.append(y)
    if len(xs) < n:
        raise ValueError(f"positions: placed {len(xs)} of {n} at min_sep {min_sep} on {size}^2")
    return np.array(xs), np.array(ys)


def approx_snr(flux, fwhm, sky, gain, rn):
    """Gaussian-equivalent optimal SNR, electrons: n_eff = 4 pi sigma^2."""
    s = fwhm / render.FWHM_PER_SIGMA; n_eff = 4.0 * math.pi * s * s
    fe = flux * gain; v = sky * gain + (rn * gain) ** 2
    return fe / math.sqrt(fe + n_eff * v)


def flux_for_snr(snr, fwhm, sky, gain, rn):
    s = fwhm / render.FWHM_PER_SIGMA; n_eff = 4.0 * math.pi * s * s
    v = sky * gain + (rn * gain) ** 2; q = snr * snr
    fe = 0.5 * (q + math.sqrt(q * q + 4.0 * q * n_eff * v))
    return fe / gain


def flux_for_peak(peak, comps, theta, fwhm_minor, fwhm_major):
    img = np.zeros((161, 161)); _, pk, _ = render.render_star(img, 80.0, 80.0, 1.0, comps, theta, fwhm_minor, fwhm_major)
    return peak / pk


def ladder_fluxes(n, fwhm, sky, gain, rn, sat_flux, rng, snr_lo=3.0, snr_hi=2000.0, sat_share=0.05):
    """Log-uniform in SNR over [snr_lo, snr_hi] for (1 - sat_share) of the stars, the rest saturated
    (flux 1.5x to 4x sat_flux). Brightest first."""
    n_sat = int(round(n * sat_share)); n_lad = n - n_sat
    snr = np.exp(rng.uniform(math.log(snr_lo), math.log(snr_hi), n_lad))
    f = np.array([flux_for_snr(s, fwhm, sky, gain, rn) for s in snr])
    f = np.minimum(f, 0.9 * sat_flux)
    fs = sat_flux * rng.uniform(1.5, 4.0, n_sat)
    return np.sort(np.concatenate([f, fs]))[::-1]
```

`noise.py`:

```python
"""Noise: Poisson on sky + star (gain e-/unit), Gaussian read noise, clip at saturation and to [0, 1]."""
import math
import numpy as np


def apply(noiseless, gain, rn, sat, rng, nframes=1):
    g = gain * nframes; r = rn / math.sqrt(nframes)
    e = rng.poisson(np.clip(noiseless, 0.0, None) * g).astype(np.float64)
    img = e / g + rng.standard_normal(noiseless.shape) * r
    return np.clip(np.clip(img, 0.0, sat), 0.0, 1.0)
```

`background.py`:

```python
"""Structured backgrounds (spec §3.2 `background`, `halo`; review I-10): additive, in image units."""
import math
import numpy as np


def gradient(size, sky, frac):
    x = np.arange(size, dtype=np.float64)[None, :]
    return np.broadcast_to(sky * (1.0 + frac * (x / (size - 1) - 0.5)), (size, size)).copy()


def vignetting(size, sky, frac):
    c = (size - 1) / 2.0; Y, X = np.mgrid[0:size, 0:size]
    r2 = ((X - c) ** 2 + (Y - c) ** 2) / (2 * c * c)
    return sky * (1.0 - frac * r2)


def fractal(size, slope, rng):
    ky = np.fft.fftfreq(size)[:, None]; kx = np.fft.rfftfreq(size)[None, :]
    k = np.sqrt(kx * kx + ky * ky); k[0, 0] = 1.0
    amp = k ** (slope / 2.0); amp[0, 0] = 0.0
    z = np.fft.irfft2((rng.standard_normal(amp.shape) + 1j * rng.standard_normal(amp.shape)) * amp, s=(size, size))
    return (z - z.min()) / (z.max() - z.min())


def nebula(size, sky, amp, rng, slope=-3.0):
    return amp * sky * fractal(size, slope, rng)


def local_sky_and_grad(bg, x, y):
    ix, iy = int(round(x)), int(round(y)); h, w = bg.shape
    ix = min(max(ix, 1), w - 2); iy = min(max(iy, 1), h - 2)
    gx = 0.5 * (bg[iy, ix + 1] - bg[iy, ix - 1]); gy = 0.5 * (bg[iy + 1, ix] - bg[iy - 1, ix])
    return float(bg[iy, ix]), float(math.hypot(gx, gy))
```

`artefacts.py`:

```python
"""Hot pixels and cosmic-ray worms with their own truth rows (spec §3.2 `artefacts`)."""
import numpy as np


def hot_pixels(img, n, rng):
    h, w = img.shape; rows = []
    for _ in range(n):
        x, y = int(rng.integers(2, w - 3)), int(rng.integers(2, h - 3)); v = float(rng.uniform(0.2, 1.0))
        img[y, x] = max(img[y, x], v); rows.append({"type": "hot", "x": x, "y": y, "value": v})
        if rng.random() < 0.3:
            img[y, x + 1] = max(img[y, x + 1], v); rows.append({"type": "hot2", "x": x + 1, "y": y, "value": v})
    return rows


def cosmic_rays(img, n, rng):
    h, w = img.shape; rows = []
    for _ in range(n):
        x, y = int(rng.integers(8, w - 9)), int(rng.integers(8, h - 9)); L = int(rng.integers(1, 7))
        a = float(rng.uniform(0.1, 0.8))
        for _ in range(L):
            img[y, x] += a; rows.append({"type": "cr", "x": x, "y": y, "value": a})
            x += int(rng.integers(-1, 2)); y += int(rng.integers(-1, 2))
    return rows


def hot_in_fit_region(img, stars, share, rng):
    rows = []
    for (sx, sy) in stars:
        if rng.random() >= share:
            continue
        r = rng.uniform(2.0, 4.0); a = rng.uniform(0, 2 * np.pi)
        x, y = int(round(sx + r * np.cos(a))), int(round(sy + r * np.sin(a))); v = float(rng.uniform(0.2, 1.0))
        img[y, x] = max(img[y, x], v); rows.append({"type": "hot_in_star", "x": x, "y": y, "value": v})
    return rows
```

`osc.py`:

```python
"""OSC: chromatic channel renders and the RGGB mosaic (spec §3.3)."""
import numpy as np

FWHM_FACTOR = (1.00, 0.95, 1.10)     # R, G, B x the family FWHM (chromatic seeing)
SKY = (0.05, 0.06, 0.04)
_PATTERNS = {"RGGB": [[0, 1], [1, 2]], "BGGR": [[2, 1], [1, 0]], "GRBG": [[1, 0], [2, 1]], "GBRG": [[1, 2], [0, 1]]}


def site_channel(pattern="RGGB"):
    return np.array(_PATTERNS[pattern])


def channel_fluxes(flux, colour):
    """colour in [0, 1]: 0 blue, 1 red. G carries the total `flux` unit."""
    return flux * (0.6 + 0.6 * colour), flux, flux * (1.3 - 0.8 * colour)


def mosaic(planes, pattern="RGGB"):
    sc = site_channel(pattern); n, h, w = planes.shape
    out = np.empty((h, w))
    for dy in (0, 1):
        for dx in (0, 1):
            out[dy::2, dx::2] = planes[sc[dy, dx], dy::2, dx::2]
    return out
```

- [ ] **Step 4: Run the tests and see them pass**

Run: `cd star-lab && python3 -m unittest tests.test_field tests.test_background tests.test_osc -v`
Expected: PASS. `test_ladder_fills_every_snr_bin` checks the ladder design. The real bins are checked per family in
Task 4.

- [ ] **Step 5: Commit and push (athenaeum-lab)**

```bash
git add star-lab/starlab/{field,noise,background,artefacts,osc}.py star-lab/tests/test_{field,background,osc}.py
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "starlab: SNR-ladder fields, noise, structured backgrounds, artefacts, the RGGB mosaic

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01AkvYzN6darz7u9XC2jYPTg"
git push origin main
```

---

### Task 4: Families, manifests and `make_v2`

**Files:**
- Create: `star-lab/starlab/families.py`, `star-lab/starlab/manifest.py`, `star-lab/bin/make_v2.py`
- Create (committed output): `star-lab/manifests/v2.json`
- Test: `star-lab/tests/test_families.py`, `star-lab/tests/test_manifest.py`

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces:
  - `families.FAMILIES: dict[str, dict]`: every family of spec §3.2 except `resampled` (Task 6) and `pairs` (P-15:
    referenced in place).
  - `families.build(name, spec, out_dir, seed) -> list[dict]`.
  - `families.snr_bin_report(records) -> list[dict(bin, count, status)]`, where status is `ok`, `short` or
    `n/a (ceiling <snr_sat>)` (P-23). Each image record is
    `{"family", "name", "path", "truth", "extra": {...}, "seed", "sha256"}`.
  - The truth CSV columns, in this order:
    ```
    id,x,y,flux,flux_img,peak,peak_snr,fisher_snr,saturated,edge,profile,beta,
    fwhm_minor,fwhm_major,ecc,theta_deg,pix_fwhm_minor,pix_fwhm_major,pix_ecc,pix_theta_deg,
    fit_fwhm_minor,fit_fwhm_major,fit_ecc,fit_theta_deg,sky_local,sky_grad,s_true,plane,pair_id,kind
    ```
    `kind` is `star | halo | double_a | double_b | track | track2`.
  - The `coma` and `trails` truth adds `mom_fwhm_minor, mom_fwhm_major, mom_ecc, mom_theta_deg, mom_x, mom_y`.
  - Every truth CSV also carries the CRLB columns of Task 2's `crlb.star_bounds` for the sampled stars (P-11: 50 per
    SNR bin, at most 300 per image), `crlb_x, crlb_y, crlb_flux, crlb_fwhm_minor, crlb_fwhm_major, crlb_ecc_or_q,
    crlb_theta`, empty for unsampled stars. The model bound is the scorecard's (P-18), so there are no `mcrlb_*`
    columns here.
  - Every image also gets its 16-bit sibling `<name>.u16.fits` (P-20), carried as `extra.u16`.
  - **Names:** every FWHM in a family name is written with one decimal: `f2.0`, `f3.5`, `f8.0`.
  - **`pairs`:** `make_v2.py` writes a `pairs` entry whose images point at the Task 16b manifests in place
    (`~/.athenaeum-acc/star-lab/grid/main/manifest.json`, `…/grid/iso/manifest.json`; P-15). Check their real file
    names with `ls` first. Each reference/target pair is listed with its `s_true`.
  - `artefacts.csv` per image: `type,x,y,value`.
  - `manifest.write(path, records)`, `manifest.load(path)`, `manifest.verify(record, regenerate_fn) -> (ok, detail)`.

**The family table** (`FAMILIES`). Defaults: size 2048, sky 0.05, gain 50000 (P-23), rn 0.0005, sat 0.95, margin 40.

| name | images | parameters |
| ---- | ---- | ---- |
| `sharp/<prof>/f<F>` | 1 per (F, prof) | F ∈ {1.2, 1.6, 2.0, 2.6, 3.5, 5.0, 8.0}; prof ∈ {gauss, m2.5, m4, m10, blur1, blur2.5}. blurX = Moffat 4 core of FWHM F/√(1+X²) convolved with a Gaussian of X × the core FWHM, so the total is ≈ F. 150 stars/Mpx (629 stars), min-sep 25. |
| `shape/f<F>/e<e>/t<θ>` | 1 each | F ∈ {2.6, 4.0}, e ∈ {0, 0.3, 0.5, 0.7}, θ ∈ {0, 30, 60, 90}, Moffat 4, density as `sharp` (e = 0 renders once per F) |
| `coma` | 1 | F 3.0; a Moffat 4 core (70 %) plus a Moffat 4 (30 %) displaced radially (away from the centre of the frame) by 0.5 F at the centre, growing linearly with radius to 1.0 F at the corners. Moment and half-max truth per star (P-10). |
| `fieldpsf` | 1 | F(r) = 3.0 × (1 + 0.3 (r/r_c)²), e(r) = 0.3 (r/r_c)², θ tangential, Moffat 4, r_c = the corner distance |
| `density/d<D>` | 1 each | D ∈ {5, 40, 200, 600, 1000} stars/Mpx, F 3.0, Moffat 4, min-sep 0 |
| `background/<kind>` | 1 each | kinds: flat; grad10 (10 % across); vig20; neb0.1, neb0.5, neb3 (fractal slope −3, amplitude × sky); `neb3_4096` at 4096² for the `analysis` path. F 3.0, Moffat 4, density as `sharp`. |
| `halo` | 1 | the `sharp/m4/f4.0` field plus one star at the centre: core Moffat 4 at F 4.0 with flux 2000 × `flux_for_peak(sat)`, and 10 % of its flux in a Moffat β 2 halo of FWHM 50 px (light to ≈ 250 px), cutout cap 300 |
| `saturated/f<F>/n<N>` | 1 each | F ∈ {2.6, 4.0}, Moffat 4. The 64 brightest have exactly N clipped (N ∈ {0, 5, 20, 60}): those N get peaks 1.5–4 × (sat − sky), the other 64 − N get 0.3–0.9. The ladder below them. Plus `saturated/f2.6/n20_14bit` with sat = 0.25 (P-14). |
| `mixture/<set>[_clip]` | 1 each | equal-weight mixtures of Moffat 4 at FWHM {2.6, 3.4, 4.5} (`a`) and {3.0, 5.0, 7.0} (`b`); `_clip` = the P-16 winsorized mean at the 70th percentile across members |
| `twins/<base>/{t1,t2,deep}` | 3 per base | base ∈ every `sharp` and `mixture` image: the same field and truth, noise seeds +1 and +2, and `deep` at 2 × the flux (seed +3) |
| `ladder/l<i>` | 8 | one field (density as `sharp`); frame i = 0…7 has FWHM 2.0 + 4.0·i/7; i ≤ 3 Moffat with β 2.5 + 0.5·i, i ≥ 4 Moffat 4 core convolved with a Gaussian whose share rises (blur ratio (i − 3)·0.6) at the same total FWHM |
| `doubles/f<F>` | 1 each | F ∈ {2.6, 4.0}; 300 pairs on a jittered grid (pitch 60 px), separations {2, 3, 5, 7, 10} px × ratios {1:1, 1:4}, random orientation; `kind` double_a (brighter) / double_b, shared `pair_id` |
| `trails` | 1 | F 2.6 seeing; 200 straight tracks of 3–15 px, 100 curved (a quarter circle of radius 10 px, arc 4–15 px), 50 two-head tracks (heads 7 px apart, each 40 %, a bridge of 20 % of the heads), random angles; `kind` track / track2 |
| `artefacts` | 1 | the `sharp/m4/f2.6` field + 40 hot pixels/Mpx + 30 cosmic rays/Mpx + `hot_in_fit_region` share 0.05 |
| `osc/f<F>/<prof>` | 1 each | F ∈ {2.0, 2.6, 3.5, 5.0}, prof ∈ {m4, blur1}. Per star colour ~ U(0, 1); channels rendered at F × `FWHM_FACTOR[k]` with `SKY[k]`. Mosaic RGGB, noise on the mosaic (gain, rn, sat); ideal planes = each channel full-res with its own noise (seed +10 + k). Files: `<name>.fits` (mosaic, `BAYERPAT='RGGB'`), `<name>.ideal.fits` (3-plane), truth with `plane` 0/1/2 rows per star. |
| `osc_pairs/f<F>` | 1 ref + 3 targets per F | F ∈ {2.6, 3.5}: the reference is the `osc` field at F with `nframes` 20; targets at F × {1.0, 1.25, 1.5} with s_true {1.0, 0.8, 1.25}; truth `s_true`, `pair_id` = the reference name |
| `density_pairs/d<D>` | 1 ref + 3 targets per D | D ∈ {200, 600}; reference F 3.0 `nframes` 20; targets F {3.0, 4.0, 5.0}, s_true {1.0, 0.8, 1.25} |
| `truthcheck` | 1 ref + 4 targets | 300 stars, min-sep 120, Moffat 4; reference F 3.0 `nframes` 20; targets F {3.0, 4.5, 6.0, 7.5}, s_true {1.0, 0.9, 1.1, 0.8} |
| `truthcrowd` | 1 ref + 3 targets | `density_pairs/d600` re-used as is (a manifest alias) |

**SNR-bin check.** After building each family image, compute every truth star's Fisher SNR (`crlb.fisher_snr` on its
own noiseless stamp). For `sharp`, `shape`, `saturated`, `mixture` and `osc` (per plane), every bin of
{< 10, 10–20, 20–40, 40–100, 100–300, > 300} must hold at least 50 stars across the family's images (for `shape`,
per (F, e)). A bin whose lower edge is above the image's saturation ceiling `snr_sat` (the Fisher SNR at 0.9 × the
saturating flux) is reported `n/a (ceiling snr_sat)` and does not fail (P-23). If any other bin falls short, raise the
family's image count (`images` key) and rebuild. Never change the bins.

**Saturated flag (Review Focus 4).** `saturated` = 1 when any pixel of sky + noiseless star inside the star's cutout
reaches `sat`. It is computed on the final noiseless image: after blur, mixture and background, before noise.

**Truth widths.** Use `shape.truth_widths` per distinct shape (P-10 cache key: profile, F rounded to 1e-6, e, θ,
blur list), with model:
- `gauss` for Gaussian;
- `moffat_fixed` with β for pure Moffat;
- `moffat_free` otherwise.

`coma` and `trails` also get `shape.moment_ellipse` (radius 4F) per star, plus the half-max ellipse of their own
fine profile.

`edge` = 1 when the star's 6F square leaves the frame.

- [ ] **Step 1: Write the failing tests**

`tests/test_families.py`:

```python
"""Family builders on small sizes (spec §3.2; review focus 4)."""
import os, tempfile, unittest, csv
import numpy as np
from starlab import families, fitsio


def rows(path):
    with open(path) as f:
        return list(csv.DictReader(f))


class TestFamilies(unittest.TestCase):
    def build(self, name, **over):
        spec = dict(families.FAMILIES[name]); spec.update(over)
        d = tempfile.mkdtemp()
        return families.build(name, spec, d, seed=11)

    def test_sharp_truth_columns_and_flux_img(self):
        recs = self.build("sharp/m4/f2.6", size=256, density=150)
        r = rows(recs[0]["truth"])
        self.assertEqual(list(r[0].keys())[:12], ["id", "x", "y", "flux", "flux_img", "peak", "peak_snr", "fisher_snr",
                                                  "saturated", "edge", "profile", "beta"])
        img, _ = fitsio.read_fits(recs[0]["path"])
        self.assertEqual(img.shape, (256, 256))
        u16, _ = fitsio.read_fits(recs[0]["extra"]["u16"])
        self.assertGreater(u16.max(), 1.5)

    def test_saturated_flag_after_blur(self):
        recs = self.build("sharp/blur2.5/f3.5", size=256, density=150)
        r = rows(recs[0]["truth"])
        img, _ = fitsio.read_fits(recs[0]["path"])
        for s in r:
            if s["saturated"] == "1":
                x, y = int(round(float(s["x"]))), int(round(float(s["y"])))
                self.assertGreaterEqual(img[y - 3:y + 4, x - 3:x + 4].max(), 0.95 - 1e-6)

    def test_saturated_family_has_exactly_n_clipped_among_the_64_brightest(self):
        recs = self.build("saturated/f2.6/n20", size=512)
        r = sorted(rows(recs[0]["truth"]), key=lambda s: -float(s["flux"]))[:64]
        self.assertEqual(sum(int(s["saturated"]) for s in r), 20)

    def test_saturation_ceiling_exempts_only_bins_above_it(self):
        recs = self.build("sharp/m4/f1.2", size=512, density=600)
        report = families.snr_bin_report(recs)
        self.assertTrue(all(b["count"] >= 50 or b["status"].startswith("n/a (ceiling") for b in report))

    def test_twins_share_truth_and_differ_in_noise(self):
        recs = self.build("twins/sharp/m4/f2.6", size=256, density=150)
        t1, t2 = recs[0], recs[1]
        self.assertEqual(open(t1["truth"]).read(), open(t2["truth"]).read())
        a, _ = fitsio.read_fits(t1["path"]); b, _ = fitsio.read_fits(t2["path"])
        self.assertGreater(np.abs(a - b).max(), 0)

    def test_osc_writes_a_bayer_mosaic_and_ideal_planes(self):
        recs = self.build("osc/f2.6/m4", size=256, density=150)
        m, h = fitsio.read_fits(recs[0]["path"]); ideal, _ = fitsio.read_fits(recs[0]["extra"]["ideal"])
        self.assertEqual(m.ndim, 2); self.assertEqual(ideal.shape[0], 3)
        self.assertIn("RGGB", h["BAYERPAT"])

    def test_doubles_pair_ids(self):
        recs = self.build("doubles/f2.6", size=512)
        r = rows(recs[0]["truth"])
        a = [s for s in r if s["kind"] == "double_a"]; b = [s for s in r if s["kind"] == "double_b"]
        self.assertEqual(len(a), len(b)); self.assertEqual({s["pair_id"] for s in a}, {s["pair_id"] for s in b})
```

`tests/test_manifest.py`:

```python
"""Manifests and determinism (spec §3.4, review M-2)."""
import os, tempfile, unittest
import numpy as np
from starlab import families, manifest


class TestManifest(unittest.TestCase):
    def test_regenerated_image_hash_matches(self):
        d = tempfile.mkdtemp()
        spec = dict(families.FAMILIES["sharp/m4/f2.6"]); spec.update(size=128, density=150)
        recs = families.build("sharp/m4/f2.6", spec, d, seed=5)
        p = os.path.join(d, "m.json"); manifest.write(p, {"sharp/m4/f2.6": {"spec": spec, "images": recs}})
        m = manifest.load(p)
        ok, detail = manifest.verify(m["families"]["sharp/m4/f2.6"]["images"][0],
                                     lambda: families.build("sharp/m4/f2.6", spec, tempfile.mkdtemp(), seed=5)[0])
        self.assertTrue(ok, detail)
        self.assertEqual(m["numpy"], np.__version__)
```

- [ ] **Step 2: Run them and see them fail**

Run: `cd star-lab && python3 -m unittest tests.test_families tests.test_manifest -v`
Expected: ImportErrors.

- [ ] **Step 3: Implement `families.py`**

Structure: one builder per family kind, sharing `_render_field(...)`, which builds a noiseless star image, a truth
list and the per-star widths. Every family passes its `size`, `density`, `sky`, `gain`, `rn`, `sat` and `seed`
through the spec dict, so the tests can shrink `size`. The shared core:

```python
"""Family builders (spec §3.2): noiseless stars + background -> noise -> FITS + truth CSV (+ artefacts CSV)."""
import csv, hashlib, math, os
import numpy as np
from . import render, shape, crlb, field, noise, background, artefacts, osc, fitsio

BASE = dict(size=2048, sky=0.05, gain=50000.0, rn=0.0005, sat=0.95, margin=40, density=150, min_sep=25.0, images=1)
COLS = ["id", "x", "y", "flux", "flux_img", "peak", "peak_snr", "fisher_snr", "saturated", "edge", "profile", "beta",
        "fwhm_minor", "fwhm_major", "ecc", "theta_deg", "pix_fwhm_minor", "pix_fwhm_major", "pix_ecc", "pix_theta_deg",
        "fit_fwhm_minor", "fit_fwhm_major", "fit_ecc", "fit_theta_deg", "sky_local", "sky_grad", "s_true", "plane",
        "pair_id", "kind"]
MOM = ["mom_fwhm_minor", "mom_fwhm_major", "mom_ecc", "mom_theta_deg", "mom_x", "mom_y"]
_WIDTH_CACHE = {}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def profile_comps(prof, fwhm, ecc=0.0):
    """prof: gauss | m<beta> | blur<X>. Returns (comps_fn(fm, fM), blur_list(fm), model, beta)."""
    if prof == "gauss":
        return (lambda fm, fM: render.components("gauss", fm, fM)), (lambda fm: []), "gauss", None
    if prof.startswith("m"):
        b = float(prof[1:])
        return (lambda fm, fM: render.components("moffat", fm, fM, beta=b)), (lambda fm: []), "moffat_fixed", b
    if prof.startswith("blur"):
        X = float(prof[4:])
        core = lambda fm: fm / math.sqrt(1.0 + X * X)
        return ((lambda fm, fM: render.components("moffat", core(fm), core(fM), beta=4.0)),
                (lambda fm: [X * core(fm)]), "moffat_free", None)
    raise ValueError(prof)


def widths(prof, fm, fM, theta, comps_fn, blur_fn, model, beta):
    key = (prof, round(fm, 6), round(fM, 6), round(theta, 9))
    if key not in _WIDTH_CACHE:
        _WIDTH_CACHE[key] = shape.truth_widths(comps_fn(*_core(prof, fm, fM)), theta, *_core(prof, fm, fM), model, beta,
                                               tuple(blur_fn(fm)))
    return _WIDTH_CACHE[key]


def _core(prof, fm, fM):
    if prof.startswith("blur"):
        X = float(prof[4:]); k = math.sqrt(1.0 + X * X)
        return fm / k, fM / k
    return fm, fM


def write_truth(path, rows, extra_cols=()):
    with open(path, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=COLS + list(extra_cols), extrasaction="ignore")
        w.writeheader()
        for r in rows:
            w.writerow(r)
```

Then `_render_field(spec, rng, xs, ys, fluxes, prof, fm, fM, theta, extra_bg=None, blur_after=True)`:
1. Build `star = np.zeros((size, size))`.
2. Call `render.render_star(star, x, y, f, comps_fn(*_core(prof, fm, fM)), theta, *_core(prof, fm, fM))` per star.
   It returns `flux_img` and the box.
3. If the profile has a blur, `star = render.blur_image(star, blur_fn(fm))`. Then recompute each star's `flux_img`
   as the blurred image's sum over its box enlarged by `3 × blur FWHM`. Recompute `peak` as the blurred maximum in
   its box.
4. Build `bg = sky + extra_bg`.
5. `noiseless = bg + star`. Set `saturated` from `noiseless` over the box (`>= sat`), and `edge` from the 6F square.
6. `img = noise.apply(noiseless, gain, rn, sat, rng)`.
7. Per star: `fisher_snr` from the star's own noiseless stamp (the box of `star`); `peak_snr = peak /
   sqrt(sky / gain + rn²)`; `sky_local` and `sky_grad` from `background.local_sky_and_grad(bg, x, y)`; the widths
   from `widths(...)`.

It returns `(img, rows, star, bg)`. Each family builder composes it. A family that needs a per-star shape
(`fieldpsf`, `coma`) renders star by star with its own (fm, fM, θ, comps), and computes widths per star with
`shape.truth_widths`. `coma` and `trails` add moment columns via `shape.fine_profile` + `shape.moment_ellipse`
(trails: the track rendered with `render_track` into a 129² zero stamp, then measured with `shape.moment_ellipse`
at step 1 and radius 4F + track length).

Generate `background/neb3_4096` last: its blur FFT peaks near 0.5 GB.

Write `<out>/<family path>/<name>.fits` with cards `{"SYNFAM": name, "SYNSEED": seed, "SYNPROF": prof, "SYNFWHM":
F}` (plus `BAYERPAT` for OSC), and `<name>.csv`, and the 16-bit sibling `<name>.u16.fits` through `fitsio.write_fits_u16` with the same cards (P-20).
`build()` returns the records with `sha256` filled (of the float file) and `extra.u16`.

`twins` builds take the base family's spec and render it three times with the same field seed and noise seeds
`seed + 1, + 2`, then `+ 3` with fluxes × 2. **They reuse the base image's truth CSV verbatim** (one
`shape`/`crlb` pass per base, P-11). The `deep` twin's truth is the base truth with `flux`, `flux_img`, `peak`,
`peak_snr`, `fisher_snr` and `crlb_*` recomputed for the doubled flux. The widths are the same. Implement this by passing `noise_seed` separately from
`field_seed` in every builder.

- [ ] **Step 4: Implement `manifest.py`**

```python
"""Manifests: family specs, image records (path, truth, seed, sha256), the numpy version (spec §3.4)."""
import json, time
import numpy as np
from . import fitsio


def write(path, families):
    doc = {"version": 2, "numpy": np.__version__, "created": time.strftime("%Y-%m-%dT%H:%M:%S"), "families": families}
    with open(path, "w") as f:
        json.dump(doc, f, indent=1, sort_keys=True)


def load(path):
    with open(path) as f:
        return json.load(f)


def verify(record, regenerate):
    """Regenerate one image; equal hash passes, else max |delta| <= 1e-7 with an identical truth CSV passes."""
    new = regenerate()
    if new["sha256"] == record["sha256"]:
        return True, "hash equal"
    a, _ = fitsio.read_fits(record["path"]); b, _ = fitsio.read_fits(new["path"])
    d = float(np.abs(a - b).max())
    same_truth = open(record["truth"]).read() == open(new["truth"]).read()
    return (d <= 1e-7 and same_truth), f"max |delta| {d:.3e}, truth identical {same_truth}"
```

- [ ] **Step 5: Implement `bin/make_v2.py`**

```python
#!/usr/bin/env python3
"""Generate the v2 families into DATA_ROOT and write star-lab/manifests/v2.json.

  make_v2.py [--families a,b,...] [--out DIR] [--check]

--check regenerates the first image of every family and verifies it against the manifest (spec §3.4)."""
import argparse, os, sys, subprocess
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from starlab import DATA_ROOT, families, manifest

HERE = os.path.dirname(os.path.abspath(__file__))
MANIFEST = os.path.join(HERE, "..", "manifests", "v2.json")


def mem_ok():
    out = subprocess.run(["memory_pressure"], capture_output=True, text=True).stdout.strip().splitlines()[-1]
    pct = int(out.rsplit(":", 1)[1].strip().rstrip("%"))
    return pct >= 30, out


def main():
    ap = argparse.ArgumentParser(); ap.add_argument("--families", default="")
    ap.add_argument("--out", default=DATA_ROOT); ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    names = [n for n in a.families.split(",") if n] or list(families.FAMILIES)
    doc = manifest.load(MANIFEST) if os.path.exists(MANIFEST) else {"families": {}}
    if a.check:
        bad = 0
        for n in names:
            rec = doc["families"][n]["images"][0]; spec = doc["families"][n]["spec"]
            ok, d = manifest.verify(rec, lambda: families.build(n, spec, os.path.join(a.out, "_check"), seed=rec["seed"])[0])
            print(f"{'ok ' if ok else 'BAD'} {n}: {d}"); bad += 0 if ok else 1
        sys.exit(1 if bad else 0)
    for n in names:
        ok, line = mem_ok()
        if not ok:
            sys.exit(f"memory_pressure below 30 % free ({line}); stop")
        spec = families.FAMILIES[n]
        recs = families.build(n, spec, a.out, seed=spec.get("seed", 1))
        doc["families"][n] = {"spec": spec, "images": recs}
        manifest.write(MANIFEST, doc["families"])
        print(f"{n}: {len(recs)} image(s)")


if __name__ == "__main__":
    main()
```

- [ ] **Step 6: Run the tests and see them pass**

Run: `cd star-lab && python3 -m unittest tests.test_families tests.test_manifest -v`
Expected: PASS.

- [ ] **Step 7: Generate every family and check the bins and the budget**

Run (one process, memory check first):

```bash
memory_pressure | tail -1
cd star-lab && python3 bin/make_v2.py && python3 bin/make_v2.py --check
du -sh ~/.athenaeum-acc/star-lab-v2
```

Expected:
- every family is built;
- `--check` prints `ok` for every family;
- the SNR-bin check passes;
- `du` reports at most 9 GB (10 GB minus the room `resampled` and injection need).

Put the bin counts per family, the disk use and the wall time in the report.

- [ ] **Step 8: Commit and push (athenaeum-lab)**

```bash
git add star-lab/starlab/families.py star-lab/starlab/manifest.py star-lab/bin/make_v2.py star-lab/manifests/v2.json star-lab/tests/test_families.py star-lab/tests/test_manifest.py
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "starlab: the v2 families (sharp, shape, coma, fieldpsf, density, background, halo, saturated, mixture, twins/ladder, doubles, trails, artefacts, osc, pairs, truth checks) and their manifest

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01AkvYzN6darz7u9XC2jYPTg"
git push origin main
```

---

### Task 5: The seeds extraction and `star_probe` (athenaeum)

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/measure.rs`. Extract `measure_plane_detailed`; `measure_plane_with_seeds`
  becomes a one-line wrapper. Add one test.
- Modify: `crates/athenaeum-core/src/stacking/register/detect.rs`. `pub(crate) fn stars_from_fits` → `pub fn`, with a
  doc line.
- Create: `crates/athenaeum-core/examples/star_probe.rs`
- Modify: `crates/athenaeum-core/Cargo.toml`. Add
  `[[example]] name = "star_probe" required-features = ["render", "solver"]` beside `measure_probe`'s entry:
  `stacking` is `cfg(all(render, solver))`, and the headless check would otherwise fail on the example.

**Interfaces:**
- Produces:
  - `athenaeum_core::stacking::measure::measure_plane_detailed(data, w, h, opts, pool, seed_source) ->
    (PlaneMeasurement, Vec<Seed>)`.
  - `stars_from_fits` is `pub`.
  - Binary `.superpowers/target-acc/release/examples/star_probe` with subcommands `measure`, `debayer`, `warp` and
    `dump-fits`. `warp` is a stub until Task 6.
  - The JSONL schema below, which Task 8's `probe_io.py` reads.

**JSONL schema** (one object per line, camelCase):
- **seed:** `{"kind":"seed","plane":p,"i":i,"x","y","peak","flux"}`. `peak` and `flux` are ÷ 65535 (P-5).
- **star:**
  - shared fields: `{"kind":"star","path":"measure|betas|register|analysis|artifact","plane":p,"beta":b|null,"x","y"}`;
  - the fit paths add `"background","amplitude","signal","peak5","fwhmX","fwhmY","fwhm","ecc","thetaDeg","residual"`
    (energies ÷ 65535; `peak5` null when NaN);
  - `register` adds `"flux","sigmaX","sigmaY"` (null without refinement);
  - `analysis` adds `"peak","flux","fwhm","fwhmX","fwhmY","ecc","thetaDeg","snr","hfr","beta","fitMethod","residual"`
    (the `StarMetric` fields, raw units).
- **frame:** `{"kind":"frame","path":...,"plane":p,"beta":b|null,"seeds":n,"fits":n,"statistic":"...","frameFwhm":v,
  "frameEcc":v,"options":{...},"conversion":"none|adu/65535","nonFinite":n,"wallMs":ms}`. `measure` adds
  `"channel":{ChannelMeasurement}`; `analysis` adds `"frame":{FrameAnalysis},"flipVertical":bool`.

- [ ] **Step 1: The failing extraction test (RED)**

Add to `stacking/measure.rs` tests:

```rust
    #[test]
    fn measure_plane_detailed_returns_the_seeds_it_fitted_and_the_same_measurement() {
        let (data, w, h) = field(7, 1.0, 0.002);
        let opts = MeasureOptions::default();
        let plain = measure_plane_with_seeds(&data, w, h, &opts, None, SeedSource::Fast);
        let (detailed, seeds) = measure_plane_detailed(&data, w, h, &opts, None, SeedSource::Fast);
        assert_eq!(seeds.len(), detailed.channel.stars_detected);
        assert_eq!(detailed.fits, plain.fits);
        assert_eq!(
            format!("{:?}", without_timings(detailed.channel.clone())),
            format!("{:?}", without_timings(plain.channel.clone()))
        );
    }
```

`without_timings` is the existing test helper in the same module.

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib stacking::measure -- --test-threads=4`
Expected: FAIL to compile (`measure_plane_detailed` not found).

- [ ] **Step 2: The extraction**

In `measure.rs`, rename the body of `measure_plane_with_seeds` to:

```rust
/// [`measure_plane_with_seeds`], also returning the seeds the fits were made
/// from (star measurement part 1: the lab's `star_probe` dumps them). Same
/// call, same numbers; the run never reads the seeds.
pub fn measure_plane_detailed(
    data: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    seed_source: SeedSource,
) -> (PlaneMeasurement, Vec<Seed>) {
    // … the former body of measure_plane_with_seeds, unchanged, except the tail:
    (
        PlaneMeasurement {
            channel,
            fits: outcome.fits,
        },
        seeds,
    )
}
```

Make the existing function a wrapper, keeping its doc comment:

```rust
pub fn measure_plane_with_seeds(
    data: &[f32],
    w: usize,
    h: usize,
    opts: &MeasureOptions,
    pool: Option<&Arc<rayon::ThreadPool>>,
    seed_source: SeedSource,
) -> PlaneMeasurement {
    measure_plane_detailed(data, w, h, opts, pool, seed_source).0
}
```

`seeds` is used by reference in `fit_stars(&scaled, w, h, &seeds, …)`, so returning it after that call needs no
clone.

In `register/detect.rs`:

```rust
/// A mono frame's registration star list from Measure's fits. `pub` for the
/// star measurement lab's probe (`examples/star_probe.rs`); the run calls it
/// from inside the crate.
pub fn stars_from_fits(
```

- [ ] **Step 3: Run the measure and register tests (GREEN, and the pins hold)**

Run:
```bash
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib stacking::measure -- --test-threads=4
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --lib stacking::register -- --test-threads=4
```
Expected: PASS, including the three pins named in P-2, untouched.

- [ ] **Step 4: Write `examples/star_probe.rs`**

```rust
//! Dev probe (star measurement part 1, spec
//! `docs/superpowers/specs/2026-10-06-star-truth-lab-design.md` §5): runs one image
//! through the production star-measurement entry points and writes every seed and
//! every measured star as JSON lines for the lab's scorecard (athenaeum-lab
//! `star-lab/`). Nothing here re-implements a measurement.
//!
//!   star_probe measure <image.fits> --out <file.jsonl> [--config <json>]
//!       [--plane all|N] [--debayer RGGB|BGGR|GRBG|GBRG] [--rect x,y,w,h]
//!       [--paths measure,betas,register,analysis] [--betas 2.5,4,6,10]
//!   star_probe debayer <mosaic.fits> --pattern RGGB --out <rgb.fits>
//!   star_probe warp <image.fits> --shift dx,dy --rotate deg --out <image.fits>
//!   star_probe dump-fits <fits artifact> --out <file.jsonl>
//!
//! Energies (background, amplitude, signal, peak5; seed peak and flux) are
//! written ÷ 65535, the lab's image units; production carries them ×65535.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use astroimage::processing::vng::vng_debayer_f32;
use astroimage::BayerPattern;
use athenaeum_core::analysis::analyzer::{analyze_frame, build_analyzer};
use athenaeum_core::analysis::config::AnalysisConfig;
use athenaeum_core::fits_writer::write_fits_f32;
use athenaeum_core::integration::plane_reader::PlaneReader;
use athenaeum_core::stacking::config::StackingConfig;
use athenaeum_core::stacking::fits_artifact;
use athenaeum_core::stacking::measure::{measure_plane_detailed, MeasureOptions, SeedSource, ADU_SCALE};
use athenaeum_core::stacking::psf_signal::{self, fit_stars_with_beta, FitParams, StarFit};
use athenaeum_core::stacking::register::detect::{detect_stars, luminance, stars_from_fits};
use athenaeum_core::stacking::register::{DetectionConfig, RegistrationConfig};
use serde_json::{json, Value};

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("star_probe: {msg}");
    std::process::exit(1)
}

fn read_planes(path: &Path) -> (Vec<Vec<f32>>, usize, usize) {
    let r = PlaneReader::open(path).unwrap_or_else(|e| die(format!("{}: {e}", path.display())));
    let planes: Vec<Vec<f32>> = (0..r.channels())
        .map(|p| r.read_plane(p).unwrap_or_else(|e| die(format!("{}: plane {p}: {e}", path.display()))))
        .collect();
    (planes, r.width(), r.height())
}

/// P-4: the harness rule — a plane above 1.5 is in the ADU domain.
fn normalise(planes: &mut [Vec<f32>]) -> &'static str {
    let adu = planes.iter().flatten().any(|v| v.is_finite() && *v > 1.5);
    if adu {
        planes.iter_mut().flatten().for_each(|v| *v /= ADU_SCALE);
        "adu/65535"
    } else {
        "none"
    }
}

fn pattern(s: &str) -> BayerPattern {
    match s {
        "RGGB" => BayerPattern::Rggb,
        "BGGR" => BayerPattern::Bggr,
        "GRBG" => BayerPattern::Grbg,
        "GBRG" => BayerPattern::Gbrg,
        o => die(format!("unknown Bayer pattern {o}")),
    }
}

fn debayer(plane: &[f32], w: usize, h: usize, pat: BayerPattern) -> Vec<Vec<f32>> {
    let rgb = vng_debayer_f32(plane, w, h, pat);
    let n = w * h;
    (0..3).map(|c| rgb[c * n..(c + 1) * n].to_vec()).collect()
}

fn crop(planes: &[Vec<f32>], w: usize, r: (usize, usize, usize, usize)) -> Vec<Vec<f32>> {
    let (x0, y0, cw, ch) = r;
    planes
        .iter()
        .map(|p| (0..ch).flat_map(|y| p[(y0 + y) * w + x0..(y0 + y) * w + x0 + cw].iter().copied()).collect())
        .collect()
}

fn n_or_null(v: f64) -> Value {
    if v.is_finite() { json!(v) } else { Value::Null }
}

fn fit_line(path: &str, plane: usize, beta: Option<f64>, f: &StarFit) -> Value {
    let s = ADU_SCALE as f64;
    json!({
        "kind": "star", "path": path, "plane": plane, "beta": beta.unwrap_or(f.beta),
        "x": f.x, "y": f.y,
        "background": f.background / s, "amplitude": f.amplitude / s, "signal": f.signal / s,
        "peak5": n_or_null(f.peak5 / s),
        "fwhmX": f.fwhm_x, "fwhmY": f.fwhm_y, "fwhm": f.fwhm(), "ecc": f.eccentricity(),
        "thetaDeg": f.theta.to_degrees(), "residual": f.residual,
    })
}

struct MeasureArgs {
    image: PathBuf,
    out: PathBuf,
    config: Option<PathBuf>,
    plane: Option<usize>,
    debayer: Option<String>,
    rect: Option<(usize, usize, usize, usize)>,
    paths: Vec<String>,
    betas: Vec<f64>,
}

fn parse_measure(mut it: impl Iterator<Item = String>) -> MeasureArgs {
    let image = PathBuf::from(it.next().unwrap_or_else(|| die("measure: <image> missing")));
    let mut a = MeasureArgs {
        image,
        out: PathBuf::new(),
        config: None,
        plane: None,
        debayer: None,
        rect: None,
        paths: vec!["measure".into(), "betas".into(), "register".into(), "analysis".into()],
        betas: vec![2.5, 4.0, 6.0, 10.0],
    };
    while let Some(k) = it.next() {
        let mut v = || it.next().unwrap_or_else(|| die(format!("{k}: value missing")));
        match k.as_str() {
            "--out" => a.out = PathBuf::from(v()),
            "--config" => a.config = Some(PathBuf::from(v())),
            "--plane" => {
                let s = v();
                a.plane = if s == "all" { None } else { Some(s.parse().unwrap_or_else(|_| die("--plane: all or N"))) }
            }
            "--debayer" => a.debayer = Some(v()),
            "--rect" => {
                let p: Vec<usize> = v().split(',').map(|t| t.parse().unwrap_or_else(|_| die("--rect x,y,w,h"))).collect();
                if p.len() != 4 { die("--rect x,y,w,h") }
                a.rect = Some((p[0], p[1], p[2], p[3]));
            }
            "--paths" => a.paths = v().split(',').map(String::from).collect(),
            "--betas" => a.betas = v().split(',').map(|t| t.parse().unwrap_or_else(|_| die("--betas"))).collect(),
            o => die(format!("unknown flag {o}")),
        }
    }
    if a.out.as_os_str().is_empty() { die("--out is required") }
    a
}

fn load_config(path: &Option<PathBuf>) -> (MeasureOptions, DetectionConfig, usize, bool) {
    match path {
        None => (MeasureOptions::default(), DetectionConfig::default(), RegistrationConfig::default().max_stars, false),
        Some(p) => {
            let text = std::fs::read_to_string(p).unwrap_or_else(|e| die(format!("{}: {e}", p.display())));
            let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| die(format!("{}: {e}", p.display())));
            let doc = v.get("config").cloned().unwrap_or(v);
            let cfg: StackingConfig = serde_json::from_value(doc).unwrap_or_else(|e| die(format!("config: {e}")));
            let opts = cfg.measurement.measure_options(cfg.normalization.scale_estimator);
            (opts, cfg.registration.detection, cfg.registration.max_stars, true)
        }
    }
}

/// P-20: the Analysis tab reads raw lights; its detector bins the background in integer ADU.
fn analysis_domain_ok(raw_max: f32) -> bool {
    raw_max > 1.5
}

fn cmd_measure(a: MeasureArgs) {
    let (mut planes, mut w, mut h) = read_planes(&a.image);
    let raw_max = planes.iter().flatten().filter(|v| v.is_finite()).fold(f32::MIN, |m, &v| m.max(v));
    let conversion = normalise(&mut planes);
    if let Some(p) = &a.debayer {
        if planes.len() != 1 { die("--debayer needs a one-plane mosaic") }
        planes = debayer(&planes[0], w, h, pattern(p));
    }
    if let Some(r) = a.rect {
        if r.0 + r.2 > w || r.1 + r.3 > h { die("--rect outside the image") }
        planes = crop(&planes, w, r);
        w = r.2;
        h = r.3;
    }
    let (opts, det, reg_max, from_config) = load_config(&a.config);
    let non_finite: usize = planes.iter().flatten().filter(|v| !v.is_finite()).count();
    let selected: Vec<usize> = match a.plane {
        Some(p) if p < planes.len() => vec![p],
        Some(p) => die(format!("plane {p} of {}", planes.len())),
        None => (0..planes.len()).collect(),
    };
    let file = std::fs::File::create(&a.out).unwrap_or_else(|e| die(format!("{}: {e}", a.out.display())));
    let mut out = BufWriter::new(file);
    let mut emit = |v: Value| writeln!(out, "{v}").unwrap_or_else(|e| die(format!("write: {e}")));
    let want = |p: &str| a.paths.iter().any(|q| q == p);
    let options = json!({ "measure": opts, "detection": { "minSnr": det.min_snr, "maxEccentricity": det.max_eccentricity },
                          "registerMaxStars": reg_max, "fromConfig": from_config, "rawMax": raw_max });

    let mut fits0: Option<Vec<StarFit>> = None;
    for &p in &selected {
        let need_measure = want("measure") || want("betas") || (want("register") && planes.len() == 1);
        if !need_measure { continue }
        let t = Instant::now();
        let (pm, seeds) = measure_plane_detailed(&planes[p], w, h, &opts, None, SeedSource::Fast);
        let wall = t.elapsed().as_millis() as u64;
        let s = ADU_SCALE as f64;
        if want("measure") {
            for (i, sd) in seeds.iter().enumerate() {
                emit(json!({"kind": "seed", "plane": p, "i": i, "x": sd.x, "y": sd.y, "peak": sd.peak / s, "flux": sd.flux / s}));
            }
            for f in &pm.fits { emit(fit_line("measure", p, None, f)) }
            emit(json!({"kind": "frame", "path": "measure", "plane": p, "beta": pm.channel.beta,
                        "seeds": seeds.len(), "fits": pm.fits.len(),
                        "statistic": "residual-weighted mean over all accepted fits",
                        "frameFwhm": pm.channel.fwhm_px, "frameEcc": pm.channel.eccentricity,
                        "channel": pm.channel, "options": options, "conversion": conversion,
                        "nonFinite": non_finite, "wallMs": wall}));
        }
        if want("betas") {
            let scaled: Vec<f32> = planes[p].iter().map(|v| v * ADU_SCALE).collect();
            for &b in &a.betas {
                let t = Instant::now();
                let o = fit_stars_with_beta(&scaled, w, h, &seeds, b, &FitParams::default(), None);
                let (ff, fe) = psf_signal::frame_shape(&o.fits).unwrap_or((0.0, 0.0));
                for f in &o.fits { emit(fit_line("betas", p, Some(b), f)) }
                emit(json!({"kind": "frame", "path": "betas", "plane": p, "beta": b, "seeds": seeds.len(),
                            "fits": o.fits.len(), "statistic": "residual-weighted mean over all accepted fits",
                            "frameFwhm": ff, "frameEcc": fe, "options": options, "conversion": conversion,
                            "nonFinite": non_finite, "wallMs": t.elapsed().as_millis() as u64}));
            }
        }
        if p == selected[0] { fits0 = Some(pm.fits.clone()) }
    }

    if want("register") {
        let t = Instant::now();
        let (stars, how) = if planes.len() == 3 {
            let lum = luminance(&[planes[0].as_slice(), planes[1].as_slice(), planes[2].as_slice()]);
            (detect_stars(&lum, w, h, &det, reg_max, None), "detect_stars on the RGB luminance")
        } else {
            let fits = fits0.clone().unwrap_or_else(|| die("register on a mono plane needs the measure fits"));
            (stars_from_fits(&fits, &det, reg_max), "stars_from_fits on the Measure fits")
        };
        for s in &stars {
            let (sx, sy) = s.sigma.map(|(a, b)| (json!(a), json!(b))).unwrap_or((Value::Null, Value::Null));
            emit(json!({"kind": "star", "path": "register", "plane": selected[0], "beta": null,
                        "x": s.x, "y": s.y, "flux": s.flux, "sigmaX": sx, "sigmaY": sy}));
        }
        emit(json!({"kind": "frame", "path": "register", "plane": selected[0], "beta": null, "seeds": null,
                    "fits": stars.len(), "statistic": how, "frameFwhm": null, "frameEcc": null,
                    "options": options, "conversion": conversion, "nonFinite": non_finite,
                    "wallMs": t.elapsed().as_millis() as u64}));
    }

    if want("analysis") {
        if a.rect.is_some() || a.debayer.is_some() {
            die("the analysis path takes the file as the Analysis tab does: no --rect, no --debayer")
        }
        if !analysis_domain_ok(raw_max) {
            die("the analysis path takes a raw-domain file (16-bit or ADU float), as the Analysis tab does; \
                 this file is in [0, 1] (use its .u16.fits sibling)")
        }
        let t = Instant::now();
        let analyzer = build_analyzer(&AnalysisConfig::default(), None);
        let img = a.image.to_str().unwrap_or_else(|| die("non-UTF-8 path"));
        match analyze_frame(img, &analyzer, "star_probe") {
            Ok((fa, stars, flip)) => {
                for s in &stars {
                    emit(json!({"kind": "star", "path": "analysis", "plane": 0, "beta": s.beta,
                                "x": s.x, "y": s.y, "peak": s.peak, "flux": s.flux, "fwhm": s.fwhm,
                                "fwhmX": s.fwhm_x, "fwhmY": s.fwhm_y, "ecc": s.eccentricity,
                                "thetaDeg": s.theta.to_degrees(), "snr": s.snr, "hfr": s.hfr,
                                "fitMethod": s.fit_method, "residual": s.fit_residual}));
                }
                emit(json!({"kind": "frame", "path": "analysis", "plane": 0, "beta": null, "seeds": null,
                            "fits": stars.len(), "statistic": "clipped weighted median over e <= 0.8",
                            "frameFwhm": fa.median_fwhm, "frameEcc": fa.median_eccentricity,
                            "frame": fa, "flipVertical": flip,
                            "options": { "analysis": AnalysisConfig::default() }, "conversion": "file as is (raw domain)",
                            "nonFinite": non_finite, "wallMs": t.elapsed().as_millis() as u64}));
            }
            Err(e) => die(format!("analysis: {e:#}")),
        }
    }
    out.flush().unwrap_or_else(|e| die(format!("flush: {e}")));
}

fn cmd_debayer(mut it: impl Iterator<Item = String>) {
    let input = PathBuf::from(it.next().unwrap_or_else(|| die("debayer: <mosaic> missing")));
    let (mut pat, mut out) = (String::from("RGGB"), PathBuf::new());
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_else(|| die(format!("{k}: value missing")));
        match k.as_str() { "--pattern" => pat = v, "--out" => out = PathBuf::from(v), o => die(format!("unknown flag {o}")) }
    }
    let (mut planes, w, h) = read_planes(&input);
    normalise(&mut planes);
    if planes.len() != 1 { die("debayer needs a one-plane mosaic") }
    let rgb: Vec<f32> = debayer(&planes[0], w, h, pattern(&pat)).concat();
    write_fits_f32(&out, w, h, 3, &rgb, &[]).unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
}

fn cmd_dump_fits(mut it: impl Iterator<Item = String>) {
    let input = PathBuf::from(it.next().unwrap_or_else(|| die("dump-fits: <artifact> missing")));
    let out = match (it.next().as_deref(), it.next()) {
        (Some("--out"), Some(p)) => PathBuf::from(p),
        _ => die("dump-fits <artifact> --out <file.jsonl>"),
    };
    let fits = fits_artifact::read_fits(&input).unwrap_or_else(|e| die(format!("{}: {e}", input.display())));
    let mut w = BufWriter::new(std::fs::File::create(&out).unwrap_or_else(|e| die(format!("{}: {e}", out.display()))));
    for f in &fits { writeln!(w, "{}", fit_line("artifact", 0, None, f)).unwrap_or_else(|e| die(e)) }
    w.flush().unwrap_or_else(|e| die(e));
}

fn main() {
    if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_writer(std::io::stderr)
            .init();
    }
    let mut it = std::env::args().skip(1);
    match it.next().as_deref() {
        Some("measure") => cmd_measure(parse_measure(it)),
        Some("debayer") => cmd_debayer(it),
        Some("dump-fits") => cmd_dump_fits(it),
        Some("warp") => die("warp: Task 6"),
        _ => die("usage: star_probe measure|debayer|warp|dump-fits …"),
    }
}
```

Verify before compiling, by reading the source:
- `StarFit::theta` is in radians (the fitter's `u = (x − x0) cos θ + …`, rustafits `fitting.rs`).
- `StarMetric::theta` is also in radians (rustafits `StarMetrics::theta`, copied in `analyzer.rs`), hence
  `.to_degrees()` on both.
- `FrameAnalysis`, `StarMetric`, `AnalysisConfig`, `ChannelMeasurement` and `MeasureOptions` implement `Serialize`.
- The `astroimage::processing::vng` path is public.

A mismatch is fixed in the probe, never by changing production code (the production diff is limited to P-2's two
items).

`--config` takes a run's `config_json` as is: it decodes directly as a `StackingConfig` through
`#[serde(default)]`. The `v.get("config")` branch is the fallback for a per-set document (`{"setId", "config",
…}`).

- [ ] **Step 5: Example tests inside the example** (Review Focus 1 and 2)

Append to `star_probe.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn gaussian_field(w: usize, h: usize, nan_border: usize, stars: bool) -> Vec<f32> {
        let mut d = vec![0.05f32; w * h];
        if stars {
            for k in 0..40 {
                let (cx, cy) = (30.0 + (k % 8) as f32 * 25.0, 30.0 + (k / 8) as f32 * 25.0);
                for y in 0..h { for x in 0..w {
                    let r2 = (x as f32 - cx).powi(2) + (y as f32 - cy).powi(2);
                    d[y * w + x] += 0.4 * (-r2 / (2.0 * 1.5f32.powi(2))).exp();
                }}
            }
        }
        // deterministic pseudo-noise (the MRS noise estimate needs some)
        for (i, v) in d.iter_mut().enumerate() {
            *v += 0.002 * (((i as u64).wrapping_mul(2_654_435_761) % 1000) as f32 / 1000.0 - 0.5);
        }
        for y in 0..h { for x in 0..w {
            if x < nan_border || y < nan_border { d[y * w + x] = f32::NAN }
        }}
        d
    }

    fn run(dir: &Path, data: Vec<f32>, w: usize, h: usize) -> Vec<Value> {
        let img = dir.join("p.fits");
        write_fits_f32(&img, w, h, 1, &data, &[]).unwrap();
        let out = dir.join("p.jsonl");
        cmd_measure(MeasureArgs { image: img, out: out.clone(), config: None, plane: None, debayer: None, rect: None,
            paths: vec!["measure".into(), "betas".into(), "register".into()], betas: vec![4.0] });
        std::fs::read_to_string(&out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    #[test]
    fn nan_border_runs_every_path() {
        let d = tempfile::tempdir().unwrap();
        let lines = run(d.path(), gaussian_field(240, 240, 6, true), 240, 240);
        let frames: Vec<&Value> = lines.iter().filter(|v| v["kind"] == "frame").collect();
        assert_eq!(frames.len(), 3);
        assert!(frames.iter().all(|f| f["nonFinite"].as_u64().unwrap() > 0));
        let measure = frames.iter().find(|f| f["path"] == "measure").unwrap();
        assert!(measure["fits"].as_u64().unwrap() > 0, "{measure}");
    }

    #[test]
    fn analysis_refuses_a_unit_domain_file() {
        assert!(!analysis_domain_ok(1.0));
        assert!(!analysis_domain_ok(f32::MIN));
        assert!(analysis_domain_ok(40000.0));
    }

    #[test]
    fn starless_plane_writes_frame_lines_only() {
        let d = tempfile::tempdir().unwrap();
        let lines = run(d.path(), gaussian_field(128, 128, 0, false), 128, 128);
        assert!(lines.iter().all(|v| v["kind"] == "frame"));
        assert_eq!(lines.len(), 3);
    }
}
```

If `tempfile` is not a dev-dependency of athenaeum-core, use `std::env::temp_dir().join(<unique name>)`.

- [ ] **Step 6: Build and test the example**

Run:
```bash
memory_pressure | tail -1
CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --example star_probe -- --test-threads=4
CARGO_BUILD_JOBS=4 cargo check -p athenaeum-core --examples
CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR=.superpowers/target-acc cargo build -p athenaeum-core --release --example star_probe
```
Expected: both example tests PASS, check clean, release binary at `.superpowers/target-acc/release/examples/star_probe`.

- [ ] **Step 7: Smoke on a lab image**

```bash
P=.superpowers/target-acc/release/examples/star_probe
IMG=$(python3 -c "import json;m=json.load(open('/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-lab/star-lab/manifests/v2.json'));print(m['families']['sharp/m4/f3.5']['images'][0]['path'])")
$P measure "$IMG" --out /private/tmp/claude-501/star_probe_smoke.jsonl
grep -c '"kind":"star"' /private/tmp/claude-501/star_probe_smoke.jsonl
cargo run --release -p athenaeum-core --example measure_probe -- "$IMG" | grep -E '"starsFitted"|"fwhmPx"'
```

Use the same `CARGO_TARGET_DIR` for `measure_probe`. Expected: the `measure` frame line's `fits` equals
`measure_probe`'s `starsFitted`, and its `frameFwhm` equals `fwhmPx` exactly. Put both numbers in the report.

- [ ] **Step 8: Commit (athenaeum, branch `star-truth-lab`, no push)**

```bash
rustfmt crates/athenaeum-core/examples/star_probe.rs
git add crates/athenaeum-core/src/stacking/measure.rs crates/athenaeum-core/src/stacking/register/detect.rs crates/athenaeum-core/examples/star_probe.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(lab): star_probe - the production star measurement paths as JSON lines; measure_plane_detailed returns the seeds it fitted

measure_plane_with_seeds is now measure_plane_detailed(..).0. The body moved unchanged, so the pinned
measurement tests pass untouched. stars_from_fits is pub for the probe.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01AkvYzN6darz7u9XC2jYPTg"
```

Do not run rustfmt on `measure.rs` or `detect.rs`. Match the surrounding style by hand.

---

### Task 6: `star_probe warp` and the `resampled` family

**Files:**
- Modify: `crates/athenaeum-core/examples/star_probe.rs` (the `warp` subcommand)
- Modify: `star-lab/starlab/families.py` (add `resampled`), `star-lab/manifests/v2.json`
- Test: `star-lab/tests/test_resampled.py`; one example test in `star_probe.rs`

**Interfaces:**
- Consumes: `athenaeum_core::resample::{warp_rows, Plane}`, `athenaeum_core::geometry::{Linear, LinearKind}`,
  `RegistrationConfig::default()` (`interpolation` = BicubicBSpline, `clamping_threshold` = 0.30).
- Produces:
  - `star_probe warp <in> --shift dx,dy --rotate deg --out <out>`. The output geometry equals the input. The output
    pixel p takes the source pixel `R(θ)(p − c) + c + (dx, dy)`, with c the frame centre.
  - `families.FAMILIES["resampled/f<F>"]` for F ∈ {2.0, 3.0}. The truth gives the positions mapped by the inverse
    transform `p = R(−θ)((xs, ys) − c − (dx, dy)) + c`, the unwarped widths, and a `kernel_var_px2` column of 1/3.

- [ ] **Step 1: The warp subcommand and its example test (RED, then GREEN)**

```rust
fn cmd_warp(mut it: impl Iterator<Item = String>) {
    let input = PathBuf::from(it.next().unwrap_or_else(|| die("warp: <image> missing")));
    let (mut dx, mut dy, mut rot, mut out) = (0.0f64, 0.0f64, 0.0f64, PathBuf::new());
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_else(|| die(format!("{k}: value missing")));
        match k.as_str() {
            "--shift" => {
                let p: Vec<f64> = v.split(',').map(|t| t.parse().unwrap_or_else(|_| die("--shift dx,dy"))).collect();
                if p.len() != 2 { die("--shift dx,dy") }
                dx = p[0];
                dy = p[1];
            }
            "--rotate" => rot = v.parse().unwrap_or_else(|_| die("--rotate deg")),
            "--out" => out = PathBuf::from(v),
            o => die(format!("unknown flag {o}")),
        }
    }
    let (planes, w, h) = read_planes(&input);
    let warped = warp_affine(&planes, w, h, dx, dy, rot);
    write_fits_f32(&out, w, h, warped.len(), &warped.concat(), &[]).unwrap_or_else(|e| die(format!("{}: {e}", out.display())));
}

/// Output pixel p reads the source at R(θ)(p − c) + c + (dx, dy), through the
/// production resampler with the registration defaults (B-spline, clamping 0.30).
fn warp_affine(planes: &[Vec<f32>], w: usize, h: usize, dx: f64, dy: f64, rot_deg: f64) -> Vec<Vec<f32>> {
    let (c, s) = (rot_deg.to_radians().cos(), rot_deg.to_radians().sin());
    let (cx, cy) = ((w as f64 - 1.0) / 2.0, (h as f64 - 1.0) / 2.0);
    let m = [
        [c, -s, cx - c * cx + s * cy + dx],
        [s, c, cy - s * cx - c * cy + dy],
        [0.0, 0.0, 1.0],
    ];
    let map = Linear { kind: LinearKind::Affine, m };
    let reg = RegistrationConfig::default();
    planes
        .iter()
        .map(|p| {
            let mut o = vec![0.0f32; w * h];
            warp_rows(&Plane::full(p, w, h), &map, w, 0, h, reg.interpolation, reg.clamping_threshold, &mut o);
            o
        })
        .collect()
}
```

Wire `Some("warp") => cmd_warp(it)` in `main`, and add the imports `athenaeum_core::resample::{warp_rows, Plane}`
and `athenaeum_core::geometry::{Linear, LinearKind}`. The test:

```rust
    #[test]
    fn a_half_pixel_shift_moves_a_star_and_keeps_its_flux() {
        let (w, h) = (96usize, 96usize);
        let mut d = vec![0.0f32; w * h];
        for y in 0..h { for x in 0..w {
            let r2 = (x as f32 - 48.0).powi(2) + (y as f32 - 48.0).powi(2);
            d[y * w + x] = (-r2 / (2.0 * 2.0f32.powi(2))).exp();
        }}
        let o = warp_affine(&[d.clone()], w, h, 0.5, 0.0, 0.0).remove(0);
        // the resampler writes NaN where the source is out of range: here the last column only
        assert_eq!(o.iter().filter(|v| !v.is_finite()).count(), h);
        let s0: f64 = d.iter().map(|&v| v as f64).sum();
        let s1: f64 = o.iter().filter(|v| v.is_finite()).map(|&v| v as f64).sum();
        assert!((s1 / s0 - 1.0).abs() < 1e-3, "flux {s1} vs {s0}");
        let cx: f64 = o.iter().enumerate().filter(|(_, v)| v.is_finite())
            .map(|(i, &v)| (i % w) as f64 * v as f64).sum::<f64>() / s1;
        assert!((cx - 47.5).abs() < 0.02, "centroid {cx}");
    }
```

The source at `p + 0.5` is read at output p, so the star at source 48 appears at output 47.5.

Run: `CARGO_BUILD_JOBS=4 cargo test -p athenaeum-core --example star_probe -- --test-threads=4` (PASS), then rebuild
the release binary as in Task 5 Step 6.

- [ ] **Step 2: The `resampled` family (lab), with its test first**

`tests/test_resampled.py`:

```python
"""resampled family: truth positions follow the inverse transform (spec §3.2, S-10)."""
import csv, math, os, shutil, tempfile, unittest
import numpy as np
from starlab import families, fitsio, PROBE


@unittest.skipUnless(os.path.exists(PROBE), "star_probe not built")
class TestResampled(unittest.TestCase):
    def test_truth_positions_land_on_the_warped_stars(self):
        spec = dict(families.FAMILIES["resampled/f3.0"]); spec.update(size=256, density=150)
        recs = families.build("resampled/f3.0", spec, tempfile.mkdtemp(), seed=4)
        img, _ = fitsio.read_fits(recs[0]["path"])
        rows = list(csv.DictReader(open(recs[0]["truth"])))
        errs = []
        for r in rows:
            x, y = float(r["x"]), float(r["y"])
            if float(r["fisher_snr"]) < 200 or r["edge"] == "1":
                continue
            ix, iy = int(round(x)), int(round(y)); st = img[iy - 4:iy + 5, ix - 4:ix + 5] - 0.05
            yy, xx = np.mgrid[iy - 4:iy + 5, ix - 4:ix + 5]
            errs.append(math.hypot((st * xx).sum() / st.sum() - x, (st * yy).sum() / st.sum() - y))
        self.assertLess(np.median(errs), 0.05)
```

Implementation in `families.py`:
1. Build the `sharp/m4/f<F>` image (the noisy frame, noise before the warp) into a temp path.
2. Run `subprocess.run([PROBE, "warp", src, "--shift", "0.5,0.5", "--rotate", "0.3", "--out", dst], check=True)`.
3. Rewrite the truth CSV with the positions mapped by the inverse transform. `edge` = 1 for stars whose 6F square
   reaches pixels the warp left NaN. Out of range, the resampler writes NaN, never 0.
4. Add the column `kernel_var_px2` = 0.3333 (P-10's widths unchanged).
5. Write the 16-bit sibling of the warped frame (P-20).

The record's `extra` carries `{"shift": [0.5, 0.5], "rotate_deg": 0.3, "source": src}`.

- [ ] **Step 3: Run, generate, commit**

```bash
cd star-lab && python3 -m unittest tests.test_resampled -v && python3 bin/make_v2.py --families resampled/f2.0,resampled/f3.0
```

`test_resampled` skips without the probe binary. **A SKIP is not a PASS:** build the binary first (Task 5 Step 6),
run the test, and report it passing.

Commit the athenaeum example change on `star-truth-lab` (no push), and the lab change with a push, each with the
trailers:
- athenaeum: `feat(lab): star_probe warp - the production resampler on an affine map`;
- lab: `starlab: the resampled family through the production warp`.

---

### Task 7: Injection into real frames

**Files:**
- Create: `star-lab/starlab/inject.py`, `star-lab/bin/inject.py`, and `star-lab/manifests/cameras.json` (the owner's
  e⁻/ADU table, P-21). Commit `star-lab/manifests/inject.json` after the run.
- Test: `star-lab/tests/test_inject.py`

**Interfaces:**
- Consumes: `render.*`, `shape.truth_widths`, `crlb.fisher_snr`, `crlb.star_bounds`, `fitsio.*`,
  `osc.site_channel`, `osc.channel_fluxes`, `osc.FWHM_FACTOR`, `field.flux_for_snr`, `families.COLS`.
- Produces:
  - `inject.real_stars(img, fwhm_frame, k=5.0) -> (peaks [(x, y)], sigma)` (P-12: the local maxima of the 3 × 3
    smoothed residual, not a dilated mask);
  - `inject.dilate(mask, r) -> bool array` (a separable box dilation by cumulative sums, O(N));
  - `inject.e_per_adu(header, cameras) -> (value or None, source)`;
  - `inject.gain_per_unit(header, e_adu, flat_value=None, channel=None) -> float`;
  - `inject.tile_gain(img, exclude) -> (gain, rn, span_ok)`;
  - `inject.place(img, peaks, fwhm_inj, n, rng, edge_set=True) -> [(x, y, edge_F)]`;
  - `inject.make_stars(img, fwhm_inj, n, rng, fwhm_frame, profiles=("m4",), snr=(10, 30, 100, 300)) -> [dict]`;
  - `inject.inject_mono(img, stars, rng, gain_fn, rn) -> (out, rows, stamp_mask)`. `gain_fn(x, y) -> e⁻ per unit`.
  - `inject.inject_mosaic(mosaic, pattern, stars, rng, gain_fn, rn, colours) -> (out, rows, stamp_mask)`. Here
    `gain_fn(x, y, channel)`.
  - `inject.inject_pair(ref_img, tgt_img, stars, s_inj, rng, gain_fn, rn) -> (ref_out, tgt_out, rows)`, with
    `psf_truth = "unwarped"`.
  - Every truth row carries every column of `families.COLS` plus `psf_truth`, `gain`, `gain_source`, `edge_F` and
    `crlb_*` (Review Focus 3, review I-8).

**Gain per unit (P-21).** Write this formula into the module docstring:
- `L_c = ((L − S)/(F/ATH_CFNM))/ATH_CSCL` (`calibration_library/light_cal.rs`), so one unit of `L_c` is
  `ATH_CSCL × F(x, y)/ATH_CFNM` ADU of the raw light, and `g(x, y) = e_adu × ATH_CSCL × F(x, y)/ATH_CFNM`.
- `F` is the master flat's pixel, from the file named in `ATH_CFLT`, read once per frame. Without it, `F/ATH_CFNM`
  is taken as 1 and the source says `"no flat"`.
- On a mosaic with `ATH_CCFA = T`, use `ATH_CFNR`, `ATH_CFNG` or `ATH_CFNB` for the channel of that CFA site.

Before writing code, verify every card name against `light_cal.rs` and one real calibrated header
(`tierA-lnB-f1/…/calibrated/mono…/c_….fits`). Read only.

**The owner's camera table.** `cameras.json` starts as
`{"ATR2600M": {"100": null}, "QHY268M": {"56": null}}`. The two keys are the set 204 and m42 cameras, whose headers
carry no `EGAIN`. The controller asks the owner for the values before Step 5. A `null` falls through to the raw-light
tile estimate: the RAW light named by `ATH_CSRN`, in ADU, `tile_gain` → e⁻/ADU, with its vignetting giving the level
span.

- [ ] **Step 1: Write the failing tests**

```python
"""Injection gates (spec §4, §9; review C-2, I-9, M-5; plan review C-3, I-8, M-2)."""
import math, time, unittest
import numpy as np
from starlab import inject, render, families

REAL = ((60, 60), (180, 90), (120, 200))


def frame(seed=1, n=256, sky=0.05, gain=12500.0, rn=0.0005, nan_border=0):
    rng = np.random.default_rng(seed)
    img = rng.poisson(np.full((n, n), sky) * gain) / gain + rng.standard_normal((n, n)) * rn
    for (x, y) in REAL:
        render.render_star(img, x, y, 0.5, render.components("moffat", 3.0, 3.0), 0.0, 3.0, 3.0)   # ~20 sigma peaks
    if nan_border:
        img[:nan_border, :] = np.nan
    return img


class TestInject(unittest.TestCase):
    def test_outside_stamps_bit_identical(self):
        src = frame(); stars = inject.make_stars(src, 3.0, 10, np.random.default_rng(3), fwhm_frame=3.0)
        out, rows, stamp = inject.inject_mono(src, stars, np.random.default_rng(4), lambda x, y: 12500.0, 0.0005)
        np.testing.assert_array_equal(out[~stamp], src[~stamp])
        self.assertTrue((out[stamp] != src[stamp]).any())

    def test_placement_keeps_12F_from_real_stars_and_nan(self):
        src = frame(nan_border=20)
        peaks, _ = inject.real_stars(src, 3.0)
        pts = inject.place(src, peaks, 3.0, 8, np.random.default_rng(5), edge_set=False)
        self.assertGreater(len(pts), 0)
        for (x, y, _) in pts:
            self.assertGreaterEqual(y, 20 + 12 * 3.0 - 1)
            for (sx, sy) in REAL:
                self.assertGreaterEqual(math.hypot(x - sx, y - sy), 12 * 3.0)

    def test_place_on_a_26mpx_nan_border_takes_seconds(self):
        img = np.zeros((4168, 6224), np.float32); img[:300, :] = np.nan
        t = time.time(); inject.place(img, [], 3.0, 20, np.random.default_rng(6), edge_set=True)
        self.assertLess(time.time() - t, 20.0)

    def test_tile_gain_recovers_the_generating_gain(self):
        rng = np.random.default_rng(7); n = 512
        level = 0.03 + 0.04 * np.linspace(0, 1, n)[None, :] * np.ones((n, 1))
        img = rng.poisson(level * 5000.0) / 5000.0 + rng.standard_normal((n, n)) * 0.0004
        g, rn, ok = inject.tile_gain(img, np.zeros_like(img, bool))
        self.assertTrue(ok); self.assertAlmostEqual(g / 5000.0, 1.0, delta=0.1)

    def test_tile_gain_refuses_a_flat_sky(self):
        rng = np.random.default_rng(8)
        img = rng.poisson(np.full((512, 512), 0.05) * 5000.0) / 5000.0
        self.assertFalse(inject.tile_gain(img, np.zeros_like(img, bool))[2])

    def test_e_per_adu_order(self):
        cams = {"ATR2600M": {"100": 0.31}}
        self.assertEqual(inject.e_per_adu({"EGAIN": "0.2467", "INSTRUME": "'ATR2600M'", "GAIN": "100"}, cams),
                         (0.2467, "EGAIN"))
        self.assertEqual(inject.e_per_adu({"INSTRUME": "'ATR2600M'", "GAIN": "100"}, cams), (0.31, "cameras.json"))
        self.assertEqual(inject.e_per_adu({"INSTRUME": "'QHY268M'", "GAIN": "56"}, cams), (None, None))

    def test_gain_per_unit_formula(self):
        h = {"ATH_CSCL": "65535.0", "ATH_CFNM": "35000.0"}
        self.assertAlmostEqual(inject.gain_per_unit(h, 0.25, flat_value=35000.0), 0.25 * 65535.0)
        self.assertAlmostEqual(inject.gain_per_unit(h, 0.25, flat_value=17500.0), 0.25 * 65535.0 * 0.5)

    def test_pair_truth_is_marked_unwarped(self):
        a, b = frame(1), frame(2)
        stars = inject.make_stars(a, 3.0, 6, np.random.default_rng(9), fwhm_frame=3.0)
        _, _, rows = inject.inject_pair(a, b, stars, 0.8, np.random.default_rng(10), lambda x, y: 12500.0, 0.0005)
        self.assertTrue(all(r["psf_truth"] == "unwarped" for r in rows))
        self.assertTrue(all(abs(float(r["s_true"]) - 0.8) < 1e-12 for r in rows))

    def test_injected_rows_have_every_scorecard_column(self):
        src = frame(); stars = inject.make_stars(src, 3.0, 4, np.random.default_rng(11), fwhm_frame=3.0)
        _, rows, _ = inject.inject_mono(src, stars, np.random.default_rng(12), lambda x, y: 12500.0, 0.0005)
        for r in rows:
            self.assertTrue(set(families.COLS) <= set(r.keys()), set(families.COLS) - set(r.keys()))
            self.assertNotEqual(r["sky_local"], ""); self.assertNotEqual(r["peak_snr"], "")
```

- [ ] **Step 2: Run them and see them fail** (`python3 -m unittest tests.test_inject -v`: ImportError).

- [ ] **Step 3: Implement `inject.py`**

```python
"""Injection of synthetic stars into real frames (spec §4; plan P-12, P-21). Calibrated frames serve every row;
registered frames and the LN reference serve only the pair rows (psf_truth = 'unwarped').

Gain per unit of a calibrated frame: L_c = ((L - S)/(F/ATH_CFNM))/ATH_CSCL, so one unit of L_c is
ATH_CSCL * F(x, y)/ATH_CFNM ADU of the raw light and g(x, y) = e_adu * ATH_CSCL * F(x, y)/ATH_CFNM (F: the master
flat's pixel; ATH_CFNR/G/B per channel on a CFA mosaic). e_adu: EGAIN, else cameras.json, else the raw light's tiles."""
import math
import numpy as np
from . import render, crlb, shape, field, families, osc


def _median_bg(img, cell=64):
    h, w = img.shape; bg = np.empty_like(img, dtype=np.float64)
    for y0 in range(0, h, cell):
        for x0 in range(0, w, cell):
            t = img[y0:y0 + cell, x0:x0 + cell]; v = t[np.isfinite(t)]
            bg[y0:y0 + cell, x0:x0 + cell] = np.median(v) if v.size else np.nan
    return bg


def real_stars(img, fwhm_frame, k=5.0):
    """Local maxima of the 3 x 3 box-smoothed residual above k sigma of that smoothed residual, over a 64-px median
    background (P-12). Smoothing first keeps noise peaks out: a 3-sigma rule on raw pixels takes ~0.1 % of the sky
    for stars and its 12 F exclusion swallows the frame. Returns ([(x, y)], sigma)."""
    fin = np.isfinite(img)
    r = np.where(fin, img - _median_bg(img), 0.0)
    sm = sum(r[1 + dy:r.shape[0] - 1 + dy, 1 + dx:r.shape[1] - 1 + dx] for dy in (-1, 0, 1) for dx in (-1, 0, 1)) / 9.0
    v = sm[fin[1:-1, 1:-1]]
    sig = 1.4826 * float(np.median(np.abs(v - np.median(v))))
    z = np.full(img.shape, -np.inf); z[1:-1, 1:-1] = np.where(fin[1:-1, 1:-1], sm, -np.inf)
    c = z[1:-1, 1:-1]
    peak = c > k * sig
    for dy in (-1, 0, 1):
        for dx in (-1, 0, 1):
            if dy or dx:
                peak &= c >= z[1 + dy:z.shape[0] - 1 + dy, 1 + dx:z.shape[1] - 1 + dx]
    ys, xs = np.nonzero(peak)
    return [(float(x + 1), float(y + 1)) for y, x in zip(ys, xs)], sig


def dilate(mask, r):
    """Separable box dilation by r px (a (2r+1)^2 square) with cumulative sums: O(N) on a 26 Mpx map."""
    m = mask.astype(np.int32)
    for axis in (0, 1):
        pad = [(r + 1, r) if a == axis else (0, 0) for a in (0, 1)]
        c = np.cumsum(np.pad(m, pad), axis=axis, dtype=np.int64)
        n = m.shape[axis]
        hi = np.take(c, np.arange(2 * r + 1, 2 * r + 1 + n), axis=axis)
        lo = np.take(c, np.arange(0, n), axis=axis)
        m = ((hi - lo) > 0).astype(np.int32)
    return m.astype(bool)


def e_per_adu(header, cameras):
    if "EGAIN" in header:
        return float(header["EGAIN"]), "EGAIN"
    cam = str(header.get("INSTRUME", "")).strip("' "); g = str(header.get("GAIN", "")).strip("' ")
    v = (cameras or {}).get(cam, {}).get(g)
    return (float(v), "cameras.json") if v is not None else (None, None)


def gain_per_unit(header, e_adu, flat_value=None, channel=None):
    cscl = float(header.get("ATH_CSCL", 65535.0))
    key = {None: "ATH_CFNM", 0: "ATH_CFNR", 1: "ATH_CFNG", 2: "ATH_CFNB"}[channel]
    norm = float(header.get(key, header.get("ATH_CFNM", "nan")))
    if flat_value is None or not math.isfinite(norm):
        return e_adu * cscl
    return e_adu * cscl * float(flat_value) / norm


def tile_gain(img, exclude, cell=64):
    """Sky variance against level on clean tiles: var = level / g + c (c = rn^2 plus any removed pedestal, so only the
    gain is trusted). Needs a level span >= 20 % of the median level."""
    lv, vr = [], []
    h, w = img.shape
    for y0 in range(0, h - cell + 1, cell):
        for x0 in range(0, w - cell + 1, cell):
            t = img[y0:y0 + cell, x0:x0 + cell]
            if exclude[y0:y0 + cell, x0:x0 + cell].any() or not np.isfinite(t).all():
                continue
            d = np.diff(t, axis=1)
            lv.append(np.median(t)); vr.append(0.5 * (1.4826 * np.median(np.abs(d - np.median(d)))) ** 2)
    lv, vr = np.array(lv), np.array(vr)
    if lv.size < 8 or (np.percentile(lv, 95) - np.percentile(lv, 5)) < 0.2 * np.median(lv):
        return float("nan"), float("nan"), False
    slope, icpt = np.linalg.lstsq(np.vstack([lv, np.ones_like(lv)]).T, vr, rcond=None)[0]
    return 1.0 / slope, math.sqrt(max(icpt, 0.0)), True


def place(img, peaks, fwhm_inj, n, rng, edge_set=True):
    """Jittered grid; >= 12 F from every real peak and every non-finite pixel, from each other, and not on a pixel above
    0.5 x the frame maximum. The edge set sits 1, 2, 3 F inside the coverage edge (or the frame border)."""
    h, w = img.shape; R = 12.0 * fwhm_inj; ri = int(math.ceil(R))
    nonfinite = ~np.isfinite(img)
    bad = dilate(nonfinite, ri) if nonfinite.any() else np.zeros((h, w), bool)
    if np.isfinite(img).any():                     # bright structure: above half the frame's peak OVER the sky
        r = np.nan_to_num(img - _median_bg(img), nan=-np.inf)
        bad |= r > 0.5 * r.max()
    grid = {}
    for (px, py) in peaks:
        grid.setdefault((int(px // R), int(py // R)), []).append((px, py))

    def clear(x, y):
        cx, cy = int(x // R), int(y // R)
        return all(math.hypot(px - x, py - y) >= R for gx in (cx - 1, cx, cx + 1) for gy in (cy - 1, cy, cy + 1)
                   for (px, py) in grid.get((gx, gy), ()))

    pitch = ri; pts = []
    cand = [(x + rng.uniform(-0.25, 0.25) * pitch, y + rng.uniform(-0.25, 0.25) * pitch)
            for y in range(ri, h - ri, pitch) for x in range(ri, w - ri, pitch)]
    rng.shuffle(cand)
    for x, y in cand:
        if len(pts) >= n:
            break
        if bad[int(round(y)), int(round(x))] or not clear(x, y) or any(math.hypot(x - p[0], y - p[1]) < R for p in pts):
            continue
        pts.append((x, y, float("inf")))
    if edge_set:
        fin_rows = np.nonzero(np.isfinite(img).any(axis=1))[0]; fin_cols = np.nonzero(np.isfinite(img).any(axis=0))[0]
        if fin_rows.size and fin_cols.size:
            y_top, x_left, x_right = fin_rows[0], fin_cols[0], fin_cols[-1]
            ym = (fin_rows[0] + fin_rows[-1]) / 2.0
            for k in (1, 2, 3):
                for (x, y) in ((x_left + k * fwhm_inj, ym + 3 * R * k), (x_right - k * fwhm_inj, ym - 3 * R * k),
                               (fin_cols[0] + w / 2.0, y_top + k * fwhm_inj)):
                    if 0 <= y < h and 0 <= x < w and np.isfinite(img[int(y), int(x)]) and clear(x, y):
                        pts.append((x, y, float(k)))
    return pts


def make_stars(img, fwhm_inj, n, rng, fwhm_frame, profiles=("m4",), snr=(10.0, 30.0, 100.0, 300.0)):
    peaks, _ = real_stars(img, fwhm_frame)
    pts = place(img, peaks, fwhm_inj, n, rng)
    return [{"x": x, "y": y, "edge_F": e, "prof": profiles[i % len(profiles)], "fwhm": fwhm_inj, "ecc": 0.0,
             "theta": 0.0, "snr": snr[i % len(snr)]} for i, (x, y, e) in enumerate(pts)]


def _render(st, shape_hw, sky, g, rn, flux=None):
    prof = st["prof"]; fm = st["fwhm"]; fM = fm / math.sqrt(1 - st["ecc"] ** 2)
    beta = float(prof[1:]) if prof.startswith("m") else None
    comps = render.components("moffat" if beta else "gauss", fm, fM, beta=beta or 4.0)
    flux = flux if flux is not None else field.flux_for_snr(st["snr"], fm, sky, g, rn)
    s = np.zeros(shape_hw)
    fimg, pk, box = render.render_star(s, st["x"], st["y"], flux, comps, math.radians(st["theta"]), fm, fM)
    return s, flux, fimg, pk, box, comps, fm, fM, beta


def _row(i, st, flux, fimg, pk, cut, sky, grad, g, rn, comps, fm, fM, beta, plane=0):
    w = shape.truth_widths(comps, math.radians(st["theta"]), fm, fM, "moffat_fixed" if beta else "gauss", beta)
    cb = crlb.star_bounds(lambda a, b: render.components("moffat" if beta else "gauss", a, b, beta=beta or 4.0),
                          st["x"], st["y"], flux, sky, g, rn, fm, fM, math.radians(st["theta"]), st["ecc"])
    r = {c: "" for c in families.COLS}
    r.update(id=i, x=st["x"], y=st["y"], flux=flux, flux_img=fimg, peak=pk, peak_snr=pk / math.sqrt(sky / g + rn * rn),
             fisher_snr=crlb.fisher_snr(cut, sky, g, rn), saturated=0, edge=int(math.isfinite(st["edge_F"])),
             edge_F=st["edge_F"], profile=st["prof"], beta=beta or 0.0, sky_local=sky, sky_grad=grad, s_true=1.0,
             plane=plane, pair_id="", kind="injected", psf_truth="calibrated", gain=g, **w)
    r.update({"crlb_" + k: v for k, v in cb.items() if k != "fisher_snr"})
    return r


def inject_mono(img, stars, rng, gain_fn, rn):
    out = img.copy(); stamp_mask = np.zeros(img.shape, bool); rows = []
    bg = _median_bg(img)
    for i, st in enumerate(stars):
        sky, grad = _local(bg, st["x"], st["y"]); g = float(gain_fn(st["x"], st["y"]))
        s, flux, fimg, pk, (ya, yb, xa, xb), comps, fm, fM, beta = _render(st, img.shape, sky, g, rn)
        cut = s[ya:yb + 1, xa:xb + 1]
        noisy = rng.poisson(np.clip(cut, 0, None) * g) / g          # the star's own Poisson noise
        region = out[ya:yb + 1, xa:xb + 1]; fin = np.isfinite(region)
        region[fin] += noisy[fin]
        stamp_mask[ya:yb + 1, xa:xb + 1] |= cut > 0
        rows.append(_row(i, st, flux, fimg, pk, cut, sky, grad, g, rn, comps, fm, fM, beta))
    return out, rows, stamp_mask


def _local(bg, x, y):
    ix, iy = int(round(x)), int(round(y)); h, w = bg.shape
    ix = min(max(ix, 1), w - 2); iy = min(max(iy, 1), h - 2)
    gx = 0.5 * (bg[iy, ix + 1] - bg[iy, ix - 1]); gy = 0.5 * (bg[iy + 1, ix] - bg[iy - 1, ix])
    return float(bg[iy, ix]), float(math.hypot(gx, gy))


def inject_mosaic(mosaic, pattern, stars, rng, gain_fn, rn, colours):
    """OSC: each channel's star at full resolution, sampled at that channel's CFA sites, with per-site gain and the
    per-channel sky (the median of that channel's sites)."""
    sc = osc.site_channel(pattern); out = mosaic.copy(); stamp = np.zeros(mosaic.shape, bool); rows = []
    yy, xx = np.mgrid[0:mosaic.shape[0], 0:mosaic.shape[1]]; site = sc[yy & 1, xx & 1]
    sky_k = [float(np.nanmedian(mosaic[site == k])) for k in range(3)]
    for i, st in enumerate(stars):
        fl = osc.channel_fluxes(1.0, colours[i])
        for k in range(3):
            stk = dict(st, fwhm=st["fwhm"] * osc.FWHM_FACTOR[k])
            g = float(gain_fn(st["x"], st["y"], k))
            base = field.flux_for_snr(st["snr"], stk["fwhm"], sky_k[k], g, rn) * fl[k]
            s, flux, fimg, pk, (ya, yb, xa, xb), comps, fm, fM, beta = _render(stk, mosaic.shape, sky_k[k], g, rn, base)
            sel = site[ya:yb + 1, xa:xb + 1] == k
            cut = s[ya:yb + 1, xa:xb + 1] * sel
            noisy = rng.poisson(np.clip(cut, 0, None) * g) / g
            region = out[ya:yb + 1, xa:xb + 1]; fin = np.isfinite(region)
            region[fin] += noisy[fin]; stamp[ya:yb + 1, xa:xb + 1] |= cut > 0
            rows.append(_row(i, stk, flux, fimg, pk, s[ya:yb + 1, xa:xb + 1], sky_k[k], 0.0, g, rn, comps, fm, fM, beta, plane=k))
    return out, rows, stamp


def inject_pair(ref_img, tgt_img, stars, s_inj, rng, gain_fn, rn):
    ref_out, rows, _ = inject_mono(ref_img, stars, rng, gain_fn, rn)
    t_out = tgt_img.copy()
    for st, r in zip(stars, rows):
        g = float(gain_fn(st["x"], st["y"]))
        s, *_rest = _render(st, tgt_img.shape, float(r["sky_local"]), g, rn, flux=float(r["flux"]))
        (ya, yb, xa, xb) = _rest[3]
        cut = s[ya:yb + 1, xa:xb + 1] / s_inj
        noisy = rng.poisson(np.clip(cut, 0, None) * g) / g
        reg = t_out[ya:yb + 1, xa:xb + 1]; fin = np.isfinite(reg); reg[fin] += noisy[fin]
        r.update(psf_truth="unwarped", s_true=s_inj, pair_id="pair")
    return ref_out, t_out, rows
```

`inject_pair` renders the target star at the same flux as the reference star and divides it by `s_inj`, so the
target holds `flux / s_inj`.

`families.COLS` comes from Task 4. Truth CSVs are written with `families.write_truth(path, rows, extra_cols=("psf_truth",
"gain", "gain_source", "edge_F", "crlb_x", …))`.

**Hot pixels.** The calibrated frame's hot-pixel correction (`ATH_CHPX`: 104 078 replaced on the set 204 frame) is
not persisted as a map. Record `"hot_pixel_map": "not available"` in the injection manifest. The noise of a replaced
pixel is not Poisson, and the report says so.

- [ ] **Step 4: Implement `bin/inject.py`**

```
inject.py calibrated <frame.fits> --fwhm-frame F --out DIR [--cameras manifests/cameras.json] [--pattern RGGB]
inject.py pair <ref.fits> <member.fits> --source-calibrated <calibrated.fits> --fwhm-frame F --s-inj S --out DIR
```

**What it writes:**
- `<stem>.inj.fits`;
- for calibrated frames, its 16-bit sibling `<stem>.inj.u16.fits` (P-20);
- `<stem>.inj.csv`;
- `<stem>.stamps.npy` (uncommitted data).

**Gain:**
- It resolves `e_adu` (P-21 order) and reads the flat named in `ATH_CFLT` when it exists.
- A pair takes its gain from `--source-calibrated`, the calibrated frame the registered member came from: a
  registered frame's own tiles are smoothed by the warp.

**Manifest record** in `star-lab/manifests/inject.json`:
`{"source": path, "source_sha256": …, "seed": …, "e_adu": …, "gain_source": …, "flat": path|null, "hot_pixel_map":
"not available", "u16": path|null, "kind": "calibrated"|"mosaic"|"pair"|"raw"}`. `run_probe.py` reads `u16` for the
`analysis` path.

OSC injection uses the `calibrated_mosaic` the run kept. Spec §4's alternative, the mosaic rebuilt from the raw
through `light_cal`, is not planned, and the report says so.

**Gain self-check (P-21).** On star-free 64-px tiles of the calibrated frame, the median ratio of the measured
variance to `level/g + c` must lie in [0.9, 1.1], with the intercept `c` free. The read noise is reported, not gated.
A miss refuses the frame and names it.

- [ ] **Step 5: Run the tests, then inject the spec §4 frames**

```bash
cd star-lab && python3 -m unittest tests.test_inject -v
```

Before injecting, the controller asks the owner for the two `cameras.json` values (P-21). A `null` uses the raw-light
tile estimate.

Locate the frames read-only (`sqlite3 "file:$D/athenaeum.db?immutable=1"`, `stacking_artifacts`):
- **Calibrated:** `tierA-lnB-f1` (set 204 mono and OSC; OSC mosaics are `calibrated_mosaic`), `hc11B-f1`
  (Horse C11 L and G, `EGAIN` 0.2467) and `lnB-runB-f1` (run B). Take 3 frames per dataset: the sharpest, the median
  and the broadest by their `metrics` FWHM, 60 stars each.
- **Star-test corpus:** the singles and m42 30 s frames named in spec §4, which are raw lights. Inject into the raw
  ADU domain with gain `e_adu` per ADU, and write the 16-bit file directly.
- **Pairs:**
  - set 204 mono and set 204 OSC: the `ln_reference` under `…/ln/<group>/reference.fits` in `tierA-lnB-f1`, plus one
    registered member each;
  - run B: `lnB-runB-f1`, `ln_reference` ×2.

  `s_inj` is 0.8 for all of them.

Horse C11 has no persisted LN reference in any acceptance copy, so it gets no injected pair (plan review I-7). Its §8
LN row reuses checkpoint B's outputs. OSC mosaic injection covers set 204 only, and the report says so.

Report every refused frame with its reason.

- [ ] **Step 6: Commit and push (athenaeum-lab)** with the message
  `starlab: injection into calibrated frames, OSC mosaics and registered pairs, with the stamp and gain gates`.

---

### Task 8: Matching, probe readers and the scorecard

**Files:**
- Create: `star-lab/starlab/match.py`, `star-lab/starlab/probe_io.py`, `star-lab/starlab/scorecard.py`,
  `star-lab/bin/run_probe.py`, `star-lab/bin/scorecard.py`
- Test: `star-lab/tests/test_match.py`, `star-lab/tests/test_scorecard.py`

**Interfaces:**
- Consumes:
  - Task 5's JSONL schema;
  - the truth CSVs (Task 4, Task 7);
  - `ln_scale_probe`'s stdout JSON lines and `--stars-dir` CSVs;
  - the ERP CSV schema `x,y,fwhm_x,fwhm_y,theta_deg,beta,flux,background,function` (Task 9).
- Produces:
  - `match.match(truth_xy, order, meas_xy, radius) -> index array (−1 = missed)`;
  - `match.classify_unmatched(meas_xy, truth_rows, artefact_rows, bg_kind) -> list[str]`;
  - `probe_io.load(path) -> {"seeds": [...], "stars": {path: [...]}, "frames": {path: [...]}}`;
  - `scorecard.rows_*()` functions, one per spec §7 row, each returning `list[dict]`;
  - `scorecard.render_markdown(rows) -> str`;
  - CLI `bin/scorecard.py <manifest> --probe-dir DIR [--erp-dir DIR] --out OUT.md --json OUT.json`.
  - `probe_io.load_erp(path) -> list[dict]` in the schema `x,y,fwhm_x,fwhm_y,theta_deg,beta,flux,background,function`,
    tested on `tests/data/erp_sample.csv`. That file is five rows copied in this task from
    `~/.athenaeum-acc/star-lab/grid/iso/m4/erp-stars-e7t0.csv`, columns `cx, cy, sx, sy, theta, beta, flux, B,
    fwhmX, fwhmY`, already at `c0 − 0.5`. Task 9 relies on this reader.

- [ ] **Step 1: Write the failing tests**

```python
"""Matcher (spec §6, S-4; review M-10; review focus 2, 3)."""
import unittest
import numpy as np
from starlab import match


class TestMatch(unittest.TestCase):
    def test_truth_positions_match_everything(self):
        rng = np.random.default_rng(1); t = rng.uniform(0, 500, (300, 2))
        idx = match.match(t, np.arange(300), t.copy(), np.full(300, 1.5))
        self.assertTrue((idx == np.arange(300)).all())

    def test_a_blend_matches_the_brighter_member(self):
        t = np.array([[100.0, 100.0], [102.0, 100.0]])        # 0 is brighter
        m = np.array([[100.8, 100.0]])
        idx = match.match(t, np.array([0, 1]), m, np.array([1.5, 1.5]))
        self.assertEqual(idx.tolist(), [0, -1])

    def test_analysis_rows_are_not_flipped(self):
        """P-6: flipVertical is recorded and never applied."""
        import json, os, tempfile
        from starlab import probe_io
        d = tempfile.mkdtemp(); p = os.path.join(d, "a.jsonl")
        with open(p, "w") as f:
            f.write(json.dumps({"kind": "star", "path": "analysis", "plane": 0, "x": 50.0, "y": 100.0, "thetaDeg": 20.0}) + "\n")
            f.write(json.dumps({"kind": "frame", "path": "analysis", "plane": 0, "flipVertical": True,
                                "frame": {"width": 2048, "height": 2048}}) + "\n")
        got = probe_io.load(p)
        self.assertEqual(got["stars"]["analysis"][0]["y"], 100.0)
        self.assertEqual(got["stars"]["analysis"][0]["thetaDeg"], 20.0)
        self.assertTrue(got["frames"]["analysis"][0]["flipVertical"])

    def test_one_to_one(self):
        t = np.array([[10.0, 10.0], [11.0, 10.0]]); m = np.array([[10.5, 10.0]])
        idx = match.match(t, np.array([1, 0]), m, np.array([1.5, 1.5]))
        self.assertEqual(idx.tolist(), [-1, 0])
```

```python
"""Scorecard rows on hand-made inputs (spec §7)."""
import unittest
from starlab import scorecard


def truth(n=4):
    return [dict(id=i, x=10.0 * i, y=5.0, flux=1.0, flux_img=0.99, fisher_snr=50.0, saturated=0, edge=0,
                 fwhm_minor=3.0, fwhm_major=3.0, ecc=0.0, theta_deg=0.0, pix_fwhm_minor=3.1, pix_fwhm_major=3.1,
                 pix_ecc=0.0, pix_theta_deg=0.0, fit_fwhm_minor=3.08, fit_fwhm_major=3.08, fit_ecc=0.0,
                 fit_theta_deg=0.0, sky_local=0.05, sky_grad=0.0, peak=0.1, peak_snr=50.0, profile="m4", beta=4.0,
                 kind="star", plane=0) for i in range(n)]


class TestScorecard(unittest.TestCase):
    def test_fwhm_bias_against_each_truth_column(self):
        meas = [dict(x=10.0 * i, y=5.0, fwhm=3.3, ecc=0.05, thetaDeg=0.0, background=0.05, signal=0.9, path="measure", plane=0, beta=4.0) for i in range(4)]
        r = scorecard.rows_fwhm(truth(), meas, "measure", "fam")
        by = {x["truth"]: x for x in r}
        self.assertAlmostEqual(by["prepixel"]["bias"], 0.10, places=6)
        self.assertAlmostEqual(by["pixbox"]["bias"], 3.3 / 3.1 - 1, places=6)

    def test_empty_path_rows(self):
        r = scorecard.rows_detection(truth(), [], "analysis", "fam", threshold_snr=5.0)
        self.assertEqual(r[0]["completeness"], 0.0)
        self.assertIn("n/a", scorecard.render_markdown(scorecard.rows_fwhm(truth(), [], "analysis", "fam")))

    def test_edge_stars_skip_width_rows(self):
        t = truth(); t[0]["edge"] = 1
        meas = [dict(x=10.0 * i, y=5.0, fwhm=3.0, ecc=0.0, thetaDeg=0.0, background=0.05, signal=0.9, path="measure", plane=0, beta=4.0) for i in range(4)]
        r = scorecard.rows_fwhm(t, meas, "measure", "fam")
        self.assertEqual(r[0]["n"], 3)
```

- [ ] **Step 2: Run them and see them fail.**

- [ ] **Step 3: Implement `match.py`** (the greedy cell-hash matcher)

```python
"""Truth <-> measurement matching (spec §6, S-4): one-to-one, greedy from the brightest truth star down,
radius max(1.5 px, 0.5 x FWHM_truth). A single blended detection takes the brighter member."""
import math
import numpy as np


def radius_for(fwhm_truth):
    return np.maximum(1.5, 0.5 * np.asarray(fwhm_truth, dtype=np.float64))


def match(truth_xy, order, meas_xy, radius):
    truth_xy = np.asarray(truth_xy, float); meas_xy = np.asarray(meas_xy, float).reshape(-1, 2)
    out = np.full(len(truth_xy), -1, dtype=np.int64)
    if len(meas_xy) == 0:
        return out
    cell = max(float(np.max(radius)), 1.0); grid = {}
    for j, (x, y) in enumerate(meas_xy):
        grid.setdefault((int(x // cell), int(y // cell)), []).append(j)
    used = np.zeros(len(meas_xy), bool)
    for i in order:
        x, y = truth_xy[i]; best, bd = -1, float(radius[i])
        cx, cy = int(x // cell), int(y // cell)
        for gx in (cx - 1, cx, cx + 1):
            for gy in (cy - 1, cy, cy + 1):
                for j in grid.get((gx, gy), ()):
                    if not used[j]:
                        d = math.hypot(meas_xy[j][0] - x, meas_xy[j][1] - y)
                        if d <= bd:
                            best, bd = j, d
        if best >= 0:
            used[best] = True; out[i] = best
    return out


def classify_unmatched(meas_xy, truth_rows, artefact_rows, nebula_map=None, cell=3.0):
    """'hot' / 'cr' / 'star_wing' (within 6 FWHM of a truth star) / 'nebula' (nebula_map > 0.5 sky there) / 'nothing'."""
    out = []
    art = [(a["x"], a["y"], a["type"]) for a in artefact_rows]
    stars = [(float(t["x"]), float(t["y"]), float(t["fwhm_major"])) for t in truth_rows]
    for (x, y) in meas_xy:
        lab = "nothing"
        for (ax, ay, ty) in art:
            if math.hypot(ax - x, ay - y) <= cell:
                lab = "cr" if ty == "cr" else "hot"; break
        if lab == "nothing" and any(math.hypot(sx - x, sy - y) <= 6 * f for sx, sy, f in stars):
            lab = "star_wing"
        if lab == "nothing" and nebula_map is not None and nebula_map[int(round(y)), int(round(x))] > 0.5:
            lab = "nebula"
        out.append(lab)
    return out
```

- [ ] **Step 4: Implement `probe_io.py` and `scorecard.py`**

`probe_io.load` reads the JSONL and splits it into seeds, stars and frames. It leaves the `analysis` coordinates as
they are (P-6) and asserts that the `analysis` frame line carries `flipVertical`.

`scorecard.py` has one function per spec §7 row:
- `rows_detection(truth, meas, path, family, threshold_snr)`;
- `rows_centroid`, `rows_fwhm`, `rows_ecc`, `rows_background`, `rows_flux`, `rows_frame`, `rows_osc`, `rows_cost`;
- `rows_selection(seeds, truth, ...)`, `rows_stability(twin_a, twin_b, ladder)`.

Shared definitions:
- **bias** = median((measured − truth) / truth) for widths and flux, median(measured − truth) for positions and
  eccentricity;
- **scatter** = 1.4826 · MAD;
- **efficiency** = scatter / median(CRLB);
- **SNR bins** are spec §6's, from `fisher_snr`;
- `edge` = 1 stars enter detection rows only (Review Focus 3);
- a row with no matched stars renders `n/a`.

For each path's declared threshold (row 1, review I-7):
- `measure`: true `peak_snr ≥ options.measure.detectionSigma` (from the frame line);
- `register`: `fisher_snr ≥ 10`;
- `analysis`: `fisher_snr ≥ 5` (the matched-filter response is approximated by the Fisher SNR; the report says so).

Completeness = matched / expected over the expected set, and the raw fraction beside it.

`rows_fwhm`, `rows_centroid` and `rows_ecc` return their all-bin rows first: for widths, three rows in the order
`prepixel`, `pixbox`, `pointfit`, comparing measured `fwhm` with `sqrt(minor × major)` of each truth column. The
per-bin rows follow. The efficiency's CRLB comes from the truth CSV's `crlb_*` columns, and from `mcrlb_*` for the
model bound.

Row 6 reports two things:
- each path's measured `signal / flux_img`;
- the noiseless enclosed fraction inside circles of 2F and 5F, where F is the median measured FWHM of the image's
  `measure` path (F1's own F). It is computed per distinct shape from `shape.fine_profile` with `pixel_box=True`.
  That is the share of light F1's circles hold for that profile.

The model bound (P-18) is computed here, per image and plane, with `crlb.star_bounds` under an elliptical Moffat at
the Auto β that the `measure` frame line reports, at the truth's point-fit widths. It is cached per (shape, β).

Row 10 recomputes σ0 from the seed lines with the production formula. Read `initial_sigma` in `psf_signal.rs` and
port it exactly; name it `sigma0_diagnostic`. Row 11 pairs twins by truth id.

`render_markdown` writes one table per row number with columns
`family | path | plane | bin | n | ours | ERP | CRLB/ideal`.

- [ ] **Step 5: Implement `bin/run_probe.py`**

```
run_probe.py <manifest.json> [--families a,b] [--out DIR]   # default DIR = DATA_ROOT/probe
```

For every image:
1. Check memory (as `make_v2.py`).
2. Run the stacking paths on the float file: `PROBE measure <img> --paths measure,betas,register --out
   DIR/<family>/<name>.jsonl`. Add `--debayer RGGB` for OSC mosaics, and `--plane all` for `.ideal.fits` (with
   `--paths measure,betas`).
3. Run `analysis` on the 16-bit sibling in a second invocation: `PROBE measure <extra.u16> --paths analysis --out
   DIR/<family>/<name>.analysis.jsonl` (P-20). For OSC this is the mosaic sibling, which the analyzer
   green-interpolates as the tab does.
4. Real frames and the 4096² image run one path per invocation (P-22).
5. Time every invocation with `/usr/bin/time -l -o <name>.<paths>.time`.

**Selection (row 10, plan review I-6).** For `saturated/*`, `artefacts`, `halo` and `sharp/m4/f3.5`, also run the
product's LN selection on a self-pair:

```
LN_SCALE_PROBE --ref <img> --plane 0 --beta auto --stars-dir DIR/<family>/sel --member self=<img>
```

Selection and its per-star dump are the product's; the scale is 1 and ignored. `rows_selection` reads the kept list
from that dump. Check the dump's columns in `examples/ln_scale_probe.rs` first: the reference dump is `x,y,fwhm,amp,
b,sig` at `:630`, and the pair dump is near `:1705`.

For pair families (`pairs`, `osc_pairs`, `density_pairs`, `truthcheck`, `truthcrowd`):
1. For OSC, `PROBE debayer` first.
2. Run `LN_SCALE_PROBE --ref <ref> --plane <p> --member <label>=<path> … --stars-dir DIR/<family>/stars` with the
   arms Task 16b used: `--beta 4`, `--beta auto`. Check its `usage()` for the exact flags before writing them.
3. Capture stdout to `<family>/ln.p<p>.jsonl`.

Never run two probes at once.

- [ ] **Step 6: Run the tests; then run the probes over every family and print the scorecard**

```bash
cd star-lab && python3 -m unittest tests.test_match tests.test_scorecard -v
python3 bin/run_probe.py manifests/v2.json && python3 bin/scorecard.py manifests/v2.json --probe-dir ~/.athenaeum-acc/star-lab-v2/probe --out /private/tmp/claude-501/scorecard-v2.md --json /private/tmp/claude-501/scorecard-v2.json
```

Expected: tests PASS. The scorecard is written with every row group present. Report wall time and peak RSS.

- [ ] **Step 7: Commit and push (athenaeum-lab)** with the message
  `starlab: matcher, probe readers, scorecard rows 1-11 and the probe runner`.

---

### Task 9: ERP drivers over the v2 manifests

**Files:**
- Create: `star-lab/erp/erp_v2.py`. Modify `star-lab/erp/erp_stars.js` only when a parameter has to come from the
  target list.
- Test: `star-lab/tests/test_erp_v2.py`. This parses a stored sample of ERP output; it does not launch the ERP.

**Interfaces:**
- Consumes: `manifests/v2.json`, `erp/run_job.sh`, `erp/erp_stars.js`, `erp/run_ln.js`, `erplib.py`.
- Produces: per image, `DATA_ROOT/erp/<family>/<name>.<function>.csv` with
  `x,y,fwhm_x,fwhm_y,theta_deg,beta,flux,background,function`, in 0-based pixel centres (the script's own
  convention); per pair family, `DATA_ROOT/erp/<family>/ln.csv` with `member,scale`.

- [ ] **Step 1: Read the existing drivers**

Read `erp/erp_stars.js`, `erp/run_ln.js`, `erp/run_job.sh` and `run_measurers.py` (Task 16b) to learn:
- their target-list format;
- their output CSV columns;
- the memory guard.

Write `test_erp_v2.py` against `tests/data/erp_sample.csv` (shipped in Task 8). It asserts that
`erp_v2.normalise(path)` equals `probe_io.load_erp(path)` row for row.

`run_job.sh` copies each job to a FIXED path, `$HOME/.athenaeum-acc/star-lab/erp/{ln,stars}-job.json`, and writes its
logs there, because the ERP script reads its job from that path. That fixed path is why only one instance can ever
run. `erp_v2.py` keeps writing its jobs there; it never moves `L` or the `-r=` script paths separately.

- [ ] **Step 2: Implement `erp_v2.py`**

```
erp_v2.py stars <manifest> [--families …] [--functions Moffat4,Gaussian,MoffatA]
erp_v2.py ln <manifest> [--families pairs,osc_pairs,density_pairs,truthcheck,truthcrowd]
```

- `stars` writes chunked target lists of at most 10 images of 2048² per instance, and launches through `run_job.sh`.
  That script runs one instance with the memory guard, and refuses when `pgrep` finds an interactive instance. It
  then normalises every output into the schema.
- OSC ideal planes are split into single-plane FITS first: the ERP's per-star fit runs per plane.
- `ln` drives `run_ln.js` with the owner's batch LN parameters (as Task 16b).

- [ ] **Step 3: Run the unit test** (`python3 -m unittest tests.test_erp_v2 -v`).

- [ ] **Step 4: One smoke job, only when no interactive ERP instance is running**

```bash
python3 erp/erp_v2.py stars manifests/v2.json --families sharp/m4/f3.5 --functions Moffat4 --smoke
```

`--smoke` first runs `run_job.sh`'s own interactive-instance check (the process name lives only in the lab's
scripts) and exits with a message when one is running. In that case skip the smoke and say so in the report. The full ERP batch is the controller's step in
Task 11, after the owner confirms.

- [ ] **Step 5: Commit and push (athenaeum-lab)** with the message
  `starlab: ERP drivers over the v2 manifests (per-star fits, LN scale), normalised for the scorecard`.

---

### Task 10: The lab's own gates

**Files:**
- Create: `star-lab/bin/gates.py`
- Test: run it; it is its own test (each gate prints PASS/FAIL with numbers and exits non-zero on any FAIL)

**Interfaces:**
- Consumes: every module above, `PROBE`, the acceptance copy `~/.athenaeum-acc/tierA-lnB-f1` (read only,
  `immutable=1`).
- Produces: `gates.py [--only name,…]`, printing one line per gate. A summary goes to
  `DATA_ROOT/gates-<date>.txt`.

The gates (spec §9 table, with this plan's rulings):

| gate | how |
| ---- | ---- |
| `pixel_integration` | re-run `tests.test_render` (the three integration tests) |
| `mosaic` | re-run `tests.test_osc.test_every_cfa_pixel_equals_its_channel` |
| `injection` | for every `manifests/inject.json` entry: reload the source and the injected frame, check bit-identity outside the stamps |
| `gain` | the recorded gain self-check per injected calibrated frame: median measured/predicted tile variance in [0.9, 1.1], intercept free; the read noise is reported, not gated (P-21) |
| `determinism` | `make_v2.py --check` |
| `crlb` | re-run `tests.test_crlb` |
| `matcher` | match every `sharp` image's truth against itself: 100 %, zero unmatched |
| `truth_normal` | the harness photometry (`ATHENAEUM_REPO`-independent: athenaeum-lab `harness/acceptance/grid-report.py`, function `photometry`, imported as `ln-scale-audit/truth.py` does) on `truthcheck`: per target mean ratio within ±0.3 % of `s_true`, slope within ±0.1 %/px² over FWHM², ≥ 30 stars per target |
| `truth_crowded` | the same in the crowded-fallback mode on `truthcrowd`: within ±0.8 % mean. Record the gap; Task 11 uses it as the error bar. |
| `erp_emulation` | `erplib.py` against the ERP's own LN log on the `pairs` targets: within 0.1 % (skipped with a note when no ERP LN output exists yet) |
| `probe_build` | `cargo check -p athenaeum-core --examples` (one compile, `CARGO_BUILD_JOBS=4`) |
| `probe_parity` | see below |
| `seed_parity` | the in-crate pin `measure_plane_detailed_returns_the_seeds_it_fitted_and_the_same_measurement` (the returned `Vec` is the one the fitter consumed; `seeds.len() == stars_detected`) passes, and on a `sharp/m4/f3.5` image the probe's seed lines are brightest-first by `flux` (the detector's order). A star-for-star external comparison is not available without a product change, and the gate's line says so. |

**`probe_parity`.** In `tierA-lnB-f1`, pick one calibrated mono frame of set 204 (6224 × 4168):
- `stacking_artifacts` kind `calibrated` gives the frame;
- its `fits.0` row gives the fits artifact (`…/fits/<group>/<stem>.p0.athf`; check that it exists on disk);
- its `metrics` row's `payload_json` is a **`FrameMeasurement`** (`{"width", "height", "channels": [...],
  "durationMs"}`), so compare `channels[0]` and ignore `durationMs`;
- the run's config comes through `config_hash`, because `stacking_artifacts` has no `run_id`:
  `SELECT config_json FROM stacking_runs WHERE config_hash = <the artifact's config_hash>`. The run's `config_json`
  decodes directly as a `StackingConfig`; the probe's `v.get("config")` fallback is for per-set documents.

Then:
1. Run `star_probe measure <calibrated> --config <run config> --paths measure --out a.jsonl`.
2. Run `star_probe dump-fits <fits.0 path> --out b.jsonl`.
3. Pass when every field of every fit is equal, except `peak5` (not persisted), and the `channel` equals the stored
   `channels[0]` (timings are `#[serde(skip)]`, so they are not in either).

- [ ] **Step 1: Implement `gates.py`** (each gate is a function returning `(ok, detail)`; the CLI runs them in table
  order).
- [ ] **Step 2: Run it**

```bash
memory_pressure | tail -1
cd star-lab && python3 bin/gates.py
```

Expected: every gate PASS. A FAIL stops the task: report it with numbers, and fix the generator, never the bar. The
`erp_emulation` gate may be `SKIP` until Task 11's ERP batch has run; then re-run that gate.

- [ ] **Step 3: Commit and push (athenaeum-lab)** with the message
  `starlab: the lab's own gates (integration, mosaic, injection, gain, determinism, CRLB, matcher, harness truth, probe parity)`.

---

### Task 11: The baseline run and report

**Files:**
- Create (athenaeum): `docs/superpowers/research/<YYYY-MM-DD>-star-measurement-baseline.md`
- Create (athenaeum-lab): `star-lab/results/baseline-<date>/` (the scorecard `.md` and `.json`, the gates summary,
  the run log; committed, small text only)

**Interfaces:**
- Consumes: everything above.
- Produces: the baseline report that part 2's spec argues from.

- [ ] **Step 1: The ERP batch (controller step, after the owner confirms the interactive instance is closed)**

```bash
cd star-lab && python3 erp/erp_v2.py stars manifests/v2.json && python3 erp/erp_v2.py ln manifests/v2.json
python3 bin/gates.py --only erp_emulation
```

- [ ] **Step 2: The probes over the injections and the real-data rows**

Run `run_probe.py` over `manifests/inject.json` (calibrated injections: every path; pair injections:
`ln_scale_probe` only).

For the LN scale per member on set 204 mono and OSC, run B and Horse C11 G:
- reuse the checkpoint-B probe outputs and harness truth already under `~/.athenaeum-acc/lnB/`, re-reading truth on
  each run's own LN reference (as checkpoint B did). Horse C11 G has no persisted LN reference in the acceptance
  copies, so it has no injected pair (Task 7);
- add the injected pairs' `s_inj` rows;
- carry the `truth_crowded` gap as the error bar on members that took the crowded fallback.

Run `run_probe.py` over the star-test corpus too: m42 (77), m101 (30) and singles (38) from `~/Pictures/star test/`,
through `star_probe measure --paths measure,analysis`, one frame at a time. Write the frame FWHM and eccentricity
per path beside the ERP's per-frame table already next to the frames, joined by basename. The
`reference_star_test_datasets` caveat applies: m101 `_0002` and `_0004` are known-bad ERP rows. Report them, never
tune toward them.

- [ ] **Step 3: The final scorecard**

```bash
python3 bin/scorecard.py manifests/v2.json --probe-dir ~/.athenaeum-acc/star-lab-v2/probe --erp-dir ~/.athenaeum-acc/star-lab-v2/erp --out results/baseline-$(date +%F)/scorecard.md --json results/baseline-$(date +%F)/scorecard.json
python3 bin/gates.py > results/baseline-$(date +%F)/gates.txt
```

- [ ] **Step 4: Write the report** (athenaeum, English, no third-party names: "the ERP")

Sections:
1. **Verdict.** The three to five largest defects, each with its number.
2. **Method.** Branch heads, the probe binary's commit, the families, the injections, and the gates summary.
3. **Rows 1–11.** For each: the headline table, and the families where ours differs from truth by more than 2 ×
   its own scatter or more than the ERP does.
4. **Real data.** The injected rows and the LN rows with their error bars.
5. **S-2 per consumer.** The three truth widths side by side for the filters and the master FWHM: the numbers the
   owner's ruling needs.
6. **The pre-warp injection mode:** "not done" (P-7).
7. **The ranked list for part 2.** Each item gives the defect, the row, the number and the consumer it harms. It
   includes the `FitDiagnostics` side channel as part 2's first task (S-11).

Leave every number exactly as the scorecard printed it. Never write "about" for a measured number.

- [ ] **Step 5: Commit**
  - athenaeum (branch `star-truth-lab`, no push): `docs(research): star measurement baseline - the truth lab's
    scorecard of today's code`.
  - athenaeum-lab (push): `results: baseline <date> scorecard and gates`.

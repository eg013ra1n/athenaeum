# Star measurement, part 1: the truth lab and the scorecard

**Date:** 2026-10-06. **Branch:** `star-truth-lab` (athenaeum), `main` (athenaeum-lab).

**Program.** This is part 1 of three, in the order the owner approved on 2026-10-06:
1. Truth lab and scorecard (this spec). The product does not change.
2. The stacking PSF.
3. The Analysis-tab detector.

**Owner's goal.** The PSF that every stacking calculation uses must be measured correctly. That covers:
- the fit's background;
- local normalization (LN): seeds, selection, F1's common FWHM;
- weights and the FWHM and eccentricity filters;
- the master's FWHM.

Registration is good today, measured by the owner against the external reference pipeline (the ERP), and it stays as
it is.

**Notes this program argues from.** None of these is a spec:
- open items, "Measurement/detector cycle", and the LN spec §15 point 18;
- the LN Task 16b lab findings (athenaeum-lab `star-lab/findings.md`):
  - §3: ten definitional differences, each with its measured cost;
  - §2.2 and §5: the ERP's errors on truth, and the never-copy list;
- `star-lab/f1v2-findings.txt`: the F1 v2 geometry search;
- research `2026-10-04-ln-scale-psf-width-audit.md` and `2026-10-04-ln-math-review.md`;
- the 2026-09-07 eccentricity audit, on branch `feature/star-detection-fix`.

## 0. What part 1 delivers

A referee for star measurement, and the baseline numbers of today's code against it:

1. **Generator v2.** Synthetic frames with exact truth, extending the Task 16b generator (`star-lab/synth.py`) where
   it is blind today: sharp and undersampled stars, OSC mosaics, structured backgrounds, densities, field-dependent
   PSFs, doubles, trails and artefacts.
2. **Injection.** Synthetic stars with known parameters, written into real calibrated and registered frames, so
   real noise, background and nebulosity are kept.
3. **`star_probe`.** An athenaeum-core example that runs one image through the production measurement paths and
   writes every measured star.
4. **The scorecard.** It matches measured stars to truth and reports each path against each fundamental and against
   the noise limit (Cramér–Rao lower bound, CRLB). The ERP's numbers on the same inputs sit beside ours, as a
   comparison.
5. **The baseline report** at the branch head, the record part 2's spec argues from.

**No product code changes in part 1.** The athenaeum diff is the example, docs, and the harness move already
committed (1bb4b277).

## 1. Doctrine (binding)

- **The truth is the referee.** It takes three forms:
  - synthetic ground truth;
  - photometric truth on real frames (the harness's growth-curve photometry);
  - injected stars on real frames.
- **The ERP is a comparison only.** It runs on the same inputs. It is never the referee and never a label, and its
  measured errors are never copied:
  - the +1 px on the measuring ellipse;
  - the unrotated ellipse;
  - a fixed β for FWHM and background;
  - the raw-maximum saturation reference.
- **Part 1 sets no bars on the product.** Part 2's spec declares its bars before any fix is measured. They are
  anchored on this baseline and on the CRLB, and are never loosened or tuned toward afterwards.
- **No third-party program is named in athenaeum.** The ERP's automation lives only in athenaeum-lab.

## 2. Where things live

| What | Where |
| ---- | ---- |
| Generator v2, injection, matcher, CRLB, scorecard, run drivers, ERP automation | athenaeum-lab `star-lab/` (Python 3 + numpy; the ERP drivers stay under `star-lab/erp/`) |
| Grid manifests (families, seeds, sizes, image hashes) | athenaeum-lab `star-lab/manifests/` (committed) |
| Images, truth CSVs, probe outputs | `~/.athenaeum-acc/star-lab-v2/` (never committed, regenerable from the manifests) |
| `star_probe` | athenaeum `crates/athenaeum-core/examples/star_probe.rs` (compiles with the product; built with `CARGO_TARGET_DIR=.superpowers/target-acc`) |
| This spec, the plan, the baseline report | athenaeum `docs/superpowers/{specs,plans,research}/` |
| Photometric truth on real data | athenaeum-lab `harness/acceptance/grid-report.py` (reused, not rebuilt) |

The lab finds athenaeum through `ATHENAEUM_REPO`, the same convention as the harness.

## 3. Generator v2

### 3.1 Rendering accuracy

Every star is integrated over each pixel's area, with a sub-pixel grid of `os × os` inside ±3 FWHM of the centre,
and centre-sampled beyond:
- `os` is at least 5;
- `os` is at least 15 when FWHM < 2.5 px. The current 5 × 5 is too coarse at FWHM 1.2.

Self-checks, which are part 1's own gates (§9):
- a Gaussian rendered at FWHM 1.2 matches the exact erf pixel integral to within 1e-3 of its peak on every pixel;
- the total flux of every profile family matches the analytic flux to within 1e-4.

**Truth parameters are those of the PSF before pixel integration** (ruling S-2):
- minor- and major-axis FWHM;
- eccentricity;
- angle;
- β for Moffat families.

The truth CSV also gives the FWHM of the PSF convolved with the pixel box, as a second column. The geometry rule from
2026-09-07 stays: the minor axis is the seeing and the major axis grows with eccentricity.

### 3.2 Families

Each family is a manifest entry: its parameters, a seed, and how many images. A frame is 2048 × 2048, float32 in
[0, 1], with Poisson noise on sky and star (gain), read noise, and a saturation clip. Fluxes follow
N(>F) ∝ F^−slope, so every family spans the SNR ladder from below detection to saturated.

| Family | Parameters | Serves |
| ---- | ---- | ---- |
| `sharp` | FWHM {1.2, 1.6, 2.0, 2.6, 3.5, 5.0, 8.0} × profile {Gaussian, Moffat β 2.5, 4, 10, blur (a Moffat 4 core convolved with a Gaussian seeing of 1× and 2.5× the core's FWHM)}, isolated, 25 stars/Mpx | detection, FWHM and β on the full width range; where the 2026-09-07 F1/F2 bite (FWHM < 3) |
| `shape` | e {0, 0.3, 0.5, 0.7} × θ {0, 30, 60, 90}° at FWHM 2.6 and 4.0, Moffat 4 | eccentricity and angle by SNR, the round-star floor |
| `density` | 5, 40, 200, 600, 1000 stars/Mpx at FWHM 3.0, Moffat 4 | crowding: fit background, F1's annulus, completeness |
| `background` | flat; linear gradient 10 % of sky across the frame; vignetting 20 % at the corners; fractal nebula at 0.5, 2 and 5 σ_sky amplitude (spectral slope −3) | background under the star; false detections on nebula |
| `fieldpsf` | FWHM × 1.3 at the corners, tangential e up to 0.3, Moffat 4 | frame aggregates and LN's common F on a varying PSF |
| `doubles` | pairs at separations {2, 3, 5, 7, 10} px, flux ratios 1:1 and 1:4, FWHM 2.6 and 4.0 | blend handling; the audit's contamination finding |
| `trails` | straight tracks 3–15 px and one curved track per star, minor axis at the seeing | trail truth for part 3 (the 2026-09-07 F4) |
| `artefacts` | hot pixels (40/Mpx, single and 2-px) and cosmic-ray hits (worm-shaped, 1–6 px) over the `sharp` FWHM 2.6 field | false detections |
| `osc` | an RGGB mosaic of three channel renders (§3.3) | per-plane flux and FWHM; the blue-plane −0.6 % |
| `pairs` | the Task 16b `iso` and `main` pair grids with known `s_true`, plus OSC pairs and `density` pairs at 200 and 600 stars/Mpx | LN relative scale, F1's slope, per plane |

### 3.3 OSC

- **Channels.** Each one is rendered at full resolution with its own PSF width (R 1.00, G 0.95, B 1.10 × the
  family's FWHM: chromatic seeing). Its own sky level and star colours come from a spread of colour indices.
- **Mosaic.** The RGGB mosaic samples each channel at its CFA pixels, and noise is applied to the mosaic.
- **The production debayer** (`astroimage::processing::vng::vng_debayer_f32`, through `star_probe --debayer`) then
  produces the three planes, as the Calibrate stage does.
- **The ideal plane.** The noiseless-then-noised full-resolution render of each channel is kept beside the
  debayered plane, so a per-plane error can be split into debayer and measurement.

### 3.4 Determinism

Same manifest, same seed, same images. Each manifest entry records the SHA-256 of every image it produced. The
self-check regenerates one image per family and compares the hashes.

## 4. Injection into real frames

`star-lab/inject.py <frame.fits> --family <spec> --out <dir>` writes:
- the frame with injected stars;
- a truth CSV;
- a mask of the injection stamps.

**Placement.** On a jittered grid, at least `12 × FWHM_inj` from any real star and from each other:
- real stars come from the harness's detection on the frame itself, dilated by `6 × FWHM_frame`;
- no star goes on a pixel above 0.5 × the frame maximum.

**Stars.** The parameters follow the `sharp` and `shape` families, at FWHMs near the frame's own (0.8×, 1.0×,
1.3×), across the SNR ladder.

**Noise.**
- A star adds its own Poisson noise with the frame's gain.
- The gain comes from the header's `EGAIN` (e-/ADU). Without it, the gain is estimated from the frame's sky variance
  against its level across tiles.
- The truth CSV records the gain and its source.

**Pairs for LN.** The same star list goes into a reference and a target, with target flux = reference flux / `s_inj`
(`s_inj` chosen per pair). The scale truth for these pairs is `s_inj`, read from the per-star pair dump of
`ln_scale_probe` over the injected stars only.

**Frames:**
- set 204 mono and OSC, both registered frames and the LN reference;
- Horse C11 L and G;
- run B;
- the sharp frames of the star-test corpus with FWHM ≤ 2.6 px (singles: LDN 1272 B, B150, 2025-08-23 B, Gamma
  Scuti; m42 30 s).

All of them come from the acceptance copies and `~/Pictures/star test/`. Nothing is written back to a catalog or to a
source file.

**Self-check:** every pixel outside the stamps is bit-identical to the source frame, and inside it the result is
the source plus the recorded star plus recorded noise.

## 5. `star_probe` (athenaeum example)

```
star_probe <image.fits> [--plane N|all] [--debayer RGGB|GRBG|GBRG|BGGR]
           [--paths measure,betas,register,analysis] [--betas 2.5,4,6,10]
           --out <file.jsonl>
```

It measures through the production entry points, never a re-implementation (ruling S-3):

| Path | Entry point | What it feeds today |
| ---- | ---- | ---- |
| `measure` | `stacking::measure::measure_frame_with_fits` with `MeasureOptions::default()` (Auto β, fast seeds) | weights, the FWHM and eccentricity filters, mono registration's star list, LN seeds |
| `betas` | `stacking::psf_signal::fit_stars_with_beta` at each listed β, on the same fast-detector seeds the `measure` path uses | β sensitivity of FWHM, flux and background: the β flips |
| `register` | `stacking::register::detect::detect_stars` with the default `DetectionConfig` | OSC registration, LN detection seeds |
| `analysis` | the analyzer the Analysis tab builds (`analysis::analyzer::build_analyzer` with the default config), `analyze_data` | the Analysis tab, the plate-solve input gate, the collab gate |

**Output.** One JSON line per measured star per path (and per β), and one frame line per path with that path's
frame aggregates (its reported median or weighted FWHM and eccentricity, star count, wall time). Star fields
are camelCase:
- `path`, `plane`, `beta`;
- `x`, `y`;
- `background`, `amplitude`, `signal`, `peak5`;
- `fwhmX`, `fwhmY`, `fwhm`, `ecc`, `thetaDeg`;
- `residual`, `snr`;
- `accepted` and the `reason` when refused;
- `flags` (saturated, contaminated, trailed, as the path reports them).

A path that does not produce a field writes `null`. `ln_scale_probe` stays the LN measurer, unchanged: pairs, F1,
selection, the per-star pair dumps.

## 6. Matching, SNR and the CRLB

- **Match.** A truth star and a measured star match when they are within `max(1.5 px, 0.5 × FWHM_truth)` of each
  other, one-to-one, assigned greedily from the brightest truth star down (ruling S-4). Every unmatched detection is
  classified by what lies under it in the lab: a hot pixel, a cosmic ray, nebula, the wing of a star, or nothing.
- **SNR.** Each truth star's SNR is its Fisher (matched-filter) SNR, `sqrt(Σ μ_star² / var)` over its noiseless
  rendered stamp, where var is sky plus star plus read noise. The bins are < 10, 10–20, 20–40, 40–100, 100–300,
  > 300, and saturated.
- **CRLB.** The Fisher matrix of (x, y, flux, background, σx, σy, θ) is computed numerically from each truth star's
  noiseless stamp under the same noise model. The bound for each parameter is `sqrt((F⁻¹)ᵢᵢ)`; FWHM and eccentricity
  take theirs by the delta method.
  - Self-check: for a bright Gaussian on zero background, the centroid bound is within 1 % of σ/√N.

## 7. The scorecard

`star-lab/scorecard.py <manifest> <probe-outputs…>` writes a Markdown table and a JSON. Each row is (path ×
fundamental × family × SNR bin), with three columns: ours, the ERP (where it measures the same thing), and the CRLB
or the ideal. The rows:

1. **Detection:**
   - completeness by SNR bin;
   - false detections per Mpx, by cause (§6);
   - bright coverage: the share of the 300 brightest unsaturated truth stars that the path measures (the 2026-09-07
     audit's metric).
2. **Centroid:** bias (x, y) and rms, and efficiency = rms / CRLB.
3. **FWHM:**
   - the median and IQR of (measured − truth) / truth by bin, against both truth columns (§3.1);
   - β sensitivity: the spread of each star's FWHM across the `betas` fits, with the β-free second-moment FWHM of
     the same stars beside it;
   - the Auto β pick distribution per family.
4. **Eccentricity and angle:**
   - eccentricity bias by bin at each truth e;
   - the round-star floor (the median measured e at truth e = 0);
   - angle error (circular rms) at e ≥ 0.3.
5. **Background under the star:** (fitted background − true local sky) / σ_sky,pixel, by family and density.
6. **Flux:**
   - the captured fraction, `signal / flux_img`, by FWHM and profile;
   - on `pairs`, the relative scale against `s_true` with its slope in %/px² of FWHM² difference
     (`ln_scale_probe`, per plane).
7. **Frame aggregates:** each path's reported frame FWHM and eccentricity against the truth population's median, over
   the stars the path measured and over all truth stars.
8. **OSC per plane:** flux ratio and FWHM against truth, on the ideal and on the debayered plane.
9. **Cost:** wall time per path per image, and peak RSS from `/usr/bin/time -l` with one path per invocation.

## 8. Real-data rows

| Row | Data | Truth |
| ---- | ---- | ---- |
| Injected-star rows 1–6 (as §7) | §4 frames | injection truth |
| LN scale per member | set 204 mono and OSC, run B, Horse C11 G | the harness photometric truth (`grid-report.py`) on the run's own LN reference, plus the injected pairs |
| Frame FWHM and eccentricity | the star-test corpus (m42, m101, singles) | none absolute: the injected rows set the referee. The ERP's per-frame and per-star tables already beside the frames are a comparison column. |

## 9. Part 1's own gates (the lab is right before it judges)

| Gate | Bar |
| ---- | ---- |
| Pixel integration | Gaussian FWHM 1.2: every pixel within 1e-3 of the peak of the erf integral; total flux within 1e-4 for every profile family |
| Mosaic | every CFA pixel equals its channel render at that pixel |
| Injection | outside the stamps bit-identical; inside it source + recorded star + recorded noise |
| Determinism | the regenerated image hashes equal the manifest's |
| CRLB | bright Gaussian centroid bound within 1 % of σ/√N |
| Matcher | a field measured at the truth positions matches 100 % with zero false detections |
| Photometric truth on synthetic | the harness photometry on `pairs` within ±0.3 % mean and ±0.1 %/px² slope of `s_true` (Task 16b read +0.19 %, −0.04) |
| ERP emulation still faithful | `erplib.py` reproduces the ERP's own LN scale within 0.1 % on the `pairs` targets |
| `star_probe` | `cargo check -p athenaeum-core --examples`; on a `sharp` FWHM 3.5 field the `measure` path's star count and frame FWHM equal `measure_probe`'s on the same image |

A gate that fails stops part 1, and the cause goes to the owner. The generator is fixed. The bar is not.

## 10. The baseline report

`docs/superpowers/research/<date>-star-measurement-baseline.md` in athenaeum, dated the day it is written:
- the scorecard for every family and every path at the branch head;
- the injected and real-data rows;
- the ERP column.

It ends with the ranked list of what part 2 must fix, each item with its numbers. Part 2's spec sets its bars from
this report before any fix is measured.

## 11. Machine budget

16 GB RAM, swap nearly full:
- one heavy job at a time;
- `memory_pressure` checked before a batch;
- the ERP runs one instance at a time through `star-lab/erp/run_job.sh`, after the owner has closed any interactive
  instance.

A 2048² float32 frame is 16 MB. A family's images are generated and measured one at a time. Disk: at most 8 GB under
`~/.athenaeum-acc/star-lab-v2/`. `star_probe` is built once into `.superpowers/target-acc` with
`CARGO_BUILD_JOBS=4`.

## 12. Out of scope

- Any product change. Fixes are parts 2 and 3.
- Registration (owner: it is good). Part 2 adds a registration-parity gate, because mono registration takes its stars
  from the Measure fits.
- The rustafits characterisation tests of the 2026-09-07 audit. Part 3 recreates them; the scorecard's `analysis`
  rows replace the lost `ecc_probe` and `ecc_floor` numbers as the baseline.
- A neural classifier for star / artefact / blend / trail. It is considered only if the scorecard shows the rules
  failing on false detections or blends, as an experiment with numbers.
- Moving existing acceptance records.

## 13. Rulings

- **S-1** Lab code, manifests and ERP automation live in athenaeum-lab. `star_probe` lives in athenaeum examples.
  The spec, plan and baseline report live in athenaeum docs (owner, 2026-10-06).
- **S-2** Truth FWHM, eccentricity and angle are those of the PSF before pixel integration. PSF ⊗ pixel is reported
  beside them. A point-sampled fit reads between the two. Which one the product should report is part 2's ruling, and
  it goes to the owner.
- **S-3** The probe calls production entry points only. A path measured through a re-implementation is not a
  baseline.
- **S-4** The match radius is `max(1.5 px, 0.5 × FWHM)`, one-to-one and greedy by truth brightness. The SNR is the
  Fisher SNR. The bins are as in §6.
- **S-5** Part 1 sets no product bars. Part 2 sets them before measuring a fix, anchored on this baseline and the
  CRLB.
- **S-6** Images are never committed. Manifests carry seeds and image hashes.
- **S-7** Injected stars carry Poisson noise at the frame's gain. The placement keeps `12 × FWHM` from real stars.
- **S-8** OSC goes through the production VNG. The ideal plane is kept beside it.
- **S-9** The ERP runs on the same inputs, as a comparison column. It is never a label, and its errors are never
  copied.

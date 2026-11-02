# Sharp-frame registration fragility (2026-11-02)

**Question** (ruling PD-39, outside Plan D). In runB mono L (83 members), the three sharpest frames register from far
fewer inliers than the rest: 162 / 49 / 39 at 87 / 214 / 355, against about 1 600–1 750. Frame 162's 87-inlier map is
off a direct fit by up to 6.4 px, and D3's change of about 2 registration stars flipped it to 159 inliers. Four
questions follow: the mechanism, how far it reaches, what it cost the masters, and what fixes it.

**Answer.** The stage is the **quad seed**. `seed_affine` (`stacking/register/align.rs:323`) fits the seed with
`solvemyastro::quad::fit_affine` (`solvemyastro/src/quad.rs:612`). That fit is a plain least squares over every quad
match (`quad.rs:636`).
- **False matches.** In 61 of the 83 frames the match set carries 1–3 false matches. The 20 % scale-ratio filter
  (`quad.rs:595`) lets them through. On every affected sharp frame they are the same two pairs, 2 405 px and 4 298 px off.
- **Seed error.** A false match pulls the seed by its offset divided by the number of true matches.
- **Fewer true matches on sharp frames.** Median 486, against 718–810 elsewhere. A sharp frame's registration list
  (Measure fits through the register cuts, `register/detect.rs:184`, brightest 2 000 at `:250`) loses twice as many of
  its brightest stars to the saturation cut. It therefore overlaps the OSC reference's list less: 0.82 against 0.88.
  So its seed is off by 13–15 px at the median and 43–50 px at the far side.
- **Pairing.** Through that seed, pairing at the fixed radius 2 × 1.9 = 3.8 px (`align.rs:905`) finds 6–21 stars.
- **The two outcomes.**
  - **Below 8 pairs:** the quad path fails, and the plate-solve seed takes over and gets it right (frame 31: 1 583
    inliers).
  - **8 or more:** RANSAC and the refit run on a small cluster. **One** re-pair (`align.rs:1057`) grows it to 95–357
    pairs, and the map is accepted, because nothing gates a quad-path map beyond 8 inliers.
- **Frame 162.** Its map is right only where its first 10 stars lay: off the direct fit by 1.05 px at the median and
  6.40 px at most.

Without the false matches, the same least squares gives a seed within 0.13 px (median) and 0.5 px (max). **Fix:** a
robust seed, clipped or RANSAC least squares over the quad centres. On the exact lists it registers all 83 frames right
(max 0.07 px off truth) and leaves the 79 good frames bit-identical.

## Method

- **Data, all read-only.** Clones `~/.athenaeum-acc/pland-{runB,s204,hc11}-{dbase,d2d,d3}` and the pinned trees
  `lnB-runB-f1`, `tierA-lnB-f1`, `hc11B-f1`, opened with `immutable=1`. Calibrated mono frames from the project's own
  folder. Python on numpy only.
- **Exact replay.** A dev probe (`regfrag_probe.rs`, kept with the job data in `~/.athenaeum-acc/regfrag/`, not in the
  repo) does four things:
  - detects the reference's stars exactly as a run does (`frame::reference_stars`, no fits: the reference is OSC);
  - re-measures mono frames exactly as the run's Measure does (`measure_frame_pass1/2` at the group β, then
    `stars_from_fits`);
  - runs the real `align()` on any pair of star lists;
  - restates each stage of `align()` with the crate's public pieces (`build_quads`, `match_quads_stats`, `fit_affine`,
    `KdTree2`, `ransac_fit`, `refit_weighted`) to count what every stage leaves.
- **Checks of exactness.**
  - **Task-13 tree (`lnB-runB-f1`, run 6).** The stored `.athf` lists reproduce all 78 quad-path frames' inliers
    exactly, 161's 176 included. They also reproduce the 5 frames whose quad path failed into the plate-solve seed.
  - **dbase / d2d.** The re-measured lists give 23 912 fits on frame 162 (equal to D0's dump and d2d's `starsFitted`).
    They reproduce 82 of 83 frames' inliers exactly, frame 162's 87 and its map included (1.05 / 6.40 px off truth). The
    83rd, frame 31, failed to the plate-solve seed in the run, and the probe passes no seed.
- **Truth.** A direct homography fit of each suspect frame's unsaturated fits against 20 well-registered members in
  reference coordinates, about 365 000 pairs at 0.30 px rms each (the D3 growth method, `maptruth.py`). Truth for the
  other frames is their stored map, which sits within 0.03–0.07 px of a direct fit.

## Q1 — the mechanism, stage by stage

Exact dbase / d2d lists, the code `main` runs today (`q1/stages_d2d.out`):

| frame | F px | list overlap | quad matches true / false | seed error med / max px | seed pairs | RANSAC → refit | re-pair → refit | final | map vs truth med / max px |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| 162 | 2.17 | 0.820 | 486 / 2 | 13.4 / 42.6 | 13 | 10 → 10 | 95 → 87 | **87** | 1.05 / 6.40 |
| 31 | 2.17 | 0.801 | 429 / 2 | 14.9 / 50.0 | 6 | — | — | fails (< 8 pairs) → plate-solve seed | (run: 1 583, 0.07 max) |
| 74 | 2.25 | 0.840 | 551 / 1 | 5.1 / 16.5 | 685 | 681 → 680 | 1 681 → 1 661 | 1 661 | 0.03 / 0.07 |
| 49 | 2.28 | 0.816 | 464 / 2 | 14.1 / 46.6 | 20 | 16 → 16 | 217 → 214 | **214** | 0.13 / 0.81 |
| 39 | 2.30 | 0.816 | 483 / 2 | 13.5 / 44.5 | 21 | 18 → 18 | 357 → 355 | **355** | 0.15 / 0.72 |
| 132 | 2.31 | 0.842 | 577 / 0 | 0.1 / 0.4 | 1 686 | 1 685 → 1 663 | not needed | 1 663 | 0.03 / 0.07 |

By FWHM band, over all 83 frames:

| F band px | frames | overlap | true quad matches (median) | false per frame (0/1/2/3) | seed error median px | seed pairs median (min) |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| < 2.5 | 7 | 0.823 | 486 | 1 / 2 / 4 / 0 | 13.4 | 21 (6) |
| 2.5–3 | 8 | 0.875 | 718 | 7 / 1 / 0 / 0 | 0.1 | 1 755 (720) |
| 3–4 | 19 | 0.874 | 782 | 4 / 12 / 3 / 0 | 3.5 | 1 025 (177) |
| 4–6 | 49 | 0.879 | 810 | 10 / 26 / 12 / 1 | 5.1 | 804 (65) |

How the stages combine:

1. **List (detect.rs:184, :250).** The saturation cut (background + Moffat amplitude ≥ 0.95) refuses 381 of a sharp
   frame's 2 500 brightest fits, against 191–280 on the others. The refused stars really are saturated: Measure's own
   flag agrees on all but 3. A star of the same flux peaks about 3.5 times higher at F 2.2 than at F 4.1, so the sharp
   list reaches fainter and overlaps the reference's list less.
2. **Quads (quad.rs).** A quad is a star and its 3 nearest neighbours within the list, so it matches only when all 4
   stars and their neighbourhood are in both lists. Fewer true matches follow from the lower overlap: 429–577 against
   518–962.
3. **Seed (quad.rs:595, :636).** The false matches are real star patterns whose 5 distance ratios agree within 0.007
   and whose scale is within 20 %. The same two pairs recur on 162, 31, 49 and 39, because the same stars are in every
   mono frame. The least squares takes them at full weight. Seed error correlates with false / true at r = 0.92.
   Dropping the two pairs leaves a seed within 0.13 px median and 0.5 px max on all four frames.
4. **Pairing (align.rs:905, :466).** The seed error is affine, so it is zero only along a line. At 3.8 px the pairs come
   from a band along that line. Below 8 pairs → `TooFewMatches` → the plate-solve fallback (align.rs:993), when both
   frames are solved.
5. **RANSAC / refit / one re-pair (align.rs:1049–1070).** On 10–18 clustered inliers, `auto` resolves to similarity or
   affine. The single re-pair through it reaches 95–357 pairs. The homography refit on them extrapolates, and nothing
   asks for a second round or a quality check.

**Fragility, measured.** Deleting one registration star (every 10th of 2 000) from 162's exact list and re-running
`align()` gives 4 / 200 maps right (≤ 0.2 px), 16 off by 0.2–1 px, **174 off by more than 1 px** and 6 failed. With a
robust seed, 200 / 200 are right. Frames 49 / 39: 0 / 11 right today and 200 / 200 with the robust seed. Frame 74, a
sharp frame with one false match, is right in all 200 either way. D3's 159-inlier map is one of the rare lucky
outcomes, and it is still 0.20 px median, 1.58 px max off truth.

**The brief's candidate causes:**
- **(a) Tolerance or list size scaled with FWHM.** Nothing in `align` reads FWHM. `ransacTolerancePx` 1.9 is 11–16
  times the final rms (0.12–0.17 px). A tighter tolerance (1.0) fails 11 frames. Not the mechanism.
- **(b) Top-2000 overlap.** Yes, as the multiplier: overlap → true matches → seed error per false match. Rank flips
  with seeing are not needed to explain it; the saturation cut alone moves the list.
- **(c) Reference.** OSC frame 123 (#0171), a different camera (6248×4176, 1.887″/px, field rotated 180°, scale
  0.99227). F 2.56 px, rank 36 / 101 by F and 18 / 101 by weight in its group, list made by the detector. It is pinned
  by the harness (`planb-ref.json`, mode manual), not picked by PD-11's Measure-time weight; s204 pins OSC frame 28949
  the same way.
  - The same sharp frames register normally against a **mono** reference: set 109 (reference 29390, ATR2600M) has
    minimum 1 414 over 368 frames, and frame 28986 gets 1 886 inliers there against 88 on OSC 28949.
  - OSC members against OSC 123 match 959–1 128 quads and keep 1 844–1 903 inliers.
- **(d) Undersampling.** Not a factor. The true quad matches agree to 0.10 px, the sharp frames' final rms is
  0.118–0.141 px (below the group's 0.17), and 74 / 132 / 141 (F 2.25–2.37) register normally.
- **(e) Also found.** The plate-solve seed is a fallback for a quad path that **fails**, never for one that succeeds
  badly. And the seed is biased in 61 of 83 frames: blunt frames survive a 3–9 px seed only because more true matches
  keep the band wide.

## Q2 — scope

Every group in the clones and pinned trees, latest run (`q2/q2.tsv`):

| dataset (run) | group | reference | median inliers | F median [min, max] | below ½ median: frame (inliers, F, F / median) | quad path failed → plate-solve seed |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| runB dbase = d1 = d2d (7) | mono L, 83 | OSC 123 | 1 745 | 4.08 [2.17, 5.12] | 162 (87, 2.17, 0.53), 49 (214, 2.28, 0.56), 39 (355, 2.30, 0.56) | 31 (F 2.17) |
| runB d3 (7) | mono L, 83 | OSC 123 | 1 745 | 4.08 [2.17, 5.11] | 162 (159), 49 (214), 39 (355) | 31 |
| runB, all stages | OSC, 101 | OSC 123 | 1 914 | 2.64 [1.86, 3.09] | none | none |
| s204 dbase / d2d / d3 (6) | mono, 92 | OSC 28949 | 1 734 | 4.12 [2.17, 5.22] | 28986 (88, 2.28, 0.55), 28994 (99, 2.30, 0.56) | 28964 (= frame 162, 1 611), 29053 (2.17, 1 580) |
| s204 | OSC, 105 | OSC 28949 | 1 908 | 2.64 [1.86, 3.51] | none | none |
| lnB-runB-f1, Task-13 tree (6) | mono L, 83 | OSC 123 | 1 697 | 4.03 [1.99, 5.02] | 161 (176, 2.40, 0.60) | 74, 31, 39, 49, 148 (F 1.99–2.48) |
| tierA-lnB-f1, set 204 (5) | mono, 92 | OSC 28934 (= runB's 123) | 1 692 | 4.05 [1.99, 5.08] | 29018 (176, 2.40, 0.59) | 5 frames, F 1.99–2.48 |
| set 109 (September) | 368 frames | **mono** 29390 | — | — | none (minimum 1 414) | — |
| hc11 dbase / d2d / d3 (6) | mono L, 76 | mono 69845 | 428 | 6.98 [5.61, 9.19] | 70062 (31, 8.32, **1.19**); 69954 (205), 69865 (211) at 0.48–0.49 | none; 1 frame failed outright (F 8.14) |
| hc11 (6) | mono G, 30 | mono 69845 | 381 | 6.58 [4.95, 13.28] | none | none; 2 failed outright (F 9.81, 13.28) |

- **Where it reaches.** Mono frames registered onto an **OSC** reference: runB mono and s204 mono, the same LDN1272 rig
  and largely the same frames. There it hits the 7–8 sharpest of 83–92 (F ≤ 2.5 px, ≤ 0.6 × the group median), in every
  catalog stage since the Task-13 tree.
- **Same frame, different outcome.** Which of those frames lands below half and which fails into the plate-solve seed
  differs from catalog to catalog. Frame 162 is 1 563 / right (Task-13 tree), 87 (dbase–d2d), 159 (d3), and 1 611 via
  the plate-solve seed in s204. That is the fragility itself.
- **Not runB-mono-only, but not everywhere.** Not in any OSC group (same camera as the reference). Not with a mono
  reference (set 109). Not in hc11, where the low frames are the **blunt** ones:
  - 70062 has 391 of its 454 fits above eccentricity 0.8 (median 0.869), so only 44 registration stars, and the probe
    reproduces its 37 inliers;
  - hc11's outright failures are frames at 1.17–2.0 × the median FWHM.

  That is a different, legitimate path: poor frames.

## Q3 — cost today

The ≤ 6.4 px map error of frame 162 (the top weight: 1.0 of Σ 24.74, **4.04 %**) is in every runB mono master built from
dbase to d2d, and in what `main` builds today: the probe reproduces `main`'s registration of 162 exactly.

**Master star shapes do not carry it.** Stars were measured at the same positions in the dbase, d2d and d3 masters
(adaptive Gaussian moments), binned by |e| = |d2d map − truth| of frame 162 at the star. dbase − d2d (same 162 map) is
the control; d2d − d3 carries 162's re-registration. Master grid ↔ reference pixels come from 1 512 matched stars.

| \|e\| px | bright stars (≥ 40 σ) | expected without rejection: FWHM / ellipticity along e | observed d2d − d3: FWHM / ellipticity | faint stars (8–30 σ, mean stamp): FWHM / ellipticity | control dbase − d2d FWHM |
| ---- | ---- | ---- | ---- | ---- | ---- |
| 0–0.5 | 1 341 | +0.02 % / +0.0002 | −0.13 % / −0.0001 | +0.14 % / +0.0002 | +1.39 % |
| 0.5–1.5 | 1 252 | +0.22 % / +0.0022 | +0.16 % / +0.0018 | +0.49 % / +0.0001 | +1.30 % |
| 1.5–3 | 1 044 | +1.11 % / +0.0110 | +0.31 % / +0.0004 | +0.34 % / +0.0007 | +1.27 % |
| 3–4.5 | 924 | +1.56 % / +0.0155 | −0.05 % / −0.0014 | −0.08 % / +0.0005 | +1.25 % |
| 4.5–9 | 212 | +1.12 % / +0.0111 | −0.37 % / −0.0030 | −0.37 % / −0.0007 | +1.13 % |

- **What the columns are.**
  - "Expected" is a Gaussian master star of the bin's FWHM plus a 4.04 % copy displaced by the bin's median |e|,
    measured with the same estimator.
  - The observed differences show no trend with |e|, and the sign even turns. They sit at the measurement floor
    (about ±0.4 % FWHM, ±0.002 ellipticity).
  - The control's +1.1–1.4 % is a flat global change between dbase and d2d with the same 162 map.
- **Rejection took the error instead.** Linear fit clip (5.0 / 3.5) rejected **4.33 %** of frame 162's pixels at dbase
  and at d2d. That is the group maximum: the mean is 1.61 %, and the other sharp frames sit at 2.29–2.36 %. At d3 it
  rejected 2.71 %. The extra 1.6 % is about frame 162's whole star population on the misaligned half of the field (its
  ~24 000 fits cover about 3 % of the frame).
- **The cost in the master** is therefore the top-weight frame's starlight missing from stars over about half the field.
  Under inverse-variance weights that is about **2 % more noise** on those star pixels (√(24.74 / 23.74) = 1.021).
  Shape and position are untouched: centroid shift ≤ 0.012 px.
- **Where it would show.** In a small group, or wherever rejection is off or weaker, the error lands in the master
  instead: +1.1–1.6 % FWHM and +0.011–0.016 ellipticity on the misaligned half at a 4 % weight, and more for a frame
  with a larger weight share.
- **What is not attributable to 162.** D3's master FWHM 0.9985 and master Blend 418 vs 437 cannot be credited to it.
  They move with D3's Measure change: dbase → d2d moved the master FWHM by +1.3 % with the same 162 map.
- **Elsewhere.**
  - Frames 49 / 39 are off by 0.13 / 0.15 px median and 0.81 / 0.72 px max: inside the rejection's reach and the
    master's noise.
  - The other cost is Plan D's: PD-39's common set halves whenever the anchor's map is the bad kind (weights ≤ 5 %).
- **Not measured.** `main`'s current default rejection normalization (local, R-SC-1) on this group. These stages ran
  with the global pair.

## Q4 — fix options

Each option was run through the real `align()` on the exact dbase / d2d lists. The robust seed and the wider tolerance
were also run on two more list sets: the Task-13 tree's `.athf` lists and a skyRing-approximated set that differs in
27 of frame 162's 2 000 entries.

| option | measured here | cost | risk | what it moves |
| ---- | ---- | ---- | ---- | ---- |
| **Robust quad seed**: clipped (or RANSAC) least squares over the quad-centre pairs in `seed_affine`; solvemyastro untouched | all 83 right on the exact lists: 162 / 31 / 49 / 39 → 1 616 / 1 583 / 1 614 / 1 612 inliers, max 0.06–0.07 px off truth. Task-13 lists: 6 bad → 0. Approximated lists: 3 bad → 0. The 79 frames already right: **bit-identical maps and inliers**. 200 / 200 right under single-star deletions on 162 / 49 / 39 | ~40 lines in `align.rs`, plus 2 tests: two far false matches among many true; a clean match set that must give a bit-identical seed | low: a clean match set gives the same seed; no config, no wire change | registration rows of the frames whose seed was biased (runB 162 / 49 / 39 / 31, s204 28986 / 28994 / 28964 / 29053) → their masters, LN, Plan D's common set. One registration acceptance round (runB, s204, hc11) with a master star check |
| Re-pair to convergence (repeat step 4b while pairs grow, ≤ 3 rounds) | measured with one extra round (the final map given back as the seed): 162 / 49 / 39 → 1 616 / 1 614 / 1 612, right; 31 still fails to the plate-solve seed | ~15 lines | low | same frames; a frame with < 8 seed pairs and no plate solve still fails |
| Inlier-quality gate + plate-solve fallback (quad map < ½ the group median, or < 25 % of its list → retry from the plate-solve seed) | where the plate-solve seed ran it was right: 31 (1 583, 0.07 px max), s204 28964 = frame 162 (1 611), 29053 (1 580), Task-13 tree 74 / 31 / 39 / 49 / 148 (1 484–1 609) | moderate: the group median needs the two-pass order, or an absolute rule; does nothing without plate solves | low–moderate | same frames |
| Pairing tolerance up (`ransacTolerancePx` 2.5) | exact lists: all right. Task-13 lists: 162 / 132 / 141, right at 1.9, drop to 599 / 527 / 650 inliers at 0.78 / 0.87 / 1.22 px off; 31 and 74 still fail; 49 / 39 end at 296 / 277 inliers, 0.73 / 1.20 px off | 1 line | **high**: a coin flip that moves every frame's RANSAC | every frame |
| FWHM-aware tolerance (sharp → tighter) | tolerance 1.0: 11 frames fail, the four sharp ones among them | small | makes it worse | — |
| Detector list for mono frames (drop the fits reuse) | 10 sharp frames (F ≤ 2.7): overlap 0.90–0.91, 824–924 true matches, all right. Seeds still 5.7–8.9 px off, and blunt frame 57 (5 false matches) is 40.9 px off with 28 pairs | +1 read and detect per mono frame (undoes Tier C Task 3's reuse) | moderate: the seed defect stays | sharp frames' lists |
| Shorter lists (`maxStars` 1 000) | all right, but inliers halve (median 835) and max map error rises to 0.12 px from 0.07 | 1 line | precision loss everywhere | every frame |
| Re-seed from the group's consensus scale / rotation | not run: needs the group's maps first; the robust seed already gets every frame right without a group prior | moderate | moderate | — |

**Recommendation: the robust seed alone, as its own plan.** It removes the cause: one false match can no longer move
the seed. It changes nothing for a frame whose matches were clean, and it needs no plate solve, no config and no
two-pass ordering.
- **Optional, as a diagnostic:** a registration warning when a quad-path map keeps fewer inliers than half of its
  group's median. One log line; no number moves.
- **Keep** the plate-solve fallback as it is.
- **Reject** the tolerance and list-size dials: measured as coin flips or as precision losses.

## Reproduction

Scripts and outputs: `~/.athenaeum-acc/regfrag/`, the `regfrag` registry row, kept until 2026-11-16.
- **Probe.** `regfrag_probe.rs` holds the source. It goes into `crates/athenaeum-core/examples/` and builds with
  `cargo build --release -p athenaeum-core --example regfrag_probe`.
- **Q1.**
  - `q1/ref123.tsv`: the reference list.
  - `q1/lists_d2d/`: exact re-measured lists; `q1/lists_f1/`: the Task-13 `.athf` lists.
  - `q1/stages.py`: the stage table; `q1/variants.py` and `q1/variants2.py`: the options; `q1/drop.py`: the
    deletions; `q1/truthmaps.py`: the truth.
- **Q2.** `q2/q2.py`, plus `q2/hc11/`.
- **Q3.** `q3/master_shape.py` and `q3/faint_stack.py`.

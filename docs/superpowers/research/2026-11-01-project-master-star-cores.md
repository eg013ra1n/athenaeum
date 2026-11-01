# Why the LDN1272 project master loses its star cores (2026-11-01)

**Question** (Plan D, D1m, Fable verdict r1, Q1): the mono master of the LDN1272 test project (`ldn1272-test-project`,
lab dataset `runB`: the mono L group registered onto OSC frame 123's grid) keeps 0.69 (base build) / 0.65 (D1 build) of
the star peak a weighted stack of its own registered frames gives at the same stars; the frame set (`s204`, set 204)
keeps 0.918 / 0.906. Suspects named, none pinned: core rejection, the project path's resampling (`compose_to_grid`),
normalization.

**Answer.** The stage is **rejection**: Linear fit clip throws out the cores of the sharpest frames. It does so because
the project's stored config normalizes frames for rejection with the **global** pair (`normalization.rejection =
scaleZeroOffset`, the shipped default), whose scale is the ratio of each frame's background **dispersion** (bwmv), not of
its star **flux**. The set's stored config uses `local` (the LN pair, a flux scale). Nothing in the project path itself
loses star light: the same frames through the set path with the project's setting lose the same amount, and the project
with `local` keeps what the set keeps. The shipped default (LN off) is worse still on this data: 0.67.

Reviewed by Fable before the owner (2026-11-01, `~/.athenaeum-acc/pcores/review/fable-r1.md`): every cell recomputed,
two independent 60-star replays, the stage confirmed.

## Method

A read-only replay of the group's integration at star cores, from the run's own artifacts, on the D1 clones
(`~/.athenaeum-acc/pland-{runB,s204,hc11}-d1`, never written): the registered frames (`registered/`), the LN sidecars
(`.athln`, evaluated with `LnGrid::evaluate_row_into`'s smoothing B-spline and edge extrapolation), each frame's
normalized weight and Measure location / scale (`stacking_run_frames`), the rejection pair of `integration/stats.rs`
(`rejection_pair`, additive with scaling) and Linear fit clip ported line for line from `integration/combine.rs`
(`reject_linear_fit`, the `medfit_line` oracle, `2·adev`, 5.0 / 3.5, 20 iterations, the `x ≠ 0` and range rules of
`combine_pixel_weighted` and the engine). Per star, every pixel of the 5×5 core box is integrated under each variant;
the peak is the 3×3 maximum above the median of a 3.5–5 F annulus (F = 3.5 px; 6 px on hc11) of the same stamp.

Stars: local maxima of the master ≥ 40 σ, < 0.25 of its 99.999th percentile, isolated by 8 F, every included frame
finite on the whole stamp; 100 per SNR band 40–150 / 150–400 / ≥ 400 (300 per dataset, 180 on hc11 G; seed 7).

**The replay is the product, to the bit that matters:**

| check | runB (project) | s204 (set) | hc11 L / G (set) |
| ---- | ---- | ---- | ---- |
| replay under the run's own setting vs the master's own pixel, \|Δ\| / peak, median / p99 | 9.5e-8 / 4.3e-7 | 1.2e-7 / 7.0e-7 | 1.0e-7 / 7.7e-7; 6.2e-8 / 2.9e-7 |
| per-frame rejection at the peak pixel vs the run's `.rej` bitmaps (drizzle input) | — (no drizzle) | mono 27 600 / 27 600, OSC 31 500 / 31 500 per plane | — |

**Limits** (none changes the answer): the replay's reference location / scale are the group medians, not the reference
frame's (Linear fit clip is invariant under a common affine map; the pixel check proves it); the "default" columns have
no product master behind them yet (Task 3's arm A4 makes one); rejection is replayed in the core box only, so a replay
column reads 0.002–0.003 below the master's own (the annulus comes from the no-rejection stack); stars sit where every
frame is finite (the thin partial-coverage edge is unsampled); groups under 20 frames (the Auto ladder's other rungs)
are not measured.

Scripts (job folder, registered; the fix plan moves the replay into athenaeum-lab):
`~/.athenaeum-acc/pcores/scripts/{frames,replay,summ2,regpair,crops}.py`; outputs `~/.athenaeum-acc/pcores/out/`.

## The stars through each stage

Median over the stars of the core peak ÷ the same stars' peak in the no-rejection stack of the same normalized frames
(the inputs' weighted stack, 1.000 by definition). "szo" = the global pair, "local" = the LN pair; output normalization
is LN (`local.enabled`) in every row but the two "default" rows.

| stage / variant | runB project mono (83 fr) | s204 set mono (92 fr) | hc11 L (73 fr) | hc11 G (28 fr) |
| ---- | ---- | ---- | ---- | ---- |
| registered frame, grid vs set reference (same raw frame, 8 frames F 2.17–5.12 px): star concentration / 2nd-moment FWHM / noise | 0.9990 / 0.9991 / 1.0003 | (reference) | — | — |
| after normalization, no rejection | 1.000 | 1.000 | 1.000 | 1.000 |
| **the master** (run's own setting) | **0.712** (szo) | **0.912** (local) | 1.001 (szo) | 0.975 (szo) |
| replay, rejection szo (the project's setting) | 0.710 | **0.717** | 1.000 | 0.975 |
| replay, rejection local (the set's setting) | **0.920** | 0.910 | 1.000 | 1.000 |
| replay, global offset with the LN flux scale | 0.924 | 0.892 | 1.000 | 1.000 |
| replay, the shipped default (LN off: szo rejection AND szo output) | 0.667 | 0.674 | 1.000 | 0.971 |
| replay, the default with rejection local (LN computed for rejection only, output global) | 0.914 | 0.903 | 1.000 | 1.000 |
| frames rejected at the peak pixel, median: szo / local | 9 / 1 | 11 / 0 | 0 / 0 | 0 / 0 (≥ 150 σ: the sharpest frame on 70 of 120 stars / none) |
| median FWHM of the frames rejected at the peak (group range) | 2.31 px (2.17–5.12) | 2.34 px (2.17–5.22) | — | 4.95 px (4.95–7.72) |

By SNR band the picture is the same (runB szo 0.737 / 0.688 / 0.697, local 0.941 / 0.962 / 0.866; s204 szo 0.739 /
0.688 / 0.724, local 0.917 / 0.906 / 0.901). The brightest band's "local" cell is noisy on both datasets (a block of
~14 sharp frames sits right at the clip line, kept on some stars and cut on others), not a project effect: s204's
master reads 0.902 there. Fable's independent 60-star samples: runB szo 0.707 → local 0.950; hc11 G 0.980 → 1.000.

Fable's D1m reader (80 stars ≥ 80 σ in the central 2400 px, 3×3 core, per-frame annulus, LN scale folded into the
weight, no LN offsets) read runB 0.69 / 0.65 and s204 0.918 / 0.906; this replay's stratified 300-star median reads
runB 0.712 (D1) — another star sample and peak definition, the same gap to the set.

**The OSC groups** (same replay, one plane at a time; replay vs master |Δ| / peak ≤ 1e-7 median, ≤ 4e-7 p99 on every
plane). The "master" columns read the master's own annulus, which puts a master 0.002–0.003 above its own setting's
replay column (runB R 0.981 vs szo 0.978, s204 R 0.972 vs local 0.969); compare replay with replay:

| plane | runB master (szo) | runB local | s204 master (local) | s204 szo | default runB / s204 | default + local rejection runB / s204 |
| ---- | ---- | ---- | ---- | ---- | ---- | ---- |
| R | 0.981 | 0.962 | 0.972 | 0.980 | 0.981 / 0.982 | 0.971 / 0.976 |
| G | 1.000 | 0.981 | 0.986 | 1.000 | 1.000 / 1.000 | 0.988 / 0.990 |
| B | 0.989 | 0.975 | 0.979 | 0.985 | 0.987 / 0.986 | 0.981 / 0.982 |

On OSC the sign is the other way and small: `local` keeps 0.9–1.9 points less than the global pair (replay szo →
local: runB R −1.6, G −1.9, B −1.2; s204 R −1.1, G −1.6, B −0.9).

**Weight concentration makes it worse.** The same replay with the base build's weights (`dbase`, D1's LN sidecars):
szo 0.742 (runB) / 0.747 (s204), local 0.930 / 0.915; with D1's weights szo 0.710 / 0.717. D1 raises the no-rejection
stack's peak by 5.3 % (runB) / 4.9 % (s204), and under szo rejection most of that gain is cut again (net +1 %); under
local it survives (+4.5 %). This is Plan D's runB mono "recorded miss" (PSF SNR 0.962, PD-21): its cause is the
rejection normalization.

## Mechanism

Linear fit clip sorts a pixel's frame values, fits a median line through them by rank and rejects what lies more than
3.5 · 2·adev above it. At a star core the values are not one population: the peak of a star scales as 1 / FWHM², so
the sharp end of the sorted values stands above the bulk's line. Whether it crosses the clip line depends on how each
frame was scaled for rejection:

- **local** scales each frame by its LN star-flux ratio. On runB's median star the sharp block stays inside the band,
  0 of 83 cut (`2026-11-01-star-cores/runB-pixel.png`, right).
- **scaleZeroOffset** scales each frame by `s_ref / s_i`, `s` = the frame's bwmv dispersion — a background-noise
  measure. Against the flux ratio this is a **per-frame scale error** of p5–p95 0.78–1.13 (runB mono) / 0.80–1.15
  (s204 mono) / 0.85–1.09 (runB OSC G) / 0.72–1.10 (hc11 L), unrelated to the stars, which makes the sharp end of a
  core pixel's values ragged; the clip cuts the ragged top. It has no fixed sign:
  - LDN1272 mono: the sharp block (2.2–2.9 px, 2.8–3.0× the median frame's peak) is scaled 12 % (runB) / 14 % (s204)
    above its flux ratio and pushed out — 9–11 frames cut at the peak (`runB-pixel.png`, left: the crosses are the
    sharpest frames). The weight concentrates on exactly those frames (the heaviest frame weighs 3.9× the median, the
    top 10 hold 31–34 % of the weight), so losing them costs ~29 % of the peak, and more as the weight sharpens.
  - hc11 G: the single sharpest frame (4.95 px) is scaled 3 % above its flux ratio while its four nearest neighbours
    (5.11–5.66 px) are scaled 14–15 % below — it is left alone at the top and cut on 70 of 120 stars ≥ 150 σ (−2.5 %).
  - LDN1272 OSC: the sharpest 15 % are scaled 10–11 % below their flux ratio, pulled toward the bulk, and the clip cuts
    fewer cores than under `local` (0.9–1.9 points).
- With the global offset but the LN flux scale the mono loss is gone (0.924 / 0.892): it is the scale, not the offset.
- `local` removes the error; what it leaves is the clip meeting the real PSF spread (below).
- hc11 L has no sharp block (FWHM 5.6–9.2 px, a continuous spread, its sharpest frames 1.34× the median frame's peak;
  heaviest frame 1.7× the median weight): nothing is cut at any core under any setting.

Stretched crops of the median runB star (SNR 239, master 0.688):
`2026-11-01-star-cores/runB-crops.png` (sharpest, median, widest frame; the inputs' stack; the project master; the same
stack with local rejection) and `runB-pixel.png` (that star's peak pixel, both normalizations).

## Not the cause

- **The project path's resampling** (`compose_to_grid`, the 90° grid, bicubic B-spline): the same raw frame registered
  onto the grid and onto the set's reference has the same star concentration (grid / set 0.9990), second-moment FWHM
  (0.9991) and noise (1.0003), over 8 frames from 2.17 to 5.12 px.
- **The project's LN** (`lnmask:1`, overlap fitting): LN global scales, each run normalized to its median, agree
  runB / s204 at p5 0.9966, median 1.0009, p95 1.0043 (ρ 0.9987); no frame fell back to `ln_global`.
- **The frames**: the project's 83 are a subset of the set's 92 (the set adds 7 wide low-weight frames and 2 at
  3.3 px); measured FWHM and weights agree (ratio 1.000, ρ 0.99994).
- **Normalization as such**: the no-rejection stacks are the reference; the loss appears only after rejection.

## Why the project had `scaleZeroOffset`

- `RejectionNormalization::default()` is `ScaleZeroOffset`, and `StackingConfig::default()` has LN off; a project's
  stored document is the global default plus `exposureMode = stops` and `coverageMap = true`
  (`project_seed_config`). The project's document has LN on and rejection global: LN was turned on, rejection was not.
- The Normalize panel couples the two one way only: turning LN OFF while rejection is `local` resets rejection to
  `scaleZeroOffset` (`NormalizePanel.tsx`, `handleLocalEnabledChange`); turning LN ON leaves rejection global.
- `preset(MaximumQuality)` turns LN on and leaves rejection global: the "best" preset carries the same trap.
- Set 204's document has `local` because the M2–M4 acceptance runs used it (`2026-09-11-m4a-acceptance-run.md`: "LN
  both sides (`local.enabled`, `rejection = local`)"). Linear fit clip's calibration (R-M4a-4, 2.985 % mono rejected)
  was made under `local`; the shipped default rejection normalization was never read at star cores.

## What remains with `local`

Even with `local`, Linear fit clip still removes 8–9 % of the peak on the LDN1272 mono groups (the sharpest frames
again, now 0–1 at the peak pixel on most stars, a 14-frame block on some bright ones) and 1.6–3.8 % on the OSC planes
(local R 0.962 / 0.969, G 0.981 / 0.984, B 0.975 / 0.976, runB / s204). That is the rejection meeting a real PSF spread,
not a normalization error; the prior record already owes a star-protecting rejection variant (open-items, M4c Task 7,
RCR / Winsorized on OSC red). Recorded here, not part of the fix.

## What the fix should own

1. The rejection normalization must scale frames by their star flux wherever a flux scale is measured: `local` whenever
   LN runs (Maximum quality, the panel's LN toggle, the stored project config).
2. The shipped default (LN off) loses 33 % of the star peak on this mono data: the default needs the same, which costs
   the LN stage (35–37 s on an 83–92-frame mono group, 90–100 s on OSC, about the integration's own time), and gives up
   the global pair's accidental 0.9–1.9 points on OSC planes where the night happens to favour it. A cheaper later
   lever exists: the global pair scaled by Measure's common star flux (Plan D's `commonTflux`), no LN stage, a new mode
   that waits for Plan D.
3. Acceptance at master level on the full groups: runB AND s204 (bars on the core-peak ratio per plane, runB within
   reach of s204), hc11 L as the no-change control and hc11 G as a measured gain, background noise and rejected
   fraction beside, the set's satellite trail still rejected.

Plan: `docs/superpowers/plans/2026-11-01-rejection-normalization-star-cores.md`.

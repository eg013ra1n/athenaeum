# M4c acceptance run — algorithms on LDN 1272 (2026-09-12)

Plan: `docs/superpowers/plans/2026-09-10-stacking-m4c-plan-algorithms.md`, Task 7.
Branch `worktree-stacking-m4c` at `7ad08d25` (code identical to `1c028e9e`; the
acceptance binary was built from `1c028e9e`), served from
`.superpowers/target-acc/release/athenaeum-web` on port 8933 against the dev
catalog. Set 109 (LDN 1272): 208 mono + 160 OSC calibrated frames, the M4a
acceptance config as the baseline (linear-fit rejection at the Auto 5.0/3.5,
local normalization on, drizzle 2×, `keepAll`).

## 1. Runs

| Run | Variant | From | Wall | Notes |
| ---- | ---- | ---- | ---- | ---- |
| 22 | A — baseline | full (measure + register stale: the M4c config fields moved both hashes) | 50 min | measure 10.1, register 1.9, normalize 12.6, integrate 10.7, drizzle 14.8 min; two-pass pick 29351 → 29390 as in run 13 |
| 23 | B:esd (0.3 / 0.05 / 1.5) | integrate, drizzle off | 5 min | integrate 4.5 min |
| 24 | B:rcr (0.5) | integrate, drizzle off | 6 min | integrate 5.0 min |
| 25 | B:minMax (1/1) | integrate, drizzle off | 5 min | integrate 4.7 min |
| 26 | B:winsorized (4.0/3.0, the new reference loop) | integrate, drizzle off | 5 min | integrate 5.0 min |
| 27 | C:0 — TPS λ = 0 + local loop, FIRST attempt | register, drizzle 2× | killed after 2 h 55 min | mono register + normalize + integrate 11 min, then the mono drizzle stalled the 16 GB machine (0 % CPU, 11.3 of 12 GB swap) — the TPS grids never released; ruling R-T4-6, fix `b03ee36e`, re-run below |
| 28 | D — large-scale rejection (protectedLayers 2, growth 2) | integrate (+ register/normalize, stale after 27) | 66 min | register 1.9, normalize 5.4, integrate 37.4, drizzle 21.3 min |
| 29 | E — LN `localScale` | normalize, drizzle 2× | 49 min | register 0.6, normalize 17.9, integrate 14.6, drizzle 15.4 min (normalize confounded by concurrent builds — see run 33) |
| 30 | C:0 — TPS λ = 0 + local loop, on the `8f7f1242` binary | register, drizzle 2× | 49 min | register 1.9, normalize 14.9, integrate 12.1, drizzle 20.5 min — no stall |
| 31 | C:0.5 | register, drizzle 2× | 49 min | register 1.9, normalize 15.1, integrate 12.2, drizzle 20.0 min |
| 32 | C:2 | register, drizzle 2× | 49 min | register 1.9, normalize 14.6, integrate 11.9, drizzle 20.1 min |
| 33 | E again, drizzle off — the clean LN-timing re-measure, nothing else on the machine | normalize | 25 min | register 1.8, normalize 12.1, integrate 10.6 min; masters byte-identical to run 29's |

The B variants ran with drizzle off: the master is the measurement and drizzle
would have added 15 min per run; nothing in the master depends on it.

## 2. Rejection algorithms (B)

Baseline (A): rejected 2.985 % (mono) / 2.733 % (OSC), master noise
1.830e-5 (mono), 1.671 / 1.726 / 1.639e-5 (OSC R/G/B), FWHM 2.734 /
2.811, 2.822, 2.717 px.

| Method | Rejected mono / OSC | Noise vs A mono | Noise vs A OSC R/G/B | FWHM mono | FWHM OSC R/G/B |
| ---- | ---- | ---- | ---- | ---- | ---- |
| ESD | 0.150 % / 0.138 % | −7.3 % | −9.7 / −10.2 / −13.5 % | 2.737 | 2.818 / 2.792 / 2.661 |
| RCR | 1.814 % / 2.250 % | −3.4 % | −2.4 / −1.7 / −3.9 % | 2.595 | 3.953 / 3.136 / 2.999 |
| min/max 1/1 | 0.985 % / 1.289 % | −7.5 % | −11.4 / −11.7 / −14.5 % | 2.743 | 2.829 / 2.660 / 2.659 |
| Winsorized 4/3 | 0.682 % / 0.712 % | −6.0 % | −6.8 / −7.2 / −10.1 % | 2.735 | 3.823 / 3.040 / 2.865 |

Targets and verdicts:

- **ESD 1–5 %: MISS (0.15 %), by design.** The generalized ESD test at
  n = 208 / α = 0.05 keeps everything but genuine single extremes. The one
  real satellite trail of the set (frame `_0085`, see §4) is still rejected
  in the ESD master (on-line median 0.9997 of the beside median), so the low
  fraction is not a leak; the linear fit's 3 % is mostly noise-tail samples.
  Master noise is 7–13 % LOWER than the baseline's for the same reason —
  outside the ± 3 % band in the better direction. The brief's target range
  was a guess at the reference's behaviour; the shipped defaults stand.
- **RCR 1–5 %: PASS on the fraction (1.8 / 2.3 %), FAIL on star profiles
  for the OSC red plane.** `star-compare.py` over 200 isolated stars ≥ 20 σ:
  peak median 0.971 of the baseline's (p10 0.673), second-moment width
  +3.4 % (p90 +5.9 %); the 30 brightest stars unchanged (peak 1.001, width
  1.003); the green plane fine (peak 1.000, width +0.2 %); mono fine (peak
  1.001, width −0.5 %). Chauvenet at n = 160 on the skewed per-pixel
  distribution of a VNG-interpolated red star core (the sub-pixel phase
  varies between frames) rejects the sharpest frames' cores of medium and
  faint stars. RCR is opt-in and outside the Auto ladder; it ships as is
  with a caution for colour data (open-items).
- **min/max = 2/n: PASS.** 0.985 % / 1.289 % against 2/208 = 0.962 % and
  2/160 = 1.25 %; the excess is the frame-edge stacks with fewer than n
  samples (2/n is exact per stack). Star profiles intact (peak 0.997, width
  1.001 on the red plane).
- **Winsorized 0.5–3 %: PASS (0.68 / 0.71 %)**, and the same red-plane
  effect as RCR in a milder form: peak median 0.992 (p10 0.731), width
  +1.0 %; the brightest 30 unchanged. `sigmaHigh = 3.0` is the mechanism
  (the sharp-seeing high side of a skewed core distribution), not the new
  σ estimator — Task 2 measured the two estimators within 8 % of each other.
  The Auto ladder selects Winsorized only for 8 ≤ n < 20, where this set
  never lands.
- **Master noise within ± 3 % of A for ESD/RCR/Winsorized: MISS for all
  three, all lower.** Every method rejects fewer samples than the 5.0/3.5
  linear fit, so every master averages more frames. The target assumed
  equal rejection; the honest reading is that the linear fit's 3 % costs
  ≈ 6–13 % of master noise on this set — a candidate for a later re-tune,
  recorded in open-items, not an M4c defect.

## 3. Winsorized before/after on a real master dark

Set 1763 (master dark, ZWO ASI294MM Pro bin 2, 180 s × 100 raw darks of set
1278, Auto → Winsorized 3/3): the file built on 2026-09-11 with the retired
estimator (copy kept) versus `rebuild_master` through the M4c server (25 s).

| Metric | Before | After | Target |
| ---- | ---- | ---- | ---- |
| median | 1922.720 | 1922.680 (−0.002 %) | ± 0.1 % |
| MAD | 1.2400 | 1.2401 (+0.010 %) | ± 2 % |
| hot pixels (> median + 10·σ_MAD) | 176 866 | 175 353 (−0.855 %) | ± 1 % |

PASS on all three; 35.3 % of pixels differ (max |diff| 1094 ADU on a hot
pixel), mean difference −0.043 ADU. The dev catalog's master 1763 is now the
M4c build.

## 4. Large-scale rejection (D)

The real trail (ruling R-M4c-10): run 22's mono `.rej` bitmaps carry the same
line run 13's did — frame `c_2025-10-19_00-18-26__-9.90_180.00s_0085`, 37.0°,
ρ = 878.3 px in reference coordinates (7 903 Hough votes, 193× the median
bin). In the baseline master the linear-fit clip already removes it
entirely: on-line median 0.9995 of the beside median, annuli 0–3 / 3–6 /
6–10 / 10–15 px at 0.9995 / 0.9993 / 0.9998 / 0.9998.

Run 28 (large-scale on, `protectedLayers = 2`, `growth = 2`, the rest as A):

| Metric | A | D | Target |
| ---- | ---- | ---- | ---- |
| rejected, mono / OSC | 2.985 % / 2.733 % | 3.053 % / 2.758 % | — |
| `large_scale_rejected_fraction`, mono / OSC | — | 0.137 % / 0.049 % | ≤ 0.5 % |
| on-line median / beside (mono, 0–3 px) | 0.99951 | 0.99942 | within 1 % |
| annuli 3–6 / 6–10 / 10–15 px | 0.99925 / 0.99983 / 0.99983 | 0.99920 / 0.99983 / 0.99981 | — |
| D − A away from the line | — | median 0, MAD 0, 5.0 % of pixels changed at all | within noise |
| master noise (MAD-σ, far from the line) | 1.8155e-4 | 1.8153e-4 | unchanged |
| integrate / drizzle wall | 10.7 / 14.8 min | 37.4 / 21.3 min | — |

PASS on every target. The honest reading: on this set the linear-fit clip
already removes the trail, so large-scale rejection changes the master only
where its forced set differs from pass 1's (5 % of pixels, all within noise)
at 3.5× the integration time; its value is the trail whose shoulders a
per-pixel clip misses — pinned synthetically in Task 3 (shoulder +11.7 % →
+0.4 %), not reachable on this set's one trail.

Sparse-mask survival (Task 3 review m5): frame 0085's pass-1 `.rej` holds
13 518 bits within 6 px of the trail out of 72 220 (≈ 30 % fill per bin);
the processed `.rejl` keeps 23 151 of its 23 360 bits ON the trail — the
cascade kept only the trail and densified it (≈ 600–750 bits per bin
against 350–650); the pass-2 bitmap has 23 643 on-trail bits of 83 033.
The real, sparse trail mask survives the majority cascade. Closed.

## 5. TPS distortion and the local loop (C)

Reference numbers: run 12 (polynomial3) OSC rms mean 0.146 / median 0.135 /
p95 0.181 / max 0.195 px, mono 0.098; run 22 (distortion off) OSC 0.215 /
0.205 / 0.255 / 0.356, mono 0.131. A TPS row's `rms_px` is a HOLD-OUT number
(ruling R-T4-4), not the in-sample residual the polynomial rows report — the
two are not comparable in one column; the drizzled FWHM ratio is the
geometry-independent metric.

The first TPS attempt (run 27) stalled the machine — see §1 and ruling
R-T4-6; the numbers below are from the re-runs on the `8f7f1242` binary,
which releases every displacement grid when a stage is done with a frame.
None of the three re-runs stalled; a TPS run costs ≈ 49 min against the
baseline's 40 (register unchanged at 1.9 min — the fit and its hold-out QA
are cheap; normalize +18 %, integrate +13 %, drizzle +38 % — the grid
rebuilds, three per OSC frame in drizzle).

| λ (`tpsSmoothing`) | hold-out rms mono (mean / median / p95 / max) | hold-out rms OSC | FWHM mono / OSC R,G,B | drizzled FWHM mono / OSC R,G,B | frames reaching round 2 / the 3-round cap |
| ---- | ---- | ---- | ---- | ---- | ---- |
| A (off) | in-sample 0.131 | in-sample 0.215 | 2.734 / 2.811, 2.822, 2.717 | 4.459 / 4.748, 4.718, 4.453 | — |
| run 12 (polynomial3) | in-sample 0.098 | in-sample 0.146 | — | — | — |
| 0 | 0.1445 / 0.1447 / 0.198 / 0.329 | 0.2032 / 0.189 / 0.245 / 0.268 | 2.736 / 2.809, 2.821, 2.720 | 4.467 / 4.758, 4.721, 4.462 | 0.9 % / 0.3 % |
| 0.5 | 0.0991 / 0.0963 / 0.132 / 0.182 | 0.1557 / 0.147 / 0.187 / 0.202 | 2.734 / 2.804, 2.812, 2.709 | 4.462 / 4.729, 4.694, 4.434 | 7.1 % / 5.6 % |
| 2 | 0.1016 / 0.0966 / 0.140 / 0.185 | 0.1654 / 0.161 / 0.189 / 0.202 | 2.734 / 2.805, 2.813, 2.710 | 4.462 / 4.732, 4.698, 4.435 | 14.8 % / 11.7 % |

Every frame registered as `homography+tps` (the one `similarity` frame is
the same one in every run); rejected fractions, master noise and the mono
FWHM are identical to the baseline's to three digits in all three runs.

- **RMS target ("≤ the polynomial3 run's"): not comparable as written.** A
  TPS row's `rms_px` is a hold-out number (R-T4-4) and the polynomial rows
  report the in-sample residual; the hold-out 0.099 / 0.156 px at λ = 0.5
  sits between run 12's in-sample 0.098 / 0.146 and the baseline's 0.131 /
  0.215, which is what an honest generalization error should do. λ = 0
  interpolates the star-position noise (hold-out 0.145 / 0.203).
- **Drizzled OSC G/B ratio (the M3 +10 % residual):** A 1.0595, λ = 0.5
  1.0586 — unchanged. Registration distortion is NOT its cause; the
  residual moves to M4d's Bayer-drizzle work (the VNG planes are the
  suspect the M4a note already named).
- **Smoothing default — ruling R-T7-1: `tpsSmoothing = 0.5`.** The brief's
  rule ("lowest RMS that keeps the loop within 3 rounds on ≥ 95 % of
  frames") is satisfied by every λ through the cap itself; read as
  "converges before the cap", λ = 0.5 sits at 94.4 % and λ = 0 at 99.7 %,
  but λ = 0's rms is worse by 45 % and its spline is the noise-fitting one.
  The default moves to 0.5 in the final fix wave (the registration hash
  moves for `tps` sets only).
- On this set TPS buys nothing measurable over the polynomial (the field
  is well corrected by a cubic — M3/M4a already showed rms 0.1 px); its case
  is the field a polynomial cannot fit, which this set does not have. It
  stays opt-in.

## 6. LN local scale (E)

Baseline OSC master corner/centre medians (512 px squares): R 0.684 / 0.696 /
0.684 / 0.716 (TL/TR/BL/BR), G 0.937 / 0.939 / 0.936 / 0.943, B 0.947 /
0.952 / 0.947 / 0.958.

Run 29 (`normalization.local.localScale = true`, from Normalize, the rest
as A):

| Metric | A | E | Target |
| ---- | ---- | ---- | ---- |
| corner/centre change, R (TL/TR/BL/BR) | — | −0.056 / −0.062 / −0.070 / −0.046 % | < 1 % |
| corner/centre change, G | — | +0.030 / +0.028 / +0.027 / +0.013 % | < 1 % |
| corner/centre change, B | — | +0.034 / +0.037 / +0.040 / +0.008 % | < 1 % |
| master noise, mono | 1.830e-5 | 1.794e-5 (−1.9 %) | — |
| master noise, OSC R / G / B | 1.671 / 1.726 / 1.639e-5 | 1.614 / 1.779 / 1.727e-5 (−3.4 / +3.1 / +5.4 %) | — |
| rejected, mono / OSC | 2.985 / 2.733 % | 2.931 / 2.789 % | — |
| normalize stage wall | 12.6 min (run 22) | 17.9 min (+42 %) | < +10 % |

From the LN log of the run: a local spline was fitted for every one of the
688 channel-frames (600 nodes each — the node cap always bites on this set,
so the reported rms is the non-node branch), σ_z median 0.099 (p90 0.199),
relative scale median 1.012; no safety-band refusal; the barycentre second
pass won on 43 of 688 channel-frames (6.2 %; 58 of 896 = 6.5 % in run 28
with `localScale` off — the pass is independent of the flag), confirming
ruling R-T5-2's denominator fires on genuinely walked fits only.

Verdicts: the corner/centre target PASSES — a flat-field-clean set does not
need the local scale and the surface changes it by < 0.1 %. The noise
column is the cost ruling R-T5-1 asked this run to weigh: at σ_z ≈ 0.1 the
per-frame surface carries the ≈ 1–2·σ_z ripple the Task 5 review measured
(pinned at 3·σ_z), and the G/B masters come out 3–5 % noisier for it (R 3 %
quieter — the plane whose LN scale is the most uneven). The feature stays
OFF by default; the math reference's surface-simplification step (or a
node-count-aware λ) is the follow-up, recorded in open-items. The LN stage
time: run 29's +42 % was confounded — its normalize stage overlapped the
Task 4 fix-round-3 test build and the acceptance rebuild on the same
machine. Run 33 repeated the stage with nothing else running: 12.06 min
against run 22's 12.6 min with the flag off (−4 %, within run-to-run
noise; the LN reference builds took 12.8 s mono / 39.2 s OSC) and produced
masters byte-identical to run 29's. The "< 10 % growth" target PASSES; the
spline fit and the barycentre tree are negligible next to the warps.

## 7. Click-through

Not performed in this run: the browser automation found two Chrome
instances connected to the account (macOS and Windows) and requires the
owner to choose one. A static check of the served bundle (built by
`m4c-build.sh`, `tsc` clean) confirms the controls exist: `IntegratePanel`
offers `esd` / `rcr` / `minMax` with their parameters and the large-scale
block (`enabled`, `protectedLayers`, `growth`); `RegisterPanel` offers `tps`
with the *TPS smoothing* field and the *Local distortion loop* checkbox;
`NormalizePanel`'s *Local scale* checkbox is live; `MeasurePanel` offers the
seed detector (`peak` / `structure`). The click-through of the four panels
on the desktop build stays owed to the owner, with the M4a/M4b ones.

## 8. Verdict

**M4c is accepted**, with the rulings below carried into the branch's final
fix wave and the follow-ups into open-items.

- Rejection algorithms: ESD, RCR, min/max and the Winsorized reference loop
  run end to end on real data with the expected shapes; the Winsorized
  before/after on a real master dark is inside every target. Two cautions
  ship with them: RCR (strongly) and Winsorized 4/3 (mildly) clip the cores
  of medium and faint stars on the OSC red plane; ESD's 0.15 % is by design.
  The Auto ladder is untouched, so no default output changes except the
  Winsorized masters (n ≥ 15, within 0.1 % / 2 % / 1 %).
- Large-scale rejection: every target met; the sparse real trail survives
  and is densified by the cascade; 3.5× the integration time and no visible
  gain on a set whose one trail the per-pixel clip already removes.
- TPS + local loop: no stall after ruling R-T4-6 (the first attempt is the
  reason the ruling exists); λ = 0.5 becomes the default (R-T7-1); no
  measurable FWHM gain on this well-corrected field; ≈ +25 % run time.
- LN local scale: corners < 0.1 %, LN time within noise; the per-frame
  ripple costs 3–5 % master noise on G/B at σ_z ≈ 0.1 — stays off by
  default, the simplification step is the follow-up.
- Barycentre pass: fires on 6 % of channel-frames with the target-side
  denominator (R-T5-2).

Owed to the owner: the desktop click-through of the four panels; the
Windows/Linux runs. Release-note line owed: every Winsorized master differs
from its pre-M4c self.

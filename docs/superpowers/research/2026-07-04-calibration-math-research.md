# Calibration Math — Deep-Research Report — 2026-07-04

Verified research for the Phase 2 calibration engine
(`../specs/2026-07-04-phase2-calibration-library-design.md` §9 is the distilled,
decision-bearing version; this document preserves the full findings).

**Method:** multi-agent deep-research run (103 agents): 5 parallel search angles →
source fetch + falsifiable-claim extraction → 3-vote adversarial verification per
claim (2/3 refutes kill a claim) → synthesis. Every finding below survived
verification with the recorded vote; 5 circulating community claims were refuted
and are listed at the end so nobody re-imports them.

## Summary

The calibration mathematics are consistent across all authoritative sources: a
calibrated light is `L_c = (L − D) / (F − O) · N` — subtract the (optionally
scaled) master dark and bias pedestal from the light, then divide by a
bias-subtracted, normalized master flat, where the bias subtraction of the flat
exists specifically to zero its level so vignetting curvature cancels in the
division, and N is the flat normalization constant (Siril: the mean of the
bias-calibrated master flat). Master recipes are well-specified: bias/dark masters
integrate with NO normalization and NO weighting plus Winsorized sigma clipping at
~3σ (PixInsight) or plain median (DeepSkyStacker default); flat masters require
multiplicative per-frame normalization with flux-equalization rejection
normalization, using percentile clipping (high limit <0.02) for sky flats/small
sets and Winsorized sigma clipping for large dome-flat sets. Dark optimization
(PixInsight's `I = I0 + k·D + B` model, k found by golden-section minimization of
a wavelet-based k-sigma noise estimate) is fully documented but requires a
bias-separated thermal dark, is sensitive to read noise (produces "dark holes"),
systematically undercorrects hot pixels, and its default-on recommendation dates
to 2012 pre-amp-glow CMOS — the correct modern-CMOS decision rule is matched raw
darks with optimization off and no separate bias. For flat pre-calibration the
verified fallback order is dark-flat when needed, else bias — preferably a
synthetic constant bias on modern sensors, since subtracting a real master frame
always injects noise; for interop, write MaxIm's CALSTAT keyword with B/D/F flags
("BDF" preferred, since VPhot requires all three letters).

## Verified findings

### 1. Light-frame calibration formula and order of operations — HIGH (9-0)

`L_c = (L − D)/(F − O)`. Subtract the bias/offset and master dark from the light
first, then divide by a flat from which the bias/offset has been subtracted. The
purpose of bias-subtracting the flat is to set its zero level so the flat's
vignetting curvature matches the dark-subtracted light during division — without
it, division under-corrects vignetting because the denominator contains an
additive constant.

- Sources: Siril calibration docs; Siril synthetic-biases tutorial; DSS theory +
  technical docs; corroborated by astropy CCD guide and PixInsight master-frames
  tutorial ("flats must be strictly composed of illumination data").
- Evidence: Siril: "When you calibrate your lights, you perform the following
  operation: L_c = (L − D)/(F − O)". Siril: "The main objective of subtracting the
  masterbias from the flats is to set their zero value so that when dividing the
  lights corrected by the masterdark, their curvatures are matched".

### 2. Flat normalization constant N — HIGH (3-0)

The division uses a normalized flat: divide by `(F − O)/N` where N is a scalar.
Siril auto-evaluates N as the **MEAN** (not median) of the master flat calibrated
with the master bias, computed over the **central third** of the frame
(width/3 × height/3 region starting at width/3, height/3) to avoid vignetting
bias. An implementation copying `L_c = (L − D)/(F − O)` literally without N would
produce ~1/K-scaled output instead of ADU-scale output.

- Sources: Siril docs; Siril source (`src/core/preprocess.c`,
  `prepro_prepare_hook` — verifier cross-checked: `stat->mean` over a central
  selection; confirmed mean, not median).

### 3. Dark optimization model and algorithm — HIGH (9-0)

PixInsight (Juan Conejero, primary source): the uncalibrated light is modeled as
`I = I0 + k·D + B`, where B is the bias pedestal and D the bias-separated
(thermal-only) master dark; optimization estimates the single multiplicative
scalar k. k is found by golden-section search (1/1000 fractional accuracy)
minimizing a noise-evaluation cost on the candidate `I0 = I − k·D − B`, where
noise is estimated by iterative k-sigma clipping (k=3, iterated to 1% convergence)
on the finest layer w1 of a single-level B3-spline wavelet transform. k scales
ONLY the thermal component — bias B is subtracted unscaled. Siril offers the
equivalent pair: noise-minimizing auto coefficient, or exposure-ratio coefficient;
both require the dark to be bias-subtracted first.

- Sources: pixinsight.com forum thread 8529 ("Dark frame optimization algorithm",
  Conejero, verbatim quotes); Siril calibration docs; Siril `calibrate -opt`.

### 4. Dark optimization decision rule for modern CMOS — HIGH (6-0)

PixInsight's default-on guidance dates to 2012 (pre-amp-glow CMOS). The same
primary source documents two failure modes: (1) significant read noise in the
master bias/dark makes the algorithm overcorrect thermal signal, in extreme cases
producing "dark holes" at hot pixels; (2) the single global factor systematically
undercorrects hot pixels (dark current is not linear over the intensity range).
Practical rule for a modern engine: **default dark scaling OFF; use
temperature/gain/offset/exposure-matched raw darks**; scaling is actively harmful
on amp-glow sensors because glow does not scale linearly with a single k.

- Sources: PixInsight forum thread 8529; corroborated by modern anti-dark-scaling
  guidance (Siril blog naming amp glow as requiring matched darks).

### 5. Master bias / master dark integration recipe — HIGH (6-0)

PixInsight: **average** combination with **NO normalization** (bias pedestal must
be preserved — output and rejection normalization both "No normalization") and
image weighting **DISABLED**; reject outliers with **Winsorized Sigma Clipping at
a permissive ~3σ** when many frames are available. DeepSkyStacker's default for
all three calibration masters (dark, flat, offset/bias) is per-pixel **MEDIAN**
(average is the default only for lights) — confirmed in docs and DSS 6.x source
(`Workspace.cpp`: `Dark/Flat/Offset_Method = MBP_MEDIAN`).

- Sources: PixInsight master-frames tutorial (Vicent Peris); DSS technical docs;
  DSS source.

### 6. Raw vs calibrated master darks — two valid conventions, do not mix — HIGH (6-0)

PixInsight integrates dark frames **RAW** (bias still included) and defers bias
subtraction of the master dark to calibration time inside ImageCalibration
(because dark optimization must rescale only the thermal component); WBPP still
defaults to this ("Calibrate Master Darks" handled internally). DeepSkyStacker
instead produces **CALIBRATED** master darks and dark-flats (master offset
subtracted from each frame before combining — confirmed in DSS source:
`Subtract(pBitmap, pMasterOffset)` before `AddToMaster`). Implementation rule:
with a **raw** master dark, `(L − D)` removes bias and dark in one subtraction and
no bias master is needed in the light equation (the recommended modern-CMOS
path); with a **calibrated** dark, the bias must also be subtracted from the
light separately, and only the calibrated-dark form is compatible with dark
scaling.

- Sources: PixInsight master-frames tutorial; PixInsight forum thread 14663 (WBPP
  "Calibrate Master Darks" checkbox); DSS technical docs + source
  (`StackingTasks.cpp` `DoDarkTask`/`DoDarkFlatTask`).

### 7. Master flat integration recipe — HIGH (3-0)

PixInsight: **multiplicative output normalization** (each calibrated flat
multiplied to match the average pixel value of all flats), **flux-equalization**
rejection normalization, weighting **DISABLED**. Rejection depends on flat type:
**percentile clipping with very restrictive limits (high limit below 0.02)** for
sky flats (stars present) or small frame counts; **Winsorized sigma clipping with
permissive limits** for large sets of dome/box flats. Matches current WBPP flat
defaults (Average / Multiplicative / Don't care / Equalize fluxes); WBPP "auto"
may pick Generalized ESD instead of Winsorized for very large sets.

- Sources: PixInsight master-frames tutorial; corroborated by Landmann guides and
  current WBPP defaults.

### 8. Flat pre-calibration fallback order for modern CMOS — HIGH (9-0, one verifier rated the <5 s point medium)

(1) Thermal signal in typical flats (exposures generally <5 s) is negligible —
dedicated dark-flats are unnecessary in the common case; a master bias suffices.
(2) On modern sensors with a stable uniform offset, prefer a **SYNTHETIC constant
bias** (Siril syntax `=2048` or `=64*$OFFSET`, ADU) over a stacked real master
bias — subtracting a real master frame never removes noise (it removes signal
while adding noise); a constant injects none. (3) Exceptions requiring a matched
dark-flat: long flat exposures (narrowband, >~10 s) and amp-glow sensors. (4) The
CCD-era PixInsight alternative is master bias + dark-optimized scaled master dark
(k ≈ t_flat/t_dark), valid across order-of-magnitude exposure differences.
**Safest engine default: use a dark-flat if the user supplies one (always
correct, near-zero cost), else synthetic/real bias.**

- Sources: Siril blog "Enough with dark flats" (lead dev Cyril Richard); Siril
  synthetic-biases tutorial + 1.5.x command docs; PixInsight master-frames
  tutorial; corroborated by Adam Block.
- Note: a related claim narrowing the exceptions to *exactly* two cases was
  refuted 0-3 as too strong.

### 9. DSS full pipeline order incl. cosmetic correction — HIGH (3-0)

Subtract master offset → subtract master dark → divide by calibrated master flat
(with flat normalization) → THEN hot-pixel cosmetic replacement **on the
calibrated image**. Hot pixels identified from the dark frames as values >
median + 16σ per channel, replaced by neighbor interpolation. Verified against
docs and DSS 6.x source (`ApplyMasterOffset → ApplyMasterDark → ApplyMasterFlat →
ApplyHotPixelInterpolation`). Design rule: cosmetic correction belongs after
calibration, never before.

### 10. CALSTAT interop convention — HIGH (3-0)

MaxIm DL's `CALSTAT` FITS keyword encodes calibration state with single-letter
flags: `B` = bias corrected, `D` = dark corrected, `F` = flat corrected. An engine
writing calibrated lights should set `CALSTAT='BDF'` (preferred over `'DF'` even
when the bias is implicit in a raw master dark, because consumers like AAVSO VPhot
only treat an image as fully calibrated when all three letters are present). ASTAP
writes it; the nom.tam FITS library codifies it.

- Sources: Diffraction Limited official FITS header definitions; AAVSO/VPhot forum
  threads; NASA HEASARC nom.tam javadoc.

## Caveats & coverage gaps (verbatim from the run)

Several sub-questions produced NO surviving verified claims and remain unanswered:

- output **pedestal** conventions (PEDESTAL keyword semantics, typical DN values,
  when to add one);
- **negative-value clipping vs offset** policy;
- **saturation/overflow** policy for f32 output;
- the expected **numeric scale for pre-calibrated f32 input** to WBPP/Siril
  ([0,1] vs ADU);
- **per-CFA-channel vs global flat normalization** for OSC;
- **BAYERPAT/CFA keyword preservation** through calibration;
- exact **WBPP configuration for consuming already-calibrated lights**;
- specific **gain/offset/temperature matching tolerances**.

Do not treat silence on these as endorsement of any approach. (The Phase 2 spec
§9 turns each of these into an explicit v1 policy + an empirical Plan B
verification task.)

Source-quality notes: pixinsight.com blocked direct fetches (HTTP 403) — tutorial
quotes verified via search-indexed text, browser-UA fetches, and independent
secondary reproductions; verbatim agreement across retrievals, but slightly weaker
provenance than a clean direct fetch. The PixInsight master-frames tutorial is
CCD-era (~2010): its "skip dark-flats, use optimized scaled darks" advice and
default-on dark optimization predate amp-glow CMOS — historical for modern
sensors. The Siril flat-normalization mean over the central third is a
source-code detail, not documented prose.

**Refuted claims (0-3) — do not rely on these even though they circulate:**

1. DSS flat multiplicative-normalization details (as commonly described).
2. DSS entropy-based dark scaling with a [0,1]-bounded coefficient.
3. The "only three valid calibration combinations" rule.
4. The "exactly two dark-flat exceptions" framing.
5. A "Siril uses median for masters" claim.

## Open questions (for empirical resolution in Plan B)

1. Authoritative PEDESTAL semantics and recommended default values (e.g. WBPP's
   auto pedestal) for preventing negative/clipped pixels in narrowband lights, and
   how an f32 engine should encode it so WBPP/Siril subtract it back correctly.
2. The numeric range WBPP assumes for externally pre-calibrated f32 FITS ([0,1] vs
   ADU) and the exact WBPP configuration (and pitfalls) for a
   debayer-and-register-only run.
3. Whether PixInsight/Siril normalize OSC master flats globally or per-CFA-plane,
   and whether per-plane normalization measurably changes color balance.
4. DSS's actual dark-scaling mechanism (the entropy-based description was
   refuted), and whether any mainstream tool implements per-region dark scaling
   for amp-glow sensors.

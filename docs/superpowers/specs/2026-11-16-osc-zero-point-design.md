# OSC zero point — a green-site aperture correction from Analysis (design)

Follows collab v3 sub-wave 6.5 (spec `2026-11-15-collab-v3-wave6-5-zero-point-design.md`, §9 and owner ruling Z5,
«Сначала моно, потом чини osc»). Ledger: `.superpowers/sdd/2026-11-16-osc-zp/progress.md`. Reviewed before the build
(opus, `review-spec-plan-opus.md` beside the ledger; every finding folded in).

## 1. Intent

A colour (OSC) frame gets a zero point and with it a transparency loss, as a mono frame already does.

Today an OSC frame is refused (`NotReady::ColourCamera`, `LossReason::ColourCamera`) because the probe of 6.5 showed its
zero point following the seeing: +0.17…+0.21 mag per px of FWHM on 2025-10-18, −0.14 on 2025-09-13, where mono reads
+0.000 (L) and +0.013 (H) with airmass fitted. The cause: Analysis measures a raw Bayer mosaic on its green sites
(PC-4), and there the aperture correction `c` is never measured (`c` = 1), so a star's stored flux is the fitted
model's flux in its FWTM ellipse, without the light of the wings. The share of light in the wings changes with the
seeing and with the PSF's shape, so the zero point follows them.

On a mono plane `c` (the growth flux at 5 F over the model flux, the median of up to 64 bright isolated stars,
`photometry::aperture_correction`) is measured and multiplied into every fit's flux. On a mosaic `growth_curve`
refuses every star, because half the pixels of its 5 F disc are red or blue sites and the green mask hides them.

## 2. Rulings

- **O1 (owner, 2026-11-16: «Да, давай поправку отдельно перемериются только osc»).** The correction is stored apart:
  `star_metrics.flux` keeps its meaning for every frame, `MEASURE_VERSION` stays 8, no number Analysis stores today
  moves, mono and stacking are untouched, and only OSC frames are re-analysed. Option A (`flux` corrected on OSC, a
  `MEASURE_VERSION` bump, the whole catalogue re-analysed and the stacking caches recomputed) is declined.
- **O2 — the green-site growth (controller).** On a mosaic a star's growth flux at 5 F is the sum of `(v − B_i)` over
  the green sites whose centres lie within 5 F, times `N_all / N_green` (the pixel centres of the disc over its green
  ones). `B_i` is already the green sky (`ring_sky` reads green sites only, `photometry.rs:854-859`); a G1 / G2 gain or
  pedestal difference cancels to first order in growth / model, because the fit's one amplitude averages the two
  sites as the growth does. The green sites form a quincunx lattice; for one star the sum over it, times two, departs
  from the full sum by `4·exp(−π²σ²)·cos πx₀·cos πy₀` (σ the PSF's Gaussian width in native px, `(x₀, y₀)` the centre's
  sub-pixel phase): 0.3 % at FWHM 2 px, 7 % at 1.5, about 20 % at 1.3 point-sampled (less with pixel integration). The
  term has zero mean over phases, so the median over up to 64 stars at random phases is unbiased; per-star noise grows
  at small FWHM. Aperture photometry on the green channel of a raw colour frame is the established practice of
  colour-camera photometry (the AAVSO DSLR observing manual). Cost if wrong: Phase A (§6) misses a bar and the build
  stops before anything is stored.
- **O3 — measured, never applied (controller).** The green-site `c` is reported on its own; no fit's `signal`
  changes, so every field the Analysis tab, Blink, the plate-solve gate and the overlay read is bit-identical before
  and after (`signal`, HFR's half-flux target, SNR read before `c`, `FrameSummary`). `aperture_correction`,
  `aperture_stars`, `aperture_se`, `aperture_ratios` and the population's `aperture_se` stay as today on a mosaic
  (`None`, 0, empty), so the lab probes' output does not move either. Only the mosaic path measures it, through a
  crate-private entry: no public option type changes and nothing in `stacking/` is edited.
- **O4 — one mosaic test, the reader's (controller).** A frame is a mosaic when its `BAYERPAT` is one of the four
  patterns the reader builds a CFA plane for and its analysis has one channel (`is_mosaic(bayerpat,
  source_channels)`, one shared function, used by the outdated rule, the zero point and the loss). Every other colour
  frame (a debayered file, a 3-plane file that kept `BAYERPAT`, an unrecognised pattern) keeps `ColourCamera`:
  rare, never probed, its flux already corrected by `measure_plane` or its layout unknown. Recorded, not owed.
- **O5 — re-analysis by the Analyze button (controller).** A mosaic analysis whose `cfa_aperture_version` is not the
  current one is outdated. `analyze_frame_set(force = false)` re-runs exactly those frames (with the frames without
  a row), and the Analysis tab counts an outdated row as not analysed, so a fully analysed mixed set offers Analyze
  again and it re-runs only its mosaic frames. On-demand analysis for Blink and the overlay
  (`get_frame_star_metrics`) does not use this rule: an old mosaic row's stars are identical, so viewing a frame never
  re-reads its file. Nothing re-analyses in the background (the release catalogue has 1 352 analysed OSC lights,
  many on network shares). The loss cell says what to do.
- **O6 — no `ZP_VERSION` bump (controller).** A mono zero point does not change. An OSC frame's stored colour refusal
  is cleared by its re-analysis (`upsert_frame_analysis` replaces the row with its five `zp_*` columns empty) and judged
  by `zero_points_after_analysis`; until then the new reason (§5.3) is read before the stored refusal, and the
  backfill selectors skip outdated mosaic rows (the `CAN_BE_MEASURED` precedent).
- **O7 — the gate does not judge an OSC frame with an empty `FILTER` (controller, interim, until the owner rules on
  §8).** Its loss is shown in the column; the gate gives no verdict and says why ("colour frame without a filter
  name: its clear-sky level may mix filtered and unfiltered nights"). This is today's behaviour for the gate (OSC is
  not judged), so nothing regresses; a dual-band night is never blocked as cloudy by mistake.

## 3. Goals, non-goals

Goals: a green-site `c` from rustafits; four stored columns; the zero point of a mosaic frame read with it; the loss
for OSC; the Analysis tab's outdated count; the Phase A bars (§6) met before anything is stored.

Non-goals: a colour term against Gaia BP − RP (a loss is a difference between frames of one rig, so a population's
colour cancels; recorded); `c` per red or blue site; any change to the mono zero point, `ZP_VERSION`, the hub or the
portal (the metric is the same `transparency_loss`); a background re-analysis; the §8 guard (owner's call).

## 4. Approaches considered

1. **Green-site growth, reported (chosen).** Local to rustafits' mosaic path plus four columns here.
2. **`c` from a debayered copy of the frame.** Analysis would read or debayer twice; the debayer adds its own
   interpolation to the wings; Analysis' star positions are on the mosaic.
3. **The stacking Measure's `c` per plane.** Exists only for frames that went through a stacking run, after
   calibration; a frame's transparency must not depend on whether it was stacked.

## 5. Design

### 5.1 rustafits (`star_measure`, a rustafits PR and a submodule bump, R-OSP-19)

- `photometry::growth_curve` gains a masked form: a masked pixel within 5 F is counted in `N_all` and skipped, a
  present pixel that is not finite still refuses the star, the sum is scaled by `N_all / N_present`, and a disc whose
  present share is outside `[0.45, 0.55]` refuses the star. Footprint, `12F` disc and `B_i` rules unchanged; the
  unmasked form is today's, bit for bit.
- `photometry::aperture_correction` passes the choice through; every candidate screen is unchanged.
- A crate-private way to ask the measure path for "measure `c` on the masked growth, never apply it", used by
  `green::measure_green_sites` only. `PlaneMeasurement` gains `reported_aperture_correction: Option<f64>`,
  `reported_aperture_stars: usize`, `reported_aperture_se: Option<f64>` (`None` / 0 on every other path) — the only
  public change. The per-plane log lines for this path say "reported, not applied".
- `star_measure::CFA_APERTURE_VERSION = 1` beside `MEASURE_VERSION` (8, unchanged): the version of the reported
  mosaic `c`, bumped when only it changes.
- `CHANGELOG.md` entry; released as 1.9.0. rustafits commits carry no trailer (its own rule).

### 5.2 Storage (Athenaeum)

- `frame_analysis` gains `cfa_aperture_correction REAL`, `cfa_aperture_stars INTEGER`, `cfa_aperture_se REAL`,
  `cfa_aperture_version INTEGER`, in `CREATE TABLE` and the `ALTER TABLE` loop of `db/schema.rs` (the 6.5 pattern).
  `FrameAnalysis` (`models.rs`, snake_case as the struct is; `ts_export.rs`; `src/types/models.ts`) carries them; the
  three SELECTs that share `row_to_analysis` and `upsert_frame_analysis` read and write them; every `FrameAnalysis`
  literal is updated.
- `analysis::analyzer::map_measurement`: on a mosaic all four are set (stars 0 and correction NULL when no star
  qualified; version = `CFA_APERTURE_VERSION`); elsewhere all four NULL.
- Personal sync carries the sender's analysis, `zp_*` included (`sync/ingest.rs:938-951`); the `cfa_*` columns travel
  with it. A received row without them is outdated like any old mosaic row.
- Outdated: a predicate `needs_reanalysis(row, bayerpat)` = today's `row_is_stale` OR (`is_mosaic` AND
  `cfa_aperture_version` ≠ current). `analyze_frame_set`'s filter uses it; `get_frame_star_metrics` keeps
  `row_is_stale`. The frame-set analysis listing the Analysis tab reads carries the flag per row, and the tab's
  "analysed" count leaves flagged rows out (no new command; both backends return the same field).

### 5.3 Zero point and loss

- `analysis::zero_point::refresh_zero_point_with`, for a mosaic frame:
  - outdated (`cfa_aperture_version` ≠ current) → `NotReady::AnalysisBeforeColour`, stores nothing, reason code
    `analysis_before_colour`;
  - correction NULL → a stored refusal, reason code `no_aperture_correction`;
  - else every star's flux is multiplied by the correction before the zero point is computed (the zero point rises by
    `2.5·log10 c`; star count and standard error unchanged); every other rule as in 6.5.
  Other colour frames keep `ColourCamera`. The backfill selectors leave outdated mosaic rows out.
- `analysis::transparency`: `load_frame_rows` selects the `cfa_*` columns. Two new `LossReason`s beside
  `ColourCamera`, after `NotSolved`: "Analysed before colour cameras were measured: run Analyze on its frame set." and
  "Too few isolated stars to measure this colour frame's light in the star halos." The rig reference reads mosaic
  frames with a current zero point; other colour frames stay out.
- `collab::gate`: O7. The reason codes and the new field names go into the logging dictionary.

### 5.4 Docs

`docs/catalog/README.md` (the colour paragraph), `CLAUDE.md`'s Transparency line, `docs/stacking/README.md` PC-4,
`docs/logging/` dictionary, `docs/superpowers/open-items.md` (6.5's "Owed: OSC" replaced; the focal-length finding
recorded), a proposed `RELEASE_NOTES.md` line applied only on the owner's yes.

## 6. Truth check before anything is stored (Phase A)

After the rustafits change and before §5.2, the probe (`examples/zp_probe.rs`, analysis in memory, writes nothing)
reports per mosaic frame `c`, its stars and error, the airmass, and the zero point with and without `c`. The bars are
fixed now; the probe may not move one. A missed bar stops the build and comes back to the owner with the numbers.
Told in advance: OSC stability already missed its bar without `c` (0.0352 mag on 2025-10-18), and `c` may not mend it.

| Check | Bar |
| ---- | ---- |
| `c` measured: mosaic frames with a correction, of those analysed | ≥ 95 % per night |
| `c`'s own error: median `se / c` | ≤ 0.01 |
| `c` steadiness: median \|Δ ln c\| of consecutive frames | ≤ 0.015 |
| Seeing: one extinction coefficient per rig shared by its nights, an intercept per night; the FWHM slope of the corrected zero point on each night whose slope error is ≤ 0.015 mag/px (others reported, unjudged) | \|slope\| ≤ 0.03 mag/px |
| `c` does the work: on every judged night where the uncorrected slope is beyond 0.06, corrected minus uncorrected | ≥ 3 errors apart |
| Seeing, airmass-free (2025-10-18, LDN 1272, against the frame-common set of stacking run 9): slope of the mechanism residual against FWHM | \|slope\| ≤ 0.03 mag/px (was +0.095) |
| Mechanism RMS on the same join | not above the uncorrected value |
| Stability inside a clear session (median absolute difference of consecutive frames) | ≤ 0.03 mag per night |
| Coverage: valid zero points of the solved and analysed frames, tiers 500 + 2000 | ≥ 90 % per night |
| Identity: the new path against `FromPlane` on the same mosaic in one build, every `StarFit` and `FrameSummary` field | bit-identical |

Sets (small, varied), every frame solved in the dev catalogue, all on `/Volumes/bigbase3`: ZWO ASI2600MC Duo,
LDN 1272, 2025-10-18 (105) and 2025-09-13 (55; the sign-flip night, FWHM 1.25–1.83 px, undersampled); vdB 152,
2025-08-23 (114) and 2025-10-24 (45). Every night of ≥ 30 frames is judged. Airmass: the solve centre's RA / Dec, the
frame's `sitelat` / `sitelong`, `DATE-OBS` + half the exposure.

## 7. Order of work

1. rustafits (§5.1). 2. The probe. 3. Phase A (GO / STOP). 4. Storage and the outdated flag (§5.2). 5. Zero point,
loss, gate (§5.3). 6. The Analysis tab's count (frontend). 7. Acceptance on a catalogue copy (headless web). 8. Docs,
final review, merge into local `main`; rustafits `osc-green-aperture` into rustafits `main` with a merge commit (D-10).
No push without the owner's word.

## 8. Open for the owner

- **Two filters under one name.** Every OSC light in the dev catalogue has an empty `FILTER`, so a night through a
  dual-band (Hα + OIII) filter and a night without one fall on the same rig; the clear-sky level would come from the
  unfiltered nights and a dual-band frame would read a loss of several magnitudes. Until the owner rules, O7 keeps
  the gate off such frames. A guard that could replace O7: a filter is constant within a night, so cluster a rig's
  night medians; two clusters ≥ 1 mag apart split the rig, confirmed by the sky rate (a filter dims the sky with the
  stars, a cloud does not). A simple gap between frames is not enough (a cloudy night fills it). Data needed: which
  ASI2600MC Pro sets used a dual-band filter. LDN 1272's two nights read 18.82 and 18.72: the same filter.
- The `RELEASE_NOTES.md` line.

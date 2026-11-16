# Collab v3 sub-wave 6.5 — transparency from the photometric zero point (design)

Date: 2026-11-15. Status: approved by the owner 2026-11-15; amended the same day by the real-data probe (§9, ruling Z5).
Parents: `2026-09-23-collab-v3-per-frame-model-design.md` (R9, §6.3, §6.4, §7) and
`2026-10-02-collab-v3-wave6-project-stacking-export-design.md` (W1, §8).
Announced for v0.7.2 as "frame and group weights from star photometry, measured against Gaia"; this
design delivers less than that line says, and rewording it is part of the wave (§7).

## 1. Intent

Athenaeum compares the brightness of a frame's stars with their Gaia G magnitudes and so knows how much
light the frame collected per second: its zero point (ZP). From it comes one number an astrophotographer
reads at once — the **transparency loss**: how much fainter this frame's stars are than the same
equipment's clear sky. Two things use it:

- the owner sees it per frame;
- a project coordinator can refuse frames that lost too much light (haze, cloud, a target low in the
  sky), with one threshold that is fair to every telescope. Today's SNR-family thresholds depend on
  exposure and equipment; this one does not.

It does not change a stack: the frame weight already drops for a hazy frame.

## 2. Rulings

Owner, 2026-11-15:

- **Z1 — The frame weight inside a group stays on the frame-common star set** (R-OSP-18): it compares
  the same stars frame to frame and is more precise than a comparison through the catalogue. No combine
  of group masters.
- **Z2 — The zero point is measured in Analysis**, from the stars Analysis already fits, when the frame
  has a plate solve. Not in the solver, not in the stacking Measure.
- **Z3 — The gate compares transparency loss**, not the absolute zero point. The absolute value depends
  on aperture and camera (80 mm against 280 mm is about 2.7 mag under one sky), so one threshold cannot
  serve a project with mixed equipment.
- **Z4 — Narrow scope: measurement, the Transparency column, the gate.** The zero point as the top rung
  of the `equivalent` exposure classes (W1) and in `WeightMode::Exposure` is **not built**. It needs the
  zero point in electrons across rigs, where `EGAIN` is unreliable for cameras under 16 bit, for a mode
  that is nobody's default. W1's slot stays open; nothing here closes it.

Controller calls (mine; the owner may overrule any):

- **C1 — Everything stays inside one rig's own units.** Loss is a difference between frames of one rig,
  so no gain, no electrons, and nothing is compared across rigs.
- **C2 — No airmass and no colour correction.** A frame low in the sky really collected less light —
  the announced behaviour — so altitude counts as loss, in the table and at the gate. The default
  threshold (0.5 mag) is wider than the extinction down to about airmass 2.5.
- **C3 — All filters and OSC are in scope**, each judged by its own bars (§6). A class of frames that
  misses them ships without a zero point: no loss value, no verdict. *Outcome: mono passes, OSC does
  not and is not measured in this wave (Z5, §9).*
- **C4 — An analysis made by an older `MEASURE_VERSION` gets no zero point** until the frame is
  re-analysed; its flux meant something else.
- **C5 — A published frame shows the loss frozen at publication** (frames are never withdrawn, and a
  later, better reference must not turn a published row red). An unpublished frame shows the live value.
- **C6 — One column, "Transparency".** The raw zero point is in a rig's own units and means nothing
  beside another rig's, so it lives in the tooltip and the frame panel, not in a column.
- **C7 — The manifest carries the loss only.** v3 §5's `zeroPoint` key stays unbuilt with Z4.
- **C8 — No new Settings entry.**

## 3. Goals, non-goals, anti-goals

Goals:

1. `zero_point`, `zp_stars`, `zp_sigma` for every analysed, plate-solved light, with no pixel read beyond
   the analysis that already ran.
2. Transparency loss per frame against its rig's clear-sky reference.
3. Gate metric `transparency_loss` in the app, the hub and the portal.
4. The loss in the frame tables, the frame panel and the publication manifest.

Non-goals: the `equivalent` rung and the `Exposure` weight (Z4); an absolute `zero_point` gate metric;
group-master combine; the zero point in any weight; an electron zero point; absolute photometric
calibration; persisting a second flux per star; any Settings knob.

Anti-goals:

- A stack of an existing set changes by one bit, or recomputes a cached stage, in any mode.
- A frame is blocked from publication because its rig has no reference yet.
- A second star-measurement path appears beside `star_measure`.

## 4. Approaches considered

| Where the zero point is measured | Trade-off | Verdict |
| ---- | ---- | ---- |
| Analysis, when a solve exists | fluxes from the one PSF path; every reader gets a stored number; needs a recompute when the solve arrives after the analysis | **chosen (Z2)** |
| The plate solver, at solve time | free matching, but detection-grade fluxes, a solver change, and frames whose WCS came from a manifest get nothing | rejected |
| The stacking Measure | the best fluxes, but it never runs at publication, so the gate gets nothing | rejected |

Gate quantity: absolute zero point (v3 §6.3 as written) against transparency loss — Z3.
Scope: with the exposure classes and the `Exposure` weight, against without — Z4.

## 5. Design

### 5.1 Measurement

One core function computes a frame's zero point from stored rows: the frame's `star_metrics` (`flux` is
the model flux times the plane's aperture correction), its `plate_solves` WCS (SIP included),
`frames.exptime`, and a cone of the installed catalogue (`plate_solve::layers::open_catalog_layers`,
proper motion to the frame's epoch). Both sides are 0-based in file row order (`plate_solve/storage.rs`,
`analysis/analyzer.rs`): no flip, no ±1.

- Catalogue stars are projected to pixels through the WCS; an analysis star takes the nearest catalogue
  star inside a small radius, and only when no second catalogue star sits near enough to blend. A star
  the installed catalogue does not hold is simply not used, so the base tier alone must be enough (§6).
- Only stars in the central part of the frame are used: Analysis runs on unflattened lights, and
  vignetting would otherwise widen the scatter.
- Per star `zp_i = G_i + 2.5·log10(flux_i / exptime)`; the frame's value is the sigma-clipped median,
  `zp_sigma` the robust scatter (1.4826·MAD), `zp_stars` the survivors. Stars near saturation are out.
- Valid when `zp_stars` reaches the minimum and the median's standard error (`zp_sigma / √zp_stars`) is
  small enough. The test is on the median's error, not the raw scatter: through a narrowband filter the
  per-star offset to G is wide, yet the median of many stars is still sharp. An invalid frame has `NULL`
  and a logged reason with the counts.
- Starting constants, fixed by the probe of §6 before any consumer is built: match radius 2 px, central
  region 0.6 of the half-diagonal, minimum 15 stars, standard error ≤ 0.05 mag, clip 2.5 σ. *The probe's
  final values are in §9.*

Storage — `frame_analysis`, additive nullable columns: `zero_point` (mag, in the analysis' flux units
per second), `zp_stars`, `zp_sigma`, `zp_version` (a `ZP_VERSION` constant).

Triggers — the function runs (a) after an analysis is saved for a solved frame, (b) after a solve is
saved for an analysed frame, (c) lazily, for frames that have both rows, a current `measure_version`
(C4) and no current `zp_version`, before the publication gate evaluates them, (d) once per launch, in
the background, for every such frame in the catalogue, so frames analysed before this wave fill in
without a re-analysis. A refusal is stored with the current `zp_version`, so a hopeless frame is not
retried. An analysis upsert replaces the row, so (a) always follows it. A stacking run's own cached solves never reach
`plate_solves`; a frame solved only there has no zero point until it is solved in the catalogue.

### 5.2 Transparency loss

- **Rig** = camera, telescope, focal length (rounded), gain setting, binning, filter. Frames of one rig
  share one unit system, so the difference of their zero points is the difference in collected light.
- **Reference** = a high percentile (start: the 90th) of the valid zero points of the rig's frames in
  the local catalogue, once the rig has enough of them (start: 30 frames from at least 2 nights). A
  percentile, not the maximum: one wrong match must not become "clear sky".
- **Reference scope**: the frame's own target first (rig × frame set, where the field's colour and
  altitude cancel); when that target has no reference yet, the rig across all its targets stands in,
  and only if the probe's cross-target bar held (§6).
- A rig needs a camera and a focal length: a frame without either has no rig and no loss. Frames
  received through a project never feed a reference.
- **Loss** = `max(0, reference − zero_point)`, in mag, never negative. No reference → no loss value.
- Computed on read (the reference moves as frames arrive), never stored locally; the value at
  publication time is written into the manifest (C5).

### 5.3 Gate, hub, portal

- New metric `transparency_loss`: unit mag, op `lte`, a positive number, not advanced. Label
  "Transparency loss"; help: "How much fainter the frame's stars are than this equipment's clear sky —
  haze, cloud or a target low in the sky. 0.5 mag is about a third of the light lost."
- Three registry copies change together: hub `collab_rules.rs`, portal `metrics.ts`, app
  `collab/gate.rs::METRIC_REGISTRY`, each with its pinning test.
- A frame with no loss value (unsolved, no zero point, no reference yet) gets the rule's row with no
  value and no verdict, and is not blocked. `RuleVerdict` gains an optional `reason` so the row can say
  why ("no clear-sky reference yet, 12 of 30 frames"); today the type has no such field.
- `zero_point` stays out of all three registries, now by ruling Z3; the open-items "do not re-flag"
  entry is reworded.
- An app older than this wave skips an unknown metric (existing behaviour), so a coordinator may add
  the rule before every member updates. The hub is deployed to test first; production on the owner's
  word.

### 5.4 Manifest

Publication appends one optional `meta` key, `transparencyLoss` (mag), beside the analysis keys that
are already there. The wire stays append-only; golden pins gain the new key.

### 5.5 UI

One column, "Transparency", in the project frame table and the stacking Frames block; the frame panel
shows it with the zero point, star count and scatter. The cell shows the loss with a minus sign (the
frame is that much fainter than clear sky); the gate's number is the same value, positive.

```
Frame             FWHM   SNR    Transparency
M31_L_0142.fits   2.9″   41.2   −0.05 mag
M31_L_0171.fits   3.4″   22.1   −0.77 mag
M31_L_0188.fits   3.1″   35.0   —            (tooltip: no clear-sky reference yet, 12 of 30 frames)
```

Design tokens only; the column sorts.

## 6. Truth check before anything is built

Task 1 of the plan is a probe (a core example, real frames from the dev catalogue and the test
corpus). The plan names the sets before it runs; the bars below are fixed now. The probe may move the
starting constants of §5.1–5.2; it may not move a bar. Frame classes: broadband mono, narrowband mono,
OSC — each class is judged on its own (C3).

| Check | Bar |
| ---- | ---- |
| Mechanism, on an OSC group, where the frame-common set reads aperture sums (an independent estimator): zero-point difference between frames against 2.5·log10 of the paired flux ratio. A mono group is reported beside it, not judged: there both sides read the same flux | residual RMS ≤ 0.05 mag |
| Stability inside a clear session: median absolute difference of consecutive frames (immune to the slow airmass drift) | ≤ 0.03 mag |
| Seeing independence: slope of the consecutive-frame zero-point difference against the FWHM difference | ≤ 0.03 mag per px |
| Coverage with the base catalogue tier alone: frames that reach a valid zero point, of the solved and analysed ones | ≥ 90 % per class |
| Match geometry: median distance of a match | ≤ 1 px |
| Cross-target: one rig's clear-sky reference on at least 3 targets | agree within 0.10 mag → the rig may stand in for a target that has no reference of its own; else it may not |

A missed bar stops the wave for that frame class and comes back to the owner with the numbers; the
cross-target row never stops, it decides whether the rig-wide fallback is allowed. If the seeing bar misses, the remedy is an
aperture sum from Analysis, which is a rustafits change (R-OSP-19) and a decision of its own. If no
class passes, the wave is closed and only the announcement is reworded.

Acceptance after the build: an existing set stacked before and after produces bit-identical masters
and recomputes no cached stage; on real data the gate refuses a known hazy frame and passes its clear
neighbours; a frame of a rig without a reference is not blocked and its row says why.

## 7. Order of work

1. Probe (§6).
2. Measurement, storage, triggers (§5.1) and transparency loss (§5.2).
3. Table column and frame panel (§5.5).
4. Gate and manifest (§5.3, §5.4), then hub and portal in the `athenaeum-hub` repo.
5. References: `docs/catalog/README.md`, `docs/transfers/README.md`, `docs/logging/README.md` (new
   field names), `docs/superpowers/open-items.md`; `docs/stacking/README.md` and the wave 6 spec say
   the `equivalent` zero-point rung is deferred by Z4, not owed.
6. `RELEASE_NOTES.md`, "Coming in 0.7.2": the line promises frame and group weights; it becomes the
   transparency number and the coordinator's threshold. Wording is the owner's to approve.

## 8. Left to the builder

- Module placement (`analysis/` beside the analyzer is the expected home) and the exact SQL of the rig
  reference.
- How saturation and blends are told from `star_metrics` rows, or whether the in-memory measurement's
  flags must be persisted for it.
- Whether a catalogue tier upgrade should recompute zero points (`catalog_identity` beside
  `zp_version`).
- Whether personal sync carries `frame_analysis` rows; if it does, the new columns travel as appended
  optional fields.
- Column width, number format and tooltips within `docs/frontend/conventions.md`.

## 9. Probe outcome and amendments (2026-11-15)

The probe of §6 ran on 616 real frames (two rigs, four Milky Way fields, 13 nights) on two catalogue
sets. Numbers and files: the wave's ledger, `.superpowers/sdd/2026-11-15-collab-v3-wave6-5/progress.md`.

| Class | Outcome with tiers 500 + 2000, final constants |
| ---- | ---- |
| Broadband mono (L, 3 targets) | every judged bar passes: stability 0.024, seeing −0.019 mag/px, cross-target 0.025 |
| Narrowband mono (H, 2 targets) | stability 0.024 passes; the seeing bar is unjudged (too few pairs); with airmass fitted the slope is +0.013 mag/px |
| OSC | does not pass: the zero point follows the seeing by +0.17…+0.21 mag/px one night and −0.14 another (the stored flux of an OSC star carries no aperture correction) |

Owner ruling:

- **Z5 — Mono first, then OSC.** This wave is built for mono cameras. An OSC frame gets no zero point,
  its cell says "not measured for colour cameras yet", and the gate does not block it. The OSC remedy
  (an aperture sum from Analysis, a rustafits change under R-OSP-19, then a re-probe) follows as a task
  of its own. With the chosen option the owner also accepted: narrowband on the indirect seeing
  evidence, and a shallow catalogue answered by a refusal that asks for the deeper tier — the "base tier
  alone" coverage bar of §6 does not hold for broadband and is dropped.

Amendments from what the probe showed:

- **Final constants**: match radius 2 px, central region 0.9 of the half-diagonal, minimum 50 stars,
  standard error ≤ 0.035 mag, clip 4 σ, the brightest 15 % of the usable stars dropped, star SNR ≥ 10,
  a catalogue neighbour within 3 FWHM counts as a blend only when it is within 4 mag. The first
  probe fixed 15 stars and 0.02 mag; the acceptance run on real data corrected both (next bullet).
- **What the validity rule guards.** A zero point is current only for the installed catalogue's
  identity (next bullet), and a transparency loss is a difference between frames of one rig measured
  against one catalogue, so a bias of the star population cancels in it. What the rule guards is
  noise, not bias: under 50 matched stars the frame-to-frame scatter was 0.10–0.12 mag. The shallow
  catalogue is where that happens. The brightest catalogue stars of a Milky Way field are red (giants,
  reddened): G reaches 1050 nm, a luminance filter stops near 700 nm, so they read 0.3–2 mag fainter
  than their G says. With only the base tier installed they are the only matches, so a frame keeps few
  stars (at most 49 on the 295 shallow-catalogue L frames) and its zero point comes out 0.2–0.3 mag
  low. It is not saturation (no flag, raw peaks at 10–45 % of full scale). §5.1's "stars near
  saturation are out" is therefore the bright-end drop, nothing more. The first limit, 0.02 mag, was
  set to refuse those frames, and it refused the wrong ones too: through a blue filter the per-star
  offset to G scatters widely, so with ~265 kept stars the standard error is 0.022–0.030, yet those
  frames are stable to 0.04 mag frame to frame; 49 of 50 blue frames were refused. Standard error
  ≤ 0.035 mag and at least 50 kept stars keep every frame measured on the deeper catalogue and still
  refuse the shallow ones. Result with tiers 500 + 2000, valid frames over frames measured: L, H, R, G
  and B each 100 %; the shallow-catalogue L frames 0 of 295.
- **The catalogue is part of a zero point's identity.** `frame_analysis` gains `zp_catalog` (the
  installed tiers' identity at measurement time). A frame measured against another tier set is
  recomputed, and a reference reads only frames measured against the installed one: a deeper tier
  changes the star population and with it the level.
- **Extinction in numbers.** 0.11–0.18 mag per airmass in these data. A frame at airmass 2.5–2.7 reads
  a loss of 0.20–0.25 mag under a clear sky; below about 19° of altitude it crosses 0.5 mag. This is C2
  as ruled, stated here so a threshold is chosen knowingly.
- **Frame-to-frame noise** is about 0.03 mag: the 500-star analysis sample shares only a few matched
  stars between consecutive frames. The sky itself moves 0.013–0.017 mag. `zp_sigma / √zp_stars`
  understates the real error by about 2×.
- **Not judged:** the green and blue filters were judged on stability only (0.024–0.041 mag), not on the
  other bars; no frame in the probe was shot through haze or cloud (the largest dip is 0.09 mag);
  the cross-target bar for narrowband; a camera under 16 bit; a field at high galactic latitude; a long
  focal length. The acceptance of §6 uses a low-altitude frame (a real 0.24 mag loss) in place of a hazy
  one; a cloudy night is owed.

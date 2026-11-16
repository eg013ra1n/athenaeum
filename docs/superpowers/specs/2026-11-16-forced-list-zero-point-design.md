# Forced-list zero point — mono and OSC (design)

Date: 2026-11-16. Status: draft for the owner. Follows `2026-11-15-collab-v3-wave6-5-zero-point-design.md` (§5.1, §6,
§9; merged to local `main`, not pushed) and `2026-11-16-osc-zero-point-design.md` (O1–O7; its Phase A stopped).
Evidence: ledger `.superpowers/sdd/2026-11-16-osc-zp/` (`task-3c-report-r0.md`, `research-web.md`); data and the
emulation of §4 in `~/.athenaeum-acc/2026-11-16-osc-zp/out/{3c,r1,4d}/` (`4d/emul4d.py`, `4d/emul4d-r3.txt`).

## 1. Intent

Today's zero point (ZP) is the median of `G + 2.5·log10(flux / t)` over the Analysis budget's catalogue-matched stars.
The budget fits 500 of 5 500–10 400 detections, chosen per frame: consecutive frames share 7.5 % (LDN 1272, 2025-09-13)
and 5.0 % (2025-10-18) of their matched stars, a star's offset to G scatters 0.1–0.95 mag, so the median jitters by
0.03 and drifts with the subset (on 09-13 its median G follows the seeing, +1.5 mag/px). On a fixed sample the same
fluxes follow the stacking photometry to 0.012–0.013 mag rms and its extinction to 0.001 mag/airmass (3c). The ZP moves
to **a list of catalogue stars chosen before any pixel is read, measured at their catalogue positions**, the same stars
in every frame of a pointing. 6.5's mono ZP reads the same budget (2–8 shared stars of ~180), so mono moves too.

## 2. Rulings

- **F0 (owner, 2026-11-16: «да, давай оба переводить»).** Mono and OSC move to the forced list now; 6.5's push waits.
  Controller calls follow (the owner may overrule any).
- **FL1 — the window is set per frame by a rig-independent rule anchored on the frame's own preliminary ZP** (§5.1);
  not per rig from data, not ensemble photometry (§4.2).
- **FL2 — a list star is measured by the one PSF path**: Analysis' Moffat fit seeded at its WCS position, centre free,
  the frame's β and field width, times an aperture correction `c` measured over the list's own bright members (§5.2).
- **FL3 — it runs inside Analysis' read** of a solved frame and in a background pass after a solve batch; nothing
  reads pixels at launch, on a tier install or at the gate (§5.4).
- **FL4 — no per-star table**; `ZP_VERSION` 2 → 3, three columns added (§5.5).
- **FL5 — the OSC spec:** O1 holds in substance (no Analysis number moves, `MEASURE_VERSION` stays 8); O2's
  phase-matched masked growth measures the list's `c` on a mosaic; O4's `is_mosaic` and O7 stay; O5, O6, the
  `cfa_aperture_*` columns and `AnalysisBeforeColour` are superseded by the ZP's own currency. 6.5's Z1–Z5 and C1–C8
  stand, except C3's outcome: mosaics are measured again, judged by §6.

## 3. Goals, non-goals, anti-goals

- Goals: a stored per-frame ZP from a frame-independent list, mono and OSC mosaics; §6's bars met in the probe before
  anything is stored; the loss, the gate and the Transparency column read it unchanged.
- Non-goals: a colour term or synthetic catalogue magnitudes; a seeing-regularised `c` (a cross-frame value); ensemble
  photometry; debayered or 3-plane colour files (keep `ColourCamera`); a Settings entry; hub and portal (same metric).
- Anti-goals: an Analysis number moves (stars, `FrameSummary`, `c`, β, the overlay, the plate-solve gate); a second
  star path beside `star_measure`; a launch pass reading every catalogued light; a stack changes by one bit.

## 4. Approaches considered

### 4.1 Emulation on the 3c tables

The catalogue is proxied by every Gaia star matched in any frame of the night (3 429 / 4 353); per frame a cubic
sky→pixel map fitted on its own matches (residual ≤ 0.01 px) projects them, and a rule (region 0.9 of the half-diagonal,
40 px margin, window, cap) gives the list. Each star's offset `a_i` and scatter `σ_e(G)` (0.038–0.108 mag) come from
the fixed-sample solution `zp_ij = a_i + b_j`. B1 = share of a frame's list its successor also has; churn-only = median
|Δ| of the list's `median a_i`; list noise = the same with `σ_e` drawn per star (no sky, no `c`). Geometry and window
only, no fit refusals; saturation does not bind for G ≥ 12.6 (70 % of full scale at G 11.25–11.72 / 10.35–10.96).

| Rule (09-13 / 10-18) | n per frame | B1 min | churn-only | list noise |
| ---- | ---- | ---- | ---- | ---- |
| today, budget sample (measured, with `c`) | 147–207 / 129–188 | median 7.5 / 5.0 % | — | stability 0.034 / 0.030 |
| fixed G 13–16, no cap | 2 866 / 3 944 | 95.3 / 98.8 % | 0.0005 / 0.0001 | 0.0032 / 0.0030 |
| **anchored on the preliminary ZP, 12.6–15.6 at the median frame, cap 1000** | 997 / 986 | **96.7 / 90.3 %** | 0.0014 / 0.0009 | **0.0053 / 0.0057** |
| fixed 12.6–15.6, cap 500; cap 300 | 485 / 514; 307 / 317 | ≥ 95.1 % | ≤ 0.0003 | 0.0078 / 0.0077; 0.0098 / 0.0096 |
| anchored on the frame's own saturation G (seeing and sky) | 3 036 / 2 149 | 92.2 / 83.8 % | 0.0014 / 0.0056 | 0.0039 / 0.0065 |

The chosen rule's 90.3 % is the 10-18 meridian flip, where the anchor follows the real +0.17 mag step. A window on the
frame's saturation carries a seeing slope from churn alone, −0.029 / +0.034 mag/px: the 3c mechanism again.

### 4.2 The brightness window

- **(a) Per frame, anchored on the preliminary ZP (chosen).** A stored per-frame value no other frame changes, so
  `transparency.rs`' reference-on-read stays. Cost: under a loss L the window follows the frame's light L mag brighter,
  and the offset depends on G (10-18: G 12.5–13 reads 18.42 against 19.13 at 13–13.5, red giants). Emulated ZP shift
  −0.008 / +0.005 mag at L = 0.5, +0.012 / +0.040 at L = 1: the loss reads up to 4 % small.
- **(b) Per rig, fixed from data.** Removes that ≤ 4 % but needs a stored window per rig and exposure, frozen by the
  first frame measured (order-dependent; two machines differ under sync) or chosen on read (a moving window turns
  stored ZPs stale, and re-measuring costs a file read). Rejected.
- **(c) Ensemble, each star against its own reference.** Churn stops mattering (3c same stars: 0.0090 / 0.0140), but
  it needs a per-star table (~1 000 rows a frame beside `star_metrics`' 500), a per-star reference on read for every
  table, and loses the rig-wide fallback across targets (6.5: 0.025). The fixed list holds churn to ≤ 0.0016 anyway.

### 4.3 The measurement

| At the catalogue position | Evidence | Verdict |
| ---- | ---- | ---- |
| Moffat fit seeded there, centre free, × `c` | 3c same stars: rms against the stack 0.012 / 0.013, FWHM slope +0.036 / −0.013 mag/px | **chosen** |
| Fixed-position, fixed-shape amplitude fit | a position error δ costs `exp(−δ²/4σ²)` of the flux: 1.3 % at 09-13's 0.12 px median match (σ 0.53 px), 3.5 % at 0.2 px; the WCS changes at a flip; new rustafits code | rejected |
| Aperture of k·FWHM × `c_k` | a second photometry path; on the mosaic ±10 % lattice phase error per star at FWHM 1.25 px | rejected |
| Plain large aperture (6 F, per-phase annulus sky) | rms 0.009 / 0.020, slope +0.015 / +0.061; a detected neighbour within 8–20 px brightens a 6 F sum by 0.28–0.47 mag against the model (10-18) | rejected; the probe's referee |

`c` over the budget's ≤ 64 qualifiers takes the seeing slope away (+0.065 → −0.013 mag/px against the stack, 10-18) but
adds jitter (ours − stack stability 0.0086 → 0.0123; `c` alone 0.0061 / 0.0121). Over the list's own isolated bright
members, up to 256, its noise should about halve (an estimate; the probe reports both).

### 4.4 Where it runs

Inside Analysis' read (chosen: plane in memory, the budget gives the anchor, one path). The solver reads its file inside
`solvemyastro` and hands back no pixels, so a solve batch's frames are measured afterwards by the Analysis entry. A
launch pass would read every solved light (1 352 OSC lights alone in the release catalogue, many on network shares).

## 5. Design

### 5.1 The list (`analysis::zero_point`, pure)

Inputs: the stored solve (SIP; a forward SIP with no reverse one stays `UnreadableSolve`), the catalogue cone (proper
motion to the epoch), the frame's size, pixel scale (from the solve), exposure, full scale as Analysis reads it, and the
budget's fits from the same measurement. Positions are 0-based in file row order on both sides.

- **Reference width** `FWHM_ref = max(2 px, 2.5″ / scale)`: the rig's PSF scale, set by its pixel scale and never by
  the night's seeing (2 px on the ASI2600MC Duo at 414 mm, 1.87″/px).
- **Anchor**: the preliminary ZP, 6.5's `measure_zero_point` over the budget fits (on a mosaic their flux × the
  reported `c`, so mono and OSC sit alike), validity relaxed to ≥ 10 matches; fewer → refusal `no_anchor`.
- **Window**: the bright edge `G_b` is 1.5 mag fainter than a star whose peak would reach 70 % of full scale at
  `FWHM_ref` (Moffat β 4) at the anchor; the faint edge `G_b + 3`, deepened by catalogue count up to `G_b + 4.5` while
  the region holds fewer than 200 isolated stars. Here `G_b` ≈ 12.5–12.7, 1.0–2.2 mag below the real 70 % level.
- **Region**: inside 0.9 of the half-diagonal (6.5) and at least `13·FWHM_ref` from every edge (the ring sky fits).
- **Isolation, by the catalogue only**: no neighbour with ΔG ≤ 4 within `4·FWHM_ref` (15″ here), no star brighter than
  `G_b − 2` within `10·FWHM_ref` (halo, bleed, spikes). Contamination goes with distance over PSF width; `FWHM_ref` is
  arcsec on a well-sampled rig, pixels at the sampling floor, and never moves with seeing, so isolation never churns
  (6.5's 3 × the frame's FWHM did). ΔG 4 is 2.5 % of the light; at `4·FWHM_ref` a β-4 Moffat puts < 0.1 % of it in the
  fit region. Proxy share with a neighbour (ΔG ≤ 5) within 10″ / 20″: 0.6–2.8 % / 3.9–5.7 % (a lower bound).
- **Cap** 1 000: above it a star stays when a hash of its catalogue record (G, position at 1″) falls under `1000 / n`,
  so the same stars are kept in every frame and the G distribution holds (list noise 0.0053 against 0.0078 at 500).

### 5.2 The measurement (rustafits PR and submodule bump, R-OSP-19)

One entry beside `analysis_mode::measure_raw_frame` reads the frame once, measures it exactly as today, lets the caller
build the list from that measurement, then fits the list on the same plane:

- seeds at the list positions, by `fit_stars_at`'s path with the plane's β and field width (on a mosaic the native
  mosaic under the green-site mask); the detected seeds keep their `3F` discs out of every ring sky, so an unlisted
  neighbour cannot lift `B_i`; a mosaic's sky is green sites only, G1 and G2 in equal shares in ring and fit, so a
  pedestal difference cancels (per-phase sky served the probe's all-site sums);
- a fit is kept when accepted, not saturated or near the clip, and centred within `max(1 px, 0.5·FWHM_ref)` of its
  projected position; no SNR cut (a frame-dependent cut churns the faint end with the sky);
- `c` over the kept fits passing Analysis' own `c` screen, at most 256 (`growth_curve` on mono, O2's phase-matched
  masked growth on a mosaic; fewer than 5 → `None`); each kept flux = model flux in its FWTM × `c`;
- returned apart: per seed its fitted position, flux, SNR, flags; `c`, its stars, its error. Cost: ≤ 1 000 fits and 256
  growth curves beside the budget's 500 fits. The Analysis measurement is bit-identical with and without a list (test).

### 5.3 Statistic and validity

`zp_i = G_i + 2.5·log10(flux_i / t)` per kept star; the ZP is the 4 σ-clipped median, `zp_sigma` its 1.4826·MAD,
`zp_stars` the survivors (6.5's code). Valid with ≥ 50 survivors and `zp_sigma / √zp_stars` ≤ 0.035 (6.5's constants;
the probe may move constants, never a bar). Stored refusals: `no_exposure`, `no_anchor`, `no_aperture_correction`,
`too_few_stars` (with the list size: a shallow catalogue or sparse field is told from failed fits), `too_scattered`.

### 5.4 Where it runs (no new command: Tauri and Axum call the same core)

- **Every stored analysis** (`analyze_frame_set`; the on-demand analysis behind `get_frame_star_metrics`): with a usable
  solve and an installed catalogue the read carries the list and the `zp_*` columns go into the same upsert. Without a
  solve the analysis is stored and the ZP waits (`NoSolve`).
- **Analyze's selection** (`force = false`) adds solved frames whose ZP is not current; the tab's listing carries the
  flag per row (both backends) and counts them apart, so Analyze re-reads only them. Blink never re-reads a file for it.
- **After a solve batch** a background pass (one at a time, the `BackfillGuard` pattern) re-measures the frames it
  solved that have a current analysis: one read each through the same entry (zip-resident frames materialised), `zp_*`
  only, compare-and-set on the analysis and solve rows; an offline file is a `debug` line, one `warn` per pass.
- **Removed**: the launch and tier-install backfill (`spawn_zero_point_backfill`) and the gate's lazy refresh
  (`collab.rs`, `refresh_listed_with`), which would read pixels. The gate reads stored values only.

### 5.5 Storage and currency

- `ZP_VERSION` = 3. `frame_analysis` keeps `zero_point` (the list's flux units per second), `zp_stars`, `zp_sigma`,
  `zp_version`, `zp_catalog`, and gains `zp_list_stars INTEGER` (the list before fitting), `zp_aperture_correction
  REAL` (the `c` applied), `zp_window_bright REAL` (`G_b`): `CREATE TABLE`, the `ALTER TABLE` loop, `FrameAnalysis`,
  `ts_export.rs`, `src/types/models.ts`; personal sync carries them as appended optional fields.
- Current = `zp_version` 3 and `zp_catalog` the installed identity; an analysis upsert or a new solve clears `zp_*` (a
  new WCS moves the list) and §5.4 fills them again. No per-star table: it would spare a file read only when the
  statistic alone changes, double `star_metrics`' rows, and not travel by personal sync.

### 5.6 Loss, gate, UI

- `transparency.rs` keeps its arithmetic (rig, frame set, 90th percentile, 30 frames, 2 nights, fallback), reads only
  version-3 ZPs; mosaics join the references (`is_mosaic`); other colour files keep `ColourCamera`.
- `NotMeasured` says "transparency not measured yet — run Analyze on its frame set"; `NoStarMetrics` goes (no stored
  star rows are read); a refusal says why in one sentence (too few isolated stars to measure the halos; too few
  catalogue stars, install the deeper tier, when the list was small). Gate: O7 stays; a frame without a current ZP gets
  no verdict and is not blocked. New fields and codes go into the logging dictionary.

## 6. Truth check — bars fixed now, judged in the probe before anything is stored

`zp_probe forced` runs §5.1–5.3's shipping functions on frames read in memory and writes TSVs and a verdict, no
database. Tiers 500 + 2000, all solved in the dev catalogue. OSC: LDN 1272 2025-09-13 (55) and 10-18 (105), common sets
of stacking runs 10 and 9; vdB 152 2025-08-22 (100), 08-23 (116), 10-24 (45). Mono: ATR2600M L, LDN 1272 2025-10-18
(82, run 9's common set); 6.5's QHY268M L (IC 5146, LDN 1042, LDN 1031) and H (IC 5146, LDN 1042). Nights of ≥ 30
frames are judged, pairs ≤ 15 min apart. The sky moves 0.0032 / 0.0041 / 0.0036 frame to frame on the common-set
nights; 6.5 put it at 0.013–0.017 on its mono nights (no common set). A missed bar stops the build; numbers to owner.

| Check | Bar | Establishes / cannot establish |
| ---- | ---- | ---- |
| B1 kept list stars a frame shares with its successor | every pair ≥ 80 % | the 3c mechanism is gone (today 5–7.5 %); not that the ZP is right |
| B2 stability, median \|ΔZP\| | ≤ 0.015 on the common-set nights; elsewhere ≤ 0.020 and not above today's (vdB 0.021–0.024, 6.5 L / H 0.024) | frame-to-frame noise; not a slow drift or a constant bias |
| B3 ZP − 2.5·log10(t_flux / t), common set | rms ≤ 0.015; FWHM slope within ±0.04 mag/px | agreement with an independent calibrated photometry; not seeing independence below the ~0.04–0.06 mag/px spread of the references' own unconverged apertures (3c) |
| B4 ZP = a + kX (+ s·[after flip], reported), per night | \|k − k_stack\| ≤ 0.05 where ΔX ≥ 0.4: ATR L, and 10-18 one pier side at a time (F7) | the ZP follows real light loss in size; not haze or cloud |
| B5 no common set: one k per rig, an intercept per night, a flip term where one exists | \|FWHM slope\| ≤ 0.03 mag/px where its error ≤ 0.015 | no seeing term on the vdB and 6.5 nights; not below the error |
| B6 coverage: valid ZPs of the solved, analysed frames | ≥ 90 % per night; 6.5's sets at 6.5's 100 % | the rule works on real frames; not accuracy |
| B7 median \|fitted − projected\| centre | ≤ 0.25·FWHM_ref | the WCS and the seeding agree; not the flux |
| B8 mono not worse: seeing slope with airmass (≤ 0.03), L cross-target agreement (≤ 0.10; 6.5 0.025) | each within 6.5's bar and not above its value | no regression on the shipped classes |
| B9 the Analysis measurement with and without a list | every `StarFit`, `FrameSummary`, `PlaneMeasurement` field bit-identical | no Analysis number moves (rustafits test + probe) |

Reported, not judged: both `c`s; list size and refusals per frame; haze (none in the corpus, largest dip 0.09 mag; a
cloudy night is owed); under 16 bit, long focal length, high latitude (no data; the faint-edge extension is unjudged).
Acceptance on a catalogue copy (headless web): a solved set analysed stores version-3 ZPs; a set solved after analysis
fills in by the solve pass; an offline file says "run Analyze"; an existing stack is bit-identical and recomputes no
cached stage; a low-altitude frame reads its loss; a rig without a reference is not blocked and says why.

## 7. Order of work

1. rustafits: §5.2's entry and identity test (PR, `CHANGELOG.md`, no trailer; submodule bump). 2. Core, pure: anchor,
list, statistic, synthetic tests (a dithered field keeps ≥ 95 % of its list; a loss moves the window by the loss; a
capped list keeps the same stars). 3. `zp_probe forced` and its verdict. 4. Phase A: GO or STOP. 5. Storage, currency,
triggers, removals. 6. Loss reasons, gate, the tab's count, the cell. 7. Acceptance. 8. Docs (`docs/catalog/README.md`,
`CLAUDE.md`'s Transparency line, `docs/logging/`, `open-items.md`, pointers in the 6.5 and OSC specs), final review,
merge into local `main`; 6.5 and this push together on the owner's word.

## 8. Owner answers (2026-11-16) and what is still open

- **F6 — existing frames: no migration** («Не нужна миграция, достаточно сказать что анализ отличается. В лайве ещё
  нет проектов больших»). Nothing reads files at launch, after a tier install or at publication: an old frame's cell
  says its analysis differs from the current one ("run Analyze"), and Analyze re-measures it. Publication does not
  measure: a frame without a current ZP is published with no transparency verdict and is not blocked (as today).
- **F7 — the 10-18 meridian-flip step** («Без понятия, возьми другие кадры»): its cause is unknown, so it is not a
  judge of the method. B4 is judged on the ATR2600M L night and on 10-18 one pier side at a time where a side has
  ΔX ≥ 0.4; the flip step `s` is reported, not judged.
- **F8 — dual-band data for the OSC spec's §8 guard** («Squid и некоторые veil снимались с узкой полосой»): the
  ASI2600MC Pro Squid set and part of the Veil sets were shot through a dual-band filter, the rest of that rig without.
  The guard is not built in this cycle; O7 keeps the gate off OSC frames with an empty `FILTER`. Those sets (none
  solved today) are the guard's data when it is built.
- Still open: the `RELEASE_NOTES.md` line (proposed at the close, applied only on the owner's yes).

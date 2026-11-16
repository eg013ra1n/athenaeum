# Forced-list zero point — mono and OSC (design)

Date: 2026-11-16. Status: draft for the owner; amended the same day by the model re-audit (F5) after the first Phase A
stopped (§4.5), and by the plan review of that amendment (fable, `review-g0-spec-plan-fable.md`). Follows
`2026-11-15-collab-v3-wave6-5-zero-point-design.md` (§5.1, §6, §9; merged to local `main`, not pushed) and
`2026-11-16-osc-zero-point-design.md` (O1–O7; its Phase A stopped).
Evidence: ledger `.superpowers/sdd/2026-11-16-osc-zp/` (`task-3c-report-r0.md`, `research-web.md`, `task-F4-report-r0.md`,
`task-F5-report-r0.md`, the controller's rulings RR1–RR4 in `progress.md`); data and the emulations in
`~/.athenaeum-acc/2026-11-16-osc-zp/out/` (`3c/`, `r1/`, `4d/emul4d.py`, `4d/emul4d-r3.txt`; the first Phase A `f0/`, its
judge `f4/`; the re-audit's run `g0/`, scripts and evidence `f5/`, the chosen candidate `e-max4020+p/verdict-forced.txt`).

## 1. Intent

Today's zero point (ZP) is the median of `G + 2.5·log10(flux / t)` over the Analysis budget's catalogue-matched stars.
The budget fits 500 of 5 500–10 400 detections, chosen per frame: consecutive frames share 7.5 % (LDN 1272, 2025-09-13)
and 5.0 % (2025-10-18) of their matched stars, a star's offset to G scatters 0.1–0.95 mag, so the median jitters by
0.03 and drifts with the subset (on 09-13 its median G follows the seeing, +1.5 mag/px). On a fixed sample the same
fluxes follow the stacking photometry to 0.012–0.013 mag rms and its extinction to 0.001 mag/airmass (3c). The ZP moves
to **a list of catalogue stars chosen before any pixel is read, measured at their catalogue positions** (mapped onto the
frame by the list's own bright stars, FL6, as plain aperture sums, FL2), the same stars in every frame of a pointing.
6.5's mono ZP reads the same budget (2–8 shared stars of ~180), so mono moves too.

## 2. Rulings

- **F0 (owner, 2026-11-16: «да, давай оба переводить»).** Mono and OSC move to the forced list now; 6.5's push waits.
  Controller calls follow (the owner may overrule any).
- **FL1 — the window is set per frame by a rig-independent rule anchored on the frame's own preliminary ZP** (§5.1);
  not per rig from data, not ensemble photometry (§4.2). *Amended (F5):* the window stays anchored on the preliminary
  ZP; what it costs (κ, §4.2) is taken out on read by a stored slope (FL8), not by the window.
- **FL2 (replaced in F5) — a list star's flux is a plain aperture sum at its mapped catalogue position.** No fit of the
  star, no aperture correction. The sum is `star_measure`'s, beside the growth sum `c`'s numerator uses (the same module,
  pixel walk, mask and present-pixel rule; on a mosaic scaled by area, since a star that is not fitted has no model to
  phase-match), so there is still one star path. Fits are made only on the map stars (FL6). Was: Analysis' Moffat fit
  seeded at the WCS position × an aperture correction `c` over the list's own bright members (§4.5).
- **FL3 — it runs inside Analysis' read** of a solved frame and in a background pass after a solve batch; nothing
  reads pixels at launch, on a tier install or at the gate (§5.4).
- **FL4 — no per-star table**; `ZP_VERSION` 2 → 3, four columns added (§5.5). *Amended (F5):* the columns are §5.5's.
- **FL5 — the OSC spec:** O1 holds in substance (no Analysis number moves, `MEASURE_VERSION` stays 8); O2's
  phase-matched masked growth gives the reported `c` the mosaic anchor reads (the list itself has no `c`, FL2); O4's
  `is_mosaic` and O7 stay; O5, O6, the `cfa_aperture_*` columns and `AnalysisBeforeColour` are superseded by the ZP's own
  currency. 6.5's Z1–Z5 and C1–C8 stand, except C3's outcome: mosaics are measured again, judged by §6.
- **FL6 — positions come from a map of the list's own fits** (§5.2); a frame whose stars do not sit where the solve
  puts them is refused as `poor_solve`.
- **FL7 — the zero point is a weighted ratio of sums over the list**, not a median of per-star magnitudes; the list has no
  cap; its window edges are ramps; the sky under an aperture is the annulus median plus a pedestal measured on the frame
  (§5.3).
- **FL8 — κ is removed on read**: each frame stores its window's bright edge and its zero point's slope against a window
  shift; a reader reduces a frame set to one window before it compares frames (§5.6).

## 3. Goals, non-goals, anti-goals

- Goals: a stored per-frame ZP from a frame-independent list, mono and OSC mosaics; §6's bars met in the probe before
  anything is stored; the loss, the gate and the Transparency column read it unchanged.
- Non-goals: a colour term or synthetic catalogue magnitudes; an aperture correction of any kind (`c`, per band, or from
  a PSF model); a pixel-integrated PSF fit of the list; ensemble photometry; debayered or 3-plane colour files (keep
  `ColourCamera`); a Settings entry; hub and portal (same metric).
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
frame's saturation carries a seeing slope from churn alone, −0.029 / +0.034 mag/px: the 3c mechanism again. (F5 dropped
the cap, §5.1: on real frames it churned a sum.)

### 4.2 The brightness window

- **(a) Per frame, anchored on the preliminary ZP (chosen).** A stored per-frame value no other frame changes, so
  `transparency.rs`' reference-on-read stays. Cost: under a loss L the window follows the frame's light L mag brighter,
  and the offset depends on G (10-18: G 12.5–13 reads 18.42 against 19.13 at 13–13.5, red giants). Emulated ZP shift
  −0.008 / +0.005 mag at L = 0.5, +0.012 / +0.040 at L = 1: the loss reads up to 4 % small. Phase A measured more: a
  real change L reads (1 + κ)·L with κ −0.18 in LDN 1272 (reddened background stars), −0.12 ATR LDN 1272, −0.08 LBN 582
  (F4). FL8 takes it out on read (§5.6): after it κ's median is within ±0.022 in every field, its 10–90 % up to
  0.074 (QHY L).
- **(b) Per rig, fixed from data.** Removes that cost but needs a stored window per rig and exposure, frozen by the
  first frame measured (order-dependent; two machines differ under sync) or chosen on read (a moving window turns
  stored ZPs stale, and re-measuring costs a file read). Rejected.
- **(c) Ensemble, each star against its own reference.** Churn stops mattering (3c same stars: 0.0090 / 0.0140), but
  it needs a per-star table (~1 000 rows a frame beside `star_metrics`' 500), a per-star reference on read for every
  table, and loses the rig-wide fallback across targets (6.5: 0.025). The fixed list holds churn to ≤ 0.0016 anyway.

### 4.3 The measurement

| At the catalogue position | Evidence | Verdict |
| ---- | ---- | ---- |
| Moffat fit seeded there, centre free, × `c` | 3c same stars: rms against the stack 0.012 / 0.013, FWHM slope +0.036 / −0.013 mag/px | chosen in r0 (Tasks F1–F3); failed Phase A (§4.5) |
| Fixed-position, fixed-shape amplitude fit | a position error δ costs `exp(−δ²/4σ²)` of the flux: 1.3 % at 09-13's 0.12 px median match (σ 0.53 px), 3.5 % at 0.2 px; the WCS changes at a flip; new rustafits code | rejected |
| Aperture of k·FWHM × `c_k` | a second photometry path; on the mosaic ±10 % lattice phase error per star at FWHM 1.25 px | rejected |
| Plain large aperture (6 F, per-phase annulus sky) | rms 0.009 / 0.020, slope +0.015 / +0.061; a detected neighbour within 8–20 px brightens a 6 F sum by 0.28–0.47 mag against the model (10-18) | rejected; the r0 probe's referee |
| **Plain aperture at the mapped position, radius `max(4·FWHM_ref, 2·F)`, per-phase annulus sky plus a frame pedestal, a weighted ratio of sums** | F5 emulation on run g0 (nine sets), judged by the probe's verdict: B2 0.0019–0.0097, B3 rms 0.0073–0.0114, B7 0.064–0.149 px; misses B4 on LBN 582 10-18 by 0.0014 and B5 on IC 5146 H 10-04 with the rig's k | **chosen (FL2, FL7)** |

Measured in F5 (run g0, nine sets): the Moffat flux × `c` fails where `c` is short (FWHM ≫ FWHM_ref) or the fit refuses
(undersampled mosaic); an aperture of k·F reads +0.06…+0.13 mag/px on the QHY nights; a radius fixed by the rig with a
frame pedestal passes. The lattice phase error of a green-site sum is a per-star scatter, unbiased over the list. Also
rejected by measurement: a fixed 6 px radius (LBN 582 09-13 −0.039 mag/px); the median of per-star magnitudes (on a fixed
window the same aperture fluxes give B3 rms 0.016–0.019 as a median, 0.008–0.011 as a ratio of sums, 09-13); the annulus
median alone as the sky (it leaves a pedestal under the aperture: −4.4 % of the pixel noise per px on LBN 582, −1.8 % on
the Duo, +10…+14 % in IC 5146, which made k depend on brightness). `c` per band and a pixel-integrated fit were not needed
and not built.

### 4.4 Where it runs

Inside Analysis' read (chosen: plane in memory, the budget gives the anchor, one path). The solver reads its file inside
`solvemyastro` and hands back no pixels, so a solve batch's frames are measured afterwards by the Analysis entry. A
launch pass would read every solved light (1 352 OSC lights alone in the release catalogue, many on network shares).

### 4.5 Phase A of the Moffat × `c` list, and the re-audit

The list as first built (Moffat fit at the SIP position × a `c` over ≤ 256 of the list's kept fits; rustafits 45236db,
30fd4e7; core 1bd79263, c7cf5c9c; probe 3134eb8a) was judged on the nine sets (`out/f0/`, 933 frames) and stopped:
STOP(missed B1, B3, B5, B7, B8), no constant buying a class pass in four tuning rounds (`out/f1–f3/`). Causes, on the frames
(`task-F4-report-r0.md`, `out/f4/`): B1 (ATR 10-17 min 0.616) and B7 (QHY L 0.55–0.59 px) are bad stored solves and a smooth
WCS scale term the list's positions inherit (a quadratic re-map leaves 0.06–0.09 px); B3 on undersampled Duo 09-13 (rms
0.0163), B3 on LBN 582 10-18 (0.0233) and B5/B8 on QHY L (−0.032…−0.047 mag/px) are one `c` per frame against a seeing term
that depends on brightness (FWHM < 1.4 px on green sites) or is short where FWHM ≫ FWHM_ref; B3 on Duo 10-18 is the anchored
window's κ at the flip (−0.18, against §4.2's ≤ 4 %); QHY H is off-axis stars on unflatted frames plus B5's shared k. The
third fix round in the subsystem became a re-audit of the model (F5, `task-F5-report-r0.md`, `out/g0/`, `out/f5/`): positions,
the flux estimator and κ, measured by emulation on the job's data; FL2, FL6–FL8 and §5 are its outcome.

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
  `FWHM_ref` (Moffat β 4) at the anchor; the faint edge `G_f` is `G_b + 3`, deepened by catalogue count up to `G_b + 4.5`
  while the region holds fewer than 200 isolated stars. Here `G_b` ≈ 12.5–12.7, 1.0–2.2 mag below the real 70 % level.
- **Window weights**: a star's weight is `w(G) = clamp(min((G − G_b)/ρ, (G_f − G)/ρ), 0, 1)`, ρ = 0.5 mag. A star
  entering at an edge enters with weight 0, so a ZP changes continuously as the window moves.
- **Region**: inside 0.9 of the half-diagonal (6.5) and at least `13·FWHM_ref` from every edge (the map fits' ring sky
  fits; a list star whose aperture annulus leaves the frame is no member, §5.3).
- **Isolation, by the catalogue and by the frame's own detections**: no neighbour with ΔG ≤ 4 within `8·FWHM_ref` (was 4: the aperture and its
  pedestal radii reach `6·FWHM_ref`), no star brighter than `G_b − 2` within `10·FWHM_ref` (halo, bleed, spikes).
  Contamination goes with distance over PSF width; `FWHM_ref` is arcsec on a well-sampled rig, pixels at the sampling
  floor, and never moves with seeing, so isolation never churns (6.5's 3 × the frame's FWHM did). ΔG 4 is 2.5 % of the
  light.
  The image carries what the catalogue lacks (G7, from G6): the tiers drop every Gaia source with RUWE ≥ 1.4 at ingest
  (`gaia_bulk.rs`, `GAIA_RUWE_MAX`), a position-quality cut that is wrong for a neighbour test, and they end at each
  cell's tier depth (G 14.4–17.4 here); 7.6–10.9 % of a frame's list weight sat beside such a star (G 10–13, RUWE
  1.5–21 on the nine checked), lifting a field's level by +0.018 (vdB 152) … +0.045 (Duo LDN 1272), a level and no
  seeing slope. So a list star is also dropped when a **detected seed** that is not its own lies within
  `8·FWHM_ref` with `G_seed ≤ G + 4` and a peak of at least 10 σ_sky (`detected_neighbour_min_peak_sigma`, the plane's
  noise): the catalogue rule's constants, asked of the image. A star's own seed is the nearest one within
  `max(1.5 px, FWHM_ref)` of its projection (`own_seed_radius_px`). A seed's magnitude is its flux read through the
  frame's own calibration `k`, the median of `flux·10^(0.4·G)` over the shallow-window candidates (the catalogue rule's
  survivors with `G ≤ G_b + 3`) that have exactly one seed within the own-seed radius, that seed being nearer to no other
  such candidate: `G_seed = −2.5·log10(flux / k)`. Fewer than 20 such pairs (`seed_calibration_min_pairs`) skip the rule
  for the frame with a `warn!`, never a refusal; the row's `seed_calibration_pairs` says how many there were and
  `detected_neighbours_removed` how many candidates this rule removed that the catalogue rule had kept. The rule runs
  before the faint edge's count and the cap, so the window deepens on the stars that are left. It needs the plane's
  matched-filter seeds (`PlaneMeasurement::detected`): a mosaic hands them to its fit as given seeds, so rustafits lends
  the native ones to the first callback (G8; the returned measurement is unchanged): in the frame's own pixels, each peak
  the green-sum peak halved (about 0.8 of the star's brightest green site above the sky, on the scale of `noise.sigma`),
  each flux the green sites' own sum (the calibration `k` absorbs the scale). The `G_b − 2` halo rule is mirrored too (G8):
  a seed with `G_seed ≤ G_b − 2` within `10·FWHM_ref` drops the star (the catalogue rule's `bright_dmag` and
  `bright_radius_fwhm`), whatever its peak. A plane that handed no seeds skips the rule with a `debug!`.
  A judged round (g2) compares the list with and without it (`--detected_neighbours false`).
- **No cap.** The hash cap churned a sum (cap 1 000: B2 0.0163 on 09-13; no cap 0.0077; ramped edges 0.0047). A guard of
  12 000 stars stays, by the same hash of the catalogue record (G, position at 1″): above it a star stays when its key
  falls under `12 000 / n`. No rig here reaches it; unjudged.
- **Map stars**: the list stars with `G ≤ G_b + 2`, at most 400, lowest keys first.

### 5.2 The measurement (rustafits PR and submodule bump, R-OSP-19)

One entry beside `analysis_mode::measure_raw_frame` reads the frame once and measures it exactly as today; two callbacks
of the caller then drive the list on the same plane. rustafits measures; the caller (core, pure) decides:

1. the first callback builds the list from the measurement (§5.1) and asks for the **map stars**; rustafits fits them
   seeded at their projections (the list path: the plane's β and field width, the saturation verdict with the plane's
   clip floor, the detected neighbours kept out of the ring sky, no aperture correction);
2. the second callback gets those fits; core builds the **map**: over the fits accepted, unsaturated, with SNR ≥ 10 and
   within `2·FWHM_ref` of their seeds, `fitted − projected` per axis as a full quadratic of the frame position, three
   rounds of 3 σ clipping (1.4826·MAD of the residuals, floor 0.15 px); fewer than 30 members → `poor_solve` (on a sparse
   list see §5.3). Every list position is moved by it;
3. rustafits fits the map stars once more at the moved seeds. **Gate** (core): fewer than 75 % of them kept (accepted,
   unsaturated, within `max(1 px, 0.5·FWHM_ref)` of the moved seed) → `poor_solve` (the eight bad frames of the corpus
   0.31–0.64, every other frame ≥ 0.86). One re-seed: a second changes nothing (F5);
4. rustafits sums **every list star** at its moved position, no fit: radius `R = max(4·FWHM_ref, 2·F)`, `F` the frame's
   Analysis median FWHM (8 px here; wider only above F = 4 px); a pixel on the edge counts by its 4 × 4 sub-sampled share;
   on a mosaic the green sites only, each green phase less its own sky, the sum scaled by the disc's area over its green
   area. Sky per star: the grouped median (of integer data) of an annulus of 2 000 px starting at
   `max(4·F, 6·FWHM_ref) + 2 px`, per phase. Returned per star: the sums at `R`, `3·`, `4·` and `6·FWHM_ref`, the sky, the
   annulus scatter, the highest pixel within `1.5·FWHM_ref`, and whether the annulus stayed on the frame; with them the
   plane's clip floor in the same units.

The Analysis measurement stays bit-identical with and without a list (B9, the rustafits test). Cost: no fit of the list,
≤ 400 map stars fitted twice, one aperture and annulus per list star (2 000–9 000 here); estimated 20–70 ms a frame on the
pool against the r0 list's 35 ms (an estimate: Phase A times it, §6).

### 5.3 Statistic and validity

Members: the list stars in the window whose annulus is on the frame and whose highest pixel is under the plane's clip
floor (any peak when the plane shows no clipped evidence).

- **Pedestal** ε: over the members (every weight), `D = S(6·FWHM_ref) − S(4·FWHM_ref)` against `S(3·FWHM_ref)`, a
  straight line with four rounds (`pedestal_clip_rounds`) dropping the residuals beyond 3 σ about zero, σ = 1.4826 × the
  median deviation of the residuals from their median (G2 review M1: the clip is about zero, as emulated, not about the
  median residual; four rounds stop unconverged on most QHY L and ATR frames, but ten move the ZP ≤ 0.005, G4);
  the line is refitted after the last round, where the emulation keeps the fit from before its last clip (one rank step
  apart; the kept share and the collapse to `None` are `pedestal_kept_share` in the row, G3); ε = intercept /
  (π·(6² − 4²)·FWHM_ref²). Each flux is
  `f = S(R) − ε·π·R²`.
- **Screen**: `zp_i = G + 2.5·log10(f / t)` over the positive fluxes gives a 4 σ-clipped median `m` and `σ` (today's
  code). A member is dropped when `f + 4·n` is below `t·10^(0.4·(m − 4σ − G))`, with `n = sky scatter·√(π·R²)`; a
  negative flux inside its noise stays. The upper rule is a taper (G2 review H1): a member with `zp_i > m + 4σ` keeps its
  own flux and gets the weight × `clamp(1 − (zp_i − m − 4σ)/(τ·σ), 0, 1)`, τ = `screen_taper_sigma`, **default 0: a
  plain drop** (G4/G7: the taper's level and rms against the drop are equal on 14 of 15 nights and worse on the
  thinnest, Duo 09-13 B2 0.0049 against 0.0034, B3 slope −0.0354 against −0.0238, so the drop, as emulated, ships; the
  taper stays reachable by `--screen_taper_sigma 1`; a NaN τ is a start-up error of the params,
  `ForcedListParams::validate`, never a stored `too_scattered`); a member whose weight reaches zero is dropped and not
  counted. A star that the jitter of `m` and `σ`
  carries across the limit then moves the ZP continuously (a plain drop does not: on the Duo's LDN 1272 ten stars near that
  limit, in some frames and out of others, moved the ZP 0.006–0.009 mag each). The first answer, counting such a member at
  the limit `10^(0.4·4σ)` times its expected flux (F5 plan review M1), is **withdrawn**: measured on g0 it biased the ZP
  by +0.006 (QHY H) to +0.026 (ATR) per set against the drop, 0.0012–0.0044 rms frame to frame (up to 0.045), following σ
  (r +0.7 on ATR), the very figure B3 and B5 judge. The taper measured +0.001…+0.007 in level, 0.0004–0.0028 rms (max
  0.021), with the pair rms at or under the better of the drop and the cap. The window slope and `zp_stars` use the
  tapered weights.
  On a mosaic the area-scaled sum has √2 times the noise of the same disc read on a mono plane (the green sites are half
  the disc's area), so the low rule's allowance `4·n` is √2 tight there; the emulation has the same property and the code
  does not correct it (G2 review L6; G4 reports it).
- **Zero point**: `2.5·log10( Σ w·f / (t·Σ w·10^(−0.4·G)) )` over the members left; `Σ w·f ≤ 0` refuses
  `too_scattered`.
- **Window slope**: `s = 1.0857·[ (Σ_faint ramp τ_i·f − Σ_bright ramp τ_i·f)/(ρ·Σ w·f) − (the same with τ_i·10^(−0.4·G)) ]`,
  the ramps the members within ρ of each edge (window weight under 1; `τ_i` is the member's screen taper, 1 for all of
  them at the default τ = 0): the ZP's change per magnitude the window moves fainter. When the
  catalogue ends before `G_f − ρ` the faint ramp is empty and its term is zero, since that edge does not move (QHY L: the
  catalogue ends at G 15.20 in a window `[14.06, 17.06]`, the faint ramp empty in all 295 frames).
- Valid with ≥ 50 members of non-zero weight and `σ ≤ 1.0` mag; `zp_stars` counts those members, `zp_sigma` is `σ`.
  Refusals: `no_exposure`, `no_anchor`, `poor_solve`, `too_few_stars` (with the list size: a shallow catalogue or sparse
  field is told from a measurement that failed), `too_scattered`; stored besides (plan, Review rulings) `colour_camera`,
  `unreadable_solve`, `frame_verdict`. `no_aperture_correction` goes.

**On a sparse list** (under ~500 stars; no corpus set is one, so this is unjudged, and Phase A's decimated round reports
it, §6):

- **the map**: when the list holds fewer than 40 map stars (30 / 0.75), a map of fewer than 30 members is
  `too_few_stars`, not `poor_solve` (a sparse field, not a bad solve); from 40 map stars on, §5.2's rule. At 40 map stars
  a good frame at 0.86 kept falls under 0.75 in about 2 % of draws and a bad one at 0.64 passes in about 7 % (3 σ between
  them asks about 100): 40 stands on zero false refusals in Phase A's decimated round (M5, §6).
- **the pedestal** is fitted over every member whatever its weight and is never replaced: its error on the ZP is about
  0.03 / √n mag at n members (an estimate at the corpus' sky noise: 0.004 at 50), inside B2, so it needs no fallback
  above the validity floor. A fit left with fewer than 20 points after its clipping, or with no spread in
  `S(3·FWHM_ref)`, refuses the frame `too_scattered`: there is never a ZP with ε = 0, which would carry the annulus bias of
  §4.3. Phase A gives per set the rms of the reduced ZP's decimated − full difference against n, and the pedestal's part.
- **the window slope** needs at least 10 members on the bright ramp (the faint ramp's term is taken as it is, zero on a
  window cut by the catalogue, above); with fewer the frame stores no slope (`zp_window_slope` NULL) and its ZP stays
  valid. On read it is reduced with its frame set's median slope (§5.6); a set in which no frame has a slope reads its
  ZPs unreduced (`s̄` = 0). Phase A gives the share of frames with a slope per set and round.
- **the faint edge** deepens to `G_b + 4.5` as before (unjudged); where it is set by count the slope's faint term is
  wrong (that edge moves by count, not with the window), which Phase A's anchor-shifted re-measures measure (§6).

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

- `ZP_VERSION` = 3. `frame_analysis` keeps `zero_point` (the aperture sums' working units per second), `zp_stars`
  (members), `zp_sigma`, `zp_version` (3), `zp_catalog`, and gains `zp_refusal TEXT` (the refusal code, NULL on a valid
  ZP; plan ruling R6), `zp_list_stars INTEGER` (the list before it is measured), **`zp_window_bright REAL`** (`G_b`) and
  **`zp_window_slope REAL`** (`s`; NULL when §5.3 gives none). `zp_aperture_correction` is not added. `CREATE TABLE`, the
  `ALTER TABLE` loop, `FrameAnalysis`, `ts_export.rs`, `src/types/models.ts`; personal sync carries them as appended
  optional fields (the analysis summary rides the package manifest as opaque JSON; no wire golden pin changes, plan
  Task 5a).
- Current = `zp_version` 3 and `zp_catalog` the installed identity; an analysis upsert or a new solve clears `zp_*` (a
  new WCS moves the list) and §5.4 fills them again. No per-star table: it would spare a file read only when the
  statistic alone changes, double `star_metrics`' rows, and not travel by personal sync.

### 5.6 Loss, gate, UI

- **Reduction on read** (FL8): for the frames of one rig, frame set and exposure (rounded to 0.1 s) holding a current
  ZP, `P` = median `zp_window_bright`, `s̄` = median `zp_window_slope` (over the frames that have one; 0 when none); a
  frame reads `ZP − s̄·(G_b − P)`. A single frame reads its ZP. The rig-wide fallback, keyed by rig and exposure, reduces
  each frame set and exposure within itself first. The exposure is in the key because `G_b` carries `+2.5·log10(t)`:
  120 s beside 300 s is a 1.0 mag window step, 60 s beside 300 s 1.75, while the linear reduction is measured to ±0.5
  only. The cost: a set shot at two exposures has two references, and the smaller may fall under the reference's
  minimum; Phase A gives the reduced ZP's residual at anchor shifts of 0.44, 1.0 and 1.75 mag so the split can be lifted
  later. Stored values are never rewritten. `Transparency::zero_point` carries the reduced value, so a row's ZP and its
  loss are one quantity (shown stored, two frames could read their ZPs in one order and their losses in the other, by
  `s̄·ΔG_b`, 0.05 mag on the corpus); the cell drops its `± zp_sigma`, now the field's per-star scatter (0.13–0.59 mag)
  beside a ZP good to 0.005. Checked against a ZP measured in a fixed window: rms 0.0008–0.0014 mag (max 0.004); κ after
  it, by a 0.5 mag window move, has its median within ±0.022 in every field (was −0.18) and its 10–90 % within 0.042 on
  the Duo's LDN 1272 and 0.057–0.074 on QHY L (where the reduction turns a raw +0.003…+0.011 into −0.016…−0.022); the
  10-18 flip step then reads 0.138 / 0.159 against the stack's 0.139 / 0.167.
- `transparency.rs` keeps its arithmetic (rig, frame set, 90th percentile, 30 frames, 2 nights, fallback), its pools
  keyed by exposure as well, reads only version-3 ZPs, reduced as above; mosaics join the references (`is_mosaic`); other
  colour files keep `ColourCamera`.
- `NotMeasured` says "transparency not measured yet — run Analyze on its frame set"; `NoStarMetrics` goes (no stored
  star rows are read); a refusal says why in one sentence: `poor_solve` reads "the stars are not where the plate solve
  puts them — solve this frame again" (the advice stands only if a new solve of a refused frame measures; refused again,
  the sentence ends before it, M5, §6); too few catalogue stars, install the deeper tier, when the list was small. Gate: O7
  stays; a frame without a current ZP gets no verdict and is not blocked. New fields and codes go into the logging
  dictionary.

## 6. Truth check — bars fixed now, judged in the probe before anything is stored

`zp_probe forced` runs §5.1–5.3's shipping functions on frames read in memory and writes TSVs and a verdict, no
database. Tiers 500 + 2000, all solved in the dev catalogue. OSC: LDN 1272 2025-09-13 (55) and 10-18 (105), common sets
of stacking runs 10 and 9; vdB 152 2025-08-22 (100), 08-23 (116), 10-24 (45). Mono: ATR2600M L on LBN 582 2025-09-13 and
2025-10-18 (139 lights; 10-18 holds stacking run 9's mono common set, 82 frames) and on LDN 1272 (65 lights with an empty
`FILTER`, night key 2025-10-17, no common set); 6.5's QHY268M L (IC 5146, LDN 1042, LDN 1031) and H (IC 5146, LDN 1042).
Nights of ≥ 30 frames are judged, pairs ≤ 15 min apart. The sky moves 0.0032 / 0.0041 / 0.0036 frame to frame on the
common-set nights; 6.5 put it at 0.013–0.017 on its mono nights (no common set). A missed bar stops the build; numbers to
owner.

| Check | Bar | Establishes / cannot establish |
| ---- | ---- | ---- |
| B1 members of non-zero weight left by the screen that a frame shares with its successor | every pair ≥ 80 % | the 3c mechanism is gone (today 5–7.5 %); not that the ZP is right |
| B2 stability, median \|ΔZP\| | ≤ 0.015 on the common-set nights; elsewhere ≤ 0.020 and not above today's (vdB 0.021–0.024, 6.5 L / H 0.024) | frame-to-frame noise; not a slow drift or a constant bias |
| B3 ZP − 2.5·log10(t_flux / t), common set | rms ≤ 0.015; FWHM slope within ±0.04 mag/px | agreement with an independent calibrated photometry; not seeing independence below the ~0.04–0.06 mag/px spread of the references' own unconverged apertures (3c) |
| B4 ZP = a + kX (+ s·[after flip], reported), per night | \|k − k_stack\| ≤ 0.02 where ΔX ≥ 0.4, one pier side at a time (F7): the ATR2600M L common set (LBN 582 10-18) and the Duo's 10-18 after the flip (before it ΔX is 0.07) | the ZP follows real light loss in size; not haze or cloud |
| B5 no common set: one night at a time, `zp = a + k·X + s·F` (+ a side step on a two-sided night) | \|s\| ≤ 0.03 mag/px where its error ≤ 0.015 | no seeing term on the vdB and 6.5 nights; not below the error |
| B6 coverage: valid ZPs of the solved, analysed frames | ≥ 90 % per night; 6.5's sets at 6.5's 100 % | the rule works on real frames; not accuracy |
| B7 the map stars' median \|fitted − seed\| after the re-seed | ≤ 0.25·FWHM_ref | the solve, its map and the seeding agree; not the flux |
| B8 mono not worse: seeing slope with airmass, per night as B5 | ≤ 0.03 and ≤ \|today\| + 2σ; L and Duo cross-target spreads reported as decision rows (not a STOP) | no regression on the shipped classes |
| B9 the Analysis measurement with and without a list | every `StarFit`, `FrameSummary`, `PlaneMeasurement` field bit-identical | no Analysis number moves (rustafits test + probe) |

A bar judged on no night of its class is a STOP (no silent pass); B1 counts pairs where both frames are valid; B2–B5 and
B8 are judged on the reduced ZP (§5.6), per rig, object (the probe's frame set) and exposure; B3 is judged one pier side
at a time on a two-sided night (F7) and prints three FWHM slopes, ZP − stack, ZP − 6r and 6r − stack (6r: the same ratio
of sums at `6·FWHM_ref` with its pedestal on the same members), a slope beyond ±0.04 against the stack being no pass
whether or not 6r echoes it (M2 below); B6 exempts frames with a plane verdict (plan, Review rulings).

Referees (the controller's rulings after F5, `progress.md`; no threshold moved):

- **RR1** — B5 and B8 fit one night at a time, `zp = a + k·X + s·F` jointly; `s` is judged with its error from the joint
  fit, unjudged above 0.015 as written. Extinction is a night's property (IC 5146 H 10-04: k −0.074 against the rig's
  −0.114; with the night's own k the slope is −0.0004 ± 0.0032, with the rig's +0.060 ± 0.010). If wrong, a real seeing term
  hides in k on a night where FWHM tracks airmass: then its error grows, the night is unjudged, and a class judged on no
  night is a STOP.
- **RR2** — B4 gets no allowance for the stack's own error (that would move a threshold). A miss inside Δk's own standard
  error goes to the owner as a stated limit, with its numbers.
- **M2 (plan review)** — 6r is not independent: it shares the list, positions, annulus, pedestal, screen and weights and
  differs by the light between 4 and 6·`FWHM_ref` only (above F = 4 px, `R = 2F` nearly reaches it), so B3's slope could
  pass on the referee alone (the Duo's 10-18 sat at −0.0376 of 0.04 in the emulation). A slope beyond ±0.04 against the
  stack that 6r does not echo is no pass: it goes to the owner as a stated limit with the three slopes (RR2's rule).
- **M5 (plan review)** — the 40 map stars of a sparse list (§5.3) stand on zero false `poor_solve` refusals in the
  decimated round (a frame valid in the full round refused in the decimated one); any goes to the owner with the round's
  figures (RR2's rule). `poor_solve`'s advice to solve again stands only if the acceptance's new solve of a refused frame
  measures; refused again, its sentence says only that the stars are not where the plate solve puts them.
- **RR3** — B3's cross-check stays as written, on a repaired referee: the same sum at `6·FWHM_ref` (the r0 6F referee's
  aperture outgrew the list's isolation); M2 sets what its echo means.
- **RR4** — Phase A again takes an identity pass (B9) and a decimated-list round (every 10th list star by its key: the
  sparse field no corpus set has; reported, not judged); the list's cost above 70 ms a frame is a STOP; every run goes
  through the job's memory guard.

Reported, not judged: the budget's reported `c` (the mosaic anchor); list size, map figures, pedestal, window slope, the
members the screen tapers (H1, per night) and refusals per frame; on every tenth frame, re-measures
with the anchor shifted by ±0.1 and ±0.5 mag (the analytic slope against the re-measured one, κ after the reduction) and
by ±0.44, ±1.0 and ±1.75 mag (the reduced ZP's residual at an exposure step, §5.6); the decimated round (§5.3's figures:
map stars, the first pass's member share, the refit kept share, false `poor_solve`, the corpus' eight bad frames, the
reduced ZP against the full round's with its pedestal part, the share of frames with a slope); one solved set outside
the nine; the cost. Unjudged (no data in the corpus): a window move beyond 0.5 mag within a set (the corpus' real moves
reach 0.14 mag, its haze dip 0.09; F5 measured the reduction at ±0.5 by moving the window, Phase A by moving the anchor;
a cloudy night is owed), where the linear reduction is unverified; a window cut by the catalogue's depth (QHY L: 1.1 mag
wide, 64 % of members on the bright ramp); a list under ~500 stars (§5.3); under 16 bit, long focal length, high
latitude; the faint-edge extension.
Acceptance on a catalogue copy (headless web): a solved set analysed stores version-3 ZPs; a set solved after analysis
fills in by the solve pass; an offline file says "run Analyze"; a frame refused `poor_solve` says to solve it again, and
solved again measures or loses that advice (M5); an existing stack is bit-identical and recomputes no cached stage; a
low-altitude frame reads its loss; a rig without a reference is not blocked and says why.

## 7. Order of work

1. rustafits: §5.2's entry and identity test (PR, `CHANGELOG.md`, no trailer; submodule bump). 2. Core, pure: anchor,
list, statistic, synthetic tests. 3. `zp_probe forced` and its verdict. 4. Phase A: STOP (§4.5). 4a. The model re-audit
(F5): this amendment. 4b. The plan's G1–G4: the rustafits list entry v2, core pure (map, gate, pedestal, statistic,
slope, the reduction), the probe and runner, Phase A again: GO or STOP. 5. Storage, currency, triggers, removals. 6. Loss
reasons and the reduction, gate, the tab's count, the cell. 7. Acceptance. 8. Docs (`docs/catalog/README.md`, `CLAUDE.md`'s
Transparency line, `docs/logging/`, `open-items.md`, pointers in the 6.5 and OSC specs), final review, merge into local
`main`; 6.5 and this push together on the owner's word.

## 8. Owner answers (2026-11-16) and what is still open

- **F6 — existing frames: no migration** («Не нужна миграция, достаточно сказать что анализ отличается. В лайве ещё
  нет проектов больших»). Nothing reads files at launch, after a tier install or at publication: an old frame's cell
  says its analysis differs from the current one ("run Analyze"), and Analyze re-measures it. Publication does not
  measure: a frame without a current ZP is published with no transparency verdict and is not blocked (as today).
- **F7 — the 10-18 meridian-flip step** («Без понятия, возьми другие кадры»): its cause is unknown, so it is not a
  judge of the method. B4 is judged on the ATR2600M L common set and on 10-18 one pier side at a time where a side has
  ΔX ≥ 0.4; the flip step `s` is reported, not judged.
- **F8 — dual-band data for the OSC spec's §8 guard** («Squid и некоторые veil снимались с узкой полосой»): the
  ASI2600MC Pro Squid set and part of the Veil sets were shot through a dual-band filter, the rest of that rig without.
  The guard is not built in this cycle; O7 keeps the gate off OSC frames with an empty `FILTER`. Those sets (none
  solved today) are the guard's data when it is built.
- Still open: the `RELEASE_NOTES.md` line (proposed at the close, applied only on the owner's yes).

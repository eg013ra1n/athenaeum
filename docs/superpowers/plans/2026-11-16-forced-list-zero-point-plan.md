# Forced-list zero point — plan

Spec: `docs/superpowers/specs/2026-11-16-forced-list-zero-point-design.md` (rulings F0–F8, FL1–FL8, design §5, bars §6
with the referees RR1–RR4, order §7). It replaces Tasks 4–8 of `2026-11-16-osc-zero-point-plan.md`; that plan's Tasks 1–3
stand and are reused.
**Status (2026-11-16).** Tasks 1–4 are done with the r0 model (a Moffat fit × `c` per list star); Phase A stopped (Task 4,
`task-F4-report-r0.md`) and the model re-audit (F5, `task-F5-report-r0.md`) amended the spec (§2 FL1–FL8, §4.3, §4.5, §5,
§6). The section **Model re-audit (F5): tasks G1–G4** below rebuilds the model before Task 5; each G task says what of Tasks
1–3 it keeps, changes or removes. Tasks 5–8 are amended in place for the new model and start only on G4's GO. The plan
review of that amendment (fable) is folded into the tasks; where each finding landed is under Review rulings, "F5 plan
review".
Worktree `.claude/worktrees/osc-zp`, branch `collab-v3-wave6-5-osc`; rustafits branch `osc-green-aperture` (head
30fd4e7, the list entry and its fix round) inside the worktree's submodule, recorded by the branch since 9f0acc66
(rustafits `main` is d2c5775, 1.8.0). rustafits commits carry no trailer (its own rule); Athenaeum commits carry
`Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
Ledger (main checkout): `.superpowers/sdd/2026-11-16-osc-zp/progress.md`. Job data: `~/.athenaeum-acc/2026-11-16-osc-zp/`
(REGISTRY row, expires 2026-12-07; the catalogue copy `catalog/athenaeum.db`, the common sets `out/common-merged.tsv`).
One implementer at a time (16 GB Mac); agent builds use `CARGO_TARGET_DIR=<main checkout>/target-agent`. Skill
`lean-execution` governs the run: a fresh agent per phase, short reports in the ledger folder, disk rule §7.

| # | Task | Model | Gate |
| ---- | ---- | ---- | ---- |
| 1 | rustafits: the list entry beside `measure_raw_frame` (R-OSP-19) — done (45236db, 30fd4e7) | rust-engineer sonnet | rustafits tests (default, `debug-pipeline`, `serde`); publish dry-run; core `cargo check --all-targets` |
| 2 | Core, pure: anchor, window, list, statistic, the composition — done (1bd79263, c7cf5c9c) | rust-engineer sonnet | `analysis::zero_point` and `analysis::analyzer` tests; 6.5's pure tests unchanged |
| 3 | `zp_probe forced`, `verdict --forced` (B1–B9), runner — done (3134eb8a; F5's emulation modes 7d26e764) | rust-engineer sonnet | example builds and tests; the 6.5 and r1 verdict regressions |
| 4 | Phase A: GO or STOP — done: STOP (F4) | sonnet runs, opus judges | §6 bars on every judged night; controller recomputes B1, B2, B6 from the TSVs |
| G1 | rustafits: the list entry v2 (map fits, `place`, aperture sums; no `c`) | rust-engineer sonnet | rustafits tests (default, `debug-pipeline`, `serde`, `test-support`); publish dry-run |
| G2 | Core, pure: map, gate, pedestal, statistic, window slope, the reduction; the pointer bump | rust-engineer sonnet | `analysis::zero_point`, `analysis::analyzer` tests; core `cargo check --all-targets`; the probe's tests |
| G3 | The probe on the new composition, `verdict --forced` with the reduction and RR1–RR4, the runner | rust-engineer sonnet | example builds and tests; the 6.5 and r1 verdict regressions; a guarded smoke |
| G4 | Phase A again: GO or STOP | sonnet runs, opus judges | §6 bars on every judged night; identity pass; decimated round reported; cost ≤ 70 ms; controller recomputes B1, B2, B6 |
| 5a | Storage: four columns, the stored figures, sync, TS | rust-engineer sonnet | `db::analysis`, `sync::ingest`, `ts_contract`; `cargo check --workspace --all-targets` |
| 5b | Analysis writes the zero point; `ZP_VERSION` 3; every 6.5 writer removed (both hosts) | rust-engineer sonnet | `analysis`, `api::analysis`, `api::collab` tests; workspace check; grep of removed names |
| 5c | The pass after a solve batch | rust-engineer sonnet | `api::analysis`, `api::plate_solve`, `log-mcp` tests; workspace check |
| 6a | Loss reasons, the reduction on read, mosaic references, the gate's O7 | rust-engineer sonnet | `analysis::transparency`, `collab::gate`, `api::collab`, `api::frame_sets` tests |
| 6b | The Analysis tab's count | frontend-dev | `npm run build`; vitest |
| 7 | Acceptance on a catalogue copy (headless web) | sonnet | the Task 7 checks |
| 8 | Docs, final review, merge | controller + fable reviewer | full core run; workspace check; build; vitest; rustafits tests |

Each task: a review (opus; fable on round 2+), the controller's own re-gate (build and tests re-run, never taken from
the report), the ledger line. Tasks 5–8 start only on Phase A's GO (G4).

## What exists, what is reused, what is dropped

- **Reused from the OSC plan (done).** rustafits Tasks 1 / 1b: the masked, phase-matched green-site growth
  (`photometry::growth_curve_in` with `masked`), the reported mosaic `c` (`PlaneMeasurement::reported_aperture_*`,
  `measure_plane_reporting_aperture`, `CFA_APERTURE_VERSION` 2). Here it gives the mosaic anchor (§5.1); the list has no
  `c` since F5 (FL2). `CFA_APERTURE_VERSION` stays rustafits' version of the reported number; nothing in Athenaeum stores
  it. The probe (Tasks 2 / 3c): the
  `measure` columns `c`, `c_stars`, `c_se`, `airmass`, `zero_point_c`, `rig`; `airmass`, `least_squares`,
  `shared_extinction_fit`, `osc_nights`, `CommonRow`, `common`, `bigap`; the runner pattern `run-osc.sh`.
- **Reused from 6.5.** `measure_zero_point` (now the anchor), `catalog_for_frame`, `catalog_cone`,
  `zero_point_catalog`, `set_zero_point`'s compare-and-set, `clear_zero_point` on a new or deleted solve,
  `transparency.rs`' arithmetic (rig, frame set, 90th percentile, 30 frames, 2 nights, fallback), the gate metric
  `transparency_loss`, the manifest key `transparencyLoss`, the three registry copies (unchanged: hub and portal are
  not touched).
- **Built now, named by the OSC spec but never built.** `is_mosaic(bayerpat, source_channels)` (O4) and the gate's O7.
- **Dropped (never built).** The `cfa_aperture_*` columns, `needs_reanalysis`, `AnalysisBeforeColour`, O5's
  version-outdated rule, O6's "no `ZP_VERSION` bump"; the OSC plan's Tasks 4–8.
- **Removed from 6.5.** The half of `analysis/zero_point.rs` that measures from stored star rows, the triggers after an
  analysis and a solve, the launch and tier-install backfill, the gate's lazy refresh, `NoStarMetrics`.

## Constants (one place, `zero_point::ForcedListParams::default()`)

As amended by F5 (G2 sets them; the r0 set is in git at 1bd79263 / c7cf5c9c). Field names in brackets where new or changed.
`FWHM_ref = max(2 px, 2.5″ / scale)`; anchor: 6.5's matcher with at least 10 matches; bright edge `G_b` = 1.5 mag
fainter than a β-4 Moffat at `FWHM_ref` whose amplitude is 0.7 of full scale; faint edge `G_f = G_b + 3`, deepened up to
`G_b + 4.5` while fewer than 200 isolated stars; window ramps ρ 0.5 mag (`window_ramp_mag`); region 0.9 of the
half-diagonal and ≥ 13·`FWHM_ref` from every edge; isolation: no catalogue star with ΔG ≤ 4 within **8**·`FWHM_ref`
(`isolation_radius_fwhm`, was 4), none brighter than `G_b − 2` within 10·`FWHM_ref`; no cap, a guard of **12 000** by
record hash (`cap`, was 1 000). Map: the list stars with `G ≤ G_b + 2` (`map_window_mag` 2.0), at most 400
(`map_max_stars`); members accepted, unsaturated, SNR ≥ 10 (`map_min_snr`), within 2·`FWHM_ref` of their seeds
(`map_radius_fwhm`); up to three rounds (`map_clip_rounds` 3) of 3 σ clipping (`map_clip_sigma`) floored at 0.15 px
(`map_clip_floor_px`); at least 30 members (`map_min_members`); gate: kept refits ≥ 0.75 of the map stars
(`map_min_kept_share`), kept = within `max(1 px, 0.5·FWHM_ref)` (`centre_tolerance_px`, `centre_tolerance_fwhm`, as r0).
Aperture `R = max(4·FWHM_ref, 2·F)` (`aperture_radius_ref` 4.0, `aperture_radius_fwhm` 2.0; `F` the Analysis median
FWHM); pedestal radii (3, 4, 6)·`FWHM_ref` (`pedestal_radii_ref: [f64; 3]`, the probe's flag `a,b,c`), its line
clipped at 3 σ about zero (`pedestal_clip_sigma`; G2 review M1) for four rounds (`pedestal_clip_rounds`, a probe flag), at
least 20 points left
(`pedestal_min_points`); annulus 2 000 px (`annulus_area_px`) from `max(4·F, 6·FWHM_ref)` (`annulus_start_fwhm` 4.0;
the outer pedestal radius) plus 2 px (`annulus_gap_px`); peak radius 1.5·`FWHM_ref` (`peak_radius_ref`). Statistic: the
screen and its clip at 4 σ (`clip_sigma`, as r0, also the `4·n` noise allowance; its upper rule a taper of width
`screen_taper_sigma` σ, default 0 = a plain drop since G7, G2 review H1); ≥ 50 members of non-zero weight
(`min_stars`); σ ≤ 1.0 mag (`max_sigma_mag`); a window slope from ≥ 10 members on the bright ramp
(`slope_min_ramp_members`; the faint ramp's term is taken as it is, H1). Gone: `c_stars`, the list's
`max_std_error_mag` (`ZpParams` keeps its own for the anchor). Probe-only, the identity by default and never set by the
product: `decimate` 1 (a list star stays when `key % decimate == 0`, RR4's round) and `anchor_shift_mag` 0.0 (added to
the anchor before the window rule, so each edge follows its own rule as under a real loss: G3's re-measures, M3). The
probe may move a constant (never a bar); a moved constant is said in the Phase A report with its reason.

## Task 1 — rustafits: the list entry (R-OSP-19)

**Status.** Done: rustafits 45236db and its fix round 30fd4e7 (pointer bumps 6d70b6a6, 9f0acc66); reviewed APPROVE
(`task-F1-review-r1.md`). The text below is the r0 model; G1 replaces the list's `c`, the request and the result types
and adds the second callback and the aperture sums ("Of Task 1" in G1).

**Files.** `rustafits/src/star_measure/analysis_mode.rs` (`FrameOptions` :39–61, `measure_raw_frame` :63–71,
`measure_raw_frame_with` :73–114, tests :232+); `green.rs` (`measure_green_sites` :49–67, `measure_green` :69–149: the
native seeds :117, the field width :119–122, the fit options :124–143, the report branch :144–148); `mod.rs`
(`MeasureOptions` :249–314, `FitStarsOptions::correction` :495–517, `PlaneMeasurement` :532–644 with `timings` /
`sub_steps` :639–643, `measure_plane_in` :695–746, `prepare_passes` :1614–1660, `measure_budget` :1909–2147 — its
verdict pass :1972–1978 and `fit_after_passes` call :1985–2019 are the composition to copy — `saturation_plane`
:2168–2203, `fit_stars_at` :2205–2241); `fit.rs` (`Seed` :104–113, `StarFlags` :162–255, `StarFit` :270–345,
`ReportedAperture` :373–382, `FitOutcome` :383–440, `Correction` :1990–2034 with `report_aperture` :2029–2033,
`Neighbours` :2036–2043, `fit_after_passes` :2144–2165, `fit_stars_with_beta_diagnosed` :2279–2312,
`ring_and_correction` :2529–2650 with its report branch :2630–2647); `photometry.rs` (`CORRECTION_STARS` :618,
`CORRECTION_MIN_RATIOS` :620, `CORRECTION_MIN_SNR` :624, `aperture_correction` :1213 with its cap :1313 and its
`masked` argument); `CHANGELOG.md` (Unreleased :8–27). Athenaeum: the `rustafits` submodule pointer only.

**Approach.**
1. Split what `Correction::report_aperture` means today (masked growth AND not applied: `ring_and_correction` passes it
   as `aperture_correction`'s `masked` at :2632 and takes the not-applied branch :2634–2647) into two crate-private
   fields, `masked_growth` and `report_aperture`, and add `max_c_stars: usize` (default `CORRECTION_STARS`) passed to
   `aperture_correction` in place of the constant at photometry.rs:1313. The green path sets both flags as before;
   every other caller sets neither and keeps 64. No public type changes; every existing output is bit-identical.
2. `analysis_mode::measure_raw_frame_and_list<F>(frame, opts, pool, list: F) -> (PlaneMeasurement,
   Option<ListMeasurement>)`, `F: FnOnce(&PlaneMeasurement) -> Option<ListRequest>`. Step one is exactly
   `measure_raw_frame_with(frame, opts, pool, false)`'s body; on a mosaic a crate-private sibling of `measure_green`
   also hands back its native seeds (`measure_green_sites` unchanged). The callback runs once, after the measurement;
   `None`, an empty request, a trailed or defocused plane, or a mask mismatch measures no list.
3. Public types, `#[non_exhaustive]`, documented in the stable entry's terms (the module doc :1–23 gains a paragraph):
   `ListRequest { positions: Vec<(f64, f64)>, max_centre_offset_px: f64, max_c_stars: usize }` with
   `ListRequest::new(positions, max_centre_offset_px)` (cap 256); `ListFit { x, y, model_flux, flux, snr, flags,
   accepted, kept, centre_offset_px }`, one per position in request order (a refused seed: NaN numbers, both flags
   false); `ListMeasurement { fits, aperture_correction: Option<f64>, aperture_stars, aperture_se: Option<f64>, beta,
   ring_fwhm: Option<f64>, refused: RefusalCounts }`. Positions are native pixels in file row order.
4. The list fit uses the budget path's halves on the same plane, not `fit_stars_at`: that entry runs no saturation
   verdict (no `SATURATED` flag on given seeds) and masks no detected neighbour in the ring sky (`neighbours: None`,
   mod.rs:506), both of which §5.2 asks for. A working copy (`domain::to_working_units`); `saturation_plane` from the
   measurement's own noise and sky; `prepare_passes` over the list seeds at the measurement's field width;
   `fit::fit_after_passes` at `PsfModel::Fixed(pm.beta)` with the plane's mask (the green mask on a mosaic) and
   `Correction { saturation, neighbours, ring_fwhm: Some(pm.frame.fwhm_px), gain and noise as measure_budget sets them
   (:1979–2002), blend_refusal: true, masked_growth: mosaic, report_aperture: true, max_c_stars }`; no defocus or
   trail verdict on the list (the plane's own verdict stands).
   - Neighbours: every detected seed (mono: the matched filter's, `PlaneMeasurement::detected`; mosaic: step 2's native
     seeds) EXCEPT one within `max_centre_offset_px` of a list position, then the list positions. Without the drop a
     list star's own detection sits at distance ≈ 0 and fails `c`'s isolation test (`CORRECTION_ISOLATION_PER_FIELD_FWHM`
     in `aperture_correction`), so no list star would ever qualify for `c`.
   - A seed handed to the verdict pass: `peak` = the working plane at the nearest present pixel less the sky median
     (floored at 0), `flux` = `peak`; the fit reads the position.
   - Kept = accepted, not `saturated()` (the verdict pass's saturated and near-clip classes), centre within
     `max_centre_offset_px` of its position. No SNR cut.
   - `c` over the kept fits through `aperture_correction`'s own screens (isolation, footprint, clip, SNR ≥ 20), at
     most `max_c_stars`, masked growth on a mosaic; fewer than 5 → `None`. `model_flux` = the fit's model flux in its
     FWTM (the report path's `signal`); `flux` = `model_flux × c`, NaN when `c` is `None`.
5. One `debug` "list measured" (`count`, `kept`, `aperture_correction`, `aperture_stars`); a list `c` under 5
   qualifiers is a `debug`, never the per-plane `warn` (the caller stores the refusal).
6. `CHANGELOG.md` Unreleased, Added: the entry and its three types; nothing existing moves; `MEASURE_VERSION` stays 8.
   Commit on `osc-green-aperture` (no trailer); then on the Athenaeum branch the submodule pointer bump (trailer).

**Tests.**
- `the_analysis_measurement_is_identical_with_and_without_a_list` (analysis_mode): a mono frame, an RGGB and a GBRG
  mosaic, a 3-plane file; `measure_raw_frame` against `measure_raw_frame_and_list` with a 200-position list: every
  `StarFit` field by `to_bits` (NaN equal to NaN), every `FrameSummary` field, every `PlaneMeasurement` field except
  `timings` and `sub_steps` (wall time). This is B9's rustafits half.
- `a_list_on_a_mono_field_recovers_its_fluxes`: `integrated_field` at FWHM 2 and 3 px, β 2.5 wings fitted with Moffat 4,
  positions off by 0.1 px: median `flux` / true total within 1 %, median centre offset < 0.2 px.
- `a_list_on_a_mosaic_recovers_its_fluxes`: the same on a mosaic, plus G1 / G2 gains 1.00 / 0.98 and a +3 ADU G2
  pedestal: within 2 % (the OSC Task 1 bar).
- `a_list_star_whose_own_detection_coincides_still_qualifies_for_c`: every list star also detected →
  `aperture_stars ≥ 5`, `aperture_correction` `Some`.
- `the_c_cap_is_the_requests`: 300 qualifiers → `aperture_stars` 256; the same plane's Analysis `aperture_stars` ≤ 64.
- `an_unlisted_neighbour_does_not_lift_the_ring_sky`: a bright detected, unlisted star 11 F from a list star: the list
  star's `flux` within 0.5 % of the field without it.
- `a_saturated_list_star_is_not_kept`; `a_far_centre_is_not_kept` (projected 3 px off, tolerance 1 px: `kept` false);
  `a_trailed_plane_measures_no_list`; `an_empty_request_measures_no_list`.
- Every existing lib test, and the pins in `analysis::fitting`, unchanged.

**Acceptance.** In `rustafits/`: `cargo test`, `cargo test --features debug-pipeline`, `cargo test --features serde`
green; `cargo publish --dry-run --allow-dirty` passes. In the worktree: `cargo check -p athenaeum-core --all-targets`
green. `grep -rn measure_raw_frame_and_list rustafits/src crates/` lists `analysis_mode` alone.

## Task 2 — core, pure: anchor, window, list, statistic

**Status.** Done: 1bd79263 and its review round c7cf5c9c. The text below is the r0 model; G2 keeps the anchor, the
window, the list builder and the composition's shape, and rewrites the statistic ("Of Task 2" in G2).

**Files.** `crates/athenaeum-core/src/analysis/zero_point.rs`: a new section after `catalog_for_frame` (:371–397);
reuses `median` / `median_and_sigma` (:172–188), `Grid` (:190–219), the scale formula (:388); the clip loop (:334–352)
and verdict (:354–368) of `measure_zero_point` move into one `clipped_median` that both call, so 6.5's function is
bit-identical. `analysis/analyzer.rs` (beside `measure_raw_frame` :45–54 and `map_measurement` :67–139) for the
composition. `xxhash-rust` (xxh3) is already a core dependency (`Cargo.toml:20`).

**Approach** (spec §5.1, §5.3; constants as above).
1. `fwhm_ref_px(scale_arcsec)`; `pixel_scale_arcsec(wcs)` = the larger CD column norm × 3600 (:388's formula).
2. `anchor_zero_point(stars, reported_c, wcs, catalog, exptime, w, h, median_fwhm)`: `measure_zero_point` over the
   budget rows, fluxes × `reported_c` on a mosaic (× 1 when the mosaic reported none), with `ZpParams { min_stars: 10,
   max_std_error_mag: f64::INFINITY, ..default }`; fewer than 10 → `ForcedRefusal::NoAnchor`; no exposure →
   `NoExposure`.
3. `window(anchor, fwhm_ref, full_scale, exptime)`: α = FWHM_ref / (2·√(2^¼ − 1)), F_sat = 0.7·full_scale·π·α² / 3
   (β 4), `G_sat = anchor − 2.5·log10(F_sat / t)`, `G_b = G_sat + 1.5`. `full_scale` in the fit's working units, as
   `saturation_plane` sets `norm` (rustafits mod.rs:2182–2185).
4. `forced_list(input, params) -> Result<ForcedList, ForcedRefusal>`: project the cone (non-finite skipped, as
   :241–250); keep stars inside 0.9 of the half-diagonal and ≥ 13·`FWHM_ref` from every edge; isolation by the
   catalogue only (a grid at 10·`FWHM_ref`): drop a star with another of `G ≤ G_i + 4` within 4·`FWHM_ref`, or one of
   `G ≤ G_b − 2` within 10·`FWHM_ref`; window `[G_b, faint]` with faint = `G_b + 3`, or, when fewer than 200 isolated
   stars lie in `[G_b, G_b + 3]`, the G of the 200th brightest isolated star in `[G_b, G_b + 4.5]` (at most
   `G_b + 4.5`); cap: key = xxh3-64 of the little-endian bytes of (round(G·1000) as i32, round(ra·3600) as i64,
   round(dec·3600) as i64), kept when `key / 2⁶⁴ < 1000 / n` (n = the list before the cap). Returns per star (key, G,
   x, y), n before the cap, `G_b`, the faint edge, `FWHM_ref`, and the `ListRequest` (tolerance
   `max(1, 0.5·FWHM_ref)`).
5. `forced_zero_point(list, measurement, exptime) -> ForcedOutcome`: `c` `None` → `NoApertureCorrection`; per kept
   star with a positive finite `flux`, `zp_i = G_i + 2.5·log10(flux_i / t)`; `clipped_median` (4 σ, ≥ 50, standard
   error ≤ 0.035) → a `ZeroPoint`, or `TooFewStars { list, kept }`, `TooScattered { std_error, kept, sigma }`. The
   outcome carries every figure §5.5 stores (value, kept, σ, list size, `c`, `G_b`).
6. `ForcedRefusal::code()`: `no_exposure`, `no_anchor`, `no_aperture_correction`, `too_few_stars`, `too_scattered`;
   Task 5b adds `unreadable_solve` and `colour_camera` (stored, see the readings).
7. The composition, in `analyzer.rs`: `measure_with_zero_point(frame, config, pool, inputs: Option<ZpInputs>, cone)
   -> (PlaneMeasurement, Option<ForcedOutcome + ForcedList>)`, `ZpInputs { wcs, exptime, epoch }`, `cone` 6.5's
   closure type. It calls Task 1's entry with a callback that maps the budget fits (`map_measurement`'s star rows),
   reads the cone, builds the anchor and the list; then the statistic. No database, no file: the probe (Task 3) and
   the analyzer (Task 5b) call this one function, so Phase A judges the code that ships.

**Tests.**
- `a_dithered_field_keeps_ninety_five_percent_of_its_list`: a synthetic catalogue (5 000 stars, G 11–19), the WCS
  shifted 20–60 px and rotated 0.05°: the share of frame A's list in frame B ≥ 0.95.
- `a_loss_moves_the_window_by_the_loss`: anchor − 0.5 → `G_b` − 0.5 (1e-12).
- `a_capped_list_keeps_the_same_stars`: 10 000 candidates, two dithers: the capped lists share ≥ 95 %; the capped
  list's median G within 0.05 of the uncapped one.
- `the_cap_key_is_pinned`: one record's key equals a recorded constant (guards a hash change).
- `isolation_reads_the_catalogue_only`: ΔG 3.9 at 3.9·`FWHM_ref` drops the star, ΔG 4.1 keeps it; a `G_b − 2.1` star at
  9.9·`FWHM_ref` drops it, at 10.1 keeps it.
- `the_list_does_not_follow_the_seeing`: two inputs that differ only in the budget's median FWHM give the same list.
- `fwhm_ref_floors_at_two_pixels` (1.87″/px → 2; 0.5″/px → 5); `the_edge_margin_and_region_hold`;
  `a_sparse_field_deepens_the_faint_edge_to_four_and_a_half_at_most`.
- `the_anchor_needs_ten_matches`; `the_anchor_takes_the_reported_c_on_a_mosaic` (c 1.25 → +2.5·log10 1.25).
- `forced_zero_point_recovers_the_truth` (600 stars, 0.1 mag scatter: |ZP − truth| < 0.01), `forty_nine_kept_refuse_
  with_the_list_size`, `a_wide_scatter_refuses`, `no_c_refuses`.
- `the_composition_measures_a_synthetic_frame` (analyzer): a mono `moffat_field` with a matching synthetic cone gives
  a valid outcome within 0.02 of truth.
- The 6.5 pure tests (zero_point.rs :1005–1265) pass unmodified.

**Acceptance.** `cargo test -p athenaeum-core --lib analysis::zero_point` and `--lib analysis::analyzer` green;
`cargo check -p athenaeum-core --all-targets` green.

## Task 3 — `zp_probe forced` and its verdict

**Status.** Done: 3134eb8a (the list flags at c7cf5c9c; F5's emulation modes 7d26e764). The text below is the r0 probe;
G3 moves it onto the new composition ("Of Task 3" in G3).

**Files.** `crates/athenaeum-core/examples/zp_probe.rs` (header :1–47; `MEASURE_HEADER` :267, `MeasureRow` :270+;
`Candidate` / `load_candidates` / `load_candidate_by_id` :1630–1749; `measure_candidate` :1818–1960, its in-memory read
:1852–1877 and airmass :1888–1897; `cmd_measure` :1977–2070; `least_squares` :975–1041; `shared_extinction_fit`
:1043–1127; `osc_nights` :1152–1176; `CommonRow` :624–639; `cross_target` :597; `cmd_verdict` :2328+; `main`
:2709–2729; tests :2735+). Runner `~/.athenaeum-acc/2026-11-16-osc-zp/run-forced.sh` (job folder, beside
`run-osc.sh`).

**Approach.**
1. `forced`: the same selection as `measure` (`--filter -` = empty `FILTER`); per light: read the file, call Task 2's
   `measure_with_zero_point`, also 6.5's budget zero point on the same measurement (× the reported `c` on a mosaic,
   what Phase A r1 called `zero_point_c`): the `zero_point_today` the B2 and B8 comparisons read. Writes nothing but
   its TSVs.
2. `<x>.tsv`, one row per light: frame_id, file_name, object, filter, night, date_obs, exptime, rig, airmass, pier,
   rotation_deg, median_fwhm, scale, fwhm_ref, anchor, anchor_stars, window_bright, window_faint, list_before_cap,
   list_stars, accepted, kept, c, c_stars, c_se, zero_point, sigma, std_error, refusal, centre_median_px,
   ring_missing, zero_point_today, identity. `pier`: from `plate_solves.field_rotation_deg`, the night's first frame
   is side 0 and a frame whose rotation differs from it by more than 90° is side 1.
3. `<x>.stars.tsv`, one row per list star: frame_id, key (hex), g, x_proj, y_proj, x_fit, y_fit, model_flux, flux,
   snr, flags, accepted, kept, zp_i, clipped.
4. `--identity-every <n>` (default 10): every n-th frame is also measured by `measure_raw_frame` and compared as Task
   1's identity test does; `identity` = `ok` or `mismatch:<field>`.
5. `verdict --forced [--common <tsv>] <x>.tsv...` (each `<x>.stars.tsv` read beside it): every night of ≥ 30 measured
   frames, pairs ≤ 15 min apart (`PAIR_MAX_GAP_S`), one line per bar (a night's entry each), ≤ 30 lines, the last the
   class line, GO or STOP. A common-set night has ≥ 30 frames joined to `--common` (by id, else file name). Bars
   exactly as §6:
   - **B1** per pair, the share of frame j's kept keys that frame j+1 also kept: every pair ≥ 0.80 (min and median
     printed).
   - **B2** median |ΔZP| of the pairs: ≤ 0.015 on a common-set night; elsewhere ≤ 0.020 and ≤ the same statistic of
     `zero_point_today` on the same pairs.
   - **B3** common-set nights, `d = ZP − 2.5·log10(t_flux / t)` (`CommonRow::green_tflux`): RMS about its median ≤
     0.015; slope of `d` on FWHM within ±0.04 mag/px.
   - **B4** `ZP = a + kX (+ s·side)` and the stack's level `2.5·log10(t_flux / t)` fitted alike: |k − k_stack| ≤ 0.05,
     judged only where ΔX ≥ 0.4, and on a night with two pier sides one side at a time (F7: ATR L and OSC 10-18); `s`
     printed, never judged; a night without the join or with ΔX < 0.4 unjudged.
   - **B5** nights without a common set: per rig `shared_extinction_fit` extended with a per-night side column on
     nights with two sides (one k, an intercept per night, an FWHM slope per night): |slope| ≤ 0.03 where its error ≤
     0.015, else printed unjudged; frames without a site have no airmass and are counted out.
   - **B6** valid / measured per night ≥ 0.90; on 6.5's sets (QHY268M L and H) = 1.00.
   - **B7** per night, the median over frames of each frame's median |fitted − projected| ≤ 0.25·`FWHM_ref`.
   - **B8** mono rigs: the seeing slope with airmass (B5's, or B4's fit plus an FWHM term on ATR L) |·| ≤ 0.03 and ≤
     the same figure of `zero_point_today`; the QHY268M L cross-target spread (`cross_target`) ≤ 0.10 and ≤ today's on
     the same frames; 6.5's committed 0.025 printed beside.
   - **B9** every compared frame `ok`; the line names Task 1's rustafits test.
   - Reported only: both `c`s, list sizes and refusals per night, ring-missing counts, and the no-data lines (haze,
     under 16 bit, long focal length, high latitude).
6. `run-forced.sh <tag>`: copies the binary into the job folder first; measures OSC `ZWO ASI2600MC Duo` LDN 1272 and
   vdB 152 (`-`), `ATR2600M` LDN 1272 (`-`), `QHY268M` L on IC 5146 / LDN 1042 / LDN 1031 and H on IC 5146 / LDN 1042;
   DB `$J/catalog/athenaeum.db` read-only, app data the release app's (tiers 500 + 2000), `--common
   $J/out/common-merged.tsv`; verdict into `out/<tag>/verdict-forced.txt`; prints ≤ 30 lines. The controller reads it
   before it runs.

**Tests.** `the_kept_share_counts_consecutive_pairs_only`; `b2_uses_the_common_bar_only_on_a_joined_night`;
`the_side_follows_the_rotation`; `the_flip_term_is_fitted_and_not_judged` (synthetic rows: k and s recovered within
1e-9); `b4_is_unjudged_below_four_tenths_of_an_airmass`; `b3_rms_is_about_the_median`;
`an_identity_mismatch_names_its_field`; `the_forced_verdict_ends_in_one_class_line_within_thirty_lines`;
`an_older_tsv_parses_and_a_forced_row_round_trips`. Regressions: `verdict` without `--forced` / `--osc-c` on
`.superpowers/sdd/2026-11-15-collab-v3-wave6-5/probe-results/final/*.tsv` reproduces each committed `verdict-*.txt`
line for line; `verdict --osc-c` on `out/r1/*.tsv` reproduces `out/r1/verdict-osc-c.txt`.

**Acceptance.** `cargo build --release --example zp_probe -p athenaeum-core` and `cargo test -p athenaeum-core
--example zp_probe` green; the two regressions; a `forced --limit 3` smoke on LDN 1272 (controller).

## Task 4 — Phase A

**Status.** Done: `run-forced.sh f0` (933 frames) STOP(missed B1, B3, B5, B7, B8); judge `task-F4-report-r0.md`, tuning
rounds f1–f3 bought no class pass; causes in spec §4.5. The re-audit followed (F5); G4 is Phase A again.

Controller first: the catalogue copy (`PRAGMA integrity_check` ok); the counts it holds (2026-11-16: OSC LDN 1272 160
solved of 164; vdB 152 261 solved; `ATR2600M` LDN 1272 65 solved, `FILTER` empty, night key 2025-10-17; `QHY268M` L
166 / 84 / 45, H 100 / 52); `/Volumes/bigbase3` mounted; disk (`df`); no other heavy job. Sonnet runs `run-forced.sh
a0`; opus judges each bar from the TSVs and explains every miss on the frames it sits on (a stretched JPG and its list
stars first), writes `task-4-report-r0.md`. GO only if every bar passes on every judged night; any miss → STOP, the
owner gets the numbers. A moved constant reruns every set and is named in the report. Controller re-gate: B1, B2 and
B6 recomputed from the TSVs by a script in `out/<tag>/`.

## Model re-audit (F5): tasks G1–G4

The spec's §5 as amended by F5, built in four tasks, one implementer each and one at a time (16 GB Mac). Each task's
implementer reads the spec §5.1–§5.3, §5.6 and §6, "Constants" above and the Review rulings with their F5 amendments at
the end of this plan first. Line numbers are those of the branch at 7d26e764: re-verify every one you edit (#15).

Build order across the two repositories: G1 changes the public list entry in rustafits, so `athenaeum-core` does not
compile against it until G2. G1 therefore commits in rustafits only and does not bump the pointer; G2's first commit bumps
the pointer and moves the core onto the new entry in the same commit, so every Athenaeum commit builds. G2 also keeps the
probe compiling: flags and columns alone do not (M4), so G2 removes the F5 emulation and the 6F referee as well (G2 step
12); G3 rebuilds the probe.

### G1 — rustafits: the list entry v2 (R-OSP-19)

**Files.** In `rustafits/` (branch `osc-green-aperture`, head 30fd4e7): `src/star_measure/analysis_mode.rs` (module doc
:16–21; `LIST_C_STARS` :132–134; `ListRequest` :136–160; `ListFit` :162–183; `ListRefusal` :185–201; `ListMeasurement`
:203–225; `measure_raw_frame_and_list` :227–329 — its branches :290–313 and the callback :314–316; the list tests
:1050–1737); `src/star_measure/list.rs` (`refused_fit` :75–88; `measure_list` :96–320 — the request checks :112–125, the
seeds :143–181, a list star's own detection :195–205, `prepare_passes` :207–221 with the plane's clip floor at :220, the
fit call :237–275, the kept rule and `c` :277–301); `src/star_measure/photometry.rs` (beside `growth_curve_in`
:1003–1078); `src/star_measure/fit.rs` (`Correction` :1995–2047 with the list-only `max_c_stars` :2040, `warn_short_c`
:2042, `max_centre_offset` :2046; its `Default` :2049–2072; their reader in `ring_and_correction` :2655–2679);
`src/star_measure/mod.rs` (`FitStarsOptions::correction` :517–519 sets the three; `FluxCorrection` :527–535;
`install_in_span` :94); `src/star_measure/testing.rs` (the list's test constructors :40–90); `CHANGELOG.md` (Unreleased
:8–47). Nothing in Athenaeum (G2 bumps the pointer).

**Approach** (spec §5.2; the entry is unreleased, so its types change in place).
1. `measure_raw_frame_and_list<F, P>(frame, opts, pool, list: F, place: P) -> (PlaneMeasurement,
   Option<ListMeasurement>)`, `F: FnOnce(&PlaneMeasurement) -> Option<ListRequest>`, `P: FnOnce(&[ListFit]) ->
   Option<ListPlacement>`. The measurement and the trailed / defocused / three-plane branches (:253–313) are unchanged.
   `place` is called at most once, with the first pass's fits in request order, and never when `list` returned `None`
   or the request was refused.
2. Public types, `#[non_exhaustive]`, documented in the stable entry's terms (the module doc :16–21 rewritten):
   - `ListRequest { fit_positions: Vec<(f64, f64)>, max_centre_offset_px: f64 }`, `ListRequest::new(fit_positions,
     max_centre_offset_px)`; `max_c_stars` and `LIST_C_STARS` go.
   - `ListPlacement { refit_positions: Vec<(f64, f64)>, aperture_positions: Vec<(f64, f64)>, radius_px: f64,
     extra_radii_px: Vec<f64>, annulus_start_px: f64, annulus_area_px: f64, peak_radius_px: f64 }` with
     `ListPlacement::new(..)` (every field, since the type is `#[non_exhaustive]`). `None` from `place` ends the list:
     the result carries the first fits, no refits and no apertures (the caller's refusal).
   - `ListFit`: `flux` goes (it was `model_flux × c`); `x`, `y`, `model_flux`, `snr`, `flags`, `accepted`, `kept`,
     `centre_offset_px` stay. `kept` as today: accepted, not saturated, within `max_centre_offset_px` of its seed.
   - `ListAperture { sum: f64, extra_sums: Vec<f64>, sky: f64, sky_scatter: f64, peak: f64, on_frame: bool }`, one per
     aperture position in order, working units (the fits' units): `sum` at `radius_px`, `extra_sums` one per
     `extra_radii_px` entry.
   - `ListMeasurement { fits, refits: Vec<ListFit>, apertures: Vec<ListAperture>, clip_level: Option<f64>, beta,
     ring_fwhm, refused, refit_refused: RefusalCounts, timings: ListTimings { fit_ms, refit_ms, aperture_ms }, refusal }`;
     `aperture_correction`, `aperture_stars`, `aperture_se` go. `timings` is wall time, exempt from the identity test
     like `PlaneMeasurement::timings`.
3. Both fit passes keep `measure_list`'s machinery (the saturation plane, the verdict pass at the measurement's field
   width with the plane's clip floor (R3), the neighbours less a list star's own detection (R2), `PsfModel::Fixed(pm.beta)`,
   the plane's mask) and measure no `c`: the fit call's `Correction` takes `flux: FluxCorrection::Given(1.0)` and
   `report_aperture: false` (step 6 merges `masked_growth` into it), so `ring_and_correction` never reaches
   `aperture_correction` and no growth curve runs (the ring sky and the SNR pass still run: the map reads the SNR). The
   refits are the same call at `refit_positions`, the own detections dropped against the refit seeds.
4. A crate-private `photometry::aperture_sums(working, mask, w, h, (x, y), radius, extra_radii, annulus_start,
   annulus_area, peak_radius) -> ListAperture` beside `growth_curve_in`, on the same working plane
   (`domain::to_working_units`) and the same present-pixel rule (finite and, on a mosaic, a green site of the mask). It
   ports the probe's `ap_fluxes` / `disc_share` / `grouped_median` (`zp_probe.rs` :3381–3449, :3357–3376, :2569–2584):
   - the edge: a pixel whose centre is within `r − 0.75` of the position counts 1, beyond `r + 0.75` counts 0, between by
     a 4 × 4 sub-sample of the pixel;
   - the sky: the annulus from `annulus_start` to `√(annulus_start² + annulus_area/π)`, its present pixels per phase (one
     phase on mono; on a mosaic the phase `(y & 1)·2 + (x & 1)`, so the two green phases), each phase's grouped median
     `m − q/2 + q·(n/2 − below) / equal` on quantised data, `q` the smallest positive difference between the annulus'
     values (1 on 16-bit data, 16 or 4 on a 12- or 14-bit camera scaled to 16 bits: the probe's `grouped_median`
     assumed 1), a plain median otherwise (by `select_nth_unstable`, no full sort);
     `sky` the mean of the phase medians, `sky_scatter` 1.4826 × the median of the pooled |pixel − its phase median|;
   - each sum: `Σ share·(v − its phase's sky)` over the present pixels, times `Σ share` over every pixel of the disc over
     `Σ share` over the present ones (the disc's area over its green area on a mosaic, 1 on a whole mono disc);
   - `peak`: the highest present pixel within `peak_radius`;
   - `on_frame` false, the sums NaN, when the annulus leaves the plane or a present pixel within the largest radius is not
     finite (the probe skipped such a pixel; a sum short of it would read low).
   The apertures run in parallel over the positions on the pool (`install_in_span`, as `snr_pass` does).
5. `clip_level`: the plane's near-clip floor in working units, `bg + clip_floor·range` of the list's saturation plane (the
   value `measure_list` hands `prepare_passes`, :220); `None` when the plane shows no clipped evidence.
6. Remove the list-only `Correction` fields `max_c_stars`, `warn_short_c` and `max_centre_offset` with their defaults and
   their settings in `FitStarsOptions::correction`: their one reader was the list's `c`, so every other caller is
   bit-identical. Task 1 step 1's `masked_growth` / `report_aperture` split is merged back into `report_aperture`
   (masked growth and not applied, as before Task 1): with the list's `c` gone its one writer sets both alike
   (mod.rs:1780–1781, :2035–2036), and the `masked` argument `ring_and_correction` hands `aperture_correction`
   (fit.rs:2677) reads `report_aperture`.
7. One `debug` "list measured" (`count`, `kept`, `refits`: kept refits, `apertures`: on-frame ones, `aperture_ms`); the
   dictionary paragraph is G2's.
8. `CHANGELOG.md` Unreleased: the list-entry bullet rewritten (the two callbacks, `ListPlacement`, `ListAperture`,
   `ListTimings`, the aperture sums; no `c`, `LIST_C_STARS` gone); `MEASURE_VERSION` stays 8. Commit on
   `osc-green-aperture`, no trailer.

**Tests.**
- Kept, adapted to the two callbacks: `the_analysis_measurement_is_identical_with_and_without_a_list` (its `place`
  returns a placement of every position, so both passes and the apertures run; B9's rustafits half),
  `the_test_constructors_build_list_rows`, `a_saturated_list_star_is_not_kept`, `a_far_centre_is_not_kept`,
  `a_trailed_or_defocused_plane_measures_no_list_and_carries_its_verdict`, `a_three_plane_file_measures_no_list_and_says_why`,
  `a_position_off_the_plane_or_not_finite_is_refused_and_the_rest_unchanged`,
  `the_list_reads_the_planes_clip_floor_not_one_from_its_own_seeds`,
  `a_mosaic_stars_own_detection_one_point_three_px_off_is_still_its_own` (no `Blend`, kept; the `c` assertion goes),
  `an_empty_request_measures_no_list`.
- Re-aimed: `a_list_on_a_mono_field_recovers_its_fluxes` → the sum at `4·FWHM_ref` of an `integrated_field` star (FWHM 2
  and 3 px, positions off by 0.1 px) is the model's integrated light inside the disc within 1 %;
  `a_list_on_a_mosaic_recovers_its_fluxes` → the same on a mosaic with G1 / G2 gains 1.00 / 0.98 and a +3 ADU G2 pedestal,
  within 2 %; `a_fit_centred_off_its_position_is_accepted_but_not_kept_and_not_in_c` → `..._but_not_kept`;
  `an_unlisted_neighbour_does_not_lift_the_ring_sky` → on the map fit's SNR (within 0.5 %).
- Removed with the list's `c`: `a_list_star_whose_own_detection_coincides_still_qualifies_for_c`,
  `the_c_cap_is_the_requests`, `an_unlisted_neighbour_ten_times_brighter_moves_c_by_its_wings_only`.
- New: `a_mosaic_stars_sum_is_twice_its_green_sum_inside_the_disc`; `an_aperture_off_the_frame_is_not_on_frame`;
  `a_placement_of_none_returns_the_first_fits_and_no_aperture`; `the_refits_are_made_at_the_placed_seeds` (seeds moved
  0.6 px: the refits' offsets are read from the placed seeds); `the_sub_pixel_edge_is_continuous` (a disc of 1 on a
  plane of 0, reaching past `r + 0.75` and short of the annulus, so the sky is 0 — a flat plane of 1 sums to 0 after its
  sky: the sum at 20 sub-pixel positions is `π·r²` within 5e-3 relative, 4 × 4 sub-sampling being good to about 1.3e-3
  at 8 px); `each_green_phase_is_less_its_own_sky` (+3 ADU on G2 moves no sum by more than 0.5 %);
  `the_grouped_median_follows_a_fractional_level` (integer data drawn about 100.3: 0.3 recovered within 0.05; data in
  steps of 16 drawn about 1604.8: 4.8 recovered within 0.8, the quantum read from the data);
  `a_non_finite_pixel_in_the_disc_makes_the_aperture_unusable`;
  `the_list_fits_take_the_given_unit_correction` (the fit call's `FitOutcome::aperture_correction` is `Some(1.0)` over 0
  stars, the `Given` branch: no growth curve ran).
- Every other lib test and the pins in `analysis::fitting` unchanged.
- Test support (`testing.rs`): `ListMeasurement::test_new(fits, refits, apertures)` (the `c` arguments go) and a new
  `ListAperture::test_new(sum, extra_sums, sky, sky_scatter, peak)` (on the frame); core's tests use them (G2).

**Of Task 1 (45236db, 30fd4e7).** Kept: the entry's measurement and its three no-list branches, `ListRefusal`, the fit
machinery of `list.rs` (saturation plane, verdict pass, R2's own detection, R3's clip floor, neighbours, the positions
refused without a fit), `ListFit` less `flux`, the identity test, the test constructors (signatures changed). Changed:
the signature (`place`), `ListRequest`, `ListMeasurement`, the fit call's `Correction` (no `c`), step 1's `masked_growth`
merged back into `report_aperture`. Removed: the list's `c`
(`LIST_C_STARS`, `max_c_stars`, R4b's centre pre-filter `max_centre_offset`, `warn_short_c`) and its three tests.

**Acceptance.** In `rustafits/`: `cargo test`, `cargo test --features debug-pipeline`, `cargo test --features serde`,
`cargo test --features test-support` green; `cargo publish --dry-run --allow-dirty` passes; `grep -rn
"LIST_C_STARS\|max_c_stars\|warn_short_c\|max_centre_offset[^_]" src` empty. The Athenaeum build is G2's.

### G2 — core, pure: map, gate, pedestal, statistic, window slope, the reduction

**Files.** The `rustafits` pointer (to G1's head, in the first commit); `crates/athenaeum-core/src/analysis/zero_point.rs`
(`clipped_median` :231–273; `ForcedListParams` :440–509; `ForcedRefusal` :511–562; `ForcedListInput` :636–650;
`ForcedStar` :652–663; `ForcedList` :665–679; `cap_key` :681–689; `forced_list` :694–799; `ForcedStarZp`,
`ForcedZeroPoint`, `ForcedOutcome` :801–837; `forced_zero_point` :839–898; `forced_tests` :2303–2789);
`analysis/analyzer.rs` (`mod forced` :180–419 — `ForcedRun` :206–226, `prepare_forced_list` :245–311,
`measure_with_zero_point` :313–418; its tests :838–1172); `examples/zp_probe.rs` only as far as the build needs
(`Args::forced_list_params` :234+, `every_forced_list_field_has_a_flag` :5263, the fields of `ListFit` /
`ListMeasurement` / `ForcedOutcome` it reads, and what step 12 removes or changes: `BIG6_*` :2851–2855, `six_f_zero_point`
:3192–3236 and its call :3784–3789, the F5 emulation :3238–3479 with `AP_*`, `MAP_*`, `AP_HEADER`, `G_HEADER`
:3250–3261, the re-seed loop :3590–3658 with the one-callback entry :3643 and the old `forced_zero_point` :3647, the
`--dump-ap` writer :3813–3898, the stars TSV loop :3791–3811); the logging dictionary
`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md:72`.
`zero_point` and the composition stay behind `solver` (`analysis/mod.rs:3–6`, `analyzer.rs:24–25` and `:180`; `analysis`
itself is `render`-gated, `lib.rs:38`).

**Approach** (spec §5.1–§5.3, §5.6; "Constants" above).
1. `ForcedListParams` as "Constants": `isolation_radius_fwhm` 8, `cap` 12 000; `c_stars` and `max_std_error_mag` removed;
   the new fields added with their defaults.
2. `forced_list`: the wider isolation (its grid cell grows with it); `decimate` (`key % decimate == 0`, on the isolated
   candidates before the faint edge's count, the guard and the map's choice: the same tenth of the catalogue in every
   frame, and a sparse field as the code would see one); `anchor_shift_mag` added to `ForcedListInput::anchor` before
   `window` (M3: a real loss moves the anchor, and the faint edge then follows by count when deepened, or not at all when
   cut by the catalogue; moving both edges after the rule measured a cousin of that); `ForcedList` gains
   `map: Vec<usize>` (indices into `stars`: `G ≤ G_b + map_window_mag`, the first `map_max_stars` in key order) and its
   `request` becomes the map stars' (`ListRequest::new(map positions, tolerance)`). `pub fn window_weight(g, list,
   params) -> f64` = `clamp(min((G − G_b)/ρ, (G_f − G)/ρ), 0, 1)`.
3. `QuadMap` and `position_map(list, fits, params) -> Result<QuadMap, ForcedRefusal>` (`fits` in `list.map` order):
   members are the fits accepted, not saturated, SNR ≥ `map_min_snr` where finite (a fit with no ring sky has a NaN SNR
   and is not refused for it, as the emulation did), centre offset ≤ `map_radius_fwhm·FWHM_ref`; coordinates about the
   frame centre over the half-diagonal; a full quadratic `(1, u, v, u², uv, v²)` per axis by least squares (a private
   normal-equations solve: the probe's `least_squares`, `zp_probe.rs:1071`, copied, since an example's items cannot be
   reached); up to `map_clip_rounds` rounds dropping residuals over `max(map_clip_sigma · 1.4826 · median |residual|,
   map_clip_floor_px)`, stopping when nothing drops or fewer than `map_min_members` would stay. Fewer than
   `map_min_members` members: `TooFewStars { list, kept: members }` when the list holds fewer than ⌈`map_min_members` /
   `map_min_kept_share`⌉ = 40 map stars (spec §5.3, a sparse field; 40 stands on zero false refusals in G4's decimated
   round, M5), else `PoorSolve { kept_share: NaN }`.
   `QuadMap::shift(x, y) -> (dx, dy)`; `members`, `offset_px` (median |fitted − projected| before), `residual_px`
   (median |residual| after).
4. `map_gate(list, refits, placement, params) -> Result<MapGate, ForcedRefusal>`: `kept_share` = kept refits / map stars
   asked; under `map_min_kept_share` → `PoorSolve { kept_share }`. `MapGate { kept_share, centre_median_px }`, the latter
   the median |refit − its placed seed| over the accepted refits (B7's figure).
5. `placement(list, map, frame_fwhm, params) -> ListPlacement`: refit seeds and aperture positions = projections +
   `map.shift`; `R = max(aperture_radius_ref·FWHM_ref, aperture_radius_fwhm·F)`; extra radii `pedestal_radii_ref ×
   FWHM_ref`; annulus start `max(annulus_start_fwhm·F, pedestal_radii_ref[2]·FWHM_ref) + annulus_gap_px`; area
   `annulus_area_px`; peak radius `peak_radius_ref·FWHM_ref`. `F` is `FrameSummary::from_measurement(pm, opts).median_fwhm`
   (the Analysis median FWHM, what the emulation read), taken in the first callback.
6. `sky_pedestal(apertures, members, fwhm_ref, params) -> Option<f64>`: `D = extra[2] − extra[1]` on `extra[0]` over the
   members, a straight line by least squares, `pedestal_clip_rounds` rounds dropping residuals over `pedestal_clip_sigma
   · 1.4826 · MAD` about zero (G2 review M1: not about the median residual); fewer than `pedestal_min_points` left or no spread in `extra[0]` → `None`; ε = intercept /
   (π·(R₆² − R₄²)), `R₆` and `R₄` the two outer pedestal radii in px.
7. `forced_zero_point(list, measured, placement, exptime, params) -> ForcedOutcome`, rewritten to spec §5.3: members (in
   the window, `on_frame`, `peak < clip_level` when the plane has one); ε (`None` → `TooScattered { kept, sigma: NaN }`);
   `f = sum − ε·π·R²`; the screen (`clipped_median` over the positive-flux members' `zp_i` at `clip_sigma`, minimum 1, no
   standard-error limit, for `m` and `σ`), then its low rule (dropped when `f + 4·n` is below `t·10^(0.4·(m − 4σ − G))`,
   `n = sky_scatter·√(π·R²)`) and its upper rule, a taper (G2 review H1; the F5 plan review's M1, counting a member at the limit, is
   withdrawn: measured on g0 it biased the ZP by +0.006…+0.026 per set against the drop, following σ, see the ledger's
   "H1"): a member with `zp_i > m + 4σ` keeps its own `f` at the weight × `clamp(1 − (zp_i − m − 4σ)/(τ·σ), 0, 1)`,
   τ = `ForcedListParams::screen_taper_sigma` (default 0 since G7: τ ≤ 1e-9, or NaN, is a plain drop, as the emulation; `validate()` refuses a NaN at start-up), a member of zero weight
   dropped and not counted, so the ZP moves continuously as `m` and `σ` jitter; the window slope's ramp sums carry the
   taper (`taper·f`, `taper·10^(−0.4·G)`) and `stars` counts the weights above zero; the ZP as the weighted ratio of sums, `Σ w·f ≤ 0` → `TooScattered { kept, sigma }`; the window slope
   (`None` under `slope_min_ramp_members` on the bright ramp; the faint ramp's term taken as it is, zero when the
   catalogue ends before `G_f − ρ`, H1); valid with ≥ `min_stars` members of non-zero weight and `σ ≤ max_sigma_mag`,
   else `TooFewStars { list, kept }` / `TooScattered { kept, sigma }`. `ForcedZeroPoint { zero_point, stars, sigma }`
   (stars = members of non-zero weight); `ForcedOutcome { result, list_stars, members, tapered (the members above
   `m + 4σ`), pedestal: Option<f64>, window_bright, window_slope: Option<f64>, per_star: Vec<ForcedStarZp {
   index, zp, weight, member, screened_in }> }` (the `c` fields and `fitted_kept` go).
8. `ForcedRefusal`: `PoorSolve { kept_share: f64 }` added (code `poor_solve`, stored); `NoApertureCorrection` removed;
   `TooScattered { kept, sigma }` (its `std_error` goes with the standard-error rule). `code()` and `is_stored()` follow.
9. The reduction, pure: `CommonWindow { pivot, slope }`; `common_window(frames: &[(f64, Option<f64>)]) ->
   Option<CommonWindow>` over `(G_b, s)` (the median `G_b`; the median of the present slopes, 0 when none; `None` for no
   frame); `CommonWindow::reduce(&self, zero_point, window_bright) -> f64` = `zp − slope·(G_b − pivot)`; and
   `reduce_to_common_window(frames: &mut [(f64, f64, Option<f64>)])` (zp, `G_b`, s; each zp reduced in place) for the
   probe. 6a's `transparency.rs` and G3's verdict call these, never their own copies.
10. The composition `measure_with_zero_point` keeps its signature (`frame, config, pool, inputs, params, cone`) and drives
    both callbacks: the first as today (`prepare_forced_list` under `catch_unwind`, which also keeps `F`), returning the
    map stars' request; `place` under its own `catch_unwind` builds the map and the placement or records the refusal and
    returns `None`; afterwards `map_gate` on the refits, then `forced_zero_point`. The refusal order stays (colour, the
    list's own refusal, the plane's verdict, an empty list, internal), with the map's and the gate's refusals after the
    list's. An empty map request (a list with no map star) is `TooFewStars { list: n, kept: 0 }` with the list size n,
    not `list: 0` (today's branch, analyzer.rs:343, :401–403, counts the request). `ForcedRun` gains `map:
    Option<QuadMap>`, `gate: Option<MapGate>`, `placement: Option<ListPlacement>`. A length mismatch of fits, refits or
    apertures with what was asked is `Internal` (not stored), as today.
11. One `debug` "forced list measured" per frame from the composition: `list`, `members`, `map_members`,
    `map_kept_share`, `map_residual_px`, `pedestal`, `window_slope`, and `zp_reason` on a refusal. The dictionary
    paragraph at :72 gains these names (`members` usize, `map_members` usize, `map_kept_share` f64, `map_residual_px` f64,
    `pedestal` f64 working units per px, `window_slope` f64 mag per mag) and rustafits' "list measured" fields (`refits`,
    `apertures`, `aperture_ms`); `max_c_stars` and `aperture_stars` are retired with the list's `c`.
12. The probe, so that `cargo check --all-targets` stays green (M4: flags and columns alone do not keep it so, and two
    of its paths would compile while pairing `list.stars` with what are now the map stars' fits). Moved here from G3
    step 1: the F5 emulation removed (`Emulation`, `--reseed`, `<x>.g.tsv`, `QuadMap`, `quad_map`, `ap_fluxes`,
    `disc_share`, `neighbour_distances`, the re-seed loop :3590–3658 on the one-callback entry :3643 and the old
    `forced_zero_point` :3647, the `--dump-ap` writer :3813–3898, the constants `AP_*`, `MAP_*`, `AP_HEADER`, `G_HEADER`
    :3250–3261) and the 6F referee (`six_f_zero_point`, its call :3784–3789, `BIG6_*`); the stars TSV loop (:3791–3811)
    reads no `m.fits` (G3 step 3 rewrites it). Then flags for the new fields (`pedestal_radii_ref` parsed as `a,b,c`), the
    removed fields' flags gone, the `c` columns written empty (:3693). Nothing else (G3).

**Tests** (`forced_tests` and the analyzer's `mod forced`).
- New: `a_shifted_and_quadratically_distorted_field_is_mapped_to_under_five_hundredths_of_a_pixel`;
  `a_field_shifted_three_pixels_over_a_third_of_the_frame_is_poor_solve` (as built, and
  corrected by the G2 review: a 3 px shift over a third of the frame as a single block **passes** the gate, on purpose —
  a quadratic map follows a block that size, 0.80–0.95 of the map stars kept, and the stars stay inside the aperture
  radius, so the ZP moves ≤ 0.002 mag; the gate is for frames whose apertures miss, the eight bad frames at 0.31–0.64;
  the refusing case is every third 500 px stripe shifted 3 px, kept 0.4975, `poor_solve`); `fewer_than_thirty_map_members_is_poor_solve`;
  `a_list_with_fewer_than_forty_map_stars_is_too_few_stars_not_poor_solve`;
  `a_pedestal_added_under_every_aperture_is_recovered_and_leaves_the_zero_point_unmoved`;
  `an_unfittable_pedestal_refuses_too_scattered`; `a_star_at_a_window_edge_changes_the_zero_point_continuously`;
  `the_screen_keeps_a_negative_flux_inside_its_noise`; `a_loss_of_half_a_magnitude_reads_half_after_the_reduction` (a
  field whose offsets fall 0.15 mag per mag: 0.5 ± 0.02 after the reduction, 0.42 before it);
  `the_analytic_window_slope_matches_a_moved_window` (synthetic apertures with a G trend: `s` against
  `anchor_shift_mag` ±0.1 within 0.01); `fewer_than_ten_bright_ramp_members_give_no_slope`;
  `a_window_cut_by_the_catalogue_keeps_its_slope` (H1: a catalogue ending before `G_f − ρ`, the faint ramp empty: a
  slope from the bright ramp alone, never NULL for it); `a_member_above_the_screen_is_tapered_and_the_zero_point_moves_continuously` (H1: one star's flux raised across
  `m + 4σ` moves the ZP with a step an eighth of a plain drop's jump at most, under τ = 1; past the taper it is dropped; one far below
  `m − 4σ` past its noise is dropped too) and `a_zero_taper_is_a_plain_drop_with_no_division_by_zero`;
  G2 fix round and G7: `the_screen_taper_defaults_to_a_drop_and_a_nan_one_is_a_params_error`,
  `the_pedestal_clip_is_about_zero_not_about_the_median` (a one-sided tail against an inline port of the reference's clip,
  1e-9; the median-centred clip gives another intercept), `a_tapered_member_moves_the_window_slope_as_it_moves_the_window`,
  `the_pedestal_reports_the_share_its_clip_kept`, `the_pedestal_clip_reads_its_rounds`; the seed rule (G7):
  `a_bright_star_absent_from_the_cone_removes_the_list_stars_around_it_and_no_others`,
  `a_stars_own_seed_never_removes_it`, `a_neighbour_the_peak_floor_or_the_magnitude_rule_spares_does_not_remove`,
  `under_twenty_pairs_the_rule_is_skipped_and_the_list_is_the_catalogues`, `a_plane_with_no_seeds_skips_the_rule`,
  `a_seed_that_two_stars_share_is_no_calibration_pair`, `the_seed_rule_does_not_depend_on_the_pool`;
  `a_non_positive_sum_refuses_too_scattered`; `an_empty_map_request_carries_the_list_size` (analyzer);
  `sixty_members_are_valid_and_forty_are_too_few_stars`; `decimate_keeps_the_same_tenth_in_every_frame`;
  `common_window_of_one_frame_reads_its_zero_point`; `a_set_without_a_slope_is_not_reduced`.
- Re-aimed: `isolation_reads_the_catalogue_only` (ΔG 3.9 at 7.9·`FWHM_ref` drops the star, 8.1 keeps it; the bright rule
  unchanged); `a_capped_list_keeps_the_same_stars` (the guard, with `cap` set low in the test);
  `forced_zero_point_recovers_the_truth` (on apertures: |ZP − truth| < 0.01); `forty_nine_kept_refuse_with_the_list_size`
  → `forty_nine_members_refuse_with_the_list_size`; `a_wide_scatter_refuses` (σ > 1.0);
  `the_refusal_codes_are_stable_and_one_is_not_stored` (`poor_solve` in, `no_aperture_correction` out);
  `an_unfitted_or_unkept_star_is_left_out_of_the_statistic` → `an_off_frame_or_clipped_aperture_is_no_member`;
  `a_fits_length_mismatch_is_internal_and_not_stored` (fits, refits, apertures); in the analyzer
  `the_composition_measures_a_synthetic_frame` (mono `moffat_field`, within 0.02 of truth) and
  `a_mosaic_is_listed_on_its_green_sites_and_anchored_with_the_reported_c` (green-site apertures, within 0.02).
- Kept as they are: the list tests `a_dithered_field_keeps_ninety_five_percent_of_its_list`,
  `a_loss_moves_the_window_by_the_loss`, `the_cap_key_is_pinned`, `the_list_does_not_follow_the_seeing`,
  `fwhm_ref_floors_at_two_pixels`, `the_edge_margin_and_region_hold`,
  `a_sparse_field_deepens_the_faint_edge_to_four_and_a_half_at_most`, the anchor tests (:2619–2715), the analyzer's
  `the_analysis_numbers_do_not_move_with_a_zero_point`, `a_three_plane_frame_and_a_trailed_plane_give_their_refusal_codes`,
  `a_trailed_three_plane_file_is_still_a_colour_camera`, `exposure_solve_and_catalogue_refusals_are_told_apart`,
  `a_panicking_cone_keeps_the_measurement_and_stores_nothing`; new beside them `a_panicking_placement_keeps_the_measurement`.
- Removed: `no_c_refuses`. The 6.5 pure tests (zero_point.rs :1506–1765) pass unmodified.

**Of Task 2 (1bd79263, c7cf5c9c).** Kept: `pixel_scale_arcsec`, `fwhm_ref_px`, `window`, `Anchor`,
`anchor_zero_point`, `ForcedListInput`, `ForcedStar`, `cap_key` and its pin, `clipped_median` (now the screen's),
`forced_list`'s region, window and deepening, the composition's shape (`ZpInputs`, `ForcedRun`, the two `catch_unwind`s,
the refusal order, `CatalogueUnreadable` / `Internal` not stored), and every refusal but one. Changed: the isolation
radius and the cap (a guard), `ForcedList` (map stars; the request is theirs), `forced_zero_point`, `ForcedOutcome`,
`ForcedZeroPoint`, `TooScattered`'s fields. Removed: the list's `c` (`c_stars`, `NoApertureCorrection`, the outcome's `c`
figures), the standard-error rule of the list.

**Acceptance.** `cargo test -p athenaeum-core --lib analysis::zero_point` and `--lib analysis::analyzer` green;
`cargo check -p athenaeum-core --all-targets` green; `cargo test -p athenaeum-core --example zp_probe` green;
`cargo check -p athenaeum-core --no-default-features --features render` reports no error in `analysis/` (its two
pre-existing errors in `api/analysis.rs`, recorded at c7cf5c9c, stay); the 6.5 pure tests unmodified; `git -C rustafits
log -1` is G1's head and the pointer commit is this task's first.

### G3 — the probe and the runner

**Files.** `crates/athenaeum-core/examples/zp_probe.rs` (`FORCED_HEADER` :2845, `FORCED_STARS_HEADER` :2847,
`ForcedRow` :2868+, `forced_star_line` :3482, `forced_frame` :3520–3909, `cmd_forced` :3954–4063, the verdict constants
:4069–4093, `read_kept_keys` :4238, `kept_shares` :4295, `b3_night` :4447, `b4_entry` :4544, `rig_fit` :4619,
`forced_verdict_lines` :4698–5132, the tests :5210+; the F5 emulation and the 6F referee are gone by G2, M4, so these
numbers move: re-verify each); the runner `~/.athenaeum-acc/2026-11-16-osc-zp/run-forced.sh` (job folder, not tracked)
and its guard `memrun.sh` beside it.

**Approach.**
1. `forced` runs the shipping composition and nothing else (the F5 emulation and the 6F referee were removed in G2 step
   12, M4).
2. The row `<x>.tsv`: kept `frame_id` … `window_faint`, `list_before_cap`, `list_stars`, `zp_stars`, `c_budget` (the
   mosaic anchor's reported `c`), `zero_point`, `sigma`, `refusal`, `centre_median_px` (now `MapGate::centre_median_px`,
   B7's figure), `zero_point_today`, `today_refusal`, `identity`, `call_ms`, `plain_ms`, `list_kb`, `rss_mb`. Gone:
   `accepted`, `kept`, `c`, `c_stars`, `c_se`, `std_error`, `ring_missing`, `zero_point_6f`. New: `map_stars`,
   `map_members`, `map_offset_px`, `map_residual_px`, `map_kept_share`, `aperture_radius_px`, `members`, `tapered` (H1:
   the members above the screen's upper limit, weight tapered), `pedestal`, `window_slope`, `slope_remeasured`, `zero_point_6r`,
   `annulus_off`, `list_fit_ms`, `list_refit_ms`, `list_ap_ms`. The f0 and g0 TSVs are not re-judged by the new verdict.
3. `<x>.stars.tsv`, compact (a list now holds 2 000–9 000 stars a frame): `frame_id`, `key` (hex), `g`, `weight`,
   `member`, `screened_in`. `--dump-ap 1` writes `<x>.ap.tsv` with every aperture figure per list star (diagnosis only;
   its size is the reason it is off by default, see the job's memory stop in the ledger).
4. `zero_point_6r` (RR3's referee): the same ratio of sums over the same screened members and weights with
   `S(6·FWHM_ref) − ε·π·(6·FWHM_ref)²`, computed in the probe from the outcome's per-star rows and the apertures.
5. `--identity-every <n>` (default 10): on those frames, besides B9's plain `measure_raw_frame` compared as today, the
   composition runs again with `anchor_shift_mag` at ±0.1 and ±0.5 (M3) and at ±0.44, ±1.0 and ±1.75 (H2: a 1.5×,
   2.5× and 5× exposure step, both signs since the pivot may sit at either exposure), probe only: `slope_remeasured` =
   (ZP₊₀.₁ − ZP₋₀.₁) / 0.2 in the row, and every re-measure in `<x>.shifts.tsv` (`frame_id`, `shift`, `zero_point`,
   `window_bright`, `window_faint`, `window_slope`, `members`, `refusal`). The ten extra runs are outside `call_ms`; they
   add about one composition run per frame to a round.
6. `--decimate <n>` is `ForcedListParams::decimate` through the existing flag table (RR4's round); nothing else.
7. `verdict --forced`:
   - the reduction (spec §5.6): per rig, object and exposure (H2; one exposure per set in the corpus), core's
     `reduce_to_common_window` over the valid rows' (`zero_point`, `window_bright`, `window_slope`) gives each row's
     reduced ZP; B2–B5 and B8 read it (`zero_point_6r` is reduced by the same group's `common_window`; today's ZP has no
     window and is read as it is);
   - B1: the share of frame j's members of non-zero weight left by the screen (`member = 1`, `weight > 0`,
     `screened_in = 1`, as the emulation and `zp_stars` count) that frame j+1 also has (`read_kept_keys` reads those);
   - B3: three FWHM slopes printed, ZP − stack, ZP − 6r and 6r − stack (`zero_point_6r` in place of `zero_point_6f`); a
     slope beyond ±0.04 against the stack is a miss whether or not 6r echoes it, and the line says which: echoed, or
     not echoed and so the owner's as a stated limit with the three numbers (M2, RR2's rule);
   - B5 and B8 one night at a time (RR1): `shared_extinction_fit_sided` (:1149) called per night with `n_nights = 1`, so
     k, the intercept, the FWHM slope and, on a two-sided night, the side step are fitted jointly on that night; judged as
     written (|s| ≤ 0.03 where its error ≤ 0.015); B8's today slope by the same per-night fit on the same frames;
   - B7: `centre_median_px` (the map refits);
   - B4 unchanged (no allowance, RR2): its line also prints Δk's standard error from the two fits, for the owner;
   - reported: per night the list, members, the members tapered (H1), map members and kept share, the pedestal, the
     analytic slope beside the re-measured one, refusals; the cost line prints per set the median `list_fit_ms`,
     `list_refit_ms`, `list_ap_ms` and the list alone (`call_ms − plain_ms` on the identity frames) and names every set
     whose list alone is above 70 ms.
8. The runner already measures LBN 582 with the rest and copies the binary through a temp name (F5). Its header says:
   every invocation goes through the guard, `memrun.sh 3000 run-forced.sh <tag> [flags]`, one guarded command at a time,
   and no analysis script beside it unguarded.

**Tests.** Re-aimed: `the_kept_share_counts_consecutive_pairs_only` (members of non-zero weight left by the screen: a
screened-out member is left out), `an_older_tsv_parses_and_a_forced_row_round_trips` (the new header round-trips; a row
missing a new column parses empty), `the_forced_verdict_ends_in_one_class_line_within_thirty_lines`,
`every_forced_list_field_has_a_flag` (with `decimate`, `anchor_shift_mag`, `pedestal_radii_ref`). Kept:
`b2_uses_the_common_bar_only_on_a_joined_night`, `the_side_follows_the_rotation`,
`the_flip_term_is_fitted_and_not_judged`, `b4_is_unjudged_below_four_tenths_of_an_airmass`,
`b3_rms_is_about_the_median`, `an_identity_mismatch_names_its_field`. New: `b5_fits_one_night_at_a_time` (two synthetic
nights with k −0.07 and −0.11 and FWHM tracking airmass on one: each night's slope recovered within 1e-9, where one rig k
would hand that night a slope); `the_verdict_reads_the_reduced_zero_point` (rows with a `G_b` spread and a slope: B2 on
the reduced values); `b7_reads_the_map_refits`; `the_six_ref_referee_uses_the_pedestal`;
`b3_a_stack_slope_miss_six_r_does_not_echo_is_no_pass` (M2: three slopes printed, the line names the owner's limit);
`the_reduction_groups_by_exposure` (H2); `a_set_whose_list_costs_over_seventy_ms_is_named`. Regressions unchanged:
`verdict` without `--forced` on `.superpowers/sdd/2026-11-15-collab-v3-wave6-5/probe-results/final/*.tsv` and `verdict
--osc-c` on `out/r1/*.tsv`.

**Of Task 3 (3134eb8a; c7cf5c9c's flags; 7d26e764).** Kept: `forced`'s selection, the identity pass, `pier_side`,
`<x>.flags.txt`, the verdict's structure (one line per bar, ≤ 30 lines, the class line, a bar judged on no night is a
STOP), B2, B4, B6, B9, the decision rows, B3 per pier side (7d26e764). Changed: the row and the stars TSV, B1, B3's
referee and its three slopes, B5 and B8 (per night), B7, the cost line, the reduction before B2–B5 and B8, the
re-measures. Removed: the `c` columns (the 6F referee, the F5 emulation modes and `<x>.g.tsv` already in G2).

**Acceptance.** `cargo build --release --example zp_probe -p athenaeum-core` and `cargo test -p athenaeum-core
--example zp_probe` green; the two regressions; the controller's smoke `memrun.sh 3000 run-forced.sh smoke --limit 3`
(every set measured, a verdict written, peak memory printed).

### G4 — Phase A again

Controller first, as Task 4: the catalogue copy (`PRAGMA integrity_check` ok) and its counts; `/Volumes/bigbase3`
mounted; disk (`df`); no other heavy job or leftover loop (`claude agents`, `ps`); the REGISTRY row. Then, one at a time
and each through the guard (RR4):
1. `memrun.sh 3000 run-forced.sh g1` — the nine sets, the identity pass on every tenth frame (B9) with its
   anchor-shifted re-measures (G3 step 5);
2. `memrun.sh 3000 run-forced.sh g1d --decimate 10` — the decimated round (every 10th list star by key), reported, not
   judged;
3. `memrun.sh 3000` the probe's `forced` on one solved set outside the nine (the controller names its camera, filter
   and object from the catalogue copy before the run and notes it in the ledger), into `out/g1x/`, and `verdict --forced`
   on its TSV alone: a reported row, never judged (the nine are the sets the aperture rule was chosen on).
An opus judge writes `task-G4-report-r0.md`: every bar from the TSVs, every miss explained on the frames it sits on (a
stretched JPG and its list stars first), g1 against the expected table (saying that the screen's upper rule, the taper of H1, differs from
the emulation, which dropped those members, with the members tapered per night; one row at `--screen_taper_sigma 0` is the
emulation's rule, and one at `--pedestal_clip_rounds 10` shows the pedestal clip's convergence; the mosaic's √2 in the
low rule's noise allowance, G2 review L6, is told as the emulation's own), the cost, and these
figures, each by a script in `out/g1/` run through `memrun.sh`:
- the window slope (M3, H1): per set and round, the analytic − re-measured slope (median, MAD), κ after the reduction at
  ±0.5 mag, `(ZP(Δ) − s̄·Δ − ZP(0)) / Δ` (median, 10–90 %), the share of frames with a slope, and the faint edge's move
  per shift where it is set by count or cut by the catalogue;
- the exposure split (H2): per set, the reduced ZP's residual `ZP(Δ) − s̄·Δ − ZP(0)` at anchor shifts of ±0.44, ±1.0 and
  ±1.75 mag (median, 10–90 %), so the split by exposure can be lifted later;
- the decimated round (spec §5.3's sparse-list rules): per set the map stars, the first pass's member share, the refit
  kept share (min, p5), `poor_solve` among the frames valid in the full round (40 map stars stand on none, M5), whether
  the eight known bad frames stay refused; the rms of the reduced ZP's decimated − full difference about its median
  against the member count n, and the pedestal's part of it; the share of frames with a slope (in the window-slope item);
  its other refusals;
- the extra set of step 3: its bar figures as one reported row.

Expected (F5's emulation on run g0, candidate `max4020+p`, judged by the verdict as written before RR1):

| Bar | mono L (ATR, QHY L) | mono H | OSC Duo |
| ---- | ---- | ---- | ---- |
| B1 ≥ 0.80 | 0.902–0.949 | 0.909–0.944 | 0.891–0.960 |
| B2 | 0.0028–0.0097 (r0 0.022–0.036) | 0.0019–0.0034 | 0.0025–0.0069 |
| B3 rms ≤ 0.015, slope ±0.04 | LBN 582 10-18: 0.0114, −0.010 | no stack | 0.0073, −0.018; 0.0077, −0.038 |
| B4 \|Δk\| ≤ 0.02 | LBN 582 10-18 side 1: 0.0214, a miss | no stack | 0.0178 |
| B5 / B8 ≤ 0.03 | −0.017…+0.029 | IC 5146 10-04 +0.060 with the rig's k (a miss); with the night's −0.0004 ± 0.0032 (RR1); +0.007, −0.011 | −0.018…+0.005 |
| B6 | 0.946–1.000 (QHY 1.000) | 1.000 | 0.990–1.000 |
| B7 ≤ 0.25·FWHM_ref | 0.064–0.143 | 0.071–0.082 | 0.064–0.149 |
| B9 | not run in g0 | not run in g0 | not run in g0 |

Decision rows expected: ATR 0.015, QHY L 0.030, QHY H 0.083, Duo 0.33 (keep R8). With RR1 the expected class line is
STOP(missed B4) on mono L by 0.0014, which RR2 sends to the owner as a stated limit with Δk's standard error and the
stack's scatter about its line (0.0133 against ours 0.0057). Beside RR2 (spec §6): a B3 slope beyond ±0.04 against the
stack that 6r does not echo goes to the owner as a stated limit with the three slopes (M2; the Duo's 10-18 sat at
−0.0376 in the emulation); a false `poor_solve` in the decimated round goes to the owner with the round's figures (M5).

STOP conditions: a bar missed on any judged night (B4 included, RR2; B3 echoed or not, M2); a bar judged on no night of
its class; any B9 mismatch; the list alone above 70 ms a frame (median per set) on any set; a panic, an `internal`
refusal or a non-finite stored figure in any run (a code fault; a refusal's NULL σ, 5a's table, is not one); a guard
kill (the run is over its memory limit: no verdict, no rerun without the controller). GO only when none holds. A moved
constant reruns every set and is named in the report. Controller re-gate: B1 (members left by the screen), B2 (reduced)
and B6 recomputed from the TSVs by a script in `out/g1/`, run through the guard.
Disk: g0's per-star dumps (`out/g0/*.ap.tsv`, the bulk of its 1.3 GB) are deleted once the G4 report is committed to the
ledger; the rest of `out/` keeps the REGISTRY row's end date (the spec cites it as evidence until the close).

## Task 5a — storage

**Amended (F5, ruling R6).** Four columns, not three: `zp_refusal TEXT`, `zp_list_stars INTEGER`, `zp_window_bright REAL`,
`zp_window_slope REAL`; `zp_aperture_correction` is not added (spec §5.5). Tasks 5a–7 count **nine** `zp_*` columns
(`zero_point`, `zp_stars`, `zp_sigma`, `zp_version`, `zp_catalog` and the four); the r0 text said eight.

**Files.** `db/schema.rs` (`CREATE TABLE frame_analysis` :1975–2012, `zp_catalog` at :2007: the four after it; the column
loop :2092–2115, 6.5's five at :2098–2102); `models.rs` (`FrameAnalysis`
:1075–1128); `db/analysis.rs` (`upsert_frame_analysis` :4–51, `set_zero_point` :53–88, `clear_zero_point` :90–101, the
three SELECTs :103–188, `row_to_analysis` :314–348, tests :350–760); every `FrameAnalysis` literal
(`analysis/analyzer.rs:142–146`, `examples/star_probe.rs:1096`, and in tests `db/analysis.rs:379–413`,
`analysis/zero_point.rs:1787–1821`, `api/analysis.rs:1061–1110` and `:1400–1410`, `api/sync.rs:8560–8595`,
`plate_solve/service.rs:852` and `:932`, `collab/gate.rs:631`, `collab/frame_meta.rs:447`, `api/frame_sets.rs:2125–2131`
— `grep -rn "zp_catalog:" crates` lists them); `analysis/zero_point.rs` (`store_outcome` :1052–1081 adapted to the new
signature, 6.5's figures with the four new ones NULL); `ts_export.rs:150` (registry unchanged) and the regenerated
`src/types/models.ts` (:550–585); `sync/ingest.rs:938–951` (unchanged code); `sync/ingest_tests.rs:3036`.

**Approach.** `ZP_VERSION` does NOT change here (it changes in 5b with the writers, so no build of the branch ever
writes a 6.5-style value under version 3).
1. The four columns in `CREATE TABLE` and the loop; four `Option` fields on `FrameAnalysis` (`zp_refusal: Option<String>`,
   `zp_list_stars: Option<i64>`, `zp_window_bright: Option<f64>`, `zp_window_slope: Option<f64>`) with
   `#[serde(default)]` and docs, beside 6.5's five (FrameAnalysis serializes its fields under their snake_case names, as
   6.5's do); upsert, SELECTs and `row_to_analysis` carry them (indices 31–34).
   **Personal sync** (the wire is frozen and append-only, `docs/transfers/README.md`): the analysis summary travels as
   opaque JSON in the package manifest's NDJSON record (`package/manifest.rs:68`, `ManifestRecord.analysis:
   Option<serde_json::Value>`), decoded by `serde_json::from_value::<FrameAnalysis>` and stored by
   `upsert_frame_analysis` (`sync/ingest.rs:938–951`). The four keys are appended optional fields: an older reader
   ignores them (neither `ManifestRecord` nor `FrameAnalysis` denies unknown fields), a newer reader defaults them.
   No golden pin changes: the postcard pins of `sharing/wire_golden_tests.rs` (24 cases, `all_wire_types_are_pinned`)
   carry no analysis summary, and `MANIFEST_VERSION` stays 1. A frame synced from a build before this one carries no
   window columns: its version-2 ZP is not current anyway (`AnalysisDiffers`).
2. `zp_outdated: bool` on `FrameAnalysis`, `#[serde(default, skip_serializing_if = "std::ops::Not::not")]`: computed by
   the Analysis listing (5b), never a column; `row_to_analysis` sets false, so the sync wire never carries it. The
   generated TS line must read `zp_outdated?: boolean` (add `#[ts(optional)]` if ts-rs does not derive it from the
   serde attribute), so no frontend literal needs the key.
3. `set_zero_point(conn, frame_id, analysis_id, solve_id, figures: &StoredZeroPoint, catalog)` writes all nine
   columns (same compare-and-set); `clear_zero_point` clears all nine. The stored figures (read by 6a):

   | Outcome | zero_point | zp_stars | zp_sigma | zp_refusal | zp_list_stars | zp_window_bright | zp_window_slope |
   | ---- | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
   | measured | ZP | members | σ | NULL | n | G_b | s, or NULL under 10 bright-ramp members |
   | too_scattered | NULL | members | σ (NULL when the pedestal could not be fitted) | `too_scattered` | n | G_b | NULL |
   | too_few_stars | NULL | members | σ (NULL with no positive flux) | `too_few_stars` | n | G_b | NULL |
   | poor_solve | NULL | NULL | NULL | `poor_solve` | n | G_b | NULL |
   | no_anchor, no_exposure, unreadable_solve, colour_camera, frame_verdict | NULL | NULL | NULL | the code | NULL | NULL | NULL |

   Every judged row carries `zp_version` and `zp_catalog`. `members` = members of non-zero weight left by the screen;
   `n` = the list before it is measured.
4. Regenerate `src/types/models.ts` (`TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`).

**Tests.** `migration_adds_the_zero_point_columns_and_keeps_rows` (:654) and `ZP_COLUMNS` (:647) → nine;
`frame_analysis_round_trips_the_zero_point` (:676) → nine; `an_old_frame_analysis_json_decodes_without_a_zero_point`
(:704) → nine keys; `set_zero_point_writes_the_value_or_nulls_and_the_version_and_catalogue` (:722) → every row of the
table; `clear_zero_point_nulls_all_five_columns` (:744) → `clear_zero_point_nulls_all_nine_columns`;
`zp_outdated_never_reaches_the_wire_or_the_database` (false: the key is absent from the JSON; a JSON carrying `true`
upserts and reads back false); `an_analysis_summary_with_and_without_the_c6_keys_ingests` (ingest_tests.rs:3036) with
and without the four keys (a `future` summary carrying an unknown `zp_*` key still ingests); the wire golden test
`wire_format_is_frozen` unchanged and green; `ts_contract` green.

**Acceptance.** `cargo test -p athenaeum-core --lib db::analysis`, `--lib sync::ingest`, `--lib sharing::wire_golden_tests`,
`--test ts_contract` green; `cargo check --workspace --all-targets` green.

## Task 5b — Analysis writes the zero point; every 6.5 writer goes

**Amended (F5).** "The nine figures" below are 5a's table, and `store_outcome` writes them from G2's
`ForcedOutcome` (members, σ, the list size, `G_b`, the window slope, the refusal code). `poor_solve` is one of R5's
stored refusals: it does not change on a re-read, so a frame refused `poor_solve` is not re-selected by Analyze until a new
solve clears its `zp_*` (`clear_zero_point` on a new or deleted solve, as 6.5). New test:
`a_poor_solve_is_stored_once_and_a_new_solve_measures_again`.

**Files.** `analysis/zero_point.rs` (`ZP_VERSION` :36–38; `is_colour_frame` :1003–1009; `load_facts` :1011–1033;
`store_outcome` :1052–1081; removed: `refresh_zero_point_with` :1083–1182, `needs_refresh` :1194–1208,
`refresh_many_stats` / `refresh_many_with` / `refresh_listed_with` / `ensure_zero_points_with` /
`recompute_zero_points_with` :1251–1329, `CAN_BE_MEASURED` / `stale_zero_point_frames` / `stale_among_frames(_counted)`
:1331–1393, `NotReady::NoStarMetrics` :924–926; kept: `ZpPassStats` :1210–1249 for 5c, `catalog_cone` :1184–1192,
`BACKFILL_CHUNK` :905 renamed `PASS_CHUNK`); `analysis/analyzer.rs` (beside `analyze_frame` :162–174);
`api/analysis.rs` (removed: :46–160 the triggers, :162–205 `backfill_zero_points`, :303–354 `installed_identity` /
`spawn_zero_point_backfill`; the guard :207–301 stays for 5c;
`row_is_stale` :436–442; `analyze_frame_set` :463–768 — selection :541–588, worker :617–678, persist :709–728, trigger
:732–737; `get_analysis_for_frame_set` :785–796; `persist_frame_analysis` :805–821; `get_frame_star_metrics` :834–947 —
fresh :872–889, on demand :891–919, trigger and read-back :921–938); `api/plate_solve.rs` (:121–122 the tier-install
backfill, :694–700 the solve trigger); `crates/athenaeum-tauri/src/commands/core.rs:143–146` and
`crates/athenaeum-web/src/main.rs:285–288` (the launch backfill, both hosts in one commit); `api/collab.rs`
(`gate_app_data_dir` :760–767 kept; removed `GATE_INLINE_ZERO_POINTS` :769–772, `GateMeasure` :774–804,
`rules_judge_transparency` :806–809, `measure_gate_stale_with` :811–858, `ensure_gate_zero_points` :860–883;
`project_gate` :1176–1250 and its call :1245; callers :1104, :1691, :1776, :2159, :4877).

**Approach.**
1. `ZP_VERSION` = 3 (the doc says what 3 is: the forced list, a weighted ratio of aperture sums at catalogue positions
   mapped onto the frame's own stars). `is_mosaic(bayerpat, source_channels)` beside
   `is_colour_frame`: the trimmed, upper-cased pattern is RGGB, BGGR, GRBG or GBRG and `source_channels == Some(1)`
   (the reader's `RawPlanes::Cfa`), O4's one test for 5b and 6a. `zp_outdated(analysis, solved, catalog) -> bool` =
   a catalogue is installed AND the frame has a solve AND (`zp_version ≠ 3` OR `zp_catalog ≠` its identity): the one
   predicate of Analyze's selection and the listing.
2. `analyzer::analyze_frame_with_zero_point(path, config, pool, config_hash, zp: Option<&ZpFrame>)`: read, Task 2's
   `measure_with_zero_point`, `map_measurement`, the nine figures into the row (`zp_version` 3, the identity). `ZpFrame
   { wcs, solve_id, exptime, epoch, bayerpat, catalog: &ZpCatalog }`. A colour file that is not a mosaic stores
   `colour_camera`; a forward SIP with no reverse stores `unreadable_solve`; no exposure stores `no_exposure`. The
   list part runs under `catch_unwind` (the 6.5 `isolated_trigger` precedent, api/analysis.rs:62–71): a panic leaves
   the nine NULL with one `warn`, never fails the analysis. `analyze_frame` stays for `get_collab_frame_star_metrics`
   (:955) and the probes.
3. `analyze_frame_set`: the selection opens the catalogue once (`zero_point_catalog`; none → no list, `debug` once)
   and reads every frame's solve and header facts in one query beside the frame list (never one per frame); with
   `force = false` it keeps `row_is_stale(existing) || zp_outdated(existing, solved, identity)`; the worker calls
   step 2; the persist writes the zero point in the same upsert only while the frame's solve is still the one read
   (`SELECT id FROM plate_solves WHERE frame_id = ?` inside the transaction), else the nine go NULL (`debug` "zero
   point superseded"). The trigger at :732–737 goes.
4. `get_analysis_for_frame_set` fills `zp_outdated` per row (the installed identity by
   `transparency::installed_catalog_identity`, stats only; solve presence by one chunked query).
5. `get_frame_star_metrics`: the fresh path is unchanged, so a row whose only fault is an outdated zero point is
   served as stored (Blink never re-reads a file for it, §5.4); the on-demand path reads the frame's solve facts and
   the catalogue and calls step 2; :921–938 go (the upsert carries the zero point).
6. Removed: the launch backfill in both hosts; the tier-install backfill (a tier install measures nothing, F6); the
   solve trigger at plate_solve.rs:694–700 (5c puts the pass there); the gate's lazy refresh. `project_gate` loses its
   `measure` argument and reads the identity alone (`gate_app_data_dir` + `installed_catalog_identity`): a frame
   without a current zero point is published with no loss and not blocked (F6).

**Tests.**
- `analysis_with_a_solve_stores_a_current_zero_point` (a synthetic FITS and a synthetic cone: `zp_version` 3, the
  identity, the nine figures by the table).
- `the_analysis_rows_are_identical_with_and_without_the_zero_point` (every `FrameAnalysis` field but `analyzed_at` and
  the nine, every `StarMetric`): B9's core half.
- `is_mosaic_mirrors_the_reader` (four patterns in any case with blanks, one channel → true; `RGB`, empty, `None`, three
  channels → false); `a_mosaic_is_measured_and_a_debayered_file_stores_colour_camera`.
- `a_frame_without_a_solve_stores_no_zero_point`; `a_forward_only_sip_stores_unreadable_solve`;
  `a_missing_exposure_stores_a_refusal`; `a_new_solve_during_the_analysis_drops_the_zero_point`;
  `a_panicking_list_does_not_fail_the_analysis`.
- `analyze_selects_solved_frames_whose_zero_point_is_not_current` (one set holding a current, a version-2, an
  other-catalogue, an unsolved and a stale-config frame: exactly the version-2, other-catalogue and stale ones run,
  count asserted); `analyze_selects_nothing_for_the_zero_point_without_a_catalogue`.
- `the_listing_flags_rows_whose_zero_point_is_not_current`;
  `a_fresh_row_with_an_outdated_zero_point_is_served_without_reanalysis` (beside :1113).
- `the_gate_reads_stored_zero_points_only` (a project with a `transparency_loss` rule and frames without a current zero
  point: no `zp_*` column changes, no cone is opened, every such frame has no loss and its reason);
  `a_publish_run_announces_frames_without_a_current_zero_point_unblocked`.
- Kept with nine columns: `deleting_a_solve_clears_the_zero_point` (zero_point.rs:2069),
  `a_new_solve_clears_the_zero_point_even_when_the_analysis_is_stale` (:2081); kept as is:
  `no_catalogue_leaves_the_analysis_saved_and_logs_once` (api/analysis.rs:1302),
  `a_single_frame_analysis_commits_its_stars_with_its_row` (:1368, plus the zero point in the same commit).

**Acceptance.** `cargo test -p athenaeum-core --lib` filtered to `analysis`, `api::analysis`, `api::collab`,
`db::analysis` green; `cargo check --workspace --all-targets` green; `grep -rn
"spawn_zero_point_backfill\|backfill_zero_points\|ensure_gate_zero_points\|refresh_zero_point_with\|stale_zero_point_frames\|zero_points_after_"
crates/` empty.

## Task 5c — the pass after a solve batch

**Amended (F5).** "The nine `zp_*`" below are 5a's. R7 stands (the pass is Analyze's own worker over the newly
solved ids); the text below predates it where they differ, and R7 wins. Step 4's dictionary paragraph is amended in
place (the F5 plan review).

**Files.** `api/analysis.rs` (the guard :207–301 renamed `SolvePassGuard` / `PassRelease`); `api/plate_solve.rs:694–700`;
`crates/log-mcp/src/query.rs:28`, `:44`; the dictionary
`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md:70` (a "Dictionary extension (forced list)" paragraph
after 6.5's).

**Approach.**
1. `spawn_solve_zero_point_pass(ctx, frame_ids)`: one pass at a time; a request while one runs appends its ids to a
   pending set the running pass takes before it ends (the guard keeps its `abandon` on a panic and its release on
   every exit path). A thread named `zero-point-solve-pass`.
2. `solve_zero_point_pass(ctx, ids) -> Result<ZpPassStats>`: no catalogue → `debug`, return; it takes the global
   compute queue as `analyze_frame_set` does (:502–539), so it never runs beside Analyze or a stack; per chunk of 200
   it keeps the frames with a current analysis (not `row_is_stale`) and a solve; per frame it reads the analysis id
   and solve id, materialises a zip-resident file (`zip_store::materialize::materialize_one`), runs 5b's analyzer and
   writes the nine `zp_*` alone through `set_zero_point` (compare-and-set on both ids: a re-analysis or a new solve
   meanwhile wins). An offline or unreadable file is a `debug` (`frame_id`, `path`, `error`) and counted; the pass
   says one `warn_if_unmeasurable("solve")`. Span `zero_point_solve_pass` (id field `catalog`), one `info` "zero point
   solve pass done" (`frames`, `zp_measured`, `duration_ms`).
3. `api/plate_solve.rs:694–700` spawns the pass with the persisted frame ids when the batch stored them.
4. log-mcp: the operation kind `zero_point_backfill` → `zero_point_solve_pass`. The dictionary paragraph, after the
   forced-list one at :72: the new fields (`zp_list_stars`, `zp_refusal`, `zp_window_bright`, `zp_window_slope`;
   `zp_aperture_correction`, `zp_anchor` and `zp_kept` are not built, G2's "forced list measured" line carries
   `members`), the codes (`no_anchor`, `poor_solve`, `unreadable_solve`, `frame_verdict`; `no_aperture_correction` is not
   built), `pass = "solve"`, the span, the lines 5b and 5c removed (backfill, trigger, gate measurement).

**Tests.** `the_solve_pass_measures_solved_frames_with_a_current_analysis` (the nine written; a stale analysis
skipped); `the_solve_pass_skips_an_offline_file_and_warns_once`; `a_reanalysis_during_the_pass_wins`;
`the_solve_pass_runs_one_at_a_time_and_takes_queued_frames` and `a_solve_pass_that_dies_releases_the_guard` (pure
guard tests, replacing `the_same_identity_does_not_start_twice` :1450, `a_new_catalogue_identity_allows_another_backfill`
:1323, `a_backfill_that_dies_releases_the_guard` :1341); log-mcp's kind test.

**Acceptance.** `cargo test -p athenaeum-core --lib api::analysis` and `--lib api::plate_solve`, `cargo test -p
log-mcp` green; `cargo check --workspace --all-targets` green.

## Task 6a — loss reasons, the reduction on read, mosaic references, the gate's O7

**Amended (F5).** Step 2 reads the stored code (R6) and gains `poor_solve`; step 3, the reduction, is new; the hub, the
portal, the registry copies and the manifest key `transparencyLoss` are untouched (the metric and its unit do not change).

**Files.** `analysis/transparency.rs` (module doc :1–18; `LossReason` :44–95; `AnalysisFacts` :134–145; `FrameRow`
:147–158; `load_frame_rows` :198–254 with the star-row column :208–209; `Pool` :293–318; `RigPools` :329–334;
`load_pools` :363–442 with its colour filter :425; `resolve_reference` :444–460; `judge` :472–521; tests :613–1379,
fixture `Spec` :620–700); `models.rs:1474–1482` (`NOT_MEASURED_REASON`
unchanged: a frame nobody asked about); `collab/gate.rs` (`GateFrameInput` :93–123, `evaluate_frame` :176–371 — the
`transparency_loss` metric :279 and the no-value branch :316–332 — tests :568+, literals :635–680); `api/collab.rs`
(`frame_gate_inputs` :487–700: the frames SELECT :526–551 gains `bayerpat`, the literal :676; the test literal :7107).

**Approach.**
1. `LossReason`, first match wins: `NotAnalysed`, `StaleAnalysis`, `NotSolved`, `ColourCamera` (a colour frame that is
   not a mosaic; sentence unchanged), `NoCatalogue` ("the star catalogue is not installed — install it to measure
   transparency"), `NotMeasured` (never judged: "transparency not measured yet — run Analyze on its frame set", §5.6),
   `AnalysisDiffers` (judged at another `ZP_VERSION` or catalogue: "its analysis differs from the current one — run
   Analyze on its frame set", F6), `Refused(code)`, `NoRig`, `NoReference`. `NoStarMetrics` and `NoZeroPoint` go, with
   the star-row column of the SELECT.
2. `Refused` reads the stored code (`zp_refusal`, ruling R6; the derivation from the figures goes) and, for
   `too_few_stars`, `zp_list_stars`: `poor_solve` → "the stars are not where the plate solve puts them — solve this frame
   again" (spec §5.6; if Task 7 check 9's new solve is refused again, the sentence and its test lose "— solve this frame
   again" in a fix round, M5); `too_few_stars` with a list under 50 → "too few catalogue stars in the field — a deeper star
   catalogue tier may help"; `too_few_stars` otherwise → "too few of the field's catalogue stars could be measured";
   `too_scattered` → "the star brightnesses scatter too widely for a reliable zero point"; `no_exposure` → "no exposure
   time in the header"; `frame_verdict` → "no stars to measure on this frame (trailed, out of focus or empty)";
   `no_anchor` and `unreadable_solve` → "too few catalogue stars matched to set the measurement — a deeper star catalogue
   tier may help"; `colour_camera` → `ColourCamera`'s sentence. `no_aperture_correction` is not built.
3. **The reduction on read** (spec §5.6, FL8): `load_frame_rows` (:204–209) and `load_pools` (:384–402) read
   `zp_window_bright`, `zp_window_slope` and `f.exptime` beside `zero_point` (`load_frame_rows` also `zp_refusal`,
   `zp_list_stars`): no new statement. The exposure, rounded to 0.1 s, joins the pool key (H2): a set pool is one
   (rig, frame set, exposure), the rig-wide pool one (rig, exposure) (`RigPools` keyed by it, `resolve_reference` given
   it); a frame with a ZP always has an exposure (`no_exposure` otherwise). A `Pool` (:293–318) keeps per frame
   `(ZP, G_b, s)`; `Pool::reference` builds `zero_point::common_window` over its members (G2), reduces each ZP by it and
   takes the 90th percentile of the reduced values; the rig-wide pool is filled with each (set, exposure) pool's reduced
   values (each reduced within itself first), never with raw ZPs. An asked frame's loss is `reference −
   CommonWindow::reduce(its ZP, its G_b)` with its own (rig, set, exposure) pool's window; a frame in no set, or whose
   set pool is empty, reads its own ZP. A version-3 frame whose `zp_window_bright` is NULL (none should exist) reads its
   own ZP and counts in no window. `Transparency::zero_point` carries the reduced value, the one `loss_mag` is taken from
   (owner-facing ruling after the plan review: a row's two numbers are one quantity); the stored columns are untouched
   and the read writes nothing.
4. References: a mosaic with a current zero point joins the pools (:425 keeps out `is_colour_frame && !is_mosaic`); the
   rig-wide fallback stays off for mosaics (R8).
5. O7: `GateFrameInput` gains `colour_without_filter` = `is_mosaic(bayerpat, source_channels)` and an empty trimmed
   `FILTER`; `evaluate_frame` then lists `transparency_loss` with no value, `pass: None`, reason "colour frame without
   a filter name: its clear-sky level may mix filtered and unfiltered nights"; nothing blocks it. The frame's own
   `Transparency` (column, panel, manifest key) is unchanged.

**Tests.** transparency: `a_debayered_colour_frame_says_so` (was `a_colour_camera_frame_says_so` :1114);
`a_mosaic_frame_reads_its_loss_and_feeds_the_reference`; `a_synced_analysis_with_a_current_zero_point_reads_its_loss`
(was `a_synced_analysis_says_to_analyse_here` :1132); `a_version_two_zero_point_says_the_analysis_differs`;
`a_zero_point_from_another_catalogue_neither_counts_nor_shows` (:1158, now `AnalysisDiffers`);
`no_installed_catalogue_says_so` (was `no_installed_catalogue_gives_not_measured` :1203); `each_refusal_says_why` (every
table row, `poor_solve` among them); `each_state_has_its_reason` (:1264) and `the_sentences_are_the_binding_ones` (:1363)
extended; `five_thousand_frames_cost_few_statements` (:1339) unchanged (the window columns ride the same statements).
The reduction: `the_loss_reads_the_reduced_zero_point` (a set of 40 frames over two nights with a `G_b` spread of 0.3 mag
and a slope of −0.15: every loss equals the reference less its reduced ZP to 1e-9, the reference the 90th percentile of
the reduced values); `a_frame_set_of_one_frame_reads_its_own_zero_point`; `the_rig_wide_fallback_reduces_each_set_first`
(two sets with different windows: the rig pool holds each set's reduced values); `a_frame_without_a_slope_is_reduced_with_its_sets_median`;
`a_set_without_any_slope_reads_unreduced`; `the_read_writes_nothing` (every `zp_*` column byte-identical after a read);
`two_exposures_in_one_set_are_two_references` (H2: one set at 120 s and 300 s gives two references, neither exposure
reduced by the other's window, and two rig-wide pools); `the_cell_shows_the_reduced_zero_point`
(`Transparency::zero_point` equals the reduced value to 1e-9, the stored column unchanged). Gate:
`an_osc_frame_without_a_filter_gets_no_transparency_verdict`, `an_osc_frame_with_a_filter_is_judged`,
`a_mono_frame_without_a_filter_is_judged`; the transparency gate tests (:1229–1300) unchanged;
`no_catalogue_does_not_fail_the_gate` (api/collab.rs:7212) with the new sentence.

**Acceptance.** `cargo test -p athenaeum-core --lib` filtered to `analysis::transparency`, `collab::gate`,
`api::collab`, `api::frame_sets` green; `cargo check --workspace --all-targets` green.

## Task 6b — the Analysis tab's count (frontend-dev)

**Files.** `src/components/LightsAnalysisView.tsx` (`loadAnalysisData` :257–270, the count :541–543, the Analyze /
Re-Analyze buttons :662–689); a new pure helper `src/components/analysisCount.ts` with `analysisCount.test.ts`;
`src/types/models.ts` (regenerated in 5a); `src/components/collab/project/transparency.ts` (`formatZeroPoint` :19–28,
read by `FramePanel.tsx:204`) and `transparency.test.ts` (:19–36). The loss and its reason need no change
(`FramesTableRows.tsx`, `collab/project/frames.tsx` and `formatTransparency` show core's values as they come).

**Approach.** A row with `zp_outdated` does not count as analysed, so a fully analysed set whose zero points are not
current shows the Analyze button, which runs `force = false` and re-reads only those frames. The count text stays
`{analysed}/{total}`; when some rows are outdated the count carries a `title`: "{n} analysed before the current
transparency measurement". Design tokens only; no `@tauri-apps` import; logic in the helper.
The zero point cell (owner-facing ruling after the plan review): `formatZeroPoint` drops its `± zpSigma` part
(`21.34 · 87 stars`): since F5 `zp_sigma` is the field's per-star scatter (0.13–0.59 mag) beside a ZP good to 0.005,
and the value shown is now the reduced one (6a). Its argument type and the wire keep `zpSigma`.

**Tests.** `analysisCount.test.ts`: all current → all analysed; one outdated → not, and the count leaves it out; no
rows → 0. `transparency.test.ts`: the `formatZeroPoint` expectations lose `· ±0.04` (`21.34 · 87 stars`,
`21.34 · 1 star`, and `21.34` alone with no star count). `npm run build` green; `npx vitest run
src/components/analysisCount.test.ts src/components/stacking src/components/collab` green.

## Task 7 — acceptance on a catalogue copy (headless web)

Controller first: a second copy `~/.athenaeum-acc/2026-11-16-osc-zp/accept/athenaeum.db` (`sqlite3 -readonly <dev db>
".backup <copy>"`, integrity ok, `account.*` and `sync.*` settings deleted, `monitoring.enabled_global = false`, no
pending `file_operations`; REGISTRY row updated); the tier files beside it as read-only symlinks; the release
`athenaeum-web` of the branch on port 3399 (memory `reference_headless_web_smoke_harness`). Checks, each with its
evidence (route, SQL diff, log query):
1. Launch reads no pixel: after 60 s idle no zero-point line in the log and no `zp_*` change (SQL).
2. Frame set 109 (LDN 1272): the listing flags exactly the solved lights whose zero point is version 2 or none;
   `analyze_frame_set(force = false)` re-reads exactly those (result count = flagged count); afterwards every solved
   light has version 3 and a value or a stated refusal; every non-`zp_*` column of every re-analysed row and every
   `star_metrics` row byte-identical to before (SQL dump diff without `id` and `analyzed_at`); the listing then flags
   none.
3. A set solved after analysis fills in by the solve pass: delete one analysed frame's solve
   (`delete_plate_solve_for_frame`), solve it again (`plate_solve_batch`): the pass line and its nine columns, no
   other column moved.
4. An offline file says "run Analyze": a frame whose path no longer resolves (in the copy) runs through the pass: one
   `debug`, one `warn` with the count, and `get_frame_set_detail` reads "transparency not measured yet — run Analyze on
   its frame set".
5. An existing stack is untouched: `get_stacking_plan` for set 109 identical before and after (no stale stage).
6. A low-altitude frame reads its loss (≥ 0.2 mag at airmass ≥ 2.5).
7. A rig without a reference is not blocked and says why; an OSC frame with an empty `FILTER` gets O7's reason in the
   gate rows, its loss still in the column; a publish run of a project with a `transparency_loss` rule reads no file
   (log).
8. Blink does not re-read: `get_frame_star_metrics` on a frame with an outdated zero point returns the stored row (no
   analysis line in the log).
9. A bad solve says so (F5): a frame whose stored solve G4 refused `poor_solve` (ATR LDN 1272 frame 29375 when the copy
   holds it, else one the G4 report names) stores `zp_refusal = 'poor_solve'` and `get_frame_set_detail` reads "the stars
   are not where the plate solve puts them — solve this frame again". Then it is solved again
   (`delete_plate_solve_for_frame`, `plate_solve_batch`) and the solve pass measures it (M5: no report shows that a new
   solve cures it): a valid ZP keeps the sentence's advice; refused `poor_solve` again, the sentence loses "— solve this
   frame again" (6a's sentence and its test in a fix round; the owner told).
10. The reduction reads, never writes (FL8): set 109's losses equal a script's `reference − (ZP − s̄·(G_b − P))` over its
    stored columns to 1e-9, and every `zp_*` column is byte-identical before and after the reads of checks 2–9 that
    write nothing.
Owed by a human: the real window (the Analysis tab's count and Analyze, the cell sentences). The copy is deleted once
`task-7-report-r0.md` is committed to the ledger.

## Task 8 — docs, final review, merge

**Files.** `docs/catalog/README.md:104–164` (the zero point section rewritten: the list, the map and `poor_solve`, the
aperture sums and the pedestal, the weighted ratio of sums, where it runs, currency, the nine stored columns, the
reduction on read, what reads it, what was removed); `CLAUDE.md:135` (the Transparency line: a ratio of aperture sums
over a fixed list of catalogue stars at positions mapped onto the frame's own stars, measured inside Analysis' read;
nothing reads pixels at launch, a tier install or the gate; a reader reduces a frame set to one window before it compares
frames, and stored values are never rewritten); `docs/transfers/README.md:383`, `:387` (the gate reads stored values only;
O7); `docs/logging/README.md:60–63` (the operation kind); the dictionary paragraphs from G2 and 5c re-read;
`docs/superpowers/open-items.md:399–409` (6.5's section: "Owed: OSC" :405 replaced by this cycle's owed items — a window
move beyond 0.5 mag (haze; a cloudy night), a window cut by the catalogue's depth, a list under ~500 stars (G4's
decimated round's numbers), under 16 bit, long focal length, high latitude, the faint-edge extension, the real-window
look, a stated limit the owner granted in G4 if any; and the lead for the solver's side: stored SIP tables carry
constant and linear terms the projection drops, F4's `out/f4/sipconv.py`; "Known, not fixed" :409 updated: the reason
is the stored code, `unreadable_solve` shares `no_anchor`'s sentence); pointers to the forced-list spec at the end of §9 of
`2026-11-15-collab-v3-wave6-5-zero-point-design.md` and at the top of `2026-11-16-osc-zero-point-design.md`; a
`RELEASE_NOTES.md` line proposed, applied only on the owner's yes.

Final review: fable, the whole branch against `main` and the rustafits diff (`git -C rustafits diff
main...osc-green-aperture`); findings folded, round 2 if any. Gate: the full core run (memory
`feedback_full_core_test_before_push`, its known flakes), `cargo check --workspace --all-targets`, `npm run build`,
vitest, rustafits tests. Merge: rustafits `osc-green-aperture` into rustafits `main` with a merge commit (no trailer);
the branch's pointer bumped to that merge commit; the main checkout's submodule fetches the merged rustafits commits
first; the branch into local `main` with `--no-ff`; the worktree removed with `git worktree remove --force` alone
(never `submodule deinit` in it). Disk close-out (the job folder's copies and probe outputs deleted once the reports are
committed; REGISTRY row closed). Push: 6.5 and this together, on the owner's word.

## 6.5 code that changes (index)

- `analysis/zero_point.rs`: `ZP_VERSION` 3; `is_mosaic` and `zp_outdated` added; the stored-rows half removed
  (:1083–1393 bar `catalog_cone` and `ZpPassStats`); `NotReady::NoStarMetrics` gone; `store_outcome` writes nine columns;
  `measure_zero_point` kept as the anchor (5b, 2, G2).
- `analysis/analyzer.rs`: `analyze_frame_with_zero_point`, `measure_with_zero_point` (2, G2, 5b).
- `analysis/transparency.rs`: reasons by stored code, refusal sentences, the reduction on read, mosaic references,
  `NoStarMetrics` and `NoZeroPoint` gone (6a).
- `db/schema.rs`, `db/analysis.rs`, `models.rs`, `ts_export.rs` → `src/types/models.ts` (5a).
- `api/analysis.rs`: triggers, backfill and its spawner removed; selection, persist, listing, on-demand path changed;
  the solve pass (5b, 5c).
- `api/plate_solve.rs`: tier-install backfill removed; the solve pass spawned (5b, 5c).
- `api/collab.rs`: the gate's lazy refresh removed, `project_gate` and its five callers, O7's input (5b, 6a).
- `collab/gate.rs`: O7 (6a).
- `crates/athenaeum-tauri/src/commands/core.rs`, `crates/athenaeum-web/src/main.rs`: the launch backfill removed (5b).
- `crates/log-mcp/src/query.rs`: the operation kind (5c).
- `src/components/LightsAnalysisView.tsx` (6b).

## 6.5 tests whose meaning changes

- `analysis/zero_point.rs`: the pure tests :1505–1762 stay unmodified and now pin the anchor. Removed with their code:
  `refresh_measures_and_stores` :1901, `a_stale_analysis_is_not_measured` :1916,
  `a_frame_without_a_solve_or_an_analysis_is_not_ready` :1929, `a_new_analysis_clears_the_zero_point_until_the_trigger_
  runs` :2005, `a_new_solve_recomputes_the_zero_point` :2025, `the_solve_trigger_recomputes_a_current_zero_point_and_
  the_analysis_trigger_skips_it` :2046, `a_synced_analysis_without_star_rows_is_not_measured` :2143,
  `ensure_skips_frames_that_are_current` :2162, `a_zero_point_from_another_catalogue_is_recomputed` :2177,
  `the_backfill_cursor_walks_the_stale_frames_in_chunks` :2190, `a_zero_point_of_an_older_version_is_stale` :2237,
  `the_gates_stale_selection_is_one_query_per_chunk` :2254, `a_synced_analysis_without_star_rows_is_never_stale` :2281.
  Rewritten against the analysis path: `a_missing_exposure_stores_a_refusal` :1946, `a_colour_camera_frame_is_not_
  measured` :1957 (a mosaic is measured), `a_debayered_colour_frame_is_not_measured` :1972,
  `a_forward_only_sip_is_not_measured` :1991 (now stored `unreadable_solve`), `a_write_after_a_reanalysis_is_dropped`
  :2106, `a_trigger_error_does_not_fail_the_analysis` :2206, `a_panicking_cone_does_not_fail_the_trigger` :2219. Kept
  with nine columns: :2069, :2081.
- `api/analysis.rs`: `a_new_catalogue_identity_allows_another_backfill` :1323, `a_backfill_that_dies_releases_the_guard`
  :1341, `the_same_identity_does_not_start_twice` :1450 → the solve pass guard; :1113, :1302, :1368 keep their names.
- `api/collab.rs`: `a_project_without_the_rule_measures_nothing_in_the_gate` :7358, `an_interactive_gate_keeps_its_
  inline_limit` :7417, `a_publish_run_measures_every_selected_frame` :7456, `the_inline_limit_applies_to_the_selection`
  :7474 removed (with `seed_stale_zero_points` :7338, `NOT_MEASURED_YET` :7403); replaced by
  `the_gate_reads_stored_zero_points_only`.
- `analysis/transparency.rs`: :1114, :1132, :1158, :1203, :1264, :1363 as in 6a.
- `db/analysis.rs`: :647, :654, :676, :704, :722, :744 as in 5a. `sync/ingest_tests.rs:3036` as in 5a.
- `api/frame_sets.rs`: `frame_set_detail_does_not_measure_anything` :2282 checks nine columns.

## Readings of the spec this plan makes

Readings 1–14 were made of the r0 spec. Since F5: 2 and 3 concern the list's `c` and are moot (G1 removes it; R2's own
detection still keeps the ring sky clean for the map fits); 4 is replaced by R6 (the code is stored); 7 is folded into
spec §6 (the ATR L common set is LBN 582); 8 now concerns the map fits' ring sky only (an aperture whose annulus leaves the
frame is no member). Two more:
15. Spec §5.2 says the aperture sum is `star_measure`'s, beside the growth `c` reads: G1 adds `photometry::aperture_sums`
    with the growth's pixel walk and present-pixel rule but scales a mosaic sum by area, not by the model's green share
    (`growth_curve_in`'s masked form needs a fitted model, which a list star does not have).
16. Spec §5.3's "on a sparse list" thresholds (40 map stars, 20 pedestal points, 10 bright-ramp members since H1) are
    this amendment's, not F5's emulation: G4's decimated round reports how they behave (its figures listed in G4); none
    of them moves a bar.

1. §5.2 (spec:126) names `fit_stars_at`'s path, which runs no saturation verdict and masks no detected neighbour in the
   ring sky (rustafits mod.rs:495–517, fit.rs:2279–2312); Task 1 composes the budget path's halves instead.
2. §5.2 (spec:132) caps `c` at 256 while the cap is the public constant `CORRECTION_STARS` = 64 used inside
   `aperture_correction` (photometry.rs:618, :1313), and `report_aperture` ties masked growth to "never applied"
   (fit.rs:2029–2033, :2632): Task 1 adds crate-private fields.
3. A list star's own detection fails `c`'s isolation test at distance ≈ 0 (photometry.rs `aperture_correction`): Task 1
   drops coinciding detections from the neighbours. The spec is silent.
4. §5.3 (spec:142) lists stored refusals; §5.5 (spec:159–162, FL4) adds no code column. Task 5a derives the reason from
   the stored figures and writes kept and σ on a refusal (NULL in 6.5); `no_anchor` and `unreadable_solve` share one
   sentence.
5. 6.5 stores nothing for an unreadable solve (zero_point.rs:625–635); with §5.4's selection (spec:149–150) such a frame
   would be re-read by every Analyze. Task 5b stores `unreadable_solve` and `colour_camera` at version 3.
6. B9 (spec:195) asks every `PlaneMeasurement` field bit-identical; `timings` and `sub_steps` are wall time
   (mod.rs:639–643) and are exempt.
7. §6 (spec:180–181) names `ATR2600M` L LDN 1272 2025-10-18 with 82 frames: run 9's common set holds 82 mono rows
   (`out/common-merged.tsv`), the catalogue copy 65 lights with an empty `FILTER` (night key 2025-10-17). The probe
   reads the 65 with `--filter -`.
8. §5.1 (spec:112) says 13·`FWHM_ref` lets the ring sky fit; the ring is at 10–12 × the frame's FWHM, so it fits only
   while that FWHM ≤ 1.08·`FWHM_ref` (10-18 reaches 3.12 px against 2 px). Kept as written (seeing-free by design); a
   truncated ring reads its present pixels, `c`'s 12 F footprint test drops edge stars, the probe reports
   `ring_missing`.
9. §5.1 (spec:107–108) "validity relaxed to ≥ 10 matches": read as 10 matches and no standard-error limit.
10. §5.1 (spec:109) "a star whose peak would reach 70 % of full scale": read as the amplitude above the sky (the
    emulation, `out/4d/emul4d.py:124`); a mosaic whose budget reports no `c` anchors at `c` = 1.
11. §5.6 (spec:171) and F6 (spec:214–216) give two sentences: Task 6a keeps both (never measured; measured by an older
    version or catalogue) and adds one for no catalogue.
12. B8 (spec:194) "not above its value": compared with today's zero point computed by the same run on the same frames.
13. B4's pier side is derived from the solve's field rotation; the solve pass takes the compute queue (not in the spec).
14. FL5 (spec:31–32) says 6.5's Z1–Z5 stand, while F0 measures mosaics, which Z5 had deferred: read as Z5's OSC
    deferral being superseded.

## Review rulings (opus review of spec + plan, `review-forced-spec-plan-opus.md`, 2026-11-16) — they override the tasks above

Controller rulings; numbers in the ledger. Each task's implementer reads this section first.

- **R1 (#1, Task 1)** — no change: `fit::Correction`'s `Default` is hand-written (`max_c_stars` = `CORRECTION_STARS`,
  fit.rs:2063); the F1 review confirms it.
- **R2 (#2, Task 1 fix round)** — a list star's own detection is the nearest one within
  max(`max_centre_offset_px`, 1.0·F_field), not within `max_centre_offset_px`. Test: a mosaic star whose own detection
  sits 1.3 px off is kept (no `Blend`) and qualifies for `c`.
- **R3 (#3, Task 1 fix round)** — the list's near-clip class uses the plane's clip floor (`pm.clip_floor`), not one
  re-derived from the list seeds.
- **R4 (#4)** — (a) the list's short-`c` warn is quiet (`warn_short_c`, debug); (b) `c` is over the kept fits only
  (pre-filter; documented); (c) no list is measured on a 3-plane file (O4: such a file keeps `ColourCamera`).
- **R5 (#5, Tasks 5a / 5b)** — every outcome that will not change on a re-read is stored at version 3 with its own
  refusal: a plane with a verdict (trailed, defocused, no stars) → `frame_verdict`; a WCS with no cone →
  `unreadable_solve`. A caught panic stores nothing and retries. Test: a trailed solved frame is selected by Analyze
  once, then not again.
- **R6 (#14)** — `zp_window_bright` is replaced by `zp_refusal TEXT` (the refusal code; NULL on a valid ZP), so loss
  reasons read the code and the plan's Reading 4 derivation goes. The probe reports the list's wall time and memory per
  frame.
- **R7 (#13)** — the pass after a solve batch is Analyze's own worker over the newly solved ids (one writer of `zp_*`),
  skipping frames already current, taking the compute queue per chunk; 6b shows "N to update" beside Analyze instead of
  counting outdated rows as not analysed.
- **R8 (#7)** — the rig-wide reference fallback stays OFF for mosaics until judged; the probe reports a cross-target
  row for the Duo (LDN 1272 against vdB 152) as a decision row for the owner.
- **Bars (spec §6, amended in the same commit):** B4 tolerance 0.05 → **0.02**, judged on ATR L and 10-18 after the flip
  (before the flip ΔX is 0.07); B8 → |slope| ≤ 0.03 and ≤ |today| + 2σ, the cross-target spread a decision row, not a
  STOP; **a bar judged on no night of its class is a STOP** (no silent pass); B1 counted over pairs where both frames are
  valid; B3's FWHM slope is also fitted against `bigap` 6F on the same list and a miss is called only when both agree
  (09-13 named a known risk: +0.036 ± 0.009 in 3c); B6 exempts frames with a plane verdict.
- **Tests (#12)** — pier side from the rotation difference mod 360 (a +179 / −179 case); the list builder's type takes
  no FWHM, so "the list does not follow the seeing" holds by type. **Lines (#15)** — implementers re-verify every
  line number they edit.

### After the re-audit (F5, controller rulings RR1–RR4 in the ledger, 2026-11-16)

- **R1** moot (the list's `c` cap is gone, G1). **R2, R3** stand (the map fits). **R4** (a) and (b) moot with the list's
  `c`; (c) stands. **R5** stands, with `poor_solve` among the stored refusals (5b). **R6** amended: `zp_refusal` stays and
  `zp_window_bright` returns beside `zp_window_slope` (spec §5.5, FL8); the probe reports the list's wall time split into
  fits, refits and apertures. **R7, R8** stand.
- **Bars**: spec §6 as amended — B1 over members of non-zero weight; B2–B5 and B8 on the reduced ZP; B3's referee the sum
  at 6·`FWHM_ref` (RR3); B5 / B8 one night at a time (RR1); B4 without an allowance, a miss inside its error to the owner
  as a stated limit (RR2); B7 on the map refits; Phase A again with an identity pass, a decimated round and a 70 ms cost
  STOP, every run through `memrun.sh` (RR4). No threshold moved.

### F5 plan review (fable, `review-g0-spec-plan-fable.md`, 2026-11-16; every finding accepted by the controller)

Settled by the review, no change: `FluxCorrection::Given(1.0)` runs no growth curve; no wire golden pin changes; B9 holds
by construction; G1 → G2 is the build order; the expected table matches the verdict file. Where each finding landed:

- **H1** (the slope rule nulled the reduction on QHY L's cut windows) — `slope_min_ramp_members` counts the bright ramp
  only, the faint term taken as it is: spec §5.3 (window slope, sparse list) and §6 unjudged; Constants; G2 step 7 and
  tests (`a_window_cut_by_the_catalogue_keeps_its_slope`, `fewer_than_ten_bright_ramp_members_give_no_slope`); 5a's table.
- **H2** (two exposures in one pool) — the exposure (0.1 s) joins the pool key, set and rig-wide: spec §5.6; G3 step 7's
  reduction; 6a step 3 and `two_exposures_in_one_set_are_two_references`; G3 step 5 and G4 the residual at anchor shifts
  of ±0.44, ±1.0 and ±1.75 mag.
- **M1** (the screen's upper cut is a step) — **WITHDRAWN** by the G2 review (H1, controller ruling 2026-11-16). The
  answer, a member above `m + 4σ` counted at the limit, measured on g0 against the emulation's drop: a bias of +0.0064
  (QHY H), +0.015 / +0.017 (Duo vdB / LDN 1272), +0.017 / +0.018 (QHY L), +0.022 / +0.026 (ATR) in the level of the ZP,
  0.0012–0.0044 rms frame to frame (up to 0.045 in one frame), and a correlation of the shift with σ of +0.66 / +0.73 on
  ATR; the ledger's `progress.md`, tail, "H1". It removed the steps and put a bias that follows σ in their place. The
  answer now: a taper, spec §5.3 and G2 step 7 — a member with `zp_i > m + 4σ` keeps its own flux at the weight ×
  `clamp(1 − (zp_i − m − 4σ)/(τ·σ), 0, 1)`, τ = `screen_taper_sigma` 1.0 (level +0.001…+0.007, rms 0.0004–0.0028, max
  0.021, continuous); τ → 0 is the emulation's drop, so G4 reports both rows. `tapered` replaces `capped` (G2 step 7;
  G3's column and per-night line); the test is `a_member_above_the_screen_is_tapered_and_the_zero_point_moves_continuously`.
  **Ruled again after G4 (G7): τ default 0.** g0 / g1 / g1t0 against each other: taper − drop is equal on 14 nights (level
  +0.001…+0.005, rms ≤ 0.0054, max 0.013, B2 within ±0.0006) and worse on the thinnest, Duo 09-13 (B2 0.0049 against
  0.0034, B3 slope −0.0354 against −0.0238, rms 0.0077 against 0.0059); the continuity gain did not show in B2. The drop,
  as emulated, ships; the taper stays reachable by `--screen_taper_sigma 1`. A NaN τ is refused at start-up
  (`ForcedListParams::validate`; the probe calls it), never stored as `too_scattered`.
- **G7** (the list isolates from the frame's own detections) — spec §5.1: the tiers drop RUWE ≥ 1.4 (G6), so bright
  neighbours of 7.6–10.9 % of the list's weight were missing from the cone; the image carries them. Rule, constants
  (`detected_neighbours`, `own_seed_radius_px` 1.5, `detected_neighbour_min_peak_sigma` 10, `seed_calibration_min_pairs`
  20; each a probe flag) and the row's `detected_neighbours_removed` / `seed_calibration_pairs` are in the spec; the code was
  `zero_point.rs::seed_isolation` (G9 moved it: `seed_rule_in`). **G8**: a mosaic's seeds reach the first
  callback (rustafits lends the native seeds, frame pixels, peaks on the `noise.sigma` scale), and the `G_b − 2` halo rule
  is mirrored on the seeds (`bright_dmag`, `bright_radius_fwhm`, no peak floor).
  **G9** (the seed rule after the map, on members only, to a fixed depth) — g2's two defects (spec §5.1: on a bad solve the
  rule read a star's own seed 1–3 px off as a neighbour and removed the stars that show the solve, the gate passed it, LBN
  582 kept share 0.58–0.61 → 0.77–0.84, `poor_solve` 6 → 1, B1 min 0.58 / 0.74, B3 rms 0.0164; the 10 σ floor followed the
  seeing, removals 3× within a night, Duo 09-13 B3 slope −0.042): the list is the catalogue's alone again (identity proof
  against g1t0 with `--detected_neighbours false`), the first callback copies the seeds, and `zero_point.rs::seed_rule_in`
  runs in the placement step on the stars at their mapped positions; a seed counts down to `G_b + seed_neighbour_depth_mag`
  (2.0, a probe flag); `forced_zero_point` leaves a removed star out of the members; `seed_peak_floor_rejected` is a row
  column. Tests: `a_bad_solve_keeps_its_gate_verdict_with_the_seed_rule_on_and_off`, `the_seed_rule_runs_at_the_mapped_positions`,
  `a_seed_fainter_than_the_depth_never_removes_and_the_peak_floor_counts_what_it_spares`,
  `a_member_the_seed_rule_removed_is_in_no_sum`.
- **M2** (6r is not an independent referee) — B3 prints three slopes, a stack miss 6r does not echo is no pass and goes
  to the owner: spec §6 (paragraph, M2 beside RR2, RR3); G3 step 7 and its test; G4's class-line paragraph and STOP list.
- **M3** (the ±0.1 re-measure moved the edges) — `anchor_shift_mag` before the window rule, re-measures at ±0.1 and ±0.5:
  Constants; G2 step 2 and the slope test; G3 step 5; G4's window-slope figures; spec §4.2, §5.6 (κ's 10–90 %) and §6
  (0.14 → 0.5 mag unjudged); Task 8's owed list.
- **M4** (G2 could not keep the probe compiling by flags alone) — G3 step 1 moved into G2 step 12 (with the stars TSV loop,
  which would pair list stars with the map stars' fits); the build-order paragraph; G2's and G3's Files.
- **M5** (`poor_solve` on few map stars, and its sentence) — spec §5.3 (the draws), §5.6 and §6 (M5 beside RR2); G2 step 3;
  6a step 2; Task 7 check 9 solves frame 29375 again; G4's decimated-round figures (zero false refusals keeps 40).
- **Lows** — B1 after the screen (`screened_in`): spec §6, G3 step 7, G4's re-gate. `Σ w·f ≤ 0` → `too_scattered`, the
  NULL σ exempt from G4's non-finite STOP, an empty map request carries n: spec §5.3 (the sum), G2 steps 7 and 10 and
  their tests, G4's STOP list. G1's tests (a
  disc of 1 on 0, 5e-3) and the grouped median's quantum from the data: G1 step 4 and tests. Stale citations: 5a, 5b, 5c
  (step 4's dictionary fields and codes), the 6.5 index. `masked_growth` / `report_aperture` merged: G1 steps 3 and 6. One
  solved set outside the nine as a reported row: G4 step 3.
- **Sparse-list numbers** — G4's decimated-round item (map stars, member and kept shares, false `poor_solve`, the eight bad
  frames, the rms against n with its pedestal part, the share of frames with a slope); spec §5.3.
- **Owner-facing** — `Transparency::zero_point` carries the reduced value, stored columns untouched (spec §5.6, 6a step 3,
  `the_cell_shows_the_reduced_zero_point`); the cell drops `± zp_sigma` (6b).

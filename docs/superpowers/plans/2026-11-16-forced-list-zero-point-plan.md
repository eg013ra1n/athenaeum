# Forced-list zero point — plan

Spec: `docs/superpowers/specs/2026-11-16-forced-list-zero-point-design.md` (rulings F0–F8, FL1–FL5, design §5, bars §6,
order §7). It replaces Tasks 4–8 of `2026-11-16-osc-zero-point-plan.md`; that plan's Tasks 1–3 stand and are reused.
Worktree `.claude/worktrees/osc-zp`, branch `collab-v3-wave6-5-osc`; rustafits branch `osc-green-aperture` (head
054c435) inside the worktree's submodule. The branch's tree still records rustafits d2c5775 (rustafits `main`, 1.8.0):
054c435 is checked out but its pointer is not committed (` M rustafits`); Task 1 commits the bump. rustafits commits
carry no trailer (its own rule); Athenaeum commits carry `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
Ledger (main checkout): `.superpowers/sdd/2026-11-16-osc-zp/progress.md`. Job data: `~/.athenaeum-acc/2026-11-16-osc-zp/`
(REGISTRY row, expires 2026-12-07; the catalogue copy `catalog/athenaeum.db`, the common sets `out/common-merged.tsv`).
One implementer at a time (16 GB Mac); agent builds use `CARGO_TARGET_DIR=<main checkout>/target-agent`. Skill
`lean-execution` governs the run: a fresh agent per phase, short reports in the ledger folder, disk rule §7.

| # | Task | Model | Gate |
| ---- | ---- | ---- | ---- |
| 1 | rustafits: the list entry beside `measure_raw_frame` (R-OSP-19) | rust-engineer sonnet | rustafits tests (default, `debug-pipeline`, `serde`); publish dry-run; core `cargo check --all-targets` |
| 2 | Core, pure: anchor, window, list, statistic, the composition | rust-engineer sonnet | `analysis::zero_point` and `analysis::analyzer` tests; 6.5's pure tests unchanged |
| 3 | `zp_probe forced`, `verdict --forced` (B1–B9), runner | rust-engineer sonnet | example builds and tests; the 6.5 and r1 verdict regressions |
| 4 | Phase A: GO or STOP | sonnet runs, opus judges | §6 bars on every judged night; controller recomputes B1, B2, B6 from the TSVs |
| 5a | Storage: three columns, the stored figures, sync, TS | rust-engineer sonnet | `db::analysis`, `sync::ingest`, `ts_contract`; `cargo check --workspace --all-targets` |
| 5b | Analysis writes the zero point; `ZP_VERSION` 3; every 6.5 writer removed (both hosts) | rust-engineer sonnet | `analysis`, `api::analysis`, `api::collab` tests; workspace check; grep of removed names |
| 5c | The pass after a solve batch | rust-engineer sonnet | `api::analysis`, `api::plate_solve`, `log-mcp` tests; workspace check |
| 6a | Loss reasons, mosaic references, the gate's O7 | rust-engineer sonnet | `analysis::transparency`, `collab::gate`, `api::collab`, `api::frame_sets` tests |
| 6b | The Analysis tab's count | frontend-dev | `npm run build`; vitest |
| 7 | Acceptance on a catalogue copy (headless web) | sonnet | the Task 7 checks |
| 8 | Docs, final review, merge | controller + fable reviewer | full core run; workspace check; build; vitest; rustafits tests |

Each task: a review (opus; fable on round 2+), the controller's own re-gate (build and tests re-run, never taken from
the report), the ledger line. Tasks 5–8 start only on Phase A's GO.

## What exists, what is reused, what is dropped

- **Reused from the OSC plan (done).** rustafits Tasks 1 / 1b: the masked, phase-matched green-site growth
  (`photometry::growth_curve_in` with `masked`), the reported mosaic `c` (`PlaneMeasurement::reported_aperture_*`,
  `measure_plane_reporting_aperture`, `CFA_APERTURE_VERSION` 2). Here it gives the mosaic anchor (§5.1) and, through the
  same masked growth, the list's own `c` (§5.2). `CFA_APERTURE_VERSION` stays rustafits' version of the reported number;
  nothing in Athenaeum stores it (the stored `c` is the list's, under `ZP_VERSION` 3). The probe (Tasks 2 / 3c): the
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

`FWHM_ref = max(2 px, 2.5″ / scale)`; anchor: 6.5's matcher with at least 10 matches; bright edge `G_b` = 1.5 mag
fainter than a β-4 Moffat at `FWHM_ref` whose amplitude is 0.7 of full scale; faint edge `G_b + 3`, deepened up to
`G_b + 4.5` while fewer than 200 isolated stars; region 0.9 of the half-diagonal and ≥ 13·`FWHM_ref` from every edge;
isolation: no catalogue star with ΔG ≤ 4 within 4·`FWHM_ref`, none brighter than `G_b − 2` within 10·`FWHM_ref`; cap
1 000 by record hash; centre tolerance `max(1 px, 0.5·FWHM_ref)`; `c` over ≤ 256 kept fits, none under 5; statistic:
4 σ clip, ≥ 50 survivors, `zp_sigma / √zp_stars` ≤ 0.035. The probe may move a constant (never a bar); a moved
constant is said in the Phase A report with its reason.

## Task 1 — rustafits: the list entry (R-OSP-19)

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

Controller first: the catalogue copy (`PRAGMA integrity_check` ok); the counts it holds (2026-11-16: OSC LDN 1272 160
solved of 164; vdB 152 261 solved; `ATR2600M` LDN 1272 65 solved, `FILTER` empty, night key 2025-10-17; `QHY268M` L
166 / 84 / 45, H 100 / 52); `/Volumes/bigbase3` mounted; disk (`df`); no other heavy job. Sonnet runs `run-forced.sh
a0`; opus judges each bar from the TSVs and explains every miss on the frames it sits on (a stretched JPG and its list
stars first), writes `task-4-report-r0.md`. GO only if every bar passes on every judged night; any miss → STOP, the
owner gets the numbers. A moved constant reruns every set and is named in the report. Controller re-gate: B1, B2 and
B6 recomputed from the TSVs by a script in `out/<tag>/`.

## Task 5a — storage

**Files.** `db/schema.rs` (`CREATE TABLE frame_analysis` :1975–2012: `zp_list_stars INTEGER`, `zp_aperture_correction
REAL`, `zp_window_bright REAL` after `zp_catalog`; the column loop :2092–2115); `models.rs` (`FrameAnalysis`
:1075–1128); `db/analysis.rs` (`upsert_frame_analysis` :4–51, `set_zero_point` :53–88, `clear_zero_point` :90–101, the
three SELECTs :103–188, `row_to_analysis` :314–347, tests :349–760); every `FrameAnalysis` literal
(`analysis/analyzer.rs:132–136`, `examples/star_probe.rs:1096`, and in tests `db/analysis.rs:379–413`,
`analysis/zero_point.rs:1286–1320`, `api/analysis.rs:1061–1110` and `:1400–1410`, `api/sync.rs:8560–8595`,
`plate_solve/service.rs:852` and `:932`, `collab/gate.rs:631`, `collab/frame_meta.rs:447`, `api/frame_sets.rs:2125–2131`
— `grep -rn "zp_catalog:" crates` lists them); `analysis/zero_point.rs` (`store_outcome` :551–580 adapted to the new
signature, 6.5's figures with the three new ones NULL); `ts_export.rs:150` (registry unchanged) and the regenerated
`src/types/models.ts` (:552–585); `sync/ingest.rs:938–951` (unchanged code); `sync/ingest_tests.rs:3036`.

**Approach.** `ZP_VERSION` does NOT change here (it changes in 5b with the writers, so no build of the branch ever
writes a 6.5-style value under version 3).
1. The three columns in `CREATE TABLE` and the loop; three `Option` fields on `FrameAnalysis` with `#[serde(default)]`
   and docs; upsert, SELECTs and `row_to_analysis` carry them (indices 31–33).
2. `zp_outdated: bool` on `FrameAnalysis`, `#[serde(default, skip_serializing_if = "std::ops::Not::not")]`: computed by
   the Analysis listing (5b), never a column; `row_to_analysis` sets false, so the sync wire never carries it. The
   generated TS line must read `zp_outdated?: boolean` (add `#[ts(optional)]` if ts-rs does not derive it from the
   serde attribute), so no frontend literal needs the key.
3. `set_zero_point(conn, frame_id, analysis_id, solve_id, figures: &StoredZeroPoint, catalog)` writes all eight
   columns (same compare-and-set); `clear_zero_point` clears all eight. The stored figures (read by 6a):

   | Outcome | zero_point | zp_stars | zp_sigma | zp_list_stars | zp_aperture_correction | zp_window_bright |
   | ---- | ---- | ---- | ---- | ---- | ---- | ---- |
   | measured | ZP | kept | σ | n | c | G_b |
   | too_scattered | NULL | kept | σ | n | c | G_b |
   | too_few_stars | NULL | kept | NULL | n | c | G_b |
   | no_aperture_correction | NULL | NULL | NULL | n | NULL | G_b |
   | no_anchor, no_exposure, unreadable_solve, colour_camera | NULL | NULL | NULL | NULL | NULL | NULL |

   Every judged row carries `zp_version` and `zp_catalog`.
4. Regenerate `src/types/models.ts` (`TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`).

**Tests.** `migration_adds_the_zero_point_columns_and_keeps_rows` (:654) and `ZP_COLUMNS` (:647) → eight;
`frame_analysis_round_trips_the_zero_point` (:676) → eight; `an_old_frame_analysis_json_decodes_without_a_zero_point`
(:704) → eight keys; `set_zero_point_writes_the_value_or_nulls_and_the_version_and_catalogue` (:722) → every row of the
table; `clear_zero_point_nulls_all_five_columns` (:747) → `clear_zero_point_nulls_all_eight_columns`;
`zp_outdated_never_reaches_the_wire_or_the_database` (false: the key is absent from the JSON; a JSON carrying `true`
upserts and reads back false); `an_analysis_summary_with_and_without_the_c6_keys_ingests` (ingest_tests.rs:3036) with
and without the three keys; `ts_contract` green.

**Acceptance.** `cargo test -p athenaeum-core --lib db::analysis`, `--lib sync::ingest`, `--test ts_contract` green;
`cargo check --workspace --all-targets` green.

## Task 5b — Analysis writes the zero point; every 6.5 writer goes

**Files.** `analysis/zero_point.rs` (`ZP_VERSION` :35–37; `is_colour_frame` :502–508; `load_facts` :510–532;
`store_outcome` :551–580; removed: `refresh_zero_point_with` :582–681, `needs_refresh` :693–707, `refresh_many_stats` /
`refresh_many_with` / `refresh_listed_with` / `ensure_zero_points_with` / `recompute_zero_points_with` :750–828,
`CAN_BE_MEASURED` / `stale_zero_point_frames` / `stale_among_frames(_counted)` :830–892, `NotReady::NoStarMetrics`
:423–425; kept: `ZpPassStats` :709–748 for 5c, `catalog_cone` :683–691, `BACKFILL_CHUNK` :404 renamed `PASS_CHUNK`);
`analysis/analyzer.rs` (beside `analyze_frame` :152–164); `api/analysis.rs` (removed: :46–160 the triggers, :162–205
`backfill_zero_points`, :303–354 `installed_identity` / `spawn_zero_point_backfill`; the guard :207–301 stays for 5c;
`row_is_stale` :436–442; `analyze_frame_set` :463–768 — selection :541–588, worker :617–678, persist :709–728, trigger
:732–737; `get_analysis_for_frame_set` :785–796; `persist_frame_analysis` :805–821; `get_frame_star_metrics` :834–947 —
fresh :872–889, on demand :891–919, trigger and read-back :921–938); `api/plate_solve.rs` (:121–122 the tier-install
backfill, :694–700 the solve trigger); `crates/athenaeum-tauri/src/commands/core.rs:143–146` and
`crates/athenaeum-web/src/main.rs:285–288` (the launch backfill, both hosts in one commit); `api/collab.rs`
(`gate_app_data_dir` :760–767 kept; removed `GATE_INLINE_ZERO_POINTS` :769–772, `GateMeasure` :774–804,
`rules_judge_transparency` :806–809, `measure_gate_stale_with` :811–858, `ensure_gate_zero_points` :860–883;
`project_gate` :1176–1250 and its call :1245; callers :1104, :1691, :1776, :2159, :4877).

**Approach.**
1. `ZP_VERSION` = 3 (the doc says what 3 is: the forced list). `is_mosaic(bayerpat, source_channels)` beside
   `is_colour_frame`: the trimmed, upper-cased pattern is RGGB, BGGR, GRBG or GBRG and `source_channels == Some(1)`
   (the reader's `RawPlanes::Cfa`), O4's one test for 5b and 6a. `zp_outdated(analysis, solved, catalog) -> bool` =
   a catalogue is installed AND the frame has a solve AND (`zp_version ≠ 3` OR `zp_catalog ≠` its identity): the one
   predicate of Analyze's selection and the listing.
2. `analyzer::analyze_frame_with_zero_point(path, config, pool, config_hash, zp: Option<&ZpFrame>)`: read, Task 2's
   `measure_with_zero_point`, `map_measurement`, the eight figures into the row (`zp_version` 3, the identity). `ZpFrame
   { wcs, solve_id, exptime, epoch, bayerpat, catalog: &ZpCatalog }`. A colour file that is not a mosaic stores
   `colour_camera`; a forward SIP with no reverse stores `unreadable_solve`; no exposure stores `no_exposure`. The
   list part runs under `catch_unwind` (the 6.5 `isolated_trigger` precedent, api/analysis.rs:62–71): a panic leaves
   the eight NULL with one `warn`, never fails the analysis. `analyze_frame` stays for `get_collab_frame_star_metrics`
   (:955) and the probes.
3. `analyze_frame_set`: the selection opens the catalogue once (`zero_point_catalog`; none → no list, `debug` once)
   and reads every frame's solve and header facts in one query beside the frame list (never one per frame); with
   `force = false` it keeps `row_is_stale(existing) || zp_outdated(existing, solved, identity)`; the worker calls
   step 2; the persist writes the zero point in the same upsert only while the frame's solve is still the one read
   (`SELECT id FROM plate_solves WHERE frame_id = ?` inside the transaction), else the eight go NULL (`debug` "zero
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
  identity, the eight figures by the table).
- `the_analysis_rows_are_identical_with_and_without_the_zero_point` (every `FrameAnalysis` field but `analyzed_at` and
  the eight, every `StarMetric`): B9's core half.
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
- Kept with eight columns: `deleting_a_solve_clears_the_zero_point` (zero_point.rs:1568),
  `a_new_solve_clears_the_zero_point_even_when_the_analysis_is_stale` (:1580); kept as is:
  `no_catalogue_leaves_the_analysis_saved_and_logs_once` (api/analysis.rs:1302),
  `a_single_frame_analysis_commits_its_stars_with_its_row` (:1368, plus the zero point in the same commit).

**Acceptance.** `cargo test -p athenaeum-core --lib` filtered to `analysis`, `api::analysis`, `api::collab`,
`db::analysis` green; `cargo check --workspace --all-targets` green; `grep -rn
"spawn_zero_point_backfill\|backfill_zero_points\|ensure_gate_zero_points\|refresh_zero_point_with\|stale_zero_point_frames\|zero_points_after_"
crates/` empty.

## Task 5c — the pass after a solve batch

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
   writes the eight `zp_*` alone through `set_zero_point` (compare-and-set on both ids: a re-analysis or a new solve
   meanwhile wins). An offline or unreadable file is a `debug` (`frame_id`, `path`, `error`) and counted; the pass
   says one `warn_if_unmeasurable("solve")`. Span `zero_point_solve_pass` (id field `catalog`), one `info` "zero point
   solve pass done" (`frames`, `zp_measured`, `duration_ms`).
3. `api/plate_solve.rs:694–700` spawns the pass with the persisted frame ids when the batch stored them.
4. log-mcp: the operation kind `zero_point_backfill` → `zero_point_solve_pass`. The dictionary paragraph: the new
   fields (`zp_list_stars`, `zp_aperture_correction`, `zp_window_bright`, `zp_anchor`, `zp_kept`), the codes
   (`no_anchor`, `no_aperture_correction`, `unreadable_solve`), `pass = "solve"`, the span, the lines 5b and 5c
   removed (backfill, trigger, gate measurement).

**Tests.** `the_solve_pass_measures_solved_frames_with_a_current_analysis` (the eight written; a stale analysis
skipped); `the_solve_pass_skips_an_offline_file_and_warns_once`; `a_reanalysis_during_the_pass_wins`;
`the_solve_pass_runs_one_at_a_time_and_takes_queued_frames` and `a_solve_pass_that_dies_releases_the_guard` (pure
guard tests, replacing `the_same_identity_does_not_start_twice` :1450, `a_new_catalogue_identity_allows_another_backfill`
:1323, `a_backfill_that_dies_releases_the_guard` :1341); log-mcp's kind test.

**Acceptance.** `cargo test -p athenaeum-core --lib api::analysis` and `--lib api::plate_solve`, `cargo test -p
log-mcp` green; `cargo check --workspace --all-targets` green.

## Task 6a — loss reasons, mosaic references, the gate's O7

**Files.** `analysis/transparency.rs` (module doc :1–18; `LossReason` :44–95; `AnalysisFacts` :134–145; `FrameRow`
:147–158; `load_frame_rows` :198–254 with the star-row column :208–209; `load_pools` :363–442 with its colour filter
:425; `judge` :472–521; tests :613–1379, fixture `Spec` :620–700); `models.rs:1474–1482` (`NOT_MEASURED_REASON`
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
2. `Refused` reads the stored figures (5a's table) and `frames.exptime` (added to `load_frame_rows`):
   `no_aperture_correction` (list set, `c` NULL) → "too few isolated stars to measure the light in the star halos";
   `too_few_stars` with a list under 50 → "too few catalogue stars in the field — a deeper star catalogue tier may
   help"; `too_few_stars` otherwise → "too few of the field's catalogue stars could be measured"; `too_scattered`
   (σ set) → "the star brightnesses scatter too widely for a reliable zero point"; no exposure → "no exposure time in
   the header"; any other all-NULL row → "too few catalogue stars matched to set the measurement — a deeper star
   catalogue tier may help".
3. References: a mosaic with a current zero point joins the pools (:425 keeps out `is_colour_frame && !is_mosaic`).
4. O7: `GateFrameInput` gains `colour_without_filter` = `is_mosaic(bayerpat, source_channels)` and an empty trimmed
   `FILTER`; `evaluate_frame` then lists `transparency_loss` with no value, `pass: None`, reason "colour frame without
   a filter name: its clear-sky level may mix filtered and unfiltered nights"; nothing blocks it. The frame's own
   `Transparency` (column, panel, manifest key) is unchanged.

**Tests.** transparency: `a_debayered_colour_frame_says_so` (was `a_colour_camera_frame_says_so` :1114);
`a_mosaic_frame_reads_its_loss_and_feeds_the_reference`; `a_synced_analysis_with_a_current_zero_point_reads_its_loss`
(was `a_synced_analysis_says_to_analyse_here` :1132); `a_version_two_zero_point_says_the_analysis_differs`;
`a_zero_point_from_another_catalogue_neither_counts_nor_shows` (:1158, now `AnalysisDiffers`);
`no_installed_catalogue_says_so` (was `no_installed_catalogue_gives_not_measured` :1203); `each_refusal_says_why` (every
table row); `each_state_has_its_reason` (:1264) and `the_sentences_are_the_binding_ones` (:1363) extended;
`five_thousand_frames_cost_few_statements` (:1339) unchanged. Gate: `an_osc_frame_without_a_filter_gets_no_transparency_
verdict`, `an_osc_frame_with_a_filter_is_judged`, `a_mono_frame_without_a_filter_is_judged`; the transparency gate tests
(:1229–1300) unchanged; `no_catalogue_does_not_fail_the_gate` (api/collab.rs:7212) with the new sentence.

**Acceptance.** `cargo test -p athenaeum-core --lib` filtered to `analysis::transparency`, `collab::gate`,
`api::collab`, `api::frame_sets` green; `cargo check --workspace --all-targets` green.

## Task 6b — the Analysis tab's count (frontend-dev)

**Files.** `src/components/LightsAnalysisView.tsx` (`loadAnalysisData` :257–270, the count :541–543, the Analyze /
Re-Analyze buttons :662–689); a new pure helper `src/components/analysisCount.ts` with `analysisCount.test.ts`;
`src/types/models.ts` (regenerated in 5a). The Transparency cell needs no change: `FramesTableRows.tsx` and
`collab/project/transparency.ts` show core's `reason` as it comes.

**Approach.** A row with `zp_outdated` does not count as analysed, so a fully analysed set whose zero points are not
current shows the Analyze button, which runs `force = false` and re-reads only those frames. The count text stays
`{analysed}/{total}`; when some rows are outdated the count carries a `title`: "{n} analysed before the current
transparency measurement". Design tokens only; no `@tauri-apps` import; logic in the helper.

**Tests.** `analysisCount.test.ts`: all current → all analysed; one outdated → not, and the count leaves it out; no
rows → 0. `npm run build` green; `npx vitest run src/components/analysisCount.test.ts src/components/stacking
src/components/collab` green.

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
   (`delete_plate_solve_for_frame`), solve it again (`plate_solve_batch`): the pass line and its eight columns, no
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
Owed by a human: the real window (the Analysis tab's count and Analyze, the cell sentences). The copy is deleted once
`task-7-report-r0.md` is committed to the ledger.

## Task 8 — docs, final review, merge

**Files.** `docs/catalog/README.md:104–164` (the zero point section rewritten: the list, where it runs, currency, the
stored figures, what reads it, what was removed); `CLAUDE.md:135` (the Transparency line: measured inside Analysis'
read over a fixed list of catalogue stars; nothing reads pixels at launch, a tier install or the gate); 
`docs/transfers/README.md:383`, `:387` (the gate reads stored values only; O7); `docs/logging/README.md:60–63` (the
operation kind); the dictionary paragraph from 5c re-read; `docs/superpowers/open-items.md:399–409` (6.5's section:
"Owed: OSC" :405 replaced by this cycle's owed items — a cloudy night, under 16 bit, long focal length, high latitude,
the faint-edge extension, the real-window look; "Known, not fixed" :409 updated: the reason is derived from the stored
figures, `unreadable_solve` shares `no_anchor`'s sentence); pointers to the forced-list spec at the end of §9 of
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

- `analysis/zero_point.rs`: `ZP_VERSION` 3; `is_mosaic` and `zp_outdated` added; the stored-rows half removed (:582–892
  bar `catalog_cone` and `ZpPassStats`); `NotReady::NoStarMetrics` gone; `store_outcome` writes eight columns;
  `measure_zero_point` kept as the anchor (5b, 2).
- `analysis/analyzer.rs`: `analyze_frame_with_zero_point`, `measure_with_zero_point` (2, 5b).
- `analysis/transparency.rs`: reasons, refusal sentences, mosaic references, `NoStarMetrics` and `NoZeroPoint` gone (6a).
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

- `analysis/zero_point.rs`: the pure tests :1005–1265 stay unmodified and now pin the anchor. Removed with their code:
  `refresh_measures_and_stores` :1400, `a_stale_analysis_is_not_measured` :1415,
  `a_frame_without_a_solve_or_an_analysis_is_not_ready` :1428, `a_new_analysis_clears_the_zero_point_until_the_trigger_
  runs` :1504, `a_new_solve_recomputes_the_zero_point` :1524, `the_solve_trigger_recomputes_a_current_zero_point_and_
  the_analysis_trigger_skips_it` :1545, `a_synced_analysis_without_star_rows_is_not_measured` :1642,
  `ensure_skips_frames_that_are_current` :1661, `a_zero_point_from_another_catalogue_is_recomputed` :1676,
  `the_backfill_cursor_walks_the_stale_frames_in_chunks` :1689, `a_zero_point_of_an_older_version_is_stale` :1736,
  `the_gates_stale_selection_is_one_query_per_chunk` :1753, `a_synced_analysis_without_star_rows_is_never_stale` :1780.
  Rewritten against the analysis path: `a_missing_exposure_stores_a_refusal` :1445, `a_colour_camera_frame_is_not_
  measured` :1456 (a mosaic is measured), `a_debayered_colour_frame_is_not_measured` :1471,
  `a_forward_only_sip_is_not_measured` :1490 (now stored `unreadable_solve`), `a_write_after_a_reanalysis_is_dropped`
  :1605, `a_trigger_error_does_not_fail_the_analysis` :1705, `a_panicking_cone_does_not_fail_the_trigger` :1718. Kept
  with eight columns: :1568, :1580.
- `api/analysis.rs`: `a_new_catalogue_identity_allows_another_backfill` :1323, `a_backfill_that_dies_releases_the_guard`
  :1341, `the_same_identity_does_not_start_twice` :1450 → the solve pass guard; :1113, :1302, :1368 keep their names.
- `api/collab.rs`: `a_project_without_the_rule_measures_nothing_in_the_gate` :7358, `an_interactive_gate_keeps_its_
  inline_limit` :7417, `a_publish_run_measures_every_selected_frame` :7456, `the_inline_limit_applies_to_the_selection`
  :7474 removed (with `seed_stale_zero_points` :7338, `NOT_MEASURED_YET` :7403); replaced by
  `the_gate_reads_stored_zero_points_only`.
- `analysis/transparency.rs`: :1114, :1132, :1158, :1203, :1264, :1363 as in 6a.
- `db/analysis.rs`: :647, :654, :676, :704, :722, :747 as in 5a. `sync/ingest_tests.rs:3036` as in 5a.
- `api/frame_sets.rs`: `frame_set_detail_does_not_measure_anything` :2282 checks eight columns.

## Readings of the spec this plan makes

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

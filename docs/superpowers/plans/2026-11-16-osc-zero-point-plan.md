# OSC zero point — plan

Spec: `docs/superpowers/specs/2026-11-16-osc-zero-point-design.md` (rulings O1–O7, bars §6). Worktree
`.claude/worktrees/osc-zp`, branch `collab-v3-wave6-5-osc`; rustafits branch `osc-green-aperture` inside the worktree's
submodule (its commits carry no trailer). Ledger (main checkout): `.superpowers/sdd/2026-11-16-osc-zp/progress.md`.
Job data: `~/.athenaeum-acc/2026-11-16-osc-zp/` (REGISTRY row, expires 2026-12-07). One implementer at a time (16 GB
Mac); agent builds use `CARGO_TARGET_DIR=<main checkout>/target-agent`.

| # | Task | Model | Gate |
| ---- | ---- | ---- | ---- |
| 1 | rustafits: masked growth, reported `c`, `CFA_APERTURE_VERSION` | rust-engineer sonnet | rustafits tests ± `debug-pipeline`; core `cargo check --all-targets` |
| 2 | Probe: `c`, airmass, corrected zero point, the §6 verdict | rust-engineer sonnet | example builds; 6.5's verdict lines reproduced |
| 3 | Phase A run and verdict | sonnet (judgment run) | §6 bars; controller re-gate from the TSVs |
| 4 | Storage, sync, outdated flag | rust-engineer sonnet | `db::analysis`, `analysis`, `sync` tests; `cargo check --workspace --all-targets` |
| 5 | Zero point, loss, gate | rust-engineer sonnet | `zero_point`, `transparency`, `collab::gate` tests |
| 6 | Analysis tab count | frontend-dev | `npm run build`; vitest if the view has tests |
| 7 | Acceptance on a catalogue copy | sonnet | Task 7 checks |
| 8 | Docs, final review, merge | controller + fable reviewer | full core run; `cargo check --workspace --all-targets` |

Each task: a review (opus; fable on round 2+), the controller's own re-gate, the ledger line.

## Task 1 — rustafits: the green-site `c`, measured and reported

**Files.** `rustafits/src/star_measure/photometry.rs` (`growth_curve` :972, `aperture_correction` :1103, its growth
call :1208–1220, the per-plane log lines :1232–1259); `fit.rs` (`ring_and_correction` :2511, the apply :2420–2423,
`correct_at_ring` :2677–2709, `FitOutcome` :404–416, `Correction.flux` :1982); `mod.rs` (`FluxCorrection` :510–520,
`MEASURE_VERSION` :237, `MeasureOptions` :243–303, `PlaneMeasurement` :522–590 and its fills :1767, :1797–1803,
:2037–2043, `measure_budget` :1912–1927, `finish_working` :1667–1689); `green.rs` (`measure_green_sites` :56, its
options :108–127, test :569–576); `CHANGELOG.md`.

**Approach.**
1. `growth_curve(…, masked: bool)` per spec §5.1; `false` is today's code path, unchanged.
2. `aperture_correction` passes it through; nothing else in it changes.
3. A crate-private selection of "report, do not apply" (a `pub(crate)` field or entry the green path sets; no change
   to `MeasureOptions`' public fields, so `crates/athenaeum-core/src/stacking/measure.rs:1117` compiles untouched).
   Under it `ring_and_correction` measures `c` with the masked growth and hands it back apart; `fit_at_width` skips
   :2420–2423; `correct_at_ring` keeps the model flux. `aperture_correction` / `_stars` / `_se`, `aperture_ratios`
   and the population's `aperture_se` stay `None` / 0 / empty on that path. `PlaneMeasurement` gains the three
   `reported_*` fields; every fill site sets them (`None` / 0 elsewhere).
4. `measure_green_sites` selects it; its doc comment says reported, not applied; the log lines say so.
5. `pub const CFA_APERTURE_VERSION: u32 = 1;` beside `MEASURE_VERSION` (unchanged), exported like it.
6. `CHANGELOG.md`: an "Unreleased" entry.

**Tests.**
- `masked_growth_matches_the_analytic_flux` (photometry): stars from `integrated_field` (pixel-integrated) at FWHM
  1.4, 2.0, 3.0 px, Moffat β 2.5, on a 4 × 4 grid of sub-pixel phases, a checkerboard mask: the median over phases of
  masked growth / analytic 5 F flux within 1 % at FWHM 2 and 3, within 3 % at 1.4; per phase within the bound
  `4·exp(−π²σ²)` plus 0.5 %.
- `growth_curve_unmasked_is_unchanged`: existing growth tests untouched and green.
- `a_cfa_plane_reports_its_aperture_correction` (green; replaces the assertion at :569–576): `aperture_correction`
  `None`, `aperture_ratios` empty, `reported_aperture_correction` `Some`, `reported_aperture_stars ≥ 5`.
- `mosaic_corrected_flux_matches_the_truth`: a mosaic field from `integrated_field` (β 2.5 wings, fitted with
  Moffat 4), FWHM 2 and 3 px: the median over stars of `signal · reported c` / true total flux within 2 %; a second
  case with G1 / G2 gains 1.00 / 0.98 and a +3 ADU pedestal on G2: the same bar.
- `reporting_moves_no_fit`: one build, the same mosaic measured through the new path and through `FromPlane` (the
  path as it was): every `StarFit` field bitwise equal (`ap_sum` NaN on both), every `FrameSummary` field equal; the
  same for a mono plane and a debayered 3-plane file (the new path not selected: outputs equal to before).
- Not a caller: `grep -rn measure_green_sites` over `rustafits/src` and `crates/` lists only `analysis_mode`.

**Acceptance.** `cargo test` in `rustafits/` with and without `--features debug-pipeline` green; in the worktree
`cargo check -p athenaeum-core --all-targets` green; `cargo publish --dry-run --allow-dirty` in `rustafits/` passes.

## Task 2 — the probe

**Files.** `crates/athenaeum-core/examples/zp_probe.rs` (`measure`, `verdict`; one-night choice :379–397, :637–658;
the in-memory analysis on the `keep_fits` path :898–904).

**Approach.**
1. `measure`: per mosaic frame write `c`, `c_stars`, `c_se` (from `measure_raw_frame`'s `PlaneMeasurement`, the
   stored mapping arrives only in Task 4), `airmass` (solve centre RA / Dec, `frames.sitelat` / `sitelong`,
   `DATE-OBS` + exptime / 2; a frame without a site → empty, counted), `zero_point_c` = `zero_point + 2.5·log10 c`
   (empty when `c` is `None`). Mono rows leave the new columns empty.
2. `verdict --osc-c` (off by default, so the regression below holds): every night of ≥ 30 frames judged; the §6
   bars, each as one line; the shared-extinction fit (one coefficient for the rig, an intercept per night, an FWHM
   slope per night with its standard error); the "`c` does the work" comparison; the airmass-free mechanism slope and
   RMS on the `--common` join for both columns.
3. A runner script `run-osc.sh` (job folder) measuring the four nights and printing ≤ 30 lines.

**Tests.** `verdict` without `--osc-c` on `.superpowers/sdd/2026-11-15-collab-v3-wave6-5/probe-results/final/*.tsv`
(main checkout) reproduces each committed `verdict-*.txt` line for line. A unit test of the shared-extinction fit on
synthetic rows (known k, intercepts, slopes) recovers them within 1e-9.

**Acceptance.** `cargo build --release --example zp_probe -p athenaeum-core` green; the regression above.

## Task 3 — Phase A run

Controller first: a catalogue copy into `~/.athenaeum-acc/2026-11-16-osc-zp/catalog/` (SQLite `.backup` of the dev
catalogue, `PRAGMA integrity_check` ok); the deep tiers read-only from the release app's data dir (as in 6.5). The
agent runs `run-osc.sh`, judges each bar, explains every miss on the frames it sits on (a stretched JPG and its stars
first), writes `task-3-report-r0.md`. GO only if every bar passes on every judged night; any miss → STOP, the owner
gets the numbers.

## Task 4 — storage, sync, outdated flag

**Files.** `db/schema.rs` (`CREATE TABLE frame_analysis` :1976–2012, the column loop :2092–2115); `db/analysis.rs`
(`upsert_frame_analysis` :5–16, the three SELECTs :104, :125, :161 and `row_to_analysis` :314–346); `models.rs`
(`FrameAnalysis` :1076); `ts_export.rs`; `src/types/models.ts` :552; `analysis/analyzer.rs` (`map_measurement`
:74–136); `api/analysis.rs` (`row_is_stale` :440–442, `analyze_frame_set`'s filter :572–583; `get_frame_star_metrics`
:873–889 stays); the listing the Analysis tab reads (find it from `src/components/LightsAnalysisView.tsx`) in core and
in both hosts; `FrameAnalysis` literals at `api/sync.rs:8562`, `plate_solve/service.rs:852`, `:932`,
`collab/frame_meta.rs:447`, `collab/gate.rs:631`, `examples/star_probe.rs:1096`; `sync/ingest.rs:938–951`.

**Approach.** Spec §5.2. `is_mosaic(bayerpat, source_channels)` lives in one place (core `analysis`, mirroring the
reader's four patterns and one channel) and is used by every consumer in Tasks 4–5.

**Tests.** Schema: a fresh DB and an old-shape DB both end with the four columns. Mapping: a mosaic fills all four
(no qualifying star → stars 0, correction NULL, version 1); mono and RGB leave them NULL. Outdated: a mosaic row with
version NULL → outdated; with version 1 → fresh; a mono row without them → fresh; a 3-plane row with `BAYERPAT` →
fresh. `analyze_frame_set(force = false)` on a mixed set re-runs only the mosaic frames (count asserted).
`get_frame_star_metrics` on an outdated mosaic row does not re-analyse. Sync ingest with and without the `cfa_*`
keys.

## Task 5 — zero point, loss, gate

**Files.** `analysis/zero_point.rs` (`refresh_zero_point_with` :582–640, `NotReady` :407–465, `is_colour_frame`
:506–508, flux use :267–319, selectors :830–892); `analysis/transparency.rs` (`LossReason` :40–83, `load_frame_rows`
:204–213, :425, :487–500, test :1114); `collab/gate.rs` (:81–90, :317–322); `collab/frame_meta.rs` (:218, :269, :291);
`docs/logging/` dictionary.

**Approach.** Spec §5.3 and O7.

**Tests.** A mosaic analysis with `c` = 1.25 gives a zero point 2.5·log10 1.25 above the same stars without it; an
outdated mosaic row → `AnalysisBeforeColour`, nothing stored, left out of `stale_zero_point_frames`; stars 3 and
correction NULL → stored refusal and its own loss reason; a debayered file → `ColourCamera`; the reference includes
mosaic frames with a current zero point; the gate gives no verdict on an OSC frame with an empty `FILTER` and judges
it when `FILTER` is set; a mono frame's zero point and loss bit-identical before and after.

## Task 6 — the Analysis tab's count

**Files.** `src/components/LightsAnalysisView.tsx` (:541–543, :663–689) and the hook that loads the rows.

**Approach.** An outdated row counts as not analysed; the Analyze button then appears and runs `force = false`. No
new wording beyond what the button shows today (the count). Design tokens; no `@tauri-apps` import.

## Task 7 — acceptance

Headless web server on a second catalogue copy (memory `reference_headless_web_smoke_harness`): frame set 109
(LDN 1272, 65 mono + 160 OSC) — `analyze_frame_set(force = false)` re-runs exactly the OSC frames, the 65 mono
`frame_analysis` and `zp_*` rows byte-identical (SQL diff); every OSC frame then has a zero point or a stated
refusal; losses read; the gate's OSC reason is O7's; an existing stacked set reports no stale cached stage. Owed by a
human: the real window (the button and the loss column).

## Task 8 — docs, review, merge

Spec §5.4. Final review (fable) of the whole branch, the rustafits diff included. Merge: rustafits `osc-green-aperture`
into rustafits `main` with a merge commit, the pointer bump on the branch, then the branch into local `main`
(`--no-ff`). Worktree removed with `git worktree remove --force` alone (never `submodule deinit` inside it), the
main checkout's submodule fetching the merged rustafits commits first. Disk close-out. Push: the owner's word.

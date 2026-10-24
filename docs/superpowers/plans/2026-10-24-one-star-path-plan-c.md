# One Star-Measurement Path — Plan C (the Analysis switch-over) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: the user skill `lean-execution` (`~/.claude/skills/lean-execution/SKILL.md`)
> above `superpowers:subagent-driven-development`: one phase per agent, a one-page handoff, reports of at most 80 lines,
> one command per lab job printing a verdict of at most 30 lines, `haiku` for proof runs (the answer fixed in advance, a
> hard STOP), `sonnet` or stronger for judgment runs. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** the Analysis tab, Blink and the plate-solve input gate read the star numbers of the one star-measurement path
(`star_measure`, Analysis mode) instead of the legacy rustafits analysis: raw FITS and XISF input, OSC measured on the
green CFA sites, the frame verdict (Trailed / Defocused / No stars) shown in place of trail R², the gate refusing a
Trailed frame and recalibrated on the new numbers, `measure_version` /
`stars_refused` stored, Settings → Analysis cut to what the module reads, the "dB" labels fixed (spec §4, §5 Plan C).

**Architecture:**
- **athenaeum-core `analysis/`** (feature `render`, never depends on `stacking`): `input.rs` (new: raw read → planes and
  `star_measure::Domain`), `cfa.rs` (new: the green-site path), `trail.rs` (new: trail R², stored and read only by the gate),
  `analyzer.rs` (rewritten onto the module; still the single writer of `FrameAnalysis` / `StarMetric`), `config.rs`
  (the §4.5 fields). `star_measure` gains three additive seams (mask validation, a handed-in sky, the threshold
  bounds); its numbers on every unmasked plane do not move. `db/schema.rs`, `db/analysis.rs`,
  `models.rs`, `plate_solve/{service,config}.rs`, `collab/frame_meta.rs` follow. No command is added or removed; both
  hosts keep their surfaces.
- **rustafits is not touched** (owner, option A, 2026-10-24): its `ImageAnalyzer` stays in rustafits, reached after this
  plan only by `star_probe`'s `legacy-analysis` arm; the owner switches rustafits himself later.
- **Frontend:** `src/settings/registry.ts`, `AnalysisSettingsPanel.tsx`, the frame-SNR labels, the stacked-SNR formula,
  a Refused column, the Verdict column in place of Trail R², "Reject unusable frames" in place of "Reject trailed", the
  PSF model choice.
- **athenaeum-lab:** one driver `harness/acceptance/planc.sh <job>` (jobs `before`, `legacy-proof`, `inject-raw`,
  `corpus`, `gate`, `cost`, `full`), each printing a verdict of at most 30 lines; results under
  `star-lab/results/plan-c/<job>/`. `star_probe`'s `analysis-mode` arm calls the product function, so the lab measures
  the product.
- **Data and machines:** the dev Mac holds the star-test corpus (`~/Pictures/star test/{m42,m101,singles,trails}`, 77 +
  30 + 39 + 7 frames; also the `root/testdata` repo, `star-test/`), the injected raw frames (`~/.athenaeum-acc/
  star-lab-inject`, the raw entries of `manifests/inject.json`: 7 raw, 3 mosaics) and the dev catalog for the app check;
  every job except `full` runs there. The runner runs `full` (it holds `b6-final`'s scorecards). No acceptance tree is
  read: the stacking path's identity is proven by the lab scorecard and the unit pins (PC-1).

**Tech stack:** Rust (athenaeum-core), React/TS + vitest (frontend), Python 3.14 + numpy 2.4 (lab, `unittest`), the
headless `athenaeum-web` harness for the gate data (memory `reference_headless_web_smoke_harness.md`).

**Spec:** `docs/superpowers/specs/2026-10-07-one-star-measurement-path-design.md`: §1 D-1…D-3 (l.59–73), §2.2 Analysis
and Blink (l.266–272), §3 raw-mode truth, the budget and the cost bars (l.562–577), §4 (l.579–669), §5 Plan C
(l.781–787), §6 (l.790–797), §7 (l.807–821). Binding beside it: `docs/stacking/README.md` R-OSP-10 (Task 12's Plan C
carries, l.3385–3427), R-OSP-15's silent limits (l.3736–3752), the B4 CFA carry (l.2587–2588); the parked Task 6a
minors 1–2 (`.superpowers/sdd/2026-10-07-one-star-path-plan-a/review-t6a.md` l.171–191); the owner's decisions of
2026-10-24 (scope: the Analysis switch only; rustafits option A; Saturation Fraction removed; the gate refuses a frame
the module calls trailed; the Verdict column replaces Trail R² and the trailed checkbox becomes "unusable", so §4.3's
flag rule and its suppressions are not ported). Out of scope, moved to a
later Plan D: every `c` item Plan B measured and did not fix (after-report §6 / §9), F1's fixed-pixel-halo family,
psfSel, "PSF signal" on nebula, the ERP items.

## Global Constraints

- No third-party program name anywhere in this repository (code, comments, docs, commit messages); the comparison
  program is "the ERP" (spec §6). Lab automation lives in athenaeum-lab; probes stay in core `examples/`.
- Never read or write the production catalog. The app check uses the dev catalog
  (`~/Library/Application Support/com.vsharifov.athenaeum.dev/athenaeum.db`) or a `.backup` copy; the gate data uses a
  sandbox catalog (`ATHENAEUM_DB_PATH` in a scratch dir).
- Never `rustfmt` `stacking/run.rs`, `plan.rs`, `ln/mod.rs`, `ln/scale.rs`, `calibration_library/light_cal.rs`,
  `stacking/measure.rs`, `stacking/register/detect.rs`; no `cargo fmt`. `rustfmt` only on files a task names.
- Cargo with `CARGO_TARGET_DIR=.superpowers/target-acc`, `CARGO_BUILD_JOBS=4`, `--test-threads=4`; one heavy job at a
  time on the dev Mac (16 GB) with `memory_pressure` ≥ 30 % free; stop below 100 GiB free (`df -k /Volumes/BigMac`).
  `cargo test` takes ONE positional filter.
- The full `cargo test -p athenaeum-core --all-targets` before any push (spec §6), split as at Plan B B6 (the runner
  skips `sharing::iroh`, `api::collab_live`, `collab_v3_live_e2e`; the dev Mac runs those 433).
- `tracing` only; never swallow an error (log, then return or fall back); a refused star is a counted outcome.
- `AnalysisConfig` keeps its **snake_case** serialization (no `rename_all`): stored `analysis.config` documents and
  `src/types/analysis-config.ts` are snake_case today. Do not "fix" it to camelCase. `FrameAnalysis` / `StarMetric` keep
  their field names (spec §4.2: wire- and sync-compatible).
- A changed default of a serialized settings field reaches existing users only through a load-time migration of the
  stale value (`plate_solve/config.rs::migrate_config` is the pattern; memory `project_platesolve_defocus_gate.md`).
- Settings page: never a Save button, never a banner, `notify()` on failure only; `src/settings/registry.ts` is the one
  description (`docs/settings/README.md`). Design tokens only; `lucide-react` icons.
- Two backends: no command changes; the web route tests that build an `AnalysisConfig` change with it.
- New `tracing` field names are added to the logging field dictionary (`docs/logging/README.md`, "Rules when adding
  code") in the task that introduces them.
- Plan by measured quantity (memory `feedback_plan_by_measured_quantity.md`): one owner per shared input (the
  owned-quantities table at the end); a consumer reads, never re-estimates; a second estimate is a STOP and a re-audit.
- A missed bar goes to a fresh `fable` agent with the numbers and the options (memory `feedback_escalate_review_to_fable.md`);
  its verdict, after a truth check on existing data, is the ruling; the owner is informed. Push, release and spending
  stay the owner's.
- athenaeum work on branch `plan-c` (from `main` 4255a3f5), not pushed; lab work committed in athenaeum-lab `main` and
  pushed after each lab step. Merge into local `main` after a clean final review (standing mandate); push only on the
  owner's word.
- Commits authored by `eg013ra1n` / `vilen.sharifov@gmail.com`, every message ending with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01TommdmktEVTYPBaCLCxj2S`.
- Every handoff states ONE job and a tool-call budget (≤ 120–150 calls); stop an agent near 250k context and hand off.
- Every test a task names is written in full with its assertions; a test that cannot fail before its change is labelled
  a pin and sits outside the RED step.

## Review Focus

1. **A trailed frame.** The module refuses every fit of a plane it judges trailed (F-10: `stars` empty), so a direct
   mapping writes 0 stars, ecc 0 and trail R² 0, and the input gate (ecc ≥ 0.85 AND R² ≥ 0.65) can never fire on the
   frames it exists for. In the 500-star mode only 17–29 fits survive the verdict on the four hard-trailed `trails/`
   frames (measured 2026-10-24), under the trail test's 20-star minimum. Expected: the row of a trailed frame carries
   `frame_verdict` Trailed and `possibly_trailed` true with no measured star, the gate refuses it on the verdict alone,
   the Verdict column says Trailed and "Reject unusable frames" drops it (PC-6, PC-13, PC-15). Pinned in C5
   (`a_trailed_frame_is_flagged_with_no_measured_star`), C6 (`the_analyzer_records_the_frame_verdict`,
   `the_input_gate_refuses_a_trailed_verdict`) and C8 (`unusable.test.ts`).
2. **A frame with no accepted fit** (clouds, pure noise, a defocused frame of donuts, a frame whose stars are all
   saturated). SQLite stores a NaN as NULL, and every `frame_analysis` median is `NOT NULL`. Expected: a row is written
   with the legacy no-star convention (0.0 medians), its `stars_refused` count, no NaN in any column; a star whose SNR or
   HFR is NaN is stored with NULL there (PC-7, PC-8). Pinned in C5 (`a_frame_without_fits_writes_zero_medians`) and C6
   (`the_analyzer_fills_measure_version_and_stars_refused`, `star_metrics_rebuild_keeps_rows_and_accepts_null_snr`).
3. **A user's stored Analysis settings from the previous version** (keys `detection_sigma`, `fit_max_iter`,
   `saturation_fraction`, `measure_cap`, `trail_threshold`, …). Expected: the document decodes, keeps `max_stars` and
   `batch_concurrency`, takes the new fields' defaults; an undecodable document is logged, never silently replaced.
   Pinned in C5 (`an_old_stored_analysis_config_decodes_with_new_defaults`, `an_undecodable_stored_config_warns_and_falls_back`).
4. **An OSC raw frame of each Bayer layout, and a flipped one.** Expected: only real green sites are fitted, the fitted
   centres sit on the stars in native pixels, and the Blink overlay draws on them, including on a `flip_vertical` frame
   and with ellipses along a trailed frame's streaks. Pinned in C3 (`green_mask_marks_the_green_sites_of_each_layout`,
   `a_star_on_a_mosaic_is_fitted_at_its_native_position`); seen in the app in C9.
5. **A catalog full of legacy rows after the update.** Expected: the Analysis tab treats them as stale and recomputes
   them on the next analysis, Blink recomputes a stale frame on demand, the plate-solve gate ignores a legacy row (no
   refusal), the collab gate still reads it and the frame meta says which version wrote it. Pinned in C5
   (`config_hash_folds_measure_version_and_ignores_batch_concurrency`) and C6 (`the_input_gate_ignores_a_legacy_row`,
   `frame_meta_carries_measure_version_when_known`).

## Rulings fixed by this plan

Recorded in `.superpowers/sdd/2026-10-24-plan-c/rulings.md` at C0 and in `docs/stacking/README.md` R-OSP-17 at C10.

| id | ruling |
| ---- | ---- |
| PC-1 | `MEASURE_VERSION` stays 5 unless a stacking-path number moves. The policy bumps it for a plan that changes what a fit accepts or measures; Plan C changes nothing on an unmasked plane, and a bump would re-measure every user's frame sets for no change. C10's scorecard identity decides; the README line "Plan C = 6" (l.1881) and the `MEASURE_VERSION` doc comment are corrected. Analysis rows store the version that measured them; `AnalysisConfig::config_hash` folds it. |
| PC-2 | Raw input scales (spec l.133 "Plan C settles it"): an integer FITS takes its full scale from BITPIX/BZERO (255 / 65535 / 4294967295 for the unsigned conventions, the signed maximum otherwise); XISF UInt16 and XISF Float32 (already ×65535 by the reader's contract) take 65535; a float FITS whose finite maximum is ≤ 1.0 is multiplied by 65535 and takes 65535, any other float FITS keeps its values and takes 65535, or its maximum with a `warn!` when that is larger. `clip` = DATAMAX when positive, else the full scale; the gain = EGAIN, else none (the SNR omits its shot term and says so). **Amended by R-PC-2 (C2 review):** 8-bit FITS is read in its own ADU (the reader's ×256 undone), full scale 255; an integer FITS wider than 16 bits takes the float rule (65535 when its maximum ≤ 65535, else that maximum with a warn); DATAMAX is scaled ×65535 only when the plane was scaled and 0 < DATAMAX ≤ 1; `clip` = min(DATAMAX · factor, full scale). |
| PC-3 | BITPIX 16 with BZERO 0 is read signed (the FITS standard), full scale 32767. **Amended by R-PC-1 (owner, 2026-10-24):** when more than 1 % of the finite stored values are negative, the file is read as if BZERO were 32768 (stored + 32768, full scale 65535) with one `warn!` per file. Evidence: C0's census, the corpus's only such file `singles/_DSC5767.fit` (written by the ERP, 3 planes): 99.8 % negative, offset reading sky 3855 ADU and stars to 65535, while the rustafits bit-pattern reading puts the stars below the sky. |
| PC-4 | A Bayer mosaic is measured on its green sites: detection on the 2×2 sum of its two green sites (G1 + G2; **R-PC-3**, C3 review — was R + G + G + B), the knots the filter flags dropped, seeds mapped to native pixels (×2 + 0.5, pinned at seed level), the pattern carrying the file's XBAYROFF / YBAYROFF (**R-PC-4**), the fit on the native mosaic with the green-site mask, the plane sky and noise read from one green phase (owner task C3). `c` = 1 there (no qualifier; the B4 carry): Analysis star fluxes on OSC are uncorrected, a recorded limit. |
| PC-5 | A 3-plane (debayered) file is measured on its G plane with `colour: true`, `PixelNoise::Mixed`, `cfa_site_share: Some(0.5)`; `source_channels` = the file's channel count. R-OSP-15's silent limit 1 (no field-width median on a debayered plane) stays recorded, not fixed. |
| PC-6 | A frame the module judges trailed or defocused has no measured star: its medians follow PC-7 and its trail R² is 0. `possibly_trailed` = the module's trailed verdict (kept in the struct and the wire for older apps and the collab gate). Trail R² is the Rayleigh R² over the measured stars' canonical θ (C4), stored for compatibility and read only by the gate pair if C7 keeps it; the legacy flag rule and its suppressions are not ported. |
| PC-7 | A frame with no fit in its frame statistic writes 0.0 for `median_fwhm`, `median_eccentricity`, `median_snr`, `median_hfr` (the legacy convention); every other column is finite. |
| PC-8 | `star_metrics.snr` / `hfr` become nullable; a NaN per-star SNR or HFR is stored as NULL (`Option<f64>` in `StarMetric`). |
| PC-9 | Under the default Moffat 4 `median_beta` is NULL (no own-β work, R12-1); under Auto it is the frame's fitted β when its source is `Fitted`, else NULL. The optional diagnostic β at Moffat 4 is not built. |
| PC-10 | Settings → Analysis: `detection_threshold` (σ of the matched filter, default `seeds::DEFAULT_THRESHOLD` 6.0, range 2.0–20.0 as stacking's), `psf_model` (`moffat4` default, `auto`), `max_stars` = the budget N (default 500, `measure_cap` folds into it), `batch_concurrency`. Removed: `detection_sigma`, `min_star_area`, `max_star_area`, `saturation_fraction`, `mrs_layers`, `measure_cap`, `fit_max_iter`, `fit_tolerance`, `fit_max_rejects`, `trail_threshold` (no reader once the flag is the verdict); the `analysis.psf` section goes, `analysis.measurement` keeps the PSF model. |
| PC-11 | `frame_snr` is background / noise, a ratio: every "dB" label goes, and the per-filter stacked value is √Σ frame_snr² (no dB conversion). |
| PC-12 | The gate's ecc AND R² pair stays only if C7 measures that it refuses a hopeless frame the verdict lets through without refusing a solvable one; it is then recalibrated (midpoints of the gap) with a migration of the stale 0.85 / 0.65. Otherwise the pair and its two settings are removed and the gate is the verdict alone. A solvable frame carrying the Trailed verdict is a STOP to the owner (spec §7). |
| PC-13 | The frame verdict is stored: `frame_analysis.frame_verdict TEXT` = `trailed` (the module's trailed verdict) / `defocused` (its defocused verdict) / `no_stars` (neither, and no fit in the frame statistic: clouds, an empty or a fully saturated frame) / NULL (a measured frame, or a legacy row). The input gate refuses a current row whose verdict is `trailed` on that alone (owner, 2026-10-24); `defocused` and `no_stars` never refuse (the solver's own minimum-star refusal is fast). An elongated frame the module measured (G 0009) has no verdict. |
| PC-14 | C7 records every frame's solve wall time and outcome. A frame that fails after walking the FOV ladder with point-like fake stars (the H 0029 class: tracking jumps leaving dotted tracks; neither the legacy nor the new analysis flags it, measured 2026-10-24) is listed with its numbers and goes to Plan D; no rule for it is invented in Plan C. |
| PC-15 | In the UI the Verdict column replaces Trail R² (the Analysis table, Blink's frame panel, the Black Hole section, the CSV export; the R² chart metric goes) and "Reject trailed" becomes "Reject unusable frames": a frame with any verdict, or a legacy row by its old `possibly_trailed` until it is re-measured. The stored threshold key `trail` keeps its name. Owner, 2026-10-24: «замени колонку R2 … галка trailed превратится в брак». |

## Tasks at a glance

| task | job | kind | model | depends on |
| ---- | ---- | ---- | ---- | ---- |
| C0 | ledger, the before tables, the corpus census | lab | sonnet | — |
| C1 | `star_measure` seams (mask, sky, bounds, fixtures) | code | rust-engineer (sonnet) | C0 |
| C2 | raw input: planes, domain, signedness | code | rust-engineer (sonnet) | C0 census |
| C3 | the CFA green-site path | code | rust-engineer (sonnet) | C1, C2 |
| C4 | trail R² | code | rust-engineer (sonnet) | — |
| C5 | the analyzer switch, config, probe arms, the raw-mode bars | code + proof + lab | rust-engineer (sonnet); proof haiku; lab sonnet | C1–C4 |
| C6 | persistence, compatibility, gate trust, frame meta | code | rust-engineer (sonnet) | C5 |
| C7 | the plate-solve gate recalibration | lab + code | sonnet | C6 |
| C8 | frontend: Settings, labels, stacked SNR, Refused, Verdict, unusable | code | frontend-dev | C7 |
| C9 | corpus comparison, cost, the app check | lab + manual | sonnet; owner eyeballs | C7, C8 |
| C10 | full lab run, suites, docs, final review, merge | close | sonnet; final review fable | C9 |

C1, C2 and C4 are independent and may run back to back in any order; everything else is serial.

---

### Task C0: Ledger, the before tables, the corpus census

**Coupling checklist:** none (no product change).

**Files:**
- Create: `.superpowers/sdd/2026-10-24-plan-c/progress.md`, `.superpowers/sdd/2026-10-24-plan-c/rulings.md` (PC-1…PC-15
  copied from this plan, plus R-PC-* as rulings are made).
- Create (athenaeum-lab): `harness/acceptance/planc.sh` (jobs `before`; later tasks add theirs), `star-lab/bin/planc_census.py`,
  `star-lab/bin/planc_table.py`, tests in `star-lab/tests/test_planc_tools.py`.

- [ ] **Step 1:** `git switch -c plan-c` at `main` 4255a3f5; write the two ledger files.
- [ ] **Step 2: test the lab tools first.** `test_planc_tools.py`: `test_census_classifies_a_bitpix16_bzero0_file`
  (a synthetic FITS written by `starlab/fitsio.py` with BITPIX 16, BZERO 0 and 3 % negative pixels → row
  `bitpix=16 bzero=0 signed_negative_share=0.03 class=signed-suspect`); `test_table_joins_paths_by_basename` (two probe
  JSONL frame lines and an ERP CSV row → one table row with the ERP columns filled).
- [ ] **Step 3:** `planc_census.py <dir>…` → `census.tsv`: per file format, BITPIX, BZERO, NAXIS3, Bayer keyword,
  integer or float, finite min / max, the negative share under the signed reading for BITPIX 16 / BZERO 0 (FITS via
  `starlab/fitsio.py`; XISF header only).
- [ ] **Step 4:** `planc.sh before`: build `star_probe` (release); run `--paths legacy-analysis` and, separately,
  `--paths analysis-mode` on every corpus file (the current arm: an OSC mosaic read as one mono plane); keep every JSONL
  (`legacy.jsonl.gz`, `mode.jsonl.gz`); `planc_table.py` → `table.tsv` (per frame and arm: stars, FWHM, ecc, trail R²,
  trailed, refused, wall ms; the ERP's `m42-psf.csv` / `m101-psf.csv` columns beside, joined by basename); the census.
- [ ] **Step 5:** commit and push the lab; record the before line (athenaeum sha, lab sha, machine, date) in the ledger.

**Acceptance:** the verdict prints `PLANC BEFORE: COMPLETE`, all 153 corpus files accounted for (read, or failed with a
reason), the census lists every BITPIX 16 / BZERO 0 file with its negative share. Budget ≤ 100 calls.

---

### Task C1: `star_measure` seams

**Coupling checklist:** re-verifies nothing numeric (every seam is inert on an unmasked plane with no handed-in sky);
the stacking pins must stay bit-identical. Rulings it may revisit: none.

**Files:**
- Modify: `crates/athenaeum-core/src/star_measure/mod.rs` (`MeasureOptions`, `PlaneMeasurement`, `measure_plane`,
  `fit_stars_at`), `star_measure/seeds.rs` (bounds),
  `stacking/config.rs` (re-export the bounds; values unchanged), `crates/athenaeum-core/src/test_support.rs` (two
  shared fixtures).
- Test: the module's own `#[cfg(test)]` sections (`mod.rs`, `fit.rs`).

**Interfaces:**
- Produces: `MeasureOptions::sky: Option<SkyLevel>` (`None` default = computed on the native plane as today; `Some` =
  used as is, the plane not sampled); `seeds::MIN_THRESHOLD = 2.0`, `seeds::MAX_THRESHOLD =
  20.0` (`stacking::config::{MIN_SEED_THRESHOLD, MAX_SEED_THRESHOLD}` become those constants). A mask whose length is not
  `w·h` makes `measure_plane` return a measurement with no seed and no star and `fit_stars_at` an empty outcome, each
  with one `warn!` ("the pixel mask does not match the plane; nothing is measured", fields `mask_len`, `w`, `h`).
  `test_support::trailed_field(w, h, seed) -> Vec<f32>` (today's private `trail_plane_fixture`, `star_measure/mod.rs`
  l.3543, moved so C5 / C6 can use it) and `test_support::donut_field(w, h, seed) -> Vec<f32>` (annular stars that
  `measure_plane` reads `defocused`).

- [ ] **Step 1: write the failing tests.**
  - `a_mask_of_the_wrong_length_measures_nothing_and_warns`: a 64×64 plane with one star, a mask of length 64·64 − 1,
    both `SeedSpec::MatchedFilter` and `SeedSpec::Given` → `stars.is_empty()`, `seeds == 0`, one captured `warn!`
    carrying `mask_len`; no panic. Same for `fit_stars_at`.
  - `a_masked_bright_pixel_inside_the_aperture_moves_signal_only_when_unmasked` (6a minor 1): one star, one pixel
    inside its aperture raised by 10⁴ ADU; with that pixel masked `signal` equals the clean star's within 1e-9
    relative; unmasked it differs by more than 1 %.
  - `pin_rescan_sampling_radius_with_a_mask`: the mask variant of the existing near-border incremental-vs-rescan pin
    (same assertions, a checkerboard mask).
  - `a_handed_in_sky_is_used_as_is`: `opts.sky = Some(s)` with `s.median` 1234.5 → `pm.sky == s` exactly.
  - `a_donut_field_reads_defocused`: `donut_field` → `pm.defocused`, `pm.stars.is_empty()` (a pin of the new fixture).
- [ ] **Step 2:** run each; expect FAIL (missing fields; the panic on the short mask).
- [ ] **Step 3:** implement the seams. Validate the mask once at the two entry points; thread `sky` where
  `sky_level` is called today; move `trail_plane_fixture` into `test_support` as `trailed_field`.
- [ ] **Step 4:** run the new tests, then the identity pins: `cargo test -p athenaeum-core --lib star_measure` and
  `--lib stacking::measure` (includes `the_matched_filter_default_measures_exactly_these_numbers`) → all pass,
  no pin edited.
- [ ] **Step 5:** commit `feat(star_measure): mask validation, a handed-in sky, shared trailed / donut fixtures (Plan C C1)`.

**Acceptance:** the five tests pass; every existing `star_measure` and `stacking::measure` test passes unedited (the moved
fixture's three tests included).

---

### Task C2: Raw input

**Coupling checklist:** owns the raw input domain (clip, full scale, gain, signedness) the module's saturation test and
SNR shot term read. Reads C0's census.

**Files:**
- Create: `crates/athenaeum-core/src/analysis/input.rs` (registered in `analysis/mod.rs`).
- Test: its `#[cfg(test)]` section (synthetic files in a temp dir; XISF through `fits_writer::xisf_writer`).

**Interfaces:**
- Produces: `pub struct RawFrame { pub planes: RawPlanes, pub w: usize, pub h: usize, pub domain: star_measure::Domain,
  pub flip_vertical: bool, pub channels: usize }`; `pub enum RawPlanes { Mono(Vec<f32>), Cfa { mosaic: Vec<f32>,
  pattern: astroimage::BayerPattern }, Rgb([Vec<f32>; 3]) }`; `pub fn read_raw_frame(path: &Path) -> anyhow::Result<RawFrame>`.
- Reads pixels through `astroimage::ImageConverter::read_raw` (spec §4.1); header keywords through
  `fits_parser::FitsHeader::from_path` (FITS) and `fits_parser::extract_xisf_header` + `parse_stored_header_keys(FileFormat::Xisf, …)` (XISF).
  The rustafits reader stores BITPIX 16 / BZERO 0 as the unswapped bit pattern in `u16`; `input.rs` reinterprets it as
  `i16` (PC-3).

- [ ] **Step 1: write the failing tests.**
  - `bitpix16_bzero0_reads_signed`: 1000 pixels, 5 of them −5 and the rest positive (≤ 1 % negative), written as
    big-endian `i16` → read as stored; `Domain::RawAdu { full_scale: 32767.0, .. }`.
  - `bitpix16_bzero32768_reads_unsigned`: stored −32768 + 40000 → 40000.0; full scale 65535.
  - `bitpix16_bzero0_holding_offset_data_reads_with_the_32768_offset` (R-PC-1): stored −30000 / 0 / 32767 with more
    than 1 % negative → 2768 / 32768 / 65535, full scale 65535, one `warn!` with `path` and `negative_share`.
  - `a_unit_float_fits_is_scaled_to_adu`: BITPIX −32, maximum 0.8 → planes ×65535, full scale 65535.
  - `an_adu_float_fits_keeps_its_values`: maximum 42000 → unchanged, full scale 65535; maximum 70000 → full scale 70000
    with one `warn!`.
  - `datamax_sets_clip_and_egain_sets_gain`: DATAMAX 60000, EGAIN 0.8 → `clip` 60000, `gain_e_per_adu` Some(0.8); no
    EGAIN → None.
  - `xisf_uint16_and_float32_read_at_full_scale_65535`.
  - `a_bayer_frame_reads_as_cfa_with_its_layout` (BAYERPAT RGGB, NAXIS 2) and `a_three_plane_frame_reads_as_rgb`
    (NAXIS3 3, `channels` 3).
  - Real-data pin, `#[ignore]`, `STAR_TEST_DIR` env: `dsc5767_reads_with_the_32768_offset` (`singles/_DSC5767.fit`:
    `Rgb`, G plane median within 1 ADU of 3855, maximum 65535).
- [ ] **Step 2:** run; expect FAIL (module missing).
- [ ] **Step 3:** implement `read_raw_frame` with PC-2 and PC-3; log a reported file once at `warn!` with `path`.
- [ ] **Step 4:** run the tests, the ignored pin with `STAR_TEST_DIR="$HOME/Pictures/star test"`, and
  `cargo check -p athenaeum-core --no-default-features`.
- [ ] **Step 5:** commit `feat(analysis): raw frame input with the signed BITPIX 16 rule (Plan C C2)`.

**Acceptance:** all tests pass; `_DSC5767.fit` reads with the offset (R-PC-1).

---

### Task C3: The CFA green-site path

**Coupling checklist:** owns the CFA plane's sky and noise and its seeds with their field width; consumers are the
threshold, the saturation test, the SNR and the Analysis `background` / `noise`. Judged on the raw truth by C5's
`inject-raw` job (completeness, the FWHM bar); a miss there comes back to this task. Rulings it may revisit: PC-4's
offset and its sky / noise source, nothing else.

**Files:**
- Create: `crates/athenaeum-core/src/analysis/cfa.rs`.
- Test: its `#[cfg(test)]` section (synthetic mosaics; `test_support` fields where they fit).

**Interfaces:**
- Consumes: C1's `MeasureOptions::sky`, the mask validation; C2's `RawPlanes::Cfa`.
- Produces: `pub fn green_mask(w: usize, h: usize, pattern: BayerPattern) -> Vec<bool>`;
  `pub fn superpixel_sum(mosaic: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize)` (w/2 × h/2, an odd last row or
  column left out); `pub fn green_phase(mosaic: &[f32], w: usize, h: usize, pattern: BayerPattern) -> (Vec<f32>, usize, usize)`
  (the green sites of one phase, w/2 × h/2); `pub fn measure_green_sites(mosaic: &[f32], w: usize, h: usize,
  pattern: BayerPattern, base: &MeasureOptions, pool: Option<&Arc<rayon::ThreadPool>>) -> star_measure::PlaneMeasurement`.
- The steps of `measure_green_sites` (PC-4): the matched filter on the super-pixel sum at `base`'s threshold (its noise
  `PixelNoise::White`); seeds and the filter's field width mapped to native pixels (×2, plus the offset C3's test fixes);
  the sky (`domain::sky_level`) and the noise model on `green_phase`; then `measure_plane` on the mosaic with
  `SeedSpec::Given`, the green mask, `field_width`, `sky`, `noise`, `colour: false`, `PixelNoise::White`,
  `cfa_site_share: None` and `base.mode`. The super-pixel detection keeps its `fieldWidthAtFloor` warn (the detection
  path); the handed width carries the flag to the fit.

- [ ] **Step 1: write the failing tests.**
  - `green_mask_marks_the_green_sites_of_each_layout`: RGGB → (0,0) false, (1,0) true, (0,1) true, (1,1) false; BGGR the
    same; GRBG and GBRG the inverse; length `w·h`.
  - `a_star_on_a_mosaic_is_fitted_at_its_native_position`, one case per layout: a 256×256 mosaic, site gains R 0.5,
    G 1.0, B 0.4, sky R 900 / G 1000 / B 800 ADU, Gaussian noise, one Moffat β 4 star of FWHM 4.0 px at (100.3, 80.7)
    plus 40 fainter stars → the bright star's fit within 0.1 px of (100.3, 80.7) and its FWHM within 2 % of 4.0.
  - `the_cfa_sky_and_noise_read_only_green_sites`: the same mosaic with every R and B site raised by 5000 ADU →
    `pm.sky.median` and `pm.noise.sigma` bit-identical, the accepted fits identical.
  - `a_hot_red_site_is_not_a_star`: one R site at 60000 ADU on an empty sky → no accepted fit within 3 px of it.
  - `an_odd_mosaic_leaves_its_last_row_and_column_out_of_detection_only`: 257×255 → `superpixel_sum` 128×127; a star on
    the last column is still fitted when the seed lands within the frame.
  - `a_cfa_plane_has_no_aperture_correction`: `pm.aperture_correction == None` (the B4 carry).
- [ ] **Step 2:** run; expect FAIL.
- [ ] **Step 3:** implement. The offset of the native mapping is whatever makes the position test pass; write it as one
  named constant with a doc line saying the pixel-centre convention it follows.
- [ ] **Step 4:** run the tests; commit `feat(analysis): OSC measured on the green CFA sites (Plan C C3)`.

**Acceptance:** the six tests pass; `cargo test -p athenaeum-core --lib star_measure` still passes unedited.

---

### Task C4: Trail R²

**Coupling checklist:** owns trail R²; reads the measured stars' canonical θ (F-2). Its only consumer is the gate pair, if
C7 keeps it, and the stored field (PC-6).

**Files:**
- Create: `crates/athenaeum-core/src/analysis/trail.rs`.
- Test: its `#[cfg(test)]` section.

**Interfaces:**
- Produces: `pub fn trail_r_squared(thetas: &[f64], field_fwhm: f64) -> f64`: the Rayleigh R² of the doubled angles,
  |Σ e^{2iθ}|² / n² (spec §4.3; `rustafits/src/analysis/mod.rs` l.1052–1059), 0.0 with fewer than 20 angles or a field
  FWHM under 2 px. The legacy flag rule and its two suppressions are not ported (PC-6, PC-15).

- [ ] **Step 1: write the failing tests.** `twenty_aligned_angles_read_near_one` (θ 0.3 ± 0.02, FWHM 3 → > 0.95);
  `nineteen_angles_read_zero`; `a_field_fwhm_below_two_reads_zero` (1.9); `uniform_angles_read_near_zero` (200 evenly
  spaced over [−π/2, π/2) → < 0.01); `mirrored_angles_read_the_same` (θ → −θ gives the same R² to 1e-12).
- [ ] **Step 2:** run; expect FAIL. **Step 3:** implement. **Step 4:** run; pass.
- [ ] **Step 5:** commit `feat(analysis): trail R² on the canonical angle (Plan C C4)`.

**Acceptance:** the five tests pass.

---

### Task C5: The analyzer switch, the config, the probe arms

**Coupling checklist:** a consumer of every owned quantity (C2's domain, C3's CFA inputs, C4's R², Plan A's frame
statistic); it estimates nothing itself. Re-verifies: the legacy probe arm's output unchanged (a proof).

**Files:**
- Modify: `crates/athenaeum-core/src/analysis/analyzer.rs` (rewritten), `analysis/config.rs`,
  `crates/athenaeum-core/src/api/analysis.rs` (l.229–309 the batch, l.477–560 the on-demand path),
  `crates/athenaeum-core/src/api/settings.rs` (defaults), `crates/athenaeum-web/src/routes/analysis.rs` (its tests),
  `crates/athenaeum-core/examples/star_probe.rs` (the two Analysis arms), `src/types/analysis-config.ts` (regenerated),
  `docs/logging/README.md` (dictionary).
- Test: `analysis/analyzer.rs` and `analysis/config.rs` test sections; web route tests.

**Interfaces:**
- Consumes: C1–C4.
- Produces: `pub fn measure_raw_frame(frame: &RawFrame, config: &AnalysisConfig, pool: Option<&Arc<rayon::ThreadPool>>)
  -> star_measure::PlaneMeasurement` (the product's measurement: mono → `measure_plane`; CFA → `cfa::measure_green_sites`;
  RGB → `measure_plane` on G per PC-5; `MeasureOptions { domain: frame.domain, psf_model, seeds: MatchedFilter
  { threshold: config.detection_threshold }, mode: Mode::Budget { budget: config.max_stars }, .. }`);
  `pub fn map_measurement(pm: &PlaneMeasurement, frame: &RawFrame, config: &AnalysisConfig, config_hash: &str)
  -> (FrameAnalysis, Vec<StarMetric>)`; `pub fn analyze_raw_frame(frame: &RawFrame, config: &AnalysisConfig,
  pool: Option<&Arc<rayon::ThreadPool>>, config_hash: &str) -> (FrameAnalysis, Vec<StarMetric>)` = map ∘ measure (CPU
  only, so the batch keeps its read overlap); `pub fn analyze_frame(path: &str, config: &AnalysisConfig, pool: Option<&Arc<rayon::ThreadPool>>,
  config_hash: &str) -> Result<(FrameAnalysis, Vec<StarMetric>, bool)>`. `build_analyzer` and every `astroimage::
  ImageAnalyzer` use leave `src/`.
- `AnalysisConfig` after PC-10: `detection_threshold: f64` (`#[serde(default)]` → 6.0), `psf_model: AnalysisPsfModel`
  (`#[serde(rename_all = "lowercase")]` enum `Moffat4` (default) | `Auto`, `#[serde(default)]`), `max_stars: u32`,
  `batch_concurrency: u32`; `validate` bounds: threshold in `[seeds::MIN_THRESHOLD, seeds::MAX_THRESHOLD]`, `max_stars`
  10–2000, concurrency ≤ 16. `config_hash()` = SHA-256 of the config JSON
  (concurrency zeroed) followed by `MEASURE_VERSION`; a private `config_hash_for(version: u32)` serves the test.
  `load_config` logs a decode error at `warn!` (`key`, `error`) before taking the default.
- The mapping (spec §4.2 with PC-6…PC-9): `stars_detected` = the matched filter's seeds; `median_fwhm` / `median_eccentricity` /
  `median_snr` / `median_hfr` = `FrameAggregates` over `pm.stars`, 0.0 when `None` (PC-6, PC-7); `background` = `pm.sky.median`, `noise` = `pm.noise.sigma` (raw domain: native ADU);
  `frame_snr` = background / noise (0.0 when noise ≤ 0); `snr_weight` = median(flux)² / (noise² · background) and
  `psf_signal` = median(amplitude) / noise, both over the frame-statistic fits; `detection_threshold` = the threshold;
  `trail_r_squared` = C4 over the frame-statistic fits' θ at the frame FWHM; `possibly_trailed` = `pm.trailed`;
  `median_beta` per PC-9; `quality_score` None (`measure_version`, `stars_refused` and `frame_verdict` are C6's). Per star: x, y (native,
  unflipped), `peak` = amplitude, `flux` = signal, `fwhm` = the per-star width the frame statistic averages
  (`frame.rs`), `fwhm_x`, `fwhm_y`, `theta` (canonical), `eccentricity` = √(1 − (fwhm_y / fwhm_x)²), `snr` / `hfr` as
  numbers here (C6 makes them `Option`), `beta`, `fit_method` "moffat", `fit_residual` = residual.
- `star_probe`: `legacy-analysis` builds `astroimage::ImageAnalyzer` itself with the legacy builder chain and today's
  defaults (moved verbatim from `analyzer.rs`, so its output cannot change); `analysis-mode` reads with
  `read_raw_frame`, measures with `measure_raw_frame`, keeps its star / refusal / frame lines from the `PlaneMeasurement`
  and adds the `FrameAnalysis` JSON to its frame line (`"product": {…}`); its own `raw_scales` / domain check go (the
  product's rules replace them).

- [ ] **Step 1: write the failing tests.**
  - `analysis_of_a_synthetic_mono_frame_maps_every_column`: a raw u16 frame (1024×768, sky 1000 ADU, 300 Moffat β 4
    stars, FWHM 3.5) → `median_fwhm` within 2 % of 3.5, `median_eccentricity` < 0.1, `stars_detected` ≥ 285,
    `fit_method` "moffat" on every star, `frame_snr == background / noise` exactly, `background` within 1 % of 1000,
    every f64 finite.
  - `a_frame_without_fits_writes_zero_medians`: pure noise, and `test_support::donut_field` → the four
    medians 0.0, every f64 of the `FrameAnalysis` finite.
  - `a_trailed_frame_is_flagged_with_no_measured_star`: `test_support::trailed_field` → `possibly_trailed`, the four
    medians 0.0, `trail_r_squared` 0.0, every f64 finite.
  - `moffat4_writes_no_median_beta_and_auto_writes_the_fitted_one`.
  - `an_osc_mosaic_is_measured_on_green_and_a_three_plane_file_on_g`: C3's RGGB mosaic through `analyze_raw_frame` →
    the bright star's `StarMetric` within 0.1 px of (100.3, 80.7), `source_channels` 1; a 3-plane frame with one star
    only in G and one only in R → the G star measured, the R star absent, `source_channels` 3.
  - `an_old_stored_analysis_config_decodes_with_new_defaults`: `{"detection_sigma":5.0,"min_star_area":5,
    "max_star_area":2000,"saturation_fraction":0.95,"max_stars":800,"trail_threshold":0.4,"mrs_layers":0,
    "measure_cap":2000,"fit_max_iter":25,"fit_tolerance":0.0001,"fit_max_rejects":5,"batch_concurrency":3}` →
    `max_stars` 800, `batch_concurrency` 3, `detection_threshold` 6.0, `psf_model` Moffat4 (`trail_threshold` ignored).
  - `an_undecodable_stored_config_warns_and_falls_back`: `"{not json"` → the default and one captured `warn!` with `key`.
  - `config_hash_folds_measure_version_and_ignores_batch_concurrency`: the default's hash equals a pinned literal (set
    once, at GREEN); `config_hash_for(MEASURE_VERSION + 1)` differs; concurrency 1 and 9 give the same hash.
  - Web: the existing `set_analysis_config` route tests rebuilt on the new fields.
- [ ] **Step 2:** run; expect FAIL. **Step 3:** implement (analyzer, config, both `api/analysis.rs` call sites, settings
  defaults, the probe arms). **Step 4:** regenerate the TS types (`TS_RS_WRITE=1 cargo test -p athenaeum-core --test
  ts_contract`), run `cargo test -p athenaeum-core --lib analysis`, `--lib api::analysis`, `cargo test -p athenaeum-web`.
- [ ] **Step 5 (haiku proof agent):** `planc.sh legacy-proof`: the `legacy-analysis` arm on the corpus against C0's
  `legacy.jsonl.gz`, every line equal except wall-time keys. Fixed answer: `LEGACY PROOF: IDENTICAL`; anything else is
  a STOP (no commit, no re-run).
- [ ] **Step 6:** commit `feat(analysis): the Analysis tab measures through the one star path (Plan C C5)`.
- [ ] **Step 7 (fresh lab agent, sonnet): `planc.sh inject-raw`.** The product arm (`star_probe --paths analysis-mode
  --budget 100000`, so N ≥ seeds) on the 7 raw entries of `manifests/inject.json` (mono and mosaics); a new
  `star-lab/bin/planc_raw_bars.py` (its test in `test_planc_tools.py`: a fit 1.4 px from a truth star matches, 1.6 px
  does not) matches the fits to the injected truth within 1.5 px.

**Acceptance:** the tests pass; the legacy proof is identical; `cargo check --no-default-features` finishes; the raw-mode
bars (spec l.562–565): completeness ≥ 99 % of the injected stars above the detection threshold on every raw entry, and
F-1 / F-4's FWHM bar on the injected stars (frame FWHM error against the truth's `pointfit` ≤ 1 % at FWHM ≥ 2 px, ≤ 3 %
below). A bar miss goes back to C3 (or C2 for a mono entry) through Fable with the per-entry table.

---

### Task C6: Persistence, compatibility, gate trust, frame meta

**Coupling checklist:** none numeric; makes `measure_version` and `stars_refused` durable and visible.

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (after l.2023), `db/analysis.rs` (upsert / get / both star-metric
  readers), `models.rs` (l.1038–1084), `analysis/analyzer.rs` (fill the three fields; NaN → None), `plate_solve/service.rs`
  (`input_gate_reason`, l.616–634), `collab/frame_meta.rs` (l.127–135 and its tests), `sync/ingest.rs` tests,
  `src/types/models.ts` (regenerated).

**Interfaces:**
- Produces: `FrameAnalysis { #[serde(default)] measure_version: Option<u32>, #[serde(default)] stars_refused: Option<i64>,
  #[serde(default)] frame_verdict: Option<FrameVerdict>, .. }` with `pub enum FrameVerdict { Trailed, Defocused, NoStars }`
  (`#[serde(rename_all = "snake_case")]`, stored as the same text, TS `'trailed' | 'defocused' | 'no_stars'`); `StarMetric { snr:
  Option<f64>, hfr: Option<f64>, .. }`; columns `frame_analysis.measure_version INTEGER`, `frame_analysis.stars_refused
  INTEGER`, `frame_analysis.frame_verdict TEXT` (NULL = no verdict, or written by the legacy analysis);
  `star_metrics.snr` / `hfr` nullable;
  the collab frame meta key `measureVersion` (present only when known; append-only on the wire).

- [ ] **Step 1: write the failing tests.**
  - `migration_adds_measure_version_and_stars_refused_and_keeps_rows`: a catalog at today's schema with one row →
    `init_db` → the row reads with `None` / `None`, every other field equal.
  - `star_metrics_rebuild_keeps_rows_and_accepts_null_snr`: today's `star_metrics` with two rows → after `init_db` both
    rows read back equal, an insert with `snr: None, hfr: None` succeeds, a second `init_db` is a no-op (row count, index
    present).
  - `frame_analysis_round_trips_measure_version_and_stars_refused` (Some(5), Some(17)).
  - `the_analyzer_fills_measure_version_and_stars_refused`: C5's pure-noise and trailed fixtures through
    `analyze_raw_frame` → `measure_version == Some(MEASURE_VERSION)`, `stars_refused` = the sum of `refused.by_reason`
    (the budget's `not_selected` / `trimmed` excluded); a star with a NaN SNR or HFR maps to `None`.
  - `the_analyzer_records_the_frame_verdict`: `trailed_field` → `Some(Trailed)`, `donut_field` → `Some(Defocused)`, pure
    noise → `Some(NoStars)`, a clean field → `None`.
  - `an_old_frame_analysis_json_decodes_with_none` and `a_frame_analysis_json_with_an_unknown_key_decodes` (both
    directions of the sync package).
  - `the_input_gate_ignores_a_legacy_row` (ecc 0.95, R² 0.9, `measure_version` None → `None`).
  - `the_input_gate_refuses_a_trailed_verdict` (current, `frame_verdict` Trailed, ecc 0.0, R² 0.0 → `Some(_)`, the
    message saying the stars are trails the measurement could not fit).
  - `the_input_gate_refuses_a_current_row_by_the_pair` (current, no verdict, ecc 0.95, R² 0.9 → `Some(_)`; C7 removes
    or re-pins it) and `the_input_gate_passes_an_elongated_measured_frame` (current, no verdict, ecc 0.73, R² 0.57 →
    `None`).
  - `the_input_gate_never_refuses_defocused_or_no_stars`.
  - `frame_meta_carries_measure_version_when_known` (`"measureVersion": 5`) and `frame_meta_omits_it_for_a_legacy_row`.
  - A sync ingest test: a package summary without the two keys ingests and reads `None`.
- [ ] **Step 2:** run; expect FAIL. **Step 3:** implement: pragma-guarded `ALTER`s (the schema.rs l.1985–2010
  pattern, three columns); the `star_metrics` rebuild inside one transaction when `pragma_table_info('star_metrics')` shows `snr`
  `NOT NULL` (create the new table, copy, drop, rename, recreate `idx_star_metrics_analysis_id`); the gate reads only a
  row at the current version (`debug!` "input gate: the frame's analysis predates the current measurement" otherwise),
  refuses on `frame_verdict` Trailed first (PC-13), then applies the ecc AND R² pair (C7 decides its fate, PC-12).
- [ ] **Step 4:** regenerate TS; run `cargo test -p athenaeum-core --lib db::`, `--lib plate_solve`, `--lib collab::frame_meta`,
  `--lib sync::ingest`, `--test ts_contract`; `cargo test -p athenaeum-web`.
- [ ] **Step 5:** commit `feat(analysis): measure_version, stars_refused and the frame verdict stored; the gate trusts only current rows (Plan C C6)`.

**Acceptance:** the tests pass; the collab wire pins move only by the appended key.

---

### Task C7: The plate-solve gate recalibration

**Coupling checklist:** owns the gate's ecc / R² pair (keep and recalibrate, or remove); reads C4's R², C5's ecc and
C6's `frame_verdict`. Rulings it may revisit: PC-12 only (PC-13's verdict rule is the owner's).

**Files:**
- Modify: `crates/athenaeum-core/src/plate_solve/config.rs` (defaults and `migrate_config` l.193–225, or the two
  fields' removal), `plate_solve/service.rs` (`input_gate_reason`), `docs/platesolving/input-and-acceptance-gates.md`
  (the 2026-09-05 calibration kept, the new measurement beside it; the "Known gap" paragraph rewritten from it). The
  Settings side of the outcome (Plate Solving → Input Gate) is C8's.
- Lab: `planc.sh gate`, `star-lab/bin/planc_gate.py` (+ test).

- [ ] **Step 1 (lab agent): the data.** `planc.sh gate` on the dev Mac with the release `athenaeum-web` and a sandbox
  catalog: scan `~/Pictures/star test/{trails,m101,singles}` and the first 20 of `m42`; `analyze_frame_set` on each set;
  the solver's catalogs taken from the dev app-data (`catalogs/`, linked or set in the sandbox's settings; the agent
  confirms one m42 frame solves before the batch); plate-solve every frame with the input gate OFF; per frame record
  solved / refused, scale against the header, rms, the solve wall time, whether it reached the FOV ladder (from the solve
  log), `median_eccentricity`, `trail_r_squared`, `possibly_trailed`, `frame_verdict`. Add the lab's `trails` family through the product
  probe arm (ecc, R² only). A frame is "hopeless" when it fails to solve or solves at a scale more than 10 % off the
  header; "solvable" otherwise.
- [ ] **Step 2: the ruling (PC-12, PC-13, PC-14).** First: no solvable frame may carry `frame_verdict` trailed (one that
  does is a STOP to the owner: the verdict rule would refuse a solvable frame). Then, over the hopeless frames WITHOUT
  the verdict, `planc_gate.py` finds whether some pair (ecc, R²) refuses at least one of them under the AND rule while
  refusing no solvable frame. **Keep:** such a pair exists → each new default is the midpoint of the gap on its axis,
  rounded to 0.01. **Remove:** none exists → the pair adds nothing beyond the verdict. Either way the report lists every
  hopeless frame the gate still lets through with its solve wall time (the H 0029 class, PC-14) for Plan D.
- [ ] **Step 3: write the failing tests.** Keep: `default_gate_values_are_the_recalibrated_ones` (the two new literals)
  and `migrate_config_moves_the_stale_gate_defaults_only` (a stored 0.85 / 0.65 → the new values; 0.80 / 0.65 → 0.80 /
  new, each field on its own; a stored new value stays); C6's pair test re-pinned at the new values. Remove:
  `a_stored_config_with_the_removed_gate_fields_decodes` (a stored document carrying `input_max_eccentricity` /
  `input_min_trail_r2` decodes, the switch kept) and C6's pair test deleted with a note in the commit.
- [ ] **Step 4:** implement the outcome (Keep: `STALE_INPUT_MAX_ECCENTRICITY = 0.85`, `STALE_INPUT_MIN_TRAIL_R2 =
  0.65`; Remove: the two fields and the pair leave `PlateSolveConfig` and `input_gate_reason`); regenerate TS; run
  `cargo test -p athenaeum-core --lib plate_solve`; write the doc with the frame table's summary.
- [ ] **Step 5:** commit `feat(plate_solve): the input gate on the one star path's verdict (Plan C C7)`.

**Acceptance:** the outcome (Keep or Remove) recorded as R-PC-gate with its table, the tests of that outcome passing, the
per-frame solve-time table and the list of hopeless frames the gate lets through committed with the lab results.

---

### Task C8: Frontend

**Coupling checklist:** none numeric; displays C5 / C6's fields.

**Files:**
- Modify: `src/settings/registry.ts` (Analysis l.384–520; Plate Solving → Input Gate l.583–600 per C7's outcome),
  `src/components/analysis/AnalysisSettingsPanel.tsx`, the Plate Solving settings panel (per C7), `docs/settings/README.md`
  (l.60 and the Analysis text), `src/components/calibration/LightsAnalysisTable.tsx` (l.7, l.206, l.259–260, l.350,
  l.360, l.457; a Refused column), `src/components/blink/FrameInfoPanel.tsx` (l.78–90; a Refused row, β "—" when null),
  `src/components/calibration/BlackholedFramesSection.tsx` (l.134, l.170–172), `src/components/analysis/chartDataTransform.ts`
  (l.102–107), `src/components/blink/FrameList.tsx` (l.222), `src/components/calibration/RejectionThresholdBar.tsx`
  (l.29, l.157), `src/components/LightsAnalysisView.tsx` (l.171–193, l.457–484, l.538–554),
  `src/components/calibration/CameraFilterTree.tsx` (l.13, l.323), `src/components/calibration/MergedCameraFilterTree.tsx`
  (l.13, l.234).
- Create: `src/components/analysis/stackedSnr.ts` (+ `.test.ts`), `src/components/analysis/unusable.ts` (+ `.test.ts`).

**Interfaces:**
- Consumes: the regenerated `src/types/analysis-config.ts` (C5) and `src/types/models.ts` (C6).
- Produces: `export function stackedFrameSnr(values: number[]): number | undefined` = √Σ v² over the finite values,
  `undefined` when none (PC-11); `export function isUnusable(a: FrameAnalysis): boolean` = `a.frame_verdict != null`,
  or for a legacy row (`measure_version == null`) its `possibly_trailed` (PC-15); `export function verdictLabel(v:
  FrameVerdict | null): string` = "Trailed" / "Defocused" / "No stars" / "—".

- [ ] **Step 1: write the failing tests.** `stackedSnr.test.ts`: [3, 4] → 5; [] → undefined; [NaN, 3] → 3; [2] → 2.
  `unusable.test.ts`: a row with `frame_verdict` 'no_stars' → true; a current row with no verdict and
  `possibly_trailed` false → false; a legacy row (`measure_version` null) with `possibly_trailed` true → true;
  `verdictLabel('trailed')` → "Trailed", `verdictLabel(null)` → "—".
- [ ] **Step 2:** run `npx vitest run src/components/analysis`; expect FAIL.
- [ ] **Step 3:** implement both helpers; use `stackedFrameSnr` in `LightsAnalysisView` (the per-filter value shown
  without "dB") and `isUnusable` in its rejection filter (l.457–484) in place of `possibly_trailed`.
- [ ] **Step 4: Settings (PC-10).** `analysis.detection`: Detection Threshold (`detection_threshold`, 2–20, step 0.5,
  description "Stars are seeded where the matched filter stands this many sigma above the noise"), Max Stars (the budget:
  "Good, unsaturated stars measured per frame, spread over the field"). `analysis.measurement`: PSF Model select,
  Moffat 4 (default) / Auto (the frame's own β). Remove Detection Sigma, Min / Max Star Area, Saturation Fraction, Trail
  Threshold, MRS Wavelet Layers and the whole `analysis.psf` section; `analysis.rejection`'s `trailed` entry becomes
  "Reject unusable frames" (help: "Frames the star measurement marks Trailed, Defocused or No stars"; stored key `trail`
  unchanged); Plate Solving → Input Gate follows C7 (Remove: only the switch stays, its help "A frame whose own analysis
  marks it Trailed is refused before solving"; Keep: the two fields with the new defaults); update keywords; the panels
  autosave as today.
- [ ] **Step 5: labels and columns.** Every frame-SNR "dB" goes (table unit, Blink rows, the rejection bar label
  "Frame SNR <" with placeholder "ratio", the CSV header "Frame SNR", both trees' suffix and doc comments). A sortable
  "Refused" column (`stars_refused`, "—" when null) and a Refused row in `FrameInfoPanel`; `median_beta` null shows "—".
  **The Verdict column replaces Trail R² (PC-15):** in `LightsAnalysisTable` (sortable by severity: none, No stars,
  Defocused, Trailed; no average), `FrameInfoPanel` (a Verdict row in place of the Trail R² row and its badge),
  `BlackholedFramesSection` and the CSV export ("Verdict", the label text); `verdictLabel` gives the text, Trailed and
  Defocused in `text-warning`, No stars in `text-content-muted`, with the tooltip "The star measurement could not fit
  this frame's stars: they are trails" / "…: they are out-of-focus rings" / "…: it found no star to fit". The rejection
  bar's checkbox reads "Unusable". The `trail_r_squared` chart metric is removed.
- [ ] **Step 6:** run `npx vitest run` (all), `npm run build`; grep `src/` for `dB` next to `snr` and for
  `trail_r_squared` → none left outside `src/types/`.
- [ ] **Step 7:** commit `feat(ui): Analysis settings, labels, Refused, the Verdict column and Reject unusable frames (Plan C C8)`.

**Acceptance:** vitest and the build pass; no frame-SNR "dB" and no displayed Trail R² remain; the Settings page shows
only PC-10's fields and C7's gate outcome.

---

### Task C9: Corpus comparison, cost, the app check

**Coupling checklist:** a checkpoint; owns no fix. A miss goes back to the owner task (C2–C5) and C9 runs again.

**Files:**
- Lab: `planc.sh corpus`, `planc.sh cost`; `star-lab/bin/planc_table.py` gains the per-set summary and the outlier list.
- Ledger: `.superpowers/sdd/2026-10-24-plan-c/c9-report.md`.

- [ ] **Step 1 (lab agent): `planc.sh corpus`.** Both arms on all 153 frames → the per-frame table (C0's columns plus
  the product's `stars_refused`, `measure_version`); per set the median new / legacy ratio of FWHM and ecc; the ERP's
  columns beside (reported, never a bar: the corpus has no truth). Every frame with |ΔFWHM| > 10 % or a trailed flag
  that changed is listed with a stretched JPG beside its row (look at the outlier frame first). STOP conditions: a frame
  that read before and fails now; a frame with zero fits that had more than 50 before, unless its refusal breakdown
  explains it (defocused, trailed).
- [ ] **Step 2 (same agent): `planc.sh cost`.** The product arm's measure wall per frame on m42 (26 Mpx, 77 frames,
  read excluded, default pool), median; the OSC frames of `singles` reported beside. Bar (spec l.574): ≤ 0.6 s per m42
  frame. The on-demand Blink path (`get_frame_star_metrics` through the headless server): its measure part held to the
  same bar, its read reported.
- [ ] **Step 3 (controller): the app.** `npm run tauri dev` on the dev catalog: analyze one mono and one OSC frame set;
  check the Analysis table (no dB, Refused, the Verdict column reading Trailed on a hard-trailed frame, "Reject unusable
  frames" dropping it, values plausible against the corpus table), Blink overlays on a mono, an OSC
  and a trailed frame (ellipses on the stars, along the streaks; a `flip_vertical` frame if the dev catalog has one),
  Settings → Analysis (fields, autosave), the calibration panels. Screen capture is blocked: the owner eyeballs the
  screens, the controller greps the log for errors.
- [ ] **Step 4:** write `c9-report.md` (≤ 80 lines: the summary rows, the outliers with their explanation, the cost,
  the owner's look).

**Acceptance:** no STOP condition open; the cost bar met (a miss → Fable, then owner informed); the owner's look done.

---

### Task C10: Full lab run, suites, docs, final review, merge

**Coupling checklist:** a checkpoint; owns no fix.

**Files:**
- Modify: `docs/stacking/README.md` (R-OSP-17: PC-1…PC-15, every R-PC ruling, every run line; l.1881–1882 per PC-1),
  `crates/athenaeum-core/src/star_measure/mod.rs` (the `MEASURE_VERSION` doc comment per PC-1),
  `docs/superpowers/open-items.md` (the Plan C entries closed; a **Plan D** section listing the moved items verbatim from
  the after-Plan-B report §6, plus C7's list of hopeless frames the gate lets through (the H 0029 class, PC-14); the
  desktop smokes owed; release notes owed).
- Create: `docs/superpowers/research/2026-10-XX-star-measurement-after-plan-c.md` (the after note).

- [ ] **Step 1 (runner, lab agent): `planc.sh full`** at the plan-c head: the full uniform lab run (`lab_cycle` full
  manifest, inject, gates). Bars: the scorecard against `b6-final` on every stacking-path row: moved 0, max |Δ| 0
  (PC-1 holds; otherwise STOP → Fable: `MEASURE_VERSION` 6 and its pins, a re-plan); the Analysis-mode rows: budget
  coverage ≥ 90 % of the cells holding measurable stars (D-3), the raw-mode bars as at C3; gates 13 / 13.
- [ ] **Step 2: suites.** Core all targets split as at B6 (runner + the dev Mac's 433 network tests), `cargo test -p
  athenaeum-web`, `npx vitest run`, `cargo check --no-default-features`; the controller re-runs them, never trusting a
  report alone.
- [ ] **Step 3: docs.** R-OSP-17 (with R-OSP-15's per-site table of `fieldWidthAtFloor`: the CFA path's super-pixel
  detection warns, given-seed callers carry the flag without a warn); the after note (before / after on the corpus, the raw-mode bars, the gate calibration,
  the cost, the owned-quantities table filled, the recorded limits with owners); open-items; release notes owed: OSC
  widths now from the green sites (a width change in its own right), every Analysis number re-measured once (legacy rows
  recompute on the next analysis), stored thresholds to review (Analysis rejection defaults, frame-set `max_fwhm_px` /
  `max_eccentricity`, collab project rules on `fwhm_arcsec`, `eccentricity`, `median_snr`, `snr_weight`, `frame_snr`),
  `detectionSigma` ignored for the new Detection Threshold, the removed settings, the "dB" labels, the round-star ecc
  floor 0.27 → 0.12 at F 2.6 (an F 2.6 figure, not generalized), the Verdict column (Trailed / Defocused / No stars)
  in place of Trail R², "Reject unusable frames" in place of "Reject trailed", Trail Threshold removed, the gate refusing
  a Trailed frame before solving and C7's outcome for its two thresholds.
- [ ] **Step 4: final whole-branch review** on `fable` against this plan and the spec, explicitly hunting plan-verbatim
  seams (memory `feedback_plan_verbatim_seam_scrutiny.md`): the NaN paths, the trailed path, the config decode, the
  gate's version check, the CFA offset. Fixes go to a fresh agent; the controller re-gates.
- [ ] **Step 5:** merge `plan-c` into local `main`, re-run the core suite on the merged tree, record the line; push only
  on the owner's word.

**Acceptance:** every bar above met or reported with its owner; the review clean; merged locally.

---

## Owned quantities at close

| quantity | owner | consumers | bar | after value |
| ---- | ---- | ---- | ---- | ---- |
| raw input domain (clip, full scale, gain, signedness) | C2 (PC-2, PC-3) | the saturation test, the SNR shot term | C2 tests, the census | C10 |
| the CFA plane's sky and noise | C3 (PC-4) | threshold, saturation, SNR, Analysis `background` / `noise` | C3 tests; raw-mode bars | C10 |
| the CFA seeds and their field width | C3 | the green-site fit | completeness ≥ 99 % at N ≥ seeds | C10 |
| the frame statistic (FWHM, ecc, SNR, HFR) | Plan A (module), read only | the Analysis columns | Plan A's held bars; C10 identity | C10 |
| trail R² | C4 (PC-6) | the gate pair if C7 keeps it; the stored field | C4 tests; C7 | C10 |
| the frame verdict (trailed / defocused / no stars) | Plan A (module, F-10) for trailed / defocused, read only; C6 maps and stores it (PC-13) | the input gate, the Verdict column, Reject unusable frames, `possibly_trailed` | Plan A's T-rows; C7: no solvable frame carries Trailed | C10 |
| the gate's ecc / R² pair | C7 (PC-12) | plate solving | C7: keep only with a separating pair | C10 |
| `c` on a CFA plane | PC-4 (= 1) | Analysis star flux on OSC | recorded limit | — |

## Execution notes

- **Models:** rust-engineer on `sonnet` for C1–C6; frontend-dev for C8; lab jobs `sonnet` (judgment) and `haiku`
  (C5's proof); task reviewers `opus`; the final review `fable`; a failure or a doubted verdict → `fable`.
- **Spend, rough:** C0 one lab agent; C1, C2, C4 one implementer + one review each; C3 and C5 the largest (an
  implementer, a review, a lab or proof agent each); C6–C8 one implementer + one review each; C9 one lab agent + the app
  session; C10 one runner lab agent, the suites, a docs agent and the fable review. About 25 agent phases.
- **Controller:** reads only the reports, re-runs every gate itself, restarts at task boundaries or near 400k context,
  keeps the ledger's "RESUME HERE" current.

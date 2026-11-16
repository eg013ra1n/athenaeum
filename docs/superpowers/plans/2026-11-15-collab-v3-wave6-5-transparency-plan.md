# Collab v3 sub-wave 6.5 — transparency from the zero point — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task, run under the user skill `lean-execution` (it overrides SDD where they conflict). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every analysed, plate-solved light gets a zero point against Gaia G and, from it, a transparency loss against its own equipment's clear sky; the loss shows in the frame tables and is a gate metric a project coordinator can set.

**Architecture:** One pure function matches Analysis' stored stars to the installed catalogue through the stored WCS and returns the frame's zero point. A second module turns zero points into a loss per frame against a per-rig reference, computed on read. The gate, the manifest and three read models carry the loss; nothing in the stacking pipeline reads it. Phase A proves the number on real frames before Phase B builds any consumer.

**Tech Stack:** Rust (`athenaeum-core`, `solvemyastro` catalogue + WCS, rusqlite, ts-rs), React 19 + TS + vitest, the hub (Axum + sqlx) and its portal (React + vitest) in `athenaeum-hub`.

**Spec:** `docs/superpowers/specs/2026-11-15-collab-v3-wave6-5-zero-point-design.md` (rulings Z1–Z4, calls C1–C8, bars §6). Executors read both.

**Ledger:** `.superpowers/sdd/2026-11-15-collab-v3-wave6-5/progress.md`.

## What exists (verified 2026-11-15 on `main` a2c924cc)

- `FrameAnalysis` (`models.rs:1067`, **snake_case** JSON, no `rename_all`) and `StarMetric` (`:1155`; `flux` = model flux × the plane's aperture correction, positions 0-based in file row order). Stored by `db::analysis::upsert_frame_analysis` (`db/analysis.rs:5`, `INSERT OR REPLACE` of 25 columns), read through `row_to_analysis` (`:254`, by index) with the column list hand-copied into three SELECTs (`:49`, `:77`, `:107`). Column migrations: the loop at `db/schema.rs:2087-2104`.
- Production writers of an analysis: `api::analysis::analyze_frame_set` (`api/analysis.rs:153`, save at `:409`), `get_frame_star_metrics` (`:499`, save at `:582`), personal-sync ingest (`sync/ingest.rs:945`, the whole struct as JSON).
- Solves: `plate_solve::storage::insert_plate_solve` (`storage.rs:115`), reached only through `plate_solve::service::store_result` (`service.rs:419`); read by `get_plate_solve` (`:172`); `PlateSolveRecord::to_solution() -> Option<WcsSolution>` (`:63`).
- WCS: `solvemyastro::WcsSolution::{pixel_to_sky, sky_to_pixel}` (`solvemyastro/src/wcs.rs:79`, `:91`), 0-based, SIP both ways, NaN on a singular CD.
- Catalogue: `plate_solve::layers::open_catalog_layers(app_data_dir)` (`layers.rs:82`); `solvemyastro::LayeredCones::{new, cone}` (`cache.rs:533`, `:545`) returns `CatalogStar { ra, dec, mag }` already moved to `epoch`; `plate_solve::hints::observation_epoch(&Frame)` (`hints.rs:247`).
- Gate: `collab/gate.rs` — `METRIC_REGISTRY` (`:80`), `GateFrameInput` (`:91`), `RuleVerdict` (`:122`, no reason field), `evaluate_frame` (`:168`, metric match `:253-264`, label match `:269-274`); inputs built by `api::collab::frame_gate_inputs` (`api/collab.rs:480`) for `project_gate` (`:957`).
- Manifest meta: `collab::frame_meta::build_frame_meta` (`frame_meta.rs:84`, analysis keys `:127-140`), parse-back `MetaWire` / `ProjectFrameMeta` (`:184-270`), the 8192-byte limit test (`:545`).
- Read models: `OwnFrameRow` (`api/collab.rs:1748`), `ProjectFrameView` (`api/collab_exchange.rs:772`, filled in `from_local_row` `:869`), frame-set light rows from `get_frame_set_detail` (`api/frame_sets.rs:1195`), `PlanProjectFrame` (stacking plan).
- Frontend: project columns are `COLUMNS: Record<string, ColumnDef<FrameVM>>` (`src/components/collab/project/frames.tsx:352`, lists in `TABLES` `:756`, mappers `fromOwn` / `fromLibrary` / `fromModeration`); `FramePanel.tsx:194-213` (Metrics, then Gate rows); stacking columns in `FramesTableRows.tsx:13-49` with cells in a `switch` (`:171-247`), rows from `frameRows.ts` (`buildFrameRows` `:203`, `buildProjectFrameRows` `:294`).
- Hub: `athenaeum-hub/src/collab_rules.rs` (`METRICS: [MetricSpec; 7]`), `tests/rules_registry.rs`, `portal/src/metrics.ts` + `metrics.test.ts`.
- Dev catalogue (`…/com.vsharifov.athenaeum.dev/athenaeum.db`): 1003 lights with analysis and solve, **every analysis at `measure_version` 7 or NULL** (current: 8); catalogue tier `tier_500` only; project run 9 (LDN 1272, done 2026-11-06) holds `commonTflux` for 182 frames in `stacking_run_frames.metrics_json`.

## Decisions taken in this plan

| # | Decision | Why |
| ---- | ---- | ---- |
| P1 | **The measurement function is written before the probe and is the probe's subject.** No storage, trigger or consumer exists until Task 3 says GO. | Spec §6: the probe fixes the constants before any consumer is built; the probe must run the code that ships. |
| P2 | **The probe re-analyses frames in memory from their files and never writes a catalogue.** | Every stored analysis is stale (C4); the dev catalogue is the owner's working data. |
| P3 | **Existing catalogues are filled by a startup backfill**, a fourth trigger beside the spec's three: frames with a current analysis, a solve and no current `zp_version`, in chunks, once per launch. | Users' frames analysed by 0.7.1 would otherwise show an empty column until re-analysed. A read path must not compute them: a 1000-frame set would stall its table. |
| P4 | **Reference: the frame's own target first, the rig across targets as a fallback.** A rig × frame-set reference is used when it exists; otherwise the rig-wide one, and only if the probe's cross-target bar held (`RIG_WIDE_FALLBACK`). | Sharper than the spec's either/or: the field's own colour and altitude cancel where the data allows it. Spec §5.2 is amended in Task 10. |
| P5 | **A refusal is stored**: `zero_point` NULL with the current `zp_version`. | Otherwise the backfill and the lazy trigger retry a hopeless frame on every launch. |
| P6 | **A rig needs a camera and a focal length.** A frame without `instrume` or `focallen` has no rig and no loss; frames received through a project never feed a reference. | Two unknown cameras merged into one rig would compare different unit systems. |
| P7 | **The clear session of a probe set is picked before any zero point is seen**: the night (≥ 20 frames) with the highest median `stars_detected`. | A bar judged on a session chosen by its own result proves nothing. |
| P8 | **The loss cell is neutral text**: no colour by value. | The gate, not the table, says what is too much. |

## Reuse ledger

| Need | Reused from | Not written again |
| ---- | ---- | ---- |
| Sky ↔ pixel with SIP | `PlateSolveRecord::to_solution`, `WcsSolution::sky_to_pixel` | any projection math |
| Catalogue cone with proper motion | `open_catalog_layers`, `LayeredCones::cone`, `observation_epoch` | catalogue reading |
| Star fluxes | `star_metrics` rows / `analysis::analyzer` | any star measurement |
| Column migration | the `schema.rs:2087` loop | a migration helper |
| Read-only probe wiring | `stacking/solve.rs:966` (`real_data_solve_probe`: env paths, `READ_ONLY` open, `open_catalog_layers`) | a new harness |

## Global Constraints

- Real logic in `athenaeum-core`; the Tauri and Axum layers stay thin and in sync. **No new command and no new route**: existing commands gain fields; the command count stays as it is. The backfill is a startup call in both hosts.
- `FrameAnalysis` keys are snake_case (the struct has no `rename_all`); every other new wire field is camelCase. After a Rust model change: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract`, commit the regenerated `src/types/*.ts`.
- `anyhow::Result` inside core; never swallow an error: a trigger that fails logs at `warn` and the analysis or solve it follows still succeeds.
- `tracing` only; message = a short stable phrase, data in snake_case fields; the new fields (`zero_point`, `zp_stars`, `zp_sigma`, `zp_matched`, `zp_reason`) are added to the dictionary in `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` as a "Dictionary extension (6.5)" paragraph in the task that first logs them.
- Never name another code base anywhere in the repo.
- Tests pass on Windows, Linux and macOS; test and fixture names use letters, digits, `_` and `-` only.
- The module that calls `solvemyastro` sits behind the same feature gate as `plate_solve`; `cargo check -p athenaeum-core --no-default-features` stays green.
- Design tokens only; primitives from `src/components/ui/`; backend only through the `api` object.
- Nothing under `crates/athenaeum-core/src/stacking/` changes except the display field of Task 7 and its test. No stage hash, group key or weight changes.
- Disk: the probe's files live in `~/.athenaeum-acc/2026-11-15-zp-probe/` with a `REGISTRY.tsv` row (class `job`); agents build into `target-agent`; one implementer at a time on this machine.
- Commits: author is the owner, with the harness trailers; work on branch `collab-v3-wave6-5`, merged to local `main` at the close, pushed on the owner's word. The hub work is its own branch in `athenaeum-hub`; no deploy without the owner's yes.
- Each agent: one job, a tool-call budget of 150, a text report of at most 80 lines.

## Review Focus

Conditions the spec implies and that bite a real user; each has its test in the task named.

1. **A frame is solved again after its zero point exists** (a better solve, a different scale): the old value must not survive. → Task 4, `a_new_solve_recomputes_the_zero_point`.
2. **No star catalogue installed** (a fresh install, the Docker build): analysis and solving succeed, no error surfaces, the cell says why. → Task 4, `no_catalogue_leaves_the_analysis_saved_and_logs_once`; Task 5, the `NotMeasured` sentence.
3. **Two cameras with empty headers**: they must not become one rig. → Task 5, `a_frame_without_a_camera_has_no_rig`.
4. **Another member's frames in the local catalogue** with the same camera model: they must not move my clear-sky reference. → Task 5, `received_project_frames_do_not_feed_a_reference`.
5. **`EXPTIME` missing or zero, a star with zero or negative flux**: a refusal, never `-inf`, NaN or a panic in a stored column. → Task 1, `zero_exposure_refuses` and `a_non_positive_flux_is_skipped`; Task 4, `a_missing_exposure_stores_a_refusal`.

---

## Phase A — truth check

### Task 1: The zero-point measurement (pure)

Agent: rust-engineer, sonnet.

**Files:**
- Create: `crates/athenaeum-core/src/analysis/zero_point.rs`
- Modify: `crates/athenaeum-core/src/analysis/mod.rs` (declare the module, feature-gated as `plate_solve` is)

**Interfaces:**
- Consumes: `models::StarMetric`, `solvemyastro::{WcsSolution, CatalogStar, StarCache, LayeredCones}`.
- Produces:
  - `pub const ZP_VERSION: u32 = 1;`
  - `pub struct ZpParams { pub match_radius_px: f64, pub central_fraction: f64, pub min_stars: usize, pub max_std_error_mag: f64, pub clip_sigma: f64, pub bright_drop_fraction: f64, pub min_snr: f64, pub blend_radius_fwhm: f64 }` with `Default` = `2.0, 0.6, 15, 0.05, 2.5, 0.10, 20.0, 3.0`.
  - `pub struct ZpInput<'a> { pub stars: &'a [StarMetric], pub wcs: &'a WcsSolution, pub catalog: &'a [CatalogStar], pub exptime_s: Option<f64>, pub width: usize, pub height: usize, pub median_fwhm_px: f64 }`
  - `pub struct ZeroPoint { pub zero_point: f64, pub stars: u32, pub sigma: f64 }`
  - `pub enum ZpRefusal { NoExposure, TooFewStars { matched: u32, kept: u32 }, TooScattered { std_error: f64, kept: u32 } }` with `pub fn code(&self) -> &'static str` (`no_exposure` / `too_few_stars` / `too_scattered`).
  - `pub struct ZpStar { pub g_mag: f32, pub zp: f64, pub radius_fraction: f64, pub match_px: f64, pub kept: bool }`
  - `pub struct ZpMeasurement { pub result: Result<ZeroPoint, ZpRefusal>, pub matched: u32, pub median_match_px: Option<f64>, pub per_star: Vec<ZpStar> }`
  - `pub fn measure_zero_point(input: &ZpInput, params: &ZpParams) -> ZpMeasurement`
  - `pub fn catalog_for_frame(layers: &[&StarCache], wcs: &WcsSolution, width: usize, height: usize, central_fraction: f64, epoch: f64) -> anyhow::Result<Vec<CatalogStar>>` — one cone at the frame centre, radius = the central region's radius plus a 10 % margin, every layer, no magnitude cut.

The algorithm, in the order the function applies it (spec §5.1):
1. `exptime_s` absent or ≤ 0 → `NoExposure`.
2. Project every catalogue star to pixels; drop NaN and anything outside the central region (distance from the frame centre ≤ `central_fraction` × half-diagonal).
3. Analysis stars inside the same region with `flux > 0` and `snr ≥ min_snr`; drop the brightest `bright_drop_fraction` of them by flux, the count rounded down (the saturation guard).
4. Match each star to its nearest catalogue star within `match_radius_px`; one catalogue star serves one analysis star (the nearer wins); a match is dropped when a second catalogue star lies within `blend_radius_fwhm × median_fwhm_px` of the star.
5. `zp_i = G_i + 2.5·log10(flux_i / exptime_s)`; iterate median and `1.4826·MAD`, dropping `|zp_i − median| > clip_sigma × sigma`, until nothing drops (at most 10 rounds).
6. Fewer than `min_stars` kept → `TooFewStars`; `sigma / √kept > max_std_error_mag` → `TooScattered`; else `ZeroPoint`.

- [ ] **Step 1: Write the failing tests** in the module's `#[cfg(test)]`, on a synthetic field built by a helper `field(n, zp, scatter_mag, seed)` (an identity-scale TAN WCS, a 4000×3000 frame, stars on a grid, catalogue magnitudes 10–16, fluxes from `exptime · 10^(0.4·(zp − G))`):
  - `a_synthetic_field_recovers_its_zero_point`: 200 stars at 21.30, no scatter → `|zero_point − 21.30| < 1e-9`, `sigma < 1e-9`, `stars ≥ 15`.
  - `saturated_bright_stars_do_not_move_the_median`: the brightest 5 % carry half their flux → within 0.01 of 21.30.
  - `a_star_between_two_catalogue_stars_is_not_used`: one star with two catalogue stars 4 px apart → `per_star` has no kept entry for it.
  - `stars_outside_the_central_region_are_not_used`: every `per_star.radius_fraction ≤ 0.6`.
  - `a_match_is_one_to_one`: two analysis stars 1 px either side of one catalogue star → one match.
  - `fewer_than_the_minimum_refuses_with_counts`: 9 usable stars → `TooFewStars { matched: 9, kept: 9 }`.
  - `a_wide_scatter_of_few_stars_refuses`: 16 stars, scatter 1.0 mag → `TooScattered`.
  - `a_wide_scatter_of_many_stars_is_valid`: 400 stars, scatter 0.30 mag → `Ok`, within 0.05 of truth.
  - `zero_exposure_refuses`: `Some(0.0)` and `None` → `NoExposure`.
  - `a_non_positive_flux_is_skipped`: fluxes 0 and −5 among good stars → result finite, those stars absent from `per_star`.
  - `a_singular_wcs_refuses_without_panic`: a zero CD matrix → `TooFewStars { matched: 0, kept: 0 }`.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --target-dir target-agent analysis::zero_point` — expected: compile error, the module does not exist.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run** the same command — expected: 11 passed. Then `cargo check -p athenaeum-core --no-default-features --target-dir target-agent` — expected: 0 errors.
- [ ] **Step 5: Commit** `feat(analysis): a frame's zero point from its fitted stars and the catalogue`.

### Task 2: The probe example

Agent: rust-engineer, sonnet.

**Files:**
- Create: `crates/athenaeum-core/examples/zp_probe.rs`

**Interfaces:**
- Consumes: Task 1's `measure_zero_point`, `catalog_for_frame`, `ZpParams`; `analysis::analyzer` (the same entry and config `analyze_frame_set` uses, config via `analysis::config::load_config`); `get_plate_solve`, `to_solution`, `open_catalog_layers`, `observation_epoch`.
- Produces a CLI with four subcommands; every `ZpParams` field is overridable by a `--<field>` flag, so Task 3 tunes without a rebuild:
  - `measure --db <path> --app-data <dir> --instrume <s> --filter <s|-> --object <s> --out <tsv>` — for each matching light with a stored solve and a file on disk (zip-resident rows are skipped and counted): read, analyse in memory, measure. One row per frame: `frame_id, object, filter, night, date_obs, exptime, stars_detected, median_fwhm, matched, kept, zero_point, sigma, std_error, median_match_px, refusal`. The catalogue is opened READ_ONLY; nothing is written but the TSV.
  - `stars --db … --app-data … --frame <id> --out <tsv>` — that frame's `per_star` rows.
  - `common --db <path> --run <id> --out <tsv>` — per frame of the run: the local `frame_id`, the plane count and each plane's `commonTflux` from `stacking_run_frames.metrics_json` (a project run's rows resolve through `project_frame` to the local frame).
  - `verdict --class <broadband|narrowband|osc> [--common <tsv>] <tsv>…` — prints at most 30 lines ending `ZP PROBE <class>: PASS | FAIL | UNJUDGED(<bars>)`, one line per bar with its value and limit:

| Bar | Computed as | Limit |
| ---- | ---- | ---- |
| coverage | frames with a zero point ÷ frames measured | ≥ 0.90 |
| geometry | median of `median_match_px` | ≤ 1.0 px |
| stability | in each set's clear night (P7): median of `|Δzero_point|` over consecutive frames less than 15 min apart | ≤ 0.03 mag |
| seeing | same pairs, those with `|ΔFWHM| ≥ 0.2 px`: least-squares slope of `Δzero_point` on `ΔFWHM`; fewer than 20 such pairs → unjudged | `|slope|` ≤ 0.03 mag/px |
| cross-target | sets with ≥ 30 valid frames: spread (max − min) of their 90th-percentile zero points; fewer than 3 sets → unjudged | ≤ 0.10 mag (decides P4, never fails the class) |
| mechanism (with `--common`) | per frame, `zero_point − 2.5·log10(commonTflux / exptime)` on the green plane; RMS about its median, inside one stacking group (the two sides are in different units, so only the spread counts) | ≤ 0.05 mag |

- [ ] **Step 1:** Write unit tests inside the example for the verdict arithmetic on hand-made rows: `stability_is_the_median_of_consecutive_differences`, `pairs_further_than_15_minutes_are_not_consecutive`, `the_clear_night_is_the_one_with_most_stars`, `a_slope_needs_twenty_pairs`, `cross_target_needs_three_sets`.
- [ ] **Step 2:** `cargo test -p athenaeum-core --target-dir target-agent --example zp_probe` — expected: fails to compile.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Same command — expected: 5 passed. Smoke: `measure` on one object with `--limit 3` against a copy of the dev catalogue prints 3 rows.
- [ ] **Step 5: Commit** `test(analysis): a probe that measures zero points on real frames and judges the bars`.

### Task 3: The probe run and the verdict (lab)

Agent: opus (a judgment run). The controller prepares the job directory first.

**Controller, before dispatch:**
- [ ] Create `~/.athenaeum-acc/2026-11-15-zp-probe/{catalog,out}`; add the `REGISTRY.tsv` row (class `job`, purpose "6.5 zero-point probe", end = the wave's close).
- [ ] With the dev app closed, copy `athenaeum.db`, `-wal`, `-shm` from the dev app-data directory into `catalog/`. The star catalogue is read in place from the dev app-data directory.
- [ ] Confirm `/Volumes/bigbase` and `/Volumes/bigbase3` are mounted.

**Sets (fixed here, before any zero point is seen):**

| Class | Rig | Sets (`--object`) |
| ---- | ---- | ---- |
| broadband | `QHY268M`, filter `L` | `IC 5146`, `LDN 1042`, `LDN 1031` |
| narrowband | `QHY268M`, filter `H` | `IC 5146`, `LDN 1042` |
| osc | `ZWO ASI2600MC Duo`, filter `-` | `LDN 1272`; mechanism from run 9 |

- [ ] **Step 1:** Run `measure` for the six sets (release build, in the background; about 570 frames) and `common --run 9`. A set whose files are unreachable is reported, not skipped silently.
- [ ] **Step 2:** Run `verdict` for the three classes with default parameters; save each output to `out/verdict-<class>-r0.txt`.
- [ ] **Step 3:** For the frame with the largest loss in each class and for every refused frame of a class under 0.90 coverage: render a stretched JPG and dump `stars`; say in one line each what the frame shows (cloud, trailing, a wrong solve, a saturated field).
- [ ] **Step 4:** If a bar misses, change **parameters only** (at most three rounds, each a full re-run of `verdict` on all three classes with `--<field>` flags): `central_fraction`, `bright_drop_fraction`, `min_snr`, `match_radius_px`, `blend_radius_fwhm`, `min_stars`. The limits of Task 2's table never move.
- [ ] **Step 5:** Report (≤ 80 lines): the final parameters, the three verdict blocks verbatim, the cross-target result, the hazy frame chosen for Task 10 (frame id, file, loss), and what could not be judged and why.

**Controller gate (not the agent's):**
- [ ] Re-run the three `verdict` commands with the reported parameters and compare with the report.
- [ ] **GO** for a class when coverage, geometry, stability and seeing pass; the OSC class also needs the mechanism bar. Write into the ledger: final `ZpParams`, `RIG_WIDE_FALLBACK` (true only if every judged cross-target spread is ≤ 0.10), the classes that pass.
- [ ] **STOP** for a class that fails or is unjudged on a required bar: report the numbers to the owner in plain words and wait. If the seeing bar is the miss, say that the remedy is an aperture sum from Analysis in rustafits (R-OSP-19), a decision of its own. If no class passes, the wave closes with Task 10's announcement step alone.
- [ ] Commit the final defaults into `ZpParams::default()` and their test values: `fix(analysis): zero-point constants from the real-data probe`.

---

## Phase A outcome (2026-11-15) — read before any Phase B task

GO for mono (owner ruling Z5: mono first, then OSC; spec §9). Tasks 1–3 are done (af37cca8, 22114df3, 0409e515,
3ad45f2f, 8aac9419 and the constants commit); the ledger holds every number. What Phase B must take from it — where a
task below says otherwise, this section wins:

- **Constants** are `ZpParams::default()` as committed; no task changes them.
- **`RIG_WIDE_FALLBACK = true`** (cross-target spread 0.025 mag on the one class that could be judged).
- **OSC is not measured (Z5).** Task 4: `NotReady` gains `ColourCamera` for a frame the catalogue knows as OSC or CFA
  (the same colour derivation the stacking plan's group key uses); it is stored like a refusal (NULL value, current
  version and catalogue) so it is not retried. Task 5: `LossReason::ColourCamera`, sentence
  `not measured for colour cameras yet`. The gate does not block such a frame (the no-value path).
- **The catalogue is part of a zero point's currency.** Task 4: a fifth column `zp_catalog TEXT` and field
  `zp_catalog: Option<String>`, written from `plate_solve::layers::catalog_identity`; `set_zero_point` takes it;
  "current" everywhere (lazy trigger, backfill) means `zp_version == ZP_VERSION AND zp_catalog == the installed
  identity`. Task 5: a reference reads only frames whose `zp_catalog` is the installed identity; a frame measured
  against another tier set has the `NotMeasured` reason until it is recomputed.
- **Refusal wording.** Task 5: `NoZeroPoint`'s sentence is
  `too few catalogue stars for a reliable zero point — a deeper star catalogue tier may help`.
- **A solve with a forward SIP and no reverse** (`sip_reverse` absent while `sip_forward` is present) is not measured:
  `NotReady::UnreadableSolve`, logged. `sky_to_pixel` has no inverse for it. Every solve the current solver stores has
  both.
- **No hazy frame exists in the probe sets.** Task 10's real-data gate check uses frame 94501 (IC 5146 L, 2024-10-04,
  airmass 2.47, loss 0.24 mag against its night) with the rule `transparency_loss lte 0.2`: refused; a frame within
  0.05 mag of the reference: passed. A cloudy night is recorded as owed in open-items.
- **Probe lesson** (owner, 2026-11-15): 616 frames were 2–3× more than the bars needed; the weak point was variety.
  Later probes: one or two nights per set, more rigs and fields.

## Phase B — build (after GO)

### Task 4: Storage and triggers

Agent: rust-engineer, sonnet.

**Files:**
- Modify: `crates/athenaeum-core/src/models.rs` (`FrameAnalysis`), `db/schema.rs` (the `frame_analysis` DDL and the add-column loop), `db/analysis.rs` (upsert, the three SELECT lists, `row_to_analysis`), `analysis/zero_point.rs`, `api/analysis.rs`, `plate_solve/service.rs` or its api-layer callers, the two hosts' startup (`crates/athenaeum-tauri/src/lib.rs`, `crates/athenaeum-web/src/main.rs` or where each spawns its background work), `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`
- Regenerate: `src/types/models.ts`

**Interfaces:**
- Consumes: Task 1.
- Produces:
  - `FrameAnalysis` gains `#[serde(default)] pub zero_point: Option<f64>`, `pub zp_stars: Option<i64>`, `pub zp_sigma: Option<f64>`, `pub zp_version: Option<u32>`; columns `zero_point REAL, zp_stars INTEGER, zp_sigma REAL, zp_version INTEGER`.
  - `db::analysis::set_zero_point(conn: &Connection, frame_id: i64, value: Option<&ZeroPoint>) -> Result<()>` — an UPDATE that writes the three values (or NULLs) and `zp_version = ZP_VERSION` (P5).
  - `pub enum NotReady { NoAnalysis, StaleAnalysis, NoSolve, UnreadableSolve, NoCatalogue }`
  - `pub enum ZpStatus { Measured(ZeroPoint), Refused(ZpRefusal), NotReady(NotReady) }`
  - `pub fn refresh_zero_point_with<F>(conn: &Connection, frame_id: i64, cone: F) -> anyhow::Result<ZpStatus> where F: FnOnce(&WcsSolution, usize, usize, f64) -> anyhow::Result<Vec<CatalogStar>>` — reads the analysis (must carry `measure_version == MEASURE_VERSION`), star metrics, solve and `exptime`; measures; stores through `set_zero_point`; logs one `info` line (`zero point measured` / `zero point refused`) with the dictionary fields. `NotReady` stores nothing.
  - `pub fn refresh_zero_point(conn: &Connection, layers: &[Arc<StarCache>], frame_id: i64) -> anyhow::Result<ZpStatus>` — the production wrapper over `catalog_for_frame`.
  - `pub fn zero_point_layers(app_data_dir: &Path) -> Option<Vec<Arc<StarCache>>>` — `NotInstalled` → `None` at `debug`; an open error → `warn` and `None`.
  - `pub fn ensure_zero_points(conn: &Connection, layers: &[Arc<StarCache>], frame_ids: &[i64]) -> anyhow::Result<usize>` — refreshes only frames with a current analysis, a solve and `zp_version IS NOT ZP_VERSION`; returns how many it measured or refused.
  - `api::analysis::backfill_zero_points(ctx: &ServiceContext) -> Result<usize, ApiError>` — every such frame in the catalogue, 200 at a time, one summary `info` line; called once after startup by both hosts, off the UI path.

Triggers: (a) after each save in `analyze_frame_set` and `get_frame_star_metrics`; (b) after `store_result` succeeds, in the lowest function that holds both the connection and the app-data directory — one helper, called from every production path that reaches `store_result` (find them with a grep; do not copy the helper). A solve trigger always recomputes, whatever `zp_version` says. The catalogue root is resolved the way `resolve_layer_caches` does (`commands/plate_solve.rs:19`, `routes/plate_solve.rs:53`); if `ServiceContext` has no app-data accessor, add one in core rather than threading a path through commands.

- [ ] **Step 1: Write the failing tests** (`db/analysis.rs` and `analysis/zero_point.rs`; the `cone` closure returns a synthetic catalogue, so no star cache is built):
  - `migration_adds_the_zero_point_columns_and_keeps_rows` (on `old_catalog()`): four columns exist, the old row reads back with `None` in all four.
  - `frame_analysis_round_trips_the_zero_point`: upsert with `Some(21.3), Some(87), Some(0.04), Some(1)` → equal on read through all three getters.
  - `an_old_frame_analysis_json_decodes_without_a_zero_point`: JSON without the keys → four `None`.
  - `refresh_measures_and_stores`: a seeded frame (analysis at the current version, 200 star metrics, a solve, `exptime` 180) → `Measured`, the row holds the value and `zp_version = 1`.
  - `a_stale_analysis_is_not_measured`: `measure_version` 7 → `NotReady(StaleAnalysis)`, the four columns stay NULL.
  - `a_missing_exposure_stores_a_refusal`: `exptime` NULL → `Refused(NoExposure)`, `zero_point` NULL, `zp_version = 1`.
  - `a_new_analysis_clears_the_zero_point_until_the_trigger_runs`: upsert again → `zero_point` NULL, `zp_version` NULL.
  - `a_new_solve_recomputes_the_zero_point`: store a value, replace the solve with one shifted by 50 px, run the solve trigger → the stored value differs or is refused, and `zp_version = 1`.
  - `ensure_skips_frames_that_are_current`: two frames, one current → returns 1.
  - `no_catalogue_leaves_the_analysis_saved_and_logs_once`: the analysis path with `zero_point_layers` → `None` returns `Ok`, the analysis row exists, ZP columns NULL.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --target-dir target-agent zero_point` and `… db::analysis` — expected: failures on the missing items.
- [ ] **Step 3: Implement.** Add the dictionary paragraph to the logging spec. If a pinned personal-sync record includes the analysis JSON, the pin gains the four keys and loses none.
- [ ] **Step 4: Run** the two commands (all pass), `TS_RS_WRITE=1 cargo test -p athenaeum-core --target-dir target-agent --test ts_contract`, `cargo check --workspace --target-dir target-agent` (0 errors), `cargo check -p athenaeum-core --no-default-features --target-dir target-agent`.
- [ ] **Step 5: Commit** `feat(analysis): the zero point is stored with the analysis and kept current`.

### Task 5: Transparency loss

Agent: rust-engineer, sonnet.

**Files:**
- Create: `crates/athenaeum-core/src/analysis/transparency.rs`
- Modify: `crates/athenaeum-core/src/analysis/mod.rs`

**Interfaces:**
- Consumes: Task 4's columns.
- Produces:
  - `pub const REFERENCE_PERCENTILE: f64 = 0.90; pub const REFERENCE_MIN_FRAMES: u32 = 30; pub const REFERENCE_MIN_NIGHTS: u32 = 2; pub const RIG_WIDE_FALLBACK: bool = <Task 3's ledger value>;`
  - `pub enum LossReason { NotAnalysed, StaleAnalysis, NotSolved, NotMeasured, NoZeroPoint, NoRig, NoReference { have_frames: u32, have_nights: u32 } }` with `pub fn sentence(&self) -> String`:

| Reason | Sentence |
| ---- | ---- |
| `NotAnalysed` | `not analysed yet` |
| `StaleAnalysis` | `analysed by an older version — analyse the frame again` |
| `NotSolved` | `not plate-solved` |
| `NotMeasured` | `zero point not measured yet — the star catalogue may not be installed` |
| `NoZeroPoint` | `too few catalogue stars for a zero point` |
| `NoRig` | `camera or focal length unknown — nothing to compare with` |
| `NoReference`, frames short | `no clear-sky reference yet, {have_frames} of 30 frames` |
| `NoReference`, nights short | `no clear-sky reference yet, {have_nights} of 2 nights` |

  - `#[derive(Serialize, ts_rs::TS)] #[serde(rename_all = "camelCase")] pub struct Transparency { pub zero_point: Option<f64>, pub zp_stars: Option<i64>, pub zp_sigma: Option<f64>, pub loss_mag: Option<f64>, pub reason: Option<String> }` (registered in `ts_export.rs`); `reason` is the sentence, present exactly when `loss_mag` is `None`.
  - `pub fn transparency_for_frames(conn: &Connection, frame_ids: &[i64]) -> anyhow::Result<HashMap<i64, Transparency>>` — chunked queries, never one per frame.

Rules: rig = `instrume`, `telescop`, `focallen` rounded to whole mm, `gain`, `xbinning`, `filter` (NULL `telescop` / `gain` / `filter` are their own value; NULL `instrume` or `focallen` → `NoRig`, P6). A reference = the nearest-rank 90th percentile of the valid zero points of the rig's own lights (never frames received through a project), needing 30 frames over 2 imaging nights (the catalogue's night derivation, not a date substring). Scope per P4: the frame's frame set first, then the whole rig when `RIG_WIDE_FALLBACK`. `loss_mag = max(0, reference − zero_point)`.

- [ ] **Step 1: Write the failing tests** (a `seed(rig, frame_set, night, zp)` helper over an in-memory catalogue):
  - `the_reference_is_the_nearest_rank_ninetieth_percentile`: 30 frames with zero points 20.00 … 20.29 over 2 nights → the frame at 20.00 has loss 0.26.
  - `a_frame_brighter_than_the_reference_has_no_loss`: 20.29 → `Some(0.0)`.
  - `twenty_nine_frames_give_no_reference`: reason `no clear-sky reference yet, 29 of 30 frames`.
  - `one_night_gives_no_reference`: 40 frames, one night → `no clear-sky reference yet, 1 of 2 nights`.
  - `another_rig_does_not_feed_the_reference`: same camera, gain 0 against gain 56.
  - `a_frame_without_a_camera_has_no_rig`: two frames with NULL `instrume` → both `NoRig`.
  - `received_project_frames_do_not_feed_a_reference`.
  - `the_frame_set_reference_wins_over_the_rig`: a set whose own percentile is 20.50 inside a rig at 21.00 → loss measured from 20.50.
  - `the_rig_stands_in_when_the_set_is_short` (asserts per `RIG_WIDE_FALLBACK`).
  - `each_state_has_its_reason`: unanalysed, stale, unsolved, unmeasured, refused → the five sentences.
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --target-dir target-agent analysis::transparency` — expected: compile error.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — expected: 10 passed; `TS_RS_WRITE=1 … --test ts_contract`.
- [ ] **Step 5: Commit** `feat(analysis): transparency loss against the equipment's clear sky`.

### Task 6: Gate and manifest (app)

Agent: rust-engineer, sonnet.

**Files:**
- Modify: `crates/athenaeum-core/src/collab/gate.rs`, `collab/frame_meta.rs`, `api/collab.rs` (`frame_gate_inputs`, `project_gate`, `OwnFrameRow` and its fill at `:2032`), `api/collab_exchange.rs` (`ProjectFrameView`, `from_local_row`)
- Regenerate: `src/types/models.ts`

**Interfaces:**
- Consumes: `transparency_for_frames`, `Transparency`, `ensure_zero_points`, `zero_point_layers`.
- Produces:
  - `METRIC_REGISTRY` gains `("transparency_loss", &["lte"])`.
  - `GateFrameInput` gains `pub transparency: Option<Transparency>`.
  - `RuleVerdict` gains `pub reason: Option<String>` (`None` for every existing metric).
  - `evaluate_frame`: metric `transparency_loss` reads `transparency.loss_mag`; label `Transparency loss`, unit `mag`, value printed with two decimals; with no value the rule's row has `value: None, pass: None, reason: Some(sentence)` and the frame's `publishable` and `failures` are untouched.
  - Manifest key `transparencyLoss` (number, mag), written by `build_frame_meta` when a loss exists; `MetaWire` and `ProjectFrameMeta` gain `transparency_loss: Option<f64>`.
  - `OwnFrameRow` gains `pub transparency: Transparency`; `ProjectFrameView` gains `pub transparency_loss: Option<f64>`.
  - Frozen value (C5): for a frame already published, the gate input and `OwnFrameRow.transparency.loss_mag` take the manifest's `transparencyLoss`; a published frame whose manifest has none shows no value with the reason `published without a transparency value`.
  - `project_gate` calls `ensure_zero_points` for its frames before it builds inputs (trigger c); no catalogue → it proceeds without.

- [ ] **Step 1: Write the failing tests:**
  - gate: `a_loss_over_the_limit_fails_the_rule` (loss 0.77, `lte 0.5` → `pass: Some(false)`, `value: Some("0.77 mag")`, not publishable, the failure names `Transparency loss`); `a_loss_under_the_limit_passes` (0.05); `no_loss_value_gives_a_reason_and_no_block` (reason `no clear-sky reference yet, 12 of 30 frames`, `publishable` true); `registry_matches_the_evaluator` extended to eight keys with a transparency value in its input; `an_unknown_metric_gets_no_verdict` unchanged.
  - meta: `frame_meta_carries_the_transparency_loss`, `frame_meta_omits_it_without_a_loss`, `the_manifest_parser_reads_the_transparency_loss`, and `full_sip_order_5_meta_stays_under_the_hub_byte_limit` with the key present.
  - frozen: `a_published_frame_is_judged_on_its_published_loss` (manifest 0.20, live 0.90, rule `lte 0.5` → passes, row value `0.20 mag`).
- [ ] **Step 2: Run** `cargo test -p athenaeum-core --target-dir target-agent collab::gate collab::frame_meta` and the `api::collab` test added — expected: failures.
- [ ] **Step 3: Implement.** If a hub-contract fixture under `crates/athenaeum-core/tests/fixtures/hub_contract/` pins a `meta` object built by `build_frame_meta`, it gains the key; no fixture loses one.
- [ ] **Step 4: Run** the same tests (pass), the ts contract regeneration, `cargo check --workspace --target-dir target-agent`.
- [ ] **Step 5: Commit** `feat(collab): transparency loss is a gate metric and travels in the manifest`.

### Task 7: Read models for the stacking Frames block

Agent: rust-engineer, sonnet.

**Files:**
- Modify: `crates/athenaeum-core/src/api/frame_sets.rs` (the light-frame row `get_frame_set_detail` returns, the one `FrameSetDetail.tsx:384` reads as `lightFrames`), `stacking/plan.rs` (`PlanProjectFrame` and where it is filled from `ProjectFrameMeta`)
- Regenerate: `src/types/*.ts`

**Interfaces:**
- Produces: that light-frame row gains `pub transparency: Transparency` (one `transparency_for_frames` call for the set); `PlanProjectFrame` gains `pub transparency_loss: Option<f64>` from the manifest.

- [ ] **Step 1: Tests:** `frame_set_detail_carries_each_lights_transparency` (a seeded set with a reference → the hazy light's `lossMag` 0.26, a light without analysis carries the `not analysed yet` reason); `a_project_plan_frame_carries_the_published_loss`; `the_plan_hashes_do_not_see_the_transparency` (the plan of a seeded project is built twice, with and without the manifest key: every stage hash and group key is equal).
- [ ] **Step 2–4:** Run (fail), implement, run (pass), regenerate types, `cargo check --workspace`.
- [ ] **Step 5: Commit** `feat(stacking): the plan and the frame set carry each frame's transparency`.

### Task 8: Frontend

Agent: frontend-dev.

**Files:**
- Create: `src/components/collab/project/transparency.ts` (+ `transparency.test.ts`)
- Modify: `src/components/collab/project/frames.tsx` (+ test), `FramePanel.tsx` (+ test), `src/components/stacking/FramesTableRows.tsx`, `frameRows.ts` (+ their tests), the `scripts/ui-harness` fixtures

**Interfaces:**
- Consumes: `OwnFrameRow.transparency`, `ProjectFrameView.transparencyLoss`, the frame-set light row's `transparency`, `PlanProjectFrame.transparencyLoss`, `RuleVerdict.reason`.
- Produces:
  - `export function formatTransparency(loss: number | null): string` — `null` → `—`; under 0.005 → `0.00 mag`; else U+2212 then two decimals and ` mag` (`−0.77 mag`).
  - `export function formatZeroPoint(t: { zeroPoint: number | null; zpStars: number | null; zpSigma: number | null }): string` — `21.34 · 87 stars · ±0.04`, or `—`.
  - `FrameVM` gains `transparencyLoss: number | null`, `transparencyReason: string | null`, `zeroPoint`, `zpStars`, `zpSigma`; the three mappers fill them (`fromLibrary` and `fromModeration` have the loss only).
  - `COLUMNS.transparency`: label `Transparency`, numeric, `value` = the loss (nulls last through `sortRows`), `cell` = `formatTransparency` with the reason as the cell's `title`; listed after `snr` in every `TABLES` entry that lists `snr`.
  - `FramePanel` Metrics gains `Transparency` and `Zero point`; a Gate row with no value shows `r.reason` where the value would be.
  - Stacking: `ColumnId` gains `'transparency'` in the metrics group for both subjects; `FrameRow` gains `transparencyLoss` and `transparencyReason`, filled by `buildFrameRows` and `buildProjectFrameRows`.

- [ ] **Step 1: Write the failing tests** (vitest):
  - `transparency.test.ts`: `formatTransparency(0.77)` → `−0.77 mag`; `(0.004)` → `0.00 mag`; `(null)` → `—`; `formatZeroPoint({21.342, 87, 0.041})` → `21.34 · 87 stars · ±0.04`.
  - frames: the column renders `−0.77 mag`; an empty cell carries the reason as its title; sorting by it puts `null` last in both directions.
  - FramePanel: a rule row with `value: null, reason: 'no clear-sky reference yet, 12 of 30 frames'` shows the sentence and neither ✓ nor ✕.
  - FramesTableRows / frameRows: a frame-set row and a project row each render the cell.
- [ ] **Step 2:** `npx vitest run src/components/collab/project src/components/stacking` — expected: the new tests fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4:** Same command passes; `npx tsc --noEmit` clean; `npm run ui:harness` renders the project page with the column in the `default` scenario.
- [ ] **Step 5: Commit** `feat(ui): a Transparency column in the frame tables and the frame panel`.

### Task 9: Hub and portal (`athenaeum-hub`)

Agent: rust-engineer, sonnet. Branch `transparency-loss-metric` in `/Volumes/BigMac/Users/astrobureau/Documents/Projects/athenaeum-hub`. No deploy.

**Files:**
- Modify: `src/collab_rules.rs`, `tests/rules_registry.rs`, `portal/src/metrics.ts`, `portal/src/metrics.test.ts`, and every other place a grep for `snr_weight` finds a metric list or a count of seven.

**Interfaces:**
- Produces: `METRICS: [MetricSpec; 8]` with `MetricSpec { key: "transparency_loss", unit: "mag", ops: &["lte"], value: ValueKind::Number, advanced: false }`; the portal's `MetricKey` union and `METRICS` entry: label `Transparency loss`, unit `mag`, ops `['lte']`, value `'number'`, advanced `false`, defaultOp `'lte'`, defaultValue `0.5`, help `How much fainter the frame's stars are than this equipment's clear sky — haze, cloud or a target low in the sky. 0.5 mag is about a third of the light lost.`

- [ ] **Step 1: Tests:** `registry_accepts_every_known_rule_shape` gains `{"metricKey": "transparency_loss", "op": "lte", "value": 0.5}`; `registry_refuses_unknown_key_op_and_value_kind` gains `gte` → `op "gte" is not allowed for transparency_loss` and `-1` → `transparency_loss must be > 0`; the portal's key-count test reads eight, and its ops test allows `lte` alone for the new key.
- [ ] **Step 2–4:** Run the hub's test command for `rules_registry` and `npx vitest run src/metrics.test.ts` in `portal/` (fail, implement, pass); the hub's full suite as its README runs it.
- [ ] **Step 5: Commit** on the branch: `feat(rules): transparency loss is a threshold metric`. Deployment to the test hub, then production, is the owner's step (Task 10 lists it as owed).

### Task 10: Acceptance, references, announcement

Controller, with one sonnet agent for the reference edits.

- [ ] **Gate re-run by the controller:** `cargo test -p athenaeum-core --target-dir target-agent` (the known flakes of the pre-push memory apart), `cargo check --workspace`, `cargo check -p athenaeum-core --no-default-features`, `npx vitest run`, `npx tsc --noEmit`.
- [ ] **No stacking change:** `git diff --stat main -- crates/athenaeum-core/src/stacking` lists `plan.rs` only, and Task 7's hash test passes.
- [ ] **Real data, the web build on a catalogue copy** (`ATHENAEUM_DB_PATH` → the copy in `~/.athenaeum-acc/2026-11-15-zp-probe/catalog/`, never the dev catalogue): analyse `LDN 1042` L again; the stacking Frames block shows the Transparency column filled; Task 3's hazy frame shows its loss within 0.05 mag of the probe's; a frame of a rig without a reference shows the reason. Screenshot for the owner.
- [ ] **The gate on real data:** an `#[ignore]` test `real_data_transparency_gate_probe` (env paths, as `real_data_solve_probe`) evaluates `transparency_loss lte 0.5` on the hazy frame and its two neighbours from the copy: refused, passed, passed.
- [ ] **References:** `docs/catalog/README.md` (the zero point, its triggers, the backfill), `docs/transfers/README.md` (the metric, the manifest key, the frozen value), `docs/stacking/README.md` and the wave 6 spec (the `equivalent` zero-point rung is deferred by Z4, not owed), `CLAUDE.md` only if a rule changed (none expected). The spec's §5.1–5.2 already carry P3, P4 and P6 (amended 2026-11-15); amend it again only if Task 3 moved a constant it names.
- [ ] **Open items** (`docs/superpowers/open-items.md`): reword the `zero_point` "do not re-flag" entry (out by ruling Z3); owed: hub test deploy and production deploy of Task 9, the portal's rule editor checked in a browser, the desktop real-window look at both tables.
- [ ] **Announcement:** propose to the owner, and apply only on a yes, the "Coming in 0.7.2" line of `RELEASE_NOTES.md`: "**See how clear the sky was for every frame.** Athenaeum will compare the brightness of each frame's stars with their Gaia catalog magnitudes and show how much light the frame lost to haze, cloud or a low target. In a collaboration project the coordinator can set one limit that is fair to every telescope."
- [ ] **Close:** whole-branch review, merge `collab-v3-wave6-5` into local `main`, delete `~/.athenaeum-acc/2026-11-15-zp-probe/` and its registry row once the probe's verdict files are quoted in the ledger, cut the wave's memory to its non-derivable facts.

# Stacking M1 — Plan 3: Registration v2 Service — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Register one calibrated light frame onto a reference frame the way
the stacking pipeline needs it — detection on the calibrated planes, a
quad-seeded correspondence search, RANSAC on the configured linear model, a
σ-weighted refit, optional polynomial distortion, per-frame QA and failure
rules, the `registration_results` row that records it, an optional
registered-frame writer, and the dev probe that lets Checkpoint A compare all
of it with the WBPP run on the owner's LDN 1272 data.

**Architecture:** A new `stacking::register` module (gated `render + solver`
with the rest of `stacking`) with four leaves: `mod.rs` (the configuration
with the spec §9.2 names and defaults), `detect.rs` (registration stars on a
luminance plane), `align.rs` (seed → KD-tree → RANSAC → refit → distortion →
QA, pure geometry over star lists), `frame.rs` (the per-frame driver over
`PlaneReader` plus the `RegistrationRecord` conversion), `writer.rs` (the
`r_<stem>.fits` writer with `ATH_REG*` cards). The existing
`registration_results` table gains the nine spec §9.1 columns through the
guarded-`ALTER` pattern; the scanner learns to skip `ATH_REG`/`ATH_STK`
artifacts. Set-level orchestration (grouping, reference choice, fan-out,
events, persistence of a whole set, retiring the old commands) stays in
Plan 5; the old plate-solve-based `registration::service` is untouched.

**Tech Stack:** Rust 2021, `athenaeum-core`; Plan 1's `geometry` (`Linear`,
`PixelMap`, `Distortion`, `KdTree2`, `ransac_fit`, `refit_weighted`) and
`resample` (`warp_rows`, `Interpolation`); the `solvemyastro` submodule's
public quad matcher (`solvemyastro::quad::{group_size_for, build_quads,
match_quads, fit_affine}`) as the seed; `astroimage::ImageAnalyzer` for
detection; `rusqlite`, `serde`/`serde_json`, `ts_rs`; `tempfile` in tests.
No new crate dependencies.

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md`
§3 (registration: detection, matching, models, geometry, interpolation, QA,
registered frames), §9.1 (`registration_results` columns and
`transform_json`), §9.2 (`registration` config names), §9.5 (`registered/`
layout), §13 (Checkpoint A targets); math in
`docs/superpowers/research/2026-09-08-stacking-math-reference.md` §5.2–5.4.

## M1 program index (this is Plan 3 of 5)

| Plan | Scope | Status |
| ---- | ---- | ---- |
| 1 | kernels, warp, window, `Linear`/`PixelMap`/`Polynomial2D`, KD-tree, RANSAC, `PlaneReader`, `FrameSource`, `RegisteredSource` | merged 2afa55ad |
| 2 | `integration/stats.rs`, `stacking/{robust,psf_signal,measure,weights}.rs`, `measure_probe` | merged (see `git log`) |
| **3 (this)** | `stacking/register/{mod,detect,align,frame,writer}.rs`, `registration_results` v2 columns, scanner skip, `register_probe`, **Checkpoint A** | — |
| 4 | Combiner v2 + engine (weights, offsets, masks, rejection maps, channel loop) + WCS writer + master cards → Checkpoint B | consumes `PixelMap` rows from here |
| 5 | Orchestration, tables, commands ×2, `ts_export`, events, the Stacking tab, acceptance run | consumes `RegistrationConfig`, `reference_stars`, `register_frame`, `to_record`, `write_registered_frame` |

**Checkpoint A (this plan's last task, run by the controller on the owner's
data):** register the app's own calibrated LDN 1272 frames
(`~/Pictures/Calibration Test/LDN1272-WBPP/LDN1272-ATH/LDN 1272/camera_*/lights/c_*.fits`,
208 mono + 160 OSC) onto the WBPP reference
`c_2025-10-18_02-02-02__-9.90_180.00s_0073.fits` and compare with the
`.xdrz` sidecars in `~/Pictures/Calibration Test/LDN1272-Output/registered/`
(a 3×3 `AlignmentMatrix`, `AlignmentOrigin 0.5`, no splines) and the log's
per-frame `delta_RMS`. Results go to
`docs/superpowers/research/2026-09-09-checkpoint-a-registration.md`.

## Rulings made while writing this plan

1. **Where the code lives.** New code goes under `stacking::register` (the
   program's module), not into the old `registration` module; only the
   `registration_results` table and `RegistrationRecord` are extended,
   because Plan 5 keeps that table. The plate-solve-based
   `registration::service` and its three commands stay until Plan 5 retires
   them.
2. **Seed.** The subject→reference seed is `solvemyastro::quad`'s matcher
   exactly as the old service calls it (subject plays "image", reference
   plays "catalog", tolerance 0.007, `fit_affine` returns image→catalog),
   wrapped into a `Linear` of kind `Affine`. One pass of KD-tree
   correspondences at `2 × ransacTolerancePx` follows; re-collecting pairs
   through the refined model (the reference's local-correction loop) is an
   M4 refinement.
3. **`model: auto`.** The RANSAC kind is chosen from the correspondence
   count (≥ 30 homography, ≥ 12 affine, else similarity); the refit kind is
   chosen again from the inlier count, so it can only step down. The
   recorded model is the refit kind.
4. **`distortion: auto`.** Polynomial order 3 when the subject's geometry
   differs from the reference's and the refit keeps ≥ 200 inliers; a
   polynomial of order `o` needs ≥ `(o+1)(o+2)` inliers or it is skipped with
   a warning (never a failure). Distortion is centred on the reference
   frame's centre with `scale = max(W, H)/2`.
5. **QA numbers** are computed through the final map (linear or with
   distortion) on the refit's inliers: RMS, σ of the residual distances,
   per-axis peak. Scale, rotation, translation and the flip come from the
   linear part.
6. **σ weights.** `refit_weighted` and `Distortion::fit` get per-pair
   `(σx, σy)` = the quadrature sum of the subject's and the reference's
   fitted centroid σ (`FastStar::sx/sy`, present when the detector's Moffat
   refinement succeeded) and uniform weights when any pair lacks them.
7. **Legacy columns.** `affine_a1..c2` receive the top two rows of the
   linear part even for a homography (the projective row lives in
   `transform_json`); the WCS columns stay NULL — v2 does not plate-solve,
   the master's WCS is Plan 4's job from the reference's own solve.
8. **Registered-frame cards.** Copy-through = optics/acquisition, DATE-OBS,
   session/target only — never WCS (the pixel grid is the reference's now)
   and never Bayer/CFA (the frame is resampled). `IMAGETYP = 'LIGHT'`,
   `ATH_REG = T`, `ATH_REGV = 1`, `ATH_REGR` (reference stem), `ATH_REGM`
   (model string), `ATH_REGK` (kernel), `ATH_REGC` (clamping), `ATH_REGS`
   (RMS), `ATH_REGT` (the `transform_json`, CONTINUE chain, no comment).
9. **Scanner skip** for `ATH_REG` and `ATH_STK` lands here (the first plan
   that writes such artifacts), extending the calibrated-artifact rule at
   both scan sites, not in Plan 4.
10. **Model string** recorded in `registration_results.model`: the linear
    kind's serde name, plus `+polynomial<o>` when distortion was fitted
    (`homography`, `homography+polynomial3`).
11. **Row order for Checkpoint A.** FITS (ours, bottom-up storage) and XISF
    (WBPP, top-down) differ by convention, and the `.xdrz` matrix lives in
    the WBPP image's coordinates with a 0.5 origin. The probe compares
    corner mappings under both row-order hypotheses and reports which one
    agrees, instead of guessing.
12. **`RegistrationRecord` gains `#[derive(Default)]`** so the three existing
    test literals and future callers can use `..Default::default()`; the
    generated `src/types/models.ts` is regenerated with
    `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` and
    committed — the only frontend file this plan touches, and only as a
    generated artifact.
13. **No `detection.sigma`.** Plan 2's final review found the fast
    detector never reads a detection sigma (its adaptive ladder targets
    `maxStars`; `minSnr` is the real sensitivity dial), and Plan 2 removed
    the knob from `MeasureOptions`. The same detector serves registration,
    so `DetectionConfig` carries only `minSnr` and `maxEccentricity`, and
    Task 6 amends spec §9.2 to drop `sigma: 5.0` from the `registration.
    detection` block. A config field that does nothing is the class of
    silent failure the project forbids.

## Carry-forwards from Plan 2's final review (for this plan's author and the next ones)

Recorded here because the Plan 2 ledger is deleted with its workspace.

- **This plan:** none beyond ruling 13; `stacking::measure::ADU_SCALE` is
  reused by `detect.rs`; the registration warns must carry the frame
  identity (they do — every `debug!`/`warn!` in `frame.rs` has `path`).
- **Plan 4 (combiner v2 + engine):** `read_rows_with_scratch` has no
  consumer yet — the banded engine is meant to use it (do not re-invent);
  `winsorize`'s "no survivors" guard keys on `lo.is_finite()` (a genuine
  ±∞ survivor mis-fires); `MRS_LAYER0_GAIN` is named "layer 0" where the
  estimator says layer 1 — rename before citing it; `background_residual`
  has no `data.len() == w·h` precondition.
- **Plan 5 (orchestration, tables, commands, UI):** `stars_detected` on
  `ChannelMeasurement` is the post-`minSnr` seed count — decide the name
  before it reaches `metrics_json` and the Frames table; `measure_plane`'s
  transient memory is ≈ 8× the plane (the ADU copy plus the mesh maps and
  the MRS layers) — a `ComputeQueue` sizing constraint, never measure
  planes concurrently; cancel is checked between planes only — cancel
  between frames and say so in the UI; `select_frames`'s `missing` is
  computed per channel (a zero-channel frame never gets "no exposure
  time"); `{:.2}` rounds a 0.125 threshold to "0.13" in the reason string;
  `measure_probe` maps an unknown model token to `Auto` silently; a
  coverage sweep before the acceptance run: PlaneReader never-shrunk
  scratch, `noise_scale_factors` two-pass loop, `BWMV_TO_SIGMA`, RCR NaN
  inputs and `n == 3`, `frame_shape`'s 1e-6 floor, the `Some(pool)` branch
  of `measure_plane`, `weights` empty/all-zero/all-excluded inputs.
- **Checkpoint B watch items:** `moment_ellipse` seeds from the whole
  stamp (crowded fields may lose stars); `aperture` clips at the image
  border for fitted centres up to 2 px off the stamp centre.

## Global Constraints

- Logic lives in `crates/athenaeum-core`; the only files outside it this
  plan touches are the generated `src/types/models.ts` (Task 1) and the two
  docs (`CLAUDE.md`, the logging spec) in Task 6. Nothing in
  `athenaeum-tauri`/`athenaeum-web`, nothing in the submodules.
- `tracing` is the only logging API; `println!`/`eprintln!` are forbidden
  outside `#[cfg(test)]` and `examples/`. Messages are short stable phrases,
  data in snake_case fields; the fields this plan introduces are listed in
  Task 6 and added to the logging dictionary there.
- Never name the reference implementation (or any other product) in code or
  comments; cite the math reference document or the papers.
- The headless build must keep passing: `cargo check -p athenaeum-core
  --no-default-features` (`stacking` is gated off there; the schema change
  compiles everywhere).
- No new crate dependencies.
- Serde: every config enum/struct uses `#[serde(rename_all = "camelCase")]`
  with the spec §9.2 names, pinned by tests, and every config struct carries
  `#[serde(default)]` so a partial JSON deserializes to defaults.
- Pixel convention: 0-based, integer coordinates are pixel centres;
  `PixelMap::forward` maps subject → reference, `Pair = (subject,
  reference)`.
- Existing behaviour is pinned: every test in `registration/`, `scanner/`,
  `tests/ts_contract.rs` and `tests/master_build.rs` passes after each task.
- Commit after every task, as the user (`git -c user.name=eg013ra1n -c
  user.email=vilen.sharifov@gmail.com commit …`), with the two trailers
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY`.
- `rustfmt` only on files this plan creates (`stacking/register/*.rs`,
  `examples/register_probe.rs`). Never rustfmt `db/schema.rs`,
  `registration/db.rs`, `scanner/mod.rs`, `calibration_library/light_resolve.rs`,
  `lib.rs`, `stacking/mod.rs` or `Cargo.toml` — hand edits only. Clippy is
  not a gate.
- Test command shape: `cargo test -p athenaeum-core stacking::register`.

---

## File structure

| File | Responsibility |
| ---- | ---- |
| `crates/athenaeum-core/src/db/schema.rs` (modify) | nine guarded `ALTER TABLE registration_results ADD COLUMN` |
| `crates/athenaeum-core/src/registration/db.rs` (modify) | `RegistrationRecord` v2 fields, upsert/select |
| `src/types/models.ts` (regenerated) | ts-rs output |
| `crates/athenaeum-core/src/stacking/register/mod.rs` (create) | `RegistrationConfig` + enums, module doc |
| `crates/athenaeum-core/src/stacking/register/detect.rs` (create) | `Star`, `luminance`, `detect_stars` |
| `crates/athenaeum-core/src/stacking/register/align.rs` (create) | `align`, `Alignment`, `AlignError` |
| `crates/athenaeum-core/src/stacking/register/frame.rs` (create) | `ReferenceStars`, `reference_stars`, `register_frame`, `FrameRegistration`, `to_record` |
| `crates/athenaeum-core/src/stacking/register/writer.rs` (create) | `source_cards_from_file`, `build_registered_cards`, `write_registered_frame` |
| `crates/athenaeum-core/src/stacking/mod.rs` (modify) | `pub mod register;` |
| `crates/athenaeum-core/src/calibration_library/light_resolve.rs` (modify) | `card_from_kv` → `pub(crate)` |
| `crates/athenaeum-core/src/scanner/mod.rs` (modify) | skip `ATH_REG`/`ATH_STK` at both scan sites + test |
| `crates/athenaeum-core/examples/register_probe.rs` (create) + `Cargo.toml` | Checkpoint A probe |
| `CLAUDE.md`, `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` (modify) | Stacking note, dictionary rows |
| `docs/superpowers/research/2026-09-09-checkpoint-a-registration.md` (create, Task 7) | the checkpoint note |

---

### Task 1: `registration_results` v2 columns and `RegistrationRecord`

**Files:**
- Modify: `crates/athenaeum-core/src/db/schema.rs` (directly after the `CREATE INDEX IF NOT EXISTS idx_registration_results_set …` statement that follows the `registration_results` CREATE TABLE)
- Modify: `crates/athenaeum-core/src/registration/db.rs` (`RegistrationRecord`, `upsert_registration`, `get_registration_for_frame_set`, the tests module)
- Regenerate: `src/types/models.ts`

**Interfaces:**
- Consumes: `column_exists(conn, table, col) -> rusqlite::Result<bool>` (schema.rs, private helper already used for other migrations).
- Produces: `RegistrationRecord` with nine new fields `model: Option<String>`, `transform_json: Option<String>`, `inlier_ratio: Option<f64>`, `peak_error_px: Option<f64>`, `scale: Option<f64>`, `rotation_deg: Option<f64>`, `flipped: bool`, `config_hash: Option<String>`, `source_kind: Option<String>`, and `#[derive(Default)]`; `upsert_registration`/`get_registration_for_frame_set` round-trip them.

- [ ] **Step 1: Write the failing test** — append inside `mod tests` of `registration/db.rs` (follow the existing fixture pattern: `Connection::open_in_memory()` + `init_db(&conn)` + whatever rows the existing `upsert_and_get_round_trip` seeds for the FKs — reuse its helper verbatim):

```rust
    #[test]
    fn v2_columns_round_trip_and_default_for_old_rows() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // Same seed rows as upsert_and_get_round_trip (frames_set + frames).
        seed_set_and_frames(&conn);
        let rec = RegistrationRecord {
            frames_set_id: 1,
            frame_id: 2,
            reference_frame_id: 1,
            matched_stars: 120,
            rms_residual_px: 0.31,
            status: "aligned".into(),
            compute_time_ms: 42,
            registered_at: "2026-09-09T00:00:00Z".into(),
            model: Some("homography+polynomial3".into()),
            transform_json: Some("{\"linear\":{}}".into()),
            inlier_ratio: Some(0.91),
            peak_error_px: Some(0.8),
            scale: Some(1.0002),
            rotation_deg: Some(-0.37),
            flipped: true,
            config_hash: Some("abc123".into()),
            source_kind: Some("calibrated".into()),
            ..Default::default()
        };
        upsert_registration(&conn, &rec).unwrap();
        // An old-style row: none of the v2 columns set.
        let legacy = RegistrationRecord {
            frames_set_id: 1,
            frame_id: 1,
            reference_frame_id: 1,
            is_reference: true,
            status: "reference".into(),
            registered_at: "2026-09-09T00:00:00Z".into(),
            ..Default::default()
        };
        upsert_registration(&conn, &legacy).unwrap();
        let rows = get_registration_for_frame_set(&conn, 1).unwrap();
        assert_eq!(rows.len(), 2);
        let (r1, r2) = (&rows[0], &rows[1]); // ordered by frame_id
        assert!(r1.is_reference && r1.model.is_none() && !r1.flipped);
        assert_eq!(r2.model.as_deref(), Some("homography+polynomial3"));
        assert_eq!(r2.transform_json.as_deref(), Some("{\"linear\":{}}"));
        assert_eq!(r2.inlier_ratio, Some(0.91));
        assert_eq!(r2.peak_error_px, Some(0.8));
        assert_eq!(r2.scale, Some(1.0002));
        assert_eq!(r2.rotation_deg, Some(-0.37));
        assert!(r2.flipped);
        assert_eq!(r2.config_hash.as_deref(), Some("abc123"));
        assert_eq!(r2.source_kind.as_deref(), Some("calibrated"));
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_db(&conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('registration_results') WHERE name IN ('model','transform_json','inlier_ratio','peak_error_px','scale','rotation_deg','flipped','config_hash','source_kind')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 9);
    }
```

If the existing tests have no shared seeding helper, extract one (`fn seed_set_and_frames(conn: &Connection)`) from `upsert_and_get_round_trip`'s setup lines and use it in all four tests — the extraction must not change what those tests insert.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p athenaeum-core registration::db` → compile errors (unknown fields, no `Default`).

- [ ] **Step 3: Implement**

`schema.rs` — after the `idx_registration_results_set` index statement:

```rust
    // Registration v2 (stacking spec §9.1): the transform model, its JSON,
    // the quality figures and the config hash — added to the existing table
    // through the guarded pattern so old catalogs keep their rows.
    for (col, ddl) in [
        ("model", "model TEXT"),
        ("transform_json", "transform_json TEXT"),
        ("inlier_ratio", "inlier_ratio REAL"),
        ("peak_error_px", "peak_error_px REAL"),
        ("scale", "scale REAL"),
        ("rotation_deg", "rotation_deg REAL"),
        ("flipped", "flipped INTEGER NOT NULL DEFAULT 0"),
        ("config_hash", "config_hash TEXT"),
        ("source_kind", "source_kind TEXT"),
    ] {
        if !column_exists(conn, "registration_results", col)? {
            conn.execute(
                &format!("ALTER TABLE registration_results ADD COLUMN {ddl}"),
                [],
            )?;
        }
    }
```

`registration/db.rs` — on `RegistrationRecord`: add `Default` to the derive list; append the nine fields after `registered_at` with this doc:

```rust
    // ── Registration v2 (stacking spec §9.1) ──────────────────────────
    /// Resolved model: the linear kind's serde name, `+polynomial<o>` when a
    /// distortion was fitted. `None` on rows written by the old service.
    pub model: Option<String>,
    /// `PixelMap::to_json()` verbatim (spec §9.1).
    pub transform_json: Option<String>,
    pub inlier_ratio: Option<f64>,
    /// Larger of the two per-axis peak residuals, px.
    pub peak_error_px: Option<f64>,
    pub scale: Option<f64>,
    pub rotation_deg: Option<f64>,
    /// The linear part has a negative determinant (a meridian flip).
    pub flipped: bool,
    /// Hash of the registration config + reference identity that produced
    /// this row (spec §9.3).
    pub config_hash: Option<String>,
    /// `"calibrated"` for v2 rows; `None` for rows of the old service.
    pub source_kind: Option<String>,
```

`upsert_registration`: extend the column list with `model, transform_json, inlier_ratio, peak_error_px, scale, rotation_deg, flipped, config_hash, source_kind` and the VALUES with `?26 … ?34`, and the `params![]` with `rec.model, rec.transform_json, rec.inlier_ratio, rec.peak_error_px, rec.scale, rec.rotation_deg, rec.flipped as i64, rec.config_hash, rec.source_kind` in that order.

`get_registration_for_frame_set`: extend the SELECT list in the same order after `registered_at` and the row mapper with

```rust
                model: row.get(26)?,
                transform_json: row.get(27)?,
                inlier_ratio: row.get(28)?,
                peak_error_px: row.get(29)?,
                scale: row.get(30)?,
                rotation_deg: row.get(31)?,
                flipped: row.get::<_, i64>(32)? != 0,
                config_hash: row.get(33)?,
                source_kind: row.get(34)?,
```

Update the three existing test literals with `..Default::default()` where they now miss fields (no other change to them).

- [ ] **Step 4: Run the tests and regenerate TS**

Run: `cargo test -p athenaeum-core registration::db` → all pass (existing + 2 new).
Run: `TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract` (writes `src/types/models.ts`), then `cargo test -p athenaeum-core --test ts_contract` → passes; `git diff --stat src/types/models.ts` shows only the `RegistrationRecord` block changed.
Run: `cargo check -p athenaeum-core --no-default-features` → clean.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/db/schema.rs crates/athenaeum-core/src/registration/db.rs src/types/models.ts
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(registration): v2 columns on registration_results and RegistrationRecord" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 2: `stacking/register/mod.rs` configuration and `detect.rs`

**Files:**
- Create: `crates/athenaeum-core/src/stacking/register/mod.rs`, `crates/athenaeum-core/src/stacking/register/detect.rs`
- Modify: `crates/athenaeum-core/src/stacking/mod.rs` (add `pub mod register;` — list stays alphabetical: `measure, psf_signal, register, robust, weights`)

**Interfaces:**
- Consumes: `crate::resample::Interpolation` (serde camelCase, no `Default`), `astroimage::ImageAnalyzer` (`new`, `with_max_stars`, `with_detection_sigma`, `with_centroid_refine`, `with_thread_pool`, `detect_fast_data(&[f32], w, h, 1) -> Result<FastAnalysisResult { stars: Vec<FastStar { x, y, peak, flux, snr, eccentricity, sx, sy, .. }>, background, .. }>`), `crate::stacking::measure::ADU_SCALE`, test fixtures `gaussian_field`, `moffat_field`, `MoffatStar`, `add_noise`.
- Produces: `RegistrationConfig` (+ `Default`, serde camelCase, container `default`), `ModelChoice`, `DistortionChoice` (+ `order()`), `DetectionConfig`; `detect::Star { x, y, flux: f64, sigma: Option<(f64, f64)> }`, `detect::SATURATION`, `detect::luminance(planes: &[&[f32]]) -> Vec<f32>`, `detect::detect_stars(lum: &[f32], w, h, cfg: &DetectionConfig, max_stars: usize, pool: Option<&Arc<rayon::ThreadPool>>) -> Vec<Star>`.

- [ ] **Step 1: Write the failing tests**

`mod.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_spec() {
        let d = RegistrationConfig::default();
        assert_eq!(d.model, ModelChoice::Auto);
        assert_eq!(d.distortion, DistortionChoice::Off);
        assert_eq!(d.interpolation, Interpolation::BicubicBSpline);
        assert_eq!(d.clamping_threshold, 0.30);
        assert_eq!(d.max_stars, 2000);
        assert_eq!(d.ransac_tolerance_px, 1.9);
        assert_eq!(d.ransac_max_iterations, 2000);
        assert_eq!(d.max_rms_px, 2.0);
        assert!(!d.fail_on_max_rms && !d.write_registered_frames);
        assert_eq!((d.detection.min_snr, d.detection.max_eccentricity), (10.0, 0.8));
    }

    #[test]
    fn serde_names_and_partial_json() {
        let json = serde_json::to_string(&RegistrationConfig::default()).unwrap();
        for needle in [
            "\"model\":\"auto\"",
            "\"distortion\":\"off\"",
            "\"interpolation\":\"bicubicBSpline\"",
            "\"clampingThreshold\":0.3",
            "\"maxStars\":2000",
            "\"ransacTolerancePx\":1.9",
            "\"ransacMaxIterations\":2000",
            "\"maxRmsPx\":2.0",
            "\"failOnMaxRms\":false",
            "\"detection\":{\"minSnr\":10.0,\"maxEccentricity\":0.8}",
            "\"writeRegisteredFrames\":false",
        ] {
            assert!(json.contains(needle), "{needle} missing in {json}");
        }
        assert!(!json.contains("sigma"), "no detection sigma: the detector is threshold-free");
        let partial: RegistrationConfig = serde_json::from_str("{\"model\":\"homography\",\"distortion\":\"polynomial3\",\"detection\":{\"minSnr\":7.5}}").unwrap();
        assert_eq!(partial.model, ModelChoice::Homography);
        assert_eq!(partial.distortion, DistortionChoice::Polynomial3);
        assert_eq!(partial.detection.min_snr, 7.5);
        assert_eq!(partial.detection.max_eccentricity, 0.8);
        assert_eq!(partial.max_stars, 2000);
        let empty: RegistrationConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, RegistrationConfig::default());
        assert_eq!(serde_json::to_string(&DistortionChoice::Polynomial4).unwrap(), "\"polynomial4\"");
        assert_eq!(serde_json::to_string(&ModelChoice::Similarity).unwrap(), "\"similarity\"");
        assert_eq!(DistortionChoice::Polynomial2.order(), Some(2));
        assert_eq!(DistortionChoice::Auto.order(), None);
    }
}
```

`detect.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{add_noise, gaussian_field, moffat_field, MoffatStar};

    fn near(stars: &[Star], x: f64, y: f64, r: f64) -> Option<&Star> {
        stars.iter().find(|s| (s.x - x).abs() <= r && (s.y - y).abs() <= r)
    }

    #[test]
    fn luminance_weights_and_passthrough() {
        let r = [1.0f32, 0.0, 0.0];
        let g = [0.0f32, 1.0, 0.0];
        let b = [0.0f32, 0.0, 1.0];
        assert_eq!(luminance(&[&r, &g, &b]), vec![0.25, 0.5, 0.25]);
        assert_eq!(luminance(&[&r]), r.to_vec());
    }

    #[test]
    fn cuts_saturated_elongated_and_faint_stars_and_keeps_the_brightest() {
        let (w, h) = (300, 300);
        // 40 ordinary stars on a grid, plus the three that must be cut.
        let mut stars: Vec<(f64, f64, f64)> = Vec::new();
        for j in 0..5 {
            for i in 0..8 {
                stars.push((30.0 + i as f64 * 34.0, 40.0 + j as f64 * 50.0, 0.05 + 0.01 * (i + j) as f64));
            }
        }
        stars.push((150.0, 150.0, 0.95)); // saturated: peak + background = 1.0
        stars.push((60.0, 280.0, 0.003)); // faint: peak ≈ 1.5 σ
        let mut data = gaussian_field(w, h, &stars, 1.8, 0.05);
        let streak = moffat_field(w, h, &[MoffatStar { x: 240.0, y: 280.0, amp: 0.3, alpha_x: 8.0, alpha_y: 2.0, theta: 0.3 }], 4.0, 0.05);
        for (d, s) in data.iter_mut().zip(streak) {
            *d += s - 0.05;
        }
        add_noise(&mut data, 0.002, 5);
        let cfg = DetectionConfig::default();
        let found = detect_stars(&data, w, h, &cfg, 2000, None);
        assert!(found.len() >= 36, "found {}", found.len());
        assert!(near(&found, 30.0, 40.0, 0.3).is_some(), "an ordinary star is missing");
        assert!(near(&found, 150.0, 150.0, 3.0).is_none(), "saturated star kept");
        assert!(near(&found, 60.0, 280.0, 3.0).is_none(), "faint star kept");
        assert!(near(&found, 240.0, 280.0, 4.0).is_none(), "elongated star kept");
        assert!(found.windows(2).all(|p| p[0].flux >= p[1].flux), "not sorted by flux");
        assert!(found.iter().filter(|s| s.sigma.is_some()).count() >= found.len() / 2, "refined σ missing on most stars");
        let top = detect_stars(&data, w, h, &cfg, 10, None);
        assert_eq!(top.len(), 10);
        assert!((top[0].flux - found[0].flux).abs() < 1e-9);
    }

    #[test]
    fn empty_and_flat_planes_yield_no_stars() {
        assert!(detect_stars(&[], 0, 0, &DetectionConfig::default(), 100, None).is_empty());
        assert!(detect_stars(&vec![0.1f32; 64 * 64], 64, 64, &DetectionConfig::default(), 100, None).is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Add `pub mod register;` to `stacking/mod.rs`; create `register/mod.rs` with `pub mod detect;` and the tests; run `cargo test -p athenaeum-core stacking::register` → compile errors.

- [ ] **Step 3: Implement**

`register/mod.rs`:

```rust
//! Registration v2 (spec §3): star detection on calibrated frames, a
//! quad-seeded correspondence search, RANSAC on the configured linear model,
//! a σ-weighted refit, optional polynomial distortion, per-frame QA and the
//! optional registered-frame writer. Per-frame services only — grouping,
//! reference selection, fan-out and persistence of a whole set belong to
//! the run orchestration. Coordinates are 0-based pixel centres;
//! `PixelMap::forward` maps subject → reference.

pub mod align;
pub mod detect;
pub mod frame;
pub mod writer;

use serde::{Deserialize, Serialize};

use crate::resample::Interpolation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ModelChoice {
    /// Homography from 30 correspondences, affine from 12, similarity below.
    #[default]
    Auto,
    Similarity,
    Affine,
    Homography,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum DistortionChoice {
    #[default]
    Off,
    Polynomial2,
    Polynomial3,
    Polynomial4,
    /// Order 3 for a subject whose geometry differs from the reference's,
    /// when the refit keeps at least `align::AUTO_DISTORTION_MIN_INLIERS`.
    Auto,
}

impl DistortionChoice {
    pub fn order(self) -> Option<u8> {
        match self {
            DistortionChoice::Polynomial2 => Some(2),
            DistortionChoice::Polynomial3 => Some(3),
            DistortionChoice::Polynomial4 => Some(4),
            DistortionChoice::Off | DistortionChoice::Auto => None,
        }
    }
}

/// Detection cuts. There is no detection sigma: the fast detector's
/// adaptive ladder is threshold-free (it targets `maxStars`), so `minSnr`
/// is the sensitivity dial.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DetectionConfig {
    pub min_snr: f32,
    pub max_eccentricity: f32,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        DetectionConfig { min_snr: 10.0, max_eccentricity: 0.8 }
    }
}

/// Spec §9.2 `registration` block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RegistrationConfig {
    pub model: ModelChoice,
    pub distortion: DistortionChoice,
    pub interpolation: Interpolation,
    pub clamping_threshold: f32,
    pub max_stars: usize,
    pub ransac_tolerance_px: f64,
    pub ransac_max_iterations: usize,
    pub max_rms_px: f64,
    pub fail_on_max_rms: bool,
    pub detection: DetectionConfig,
    pub write_registered_frames: bool,
}

impl Default for RegistrationConfig {
    fn default() -> Self {
        RegistrationConfig {
            model: ModelChoice::Auto,
            distortion: DistortionChoice::Off,
            interpolation: Interpolation::BicubicBSpline,
            clamping_threshold: 0.30,
            max_stars: 2000,
            ransac_tolerance_px: 1.9,
            ransac_max_iterations: 2000,
            max_rms_px: 2.0,
            fail_on_max_rms: false,
            detection: DetectionConfig::default(),
            write_registered_frames: false,
        }
    }
}
```

(Until Tasks 3–5 land, `pub mod align; pub mod frame; pub mod writer;` must not be present — add each line in its own task.)

`register/detect.rs`:

```rust
//! Registration stars (spec §3.1): the detector with Moffat centroid
//! refinement on a luminance plane, then the saturation / eccentricity /
//! SNR cuts and the brightest-`maxStars` truncation.

use std::sync::Arc;

use astroimage::ImageAnalyzer;

use super::DetectionConfig;
use crate::stacking::measure::ADU_SCALE;

/// Absolute saturation level in native `[0, 1]` units: a star whose
/// `peak + background` reaches it has a flat top and no usable centroid.
pub const SATURATION: f32 = 0.95;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Star {
    pub x: f64,
    pub y: f64,
    pub flux: f64,
    /// Fitted centroid σ per axis (px) when the detector's refinement
    /// succeeded; `None` keeps the pass-1 centroid unweighted.
    pub sigma: Option<(f64, f64)>,
}

/// `0.25 R + 0.5 G + 0.25 B` for three planes, the plane itself for one,
/// the plain mean otherwise. Planes are row-major `width × height`.
pub fn luminance(planes: &[&[f32]]) -> Vec<f32> {
    match planes {
        [p] => p.to_vec(),
        [r, g, b] => r
            .iter()
            .zip(g.iter())
            .zip(b.iter())
            .map(|((r, g), b)| 0.25 * r + 0.5 * g + 0.25 * b)
            .collect(),
        [] => Vec::new(),
        _ => {
            let k = 1.0 / planes.len() as f32;
            let mut acc = vec![0.0f32; planes[0].len()];
            for p in planes {
                for (a, v) in acc.iter_mut().zip(p.iter()) {
                    *a += k * v;
                }
            }
            acc
        }
    }
}

/// Detect registration stars on one luminance plane in native units. The
/// detector runs on an ADU-scaled copy (its thresholds are tuned for 16-bit
/// data); results come back in pixel coordinates and native flux.
pub fn detect_stars(
    lum: &[f32],
    w: usize,
    h: usize,
    cfg: &DetectionConfig,
    max_stars: usize,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Vec<Star> {
    if w < 8 || h < 8 || lum.len() < w * h {
        return Vec::new();
    }
    let scaled: Vec<f32> = lum.iter().map(|v| v * ADU_SCALE).collect();
    let mut analyzer = ImageAnalyzer::new()
        .with_max_stars((max_stars + max_stars / 4).max(8))
        .with_centroid_refine(true);
    if let Some(p) = pool {
        analyzer = analyzer.with_thread_pool(Arc::clone(p));
    }
    let result = match analyzer.detect_fast_data(&scaled, w, h, 1) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "star detection failed; no registration stars");
            return Vec::new();
        }
    };
    let saturation = SATURATION * ADU_SCALE;
    let mut stars: Vec<Star> = result
        .stars
        .iter()
        .filter(|s| s.peak + result.background < saturation)
        .filter(|s| s.eccentricity <= cfg.max_eccentricity)
        .filter(|s| s.snr >= cfg.min_snr)
        .map(|s| Star {
            x: s.x as f64,
            y: s.y as f64,
            flux: (s.flux / ADU_SCALE) as f64,
            sigma: (s.sx > 0.0 && s.sy > 0.0).then(|| (s.sx as f64, s.sy as f64)),
        })
        .collect();
    stars.sort_by(|a, b| b.flux.total_cmp(&a.flux));
    stars.truncate(max_stars);
    stars
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p athenaeum-core stacking::register` → 5 pass. If the cut test fails, report the offending assertion with the detections found within 5 px of each planted star (x, y, peak + background, eccentricity, snr) — do not loosen the thresholds; the controller rules.
Run: `cargo check -p athenaeum-core --no-default-features` → clean.
Run: `rustfmt crates/athenaeum-core/src/stacking/register/mod.rs crates/athenaeum-core/src/stacking/register/detect.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/stacking/register crates/athenaeum-core/src/stacking/mod.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): registration config and star detection on calibrated planes" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 3: `align.rs` — seed, correspondences, RANSAC, refit, distortion, QA

**Files:**
- Create: `crates/athenaeum-core/src/stacking/register/align.rs`
- Modify: `crates/athenaeum-core/src/stacking/register/mod.rs` (add `pub mod align;`)

**Interfaces:**
- Consumes: `solvemyastro::quad::{build_quads, fit_affine, group_size_for, match_quads}` (`build_quads(points: &[(f64,f64)], n, group_size) -> Vec<Quad>`, `match_quads(image, catalog, tol) -> Vec<(usize, usize)>`, `fit_affine(matches, image_quads, catalog_quads) -> Option<Affine { a1, b1, c1, a2, b2, c2 }>` giving image→catalog); `crate::geometry::{ransac_fit, refit_weighted, Distortion, KdTree2, Linear, LinearKind, Pair, PixelMap, RansacConfig}` (`RansacConfig::new(kind, frame_w, frame_h)` then set `tolerance_px`, `max_iterations`, `min_inliers`; `ransac_fit(&[Pair], &RansacConfig) -> Option<RansacResult { linear, inliers: Vec<usize>, rms_px, quality: Quality { inlier_ratio, overlap, regularity, score }, iterations }>`; `refit_weighted(&[Pair], &[usize], Option<&[(f64,f64)]>, LinearKind, clip_sigma) -> Option<RefitResult { linear, inliers, rms_px, sigma_rms_px, peak_px, rounds }>`; `Distortion::fit(order, &Linear, &[Pair], Option<&[f64]>, center, scale) -> Option<Distortion>` + `is_well_formed()`; `PixelMap::{linear, with_distortion, forward}`; `Linear::{from_flat, apply, scale, rotation_deg, translation, is_flipped}`; `KdTree2::{build, nearest_within}`); `super::{ModelChoice, DistortionChoice, RegistrationConfig}`, `super::detect::Star`.
- Produces: constants `QUAD_TOLERANCE = 0.007`, `MIN_INLIERS = 8`, `SCALE_RANGE = (0.8, 1.25)`, `AUTO_HOMOGRAPHY_MIN = 30`, `AUTO_AFFINE_MIN = 12`, `AUTO_DISTORTION_MIN_INLIERS = 200`, `CLIP_SIGMA = 3.0`; `enum AlignError` (serde camelCase, `Display`); `struct Alignment`; `fn align(subject: &[Star], reference: &[Star], reference_geometry: (usize, usize), subject_geometry: (usize, usize), cfg: &RegistrationConfig) -> Result<Alignment, AlignError>`; `fn resolve_model(choice: ModelChoice, n: usize) -> LinearKind`; `fn model_name(kind: LinearKind, distortion_order: Option<u8>) -> String`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::ransac::SplitMix64;

    fn field(seed: u64, n: usize, w: f64, h: f64) -> Vec<Star> {
        let mut rng = SplitMix64(seed);
        (0..n)
            .map(|_| Star {
                x: 20.0 + rng.next_f64() * (w - 40.0),
                y: 20.0 + rng.next_f64() * (h - 40.0),
                flux: 100.0 + rng.next_f64() * 900.0,
                sigma: Some((0.05, 0.05)),
            })
            .collect()
    }

    fn similarity(scale: f64, rot_deg: f64, tx: f64, ty: f64) -> Linear {
        let (s, c) = rot_deg.to_radians().sin_cos();
        Linear::from_flat(
            LinearKind::Similarity,
            [scale * c, -scale * s, tx, scale * s, scale * c, ty, 0.0, 0.0, 1.0],
        )
    }

    /// Reference stars = `truth.forward(subject)` + jitter, dropped when they
    /// leave the reference frame; `outliers` extra unmatched stars on each
    /// side; both lists shuffled.
    fn scene(subject: &[Star], truth: &Linear, jitter: f64, outliers: usize, w: f64, h: f64, seed: u64) -> (Vec<Star>, Vec<Star>) {
        let mut rng = SplitMix64(seed);
        let mut reference: Vec<Star> = subject
            .iter()
            .filter_map(|s| {
                let (x, y) = truth.apply(s.x, s.y);
                let (jx, jy) = ((rng.next_f64() - 0.5) * 2.0 * jitter, (rng.next_f64() - 0.5) * 2.0 * jitter);
                (x >= 0.0 && y >= 0.0 && x < w && y < h).then_some(Star { x: x + jx, y: y + jy, flux: s.flux, sigma: s.sigma })
            })
            .collect();
        let mut subject = subject.to_vec();
        reference.extend(field(seed + 1, outliers, w, h));
        subject.extend(field(seed + 2, outliers, w, h));
        for v in [&mut reference, &mut subject] {
            for i in (1..v.len()).rev() {
                let j = rng.below(i + 1);
                v.swap(i, j);
            }
        }
        (subject, reference)
    }

    const W: f64 = 1000.0;
    const H: f64 = 800.0;

    #[test]
    fn recovers_a_similarity_with_outliers_and_jitter() {
        let subject = field(1, 300, W, H);
        let truth = similarity(1.002, 3.0, 12.3, -7.7);
        let (sub, refs) = scene(&subject, &truth, 0.05, 90, W, H, 7);
        let a = align(&sub, &refs, (W as usize, H as usize), (W as usize, H as usize), &RegistrationConfig::default()).unwrap();
        assert_eq!(a.model, LinearKind::Homography, "auto picks homography above 30 pairs");
        assert!(a.inliers >= 150, "inliers {}", a.inliers);
        assert!(a.rms_px < 0.1, "rms {}", a.rms_px);
        assert!((a.scale - 1.002).abs() < 1e-4, "scale {}", a.scale);
        assert!((a.rotation_deg - 3.0).abs() < 1e-3, "rotation {}", a.rotation_deg);
        assert!((a.translation.0 - 12.3).abs() < 0.05 && (a.translation.1 + 7.7).abs() < 0.05, "translation {:?}", a.translation);
        assert!(!a.flipped && a.distortion_order.is_none());
        assert!(a.inlier_ratio > 0.5 && a.quality_score > 0.0);
        let (fx, fy) = a.map.forward(500.0, 400.0);
        let (tx, ty) = truth.apply(500.0, 400.0);
        assert!((fx - tx).abs() < 0.05 && (fy - ty).abs() < 0.05);
        assert_eq!(model_name(a.model, a.distortion_order), "homography");
    }

    #[test]
    fn a_mirrored_subject_is_flipped_not_failed() {
        let subject = field(2, 250, W, H);
        let truth = Linear::from_flat(LinearKind::Affine, [-1.0, 0.0, W - 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        let (sub, refs) = scene(&subject, &truth, 0.05, 60, W, H, 8);
        let a = align(&sub, &refs, (W as usize, H as usize), (W as usize, H as usize), &RegistrationConfig::default()).unwrap();
        assert!(a.flipped);
        assert!(a.rms_px < 0.1 && a.inliers >= 120);
    }

    #[test]
    fn auto_model_steps_down_with_few_stars() {
        let cfg = RegistrationConfig::default();
        let truth = similarity(1.0, 0.5, 3.0, -2.0);
        for (n, kind) in [(10usize, LinearKind::Similarity), (20, LinearKind::Affine), (60, LinearKind::Homography)] {
            let subject = field(3, n, W, H);
            let (sub, refs) = scene(&subject, &truth, 0.02, 0, W, H, 9);
            let a = align(&sub, &refs, (W as usize, H as usize), (W as usize, H as usize), &cfg).unwrap();
            assert_eq!(a.model, kind, "n = {n}");
            assert!(a.rms_px < 0.1, "n = {n} rms {}", a.rms_px);
        }
        let fixed = RegistrationConfig { model: ModelChoice::Similarity, ..Default::default() };
        let subject = field(3, 60, W, H);
        let (sub, refs) = scene(&subject, &truth, 0.02, 0, W, H, 9);
        assert_eq!(align(&sub, &refs, (W as usize, H as usize), (W as usize, H as usize), &fixed).unwrap().model, LinearKind::Similarity);
    }

    #[test]
    fn failure_modes_are_named() {
        let cfg = RegistrationConfig::default();
        let geo = (W as usize, H as usize);
        let few = field(4, 5, W, H);
        assert!(matches!(align(&few, &few, geo, geo, &cfg), Err(AlignError::TooFewStars { .. })));
        let subject = field(5, 200, W, H);
        let big = similarity(1.5, 0.0, 0.0, 0.0);
        let (sub, refs) = scene(&subject, &big, 0.02, 0, W, H, 10);
        assert!(matches!(align(&sub, &refs, geo, geo, &cfg), Err(AlignError::ScaleOutOfRange { .. })));
        let noisy = similarity(1.0, 1.0, 5.0, 5.0);
        let (sub, refs) = scene(&subject, &noisy, 1.5, 0, W, H, 11);
        let strict = RegistrationConfig { max_rms_px: 0.5, fail_on_max_rms: true, ransac_tolerance_px: 4.0, ..Default::default() };
        assert!(matches!(align(&sub, &refs, geo, geo, &strict), Err(AlignError::RmsTooHigh { .. })));
        let lenient = RegistrationConfig { max_rms_px: 0.5, ransac_tolerance_px: 4.0, ..Default::default() };
        let a = align(&sub, &refs, geo, geo, &lenient).unwrap();
        assert!(a.warnings.iter().any(|w| w.contains("RMS")), "{:?}", a.warnings);
        let unrelated = field(6, 200, W, H);
        assert!(align(&subject, &unrelated, geo, geo, &cfg).is_err());
        assert_eq!(format!("{}", AlignError::TooFewInliers { inliers: 5 }), "only 5 inliers");
    }

    #[test]
    fn polynomial_distortion_absorbs_a_radial_term() {
        let subject = field(12, 400, W, H);
        let truth = similarity(1.0, 1.0, 4.0, -3.0);
        let (cx, cy) = (W / 2.0, H / 2.0);
        // reference = truth(subject) then a barrel term r' = r(1 + k r²).
        let k = 8e-9;
        let reference: Vec<Star> = subject
            .iter()
            .filter_map(|s| {
                let (x, y) = truth.apply(s.x, s.y);
                let (dx, dy) = (x - cx, y - cy);
                let f = 1.0 + k * (dx * dx + dy * dy);
                let (x, y) = (cx + dx * f, cy + dy * f);
                (x >= 0.0 && y >= 0.0 && x < W && y < H).then_some(Star { x, y, flux: s.flux, sigma: s.sigma })
            })
            .collect();
        let geo = (W as usize, H as usize);
        let linear_only = align(&subject, &reference, geo, geo, &RegistrationConfig::default()).unwrap();
        assert!(linear_only.rms_px > 0.25, "linear rms {}", linear_only.rms_px);
        let cfg = RegistrationConfig { distortion: DistortionChoice::Polynomial3, ..Default::default() };
        let a = align(&subject, &reference, geo, geo, &cfg).unwrap();
        assert_eq!(a.distortion_order, Some(3));
        assert!(a.rms_px < 0.05, "polynomial rms {}", a.rms_px);
        assert_eq!(model_name(a.model, a.distortion_order), "homography+polynomial3");
        let auto = RegistrationConfig { distortion: DistortionChoice::Auto, ..Default::default() };
        assert_eq!(align(&subject, &reference, geo, geo, &auto).unwrap().distortion_order, None, "same geometry: auto stays linear");
        assert_eq!(align(&subject, &reference, geo, (1010, 800), &auto).unwrap().distortion_order, Some(3), "cross geometry with ≥ 200 inliers: auto fits order 3");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Add `pub mod align;` to `register/mod.rs`; run `cargo test -p athenaeum-core stacking::register::align` → compile errors.

- [ ] **Step 3: Implement**

```rust
//! Subject → reference alignment (spec §3.2–3.3, §3.6): a quad-matched
//! seed, KD-tree correspondences, RANSAC on the configured linear model,
//! a σ-weighted refit, optional polynomial distortion and the QA gates.
//! Pure geometry over star lists; no I/O.

use std::fmt;

use serde::{Deserialize, Serialize};
use solvemyastro::quad::{build_quads, fit_affine, group_size_for, match_quads};

use super::detect::Star;
use super::{DistortionChoice, ModelChoice, RegistrationConfig};
use crate::geometry::{ransac_fit, refit_weighted, Distortion, KdTree2, Linear, LinearKind, Pair, PixelMap, RansacConfig};

/// Quad-ratio tolerance of the seed matcher (the plate solver's default).
pub const QUAD_TOLERANCE: f64 = 0.007;
/// RANSAC and the QA gates need at least this many inliers.
pub const MIN_INLIERS: usize = 8;
/// A linear scale outside this range fails the frame (spec §3.6).
pub const SCALE_RANGE: (f64, f64) = (0.8, 1.25);
/// `model: auto` — homography from this many correspondences …
pub const AUTO_HOMOGRAPHY_MIN: usize = 30;
/// … affine from this many, similarity below.
pub const AUTO_AFFINE_MIN: usize = 12;
/// `distortion: auto` needs this many refit inliers (spec §3.3).
pub const AUTO_DISTORTION_MIN_INLIERS: usize = 200;
/// Refit clipping (spec §3.2 step 4).
pub const CLIP_SIGMA: f64 = 3.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AlignError {
    TooFewStars { subject: usize, reference: usize },
    NoSeed { matches: usize },
    TooFewMatches { matches: usize },
    TooFewInliers { inliers: usize },
    Degenerate,
    ScaleOutOfRange { scale: f64 },
    RmsTooHigh { rms_px: f64, max_rms_px: f64 },
}

impl fmt::Display for AlignError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AlignError::TooFewStars { subject, reference } => write!(f, "too few stars (subject {subject}, reference {reference})"),
            AlignError::NoSeed { matches } => write!(f, "quad seed failed ({matches} quad matches)"),
            AlignError::TooFewMatches { matches } => write!(f, "only {matches} correspondences within tolerance"),
            AlignError::TooFewInliers { inliers } => write!(f, "only {inliers} inliers"),
            AlignError::Degenerate => write!(f, "degenerate transform"),
            AlignError::ScaleOutOfRange { scale } => write!(f, "scale {scale:.4} outside [{}, {}]", SCALE_RANGE.0, SCALE_RANGE.1),
            AlignError::RmsTooHigh { rms_px, max_rms_px } => write!(f, "RMS {rms_px:.2} px above {max_rms_px:.2}"),
        }
    }
}

impl std::error::Error for AlignError {}

#[derive(Debug, Clone, PartialEq)]
pub struct Alignment {
    /// Subject → reference (with the stored inverse).
    pub map: PixelMap,
    pub model: LinearKind,
    pub distortion_order: Option<u8>,
    pub seed_matches: usize,
    /// Correspondences the seed produced.
    pub pairs: usize,
    /// Refit inliers.
    pub inliers: usize,
    pub inlier_ratio: f64,
    /// Through the final map, over the refit inliers.
    pub rms_px: f64,
    pub sigma_rms_px: f64,
    pub peak_px: (f64, f64),
    pub scale: f64,
    pub rotation_deg: f64,
    pub translation: (f64, f64),
    pub flipped: bool,
    pub quality_score: f64,
    pub overlap: f64,
    pub regularity: f64,
    pub ransac_iterations: usize,
    pub refit_rounds: usize,
    pub warnings: Vec<String>,
}

pub fn resolve_model(choice: ModelChoice, n: usize) -> LinearKind {
    match choice {
        ModelChoice::Similarity => LinearKind::Similarity,
        ModelChoice::Affine => LinearKind::Affine,
        ModelChoice::Homography => LinearKind::Homography,
        ModelChoice::Auto => {
            if n >= AUTO_HOMOGRAPHY_MIN {
                LinearKind::Homography
            } else if n >= AUTO_AFFINE_MIN {
                LinearKind::Affine
            } else {
                LinearKind::Similarity
            }
        }
    }
}

/// `registration_results.model`: the linear kind's serde name, plus
/// `+polynomial<o>` when a distortion was fitted.
pub fn model_name(kind: LinearKind, distortion_order: Option<u8>) -> String {
    let base = serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    match distortion_order {
        Some(o) => format!("{base}+polynomial{o}"),
        None => base,
    }
}

/// A polynomial of `order` needs `(order+1)(order+2)` pairs (twice its
/// per-axis term count).
fn min_pairs_for(order: u8) -> usize {
    let o = order as usize;
    (o + 1) * (o + 2)
}

/// Seed subject → reference from quad matching (subject plays "image",
/// reference plays "catalog"; the fitted affine maps image → catalog).
fn seed_affine(sub: &[(f64, f64)], refp: &[(f64, f64)]) -> Result<(Linear, usize), AlignError> {
    let sub_q = build_quads(sub, sub.len(), group_size_for(sub.len()));
    let ref_q = build_quads(refp, refp.len(), group_size_for(refp.len()));
    let matches = match_quads(&sub_q, &ref_q, QUAD_TOLERANCE);
    let a = fit_affine(&matches, &sub_q, &ref_q).ok_or(AlignError::NoSeed { matches: matches.len() })?;
    Ok((
        Linear::from_flat(LinearKind::Affine, [a.a1, a.b1, a.c1, a.a2, a.b2, a.c2, 0.0, 0.0, 1.0]),
        matches.len(),
    ))
}

fn residual_stats(map: &PixelMap, pairs: &[Pair]) -> (f64, f64, (f64, f64)) {
    let mut sum2 = 0.0;
    let mut sum = 0.0;
    let (mut px, mut py) = (0.0f64, 0.0f64);
    for &((sx, sy), (rx, ry)) in pairs {
        let (fx, fy) = map.forward(sx, sy);
        let (dx, dy) = (fx - rx, fy - ry);
        let d = (dx * dx + dy * dy).sqrt();
        sum2 += d * d;
        sum += d;
        px = px.max(dx.abs());
        py = py.max(dy.abs());
    }
    let n = pairs.len().max(1) as f64;
    let rms = (sum2 / n).sqrt();
    let mean = sum / n;
    let var = (sum2 / n - mean * mean).max(0.0);
    (rms, var.sqrt(), (px, py))
}

pub fn align(
    subject: &[Star],
    reference: &[Star],
    reference_geometry: (usize, usize),
    subject_geometry: (usize, usize),
    cfg: &RegistrationConfig,
) -> Result<Alignment, AlignError> {
    if subject.len() < MIN_INLIERS || reference.len() < MIN_INLIERS {
        return Err(AlignError::TooFewStars { subject: subject.len(), reference: reference.len() });
    }
    let sub_pts: Vec<(f64, f64)> = subject.iter().map(|s| (s.x, s.y)).collect();
    let ref_pts: Vec<(f64, f64)> = reference.iter().map(|s| (s.x, s.y)).collect();

    // 1. Seed.
    let (seed, seed_matches) = seed_affine(&sub_pts, &ref_pts)?;

    // 2. Correspondences through the seed, nearest reference star within 2·tol.
    let tree = KdTree2::build(&ref_pts);
    let radius = 2.0 * cfg.ransac_tolerance_px;
    let mut pairs: Vec<Pair> = Vec::new();
    let mut sigmas: Vec<(f64, f64)> = Vec::new();
    let mut all_sigmas = true;
    for s in subject {
        let (px, py) = seed.apply(s.x, s.y);
        if let Some((j, _)) = tree.nearest_within(px, py, radius) {
            let r = &reference[j];
            pairs.push(((s.x, s.y), (r.x, r.y)));
            match (s.sigma, r.sigma) {
                (Some(a), Some(b)) => sigmas.push(((a.0 * a.0 + b.0 * b.0).sqrt(), (a.1 * a.1 + b.1 * b.1).sqrt())),
                _ => {
                    all_sigmas = false;
                    sigmas.push((0.0, 0.0));
                }
            }
        }
    }
    if pairs.len() < MIN_INLIERS {
        return Err(AlignError::TooFewMatches { matches: pairs.len() });
    }

    // 3. RANSAC on the model resolved from the correspondence count.
    let ransac_kind = resolve_model(cfg.model, pairs.len());
    let mut rc = RansacConfig::new(ransac_kind, reference_geometry.0 as f64, reference_geometry.1 as f64);
    rc.tolerance_px = cfg.ransac_tolerance_px;
    rc.max_iterations = cfg.ransac_max_iterations;
    rc.min_inliers = MIN_INLIERS;
    let ransac = ransac_fit(&pairs, &rc).ok_or(AlignError::TooFewInliers { inliers: 0 })?;
    if ransac.inliers.len() < MIN_INLIERS {
        return Err(AlignError::TooFewInliers { inliers: ransac.inliers.len() });
    }

    // 4. σ-weighted refit; `auto` re-resolves from the inlier count (it can only step down).
    let kind = resolve_model(cfg.model, ransac.inliers.len());
    let sig = all_sigmas.then_some(sigmas.as_slice());
    let refit = refit_weighted(&pairs, &ransac.inliers, sig, kind, CLIP_SIGMA)
        .ok_or(AlignError::TooFewInliers { inliers: ransac.inliers.len() })?;
    if refit.inliers.len() < MIN_INLIERS {
        return Err(AlignError::TooFewInliers { inliers: refit.inliers.len() });
    }
    let linear = refit.linear;
    let scale = linear.scale();
    if !(scale >= SCALE_RANGE.0 && scale <= SCALE_RANGE.1) {
        return Err(AlignError::ScaleOutOfRange { scale });
    }

    // 5. Optional distortion on the refit inliers.
    let inlier_pairs: Vec<Pair> = refit.inliers.iter().map(|&i| pairs[i]).collect();
    let mut warnings = Vec::new();
    let cross_geometry = subject_geometry != reference_geometry;
    let wanted = match cfg.distortion {
        DistortionChoice::Auto => (cross_geometry && refit.inliers.len() >= AUTO_DISTORTION_MIN_INLIERS).then_some(3u8),
        other => other.order(),
    };
    let linear_map = || PixelMap::linear(linear).ok_or(AlignError::Degenerate);
    let (map, distortion_order) = match wanted {
        None => (linear_map()?, None),
        Some(o) if refit.inliers.len() < min_pairs_for(o) => {
            warnings.push(format!(
                "polynomial{o} distortion needs {} inliers, have {}; linear model kept",
                min_pairs_for(o),
                refit.inliers.len()
            ));
            (linear_map()?, None)
        }
        Some(o) => {
            let center = (reference_geometry.0 as f64 / 2.0, reference_geometry.1 as f64 / 2.0);
            let norm = reference_geometry.0.max(reference_geometry.1) as f64 / 2.0;
            let weights: Option<Vec<f64>> = all_sigmas.then(|| {
                refit.inliers.iter().map(|&i| {
                    let (sx, sy) = sigmas[i];
                    1.0 / (sx * sx + sy * sy + 1e-4)
                }).collect()
            });
            let fitted = Distortion::fit(o, &linear, &inlier_pairs, weights.as_deref(), center, norm)
                .filter(|d| d.is_well_formed())
                .and_then(|d| PixelMap::with_distortion(linear, d));
            match fitted {
                Some(m) => (m, Some(o)),
                None => {
                    warnings.push(format!("polynomial{o} distortion did not fit; linear model kept"));
                    (linear_map()?, None)
                }
            }
        }
    };

    // 6. QA through the final map.
    let (rms_px, sigma_rms_px, peak_px) = residual_stats(&map, &inlier_pairs);
    if rms_px > cfg.max_rms_px {
        if cfg.fail_on_max_rms {
            return Err(AlignError::RmsTooHigh { rms_px, max_rms_px: cfg.max_rms_px });
        }
        warnings.push(format!("RMS {rms_px:.2} px above {:.2}", cfg.max_rms_px));
    }
    Ok(Alignment {
        map,
        model: kind,
        distortion_order,
        seed_matches,
        pairs: pairs.len(),
        inliers: refit.inliers.len(),
        inlier_ratio: refit.inliers.len() as f64 / pairs.len() as f64,
        rms_px,
        sigma_rms_px,
        peak_px,
        scale,
        rotation_deg: linear.rotation_deg(),
        translation: linear.translation(),
        flipped: linear.is_flipped(),
        quality_score: ransac.quality.score,
        overlap: ransac.quality.overlap,
        regularity: ransac.quality.regularity,
        ransac_iterations: ransac.iterations,
        refit_rounds: refit.rounds,
        warnings,
    })
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p athenaeum-core stacking::register::align` → 5 pass. These fixtures were derived by hand (jitter 0.05 px gives RMS ≈ 0.07; 300 stars over 1000×800 give translation errors ≈ 0.005 px, rotation ≈ 1e-5 rad, scale ≈ 1e-5; the barrel term is 1.0 px at 500 px radius). If an assertion fails with a faithful transcription, report the measured values (model, pairs, inliers, rms, scale, rotation, translation, distortion order, warnings) and do not loosen — the controller rules. If the seed itself fails (`NoSeed`) on the 300-star scene, report the quad-match count.
Run: `rustfmt crates/athenaeum-core/src/stacking/register/align.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/stacking/register/align.rs crates/athenaeum-core/src/stacking/register/mod.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): quad-seeded RANSAC alignment with refit, distortion and QA" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 4: `frame.rs` — per-frame driver and the record conversion

**Files:**
- Create: `crates/athenaeum-core/src/stacking/register/frame.rs`
- Modify: `crates/athenaeum-core/src/stacking/register/mod.rs` (add `pub mod frame;`)

**Interfaces:**
- Consumes: `crate::integration::plane_reader::PlaneReader`, `crate::integration::IntegrationError`, `super::detect::{detect_stars, luminance, Star}`, `super::align::{align, model_name, AlignError, Alignment}`, `super::RegistrationConfig`, `crate::registration::db::RegistrationRecord`, `crate::geometry::{Linear, LinearKind, PixelMap}`.
- Produces: `struct ReferenceStars { stars: Vec<Star>, width, height: usize }`; `fn reference_stars(path: &Path, cfg: &RegistrationConfig, pool: Option<&Arc<rayon::ThreadPool>>) -> Result<ReferenceStars, IntegrationError>`; `struct FrameRegistration { width, height, detections: usize, outcome: Result<Alignment, AlignError>, duration_ms: u64 }`; `fn register_frame(reference: &ReferenceStars, subject: &Path, cfg, pool, cancel: &AtomicBool) -> Result<FrameRegistration, IntegrationError>`; `fn identity_registration(reference: &ReferenceStars) -> FrameRegistration`; `fn to_record(frames_set_id: i64, frame_id: i64, reference_frame_id: i64, is_reference: bool, reg: &FrameRegistration, config_hash: &str, registered_at: &str) -> RegistrationRecord`; `pub(crate) fn read_luminance(path: &Path) -> Result<(Vec<f32>, usize, usize), IntegrationError>`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::geometry::ransac::SplitMix64;
    use crate::test_support::{add_noise, gaussian_field};
    use std::path::PathBuf;

    fn stars(seed: u64, n: usize, w: f64, h: f64) -> Vec<(f64, f64, f64)> {
        let mut rng = SplitMix64(seed);
        (0..n).map(|_| (30.0 + rng.next_f64() * (w - 60.0), 30.0 + rng.next_f64() * (h - 60.0), 0.1 + rng.next_f64() * 0.4)).collect()
    }

    /// Reference = the field; subject = the same stars shifted by (dx, dy)
    /// and rotated by `rot_deg` about the centre (subject → reference is the
    /// inverse of that), written as 1- or 3-plane FITS.
    fn pair(dir: &std::path::Path, seed: u64, dx: f64, dy: f64, rot_deg: f64, planes: usize) -> (PathBuf, PathBuf, Vec<(f64, f64, f64)>) {
        let (w, h) = (640usize, 480usize);
        let refs = stars(seed, 160, w as f64, h as f64);
        let (s, c) = rot_deg.to_radians().sin_cos();
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let subs: Vec<(f64, f64, f64)> = refs
            .iter()
            .map(|&(x, y, a)| {
                let (u, v) = (x - cx, y - cy);
                (cx + c * u - s * v + dx, cy + s * u + c * v + dy, a)
            })
            .collect();
        let mut write = |name: &str, list: &[(f64, f64, f64)]| {
            let mut plane = gaussian_field(w, h, list, 1.6, 0.08);
            add_noise(&mut plane, 0.002, seed + 11);
            let mut all = Vec::new();
            for k in 0..planes {
                all.extend(plane.iter().map(|v| v * (1.0 - 0.2 * k as f32)));
            }
            let p = dir.join(name);
            write_fits_f32(&p, w, h, planes, &all, &[]).unwrap();
            p
        };
        let r = write("reference.fits", &refs);
        let s = write("subject.fits", &subs);
        (r, s, refs)
    }

    #[test]
    fn registers_a_shifted_rotated_subject_onto_the_reference() {
        let dir = tempfile::tempdir().unwrap();
        let (r, s, _) = pair(dir.path(), 21, 7.3, -4.1, 1.5, 1);
        let cfg = RegistrationConfig::default();
        let reference = reference_stars(&r, &cfg, None).unwrap();
        assert!(reference.stars.len() >= 120, "{}", reference.stars.len());
        assert_eq!((reference.width, reference.height), (640, 480));
        let reg = register_frame(&reference, &s, &cfg, None, &AtomicBool::new(false)).unwrap();
        let a = reg.outcome.as_ref().expect("registration succeeded");
        assert!(a.inliers >= 100 && a.rms_px < 0.15, "inliers {} rms {}", a.inliers, a.rms_px);
        // A subject pixel maps back onto the reference: the subject star that
        // sits at the reference star (200, 200) rotated+shifted lands on (200, 200).
        let (s_, c_) = 1.5f64.to_radians().sin_cos();
        let (u, v) = (200.0 - 320.0, 200.0 - 240.0);
        let (sx, sy) = (320.0 + c_ * u - s_ * v + 7.3, 240.0 + s_ * u + c_ * v - 4.1);
        let (fx, fy) = a.map.forward(sx, sy);
        assert!((fx - 200.0).abs() < 0.05 && (fy - 200.0).abs() < 0.05, "forward {fx} {fy}");
        assert!((a.rotation_deg + 1.5).abs() < 0.01, "rotation {}", a.rotation_deg);
        assert!((a.scale - 1.0).abs() < 1e-3);
        assert!(reg.detections >= 120 && reg.duration_ms < 60_000);
        let rec = to_record(7, 42, 41, false, &reg, "hash", "2026-09-09T00:00:00Z");
        assert_eq!((rec.frames_set_id, rec.frame_id, rec.reference_frame_id), (7, 42, 41));
        assert_eq!(rec.status, "aligned");
        assert_eq!(rec.model.as_deref(), Some("homography"));
        assert_eq!(rec.source_kind.as_deref(), Some("calibrated"));
        assert_eq!(rec.config_hash.as_deref(), Some("hash"));
        assert!(!rec.flipped && rec.transform_json.is_some());
        assert_eq!(rec.matched_stars, a.inliers as i64);
        assert!((rec.rms_residual_px - a.rms_px).abs() < 1e-12);
        assert_eq!(rec.affine_a1.map(|v| (v - a.map.linear.m[0][0]).abs() < 1e-12), Some(true));
        let back = PixelMap::from_json(rec.transform_json.as_deref().unwrap()).unwrap();
        let (bx, by) = back.forward(sx, sy);
        assert!((bx - fx).abs() < 1e-9 && (by - fy).abs() < 1e-9);
        assert!(rec.crpix1.is_none() && rec.error.is_none());
    }

    #[test]
    fn rgb_subject_uses_luminance_and_cancel_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let (r, s, _) = pair(dir.path(), 22, -3.0, 2.5, 0.0, 3);
        let cfg = RegistrationConfig::default();
        let reference = reference_stars(&r, &cfg, None).unwrap();
        let reg = register_frame(&reference, &s, &cfg, None, &AtomicBool::new(false)).unwrap();
        let a = reg.outcome.as_ref().unwrap();
        assert!((a.translation.0 - 3.0).abs() < 0.05 && (a.translation.1 + 2.5).abs() < 0.05, "{:?}", a.translation);
        assert!(matches!(
            register_frame(&reference, &s, &cfg, None, &AtomicBool::new(true)),
            Err(IntegrationError::Cancelled)
        ));
        let bad = dir.path().join("nope.txt");
        std::fs::write(&bad, b"x").unwrap();
        assert!(matches!(register_frame(&reference, &bad, &cfg, None, &AtomicBool::new(false)), Err(IntegrationError::BadInput(_))));
    }

    #[test]
    fn an_unrelated_field_fails_with_a_reason_and_a_failed_record() {
        let dir = tempfile::tempdir().unwrap();
        let (r, _, _) = pair(dir.path(), 23, 0.0, 0.0, 0.0, 1);
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let (_, s2, _) = pair(&other, 99, 0.0, 0.0, 0.0, 1);
        let cfg = RegistrationConfig::default();
        let reference = reference_stars(&r, &cfg, None).unwrap();
        let reg = register_frame(&reference, &s2, &cfg, None, &AtomicBool::new(false)).unwrap();
        let err = reg.outcome.as_ref().err().expect("unrelated field must fail");
        let rec = to_record(1, 2, 3, false, &reg, "h", "t");
        assert_eq!(rec.status, "failed");
        assert_eq!(rec.error.as_deref(), Some(format!("{err}").as_str()));
        assert!(rec.transform_json.is_none() && rec.model.is_none() && rec.affine_a1.is_none());
        assert_eq!(rec.matched_stars, 0);
    }

    #[test]
    fn the_reference_row_is_the_identity() {
        let dir = tempfile::tempdir().unwrap();
        let (r, _, _) = pair(dir.path(), 24, 0.0, 0.0, 0.0, 1);
        let reference = reference_stars(&r, &RegistrationConfig::default(), None).unwrap();
        let reg = identity_registration(&reference);
        let a = reg.outcome.as_ref().unwrap();
        assert_eq!(a.map.forward(10.5, 20.25), (10.5, 20.25));
        let rec = to_record(1, 3, 3, true, &reg, "h", "t");
        assert!(rec.is_reference && rec.status == "reference");
        assert_eq!(rec.matched_stars, reference.stars.len() as i64);
        assert_eq!(rec.affine_a1, Some(1.0));
        assert_eq!(rec.model.as_deref(), Some("similarity"));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Add `pub mod frame;` to `register/mod.rs`; run `cargo test -p athenaeum-core stacking::register::frame` → compile errors.

- [ ] **Step 3: Implement**

```rust
//! The per-frame registration driver: read a calibrated frame through
//! `PlaneReader`, detect on its luminance, align onto the reference's stars,
//! and turn the outcome into a `registration_results` row.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, warn};

use super::align::{align, model_name, AlignError, Alignment};
use super::detect::{detect_stars, luminance, Star};
use super::RegistrationConfig;
use crate::geometry::{Linear, LinearKind, PixelMap};
use crate::integration::plane_reader::PlaneReader;
use crate::integration::IntegrationError;
use crate::registration::db::RegistrationRecord;

#[derive(Debug, Clone)]
pub struct ReferenceStars {
    pub stars: Vec<Star>,
    pub width: usize,
    pub height: usize,
}

#[derive(Debug, Clone)]
pub struct FrameRegistration {
    pub width: usize,
    pub height: usize,
    /// Registration stars found on the subject.
    pub detections: usize,
    pub outcome: Result<Alignment, AlignError>,
    pub duration_ms: u64,
}

/// All planes of a calibrated frame collapsed to one luminance plane.
pub(crate) fn read_luminance(path: &Path) -> Result<(Vec<f32>, usize, usize), IntegrationError> {
    let reader = PlaneReader::open(path)?;
    let (w, h) = (reader.width(), reader.height());
    let planes: Vec<Vec<f32>> = (0..reader.channels()).map(|p| reader.read_plane(p)).collect::<Result<_, _>>()?;
    let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
    Ok((luminance(&refs), w, h))
}

pub fn reference_stars(
    path: &Path,
    cfg: &RegistrationConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Result<ReferenceStars, IntegrationError> {
    let (lum, width, height) = read_luminance(path)?;
    let stars = detect_stars(&lum, width, height, &cfg.detection, cfg.max_stars, pool);
    debug!(path = %path.display(), detections = stars.len(), "reference stars detected");
    Ok(ReferenceStars { stars, width, height })
}

/// Register one subject onto the reference. I/O errors and cancellation
/// are `Err`; an alignment failure is a successful measurement of a frame
/// that cannot be registered (`outcome: Err(AlignError)`).
pub fn register_frame(
    reference: &ReferenceStars,
    subject: &Path,
    cfg: &RegistrationConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<FrameRegistration, IntegrationError> {
    let start = Instant::now();
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled);
    }
    let (lum, width, height) = read_luminance(subject)?;
    let stars = detect_stars(&lum, width, height, &cfg.detection, cfg.max_stars, pool);
    drop(lum);
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled);
    }
    let outcome = align(&stars, &reference.stars, (reference.width, reference.height), (width, height), cfg);
    let duration_ms = start.elapsed().as_millis() as u64;
    match &outcome {
        Ok(a) => debug!(
            path = %subject.display(),
            detections = stars.len(),
            inliers = a.inliers,
            rms_px = a.rms_px,
            model = %model_name(a.model, a.distortion_order),
            flipped = a.flipped,
            duration_ms,
            "frame registered"
        ),
        Err(e) => warn!(path = %subject.display(), detections = stars.len(), error = %e, duration_ms, "frame registration failed"),
    }
    Ok(FrameRegistration { width, height, detections: stars.len(), outcome, duration_ms })
}

/// The reference frame's own row: an identity map over its stars.
pub fn identity_registration(reference: &ReferenceStars) -> FrameRegistration {
    let map = PixelMap::linear(Linear::identity()).expect("identity is invertible");
    FrameRegistration {
        width: reference.width,
        height: reference.height,
        detections: reference.stars.len(),
        outcome: Ok(Alignment {
            map,
            model: LinearKind::Similarity,
            distortion_order: None,
            seed_matches: 0,
            pairs: reference.stars.len(),
            inliers: reference.stars.len(),
            inlier_ratio: 1.0,
            rms_px: 0.0,
            sigma_rms_px: 0.0,
            peak_px: (0.0, 0.0),
            scale: 1.0,
            rotation_deg: 0.0,
            translation: (0.0, 0.0),
            flipped: false,
            quality_score: 1.0,
            overlap: 1.0,
            regularity: 1.0,
            ransac_iterations: 0,
            refit_rounds: 0,
            warnings: Vec::new(),
        }),
        duration_ms: 0,
    }
}

/// The `registration_results` row for one outcome (spec §9.1). The legacy
/// `affine_*` columns carry the linear part's top two rows even for a
/// homography (its projective row lives in `transform_json`); the WCS
/// columns stay `None` — v2 does not plate-solve.
pub fn to_record(
    frames_set_id: i64,
    frame_id: i64,
    reference_frame_id: i64,
    is_reference: bool,
    reg: &FrameRegistration,
    config_hash: &str,
    registered_at: &str,
) -> RegistrationRecord {
    let mut rec = RegistrationRecord {
        frames_set_id,
        frame_id,
        reference_frame_id,
        is_reference,
        compute_time_ms: reg.duration_ms as i64,
        registered_at: registered_at.to_string(),
        config_hash: Some(config_hash.to_string()),
        source_kind: Some("calibrated".to_string()),
        ..Default::default()
    };
    match &reg.outcome {
        Ok(a) => {
            let m = a.map.linear.m;
            rec.affine_a1 = Some(m[0][0]);
            rec.affine_b1 = Some(m[0][1]);
            rec.affine_c1 = Some(m[0][2]);
            rec.affine_a2 = Some(m[1][0]);
            rec.affine_b2 = Some(m[1][1]);
            rec.affine_c2 = Some(m[1][2]);
            rec.matched_stars = a.inliers as i64;
            rec.rms_residual_px = a.rms_px;
            rec.status = if is_reference {
                "reference"
            } else if a.flipped {
                "aligned_flipped"
            } else {
                "aligned"
            }
            .to_string();
            rec.model = Some(model_name(a.model, a.distortion_order));
            rec.transform_json = Some(a.map.to_json());
            rec.inlier_ratio = Some(a.inlier_ratio);
            rec.peak_error_px = Some(a.peak_px.0.max(a.peak_px.1));
            rec.scale = Some(a.scale);
            rec.rotation_deg = Some(a.rotation_deg);
            rec.flipped = a.flipped;
        }
        Err(e) => {
            rec.status = "failed".to_string();
            rec.error = Some(e.to_string());
        }
    }
    rec
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p athenaeum-core stacking::register` → all pass (frame: 4). If the forward-mapping or rotation assertion fails on a faithful transcription, report the measured `Alignment` (inliers, rms, scale, rotation, translation, model) and the number of reference/subject stars — the controller rules; the sign convention of `rotation_deg` (subject → reference is the inverse of the fixture's rotation) is the likeliest culprit and is reported, not "fixed" by flipping the test.
Run: `cargo check -p athenaeum-core --no-default-features` → clean.
Run: `rustfmt crates/athenaeum-core/src/stacking/register/frame.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/stacking/register/frame.rs crates/athenaeum-core/src/stacking/register/mod.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): per-frame registration driver and registration_results conversion" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 5: `writer.rs` — registered frames with `ATH_REG*` cards, and the scanner skip

**Files:**
- Create: `crates/athenaeum-core/src/stacking/register/writer.rs`
- Modify: `crates/athenaeum-core/src/stacking/register/mod.rs` (add `pub mod writer;`)
- Modify: `crates/athenaeum-core/src/calibration_library/light_resolve.rs` (`fn card_from_kv` → `pub(crate) fn card_from_kv`)
- Modify: `crates/athenaeum-core/src/scanner/mod.rs` — both scan sites that carry the comment `Calibrated-LIGHT artifacts (spec 2026-08-31 §4) are recognized by their header cards and NEVER cataloged` (around lines 425–447 and again around 1596–1608): after the `if let Some(identity) = calibrated_light_identity(&keys) { … }` block, still inside `if let Some(ref header) = header_text { … }`, add the stacking-artifact skip; plus a test next to `scan_never_catalogs_a_calibrated_artifact`.

**Interfaces:**
- Consumes: `crate::fits_parser::FitsHeader::{from_path, to_header_text}`, `crate::fits_parser::stored_header::parse_stored_header_keys(FileFormat, &str) -> HashMap<String, String>`, `crate::models::FileFormat::FITS`, `crate::calibration_library::light_resolve::card_from_kv(&str, &str) -> Option<Card>`, `crate::fits_writer::{write_fits_f32, Card, CardValue}`, `crate::resample::{warp_rows, Plane, Interpolation}`, `crate::geometry::PixelMap`, `PlaneReader`.
- Produces: `REGISTERED_COPY_THROUGH: &[&str]`, `ATH_REG_VERSION: i64 = 1`, `struct RegisteredCards<'a> { reference_name, model, transform_json: &'a str, interpolation: Interpolation, clamping: f32, rms_px: f64 }`, `fn source_cards_from_file(path: &Path) -> anyhow::Result<Vec<Card>>`, `fn build_registered_cards(source: &[Card], reg: &RegisteredCards) -> anyhow::Result<Vec<Card>>`, `fn write_registered_frame(subject: &Path, map: &PixelMap, ref_w: usize, ref_h: usize, interp: Interpolation, clamping: f32, cards: &[Card], out: &Path) -> anyhow::Result<usize>` (channels written). Scanner: files carrying `ATH_REG` or `ATH_STK` are never cataloged.

- [ ] **Step 1: Write the failing tests**

`writer.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_parser::FitsHeader;
    use crate::fits_writer::CardValue;
    use crate::geometry::{Linear, LinearKind};
    use crate::integration::plane_reader::PlaneReader;
    use crate::test_support::{centroid, gaussian_field};

    #[test]
    fn writes_the_subject_into_reference_geometry_with_provenance_cards() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (120usize, 90usize);
        // One star at (40.25, 30.5) in the subject; the map shifts by (+10, −5).
        let data = gaussian_field(w, h, &[(40.25, 30.5, 0.5)], 1.5, 0.05);
        let subject = dir.path().join("c_sub.fits");
        let src_cards = vec![
            Card::new("EXPTIME", CardValue::Real(180.0)).unwrap(),
            Card::new("INSTRUME", CardValue::Str("cam".into())).unwrap(),
            Card::new("CRVAL1", CardValue::Real(12.5)).unwrap(),
            Card::new("BAYERPAT", CardValue::Str("RGGB".into())).unwrap(),
            Card::new("DATE-OBS", CardValue::Str("2026-09-09T00:00:00".into())).unwrap(),
        ];
        write_fits_f32(&subject, w, h, 1, &data, &src_cards).unwrap();
        let map = PixelMap::linear(Linear::from_flat(LinearKind::Similarity, [1.0, 0.0, 10.0, 0.0, 1.0, -5.0, 0.0, 0.0, 1.0])).unwrap();
        let source = source_cards_from_file(&subject).unwrap();
        let kws: Vec<&str> = source.iter().map(|c| c.keyword.as_str()).collect();
        assert!(kws.contains(&"EXPTIME") && kws.contains(&"INSTRUME") && kws.contains(&"DATE-OBS"));
        assert!(!kws.contains(&"CRVAL1") && !kws.contains(&"BAYERPAT"), "{kws:?}");
        let reg = RegisteredCards { reference_name: "c_ref", model: "homography", transform_json: &map.to_json(), interpolation: Interpolation::BicubicBSpline, clamping: 0.3, rms_px: 0.42 };
        let cards = build_registered_cards(&source, &reg).unwrap();
        let out = dir.path().join("registered").join("r_sub.fits");
        let channels = write_registered_frame(&subject, &map, 160, 100, Interpolation::BicubicBSpline, 0.3, &cards, &out).unwrap();
        assert_eq!(channels, 1);
        let r = PlaneReader::open(&out).unwrap();
        assert_eq!((r.width(), r.height(), r.channels()), (160, 100, 1));
        let plane = r.read_plane(0).unwrap();
        let (cx, cy) = centroid(&plane, 160, 50.25, 25.5, 5, 0.05);
        assert!((cx - 50.25).abs() < 0.05 && (cy - 25.5).abs() < 0.05, "centroid {cx} {cy}");
        assert!(plane[0].is_nan(), "outside the subject's coverage must be NaN");
        assert!(plane[99 * 160 + 159].is_nan());
        let header = FitsHeader::from_path(&out).unwrap();
        assert_eq!(header.get_str("IMAGETYP").as_deref(), Some("LIGHT"));
        assert_eq!(header.get_str("ATH_REG").as_deref(), Some("T"));
        assert_eq!(header.get_i32("ATH_REGV"), Some(1));
        assert_eq!(header.get_str("ATH_REGR").as_deref(), Some("c_ref"));
        assert_eq!(header.get_str("ATH_REGM").as_deref(), Some("homography"));
        assert_eq!(header.get_str("ATH_REGK").as_deref(), Some("bicubicBSpline"));
        assert!((header.get_f64("ATH_REGC").unwrap() - 0.3).abs() < 1e-6);
        assert!((header.get_f64("ATH_REGS").unwrap() - 0.42).abs() < 1e-9);
        let back = PixelMap::from_json(&header.get_str("ATH_REGT").unwrap()).unwrap();
        assert_eq!(back.forward(1.0, 2.0), (11.0, -3.0));
        assert_eq!(header.get_f64("EXPTIME"), Some(180.0));
        assert!(header.get_str("CRVAL1").is_none() && header.get_str("BAYERPAT").is_none());
    }

    #[test]
    fn three_plane_subjects_keep_their_planes() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (64usize, 48usize);
        let base = gaussian_field(w, h, &[(20.0, 20.0, 0.4)], 1.5, 0.05);
        let mut all = Vec::new();
        for k in 0..3 {
            all.extend(base.iter().map(|v| v * (1.0 - 0.25 * k as f32)));
        }
        let subject = dir.path().join("c_rgb.fits");
        write_fits_f32(&subject, w, h, 3, &all, &[]).unwrap();
        let map = PixelMap::linear(Linear::identity()).unwrap();
        let out = dir.path().join("r_rgb.fits");
        assert_eq!(write_registered_frame(&subject, &map, w, h, Interpolation::Bilinear, 0.3, &[], &out).unwrap(), 3);
        let r = PlaneReader::open(&out).unwrap();
        let p0 = r.read_plane(0).unwrap();
        let p2 = r.read_plane(2).unwrap();
        assert!((p0[20 * w + 20] - base[20 * w + 20]).abs() < 1e-5);
        assert!((p2[20 * w + 20] - 0.5 * base[20 * w + 20]).abs() < 1e-5);
    }
}
```

Scanner test (next to `scan_never_catalogs_a_calibrated_artifact`, same fixture style — a scan root in a temp dir, one file written by `write_fits_f32` with the cards below, a scan, then the assertion that no `files` row exists):

```rust
    #[test]
    fn scan_never_catalogs_a_stacking_artifact() {
        // Same setup as scan_never_catalogs_a_calibrated_artifact, but the
        // file carries ATH_REG = T (a registered frame) and no CALSTAT.
        // … fixture …
        let cards = vec![Card::new("ATH_REG", CardValue::Logical(true)).unwrap(), Card::new("IMAGETYP", CardValue::Str("LIGHT".into())).unwrap()];
        // write_fits_f32(&path, 16, 16, 1, &vec![0.1; 256], &cards)
        // scan the root
        // assert the files table has no row for `path`; repeat with ATH_STK = T
    }
```

(Write the fixture lines by copying the neighbouring test's; the assertion shape is the same.)

- [ ] **Step 2: Run to verify failure**

Add `pub mod writer;`; run `cargo test -p athenaeum-core stacking::register::writer` → compile errors; `cargo test -p athenaeum-core scan_never_catalogs_a_stacking_artifact` → fails (the file is cataloged).

- [ ] **Step 3: Implement**

`light_resolve.rs`: `fn card_from_kv(` → `pub(crate) fn card_from_kv(` (no other change).

`writer.rs`:

```rust
//! Optional registered frames (spec §3.7): the subject resampled once into
//! the reference geometry, NaN outside its coverage, with the source's
//! acquisition cards and the `ATH_REG*` provenance. Artifacts, never
//! cataloged; nothing downstream reads them.

use std::path::Path;

use anyhow::Context;

use crate::calibration_library::light_resolve::card_from_kv;
use crate::fits_parser::stored_header::parse_stored_header_keys;
use crate::fits_parser::FitsHeader;
use crate::fits_writer::{write_fits_f32, Card, CardValue};
use crate::geometry::PixelMap;
use crate::integration::plane_reader::PlaneReader;
use crate::models::FileFormat;
use crate::resample::{warp_rows, Interpolation, Plane};

/// Source cards a registered frame keeps: acquisition and target metadata
/// only. The pixel grid is the reference's, so the subject's WCS is wrong
/// for it, and the frame is resampled, so it has no CFA.
pub const REGISTERED_COPY_THROUGH: &[&str] = &[
    "EXPTIME", "GAIN", "OFFSET", "EGAIN", "XBINNING", "YBINNING", "XPIXSZ", "YPIXSZ", "CCD-TEMP",
    "SET-TEMP", "INSTRUME", "TELESCOP", "FOCALLEN", "APTDIA", "FILTER", "DATE-OBS", "OBJECT",
    "OBJCTRA", "OBJCTDEC", "RA", "DEC", "SITELAT", "SITELONG", "SITEELEV", "OBSERVER",
];

pub const ATH_REG_VERSION: i64 = 1;

pub struct RegisteredCards<'a> {
    pub reference_name: &'a str,
    pub model: &'a str,
    pub transform_json: &'a str,
    pub interpolation: Interpolation,
    pub clamping: f32,
    pub rms_px: f64,
}

/// The copy-through cards of a FITS file, read from its own header (no
/// catalog needed).
pub fn source_cards_from_file(path: &Path) -> anyhow::Result<Vec<Card>> {
    let header = FitsHeader::from_path(path).with_context(|| format!("reading header of {}", path.display()))?;
    let keys = parse_stored_header_keys(FileFormat::FITS, &header.to_header_text());
    let mut cards: Vec<Card> = keys
        .iter()
        .filter(|(k, _)| REGISTERED_COPY_THROUGH.contains(&k.to_ascii_uppercase().as_str()))
        .filter_map(|(k, v)| card_from_kv(k, v))
        .collect();
    cards.sort_by(|a, b| a.keyword.cmp(&b.keyword));
    Ok(cards)
}

/// Copy-through cards filtered once more, then `IMAGETYP` and the
/// `ATH_REG*` provenance.
pub fn build_registered_cards(source: &[Card], reg: &RegisteredCards) -> anyhow::Result<Vec<Card>> {
    let mut cards: Vec<Card> = source
        .iter()
        .filter(|c| REGISTERED_COPY_THROUGH.contains(&c.keyword.as_str()))
        .cloned()
        .collect();
    let kernel = serde_json::to_value(reg.interpolation)?
        .as_str()
        .unwrap_or("")
        .to_string();
    cards.push(Card::new("IMAGETYP", CardValue::Str("LIGHT".into()))?.with_comment("registered light"));
    cards.push(Card::new("ATH_REG", CardValue::Logical(true))?.with_comment("registered by Athenaeum; never cataloged"));
    cards.push(Card::new("ATH_REGV", CardValue::Integer(ATH_REG_VERSION))?.with_comment("registered-frame format version"));
    cards.push(Card::new("ATH_REGR", CardValue::Str(reg.reference_name.to_string()))?.with_comment("reference frame"));
    cards.push(Card::new("ATH_REGM", CardValue::Str(reg.model.to_string()))?.with_comment("registration model"));
    cards.push(Card::new("ATH_REGK", CardValue::Str(kernel))?.with_comment("resampling kernel"));
    cards.push(Card::new("ATH_REGC", CardValue::Real(reg.clamping as f64))?.with_comment("kernel clamping threshold"));
    cards.push(Card::new("ATH_REGS", CardValue::Real(reg.rms_px))?.with_comment("[px] registration RMS"));
    // Arbitrary length → CONTINUE chain; no comment (it could overflow the last record).
    cards.push(Card::new("ATH_REGT", CardValue::Str(reg.transform_json.to_string()))?);
    Ok(cards)
}

/// Resample every plane of `subject` into the reference geometry through
/// `map` (subject → reference; the resampler gathers through its inverse)
/// and write the float32 FITS. Returns the plane count.
pub fn write_registered_frame(
    subject: &Path,
    map: &PixelMap,
    ref_w: usize,
    ref_h: usize,
    interp: Interpolation,
    clamping: f32,
    cards: &[Card],
    out: &Path,
) -> anyhow::Result<usize> {
    let reader = PlaneReader::open(subject)?;
    let (w, h, channels) = (reader.width(), reader.height(), reader.channels());
    let plane_len = ref_w * ref_h;
    let mut all = vec![f32::NAN; plane_len * channels];
    for plane in 0..channels {
        let data = reader.read_plane(plane)?;
        let src = Plane::full(&data, w, h);
        warp_rows(&src, map, ref_w, 0, ref_h, interp, clamping, &mut all[plane * plane_len..(plane + 1) * plane_len]);
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    write_fits_f32(out, ref_w, ref_h, channels, &all, cards).with_context(|| format!("writing {}", out.display()))?;
    Ok(channels)
}
```

`warp_rows` takes `map: &dyn InverseMap` and `PixelMap` implements `InverseMap`, so `map` coerces as written. `FitsWriteError` and `IntegrationError` both implement `std::error::Error`, so `?`/`.with_context` convert them into `anyhow::Error`.

Scanner (both sites), right after the closing brace of `if let Some(identity) = calibrated_light_identity(&keys) { … }`:

```rust
        // Stacking artifacts (registered frames, masters — stacking spec §3.7,
        // §6.4) are recognized the same way and never cataloged.
        if keys.contains_key("ATH_REG") || keys.contains_key("ATH_STK") {
            tracing::debug!(
                root_id,
                path = %current_path,
                "stacking artifact (ATH_REG/ATH_STK) — never cataloged"
            );
            return Ok(None);
        }
```

(Use whatever the surrounding block binds for the root id and path — mirror the calibrated-artifact `debug!` two lines above it exactly. Check the key map's case: `calibrated_light_identity` looks up `ATH_CSRC` uppercase in the same map, so uppercase keys are the convention.)

- [ ] **Step 4: Run the tests**

Run: `cargo test -p athenaeum-core stacking::register::writer` → 2 pass; `cargo test -p athenaeum-core scanner::` → all pass incl. the new one; `cargo test -p athenaeum-core calibration_library::` and `export::` → unchanged.
Run: `cargo check -p athenaeum-core --no-default-features` → clean.
Run: `rustfmt crates/athenaeum-core/src/stacking/register/writer.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/stacking/register/writer.rs crates/athenaeum-core/src/stacking/register/mod.rs crates/athenaeum-core/src/calibration_library/light_resolve.rs crates/athenaeum-core/src/scanner/mod.rs
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "feat(stacking): registered-frame writer with ATH_REG cards; scanner skips stacking artifacts" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 6: `register_probe` example, docs

**Files:**
- Create: `crates/athenaeum-core/examples/register_probe.rs`
- Modify: `crates/athenaeum-core/Cargo.toml` — after the `measure_probe` `[[example]]` block add:

```toml
[[example]]
name = "register_probe"
required-features = ["render", "solver"]
```

- Modify: `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` — insert directly BEFORE the paragraph beginning `**Dictionary note (fix round 2, 2026-09-06):**` (i.e. after the stacking-measurement paragraph Plan 2 added):

```markdown
**Dictionary extension (stacking registration, `stacking::register`, 2026-09-09):** `detections` (usize; registration stars found on a frame, on the `"reference stars detected"` and `"frame registered"` debugs and the `"frame registration failed"` warn), `model` (string; the resolved registration model, e.g. `homography+polynomial3`, on `"frame registered"`), `flipped` (bool; the linear part has a negative determinant — reuses the registration domain's existing name on `"frame registered"`). Reuses `path`, `inliers`, `rms_px` (the solve domain's names, same meaning: refit inliers and the RMS through the final map), `error` and `duration_ms`.
```

- Modify: `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §9.2 — in the `registration:` block change `detection: { sigma: 5.0, minSnr: 10, maxEccentricity: 0.8 }` to `detection: { minSnr: 10, maxEccentricity: 0.8 }` (ruling 13); and in §3.1 replace the sentence beginning "`detect_fast` with Moffat centroid refinement" with: "`detect_fast` with Moffat centroid refinement (~0.05 px on well-sampled stars), per-star σ from the fit; the detector is threshold-free (its adaptive ladder targets `maxStars`), so there is no detection sigma."
- Modify: `CLAUDE.md` — append one sentence to the end of the **Module Map** paragraph that begins `**\`athenaeum-core\` (\`crates/athenaeum-core/src/\`)**` (after its final period): ` The stacking pipeline (spec \`docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md\`) lives in \`stacking/\` — \`measure\`/\`weights\` (frame quality, Plan 2), \`register\` (registration v2, Plan 3); dev probes \`examples/measure_probe.rs\` and \`examples/register_probe.rs\`.`

**Interfaces:**
- Consumes: `athenaeum_core::stacking::register::{RegistrationConfig, ModelChoice, DistortionChoice}`, `::detect::{detect_stars, luminance, Star}`, `::frame::{reference_stars, register_frame, read_luminance is pub(crate) — the example re-reads through PlaneReader itself}`, `::writer::{source_cards_from_file, build_registered_cards, write_registered_frame, RegisteredCards}`, `::align::model_name`; `athenaeum_core::integration::plane_reader::PlaneReader`; `athenaeum_core::resample::{warp_rows, Plane}`; `astroimage::ImageConverter::read_raw(path) -> Result<(ImageMetadata { width, height, channels, flip_vertical, .. }, PixelData::{Uint16(Vec<u16>), Float32(Vec<f32>)})>` (planar).

- [ ] **Step 1: Write the example**

```rust
//! Dev probe for Checkpoint A: register calibrated subjects onto a
//! calibrated reference and print the alignment as JSON; optionally compare
//! with a WBPP `.xdrz` sidecar, write the registered frame, or compare our
//! resampled frame with a WBPP registered image.
//!
//! cargo run --release -p athenaeum-core --example register_probe -- \
//!   <reference.fits> <subject.fits> [--xdrz <subject.xdrz>] [--write <dir>] \
//!   [--compare <wbpp_registered.xisf>] [--model auto|similarity|affine|homography] \
//!   [--distortion off|polynomial2|polynomial3|polynomial4|auto]

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use athenaeum_core::geometry::PixelMap;
use athenaeum_core::integration::plane_reader::PlaneReader;
use athenaeum_core::resample::{warp_rows, Plane};
use athenaeum_core::stacking::register::align::model_name;
use athenaeum_core::stacking::register::detect::{detect_stars, luminance, Star};
use athenaeum_core::stacking::register::frame::{reference_stars, register_frame};
use athenaeum_core::stacking::register::writer::{build_registered_cards, source_cards_from_file, write_registered_frame, RegisteredCards};
use athenaeum_core::stacking::register::{DistortionChoice, ModelChoice, RegistrationConfig};

fn usage() -> ! {
    eprintln!("usage: register_probe <reference.fits> <subject.fits> [--xdrz f] [--write dir] [--compare f.xisf] [--model m] [--distortion d]");
    std::process::exit(2);
}

struct Args {
    reference: PathBuf,
    subject: PathBuf,
    xdrz: Option<PathBuf>,
    write: Option<PathBuf>,
    compare: Option<PathBuf>,
    cfg: RegistrationConfig,
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let reference = PathBuf::from(it.next().unwrap_or_else(|| usage()));
    let subject = PathBuf::from(it.next().unwrap_or_else(|| usage()));
    let mut a = Args { reference, subject, xdrz: None, write: None, compare: None, cfg: RegistrationConfig::default() };
    while let Some(flag) = it.next() {
        let value = it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--xdrz" => a.xdrz = Some(value.into()),
            "--write" => a.write = Some(value.into()),
            "--compare" => a.compare = Some(value.into()),
            "--model" => a.cfg.model = serde_json::from_value(serde_json::Value::String(value)).unwrap_or_else(|_| usage()),
            "--distortion" => a.cfg.distortion = serde_json::from_value(serde_json::Value::String(value)).unwrap_or_else(|_| usage()),
            _ => usage(),
        }
    }
    a
}

/// `<AlignmentMatrix>` (9 comma-separated floats, row-major, reference →
/// target in the sidecar's coordinates) and `<AlignmentOrigin x y>`.
fn parse_xdrz(path: &Path) -> Result<([[f64; 3]; 3], (f64, f64)), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let between = |open: &str, close: &str| -> Option<String> {
        let s = text.find(open)? + open.len();
        let e = text[s..].find(close)? + s;
        Some(text[s..e].to_string())
    };
    let m = between("<AlignmentMatrix>", "</AlignmentMatrix>").ok_or("no AlignmentMatrix")?;
    let v: Vec<f64> = m.split(',').map(|t| t.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    if v.len() != 9 {
        return Err(format!("AlignmentMatrix has {} values", v.len()));
    }
    let matrix = [[v[0], v[1], v[2]], [v[3], v[4], v[5]], [v[6], v[7], v[8]]];
    let attr = |name: &str| -> Option<f64> {
        let tag = between("<AlignmentOrigin", "/>")?;
        let key = format!("{name}=\"");
        let s = tag.find(&key)? + key.len();
        let e = tag[s..].find('"')? + s;
        tag[s..e].parse().ok()
    };
    Ok((matrix, (attr("x").unwrap_or(0.5), attr("y").unwrap_or(0.5))))
}

fn apply3(m: &[[f64; 3]; 3], x: f64, y: f64) -> (f64, f64) {
    let w = m[2][0] * x + m[2][1] * y + m[2][2];
    ((m[0][0] * x + m[0][1] * y + m[0][2]) / w, (m[1][0] * x + m[1][1] * y + m[1][2]) / w)
}

/// Max corner/centre disagreement between our reference→subject inverse
/// and the sidecar matrix, under one row-order hypothesis: `flip` mirrors
/// y (`y' = H − 1 − y`) on both sides before the sidecar's origin shift.
fn corner_delta(map: &PixelMap, theirs: &[[f64; 3]; 3], origin: (f64, f64), ref_wh: (usize, usize), sub_h: usize, flip: bool) -> f64 {
    let (w, h) = (ref_wh.0 as f64, ref_wh.1 as f64);
    let pts = [(0.0, 0.0), (w - 1.0, 0.0), (0.0, h - 1.0), (w - 1.0, h - 1.0), (w / 2.0, h / 2.0)];
    let mut worst = 0.0f64;
    for (x, y) in pts {
        let (ox, oy) = map.inverse(x, y);
        let (ty, oy_t) = if flip { (h - 1.0 - y, sub_h as f64 - 1.0 - oy) } else { (y, oy) };
        let (px, py) = apply3(theirs, x + origin.0, ty + origin.1);
        let (px, py) = (px - origin.0, py - origin.1);
        worst = worst.max(((px - ox).powi(2) + (py - oy_t).powi(2)).sqrt());
    }
    worst
}

fn read_luminance_any(path: &Path) -> Result<(Vec<f32>, usize, usize), String> {
    if path.extension().map(|e| e.eq_ignore_ascii_case("fits") || e.eq_ignore_ascii_case("fit")).unwrap_or(false) {
        let r = PlaneReader::open(path).map_err(|e| e.to_string())?;
        let planes: Vec<Vec<f32>> = (0..r.channels()).map(|p| r.read_plane(p)).collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        return Ok((luminance(&refs), r.width(), r.height()));
    }
    let (meta, pixels) = astroimage::ImageConverter::read_raw(path).map_err(|e| e.to_string())?;
    let data: Vec<f32> = match pixels {
        astroimage::PixelData::Float32(v) => v,
        astroimage::PixelData::Uint16(v) => v.iter().map(|&u| u as f32 / 65535.0).collect(),
    };
    let n = meta.width * meta.height;
    let planes: Vec<&[f32]> = (0..meta.channels).map(|c| &data[c * n..(c + 1) * n]).collect();
    Ok((luminance(&planes), meta.width, meta.height))
}

fn main() {
    let args = parse_args();
    let reference = reference_stars(&args.reference, &args.cfg, None).unwrap_or_else(|e| {
        eprintln!("reference: {e}");
        std::process::exit(1);
    });
    let reg = register_frame(&reference, &args.subject, &args.cfg, None, &AtomicBool::new(false)).unwrap_or_else(|e| {
        eprintln!("subject: {e}");
        std::process::exit(1);
    });
    let mut out = serde_json::json!({
        "reference": args.reference.display().to_string(),
        "subject": args.subject.display().to_string(),
        "referenceStars": reference.stars.len(),
        "detections": reg.detections,
        "durationMs": reg.duration_ms,
    });
    let a = match &reg.outcome {
        Ok(a) => a,
        Err(e) => {
            out["status"] = serde_json::json!("failed");
            out["error"] = serde_json::json!(e.to_string());
            println!("{}", serde_json::to_string_pretty(&out).unwrap());
            std::process::exit(1);
        }
    };
    out["status"] = serde_json::json!(if a.flipped { "aligned_flipped" } else { "aligned" });
    out["model"] = serde_json::json!(model_name(a.model, a.distortion_order));
    out["seedMatches"] = serde_json::json!(a.seed_matches);
    out["pairs"] = serde_json::json!(a.pairs);
    out["inliers"] = serde_json::json!(a.inliers);
    out["inlierRatio"] = serde_json::json!(a.inlier_ratio);
    out["rmsPx"] = serde_json::json!(a.rms_px);
    out["sigmaRmsPx"] = serde_json::json!(a.sigma_rms_px);
    out["peakPx"] = serde_json::json!([a.peak_px.0, a.peak_px.1]);
    out["scale"] = serde_json::json!(a.scale);
    out["rotationDeg"] = serde_json::json!(a.rotation_deg);
    out["translation"] = serde_json::json!([a.translation.0, a.translation.1]);
    out["quality"] = serde_json::json!({ "score": a.quality_score, "overlap": a.overlap, "regularity": a.regularity });
    out["warnings"] = serde_json::json!(a.warnings);
    out["linear"] = serde_json::json!(a.map.linear.m);
    out["linearInv"] = serde_json::json!(a.map.linear_inv.m);

    if let Some(x) = &args.xdrz {
        match parse_xdrz(x) {
            Ok((theirs, origin)) => {
                let plain = corner_delta(&a.map, &theirs, origin, (reference.width, reference.height), reg.height, false);
                let flipped = corner_delta(&a.map, &theirs, origin, (reference.width, reference.height), reg.height, true);
                let (sx, sy) = ((theirs[0][0].powi(2) + theirs[1][0].powi(2)).sqrt(), (theirs[0][1].powi(2) + theirs[1][1].powi(2)).sqrt());
                out["xdrz"] = serde_json::json!({
                    "matrix": theirs,
                    "origin": [origin.0, origin.1],
                    "theirScale": (sx * sy).sqrt(),
                    "theirRotationDeg": theirs[1][0].atan2(theirs[0][0]).to_degrees(),
                    "cornerDeltaPx": { "sameRowOrder": plain, "flippedRowOrder": flipped },
                });
            }
            Err(e) => out["xdrz"] = serde_json::json!({ "error": e }),
        }
    }

    if let Some(dir) = &args.write {
        let stem = args.subject.file_stem().and_then(|s| s.to_str()).unwrap_or("subject");
        let target = dir.join(format!("r_{}.fits", stem.trim_start_matches("c_")));
        let result = source_cards_from_file(&args.subject).and_then(|src| {
            let json = a.map.to_json();
            let ref_name = args.reference.file_stem().and_then(|s| s.to_str()).unwrap_or("reference");
            let cards = build_registered_cards(&src, &RegisteredCards {
                reference_name: ref_name,
                model: &model_name(a.model, a.distortion_order),
                transform_json: &json,
                interpolation: args.cfg.interpolation,
                clamping: args.cfg.clamping_threshold,
                rms_px: a.rms_px,
            })?;
            write_registered_frame(&args.subject, &a.map, reference.width, reference.height, args.cfg.interpolation, args.cfg.clamping_threshold, &cards, &target)
        });
        out["written"] = match result {
            Ok(ch) => serde_json::json!({ "path": target.display().to_string(), "channels": ch }),
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
    }

    if let Some(theirs) = &args.compare {
        match read_luminance_any(theirs) {
            Ok((tl, tw, th)) => {
                // Our resampled luminance of the subject in reference geometry.
                let ours: Result<(Vec<f32>, usize, usize), String> = read_luminance_any(&args.subject).map(|(sl, sw, sh)| {
                    let mut o = vec![f32::NAN; reference.width * reference.height];
                    warp_rows(&Plane::full(&sl, sw, sh), &a.map, reference.width, 0, reference.height, args.cfg.interpolation, args.cfg.clamping_threshold, &mut o);
                    (o, reference.width, reference.height)
                });
                match ours {
                    Ok((ol, ow, oh)) if (ow, oh) == (tw, th) => {
                        let stars_o: Vec<Star> = detect_stars(&ol, ow, oh, &args.cfg.detection, 500, None);
                        let stars_t: Vec<Star> = detect_stars(&tl, tw, th, &args.cfg.detection, 500, None);
                        let mut dx = Vec::new();
                        let mut dy = Vec::new();
                        let mut ratio = Vec::new();
                        for s in &stars_o {
                            // Try both row orders for the WBPP image.
                            let cand = stars_t.iter().min_by(|p, q| {
                                let d = |t: &Star| ((t.x - s.x).powi(2) + (t.y - s.y).powi(2)).min((t.x - s.x).powi(2) + ((th as f64 - 1.0 - t.y) - s.y).powi(2));
                                d(p).total_cmp(&d(q))
                            });
                            if let Some(t) = cand {
                                let same = ((t.x - s.x).powi(2) + (t.y - s.y).powi(2)).sqrt();
                                let flip = ((t.x - s.x).powi(2) + ((th as f64 - 1.0 - t.y) - s.y).powi(2)).sqrt();
                                let (d, ty) = if same <= flip { (same, t.y) } else { (flip, th as f64 - 1.0 - t.y) };
                                if d < 1.5 {
                                    dx.push(t.x - s.x);
                                    dy.push(ty - s.y);
                                    if t.flux > 0.0 && s.flux > 0.0 {
                                        ratio.push(s.flux / t.flux);
                                    }
                                }
                            }
                        }
                        let med = |v: &mut Vec<f64>| { v.sort_by(|a, b| a.total_cmp(b)); if v.is_empty() { f64::NAN } else { v[v.len() / 2] } };
                        out["compare"] = serde_json::json!({
                            "ourStars": stars_o.len(), "theirStars": stars_t.len(), "matched": dx.len(),
                            "medianDxPx": med(&mut dx), "medianDyPx": med(&mut dy), "medianFluxRatioOursOverTheirs": med(&mut ratio),
                        });
                    }
                    Ok((_, ow, oh)) => out["compare"] = serde_json::json!({ "error": format!("geometry mismatch: ours {ow}x{oh}, theirs {tw}x{th}") }),
                    Err(e) => out["compare"] = serde_json::json!({ "error": e }),
                }
            }
            Err(e) => out["compare"] = serde_json::json!({ "error": e }),
        }
    }

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
```

Facts the example relies on (verified while writing this plan): `warp_rows` takes `map: &dyn InverseMap`, so `&a.map` coerces; `PixelMap`'s `linear`/`linear_inv` fields and `Linear::m` are public; `astroimage` re-exports `ImageConverter` and `PixelData` at its crate root (`rustafits/src/lib.rs`).

- [ ] **Step 2: Build and smoke it on synthetic data**

Run: `cargo build -p athenaeum-core --example register_probe` → builds.
Run: `cargo run -p athenaeum-core --example register_probe; echo "exit=$?"` → the usage line, `exit=2`.
Smoke: write two synthetic FITS with a throwaway test (or reuse the `frame.rs` fixture through `cargo test -- --nocapture` is not needed): run the probe on `crates/athenaeum-core/src/stacking/register/frame.rs`'s fixture by adding a temporary `#[test] #[ignore]` that writes `/tmp/…/reference.fits` + `subject.fits` — or simply run the probe on any two real calibrated FITS the machine has (the LDN 1272 export, see Task 7) and confirm a JSON with `"status": "aligned"` prints. Remove any temporary test before committing.

- [ ] **Step 3: Make the doc edits** exactly as specified above.

- [ ] **Step 4: Full gate**

Run: `cargo test -p athenaeum-core` → everything green (lib tests ≥ 1853 + the new ones; 12 ignored unchanged); `cargo test -p athenaeum-core --test ts_contract` → passes; `cargo check -p athenaeum-core --no-default-features` → clean; `cargo check --workspace --all-targets` → no new warnings from `athenaeum-core`; `rustfmt crates/athenaeum-core/examples/register_probe.rs`.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/examples/register_probe.rs crates/athenaeum-core/Cargo.toml CLAUDE.md docs/superpowers/specs/2026-07-03-logging-overhaul-design.md
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "chore(stacking): register_probe example and registration dictionary rows" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

---

### Task 7 (controller-run): Checkpoint A on LDN 1272

**Files:**
- Create: `docs/superpowers/research/2026-09-09-checkpoint-a-registration.md`

**Inputs:** `ATH="/Volumes/BigMac/Users/astrobureau/Pictures/Calibration Test/LDN1272-WBPP/LDN1272-ATH/LDN 1272"`, `OUT="/Volumes/BigMac/Users/astrobureau/Pictures/Calibration Test/LDN1272-Output"`; reference `$ATH/camera_atr2600m/lights/c_2025-10-18_02-02-02__-9.90_180.00s_0073.fits`; sidecars under `$OUT/registered/Light_BIN-1_6224x4168_EXPOSURE-180.00s_FILTER-NoFilter_mono/<stem>_c_r.xdrz` and `…_6248x4176_…_RGB/<stem>_c_d_r.xdrz`; WBPP registered images `<stem>_c_r.xisf`; the log `$OUT/logs/20260908121336.log` (`* Loading target file: …<stem>_c.xisf` followed by `delta_RMS`).

- [ ] **Step 1: Build the probe in release mode** — `cargo build --release -p athenaeum-core --example register_probe`.

- [ ] **Step 2: Mono frames** — for 6 mono subjects spread over the night (the first, the last and four in between by filename order), run

```bash
target/release/examples/register_probe "$REF" "$ATH/camera_atr2600m/lights/c_<stem>.fits" --xdrz "$OUT/registered/…_mono/<stem>_c_r.xdrz" > /tmp/cpA-mono-<n>.json
```

and record per frame: our `inliers`, `rmsPx`, `scale`, `rotationDeg`, `translation`, `model`; the sidecar's scale/rotation; `cornerDeltaPx` under both row-order hypotheses (the smaller one is the comparison; note which); WBPP's `delta_RMS` for the same stem from the log.

- [ ] **Step 3: OSC frames** — the same for 4 OSC subjects (`camera_zwoasi2600mcduo/lights/`, sidecars in the `…_RGB` directory) with `--distortion auto` (cross-geometry → polynomial 3 when ≥ 200 inliers) and once more with `--distortion off`.

- [ ] **Step 4: Pixel-level check** — one mono subject: `--write /tmp/cpA-write --compare "$OUT/registered/…_mono/<stem>_c_r.xisf"`; record `matched`, `medianDxPx`, `medianDyPx`, `medianFluxRatioOursOverTheirs`.

- [ ] **Step 5: Timing** — wall time of one mono registration (`durationMs`) and of the reference detection; extrapolate to 368 frames single-threaded.

- [ ] **Step 6: Write the note** with the tables, the spec §13 targets (translation ≤ 0.05 px, rotation ≤ 0.001°, scale ≤ 1e-4 against the sidecar; our RMS ≤ WBPP's `delta_RMS` per frame), what passed, what did not, the row-order finding, and the carry-forwards for Plan 4/5. Commit:

```bash
git add docs/superpowers/research/2026-09-09-checkpoint-a-registration.md
git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit -m "docs(stacking): checkpoint A — registration v2 against the WBPP sidecars" -m "Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>" -m "Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY"
```

A target missed by a wide margin is a defect to fix in this plan (a fix round on the responsible task) before Plan 4 starts; a near miss is recorded with its cause and a ruling.

---

## Self-review (done while writing)

**Spec coverage.** §3.1 detection on calibrated planes with the Moffat-refined
centroids, the three cuts, the 2000-brightest cap, luminance for RGB (Task 2).
§3.2 quad seed → KD-tree correspondences at 2·tol → RANSAC with the spec's
tolerance/iterations/min-inliers → σ-weighted refit with 3σ clipping →
optional distortion (Task 3). §3.3 the four linear/polynomial models and the
`auto` rules (Task 3; TPS is M4). §3.4 reference geometry + NaN coverage
(Task 5's writer through `warp_rows`; the lazy path is Plan 1's
`RegisteredSource`). §3.5 kernels/clamping are Plan 1's; the config exposes
them (Task 2). §3.6 QA fields and the three failure rules (Task 3),
recorded per frame (Task 4). §3.7 registered frames with `ATH_REG` cards
(Task 5). §9.1 the nine columns and `transform_json = PixelMap::to_json()`
(Tasks 1, 4). §9.2 names/defaults with `#[serde(default)]` (Task 2). §6.4's
scanner rule for `ATH_REG`/`ATH_STK` (Task 5). §13 Checkpoint A (Task 7).
Not here by design: set-level orchestration, events, the `registration`
span, retiring the old commands, `excludeOnRegistrationFailure` (Plan 5).

**Placeholder scan.** The scanner test body and the Task 6 smoke are the only
steps that lean on neighbouring code the implementer copies; both name the
exact neighbour. No TBD/TODO.

**Type consistency.** `Star { x, y, flux, sigma }` (Task 2) is what `align`
(Task 3), `frame` (Task 4) and the probe (Task 6) consume; `Alignment`'s
fields (Task 3) are what `to_record` (Task 4) and the probe read
(`quality_score`/`overlap`/`regularity` rather than a `Quality` value, so no
derive is assumed on Plan 1's struct); `RegistrationRecord`'s nine fields
(Task 1) are exactly the ones `to_record` sets; `RegisteredCards` (Task 5)
matches the probe's literal; `model_name` (Task 3) is used by Tasks 4–6;
`RegistrationConfig::default()` values match the serde test and the spec.

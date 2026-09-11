# Stacking M4b — Mixed Pixel Scales Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One frame set may hold groups (or members of one group) shot at different pixel scales — a bin-2 group, a second telescope, another camera — and the pipeline integrates all of them (spec §15, owner requirement 2026-09-09) in one of two modes the owner picks per set: **co-registered** (every group resampled into the set reference's geometry, one master per group in one geometry) or **native** (a per-group reference, one master per group in its own geometry). Today a foreign-scale frame is refused one at a time by registration's `[0.8, 1.25]` scale gate and the plan gate says nothing.

**Architecture:** Three changes, each behind the group discovery that already exists. (1) Every `GroupFrame` learns its pixel scale — the stored plate solve's `pixel_scale_arcsec` when the frame is solved, else the header-implied `206.2648 · XPIXSZ / FOCALLEN` (`XPIXSZ` is the effective, already-binned pixel size in every capture program this catalog has seen — ruling R-M4b-1) — and the plan gate turns a scale spread into a named WARNING (never a blocker). (2) Registration's scale gate becomes per frame: the allowed range is centred on the frame's implied ratio to its reference (`r = scale_frame / scale_ref`, range `[r/1.25, r·1.25]`), and when both frames carry a plate solve the alignment is SEEDED from the two WCS solutions (subject pixel → sky → reference pixel over a grid, an affine fit) the way a coordinate-based aligner works — the owner's hint: the external tool's star aligner cannot cross pixel scales either, its users go through its coordinate-based aligner — and only refined with star pairs; quad matching (scale-invariant by construction) stays the seed when a solve is missing. (3) `native` mode gives each group its own reference (the group's best-by-weight member, two-pass re-picked per M4a) and its own geometry for LN, integration, drizzle, the master's WCS and the `.rej` bitmaps; `coRegistered` (the default, today's behaviour generalized) keeps the ONE run-wide reference. Nothing about group keys changes (camera-agnostic since M2 Task 10; binning stays a key).

**Tech Stack:** Rust (athenaeum-core `stacking/{groups,plan,run,register/*,master_cards,provenance}.rs`, `plate_solve/storage.rs`, `fits_writer/wcs.rs`), React/TS (`RegisterPanel.tsx`, `GroupsTable.tsx`, `stageSummary.ts`, `ResultsPanel.tsx`), ts-rs regeneration, no new dependencies. The solver crate's `WcsSolution` (`solvemyastro::wcs`, linear TAN + SIP forward/reverse) is the only WCS evaluator — no second projection implementation.

**Spec:** `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` §2 (grouping keys), §3.1–3.6 (registration, the scale gate), §9.2 (`registration.*`), §15 ("Mixed pixel scales in one set") · **Math:** `docs/superpowers/research/2026-09-08-stacking-math-reference.md` §5.2–5.3 (matching, transformation models) · **Prior rulings:** M3 R-M3-13 (`PlateSolveRecord.crpix` is 0-based; the card is `crpix + 1`), M3 R-M3-17 (normalization anchor is per group already), M4a R-M4a-5/6 (two-pass reference), R-M4a-7 (the pixel-scale warning is THIS plan's Task 1).

## Global Constraints

- No new crate dependencies; `tracing` only; never name other software in code or comments.
- Two backends in sync: no new commands are expected; if one is added, Tauri + Axum + `invoke_handler` + `build_router` + `ts_export.rs` in the same task.
- Headless build: `cargo check -p athenaeum-core --no-default-features` must stay clean — `stacking/` is gated `#[cfg(all(feature = "render", feature = "solver"))]`; the new `PlateSolveRecord::to_solution` lives in `plate_solve/storage.rs`, which is itself gated `solver` — verify with the headless check before assuming.
- New log field names go into the "Unified event schema" dictionary of `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md` in the same task.
- rustfmt only on new leaf files and rustfmt-clean files (`rustfmt --check` first); never `run.rs`, `plan.rs`, `groups.rs`, `align.rs`, `mod.rs`, `ts_export.rs`, `schema.rs`.
- Serde names are spec §9.2's verbatim; new config fields are `#[serde(default)]`; no `STACKING_CONFIG_VERSION` bump.
- Every existing registration test keeps passing unchanged unless it pinned the fixed `[0.8, 1.25]` gate — those update to the per-frame gate with a message line saying so.
- Coordinate convention (the pipeline's): integer pixel coordinates are pixel CENTRES; `PixelMap::forward` maps subject → reference; `PlateSolveRecord.crpix` is 0-based; `WcsSolution.crpix` is 0-based too (its doc says so) — no ±1 anywhere between them.
- Commit as `git -c user.name=eg013ra1n -c user.email=vilen.sharifov@gmail.com commit …` with the `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01DjyxL6wsv1HT5GiXxedbAY` trailers.

## Rulings made while planning (binding unless the spec says otherwise)

- **R-M4b-1 Scale per frame, two sources, one field.** `GroupFrame.pixel_scale_arcsec: Option<f64>` and `GroupFrame.scale_source: ScaleSource { Solve, Header }` (`None` when neither exists). Solve wins over header (a solve measures, a header assumes). Header formula: `206.2648 · xpixsz_um / focallen_mm` — NO binning factor: `XPIXSZ` is written by every capture program in this catalog as the EFFECTIVE pixel size after binning (verified 2026-09-11 on the owner's catalog: the ASI294MM bin-2 lights carry `XPIXSZ = 4.63 = 2 × 2.315`, the ASI6200MM bin-2 lights `7.52 = 2 × 3.76`, both with `XBINNING = 2`; multiplying by the binning would double-count and report ×2 the true scale). `None` when `xpixsz` or `focallen` is missing, non-finite or ≤ 0. `xbinning` is still read (the group key and the display need it) but never enters the scale.
- **R-M4b-2 The gate is per frame and centred on the implied ratio.** `r = scale_frame / scale_reference` when both are known, else `1.0`; the accepted refit scale is `[r / 1.25, r · 1.25]` (`SCALE_TOLERANCE = 1.25`, the existing constant's half-width kept). A frame whose refit scale falls outside is refused with `ScaleOutOfRange { scale, expected: r }` (the message names both). Nothing else about registration's success criteria changes.
- **R-M4b-3 WCS seed when both frames are solved.** `seed_from_solves` maps a 5×5 grid of subject pixel centres (inset 5 % from each edge) through the subject's `WcsSolution::pixel_to_sky` and the reference's `sky_to_pixel`, least-squares-fits an affine, and returns it when every mapped point is finite and the affine's scale is within `[0.05, 20]`. `align` pairs through the hint with radius `WCS_SEED_RADIUS_PX = max(4 · ransac_tolerance_px, 8.0)` (a stored solve's own residual is ≤ 1 px; a bad solve pairs nothing); fewer than `MIN_INLIERS` pairs → the hint is discarded with a warning `wcs seed rejected (<n> pairs); quad seed used` and the quad seed runs as today. `Alignment.seed: SeedKind { Wcs, Quads }` records which one shipped; `registration_results` does not change shape — the seed kind rides `transform_json`'s sibling `model` string as a `+wcs` suffix (e.g. `homography+polynomial3+wcs`) so the frames table can show it without a column.
- **R-M4b-4 Two modes, one config field.** `registration.geometry: "coRegistered" | "native"` (`RegistrationConfig.geometry: RegistrationGeometry`, default `CoRegistered`). Co-registered = the run-wide reference (spec §4.4, two-pass per M4a); native = per group: the group's `best_by_weight` member (a manual reference applies to its own group only; every other group picks auto), two-pass re-pick per group. The run-level `stacking_runs.reference_frame_id` stays the largest group's reference in both modes (it is what the plan gate and the results header show); `SummaryGroup.reference_frame_id: Option<i64>` (`#[serde(default)]`) carries each group's own.
- **R-M4b-5 Geometry follows the reference.** `RunContext` grows `group_geometry: HashMap<String, GroupGeometry { reference_frame_id, width, height, calibrated: PathBuf, hash: String }>`; every consumer of `rc.reference_width/height` (the register admission bound, the LN driver, `process_group_output`'s integration and drizzle geometry, the coverage filter, the master's WCS lookup) reads `rc.geometry_of(&group.key)`. In co-registered mode every entry equals the run-wide one — the M1–M4a byte-identical pins therefore keep passing with the default config.
- **R-M4b-6 Level and resolution across scales are the resampler's business, not new code.** A coarse frame registered onto a finer reference is up-sampled by `RegisteredSource`'s existing inverse-mapped gather (bicubic B-spline by default) at the frame's OWN level (normalization runs after resampling, per pixel, as today); drizzle's forward-mapped drops map a coarse source pixel onto a `r × r` output-pixel quad through the same `PixelMap` — the exact clipping already handles any quad, and `I / W` stays level-preserving (Task 4 pins both). A finer frame onto a coarser reference is down-sampled by the same gather (aliasing accepted — the spec says "the finer scale loses resolution"; a pre-filter is M4c's interpolating-prefilter item).
- **R-M4b-7 Warnings, never blockers.** The plan gate emits at most one warning per group: a group whose scale is outside `[0.8, 1.25]` of the reference's, and a group whose members spread beyond ×1.25. Wording in Task 1. A group with NO known scale on any member gets no warning (nothing to compare).
- **R-M4b-8 Master header.** `ATH_RGEO = 'coRegistered' | 'native'` on every master (and drizzled master); in native mode the master's WCS is the GROUP reference's solve; `bin<n>` in the filename stays the group's own binning in both modes (M11) — the WCS says the delivered scale.
- **R-M4b-9 Acceptance data — REAL sets from the owner's catalog (owner suggestion 2026-09-11, replacing the synthetic bin-2 recipe).** The dev catalog holds 16 frame sets shot at more than one pixel scale (`find-mixed-fov.py`/`mixed-fov-detail.py` in the session scratchpad). Task 6 uses three of them, each a different case: (a) **set 195 "Ghost Nebula QHY"** — the WITHIN-GROUP case: one camera (QHY268M, 6252×4176, bin 1), two focal lengths 352 mm (2023: H 50, O 61, S 67 frames at 300 s) and 448 mm (2025: H 68, O 60, S 60 at 300 s), scale ratio ×1.27 — just outside today's `[0.8, 1.25]` gate, so every narrowband group (`mono__H__bin1__300s` etc.) currently drops one of its two subsets; (b) **set 108 "M 78"** — the CROSS-GROUP binning case: ASI294MM at 1000 mm, L 120 s at bin 1 (33 frames, 8288×5644, 0.48 "/px) and bin 2 (549 frames, 4144×2822, 0.96 "/px) → two groups `mono__L__bin1__120s` / `mono__L__bin2__120s`, ratio ×2.0, the reference in the bin-2 group; (c) **set 138 ("Unknown", RA 5h? — the 2023-01 QHY268M pair)** — the CROSS-GROUP large-FOV case: the same camera at 1000 mm (R/G/B 60 s, 25–37 frames, 0.78 "/px) and 352 mm (R/G/B 20 s, 16–20 frames, 2.20 "/px), ratio ×2.82 with a ×8 field-area difference — the case the quad seed is expected to fail on and the WCS seed to carry. None of these frames has a plate solve yet: Task 6 first solves the chosen frames through the app's own solve queue (the Analysis tab / `plate_solve` commands) — solves are the WCS seeds AND the ground-truth scales for the plan-time warning — then runs each set twice (co-registered, native). Optional fourth: **set 100 "M 33"** (four cameras, OSC + mono, 0.78–1.9 "/px) as the everything-at-once smoke, results recorded, no targets.

---

## File structure

- Modify `crates/athenaeum-core/src/stacking/groups.rs` — `LightMember.{focallen, xpixsz}`, `GroupFrame.{pixel_scale_arcsec, scale_source}`, `IntegrationGroup.pixel_scale_arcsec` (median over members with a scale), `ScaleSource`.
- Modify `crates/athenaeum-core/src/stacking/plan.rs` — the per-frame solve-scale join, the two warnings, `PlanGroup.{pixel_scale_arcsec, scale_ratio_to_reference}`.
- Modify `crates/athenaeum-core/src/plate_solve/storage.rs` — `PlateSolveRecord::to_solution(&self) -> Option<solvemyastro::wcs::WcsSolution>`.
- Create `crates/athenaeum-core/src/stacking/register/wcs_seed.rs` — `seed_from_solves`, the grid, the affine fit (reuses `align.rs`'s `fit_affine`/`Linear`).
- Modify `crates/athenaeum-core/src/stacking/register/align.rs` — `align(.., hint: Option<&Linear>, scale_gate: (f64, f64))`, `SeedKind`, the per-frame gate; `register/frame.rs` — `register_frame(.., hint, scale_gate)`; `register/mod.rs` — `RegistrationGeometry`, `RegistrationConfig.geometry`.
- Modify `crates/athenaeum-core/src/stacking/run.rs` — `GroupGeometry`, `geometry_of`, per-group references in native mode, the solve cache for hints, the per-frame gate, `ATH_RGEO`; `stacking/master_cards.rs` — the card; `stacking/provenance.rs` — `SummaryGroup.reference_frame_id`; `db/stacking.rs` if a group-row column is wanted (it is not — the summary JSON carries it).
- Modify `src/components/stacking/panels/RegisterPanel.tsx` (geometry radio), `src/components/stacking/GroupsTable.tsx` (Scale column, `×r` badge), `src/components/stacking/stageSummary.ts`, `src/components/stacking/ResultsPanel.tsx` (per-group reference line in native mode), `src/components/stacking/FramesTable.tsx` (the `+wcs` model suffix renders as-is), `src/types/stacking.ts` (regenerated).
- Modify `docs/superpowers/specs/2026-09-08-stacking-pipeline-design.md` (§3.6 per-frame gate, new §3.8 "Mixed pixel scales", §9.2 `registration.geometry`, §15 entry closed), `docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`, `CLAUDE.md` → Stacking.
- Create `docs/superpowers/research/2026-09-1x-m4b-acceptance-run.md` (Task 6).

---

### Task 1: Pixel scale per frame and the plan-time warning

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/groups.rs` (`LightMember`, `load_group_members`'s SELECT, `GroupFrame`, `to_group_frame`, `IntegrationGroup`, `group_frames`)
- Modify: `crates/athenaeum-core/src/stacking/plan.rs` (`PlanGroup`, `build_plan`'s warnings block right after the missing-`EXPTIME` warning — search `EXPTIME_WARNING_NAME_CAP`)
- Modify: `src/components/stacking/GroupsTable.tsx`, `src/types/stacking.ts` (regenerated), spec §2 (one sentence: the per-frame scale fields), the logging dictionary
- Test: `groups.rs` tests, `plan.rs` tests

**Interfaces:**
- Consumes: `frames.focallen`, `frames.xpixsz`, `frames.xbinning` (all exist in the schema), `plate_solves.pixel_scale_arcsec` via `plate_solve::storage::get_plate_solve`.
- Produces: `pub enum ScaleSource { Solve, Header }` (serde camelCase, ts-rs); `GroupFrame.pixel_scale_arcsec: Option<f64>`, `GroupFrame.scale_source: Option<ScaleSource>`; `IntegrationGroup.pixel_scale_arcsec: Option<f64>`; `pub fn header_pixel_scale_arcsec(xpixsz_um: Option<f64>, focallen_mm: Option<f64>) -> Option<f64>` (groups.rs; R-M4b-1 — no binning factor); `PlanGroup.pixel_scale_arcsec: Option<f64>`, `PlanGroup.scale_ratio_to_reference: Option<f64>`; `pub const SCALE_TOLERANCE: f64 = 1.25` moves from `align.rs`'s `SCALE_RANGE` tuple to a shared `stacking::register::SCALE_TOLERANCE` (align keeps `SCALE_RANGE = (1.0 / SCALE_TOLERANCE, SCALE_TOLERANCE)` for its default gate until Task 2 replaces it).

- [ ] **Step 1: Failing tests**

`groups.rs`: `header_pixel_scale_arcsec(Some(3.76), Some(1000.0))` ≈ 0.7755 (`206.2648 · 3.76 / 1000`); `(Some(4.63), Some(1000.0))` ≈ 0.9550 (an ASI294MM bin-2 light — `XPIXSZ` already effective, R-M4b-1); any `None`/zero/negative factor → `None`. The signature is `(xpixsz_um: Option<f64>, focallen_mm: Option<f64>)` — no binning parameter. `group_frames` with two members whose `frames` rows carry `focallen`/`xpixsz` → `pixel_scale_arcsec` set with `scale_source = Header`; a member with a `plate_solves` row (`pixel_scale_arcsec = 0.80`) → `0.80`/`Solve` even though the header implies 0.7755. The group's `pixel_scale_arcsec` is the median of the members that have one.

`plan.rs`: a set whose reference group is at 0.78 "/px and a second group at 1.55 "/px → exactly one warning containing `×2.0` and the group key; a group whose members span 0.78–1.55 → one warning containing `mixes pixel scales`; a set where no frame has any scale → no scale warning; no blocker in any of the three.

Run: `cargo test -p athenaeum-core stacking::groups::tests::header_pixel_scale stacking::plan::tests::scale_warning` → FAIL.

- [ ] **Step 2: Implement**

`load_group_members`: add `f.focallen, f.xpixsz` to the SELECT (indices 14, 15). `to_group_frame`: `pixel_scale_arcsec = header_pixel_scale_arcsec(m.xpixsz, m.focallen)`, `scale_source = Some(Header)` when `Some`. In `group_frames` (or the caller in `plan.rs::build_plan` — wherever the `Connection` is at hand; `groups.rs` already has it in `load_group_members`), one query `SELECT frame_id, pixel_scale_arcsec FROM plate_solves WHERE frame_id IN (…)` over the set's light frame ids (chunk the `IN` list by 500) overrides the header value with `Solve`. `IntegrationGroup.pixel_scale_arcsec` = median. `build_plan`: `reference_scale` = the plan's reference frame's own `pixel_scale_arcsec` (the resolved reference — Manual or best-by-weight, whichever `build_plan` already reports), then per group: `ratio = group.pixel_scale_arcsec / reference_scale` → `PlanGroup.scale_ratio_to_reference`; warning when `ratio > SCALE_TOLERANCE || ratio < 1.0 / SCALE_TOLERANCE`:

```text
group `<key>` is at <scale:.2> "/px — ×<ratio:.1> the reference's <ref:.2> "/px; co-registered mode resamples it into the reference geometry, native mode keeps its own
```

and per group when `max/min` over members' scales `> SCALE_TOLERANCE`:

```text
group `<key>` mixes pixel scales (<min:.2>–<max:.2> "/px); frames outside ×1.25 of the group's reference are registered with a scale factor
```

`tracing::debug!(group_key, pixel_scale_arcsec, scale_ratio, "group pixel scale")` per group on the plan (`pixel_scale_arcsec` (f64), `scale_ratio` (f64) → dictionary). `GroupsTable.tsx`: a `Scale` column after `Bin`: `0.78"` (solve) / `~0.78"` (header-implied, with a title "from the header's focal length and pixel size") / `—`; a `×2.0` badge (`text-warning`) when `scaleRatioToReference` is outside the tolerance. Regenerate `src/types/stacking.ts`.

Run: `cargo test -p athenaeum-core stacking::groups stacking::plan && npx tsc --noEmit` → PASS.

- [ ] **Step 3: Commit** — `feat(stacking): per-frame pixel scale (solve or header) and the plan-time scale warnings (M4b Task 1, rulings R-M4b-1/7)`.

---

### Task 2: WCS-seeded alignment and the per-frame scale gate

**Files:**
- Modify: `crates/athenaeum-core/src/plate_solve/storage.rs` (`PlateSolveRecord::to_solution`)
- Create: `crates/athenaeum-core/src/stacking/register/wcs_seed.rs`
- Modify: `crates/athenaeum-core/src/stacking/register/align.rs` (`align`, `Alignment.seed`, `SeedKind`, `AlignError::ScaleOutOfRange { scale, expected }`), `register/frame.rs` (`register_frame`, `model_name`), `register/mod.rs` (`pub mod wcs_seed`, `SCALE_TOLERANCE`), `registration/db.rs` only if `model` is validated somewhere (it is a free string — check `to_record` in `run.rs`)
- Modify: `crates/athenaeum-core/src/stacking/run.rs` (`stage_register`: the solve cache, per-frame `scale_gate`, the hint), the logging dictionary
- Test: `wcs_seed.rs` tests, `align.rs` tests, `frame.rs` tests, `storage.rs` tests

**Interfaces:**
- Consumes: `solvemyastro::wcs::{WcsSolution, SipCoefficients}` (`sip_forward: Option<(A, B)>`, `sip_reverse: Option<(AP, BP)>`, `coeffs: Vec<Vec<f64>>` — the same `Vec<Vec<f64>>` the record stores as JSON, see `plate_solve/service.rs`'s adapter and `fits_writer::wcs::parse_sip_table`), `align.rs`'s private `fit_affine` (make it `pub(super)`), `Linear::from_flat`, `Linear::scale()`.
- Produces: `impl PlateSolveRecord { pub fn to_solution(&self) -> Option<WcsSolution> }` (`None` when a SIP JSON fails to parse — log a `warn!` with `frame_id` and `error`); `pub fn seed_from_solves(subject: &PlateSolveRecord, reference: &PlateSolveRecord, subject_geometry: (usize, usize)) -> Option<Linear>`; `pub const WCS_SEED_GRID: usize = 5; pub const WCS_SEED_INSET: f64 = 0.05; pub const WCS_SEED_SCALE_RANGE: (f64, f64) = (0.05, 20.0); pub const WCS_SEED_RADIUS_FACTOR: f64 = 4.0; pub const WCS_SEED_RADIUS_MIN_PX: f64 = 8.0;` `pub enum SeedKind { Quads, Wcs }`; `Alignment.seed: SeedKind`; `pub fn align(subject, reference, reference_geometry, subject_geometry, cfg, hint: Option<&Linear>, scale_gate: (f64, f64)) -> Result<Alignment, AlignError>`; `pub fn register_frame(reference, subject, cfg, pool, cancel, hint: Option<&Linear>, scale_gate: (f64, f64))`; `pub fn scale_gate_for(frame_scale: Option<f64>, reference_scale: Option<f64>) -> (f64, f64)` in `register/mod.rs` (`r = frame/reference` when both `Some` and finite and > 0, else 1.0; returns `(r / SCALE_TOLERANCE, r * SCALE_TOLERANCE)`); `model_name` appends `+wcs` when `seed == Wcs`.

- [ ] **Step 1: `to_solution` (failing test first)**

`storage.rs` test: a record with `crpix (1234.5, 678.9)`, `crval (83.82, −5.39)`, `cd [[5.5e-5, 1.1e-7], [−1.1e-7, 5.5e-5]]`, `sip_order Some(2)`, `sip_a_coeffs = serde_json::to_string(&vec![vec![0.0, 0.0, 1e-6], vec![0.0, 2e-6], vec![3e-6]])` etc. → `to_solution()` returns a `WcsSolution` whose `pixel_to_sky(1234.5, 678.9) == (83.82, −5.39)` to 1e-9 and whose `sip_forward.unwrap().0.coeffs[0][2] == 1e-6`; a record with `sip_a_coeffs = Some("not json")` → `None`. Implement: `sip_forward = match (sip_order, a, b) { (Some(o), Some(a), Some(b)) => Some((SipCoefficients { order: o as u8, coeffs: parse(a)? }, …)), _ => None }`, same for reverse.

Run: `cargo test -p athenaeum-core plate_solve::storage::tests::to_solution` → PASS.

- [ ] **Step 2: `seed_from_solves` (failing test first)**

`wcs_seed.rs` test: build two records that encode a known similarity — reference: `crpix (3112, 2084)`, `crval (300.0, 60.0)`, `cd` = scale 0.78"/px rotated 0°; subject: `crpix (1556, 1042)`, same `crval`, `cd` = scale 1.56"/px rotated 3° — geometry (3112, 2084) for the subject; the returned affine maps the subject centre to the reference centre within 0.01 px and has `scale()` ≈ 2.0 within 1e-3 and rotation ≈ 3° within 0.01°; a subject whose `crval` is 5° away (no overlap: the mapped points land at ±30 000 px) still returns `Some` (the seed does not know about overlap — the pairing step decides); a record whose `to_solution` is `None` → `None`.

Implement: the 5×5 grid over `[inset·w, (1−inset)·w] × [inset·h, (1−inset)·h]`, `sub → sky → ref`, `fit_affine` on the 25 pairs (it takes `(sub, ref)` pairs — reuse `Pair` if `fit_affine` is quad-shaped, else a 6-parameter least squares here: normal equations, 3×3 solve), scale check, `Linear::from_flat(LinearKind::Affine, …)`.

Run: `cargo test -p athenaeum-core stacking::register::wcs_seed` → PASS.

- [ ] **Step 3: `align` with a hint and a gate (failing tests first)**

`align.rs` tests (the module's synthetic-field helpers — read the existing `align_*` tests first and reuse their star generator):
(a) reference field 4000×3000, subject = the same stars at HALF the pixel scale (coordinates × 0.5, geometry 2000×1500), rotated 3°, with the default gate `(0.8, 1.25)` and no hint → `Err(ScaleOutOfRange { scale ≈ 2.0, expected: 1.0 })` (the quad seed itself succeeds — pinned by the error being the gate's, not `NoSeed`);
(b) same, `scale_gate = scale_gate_for(Some(1.56), Some(0.78)) = (1.6, 2.5)`, no hint → `Ok`, `scale` within 0.5 % of 2.0, `seed == Quads`;
(c) same, with `hint = Some(&exact similarity)` → `Ok`, `seed == Wcs`, `seed_matches == 0`, inliers ≥ 90 % of the subject stars;
(d) a WRONG hint (the exact one translated by 30 px, beyond `WCS_SEED_RADIUS`) → `Ok` with `seed == Quads` and `warnings` containing `wcs seed rejected`;
(e) the existing tests all pass with `hint = None, scale_gate = SCALE_RANGE`.

Implement in `align`: step 1 becomes `let (seed, seed_matches, seed_kind) = match hint { Some(h) => { let p = pair_through(h, …, radius_wcs); if p.pairs.len() >= MIN_INLIERS { (h.clone(), 0, SeedKind::Wcs) } else { warnings.push(format!("wcs seed rejected ({} pairs); quad seed used", p.pairs.len())); let (s, m) = seed_affine(..)?; (s, m, SeedKind::Quads) } } None => { let (s, m) = seed_affine(..)?; (s, m, SeedKind::Quads) } }`; the gate check uses `scale_gate`; `Alignment { seed: seed_kind, .. }`; `identity_registration` sets `SeedKind::Quads`. `frame.rs`: thread the two parameters; `model_name(model, distortion_order, seed)` appends `+wcs`.

Run: `cargo test -p athenaeum-core stacking::register` → PASS.

- [ ] **Step 4: The run — solve cache, per-frame gate, hint**

`stage_register`: a `HashMap<i64, Option<PlateSolveRecord>>` solve cache filled lazily (`get_plate_solve` on first use; the reference's fetched first); per frame `scale_gate = scale_gate_for(frame.pixel_scale_arcsec, reference_frame.pixel_scale_arcsec)`; `hint = if gate != SCALE_RANGE || cfg.registration.geometry == Native { seed_from_solves(subject_solve?, reference_solve?, (frame.width, frame.height)) } else { None }` — i.e. the WCS seed is only tried when a scale factor is expected or in native mode (same-scale frames keep the exact M1–M4a code path, so the pins hold); log `tracing::debug!(run_id, frame_id, scale_gate_low, scale_gate_high, hint = hint.is_some(), "registration gate")` (`scale_gate_low`/`scale_gate_high` (f64), `hint` (bool) → dictionary). `to_record`'s `model` gets the `+wcs` suffix through `model_name`.

Test (`run.rs`, `RunContext`-driven): a 6-frame set where 3 frames carry `pixel_scale_arcsec = 2×` the reference's and are software-binned copies of the others → after `stage_register` every frame is `Aligned` with `scale` ≈ 2.0 for the binned ones; with the M1 fixed gate (simulate by `scale_source = None` on those frames) they are excluded with `registration failed: scale 2.00 outside [0.80, 1.25] (expected 1.00)`.

Run: `cargo test -p athenaeum-core stacking::run::tests::mixed_scale` → PASS.

- [ ] **Step 5: Commit** — `feat(stacking): WCS-seeded cross-scale alignment and the per-frame scale gate (M4b Task 2, rulings R-M4b-2/3)`.

---

### Task 3: Native geometry mode

**Files:**
- Modify: `crates/athenaeum-core/src/stacking/register/mod.rs` (`RegistrationGeometry`, `RegistrationConfig.geometry`), `crates/athenaeum-core/src/stacking/run.rs` (`GroupGeometry`, `RunContext.group_geometry`, `geometry_of`, `stage_reference`, `stage_register`, the LN driver, `process_group_output`, `stage_output`), `crates/athenaeum-core/src/stacking/plan.rs` (`compute_register_stale` per group; the reference block reports the largest group's), `crates/athenaeum-core/src/stacking/provenance.rs` (`SummaryGroup.reference_frame_id`), `crates/athenaeum-core/src/stacking/master_cards.rs` (`ATH_RGEO`)
- Modify: `src/components/stacking/panels/RegisterPanel.tsx`, `stageSummary.ts`, `ResultsPanel.tsx`, `src/types/stacking.ts` (regenerated), spec §3.8/§9.2, the logging dictionary
- Test: `run.rs` tests, `plan.rs` tests, `master_cards.rs` tests

**Interfaces:**
- Consumes: M4a's `two_pass_pick`; Task 2's `scale_gate_for`/hint plumbing.
- Produces: `pub enum RegistrationGeometry { CoRegistered, Native }` (serde camelCase, default `CoRegistered`, ts-rs); `pub(crate) struct GroupGeometry { reference_frame_id: i64, width: usize, height: usize, calibrated: PathBuf, hash: String }`; `RunContext::geometry_of(&self, key: &str) -> &GroupGeometry` (panics with the key when missing — a programming error, every group is inserted in `stage_reference`); `SummaryGroup.reference_frame_id: Option<i64>`; the `ATH_RGEO` card (string, `coRegistered`/`native`) on every master and drizzled master.

- [ ] **Step 1: Config + card (failing tests first)**

`register/mod.rs`: `geometry: RegistrationGeometry` with `#[serde(default)]`; `defaults_match_the_spec` test extended; config-hash test (two configs differing only in `geometry` hash differently). `master_cards.rs`: `build_master_cards`/`build_drizzle_cards` take `geometry: RegistrationGeometry` and emit `ATH_RGEO`; the card-set pin tests gain the card.

- [ ] **Step 2: Per-group references**

`stage_reference`: after resolving the run-wide reference exactly as today, build `rc.group_geometry`: co-registered → one entry per group, all equal to the run-wide reference (id, calibrated path, hash; width/height filled in `stage_register` after `reference_stars`, as `rc.reference_width/height` are today); native → per group `best_by_weight` over the group's included entries (the Manual reference for its own group), width/height from that frame's measurement. `stage_register`: in native mode the outer loop detects each group's reference stars (`reference_stars` per group, cached in the loop), two-pass per group (M4a's pick over the group's own members — the same function, the group is the candidate set), then the persisting pass; `registration_results.reference_frame_id` = the group's. Every `rc.reference_width/height` read listed in ruling R-M4b-5 becomes `rc.geometry_of(&group.key)` (the run-wide fields stay for the run-level DB row and the results header). `stage_output`: the WCS is fetched per group from `geometry_of(key).reference_frame_id` (co-registered: the same id every time — one warn, not one per group, when it has no solve; keep a `warned` flag).

Tests (`run.rs`): (a) native mode on a 2-group synthetic set → each group's `SummaryGroup.reference_frame_id` is its own best-by-weight member and its master's `NAXIS1/2` equal that member's geometry; (b) co-registered on the same set → both masters have the run-wide reference's geometry and `reference_frame_id` equal to the run's; (c) the M1 pins (`grep -rn "byte-identical\|bit-identical" crates/athenaeum-core/src/stacking/run.rs`) run unchanged with the default config.

- [ ] **Step 3: Stale check, summary, UI, docs**

`plan.rs::compute_register_stale`: in native mode the expected reference per group is the group's best-by-weight from stored metrics (or the last done run's per-group `reference_frame_id` from `summary_json` when two-pass is on — the M4a rule, per group). `RegisterPanel.tsx`: a "Geometry" radio — `Co-registered — every group is resampled into the set reference's geometry (one geometry for all masters)` / `Native — each group keeps its own reference and geometry (no cross-group registration)`. `stageSummary.ts` register row prefixes `native ·` when set. `ResultsPanel.tsx`: per group `reference #<id>` in native mode. Spec: new §3.8 "Mixed pixel scales" (rulings R-M4b-1…8 verbatim), §9.2 `geometry: coRegistered`, §15's entry marked done with the plan's file name. `CLAUDE.md` Stacking: the M4b paragraph.

Run: `cargo test -p athenaeum-core stacking && npx tsc --noEmit && cargo check -p athenaeum-core --no-default-features` → PASS.

- [ ] **Step 4: Commit** — `feat(stacking): native geometry mode — per-group references and geometry, ATH_RGEO (M4b Task 3, rulings R-M4b-4/5/8)`.

---

### Task 4: Cross-scale pins for the resampler and drizzle

**Files:**
- Test: `crates/athenaeum-core/src/integration/source.rs` tests (`RegisteredSource`), `crates/athenaeum-core/src/stacking/drizzle/mod.rs` tests, `crates/athenaeum-core/src/stacking/ln/mod.rs` tests (one grid-geometry test)

**Interfaces:** none new — this task PINS ruling R-M4b-6 with tests; it changes production code only if a pin fails.

- [ ] **Step 1: Resampler pin** — a 200×150 source plane of constant 0.25 registered through a `PixelMap::linear(scale 2.0)` onto a 400×300 reference: every interior output pixel is 0.25 within 1e-6 (level), NaN only in the uncovered margin; a Gaussian star (σ 1.5 px) at (100, 75) lands at (200, 150) with σ ≈ 3.0 px (resolution follows the scale, no sharpening).
- [ ] **Step 2: Drizzle pin** — the same constant plane through the same map at drizzle 1× and 2×: `I/W` = 0.25 within 1e-5 wherever `W > 0` (level-preserving across a ×2 registration scale — the drop is a 2×2 output-pixel quad at 1×, 4×4 at 2×), coverage 1.0 inside the mapped rectangle.
- [ ] **Step 3: LN pin** — an `LnGrid` built in a 400×300 reference geometry evaluates a 200×150 source registered at ×2 through the SAME reference-coordinate lookup the engine uses (the grid never sees source coordinates) — a one-assert test that the band loop's `(round(u), round(v))` lookup on a ×2 map reads the reference cell, mirroring the drizzle driver's own LN lookup test.
- [ ] **Step 4: Commit** — `test(stacking): cross-scale level and resolution pins for the resampler, drizzle and LN (M4b Task 4, ruling R-M4b-6)`. If a pin fails, fix the production code in the same task and say so in the message.

---

### Task 5: Docs and the frames table

**Files:**
- Modify: `src/components/stacking/FramesTable.tsx` (the model column shows `+wcs` as a `WCS` chip with title "seeded from the plate solves"), `CLAUDE.md` (M4b paragraph — the two modes, the per-frame gate, the WCS seed, the pixel-scale warning), `docs/superpowers/open-items.md` (the M2 subsection's "plan-time pixel-scale warning" item deleted, the M3 subsection's two-pass item deleted — both shipped; a new "Stacking M4b" subsection with the owed owner smoke on a real mixed-scale set), spec §14 (M4b line).

- [ ] **Step 1: Write the three edits; commit** `docs(stacking): M4b — mixed pixel scales, frames-table WCS chip, open-items`.

---

### Task 6 (controller-run): M4b acceptance run on three real mixed-scale sets

**Setup (ruling R-M4b-9):** the M4a acceptance's build recipe (`.superpowers/target-acc` + `dist-acc`), the dev catalog. For each of sets 195, 108 and 138: confirm the frames are on disk (`files` rows not archived), that calibration masters exist or can be built by stage 0.5 (the Coverage tab; a set without any matching darks/flats is swapped for the next candidate in `find-mixed-fov.py`'s list and the swap is recorded), then plate-solve every light of the set through the app's solve queue and record the solved-scale histogram (this is also the first real-data check of R-M4b-1's header formula: header vs solve within 2 %). Then run A (co-registered, LN on, drizzle off — drizzle is not under test here) and run B (native, same) per set, from a clean working folder.

**Measurements (note `docs/superpowers/research/<date>-m4b-acceptance-run.md`):**

| Metric | Target | How |
| ---- | ---- | ---- |
| Header-implied vs solved scale, every solved frame | within 2 % (R-M4b-1's convention holds on real headers) | plan JSON per frame / `plate_solves` |
| Plan warnings | set 195: one `mixes pixel scales (1.73–2.20)` warning per narrowband group; set 108: one `×2.0` warning for `mono__L__bin1__120s`; set 138: `×2.8` warnings for the 352-mm groups; no blocker anywhere | plan JSON |
| Groups table | the Scale column reads the solved value with the `×r` badge on the foreign groups | click-through |
| Set 195 run A (within-group) | every H/O/S group registers BOTH subsets: ≥ 95 % of the 352-mm frames aligned with `scale` 1.27 ± 1 % and the `+wcs` suffix (or `Quads` when the quad seed sufficed — record which), RMS ≤ 1.0 reference px; the master's included count ≈ the whole group (today one subset is dropped) | `stacking_run_frames` |
| Set 108 run A (cross-group ×2) | the bin-1 group registers onto the bin-2 reference at `scale` 0.50 ± 1 % (its pixels are finer), master in the reference geometry (4144×2822), level within 1 % of the bin-2 master over the common field, 50 matched-star centroids within 0.5 px | `fitsdiff.py`-style medians, `samestar.py` |
| Set 138 run A (cross-group ×2.8, FOV ×8) | the 352-mm groups register onto the 1000-mm reference (or vice versa — whichever group is largest wins the reference; record it) with the `+wcs` suffix on ≥ 95 % of frames, RMS ≤ 1.5 reference px; masters in one geometry | `stacking_run_frames` |
| Run B (native) on each set | each group's master in its OWN geometry with its own reference (`reference #<id>` on the card, `ATH_RGEO = 'native'`), no cross-group registration rows | headers + results |
| Same-scale groups | registration records unchanged vs run A of the previous milestone on any set that has them (no `+wcs`, same inlier counts ± 2 %) | `stacking_run_frames` |
| Time | the register stage's per-frame time with a WCS seed ≤ the quad-seed time (the seed skips quad matching) | stage timings |

**Docs:** the acceptance note (with the three sets' histograms and the swap log if any); `docs/superpowers/open-items.md` (the owed owner smoke = the owner's own look at the Ghost Nebula and M 78 masters); `CLAUDE.md` Stacking (the M4b acceptance sentence); spec §15 closed; memory.

---

## Self-review (done while writing)

- **Spec coverage:** §15's requirement — the warning (Task 1), both modes (Tasks 2–3), keys unchanged (nothing to do), the acceptance (Task 6); §3.6's fixed gate replaced by the per-frame gate (Task 2); the owner's coordinate-based hint (Task 2's WCS seed).
- **Placeholders:** none — every constant is named with a value; the acceptance recipe is spelled out.
- **Type consistency:** `ScaleSource`/`pixel_scale_arcsec` (Task 1) → `scale_gate_for` (Task 2) → `stage_register` (Tasks 2–3); `RegistrationGeometry` (Task 3) read by `run.rs`, `plan.rs`, `master_cards.rs`, `RegisterPanel.tsx`; `GroupGeometry`/`geometry_of` (Task 3) consumed by every former `reference_width/height` site; `SeedKind`/`+wcs` (Task 2) rendered by Task 5.

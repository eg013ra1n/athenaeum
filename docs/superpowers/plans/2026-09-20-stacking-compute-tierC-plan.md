# Stacking compute Tier C — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development, one implementer at a time, a task review after every task, interleaved before/after measurements (ruling R-TA-8), the product-build checkpoint as the arbiter (R-TA-9). Steps use checkbox (`- [ ]`) syntax.

**Goal:** the stacking run on the reduced set (`LDN1272-test`, prod set 204) from ≈ 22 min (Tier A) to ≈ 15 min by removing the work that produces the same physical answer twice or by a needlessly expensive route — outputs move, gated by the M-run acceptance.

**Architecture:** six items, each an added artifact or an alternative route inside a stage that already runs (spec §1). C1 persists Measure's star fits and lets Normalize map them into the reference geometry instead of re-detecting; C2 tabulates the drizzle drop's overlap areas per sub-pixel phase; C3/C4 shorten two inner numeric loops; C5 bins the LN background input; C6 lets Register reuse the fits on mono frames. Every item bumps the config hash of the stage it moves and lands with a DELTA pin against the pre-change code on a fixture, plus the acceptance table.

**Tech stack:** Rust (athenaeum-core, the rustafits submodule branch `perf/stacking-kernels`), the acceptance harness (`docs/superpowers/research/scripts/acceptance/`).

**Spec:** `docs/superpowers/specs/2026-09-20-stacking-compute-tierC-design.md`. **Audit:** `docs/superpowers/research/2026-09-19-stacking-compute-audit.md`.

## Global Constraints

- **Owner decision 2026-09-20**: no approval gate on this plan; execute at once; push only on the owner's word.
- **Outputs may move only inside the spec's §8 tolerances**: master median ± 0.1 %, MAD ± 1 %, noise ± 2 %, FWHM ± 1 %, rejected fraction ± 0.3 pp, per-frame weight Spearman ρ ≥ 0.99 / top-20 ≥ 18, LN scale median ratio 1 ± 0.5 % with scatter ≤ 1 %, drizzle level 0.998–1.002, coverage 1.0 — measured against `tierA-tierA` (the Tier A acceptance run's outputs — **the Tier C ruler**) and against the external reference's masters of the same frames.
- **Every item = its own config-hash bump** (the subtree whose artifacts it moves) so caches recompute exactly once, **its own DELTA pin** (the old code kept under `#[cfg(test)]`, the pin recording the measured delta on the fixture with a tolerance, never identity) and **its own interleaved measurement** (B A B A B A on the probe that exercises it).
- **The measurement unit** is the reduced set through `checkpoint.sh <name>` on a quiet machine (`tmutil` Running = 0, no cargo), ≥ 135 GB free (the harness refuses a plan blocker loudly since `a8506f87`); the acceptance also re-runs the ruler build back-to-back (R-TA-9).
- **One implementer at a time**; reviewers may overlap. **No third-party software names** anywhere (code, comments, docs, commits). **No `println!`/`eprintln!` in `src/`.** Logging fields from the dictionary (`docs/superpowers/specs/2026-07-03-logging-overhaul-design.md`); new names go into the dictionary in the same commit. `rustfmt <file>` only on non-module-root files (module roots cascade — hand-format). rustafits changes on `perf/stacking-kernels`, gitlink bumped in the same superproject commit as the core change that needs it. Commits as `eg013ra1n <vilen.sharifov@gmail.com>` with the two session trailers.
- **A measured ≤ 0 item is reverted with its numbers recorded** (Tier A's pattern) — the spec's estimates are hypotheses.

## File structure

- `crates/athenaeum-core/src/stacking/fits_artifact.rs` (new) — the persisted star-fit record: fixed-width LE binary, `write_fits(path, &[StarFit])` / `read_fits(path) -> Vec<StarFit>`, versioned header (`ATHF` magic, `u16` version, `u32` count), one responsibility: the on-disk shape.
- `crates/athenaeum-core/src/stacking/measure.rs` — returns the fits alongside `ChannelMeasurement` (`PlaneMeasurement { channel, fits }`), no other change.
- `crates/athenaeum-core/src/stacking/run.rs` — stage 3 writes the `fits` artifact beside `metrics` and records the group β; stage 6 hands the fits + map to LN; stage 5 hands the fits to Register on mono frames.
- `crates/athenaeum-core/src/stacking/psf_signal.rs` — `PsfModel::Fixed(f64)` (internal, not on the config wire), the group-β resolution helper.
- `crates/athenaeum-core/src/stacking/ln/scale.rs` — `relative_scale_from_fits(...)`: the mapped-fit path sharing `choose_pairing`/`ratio_sample`/RCR with today's `relative_scale_against`; the wider-radius pass 2.
- `crates/athenaeum-core/src/stacking/ln/mod.rs` — `normalize_frame` prefers the fits, falls back to detection with a `warn!`.
- `crates/athenaeum-core/src/stacking/register/{frame,detect}.rs` — `Star` from `StarFit` on mono frames.
- `crates/athenaeum-core/src/stacking/drizzle/phase_table.rs` (new) — `PhaseTable { phases, cells, area }` built from `clip_area`; `deposit_band` consumes it.
- `rustafits/src/analysis/fitting.rs` — the Moffat LM inner loop (C3).
- `crates/athenaeum-core/src/integration/combine.rs` — `medfit_line` bracket/early exit (C4).
- `crates/athenaeum-core/src/stacking/ln/background.rs` + `ln/mod.rs` — binned input (C5).
- `crates/athenaeum-core/src/stacking/config.rs` — hash-subtree constants: `PSF_FIT_VERSION` 2 → 3 (C1), `DRIZZLE_KERNEL_VERSION` (new, C2 — drizzle has no per-frame artifact; the version enters the whole-config fingerprint so the plan gate shows a fresh run), `LN_BACKGROUND_VERSION` (new, C5 — folds into the `ln` artifact hash).
- `docs/superpowers/research/scripts/acceptance/tier1/tier1-compare.py` — a `--numeric` mode (T0).

---

## Group 0 — the numeric harness

### Task 0: `tier1-compare.py --numeric` and the acceptance script

**Files:** `docs/superpowers/research/scripts/acceptance/tier1/tier1-compare.py` (a `--numeric` mode: per master median / MAD / MRS noise / FWHM (through `measure_probe` on the master) / rejected fraction (from the run's `summary_json`) with the ratio to the base and PASS/FAIL against the spec §8 tolerances; per-frame weights from `registration_results`/`summary_json` → Spearman ρ + top-20 overlap; LN scale per frame from the `.athln` sidecar header (`A` grid mean) → median ratio + scatter; drizzle level = drizzled/undrizzled median; coverage from the weight map), `tier1/checkpoint.sh` (a `--numeric` flag passed through; default stays byte identity), `docs/superpowers/research/scripts/acceptance/tierC-external.sh` (new: the external-reference comparison the M-runs used — `docs/superpowers/research/scripts/weight_audit_compare.py` for weights, `imgcmp.py`-style stats for the masters — one script that takes `<checkpoint-dir> <external-masters-dir>` and prints the same table).
- [ ] Run `--numeric` on `tierA-tierA` vs `tierA-baseline` first: every row must read 1.000 / identical — that is the mode's own pin.
- [ ] Commit `harness: numeric acceptance compare for Tier C`.

---

## Group 1 — LN takes Measure's fits (C1, C6)

### Task 1: persist the fits (`fits` artifact) and the group β

**Files:** `stacking/fits_artifact.rs` (new), `stacking/measure.rs:265-510` (`measure_plane_with_seeds` returns `(ChannelMeasurement, Vec<StarFit>)` — today `outcome.fits` is consumed by `signal_totals`/`frame_shape` at `:469-470` and dropped), `stacking/run.rs` stage 3 (`:2984`/`:3105` — the `metrics` artifact write: add the `fits` row + file per plane, hash = the measurement hash, path `<working_dir>/<set_slug>/fits/<group>/<stem>.p<plane>.athf`), `stacking/paths.rs` (`fits/` in the layout; `CleanupWhat::All` only, like `metrics`), `stacking/plan.rs` (footprint + a `fits_cached` count mirroring `metrics_cached`; Measure staleness follows the pair), `psf_signal.rs` (`PSF_FIT_VERSION = 3`; `PsfModel::Fixed(f64)`; `pub fn group_beta(betas: &[f64]) -> f64` = the LOWER median so the result is always one of `AUTO_BETAS` — ruling C-1a), `db/stacking.rs` (the group row gets `beta REAL NULL`; `summary_json` carries it).
**Interfaces:** `fits_artifact::{write_fits, read_fits, FITS_ARTIFACT_VERSION}`; `run.rs::MeasuredGroup.beta: Option<f64>`; the `fits` artifact kind string `"fits"`.
- [ ] Failing test: `fits_artifact` round-trips 1 000 random `StarFit`s bit-exactly (`to_bits` per field), rejects a wrong magic/version with a typed error.
- [ ] Failing run test: a fixture run writes one `fits` row per (frame, plane) with the measurement hash, `read_fits` returns `stars_fitted` records, the group row's `beta` equals `group_beta` of the members' `ChannelMeasurement.beta`; a re-run from Measure with an unchanged config reads them as cached.
- [ ] Implement; full core gate; commit `stacking: persist Measure's star fits as the fits artifact; the group β`.
- [ ] Measure: `measure_probe` before/after (interleaved) — the write must cost < 5 ms per plane.

### Task 2: `relative_scale_from_fits` — LN maps the fits instead of re-detecting

**Files:** `stacking/ln/scale.rs` (new `pub fn relative_scale_from_fits(prepared: &PreparedReferenceChannel, fits: &[StarFit], map: &PixelMap, width, height, match_radius_px, rcr_limit, local_scale, pool) -> Result<ScaleResult, LnError>`: (1) cuts — apply `register/detect.rs`'s saturation/eccentricity/SNR rules to the fits (read them there; expose as `pub(crate) fn passes_register_cuts(&StarFit) -> bool` in one place); (2) map each survivor's `(x, y)` through `map.forward_exact` (ruling R-T4-3); drop those outside `[0, ref_w) × [0, ref_h)`; (3) flux under the map: read `ratio_sample` to see which quantity it ratios (`mean_flux = signal/area` or `signal`) — `signal` is invariant under a flux-conserving warp, `area` scales by `|det J|` of the map's linear part at the star (the `Linear` 2×2 block for Similarity/Affine; the local Jacobian of `forward_exact` under a homography/distortion) — correct the mapped fit's `area` by `|det J|` so `mean_flux` matches what a fit on the warped frame would give (ruling C-1b: `area *= |det J|`, `signal` untouched); (4) `choose_pairing` on the mapped positions against `prepared.tree` with pass 2 = the SAME matcher at `2 × match_radius_px` when pass 1 covers < `LN_BARYCENTRE_PASS_THRESHOLD` of the fits (ruling C-2 — the `barycentre_tree` path stays for the detection fallback); (5) `ratio_sample` → `rcr` → `fit_local_scale` unchanged; `ScaleTimings { detect_ms: 0, refine_ms: 0, fit_ms: 0, match_ms }`), `stacking/ln/mod.rs::normalize_frame` (prefers `relative_scale_from_fits` when the frame's `fits` artifact for that plane exists and is fresh; else today's path with ONE `warn!(frame_id, path, "ln: no measured fits, detecting on the warped frame")`; `LnFrameOutcome` gains `scale_source: "fits" | "detected"` for the log), `stacking/run.rs` stage 6 (reads the `fits` artifact rows for the group's members, passes them + the registration map + the group β to `normalize_frame`; the LN reference build (`PreparedReferenceChannel::build`) fits at `PsfModel::Fixed(group_beta)`), `stacking/config.rs` (`normalization_subtree_with_fit_version` already folds `PSF_FIT_VERSION` — the bump in Task 1 invalidates every `.athln` once; no second bump).
**Interfaces:** `normalize_frame(..., fits: Option<&[StarFit]>, group_beta: f64, ...)`.
- [ ] Failing pin (DELTA, not identity): on the M2 LN fixture group, `relative_scale_from_fits` vs `relative_scale_against` (the detection path, kept as the oracle) — `scale` within 0.5 %, `matches` ≥ 0.8 × the oracle's; on a fixture with a 1.2× Similarity map the corrected ratio equals the uncorrected one × `1/1.44` within 1e-9 (the `|det J|` rule).
- [ ] Failing run test: a fixture run normalizes every frame with `scale_source = "fits"`; deleting one frame's `fits` file makes that frame fall back with the warning and the run still completes.
- [ ] Implement; full core gate; commit `stacking: LN takes Measure's fits — mapped through the registration, no second detection`.
- [ ] Measure: `ln_probe` interleaved: `scale_ms` (was 5 034 ms per frame at g2) expected → < 300 ms; the sidecar's `scale` per frame vs before: median ratio, scatter (this IS the acceptance metric, first read here).

### Task 3: Register reuses the fits on mono frames (C6)

**Files:** `stacking/register/frame.rs::detect_frame_stars` / `reference_stars` (when the frame is single-plane and a fresh `fits` artifact exists: `Star { x, y, flux: fit.signal, size: fit.fwhm() }` for every fit passing `passes_register_cuts`, sorted by flux descending, truncated to `max_stars`; else today's detection), `stacking/run.rs` stage 5 (passes the fits handle; OSC frames unchanged — ruling C-6), `config.rs` (`registration_subtree` folds `PSF_FIT_VERSION` too, so the first run re-registers once — say so in the doc).
- [ ] Failing pin (DELTA): on the M1 registration fixtures (mono), the alignment from the fit-derived list vs the detected list — rms within 0.05 px, inliers ≥ 0.9×.
- [ ] Implement; full core gate; commit.
- [ ] Measure: `register_probe` interleaved on the mono frame: `detect_ms` (833 ms at g2) → ≈ read-only (< 50 ms).

### CHECKPOINT C-1
- [ ] `checkpoint.sh c1 --numeric` (vs `tierA-tierA`): Normalize stage (5.8 min at g2/g3) → ≈ 2.5; Register −0.2; the §8 LN-scale row and the weight-ρ row PASS; masters within tolerance. Ledger the deltas. A FAIL on the LN-scale row stops the plan here (the flux-correction rule is the first suspect).

---

## Group 2 — drizzle (C2)

### Task 4: the phase table

**Files:** `stacking/drizzle/phase_table.rs` (new: `PhaseTable::build(quad_at_origin: &Quad, scale, phases: usize) -> PhaseTable` — for `iy in 0..phases, ix in 0..phases` the drop translated by `((ix + 0.5)/phases, (iy + 0.5)/phases)` clipped against every output pixel its bbox touches via the SAME `geom::clip_area` (`geom.rs:386`), stored as `cells: Vec<(i8, i8)>` (offsets from the floor of the mapped centre) + `area: Vec<f32>` per phase, `PHASES = 32`; `lookup(fx, fy) -> (&[(i8,i8)], &[f32])`), `stacking/drizzle/geom.rs` (`map_drop_at` = the parallelogram from the map's local linear part at a point — for a `Linear` map the corners' offsets from the mapped centre; for a distortion map the local Jacobian at the tile centre by finite differences of `forward_exact`), `stacking/drizzle/mod.rs::deposit_band` (`:907`: per source pixel `fwd.at` once → `oy` band skip (Task 11) → `(cx, cy) = floor`, `(fx, fy) = frac` → `lookup` → for each cell `out[cy+dy][cx+dx] += w · v · area`, weight/coverage/rejection exactly as today; the table is per frame for `distortion.is_none()` and per `TILE = 256` source px otherwise (rebuilt when the pixel enters a new tile); `scale == 1 && drop_shrink == 1.0` keeps the exact clip — ruling C-3), `drizzle/mod.rs` (the exact per-pixel clip path stays under `#[cfg(test)]` as the oracle AND as the `scale == 1` runtime path), `config.rs` (`DRIZZLE_KERNEL_VERSION = 2` in the whole-config fingerprint).
- [ ] Failing pins: (a) every table entry equals `clip_area` of the exactly-translated drop at that phase to 1e-6 (the table is built from the same function — this pins the offsets/indexing); (b) `Σ area` per phase = the drop's area ± 1e-6 (level preservation, R-M3-2); (c) DELTA on the M3 fixtures at 1°/5°/30° rotation × scale 2 × dropShrink 0.9: drizzled plane vs the exact oracle — per-pixel |Δ| ≤ 2 % of the local value, level ratio 1 ± 1e-3, coverage identical; (d) a TPS map at the tile rule: same bounds.
- [ ] Implement; full core gate; commit `stacking: drizzle deposits through a per-phase overlap table`.
- [ ] Measure: the throwaway `drizzle_group` timing harness Task 11 used (10 real mono frames, interleaved): `deposit_ms` expected −80 %.

### CHECKPOINT C-2
- [ ] `checkpoint.sh c2 --numeric` (vs `tierA-tierA`): Drizzle 4.3 → ≈ 1.2 min; the §8 drizzle rows (level 0.998–1.002, coverage 1.0) PASS; the drizzled masters' FWHM within 1 % of the ruler's.

---

## Group 3 — the inner loops (C3, C4, C5)

### Task 5: Moffat LM arithmetic (C3, rustafits)

**Files:** `rustafits/src/analysis/fitting.rs:387-700` (`fit_moffat_2d`: `power = base.powf(-beta)` once, `dpower = -beta * power / base` (`:549` today recomputes `powf(-beta-1)`), `j[7]` needs `ln(base)` ONLY when β is free — with `PsfModel::Fixed`/`Moffat4` pass `fit_beta = false` and skip it (`:566`); `residual_cost_moffat` (`:692`) reuses the residual vector the last accepted Jacobian pass produced instead of recomputing; `cholesky_solve` (`:303`) on a thread-local scratch; `PixelSample` → SoA `(x: Vec<f32>, y, v)` with the stamp's implicit grid), the core caller `psf_signal::fit_one` (passes `fit_beta`).
- [ ] Failing pins (DELTA): on the fixture stars (`fitting.rs` tests at `:835`/`:870`), position within 1e-5 px, flux within 1e-5 relative, β identical when fixed; the iteration count identical (ruling C-4 — the control flow is untouched).
- [ ] Implement on `perf/stacking-kernels`; rustafits `cargo test` both feature sets; gitlink bump; core gate; commit.
- [ ] Measure: `measure_probe` interleaved: `fit_ms` (337–392 ms per plane) expected −30 %.

### Task 6: `medfit_line` warm bracket and early exit (C4)

**Files:** `integration/combine.rs:930-985` (`medfit_line`: take `prev: Option<(b, half_width)>`; when `Some`, bracket `b_prev ± half_width` and widen ×2 until the sign changes (cap 4 widenings, then the cold `± 3σ_b` bracket at `:937`); bisect to the SAME tolerance; return the final half-width for the next call), `:987-1060` (`reject_linear_fit`: stop when an iteration rejected 0 samples — today's confirming iteration; `MAX_REJECTION_ITERS` unchanged), the T0 counters keep counting (`MEDFIT_EVALS` will show the drop).
- [ ] Failing pins (DELTA): 1 000 random stacks — `(a, b)` within 1e-6 relative of the cold-bracket oracle; survivors identical on ≥ 99.9 % of stacks and never more than one sample apart; `medfit_evals_mean` on the engine fixture ≤ 0.5 × before.
- [ ] Implement; full core gate; commit.
- [ ] Measure: `integrate_probe --limit 60 --rejection linearFit` interleaved: `combine_cpu_ms` expected −25 %, rejected fraction within 0.1 pp.

### Task 7: LN background on a binned plane (C5)

**Files:** `stacking/ln/background.rs` (`background_grid` bins the plane `LN_BIN = 4` (mean of finite pixels, NaN when < 8 finite) into a scratch before `clean_plane` and the cell loop; cells map to the binned stride — `scale/8/LN_BIN` pixels per cell edge), `ln/mod.rs::median_of_finite` (from `stats::stratified_sample`), `config.rs` (`LN_BACKGROUND_VERSION = 2` folded into the `ln` artifact hash).
- [ ] Failing pins (DELTA): on the M2 LN fixtures, node values within 1e-3 relative of the unbinned oracle; `invalid_cells` identical; `median_of_finite` within 0.1 %.
- [ ] Implement; full core gate; commit.
- [ ] Measure: `ln_probe` interleaved: `background_ms` (76 ms on the 4-thread probe after Task 12) → < 20 ms.

### CHECKPOINT C-3 (= Tier C acceptance)
- [ ] `checkpoint.sh tierC --numeric` against `tierA-tierA`, then the ruler build re-run back-to-back (`tierA-tierA` was built from `cecc988d` — rebuild it in a throwaway worktree the way the Tier A acceptance re-ran `548eeec3`), then `tierC-external.sh` against the external reference's masters (`~/acc-xisf/` holds the external set; the M-run reports name the paths). Every §8 row PASS; the full stage table ruler → Tier C; `sample` of `ln_probe` and `integrate_probe` after. Write `docs/superpowers/research/2026-09-XX-stacking-compute-tierC-acceptance.md` (the M-run shape), update the audit's §7 with the measured Tier C column, add the Tier C paragraph to `CLAUDE.md`'s Stacking section (what moved, the new `fits` artifact, the group β, the phase table, the versions), commit, merge to local main. Expected: ≈ 22 → ≈ 15 min.

---

## Self-review

**Spec coverage:** C1 → T1+T2 (β, fits, mapping, fallback, pass 2); C2 → T4; C3 → T5; C4 → T6; C5 → T7; C6 → T3; §8 acceptance → T0 + the three checkpoints. Rulings C-1…C-7 from the spec are cited at the tasks that apply them; C-1a (lower median β) and C-1b (`area *= |det J|`) are added here and go into the spec at Task 1/2's commit.

**Placeholders:** none — each task names files, the pin (with its tolerance), the gate and the measurement.

**Type consistency:** `StarFit` (psf_signal.rs) is the record `fits_artifact` persists and `relative_scale_from_fits`/`detect_frame_stars` consume; `PsfModel::Fixed(f64)` (T1) is what T2's reference build and T5's `fit_beta = false` key on; `PhaseTable` (T4) is consumed only inside `deposit_band`; the three version constants are each read by exactly one hash subtree.

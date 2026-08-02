# Calibration Supersede-Lifecycle Hardening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close every Critical and Important finding of the 2026-08-02 calibration audit (`docs/superpowers/research/2026-08-02-calibration-audit.md`) so the master calibration library survives routine use: re-matching, deleting, archiving, rescanning, and bad input data.

**Architecture:** Four phases. A — teach the matching layer (auto-link, on-demand set creation, flats path) that masters and `superseded_by_set_id` exist. B — numerical hygiene per the ratified research policy (non-finite exclusion at integration, epsilon floor at flat division, BITPIX-aware output scale). C — master lifecycle guards (unregister primitive, `delete_master` command, Black Hole interception, archive Copy enforcement, recluster-cascade guard). D — robustness/UI (panic-safety, Show-All visibility, explicit manual clear, claim-based filename resolution, band budget, minors).

**Tech Stack:** Rust (athenaeum-core + Tauri/Axum wrappers), rusqlite, React/TS frontend.

## Global Constraints

- Every new/changed Tauri command gets its Axum mirror in the same task; register in `invoke_handler![]` (`crates/athenaeum-tauri/src/lib.rs`) and `build_router` (`crates/athenaeum-web/src/routes/mod.rs`).
- Serde boundary: `#[serde(rename_all = "camelCase")]` on new wire types; new model types registered in `ts_export.rs`.
- Never swallow errors: `tracing::error!`/`warn!` before returning; commands wear `#[tracing::instrument(skip_all, err)]` (web: `err(Debug)`).
- No third-party tool names in code or comments (docs may cite them).
- Design tokens only in frontend styling (`bg-surface`, `text-warning`, …).
- Gates per task: `cargo test -p athenaeum-core <module>`; full gates before merge: `cargo build --workspace`, `cargo test -p athenaeum-core`, `npx tsc --noEmit`. clippy is NOT a gate; format touched files with `rustfmt <files>`.
- Commit as the user (`eg013ra1n <vilen.sharifov@gmail.com>`), one commit per task, on the active version branch.
- Tests use in-memory SQLite via `crate::db::schema::init_db` fixtures (existing pattern in each touched module).

---

## Phase A — Matching learns about masters

### Task 1: Masters are always auto-link candidates

**Files:**
- Modify: `crates/athenaeum-core/src/calibration/configurable_matcher.rs:488-499` (OnlyCompatible arm), `:1178-1216` (old pin test)
- Modify: `crates/athenaeum-core/src/calibration/config.rs:243-246` (Default impl), `:472-476` (default map)

**Interfaces:**
- Produces: `CandidateMode::OnlyCompatible` now returns masters too (ordering still via `apply_master_preference`). Task 3 relies on this.

- [ ] **Step 1: Rewrite the pin test to assert the NEW contract** (replace `engine_no_preference_excludes_masters_for_auto_only`):

```rust
#[test]
fn engine_no_preference_includes_masters_for_auto_link() {
    // Same fixture as before: one raw Dark set + one MasterDark set, both
    // parameter-compatible with the frame. Under NoPreference the master must
    // now APPEAR in OnlyCompatible results (ordering by score only).
    let (conn, frame, config) = fixture_with_raw_and_master_dark(); // reuse the old test's fixture code
    let ids: Vec<i64> = find_calibration_candidates(
        &conn, &frame, "lights", "dark", &config, CandidateMode::OnlyCompatible,
    ).unwrap().into_iter().map(|c| c.set_id).collect();
    assert!(ids.contains(&MASTER_SET_ID), "master must be an auto-link candidate");
    assert!(ids.contains(&RAW_SET_ID), "raw set still competes on score");
}
```

- [ ] **Step 2: Run it — expect FAIL** (`cargo test -p athenaeum-core engine_no_preference_includes_masters`) — the retain filter still drops the master.

- [ ] **Step 3: Remove the filter.** In the `CandidateMode::OnlyCompatible` arm replace:

```rust
CandidateMode::OnlyCompatible => {
    // Preserve historic auto-link semantics: when master_pref is
    // NoPreference, callers don't expect masters mixed into auto-link
    // results. PreferMaster / PreferFrameset already include both.
    if master_pref == MasterPreference::NoPreference {
        compatible.retain(|c| !c.is_master);
    }
    Ok(compatible)
}
```

with:

```rust
CandidateMode::OnlyCompatible => {
    // Masters are ALWAYS auto-link candidates (2026-08-02 audit C1): a
    // superseded raw set is excluded by the candidate query, so hiding its
    // master too left whole lineages unmatchable and re-matching minted
    // duplicate raw sets. `master_preferences` only orders the list now.
    Ok(compatible)
}
```

- [ ] **Step 4: Flip the shipped default to PreferMaster** (fresh configs order masters first; saved configs keep the user's choice — the filter removal already un-breaks them):

```rust
impl Default for MasterPreference {
    fn default() -> Self {
        MasterPreference::PreferMaster
    }
}
```

and in `config.rs` `default_*` (four inserts at :473-476) replace `MasterPreference::NoPreference` with `MasterPreference::PreferMaster`.

- [ ] **Step 5: Run the module tests** (`cargo test -p athenaeum-core configurable_matcher`) — fix any other test pinning the old default (assert intent, not the enum literal). Expected: PASS.

- [ ] **Step 6: Commit** — `fix(calibration): masters are always auto-link candidates; preference orders only`

### Task 2: On-demand set creation must not resurrect a superseded lineage

**Files:**
- Create: `crates/athenaeum-core/src/calibration/superseded_guard.rs`
- Modify: `crates/athenaeum-core/src/calibration/mod.rs` (declare module)
- Modify: `crates/athenaeum-core/src/calibration/dark_bias_groups.rs:826` (`create_dark_calibration_set`), `:942` (`create_bias_calibration_set`)
- Modify: `crates/athenaeum-core/src/calibration/flat_groups.rs:437` (`create_flat_calibration_set`)

**Interfaces:**
- Produces: `pub fn superseding_master_for_frames(conn: &Connection, frame_ids: &[i64]) -> anyhow::Result<Option<i64>>` — Task 3's test scenario also exercises it via the flat path.
- Consumes: group structs' `frame_ids: Vec<i64>` (all three group types carry it).

- [ ] **Step 1: Write the failing test** (in the new module):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detects_superseded_membership() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO calibration_set (id, imagetyp, is_master_library) VALUES (10, 'Dark', 0);
             INSERT INTO calibration_set (id, imagetyp, is_master_library) VALUES (11, 'MasterDark', 1);
             UPDATE calibration_set SET superseded_by_set_id = 11 WHERE id = 10;
             INSERT INTO files (id, path, filename, size, format) VALUES (1, '/t/a.fits', 'a.fits', 1, 'fits');
             INSERT INTO frames (id, file_id, imagetyp) VALUES (100, 1, 'DARK');
             INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (10, 100);",
        ).unwrap();
        assert_eq!(superseding_master_for_frames(&conn, &[100]).unwrap(), Some(11));
        assert_eq!(superseding_master_for_frames(&conn, &[999]).unwrap(), None);
        assert_eq!(superseding_master_for_frames(&conn, &[]).unwrap(), None);
    }
}
```

(Adjust the INSERT column lists to the real NOT NULL schema of `files`/`frames` at implementation time — copy from an existing dark_bias_groups test fixture.)

- [ ] **Step 2: Run — FAIL** (module doesn't exist).

- [ ] **Step 3: Implement:**

```rust
//! Guard shared by every on-demand calibration-set creation path (dark, bias,
//! flat): a frame group whose members already belong to a superseded raw set
//! is a lineage a master replaced — minting a fresh raw set from those frames
//! would silently divert auto-links away from the master (2026-08-02 audit C1).
use anyhow::Result;
use rusqlite::Connection;

/// Returns the superseding master's set id when any of `frame_ids` belongs to
/// a superseded calibration set. When several superseded sets cover the group,
/// the one covering the most frames wins (ties: lowest master id).
pub fn superseding_master_for_frames(conn: &Connection, frame_ids: &[i64]) -> Result<Option<i64>> {
    if frame_ids.is_empty() {
        return Ok(None);
    }
    let placeholders: String = frame_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT cs.superseded_by_set_id
           FROM calibration_set_frames csf
           JOIN calibration_set cs ON cs.id = csf.set_id
          WHERE csf.frame_id IN ({placeholders})
            AND cs.superseded_by_set_id IS NOT NULL
          GROUP BY cs.superseded_by_set_id
          ORDER BY COUNT(*) DESC, cs.superseded_by_set_id ASC
          LIMIT 1"
    );
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(&sql, rusqlite::params_from_iter(frame_ids.iter()), |r| r.get::<_, i64>(0))
        .optional()?)
}
```

Declare `pub mod superseded_guard;` in `calibration/mod.rs`.

- [ ] **Step 4: Wire into all three creation functions.** At the top of `create_dark_calibration_set`, `create_bias_calibration_set`, and `create_flat_calibration_set` (immediately after argument extraction, before the `check_for_existing_*` call), using each function's group variable:

```rust
if let Some(master_id) =
    crate::calibration::superseded_guard::superseding_master_for_frames(conn, &dark_group.frame_ids)?
{
    tracing::info!(
        master_set_id = master_id,
        frames = dark_group.frame_ids.len(),
        "group belongs to a superseded lineage — reusing its master instead of minting a duplicate raw set"
    );
    return Ok(master_id);
}
```

(`flat_group.frame_ids` / `bias_group.frame_ids` in the respective functions.)

- [ ] **Step 5: Add one regression test per type** in the respective module's tests: fixture = raw set with members + superseded marker + master set; call `create_*_calibration_set` with a group listing those frame ids; assert the returned id is the MASTER id and `SELECT COUNT(*) FROM calibration_set` did not grow.

- [ ] **Step 6: Run** `cargo test -p athenaeum-core superseded_guard dark_bias_groups flat_groups` — PASS. **Commit** — `fix(calibration): on-demand set creation reuses the superseding master instead of minting duplicates`

### Task 3: The flats auto-path can reach a master flat

**Files:**
- Modify: `crates/athenaeum-core/src/calibration/hierarchy.rs:332-345` (flat auto-detect branch) + imports at `:4-8`

**Interfaces:**
- Consumes: Task 1's OnlyCompatible-includes-masters contract; `CalibrationCandidate { set_id, is_master }` (`calibration/finder.rs:64`); `find_calibration_candidates(conn, frame, "lights", "flat", &config, CandidateMode::OnlyCompatible)`.

- [ ] **Step 1: Write the failing test** (in `hierarchy.rs` tests; copy an existing fixture that builds a light frame + DB):

```rust
#[test]
fn auto_flat_prefers_master_over_group_recreation() {
    // Fixture: light frame; a MasterFlat calibration_set parameter-compatible
    // with it (same instrume/binning/filter, date within max_age); the raw
    // flat set superseded by it. No manual selections.
    let (conn, light) = fixture_light_with_master_flat();
    let h = build_complete_hierarchy(&conn, &light, &default_tolerance(), None, None, None,
                                     365, 240, 1.0).unwrap();
    let flat = &h.flat_sets_with_links[0].set;
    assert_eq!(flat.is_master_library, 1, "auto path must land on the master flat");
}
```

- [ ] **Step 2: Run — FAIL** (legacy path groups raw frames; with Task 2 it would return the master id from group creation, but the master must ALSO be reachable when the raw flat frames are gone from the catalog — that's what this pre-step adds; the test fixture should therefore *omit* raw flat frames and keep only the superseded set row).

- [ ] **Step 3: Implement the pre-step.** In `build_complete_hierarchy`, replace the auto-detect arm of `let flat_set_id = …` with:

```rust
} else {
    // Master flats exist only as calibration_set rows (their MASTERFLAT frame
    // is invisible to raw-frame grouping), so consult the configurable
    // matcher first; fall back to pattern-based grouping when no master
    // matches (2026-08-02 audit C1, flats arm).
    let master_flat = crate::calibration::configurable_matcher::find_calibration_candidates(
        conn, light_frame, "lights", "flat", &config,
        crate::calibration::finder::CandidateMode::OnlyCompatible,
    )?
    .into_iter()
    .find(|c| c.is_master);

    if let Some(m) = master_flat {
        tracing::debug!(frame_id, set_id = m.set_id, "auto-linked master flat via configurable matcher");
        Some(m.set_id)
    } else {
        // …existing find_flat_groups_for_light_frame / pattern-selection code,
        // unchanged, indented into this else-branch…
    }
}
```

- [ ] **Step 4: Run the test — PASS.** Also run the full `cargo test -p athenaeum-core hierarchy` to catch fixture fallout.

- [ ] **Step 5: Commit** — `fix(calibration): flats auto-path consults the configurable matcher for master flats first`

---

## Phase B — Numerical hygiene (ratified research policy)

### Task 4: Integration excludes non-finite samples per pixel

**Files:**
- Modify: `crates/athenaeum-core/src/integration/engine.rs:53-125` (`run_banded`), `IntegrationOutput` struct (same file/`mod.rs` — wherever declared)
- Modify: `crates/athenaeum-core/src/api/masters.rs` (`run_build` post-integration reporting; `MasterBuildCompleteEvent` gains `warning`)
- Modify: `src/types/models.ts` (event type) + `src/hooks/useMasterBuilds.ts` (surface warning in the completion `notify()` detail)

**Interfaces:**
- Produces: `IntegrationOutput { …existing…, bad_samples_per_frame: Vec<usize>, all_bad_pixels: usize }`; `MasterBuildCompleteEvent { …existing…, warning: Option<String> }` (camelCase on the wire).

- [ ] **Step 1: Write the failing engine test** (engine.rs tests; frames written with the crate's own writer):

```rust
#[test]
fn non_finite_samples_are_excluded_not_propagated() {
    let dir = tempfile::tempdir().unwrap();
    let w = 4; let h = 4;
    let mut paths = Vec::new();
    for i in 0..16 {
        let mut data = vec![100.0f32; w * h];
        if i == 3 { data[5] = f32::NAN; }          // one bad sample in one frame
        if i < 16 { data[9] = f32::INFINITY; }      // pixel 9: bad in EVERY frame
        let p = dir.path().join(format!("f{i}.fits"));
        crate::fits_writer::write_fits_f32(&p, w, h, 1, &data, &[]).unwrap();
        paths.push(p);
    }
    let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap();
    let out = integrate_bias_like(
        &paths,
        IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 }),
        &pool, dir.path(), &AtomicBool::new(false), EngineProgress::noop(),
    ).unwrap();
    assert!(out.data.iter().all(|v| v.is_finite()), "master must never contain non-finite pixels");
    assert!((out.data[5] - 100.0).abs() < 1e-3, "pixel 5 combines the 15 good samples");
    assert_eq!(out.data[9], 0.0, "all-bad pixel becomes 0");
    assert_eq!(out.bad_samples_per_frame[3], 2, "frame 3: its own NaN + the shared Inf pixel");
    assert_eq!(out.all_bad_pixels, 1);
}
```

(Use the module's existing `EngineProgress` no-op construction if `noop()` doesn't exist — copy from a neighboring test.)

- [ ] **Step 2: Run — FAIL** (today: winsorized clamp panic).

- [ ] **Step 3: Implement.** In `run_banded`: before the band loop add
`let bad_samples: Vec<std::sync::atomic::AtomicUsize> = (0..n).map(|_| Default::default()).collect();`
`let all_bad = AtomicUsize::new(0);`
and replace the per-pixel inner loop body with:

```rust
column.clear();
let idx = row_in_band * w + x;
for (i, frame) in band_bufs.iter().enumerate() {
    let mut v = frame[idx];
    if let Some(p) = precal {
        match p {
            FlatPrecal::MasterFrame { data, width, .. } => {
                let gy = y0 + row_in_band;
                v -= data[gy * *width + x];
            }
            FlatPrecal::SyntheticBias(b) => v -= *b,
            FlatPrecal::None => {}
        }
    }
    v *= scales[i];
    if !v.is_finite() {
        // FITS: NaN in float data means "undefined pixel" — excluded from the
        // stack (with accounting), exactly like an out-of-range rejection.
        bad_samples[i].fetch_add(1, Ordering::Relaxed);
        continue;
    }
    column.push(v);
}
if column.is_empty() {
    *out_px = 0.0;
    all_bad.fetch_add(1, Ordering::Relaxed);
} else {
    let (val, rej) = combine_pixel(&mut column, recipe);
    *out_px = val;
    if rej > 0 { rejected.fetch_add(rej, Ordering::Relaxed); }
}
```

After the band loop, enforce the invariant and extend the output:

```rust
if let Some(bad) = out.iter().find(|v| !v.is_finite()) {
    // Unreachable by construction; a hard error beats shipping a poisoned master.
    return Err(IntegrationError::Decode(format!(
        "internal: non-finite value {bad} survived input filtering"
    )));
}
Ok(IntegrationOutput {
    width: w,
    height: h,
    data: out,
    rejected_fraction: rejected.load(Ordering::Relaxed) as f64 / total_samples as f64,
    flat_norm: None,
    bad_samples_per_frame: bad_samples.into_iter().map(|a| a.into_inner()).collect(),
    all_bad_pixels: all_bad.into_inner(),
})
```

Add the two fields to `IntegrationOutput` and fix every construction site (`integrate_flat` path included).

- [ ] **Step 4: Report at the build layer.** In `api/masters.rs::run_build`, after integration returns, map frame index → source path (the ordered `paths` list already exists there) and:

```rust
let total_bad: usize = output.bad_samples_per_frame.iter().sum::<usize>() + output.all_bad_pixels;
let build_warning = if total_bad > 0 {
    for (i, &count) in output.bad_samples_per_frame.iter().enumerate() {
        if count > 0 {
            tracing::warn!(set_id, path = %paths[i].display(), count, "non-finite samples excluded from integration");
        }
    }
    if output.all_bad_pixels > 0 {
        tracing::warn!(set_id, count = output.all_bad_pixels, "pixels with no valid sample written as 0");
    }
    Some(format!(
        "{} undefined sample(s) excluded across {} frame(s){}",
        output.bad_samples_per_frame.iter().sum::<usize>(),
        output.bad_samples_per_frame.iter().filter(|&&c| c > 0).count(),
        if output.all_bad_pixels > 0 { format!("; {} pixel(s) had no valid data", output.all_bad_pixels) } else { String::new() },
    ))
} else { None };
```

Thread `build_warning` to the single exit path and add `pub warning: Option<String>` to `MasterBuildCompleteEvent` (serde camelCase). In `src/types/models.ts` add `warning?: string | null` to the event interface; in `useMasterBuilds.ts` append it to the completion notification `detail` when present.

- [ ] **Step 5: Run** `cargo test -p athenaeum-core integration` + `npx tsc --noEmit` — PASS. **Commit** — `fix(integration): exclude non-finite samples per pixel with accounting; finite-output invariant`

### Task 5: Flat-division epsilon floor in light calibration

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/light_cal.rs` (`:54` area — new const; `:167-184` pixel loop; `LightCalOutcome`; `flat_norm_constant` validation)
- Modify: `crates/athenaeum-core/src/api/lights.rs` (log the counter after a frame completes)

**Interfaces:**
- Produces: `pub const FLAT_DENOM_FLOOR: f64 = 2.0e-5;`, `LightCalOutcome { …existing…, floored_flat_pixels: u64 }`.

- [ ] **Step 1: Failing engine test** (light_cal.rs tests, same fixture style as its existing formula tests):

```rust
#[test]
fn zero_and_negative_flat_pixels_are_floored_not_inf() {
    // light = 100 everywhere; flat = 1.0 everywhere except pixel 0 = 0.0 and
    // pixel 1 = -0.5; ATH_FNRM = 1.0 (norm off keeps the divisor 1.0).
    let out = run_engine_with_flat(&[0.0, -0.5, 1.0, 1.0], /* flat_norm: */ false);
    assert!(out.result.data.iter().all(|v| v.is_finite()));
    assert!(out.result.data[0] > 0.0 && out.result.data[1] > 0.0, "no sign flips");
    assert_eq!(out.outcome.floored_flat_pixels, 2);
}
```

(`run_engine_with_flat` = small local helper writing light+flat FITS via `write_fits_f32` and invoking `calibrate_light`; model it on the module's existing tests.)

- [ ] **Step 2: Run — FAIL** (Inf/negative output today).

- [ ] **Step 3: Implement.** Next to `OUTPUT_SCALE_DIVISOR`:

```rust
/// Floor for the flat denominator in normalized units. A dead / negative flat
/// pixel must not produce Inf/NaN or flip the light's sign; established
/// stacking tools floor this division the same way and count the hits.
pub const FLAT_DENOM_FLOOR: f64 = 2.0e-5;
```

In the pixel loop (the band loop is serial — a plain counter suffices), replace the flat division:

```rust
let mut floored_flat_pixels: u64 = 0;
// … inside the per-pixel body:
if let Some(fi) = flat_idx {
    let denom = band_bufs[fi][idx] as f64 / flat_norm_divisor;
    if denom.is_finite() && denom >= FLAT_DENOM_FLOOR {
        v /= denom;
    } else {
        v /= FLAT_DENOM_FLOOR;
        floored_flat_pixels += 1;
    }
}
```

After the loop, before building the outcome:

```rust
if floored_flat_pixels > 0 {
    tracing::warn!(
        src = %inputs.light_path.display(),
        count = floored_flat_pixels,
        "flat denominator floored (dead/negative flat pixels)"
    );
}
```

Add `pub floored_flat_pixels: u64` to `LightCalOutcome` and set it. In `flat_norm_constant`, after each mode computes its value `n`, guard:

```rust
if !(n.is_finite() && n > 0.0) {
    return Err(IntegrationError::BadInput(format!(
        "flat normalization constant {n} is not a positive finite number ({})",
        flat_path.display()
    )));
}
```

and in `read_ath_fnrm`'s consumer branch treat a non-finite/non-positive card value as absent (falls through to recompute).

- [ ] **Step 4:** In `api/lights.rs` where the outcome lands in the tracking row, add `floored_flat_pixels` to the per-frame `tracing::debug!`/summary counters (no schema change — log/report only).

- [ ] **Step 5: Run** `cargo test -p athenaeum-core light_cal` — PASS. **Commit** — `fix(lights): floor the flat denominator; count and warn on dead flat pixels`

### Task 6: BITPIX-aware output scale divisor (+ single engine-version bump for Phase B)

**Files:**
- Modify: `crates/athenaeum-core/src/integration/banded.rs` (expose `probe_bitpix`)
- Modify: `crates/athenaeum-core/src/calibration_library/light_cal.rs` (`LightCalInputs.scale_divisor`, use it instead of the const)
- Modify: `crates/athenaeum-core/src/api/lights.rs:900-1010` (compute divisor, thread through `card_inputs` + `inputs`)
- Modify: `crates/athenaeum-core/src/models.rs` (`LIGHT_CAL_ENGINE_VERSION` bump)

**Interfaces:**
- Produces: `pub fn probe_bitpix(path: &Path) -> Option<i32>` (banded.rs); `pub fn scale_divisor_for_bitpix(bitpix: Option<i32>) -> f64` (light_cal.rs); `LightCalInputs { …, scale_divisor: f64 }`.

- [ ] **Step 1: Failing unit test** (light_cal.rs):

```rust
#[test]
fn scale_divisor_follows_source_bit_depth() {
    assert_eq!(scale_divisor_for_bitpix(Some(8)), 255.0);
    assert_eq!(scale_divisor_for_bitpix(Some(16)), 65535.0);
    assert_eq!(scale_divisor_for_bitpix(Some(32)), 4294967295.0);
    assert_eq!(scale_divisor_for_bitpix(Some(-32)), 1.0);
    assert_eq!(scale_divisor_for_bitpix(Some(-64)), 1.0);
    assert_eq!(scale_divisor_for_bitpix(None), 65535.0); // unknown → legacy behavior
}
```

- [ ] **Step 2: Implement.**

banded.rs:

```rust
/// BITPIX of a FITS file's primary HDU, for callers that need the source bit
/// depth without opening a full BandSource (None: unreadable / non-simple —
/// same files the decode-and-spill fallback covers).
pub fn probe_bitpix(path: &Path) -> Option<i32> {
    probe_fits(path).map(|i| i.bitpix)
}
```

light_cal.rs:

```rust
/// Output scale divisor per the source's bit depth (spec §2: "the source
/// bit-depth maximum"). Float sources are already physically scaled → 1.0.
/// Unknown (spill-path formats) keeps the historic 16-bit divisor.
pub fn scale_divisor_for_bitpix(bitpix: Option<i32>) -> f64 {
    match bitpix {
        Some(8) => 255.0,
        Some(16) => 65535.0,
        Some(32) => 4294967295.0,
        Some(-32) | Some(-64) => 1.0,
        _ => OUTPUT_SCALE_DIVISOR,
    }
}
```

Add `pub scale_divisor: f64` to `LightCalInputs`; in `calibrate_light` replace both uses of `OUTPUT_SCALE_DIVISOR` (`v /= OUTPUT_SCALE_DIVISOR;` and `pedestal_dn / OUTPUT_SCALE_DIVISOR`) with `inputs.scale_divisor`. In `api/lights.rs::calibrate_one_inner`:

```rust
let scale_divisor = crate::calibration_library::light_cal::scale_divisor_for_bitpix(
    crate::integration::banded::probe_bitpix(&resolved.light_path),
);
```

then `card_inputs.scale_divisor = scale_divisor` (replacing the `OUTPUT_SCALE_DIVISOR` literal at `:946`) and set the field on `LightCalInputs`. Fix every other `LightCalInputs` construction site (tests, e2e harness) with `scale_divisor: OUTPUT_SCALE_DIVISOR`.

- [ ] **Step 3: End-to-end assertion** — extend one existing calibrate test: write the light as BITPIX −32 (float) and assert the output pixel equals `(L − D)` unscaled (divisor 1.0) and `ATH_CSCL` reads `1.0`.

- [ ] **Step 4: Bump `LIGHT_CAL_ENGINE_VERSION`** once (Phase B changes output math: floor + divisor). NOTE in the commit body: existing `light_calibrations` rows derive as *stale* → users get an honest re-calibrate prompt; this is intended.

- [ ] **Step 5: Run** `cargo test -p athenaeum-core light_cal lights` — PASS. **Commit** — `fix(lights): scale divisor follows source BITPIX; bump light-cal engine version`

---

## Phase C — Master lifecycle guards

### Task 7: `unregister_master_set` primitive

**Files:**
- Create: `crates/athenaeum-core/src/db/master_unregister.rs`
- Modify: `crates/athenaeum-core/src/db/mod.rs` (declare + re-export)

**Interfaces:**
- Produces (Tasks 8/9 consume):

```rust
pub struct MasterUnregisterSummary {
    pub master_set_id: i64,
    pub restored_raw_set_id: Option<i64>,
    pub links_repointed: usize,
    pub file_ids: Vec<i64>,
}
pub fn unregister_master_set(conn: &Connection, master_set_id: i64) -> anyhow::Result<MasterUnregisterSummary>;
pub fn master_set_id_for_file(conn: &Connection, file_id: i64) -> anyhow::Result<Option<i64>>;
```

- [ ] **Step 1: Failing test** (fixture mirrors `register_master`'s end state by hand):

```rust
#[test]
fn unregister_restores_the_raw_lineage() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::schema::init_db(&conn).unwrap();
    conn.execute_batch(
        "INSERT INTO calibration_set (id, imagetyp, is_master_library) VALUES (10, 'Dark', 0);
         INSERT INTO calibration_set (id, imagetyp, is_master_library) VALUES (11, 'MasterDark', 1);
         UPDATE calibration_set SET superseded_by_set_id = 11 WHERE id = 10;
         INSERT INTO files (id, path, filename, size, format) VALUES (1, '/lib/m.fits', 'm.fits', 1, 'fits');
         INSERT INTO frames (id, file_id, imagetyp) VALUES (100, 1, 'MASTERDARK');
         INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (11, 100);
         INSERT INTO master_provenance (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
              VALUES (11, 10, '{}', '[]', 'h', '2026-08-02T00:00:00Z');
         INSERT INTO calibration_set_to_frames (source_id, source_type, calibration_set_id, calibration_type, is_manual_override)
              VALUES (500, 'frame', 11, 'Dark', 0);",
    ).unwrap();
    let s = unregister_master_set(&conn, 11).unwrap();
    assert_eq!(s.restored_raw_set_id, Some(10));
    assert_eq!(s.links_repointed, 1);
    assert_eq!(s.file_ids, vec![1]);
    let target: i64 = conn.query_row(
        "SELECT calibration_set_id FROM calibration_set_to_frames WHERE source_id = 500", [], |r| r.get(0)).unwrap();
    assert_eq!(target, 10, "consumer link repointed back to the raw set");
    let sup: Option<i64> = conn.query_row(
        "SELECT superseded_by_set_id FROM calibration_set WHERE id = 10", [], |r| r.get(0)).unwrap();
    assert_eq!(sup, None, "raw set is matchable again");
    let masters: i64 = conn.query_row(
        "SELECT COUNT(*) FROM calibration_set WHERE id = 11", [], |r| r.get(0)).unwrap();
    assert_eq!(masters, 0, "master shell row is gone");
    assert!(unregister_master_set(&conn, 10).is_err(), "refuses non-master sets");
}
```

(Adjust INSERT column lists to the real schema; add the imported-master variant test: no raw set → links are DELETED, `restored_raw_set_id == None`.)

- [ ] **Step 2: Run — FAIL.** **Step 3: Implement:**

```rust
//! Reverse of `calibration_library::register::register_master` at the DB
//! layer: repoint consumers back onto the raw source set, un-supersede it,
//! drop provenance and the master's shell row. Runs inside the CALLER's
//! transaction; file rows / disk files are the caller's responsibility
//! (`file_ids` says which).
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub struct MasterUnregisterSummary {
    pub master_set_id: i64,
    pub restored_raw_set_id: Option<i64>,
    pub links_repointed: usize,
    pub file_ids: Vec<i64>,
}

pub fn unregister_master_set(conn: &Connection, master_set_id: i64) -> Result<MasterUnregisterSummary> {
    let is_master: i64 = conn
        .query_row(
            "SELECT COALESCE(is_master_library, 0) FROM calibration_set WHERE id = ?1",
            params![master_set_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("calibration set {master_set_id} not found"))?;
    if is_master == 0 {
        bail!("set {master_set_id} is not a master library set");
    }

    let raw_set_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM calibration_set WHERE superseded_by_set_id = ?1 ORDER BY id LIMIT 1",
            params![master_set_id],
            |r| r.get(0),
        )
        .optional()?;

    let links_repointed = match raw_set_id {
        Some(raw) => conn.execute(
            "UPDATE calibration_set_to_frames SET calibration_set_id = ?1 WHERE calibration_set_id = ?2",
            params![raw, master_set_id],
        )?,
        None => conn.execute(
            "DELETE FROM calibration_set_to_frames WHERE calibration_set_id = ?1",
            params![master_set_id],
        )?,
    };
    conn.execute(
        "UPDATE calibration_set SET superseded_by_set_id = NULL WHERE superseded_by_set_id = ?1",
        params![master_set_id],
    )?;
    conn.execute("DELETE FROM master_provenance WHERE master_set_id = ?1", params![master_set_id])?;
    // Sub-cal links the master held as a SOURCE (e.g. a master flat's dark link).
    conn.execute(
        "DELETE FROM calibration_set_to_frames WHERE source_type = 'calibration_set' AND source_id = ?1",
        params![master_set_id],
    )?;

    let mut stmt = conn.prepare(
        "SELECT fr.file_id FROM calibration_set_frames csf
           JOIN frames fr ON fr.id = csf.frame_id
          WHERE csf.set_id = ?1",
    )?;
    let file_ids: Vec<i64> = stmt
        .query_map(params![master_set_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    conn.execute("DELETE FROM calibration_set_frames WHERE set_id = ?1", params![master_set_id])?;
    conn.execute("DELETE FROM calibration_set WHERE id = ?1", params![master_set_id])?;

    tracing::info!(master_set_id, restored_raw_set_id = ?raw_set_id, links_repointed, "master unregistered");
    Ok(MasterUnregisterSummary { master_set_id, restored_raw_set_id: raw_set_id, links_repointed, file_ids })
}

pub fn master_set_id_for_file(conn: &Connection, file_id: i64) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT cs.id FROM calibration_set cs
               JOIN calibration_set_frames csf ON csf.set_id = cs.id
               JOIN frames fr ON fr.id = csf.frame_id
              WHERE fr.file_id = ?1 AND COALESCE(cs.is_master_library, 0) = 1
              LIMIT 1",
            params![file_id],
            |r| r.get(0),
        )
        .optional()?)
}
```

- [ ] **Step 4: Run — PASS. Commit** — `feat(calibration): unregister_master_set primitive — reverse of register_master`

### Task 8: `delete_master` command (core + both backends + UI)

**Files:**
- Modify: `crates/athenaeum-core/src/api/masters.rs` (new `delete_master` + `DeleteMasterResult`)
- Modify: `crates/athenaeum-tauri/src/commands/masters.rs`, `crates/athenaeum-tauri/src/lib.rs`
- Modify: `crates/athenaeum-web/src/routes/masters.rs`, `crates/athenaeum-web/src/routes/mod.rs`
- Modify: `crates/athenaeum-core/src/ts_export.rs` (register `DeleteMasterResult`), `src/types/models.ts`
- Modify: `src/components/CalibrationSetTable.tsx` (delete action on master rows via the existing `ConfirmDialog`)

**Interfaces:**
- Consumes: Task 7's `unregister_master_set`.
- Produces (wire, camelCase): `DeleteMasterResult { masterSetId, restoredRawSetId, linksRepointed, filesDeleted }`.

- [ ] **Step 1: Failing core test** (api/masters.rs tests): fixture as in Task 7 but with a real temp file on disk at `files.path`; call `delete_master`; assert file gone from disk, `files` row gone, raw set un-superseded, result counts correct; assert `Err` while `active_master_builds` holds the id.

- [ ] **Step 2: Implement core:**

```rust
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteMasterResult {
    pub master_set_id: i64,
    pub restored_raw_set_id: Option<i64>,
    pub links_repointed: usize,
    pub files_deleted: usize,
}

/// Delete a built/imported master: un-supersede + repoint consumers back to
/// the raw set (Task 7), drop catalog rows, remove the file from disk.
/// The audit's C3: without this, a bad master permanently locks its lineage.
pub fn delete_master(ctx: Arc<ServiceContext>, master_set_id: i64) -> Result<DeleteMasterResult, ApiError> {
    if ctx.active_master_builds.lock().unwrap().contains_key(&master_set_id) {
        return Err(ApiError::Invalid(format!(
            "a build for set {master_set_id} is in progress — cancel it first"
        )));
    }
    let db = db(&ctx)?;
    let conn = db.conn();
    let tx = conn.unchecked_transaction().map_err(internal)?;

    let summary = crate::db::master_unregister::unregister_master_set(&tx, master_set_id)
        .map_err(|e| { tracing::error!(master_set_id, error = %e, "delete_master: unregister failed"); ApiError::Internal(e.to_string()) })?;

    // Resolve paths, then drop the file rows (cascades frames / fits_header /
    // black_hole) inside the same transaction.
    let mut paths: Vec<String> = Vec::new();
    for fid in &summary.file_ids {
        if let Ok(p) = tx.query_row("SELECT path FROM files WHERE id = ?1", rusqlite::params![fid], |r| r.get::<_, String>(0)) {
            paths.push(p);
        }
        tx.execute("DELETE FROM files WHERE id = ?1", rusqlite::params![fid]).map_err(internal)?;
    }
    tx.commit().map_err(internal)?;

    let mut files_deleted = 0usize;
    for p in &paths {
        match std::fs::remove_file(p) {
            Ok(()) => files_deleted += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(path = %p, "delete_master: file already absent on disk");
            }
            Err(e) => tracing::error!(path = %p, error = %e, "delete_master: disk delete failed (catalog rows already removed)"),
        }
    }
    tracing::info!(master_set_id, restored = ?summary.restored_raw_set_id,
        links = summary.links_repointed, files_deleted, "master deleted; lineage restored");
    Ok(DeleteMasterResult {
        master_set_id,
        restored_raw_set_id: summary.restored_raw_set_id,
        links_repointed: summary.links_repointed,
        files_deleted,
    })
}
```

(`internal` = the module's existing error-mapping helper; reuse whatever `api/masters.rs` already uses.)

- [ ] **Step 3: Wrappers** (mirror the `start_master_build` pair verbatim in style):

```rust
// crates/athenaeum-tauri/src/commands/masters.rs
#[tauri::command]
#[tracing::instrument(skip_all, err)]
pub async fn delete_master(state: State<'_, AppState>, master_set_id: i64) -> Result<DeleteMasterResult, String> {
    api::delete_master(state.ctx.clone(), master_set_id).map_err(|e| e.to_string())
}

// crates/athenaeum-web/src/routes/masters.rs
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteMasterArgs { pub master_set_id: i64 }

#[tracing::instrument(skip_all, err(Debug))]
pub async fn delete_master(
    State(state): State<WebAppState>,
    Json(args): Json<DeleteMasterArgs>,
) -> Result<Json<DeleteMasterResult>, (StatusCode, String)> {
    api::delete_master(state.ctx.clone(), args.master_set_id).map(Json).map_err(api_err)
}
```

Register in `invoke_handler![]` and `build_router`; add `DeleteMasterResult` to `ts_export.rs` and `src/types/models.ts`.

- [ ] **Step 4: UI.** In `src/components/CalibrationSetTable.tsx`, on rows where `set.is_master_library`, add a `Trash2` (lucide) icon button styled `text-error`; clicking opens the existing `ConfirmDialog` (`src/components/ConfirmDialog.tsx`) with title "Delete master?", body naming the file and stating "the raw source set becomes matchable again"; on confirm:

```tsx
const res = await api.invoke<DeleteMasterResult>('delete_master', { masterSetId: set.id });
notify({
  title: 'Master deleted',
  detail: res.restoredRawSetId != null
    ? `Raw set #${res.restoredRawSetId} restored; ${res.linksRepointed} link(s) repointed`
    : `${res.linksRepointed} link(s) removed (imported master)`,
  kind: 'files', tone: 'success',
});
onRefresh?.();
```

with a `.catch` that logs `console.error` AND notifies `tone: 'warning', hasErrors: true`.

- [ ] **Step 5: Gates** — `cargo test -p athenaeum-core masters`, `cargo build --workspace`, `npx tsc --noEmit`. **Commit** — `feat(calibration): delete_master command with un-supersede + lineage restore (both backends, UI)`

### Task 9: Black Hole interception for master files

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations_blackhole.rs` (`move_to_black_hole` single-file fn, `bulk_move_to_black_hole` loop, `send_to_void`)

**Interfaces:**
- Consumes: Task 7's `master_set_id_for_file` + `unregister_master_set`.

- [ ] **Step 1: Failing test** (operations_blackhole tests): fixture from Task 7 (master + superseded raw + consumer link + real temp file); `send_to_void(&conn, 1)`; assert raw un-superseded, link repointed to raw, master set row gone, `files` row gone. Second test: `bulk_move_to_black_hole(&conn, &[1], "test", None)` — same lineage assertions, but `files` row SURVIVES (black-holed, not voided) and `black_hole` has the row.

- [ ] **Step 2: Implement.** In all three functions, right after the file's path/row is resolved and before the destructive step, insert:

```rust
// A master's file leaving the library through the generic delete path must
// not strand its lineage (2026-08-02 audit C3): un-supersede and restore
// consumer links exactly like delete_master does.
match crate::db::master_unregister::master_set_id_for_file(conn, file_id) {
    Ok(Some(master_set_id)) => {
        if let Err(e) = crate::db::master_unregister::unregister_master_set(conn, master_set_id) {
            tracing::error!(file_id, master_set_id, error = %e,
                "failed to unregister master before black-hole/void — aborting this file");
            // send_to_void: return Err(e); bulk: push to `failed` and `continue`.
        }
    }
    Ok(None) => {}
    Err(e) => tracing::error!(file_id, error = %e, "master lookup before black-hole/void failed"),
}
```

In `bulk_move_to_black_hole` this runs inside the existing `BEGIN TRANSACTION`; in `send_to_void` it runs on the caller's conn before the row deletes (matching the function's existing statement-by-statement style). Note: `unregister_master_set` deletes the master's `calibration_set_frames` membership, so after interception the file is an ordinary catalog file — the rest of each function proceeds unchanged.

- [ ] **Step 3: Run — PASS. Commit** — `fix(calibration): black-holing/voiding a master file restores its lineage instead of stranding it`

### Task 10: Frame-set archive — master files are always Copy

**Files:**
- Modify: `crates/athenaeum-core/src/archive/planner.rs:602-632` (`collect_calibration_files` returns an is-master flag), `:103-120` (build_plan loop forces Copy)

**Interfaces:**
- Produces: `collect_calibration_files(…) -> Result<Vec<(i64, String, i64, bool)>>` (id, path, size, is_master).

- [ ] **Step 1: Failing planner test:** fixture = frame set whose light links point at a master set (one file); dispositions all `Move`; `build_plan` → the master's plan entry has `disposition == ArchiveDisposition::Copy` while a raw calibration file in the same plan keeps `Move`.

- [ ] **Step 2: Implement.** Rewrite the collector query:

```rust
let mut stmt = conn.prepare(
    "SELECT fi.id, fi.path, fi.size, MAX(COALESCE(cs.is_master_library, 0))
       FROM files fi
       JOIN frames f ON f.file_id = fi.id
       JOIN calibration_set_frames csf ON csf.frame_id = f.id
       JOIN calibration_set cs ON cs.id = csf.set_id
       JOIN calibration_set_to_frames cstf ON cstf.calibration_set_id = csf.set_id
       JOIN frames lf ON lf.id = cstf.source_id AND cstf.source_type = 'frame'
       JOIN session_members sm ON sm.frame_id = lf.id
       JOIN sessions s ON s.id = sm.session_id
       JOIN imaging_nights n ON n.id = s.imaging_night_id
      WHERE n.frames_set_id = ?1
        AND cstf.calibration_type = ?2
      GROUP BY fi.id, fi.path, fi.size
      ORDER BY fi.path",
)?;
```

and map the 4th column to `bool` (`> 0`). In the `build_plan` loop:

```rust
for (file_id, path, size, is_master) in collect_calibration_files(conn, frames_set_id, role)? {
    let disposition = if is_master && d == ArchiveDisposition::Move {
        // A library master is never moved out of the Calibration Library by a
        // frame-set archive (2026-08-02 audit C4); the zip gets a copy, the
        // library keeps the original. Skip stays skip.
        tracing::info!(file_id, path = %path, "master file: forcing Copy disposition in frame-set archive");
        ArchiveDisposition::Copy
    } else {
        d
    };
    // …existing candidate construction, using `disposition`…
}
```

- [ ] **Step 3: Run** `cargo test -p athenaeum-core archive` — PASS (fix other collector callers/tests for the new tuple). **Commit** — `fix(archive): master files are always Copy in frame-set archives (server-side)`

### Task 11: Guard the `unique_camera` recluster cascade

**Files:**
- Modify: `crates/athenaeum-core/src/db/operations.rs:430-445` (`delete_calibration_sets_for_root` affected-ids query)

- [ ] **Step 1: Failing regression test** (operations.rs tests, modeled on `delete_scan_root_preserves_master_source_lineage` at `:5199`): fixture = superseded raw set + its master, member files under a root prefix; call `delete_calibration_sets_for_root` (or its public caller) for that prefix; assert it returns `Ok`, both sets survive, and an unrelated plain raw set under the same prefix IS deleted.

- [ ] **Step 2: Implement.** Replace the affected-ids SELECT with the prune-mirroring guard (keep the surrounding cascade deletes untouched — they now only ever see prunable ids):

```rust
// Guard mirrors prune_orphaned_calibration_sets (db/schema.rs): master shells,
// superseded raw sets, and provenance-anchored sets are frozen lineage — an
// unguarded delete here either trips their NO-ACTION FKs (failing every scan
// of the root, 2026-08-02 audit C5) or CASCADE-drops archive audit rows.
let affected_set_ids: Vec<i64> = {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT csf.set_id
           FROM calibration_set_frames csf
           JOIN frames fr ON csf.frame_id = fr.id
           JOIN files f ON fr.file_id = f.id
           JOIN calibration_set cs ON cs.id = csf.set_id
          WHERE f.path >= ?1 AND (?2 IS NULL OR f.path < ?2)
            AND COALESCE(cs.is_master_library, 0) = 0
            AND cs.superseded_by_set_id IS NULL
            AND cs.id NOT IN (SELECT source_set_id FROM master_provenance
                               WHERE source_set_id IS NOT NULL)",
    )?;
    let rows = stmt.query_map(params![root_prefix, path_hi], |row| row.get(0))?;
    rows.filter_map(|r| r.ok()).collect()
};
```

- [ ] **Step 3: Run — PASS. Commit** — `fix(db): unique_camera recluster cascade spares master/superseded/provenance lineage`

---

## Phase D — Robustness + UI

### Task 12: `archive_after` chain is panic-safe

**Files:**
- Modify: `crates/athenaeum-core/src/api/masters.rs:1122-1140`

- [ ] **Step 1:** Wrap the chain call exactly like its sibling `run_build` block:

```rust
if was_new_build && recipe.archive_after {
    if let Ok(_master_set_id) = &result {
        // Same catch_unwind discipline as run_build above: a panic in the
        // archive chain must never skip handle removal / completion emission.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            archive_originals(ctx.clone(), emitter.clone(), set_id)
        })) {
            Ok(Ok(archive_op_id)) => {
                tracing::info!(set_id, archive_op_id,
                    "archive_after: queued archive-of-originals for the just-superseded source set");
            }
            Ok(Err(e)) => {
                tracing::error!(set_id, error = %e,
                    "archive_after: failed to queue archive-of-originals — the master build itself still succeeded; originals were left in place");
            }
            Err(panic) => {
                let detail = panic.downcast_ref::<&str>().map(|s| (*s).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown".to_string());
                tracing::error!(set_id, error = %detail,
                    "archive_after: PANICKED — build still reported as succeeded; originals left in place");
            }
        }
    }
}
```

- [ ] **Step 2:** `cargo build -p athenaeum-core` + existing masters tests PASS. **Commit** — `fix(masters): archive_after chain wrapped in catch_unwind (no stuck build handle)`

### Task 13: `skip_matching` candidates stay visible in "Show All"

**Files:**
- Modify: `crates/athenaeum-core/src/calibration/configurable_matcher.rs:407-415`

- [ ] **Step 1: Failing test:** set with NULL `gain`, config `gain = Exact + required`; `IncludeIncompatible` result CONTAINS the set with `passed_hard_filter == false`; `OnlyCompatible` result does NOT contain it.

- [ ] **Step 2: Implement** — delete the early `continue` block at `:407-415` entirely (its comment included). The existing `passed_hard_filter = match_result.matches && !match_result.skip_matching` (`:438`) plus the `OnlyCompatible && !passed_hard_filter → continue` gate (`:440-442`) already implement exactly the intended split; the early continue was hiding the candidate from the manual modal too (2026-08-02 audit I3 — reference in the commit body, not a code comment).

- [ ] **Step 3: Run the matcher tests — PASS** (update any test that pinned the skip-hides-everywhere behavior). **Commit** — `fix(calibration): incomparable sets surface as incompatible in Show All instead of vanishing`

### Task 14: Manual Calibration modal — explicit clear works

**Files:**
- Modify: `src/components/ManualCalibrationModal.tsx` (`handleApply`, `onApply` prop type)
- Modify: `src/components/CalibrationHierarchyView.tsx` (`handleManualCalibrationApply`)

**Interfaces:**
- Produces: `export type ManualPick = number | null | 'clear';` (modal file, exported); `onApply(flat: ManualPick, dark: ManualPick, bias: ManualPick)`.

- [ ] **Step 1: Change the modal contract:**

```tsx
export type ManualPick = number | null | 'clear';

const pick = (selected: number | null, current: number | null): ManualPick => {
  if (selected === current) return null;      // untouched
  if (selected === null) return 'clear';      // user explicitly deselected
  return selected;
};

const handleApply = () => {
  onApply(
    pick(selectedFlatId, currentFlatFromBackend),
    pick(selectedDarkId, currentDarkFromBackend),
    pick(selectedBiasId, currentBiasFromBackend),
  );
};
```

(`hasChanges` stays as is — `null !== current` already covers the clear case.)

- [ ] **Step 2: Handle 'clear' in the parent.** In `handleManualCalibrationApply`, for each of the three types replace the `xSetId !== null && …` block with:

```tsx
if (flatSetId === 'clear') {
  await api.invoke('clear_manual_calibration_override', {
    frameIds: manualModalFrameIds,
    calibrationType: 'Flat',
  });
  setManualModalCurrentFlat(null);
} else if (flatSetId !== null && flatSetId !== manualModalCurrentFlat) {
  await api.invoke('manual_assign_calibration', {
    frameIds: manualModalFrameIds,
    calibrationSetId: flatSetId,
    calibrationType: 'Flat',
  });
  setManualModalCurrentFlat(flatSetId);
}
```

(same for Dark/Bias; update the callback's parameter types to `ManualPick`).

- [ ] **Step 3:** `npx tsc --noEmit` PASS; manual smoke listed in the audit doc. **Commit** — `fix(ui): manual calibration deselect actually clears the link (per-type clear)`

### Task 15: Claim-based collision resolution for master outputs

**Files:**
- Modify: `crates/athenaeum-core/src/calibration_library/paths.rs` (new `claim_collision_free` + `release_claim`)
- Modify: `crates/athenaeum-core/src/api/masters.rs:932-948` (use claim; `create_dir_all` BEFORE the claim; `release_claim` on the error path)

**Interfaces:**
- Produces: `pub fn claim_collision_free(abs: &Path, is_taken: &dyn Fn(&str) -> bool) -> std::io::Result<PathBuf>`; `pub fn release_claim(p: &Path)`.

- [ ] **Step 1: Failing test** (paths.rs): two sequential `claim_collision_free` calls on the same base name return different paths (both files now exist as 0-byte claims); `release_claim` removes an empty claim but leaves a non-empty file alone.

- [ ] **Step 2: Implement:**

```rust
/// Resolve AND atomically claim an output path: the chosen name is created as
/// a zero-byte placeholder with `create_new`, so two concurrent builds can
/// never resolve to the same target (check-then-write race, 2026-08-02 audit
/// I7). The caller's atomic tmp+rename overwrites the placeholder; on build
/// failure call [`release_claim`].
pub fn claim_collision_free(abs: &Path, is_taken: &dyn Fn(&str) -> bool) -> std::io::Result<PathBuf> {
    fn try_claim(p: &Path) -> std::io::Result<bool> {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(p) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }
    if !is_taken(&abs.to_string_lossy()) && try_claim(abs)? {
        return Ok(abs.to_path_buf());
    }
    let stem = abs.file_stem().and_then(|s| s.to_str()).unwrap_or("master");
    let ext = abs.extension().and_then(|s| s.to_str()).unwrap_or("fits");
    for n in 2u32.. {
        let candidate = abs.with_file_name(format!("{stem}_{n}.{ext}"));
        if !is_taken(&candidate.to_string_lossy()) && try_claim(&candidate)? {
            return Ok(candidate);
        }
    }
    unreachable!()
}

/// Remove a still-empty claim placeholder (a real output is never empty).
pub fn release_claim(p: &Path) {
    if let Ok(m) = std::fs::metadata(p) {
        if m.len() == 0 {
            let _ = std::fs::remove_file(p);
        }
    }
}
```

- [ ] **Step 3: Switch the master build** in `api/masters.rs`: move the output-dir `create_dir_all` above path resolution, replace `resolve_collision_free(…)` with `claim_collision_free(…)` (map the `io::Error`), and on every post-claim failure exit of the build add `crate::calibration_library::paths::release_claim(&output_path);` (this composes with the existing "never delete a fully built file" stance — a real output is non-empty, so `release_claim` is a no-op for it). `resolve_collision_free` stays for the lights path (per-frame outputs are row-UNIQUE-protected).

- [ ] **Step 4: Run** `cargo test -p athenaeum-core paths masters` — PASS. **Commit** — `fix(masters): atomic filename claim kills the concurrent-build collision race`

### Task 16: Band budget floor respects the budget

**Files:**
- Modify: `crates/athenaeum-core/src/integration/banded.rs:216-219`

- [ ] **Step 1: Failing test:** `band_rows_for_budget(9576, 3000, 256 * 1024 * 1024)` → assert `rows * (3000 + 2) * 9576 * 4 <= 256 * 1024 * 1024` OR `rows == 1`.

- [ ] **Step 2:** change `.max(16)` to `.max(1)` with the comment:

```rust
// Floor of 1 (not 16): the floor must never override the budget — at very
// large frame counts a 16-row floor grows band memory unbounded
// (2026-08-02 audit I5). One row per band is slow but bounded.
```

- [ ] **Step 3: Run integration tests — PASS. Commit** — `fix(integration): band-row floor no longer overrides the memory budget`

### Task 17: Minors batch (notification surface, silent catch, design tokens)

**Files:**
- Modify: `src/components/CalibrationSetTable.tsx:136` — replace `alert(...)` with `notify({ title: 'Failed to load frames', detail: String(err), kind: 'files', tone: 'warning', hasErrors: true })` (import `useNotifications`).
- Modify: `src/components/CalibrationHierarchyView.tsx:96` — `.catch(() => {})` → `.catch((err) => console.error('[CalibrationHierarchyView] set_setting ui.tree_view_mode failed:', err))`.
- Modify (token sweep, mechanical): `src/components/calibration/MatchingMatrixTable.tsx` (`rose-*` → `error` token family, `orange-<n>` → `orange` token), `src/components/calibration/CalibrationSetsTable.tsx` + `src/components/calibration/LightsAnalysisTable.tsx` + `src/components/calibration/BlackholedFramesSection.tsx` (`amber-*` → `warning`), `src/components/calibration/CameraFilterTree.tsx` (`purple-400` → `purple`), `src/components/ManualCalibrationModal.tsx` (`yellow-*` → `warning`). Use opacity modifiers on the tokens (`bg-warning/20`, `text-warning`, `border-warning/40`) to keep the current visual weight.

- [ ] **Step 1:** apply all three groups; `npx tsc --noEmit` PASS; visually spot-check dark + light themes.
- [ ] **Step 2: Commit** — `fix(ui): calibration minors — notify() surface, logged catch, Nord tokens`

---

## Final gates (before merge)

- [ ] `cargo build --workspace`
- [ ] `cargo test -p athenaeum-core`
- [ ] `npx tsc --noEmit`
- [ ] Smoke list (owner): build master → *Find Calibration* keeps the master linked (dark AND flat); delete master → raw set matchable again, links restored; F8 a master file → same; archive a frame set with Move → master file still in the library, copy in the zip; toggle unique_camera on a root with a superseded set → scan succeeds; build a master from a set containing one NaN frame → build succeeds with a warning naming the count; calibrate a light against a flat with a dead pixel → finite output + warning.
- [ ] Update `CLAUDE.md` Master Calibration Library section: un-supersede now exists (`delete_master` / Black Hole interception); masters are always auto-link candidates; frame-set archives always Copy masters.

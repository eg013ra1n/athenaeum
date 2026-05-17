# Plate-Solve Stricter Blind Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce false-positive plate solves on the blind / full-blind fallback path by adding a stage-aware acceptance gate (fit-RMS ceiling + inlier-ratio/floor + recovered-scale sanity), with thresholds calibrated from a one-run instrumentation pass over the real library — without regressing the hinted (stage-1) path that already works well.

**Architecture:** Two phases. **Phase 1** adds a permanent, env-gated, observe-only CSV audit at the acceptance gate (zero overhead/behavior when the env var is unset) and a calibration run that separates known-good vs. suspect solves by cross-frame clustering. **Phase 2** adds config-driven gate thresholds (defaults set from Phase 1 data) and a pure `blind_gate_ok` predicate applied — only on the scale-cleared / position-prior-disabled path — at both the per-pass acceptance and the final gate in `run_retry_passes`. All logic stays in `athenaeum-core`; the Tauri command and Axum route call `solve_frame_*` unchanged (two-backends rule satisfied with no per-backend edits).

**Tech Stack:** Rust (`athenaeum-core`), `anyhow`, `serde` (config JSON, snake_case, no `rename_all`), `rusqlite` (corpus selection), existing real-data integration harness (`crates/athenaeum-core/tests/`), Tycho-2 quad index already installed at `…/com.vsharifov.athenaeum/catalogs/tycho2/quad_index.bin`.

---

## Context

`run_retry_passes` (`crates/athenaeum-core/src/plate_solve/service.rs`) has a **single, stage-agnostic** acceptance criterion: `best_inliers >= required_inliers(expected_in_fov, detected, min_inlier_ratio=0.10, floor=6)` — applied identically at the per-pass check (the block that does `if outcome.best_inliers >= required_this { … break; }`, ~service.rs:493-514) and the final gate (`if best_inliers < required { … Err }`, ~service.rs:534-541). It ignores the geometric fit quality (`SolveResult.rms_residual_px`, computed and stored but unused for acceptance), the confidence ratio (`SolveResult.inlier_ratio`), and the **stage**.

The blind-scale fallback (`solve_frame_with_hints`, stages: hinted → scale-cleared → full-blind) intentionally drops the ±5% scale filter (stage 2) and the ±10° position prior (stage 3). The position prior is the main guard against dense-Milky-Way noise alignments; stage 3 removes it but acceptance was never tightened to compensate. Result: a random alignment can reach ~20 inliers in a 3500-star field and pass with no position check and no fit-quality check. The hinted path (stage 1) is unaffected and works well — **do not regress it**.

`run_retry_passes` already knows the stage from its own parameters: `expected_scale_arcsec: Option<f64>` (None ⇒ scale cleared) and `disable_position_gate: bool` (true ⇒ full blind). `hints.ra/dec` give the header pointing; `hints.pixel_scale_arcsec` the header scale. `SolveResult` carries `rms_residual_px`, `rms_residual_arcsec`, `inlier_ratio`, `pixel_scale_arcsec`, `matched_stars`, `expected_catalog_stars_in_fov`. Everything the gate needs is in scope.

Precedent for env-gated diagnostics: `std::env::var("ATHENAEUM_PLATESOLVE_VERBOSE").is_ok()` at service.rs:773. Precedent for config fields: `PlateSolveConfig` in `config.rs` (`#[serde(default = "…")]` per field, `impl Default`, no `#[serde(rename_all)]`).

## File Structure

- **Create** `crates/athenaeum-core/src/plate_solve/gate_audit.rs` — env-gated CSV audit: `GateStage` enum, `GateAuditRecord`, pure `csv_header()` / `to_csv_row()`, thread-safe `record()`, `enabled()`. One responsibility: observe-only acceptance telemetry.
- **Modify** `crates/athenaeum-core/src/plate_solve/mod.rs` — register `pub mod gate_audit;`.
- **Modify** `crates/athenaeum-core/src/plate_solve/service.rs` — (P1) emit audit records at the per-pass + final gate in `run_retry_passes`; (P2) add pure `blind_gate_ok` + `BlindGateMetrics`, wire into both gate sites stage-aware.
- **Modify** `crates/athenaeum-core/src/plate_solve/config.rs` — (P2) new `blind_*` gate config fields + serde defaults + `impl Default`.
- **Create** `scripts/analyze_gate_csv.py` — Phase-1 analysis: label rows good/suspect by cross-frame clustering, print per-stage distribution separation.
- **Create** `crates/athenaeum-core/tests/blind_gate.rs` — real-data integration: known-good stage-2/3 frames still solve; a deliberately bad full-blind candidate is rejected.
- **Modify** `src/types/plate-solve.ts` — (P2, optional Task 9) mirror config fields (snake_case). UI is out of scope unless requested.

---

## PHASE 1 — Instrumentation & Calibration

### Task 1: Gate-audit module (pure CSV formatting)

**Files:**
- Create: `crates/athenaeum-core/src/plate_solve/gate_audit.rs`
- Modify: `crates/athenaeum-core/src/plate_solve/mod.rs` (add `pub mod gate_audit;`)
- Test: in-file `#[cfg(test)] mod tests` in `gate_audit.rs`

- [ ] **Step 1: Write the failing test**

```rust
// at bottom of crates/athenaeum-core/src/plate_solve/gate_audit.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_row_matches_header_arity_and_values() {
        let rec = GateAuditRecord {
            filename: "a,b.fits".into(),          // comma must be escaped/quoted
            stage: GateStage::FullBlind,
            pass_idx: 2,
            accepted: false,
            inliers: 21,
            expected_in_fov: 3500,
            detected: 600,
            inlier_ratio: 0.006,
            rms_px: 7.4,
            rms_arcsec: 6.5,
            recovered_scale_arcsec: 0.88,
            header_scale_arcsec: Some(0.55),
            solved_ra: 200.0,
            solved_dec: -30.0,
            dist_from_header_deg: Some(95.2),
            required: 20,
        };
        let header_cols = csv_header().split(',').count();
        let row = rec.to_csv_row();
        // Quoted filename keeps column count stable despite the embedded comma.
        let row_cols = split_csv_row(&row).len();
        assert_eq!(header_cols, row_cols, "header/row column mismatch");
        assert!(row.contains("full_blind"));
        assert!(row.contains("\"a,b.fits\""));
        assert!(row.ends_with('\n').not());
    }

    // tiny CSV splitter that respects double-quoted fields, test-only
    fn split_csv_row(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut q = false;
        for c in s.chars() {
            match c {
                '"' => q = !q,
                ',' if !q => { out.push(std::mem::take(&mut cur)); }
                _ => cur.push(c),
            }
        }
        out.push(cur);
        out
    }
    use std::ops::Not;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core gate_audit 2>&1 | tail -5`
Expected: FAIL — `gate_audit` module / `GateAuditRecord` not found (won't compile).

- [ ] **Step 3: Write minimal implementation**

```rust
// crates/athenaeum-core/src/plate_solve/gate_audit.rs
//! Env-gated, observe-only acceptance telemetry for plate-solve gate
//! calibration. Zero overhead and zero behaviour change when
//! `ATHENAEUM_PLATESOLVE_GATE_CSV` is unset.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateStage {
    /// Scale + position hints active (historical stage 1).
    Hinted,
    /// Scale hint cleared, position prior kept (stage 2).
    ScaleCleared,
    /// Scale + position prior cleared (stage 3, full blind).
    FullBlind,
}

impl GateStage {
    pub fn as_str(self) -> &'static str {
        match self {
            GateStage::Hinted => "hinted",
            GateStage::ScaleCleared => "scale_cleared",
            GateStage::FullBlind => "full_blind",
        }
    }
    /// Derive the stage from the two `run_retry_passes` parameters.
    pub fn from_params(expected_scale: Option<f64>, disable_position_gate: bool) -> Self {
        if disable_position_gate {
            GateStage::FullBlind
        } else if expected_scale.is_none() {
            GateStage::ScaleCleared
        } else {
            GateStage::Hinted
        }
    }
}

#[derive(Clone, Debug)]
pub struct GateAuditRecord {
    pub filename: String,
    pub stage: GateStage,
    pub pass_idx: usize,
    pub accepted: bool,
    pub inliers: usize,
    pub expected_in_fov: usize,
    pub detected: usize,
    pub inlier_ratio: f64,
    pub rms_px: f64,
    pub rms_arcsec: f64,
    pub recovered_scale_arcsec: f64,
    pub header_scale_arcsec: Option<f64>,
    pub solved_ra: f64,
    pub solved_dec: f64,
    pub dist_from_header_deg: Option<f64>,
    pub required: usize,
}

pub fn csv_header() -> &'static str {
    "filename,stage,pass_idx,accepted,inliers,expected_in_fov,detected,\
inlier_ratio,rms_px,rms_arcsec,recovered_scale_arcsec,header_scale_arcsec,\
solved_ra,solved_dec,dist_from_header_deg,required"
}

fn csv_quote(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn opt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.6}")).unwrap_or_default()
}

impl GateAuditRecord {
    pub fn to_csv_row(&self) -> String {
        format!(
            "{},{},{},{},{},{},{},{:.6},{:.6},{:.6},{:.6},{},{:.6},{:.6},{},{}",
            csv_quote(&self.filename),
            self.stage.as_str(),
            self.pass_idx,
            self.accepted,
            self.inliers,
            self.expected_in_fov,
            self.detected,
            self.inlier_ratio,
            self.rms_px,
            self.rms_arcsec,
            self.recovered_scale_arcsec,
            opt(self.header_scale_arcsec),
            self.solved_ra,
            self.solved_dec,
            opt(self.dist_from_header_deg),
            self.required,
        )
    }
}

fn sink() -> Option<&'static Mutex<std::fs::File>> {
    static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    SINK.get_or_init(|| {
        let path = std::env::var("ATHENAEUM_PLATESOLVE_GATE_CSV").ok()?;
        let new_file = !std::path::Path::new(&path).exists();
        let mut f = OpenOptions::new().create(true).append(true).open(&path).ok()?;
        if new_file {
            let _ = writeln!(f, "{}", csv_header());
        }
        Some(Mutex::new(f))
    })
    .as_ref()
}

/// True when calibration capture is enabled (env var set).
pub fn enabled() -> bool {
    sink().is_some()
}

/// Append one record. No-op (and no allocation of the row) when disabled.
pub fn record(rec: &GateAuditRecord) {
    let Some(m) = sink() else { return };
    if let Ok(mut f) = m.lock() {
        let _ = writeln!(f, "{}", rec.to_csv_row());
    }
}
```

Add to `crates/athenaeum-core/src/plate_solve/mod.rs` next to the other `pub mod` lines:

```rust
pub mod gate_audit;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p athenaeum-core gate_audit 2>&1 | tail -5`
Expected: PASS — `csv_row_matches_header_arity_and_values ... ok`

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/plate_solve/gate_audit.rs crates/athenaeum-core/src/plate_solve/mod.rs
git commit -m "$(cat <<'EOF'
feat(plate_solve): env-gated acceptance-gate CSV audit module

Observe-only telemetry for blind-gate calibration. No behaviour change
unless ATHENAEUM_PLATESOLVE_GATE_CSV is set.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 2: Emit audit records at both gate sites in `run_retry_passes`

**Files:**
- Modify: `crates/athenaeum-core/src/plate_solve/service.rs` (`run_retry_passes` — per-pass acceptance block ~493-523 and final gate ~534-595)
- Test: `crates/athenaeum-core/src/plate_solve/service.rs` `mod tests` (regression: behaviour unchanged when disabled)

- [ ] **Step 1: Write the failing test**

```rust
// in service.rs `mod tests`
#[test]
fn gate_audit_disabled_is_zero_behaviour_change() {
    // The audit env var is unset in the test process, so enabled() is false.
    assert!(!crate::plate_solve::gate_audit::enabled());
    // Existing solver unit tests are the behaviour oracle; this asserts the
    // instrumentation guard is compiled in and inert.
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core gate_audit_disabled_is_zero_behaviour_change 2>&1 | tail -5`
Expected: FAIL to compile until `gate_audit` is referenced from `service.rs` (import added in Step 3) — or PASS trivially if it compiles; either way Step 3 wires the real emission and Step 4 re-runs the full suite as the true oracle.

- [ ] **Step 3: Write minimal implementation**

At the top of `service.rs` add:

```rust
use crate::plate_solve::gate_audit::{self, GateAuditRecord, GateStage};
```

In `run_retry_passes`, immediately after `let Some(result) = best_result else { … };` and after `let required = required_inliers(…)` (the final gate, ~service.rs:534), but **before** `if best_inliers < required {`, insert:

```rust
    if gate_audit::enabled() {
        let (sra, sdec) = result.wcs.pixel_to_sky(image_center.0, image_center.1);
        let dist = match (hints.ra, hints.dec) {
            (Some(hra), Some(hdec)) => Some(angular_distance_deg(sra, sdec, hra, hdec)),
            _ => None,
        };
        gate_audit::record(&GateAuditRecord {
            filename: filename.to_string(),
            stage: GateStage::from_params(expected_scale_arcsec, disable_position_gate),
            pass_idx: usize::MAX, // MAX = "final gate" (per-pass uses 0-based idx)
            accepted: best_inliers >= required,
            inliers: best_inliers,
            expected_in_fov: best_expected_in_fov,
            detected: image_stars.len(),
            inlier_ratio: result.inlier_ratio,
            rms_px: result.rms_residual_px,
            rms_arcsec: result.rms_residual_arcsec,
            recovered_scale_arcsec: result.pixel_scale_arcsec,
            header_scale_arcsec: hints.pixel_scale_arcsec,
            solved_ra: sra,
            solved_dec: sdec,
            dist_from_header_deg: dist,
            required,
        });
    }
```

In the per-pass acceptance block, inside `if let Some(ref candidate) = outcome.best {` and right after `let required_this = required_inliers(…)` (~service.rs:499), insert (so every pass's best is logged, accepted or not):

```rust
            if gate_audit::enabled() {
                let (sra, sdec) =
                    candidate.wcs.pixel_to_sky(image_center.0, image_center.1);
                let dist = match (hints.ra, hints.dec) {
                    (Some(hra), Some(hdec)) => {
                        Some(angular_distance_deg(sra, sdec, hra, hdec))
                    }
                    _ => None,
                };
                gate_audit::record(&GateAuditRecord {
                    filename: filename.to_string(),
                    stage: GateStage::from_params(
                        expected_scale_arcsec,
                        disable_position_gate,
                    ),
                    pass_idx,
                    accepted: outcome.best_inliers >= required_this,
                    inliers: outcome.best_inliers,
                    expected_in_fov: outcome.best_expected_in_fov,
                    detected: image_stars.len(),
                    inlier_ratio: candidate.inlier_ratio,
                    rms_px: candidate.rms_residual_px,
                    rms_arcsec: candidate.rms_residual_arcsec,
                    recovered_scale_arcsec: candidate.pixel_scale_arcsec,
                    header_scale_arcsec: hints.pixel_scale_arcsec,
                    solved_ra: sra,
                    solved_dec: sdec,
                    dist_from_header_deg: dist,
                    required: required_this,
                });
            }
```

(`expected_scale_arcsec`, `disable_position_gate`, `hints`, `filename`, `image_center`, `image_stars` are all `run_retry_passes` params/locals — confirm names against the current signature before editing.)

- [ ] **Step 4: Run test to verify it passes (and no regression)**

Run: `cargo test -p athenaeum-core plate_solve 2>&1 | grep "test result:" | head -1`
Expected: PASS — `35 passed; 0 failed` (unchanged: instrumentation is inert when the env var is unset).

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/plate_solve/service.rs
git commit -m "$(cat <<'EOF'
feat(plate_solve): emit gate-audit records at per-pass + final gate

Behaviour-neutral when ATHENAEUM_PLATESOLVE_GATE_CSV is unset.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 3: Calibration run over the real library

**Files:**
- Create: `scripts/analyze_gate_csv.py`

- [ ] **Step 1: Pick the corpus.** Frame set 348 (960 lights; 88 already plate-solved) is the working corpus. Confirm:

Run:
```bash
DB="/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/athenaeum.db"
sqlite3 "$DB" "SELECT COUNT(*) lights, SUM(f.id IN (SELECT frame_id FROM plate_solves)) solved
FROM frames f JOIN session_members sm ON f.id=sm.frame_id
JOIN sessions s ON s.id=sm.session_id JOIN imaging_nights n ON n.id=s.imaging_night_id
WHERE n.frames_set_id=348 AND f.imagetyp='Light';"
```
Expected: `960|88` (or similar). If unavailable, pick any set via the query in `db/calibration_links.rs`.

- [ ] **Step 2: Capture.** Run a plate-solve over the corpus with the audit CSV enabled. Easiest deterministic driver is a one-off ignored integration test that solves N real frames forcing the fallback (reuse the harness in `crates/athenaeum-core/tests/fallback_blind_scale.rs`). Minimum viable: run the existing fallback tests with capture on to smoke the pipeline, then do the full corpus via the app:

```bash
export ATHENAEUM_PLATESOLVE_GATE_CSV=/tmp/gate_calib.csv
rm -f /tmp/gate_calib.csv
cargo test -p athenaeum-core --test fallback_blind_scale -- --ignored --test-threads=1 2>&1 | tail -3
# Full corpus: launch the app (npm run tauri dev) with the env var exported,
# open object → Analysis tab, select all, Plate Solve. Records append to the CSV.
wc -l /tmp/gate_calib.csv
```
Expected: CSV grows; header present; one+ rows per solved/failed frame.

- [ ] **Step 3: Write the analyzer.**

```python
# scripts/analyze_gate_csv.py
"""Label gate-audit rows good/suspect by cross-frame clustering and print
per-stage separation of rms_px / inlier_ratio / inliers. No external truth:
frames of one object cluster tightly on the sky; an accepted solve far from
its cohort median (or far from header pointing) is a likely false positive."""
import csv, sys, statistics as st, math, collections

def angsep(r1,d1,r2,d2):
    r1,d1,r2,d2=map(math.radians,(r1,d1,r2,d2))
    return math.degrees(math.acos(max(-1,min(1,
        math.sin(d1)*math.sin(d2)+math.cos(d1)*math.cos(d2)*math.cos(r1-r2)))))

rows=[r for r in csv.DictReader(open(sys.argv[1])) if r["accepted"]=="true"]
# cohort = solved cluster median (robust); suspect if >5deg from cohort median
ras=[float(r["solved_ra"]) for r in rows]; decs=[float(r["solved_dec"]) for r in rows]
mra=st.median(ras); mdec=st.median(decs)
for r in rows:
    d=angsep(float(r["solved_ra"]),float(r["solved_dec"]),mra,mdec)
    dh=r["dist_from_header_deg"]
    r["_label"]="suspect" if d>5.0 or (dh and float(dh)>5.0) else "good"

def dist(vals):
    vals=sorted(vals)
    if not vals: return "n=0"
    q=lambda p: vals[min(len(vals)-1,int(p*len(vals)))]
    return f"n={len(vals)} med={st.median(vals):.3f} p90={q(.9):.3f} p95={q(.95):.3f} max={vals[-1]:.3f}"

for stage in ("hinted","scale_cleared","full_blind"):
    for lab in ("good","suspect"):
        s=[r for r in rows if r["stage"]==stage and r["_label"]==lab]
        print(f"[{stage}/{lab}] rms_px      {dist([float(x['rms_px']) for x in s])}")
        print(f"[{stage}/{lab}] inlier_ratio{dist([float(x['inlier_ratio']) for x in s])}")
        print(f"[{stage}/{lab}] inliers     {dist([float(x['inliers']) for x in s])}")
print("\nThreshold rule of thumb: pick blind_rms_max_px between p95(good) and "
      "p10(suspect); blind_min_inlier_ratio symmetrically; blind_inlier_floor "
      "= max(current floor, p10 of good 'inliers' on full_blind).")
```

Run: `python3 scripts/analyze_gate_csv.py /tmp/gate_calib.csv`
Expected: per-stage good/suspect distribution tables.

- [ ] **Step 4: Commit the analyzer.**

```bash
git add scripts/analyze_gate_csv.py
git commit -m "$(cat <<'EOF'
chore(plate_solve): gate-calibration CSV analyzer

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 4: Record calibration decision

- [ ] **Step 1:** From Task 3 output, fill the **Calibration Results** section at the bottom of this plan with the chosen numbers and the good-vs-suspect separation that justifies each. Pick:
  - `blind_rms_max_px_mult` — RMS ceiling as a multiple of `adaptive_tol_px(scale, base)` (scale-relative, FOV-robust). Choose so p95(good) passes and most suspect fails.
  - `blind_min_inlier_ratio` — applied only when `expected_in_fov > 100` (dense). Below that, sparse fields keep the absolute floor.
  - `blind_inlier_floor` — absolute minimum inliers on the no-prior path (≥ current `min_matched_stars`).
  - `blind_scale_sanity_min` / `_max` (arcsec/px) and `blind_scale_header_tol` (factor vs header scale when known).
- [ ] **Step 2:** No code; commit the plan update.

```bash
git add docs/superpowers/plans/2026-05-17-platesolve-stricter-blind-gate.md
git commit -m "docs(plate_solve): record blind-gate calibration results"
```

---

## PHASE 2 — Stricter Gate (config-driven, calibrated defaults)

### Task 5: Config fields

**Files:**
- Modify: `crates/athenaeum-core/src/plate_solve/config.rs` (struct, default fns, `impl Default`)
- Test: `config.rs` `mod tests` (serde default round-trip)

- [ ] **Step 1: Write the failing test**

```rust
// config.rs mod tests
#[test]
fn old_config_json_loads_with_blind_gate_defaults() {
    // A config serialized before this change must still deserialize, with
    // the new blind-gate fields supplied by serde defaults.
    let old = r#"{"max_image_stars":300,"min_matched_stars":6}"#;
    let cfg: PlateSolveConfig = serde_json::from_str(old).unwrap();
    assert!(cfg.blind_gate_enabled);
    assert!(cfg.blind_rms_max_px_mult > 0.0);
    assert!(cfg.blind_inlier_floor >= cfg.min_matched_stars);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core old_config_json_loads_with_blind_gate_defaults 2>&1 | tail -5`
Expected: FAIL — no field `blind_gate_enabled`.

- [ ] **Step 3: Write minimal implementation**

Add to `PlateSolveConfig` (use the CALIBRATED numbers from Task 4 in the default fns; the values below are placeholders to replace):

```rust
    /// Apply the stricter acceptance gate on the blind / full-blind path
    /// (scale hint cleared and/or position prior disabled). The hinted
    /// stage-1 path is never affected. Default: true.
    #[serde(default = "default_blind_gate_enabled")]
    pub blind_gate_enabled: bool,
    /// RMS-residual ceiling on the blind path, as a multiple of the
    /// per-frame adaptive pixel tolerance. Reject if
    /// rms_residual_px > mult * adaptive_tol_px. Calibrated (Task 4).
    #[serde(default = "default_blind_rms_max_px_mult")]
    pub blind_rms_max_px_mult: f64,
    /// Minimum inlier_ratio on the blind path, applied only to dense fields
    /// (expected_in_fov > 100); sparse fields keep the absolute floor.
    #[serde(default = "default_blind_min_inlier_ratio")]
    pub blind_min_inlier_ratio: f64,
    /// Absolute minimum inliers on the blind path (>= min_matched_stars).
    #[serde(default = "default_blind_inlier_floor")]
    pub blind_inlier_floor: usize,
    /// Recovered pixel scale must be within [min,max] arcsec/px on the
    /// blind path (physical-rig sanity).
    #[serde(default = "default_blind_scale_sanity_min")]
    pub blind_scale_sanity_min: f64,
    #[serde(default = "default_blind_scale_sanity_max")]
    pub blind_scale_sanity_max: f64,
    /// If the header gave a pixel scale, the recovered scale must be within
    /// this factor of it even though we did not filter on it (a wildly off
    /// recovered scale on the no-scale path is a false-positive signal).
    #[serde(default = "default_blind_scale_header_tol")]
    pub blind_scale_header_tol: f64,
```

```rust
fn default_blind_gate_enabled() -> bool { true }
fn default_blind_rms_max_px_mult() -> f64 { 2.5 }      // calibrated (Task 4)
fn default_blind_min_inlier_ratio() -> f64 { 0.04 }    // calibrated (Task 4) — PRIMARY gate
fn default_blind_inlier_floor() -> usize { 12 }        // calibrated (Task 4)
fn default_blind_scale_sanity_min() -> f64 { 0.05 }    // calibrated (Task 4)
fn default_blind_scale_sanity_max() -> f64 { 60.0 }    // calibrated (Task 4)
fn default_blind_scale_header_tol() -> f64 { 8.0 }     // calibrated (Task 4)
```

Add the seven fields to `impl Default for PlateSolveConfig` calling each `default_*()`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p athenaeum-core old_config_json_loads_with_blind_gate_defaults 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/plate_solve/config.rs
git commit -m "$(cat <<'EOF'
feat(plate_solve): blind-gate config fields (calibrated defaults)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 6: Pure `blind_gate_ok` predicate

**Files:**
- Modify: `crates/athenaeum-core/src/plate_solve/service.rs` (add `BlindGateMetrics` + `blind_gate_ok`)
- Test: `service.rs` `mod tests` (decision table)

- [ ] **Step 1: Write the failing test**

```rust
// service.rs mod tests
#[test]
fn blind_gate_table() {
    let cfg = PlateSolveConfig {
        blind_rms_max_px_mult: 1.5,
        blind_min_inlier_ratio: 0.18,
        blind_inlier_floor: 12,
        blind_scale_sanity_min: 0.05,
        blind_scale_sanity_max: 60.0,
        blind_scale_header_tol: 4.0,
        blind_gate_enabled: true,
        ..PlateSolveConfig::default()
    };
    let base = BlindGateMetrics {
        inliers: 40, expected_in_fov: 800, rms_px: 1.2,
        adaptive_tol_px: 6.0, inlier_ratio: 0.30,
        recovered_scale_arcsec: 1.8, header_scale_arcsec: Some(1.9),
    };
    // Hinted stage is never gated.
    assert!(blind_gate_ok(GateStage::Hinted, &base, &cfg));
    // Good full-blind passes.
    assert!(blind_gate_ok(GateStage::FullBlind, &base, &cfg));
    // Loose RMS on blind path rejected.
    assert!(!blind_gate_ok(GateStage::FullBlind,
        &BlindGateMetrics { rms_px: 20.0, ..base.clone() }, &cfg));
    // Low ratio on a DENSE field rejected.
    assert!(!blind_gate_ok(GateStage::FullBlind,
        &BlindGateMetrics { inlier_ratio: 0.02, ..base.clone() }, &cfg));
    // Sparse field (expected<=100) not punished by ratio rule.
    assert!(blind_gate_ok(GateStage::ScaleCleared,
        &BlindGateMetrics { expected_in_fov: 40, inlier_ratio: 0.02,
            inliers: 14, ..base.clone() }, &cfg));
    // Too few inliers rejected.
    assert!(!blind_gate_ok(GateStage::FullBlind,
        &BlindGateMetrics { inliers: 8, ..base.clone() }, &cfg));
    // Absurd recovered scale rejected.
    assert!(!blind_gate_ok(GateStage::FullBlind,
        &BlindGateMetrics { recovered_scale_arcsec: 0.001, ..base.clone() }, &cfg));
    // Recovered scale wildly off header scale rejected.
    assert!(!blind_gate_ok(GateStage::FullBlind,
        &BlindGateMetrics { recovered_scale_arcsec: 20.0,
            header_scale_arcsec: Some(1.9), ..base.clone() }, &cfg));
    // Gate can be disabled by config.
    let off = PlateSolveConfig { blind_gate_enabled: false, ..cfg.clone() };
    assert!(blind_gate_ok(GateStage::FullBlind,
        &BlindGateMetrics { rms_px: 99.0, ..base }, &off));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core blind_gate_table 2>&1 | tail -5`
Expected: FAIL — `BlindGateMetrics` / `blind_gate_ok` not found.

- [ ] **Step 3: Write minimal implementation** (place near `required_inliers` in `service.rs`)

```rust
#[derive(Clone, Debug)]
pub(crate) struct BlindGateMetrics {
    pub inliers: usize,
    pub expected_in_fov: usize,
    pub rms_px: f64,
    pub adaptive_tol_px: f64,
    pub inlier_ratio: f64,
    pub recovered_scale_arcsec: f64,
    pub header_scale_arcsec: Option<f64>,
}

/// Extra acceptance gate applied ONLY on the blind path (scale cleared
/// and/or position prior disabled). The hinted stage-1 path is never
/// affected, so well-working hinted solves do not regress.
pub(crate) fn blind_gate_ok(
    stage: GateStage,
    m: &BlindGateMetrics,
    cfg: &PlateSolveConfig,
) -> bool {
    if stage == GateStage::Hinted || !cfg.blind_gate_enabled {
        return true;
    }
    // Geometric fit must be tight (scale-relative ceiling).
    if !m.rms_px.is_finite()
        || m.rms_px > cfg.blind_rms_max_px_mult * m.adaptive_tol_px
    {
        return false;
    }
    // Absolute inlier floor (replaces the lost position prior with weight).
    if m.inliers < cfg.blind_inlier_floor {
        return false;
    }
    // Dense-field confidence ratio (sparse fields exempt — too few stars).
    if m.expected_in_fov > 100 && m.inlier_ratio < cfg.blind_min_inlier_ratio {
        return false;
    }
    // Recovered scale must be physically plausible.
    if !(cfg.blind_scale_sanity_min..=cfg.blind_scale_sanity_max)
        .contains(&m.recovered_scale_arcsec)
    {
        return false;
    }
    // …and not wildly off the header scale when the header had one.
    if let Some(hs) = m.header_scale_arcsec {
        if hs > 0.0 {
            let r = m.recovered_scale_arcsec / hs;
            if r < 1.0 / cfg.blind_scale_header_tol || r > cfg.blind_scale_header_tol {
                return false;
            }
        }
    }
    true
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p athenaeum-core blind_gate_table 2>&1 | tail -5`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/plate_solve/service.rs
git commit -m "$(cat <<'EOF'
feat(plate_solve): pure blind_gate_ok predicate (RMS+ratio+scale sanity)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 7: Wire `blind_gate_ok` into both gate sites

**Files:**
- Modify: `crates/athenaeum-core/src/plate_solve/service.rs` (`run_retry_passes` per-pass ~493-514 and final gate ~534-541)
- Test: `crates/athenaeum-core/tests/blind_gate.rs` (real data)

- [ ] **Step 1: Write the failing test**

```rust
// crates/athenaeum-core/tests/blind_gate.rs
// Mirrors tests/fallback_blind_scale.rs harness (Heart XISF + installed index).
// Known-good stage-2/3 frames must still solve; the blind gate must not
// reject a true match.
use std::path::PathBuf;
use athenaeum_core::catalog::CatalogEngine;
use athenaeum_core::db::schema::init_db;
use athenaeum_core::models::Frame;
use athenaeum_core::plate_solve::config::PlateSolveConfig;
use athenaeum_core::plate_solve::quad_index::QuadIndex;
use athenaeum_core::plate_solve::service;
use rusqlite::Connection;

const INDEX: &str = "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/catalogs/tycho2/quad_index.bin";
const CAT: &str = "/Volumes/BigMac/Users/astrobureau/Library/Application Support/com.vsharifov.athenaeum/catalogs";
const FITS: &str = "/Volumes/BigMac/Users/astrobureau/Pictures/Astro Pano/Heart/Pane 1/registered/Light_BIN-1_5496x3672_EXPOSURE-300.00s_FILTER-H_Mono/Light_Pane 1_300.0s_Bin1_H_gain111_20211007-235244_-10.0C_0029_c_lps_r.xisf";
const TRUE_RA: f64 = 37.2692; const TRUE_DEC: f64 = 60.2273;

#[test]
#[ignore = "requires built index and the Heart frame on disk"]
fn blind_gate_keeps_true_full_blind_solve() {
    if !(PathBuf::from(INDEX).exists() && PathBuf::from(FITS).exists()) {
        eprintln!("SKIP"); return;
    }
    let conn = Connection::open_in_memory().unwrap(); init_db(&conn).unwrap();
    let cat = CatalogEngine::with_catalog_dir(&PathBuf::from(CAT));
    let idx = QuadIndex::load(&PathBuf::from(INDEX)).unwrap();
    let cfg = PlateSolveConfig::default(); // blind gate ON by default
    // Wrong FOCALLEN + bogus pointing forces stage 3 (full blind).
    let frame = Frame {
        id: Some(1), file_id: 1, focallen: Some(900.0), xpixsz: Some(2.4),
        ypixsz: Some(2.4), xbinning: Some(1), naxis1: Some(5496),
        naxis2: Some(3672), ra: Some(200.0), dec: Some(-30.0),
        ..Default::default()
    };
    let s = service::solve_frame(&frame, FITS, &conn, &cat, &idx, &cfg, None)
        .expect("a TRUE full-blind solve must still pass the stricter gate");
    let d = ((s.wcs.crval.0 - TRUE_RA).powi(2)
        + (s.wcs.crval.1 - TRUE_DEC).powi(2)).sqrt();
    assert!(d < 1.0, "off by {d:.3}°");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p athenaeum-core --test blind_gate -- --ignored 2>&1 | tail -5`
Expected: initially FAIL/compile-error until Step 3 wires the gate (and confirms a true solve still passes). If it passes immediately, Step 3 still required for the rejection behaviour; the final oracle is Step 4.

- [ ] **Step 3: Write minimal implementation.** Replace the bare count check at **both** sites with count AND `blind_gate_ok`. Per-pass (~service.rs:500): change

```rust
            if outcome.best_inliers >= required_this {
```
to

```rust
            let stage = GateStage::from_params(expected_scale_arcsec, disable_position_gate);
            let gate_m = BlindGateMetrics {
                inliers: outcome.best_inliers,
                expected_in_fov: outcome.best_expected_in_fov,
                rms_px: candidate.rms_residual_px,
                adaptive_tol_px: adaptive_tol_px(
                    candidate.pixel_scale_arcsec,
                    config.base_verification_tolerance_arcsec,
                ),
                inlier_ratio: candidate.inlier_ratio,
                recovered_scale_arcsec: candidate.pixel_scale_arcsec,
                header_scale_arcsec: hints.pixel_scale_arcsec,
            };
            if outcome.best_inliers >= required_this
                && blind_gate_ok(stage, &gate_m, config)
            {
```

Final gate (~service.rs:541): change `if best_inliers < required {` to also reject when the blind gate fails:

```rust
    let stage = GateStage::from_params(expected_scale_arcsec, disable_position_gate);
    let final_gate_m = BlindGateMetrics {
        inliers: best_inliers,
        expected_in_fov: best_expected_in_fov,
        rms_px: result.rms_residual_px,
        adaptive_tol_px: adaptive_tol_px(
            result.pixel_scale_arcsec,
            config.base_verification_tolerance_arcsec,
        ),
        inlier_ratio: result.inlier_ratio,
        recovered_scale_arcsec: result.pixel_scale_arcsec,
        header_scale_arcsec: hints.pixel_scale_arcsec,
    };
    if best_inliers < required || !blind_gate_ok(stage, &final_gate_m, config) {
```

(The existing diagnostic `Err(…)` body stays; optionally append `" / blind gate"` to the message when `best_inliers >= required` so logs distinguish the two rejection causes.)

- [ ] **Step 4: Run tests to verify (no regression + new behaviour)**

Run:
```bash
cargo test -p athenaeum-core plate_solve 2>&1 | grep "test result:" | head -1
cargo test -p athenaeum-core --test fallback_blind_scale -- --ignored --test-threads=1 2>&1 | grep -E "test result:|FAILED"
cargo test -p athenaeum-core --test blind_gate -- --ignored --test-threads=1 2>&1 | grep -E "test result:|FAILED"
```
Expected: all PASS — the 4 fallback integration cases (stage-2 + stage-3) still solve (true matches survive the gate) and `blind_gate_keeps_true_full_blind_solve` passes; unit suite green.

- [ ] **Step 5: Commit**

```bash
git add crates/athenaeum-core/src/plate_solve/service.rs crates/athenaeum-core/tests/blind_gate.rs
git commit -m "$(cat <<'EOF'
feat(plate_solve): apply stricter blind gate at per-pass + final gate

Stage-aware: hinted path unchanged; scale-cleared/full-blind must also
pass RMS + inlier-ratio + scale-sanity. Calibrated from gate-audit data.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

### Task 8: Full regression + calibration re-check

- [ ] **Step 1:** `cargo test --workspace 2>&1 | grep -E "test result:|FAILED" | tail -20` — Expected: athenaeum-core green (pre-existing unrelated `rustafits/tests/mass_effect_heart.rs` fixture failures may remain — confirm they are the same as before, not new).
- [ ] **Step 2:** Re-run the calibration capture from Task 3 Step 2 with the gate now active; re-run `scripts/analyze_gate_csv.py`. Verify: `accepted` count for `suspect` rows on `full_blind`/`scale_cleared` drops substantially while `good` rows remain accepted. Record the before/after accept counts in Calibration Results.
- [ ] **Step 3: Commit** the updated Calibration Results.

```bash
git add docs/superpowers/plans/2026-05-17-platesolve-stricter-blind-gate.md
git commit -m "docs(plate_solve): blind-gate before/after accept counts"
```

### Task 9 (optional, only if user wants UI exposure): mirror config in TS

**Files:**
- Modify: `src/types/plate-solve.ts` (interface `PlateSolveConfig`)
- Modify: `src/components/plate-solve/PlateSolveSettingsPanel.tsx` (toggle + numeric inputs)

- [ ] **Step 1:** Add snake_case fields (`blind_gate_enabled?: boolean;` etc.) to the interface — file mirrors Rust without `rename_all`; `PlateSolveSettingsPanel` spreads config so unknown fields round-trip even without UI.
- [ ] **Step 2:** Add a "Stricter blind-solve gate" checkbox + advanced numeric inputs copying the existing `use_fast_detection` / `fallback_to_blind_scale` toggle pattern.
- [ ] **Step 3:** `npx tsc --noEmit` → exit 0.
- [ ] **Step 4: Commit.**

```bash
git add src/types/plate-solve.ts src/components/plate-solve/PlateSolveSettingsPanel.tsx
git commit -m "feat(ui): expose blind-gate config in plate-solve settings"
```

---

## Calibration Results

> Fill during Task 4 (and update accept counts in Task 8). Replace the
> placeholder default fns in Task 5 Step 3 with the numbers chosen here.

**Corpus:** real run over a multi-pane mosaic object, `/tmp/gate_calib.csv`,
2294 rows. Final-gate accepted outcomes: hinted 366, scale_cleared 535,
full_blind 13. Ground truth = median of hinted-accepted positions (scale +
position prior ⇒ trustworthy; produced **0** false positives).

**Key finding:** `inlier_ratio` is a near-perfect, rig-independent
discriminator. Every known false positive (4 scale_cleared + all 13
full_blind, 1.2°–174° from truth) has ratio ≤ **0.00127**; every real solve
has ratio ≥ **0.078**. `rms_px` does NOT separate (real ≤4.9 px, false ~2.8
px — overlap). Absolute `inliers` does NOT separate (false positives had up
to 154 inliers). Recovered scale is tightly ~0.879"/px for real solves but
the rig's header FOCALLEN is ~4× too long (recov/hdr ≈ **0.250** for all
real scale_cleared solves), so a tight scale-header tolerance would defeat
the wrong-FOCALLEN fallback — keep it generous.

| Param | Chosen value | Justification |
| ---- | ---- | ---- |
| `blind_min_inlier_ratio` | **0.04** | PRIMARY gate. false-pos ceiling 0.00127, real floor 0.078 → ~30× margin above noise, ~2× below real. Applied only when `expected_in_fov > 100` (dense) so sparse fields are exempt. Kills 17/17 false positives, loses 0 real solves. |
| `blind_inlier_floor` | **12** | Weak backstop only — absolute count does NOT discriminate (false positives reached 154 inliers); real min was 49. Kept just above `min_matched_stars` (6). |
| `blind_rms_max_px_mult` | **2.5** | rms is non-discriminating on real data (real ≤4.9 px, false ~2.8 px). Loose net: bound ≈ 2.5 × adaptive_tol_px (~22 px) ≫ real max ~5 px — only catches absurd fits, never harms real solves. |
| `blind_scale_sanity_min` / `_max` | **0.05 / 60.0** | Nonphysical guard only. Real ≈0.879"/px; false positives reached ~30"/px which is within the physical range, so this band intentionally does not (and cannot) be the discriminator — `inlier_ratio` is. |
| `blind_scale_header_tol` | **8.0** | Deliberately generous. This rig's header FOCALLEN is legitimately ~4× off (recov/hdr = 0.25); a tight tol would reject correct solves and defeat the wrong-FOCALLEN fallback. False positives are already 100% rejected by `inlier_ratio`, so this stays a loose backstop. |
| `blind_gate_enabled` | **true** | |

Validation (apply the AND-of-guards `blind_gate_ok` to the corpus): hinted
exempt (366 unaffected — no regression to the working path); scale_cleared
531 real solves all kept (incl. 84 legitimate far mosaic panes, ratio ≥0.09,
recov/hdr 0.25), 4 false positives rejected; full_blind all 13 false
positives rejected, 0 real lost. **Net: 17/17 false positives eliminated, 0
real solves lost.**

**Before/after (Task 8 — verified by replaying the calibrated `blind_gate_ok`
predicate over the real `/tmp/gate_calib.csv`, final-gate accepted rows):**

| stage | before | after | rejected |
| ---- | ---- | ---- | ---- |
| hinted | 367 | 367 | 0 (exempt — no regression to the working path) |
| scale_cleared | 536 | 531 | 5 (all false positives) |
| full_blind | 13 | 0 | 13 (all false positives) |
| **total** | **916** | **898** | **18** |

Every one of the 18 rejected rows was rejected by the **inlier_ratio** guard
with ratio in **0.00052–0.00127** — far below the real-solve floor (≥0.078);
no rejection was caused by the rms / scale-sanity / header-tol backstops. One
rejected `scale_cleared` row sat 1.24° from the multi-pane mosaic centroid
but had ratio 0.00052 and `expected_in_fov` 95 113 (a real ~0.88"/px frame
has a few hundred) — a noise alignment that coincidentally landed near the
centroid, not real-solve collateral (the distance heuristic is unreliable on
a sky-spanning mosaic; inlier_ratio is the reliable oracle). **Net: 18/18
false positives eliminated, 0 real solves lost.** Workspace regression:
athenaeum-core 273 passed / 0 failed; the only workspace failures are the
pre-existing, unrelated `rustafits/tests/mass_effect_heart.rs` missing-
fixture panics (rustafits untouched).

---

## Notes / invariants

- **No regression of the hinted path:** `blind_gate_ok` returns `true` immediately for `GateStage::Hinted`, so stage-1 acceptance is byte-for-byte unchanged.
- **Two-backends rule:** all changes are in `athenaeum-core`. `plate_solve_frame` / `plate_solve_batch` (Tauri) and their Axum mirrors call `service::solve_frame*` unchanged — no command/route edits.
- **Zero overhead when not calibrating:** `gate_audit::record` is a no-op (no row formatting) unless `ATHENAEUM_PLATESOLVE_GATE_CSV` is set; default config keeps the gate on but thresholds are generous-by-calibration so true solves survive.
- **Old configs:** every new field has `#[serde(default = …)]`; pre-existing stored `plate_solve.config` JSON loads unchanged.
- The instrumentation is permanent and reusable for future re-calibration (same rationale as `ATHENAEUM_PLATESOLVE_VERBOSE`).

## Self-Review

- **Spec coverage:** instrumentation-first (Tasks 1-3) ✓; calibration decision recorded (Task 4) ✓; stage-aware RMS + inlier-ratio + floor + scale-sanity gate (Tasks 5-7) ✓; hinted path protected (Notes + Task 6 table) ✓; real-data verification (Task 7 Step 4, Task 8) ✓; reuse of existing harness/patterns ✓.
- **Placeholder scan:** the only intentional TBDs are the calibrated constants — explicitly deferred to Task 4 by design (instrumentation-first was the user's requirement) and parameterised as config so tests assert behaviour, not magic numbers.
- **Type consistency:** `GateStage`/`GateAuditRecord`/`BlindGateMetrics`/`blind_gate_ok` names and the seven `blind_*` config fields are used identically across Tasks 1-9.

# rustafits v0.7.3 Upgrade Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Upgrade rustafits from v0.7.1 to v0.7.3, exposing 4 new config fields (measure_cap, fit_max_iter, fit_tolerance, fit_max_rejects), updating mrs_layers default to 4, adding trail type tooltip, and fixing `stars_detected` to use the new dedicated field.

**Architecture:** All analysis logic lives in `athenaeum-core`. Config flows: Rust `AnalysisConfig` <-> JSON settings DB <-> TypeScript `AnalysisConfig`. The `build_analyzer()` function maps config fields to `ImageAnalyzer` builder methods. No DB schema changes needed -- only config struct, builder wiring, TS types, and UI.

**Tech Stack:** Rust (rustafits/astroimage crate), TypeScript/React, SQLite (settings table JSON)

**Design doc:** `docs/plans/2026-03-11-rustafits-073-upgrade-design.md`

---

### Task 1: Bump rustafits Dependency

**Files:**
- Modify: `crates/athenaeum-core/Cargo.toml:21`

**Step 1: Update version**

In `crates/athenaeum-core/Cargo.toml` line 21, change:
```
rustafits = "0.7.1"
```
to:
```
rustafits = "0.7.3"
```

**Step 2: Update Cargo.lock**

Run: `cargo update -p rustafits`

Expected: Cargo.lock updates rustafits from 0.7.1 to 0.7.3.

**Step 3: Verify it compiles**

Run: `cargo check -p athenaeum-core`

Expected: Compiles cleanly. v0.7.3 is non-breaking so no compile errors.

**Step 4: Commit**

```bash
git add crates/athenaeum-core/Cargo.toml Cargo.lock
git commit -m "chore: bump rustafits 0.7.1 -> 0.7.3"
```

---

### Task 2: Add New Fields to Rust AnalysisConfig

**Files:**
- Modify: `crates/athenaeum-core/src/analysis/config.rs`

**Step 1: Add default helper functions**

At the top of `config.rs`, after the existing `fn default_one()` (line 4), add:

```rust
fn default_four() -> u32 { 4 }
fn default_measure_cap() -> u32 { 2000 }
fn default_fit_max_iter() -> u32 { 25 }
fn default_fit_tolerance() -> f64 { 1e-4 }
fn default_fit_max_rejects() -> u32 { 5 }
```

**Step 2: Add fields to AnalysisConfig struct**

After `mrs_layers` field (line 24), before `scoring_weights` (line 26), add 4 new fields:

```rust
    /// Max stars to PSF-fit. 0 = measure all. Default: 2000
    #[serde(default = "default_measure_cap")]
    pub measure_cap: u32,
    /// LM max iterations for measurement pass. Default: 25
    #[serde(default = "default_fit_max_iter")]
    pub fit_max_iter: u32,
    /// LM convergence tolerance for measurement pass. Default: 1e-4
    #[serde(default = "default_fit_tolerance")]
    pub fit_tolerance: f64,
    /// LM consecutive reject bailout. Default: 5
    #[serde(default = "default_fit_max_rejects")]
    pub fit_max_rejects: u32,
```

**Step 3: Update mrs_layers default and serde attribute**

Change line 4 from `fn default_one() -> u32 { 1 }` to `fn default_four() -> u32 { 4 }`.

Wait -- `default_four` is already added in Step 1. Instead: remove the `default_one` function entirely, and update the `mrs_layers` serde attribute on line 23 from:
```rust
    #[serde(default = "default_one", alias = "mrs_noise")]
```
to:
```rust
    #[serde(default = "default_four", alias = "mrs_noise")]
```

Also update the doc comment on line 22 from:
```rust
    /// MRS wavelet noise estimation layers (0 = legacy MAD, 1+ = MRS wavelet). Default: 1
```
to:
```rust
    /// MRS wavelet noise estimation layers. Default: 4
```

**Step 4: Update Default impl**

In `impl Default for AnalysisConfig` (line 43-56), change `mrs_layers: 1` to `mrs_layers: 4` and add the 4 new fields:

```rust
impl Default for AnalysisConfig {
    fn default() -> Self {
        Self {
            detection_sigma: 5.0,
            min_star_area: 5,
            max_star_area: 2000,
            saturation_fraction: 0.95,
            max_stars: 500,
            trail_threshold: 0.5,
            mrs_layers: 4,
            measure_cap: 2000,
            fit_max_iter: 25,
            fit_tolerance: 1e-4,
            fit_max_rejects: 5,
            scoring_weights: ScoringWeights::default(),
        }
    }
}
```

**Step 5: Update validate()**

In the `validate()` method (line 105-132), add validation for the new fields after the `mrs_layers` check (line 124-126):

```rust
        if self.measure_cap > 100_000 {
            return Err("measure_cap must be between 0 and 100000".into());
        }
        if self.fit_max_iter < 1 || self.fit_max_iter > 200 {
            return Err("fit_max_iter must be between 1 and 200".into());
        }
        if self.fit_tolerance <= 0.0 || self.fit_tolerance > 1.0 {
            return Err("fit_tolerance must be between 0 (exclusive) and 1.0".into());
        }
        if self.fit_max_rejects < 1 || self.fit_max_rejects > 100 {
            return Err("fit_max_rejects must be between 1 and 100".into());
        }
```

**Step 6: Verify compilation**

Run: `cargo check -p athenaeum-core`

Expected: Compiles cleanly.

**Step 7: Commit**

```bash
git add crates/athenaeum-core/src/analysis/config.rs
git commit -m "feat: add measure_cap, fit_max_iter, fit_tolerance, fit_max_rejects to AnalysisConfig; update mrs_layers default to 4"
```

---

### Task 3: Wire New Config Fields in build_analyzer() and Fix stars_detected

**Files:**
- Modify: `crates/athenaeum-core/src/analysis/analyzer.rs`

**Step 1: Add new builder methods to build_analyzer()**

In `build_analyzer()` (lines 14-21), add 4 new method calls after `.with_mrs_layers()`:

Change lines 14-21 from:
```rust
    let mut analyzer = ImageAnalyzer::new()
        .with_detection_sigma(config.detection_sigma as f32)
        .with_min_star_area(config.min_star_area as usize)
        .with_max_star_area(config.max_star_area as usize)
        .with_saturation_fraction(config.saturation_fraction as f32)
        .with_max_stars(config.max_stars as usize)
        .with_trail_threshold(config.trail_threshold as f32)
        .with_mrs_layers(config.mrs_layers as usize);
```

to:
```rust
    let mut analyzer = ImageAnalyzer::new()
        .with_detection_sigma(config.detection_sigma as f32)
        .with_min_star_area(config.min_star_area as usize)
        .with_max_star_area(config.max_star_area as usize)
        .with_saturation_fraction(config.saturation_fraction as f32)
        .with_max_stars(config.max_stars as usize)
        .with_trail_threshold(config.trail_threshold as f32)
        .with_mrs_layers(config.mrs_layers as usize)
        .with_measure_cap(config.measure_cap as usize)
        .with_fit_max_iter(config.fit_max_iter as usize)
        .with_fit_tolerance(config.fit_tolerance)
        .with_fit_max_rejects(config.fit_max_rejects as usize);
```

**Step 2: Fix stars_detected to use the new dedicated field**

In `analyze_frame()` line 43, change:
```rust
        stars_detected: result.stars.len() as i64,
```
to:
```rust
        stars_detected: result.stars_detected as i64,
```

In v0.7.3, `result.stars_detected` reports the raw detection count before measure cap, while `result.stars.len()` is capped by `max_stars`. The DB field should store the true detection count.

**Step 3: Verify compilation**

Run: `cargo check -p athenaeum-core`

Expected: Compiles cleanly.

**Step 4: Commit**

```bash
git add crates/athenaeum-core/src/analysis/analyzer.rs
git commit -m "feat: wire measure_cap/fit_max_iter/fit_tolerance/fit_max_rejects to ImageAnalyzer; use result.stars_detected for true detection count"
```

---

### Task 4: Update TypeScript Types

**Files:**
- Modify: `src/types/analysis-config.ts`

**Step 1: Add fields to AnalysisConfig interface**

In `src/types/analysis-config.ts`, add 4 fields to the `AnalysisConfig` interface (after `mrs_layers` on line 9, before `scoring_weights` on line 10):

```typescript
  measure_cap: number;
  fit_max_iter: number;
  fit_tolerance: number;
  fit_max_rejects: number;
```

**Step 2: Update DEFAULT_ANALYSIS_CONFIG**

In `DEFAULT_ANALYSIS_CONFIG` (lines 22-36), change `mrs_layers: 1` to `mrs_layers: 4` and add the 4 new fields:

```typescript
export const DEFAULT_ANALYSIS_CONFIG: AnalysisConfig = {
  detection_sigma: 5.0,
  min_star_area: 5,
  max_star_area: 2000,
  saturation_fraction: 0.95,
  max_stars: 500,
  trail_threshold: 0.5,
  mrs_layers: 4,
  measure_cap: 2000,
  fit_max_iter: 25,
  fit_tolerance: 1e-4,
  fit_max_rejects: 5,
  scoring_weights: {
    fwhm: 0.35,
    eccentricity: 0.15,
    snr_weight: 0.40,
    star_count: 0.10,
  },
};
```

**Step 3: Commit**

```bash
git add src/types/analysis-config.ts
git commit -m "feat: add measure_cap/fit_max_iter/fit_tolerance/fit_max_rejects to TS AnalysisConfig; update mrs_layers default to 4"
```

---

### Task 5: Update AnalysisSettingsPanel UI

**Files:**
- Modify: `src/components/analysis/AnalysisSettingsPanel.tsx`

**Step 1: Update MRS Layers description**

On line 206, change the description text from:
```
MRS wavelet noise estimation layers. 0 = legacy MAD, 1+ = wavelet-based. Default: 1 (0-10).
```
to:
```
MRS wavelet noise estimation layers. Higher = more accurate noise on nebula-rich fields. Default: 4 (0-10).
```

**Step 2: Add PSF Fitting section with 4 new inputs**

After the "Measurement Method" `</div>` closing tag (line 209), add a new section:

```tsx
      {/* PSF Fitting */}
      <div>
        <h4 className="text-sm font-semibold text-content mb-3">PSF Fitting</h4>
        <div className="grid grid-cols-2 gap-4">
          <div>
            <label className="block text-xs text-content-secondary mb-1">Measure Cap</label>
            <input
              type="number"
              value={config.measure_cap}
              onChange={e => updateField('measure_cap', Math.max(0, parseInt(e.target.value) || 0))}
              min="0" max="100000" step="100"
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            />
            <p className="text-xs text-content-muted mt-1">Max stars to PSF-fit. 0 = measure all. Default: 2000.</p>
          </div>
          <div>
            <label className="block text-xs text-content-secondary mb-1">Fit Max Iterations</label>
            <input
              type="number"
              value={config.fit_max_iter}
              onChange={e => updateField('fit_max_iter', Math.max(1, Math.min(200, parseInt(e.target.value) || 25)))}
              min="1" max="200" step="5"
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            />
            <p className="text-xs text-content-muted mt-1">LM max iterations. Increase for accuracy, decrease for speed. Default: 25.</p>
          </div>
          <div>
            <label className="block text-xs text-content-secondary mb-1">Fit Tolerance</label>
            <input
              type="number"
              value={config.fit_tolerance}
              onChange={e => updateField('fit_tolerance', Math.max(1e-8, Math.min(1e-2, parseFloat(e.target.value) || 1e-4)))}
              min="1e-8" max="0.01" step="0.0001"
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            />
            <p className="text-xs text-content-muted mt-1">LM convergence tolerance. Lower = tighter convergence. Default: 0.0001.</p>
          </div>
          <div>
            <label className="block text-xs text-content-secondary mb-1">Fit Max Rejects</label>
            <input
              type="number"
              value={config.fit_max_rejects}
              onChange={e => updateField('fit_max_rejects', Math.max(1, Math.min(100, parseInt(e.target.value) || 5)))}
              min="1" max="100" step="1"
              className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
            />
            <p className="text-xs text-content-muted mt-1">LM consecutive reject bailout. Default: 5.</p>
          </div>
        </div>
      </div>
```

**Step 3: Verify frontend compiles**

Run: `npx tsc --noEmit`

Expected: No type errors.

**Step 4: Commit**

```bash
git add src/components/analysis/AnalysisSettingsPanel.tsx
git commit -m "feat: add PSF fitting controls (measure_cap, fit_max_iter, fit_tolerance, fit_max_rejects) to analysis settings; update MRS description"
```

---

### Task 6: Add Trail Type Tooltip

**Files:**
- Modify: `src/components/calibration/LightsAnalysisTable.tsx:398-406`
- Modify: `src/components/blink/FrameInfoPanel.tsx:72-84`

**Step 1: Update LightsAnalysisTable trail tooltip**

In `LightsAnalysisTable.tsx`, replace lines 402-405:

```tsx
                      {analysis.possibly_trailed && (
                        <span title="Possible star trails detected">
                          <AlertTriangle size={14} className="text-amber-400" />
                        </span>
                      )}
```

with:

```tsx
                      {analysis.possibly_trailed && (
                        <span title={analysis.trail_r_squared >= 0.3
                          ? "Directional trail detected (RA drift)"
                          : "Guiding issue (wind/vibration)"
                        }>
                          <AlertTriangle size={14} className="text-amber-400" />
                        </span>
                      )}
```

**Step 2: Update FrameInfoPanel trail tooltip**

In `FrameInfoPanel.tsx`, replace lines 77-80:

```tsx
                  {metrics.possibly_trailed && (
                    <span title="Possibly trailed">
                      <AlertTriangle size={12} className="text-warning" />
                    </span>
                  )}
```

with:

```tsx
                  {metrics.possibly_trailed && (
                    <span title={metrics.trail_r_squared >= 0.3
                      ? "Directional trail detected (RA drift)"
                      : "Guiding issue (wind/vibration)"
                    }>
                      <AlertTriangle size={12} className="text-warning" />
                    </span>
                  )}
```

**Step 3: Verify frontend compiles**

Run: `npx tsc --noEmit`

Expected: No type errors.

**Step 4: Commit**

```bash
git add src/components/calibration/LightsAnalysisTable.tsx src/components/blink/FrameInfoPanel.tsx
git commit -m "feat: add trail type tooltip - distinguish directional trails from guiding issues"
```

---

### Task 7: Full Build Verification

**Step 1: Run Rust tests**

Run: `cargo test -p athenaeum-core`

Expected: All existing tests pass. No test changes needed -- the upgrade is additive.

**Step 2: Run full cargo check across workspace**

Run: `cargo check`

Expected: All crates compile (athenaeum-core, athenaeum-tauri, athenaeum-web).

**Step 3: Run frontend type check**

Run: `npx tsc --noEmit`

Expected: No type errors.

**Step 4: Manual smoke test (optional)**

Start the dev server with `npm run tauri dev`, open Settings > Analysis, and verify:
- MRS Layers shows default 4 (for new/reset config)
- New PSF Fitting section visible with 4 inputs
- Existing saved config still loads correctly (serde defaults fill missing fields)

---

## Summary of All Changed Files

| File | Change |
| ---- | ---- |
| `crates/athenaeum-core/Cargo.toml` | rustafits 0.7.1 -> 0.7.3 |
| `Cargo.lock` | Auto-updated |
| `crates/athenaeum-core/src/analysis/config.rs` | +4 fields, mrs_layers default 1->4, validation |
| `crates/athenaeum-core/src/analysis/analyzer.rs` | Wire 4 new builder methods, fix stars_detected |
| `src/types/analysis-config.ts` | +4 fields, mrs_layers default 1->4 |
| `src/components/analysis/AnalysisSettingsPanel.tsx` | +PSF Fitting section, update MRS description |
| `src/components/calibration/LightsAnalysisTable.tsx` | Trail type tooltip |
| `src/components/blink/FrameInfoPanel.tsx` | Trail type tooltip |

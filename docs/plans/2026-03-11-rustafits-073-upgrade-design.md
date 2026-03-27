# rustafits v0.7.1 to v0.7.3 Upgrade

## Summary

Bump rustafits dependency and integrate v0.7.3 additions: 4 new AnalysisConfig fields, updated mrs_layers default, trail detection tooltip, and UI description fixes.

## What Changed in rustafits v0.7.3

**New per-star field:** `StarMetrics.fit_residual: f32` (not stored in Athenaeum DB -- per-star, not per-frame)

**New builder methods:**

- `with_measure_cap(2000)` -- max stars to PSF-fit (0 = all)
- `with_fit_max_iter(25)` -- LM iterations for measurement pass
- `with_fit_tolerance(1e-4)` -- LM convergence tolerance
- `with_fit_max_rejects(5)` -- LM consecutive reject bailout

**Behavioral changes (transparent):**

- Trail detection is two-stage (Rayleigh + PSF-fit ecc > 0.55)
- `stars_detected` now means "before measure cap"
- Statistics use fit-residual-weighted sigma-clipped medians
- FWHM R^2 0.972 to 0.995 vs PI, Ecc R^2 0.916 to 0.943
- MRS layers library default changed from 1 to 4

## Design Decisions

1. **Expose all 4 new config fields** (measure_cap, fit_max_iter, fit_tolerance, fit_max_rejects) in AnalysisConfig and settings UI
2. **Update mrs_layers default to 4** to match library and PixInsight
3. **Add trail tooltip hint** distinguishing directional trails (high R^2) from guiding issues (low R^2)
4. **No version tracking** in DB -- config_hash handles staleness
5. **No DB schema changes** -- fit_residual is per-star (not stored), no new frame-level fields

## Changes by Layer

### Dependency

- `crates/athenaeum-core/Cargo.toml`: `rustafits = "0.7.1"` to `rustafits = "0.7.3"`

### Rust: AnalysisConfig

File: `crates/athenaeum-core/src/analysis/config.rs`

- Add fields: `measure_cap: u32` (default 2000), `fit_max_iter: u32` (default 25), `fit_tolerance: f64` (default 1e-4), `fit_max_rejects: u32` (default 5)
- Change `mrs_layers` default from 1 to 4
- All new fields use `#[serde(default = "...")]` for backward-compatible deserialization
- Update `validate()` for new field ranges

### Rust: build_analyzer()

File: `crates/athenaeum-core/src/analysis/analyzer.rs`

- Wire 4 new config fields to builder: `.with_measure_cap()`, `.with_fit_max_iter()`, `.with_fit_tolerance()`, `.with_fit_max_rejects()`

### TypeScript: analysis-config.ts

- Add 4 fields to `AnalysisConfig` interface and `DEFAULT_ANALYSIS_CONFIG`
- Update `mrs_layers` default to 4

### TypeScript: AnalysisSettingsPanel.tsx

- Add "Measure Cap" input (0-10000, step 100)
- Add "Fit Max Iterations" input (5-100, step 5)
- Add "Fit Tolerance" input (1e-8 to 1e-2)
- Add "Fit Max Rejects" input (1-20, step 1)
- Update MRS Layers description: default 4, remove "0 = legacy MAD" wording

### TypeScript: Trail tooltip

- In LightsAnalysisTable / FrameInfoPanel trail warning icon:
  - `possibly_trailed && trail_r_squared >= 0.3` -> "Directional trail detected"
  - `possibly_trailed && trail_r_squared < 0.3` -> "Guiding issue (wind/vibration)"

### Unchanged

- DB schema (no new columns)
- FrameAnalysis model (no new fields)
- db/analysis.rs (no query changes)
- Web backend routes (pass-through from athenaeum-core)
- Batch analysis parallelization

## Migration

- Existing saved configs missing new fields: `#[serde(default)]` fills defaults
- Existing `mrs_layers: 1` configs keep their value; only new/reset gets 4
- config_hash changes trigger re-analysis on next run (desirable for better metrics)

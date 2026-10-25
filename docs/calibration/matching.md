# Calibration matching


## Detail moved from CLAUDE.md (2026-10-20) - Calibration matching

### Calibration Matching

Fully configurable via UI (Settings → Calibration Matching). Stored as a single `CalibrationMatchingConfig` JSON in `settings` under key `calibration.matching_config`.

**Components**: parameter-matching rules, clustering settings (max age, time-cluster window per type), scoring weights, warning thresholds, master preferences.

**Source → calibration links**:

- Lights → Flat, Dark, Bias
- Flats → DarkFlat, Dark, Bias (fallback chain DarkFlat → Dark → Bias)
- Darks → Bias (when "BIAS for Dark Optimization" is on)

**Per-pair parameters** (each `Exact` / `Warning` / `Ignore`): `instrume`, `binning`, `gain`, `offset`, `exptime`, `focallen`, `filter` (Lights→Flat only), `ccd_temp`. Defaults reproduce the original hardcoded behavior — see the `Default` impls in `calibration/config.rs` for the matrix.

**Geometry is not a parameter** (2026-10-20): a set of another frame size (NAXIS1×NAXIS2, after binning) never calibrates — auto-link, the manual list, manual assignment, the readiness and stacking-plan gates all ask `calibration::geometry::fit`. Another sub-frame origin at the same size (`XORGSUBF`/`YORGSUBF` from the stored header, compared only when both record it): auto-link never, by hand with a warning (owner, G-7). In-app masters stamp their members' agreed origin (G-5). Reference: `docs/catalog/README.md` → Calibration geometry.

**Key files**:

- `crates/athenaeum-core/src/calibration/config.rs` — `CalibrationMatchingConfig`, `ParameterConfig`, `MatchMode`.
- `crates/athenaeum-core/src/calibration/configurable_matcher.rs` — `find_calibration_sets`, `load_config`.
- `crates/athenaeum-core/src/calibration/hierarchy.rs` — hierarchy builder (uses configurable matcher).
- `src/types/calibration-config.ts`, `src/components/calibration/`.

**Tauri commands**: `get_calibration_matching_config`, `set_calibration_matching_config`, `reset_calibration_matching_config`.


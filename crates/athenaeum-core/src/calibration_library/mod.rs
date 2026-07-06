//! Calibration Library: fixed v1 master-file naming (`paths`) and
//! consolidated master FITS header construction (`headers`) for Phase 2
//! master calibration. `register` (Task 11) builds runtime plumbing on top
//! of these: registering a just-written master file's DB rows, provenance,
//! relink, and supersede.

pub mod headers;
// The light-calibration engine streams raw pixels via the render-gated
// `integration` module. Its shared data types (FlatNormMode, BiasFallback,
// LightCalParams, LIGHT_CAL_ENGINE_VERSION, PI_TRIM_FRACTION) live in `models`
// so ungated consumers (db::light_calibrations, scanner, export) keep working.
#[cfg(feature = "render")]
pub mod light_cal;
pub mod light_headers;
pub mod paths;
pub mod register;

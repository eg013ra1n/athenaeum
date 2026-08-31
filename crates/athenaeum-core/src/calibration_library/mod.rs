//! Calibration Library: fixed v1 master-file naming (`paths`) and
//! consolidated master FITS header construction (`headers`) for Phase 2
//! master calibration. `register` (Task 11) builds runtime plumbing on top
//! of these: registering a just-written master file's DB rows, provenance,
//! relink, and supersede.

pub mod headers;
// Hot-pixel cosmetic correction reads a master dark through the render-gated
// `integration` module, same as the light-calibration engine below.
#[cfg(feature = "render")]
pub mod cosmetic;
// The light-calibration engine streams raw pixels via the render-gated
// `integration` module. Its shared data types (FlatNormMode, BiasFallback,
// LightCalParams, LIGHT_CAL_ENGINE_VERSION, PI_TRIM_FRACTION) live in `models`
// so ungated consumers (db::light_calibrations, scanner, export) keep working.
#[cfg(feature = "render")]
pub mod light_cal;
pub mod light_headers;
// Per-frame master resolution for light calibration (Task 5, moved out of
// `api::lights`): resolves a light frame's Dark/Flat/Bias links against the
// catalog into `ResolvedFrameInputs`. Render-gated because it names
// `integration::cfa::CfaGeometry`, itself render-gated — same reasoning as
// `light_cal` above. A later export-side generator (also render-gated) will
// consume it directly, without going through `api`.
#[cfg(feature = "render")]
pub mod light_resolve;
pub mod paths;
pub mod register;

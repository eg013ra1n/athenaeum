//! Calibration Library: fixed v1 master-file naming (`paths`) and
//! consolidated master FITS header construction (`headers`) for Phase 2
//! master calibration. `register` (Task 11) builds runtime plumbing on top
//! of these: registering a just-written master file's DB rows, provenance,
//! relink, and supersede.

pub mod headers;
// Hot-pixel cosmetic correction lives in `starloom` (its `engine` layer); the old path stays.
#[cfg(feature = "render")]
pub use starloom::calibration_library::cosmetic;
// The light-calibration pixel math lives in `starloom` (its `engine` layer); `light_cal` re-exports it and keeps
// the reading of Athenaeum's own flat cards. Its shared data types (FlatNormMode, BiasFallback, LightCalParams,
// LIGHT_CAL_ENGINE_VERSION, TRIMMED_MEAN_TAIL_FRACTION) live in `models` so ungated consumers (scanner, export)
// keep working.
#[cfg(feature = "render")]
pub mod light_cal;
pub mod light_headers;
// Per-frame master resolution for light calibration (Task 5, moved out of
// `api::lights`): resolves a light frame's Dark/Flat/Bias links against the
// catalog into `ResolvedFrameInputs`. Render-gated because it names
// `integration::cfa::CfaGeometry`, itself render-gated — same reasoning as
// `light_cal` above. The export-side generator (also render-gated) consumes it
// directly, without going through `api`.
#[cfg(feature = "render")]
pub mod light_resolve;
pub mod paths;
// Masters built twice (the `_2` / `_3` suffix): the copies of one master and which of them calibration links use.
pub mod rebuilt_copies;
pub mod register;

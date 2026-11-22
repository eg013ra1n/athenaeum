//! Calibration library: the parameter types (base) and the light-calibration math (`engine`) (Tasks 4 and 5).

pub mod params;

/// The version of the light-calibration math. A change to the pixel arithmetic in `light_cal` or `cosmetic` bumps
/// it; a dependent application watches it with a tripwire test (spec SL-19).
pub const LIGHT_CAL_VERSION: u32 = 1;
#[cfg(feature = "engine")]
pub mod cosmetic;
#[cfg(feature = "engine")]
pub mod light_cal;

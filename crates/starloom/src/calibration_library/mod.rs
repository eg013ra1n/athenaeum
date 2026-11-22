//! Calibration library: the parameter types (base) and the light-calibration math (`engine`) (Tasks 4 and 5).

pub mod params;
#[cfg(feature = "engine")]
pub mod cosmetic;
#[cfg(feature = "engine")]
pub mod light_cal;

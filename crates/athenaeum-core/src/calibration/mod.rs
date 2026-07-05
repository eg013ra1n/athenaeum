// Calibration library module
// Manages calibration frames and linking to days/setups

pub mod config;
pub mod configurable_matcher;
pub mod finder;
pub mod hierarchy;
pub mod manual;
pub mod processor;
pub mod flat_groups;
pub mod flat_matcher;
pub mod dark_bias_groups;
pub mod scan_integration;

// Re-export config types for convenience
pub use config::CalibrationMatchingConfig;

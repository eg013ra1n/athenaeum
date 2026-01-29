//! Configuration types for stacking operations
//!
//! This module defines the configuration structures used to control
//! stacking behavior, including rejection algorithms, normalization,
//! and output options.

use serde::{Deserialize, Serialize};

/// Stacking method to use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackingMethod {
    /// Median stacking - robust against outliers
    Median,
    /// Mean stacking - higher SNR but sensitive to outliers
    Mean,
    /// Sum stacking - preserves total signal
    Sum,
}

impl Default for StackingMethod {
    fn default() -> Self {
        StackingMethod::Median
    }
}

/// Pixel rejection algorithm
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectionAlgorithm {
    /// No rejection
    None,
    /// Percentile clipping
    Percentile,
    /// Sigma clipping
    Sigma,
    /// Median Absolute Deviation
    Mad,
    /// Sigma-median clipping
    SigmaMedian,
    /// Winsorized sigma clipping
    Winsorized,
    /// Linear fit clipping
    LinearFit,
    /// Generalized ESD (Extreme Studentized Deviate)
    Gesdt,
}

impl Default for RejectionAlgorithm {
    fn default() -> Self {
        RejectionAlgorithm::None
    }
}

/// Normalization method for stacking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationMethod {
    /// No normalization
    None,
    /// Additive normalization (shift to match)
    Additive,
    /// Multiplicative normalization (scale to match)
    Multiplicative,
    /// Additive with scaling
    AdditiveScaling,
    /// Multiplicative with scaling
    MultiplicativeScaling,
}

impl Default for NormalizationMethod {
    fn default() -> Self {
        NormalizationMethod::None
    }
}

/// Image weighting method for stacking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeightingMethod {
    /// Equal weights for all images
    None,
    /// Weight by number of stars detected
    StarCount,
    /// Weight inversely by FWHM (sharper = more weight)
    Fwhm,
    /// Weight inversely by noise level
    Noise,
    /// Weight by number of stacked frames (for pre-stacked masters)
    StackCount,
}

impl Default for WeightingMethod {
    fn default() -> Self {
        WeightingMethod::None
    }
}

/// Rejection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectionConfig {
    /// Rejection algorithm to use
    pub algorithm: RejectionAlgorithm,
    /// Low sigma threshold (for sigma-based rejection)
    pub sigma_low: f64,
    /// High sigma threshold (for sigma-based rejection)
    pub sigma_high: f64,
}

impl Default for RejectionConfig {
    fn default() -> Self {
        RejectionConfig {
            algorithm: RejectionAlgorithm::None,
            sigma_low: 3.0,
            sigma_high: 3.0,
        }
    }
}

impl RejectionConfig {
    /// Create a sigma clipping configuration
    pub fn sigma(low: f64, high: f64) -> Self {
        RejectionConfig {
            algorithm: RejectionAlgorithm::Sigma,
            sigma_low: low,
            sigma_high: high,
        }
    }

    /// Create a MAD (Median Absolute Deviation) configuration
    pub fn mad(low: f64, high: f64) -> Self {
        RejectionConfig {
            algorithm: RejectionAlgorithm::Mad,
            sigma_low: low,
            sigma_high: high,
        }
    }

    /// Create a winsorized sigma clipping configuration
    pub fn winsorized(low: f64, high: f64) -> Self {
        RejectionConfig {
            algorithm: RejectionAlgorithm::Winsorized,
            sigma_low: low,
            sigma_high: high,
        }
    }
}

/// Configuration for creating master calibration frames
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterConfig {
    /// Stacking method (median or mean)
    pub method: StackingMethod,
    /// Pixel rejection configuration (optional)
    pub rejection: Option<RejectionConfig>,
    /// Output 32-bit float (true) or 16-bit integer (false)
    pub output_32bit: bool,
}

impl Default for MasterConfig {
    fn default() -> Self {
        MasterConfig {
            method: StackingMethod::Median,
            rejection: None,
            output_32bit: true,
        }
    }
}

impl MasterConfig {
    /// Create a configuration for median stacking without rejection
    pub fn median() -> Self {
        MasterConfig {
            method: StackingMethod::Median,
            rejection: None,
            output_32bit: true,
        }
    }

    /// Create a configuration for mean stacking with sigma rejection
    pub fn mean_with_sigma(sigma_low: f64, sigma_high: f64) -> Self {
        MasterConfig {
            method: StackingMethod::Mean,
            rejection: Some(RejectionConfig::sigma(sigma_low, sigma_high)),
            output_32bit: true,
        }
    }

    /// Set output bit depth
    pub fn with_output_32bit(mut self, output_32bit: bool) -> Self {
        self.output_32bit = output_32bit;
        self
    }

    /// Set rejection configuration
    pub fn with_rejection(mut self, rejection: RejectionConfig) -> Self {
        self.rejection = Some(rejection);
        self
    }
}

/// Configuration for creating master flat frames
///
/// Flat frames may need calibration with dark/bias before stacking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlatConfig {
    /// Base stacking configuration
    pub stacking: MasterConfig,
    /// Path to master dark for flat calibration (optional)
    pub calibrate_with_dark: Option<String>,
    /// Path to master bias for flat calibration (optional)
    pub calibrate_with_bias: Option<String>,
    /// Equalize CFA channels for OSC flats
    pub equalize_cfa: bool,
}

impl Default for FlatConfig {
    fn default() -> Self {
        FlatConfig {
            stacking: MasterConfig::median(),
            calibrate_with_dark: None,
            calibrate_with_bias: None,
            equalize_cfa: true,
        }
    }
}

impl FlatConfig {
    /// Create a flat configuration with dark calibration
    pub fn with_dark<S: Into<String>>(mut self, dark_path: S) -> Self {
        self.calibrate_with_dark = Some(dark_path.into());
        self
    }

    /// Create a flat configuration with bias calibration
    pub fn with_bias<S: Into<String>>(mut self, bias_path: S) -> Self {
        self.calibrate_with_bias = Some(bias_path.into());
        self
    }
}

/// Configuration for stacking light frames
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightStackConfig {
    /// Stacking method
    pub method: StackingMethod,
    /// Pixel rejection configuration
    pub rejection: Option<RejectionConfig>,
    /// Normalization method
    pub normalization: NormalizationMethod,
    /// Image weighting method
    pub weighting: WeightingMethod,
    /// Output 32-bit float
    pub output_32bit: bool,
    /// Enable drizzle upscaling
    pub drizzle: Option<DrizzleConfig>,
}

impl Default for LightStackConfig {
    fn default() -> Self {
        LightStackConfig {
            method: StackingMethod::Mean,
            rejection: Some(RejectionConfig::sigma(2.5, 2.5)),
            normalization: NormalizationMethod::Additive,
            weighting: WeightingMethod::Fwhm,
            output_32bit: true,
            drizzle: None,
        }
    }
}

/// Drizzle upscaling configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrizzleConfig {
    /// Drizzle scale factor (1.5, 2.0, or 3.0)
    pub scale: f64,
    /// Drop shrink factor (default 1.0)
    pub drop_shrink: f64,
}

impl Default for DrizzleConfig {
    fn default() -> Self {
        DrizzleConfig {
            scale: 2.0,
            drop_shrink: 1.0,
        }
    }
}

/// Progress callback type
///
/// Called during long-running operations with progress percentage (0.0-1.0)
/// and an optional status message.
pub type ProgressCallback = Box<dyn Fn(f32, &str) + Send>;

/// Result of a master frame creation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterResult {
    /// Path to the output master frame
    pub output_path: String,
    /// Number of frames stacked
    pub frame_count: usize,
    /// Processing time in milliseconds
    pub processing_time_ms: u64,
    /// Total exposure time (if applicable)
    pub total_exposure_seconds: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = MasterConfig::default();
        assert_eq!(config.method, StackingMethod::Median);
        assert!(config.rejection.is_none());
        assert!(config.output_32bit);
    }

    #[test]
    fn test_sigma_rejection() {
        let config = MasterConfig::mean_with_sigma(2.0, 3.0);
        assert_eq!(config.method, StackingMethod::Mean);
        let rejection = config.rejection.unwrap();
        assert_eq!(rejection.algorithm, RejectionAlgorithm::Sigma);
        assert_eq!(rejection.sigma_low, 2.0);
        assert_eq!(rejection.sigma_high, 3.0);
    }

    #[test]
    fn test_flat_config() {
        let config = FlatConfig::default()
            .with_dark("/path/to/dark.fits")
            .with_bias("/path/to/bias.fits");

        assert_eq!(config.calibrate_with_dark, Some("/path/to/dark.fits".to_string()));
        assert_eq!(config.calibrate_with_bias, Some("/path/to/bias.fits".to_string()));
    }
}

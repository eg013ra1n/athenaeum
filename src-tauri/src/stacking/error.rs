//! Error types for stacking operations

use thiserror::Error;

/// Errors that can occur during stacking operations
#[derive(Debug, Error)]
pub enum StackingError {
    // I/O Errors
    #[error("Failed to read FITS file '{path}': {details}")]
    ReadError { path: String, details: String },

    #[error("Failed to write FITS file '{path}': {details}")]
    WriteError { path: String, details: String },

    #[error("File not found: {0}")]
    FileNotFound(String),

    // Format Errors
    #[error("Invalid image format: {0}")]
    FormatError(String),

    #[error("Unsupported data type: {0}")]
    UnsupportedDataType(String),

    #[error("Invalid image dimensions: {0}")]
    InvalidDimensions(String),

    #[error("Image dimensions mismatch: expected {expected}, got {actual}")]
    DimensionsMismatch { expected: String, actual: String },

    #[error("Dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: String, actual: String },

    #[error("Process execution failed: {0}")]
    ProcessError(String),

    // Memory Errors
    #[error("Memory allocation failed")]
    AllocationError,

    #[error("Out of memory: required {required_mb} MB, available {available_mb} MB")]
    OutOfMemory { required_mb: u64, available_mb: u64 },

    // Processing Errors
    #[error("Stacking operation failed: {0}")]
    StackingFailed(String),

    #[error("Calibration failed: {0}")]
    CalibrationFailed(String),

    #[error("Debayering failed: {0}")]
    DebayerError(String),

    #[error("No stars detected in frame")]
    NoStarsDetected,

    #[error("Insufficient stars for registration: found {found}, need {required}")]
    InsufficientStars { found: usize, required: usize },

    #[error("Plate solving failed: {0}")]
    PlateSolveFailed(String),

    #[error("Registration failed: {0}")]
    RegistrationFailed(String),

    #[error("Image transformation failed")]
    TransformFailed,

    #[error("Too many plate-solve failures: {0} frames failed")]
    TooManyPlateSolveFailed(usize),

    // Sequence Errors
    #[error("Empty sequence: no frames provided")]
    EmptySequence,

    #[error("No frames to process")]
    NoFrames,

    #[error("Reference frame index {index} out of range (0..{count})")]
    InvalidReferenceIndex { index: usize, count: usize },

    // Configuration Errors
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    #[error("Missing required parameter: {0}")]
    MissingParameter(String),

    #[error("No initial coordinates available for plate solving")]
    NoInitialCoordinates,

    #[error("Missing WCS data in frame")]
    MissingWCS,

    // State Errors
    #[error("Library not initialized - call initialize() first")]
    NotInitialized,

    #[error("Operation cancelled by user")]
    Cancelled,

    #[error("Unsupported step type: {0}")]
    UnsupportedStepType(String),

    // Siril-specific errors
    #[error("Siril error code {code}: {message}")]
    SirilError { code: i32, message: String },

    // Generic errors
    #[error("{0}")]
    Other(String),
}

impl StackingError {
    /// Create a new SirilError from an error code
    pub fn from_siril_code(code: i32) -> Self {
        let message = match code {
            -10 => "Allocation error".to_string(),
            -9 => "Operation cancelled".to_string(),
            -2 => "Sequence error".to_string(),
            -1 => "Generic error".to_string(),
            0 => "Success (not an error)".to_string(),
            _ => format!("Unknown error code: {}", code),
        };
        StackingError::SirilError { code, message }
    }

    /// Check if this error indicates the operation was cancelled
    pub fn is_cancelled(&self) -> bool {
        matches!(self, StackingError::Cancelled | StackingError::SirilError { code: -9, .. })
    }

    /// Check if this error is recoverable (can retry)
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            StackingError::OutOfMemory { .. }
                | StackingError::NoStarsDetected
                | StackingError::PlateSolveFailed(_)
        )
    }
}

/// Result type for stacking operations
pub type StackingResult<T> = Result<T, StackingError>;

/// Convert a string error message into a StackingError
impl From<String> for StackingError {
    fn from(s: String) -> Self {
        StackingError::Other(s)
    }
}

impl From<&str> for StackingError {
    fn from(s: &str) -> Self {
        StackingError::Other(s.to_string())
    }
}

impl From<std::io::Error> for StackingError {
    fn from(e: std::io::Error) -> Self {
        StackingError::Other(format!("I/O error: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_siril_error_codes() {
        let err = StackingError::from_siril_code(-9);
        assert!(err.is_cancelled());

        let err = StackingError::from_siril_code(-10);
        assert!(!err.is_cancelled());
    }

    #[test]
    fn test_recoverable() {
        assert!(StackingError::NoStarsDetected.is_recoverable());
        assert!(!StackingError::Cancelled.is_recoverable());
    }
}

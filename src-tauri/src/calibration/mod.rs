// Calibration library module
// Manages calibration frames and linking to days/setups

use crate::models::{CalibrationSet, Frame};
use anyhow::Result;

/// Find matching calibration frames for a light frame
pub fn find_matching_calibrations(
    _frame: &Frame,
    _tolerance: &CalibrationTolerance,
) -> Result<MatchedCalibrations> {
    // TODO: Query for calibration frames matching:
    // - IMAGETYP (Dark, Flat, Bias, DarkFlat)
    // - EXPTIME (for Darks, within tolerance)
    // - FILTER (for Flats)
    // - INSTRUME
    // - GAIN/ISO (within tolerance)
    // - CCD-TEMP (within tolerance)
    // - Date proximity

    unimplemented!("Calibration matching not yet implemented")
}

/// Suggest calibration sets for a capture day
pub fn suggest_calibrations(_date: &str) -> Result<Vec<CalibrationSet>> {
    // TODO: Auto-suggest calibration frames for a given day
    // Group by parameters and suggest matches

    unimplemented!("Calibration suggestion not yet implemented")
}

pub struct CalibrationTolerance {
    pub temp_delta: f64,        // °C
    pub exptime_percent: f64,   // percentage
    pub gain_delta: f64,
}

pub struct MatchedCalibrations {
    pub darks: Vec<Frame>,
    pub flats: Vec<Frame>,
    pub bias: Vec<Frame>,
    pub dark_flats: Vec<Frame>,
}

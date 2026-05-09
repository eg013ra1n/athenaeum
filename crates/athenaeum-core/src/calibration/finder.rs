// Calibration finder - matches calibration sets to light frames
use crate::calibration::config::MatchMode;
use crate::models::ImageType;

/// Per-parameter match outcome surfaced for both auto-link and manual-suggestion UIs.
///
/// Captures *what was checked*: the configured mode, the compared values, the
/// numeric diff (when applicable), and the thresholds the diff was compared
/// against. The auto-link path uses `matched` to decide hard-filter pass/fail;
/// the manual modal additionally renders `frame_value` / `set_value` and the
/// thresholds so the user can see why a set was flagged.
#[derive(Debug, Clone, Default)]
pub struct ParameterMatch {
    pub matched: bool,
    pub mode: MatchMode,
    pub frame_value: Option<String>,
    pub set_value: Option<String>,
    pub diff: Option<f64>,
    pub warning_threshold: Option<f64>,
    pub matching_threshold: Option<f64>,
    /// Set when the result is "accept with warning" (Warning mode, diff between
    /// warning_threshold and matching_threshold). The auto-link path stores
    /// these as warnings; the modal can highlight them.
    pub warning: bool,
}

/// Per-parameter details populated by `check_calibration_match` so callers can
/// reconstruct exactly how each parameter compared.
#[derive(Debug, Clone, Default)]
pub struct CandidateMatchDetails {
    pub instrume: ParameterMatch,
    pub binning: ParameterMatch,
    pub gain: ParameterMatch,
    pub offset: ParameterMatch,
    pub telescop: ParameterMatch,
    pub exptime: ParameterMatch,
    pub focallen: ParameterMatch,
    pub filter: ParameterMatch,
    pub ccd_temp: ParameterMatch,
    pub date_diff_days: i64,
    pub temp_diff: Option<f64>,
    pub exptime_diff: Option<f64>,
}

/// Whether the candidate engine should return only sets that pass the
/// config-driven hard filter, or also include incompatible ones (flagged via
/// `passed_hard_filter`) so the manual modal's "Show All" can show them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateMode {
    /// Auto-link / "Compatible" tab: only return sets that passed every
    /// Exact / Warning parameter check.
    OnlyCompatible,
    /// Manual modal "Show All": return every set of the requested type, with
    /// `passed_hard_filter = false` on the ones that would be rejected. Score
    /// is still computed so the UI can sort and badge them.
    IncludeIncompatible,
}

/// Represents a candidate calibration set with its match score and details.
///
/// Returned by both auto-link (`find_calibration_sets`) and manual-suggestion
/// (`find_calibration_candidates`) paths so the two UIs see identical scoring.
#[derive(Debug, Clone)]
pub struct CalibrationCandidate {
    pub set_id: i64,
    pub imagetyp: ImageType,
    pub match_score: f64,  // 0.0-1.0
    pub date_diff_days: i64,
    pub temp_diff: Option<f64>,
    pub date_warning: bool,
    pub temp_warning: bool,
    pub is_master: bool,  // is_master_library = 1
    /// True iff the candidate passed every Exact/Warning parameter check at
    /// the configured thresholds. Always true for `OnlyCompatible` mode.
    pub passed_hard_filter: bool,
    pub details: CandidateMatchDetails,
    pub warnings: Vec<String>,
}

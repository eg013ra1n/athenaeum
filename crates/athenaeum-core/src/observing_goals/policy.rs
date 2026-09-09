//! Deterministic qualification, applied AFTER confirmed exposure deduplication.
use super::models::ObservingGoal;
use anyhow::{bail, Result};

/// Validate a positive target (seconds) and optional image-quality thresholds.
/// Quality limits require analysis; absent limits impose no threshold.
pub fn validate(goal: &ObservingGoal) -> Result<()> {
    if goal.frame_set_id <= 0
        || goal.revision < 0
        || goal.filter.len() > 256
        || !goal.target_seconds.is_finite()
        || goal.target_seconds <= 0.0
        || goal.target_seconds > 1e9
    {
        bail!("Invalid field, filter or target integration (seconds)");
    }
    if goal.max_fwhm_px.is_some_and(|v| !v.is_finite() || v <= 0.0)
        || goal
            .max_eccentricity
            .is_some_and(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
    {
        bail!("FWHM must be positive; eccentricity must be between 0 and 1");
    }
    if !goal.require_analysis
        && (goal.max_fwhm_px.is_some() || goal.max_eccentricity.is_some() || goal.reject_trailed)
    {
        bail!("Quality thresholds require analysis");
    }
    Ok(())
}

pub enum Qualification {
    Accepted,
    Rejected,
    Unknown,
}

/// Absent/invalid EXPTIME always remains unknown. When analysis is required,
/// no-star or invalid measurements remain unknown instead of passing as zero.
/// Missing metrics take precedence over rejection: evidence is incomplete.
///
/// `seconds` is the representative frame's EXPTIME in seconds. `metrics` contains
/// (star count, median FWHM in image pixels, median eccentricity, trail flag).
/// Eccentricity is dimensionless in [0, 1]. With no goal or no analysis requirement,
/// a positive finite exposure is accepted regardless of the supplied metrics.
/// The caller must validate the goal and choose one representative per exposure.
pub fn qualify(
    goal: Option<&ObservingGoal>,
    seconds: Option<f64>,
    metrics: Option<(i64, f64, f64, bool)>,
) -> Qualification {
    if !seconds.is_some_and(|v| v.is_finite() && v > 0.0) {
        return Qualification::Unknown;
    }
    let Some(goal) = goal.filter(|goal| goal.require_analysis) else {
        return Qualification::Accepted;
    };
    let Some((stars, fwhm, eccentricity, trailed)) = metrics else {
        return Qualification::Unknown;
    };
    if stars <= 0
        || !fwhm.is_finite()
        || fwhm <= 0.0
        || !eccentricity.is_finite()
        || !(0.0..=1.0).contains(&eccentricity)
    {
        return Qualification::Unknown;
    }
    if goal.max_fwhm_px.is_some_and(|v| fwhm > v)
        || goal.max_eccentricity.is_some_and(|v| eccentricity > v)
        || (goal.reject_trailed && trailed)
    {
        Qualification::Rejected
    } else {
        Qualification::Accepted
    }
}

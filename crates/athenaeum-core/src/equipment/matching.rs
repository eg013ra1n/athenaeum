//! Pure small-angle geometry. No database access or file operations.
use super::models::{EquipmentCandidate, EquipmentProfile};
use anyhow::{bail, Result};

/// Validate names, positive optical dimensions, symmetric binning and tolerance.
/// Returns an error for an invalid profile or a nonfinite/nonpositive pixel scale.
pub fn validate(profile: &EquipmentProfile) -> Result<()> {
    if profile.name.trim().is_empty()
        || profile.telescope.trim().is_empty()
        || profile.camera.trim().is_empty()
    {
        bail!("Name, telescope and exact camera name are required");
    }
    if [
        profile.name.len(),
        profile.telescope.len(),
        profile.camera.len(),
    ]
    .iter()
    .any(|n| *n > 256)
    {
        bail!("Equipment names must be at most 256 bytes");
    }
    for value in [
        profile.focal_length_mm,
        profile.optical_multiplier,
        profile.pixel_size_um,
    ] {
        if !value.is_finite() || value <= 0.0 {
            bail!("Optical dimensions and multiplier must be finite and positive");
        }
    }
    if !(1..=16).contains(&profile.binning) {
        bail!("Symmetric binning must be between 1 and 16");
    }
    if !profile.tolerance_percent.is_finite() || !(0.1..=50.0).contains(&profile.tolerance_percent)
    {
        bail!("Tolerance must be between 0.1 and 50 percent");
    }
    let scale = expected_scale(profile);
    if !scale.is_finite() || scale <= 0.0 {
        bail!("Configuration produces an invalid pixel scale");
    }
    Ok(())
}

/// s = (180/pi)*3600/1000 * p*b/(f*m), in arcsec/pixel.
/// p is unbinned pitch (µm), b symmetric binning, f native focal length (mm),
/// m optical magnification. Small-angle approximation for an unresampled grid.
/// The conversion constant follows radians→arcseconds and µm→mm exactly.
/// Call `validate` before use: this pure conversion does not reject invalid inputs.
/// It predicts physical sampling, without correcting for resampling or drizzle.
pub fn expected_scale(profile: &EquipmentProfile) -> f64 {
    (180.0 / std::f64::consts::PI) * 3.6 * profile.pixel_size_um * f64::from(profile.binning)
        / (profile.focal_length_mm * profile.optical_multiplier)
}

/// Rank all compatible profiles by |measured_scale/expected - 1| * 100 (percent).
/// Missing/asymmetric binning or camera mismatch yields no candidate. Ties stay
/// visible; a scale agreement cannot identify an instrument uniquely.
///
/// `measured_scale` is the saved solve's arcsec per image pixel; `binning_x` and `binning_y`
/// are header binning factors. Invalid scales or profiles yield no candidate.
/// Camera names must match exactly. Candidates within each profile's engineering
/// tolerance are ordered by relative difference, then profile ID; this is not
/// a probability ranking and does not record any equipment association.
pub fn candidates(
    profiles: &[EquipmentProfile],
    camera: &str,
    binning_x: Option<i32>,
    binning_y: Option<i32>,
    measured_scale: f64,
) -> Vec<EquipmentCandidate> {
    if !measured_scale.is_finite() || measured_scale <= 0.0 {
        return Vec::new();
    }
    let mut result: Vec<_> = profiles
        .iter()
        .filter_map(|profile| {
            if validate(profile).is_err()
                || profile.camera != camera
                || binning_x != Some(profile.binning)
                || binning_y != binning_x
            {
                return None;
            }
            let scale = expected_scale(profile);
            let difference = (measured_scale / scale - 1.0).abs() * 100.0;
            (difference <= profile.tolerance_percent).then(|| EquipmentCandidate {
                profile: profile.clone(),
                expected_scale: scale,
                difference_percent: difference,
            })
        })
        .collect();
    result.sort_by(|a, b| {
        a.difference_percent
            .total_cmp(&b.difference_percent)
            .then(a.profile.id.cmp(&b.profile.id))
    });
    result
}

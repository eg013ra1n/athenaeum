use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

const SETTINGS_KEY: &str = "plate_solve.config";

/// Plate solve configuration, stored as JSON in the settings table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlateSolveConfig {
    #[serde(default = "default_max_image_stars")]
    pub max_image_stars: usize,
    #[serde(default = "default_min_matched_stars")]
    pub min_matched_stars: usize,
    #[serde(default = "default_verification_tolerance_px")]
    pub verification_tolerance_px: f64,
    #[serde(default = "default_index_mag_limit")]
    pub index_mag_limit: f32,
    #[serde(default = "default_hash_tolerance")]
    pub hash_tolerance: f64,
    #[serde(default = "default_sip_order")]
    pub sip_order: u8,
    /// Minimum inlier / expected-in-FOV ratio for the density-aware
    /// acceptance gate. 0.10 means "10% of catalog stars in the FOV must
    /// match". Only used on the dense-field branch (>100 catalog stars
    /// in FOV); sparse fields use a lower absolute floor. Default: 0.10.
    #[serde(default = "default_min_inlier_ratio")]
    pub min_inlier_ratio: f64,
    /// Progressive star-count retry passes. The solver first tries with
    /// the first value; if the density-aware acceptance fails, it retries
    /// with the next. Default: [150, 300, 600]. Dim fields and dense
    /// fields that need more stars to find good quads benefit from the
    /// later passes.
    #[serde(default = "default_retry_passes")]
    pub retry_passes: Vec<usize>,
    /// Base verification tolerance in arcseconds. The actual per-frame
    /// pixel tolerance is `base_arcsec / pixel_scale_arcsec`, clamped to
    /// [4, 20] px. Replaces the old fixed-pixel `verification_tolerance_px`
    /// which was too tight on slightly-defocused frames and too loose on
    /// sharp narrow-FOV frames. Default: 8.0".
    #[serde(default = "default_base_verification_tolerance_arcsec")]
    pub base_verification_tolerance_arcsec: f64,
    /// Use the rough (no-PSF) star detector for blind solving. When true,
    /// `solve_frame` calls `ImageAnalyzer::detect_fast` instead of `analyze`,
    /// cutting end-to-end blind-solve time from ~6 s to ~1 s on a typical
    /// full-frame image. Defaults to `true`.
    #[serde(default = "default_use_fast_detection")]
    pub use_fast_detection: bool,
    /// Maximum great-circle distance (in degrees) for the "Autofind object
    /// from coordinates" feature to accept a DSO match as a label. Tighter
    /// values reject more frames but avoid labelling unrelated fields with
    /// distant named objects. Default: 0.5°.
    #[serde(default = "default_autofind_tolerance_deg")]
    pub autofind_tolerance_deg: f64,
    /// Number of worker threads for batch plate solving. `0` means auto:
    /// `(cores / 3).clamp(2, 8)`. Each worker solves one frame at a time and
    /// shares the global rayon pool for intra-frame star detection.
    #[serde(default = "default_batch_concurrency")]
    pub batch_concurrency: u32,
    /// Maximum great-circle distance (degrees) between a candidate's seed
    /// WCS center and the FITS positional hint (RA/Dec) before the candidate
    /// is rejected. A generous default tolerates moderate mount sync errors
    /// while still eliminating dense-Milky-Way false positives that would
    /// otherwise win the inlier tournament on raw count alone. Skipped when
    /// no positional hint is available (true blind solve). Set to a value
    /// >= 180 to disable gating entirely. Default: 10.0°.
    #[serde(default = "default_position_hint_radius_deg")]
    pub position_hint_radius_deg: f64,
    /// Per-dimension tolerance for the catalog quad-index hash lookup. With
    /// `1` (the default), each query probes the target bucket plus ±1
    /// neighbors and accepts entries whose 5 hash dimensions are all within
    /// ±1 bin. With `2`, it widens to ±2 — ~7× more candidates per query
    /// at the cost of more verification work, but rescues frames whose
    /// detection produces ratios drifted by 1-2% (typical for OSC images
    /// whose green-only interpolation biases red/blue star centroids).
    /// Don't go higher than 2: the false-positive rate grows fast and the
    /// positional-prior gate is the only thing keeping noise alignments out.
    /// Default: 1.
    #[serde(default = "default_index_lookup_tolerance")]
    pub index_lookup_tolerance: i32,
    /// When a focal-length-hinted solve fails, automatically escalate through
    /// less-constrained retries before giving up: (2) clear the pixel-scale
    /// hint but keep the positional prior, then (3) a full blind solve with
    /// both the scale hint and the positional prior cleared. A wrong FITS
    /// `FOCALLEN` (focal reducer not accounted for, wrong rig profile, binning
    /// mismatch) otherwise filters out every correct candidate and the solve
    /// fails permanently even though a blind solve would succeed. On success
    /// after the scale hint was cleared the corrected focal length is written
    /// back to the frame. Default: true.
    #[serde(default = "default_fallback_to_blind_scale")]
    pub fallback_to_blind_scale: bool,
    /// Apply the stricter acceptance gate on the blind / full-blind path
    /// (scale hint cleared and/or position prior disabled). The hinted
    /// stage-1 path is never affected. Default: true.
    #[serde(default = "default_blind_gate_enabled")]
    pub blind_gate_enabled: bool,
    /// RMS-residual ceiling on the blind path, as a multiple of the
    /// per-frame adaptive pixel tolerance. Reject if
    /// rms_residual_px > mult * adaptive_tol_px. Loose backstop (rms was
    /// non-discriminating in calibration). Default: 2.5.
    #[serde(default = "default_blind_rms_max_px_mult")]
    pub blind_rms_max_px_mult: f64,
    /// Minimum inlier_ratio on the blind path, applied only to dense fields
    /// (expected_in_fov > 100); sparse fields keep the absolute floor. This
    /// is the primary false-positive discriminator. Default: 0.04.
    #[serde(default = "default_blind_min_inlier_ratio")]
    pub blind_min_inlier_ratio: f64,
    /// Absolute minimum inliers on the blind path (>= min_matched_stars).
    /// Weak backstop only. Default: 12.
    #[serde(default = "default_blind_inlier_floor")]
    pub blind_inlier_floor: usize,
    /// Recovered pixel scale must be within [min,max] arcsec/px on the
    /// blind path (nonphysical-rig guard). Default: 0.05 .. 60.0.
    #[serde(default = "default_blind_scale_sanity_min")]
    pub blind_scale_sanity_min: f64,
    #[serde(default = "default_blind_scale_sanity_max")]
    pub blind_scale_sanity_max: f64,
    /// If the header gave a pixel scale, the recovered scale must be within
    /// this factor of it. Deliberately generous so a legitimately very-wrong
    /// header FOCALLEN (the whole point of the blind fallback) is not
    /// rejected. Default: 8.0.
    #[serde(default = "default_blind_scale_header_tol")]
    pub blind_scale_header_tol: f64,
    /// Solver backend: `"legacy"` (prebuilt-index retry cascade) or
    /// `"astap"` (per-trial gnomonic ASTAP port). Default `"legacy"` until
    /// the astap path is bench-proven; flipping this is the cutover switch.
    #[serde(default = "default_solver_backend")]
    pub solver_backend: String,
    /// Per-camera pixel-size defaults (`INSTRUME` or `TELESCOP` → xpixsz_µm).
    /// Consulted by the focallen back-fill when a frame's FITS header lacks
    /// `XPIXSZ` (some surveys ship sparse headers — e.g. SkyMapper). Without
    /// a default, focallen cannot be algebraically derived from arcsec/px
    /// alone (two unknowns, one equation), so `frames.focallen` stays NULL.
    /// Empty default → behaviour unchanged.
    #[serde(default)]
    pub camera_defaults: HashMap<String, f64>,
}

fn default_max_image_stars() -> usize { 300 }
fn default_min_matched_stars() -> usize { 6 }
fn default_verification_tolerance_px() -> f64 { 10.0 }
fn default_index_mag_limit() -> f32 { 13.0 }
fn default_hash_tolerance() -> f64 { 0.005 }
fn default_sip_order() -> u8 { 3 }
fn default_use_fast_detection() -> bool { true }
fn default_autofind_tolerance_deg() -> f64 { 0.5 }
fn default_batch_concurrency() -> u32 { 0 }
fn default_min_inlier_ratio() -> f64 { 0.10 }
fn default_retry_passes() -> Vec<usize> { vec![50, 150, 300, 600] }
fn default_base_verification_tolerance_arcsec() -> f64 { 8.0 }
fn default_position_hint_radius_deg() -> f64 { 10.0 }
fn default_index_lookup_tolerance() -> i32 { 1 }
fn default_fallback_to_blind_scale() -> bool { true }
fn default_blind_gate_enabled() -> bool { true }
fn default_blind_rms_max_px_mult() -> f64 { 2.5 }
fn default_blind_min_inlier_ratio() -> f64 { 0.04 }
fn default_blind_inlier_floor() -> usize { 12 }
fn default_blind_scale_sanity_min() -> f64 { 0.05 }
fn default_blind_scale_sanity_max() -> f64 { 60.0 }
fn default_blind_scale_header_tol() -> f64 { 8.0 }
fn default_solver_backend() -> String { "legacy".to_string() }

impl Default for PlateSolveConfig {
    fn default() -> Self {
        Self {
            max_image_stars: default_max_image_stars(),
            min_matched_stars: default_min_matched_stars(),
            verification_tolerance_px: default_verification_tolerance_px(),
            index_mag_limit: default_index_mag_limit(),
            hash_tolerance: default_hash_tolerance(),
            sip_order: default_sip_order(),
            use_fast_detection: default_use_fast_detection(),
            autofind_tolerance_deg: default_autofind_tolerance_deg(),
            batch_concurrency: default_batch_concurrency(),
            min_inlier_ratio: default_min_inlier_ratio(),
            retry_passes: default_retry_passes(),
            base_verification_tolerance_arcsec: default_base_verification_tolerance_arcsec(),
            position_hint_radius_deg: default_position_hint_radius_deg(),
            index_lookup_tolerance: default_index_lookup_tolerance(),
            fallback_to_blind_scale: default_fallback_to_blind_scale(),
            blind_gate_enabled: default_blind_gate_enabled(),
            blind_rms_max_px_mult: default_blind_rms_max_px_mult(),
            blind_min_inlier_ratio: default_blind_min_inlier_ratio(),
            blind_inlier_floor: default_blind_inlier_floor(),
            blind_scale_sanity_min: default_blind_scale_sanity_min(),
            blind_scale_sanity_max: default_blind_scale_sanity_max(),
            blind_scale_header_tol: default_blind_scale_header_tol(),
            solver_backend: default_solver_backend(),
            camera_defaults: HashMap::new(),
        }
    }
}

/// Load the plate solve config from the database. Returns default if not set.
pub fn load_config(conn: &Connection) -> PlateSolveConfig {
    let result: Result<String, _> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        [SETTINGS_KEY],
        |row| row.get(0),
    );

    match result {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => PlateSolveConfig::default(),
    }
}

/// Save the plate solve config to the database.
pub fn save_config(conn: &Connection, config: &PlateSolveConfig) -> Result<()> {
    let json = serde_json::to_string(config)?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![SETTINGS_KEY, json],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_json_loads_with_blind_gate_defaults() {
        let old = r#"{"max_image_stars":300,"min_matched_stars":6}"#;
        let cfg: PlateSolveConfig = serde_json::from_str(old).unwrap();
        assert!(cfg.blind_gate_enabled);
        assert_eq!(cfg.blind_min_inlier_ratio, 0.04);
        assert!(cfg.blind_inlier_floor >= cfg.min_matched_stars);
        assert_eq!(cfg.blind_scale_header_tol, 8.0);
    }
}

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

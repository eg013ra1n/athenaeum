use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

const SETTINGS_KEY: &str = "plate_solve.config";

/// Plate solve configuration, stored as JSON in the settings table.
///
/// The solver itself is `solvemyastro` (the only backend); these are the
/// app-level knobs around it — the SIP order passed through, the DSO-label
/// search radius, batch concurrency, the persisted-result confidence gate,
/// and per-camera pixel-size defaults. Every field is `#[serde(default)]`,
/// so configs written by older versions (which carried extra, now-removed
/// keys) still load — unknown keys are ignored.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlateSolveConfig {
    /// SIP distortion polynomial order passed to `solvemyastro` (2 or 3).
    /// Higher orders fit more distortion at the cost of needing more matched
    /// stars. Default: 3.
    #[serde(default = "default_sip_order")]
    pub sip_order: u8,
    /// Base verification tolerance in arcseconds. The actual per-frame
    /// pixel tolerance is `base_arcsec / pixel_scale_arcsec`, clamped to
    /// [4, 20] px — tight FOVs get smaller pixel tolerances, wide-field
    /// frames larger ones. Default: 8.0".
    #[serde(default = "default_base_verification_tolerance_arcsec")]
    pub base_verification_tolerance_arcsec: f64,
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
    /// Apply the stricter acceptance gate before persisting a solve (defends
    /// against catalog-corruption false positives writing WCS/focal length
    /// back with override=1). Default: true.
    #[serde(default = "default_blind_gate_enabled")]
    pub blind_gate_enabled: bool,
    /// RMS-residual ceiling, as a multiple of the per-frame adaptive pixel
    /// tolerance. Reject if `rms_residual_px > mult * adaptive_tol_px`. Loose
    /// backstop (rms was non-discriminating in calibration). Default: 2.5.
    #[serde(default = "default_blind_rms_max_px_mult")]
    pub blind_rms_max_px_mult: f64,
    /// Minimum inlier_ratio, applied only to dense fields (expected_in_fov >
    /// 100); sparse fields keep the absolute floor. This is the primary
    /// false-positive discriminator. Default: 0.04.
    #[serde(default = "default_blind_min_inlier_ratio")]
    pub blind_min_inlier_ratio: f64,
    /// Absolute minimum inliers — a TRUE hard floor, not a discriminator.
    /// The real-library calibration found inlier COUNT does not separate real
    /// solves from false positives (`inlier_ratio` does — see
    /// `blind_min_inlier_ratio`), so this is only a backstop. Set to 6 to match
    /// solvemyastro's own `MIN_ABSOLUTE_INLIERS` acceptance floor: the solver
    /// never emits a solution below 6 inliers, so athenaeum never
    /// count-rejects a solve the solver itself already accepted. Raising it
    /// above 6 silently discards correct-but-sparse solves — e.g. a slightly
    /// out-of-focus frame whose bloated stars yield only ~10 inliers but a
    /// healthy ratio and the right sky position. Default: 6.
    #[serde(default = "default_blind_inlier_floor")]
    pub blind_inlier_floor: usize,
    /// Recovered pixel scale must be within [min,max] arcsec/px
    /// (nonphysical-rig guard). Default: 0.05 .. 60.0.
    #[serde(default = "default_blind_scale_sanity_min")]
    pub blind_scale_sanity_min: f64,
    #[serde(default = "default_blind_scale_sanity_max")]
    pub blind_scale_sanity_max: f64,
    /// If the header gave a pixel scale, the recovered scale must be within
    /// this factor of it. Deliberately generous so a legitimately very-wrong
    /// header FOCALLEN is not rejected. Default: 8.0.
    #[serde(default = "default_blind_scale_header_tol")]
    pub blind_scale_header_tol: f64,
    /// Per-camera pixel-size defaults (`INSTRUME` or `TELESCOP` → xpixsz_µm).
    /// Consulted by the focallen back-fill when a frame's FITS header lacks
    /// `XPIXSZ` (some surveys ship sparse headers — e.g. SkyMapper). Without
    /// a default, focallen cannot be algebraically derived from arcsec/px
    /// alone (two unknowns, one equation), so `frames.focallen` stays NULL.
    /// Empty default → behaviour unchanged.
    #[serde(default)]
    pub camera_defaults: HashMap<String, f64>,
    /// Path to the optional bright sub-catalog used by the solvemyastro
    /// backend for fast quad matching (G<14 stars with per-HEALPix-cell
    /// density top-up; see `solvemyastro build-bright-cache`). When
    /// `None` or absent, solvemyastro uses only the deep catalog. The verify
    /// stage always uses the deep catalog regardless of this setting.
    #[serde(default)]
    pub bright_cache_path: Option<String>,
}

fn default_sip_order() -> u8 {
    3
}
fn default_base_verification_tolerance_arcsec() -> f64 {
    8.0
}
fn default_autofind_tolerance_deg() -> f64 {
    0.5
}
fn default_batch_concurrency() -> u32 {
    0
}
fn default_blind_gate_enabled() -> bool {
    true
}
fn default_blind_rms_max_px_mult() -> f64 {
    2.5
}
fn default_blind_min_inlier_ratio() -> f64 {
    0.04
}
fn default_blind_inlier_floor() -> usize {
    // = solvemyastro MIN_ABSOLUTE_INLIERS. Not a discriminator (the calibration
    // showed count does not separate real/false — inlier_ratio does); only a
    // floor matching the solver's own acceptance minimum. See the field doc.
    6
}
fn default_blind_scale_sanity_min() -> f64 {
    0.05
}
fn default_blind_scale_sanity_max() -> f64 {
    60.0
}
fn default_blind_scale_header_tol() -> f64 {
    8.0
}

impl Default for PlateSolveConfig {
    fn default() -> Self {
        Self {
            sip_order: default_sip_order(),
            base_verification_tolerance_arcsec: default_base_verification_tolerance_arcsec(),
            autofind_tolerance_deg: default_autofind_tolerance_deg(),
            batch_concurrency: default_batch_concurrency(),
            blind_gate_enabled: default_blind_gate_enabled(),
            blind_rms_max_px_mult: default_blind_rms_max_px_mult(),
            blind_min_inlier_ratio: default_blind_min_inlier_ratio(),
            blind_inlier_floor: default_blind_inlier_floor(),
            blind_scale_sanity_min: default_blind_scale_sanity_min(),
            blind_scale_sanity_max: default_blind_scale_sanity_max(),
            blind_scale_header_tol: default_blind_scale_header_tol(),
            camera_defaults: HashMap::new(),
            bright_cache_path: None,
        }
    }
}

/// The pre-2026-05 default for `blind_inlier_floor`. `save_config` serializes
/// every field, so this old default was baked into stored configs and a plain
/// `serde(default)` bump never reaches existing users (the key is present, so
/// the default is not applied). The field is an internal backstop with no UI,
/// so a stored 12 is always a stale default — never a deliberate choice — and
/// `load_config` normalizes it to the current default on read.
const STALE_BLIND_INLIER_FLOOR: usize = 12;

/// Load the plate solve config from the database. Returns default if not set.
pub fn load_config(conn: &Connection) -> PlateSolveConfig {
    let result: Result<String, _> = conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        [SETTINGS_KEY],
        |row| row.get(0),
    );

    let mut config = match result {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => PlateSolveConfig::default(),
    };
    migrate_config(&mut config);
    config
}

/// In-place normalization of stale serialized defaults for internal (no-UI)
/// gate fields, applied on every load. New/unset configs already get the
/// current defaults via serde, so this only affects configs persisted before
/// a default changed. Currently: the old `blind_inlier_floor` of 12 — too
/// strict, it discarded correct-but-sparse solves (e.g. slightly out-of-focus
/// frames that match only ~10 stars) — is mapped to the current default (6).
fn migrate_config(config: &mut PlateSolveConfig) {
    if config.blind_inlier_floor == STALE_BLIND_INLIER_FLOOR {
        config.blind_inlier_floor = default_blind_inlier_floor();
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
    fn legacy_config_json_loads_ignoring_removed_keys() {
        // A config written by an older version carried keys that no longer
        // exist (max_image_stars, min_matched_stars, solver_backend, …). serde
        // ignores unknown keys, so it must still deserialize and fill every
        // field from defaults.
        let old = r#"{"max_image_stars":300,"min_matched_stars":6,"solver_backend":"legacy"}"#;
        let cfg: PlateSolveConfig = serde_json::from_str(old).unwrap();
        assert!(cfg.blind_gate_enabled);
        assert_eq!(cfg.blind_min_inlier_ratio, 0.04);
        assert!(cfg.blind_inlier_floor >= 6);
        assert_eq!(cfg.blind_scale_header_tol, 8.0);
        assert_eq!(cfg.sip_order, 3);
    }

    #[test]
    fn load_config_migrates_stale_blind_inlier_floor() {
        use crate::db::schema::init_db;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // A config persisted before the fix — real shape: removed keys + the
        // old serialized blind_inlier_floor default of 12. serde keeps the
        // present 12 (the new serde default of 6 never applies), so load_config
        // must normalize it down so the defocus fix actually reaches the gate.
        let stored = r#"{"sip_order":3,"blind_gate_enabled":true,"blind_min_inlier_ratio":0.04,"blind_inlier_floor":12,"solver_backend":"legacy"}"#;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('plate_solve.config', ?1)",
            [stored],
        )
        .unwrap();
        assert_eq!(
            load_config(&conn).blind_inlier_floor,
            6,
            "stale old-default floor of 12 must be normalized to the current default on load"
        );

        // A non-stale explicit value is preserved — the migration is narrowly
        // scoped to the old default, not a blanket clamp.
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('plate_solve.config', ?1)",
            [r#"{"blind_inlier_floor":20}"#],
        )
        .unwrap();
        assert_eq!(
            load_config(&conn).blind_inlier_floor,
            20,
            "non-stale floors must be preserved"
        );
    }
}

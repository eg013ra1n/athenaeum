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
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
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
    /// Refuse to solve a frame whose own analysis carries the `trailed` verdict (stars the measurement could not fit
    /// as stars: streaks). The verdict alone decides; the former eccentricity / trail-R² pair was removed (Plan C
    /// C7, `docs/platesolving/input-and-acceptance-gates.md`). A stored config that still carries the two old keys
    /// decodes: they are ignored. Default: on.
    #[serde(default = "default_true")]
    pub input_gate_enabled: bool,
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
    ///
    /// Unread since 2026-09-10 (the plate-solve-era registration flow that
    /// consulted this field, `require_bright_cache`, was retired): kept for
    /// JSON compatibility with a stored config; the solver resolves its
    /// caches itself — see `resolve_layer_caches`.
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
fn default_true() -> bool {
    true
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
            input_gate_enabled: default_true(),
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

/// Bumped when the solver recipe hard-coded in
/// [`crate::plate_solve::service::solve_frame_with_hints`] changes (quad
/// tolerance, catalog magnitude limit, SIP fitting), and whenever a solver
/// upgrade should re-solve frames a stacking run cached as SOLVED: bumping it
/// moves every cached solve's hash. A cached failure or refusal needs no
/// bump: one written by another app version is tried once more anyway
/// (review I7, `stacking::solve::cached_outcome_holds`).
pub const SOLVE_RECIPE_VERSION: u32 = 1;

/// What a cached solve depends on (plan 6b-2 C14): the recipe, the SIP
/// order, and every acceptance-gate field. Not the batch concurrency, the
/// autofind tolerance, the input gate (a run applies none) or the camera
/// defaults (they only back-fill a catalog row's focal length).
pub fn solver_config_value(cfg: &PlateSolveConfig) -> serde_json::Value {
    serde_json::json!({
        "recipe": SOLVE_RECIPE_VERSION,
        "sipOrder": cfg.sip_order,
        "baseVerificationToleranceArcsec": cfg.base_verification_tolerance_arcsec,
        "blindGateEnabled": cfg.blind_gate_enabled,
        "blindRmsMaxPxMult": cfg.blind_rms_max_px_mult,
        "blindMinInlierRatio": cfg.blind_min_inlier_ratio,
        "blindInlierFloor": cfg.blind_inlier_floor,
        "blindScaleSanityMin": cfg.blind_scale_sanity_min,
        "blindScaleSanityMax": cfg.blind_scale_sanity_max,
        "blindScaleHeaderTol": cfg.blind_scale_header_tol,
    })
}

/// [`solver_config_value`]'s canonical JSON as XXH3-64, 16 hex digits. The
/// keys come out sorted because no dependency enables serde_json's
/// `preserve_order`, the same convention `stacking/config.rs` documents for
/// its stage hashes.
pub fn solver_config_hash(cfg: &PlateSolveConfig) -> String {
    let json = serde_json::to_string(&solver_config_value(cfg))
        .expect("a solver config value always serializes");
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(json.as_bytes()))
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

    /// Plan C C7 (PC-12, REMOVE): a config stored while the eccentricity / trail-R² pair existed still decodes; the two
    /// keys are ignored and every other value, the switch included, is kept.
    #[test]
    fn a_stored_config_with_the_removed_gate_fields_decodes() {
        use crate::db::schema::init_db;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let stored = r#"{"sip_order":2,"input_gate_enabled":false,"input_max_eccentricity":0.85,"input_min_trail_r2":0.65,"blind_inlier_floor":20}"#;
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('plate_solve.config', ?1)",
            [stored],
        )
        .unwrap();
        let cfg = load_config(&conn);
        assert!(!cfg.input_gate_enabled, "the switch is kept");
        assert_eq!((cfg.sip_order, cfg.blind_inlier_floor), (2, 20), "the document decoded as a whole, not to defaults");
        // Saving writes the two keys no more.
        save_config(&conn, &cfg).unwrap();
        let json: String = conn
            .query_row("SELECT value FROM settings WHERE key = 'plate_solve.config'", [], |r| r.get(0))
            .unwrap();
        assert!(!json.contains("input_max_eccentricity") && !json.contains("input_min_trail_r2"), "{json}");
    }

    /// Plan 6b-2 C14: a cached solve depends on the recipe and on every field
    /// that shapes a solve or its acceptance, and on nothing else.
    #[test]
    fn the_solver_config_hash_covers_what_shapes_a_solve() {
        let cfg = PlateSolveConfig::default();
        assert_eq!(
            serde_json::to_string(&solver_config_value(&cfg)).unwrap(),
            r#"{"baseVerificationToleranceArcsec":8.0,"blindGateEnabled":true,"blindInlierFloor":6,"blindMinInlierRatio":0.04,"blindRmsMaxPxMult":2.5,"blindScaleHeaderTol":8.0,"blindScaleSanityMax":60.0,"blindScaleSanityMin":0.05,"recipe":1,"sipOrder":3}"#
        );
        let h = solver_config_hash(&cfg);
        assert_eq!(h.len(), 16);
        assert_eq!(h, "b1516bcd97659ab0");
        let mut same = cfg.clone();
        same.batch_concurrency = 4;
        same.autofind_tolerance_deg = 2.0;
        same.input_gate_enabled = false;
        same.bright_cache_path = Some("/x/bright".into());
        same.camera_defaults.insert("cam".into(), 3.76);
        assert_eq!(solver_config_hash(&same), h);
        let mut sip = cfg.clone();
        sip.sip_order = 2;
        assert_ne!(solver_config_hash(&sip), h);
        let mut tol = cfg.clone();
        tol.blind_scale_header_tol = 4.0;
        assert_ne!(solver_config_hash(&tol), h);
    }
}

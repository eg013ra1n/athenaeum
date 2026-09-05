// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

export type PlateSolveConfig = { 
/**
 * SIP distortion polynomial order passed to `solvemyastro` (2 or 3).
 * Higher orders fit more distortion at the cost of needing more matched
 * stars. Default: 3.
 */
sip_order: number, 
/**
 * Base verification tolerance in arcseconds. The actual per-frame
 * pixel tolerance is `base_arcsec / pixel_scale_arcsec`, clamped to
 * [4, 20] px — tight FOVs get smaller pixel tolerances, wide-field
 * frames larger ones. Default: 8.0".
 */
base_verification_tolerance_arcsec: number, 
/**
 * Maximum great-circle distance (in degrees) for the "Autofind object
 * from coordinates" feature to accept a DSO match as a label. Tighter
 * values reject more frames but avoid labelling unrelated fields with
 * distant named objects. Default: 0.5°.
 */
autofind_tolerance_deg: number, 
/**
 * Number of worker threads for batch plate solving. `0` means auto:
 * `(cores / 3).clamp(2, 8)`. Each worker solves one frame at a time and
 * shares the global rayon pool for intra-frame star detection.
 */
batch_concurrency: number, 
/**
 * Apply the stricter acceptance gate before persisting a solve (defends
 * against catalog-corruption false positives writing WCS/focal length
 * back with override=1). Default: true.
 */
blind_gate_enabled: boolean, 
/**
 * RMS-residual ceiling, as a multiple of the per-frame adaptive pixel
 * tolerance. Reject if `rms_residual_px > mult * adaptive_tol_px`. Loose
 * backstop (rms was non-discriminating in calibration). Default: 2.5.
 */
blind_rms_max_px_mult: number, 
/**
 * Minimum inlier_ratio, applied only to dense fields (expected_in_fov >
 * 100); sparse fields keep the absolute floor. This is the primary
 * false-positive discriminator. Default: 0.04.
 */
blind_min_inlier_ratio: number, 
/**
 * Absolute minimum inliers — a TRUE hard floor, not a discriminator.
 * The real-library calibration found inlier COUNT does not separate real
 * solves from false positives (`inlier_ratio` does — see
 * `blind_min_inlier_ratio`), so this is only a backstop. Set to 6 to match
 * solvemyastro's own `MIN_ABSOLUTE_INLIERS` acceptance floor: the solver
 * never emits a solution below 6 inliers, so athenaeum never
 * count-rejects a solve the solver itself already accepted. Raising it
 * above 6 silently discards correct-but-sparse solves — e.g. a slightly
 * out-of-focus frame whose bloated stars yield only ~10 inliers but a
 * healthy ratio and the right sky position. Default: 6.
 */
blind_inlier_floor: number, 
/**
 * Recovered pixel scale must be within [min,max] arcsec/px
 * (nonphysical-rig guard). Default: 0.05 .. 60.0.
 */
blind_scale_sanity_min: number, blind_scale_sanity_max: number, 
/**
 * If the header gave a pixel scale, the recovered scale must be within
 * this factor of it. Deliberately generous so a legitimately very-wrong
 * header FOCALLEN is not rejected. Default: 8.0.
 */
blind_scale_header_tol: number, 
/**
 * Refuse to solve a frame whose own analysis says its stars are streaks.
 * Default: on.
 */
input_gate_enabled: boolean, 
/**
 * A frame is refused when its median star eccentricity is at least this
 * AND its trail R² is at least [`PlateSolveConfig::input_min_trail_r2`].
 *
 * Both, because either alone catches frames that solve perfectly well:
 * measured on real files (2026-09-05), frames that solve correctly sit at
 * eccentricity 0.62–0.72 with trail R² 0.30–0.57, while hopeless ones sit
 * at 0.90–0.96 with 0.73–0.93. Default: 0.85.
 */
input_max_eccentricity: number, 
/**
 * Trail-fit R² above which the elongation is a tracking failure rather
 * than seeing. Default: 0.65.
 */
input_min_trail_r2: number, 
/**
 * Per-camera pixel-size defaults (`INSTRUME` or `TELESCOP` → xpixsz_µm).
 * Consulted by the focallen back-fill when a frame's FITS header lacks
 * `XPIXSZ` (some surveys ship sparse headers — e.g. SkyMapper). Without
 * a default, focallen cannot be algebraically derived from arcsec/px
 * alone (two unknowns, one equation), so `frames.focallen` stays NULL.
 * Empty default → behaviour unchanged.
 */
camera_defaults: { [key in string]: number }, 
/**
 * Path to the optional bright sub-catalog used by the solvemyastro
 * backend for fast quad matching (G<14 stars with per-HEALPix-cell
 * density top-up; see `solvemyastro build-bright-cache`). When
 * `None` or absent, solvemyastro uses only the deep catalog. The verify
 * stage always uses the deep catalog regardless of this setting.
 */
bright_cache_path: string | null, };

export type PlateSolveRecord = { id: number | null, frame_id: number, crpix1: number, crpix2: number, crval1: number, crval2: number, cd1_1: number, cd1_2: number, cd2_1: number, cd2_2: number, sip_order: number | null, sip_a_coeffs: string | null, sip_b_coeffs: string | null, sip_ap_coeffs: string | null, sip_bp_coeffs: string | null, matched_stars: number, total_detected: number, rms_residual_px: number, rms_residual_arcsec: number, pixel_scale_arcsec: number, field_rotation_deg: number, solve_time_ms: number, catalog_used: string, algorithm_used: string, solved_at: string, 
/**
 * Number of catalog stars that fell within the solved field of view.
 * Used by the density-aware acceptance gate. None for pre-density-aware
 * solves stored before the migration.
 */
expected_catalog_stars_in_fov: number | null, 
/**
 * matched_stars / expected_catalog_stars_in_fov — solve confidence
 * signal independent of absolute star count. Closer to 1.0 = stronger
 * match. None for pre-density-aware solves.
 */
inlier_ratio: number | null, };

export type FovSummary = { light_count: number, computable_count: number, min_fov_deg: number | null, narrowest_instrume: string | null, };

export type AutofindStatus = "processing" | "labeled" | "no_match" | "already_labeled" | "missing_coords" | "error";


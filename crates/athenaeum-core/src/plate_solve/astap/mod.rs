//! ASTAP-port plate solver (per-trial gnomonic-consistent quad matching).
//!
//! A faithful Rust port of ASTAP's command-line blind solver: an autoFOV
//! ladder × square-spiral sky search where, at each (FOV, sky-cell) trial,
//! the catalog cone is gnomonically projected onto the *same flat plane as
//! the image* and quads are built with the *same function on both sides*
//! (the verified fix for the wide-field/dense failures of the legacy
//! prebuilt-great-circle-index path).
//!
//! Lands behind `PlateSolveConfig.solver_backend == "astap"`; the legacy
//! path is byte-identical while the flag is `"legacy"` (the default). The
//! `index: &QuadIndex` parameter is accepted for API compatibility and
//! ignored here (per-trial quads make a prebuilt index unnecessary).
//!
//! Submodules (filled in by subsequent tasks):
//!  - `fov_ladder`    — ASTAP autoFOV rung generator
//!  - `sky_search`    — ASTAP square-spiral sky-cell generator
//!  - `catalog_source`— pluggable catalog-quad source (Tycho-2 now, Gaia later)
//!  - `trial`         — per-cell project→quad→match→fit→verify
//!  - `refine`        — second full-image solve → WCS

pub mod catalog_source;
pub mod fov_ladder;
pub mod refine;
pub mod sky_search;
pub mod trial;

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;

use astroimage::platesolving::{build_quads_multi, SolveHints};

use crate::catalog::CatalogEngine;
use crate::models::Frame;
use crate::plate_solve::config::PlateSolveConfig;
use crate::plate_solve::gate_audit::GateStage;
use crate::plate_solve::hints::observation_epoch;
use crate::plate_solve::quad_index::QuadIndex;
use crate::plate_solve::service::{
    angular_distance_deg, blind_gate_ok, detect_image_stars, make_gate_metrics,
    required_inliers, SolveResult,
};

use catalog_source::{CatalogQuadSource, Tycho2ConeSource};

/// ASTAP `find_fit` continuous quad-ratio tolerance
/// (`unit_command_line_solving.pas:860-862`, ≈0.007).
const QUAD_TOLERANCE: f64 = 0.007;

/// When a hinted-stage solve lands within this of the user's stated target,
/// the position is independently corroborated and the density inlier floor
/// is relaxed (mirrors the legacy `POS_CORROBORATION_RADIUS_DEG` /
/// `POS_CORROBORATED_MIN_INLIERS` in `service.rs`).
const POS_CORROBORATION_RADIUS_DEG: f64 = 1.0;
const POS_CORROBORATED_MIN_INLIERS: usize = 8;

/// Generous magnitude ceiling for the per-trial catalog read. ASTAP reads
/// the *brightest N* stars regardless of depth (`read_stars`); the
/// brightest-N budget — not this cap — bounds the pool, so this only needs
/// to be deep enough to expose Gaia (G≤16) while harmlessly covering
/// Tycho-2 (tops ≈V12).
const CAT_READ_MAG_LIMIT: f32 = 18.0;

/// Entry point for the ASTAP-port backend. Same signature as
/// [`crate::plate_solve::service::solve_frame_with_hints`] so the dispatch
/// in `solve_frame_with_hints` is a one-line delegation. The `index`
/// parameter is accepted for API compatibility and ignored (per-trial quads
/// make a prebuilt index unnecessary — see plan).
pub fn solve(
    frame: &Frame,
    file_path: &str,
    hints: &SolveHints,
    catalog: &CatalogEngine,
    _index: &QuadIndex,
    config: &PlateSolveConfig,
    thread_pool: Option<Arc<rayon::ThreadPool>>,
) -> Result<SolveResult> {
    let total_start = Instant::now();
    let filename = std::path::Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.to_string());

    // 1. Detection front-end (shared with legacy — same stars, size).
    let (image_stars, image_size, _snr_twin) =
        detect_image_stars(file_path, &filename, config, thread_pool)?;
    let image_positions: Vec<(f64, f64)> =
        image_stars.iter().map(|s| (s.x, s.y)).collect();
    let image_center =
        (image_size.0 as f64 / 2.0, image_size.1 as f64 / 2.0);
    let obs_epoch = observation_epoch(frame);
    let n = image_stars.len();

    // 2. Image quads — ASTAP `find_many_quads` group-size ladder by star
    // count; the same group size is used for the projected catalog so both
    // quad families are structurally identical (trial.rs).
    let group_size = if n <= 29 {
        6
    } else if n <= 59 {
        5
    } else {
        4
    };
    let image_quads = build_quads_multi(&image_positions, image_positions.len(), group_size);
    if image_quads.len() < 10 {
        anyhow::bail!(
            "[{filename}] astap: only {} image quads from {n} stars (need ≥10)",
            image_quads.len()
        );
    }
    let min_quads = trial::minimum_quads(n);

    // 3. autoFOV ladder × square-spiral sky search. Hinted ⇒ small ladder +
    // a disk around the prior; blind ⇒ full ladder, full-sky spiral.
    let hint_scale = hints.pixel_scale_arcsec;
    let long_px = image_size.0.max(image_size.1) as usize;
    let rungs = fov_ladder::fov_rungs(long_px, hint_scale);
    let center = match (hints.ra, hints.dec) {
        (Some(ra), Some(dec)) => Some((ra, dec)),
        _ => None,
    };
    let stage = GateStage::from_params(hint_scale, center.is_none());
    let src = Tycho2ConeSource::new(catalog);

    eprintln!(
        "plate_solve [{}]: astap backend — {n} stars, {} quads (g{group_size}), \
         {} FOV rungs, {} (stage {:?})",
        filename,
        image_quads.len(),
        rungs.len(),
        if center.is_some() { "hinted spiral" } else { "blind full-sky" },
        stage,
    );

    // ASTAP `solve_image` density matching: read the BRIGHTEST `budget`
    // catalog stars within an `oversize·FOV` window so the catalog quad pool
    // has the same star density as the image — the alignment that makes the
    // correct quad co-occur in both pools (the verified fix; the prior fixed
    // mag-cut over a 0.75·FOV cone broke this).
    let ovr = trial::oversize(n);
    let budget = trial::catalog_star_budget(n, image_size.0, image_size.1, ovr);
    eprintln!(
        "plate_solve [{}]: astap density-match — oversize {:.2}, catalog budget {} \
         (brightest, ≤mag {})",
        filename, ovr, budget, CAT_READ_MAG_LIMIT
    );

    for rung in &rungs {
        let cone_radius = (rung.fov_deg * ovr).min(89.0);
        for cell in
            sky_search::spiral_cells(center, config.position_hint_radius_deg, rung.fov_deg)
        {
            let mut cat_stars = src.stars_in_cone(
                cell.0,
                cell.1,
                cone_radius,
                CAT_READ_MAG_LIMIT,
                obs_epoch,
            );
            if cat_stars.len() < 4 {
                continue;
            }
            // Keep only the brightest `budget` (ASTAP reads brightest-N, not
            // a fixed magnitude cut). cone_search aggregates per-pixel files
            // so the result isn't globally mag-sorted — sort then truncate.
            if cat_stars.len() > budget {
                cat_stars.sort_by(|a, b| {
                    a.mag.partial_cmp(&b.mag).unwrap_or(std::cmp::Ordering::Equal)
                });
                cat_stars.truncate(budget);
            }

            let Some(fit) = trial::run_cell(
                &image_quads,
                &cat_stars,
                cell,
                group_size,
                QUAD_TOLERANCE,
                min_quads,
            ) else {
                continue;
            };

            let Some((mut result, inliers, expected)) = refine::refine_trial(
                &fit,
                &image_stars,
                image_size,
                image_center,
                catalog,
                config,
                obs_epoch,
                hint_scale,
                &filename,
            ) else {
                continue;
            };

            // Density-aware acceptance gate — identical policy to the legacy
            // `run_retry_passes` final gate, including the hinted
            // position-corroborated relaxation.
            let required_density = required_inliers(
                expected,
                n,
                config.min_inlier_ratio,
                config.min_matched_stars,
            );
            let (cra, cdec) =
                result.wcs.pixel_to_sky(image_center.0, image_center.1);
            let required = if stage == GateStage::Hinted {
                match (hints.ra, hints.dec) {
                    (Some(hra), Some(hdec))
                        if angular_distance_deg(cra, cdec, hra, hdec)
                            <= POS_CORROBORATION_RADIUS_DEG =>
                    {
                        required_density.min(
                            config
                                .min_matched_stars
                                .max(POS_CORROBORATED_MIN_INLIERS),
                        )
                    }
                    _ => required_density,
                }
            } else {
                required_density
            };
            let gate_m = make_gate_metrics(
                inliers,
                expected,
                result.rms_residual_px,
                result.pixel_scale_arcsec,
                result.inlier_ratio,
                hint_scale,
                config.base_verification_tolerance_arcsec,
            );
            if inliers < required || !blind_gate_ok(stage, &gate_m, config) {
                continue;
            }

            // Derived focal length when the header lacks FOCALLEN (mirrors
            // the legacy post-solve block; the astap path never *clears* a
            // hint, so there is no FOCALLEN correction here).
            let pixel_scale = result.pixel_scale_arcsec;
            result.derived_focallen_mm = if frame.focallen.is_none() && pixel_scale > 0.0 {
                frame.xpixsz.and_then(|xpixsz| {
                    if xpixsz <= 0.0 {
                        return None;
                    }
                    let pixel_size_mm = xpixsz / 1000.0;
                    let binning = frame.xbinning.unwrap_or(1).max(1) as f64;
                    let effective_pixel_mm = pixel_size_mm * binning;
                    let scale_tan = (pixel_scale / 3600.0).to_radians().tan();
                    if scale_tan > 0.0 {
                        Some(effective_pixel_mm / scale_tan)
                    } else {
                        None
                    }
                })
            } else {
                None
            };
            result.focallen_corrected = false;
            result.solve_time_ms = total_start.elapsed().as_millis() as u64;

            eprintln!(
                "plate_solve [{}]: SOLVED (astap) RA={:.4} Dec={:.4} \
                 scale={:.3}\"/px rot={:.1}° inliers={} {}ms",
                filename,
                cra,
                cdec,
                result.pixel_scale_arcsec,
                result.field_rotation_deg,
                inliers,
                result.solve_time_ms,
            );
            return Ok(result);
        }
    }

    anyhow::bail!(
        "[{filename}] astap: no trial passed verification across {} FOV rung(s)",
        rungs.len()
    )
}

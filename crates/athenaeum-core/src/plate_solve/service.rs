//! Blind plate solving using a pre-built all-sky quad index.
//!
//! Pipeline:
//! 1. Detect stars in the image
//! 2. Build image quads (nearest-neighbor, one per star)
//! 3. Compute hash keys
//! 4. Look up each hash in the index → candidate catalog quads
//! 5. Cluster candidates by derived sky position (robust against hash collisions)
//! 6. For the best cluster, compute per-candidate sky-to-pixel correspondences
//! 7. Fit a similarity transform (rotation + scale + translation)
//! 8. Verify: cone search around derived position, count re-projected inliers
//! 9. Promote to full WCS

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use rusqlite::Connection;

use astroimage::platesolving::{
    build_quads_multi, CatalogStar, GnomonicProjection, ImageStar, Quad, SolveHints, WcsSolution,
};
use astroimage::ImageAnalyzer;

use crate::catalog::CatalogEngine;
use crate::models::Frame;
use crate::plate_solve::config::PlateSolveConfig;
use crate::plate_solve::gate_audit::{self, GateStage};
use crate::plate_solve::hints::{extract_hints, observation_epoch};
use crate::plate_solve::quad_index::{hash_key_from_ratios, QuadIndex, QuadLookup};
use crate::plate_solve::dso_lookup::DsoCatalog;
use crate::plate_solve::storage::{
    insert_plate_solve, update_frame_from_solve, update_frame_object_if_missing, PlateSolveRecord,
};

/// Minimum stars for a depth-matched pass to be worth running (a quad needs
/// 4; below ~this the field is effectively unsolvable anyway).
const QUAD_MIN_STARS: usize = 4;

/// When a hinted-stage candidate lands within this radius of the user's
/// stated target (OBJCTRA/OBJCTDEC), the field position is independently
/// corroborated, so the absolute inlier-count requirement is relaxed to
/// [`POS_CORROBORATED_MIN_INLIERS`]. Far tighter than the 10° positional
/// prior — these solves land within ~0.1°.
const POS_CORROBORATION_RADIUS_DEG: f64 = 1.0;

/// ASTAP-class minimum inliers for a position+scale-corroborated solve.
/// ASTAP accepts on ≥3 matched quads + transform sanity; astrometry.net
/// treats a position prior as decisive. 8 geometrically-consistent inliers
/// at the exact stated target with a ±5%-matched scale and tight RMS is
/// unambiguously a true solve (sparse long-FL narrowband fields physically
/// contain fewer than the density-based requirement of ~20).
const POS_CORROBORATED_MIN_INLIERS: usize = 8;

/// Result of a single frame plate solve.
#[derive(Clone, Debug)]
pub struct SolveResult {
    pub wcs: WcsSolution,
    pub matched_stars: usize,
    pub total_detected: usize,
    pub rms_residual_px: f64,
    pub rms_residual_arcsec: f64,
    pub pixel_scale_arcsec: f64,
    pub field_rotation_deg: f64,
    pub solve_time_ms: u64,
    pub catalog_used: String,
    pub algorithm_used: String,
    pub derived_focallen_mm: Option<f64>,
    /// True when `derived_focallen_mm` overrides a wrong header FOCALLEN
    /// (the solve only succeeded after the scale hint was cleared). Drives
    /// the unconditional focal-length write-back in `store_result`.
    pub focallen_corrected: bool,
    /// Number of catalog stars inside the solved FOV (from the verification
    /// cone search). Drives the density-aware acceptance gate.
    pub expected_catalog_stars_in_fov: usize,
    /// matched_stars / expected_catalog_stars_in_fov. Confidence signal
    /// independent of absolute inlier count — a 8-of-10 match in a sparse
    /// field can be stronger than 25-of-500 in a dense field.
    pub inlier_ratio: f64,
}

/// Solve a single frame using the pre-built all-sky quad index.
///
/// Convenience wrapper: extracts hints from the frame (hitting `conn` for
/// the "nearby solved frame" fallback) then delegates to
/// [`solve_frame_with_hints`]. Use this for one-off single-frame callers.
/// Batch callers should pre-extract hints in the main thread and call
/// [`solve_frame_with_hints`] directly so workers never touch the DB.
pub fn solve_frame(
    frame: &Frame,
    file_path: &str,
    conn: &Connection,
    catalog: &CatalogEngine,
    index: &QuadIndex,
    config: &PlateSolveConfig,
    thread_pool: Option<Arc<rayon::ThreadPool>>,
) -> Result<SolveResult> {
    let hints = extract_hints(frame, Some(conn));
    solve_frame_with_hints(frame, file_path, &hints, catalog, index, config, thread_pool)
}

/// Solve a single frame using pre-extracted hints. This is the hot-path
/// function used by the batch worker pool — it is DB-free, fully `Send`,
/// and shares read-only catalog/index/config state across threads.
pub fn solve_frame_with_hints(
    frame: &Frame,
    file_path: &str,
    hints: &SolveHints,
    catalog: &CatalogEngine,
    index: &QuadIndex,
    config: &PlateSolveConfig,
    thread_pool: Option<Arc<rayon::ThreadPool>>,
) -> Result<SolveResult> {
    let total_start = Instant::now();
    let filename = std::path::Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.to_string());

    // Backend dispatch. The ASTAP-port solver owns its entire pipeline; the
    // legacy path below is byte-identical while solver_backend == "legacy"
    // (the default until the astap path is bench-proven — see plan).
    if config.solver_backend.eq_ignore_ascii_case("astap") {
        return crate::plate_solve::astap::solve(
            frame, file_path, hints, catalog, index, config, thread_pool,
        );
    }

    // 1. Star detection — fast (no PSF fit) or precise depending on config.
    // Cap at max(retry_passes) or 500 so the later retry passes have enough
    // stars to work with.
    let max_detection_cap = config
        .retry_passes
        .iter()
        .copied()
        .max()
        .unwrap_or(config.max_image_stars)
        .max(500);

    let t0 = Instant::now();
    // Disable the saturation-fraction reject: the analyzer's default 0.95
    // (peak > 62258 of 65535 → "saturated, drop") is appropriate for FWHM /
    // PSF measurement where a clipped flat top biases the fit, but it's
    // counterproductive for plate-solving. The brightest stars in the field
    // are exactly the ones most likely to be in the catalog (Tycho-2 caps
    // at V≈11.5 — anything brighter than that saturates a 180s OSC exposure
    // first), and they're indispensable for quad construction. Saturation
    // shifts centroids by ~1-2 px which is fine: quad ratios are scale-
    // invariant (a 1 px shift on a 200 px edge changes a ratio by 0.5%) and
    // the verification gate is 4 px wide.
    //
    // Without this, OSC frames whose brightest stars saturate the green
    // channel after green-interpolation lose all their hash-matchable bright
    // stars, leaving only mid-faint stars whose quad ratios don't match
    // catalog quads built from bright catalog stars.
    let mut analyzer = ImageAnalyzer::new()
        .with_detection_sigma(5.0)
        .with_max_stars(max_detection_cap)
        .with_saturation_fraction(1.0);
    if let Some(pool) = thread_pool {
        analyzer = analyzer.with_thread_pool(pool);
    }

    let (image_stars, image_size, snr_first): (
        Vec<ImageStar>,
        (u32, u32),
        Option<Vec<ImageStar>>,
    ) = if config.use_fast_detection {
        // Adaptive multi-level detector (falling-threshold ladder with
        // occupancy mask + per-tile deep pass): pulls usable, well-centroided
        // stars out of nebulosity/galaxy-swamped, long-focal-length fields
        // where a single global threshold under-detects.
        let r = analyzer
            .detect_fast(file_path)
            .with_context(|| format!("star detection failed for {file_path}"))?;
        if r.stars.len() < 20 {
            return Err(anyhow::anyhow!(
                "only {} stars detected (need >= 20)",
                r.stars.len()
            ));
        }
        let stars: Vec<ImageStar> = r
            .stars
            .iter()
            .map(|s| ImageStar {
                x: s.x as f64,
                y: s.y as f64,
                flux: s.flux as f64,
            })
            .collect();
        // Same stars, re-ranked by aperture SNR. A bright galaxy/nebula
        // injects extended knots with huge flux but low SNR (flux spread
        // over a large aperture: `flux / sqrt(flux + π r² σ²)`); they top
        // the flux ranking and poison the quad pool, while real point
        // sources have high SNR. SNR-ranking (ASTAP `get_brightest_stars`)
        // demotes the structure. Used only by the additive SNR rescue
        // (after the normal flux-ordered cascade fails) so frames that
        // solve normally are never reordered.
        let mut pk: Vec<&_> = r.stars.iter().collect();
        pk.sort_by(|a, b| {
            b.snr.partial_cmp(&a.snr).unwrap_or(std::cmp::Ordering::Equal)
        });
        let snr_first: Vec<ImageStar> = pk
            .iter()
            .map(|s| ImageStar {
                x: s.x as f64,
                y: s.y as f64,
                flux: s.flux as f64,
            })
            .collect();
        (stars, (r.width as u32, r.height as u32), Some(snr_first))
    } else {
        let analysis = analyzer
            .analyze(file_path)
            .with_context(|| format!("star detection failed for {file_path}"))?;
        if analysis.stars.len() < 20 {
            return Err(anyhow::anyhow!(
                "only {} stars detected (need >= 20)",
                analysis.stars.len()
            ));
        }
        let stars: Vec<ImageStar> = analysis
            .stars
            .iter()
            .map(|s| ImageStar {
                x: s.x as f64,
                y: s.y as f64,
                flux: s.flux as f64,
            })
            .collect();
        (stars, (analysis.width as u32, analysis.height as u32), None)
    };

    let image_center = (image_size.0 as f64 / 2.0, image_size.1 as f64 / 2.0);
    eprintln!(
        "plate_solve [{}]: star detection {}ms ({} stars, {}x{}, fast={})",
        filename,
        t0.elapsed().as_millis(),
        image_stars.len(),
        image_size.0,
        image_size.1,
        config.use_fast_detection,
    );

    // 2. Progressive retry + escalating fallback. Star detection (the slow
    // step) ran once above; the constraint-only retry loop lives in
    // `run_retry_passes` so it can be re-invoked with progressively looser
    // hints while reusing the same detected stars.
    let obs_epoch = observation_epoch(frame);
    // Positions computed once; every retry pass across every stage reads the
    // same data.
    let image_positions: Vec<(f64, f64)> =
        image_stars.iter().map(|s| (s.x, s.y)).collect();

    // Three-stage escalating fallback. Stage 1 is the historical behavior
    // (scale + position hints). When it fails and the user hasn't disabled
    // the fallback, escalate: stage 2 drops the pixel-scale hint (a wrong
    // FITS FOCALLEN — focal reducer, wrong rig profile, binning mismatch —
    // otherwise filters out every correct candidate), stage 3 additionally
    // drops the positional prior (bad mount sync). Star detection already
    // ran once above; each stage just re-runs the cheap quad/verify loop.
    let has_scale_hint = hints.pixel_scale_arcsec.is_some();

    // The 3-stage escalation as a closure, so it can be re-run on a
    // stellarity-filtered star list (extended-object rescue below).
    let solve_cascade = |stars: &[ImageStar],
                         positions: &[(f64, f64)]|
     -> (anyhow::Result<(SolveResult, usize, usize)>, bool) {
        let mut sc = false;
        // Stage 1 — hinted (scale + position). Tight 5% scale filter.
        let mut solved = run_retry_passes(
            stars, positions, image_size, image_center,
            hints.pixel_scale_arcsec, false, hints, catalog, index, config,
            obs_epoch, &filename, 0.05,
        );
        if solved.is_err() && config.fallback_to_blind_scale {
            // Stage 2 — clear the pixel-scale hint, keep the positional prior.
            if has_scale_hint {
                let e1 = solved.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
                eprintln!(
                    "plate_solve [{}]: hinted solve failed ({}); retrying blind \
                     (scale hint cleared, position prior kept)",
                    filename, e1
                );
                solved = run_retry_passes(
                    stars, positions, image_size, image_center, None, false,
                    hints, catalog, index, config, obs_epoch, &filename, 0.05,
                );
                if solved.is_ok() {
                    sc = true;
                }
            }
            // Stage 3a — ASTAP-style blind FOV/scale ladder. With no scale
            // hint at all (headerless frame) a scaleless solve is swamped by
            // false high-inlier matches at absurd scales, so instead sweep
            // FOV coarse→fine, largest first, ÷1.5 per rung (9.5°→~0.37°).
            // Each rung feeds a candidate pixel scale to the proven hinted
            // machinery; the all-sky position search is implicit in the
            // global Tycho-2 quad index, so no positional spiral is needed.
            // The scale filter is widened to ~0.30 so the true scale falls
            // within tolerance of the nearest rung while absurd-scale hash
            // collisions are still rejected. First accepted rung wins.
            // Frames that *had* a FOCALLEN keep only the scaleless fallback
            // below (the wide band adds false-positive risk and they do not
            // reach here in practice).
            if solved.is_err() && hints.pixel_scale_arcsec.is_none() {
                let long_px = image_size.0.max(image_size.1) as f64;
                let mut fov = 9.5_f64;
                while fov >= 0.37 && solved.is_err() {
                    let scale = fov * 3600.0 / long_px;
                    eprintln!(
                        "plate_solve [{}]: blind FOV ladder — FOV {:.2}° (≈{:.2}\"/px)",
                        filename, fov, scale
                    );
                    solved = run_retry_passes(
                        stars, positions, image_size, image_center,
                        Some(scale), true, hints, catalog, index, config,
                        obs_epoch, &filename, 0.30,
                    );
                    fov /= 1.5;
                }
                if solved.is_ok() {
                    // Scale was recovered from no prior → write it back.
                    sc = true;
                }
            }
            // Stage 3b — scaleless full blind: also drop the positional prior.
            if solved.is_err() {
                let prev = solved.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
                eprintln!(
                    "plate_solve [{}]: retrying full blind \
                     (scale + position prior cleared) [prev: {}]",
                    filename, prev
                );
                solved = run_retry_passes(
                    stars, positions, image_size, image_center, None, true,
                    hints, catalog, index, config, obs_epoch, &filename, 0.05,
                );
                if solved.is_ok() {
                    sc = has_scale_hint;
                }
            }
        }
        (solved, sc)
    };

    // `scale_corrected` is true once a stage that cleared the scale hint
    // produced the winning result — the header FOCALLEN is then known-wrong
    // and gets corrected on writeback.
    let (mut solved, mut scale_corrected) = solve_cascade(&image_stars, &image_positions);

    // SNR-ordered rescue (cheap; tried before the analyze() rescue). A
    // bright galaxy/nebula injects extended knots with huge FLUX but low
    // aperture SNR; they top the flux-ranked quad pool and stop a correct
    // quad forming (visually confirmed on M51 — the brightest-by-flux
    // detections sat on the galaxy core/spiral). Re-run the cascade with
    // the SAME detections re-ranked by SNR: compact point sources lead,
    // extended structure sinks. Purely additive — runs only after the
    // normal flux-ordered cascade has failed, so frames that solve normally
    // (incl. NGC 2024, which a global SNR re-rank regressed) are never
    // reordered.
    if solved.is_err() {
        if let Some(pk) = snr_first.as_ref() {
            if pk.len() >= 20 {
                let pos: Vec<(f64, f64)> = pk.iter().map(|s| (s.x, s.y)).collect();
                eprintln!(
                    "plate_solve [{}]: SNR-ordered rescue — re-solving with {} \
                     SNR-ranked stars",
                    filename,
                    pk.len()
                );
                let (s2, sc2) = solve_cascade(pk, &pos);
                if s2.is_ok() {
                    solved = s2;
                    scale_corrected = sc2;
                }
            }
        }
    }

    // Extended-object rescue: `detect_fast` returns flux only (no shape), so
    // on frames with a bright nebula/galaxy the brightest detections — which
    // feed quad building — are non-stellar structure, not stars, and no
    // correct quad ever forms (ASTAP-oracle bench: galaxy/nebula frames had
    // only ~40% of the top-50 detections be real stars vs ~88% on solvable
    // starfields). If everything failed and we used the fast path, re-detect
    // with the full analyzer and keep only compact, round, well-detected
    // sources, then re-run the cascade. Fast-solving frames never reach here,
    // so their speed/behaviour is unchanged.
    if solved.is_err() && config.use_fast_detection {
        if let Ok(an) = analyzer.analyze(file_path) {
            let mut fwhms: Vec<f64> = an
                .stars
                .iter()
                .map(|s| s.fwhm as f64)
                .filter(|v| v.is_finite() && *v > 0.0)
                .collect();
            if fwhms.len() >= 20 {
                fwhms.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let med = fwhms[fwhms.len() / 2];
                let (lo, hi) = (med * 0.4, med * 2.5);
                // ASTAP `find_stars` star selection ported faithfully
                // (`unit_command_line_solving.pas:1454` accept gate,
                // `:1571` `get_brightest_stars`): reject too-large
                // (nebula/galaxy/saturated blob — HFD-ceiling ≈ fwhm≤2.5·med),
                // too-small (hot pixel — fwhm≥0.4·med ≡ hfd_min), elongated
                // (ecc), and faint (ASTAP **SNR>10**, not the old ≥5), then
                // keep the survivors **ranked by SNR** (ASTAP keeps the
                // brightest-by-SNR `max_stars`; the cascade builds quads from
                // the leading N, so SNR-first ordering = ASTAP's exact
                // compact-point-source quad pool — the M78 fix).
                let mut kept: Vec<&_> = an
                    .stars
                    .iter()
                    .filter(|s| {
                        let f = s.fwhm as f64;
                        s.eccentricity < 0.7
                            && f.is_finite()
                            && f >= lo
                            && f <= hi
                            && s.snr > 10.0
                    })
                    .collect();
                kept.sort_by(|a, b| {
                    b.snr.partial_cmp(&a.snr).unwrap_or(std::cmp::Ordering::Equal)
                });
                let clean: Vec<ImageStar> = kept
                    .iter()
                    .map(|s| ImageStar {
                        x: s.x as f64,
                        y: s.y as f64,
                        flux: s.flux as f64,
                    })
                    .collect();
                eprintln!(
                    "plate_solve [{}]: extended-object rescue — ASTAP star select \
                     {} → {} stars (ecc<0.7, fwhm∈[{:.1},{:.1}], SNR>10, SNR-ranked); re-solving",
                    filename,
                    an.stars.len(),
                    clean.len(),
                    lo,
                    hi
                );
                if clean.len() >= 20 {
                    let pos2: Vec<(f64, f64)> = clean.iter().map(|s| (s.x, s.y)).collect();
                    let (s2, sc2) = solve_cascade(&clean, &pos2);
                    if s2.is_ok() {
                        solved = s2;
                        scale_corrected = sc2;
                    }
                }
            }
        }
    }

    // First stage to succeed wins; if all failed, surface the last (most
    // permissive) attempt's error.
    let (mut result, _best_inliers, _best_expected_in_fov) = solved?;

    let pixel_scale = result.pixel_scale_arcsec;

    // 10. Derived focal length. Normally only filled when the header lacks
    // FOCALLEN. But when the solve only succeeded after the (FOCALLEN-
    // derived) scale hint was cleared, the header value is proven wrong, so
    // we recompute and overwrite it. Inverts hints.rs's pixel-scale formula
    // exactly (atan, binning-aware) so a re-solve of the corrected frame
    // round-trips to the same scale.
    let derived_fl = if (frame.focallen.is_none() || scale_corrected) && pixel_scale > 0.0 {
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
    // Only a write-back *correction* when the header had a (wrong) value we
    // are overriding — scale_corrected implies a FOCALLEN-derived hint, so
    // frame.focallen was necessarily Some.
    result.focallen_corrected = scale_corrected && derived_fl.is_some();

    let total_ms = total_start.elapsed().as_millis() as u64;
    let (solved_ra, solved_dec) = result.wcs.pixel_to_sky(image_center.0, image_center.1);
    eprintln!(
        "plate_solve [{}]: SOLVED RA={:.4} Dec={:.4} scale={:.3}\"/px rot={:.1}° {}ms",
        filename,
        solved_ra,
        solved_dec,
        result.pixel_scale_arcsec,
        result.field_rotation_deg,
        total_ms
    );

    result.solve_time_ms = total_ms;
    result.derived_focallen_mm = derived_fl;
    Ok(result)
}

/// Persist a solve result to the database.
///
/// If `dso_catalog` is provided, the nearest named deep-sky object at the
/// solved position is looked up and — if the frame's `object` field is
/// currently NULL or empty — used to label the frame.
pub fn store_result(
    conn: &Connection,
    frame_id: i64,
    result: &SolveResult,
    dso_catalog: Option<&DsoCatalog>,
    config: &PlateSolveConfig,
) -> Result<()> {
    // Defense-in-depth against catalog corruption: never write a solve's
    // WCS / focal length back (with override=1) unless it clears the strict
    // confidence bar — applied here REGARDLESS of acceptance stage, so a
    // false positive that slipped through the gate-exempt hinted path (e.g.
    // because a wrong/corrupt header made it look hinted) cannot overwrite
    // frame metadata. inlier_ratio is the rig-independent discriminator
    // (real solves >= ~0.08, noise alignments <= ~0.001); sparse fields are
    // exempted inside blind_gate_ok exactly as during acceptance.
    if config.blind_gate_enabled {
        let m = BlindGateMetrics {
            inliers: result.matched_stars,
            expected_in_fov: result.expected_catalog_stars_in_fov,
            rms_px: result.rms_residual_px,
            adaptive_tol_px: adaptive_tol_px(
                result.pixel_scale_arcsec,
                config.base_verification_tolerance_arcsec,
            ),
            inlier_ratio: result.inlier_ratio,
            recovered_scale_arcsec: result.pixel_scale_arcsec,
            header_scale_arcsec: None,
        };
        if !blind_gate_ok(GateStage::ScaleCleared, &m, config) {
            eprintln!(
                "plate_solve: refusing to persist low-confidence solve for frame {frame_id} \
                 (inliers={} ratio={:.5} scale={:.3}\"/px) — WCS/focal length NOT written back",
                result.matched_stars, result.inlier_ratio, result.pixel_scale_arcsec
            );
            return Ok(());
        }
    }

    let record = PlateSolveRecord {
        id: None,
        frame_id,
        crpix1: result.wcs.crpix.0,
        crpix2: result.wcs.crpix.1,
        crval1: result.wcs.crval.0,
        crval2: result.wcs.crval.1,
        cd1_1: result.wcs.cd[0][0],
        cd1_2: result.wcs.cd[0][1],
        cd2_1: result.wcs.cd[1][0],
        cd2_2: result.wcs.cd[1][1],
        sip_order: None,
        sip_a_coeffs: None,
        sip_b_coeffs: None,
        sip_ap_coeffs: None,
        sip_bp_coeffs: None,
        matched_stars: result.matched_stars as i32,
        total_detected: result.total_detected as i32,
        rms_residual_px: result.rms_residual_px,
        rms_residual_arcsec: result.rms_residual_arcsec,
        pixel_scale_arcsec: result.pixel_scale_arcsec,
        field_rotation_deg: result.field_rotation_deg,
        solve_time_ms: result.solve_time_ms as i64,
        catalog_used: result.catalog_used.clone(),
        algorithm_used: result.algorithm_used.clone(),
        solved_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        expected_catalog_stars_in_fov: Some(result.expected_catalog_stars_in_fov as i32),
        inlier_ratio: Some(result.inlier_ratio),
    };
    insert_plate_solve(conn, &record)?;
    update_frame_from_solve(
        conn,
        frame_id,
        result.wcs.crval.0,
        result.wcs.crval.1,
        result.field_rotation_deg,
        result.derived_focallen_mm,
        result.focallen_corrected,
    )?;

    // Optional: look up the nearest named DSO and set frame.object if empty.
    if let Some(catalog) = dso_catalog {
        if let Some(m) = catalog.find_best(result.wcs.crval.0, result.wcs.crval.1) {
            match update_frame_object_if_missing(conn, frame_id, &m.designation) {
                Ok(true) => eprintln!(
                    "plate_solve: labelled frame {} as '{}' ({:?}, {:.2}° away)",
                    frame_id, m.designation, m.reason, m.distance_deg
                ),
                Ok(false) => {}
                Err(e) => eprintln!(
                    "plate_solve: failed to update frame.object for {}: {}",
                    frame_id, e
                ),
            }
        }
    }

    Ok(())
}

/// Star-detection front-end shared with the ASTAP-port backend.
///
/// This is the exact detection logic used by the legacy path above
/// (`ImageAnalyzer` with the saturation reject disabled, fast or precise
/// per config, the ≥20 floor, and the SNR-reranked twin used by the SNR
/// rescue). The legacy `solve_frame_with_hints` body is intentionally left
/// inline and byte-identical (the #1 project rule: legacy stays unchanged
/// until the astap path is bench-proven — see plan); this helper is the
/// astap path's single source for the same behaviour. The brief duplication
/// is removed when the legacy cascade is deleted (plan Task 9).
///
/// Returns `(stars, (width,height), snr_reranked_twin)`. `snr_twin` is
/// `Some` only on the fast path (it feeds the additive SNR rescue).
pub(crate) fn detect_image_stars(
    file_path: &str,
    filename: &str,
    config: &PlateSolveConfig,
    thread_pool: Option<Arc<rayon::ThreadPool>>,
) -> Result<(Vec<ImageStar>, (u32, u32), Option<Vec<ImageStar>>)> {
    let max_detection_cap = config
        .retry_passes
        .iter()
        .copied()
        .max()
        .unwrap_or(config.max_image_stars)
        .max(500);

    let t0 = Instant::now();
    let mut analyzer = ImageAnalyzer::new()
        .with_detection_sigma(5.0)
        .with_max_stars(max_detection_cap)
        .with_saturation_fraction(1.0);
    if let Some(pool) = thread_pool {
        analyzer = analyzer.with_thread_pool(pool);
    }

    let (image_stars, image_size, snr_first): (
        Vec<ImageStar>,
        (u32, u32),
        Option<Vec<ImageStar>>,
    ) = if config.use_fast_detection {
        let r = analyzer
            .detect_fast(file_path)
            .with_context(|| format!("star detection failed for {file_path}"))?;
        if r.stars.len() < 20 {
            return Err(anyhow::anyhow!(
                "only {} stars detected (need >= 20)",
                r.stars.len()
            ));
        }
        let stars: Vec<ImageStar> = r
            .stars
            .iter()
            .map(|s| ImageStar {
                x: s.x as f64,
                y: s.y as f64,
                flux: s.flux as f64,
            })
            .collect();
        let mut pk: Vec<&_> = r.stars.iter().collect();
        pk.sort_by(|a, b| {
            b.snr.partial_cmp(&a.snr).unwrap_or(std::cmp::Ordering::Equal)
        });
        let snr_first: Vec<ImageStar> = pk
            .iter()
            .map(|s| ImageStar {
                x: s.x as f64,
                y: s.y as f64,
                flux: s.flux as f64,
            })
            .collect();
        (stars, (r.width as u32, r.height as u32), Some(snr_first))
    } else {
        let analysis = analyzer
            .analyze(file_path)
            .with_context(|| format!("star detection failed for {file_path}"))?;
        if analysis.stars.len() < 20 {
            return Err(anyhow::anyhow!(
                "only {} stars detected (need >= 20)",
                analysis.stars.len()
            ));
        }
        let stars: Vec<ImageStar> = analysis
            .stars
            .iter()
            .map(|s| ImageStar {
                x: s.x as f64,
                y: s.y as f64,
                flux: s.flux as f64,
            })
            .collect();
        (stars, (analysis.width as u32, analysis.height as u32), None)
    };

    eprintln!(
        "plate_solve [{}]: star detection {}ms ({} stars, {}x{}, fast={})",
        filename,
        t0.elapsed().as_millis(),
        image_stars.len(),
        image_size.0,
        image_size.1,
        config.use_fast_detection,
    );

    Ok((image_stars, image_size, snr_first))
}

// ────────── helpers ──────────

#[derive(Clone, Debug)]
struct Candidate {
    image_quad: Quad,
    catalog: QuadLookup,
}

/// Run the progressive-retry pass loop and the density-aware acceptance
/// gate for one constraint configuration, then return the accepted result
/// (plus its inlier count and FOV density) or an `Err` describing the
/// failure mode.
///
/// Star detection is the caller's job — this function is constraint-only,
/// so the caller can re-invoke it with progressively looser hints while
/// reusing the same detected stars. `expected_scale_arcsec = None` disables
/// the ±5 % candidate scale filter (blind-on-scale); `disable_position_gate`
/// suppresses the positional-prior gate (full blind).
#[allow(clippy::too_many_arguments)]
fn run_retry_passes(
    image_stars: &[ImageStar],
    image_positions: &[(f64, f64)],
    image_size: (u32, u32),
    image_center: (f64, f64),
    expected_scale_arcsec: Option<f64>,
    disable_position_gate: bool,
    hints: &SolveHints,
    catalog: &CatalogEngine,
    index: &QuadIndex,
    config: &PlateSolveConfig,
    obs_epoch: f64,
    filename: &str,
    scale_filter_tol: f64,
) -> Result<(SolveResult, usize, usize)> {
    if let Some(s) = expected_scale_arcsec {
        eprintln!(
            "plate_solve [{}]: expected pixel scale from header: {:.3}\"/px",
            filename, s
        );
    }

    // If the config's retry_passes is empty or all values are 0, fall back
    // to a single pass at the legacy max_image_stars value.
    let mut passes: Vec<usize> = if config.retry_passes.iter().any(|n| *n > 0) {
        config.retry_passes.iter().copied().filter(|n| *n > 0).collect()
    } else {
        vec![config.max_image_stars]
    };

    // ASTAP-style catalog-depth matching. The fixed [50,150,300,600] ladder
    // takes the brightest-by-flux detections; at long focal length a 300 s
    // sub detects far fainter than the Tycho-2 quad index goes (V≤~11.5), so
    // the brightest-50 quad pool is dominated by faint stars with NO catalog
    // counterpart and a correct quad hash never forms — even though the field
    // has plenty of usable Tycho-2 stars. ASTAP caps the image star count to
    // what the catalog actually contains in this FOV. When we have a pointing
    // and a scale hint, count the catalog stars in the FOV at the index's
    // magnitude depth and prepend passes sized to that (and 2×, for detection
    // incompleteness), so the brightest-N image stars correspond to the same
    // physical stars the catalog quad index was built from. Only active with
    // a scale hint (stage 1) → blind/scale-cleared stages are unchanged, and
    // dense wide fields just see two extra small early passes.
    if let (Some(scale), Some(ra), Some(dec)) =
        (expected_scale_arcsec, hints.ra, hints.dec)
    {
        let (w, h) = (image_size.0 as f64, image_size.1 as f64);
        let fov_diag_deg = (w * w + h * h).sqrt() * scale / 3600.0;
        if fov_diag_deg.is_finite() && fov_diag_deg > 0.0 && fov_diag_deg < 90.0 {
            let radius = (0.55 * fov_diag_deg).min(89.0);
            let n_cat = catalog
                .cone_search(ra, dec, radius, config.index_mag_limit, obs_epoch)
                .map(|(s, _)| s.len())
                .unwrap_or(0);
            if n_cat >= QUAD_MIN_STARS {
                let avail = image_stars.len();
                let mut matched: Vec<usize> = [n_cat, n_cat.saturating_mul(2)]
                    .into_iter()
                    .map(|n| n.clamp(QUAD_MIN_STARS, avail.max(QUAD_MIN_STARS)))
                    .filter(|&n| !passes.contains(&n))
                    .collect();
                matched.dedup();
                eprintln!(
                    "plate_solve [{}]: catalog-depth match — {} Tycho-2 stars \
                     in {:.2}° FOV (mag≤{:.1}); prepending depth-matched \
                     passes {:?}",
                    filename, n_cat, fov_diag_deg, config.index_mag_limit, matched
                );
                matched.extend(std::mem::take(&mut passes));
                passes = matched;
            }
        }
    }

    let mut best_result: Option<SolveResult> = None;
    let mut best_inliers: usize = 0;
    let mut best_expected_in_fov: usize = 0;

    for (pass_idx, pass_size) in passes.iter().copied().enumerate() {
        let outcome = try_solve_pass(
            image_stars,
            image_positions,
            pass_size,
            filename,
            image_size,
            image_center,
            expected_scale_arcsec,
            hints,
            catalog,
            index,
            config,
            obs_epoch,
            disable_position_gate,
            scale_filter_tol,
        );

        eprintln!(
            "plate_solve [{}]: pass {}/{} stars={} → {} quads, {} candidates, best {} inliers",
            filename,
            pass_idx + 1,
            passes.len(),
            pass_size,
            outcome.image_quads_built,
            outcome.total_candidates,
            outcome.best_inliers
        );

        // Acceptance check on THIS pass's own best — uses its own expected
        // FOV density, not the carried-over best. This prevents a weaker
        // pass from being over-credited (a high-ratio 8-of-10 match should
        // be accepted, even if a later pass finds a 25-of-500 dense-field
        // match with more raw inliers but worse ratio).
        if let Some(ref candidate) = outcome.best {
            let required_this = required_inliers(
                outcome.best_expected_in_fov,
                image_stars.len(),
                config.min_inlier_ratio,
                config.min_matched_stars,
            );
            let stage =
                GateStage::from_params(expected_scale_arcsec, disable_position_gate);

            // Position-corroborated relaxation. The per-candidate path has
            // already required: a hash match, a verifying seed WCS, scale
            // within ±5% of the header, and a tight per-candidate RMS. If, on
            // the Hinted stage, the resulting field centre also lands within
            // ~1° of the user's stated target, the position is independently
            // corroborated and the density-based absolute inlier floor (which
            // assumes ~20 matchable catalog stars) is wrong for sparse
            // long-focal-length narrowband fields — they simply do not
            // contain that many Tycho-2 stars. Relax the *count* only (ASTAP /
            // astrometry.net behaviour); blind stages keep the full gate.
            let (cand_ra, cand_dec) =
                candidate.wcs.pixel_to_sky(image_center.0, image_center.1);
            let required_eff = if stage == GateStage::Hinted {
                match (hints.ra, hints.dec) {
                    (Some(hra), Some(hdec))
                        if angular_distance_deg(cand_ra, cand_dec, hra, hdec)
                            <= POS_CORROBORATION_RADIUS_DEG =>
                    {
                        required_this.min(
                            config
                                .min_matched_stars
                                .max(POS_CORROBORATED_MIN_INLIERS),
                        )
                    }
                    _ => required_this,
                }
            } else {
                required_this
            };

            let gate_m = make_gate_metrics(
                outcome.best_inliers,
                outcome.best_expected_in_fov,
                candidate.rms_residual_px,
                candidate.pixel_scale_arcsec,
                candidate.inlier_ratio,
                hints.pixel_scale_arcsec,
                config.base_verification_tolerance_arcsec,
            );
            let accept = outcome.best_inliers >= required_eff
                && blind_gate_ok(stage, &gate_m, config);
            if gate_audit::enabled() {
                gate_audit::record_event(
                    filename,
                    stage,
                    pass_idx,
                    accept,
                    outcome.best_inliers,
                    outcome.best_expected_in_fov,
                    image_stars.len(),
                    candidate.inlier_ratio,
                    candidate.rms_residual_px,
                    candidate.rms_residual_arcsec,
                    candidate.pixel_scale_arcsec,
                    hints.pixel_scale_arcsec,
                    cand_ra,
                    cand_dec,
                    hints.ra,
                    hints.dec,
                    required_eff,
                );
            }
            if accept {
                best_inliers = outcome.best_inliers;
                best_expected_in_fov = outcome.best_expected_in_fov;
                best_result = Some(candidate.clone());
                eprintln!(
                    "plate_solve [{}]: pass {} accepted — {} inliers ≥ {} required \
                     (FOV density {}, {} required by density)",
                    filename,
                    pass_idx + 1,
                    best_inliers,
                    required_eff,
                    best_expected_in_fov,
                    required_this
                );
                break;
            }
        }

        // Didn't meet density threshold on this pass. Track it as a fallback
        // only if it has more inliers than the running best — the final gate
        // will still reject it if no pass qualifies.
        if outcome.best_inliers > best_inliers {
            best_inliers = outcome.best_inliers;
            best_expected_in_fov = outcome.best_expected_in_fov;
            best_result = outcome.best;
        }
    }

    let Some(result) = best_result else {
        return Err(anyhow::anyhow!(
            "[{}] no candidate passed verification across {} pass(es)",
            filename,
            passes.len()
        ));
    };

    // Final density-aware acceptance gate. Mirrors the per-pass gate,
    // including the position-corroborated relaxation: a Hinted-stage result
    // that lands within ~1° of the user's stated target (with scale already
    // ±5%-filtered and a tight per-candidate RMS) is a true solve even with
    // few inliers — sparse long-focal-length narrowband fields physically
    // contain fewer than the density-based ~20. Without applying the same
    // relaxation here, the per-pass acceptance is silently overridden and
    // the solve is rejected as a "near-miss".
    let required_density = required_inliers(
        best_expected_in_fov,
        image_stars.len(),
        config.min_inlier_ratio,
        config.min_matched_stars,
    );
    let stage = GateStage::from_params(expected_scale_arcsec, disable_position_gate);
    let (best_ra, best_dec) = result.wcs.pixel_to_sky(image_center.0, image_center.1);
    let required = if stage == GateStage::Hinted {
        match (hints.ra, hints.dec) {
            (Some(hra), Some(hdec))
                if angular_distance_deg(best_ra, best_dec, hra, hdec)
                    <= POS_CORROBORATION_RADIUS_DEG =>
            {
                required_density
                    .min(config.min_matched_stars.max(POS_CORROBORATED_MIN_INLIERS))
            }
            _ => required_density,
        }
    } else {
        required_density
    };
    let gate_m = make_gate_metrics(
        best_inliers,
        best_expected_in_fov,
        result.rms_residual_px,
        result.pixel_scale_arcsec,
        result.inlier_ratio,
        hints.pixel_scale_arcsec,
        config.base_verification_tolerance_arcsec,
    );
    let accept = best_inliers >= required && blind_gate_ok(stage, &gate_m, config);
    if gate_audit::enabled() {
        gate_audit::record_event(
            filename,
            stage,
            usize::MAX, // sentinel: final gate, not a pass
            accept,
            best_inliers,
            best_expected_in_fov,
            image_stars.len(),
            result.inlier_ratio,
            result.rms_residual_px,
            result.rms_residual_arcsec,
            result.pixel_scale_arcsec,
            hints.pixel_scale_arcsec,
            best_ra,
            best_dec,
            hints.ra,
            hints.dec,
            required,
        );
    }
    if !accept {
        // Hint depends on what we know about the failure mode. The previous
        // implementation always said "consider rebuilding the quad index with
        // a higher magnitude limit" whenever `best_expected_in_fov > 2000`,
        // which fires precisely on dense-region false positives — sending
        // users to spend 30 minutes rebuilding the index for a no-op (Tycho-2
        // saturates at V≈11.5, so raising `index_mag_limit` does nothing).
        let hint = match (hints.ra, hints.dec) {
            (Some(hra), Some(hdec)) => {
                let dist = angular_distance_deg(best_ra, best_dec, hra, hdec);
                if dist > 5.0 {
                    format!(
                        " — best candidate is {:.0}° from FITS pointing (RA={:.2} Dec={:.2}); \
                         this is almost certainly a noise alignment in a dense region. \
                         Likely cause: the FITS header RA/Dec is wrong (mount sync error), \
                         or the positional-prior gate (config.position_hint_radius_deg, \
                         currently {:.1}°) is too loose. Rebuilding the quad index will NOT help.",
                        dist, hra, hdec, config.position_hint_radius_deg
                    )
                } else {
                    format!(
                        " — best candidate is within {:.1}° of FITS pointing; this is a real \
                         near-miss. Likely cause: centroid noise (try use_fast_detection=false), \
                         pixel scale hint is wrong, or quad geometry is degenerate. \
                         Rebuilding the quad index will NOT help.",
                        dist
                    )
                }
            }
            _ => format!(
                " — true blind solve (no FITS positional hint). Try use_fast_detection=false \
                 for sharper centroids, or set valid RA/DEC (or OBJCTRA/OBJCTDEC) in the FITS \
                 header. Rebuilding the quad index does NOT help on Tycho-2 (catalog saturates \
                 at V≈11.5)."
            ),
        };
        return Err(anyhow::anyhow!(
            "[{}] verification failed: best candidate has {} inliers at RA={:.2} Dec={:.2} (required {}, density {} detected / {} catalog in FOV){}",
            filename,
            best_inliers,
            best_ra,
            best_dec,
            required,
            image_stars.len(),
            best_expected_in_fov,
            hint
        ));
    }

    Ok((result, best_inliers, best_expected_in_fov))
}

/// Outcome of one retry pass — the best result produced, plus diagnostics
/// for the caller to log and to drive the density-aware acceptance gate.
#[derive(Default)]
struct PassOutcome {
    best: Option<SolveResult>,
    best_inliers: usize,
    best_expected_in_fov: usize,
    image_quads_built: usize,
    total_candidates: usize,
}

/// Density-aware minimum inlier requirement. Sparse fields accept 6, mid-
/// density fields use a 20% ratio, dense fields use `min_ratio` with a
/// floor of 20. Caller passes `config.min_matched_stars` as the absolute
/// floor so the gate can never go below the user-configured minimum.
///
/// `detected_count` caps the effective density: the true-inlier ceiling is
/// bounded by the number of stars actually detected in the image, so a
/// Milky Way field with 3 500 catalog stars but only 600 detected image
/// stars should be gated against ~600, not 3 500. This prevents the gate
/// from demanding more inliers than the detector can possibly produce.
pub(crate) fn required_inliers(
    expected_in_fov: usize,
    detected_count: usize,
    min_ratio: f64,
    floor: usize,
) -> usize {
    let effective = expected_in_fov.min(detected_count);
    let target = if effective == 0 {
        floor
    } else if effective <= 30 {
        6
    } else if effective <= 100 {
        (effective as f64 * 0.20).round() as usize
    } else {
        ((effective as f64 * min_ratio).round() as usize).max(20)
    };
    target.max(floor)
}

#[derive(Clone, Debug)]
pub(crate) struct BlindGateMetrics {
    pub inliers: usize,
    pub expected_in_fov: usize,
    pub rms_px: f64,
    pub adaptive_tol_px: f64,
    pub inlier_ratio: f64,
    pub recovered_scale_arcsec: f64,
    pub header_scale_arcsec: Option<f64>,
}

/// Extra acceptance gate applied ONLY on the blind path (scale cleared
/// and/or position prior disabled). The hinted stage-1 path is never
/// affected, so well-working hinted solves do not regress. Calibrated from
/// a real-library audit: `inlier_ratio` is the primary discriminator
/// (real solves >= ~0.08, false positives <= ~0.001); rms and absolute
/// inlier count do not separate, so those are loose backstops.
pub(crate) fn blind_gate_ok(
    stage: GateStage,
    m: &BlindGateMetrics,
    cfg: &PlateSolveConfig,
) -> bool {
    if stage == GateStage::Hinted || !cfg.blind_gate_enabled {
        return true;
    }
    // Geometric fit must be tight (scale-relative ceiling; loose backstop).
    if !m.rms_px.is_finite()
        || m.rms_px > cfg.blind_rms_max_px_mult * m.adaptive_tol_px
    {
        return false;
    }
    // Absolute inlier floor (weak backstop).
    if m.inliers < cfg.blind_inlier_floor {
        return false;
    }
    // Dense-field confidence ratio — the primary false-positive gate.
    // Sparse fields (<=100 expected) are exempt: too few stars to trust a
    // ratio there, and the noise alignments occur in dense regions.
    if m.expected_in_fov > 100 && m.inlier_ratio < cfg.blind_min_inlier_ratio {
        return false;
    }
    // Recovered scale must be physically plausible.
    if !(cfg.blind_scale_sanity_min..=cfg.blind_scale_sanity_max)
        .contains(&m.recovered_scale_arcsec)
    {
        return false;
    }
    // ...and not wildly off the header scale when the header had one
    // (generous: a legitimately very-wrong FOCALLEN is the whole point of
    // the blind fallback, so this is only a coarse backstop).
    if let Some(hs) = m.header_scale_arcsec {
        if hs > 0.0 {
            let r = m.recovered_scale_arcsec / hs;
            if r < 1.0 / cfg.blind_scale_header_tol || r > cfg.blind_scale_header_tol {
                return false;
            }
        }
    }
    true
}

/// Build the blind-gate inputs from the few per-candidate scalars. Single
/// construction site so the per-pass and final-gate callers cannot drift
/// (mirrors the gate_audit::record_event extraction). Cheap — field copies
/// plus one `adaptive_tol_px` division; built unconditionally even on the
/// hinted path (where `blind_gate_ok` early-returns true), which is fine.
pub(crate) fn make_gate_metrics(
    inliers: usize,
    expected_in_fov: usize,
    rms_px: f64,
    pixel_scale_arcsec: f64,
    inlier_ratio: f64,
    header_scale_arcsec: Option<f64>,
    base_tol_arcsec: f64,
) -> BlindGateMetrics {
    BlindGateMetrics {
        inliers,
        expected_in_fov,
        rms_px,
        adaptive_tol_px: adaptive_tol_px(pixel_scale_arcsec, base_tol_arcsec),
        inlier_ratio,
        recovered_scale_arcsec: pixel_scale_arcsec,
        header_scale_arcsec,
    }
}

/// Convert an arcsecond-scale base tolerance to a per-frame pixel tolerance,
/// clamped to [4, 20] px. Tight FOVs get smaller pixel tolerances (fewer
/// false matches); wide-field frames get larger ones (slightly defocused
/// stars still count). Used in place of the old fixed `verification_tolerance_px`.
pub(crate) fn adaptive_tol_px(pixel_scale_arcsec: f64, base_arcsec: f64) -> f64 {
    if pixel_scale_arcsec.abs() < 1e-6 {
        return 10.0;
    }
    (base_arcsec / pixel_scale_arcsec).clamp(4.0, 20.0)
}

/// Great-circle angular distance between two sky positions, in degrees.
pub(crate) fn angular_distance_deg(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
    let dec1r = dec1.to_radians();
    let dec2r = dec2.to_radians();
    let delta_ra = (ra2 - ra1).to_radians();
    let cos_c = dec1r.sin() * dec2r.sin() + dec1r.cos() * dec2r.cos() * delta_ra.cos();
    cos_c.clamp(-1.0, 1.0).acos().to_degrees()
}

/// Reusable cone-search result. Kept across iterations of the candidate
/// loop so that spatially-clustered correct candidates share a single
/// catalog query.
struct ConeCache {
    ra_deg: f64,
    dec_deg: f64,
    radius_deg: f64,
    stars: std::sync::Arc<Vec<CatalogStar>>,
}

/// One solve attempt using the brightest `pass_size` image stars. Runs the
/// full quad build → hash lookup → scale filter → per-candidate verify
/// pipeline and returns the best candidate from this pass.
#[allow(clippy::too_many_arguments)]
fn try_solve_pass(
    image_stars: &[ImageStar],
    image_positions: &[(f64, f64)],
    pass_size: usize,
    filename: &str,
    image_size: (u32, u32),
    image_center: (f64, f64),
    expected_scale_arcsec: Option<f64>,
    hints: &SolveHints,
    catalog: &CatalogEngine,
    index: &QuadIndex,
    config: &PlateSolveConfig,
    obs_epoch: f64,
    disable_position_gate: bool,
    scale_filter_tol: f64,
) -> PassOutcome {
    // Build quads from the brightest `pass_size` stars. Group size follows
    // ASTAP's `find_many_quads` ladder: when only a handful of (catalog-depth)
    // stars are usable — exactly the long-focal-length / small-FOV case — one
    // quad per star almost never produces a correct asterism, so densify to
    // C(6,4)=15 quads/star (≤29 stars) or C(5,4)=5 (≤59). Dense wide fields
    // keep the classic 1 quad/star (group 4) and are unaffected.
    let effective_stars = image_positions.len().min(pass_size);
    let group_size = if effective_stars <= 29 {
        6
    } else if effective_stars <= 59 {
        5
    } else {
        4
    };
    let image_quads = build_quads_multi(image_positions, pass_size, group_size);
    if image_quads.len() < 10 {
        return PassOutcome {
            image_quads_built: image_quads.len(),
            ..Default::default()
        };
    }

    // Hash lookup → candidates. The lookup tolerance is configurable; the
    // default ±1 keeps hash false-positive noise low for sharp mono frames,
    // and bumping to ±2 rescues OSC frames whose centroid bias drifts the
    // image-quad ratios just outside the ±1 window. The positional-prior
    // gate downstream filters out the extra noise from the wider probe, so
    // raising this knob is safe when a hint is present.
    let tolerance = index.hash_tolerance();
    let lookup_tol = config.index_lookup_tolerance;
    let mut candidates: Vec<Candidate> = Vec::new();
    for iq in &image_quads {
        let ratios = [
            iq.ratios[0], iq.ratios[1], iq.ratios[2], iq.ratios[3], iq.ratios[4],
        ];
        let hash_key = hash_key_from_ratios(&ratios, tolerance);
        for hit in index.lookup_with_tolerance(&hash_key, lookup_tol) {
            candidates.push(Candidate {
                image_quad: iq.clone(),
                catalog: hit,
            });
        }
    }

    let total_candidates = candidates.len();
    if candidates.is_empty() {
        return PassOutcome {
            image_quads_built: image_quads.len(),
            ..Default::default()
        };
    }

    // Scale tolerances — intentionally split:
    //   - `filter_scale_tolerance` (caller-supplied) tightens the initial
    //     candidate filter against the expected scale. 0.05 for a real
    //     FOCALLEN+XPIXSZ hint (camera/scope pairs report scale to <1%);
    //     widened (~0.30) for the blind FOV ladder so the true scale falls
    //     within tolerance of the nearest ÷1.5 rung while absurd-scale hash
    //     collisions are still rejected.
    //   - `scale_tolerance` = 0.10 stays generous for the downstream
    //     refit/WCS sanity checks so a fit that drifts slightly during
    //     convergence isn't prematurely rejected.
    let filter_scale_tolerance = scale_filter_tol;
    let scale_tolerance = 0.1;
    let candidates_filtered: Vec<&Candidate> = if let Some(expected) = expected_scale_arcsec {
        candidates
            .iter()
            .filter(|c| {
                let implied_scale =
                    (c.catalog.longest_dist_deg as f64 * 3600.0) / c.image_quad.longest_dist;
                let rel_error = (implied_scale - expected).abs() / expected;
                rel_error < filter_scale_tolerance
            })
            .collect()
    } else {
        candidates.iter().collect()
    };

    // Per-candidate verify + refit. Adaptive tolerance: if we have a scale
    // hint, compute the pixel tolerance up front; otherwise fall back to
    // deriving it from each seed WCS below.
    const MIN_REFIT_INLIERS: usize = 6;
    let base_tol_arcsec = config.base_verification_tolerance_arcsec;
    let hint_tol_px = expected_scale_arcsec.map(|s| adaptive_tol_px(s, base_tol_arcsec));

    let mut outcome = PassOutcome {
        image_quads_built: image_quads.len(),
        total_candidates,
        ..Default::default()
    };

    // Reusable cone-search cache: correct candidates cluster spatially, so
    // a single cone search typically covers many of them. Skip a new query
    // when the current seed's FOV is entirely inside the cached cone.
    let mut cone_cache: Option<ConeCache> = None;

    // Verbose diagnostic mode (env-gated): print every candidate's seed-WCS
    // sky position + distance from the FITS hint, so we can tell whether the
    // candidate set lands near the truth (hash matches but verification fails)
    // or far from it (hash mismatch — image quads have no catalog counterpart).
    // Useful when a frame fails with all-pass "best 0 inliers" and we need to
    // know whether to investigate hash tolerance or verification tolerance.
    let verbose_candidates = std::env::var("ATHENAEUM_PLATESOLVE_VERBOSE").is_ok();
    let mut hint_distance_histogram: [usize; 6] = [0; 6]; // <2°, <5°, <10°, <30°, <90°, ≥90°
    let mut surviving_seeds = 0usize;

    for cand in &candidates_filtered {
        let approx_center = catalog_centroid(&cand.catalog);

        let Some((best_pairs, _best_residual)) = best_permutation_fit(
            &cand.image_quad,
            &cand.catalog,
            &image_positions,
            approx_center,
            image_center,
        ) else {
            continue;
        };

        let Some(similarity) =
            fit_similarity_to_tangent(&best_pairs, approx_center, image_center)
        else {
            continue;
        };
        let seed_wcs = similarity_to_wcs(&similarity, image_center, approx_center);

        if !scale_is_plausible(&seed_wcs, expected_scale_arcsec, scale_tolerance) {
            continue;
        }

        let (seed_ra, seed_dec) = seed_wcs.pixel_to_sky(image_center.0, image_center.1);
        surviving_seeds += 1;

        // Positional-prior gate: when the FITS header gives an approximate
        // pointing, reject any candidate whose seed-WCS center is far from it.
        // This eliminates dense-Milky-Way false positives that win on raw
        // inlier count alone (random alignments in a star-rich region beating
        // a correct candidate elsewhere). Default radius is generous (10°) to
        // tolerate mount sync slop. Set `position_hint_radius_deg >= 180.0`
        // in the config to disable. Skipped silently when no hint is set
        // (true blind solve) or when `disable_position_gate` is set (the
        // full-blind fallback stage — the caller has already established that
        // the FITS pointing is untrustworthy).
        if !disable_position_gate {
            if let (Some(hra), Some(hdec)) = (hints.ra, hints.dec) {
                let dist = angular_distance_deg(seed_ra, seed_dec, hra, hdec);
                // Bucket the distance for the end-of-pass histogram.
                let bucket = if dist < 2.0 { 0 }
                    else if dist < 5.0 { 1 }
                    else if dist < 10.0 { 2 }
                    else if dist < 30.0 { 3 }
                    else if dist < 90.0 { 4 }
                    else { 5 };
                hint_distance_histogram[bucket] += 1;
                if verbose_candidates {
                    let scale = seed_wcs.pixel_scale_arcsec();
                    eprintln!(
                        "plate_solve [{}]:   candidate seed RA={:.2} Dec={:.2} scale={:.2}\"/px → {:.1}° from hint",
                        filename, seed_ra, seed_dec, scale, dist
                    );
                }
                if config.position_hint_radius_deg < 180.0 && dist > config.position_hint_radius_deg {
                    continue;
                }
            }
        }

        let seed_scale = seed_wcs.pixel_scale_arcsec();
        let image_fov_deg = (image_size.0 as f64).max(image_size.1 as f64) * seed_scale / 3600.0;
        let cone_radius = image_fov_deg * 0.7;
        // A degenerate similarity fit (more common without the scale hint in
        // the blind-scale fallback) can yield a non-finite or absurd seed
        // scale → bogus cone radius. Skip the candidate rather than feed an
        // out-of-domain radius to the catalog cone search.
        if !cone_radius.is_finite() || cone_radius <= 0.0 || cone_radius > 90.0 {
            continue;
        }

        // Cache hit when the current seed's FOV fits entirely inside the
        // cached cone — `distance + fov_radius ≤ cache_radius` with
        // `fov_radius = 0.5 * fov`. That leaves a ~0.2×FOV margin which
        // captures essentially all correct-candidate clustering.
        let cached = cone_cache.as_ref().and_then(|c| {
            let dist = angular_distance_deg(seed_ra, seed_dec, c.ra_deg, c.dec_deg);
            if dist + 0.5 * image_fov_deg <= c.radius_deg {
                Some(std::sync::Arc::clone(&c.stars))
            } else {
                None
            }
        });

        let verify_stars_arc = if let Some(arc) = cached {
            arc
        } else {
            let Ok((stars, _)) =
                catalog.cone_search(seed_ra, seed_dec, cone_radius, 12.0, obs_epoch)
            else {
                continue;
            };
            let arc = std::sync::Arc::new(stars);
            cone_cache = Some(ConeCache {
                ra_deg: seed_ra,
                dec_deg: seed_dec,
                radius_deg: cone_radius,
                stars: std::sync::Arc::clone(&arc),
            });
            arc
        };
        let verify_stars: &[CatalogStar] = verify_stars_arc.as_slice();

        // Adaptive tolerance: use the hint-derived tolerance when we have a
        // scale hint; otherwise compute from the seed's derived scale.
        let tol_px = hint_tol_px.unwrap_or_else(|| adaptive_tol_px(seed_scale, base_tol_arcsec));
        // Seed-stage verification uses a wider gate than the final acceptance
        // tolerance: a similarity-only seed WCS can be off by up to ~2× the
        // tight tolerance in small but systematic ways, so counting inliers
        // at the tight value alone misses the signal that would trigger
        // `translation_refit`. The refit then iterates from 3× tight down to
        // tight and returns its own tight-tolerance inlier count.
        let seed_gate_tol_px = (tol_px * 2.0).min(20.0);

        let (seed_inliers, seed_residual_sq, _) = count_inliers(
            &seed_wcs,
            verify_stars,
            image_stars,
            image_size,
            tol_px,
        );
        let (gate_inliers, _, gate_pairs) = count_inliers(
            &seed_wcs,
            verify_stars,
            image_stars,
            image_size,
            seed_gate_tol_px,
        );
        let seed_rms = if seed_inliers > 0 {
            (seed_residual_sq / seed_inliers as f64).sqrt()
        } else {
            0.0
        };

        let refit_tangent_center = seed_wcs.pixel_to_sky(image_center.0, image_center.1);
        let (final_wcs, final_inliers, final_residual_sq) =
            if gate_inliers >= MIN_REFIT_INLIERS {
                match translation_refit(
                    &seed_wcs,
                    &gate_pairs,
                    &verify_stars,
                    image_stars,
                    image_size,
                    tol_px,
                    refit_tangent_center,
                    image_center,
                    expected_scale_arcsec,
                    scale_tolerance,
                ) {
                    Some((refit_wcs, _ri_raw, _rrs_raw)) => {
                        // Re-count at TIGHT tolerance on the refit's WCS. The
                        // refit's returned (ri, rrs) can be a loose-tolerance
                        // baseline leak when tight iterations never beat the
                        // seed — always re-measure tight here so final_inliers
                        // / final_residual_sq carry consistent semantics.
                        let (rt_inliers, rt_residual_sq, _) = count_inliers(
                            &refit_wcs,
                            verify_stars,
                            image_stars,
                            image_size,
                            tol_px,
                        );
                        let refit_rms = if rt_inliers > 0 {
                            (rt_residual_sq / rt_inliers as f64).sqrt()
                        } else {
                            0.0
                        };
                        let verdict = if rt_inliers > seed_inliers { "improved" }
                            else if rt_inliers == seed_inliers { "same" }
                            else { "worse" };
                        eprintln!(
                            "plate_solve [{}]:   refit ({}): seed {} @ {:.2}px → refit {} @ {:.2}px",
                            filename, verdict, seed_inliers, seed_rms, rt_inliers, refit_rms
                        );
                        if rt_inliers > seed_inliers {
                            (refit_wcs, rt_inliers, rt_residual_sq)
                        } else {
                            (seed_wcs, seed_inliers, seed_residual_sq)
                        }
                    }
                    None => (seed_wcs, seed_inliers, seed_residual_sq),
                }
            } else {
                (seed_wcs, seed_inliers, seed_residual_sq)
            };

        if final_inliers > outcome.best_inliers {
            outcome.best_inliers = final_inliers;
            outcome.best_expected_in_fov = verify_stars.len();
            let (fra, fdec) = final_wcs.pixel_to_sky(image_center.0, image_center.1);
            let fscale = final_wcs.pixel_scale_arcsec();
            eprintln!(
                "plate_solve [{}]:   new best: RA={:.2} Dec={:.2} scale={:.2} inliers={}/{}",
                filename, fra, fdec, fscale, final_inliers, verify_stars.len()
            );
            let rms_px = if final_inliers > 0 {
                (final_residual_sq / final_inliers as f64).sqrt()
            } else {
                0.0
            };

            let ratio = if verify_stars.is_empty() {
                0.0
            } else {
                final_inliers as f64 / verify_stars.len() as f64
            };
            outcome.best = Some(SolveResult {
                wcs: final_wcs.clone(),
                matched_stars: final_inliers,
                total_detected: image_stars.len(),
                rms_residual_px: rms_px,
                rms_residual_arcsec: rms_px * fscale,
                pixel_scale_arcsec: fscale,
                field_rotation_deg: final_wcs.field_rotation_deg(),
                solve_time_ms: 0,
                catalog_used: "tycho2".to_string(),
                algorithm_used: "blind_index".to_string(),
                derived_focallen_mm: None,
                focallen_corrected: false,
                expected_catalog_stars_in_fov: verify_stars.len(),
                inlier_ratio: ratio,
            });

            // Early exit within a single pass: if this candidate passes the
            // density-aware threshold with a comfortable margin, stop trying.
            let required = required_inliers(
                verify_stars.len(),
                image_stars.len(),
                config.min_inlier_ratio,
                config.min_matched_stars,
            );
            // Early exit: `required * 3 / 2` (≈1.5×) is a confident match
            // and gives enough headroom over the acceptance floor that the
            // next candidate is very unlikely to beat it in practice. The
            // old 2× bound kept us churning on already-good solves.
            if final_inliers >= required * 3 / 2 {
                break;
            }
        }
    }

    // End-of-pass diagnostic: histogram of how far candidate seed-WCS centers
    // landed from the FITS hint. Only printed when a hint was set AND we have
    // candidates to bucket. Helps disambiguate failure modes:
    //   - Most candidates in <2° / <5° → the candidate set is healthy; if the
    //     pass also has best 0 inliers, the bottleneck is verification
    //     (centroid quality, scale prior, or quad-permutation choice).
    //   - Most candidates in ≥30° → the hash matching is producing wrong-
    //     orientation seeds; the gate is correctly rejecting them but no
    //     real candidate is being produced. Look at hash tolerance,
    //     centroid noise, or the source detection itself.
    if hints.ra.is_some() && hints.dec.is_some() && surviving_seeds > 0 {
        eprintln!(
            "plate_solve [{}]:   seed-distance histogram (of {} candidates that produced a seed-WCS): \
             <2°={} <5°={} <10°={} <30°={} <90°={} ≥90°={}",
            filename,
            surviving_seeds,
            hint_distance_histogram[0],
            hint_distance_histogram[1],
            hint_distance_histogram[2],
            hint_distance_histogram[3],
            hint_distance_histogram[4],
            hint_distance_histogram[5],
        );
    }

    outcome
}

/// Check whether a WCS's derived pixel scale is physically plausible and
/// (if we have a header hint) agrees with the expected scale.
pub(crate) fn scale_is_plausible(
    wcs: &WcsSolution,
    expected_scale_arcsec: Option<f64>,
    scale_tolerance: f64,
) -> bool {
    let scale = wcs.pixel_scale_arcsec();
    if !(0.1..30.0).contains(&scale) {
        return false;
    }
    if let Some(expected) = expected_scale_arcsec {
        let rel_error = (scale - expected).abs() / expected;
        if rel_error > scale_tolerance {
            return false;
        }
    }
    true
}

/// Re-project each catalog star with `wcs`, find its nearest image star,
/// and count how many fall within `tolerance_px`. Also collects the
/// matched `((image_px, image_py), (catalog_ra, catalog_dec))` pairs so a
/// caller can refit the transform over the full inlier set.
pub(crate) fn count_inliers(
    wcs: &WcsSolution,
    verify_stars: &[CatalogStar],
    image_stars: &[ImageStar],
    image_size: (u32, u32),
    tolerance_px: f64,
) -> (usize, f64, Vec<((f64, f64), (f64, f64))>) {
    let mut inliers = 0usize;
    let mut total_residual_sq = 0.0f64;
    let mut pairs: Vec<((f64, f64), (f64, f64))> = Vec::new();

    for cs in verify_stars {
        let (px, py) = wcs.sky_to_pixel(cs.ra, cs.dec);
        if px < 0.0 || py < 0.0 || px >= image_size.0 as f64 || py >= image_size.1 as f64 {
            continue;
        }
        let mut best_d = f64::INFINITY;
        let mut best_is: Option<&ImageStar> = None;
        for is in image_stars {
            let d = ((is.x - px).powi(2) + (is.y - py).powi(2)).sqrt();
            if d < best_d {
                best_d = d;
                best_is = Some(is);
            }
        }
        if best_d < tolerance_px {
            inliers += 1;
            total_residual_sq += best_d * best_d;
            if let Some(is) = best_is {
                pairs.push(((is.x, is.y), (cs.ra as f64, cs.dec as f64)));
            }
        }
    }

    (inliers, total_residual_sq, pairs)
}

/// Closed-form 4-parameter similarity fit (translation + rotation +
/// uniform scale) from pixel ↔ sky correspondences, using complex-
/// valued LSQ. Unlike the 6-param affine in [`fit_similarity_to_tangent`],
/// this enforces the constraint that the linear part is a scaled
/// rotation (no shear), which is what a real pinhole camera actually
/// produces. The reduced parameter count is dramatically more stable
/// when inliers are clustered or few — the under-determination that
/// makes 6-param drift on sparse data doesn't apply.
///
/// Math: represent pixel offsets as complex z = (px - cx) + i(py - cy),
/// tangent-plane positions as complex w = xi + i*eta. Fit w = c*z + t
/// where c = s*e^(iθ) encodes uniform scale + rotation, t encodes
/// translation. Closed form:
///     c = Σ conj(z_i - z̄) (w_i - w̄) / Σ |z_i - z̄|²
///     t = w̄ - c * z̄
/// Then map (c_re, c_im) into the 6-param `Similarity` struct via the
/// similarity constraint a = d = c_re, b = -c_im, c = c_im.
pub(crate) fn fit_similarity_4param(
    pairs: &[((f64, f64), (f64, f64))],
    tangent_center: (f64, f64),
    image_center: (f64, f64),
) -> Option<Similarity> {
    let n = pairs.len();
    if n < 2 {
        return None;
    }

    let mut u = Vec::with_capacity(n);
    let mut v = Vec::with_capacity(n);
    let mut xi = Vec::with_capacity(n);
    let mut eta = Vec::with_capacity(n);
    for &((px, py), (ra, dec)) in pairs {
        let (xi_rad, eta_rad) =
            GnomonicProjection::sky_to_tangent(ra, dec, tangent_center.0, tangent_center.1);
        u.push(px - image_center.0);
        v.push(py - image_center.1);
        xi.push(xi_rad);
        eta.push(eta_rad);
    }

    let nf = n as f64;
    let u_mean = u.iter().sum::<f64>() / nf;
    let v_mean = v.iter().sum::<f64>() / nf;
    let xi_mean = xi.iter().sum::<f64>() / nf;
    let eta_mean = eta.iter().sum::<f64>() / nf;

    let mut num_re = 0.0f64;
    let mut num_im = 0.0f64;
    let mut denom = 0.0f64;
    for i in 0..n {
        let dz_re = u[i] - u_mean;
        let dz_im = v[i] - v_mean;
        let dw_re = xi[i] - xi_mean;
        let dw_im = eta[i] - eta_mean;
        // conj(dz) * dw  =  (dz_re - i*dz_im)(dw_re + i*dw_im)
        //               =  (dz_re*dw_re + dz_im*dw_im) + i*(dz_re*dw_im - dz_im*dw_re)
        num_re += dz_re * dw_re + dz_im * dw_im;
        num_im += dz_re * dw_im - dz_im * dw_re;
        denom += dz_re * dz_re + dz_im * dz_im;
    }
    if denom.abs() < 1e-30 {
        return None;
    }

    let c_re = num_re / denom;
    let c_im = num_im / denom;

    // t = w̄ - c*z̄, where c*z̄ = (c_re*u_mean - c_im*v_mean) + i(c_re*v_mean + c_im*u_mean)
    let cz_re = c_re * u_mean - c_im * v_mean;
    let cz_im = c_re * v_mean + c_im * u_mean;
    let tx = xi_mean - cz_re;
    let ty = eta_mean - cz_im;

    // Map complex c to the 6-param Similarity struct with the similarity
    // constraint enforced: xi = c_re*u - c_im*v + tx, eta = c_im*u + c_re*v + ty.
    Some(Similarity {
        a: c_re,
        b: -c_im,
        c: c_im,
        d: c_re,
        tx,
        ty,
    })
}

/// Iterative 4-param similarity refit: starting from the seed WCS's
/// tight inliers, fit a constrained similarity transform (translation +
/// rotation + uniform scale) via closed-form complex LSQ, apply it to
/// the WCS, re-collect the tight inliers under the new WCS, and repeat
/// until the inlier set stops growing. Handles seed biases in ALL
/// three linear components (translation, rotation, scale) without the
/// drift that plagues 6-param affine LSQ on sparse clustered points.
///
/// Keeps the best-seen WCS (by tight inlier count) across iterations so
/// a late bad iteration can't regress a good intermediate result.
#[allow(clippy::too_many_arguments)]
pub(crate) fn translation_refit(
    seed_wcs: &WcsSolution,
    seed_pairs: &[((f64, f64), (f64, f64))],
    verify_stars: &[CatalogStar],
    image_stars: &[ImageStar],
    image_size: (u32, u32),
    tight_tol_px: f64,
    tangent_center: (f64, f64),
    image_center: (f64, f64),
    expected_scale_arcsec: Option<f64>,
    scale_tolerance: f64,
) -> Option<(WcsSolution, usize, f64)> {
    const MAX_ITERS: usize = 4;

    if seed_pairs.len() < 2 {
        return None;
    }

    let seed_count = seed_pairs.len();
    let mut best_wcs: WcsSolution = seed_wcs.clone();
    let mut best_count = seed_count;
    // Initialise best residual from the seed pairs for a comparable
    // starting point.
    let mut best_residual_sq = {
        let mut rs = 0.0f64;
        for &((ipx, ipy), (ra, dec)) in seed_pairs {
            let (ppx, ppy) = seed_wcs.sky_to_pixel(ra, dec);
            rs += (ipx - ppx).powi(2) + (ipy - ppy).powi(2);
        }
        rs
    };

    // Start from pairs at a LOOSE tolerance so the LSQ has coverage
    // across the full field — a small rotation error in the seed is
    // invisible near the image center (where the clustered tight
    // inliers live) but obvious far from centre. Using only tight
    // pairs leaves the rotation under-constrained; using the loose
    // pool exposes it. The 4-param similarity constraint keeps the
    // fit stable even with a few contaminated matches.
    let (_, _, loose_seed_pairs) = count_inliers(
        seed_wcs,
        verify_stars,
        image_stars,
        image_size,
        tight_tol_px * 3.0,
    );
    let start_pairs = if loose_seed_pairs.len() >= seed_pairs.len() {
        loose_seed_pairs
    } else {
        seed_pairs.to_vec()
    };

    let mut current_pairs: Vec<((f64, f64), (f64, f64))> = start_pairs;

    for _iter in 0..MAX_ITERS {
        if current_pairs.len() < 2 {
            break;
        }
        let sim = fit_similarity_4param(&current_pairs, tangent_center, image_center)?;
        let new_wcs = similarity_to_wcs(&sim, image_center, tangent_center);
        if !scale_is_plausible(&new_wcs, expected_scale_arcsec, scale_tolerance) {
            break;
        }
        // Evaluate at the TIGHT tolerance (which is what acceptance uses)
        // so a successful refit shows up as real inlier growth.
        let (new_count, new_rs, _) = count_inliers(
            &new_wcs,
            verify_stars,
            image_stars,
            image_size,
            tight_tol_px,
        );

        if new_count > best_count || (new_count == best_count && new_rs < best_residual_sq) {
            best_wcs = new_wcs.clone();
            best_count = new_count;
            best_residual_sq = new_rs;
        }

        // Next iteration works from a slightly tighter pool so the fit
        // progressively rejects outliers. After a few iterations we
        // converge to the true transform.
        let next_tol = tight_tol_px * (3.0 - (_iter + 1) as f64 * 0.5).max(1.0);
        let (_, _, next_pairs) = count_inliers(
            &new_wcs,
            verify_stars,
            image_stars,
            image_size,
            next_tol,
        );
        if next_pairs.len() < 2 || next_pairs.len() == current_pairs.len() {
            break;
        }
        current_pairs = next_pairs;
    }

    Some((best_wcs, best_count, best_residual_sq))
}

fn catalog_centroid(q: &QuadLookup) -> (f64, f64) {
    let ra = (q.stars_ra[0] as f64
        + q.stars_ra[1] as f64
        + q.stars_ra[2] as f64
        + q.stars_ra[3] as f64)
        / 4.0;
    let dec = (q.stars_dec[0] as f64
        + q.stars_dec[1] as f64
        + q.stars_dec[2] as f64
        + q.stars_dec[3] as f64)
        / 4.0;
    (ra, dec)
}

const PERMUTATIONS_4: [[usize; 4]; 24] = [
    [0, 1, 2, 3], [0, 1, 3, 2], [0, 2, 1, 3], [0, 2, 3, 1], [0, 3, 1, 2], [0, 3, 2, 1],
    [1, 0, 2, 3], [1, 0, 3, 2], [1, 2, 0, 3], [1, 2, 3, 0], [1, 3, 0, 2], [1, 3, 2, 0],
    [2, 0, 1, 3], [2, 0, 3, 1], [2, 1, 0, 3], [2, 1, 3, 0], [2, 3, 0, 1], [2, 3, 1, 0],
    [3, 0, 1, 2], [3, 0, 2, 1], [3, 1, 0, 2], [3, 1, 2, 0], [3, 2, 0, 1], [3, 2, 1, 0],
];

/// Try all 24 permutations of catalog-to-image pairing for a candidate.
/// Returns the chosen pairing along with its 4-point fit residual, or None
/// if no permutation produces a valid fit.
///
/// **Tiered tiebreak.** The fitting residual alone is unreliable on near-
/// symmetric (rhombus / near-square) quads — multiple permutations give
/// near-zero residuals just from geometric symmetry, and the lowest-by-
/// residual one is essentially random. To break those ties we add a
/// rank-consistency check: distance-from-centroid is a rotation/reflection
/// invariant of the quad, so the image-side ranking by distance from image
/// centroid must agree with the catalog-side ranking by distance from
/// catalog centroid in the correct permutation.
///
/// Algorithm:
///   1. Score every permutation by 4-point squared residual.
///   2. Filter to the perms within `2 * min_residual + tiny_floor` — this
///      keeps clear winners (single best by residual) intact while gathering
///      all near-ties when the quad is symmetric.
///   3. Among the filtered set, pick the highest rank-consistency (agreement
///      count out of 6 ordered pairs). Break ties by lowest residual.
fn best_permutation_fit(
    image_quad: &Quad,
    catalog: &QuadLookup,
    image_positions: &[(f64, f64)],
    approx_center: (f64, f64),
    image_center: (f64, f64),
) -> Option<(Vec<((f64, f64), (f64, f64))>, f64)> {
    let img_stars: [(f64, f64); 4] = [
        image_positions[image_quad.star_indices[0]],
        image_positions[image_quad.star_indices[1]],
        image_positions[image_quad.star_indices[2]],
        image_positions[image_quad.star_indices[3]],
    ];
    let cat_stars: [(f64, f64); 4] = [
        (catalog.stars_ra[0] as f64, catalog.stars_dec[0] as f64),
        (catalog.stars_ra[1] as f64, catalog.stars_dec[1] as f64),
        (catalog.stars_ra[2] as f64, catalog.stars_dec[2] as f64),
        (catalog.stars_ra[3] as f64, catalog.stars_dec[3] as f64),
    ];

    // Guard the gnomonic projection below. `GnomonicProjection::sky_to_tangent`
    // asserts `cos_c > 0` and panics ("opposite hemisphere from tangent
    // point"). A degenerate candidate quad (hash collision; or `approx_center`
    // landing far away when the catalog quad straddles the RA=0/360 wrap) can
    // put a star ≥90° from the centre. Such candidates are garbage anyway —
    // skip them (the caller already does `else { continue }`) instead of
    // crashing. Same hemisphere check the projection uses, with a small
    // margin so near-limb points (xi/eta → ±∞) are skipped too.
    if !approx_center.0.is_finite() || !approx_center.1.is_finite() {
        return None;
    }
    let (ra0, dec0) = (approx_center.0.to_radians(), approx_center.1.to_radians());
    let (sin_d0, cos_d0) = (dec0.sin(), dec0.cos());
    for &(ra, dec) in &cat_stars {
        if !ra.is_finite() || !dec.is_finite() {
            return None;
        }
        let (rr, dr) = (ra.to_radians(), dec.to_radians());
        let cos_c = sin_d0 * dr.sin() + cos_d0 * dr.cos() * (rr - ra0).cos();
        if !(cos_c > 1e-6) {
            return None;
        }
    }

    // Pre-compute per-star distances from each side's centroid. These are
    // rotation/reflection invariants — in the correct permutation, the
    // image-side ordering by `img_dist` must match the catalog-side ordering
    // by `cat_dist`. Image-side units are pixels^2, catalog-side units are
    // tangent-plane radians^2; we only compare orderings, never absolute
    // values, so the unit mismatch doesn't matter.
    let img_centroid_x = (img_stars[0].0 + img_stars[1].0 + img_stars[2].0 + img_stars[3].0) / 4.0;
    let img_centroid_y = (img_stars[0].1 + img_stars[1].1 + img_stars[2].1 + img_stars[3].1) / 4.0;
    let img_dist: [f64; 4] = [0, 1, 2, 3].map(|k| {
        let dx = img_stars[k].0 - img_centroid_x;
        let dy = img_stars[k].1 - img_centroid_y;
        dx * dx + dy * dy
    });
    let cat_xy: [(f64, f64); 4] = [0, 1, 2, 3].map(|k| {
        GnomonicProjection::sky_to_tangent(
            cat_stars[k].0,
            cat_stars[k].1,
            approx_center.0,
            approx_center.1,
        )
    });
    let cat_centroid_x = (cat_xy[0].0 + cat_xy[1].0 + cat_xy[2].0 + cat_xy[3].0) / 4.0;
    let cat_centroid_y = (cat_xy[0].1 + cat_xy[1].1 + cat_xy[2].1 + cat_xy[3].1) / 4.0;
    let cat_dist: [f64; 4] = [0, 1, 2, 3].map(|k| {
        let dx = cat_xy[k].0 - cat_centroid_x;
        let dy = cat_xy[k].1 - cat_centroid_y;
        dx * dx + dy * dy
    });

    // Pass 1: compute residual for every viable permutation.
    let mut scored: Vec<(usize, Vec<((f64, f64), (f64, f64))>, f64)> = Vec::with_capacity(24);
    for (idx, perm) in PERMUTATIONS_4.iter().enumerate() {
        let pairs: Vec<((f64, f64), (f64, f64))> = (0..4)
            .map(|k| (img_stars[k], cat_stars[perm[k]]))
            .collect();

        let Some(similarity) = fit_similarity_to_tangent(&pairs, approx_center, image_center)
        else {
            continue;
        };

        let mut residual = 0.0;
        for ((px, py), (ra, dec)) in &pairs {
            let (xi_rad, eta_rad) =
                GnomonicProjection::sky_to_tangent(*ra, *dec, approx_center.0, approx_center.1);
            let u = px - image_center.0;
            let v = py - image_center.1;
            let pred_xi = similarity.a * u + similarity.b * v + similarity.tx;
            let pred_eta = similarity.c * u + similarity.d * v + similarity.ty;
            residual += (pred_xi - xi_rad).powi(2) + (pred_eta - eta_rad).powi(2);
        }

        scored.push((idx, pairs, residual));
    }
    if scored.is_empty() {
        return None;
    }

    // Pass 2: tiered tiebreak. Pick min residual, build a near-tie cohort.
    let min_residual = scored.iter().map(|(_, _, r)| *r).fold(f64::INFINITY, f64::min);
    // The cohort threshold is intentionally generous: 2× min residual plus a
    // tiny absolute floor so that machine-zero ties (near-perfect symmetric
    // fits) all collapse into the cohort. Without the absolute floor, two
    // residuals of 1e-30 and 3e-30 would not be considered tied even though
    // they are functionally identical noise.
    let cohort_threshold = (min_residual * 2.0).max(min_residual + 1e-20);

    // Among the cohort, pick the perm that maximizes rank-consistency between
    // image-side and catalog-side distance-from-centroid orderings. This
    // breaks the symmetry of near-rhombus quads — wrong-orientation perms
    // have low rank-consistency (random ~3/6) while the correct perm has
    // high (5/6 or 6/6).
    let mut best: Option<(Vec<((f64, f64), (f64, f64))>, f64, i32)> = None;
    for (idx, pairs, residual) in scored {
        if residual > cohort_threshold {
            continue;
        }
        let perm = &PERMUTATIONS_4[idx];
        let agreement = rank_agreement(&img_dist, &cat_dist, perm);

        let take = match &best {
            None => true,
            Some((_, best_res, best_agree)) => {
                agreement > *best_agree
                    || (agreement == *best_agree && residual < *best_res)
            }
        };
        if take {
            best = Some((pairs, residual, agreement));
        }
    }
    best.map(|(pairs, residual, _)| (pairs, residual))
}

/// Count, out of the 6 ordered pairs of quad-star indices, how many agree
/// between the image-side and catalog-side distance-from-centroid orderings
/// under permutation `perm`. Returns a value in [0, 6].
///
/// The correct catalog→image pairing produces 6/6 agreement on a non-
/// degenerate quad. Wrong rotations/reflections of a near-symmetric quad
/// typically score 2-4. Random permutations average 3.
fn rank_agreement(img_dist: &[f64; 4], cat_dist: &[f64; 4], perm: &[usize; 4]) -> i32 {
    let mut agreement = 0;
    for k1 in 0..4 {
        for k2 in (k1 + 1)..4 {
            let img_order = img_dist[k1] < img_dist[k2];
            let cat_order = cat_dist[perm[k1]] < cat_dist[perm[k2]];
            if img_order == cat_order {
                agreement += 1;
            }
        }
    }
    agreement
}

/// A similarity transform: pixel → tangent plane (in radians).
#[derive(Clone, Debug)]
pub(crate) struct Similarity {
    // tangent_xi  = a * (px - cx) + b * (py - cy) + tx
    // tangent_eta = c * (px - cx) + d * (py - cy) + ty
    pub(crate) a: f64,
    pub(crate) b: f64,
    pub(crate) c: f64,
    pub(crate) d: f64,
    pub(crate) tx: f64,
    pub(crate) ty: f64,
}

/// Fit a 6-parameter affine (call it "similarity" in spirit) from quad center
/// correspondences.
///
/// Input: pairs of (image_pixel, catalog_sky_deg). The catalog side is first
/// gnomonically projected to the tangent plane centered at `tangent_center`,
/// giving (xi, eta) in radians. Then we solve: xi = a*(x - cx) + b*(y - cy) + tx
/// and eta = c*(x - cx) + d*(y - cy) + ty.
fn fit_similarity_to_tangent(
    pairs: &[((f64, f64), (f64, f64))],
    tangent_center: (f64, f64),
    image_center: (f64, f64),
) -> Option<Similarity> {
    let n = pairs.len();
    if n < 3 {
        return None;
    }

    // Build the tangent-plane targets
    let mut u = Vec::with_capacity(n);
    let mut v = Vec::with_capacity(n);
    let mut xi = Vec::with_capacity(n);
    let mut eta = Vec::with_capacity(n);
    for ((px, py), (ra, dec)) in pairs {
        let (xi_rad, eta_rad) =
            GnomonicProjection::sky_to_tangent(*ra, *dec, tangent_center.0, tangent_center.1);
        u.push(px - image_center.0);
        v.push(py - image_center.1);
        xi.push(xi_rad);
        eta.push(eta_rad);
    }

    // Solve two 3-param least squares: xi = a*u + b*v + tx, same for eta
    let (a, b, tx) = solve_3param(&u, &v, &xi)?;
    let (c, d, ty) = solve_3param(&u, &v, &eta)?;

    Some(Similarity { a, b, c, d, tx, ty })
}

/// Solve for [a, b, c] in `target_i = a*u_i + b*v_i + c` via normal equations.
fn solve_3param(u: &[f64], v: &[f64], target: &[f64]) -> Option<(f64, f64, f64)> {
    let n = u.len() as f64;
    if n < 3.0 {
        return None;
    }
    let sum_u: f64 = u.iter().sum();
    let sum_v: f64 = v.iter().sum();
    let sum_t: f64 = target.iter().sum();
    let sum_uu: f64 = u.iter().map(|x| x * x).sum();
    let sum_vv: f64 = v.iter().map(|x| x * x).sum();
    let sum_uv: f64 = u.iter().zip(v.iter()).map(|(a, b)| a * b).sum();
    let sum_ut: f64 = u.iter().zip(target.iter()).map(|(a, b)| a * b).sum();
    let sum_vt: f64 = v.iter().zip(target.iter()).map(|(a, b)| a * b).sum();

    // 3x3 symmetric system:
    // [ sum_uu  sum_uv  sum_u ] [a]   [sum_ut]
    // [ sum_uv  sum_vv  sum_v ] [b] = [sum_vt]
    // [ sum_u   sum_v   n     ] [c]   [sum_t ]
    let m = [
        [sum_uu, sum_uv, sum_u],
        [sum_uv, sum_vv, sum_v],
        [sum_u, sum_v, n],
    ];
    let rhs = [sum_ut, sum_vt, sum_t];

    // Cramer's rule
    let det = det3(&m);
    if det.abs() < 1e-20 {
        return None;
    }
    let a = det3(&replace_col(&m, 0, &rhs)) / det;
    let b = det3(&replace_col(&m, 1, &rhs)) / det;
    let c = det3(&replace_col(&m, 2, &rhs)) / det;
    Some((a, b, c))
}

fn det3(m: &[[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn replace_col(m: &[[f64; 3]; 3], col: usize, v: &[f64; 3]) -> [[f64; 3]; 3] {
    let mut out = *m;
    for i in 0..3 {
        out[i][col] = v[i];
    }
    out
}

/// Convert a fitted similarity (pixel → tangent-plane-radians) to a WcsSolution.
pub(crate) fn similarity_to_wcs(
    sim: &Similarity,
    image_center: (f64, f64),
    tangent_center: (f64, f64),
) -> WcsSolution {
    // The similarity maps (px - cx, py - cy) → (xi_rad, eta_rad). To build a
    // CD matrix we need degrees/pixel (CD convention). Multiply radian
    // coefficients by 180/π.
    let rad2deg = 180.0 / std::f64::consts::PI;
    let cd = [
        [sim.a * rad2deg, sim.b * rad2deg],
        [sim.c * rad2deg, sim.d * rad2deg],
    ];

    // Compute the image center's sky position using the similarity.
    // At image center: u = v = 0, so xi = tx, eta = ty.
    let (crval_ra, crval_dec) = GnomonicProjection::tangent_to_sky(
        sim.tx,
        sim.ty,
        tangent_center.0,
        tangent_center.1,
    );

    WcsSolution {
        crpix: image_center,
        crval: (crval_ra, crval_dec),
        cd,
        sip_forward: None,
        sip_reverse: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use a large detected_count in these tests so the catalog density is the
    // active ceiling (mirrors well-detected non-dense frames).
    const DET: usize = 100_000;

    #[test]
    fn gate_audit_disabled_is_zero_behaviour_change() {
        assert!(!crate::plate_solve::gate_audit::enabled());
    }

    #[test]
    fn required_inliers_sparse_fields_use_absolute_floor() {
        // ≤30 catalog stars in FOV → fixed 6 (or floor if higher).
        assert_eq!(required_inliers(0, DET, 0.10, 6), 6);
        assert_eq!(required_inliers(10, DET, 0.10, 6), 6);
        assert_eq!(required_inliers(30, DET, 0.10, 6), 6);
        // Floor overrides when larger than the sparse-field default.
        assert_eq!(required_inliers(10, DET, 0.10, 8), 8);
    }

    #[test]
    fn required_inliers_mid_density_uses_20_percent() {
        // 31-100 catalog stars → round(N * 0.20).
        assert_eq!(required_inliers(50, DET, 0.10, 6), 10); // round(50 * 0.20)
        assert_eq!(required_inliers(100, DET, 0.10, 6), 20); // round(100 * 0.20)
        // Floor still wins if higher.
        assert_eq!(required_inliers(50, DET, 0.10, 15), 15);
    }

    #[test]
    fn required_inliers_dense_uses_ratio_with_floor_of_20() {
        // >100 catalog stars → max(round(N * min_ratio), 20).
        assert_eq!(required_inliers(150, DET, 0.10, 6), 20); // round(15) bumped to 20
        assert_eq!(required_inliers(500, DET, 0.10, 6), 50); // round(50)
        assert_eq!(required_inliers(1000, DET, 0.10, 6), 100);
        // Tighter ratio in a dense field still respects the 20-inlier floor.
        assert_eq!(required_inliers(150, DET, 0.05, 6), 20);
    }

    #[test]
    fn required_inliers_capped_by_detected_count() {
        // Milky-Way style: 3493 catalog stars in FOV but only 600 detected.
        // Effective density is 600 → required = max(20, round(600 * 0.10)) = 60.
        // Without the detected-count cap this would demand ≈349.
        assert_eq!(required_inliers(3493, 600, 0.10, 6), 60);
        // When detected > catalog (unusual but possible with deep detection on
        // sparse fields) the catalog count is the limiter.
        assert_eq!(required_inliers(80, 500, 0.10, 6), 16); // mid-density: round(80*0.20)
        // Detected < 30 forces the sparse-floor branch even if catalog is large.
        assert_eq!(required_inliers(1000, 25, 0.10, 6), 6);
    }

    fn mk_result(
        matched: usize,
        expected: usize,
        ratio: f64,
        scale: f64,
        rms: f64,
    ) -> SolveResult {
        SolveResult {
            wcs: WcsSolution {
                crpix: (0.0, 0.0),
                crval: (123.45, 67.89),
                cd: [[1e-4, 0.0], [0.0, 1e-4]],
                sip_forward: None,
                sip_reverse: None,
            },
            matched_stars: matched,
            total_detected: 600,
            rms_residual_px: rms,
            rms_residual_arcsec: rms * scale,
            pixel_scale_arcsec: scale,
            field_rotation_deg: 0.0,
            solve_time_ms: 0,
            catalog_used: "tycho2".into(),
            algorithm_used: "blind_index".into(),
            derived_focallen_mm: None,
            focallen_corrected: false,
            expected_catalog_stars_in_fov: expected,
            inlier_ratio: ratio,
        }
    }

    #[test]
    fn store_result_refuses_low_confidence_writeback() {
        // Defense-in-depth: a low-confidence (false-positive) solve must
        // never write WCS/focal length back or create a plate_solves row,
        // regardless of how acceptance classified it — this is what
        // corrupted the catalog during the gate-less calibration run.
        use crate::db::schema::init_db;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let cfg = PlateSolveConfig::default();
        for (fid, name) in [(1, "a.fits"), (2, "b.fits")] {
            conn.execute(
                "INSERT INTO files (id,path,filename,size,modified_at,format,created_at)
                 VALUES (?1,?2,?3,0,'2025-01-01','FITS','2025-01-01')",
                rusqlite::params![fid, format!("/x/{name}"), name],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id,file_id) VALUES (?1,?2)",
                [fid, fid],
            )
            .unwrap();
        }

        // High-confidence (real-solve shape) — must persist.
        store_result(&conn, 1, &mk_result(120, 800, 0.15, 1.5, 1.0), None, &cfg)
            .unwrap();
        let ps1: i64 = conn
            .query_row("SELECT COUNT(*) FROM plate_solves WHERE frame_id=1", [], |r| r.get(0))
            .unwrap();
        let (ra1, ovr1): (Option<f64>, i64) = conn
            .query_row("SELECT ra,override FROM frames WHERE id=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(ps1, 1, "high-confidence solve must persist");
        assert!(ra1.is_some() && ovr1 == 1, "must write WCS + override");

        // Low-confidence false-positive shape — must be refused.
        store_result(&conn, 2, &mk_result(90, 150_000, 0.0006, 22.0, 2.8), None, &cfg)
            .unwrap();
        let ps2: i64 = conn
            .query_row("SELECT COUNT(*) FROM plate_solves WHERE frame_id=2", [], |r| r.get(0))
            .unwrap();
        let (ra2, ovr2): (Option<f64>, i64) = conn
            .query_row("SELECT ra,override FROM frames WHERE id=2", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(ps2, 0, "low-confidence solve must NOT create a plate_solves row");
        assert!(
            ra2.is_none() && ovr2 == 0,
            "low-confidence solve must NOT mutate the frame"
        );
    }

    #[test]
    fn blind_gate_table() {
        let cfg = PlateSolveConfig {
            blind_rms_max_px_mult: 2.5,
            blind_min_inlier_ratio: 0.04,
            blind_inlier_floor: 12,
            blind_scale_sanity_min: 0.05,
            blind_scale_sanity_max: 60.0,
            blind_scale_header_tol: 8.0,
            blind_gate_enabled: true,
            ..PlateSolveConfig::default()
        };
        let base = BlindGateMetrics {
            inliers: 40,
            expected_in_fov: 800,
            rms_px: 1.2,
            adaptive_tol_px: 6.0,
            inlier_ratio: 0.30,
            recovered_scale_arcsec: 1.8,
            header_scale_arcsec: Some(1.9),
        };
        // Hinted stage is never gated.
        assert!(blind_gate_ok(GateStage::Hinted, &base, &cfg));
        // Good full-blind passes.
        assert!(blind_gate_ok(GateStage::FullBlind, &base, &cfg));
        // Loose RMS on blind path rejected.
        assert!(!blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { rms_px: 20.0, ..base.clone() }, &cfg));
        // Low ratio on a DENSE field rejected (the calibrated primary gate).
        assert!(!blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { inlier_ratio: 0.001, ..base.clone() }, &cfg));
        // Sparse field (expected<=100) NOT punished by the ratio rule — stage is irrelevant here.
        assert!(blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { expected_in_fov: 40, inlier_ratio: 0.001,
                inliers: 14, ..base.clone() }, &cfg));
        // Too few inliers rejected.
        assert!(!blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { inliers: 8, ..base.clone() }, &cfg));
        // Absurd recovered scale rejected.
        assert!(!blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { recovered_scale_arcsec: 0.001, ..base.clone() }, &cfg));
        // Recovered scale wildly off header scale rejected (ratio ~10 > tol 8).
        assert!(!blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { recovered_scale_arcsec: 20.0,
                header_scale_arcsec: Some(1.9), ..base.clone() }, &cfg));
        // Non-finite RMS is never a real solve (guards the is_finite branch;
        // degenerate blind solves can produce NaN/inf).
        assert!(!blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { rms_px: f64::NAN, ..base.clone() }, &cfg));
        assert!(!blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { rms_px: f64::INFINITY, ..base.clone() }, &cfg));
        // No header scale: the header-tol guard is skipped entirely, so a
        // recovered scale that WOULD fail the ratio-to-header check still
        // passes when header_scale_arcsec is None.
        assert!(blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { header_scale_arcsec: None, recovered_scale_arcsec: 20.0,
                ..base.clone() }, &cfg));
        // Scale-sanity MAX bound (the existing table only covered the min side).
        assert!(!blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { recovered_scale_arcsec: 100.0, ..base.clone() }, &cfg));
        // Gate can be disabled by config.
        let off = PlateSolveConfig { blind_gate_enabled: false, ..cfg.clone() };
        assert!(blind_gate_ok(GateStage::FullBlind,
            &BlindGateMetrics { rms_px: 99.0, ..base }, &off));
    }

    #[test]
    fn adaptive_tol_clamps_to_4_to_20_px() {
        // Narrow FOV (0.5"/px): 8 / 0.5 = 16, within band.
        assert!((adaptive_tol_px(0.5, 8.0) - 16.0).abs() < 1e-9);
        // Wide FOV (5"/px): 8 / 5 = 1.6, clamps UP to 4.
        assert!((adaptive_tol_px(5.0, 8.0) - 4.0).abs() < 1e-9);
        // Tiny scale: huge tolerance clamps to 20.
        assert!((adaptive_tol_px(0.1, 8.0) - 20.0).abs() < 1e-9);
        // Zero pixel scale (defensive) → safe 10 px default.
        assert!((adaptive_tol_px(0.0, 8.0) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn angular_distance_identity_and_wraparound() {
        // Same point → 0°.
        assert!(angular_distance_deg(10.0, 20.0, 10.0, 20.0).abs() < 1e-9);
        // Across the RA=0 boundary.
        let d = angular_distance_deg(359.5, 0.0, 0.5, 0.0);
        assert!((d - 1.0).abs() < 1e-6, "expected ~1°, got {d}");
        // 90° apart at equator.
        let d = angular_distance_deg(0.0, 0.0, 90.0, 0.0);
        assert!((d - 90.0).abs() < 1e-6, "expected 90°, got {d}");
    }

    #[test]
    fn rank_agreement_identity_perm_on_matching_orderings_is_six() {
        // img and cat both have distances 1, 2, 3, 4 in the same order — the
        // identity permutation should give all 6 pair-orderings agreeing.
        let img = [1.0, 2.0, 3.0, 4.0];
        let cat = [10.0, 20.0, 30.0, 40.0]; // monotonic, matching img
        assert_eq!(rank_agreement(&img, &cat, &[0, 1, 2, 3]), 6);
    }

    #[test]
    fn rank_agreement_reversed_perm_on_matching_orderings_is_zero() {
        // Reversing the catalog mapping should disagree on every pair-ordering.
        let img = [1.0, 2.0, 3.0, 4.0];
        let cat = [10.0, 20.0, 30.0, 40.0];
        assert_eq!(rank_agreement(&img, &cat, &[3, 2, 1, 0]), 0);
    }

    #[test]
    fn rank_agreement_random_perm_around_three() {
        // A perm that swaps two adjacent elements should disagree on 1 pair
        // ordering and agree on 5. Concretely, perm [1, 0, 2, 3] swaps img
        // ranks 0 and 1 — only the (0, 1) pair-ordering flips.
        let img = [1.0, 2.0, 3.0, 4.0];
        let cat = [10.0, 20.0, 30.0, 40.0];
        assert_eq!(rank_agreement(&img, &cat, &[1, 0, 2, 3]), 5);
    }

    #[test]
    fn best_permutation_fit_picks_correct_perm_on_symmetric_quad() {
        // A nearly-square 4-star quad in the image with a known mapping to a
        // catalog quad. The image has a specific brightness/distance pattern
        // (one star displaced slightly inward toward centroid) and the catalog
        // mirrors that pattern. The correct permutation is the identity.
        // Wrong permutations will produce small residuals from the symmetry,
        // so the rank-agreement tiebreak should pick the identity.
        use astroimage::platesolving::Quad;
        // Image quad: 4 stars near corners of a 100-px square, with star 0
        // pushed slightly inward (closer to centroid than the others).
        let positions: Vec<(f64, f64)> = vec![
            (3010.0, 2010.0), // index 0: slightly inside top-left corner
            (3100.0, 2000.0), // index 1: top-right corner
            (3000.0, 2100.0), // index 2: bottom-left corner
            (3100.0, 2100.0), // index 3: bottom-right corner
        ];
        let image_quad = Quad {
            star_indices: [0, 1, 2, 3],
            ratios: [0.0; 5], // unused by best_permutation_fit
            center: (3050.0, 2050.0),
            longest_dist: 100.0 * 2f64.sqrt(),
        };
        // Catalog quad: same configuration projected to RA/Dec near (10°, 20°).
        // 100 px ≈ 100 * 2"/3600 ≈ 0.0556°. We mimic the image by placing
        // catalog star 0 slightly inside the corresponding corner.
        let cat = QuadLookup {
            hash_key: [0; 5],
            longest_dist_deg: 0.0556 * 2f64.sqrt() as f32,
            stars_ra: [10.0028, 10.0556, 10.0000, 10.0556], // mirrors x-displacement
            stars_dec: [20.0028, 20.0000, 20.0556, 20.0556],
        };
        let approx_center = catalog_centroid(&cat);
        let image_center = (3050.0, 2050.0);

        let result = best_permutation_fit(
            &image_quad,
            &cat,
            &positions,
            approx_center,
            image_center,
        );
        let (pairs, _residual) = result.expect("must produce a fit");
        // Identity perm: pairs[k].1 should be (cat_ra[k], cat_dec[k]).
        for k in 0..4 {
            let (cat_ra_paired, cat_dec_paired) = pairs[k].1;
            assert!(
                (cat_ra_paired - cat.stars_ra[k] as f64).abs() < 1e-9
                    && (cat_dec_paired - cat.stars_dec[k] as f64).abs() < 1e-9,
                "expected identity perm, but pair {k} catalog side is ({cat_ra_paired}, {cat_dec_paired}) \
                 not ({}, {})",
                cat.stars_ra[k], cat.stars_dec[k],
            );
        }
    }

    #[test]
    fn best_permutation_fit_skips_opposite_hemisphere_quad() {
        // Regression: without the scale/position pre-filters (blind-scale
        // fallback), a degenerate candidate catalog "quad" whose stars
        // straddle the RA=0/360 wrap makes the naive `catalog_centroid`
        // land ~180° away. Projecting those stars about that centre hits
        // `GnomonicProjection::sky_to_tangent`'s `assert!(cos_c > 0)` and
        // used to panic the whole plate-solve batch. It must now skip the
        // candidate (return None) without panicking.
        use astroimage::platesolving::Quad;
        let positions: Vec<(f64, f64)> =
            vec![(10.0, 10.0), (90.0, 10.0), (10.0, 90.0), (90.0, 90.0)];
        let image_quad = Quad {
            star_indices: [0, 1, 2, 3],
            ratios: [0.0; 5],
            center: (50.0, 50.0),
            longest_dist: 80.0 * 2f64.sqrt(),
        };
        // Stars near RA 0 and RA 360 → naive mean RA = 180° (opposite side).
        let cat = QuadLookup {
            hash_key: [0; 5],
            longest_dist_deg: 0.2,
            stars_ra: [0.1, 0.2, 359.8, 359.9],
            stars_dec: [0.0, 0.0, 0.0, 0.0],
        };
        let approx_center = catalog_centroid(&cat);
        let result =
            best_permutation_fit(&image_quad, &cat, &positions, approx_center, (50.0, 50.0));
        assert!(
            result.is_none(),
            "opposite-hemisphere candidate must be skipped, not panic"
        );
    }
}

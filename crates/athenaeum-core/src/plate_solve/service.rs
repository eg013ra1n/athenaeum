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
    build_quads, CatalogStar, GnomonicProjection, ImageStar, Quad, SolveHints, WcsSolution,
};
use astroimage::ImageAnalyzer;

use crate::catalog::CatalogEngine;
use crate::models::Frame;
use crate::plate_solve::config::PlateSolveConfig;
use crate::plate_solve::hints::{extract_hints, observation_epoch};
use crate::plate_solve::quad_index::{hash_key_from_ratios, QuadIndex, QuadLookup};
use crate::plate_solve::dso_lookup::DsoCatalog;
use crate::plate_solve::storage::{
    insert_plate_solve, update_frame_from_solve, update_frame_object_if_missing, PlateSolveRecord,
};

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
    let mut analyzer = ImageAnalyzer::new()
        .with_detection_sigma(5.0)
        .with_max_stars(max_detection_cap);
    if let Some(pool) = thread_pool {
        analyzer = analyzer.with_thread_pool(pool);
    }

    let (image_stars, image_size) = if config.use_fast_detection {
        let r = analyzer
            .detect_fast(file_path)
            .with_context(|| format!("fast star detection failed for {file_path}"))?;
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
        (stars, (r.width as u32, r.height as u32))
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
        (stars, (analysis.width as u32, analysis.height as u32))
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

    // 2. Progressive retry: try with smaller star counts first (fast path
    // for sparse/bright fields), escalate to more stars only if acceptance
    // fails. Each pass builds its own quads from a subset of the brightest
    // stars and runs the full hash-lookup → scale-filter → verify loop.
    let obs_epoch = observation_epoch(frame);
    let expected_scale_arcsec = hints.pixel_scale_arcsec;
    if let Some(s) = expected_scale_arcsec {
        eprintln!(
            "plate_solve [{}]: expected pixel scale from header: {:.3}\"/px",
            filename, s
        );
    }

    // If the config's retry_passes is empty or all values are 0, fall back
    // to a single pass at the legacy max_image_stars value.
    let passes: Vec<usize> = if config.retry_passes.iter().any(|n| *n > 0) {
        config.retry_passes.iter().copied().filter(|n| *n > 0).collect()
    } else {
        vec![config.max_image_stars]
    };

    // Compute positions once; every retry pass reads the same data.
    let image_positions: Vec<(f64, f64)> =
        image_stars.iter().map(|s| (s.x, s.y)).collect();

    let mut best_result: Option<SolveResult> = None;
    let mut best_inliers: usize = 0;
    let mut best_expected_in_fov: usize = 0;

    for (pass_idx, pass_size) in passes.iter().copied().enumerate() {
        let outcome = try_solve_pass(
            &image_stars,
            &image_positions,
            pass_size,
            &filename,
            image_size,
            image_center,
            expected_scale_arcsec,
            catalog,
            index,
            config,
            obs_epoch,
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
            if outcome.best_inliers >= required_this {
                best_inliers = outcome.best_inliers;
                best_expected_in_fov = outcome.best_expected_in_fov;
                best_result = Some(candidate.clone());
                eprintln!(
                    "plate_solve [{}]: pass {} accepted — {} inliers ≥ {} required (FOV density {})",
                    filename,
                    pass_idx + 1,
                    best_inliers,
                    required_this,
                    best_expected_in_fov
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

    let Some(mut result) = best_result else {
        return Err(anyhow::anyhow!(
            "[{}] no candidate passed verification across {} pass(es)",
            filename,
            passes.len()
        ));
    };

    // Final density-aware acceptance gate.
    let required = required_inliers(
        best_expected_in_fov,
        image_stars.len(),
        config.min_inlier_ratio,
        config.min_matched_stars,
    );
    if best_inliers < required {
        let (best_ra, best_dec) = result
            .wcs
            .pixel_to_sky(image_center.0, image_center.1);
        let hint = if best_expected_in_fov > 2000 {
            " — dense field; consider rebuilding the quad index with a higher magnitude limit"
        } else {
            ""
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

    let pixel_scale = result.pixel_scale_arcsec;

    // 10. Build final SolveResult
    let derived_fl = if frame.focallen.is_none() {
        frame.xpixsz.map(|xpixsz| {
            let pix_mm = xpixsz / 1000.0;
            206265.0 * pix_mm / pixel_scale
        })
    } else {
        None
    };

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
) -> Result<()> {
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

// ────────── helpers ──────────

#[derive(Clone, Debug)]
struct Candidate {
    image_quad: Quad,
    catalog: QuadLookup,
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
fn required_inliers(
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

/// Convert an arcsecond-scale base tolerance to a per-frame pixel tolerance,
/// clamped to [4, 20] px. Tight FOVs get smaller pixel tolerances (fewer
/// false matches); wide-field frames get larger ones (slightly defocused
/// stars still count). Used in place of the old fixed `verification_tolerance_px`.
fn adaptive_tol_px(pixel_scale_arcsec: f64, base_arcsec: f64) -> f64 {
    if pixel_scale_arcsec.abs() < 1e-6 {
        return 10.0;
    }
    (base_arcsec / pixel_scale_arcsec).clamp(4.0, 20.0)
}

/// Great-circle angular distance between two sky positions, in degrees.
fn angular_distance_deg(ra1: f64, dec1: f64, ra2: f64, dec2: f64) -> f64 {
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
    catalog: &CatalogEngine,
    index: &QuadIndex,
    config: &PlateSolveConfig,
    obs_epoch: f64,
) -> PassOutcome {
    // Build quads from the brightest `pass_size` stars.
    let image_quads = build_quads(image_positions, pass_size);
    if image_quads.len() < 10 {
        return PassOutcome {
            image_quads_built: image_quads.len(),
            ..Default::default()
        };
    }

    // Hash lookup → candidates.
    let tolerance = index.hash_tolerance();
    let mut candidates: Vec<Candidate> = Vec::new();
    for iq in &image_quads {
        let ratios = [
            iq.ratios[0], iq.ratios[1], iq.ratios[2], iq.ratios[3], iq.ratios[4],
        ];
        let hash_key = hash_key_from_ratios(&ratios, tolerance);
        for hit in index.lookup(&hash_key) {
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
    //   - `filter_scale_tolerance` = 0.05  tightens the initial candidate
    //     filter when we have a FOCALLEN+XPIXSZ hint. Real camera/scope
    //     pairs report pixel scale to <1% accuracy, so a 5% band keeps
    //     every real candidate while cutting the hash-collision noise
    //     roughly in half (matters with deeper indexes).
    //   - `scale_tolerance` = 0.10 stays generous for the downstream
    //     refit/WCS sanity checks so a fit that drifts slightly during
    //     convergence isn't prematurely rejected.
    let filter_scale_tolerance = 0.05;
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
        let seed_scale = seed_wcs.pixel_scale_arcsec();
        let image_fov_deg = (image_size.0 as f64).max(image_size.1 as f64) * seed_scale / 3600.0;
        let cone_radius = image_fov_deg * 0.7;

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

    outcome
}

/// Check whether a WCS's derived pixel scale is physically plausible and
/// (if we have a header hint) agrees with the expected scale.
fn scale_is_plausible(
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
fn count_inliers(
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
fn fit_similarity_4param(
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
fn translation_refit(
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
/// Returns the permutation with the smallest fitting residual, or None if
/// no permutation produces a valid fit.
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

    let mut best: Option<(Vec<((f64, f64), (f64, f64))>, f64)> = None;

    for perm in &PERMUTATIONS_4 {
        let pairs: Vec<((f64, f64), (f64, f64))> = (0..4)
            .map(|k| (img_stars[k], cat_stars[perm[k]]))
            .collect();

        let Some(similarity) = fit_similarity_to_tangent(&pairs, approx_center, image_center)
        else {
            continue;
        };

        // Compute fitting residual — the lower, the better this permutation
        // fits the data. Points that are geometrically consistent give near-
        // zero residual; incorrect permutations have large residuals.
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

        match &best {
            None => best = Some((pairs, residual)),
            Some((_, best_res)) if residual < *best_res => best = Some((pairs, residual)),
            _ => {}
        }
    }
    best
}

/// A similarity transform: pixel → tangent plane (in radians).
#[derive(Clone, Debug)]
struct Similarity {
    // tangent_xi  = a * (px - cx) + b * (py - cy)
    // tangent_eta = c * (px - cx) + d * (py - cy)
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    tx: f64,
    ty: f64,
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
fn similarity_to_wcs(
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
}

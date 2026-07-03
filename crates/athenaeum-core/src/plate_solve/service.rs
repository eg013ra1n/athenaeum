//! Plate solving adapter — dispatches to `solvemyastro` as the single solver.
//!
//! # Public API (contract-preserving — these names/signatures are the external surface)
//!
//! * [`solve_frame`] — convenience wrapper (extracts hints, then calls with_hints)
//! * [`solve_frame_with_hints`] — hot path for the batch worker pool
//! * [`store_result`] — persist a [`SolveResult`] to the DB

use std::time::Instant;

use anyhow::Result;
use rusqlite::Connection;

use astroimage::platesolving::{SolveHints, WcsSolution};

use crate::models::Frame;
use crate::plate_solve::config::PlateSolveConfig;
use crate::plate_solve::dso_lookup::DsoCatalog;
use crate::plate_solve::gate_audit::GateStage;
use crate::plate_solve::hints::{extract_hints, observation_epoch};
use crate::plate_solve::storage::{
    insert_plate_solve, update_frame_from_solve, update_frame_object_if_missing, PlateSolveRecord,
};

use solvemyastro::{SolveConfig, StarCache};

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
    // ── SIP distortion coefficients (populated by solvemyastro) ──────────
    /// SIP polynomial order (None → no SIP fitted).
    pub sip_order: Option<u8>,
    /// SIP forward A coefficients serialized as JSON `[[f64]]` (triangular).
    pub sip_a_coeffs: Option<String>,
    /// SIP forward B coefficients serialized as JSON `[[f64]]` (triangular).
    pub sip_b_coeffs: Option<String>,
    /// SIP reverse AP coefficients serialized as JSON `[[f64]]` (triangular).
    pub sip_ap_coeffs: Option<String>,
    /// SIP reverse BP coefficients serialized as JSON `[[f64]]` (triangular).
    pub sip_bp_coeffs: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Phase 3 adapter: solvemyastro backend
// ═══════════════════════════════════════════════════════════════════════════

/// Solve a single frame using the `solvemyastro` star-cache backend.
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
    cache: &StarCache,
    config: &PlateSolveConfig,
) -> Result<SolveResult> {
    solve_frame_tiered(frame, file_path, conn, cache, None, config)
}

/// Variant of [`solve_frame`] that also accepts an optional bright
/// sub-catalog. When `Some`, the solvemyastro backend uses
/// `Caches::tiered(cache, bright)` for fast quad matching with
/// auto-fallback to `cache` on under-population. When `None`, behaves
/// identically to [`solve_frame`].
pub fn solve_frame_tiered(
    frame: &Frame,
    file_path: &str,
    conn: &Connection,
    cache: &StarCache,
    bright_cache: Option<&StarCache>,
    config: &PlateSolveConfig,
) -> Result<SolveResult> {
    let hints = extract_hints(frame, Some(conn));
    let caches = match bright_cache {
        Some(b) => solvemyastro::Caches::tiered(cache, b),
        None => solvemyastro::Caches::deep_only(cache),
    };
    solve_frame_with_hints(frame, file_path, &hints, &caches, config, None)
}

/// Layered variant of [`solve_frame`] — solves against an additive density-tier
/// stack via `Caches::layered`. `layers` is the ordered tier stack (base →
/// deepest), typically opened from [`crate::plate_solve::discover_layers`].
pub fn solve_frame_layered(
    frame: &Frame,
    file_path: &str,
    conn: &Connection,
    layers: &[&StarCache],
    config: &PlateSolveConfig,
) -> Result<SolveResult> {
    let hints = extract_hints(frame, Some(conn));
    let caches = solvemyastro::Caches::layered(layers);
    solve_frame_with_hints(frame, file_path, &hints, &caches, config, None)
}

/// Solve a single frame using pre-extracted hints and the `solvemyastro`
/// star-cache backend. This is the hot-path function used by the batch
/// worker pool — it is DB-free, fully `Send`, and shares read-only
/// cache/config state across threads.
///
/// `caches` selects the catalog strategy: `Caches::layered(..)` for the
/// additive density-tier stack, or `Caches::deep_only`/`tiered` for the legacy
/// deep(+bright) path. The caller builds it (the tier stack is opened via
/// [`crate::plate_solve::discover_layers`]).
pub fn solve_frame_with_hints(
    frame: &Frame,
    file_path: &str,
    hints: &SolveHints,
    caches: &solvemyastro::Caches<'_>,
    config: &PlateSolveConfig,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<SolveResult> {
    let span = tracing::info_span!("solve", frame_id = frame.id.unwrap_or(-1));
    let _g = span.enter();

    let total_start = Instant::now();

    // Map astroimage::platesolving::SolveHints → solvemyastro::SolveHints.
    // `rotation` has no counterpart in solvemyastro (not yet used by the
    // solver core); `search_radius_deg` is left as None so the solver uses
    // its default (DEFAULT_HINTED_RADIUS_DEG = 10°), matching the legacy
    // `position_hint_radius_deg` default of 10°.
    let sma_hints = solvemyastro::SolveHints {
        ra: hints.ra,
        dec: hints.dec,
        fov_deg: hints.fov_deg,
        pixel_scale_arcsec: hints.pixel_scale_arcsec,
        search_radius_deg: None,
        // Real per-frame observation epoch (DATE-OBS) so catalog proper motions
        // are propagated to when the frame was shot, not a hardcoded 2024.04.
        epoch: Some(observation_epoch(frame)),
    };

    let sma_cfg = SolveConfig {
        quad_tolerance: 0.007,
        catalog_mag_limit: 19.0,
        fit_sip: true,
        sip_order: config.sip_order,
        print_timing: false,
        ..SolveConfig::default()
    };

    // Propagate the solver error verbatim (no lossy context wrapper) so the
    // structured `solvemyastro::SolveFailure` stays at the top of the chain
    // and `plate_solve::failure::describe_solve_failure` can downcast it.
    let solution = solvemyastro::solve(
        std::path::Path::new(file_path),
        &sma_hints,
        caches,
        &sma_cfg,
        cancel,
    )?;

    let total_ms = total_start.elapsed().as_millis() as u64;

    // Map solvemyastro::WcsSolution → astroimage::platesolving::WcsSolution.
    // Both types carry SIP; we populate the astroimage WcsSolution with the
    // SIP polynomials from solvemyastro so that pixel_to_sky / sky_to_pixel
    // on the result is SIP-corrected. We also serialize the coefficients into
    // the SolveResult extension fields for DB persistence.
    //
    // solvemyastro::SipCoefficients.coeffs is Vec<Vec<f64>> (triangular, dynamic).
    // astroimage::platesolving::SipCoefficients.coeffs is [[f64; 6]; 6] (fixed, max order 5).
    // We copy up to min(6, len) × min(6, len) entries; excess is left zero.
    let convert_sip = |sma: &solvemyastro::SipCoefficients| {
        let mut coeffs = [[0.0f64; 6]; 6];
        for (i, row) in sma.coeffs.iter().enumerate().take(6) {
            for (j, &v) in row.iter().enumerate().take(6) {
                coeffs[i][j] = v;
            }
        }
        astroimage::platesolving::SipCoefficients {
            order: sma.order.min(5),
            coeffs,
        }
    };

    let wcs_sip_forward = solution
        .wcs
        .sip_forward
        .as_ref()
        .map(|(a, b)| (convert_sip(a), convert_sip(b)));
    let wcs_sip_reverse = solution
        .wcs
        .sip_reverse
        .as_ref()
        .map(|(ap, bp)| (convert_sip(ap), convert_sip(bp)));

    let wcs = WcsSolution {
        crpix: solution.wcs.crpix,
        crval: solution.wcs.crval,
        cd: solution.wcs.cd,
        sip_forward: wcs_sip_forward,
        sip_reverse: wcs_sip_reverse,
    };

    // Serialize SIP coefficient matrices as JSON strings for DB storage.
    // `SipCoefficients.coeffs` is `Vec<Vec<f64>>` (triangular, i+j ≤ order).
    let (sip_order, sip_a, sip_b, sip_ap, sip_bp) =
        if let Some((ref a, ref b)) = solution.wcs.sip_forward {
            let order = a.order;
            let a_json = serde_json::to_string(&a.coeffs).ok();
            let b_json = serde_json::to_string(&b.coeffs).ok();
            let (ap_json, bp_json) = if let Some((ref ap, ref bp)) = solution.wcs.sip_reverse {
                (
                    serde_json::to_string(&ap.coeffs).ok(),
                    serde_json::to_string(&bp.coeffs).ok(),
                )
            } else {
                (None, None)
            };
            (Some(order), a_json, b_json, ap_json, bp_json)
        } else {
            (None, None, None, None, None)
        };

    // Verification cone: query the cache around the solved centre to get
    // `expected_catalog_stars_in_fov` and `inlier_ratio`. We use the same
    // FOV radius as the legacy solver (half the diagonal of the image).
    // `solution.inlier_ratio` already comes from solvemyastro and reflects
    // matched_stars / bright-catalog-in-fov; we also expose the raw cone
    // count for the density-aware gate in `store_result`.
    let obs_epoch = observation_epoch(frame);
    let fov_radius_deg = {
        // If we have a pixel scale and total_detected, reconstruct the FOV.
        // Fall back to a reasonable default if not available.
        if solution.pixel_scale_arcsec > 0.0 && solution.total_detected > 0 {
            // Approximate: assume square-ish image, diagonal ~ sqrt(2) * side
            // We can't recover exact image dimensions here, so use a generous
            // half-diagonal approximation that matches solvemyastro's verify cone.
            // 3 degrees is a safe upper bound for common astrophotography setups.
            3.0_f64.min(
                solution.pixel_scale_arcsec * (solution.total_detected as f64).sqrt()
                    / 3600.0
                    / 2.0
                    * 1.41,
            )
        } else {
            1.5
        }
    };
    let bright_mag_limit = 12.0_f32; // mirrors VERIFY_MAG_LIMIT in orchestrate.rs
    // Representative catalog for the bright-star FOV count: the legacy deep
    // cache, or — for an additive tier stack — the base layer (all bright stars
    // brighter than `bright_mag_limit` live in the brightest/base tier).
    let count_bright_in_fov = |c: &StarCache| {
        c.cone(
            solution.wcs.crval.0,
            solution.wcs.crval.1,
            fov_radius_deg,
            bright_mag_limit,
            obs_epoch,
        )
        .map(|v| v.len())
        .unwrap_or(0)
    };
    let expected_catalog_stars_in_fov = match *caches {
        solvemyastro::Caches::Legacy { deep, .. } => count_bright_in_fov(deep),
        solvemyastro::Caches::Layered { layers } => {
            layers.first().map(|&c| count_bright_in_fov(c)).unwrap_or(0)
        }
    };

    // Use solvemyastro's inlier_ratio directly: it is matched_stars /
    // total_detected (see solvemyastro `Solution::inlier_ratio`) — the
    // discriminator the persist gate keys on. Real solves sit well above the
    // ~0.04 floor; noise/false-positive alignments are ~0.001.
    let inlier_ratio = solution.inlier_ratio;

    // Derive focal length from the solved pixel scale + the frame's pixel
    // size, ALWAYS when computable (not only when header was missing). This
    // lets us detect-and-correct a wrong header FOCALLEN — without it, a
    // bogus header value silently survives every solve and locks subsequent
    // solves into the wrong scale-ladder rung. Inverts the hint formula in
    // hints.rs (scale_tan = px_mm/fl ⇒ fl = px_mm / tan(scale)).
    //
    // When the FITS header lacks XPIXSZ entirely (some surveys ship sparse
    // headers — e.g. SkyMapper), fall back to a user-configured per-camera
    // default keyed by INSTRUME or TELESCOP. Without that fallback, focallen
    // cannot be algebraically derived from arcsec/px alone (two unknowns).
    let effective_xpixsz = frame.xpixsz.or_else(|| {
        let lookup = |key: Option<&str>| -> Option<f64> {
            let k = key.map(str::trim).filter(|s| !s.is_empty())?;
            config.camera_defaults.get(k).copied()
        };
        lookup(frame.instrume.as_deref()).or_else(|| lookup(frame.telescop.as_deref()))
    });
    let derived_focallen_mm = if solution.pixel_scale_arcsec > 0.0 {
        effective_xpixsz.and_then(|xpixsz| {
            if xpixsz <= 0.0 {
                return None;
            }
            let pixel_size_mm = xpixsz / 1000.0;
            // solvemyastro uses XPIXSZ directly (no binning multiply), so
            // we mirror that convention here.
            let scale_tan = (solution.pixel_scale_arcsec / 3600.0).to_radians().tan();
            if scale_tan > 0.0 {
                Some(pixel_size_mm / scale_tan)
            } else {
                None
            }
        })
    } else {
        None
    };

    // `focallen_corrected = true` makes update_frame_from_solve OVERWRITE the
    // frames.focallen value (storage.rs:198); `false` only writes when the
    // header was NULL. We mark corrected when either:
    //   - header focallen was missing (fill), or
    //   - the solved value differs from the header by > 2% (header was wrong
    //     — solvemyastro must have discarded the hint and re-derived scale).
    let focallen_corrected = match (frame.focallen, derived_focallen_mm) {
        (None, Some(_)) => true,
        (Some(hdr), Some(derived)) if derived > 0.0 => ((derived - hdr).abs() / derived) > 0.02,
        _ => false,
    };

    Ok(SolveResult {
        wcs,
        matched_stars: solution.matched_stars,
        total_detected: solution.total_detected,
        rms_residual_px: solution.rms_residual_px,
        rms_residual_arcsec: solution.rms_residual_arcsec,
        pixel_scale_arcsec: solution.pixel_scale_arcsec,
        field_rotation_deg: solution.field_rotation_deg,
        solve_time_ms: total_ms,
        catalog_used: solution.catalog_used,
        algorithm_used: solution.algorithm,
        derived_focallen_mm,
        focallen_corrected,
        expected_catalog_stars_in_fov,
        inlier_ratio,
        sip_order,
        sip_a_coeffs: sip_a,
        sip_b_coeffs: sip_b,
        sip_ap_coeffs: sip_ap,
        sip_bp_coeffs: sip_bp,
    })
}

/// Outcome of [`store_result`]: whether the solve was persisted or refused by
/// the confidence gate. The batch reports `RejectedLowConfidence` to the UI as
/// a failure-with-reason rather than silently counting it as solved.
#[derive(Clone, Debug)]
pub enum StoreOutcome {
    /// WCS / focal length written back and a `plate_solves` row inserted.
    Persisted,
    /// The solve cleared solvemyastro but failed Athenaeum's confidence gate;
    /// nothing was written. `reason` is a user-facing explanation.
    RejectedLowConfidence { reason: String },
}

/// Persist a solve result to the database.
///
/// If `dso_catalog` is provided, the nearest named deep-sky object at the
/// solved position is looked up and — if the frame's `object` field is
/// currently NULL or empty — used to label the frame.
///
/// Returns [`StoreOutcome::RejectedLowConfidence`] (without writing anything)
/// when the solve fails the confidence gate, so the caller can surface it.
pub fn store_result(
    conn: &Connection,
    frame_id: i64,
    result: &SolveResult,
    dso_catalog: Option<&DsoCatalog>,
    config: &PlateSolveConfig,
) -> Result<StoreOutcome> {
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
            let reason = format!(
                "Rejected: low confidence ({} inliers, ratio {:.2}, {:.2}\"/px)",
                result.matched_stars, result.inlier_ratio, result.pixel_scale_arcsec
            );
            tracing::warn!(
                frame_id,
                stage = "store",
                outcome = "rejected_low_confidence",
                inliers = result.matched_stars,
                inlier_ratio = result.inlier_ratio,
                scale_arcsec_px = result.pixel_scale_arcsec,
                "refusing to persist low-confidence solve, WCS/focal length not written back"
            );
            return Ok(StoreOutcome::RejectedLowConfidence { reason });
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
        // SIP coefficients — populated from solvemyastro when fit_sip=true.
        // The DB columns already exist (schema unchanged); legacy solves wrote
        // NULL, now we write real values.
        sip_order: result.sip_order.map(|o| o as i32),
        sip_a_coeffs: result.sip_a_coeffs.clone(),
        sip_b_coeffs: result.sip_b_coeffs.clone(),
        sip_ap_coeffs: result.sip_ap_coeffs.clone(),
        sip_bp_coeffs: result.sip_bp_coeffs.clone(),
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
                Ok(true) => tracing::debug!(
                    frame_id,
                    designation = %m.designation,
                    reason = ?m.reason,
                    distance_deg = m.distance_deg,
                    "labelled frame from solved position"
                ),
                Ok(false) => {}
                Err(e) => tracing::warn!(frame_id, error = %e, "failed to update frame.object after solve"),
            }
        }
    }

    Ok(StoreOutcome::Persisted)
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
    if !m.rms_px.is_finite() || m.rms_px > cfg.blind_rms_max_px_mult * m.adaptive_tol_px {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_audit_disabled_is_zero_behaviour_change() {
        assert!(!crate::plate_solve::gate_audit::enabled());
    }

    fn mk_result(matched: usize, expected: usize, ratio: f64, scale: f64, rms: f64) -> SolveResult {
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
            catalog_used: "smac_gaia".into(),
            algorithm_used: "blind_index".into(),
            derived_focallen_mm: None,
            focallen_corrected: false,
            expected_catalog_stars_in_fov: expected,
            inlier_ratio: ratio,
            sip_order: None,
            sip_a_coeffs: None,
            sip_b_coeffs: None,
            sip_ap_coeffs: None,
            sip_bp_coeffs: None,
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
            conn.execute("INSERT INTO frames (id,file_id) VALUES (?1,?2)", [fid, fid])
                .unwrap();
        }

        // High-confidence (real-solve shape) — must persist.
        let out1 =
            store_result(&conn, 1, &mk_result(120, 800, 0.15, 1.5, 1.0), None, &cfg).unwrap();
        assert!(
            matches!(out1, StoreOutcome::Persisted),
            "high-confidence solve must report Persisted"
        );
        let ps1: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM plate_solves WHERE frame_id=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let (ra1, ovr1): (Option<f64>, i64) = conn
            .query_row("SELECT ra,override FROM frames WHERE id=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(ps1, 1, "high-confidence solve must persist");
        assert!(ra1.is_some() && ovr1 == 1, "must write WCS + override");

        // Low-confidence false-positive shape — must be refused.
        let out2 = store_result(
            &conn,
            2,
            &mk_result(90, 150_000, 0.0006, 22.0, 2.8),
            None,
            &cfg,
        )
        .unwrap();
        assert!(
            matches!(out2, StoreOutcome::RejectedLowConfidence { .. }),
            "low-confidence solve must report RejectedLowConfidence"
        );
        let ps2: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM plate_solves WHERE frame_id=2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let (ra2, ovr2): (Option<f64>, i64) = conn
            .query_row("SELECT ra,override FROM frames WHERE id=2", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(
            ps2, 0,
            "low-confidence solve must NOT create a plate_solves row"
        );
        assert!(
            ra2.is_none() && ovr2 == 0,
            "low-confidence solve must NOT mutate the frame"
        );
    }

    #[test]
    fn store_result_persists_defocused_low_count_solve() {
        // Regression (M51 _0060): a slightly OUT-OF-FOCUS but CORRECT solve —
        // bloated stars → 27% fewer detections → only ~10 inliers, yet
        // inlier_ratio 0.070 (70x above the ~0.001 false-positive ceiling) and
        // the right sky position — was silently discarded by the old fixed
        // inlier-count floor of 12. The count is a weak backstop (it does not
        // separate real from false — inlier_ratio / rms / scale do), so the
        // floor is now 6 = solvemyastro's own MIN_ABSOLUTE_INLIERS. Defocus
        // must no longer cost a correct solve its WCS write-back.
        use crate::db::schema::init_db;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        for (fid, name) in [(1, "m51_0060.fits"), (2, "m51_0060b.fits")] {
            conn.execute(
                "INSERT INTO files (id,path,filename,size,modified_at,format,created_at)
                 VALUES (?1,?2,?3,0,'2025-01-01','FITS','2025-01-01')",
                rusqlite::params![fid, format!("/x/{name}"), name],
            )
            .unwrap();
            conn.execute("INSERT INTO frames (id,file_id) VALUES (?1,?2)", [fid, fid])
                .unwrap();
        }

        // _0060 shape: 10 inliers, dense field (expected 800 so the ratio gate
        // is active), ratio 0.070, scale 0.48"/px, rms 2.35px.
        let defocused = mk_result(10, 800, 0.070, 0.48, 2.35);

        // Default gate (floor now 6): the correct-but-sparse solve PERSISTS.
        let out =
            store_result(&conn, 1, &defocused, None, &PlateSolveConfig::default()).unwrap();
        assert!(
            matches!(out, StoreOutcome::Persisted),
            "defocused-but-correct solve (10 inliers, ratio 0.070) must persist"
        );
        let ps: i64 = conn
            .query_row("SELECT COUNT(*) FROM plate_solves WHERE frame_id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(ps, 1, "must write a plate_solves row");

        // Same shape under the OLD floor of 12 is refused — proof the count
        // floor (not ratio / rms / scale) was the binding constraint the fix
        // removes.
        let old_cfg = PlateSolveConfig {
            blind_inlier_floor: 12,
            ..PlateSolveConfig::default()
        };
        let out_old = store_result(&conn, 2, &defocused, None, &old_cfg).unwrap();
        assert!(
            matches!(out_old, StoreOutcome::RejectedLowConfidence { .. }),
            "old floor of 12 rejected the same 10-inlier solve — that was the bug"
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
        assert!(!blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                rms_px: 20.0,
                ..base.clone()
            },
            &cfg
        ));
        // Low ratio on a DENSE field rejected (the calibrated primary gate).
        assert!(!blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                inlier_ratio: 0.001,
                ..base.clone()
            },
            &cfg
        ));
        // Sparse field (expected<=100) NOT punished by the ratio rule — stage is irrelevant here.
        assert!(blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                expected_in_fov: 40,
                inlier_ratio: 0.001,
                inliers: 14,
                ..base.clone()
            },
            &cfg
        ));
        // Too few inliers rejected.
        assert!(!blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                inliers: 8,
                ..base.clone()
            },
            &cfg
        ));
        // Absurd recovered scale rejected.
        assert!(!blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                recovered_scale_arcsec: 0.001,
                ..base.clone()
            },
            &cfg
        ));
        // Recovered scale wildly off header scale rejected (ratio ~10 > tol 8).
        assert!(!blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                recovered_scale_arcsec: 20.0,
                header_scale_arcsec: Some(1.9),
                ..base.clone()
            },
            &cfg
        ));
        // Non-finite RMS is never a real solve (guards the is_finite branch;
        // degenerate blind solves can produce NaN/inf).
        assert!(!blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                rms_px: f64::NAN,
                ..base.clone()
            },
            &cfg
        ));
        assert!(!blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                rms_px: f64::INFINITY,
                ..base.clone()
            },
            &cfg
        ));
        // No header scale: the header-tol guard is skipped entirely, so a
        // recovered scale that WOULD fail the ratio-to-header check still
        // passes when header_scale_arcsec is None.
        assert!(blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                header_scale_arcsec: None,
                recovered_scale_arcsec: 20.0,
                ..base.clone()
            },
            &cfg
        ));
        // Scale-sanity MAX bound (the existing table only covered the min side).
        assert!(!blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                recovered_scale_arcsec: 100.0,
                ..base.clone()
            },
            &cfg
        ));
        // Gate can be disabled by config.
        let off = PlateSolveConfig {
            blind_gate_enabled: false,
            ..cfg.clone()
        };
        assert!(blind_gate_ok(
            GateStage::FullBlind,
            &BlindGateMetrics {
                rms_px: 99.0,
                ..base
            },
            &off
        ));
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

    // ── Gate 4: SolveSolution → SolveResult SIP mapping ────────────────────
    //
    // Proves that the Phase-3 adapter correctly populates the `sip_*` fields
    // on `SolveResult` (non-None) when `solvemyastro::WcsSolution::sip_forward`
    // is `Some`, and that WCS CD/CRVAL/CRPIX copy through 1:1.
    //
    // Rather than calling the live solver (which needs a real file + cache),
    // we replicate the exact conversion snippet from `solve_frame_with_hints`
    // and verify its outputs — this is the right unit-of-test for the mapping
    // logic rather than the I/O logic.
    #[test]
    fn sma_solution_sip_forward_populates_solve_result_sip_fields() {
        use solvemyastro::{SipCoefficients as SmaSip, WcsSolution as SmaWcs};

        // Build a SolveSolution with a non-trivial order-2 SIP A/B pair and
        // an inverse (AP/BP) pair.  Coefficients are triangular: index [i][j]
        // where i+j ≤ order.
        let make_sip = |seed: f64| SmaSip {
            order: 2,
            coeffs: vec![
                vec![0.0, seed * 0.1, seed * 0.2],
                vec![seed * 0.3, seed * 0.4],
                vec![seed * 0.5],
            ],
        };
        let sip_a = make_sip(1.0);
        let sip_b = make_sip(2.0);
        let sip_ap = make_sip(3.0);
        let sip_bp = make_sip(4.0);

        let crpix = (1234.5, 678.9);
        let crval = (83.82, -5.39); // Orion Nebula-ish
        let cd_mat = [[5.5e-5_f64, 1.1e-7], [-1.1e-7, 5.5e-5]];

        let sma_wcs = SmaWcs {
            crpix,
            crval,
            cd: cd_mat,
            sip_forward: Some((sip_a.clone(), sip_b.clone())),
            sip_reverse: Some((sip_ap.clone(), sip_bp.clone())),
        };

        // ── replicate the adapter's convert_sip closure ──────────────────
        let convert_sip = |sma: &SmaSip| {
            let mut coeffs = [[0.0f64; 6]; 6];
            for (i, row) in sma.coeffs.iter().enumerate().take(6) {
                for (j, &v) in row.iter().enumerate().take(6) {
                    coeffs[i][j] = v;
                }
            }
            astroimage::platesolving::SipCoefficients {
                order: sma.order.min(5),
                coeffs,
            }
        };

        let wcs_sip_fwd = sma_wcs
            .sip_forward
            .as_ref()
            .map(|(a, b)| (convert_sip(a), convert_sip(b)));
        let wcs_sip_rev = sma_wcs
            .sip_reverse
            .as_ref()
            .map(|(ap, bp)| (convert_sip(ap), convert_sip(bp)));

        let wcs = WcsSolution {
            crpix: sma_wcs.crpix,
            crval: sma_wcs.crval,
            cd: sma_wcs.cd,
            sip_forward: wcs_sip_fwd,
            sip_reverse: wcs_sip_rev,
        };

        // ── replicate the adapter's SIP JSON serialization block ─────────
        let (sip_order, sip_a_json, sip_b_json, sip_ap_json, sip_bp_json) =
            if let Some((ref a, ref b)) = sma_wcs.sip_forward {
                let order = a.order;
                let a_j = serde_json::to_string(&a.coeffs).ok();
                let b_j = serde_json::to_string(&b.coeffs).ok();
                let (ap_j, bp_j) = if let Some((ref ap, ref bp)) = sma_wcs.sip_reverse {
                    (
                        serde_json::to_string(&ap.coeffs).ok(),
                        serde_json::to_string(&bp.coeffs).ok(),
                    )
                } else {
                    (None, None)
                };
                (Some(order), a_j, b_j, ap_j, bp_j)
            } else {
                (None, None, None, None, None)
            };

        // ── assertions ───────────────────────────────────────────────────

        // 1. WCS fields copy 1:1.
        assert_eq!(wcs.crpix, crpix, "crpix must copy 1:1");
        assert_eq!(wcs.crval, crval, "crval must copy 1:1");
        assert_eq!(wcs.cd, cd_mat, "CD matrix must copy 1:1");

        // 2. SIP forward transferred into fixed-array astroimage type.
        let (fwd_a, fwd_b) = wcs.sip_forward.as_ref().expect("sip_forward must be Some");
        assert_eq!(fwd_a.order, 2, "A order preserved");
        assert_eq!(fwd_b.order, 2, "B order preserved");
        // Check a few representative coefficients against make_sip(1.0)/make_sip(2.0).
        assert!(
            (fwd_a.coeffs[1][0] - 0.3_f64).abs() < 1e-12,
            "A[1][0] = seed*0.3"
        );
        assert!(
            (fwd_b.coeffs[0][1] - 0.2_f64).abs() < 1e-12,
            "B[0][1] = seed*0.2 (seed=2→0.2 from make_sip(2): 2*0.1=0.2)"
        );

        // 3. SIP reverse transferred.
        assert!(wcs.sip_reverse.is_some(), "sip_reverse must be Some");

        // 4. SolveResult SIP extension fields are all non-None.
        assert_eq!(sip_order, Some(2), "sip_order must be Some(2)");
        assert!(sip_a_json.is_some(), "sip_a_coeffs must be Some");
        assert!(sip_b_json.is_some(), "sip_b_coeffs must be Some");
        assert!(sip_ap_json.is_some(), "sip_ap_coeffs must be Some");
        assert!(sip_bp_json.is_some(), "sip_bp_coeffs must be Some");

        // 5. JSON round-trips — deserialised coefficients match originals.
        let a_rt: Vec<Vec<f64>> = serde_json::from_str(sip_a_json.as_ref().unwrap()).unwrap();
        assert_eq!(
            a_rt.len(),
            sip_a.coeffs.len(),
            "A JSON round-trip row count"
        );
        for (i, row) in sip_a.coeffs.iter().enumerate() {
            for (j, &expected_val) in row.iter().enumerate() {
                assert!(
                    (a_rt[i][j] - expected_val).abs() < 1e-15,
                    "A[{i}][{j}] JSON round-trip: got {} expected {}",
                    a_rt[i][j],
                    expected_val
                );
            }
        }

        // 6. When sip_forward is None the result fields are all None.
        let sma_wcs_no_sip = SmaWcs {
            crpix,
            crval,
            cd: cd_mat,
            sip_forward: None,
            sip_reverse: None,
        };
        let (ord2, a2, b2, ap2, bp2) = if let Some((ref a, ref b)) = sma_wcs_no_sip.sip_forward {
            let order = a.order;
            let a_j = serde_json::to_string(&a.coeffs).ok();
            let b_j = serde_json::to_string(&b.coeffs).ok();
            let (ap_j, bp_j) = if let Some((ref ap, ref bp)) = sma_wcs_no_sip.sip_reverse {
                (
                    serde_json::to_string(&ap.coeffs).ok(),
                    serde_json::to_string(&bp.coeffs).ok(),
                )
            } else {
                (None, None)
            };
            (Some(order), a_j, b_j, ap_j, bp_j)
        } else {
            (None, None, None, None, None)
        };
        assert!(ord2.is_none(), "no-SIP solution: sip_order must be None");
        assert!(a2.is_none(), "no-SIP solution: sip_a_coeffs must be None");
        assert!(b2.is_none(), "no-SIP solution: sip_b_coeffs must be None");
        assert!(ap2.is_none(), "no-SIP solution: sip_ap_coeffs must be None");
        assert!(bp2.is_none(), "no-SIP solution: sip_bp_coeffs must be None");
    }

    // ── focallen derivation + correction policy ──────────────────────────
    //
    // Replicates the (xpixsz, scale) → derived_focallen_mm computation and
    // the focallen_corrected match used in `solve_frame_with_hints`. Mirrors
    // the SIP-mapping test pattern (verify the conversion snippet without
    // running the live solver).
    fn derived_focallen_mm(xpixsz_um: f64, scale_arcsec: f64) -> Option<f64> {
        if scale_arcsec <= 0.0 || xpixsz_um <= 0.0 {
            return None;
        }
        let pixel_size_mm = xpixsz_um / 1000.0;
        let scale_tan = (scale_arcsec / 3600.0).to_radians().tan();
        if scale_tan > 0.0 {
            Some(pixel_size_mm / scale_tan)
        } else {
            None
        }
    }

    fn focallen_corrected(header: Option<f64>, derived: Option<f64>) -> bool {
        match (header, derived) {
            (None, Some(_)) => true,
            (Some(hdr), Some(d)) if d > 0.0 => ((d - hdr).abs() / d) > 0.02,
            _ => false,
        }
    }

    #[test]
    fn derived_focallen_roundtrips_through_hint_formula() {
        // 1750 mm scope, 3.76 µm pixels: 0.443"/px expected.
        let derived = derived_focallen_mm(3.76, 0.443).expect("solvable");
        assert!(
            (derived - 1750.0).abs() < 10.0,
            "expected ~1750 mm, got {derived:.2}"
        );
    }

    #[test]
    fn derived_focallen_returns_none_for_invalid_inputs() {
        assert!(derived_focallen_mm(0.0, 0.443).is_none(), "xpixsz=0 → None");
        assert!(
            derived_focallen_mm(-3.76, 0.443).is_none(),
            "xpixsz<0 → None"
        );
        assert!(derived_focallen_mm(3.76, 0.0).is_none(), "scale=0 → None");
    }

    #[test]
    fn focallen_corrected_fills_null_header() {
        assert!(
            focallen_corrected(None, Some(1750.0)),
            "null header + derived → fill"
        );
        assert!(!focallen_corrected(None, None), "no derived → no write");
    }

    /// Replicates the `effective_xpixsz` closure in `solve_frame_with_hints`.
    /// Header xpixsz wins; otherwise INSTRUME default; then TELESCOP default;
    /// blank/whitespace keys are ignored.
    fn effective_xpixsz(
        frame_xpixsz: Option<f64>,
        instrume: Option<&str>,
        telescop: Option<&str>,
        defaults: &std::collections::HashMap<String, f64>,
    ) -> Option<f64> {
        frame_xpixsz.or_else(|| {
            let lookup = |key: Option<&str>| -> Option<f64> {
                let k = key.map(str::trim).filter(|s| !s.is_empty())?;
                defaults.get(k).copied()
            };
            lookup(instrume).or_else(|| lookup(telescop))
        })
    }

    #[test]
    fn effective_xpixsz_resolution_order() {
        let mut defaults = std::collections::HashMap::new();
        defaults.insert("SkyMapper".to_string(), 10.5);
        defaults.insert("ASI294MM".to_string(), 4.63);

        // Header xpixsz present → wins; defaults ignored.
        assert_eq!(
            effective_xpixsz(Some(3.76), Some("SkyMapper"), None, &defaults),
            Some(3.76)
        );
        // Header missing → INSTRUME default kicks in.
        assert_eq!(
            effective_xpixsz(None, Some("SkyMapper"), None, &defaults),
            Some(10.5)
        );
        // INSTRUME not in defaults → TELESCOP fallback.
        assert_eq!(
            effective_xpixsz(None, Some("UnknownCam"), Some("SkyMapper"), &defaults),
            Some(10.5)
        );
        // INSTRUME missing entirely → TELESCOP fallback.
        assert_eq!(
            effective_xpixsz(None, None, Some("SkyMapper"), &defaults),
            Some(10.5)
        );
        // Neither in defaults → None (focallen stays NULL, as before).
        assert_eq!(
            effective_xpixsz(None, Some("Foo"), Some("Bar"), &defaults),
            None
        );
        // No identifiers at all → None.
        assert_eq!(effective_xpixsz(None, None, None, &defaults), None);
        // Empty/whitespace keys are not looked up.
        assert_eq!(
            effective_xpixsz(None, Some(""), Some("  "), &defaults),
            None
        );
    }

    #[test]
    fn focallen_corrected_overwrites_wrong_header() {
        // 5% mismatch → corrected (legacy threshold is 2%).
        assert!(
            focallen_corrected(Some(1666.0), Some(1750.0)),
            "header off by 5% → must overwrite"
        );
        // 1% mismatch → leave header alone (within tolerance).
        assert!(
            !focallen_corrected(Some(1733.0), Some(1750.0)),
            "header within 2% → keep"
        );
        // Exact match → no correction needed.
        assert!(
            !focallen_corrected(Some(1750.0), Some(1750.0)),
            "exact match → keep"
        );
    }
}

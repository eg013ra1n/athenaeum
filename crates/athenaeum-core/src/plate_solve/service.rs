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
    /// The reference stars inside the solved frame (`N_R`, solvemyastro's
    /// `reference_stars_in_frame`): the denominator of `inlier_ratio`. Shown,
    /// never judged.
    pub expected_catalog_stars_in_fov: usize,
    /// `matched_stars / expected_catalog_stars_in_fov` (solvemyastro's
    /// `inlier_ratio`). Stored for display; nothing gates on it.
    pub inlier_ratio: f64,
    /// `log10` of the solver's odds that the match is real (solvemyastro's
    /// `log10_odds`): the measure of confidence the persist gate re-reads at
    /// `verify::ACCEPT_LOG10_ODDS`.
    pub log10_odds: f64,
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
    solve_frame_with_hints(frame, file_path, &hints, &caches, &[], config, None)
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
    solve_frame_with_hints(frame, file_path, &hints, &caches, &[], config, None)
}

/// [`solve_frame_with_hints`] for a catalog path: a zip-resident frame is extracted (or reused from the
/// cache) first and its cache pin is held until the solve returns. A frame that cannot be extracted (its zip
/// is not reachable) fails with the extraction error, which the batch reports as that frame's failure.
#[allow(clippy::too_many_arguments)]
pub fn solve_catalog_frame_with_hints(
    ctx: &crate::services::ServiceContext,
    frame: &Frame,
    catalog_path: &str,
    hints: &SolveHints,
    caches: &solvemyastro::Caches<'_>,
    indexes: &[&solvemyastro::QuadIndex],
    config: &PlateSolveConfig,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<SolveResult> {
    let (local, _pin) = crate::zip_store::materialize::materialize_one(ctx, catalog_path).map_err(|e| {
        tracing::warn!(frame_id = frame.id.unwrap_or(-1), path = %catalog_path, error = %e, "plate solve: frame is not readable");
        e
    })?;
    solve_frame_with_hints(frame, &local.to_string_lossy(), hints, caches, indexes, config, cancel)
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
///
/// `indexes` are the installed quad-index packs
/// ([`crate::plate_solve::open_index_packs`]). With none this is exactly the
/// spiral search; with packs, a frame lacking a position or a scale hint is
/// looked up in the index first (`solvemyastro::solve_with_index`). Fully
/// hinted frames never use it.
pub fn solve_frame_with_hints(
    frame: &Frame,
    file_path: &str,
    hints: &SolveHints,
    caches: &solvemyastro::Caches<'_>,
    indexes: &[&solvemyastro::QuadIndex],
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

    // Changing any of these: bump config::SOLVE_RECIPE_VERSION.
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
    let solution = solvemyastro::solve_with_index(
        std::path::Path::new(file_path),
        &sma_hints,
        caches,
        indexes,
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

    // The solver reports its own confidence: the in-frame reference stars
    // (`N_R`), the share of them matched, and the odds that the match is real.
    // A change to the verification magnitude limit
    // (`orchestrate::VERIFY_MAG_LIMIT`) or to the solver's bar needs
    // `config::SOLVE_RECIPE_VERSION` bumped.
    let expected_catalog_stars_in_fov = solution.reference_stars_in_frame;
    let inlier_ratio = solution.inlier_ratio;
    let log10_odds = solution.log10_odds;

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
        log10_odds,
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
    // frame metadata. The solver's odds are the discriminator (`log10_odds`
    // against `verify::ACCEPT_LOG10_ODDS`); the gate re-reads them and holds the
    // RMS, matched-star and scale backstops, exactly as during acceptance.
    if config.blind_gate_enabled {
        // The frame's own optics, when it declares them (2026-09-05: on
        // wind-shaken frames the solver walks its autoFOV ladder and returns a
        // confident-looking solve at 16-193x the header's scale). Read only
        // while the gate is on, as before.
        if let Some(reason) =
            acceptance_refusal(result, header_pixel_scale_arcsec(conn, frame_id), config)
        {
            tracing::warn!(
                frame_id,
                stage = "store",
                outcome = "rejected_low_confidence",
                inliers = result.matched_stars,
                log10_odds = result.log10_odds,
                scale_arcsec_px = result.pixel_scale_arcsec,
                "refusing to persist low-confidence solve, WCS/focal length not written back"
            );
            return Ok(StoreOutcome::RejectedLowConfidence { reason });
        }
    }

    let record = record_from_result(frame_id, result);
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
                Err(e) => {
                    tracing::warn!(frame_id, error = %e, "failed to update frame.object after solve")
                }
            }
        }
    }

    Ok(StoreOutcome::Persisted)
}

/// The persist gate as a pure check (plan 6b-2 C14): `Some(user-facing
/// reason)` when `config` refuses `result`, given the scale the frame's own
/// optics, or its manifest, imply. [`store_result`] and a stacking run's
/// local solve share it.
pub(crate) fn acceptance_refusal(
    result: &SolveResult,
    expected_scale_arcsec: Option<f64>,
    config: &PlateSolveConfig,
) -> Option<String> {
    if !config.blind_gate_enabled {
        return None;
    }
    let gate = super::config::acceptance_gate(config);
    let m = solvemyastro::accept::GateMetrics {
        inliers: result.matched_stars,
        rms_px: result.rms_residual_px,
        tol_px: solvemyastro::accept::gate_tol_px(result.pixel_scale_arcsec, gate.base_tolerance_arcsec),
        log10_odds: result.log10_odds,
        recovered_scale_arcsec: result.pixel_scale_arcsec,
        header_scale_arcsec: expected_scale_arcsec,
    };
    gate.refusal(&m).map(|refusal| format!("Rejected: {refusal}"))
}

/// The `plate_solves` row a solve becomes (pure; `solved_at` is now).
pub(crate) fn record_from_result(frame_id: i64, result: &SolveResult) -> PlateSolveRecord {
    PlateSolveRecord {
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
        log10_odds: Some(result.log10_odds),
    }
}

/// Why this frame should not be sent to the solver at all, or `None`.
///
/// Reads the frame's own analysis, when it has one and the current measurement wrote it (`measure_version` equal to
/// `star_measure::MEASURE_VERSION`; a legacy row is not trusted and never refuses). A frame whose stars are streaks has
/// nothing for the solver to match, and letting it through costs minutes per frame while the autoFOV ladder is walked
/// to its end. The measurement's own verdict decides, and alone (PC-13, PC-12 REMOVE): `trailed` refuses; every other
/// row — `defocused`, `no_stars`, a measured frame of any eccentricity or trail R² — passes (the solver's
/// minimum-star refusal is fast). The former eccentricity / trail-R² pair refused no hopeless frame the verdict lets
/// through without also refusing a solvable one (`docs/platesolving/input-and-acceptance-gates.md`).
///
/// A frame with no analysis is never refused here — the answer is unknown, and the solver's own detection cut is the
/// backstop.
pub fn input_gate_reason(
    conn: &Connection,
    frame_id: i64,
    config: &PlateSolveConfig,
) -> Option<String> {
    if !config.input_gate_enabled {
        return None;
    }
    let a = crate::db::analysis::get_frame_analysis(conn, frame_id).ok()??;
    if a.measure_version != Some(crate::star_measure::MEASURE_VERSION) {
        tracing::debug!(
            frame_id,
            measure_version = ?a.measure_version,
            "input gate: the frame's analysis predates the current measurement"
        );
        return None;
    }
    if a.frame_verdict == Some(crate::models::FrameVerdict::Trailed) {
        return Some(
            "Stars are trails the measurement could not fit — nothing to match".to_string(),
        );
    }
    None
}

/// The pixel scale the frame's own optics imply, or `None` when it does not
/// declare both of them.
///
/// `206.265 · XPIXSZ_µm / FOCALLEN_mm`, the same convention
/// [`crate::plate_solve::hints`] uses for the scale hint — XPIXSZ is already
/// the effective pitch of the saved image, so binning must not be folded in
/// a second time.
fn header_pixel_scale_arcsec(conn: &Connection, frame_id: i64) -> Option<f64> {
    let (focallen, xpixsz): (Option<f64>, Option<f64>) = conn
        .query_row(
            "SELECT focallen, xpixsz FROM frames WHERE id = ?1",
            [frame_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok()?;
    let (focallen, xpixsz) = (focallen?, xpixsz?);
    if focallen <= 0.0 || xpixsz <= 0.0 {
        return None;
    }
    Some((xpixsz / 1000.0 / focallen).atan().to_degrees() * 3600.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip-resident frame whose zip is gone fails with the extraction error (the batch reports it as that
    /// frame's failure) before the solver is ever reached.
    #[test]
    fn a_catalog_frame_in_an_unreachable_zip_fails_with_the_extraction_error() {
        use crate::zip_store::scan::test_support::{light, make_zip};
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("astro")).unwrap();
        let ctx = crate::services::ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let zip = make_zip(
            &tmp.path().join("astro"),
            "gone.zip",
            &[("Lights/L_001.fits", light(1))],
            crate::archive::models::ArchiveCompression::Deflate,
        );
        {
            let conn = ctx.db.get().unwrap().conn();
            conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)", [tmp.path().join("astro").to_str().unwrap()]).unwrap();
            crate::zip_store::scan::scan_zip(&conn, &zip, 1, false).unwrap();
        }
        std::fs::remove_file(&zip).unwrap();
        let catalog = crate::zip_store::scan::virtual_path(&zip, "Lights/L_001.fits");
        let caches = solvemyastro::Caches::layered(&[]);
        let err = solve_catalog_frame_with_hints(
            &ctx,
            &Frame::default(),
            &catalog,
            &SolveHints::default(),
            &caches,
            &[],
            &PlateSolveConfig::default(),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("gone.zip") && err.contains("not reachable"), "{err}");
    }

    /// `in_field` is `N_R`, the in-frame reference stars; `odds` is `log10_odds`.
    fn mk_result(matched: usize, in_field: usize, ratio: f64, scale: f64, rms: f64, odds: f64) -> SolveResult {
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
            expected_catalog_stars_in_fov: in_field,
            inlier_ratio: ratio,
            log10_odds: odds,
            sip_order: None,
            sip_a_coeffs: None,
            sip_b_coeffs: None,
            sip_ap_coeffs: None,
            sip_bp_coeffs: None,
        }
    }

    /// The persist gate's decisions under solvemyastro 0.4.0: the six-match noise alignment (2026-09-05: 6 inliers
    /// in a field with hundreds of reference stars) has odds far below the solver's bar and is refused in the
    /// solver's own words, the same field at a real solve's odds is accepted, and the gate switched off accepts the
    /// noise.
    #[test]
    fn acceptance_refusal_keeps_its_decisions() {
        let cfg = PlateSolveConfig::default();
        let noise = mk_result(6, 480, 0.0125, 0.2178, 1.159, 4.8);
        let refusal = acceptance_refusal(&noise, None, &cfg).expect("noise is refused");
        assert!(refusal.starts_with("Rejected: the match could be a chance alignment"), "{refusal}");
        let real = mk_result(60, 61, 0.98, 0.2178, 1.159, 120.0);
        assert_eq!(acceptance_refusal(&real, None, &cfg), None);
        let off = PlateSolveConfig { blind_gate_enabled: false, ..PlateSolveConfig::default() };
        assert_eq!(acceptance_refusal(&noise, None, &off), None);
    }

    /// Every refusal the gate can make reads as `Rejected: {GateRefusal}`, in solvemyastro's one wording.
    #[test]
    fn acceptance_refusal_names_the_failed_check() {
        let cfg = PlateSolveConfig::default();
        let text = |r: &SolveResult, header: Option<f64>| acceptance_refusal(r, header, &cfg).expect("refused");
        assert_eq!(
            text(&mk_result(6, 61, 0.098, 0.2178, 1.159, 4.8), None),
            "Rejected: the match could be a chance alignment (odds 10^4.8 to 1, 10^9 needed; 6 stars matched)"
        );
        assert_eq!(
            text(&mk_result(5, 400, 0.0125, 1.9, 1.0, 50.0), None),
            "Rejected: only 5 stars matched (6 needed)"
        );
        assert_eq!(
            text(&mk_result(120, 476, 0.25, 1.9, 20.0, 300.0), None),
            "Rejected: the stars fit too loosely (20.00 px RMS, 10.53 px allowed)"
        );
        assert_eq!(
            text(&mk_result(120, 476, 0.25, 100.0, 1.0, 300.0), None),
            "Rejected: the solved scale 100.00\"/px is outside 0.05..60\"/px"
        );
        assert_eq!(
            text(&mk_result(120, 476, 0.25, 29.6, 1.0, 300.0), Some(1.873)),
            "Rejected: the solved scale 29.60\"/px is more than 8x off the header's 1.87\"/px"
        );
    }

    /// The input gate refuses a frame whose own analysis carries the `trailed` verdict — and refuses nothing else,
    /// whatever the eccentricity / trail R² (the pair was removed, Plan C C7). The numbers are the 2026-09-05 ones:
    /// frames that solve correctly sit at 0.62–0.72 eccentricity with trail R² 0.30–0.57; hopeless ones at
    /// 0.90–0.96 with 0.73–0.93.
    #[test]
    fn input_gate_refuses_trailed_frames_and_passes_the_rest() {
        use crate::db::schema::init_db;
        use crate::models::FrameAnalysis;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let cfg = PlateSolveConfig::default();

        let seed = |fid: i64, ecc: f64, trail: f64, verdict: Option<crate::models::FrameVerdict>| {
            conn.execute(
                "INSERT INTO files (id,path,filename,size,modified_at,format,created_at)
                 VALUES (?1,?2,?3,0,'2025-01-01','FITS','2025-01-01')",
                rusqlite::params![fid, format!("/x/{fid}.fits"), format!("{fid}.fits")],
            )
            .unwrap();
            conn.execute("INSERT INTO frames (id,file_id) VALUES (?1,?1)", [fid]).unwrap();
            let a = FrameAnalysis {
                id: None,
                frame_id: fid,
                file_id: fid,
                stars_detected: 2000,
                median_fwhm: 4.0,
                median_eccentricity: ecc,
                median_snr: 30.0,
                median_hfr: 2.0,
                frame_snr: 30.0,
                snr_weight: 1.0,
                psf_signal: 100.0,
                background: 100.0,
                noise: 5.0,
                detection_threshold: 5.0,
                width: 4144,
                height: 2822,
                source_channels: 1,
                trail_r_squared: trail,
                possibly_trailed: trail > 0.5,
                median_beta: None,
                quality_score: None,
                config_hash: Some("t".into()),
                analyzed_at: "2026-09-05T00:00:00Z".into(),
                measure_version: Some(crate::star_measure::MEASURE_VERSION),
                stars_refused: None,
                frame_verdict: verdict,
                zero_point: None,
                zp_stars: None,
                zp_sigma: None,
                zp_version: None,
                zp_catalog: None,
                zp_refusal: None,
                zp_list_stars: None,
                zp_window_bright: None,
                zp_window_slope: None,
            };
            crate::db::analysis::upsert_frame_analysis(&conn, &a).unwrap();
        };

        seed(1, 0.96, 0.73, Some(crate::models::FrameVerdict::Trailed)); // wind-shaken: refused on the verdict
        seed(2, 0.72, 0.57, None); // trailed but solvable: passed
        seed(3, 0.62, 0.30, None); // the doubled frame, solves in 0.3 s: passed
        seed(4, 0.91, 0.40, None); // elongated optics, not a tracking failure: passed
        seed(5, 0.96, 0.73, None); // the same numbers WITHOUT the verdict: passed (the pair is gone)

        assert!(input_gate_reason(&conn, 1, &cfg).is_some(), "streaks are refused");
        assert!(input_gate_reason(&conn, 5, &cfg).is_none(), "numbers alone never refuse");
        assert!(input_gate_reason(&conn, 2, &cfg).is_none(), "a solvable trail passes");
        assert!(input_gate_reason(&conn, 3, &cfg).is_none(), "the doubled frame passes");
        assert!(
            input_gate_reason(&conn, 4, &cfg).is_none(),
            "elongation without a trail fit is not a tracking failure"
        );
        // A frame nobody analysed is never refused here.
        conn.execute(
            "INSERT INTO files (id,path,filename,size,modified_at,format,created_at)
             VALUES (9,'/x/9.fits','9.fits',0,'2025-01-01','FITS','2025-01-01')",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO frames (id,file_id) VALUES (9,9)", []).unwrap();
        assert!(input_gate_reason(&conn, 9, &cfg).is_none(), "no analysis, no verdict");

        let off = PlateSolveConfig { input_gate_enabled: false, ..cfg.clone() };
        assert!(input_gate_reason(&conn, 1, &off).is_none(), "the gate can be turned off");
    }

    /// One `frame_analysis` row for the gate tests: (eccentricity, trail R², measure version, verdict).
    fn gate_conn(
        rows: &[(f64, f64, Option<u32>, Option<crate::models::FrameVerdict>)],
    ) -> rusqlite::Connection {
        use crate::models::FrameAnalysis;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        for (i, (ecc, trail, version, verdict)) in rows.iter().enumerate() {
            let fid = i as i64 + 1;
            conn.execute(
                "INSERT INTO files (id,path,filename,size,modified_at,format,created_at)
                 VALUES (?1,?2,?3,0,'2025-01-01','FITS','2025-01-01')",
                rusqlite::params![fid, format!("/x/{fid}.fits"), format!("{fid}.fits")],
            )
            .unwrap();
            conn.execute("INSERT INTO frames (id,file_id) VALUES (?1,?1)", [fid]).unwrap();
            let a = FrameAnalysis {
                id: None,
                frame_id: fid,
                file_id: fid,
                stars_detected: 2000,
                median_fwhm: 4.0,
                median_eccentricity: *ecc,
                median_snr: 30.0,
                median_hfr: 2.0,
                frame_snr: 30.0,
                snr_weight: 1.0,
                psf_signal: 100.0,
                background: 100.0,
                noise: 5.0,
                detection_threshold: 5.0,
                width: 4144,
                height: 2822,
                source_channels: 1,
                trail_r_squared: *trail,
                possibly_trailed: false,
                median_beta: None,
                quality_score: None,
                config_hash: Some("t".into()),
                analyzed_at: "2026-09-05T00:00:00Z".into(),
                measure_version: *version,
                stars_refused: None,
                frame_verdict: *verdict,
                zero_point: None,
                zp_stars: None,
                zp_sigma: None,
                zp_version: None,
                zp_catalog: None,
                zp_refusal: None,
                zp_list_stars: None,
                zp_window_bright: None,
                zp_window_slope: None,
            };
            crate::db::analysis::upsert_frame_analysis(&conn, &a).unwrap();
        }
        conn
    }

    #[test]
    fn the_input_gate_ignores_a_legacy_row() {
        let conn = gate_conn(&[(0.95, 0.9, None, None)]);
        assert!(input_gate_reason(&conn, 1, &PlateSolveConfig::default()).is_none());
    }

    #[test]
    fn the_input_gate_ignores_a_row_of_another_version() {
        let cur = crate::star_measure::MEASURE_VERSION;
        let conn = gate_conn(&[(0.95, 0.9, Some(cur + 1), Some(crate::models::FrameVerdict::Trailed))]);
        assert!(input_gate_reason(&conn, 1, &PlateSolveConfig::default()).is_none());
    }

    #[test]
    fn the_input_gate_refuses_a_trailed_verdict() {
        use crate::models::FrameVerdict;
        let cur = Some(crate::star_measure::MEASURE_VERSION);
        let conn = gate_conn(&[(0.0, 0.0, cur, Some(FrameVerdict::Trailed))]);
        let why = input_gate_reason(&conn, 1, &PlateSolveConfig::default()).expect("refused on the verdict alone");
        assert!(why.contains("trails the measurement could not fit"), "{why}");
    }

    #[test]
    fn the_input_gate_passes_an_elongated_measured_frame() {
        let cur = Some(crate::star_measure::MEASURE_VERSION);
        let conn = gate_conn(&[(0.73, 0.57, cur, None)]);
        assert!(input_gate_reason(&conn, 1, &PlateSolveConfig::default()).is_none());
    }

    #[test]
    fn the_input_gate_never_refuses_defocused_or_no_stars() {
        use crate::models::FrameVerdict;
        let cur = Some(crate::star_measure::MEASURE_VERSION);
        // The rows carry the old pair's hopeless numbers: the verdict alone decides, and these two never refuse.
        let conn = gate_conn(&[
            (0.95, 0.9, cur, Some(FrameVerdict::Defocused)),
            (0.95, 0.9, cur, Some(FrameVerdict::NoStars)),
        ]);
        let cfg = PlateSolveConfig::default();
        assert!(input_gate_reason(&conn, 1, &cfg).is_none());
        assert!(input_gate_reason(&conn, 2, &cfg).is_none());
    }

    /// A solve whose recovered scale contradicts the frame's own optics is a
    /// false positive, however confident it looks. Measured on wind-shaken
    /// frames (2026-09-05): the solver walked its autoFOV ladder and "solved"
    /// them at 16–193× the header's pixel scale, with a small pixel RMS and
    /// an inlier ratio that cleared every other bar — and that astrometry
    /// would have been written into the catalog. `blind_scale_header_tol`
    /// already existed to catch exactly this; `store_result` simply never
    /// gave it the header's scale to compare against.
    #[test]
    fn store_result_refuses_a_solve_that_contradicts_the_headers_optics() {
        use crate::db::schema::init_db;
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let cfg = PlateSolveConfig::default();
        // 414 mm focal length, 3.76 µm pixels → 1.873 "/px.
        for fid in [1i64, 2] {
            conn.execute(
                "INSERT INTO files (id,path,filename,size,modified_at,format,created_at)
                 VALUES (?1,?2,?3,0,'2025-01-01','FITS','2025-01-01')",
                rusqlite::params![fid, format!("/x/{fid}.fits"), format!("{fid}.fits")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id,file_id,focallen,xpixsz) VALUES (?1,?1,414.0,3.76)",
                [fid],
            )
            .unwrap();
        }

        // 29.6 "/px against the header's 1.873 — the real shape of the false
        // solutions, confident-looking in every other respect.
        let out = store_result(&conn, 1, &mk_result(120, 800, 0.15, 29.6, 1.0, 300.0), None, &cfg).unwrap();
        assert!(
            !matches!(out, StoreOutcome::Persisted),
            "a solve 16x off the header's scale must not be stored, got {out:?}"
        );
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM plate_solves WHERE frame_id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0, "and it must not leave a plate_solves row");

        // The same confidence at the header's own scale still persists — the
        // tolerance is deliberately generous, this is not a scale gate.
        let ok = store_result(&conn, 2, &mk_result(120, 800, 0.15, 1.9, 1.0, 300.0), None, &cfg).unwrap();
        assert!(matches!(ok, StoreOutcome::Persisted), "a scale that agrees is stored");
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
            store_result(&conn, 1, &mk_result(120, 800, 0.15, 1.5, 1.0, 300.0), None, &cfg).unwrap();
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
            &mk_result(90, 150_000, 0.0006, 22.0, 2.8, 2.0),
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
        let defocused = mk_result(10, 800, 0.070, 0.48, 2.35, 40.0);

        // Default gate (floor now 6): the correct-but-sparse solve PERSISTS.
        let out = store_result(&conn, 1, &defocused, None, &PlateSolveConfig::default()).unwrap();
        assert!(
            matches!(out, StoreOutcome::Persisted),
            "defocused-but-correct solve (10 inliers, ratio 0.070) must persist"
        );
        let ps: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM plate_solves WHERE frame_id=1",
                [],
                |r| r.get(0),
            )
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

    /// Plan 6b-2 C14: the extracted gate is `store_result`'s own. It refuses
    /// a solve 16x off the expected scale, passes the same confidence at that
    /// scale, has nothing to compare without an expected scale, and refuses
    /// nothing with the gate off.
    #[test]
    fn acceptance_refusal_is_store_results_gate() {
        let cfg = PlateSolveConfig::default();
        let wrong = mk_result(120, 800, 0.15, 29.6, 1.0, 300.0);
        assert_eq!(
            acceptance_refusal(&wrong, Some(1.873), &cfg).as_deref(),
            Some("Rejected: the solved scale 29.60\"/px is more than 8x off the header's 1.87\"/px")
        );
        assert_eq!(
            acceptance_refusal(&mk_result(120, 800, 0.15, 1.9, 1.0, 300.0), Some(1.873), &cfg),
            None
        );
        assert_eq!(acceptance_refusal(&wrong, None, &cfg), None);
        let off = PlateSolveConfig {
            blind_gate_enabled: false,
            ..PlateSolveConfig::default()
        };
        assert_eq!(acceptance_refusal(&wrong, Some(1.873), &off), None);
    }

    /// Plan 6b-2 C14: the row a solve becomes, the same for `store_result`
    /// and for a stacking run's cached solve.
    #[test]
    fn record_from_result_is_the_stored_row() {
        let r = record_from_result(7, &mk_result(120, 476, 0.25, 1.9, 1.0, 300.0));
        assert_eq!((r.id, r.frame_id), (None, 7));
        assert_eq!((r.crval1, r.crval2), (123.45, 67.89));
        assert_eq!((r.cd1_1, r.cd1_2, r.cd2_1, r.cd2_2), (1e-4, 0.0, 0.0, 1e-4));
        assert_eq!((r.matched_stars, r.total_detected), (120, 600));
        assert_eq!(
            (r.expected_catalog_stars_in_fov, r.inlier_ratio, r.log10_odds),
            (Some(476), Some(0.25), Some(300.0))
        );
        assert_eq!(
            (r.catalog_used.as_str(), r.algorithm_used.as_str()),
            ("smac_gaia", "blind_index")
        );
        assert_eq!(r.pixel_scale_arcsec, 1.9);
    }
}

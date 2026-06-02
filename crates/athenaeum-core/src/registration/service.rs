//! Frame-set registration service.
//!
//! Orchestrates star detection, reference selection, reference solve, and
//! per-member alignment for a single frame set.  Results are persisted to the
//! `registration_results` table; no other table is touched.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{bail, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use astroimage::ImageAnalyzer;
use solvemyastro::{Caches, CentroidRefinement, SolveConfig, StarCache};

use crate::events::{emit_event, ProgressEmitter};
use crate::models::Frame;
use crate::plate_solve::config::PlateSolveConfig;
use crate::plate_solve::hints::{extract_hints, observation_epoch};

use super::db::{
    clear_registration_for_frame_set, get_frame_set_reference, get_light_frame_ids_for_frame_set,
    upsert_registration, RegistrationRecord,
};
use super::reference::select_reference;

// ── progress event shapes ─────────────────────────────────────────────────────

/// Per-frame progress event emitted on the `stacking-prep-progress` channel.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StackingPrepProgressEvent {
    frame_id: i64,
    current: usize,
    total: usize,
    status: String,
    matched_stars: Option<usize>,
    rms_px: Option<f64>,
    error: Option<String>,
    filename: Option<String>,
}

/// Summary event emitted once on the `stacking-prep-complete` channel.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StackingPrepCompleteEvent {
    reference_frame_id: i64,
    aligned: usize,
    failed: usize,
    total: usize,
}

// ── public summary ────────────────────────────────────────────────────────────

/// Summary returned by [`register_frame_set`].
#[derive(Clone, Debug)]
pub struct RegistrationSummary {
    pub reference_frame_id: i64,
    pub aligned: usize,
    pub failed: usize,
    pub total: usize,
}

// ── internal per-frame helper types ──────────────────────────────────────────

struct MemberInfo {
    frame_id: i64,
    frame: Frame,
    path: String,
    /// (x, y) centroids — filled during the detection phase.
    detections: Vec<(f64, f64)>,
    detection_count: usize,
}

// ── main entry point ─────────────────────────────────────────────────────────

/// Register all LIGHT members of `frames_set_id` to each other.
///
/// # Steps
///
/// 1. Load LIGHT frame IDs and their metadata from the DB.
/// 2. Detect stars in each member (ImageAnalyzer::detect_fast with
///    centroid_refine = true; capped at 400 stars). Honours `cancel`.
/// 3. Select the reference frame (most detections, tie-break smallest frame_id,
///    or `override_reference_id` when supplied and valid).
/// 4. Precise-solve the reference frame (centroid_refinement = Auto).
///    Retry up to 3 distinct candidate frames if the first fails.
/// 5. For each non-reference member: call `solvemyastro::register` and persist
///    the result. Honours `cancel`.
/// 6. Return a [`RegistrationSummary`].
///
/// Progress is broadcast on `stacking-prep-progress` (per frame) and
/// `stacking-prep-complete` (once at the end).
pub fn register_frame_set(
    conn: &Connection,
    frames_set_id: i64,
    override_reference_id: Option<i64>,
    cache: &StarCache,
    bright_cache: Option<&StarCache>,
    ps_config: &PlateSolveConfig,
    emitter: &dyn ProgressEmitter,
    cancel: Option<&AtomicBool>,
) -> Result<RegistrationSummary> {
    // ── Step 1: load LIGHT members ────────────────────────────────────────────
    let frame_ids = get_light_frame_ids_for_frame_set(conn, frames_set_id)?;
    if frame_ids.is_empty() {
        bail!("Frame set {frames_set_id} has no LIGHT members — cannot register");
    }

    let total = frame_ids.len();
    eprintln!(
        "registration: frame set {frames_set_id}: {total} LIGHT members to register"
    );

    // Load frame metadata + file path for every member.
    let mut members: Vec<MemberInfo> = Vec::with_capacity(total);
    for &fid in &frame_ids {
        match load_frame_with_path(conn, fid) {
            Ok((frame, path)) => members.push(MemberInfo {
                frame_id: fid,
                frame,
                path,
                detections: Vec::new(),
                detection_count: 0,
            }),
            Err(e) => {
                eprintln!(
                    "registration: failed to load frame {fid} — skipping: {e}"
                );
            }
        }
    }

    if members.is_empty() {
        bail!("No loadable LIGHT frames in frame set {frames_set_id}");
    }

    // ── Step 2: star detection ────────────────────────────────────────────────
    const MAX_STARS: usize = 400;
    let analyzer = ImageAnalyzer::new()
        .with_max_stars(MAX_STARS)
        .with_detection_sigma(5.0)
        .with_centroid_refine(true);

    for (idx, member) in members.iter_mut().enumerate() {
        if cancel.map(|c| c.load(Ordering::Relaxed)).unwrap_or(false) {
            bail!("Registration cancelled during star detection");
        }

        emit_event(
            emitter,
            "stacking-prep-progress",
            &StackingPrepProgressEvent {
                frame_id: member.frame_id,
                current: idx,
                total,
                status: "detecting".to_string(),
                matched_stars: None,
                rms_px: None,
                error: None,
                filename: filename_from_path(&member.path),
            },
        );

        match analyzer.detect_fast(&member.path) {
            Ok(result) => {
                member.detections = result.stars.iter().map(|s| (s.x as f64, s.y as f64)).collect();
                member.detection_count = member.detections.len();
                eprintln!(
                    "registration: frame {} — {} detections",
                    member.frame_id, member.detection_count
                );
            }
            Err(e) => {
                eprintln!(
                    "registration: detect_fast failed for frame {}: {e}",
                    member.frame_id
                );
                // Zero detections; the reference selection will deprioritise this frame.
            }
        }
    }

    // ── Step 3: select reference ──────────────────────────────────────────────
    //
    // Priority order:
    //   1. Caller-supplied `override_reference_id` (programmatic override, e.g.
    //      a retry after a bad solve).
    //   2. User-persisted choice in `frame_set_reference` (set via the UI).
    //   3. Auto-pick: frame with the most star detections (tie-break: smallest
    //      frame_id).
    let effective_override = if override_reference_id.is_some() {
        override_reference_id
    } else {
        // Consult the persisted user preference. Log but do not fail if the DB
        // read itself errors — fall back gracefully to auto-pick.
        match get_frame_set_reference(conn, frames_set_id) {
            Ok(Some(r)) => {
                eprintln!(
                    "registration: using persisted user reference frame {}",
                    r.reference_frame_id
                );
                Some(r.reference_frame_id)
            }
            Ok(None) => None,
            Err(e) => {
                eprintln!(
                    "registration: failed to read frame_set_reference (falling back to auto): {e}"
                );
                None
            }
        }
    };

    let count_pairs: Vec<(i64, usize)> = members
        .iter()
        .map(|m| (m.frame_id, m.detection_count))
        .collect();
    let ref_id = select_reference(&count_pairs, effective_override);
    eprintln!("registration: reference frame selected: {ref_id}");

    // Build a ranked list of candidates to try as reference (reference first,
    // then others descending by detection count).
    let mut solve_candidates: Vec<usize> = {
        // Index of the chosen reference first.
        let ref_idx = members
            .iter()
            .position(|m| m.frame_id == ref_id)
            .unwrap_or(0);
        let mut rest: Vec<usize> = (0..members.len()).filter(|&i| i != ref_idx).collect();
        // Sort rest by descending detection count.
        rest.sort_by(|&a, &b| {
            members[b]
                .detection_count
                .cmp(&members[a].detection_count)
        });
        let mut candidates = vec![ref_idx];
        candidates.extend_from_slice(&rest);
        candidates
    };

    // ── Step 4: precise-solve the reference (up to 3 attempts) ───────────────
    let sma_cfg = SolveConfig {
        quad_tolerance: 0.007,
        catalog_mag_limit: 19.0,
        fit_sip: true,
        sip_order: ps_config.sip_order,
        print_timing: false,
        centroid_refinement: CentroidRefinement::Auto,
        ..SolveConfig::default()
    };

    let caches = match bright_cache {
        Some(b) => Caches::tiered(cache, b),
        None => Caches::deep_only(cache),
    };

    const MAX_REF_TRIES: usize = 3;
    let mut ref_solve: Option<(i64, solvemyastro::WcsSolution, f64, Vec<(f64, f64)>)> = None;

    for &candidate_idx in solve_candidates.iter().take(MAX_REF_TRIES) {
        if cancel.map(|c| c.load(Ordering::Relaxed)).unwrap_or(false) {
            bail!("Registration cancelled before reference solve");
        }
        let m = &members[candidate_idx];
        let hints = build_sma_hints(&m.frame);
        let t0 = Instant::now();
        match solvemyastro::solve(&std::path::Path::new(&m.path), &hints, &caches, &sma_cfg, cancel) {
            Ok(solution) => {
                let elapsed_ms = t0.elapsed().as_millis() as i64;
                eprintln!(
                    "registration: reference solve succeeded for frame {} \
                     ({} matched, rms={:.2}px, t={}ms)",
                    m.frame_id, solution.matched_stars, solution.rms_residual_px, elapsed_ms
                );
                let pixel_scale = solution.pixel_scale_arcsec;
                let ref_detections: Vec<(f64, f64)> = m.detections.clone();

                // Persist the reference row.
                let registered_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let rec = RegistrationRecord {
                    id: None,
                    frames_set_id,
                    frame_id: m.frame_id,
                    reference_frame_id: m.frame_id,
                    is_reference: true,
                    crpix1: Some(solution.wcs.crpix.0),
                    crpix2: Some(solution.wcs.crpix.1),
                    crval1: Some(solution.wcs.crval.0),
                    crval2: Some(solution.wcs.crval.1),
                    cd1_1: Some(solution.wcs.cd[0][0]),
                    cd1_2: Some(solution.wcs.cd[0][1]),
                    cd2_1: Some(solution.wcs.cd[1][0]),
                    cd2_2: Some(solution.wcs.cd[1][1]),
                    // Identity affine for the reference (sub == ref).
                    affine_a1: Some(1.0),
                    affine_b1: Some(0.0),
                    affine_c1: Some(0.0),
                    affine_a2: Some(0.0),
                    affine_b2: Some(1.0),
                    affine_c2: Some(0.0),
                    matched_stars: solution.matched_stars as i64,
                    rms_residual_px: solution.rms_residual_px,
                    rms_residual_arcsec: Some(solution.rms_residual_arcsec),
                    status: "reference".to_string(),
                    error: None,
                    compute_time_ms: elapsed_ms,
                    registered_at: registered_at.clone(),
                };
                upsert_registration(conn, &rec)?;

                emit_event(
                    emitter,
                    "stacking-prep-progress",
                    &StackingPrepProgressEvent {
                        frame_id: m.frame_id,
                        current: 1,
                        total,
                        status: "reference".to_string(),
                        matched_stars: Some(solution.matched_stars),
                        rms_px: Some(solution.rms_residual_px),
                        error: None,
                        filename: filename_from_path(&m.path),
                    },
                );

                ref_solve = Some((m.frame_id, solution.wcs, pixel_scale, ref_detections));
                // Rearrange solve_candidates so the winning reference is first.
                let winning_pos = solve_candidates.iter().position(|&i| i == candidate_idx).unwrap();
                solve_candidates.swap(0, winning_pos);
                break;
            }
            Err(e) => {
                eprintln!(
                    "registration: reference solve failed for frame {} (attempt): {e}",
                    m.frame_id
                );
            }
        }
    }

    let (actual_ref_id, ref_wcs, ref_pixel_scale, ref_detections) = match ref_solve {
        Some(v) => v,
        None => {
            bail!(
                "registration: failed to solve any reference candidate after {} tries",
                MAX_REF_TRIES
            );
        }
    };

    // Clear stale rows from any previous run now that we know the registration
    // will proceed. This is done after the reference solve so an early cancel
    // does not wipe previous results without replacing them.
    clear_registration_for_frame_set(conn, frames_set_id)?;
    // Re-persist the reference row we just computed.
    let registered_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    {
        // Find the MemberInfo for actual_ref_id.
        let m = members.iter().find(|m| m.frame_id == actual_ref_id).unwrap();
        // We already persisted above; but clear_registration just deleted it.
        // Reconstruct and re-upsert.
        upsert_registration(
            conn,
            &RegistrationRecord {
                id: None,
                frames_set_id,
                frame_id: actual_ref_id,
                reference_frame_id: actual_ref_id,
                is_reference: true,
                crpix1: Some(ref_wcs.crpix.0),
                crpix2: Some(ref_wcs.crpix.1),
                crval1: Some(ref_wcs.crval.0),
                crval2: Some(ref_wcs.crval.1),
                cd1_1: Some(ref_wcs.cd[0][0]),
                cd1_2: Some(ref_wcs.cd[0][1]),
                cd2_1: Some(ref_wcs.cd[1][0]),
                cd2_2: Some(ref_wcs.cd[1][1]),
                affine_a1: Some(1.0),
                affine_b1: Some(0.0),
                affine_c1: Some(0.0),
                affine_a2: Some(0.0),
                affine_b2: Some(1.0),
                affine_c2: Some(0.0),
                matched_stars: 0, // not tracked here; already logged above
                rms_residual_px: 0.0,
                rms_residual_arcsec: None,
                status: "reference".to_string(),
                error: None,
                compute_time_ms: 0,
                registered_at: registered_at.clone(),
            },
        )?;
        let _ = m; // silence unused warning
    }

    // ── Step 5: align non-reference members ───────────────────────────────────
    let mut aligned = 0usize;
    let mut failed = 0usize;
    let non_ref_indices: Vec<usize> = (0..members.len())
        .filter(|&i| members[i].frame_id != actual_ref_id)
        .collect();
    let non_ref_total = non_ref_indices.len();

    for (sub_pos, &idx) in non_ref_indices.iter().enumerate() {
        if cancel.map(|c| c.load(Ordering::Relaxed)).unwrap_or(false) {
            bail!("Registration cancelled during sub-frame alignment");
        }

        let m = &members[idx];
        let progress_current = sub_pos + 2; // 1=reference, 2..=N=subs

        emit_event(
            emitter,
            "stacking-prep-progress",
            &StackingPrepProgressEvent {
                frame_id: m.frame_id,
                current: progress_current,
                total,
                status: "aligning".to_string(),
                matched_stars: None,
                rms_px: None,
                error: None,
                filename: filename_from_path(&m.path),
            },
        );

        let t0 = Instant::now();
        match solvemyastro::register(&ref_wcs, &ref_detections, &m.detections, &sma_cfg) {
            Ok(reg) => {
                let elapsed_ms = t0.elapsed().as_millis() as i64;
                let rms_arcsec = if ref_pixel_scale > 0.0 {
                    Some(reg.rms_px * ref_pixel_scale)
                } else {
                    None
                };

                let rec = RegistrationRecord {
                    id: None,
                    frames_set_id,
                    frame_id: m.frame_id,
                    reference_frame_id: actual_ref_id,
                    is_reference: false,
                    crpix1: Some(reg.refined_wcs.crpix.0),
                    crpix2: Some(reg.refined_wcs.crpix.1),
                    crval1: Some(reg.refined_wcs.crval.0),
                    crval2: Some(reg.refined_wcs.crval.1),
                    cd1_1: Some(reg.refined_wcs.cd[0][0]),
                    cd1_2: Some(reg.refined_wcs.cd[0][1]),
                    cd2_1: Some(reg.refined_wcs.cd[1][0]),
                    cd2_2: Some(reg.refined_wcs.cd[1][1]),
                    affine_a1: Some(reg.transform.a1),
                    affine_b1: Some(reg.transform.b1),
                    affine_c1: Some(reg.transform.c1),
                    affine_a2: Some(reg.transform.a2),
                    affine_b2: Some(reg.transform.b2),
                    affine_c2: Some(reg.transform.c2),
                    matched_stars: reg.matched as i64,
                    rms_residual_px: reg.rms_px,
                    rms_residual_arcsec: rms_arcsec,
                    status: "aligned".to_string(),
                    error: None,
                    compute_time_ms: elapsed_ms,
                    registered_at: registered_at.clone(),
                };
                upsert_registration(conn, &rec)?;
                aligned += 1;

                emit_event(
                    emitter,
                    "stacking-prep-progress",
                    &StackingPrepProgressEvent {
                        frame_id: m.frame_id,
                        current: progress_current,
                        total,
                        status: "aligned".to_string(),
                        matched_stars: Some(reg.matched),
                        rms_px: Some(reg.rms_px),
                        error: None,
                        filename: filename_from_path(&m.path),
                    },
                );
            }
            Err(e) => {
                let elapsed_ms = t0.elapsed().as_millis() as i64;
                eprintln!(
                    "registration: alignment failed for frame {}: {e}",
                    m.frame_id
                );

                let rec = RegistrationRecord {
                    id: None,
                    frames_set_id,
                    frame_id: m.frame_id,
                    reference_frame_id: actual_ref_id,
                    is_reference: false,
                    crpix1: None,
                    crpix2: None,
                    crval1: None,
                    crval2: None,
                    cd1_1: None,
                    cd1_2: None,
                    cd2_1: None,
                    cd2_2: None,
                    affine_a1: None,
                    affine_b1: None,
                    affine_c1: None,
                    affine_a2: None,
                    affine_b2: None,
                    affine_c2: None,
                    matched_stars: 0,
                    rms_residual_px: 0.0,
                    rms_residual_arcsec: None,
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                    compute_time_ms: elapsed_ms,
                    registered_at: registered_at.clone(),
                };
                upsert_registration(conn, &rec)?;
                failed += 1;

                emit_event(
                    emitter,
                    "stacking-prep-progress",
                    &StackingPrepProgressEvent {
                        frame_id: m.frame_id,
                        current: progress_current,
                        total,
                        status: "failed".to_string(),
                        matched_stars: None,
                        rms_px: None,
                        error: Some(e.to_string()),
                        filename: filename_from_path(&m.path),
                    },
                );
            }
        }

        let _ = non_ref_total; // suppress dead-code lint
    }

    eprintln!(
        "registration: frame set {frames_set_id} done — \
         aligned={aligned}, failed={failed}, total={total}"
    );

    emit_event(
        emitter,
        "stacking-prep-complete",
        &StackingPrepCompleteEvent {
            reference_frame_id: actual_ref_id,
            aligned,
            failed,
            total,
        },
    );

    Ok(RegistrationSummary {
        reference_frame_id: actual_ref_id,
        aligned,
        failed,
        total,
    })
}

// ── private helpers ───────────────────────────────────────────────────────────

/// Build a `solvemyastro::SolveHints` from a frame's metadata.
///
/// Mirrors the mapping in `plate_solve::service::solve_frame_with_hints` but
/// uses only the frame fields (no DB round-trip — this is called from a
/// worker context where the connection is not available).
fn build_sma_hints(frame: &Frame) -> solvemyastro::SolveHints {
    let athena_hints = extract_hints(frame, None);
    solvemyastro::SolveHints {
        ra: athena_hints.ra,
        dec: athena_hints.dec,
        fov_deg: athena_hints.fov_deg,
        pixel_scale_arcsec: athena_hints.pixel_scale_arcsec,
        search_radius_deg: None,
        epoch: Some(observation_epoch(frame)),
    }
}

/// Extract the filename component from a file path for progress messages.
fn filename_from_path(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Load a `Frame` and its on-disk path from the DB for a given `frame_id`.
///
/// Mirrors the helper used by `plate_solve::commands::plate_solve.rs`.
fn load_frame_with_path(
    conn: &Connection,
    frame_id: i64,
) -> Result<(Frame, String)> {
    let mut stmt = conn
        .prepare(
            "SELECT f.*, fl.path
             FROM frames f
             JOIN files fl ON fl.id = f.file_id
             WHERE f.id = ?1",
        )
        .map_err(|e| anyhow::anyhow!("prepare: {e}"))?;

    stmt.query_row([frame_id], |row| {
        let frame = Frame {
            id: row.get("id")?,
            file_id: row.get("file_id")?,
            object: row.get("object")?,
            date_obs: row
                .get::<_, Option<String>>("date_obs")
                .ok()
                .flatten()
                .and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .or_else(|| {
                            chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S")
                                .ok()
                                .map(|ndt| ndt.and_utc().fixed_offset())
                        })
                        .map(|dt| dt.with_timezone(&chrono::Utc))
                }),
            telescop: row.get("telescop")?,
            instrume: row.get("instrume")?,
            exptime: row.get("exptime")?,
            filter: row.get("filter")?,
            imagetyp: None,
            is_master: row.get::<_, i32>("is_master").unwrap_or(0) != 0,
            gain: row.get("gain")?,
            offset: row.get("offset")?,
            binning: row.get("binning")?,
            xbinning: row.get("xbinning")?,
            ybinning: row.get("ybinning")?,
            ccd_temp: row.get("ccd_temp")?,
            set_temp: row.get("set_temp")?,
            focallen: row.get("focallen")?,
            xpixsz: row.get("xpixsz")?,
            ypixsz: row.get("ypixsz")?,
            naxis1: row.get("naxis1")?,
            naxis2: row.get("naxis2")?,
            ra: row.get("ra")?,
            dec: row.get("dec")?,
            sitelat: row.get("sitelat")?,
            lat_obs: row.get("lat_obs")?,
            sitelong: row.get("sitelong")?,
            long_obs: row.get("long_obs")?,
            objctra: row.get("objctra")?,
            objctdec: row.get("objctdec")?,
            override_: row.get::<_, i32>("override").unwrap_or(0) != 0,
            swcreate: row.get("swcreate")?,
            bayerpat: row.get("bayerpat")?,
            rotation: row.get("rotation")?,
        };
        let path: String = row.get("path")?;
        Ok((frame, path))
    })
    .map_err(|e| anyhow::anyhow!("Frame {frame_id} not found: {e}"))
}

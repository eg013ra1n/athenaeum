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

use crate::db::load_frame_with_path;
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
///
/// `pub(crate)` (not module-private) so `ts_export.rs` can reference the type
/// path from outside this module — the ts-rs harness lives in the same crate,
/// so crate-visibility is sufficient; no need to expose it outside the crate.
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StackingPrepProgressEvent {
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
///
/// `pub(crate)` for the same reason as `StackingPrepProgressEvent` above.
#[derive(Clone, Debug, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StackingPrepCompleteEvent {
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
/// 2. Pre-registration consistency gate (R2): reject the run if members mix
///    binning or focal length beyond tolerance — see
///    [`check_registration_consistency`]. Runs before any per-frame work.
/// 3. Detect stars in each member (ImageAnalyzer::detect_fast with
///    centroid_refine = true; capped at 400 stars). Honours `cancel`.
/// 4. Select the reference frame (most detections, tie-break smallest frame_id,
///    or `override_reference_id` when supplied and valid).
/// 5. Precise-solve the reference frame (centroid_refinement = Auto).
///    Retry up to 3 distinct candidate frames if the first fails.
/// 6. For each non-reference member: call `solvemyastro::register` and persist
///    the result. Honours `cancel`.
/// 7. Return a [`RegistrationSummary`].
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
    // Sync fn, called via `spawn_blocking` from both hosts (see
    // `commands/registration.rs` / `routes/registration.rs`) — a sync
    // `enter()` guard is correct here (no `.await` anywhere in this function
    // body). Both hosts' spawn_blocking closures inherit this span without
    // either needing to open it themselves.
    let span = tracing::info_span!("registration", frame_set_id = frames_set_id);
    let _g = span.enter();

    // ── Step 1: load LIGHT members ────────────────────────────────────────────
    let frame_ids = get_light_frame_ids_for_frame_set(conn, frames_set_id)?;
    if frame_ids.is_empty() {
        bail!("Frame set {frames_set_id} has no LIGHT members — cannot register");
    }

    let total = frame_ids.len();
    tracing::info!(frame_set_id = frames_set_id, total, "registration starting");

    // Load frame metadata + file path for every member.
    let mut members: Vec<MemberInfo> = Vec::with_capacity(total);
    for &fid in &frame_ids {
        match load_frame_with_path(conn, fid) {
            Ok(Some((frame, path))) => members.push(MemberInfo {
                frame_id: fid,
                frame,
                path,
                detections: Vec::new(),
                detection_count: 0,
            }),
            Ok(None) => {
                tracing::warn!(frame_id = fid, "frame not found, skipping");
            }
            Err(e) => {
                tracing::warn!(frame_id = fid, error = %e, "failed to load frame, skipping");
            }
        }
    }

    if members.is_empty() {
        bail!("No loadable LIGHT frames in frame set {frames_set_id}");
    }

    // ── Step 2: pre-registration consistency gate (R2) ───────────────────────
    //
    // Runs before star detection / solving so a mismatched set fails fast
    // without spending any per-frame compute on a run that cannot register
    // correctly.
    if let Err(e) = check_registration_consistency(&members) {
        tracing::error!(frame_set_id = frames_set_id, error = %e, "registration consistency gate failed");
        emit_event(
            emitter,
            "stacking-prep-progress",
            &StackingPrepProgressEvent {
                frame_id: members[0].frame_id,
                current: 0,
                total,
                status: "failed".to_string(),
                matched_stars: None,
                rms_px: None,
                error: Some(e.to_string()),
                filename: filename_from_path(&members[0].path),
            },
        );
        return Err(e);
    }

    // ── Step 3: star detection ────────────────────────────────────────────────
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
                member.detections = result
                    .stars
                    .iter()
                    .map(|s| (s.x as f64, s.y as f64))
                    .collect();
                member.detection_count = member.detections.len();
                tracing::debug!(frame_id = member.frame_id, detections = member.detection_count, "star detection complete");
            }
            Err(e) => {
                tracing::warn!(frame_id = member.frame_id, error = %e, "star detection failed, frame deprioritised for reference selection");
                // Zero detections; the reference selection will deprioritise this frame.
            }
        }
    }

    // ── Step 4: select reference ──────────────────────────────────────────────
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
                tracing::debug!(frame_id = r.reference_frame_id, "using persisted user reference frame");
                Some(r.reference_frame_id)
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, "failed to read frame_set_reference, falling back to auto-pick");
                None
            }
        }
    };

    let count_pairs: Vec<(i64, usize)> = members
        .iter()
        .map(|m| (m.frame_id, m.detection_count))
        .collect();
    let ref_id = select_reference(&count_pairs, effective_override);
    tracing::info!(frame_id = ref_id, "reference frame selected");

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
        rest.sort_by(|&a, &b| members[b].detection_count.cmp(&members[a].detection_count));
        let mut candidates = vec![ref_idx];
        candidates.extend_from_slice(&rest);
        candidates
    };

    // ── Step 5: precise-solve the reference (up to 3 attempts) ───────────────
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
        match solvemyastro::solve(
            &std::path::Path::new(&m.path),
            &hints,
            &caches,
            &sma_cfg,
            cancel,
        ) {
            Ok(solution) => {
                let elapsed_ms = t0.elapsed().as_millis() as i64;
                tracing::info!(
                    frame_id = m.frame_id,
                    stage = "reference",
                    matched_stars = solution.matched_stars,
                    rms_px = solution.rms_residual_px,
                    duration_ms = elapsed_ms,
                    outcome = "ok",
                    "reference solve succeeded"
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
                let winning_pos = solve_candidates
                    .iter()
                    .position(|&i| i == candidate_idx)
                    .unwrap();
                solve_candidates.swap(0, winning_pos);
                break;
            }
            Err(e) => {
                tracing::warn!(
                    frame_id = m.frame_id,
                    stage = "reference",
                    error = %e,
                    "reference solve attempt failed, trying next candidate"
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
        let m = members
            .iter()
            .find(|m| m.frame_id == actual_ref_id)
            .unwrap();
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

    // ── Step 6: align non-reference members ───────────────────────────────────
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
                let status = aligned_status(reg.flipped);
                if reg.flipped {
                    tracing::debug!(
                        frame_id = m.frame_id,
                        filename = %filename_from_path(&m.path).unwrap_or_else(|| "<unknown>".to_string()),
                        status,
                        "frame is meridian-flipped (fitted transform includes a reflection)"
                    );
                }

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
                    status: status.to_string(),
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
                        status: status.to_string(),
                        matched_stars: Some(reg.matched),
                        rms_px: Some(reg.rms_px),
                        error: None,
                        filename: filename_from_path(&m.path),
                    },
                );
            }
            Err(e) => {
                let elapsed_ms = t0.elapsed().as_millis() as i64;
                tracing::warn!(
                    frame_id = m.frame_id,
                    stage = "align",
                    error = %e,
                    "frame alignment failed"
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

    tracing::info!(
        frame_set_id = frames_set_id,
        aligned,
        failed,
        total,
        "registration finished"
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

/// Map a completed alignment's `flipped` flag to the persisted/live status
/// string.
///
/// `"aligned_flipped"` stays inside the existing `status` vocabulary (an
/// aligned variant, not a new column/state) — the registration is still
/// valid; only the fitted transform's determinant is negative (a reflection,
/// i.e. a meridian-flipped sub). Consumers (UI, stacking) treat it as aligned
/// but know to mirror the frame at resample time.
fn aligned_status(flipped: bool) -> &'static str {
    if flipped {
        "aligned_flipped"
    } else {
        "aligned"
    }
}

// ── pre-registration consistency gate (R2) ───────────────────────────────────
//
// `register_frame_set`'s WCS composition (`CD' = CD_ref · M`, around
// `ref_pixel_scale` in Step 5) assumes every sub shares the reference pixel
// scale. Mixed binning (e.g. 1×1 + 2×2) in one frame set silently composes a
// scale-wrong WCS with no warning. This gate rejects such a set before any
// per-frame work (star detection, solving) is spent on a run that cannot
// register correctly.
//
// Deliberately NOT auto-rescaling the minority group — that is stacking
// Phase B territory; the fix here is "detect and refuse", not "correct".

/// Pure pre-registration consistency check: every member must share the same
/// `(xbinning, ybinning)` and the same focal length (±1% relative tolerance)
/// as every other member.
///
/// NULL `xbinning`/`ybinning` is treated as 1 (unbinned), logged per member.
/// NULL `focallen` is excluded from the focal-length check entirely (also
/// logged) — it neither joins nor breaks a group.
///
/// No DB, no file I/O — operates purely on the already-loaded `members`
/// slice, so it is unit-testable with fake `Frame`s.
fn check_registration_consistency(members: &[MemberInfo]) -> Result<()> {
    check_binning_consistency(members)?;
    check_focallen_consistency(members)?;
    Ok(())
}

/// One `(key, member-ids)` group discovered while scanning for a mismatch,
/// plus a single example filename for logging.
struct MismatchGroup<K> {
    key: K,
    frame_ids: Vec<i64>,
    example_filename: Option<String>,
}

/// Join formatted `"key (N frames)"` parts into a human sentence:
/// `"a and b"` for two groups, `"a, b, and c"` for three or more.
fn join_group_parts(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [only] => only.clone(),
        [a, b] => format!("{a} and {b}"),
        _ => {
            let (last, rest) = parts.split_last().expect("parts is non-empty");
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

fn check_binning_consistency(members: &[MemberInfo]) -> Result<()> {
    let mut groups: Vec<MismatchGroup<(i32, i32)>> = Vec::new();

    for m in members {
        let x = m.frame.xbinning.unwrap_or_else(|| {
            tracing::debug!(frame_id = m.frame_id, "NULL xbinning, assuming 1 (unbinned)");
            1
        });
        let y = m.frame.ybinning.unwrap_or_else(|| {
            tracing::debug!(frame_id = m.frame_id, "NULL ybinning, assuming 1 (unbinned)");
            1
        });
        let key = (x, y);

        match groups.iter_mut().find(|g| g.key == key) {
            Some(g) => g.frame_ids.push(m.frame_id),
            None => groups.push(MismatchGroup {
                key,
                frame_ids: vec![m.frame_id],
                example_filename: filename_from_path(&m.path),
            }),
        }
    }

    if groups.len() <= 1 {
        return Ok(());
    }

    groups.sort_by(|a, b| b.frame_ids.len().cmp(&a.frame_ids.len()));

    for g in &groups {
        tracing::debug!(
            binning_x = g.key.0,
            binning_y = g.key.1,
            count = g.frame_ids.len(),
            filename = g.example_filename.as_deref().unwrap_or("<unknown>"),
            "binning group"
        );
    }

    let parts: Vec<String> = groups
        .iter()
        .map(|g| format!("{}x{} ({} frames)", g.key.0, g.key.1, g.frame_ids.len()))
        .collect();
    let joined = join_group_parts(&parts);

    bail!(
        "registration: frame set mixes binning {joined}; registration requires \
         uniform binning — split the set or exclude the minority"
    );
}

fn check_focallen_consistency(members: &[MemberInfo]) -> Result<()> {
    /// Relative tolerance for focal-length grouping, per plan T2 (R2).
    const REL_TOLERANCE: f64 = 0.01; // ±1%

    let mut groups: Vec<MismatchGroup<f64>> = Vec::new();

    for m in members {
        let Some(focallen) = m.frame.focallen else {
            tracing::debug!(frame_id = m.frame_id, "NULL focal length, excluded from consistency check");
            continue;
        };

        let existing = groups.iter_mut().find(|g| {
            let denom = if g.key.abs() > f64::EPSILON {
                g.key.abs()
            } else {
                1.0
            };
            ((focallen - g.key).abs() / denom) <= REL_TOLERANCE
        });

        match existing {
            Some(g) => g.frame_ids.push(m.frame_id),
            None => groups.push(MismatchGroup {
                key: focallen,
                frame_ids: vec![m.frame_id],
                example_filename: filename_from_path(&m.path),
            }),
        }
    }

    if groups.len() <= 1 {
        return Ok(());
    }

    groups.sort_by(|a, b| b.frame_ids.len().cmp(&a.frame_ids.len()));

    for g in &groups {
        tracing::debug!(
            focallen_mm = g.key,
            count = g.frame_ids.len(),
            filename = g.example_filename.as_deref().unwrap_or("<unknown>"),
            "focal-length group"
        );
    }

    let parts: Vec<String> = groups
        .iter()
        .map(|g| format!("{:.1}mm ({} frames)", g.key, g.frame_ids.len()))
        .collect();
    let joined = join_group_parts(&parts);

    bail!(
        "registration: frame set mixes focal length {joined}; registration requires \
         uniform focal length — split the set or exclude the minority"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::registration::db::get_registration_for_frame_set;
    use solvemyastro::WcsSolution;

    // ── synthetic fixture helpers ───────────────────────────────────────────
    //
    // Mirrors solvemyastro's own `register()` test fixtures (see
    // `solvemyastro/src/register.rs::tests`) so this test exercises the real
    // `solvemyastro::register` primitive — not a mock — with a hand-built
    // star field, rather than requiring a real on-disk mirrored FITS frame.

    /// Deterministic irregular pseudo-random star field (LCG). Grids are
    /// pathological for quad matching — every 4-subset shares the same ratio
    /// fingerprint — so an irregular field is required for `register` to
    /// converge reliably.
    fn irregular_stars(n: usize, seed: u64, width: f64, height: f64) -> Vec<(f64, f64)> {
        let mut state = seed;
        let mut next = || -> f64 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f64) / ((1u64 << 31) as f64)
        };
        (0..n).map(|_| (next() * width, next() * height)).collect()
    }

    /// 1"/px, no rotation, centred at (ra0, dec0).
    fn test_wcs(ra0: f64, dec0: f64) -> WcsSolution {
        let s = 1.0 / 3600.0;
        WcsSolution {
            crpix: (2000.0, 1500.0),
            crval: (ra0, dec0),
            cd: [[-s, 0.0], [0.0, s]],
            sip_forward: None,
            sip_reverse: None,
        }
    }

    /// Invert a similarity transform `(a1,b1,c1,a2,b2,c2)` mapping sub→ref:
    /// given `ref_pts`, return the sub-frame coordinates that map to them.
    fn invert_similarity(
        a1: f64,
        b1: f64,
        c1: f64,
        a2: f64,
        b2: f64,
        c2: f64,
        ref_pts: &[(f64, f64)],
    ) -> Vec<(f64, f64)> {
        let det = a1 * b2 - b1 * a2;
        let inv = 1.0 / det;
        ref_pts
            .iter()
            .map(|&(rx, ry)| {
                let dx = rx - c1;
                let dy = ry - c2;
                let sx = inv * (b2 * dx - b1 * dy);
                let sy = inv * (-a2 * dx + a1 * dy);
                (sx, sy)
            })
            .collect()
    }

    /// Build a (ref_wcs, ref_pts, sub_pts) fixture: 40-star irregular field,
    /// a known 2°-rotation + 0.3%-scale + small-translation sub→ref
    /// transform. `mirror` additionally reflects the sub detections about
    /// the frame width to simulate a meridian-flipped sub, exactly like
    /// solvemyastro's `mirrored_frame_sets_flipped_flag` test.
    fn build_fixture(mirror: bool) -> (WcsSolution, Vec<(f64, f64)>, Vec<(f64, f64)>) {
        let width = 4096.0;
        let height = 3072.0;
        let ref_wcs = test_wcs(83.0, -5.0);
        let ref_pts = irregular_stars(40, 0x1234_5678_9ABC_DEF0, width, height);

        let theta = 2.0_f64.to_radians();
        let s = 1.003_f64;
        let (sin_t, cos_t) = (theta.sin(), theta.cos());
        let sub_pts = invert_similarity(
            s * cos_t,
            -s * sin_t,
            5.0,
            s * sin_t,
            s * cos_t,
            -3.0,
            &ref_pts,
        );

        let sub_pts = if mirror {
            sub_pts.iter().map(|&(x, y)| (width - 1.0 - x, y)).collect()
        } else {
            sub_pts
        };

        (ref_wcs, ref_pts, sub_pts)
    }

    // ── pre-registration consistency gate (R2) ──────────────────────────────

    /// Build a fake `MemberInfo` for the consistency-gate tests — no DB, no
    /// on-disk file; `detections`/`detection_count` are irrelevant to the
    /// gate and left empty/zero.
    fn fake_member(
        frame_id: i64,
        xbinning: Option<i32>,
        ybinning: Option<i32>,
        focallen: Option<f64>,
        filename: &str,
    ) -> MemberInfo {
        MemberInfo {
            frame_id,
            frame: Frame {
                xbinning,
                ybinning,
                focallen,
                ..Frame::default()
            },
            path: filename.to_string(),
            detections: Vec::new(),
            detection_count: 0,
        }
    }

    /// Required test #1 (plan T2): two fake members, 1×1 and 2×2 binning —
    /// must bail and the message must mention both groups (and their sizes,
    /// matching the plan's verbatim example: "1x1 (42 frames) and 2x2 (8
    /// frames)").
    #[test]
    fn mixed_binning_bails_and_mentions_both_groups() {
        let mut members: Vec<MemberInfo> = Vec::new();
        for i in 0..42 {
            members.push(fake_member(
                i,
                Some(1),
                Some(1),
                Some(600.0),
                &format!("light_{i:03}.fits"),
            ));
        }
        for i in 42..50 {
            members.push(fake_member(
                i,
                Some(2),
                Some(2),
                Some(600.0),
                &format!("light_{i:03}.fits"),
            ));
        }

        let err =
            check_registration_consistency(&members).expect_err("mixed binning must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("1x1"),
            "message must mention the 1x1 group: {msg}"
        );
        assert!(
            msg.contains("2x2"),
            "message must mention the 2x2 group: {msg}"
        );
        assert!(
            msg.contains("42 frames"),
            "message must mention the majority group size: {msg}"
        );
        assert!(
            msg.contains("8 frames"),
            "message must mention the minority group size: {msg}"
        );
        assert!(
            msg.contains("uniform binning"),
            "message must name the failing check: {msg}"
        );
    }

    /// Required test #2 (plan T2): a uniform set (same binning, same focal
    /// length) must pass.
    #[test]
    fn uniform_set_passes() {
        let members: Vec<MemberInfo> = (0..10)
            .map(|i| fake_member(i, Some(1), Some(1), Some(600.0), &format!("f{i}.fits")))
            .collect();
        assert!(check_registration_consistency(&members).is_ok());
    }

    /// Required test #3 (plan T2): NULL binning must pass under the 1×1
    /// assumption (not be treated as its own mismatching group).
    #[test]
    fn null_binning_passes_with_1x1_assumption() {
        let members = vec![
            fake_member(1, None, None, Some(600.0), "a.fits"),
            fake_member(2, Some(1), Some(1), Some(600.0), "b.fits"),
        ];
        assert!(check_registration_consistency(&members).is_ok());
    }

    /// Focal length within the ±1% tolerance must pass (600.0 vs 605.0 is
    /// ~0.83% — inside the 1% band).
    #[test]
    fn focallen_within_one_percent_tolerance_passes() {
        let members = vec![
            fake_member(1, Some(1), Some(1), Some(600.0), "a.fits"),
            fake_member(2, Some(1), Some(1), Some(605.0), "b.fits"),
        ];
        assert!(check_registration_consistency(&members).is_ok());
    }

    /// Focal length beyond the ±1% tolerance must bail with the same message
    /// shape as the binning gate, naming "focal length".
    #[test]
    fn focallen_beyond_one_percent_bails() {
        let members = vec![
            fake_member(1, Some(1), Some(1), Some(600.0), "a.fits"),
            fake_member(2, Some(1), Some(1), Some(750.0), "b.fits"),
        ];
        let err = check_registration_consistency(&members)
            .expect_err("mismatched focal length must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("focal length"), "{msg}");
        assert!(msg.contains("uniform focal length"), "{msg}");
    }

    /// NULL focal length must be excluded from the check entirely — it must
    /// not itself form a mismatching singleton group.
    #[test]
    fn null_focallen_excluded_from_check() {
        let members = vec![
            fake_member(1, Some(1), Some(1), None, "a.fits"),
            fake_member(2, Some(1), Some(1), Some(600.0), "b.fits"),
            fake_member(3, Some(1), Some(1), Some(600.5), "c.fits"),
        ];
        assert!(
            check_registration_consistency(&members).is_ok(),
            "a NULL focal length must not itself trigger a mismatch"
        );
    }

    #[test]
    fn aligned_status_maps_flip_flag() {
        assert_eq!(aligned_status(false), "aligned");
        assert_eq!(aligned_status(true), "aligned_flipped");
    }

    /// A meridian-flipped sub (mirrored detections, real `solvemyastro::register`
    /// call) must persist `status = "aligned_flipped"` through the exact
    /// `RegistrationRecord` / `upsert_registration` path used by
    /// `register_frame_set`'s two "aligned" sites.
    #[test]
    fn flipped_registration_persists_aligned_flipped_status() {
        let (ref_wcs, ref_pts, mirrored_sub_pts) = build_fixture(true);

        let cfg = SolveConfig::default();
        let reg = solvemyastro::register(&ref_wcs, &ref_pts, &mirrored_sub_pts, &cfg)
            .expect("register should succeed on a mirrored (meridian-flipped) synthetic field");
        assert!(reg.flipped, "fixture must produce a flipped registration");

        let status = aligned_status(reg.flipped);
        assert_eq!(status, "aligned_flipped");

        // Persist through the real DB helper, exactly like the service does
        // at its "aligned" sites, and read it back.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format, created_at)
             VALUES (1, '/x/ref.fits', 'ref.fits', 0, '2025-01-01', 'FITS', '2025-01-01'),
                    (2, '/x/sub.fits', 'sub.fits', 0, '2025-01-01', 'FITS', '2025-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, imagetyp) VALUES (1, 1, 'LIGHT'), (2, 2, 'LIGHT')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO frames_set (id, name) VALUES (10, 'TestSet')",
            [],
        )
        .unwrap();

        let rec = RegistrationRecord {
            id: None,
            frames_set_id: 10,
            frame_id: 2,
            reference_frame_id: 1,
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
            rms_residual_arcsec: None,
            status: status.to_string(),
            error: None,
            compute_time_ms: 0,
            registered_at: "2026-01-01 00:00:00".to_string(),
        };
        upsert_registration(&conn, &rec).unwrap();

        let rows = get_registration_for_frame_set(&conn, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].status, "aligned_flipped",
            "persisted status must be aligned_flipped for a flipped registration"
        );
    }

    /// The unmodified (non-mirrored) fixture must still map to plain
    /// "aligned" — the flip flag must not fire spuriously.
    #[test]
    fn non_flipped_registration_maps_to_aligned_status() {
        let (ref_wcs, ref_pts, sub_pts) = build_fixture(false);

        let cfg = SolveConfig::default();
        let reg = solvemyastro::register(&ref_wcs, &ref_pts, &sub_pts, &cfg)
            .expect("register should succeed on the non-mirrored synthetic field");
        assert!(
            !reg.flipped,
            "non-mirrored fixture must not be flagged as flipped"
        );
        assert_eq!(aligned_status(reg.flipped), "aligned");
    }
}

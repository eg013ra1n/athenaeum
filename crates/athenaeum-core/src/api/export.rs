//! The blocking phase of a WBPP export: admission, plan resolution, and the
//! file placement itself.
//!
//! Both transports call [`run_export_organize`] from inside
//! `tokio::task::spawn_blocking`. It parks on a condvar waiting for a compute
//! slot and then streams pixels for minutes, so running it on an async worker
//! would block that worker for the whole export.
//!
//! Everything BEFORE it (readiness gate, mode transform) stays in the command
//! wrappers, which also own the `ExportResult`/event shaping — this module owns
//! only the part that must not diverge between the two backends.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::api::{db, ApiError};
use crate::events::ProgressEmitter;
use crate::export::file_organizer::{
    organize_files_wbpp, zip_masters_for_export, GenerationBatch, OrganizeResult,
};
use crate::export::ZipMasters;
use crate::export::models::{CalibratedLightOptions, ExportData, ExportMode, WbppExportConfig};
use crate::services::compute_queue::ComputeJobKind;
use crate::services::ServiceContext;

/// What the blocking phase produced.
pub enum ExportRunOutcome {
    /// The organizer ran; its per-file warnings are inside.
    Organized(OrganizeResult),
    /// Cancelled while queued for a compute slot — nothing was written, and the
    /// caller turns this into its cancelled `ExportResult`.
    Cancelled,
}

/// Admission → plan resolution → placement, in that order.
///
/// The ORDER is the point: the compute slot is acquired FIRST, and the catalog
/// connection is borrowed *inside* it and dropped before any pixel work.
/// Resolving before the slot would hold a pooled connection for the whole queue
/// wait — which can be an entire master build or another export — for no reason
/// at all. `api::sync_prepare::open_generation` follows the same order for a
/// transfer's package.
///
/// A slot is taken only when the run actually generates files: the mode is
/// calibrated-lights AND at least one light is marked for generation (an
/// attested set's lights are copied as they are, so it takes none). A plain
/// copy export is not CPU work and must not queue behind master builds.
#[allow(clippy::too_many_arguments)]
pub fn run_export_organize(
    ctx: &ServiceContext,
    output_dir: &Path,
    data: &ExportData,
    use_symlinks: bool,
    config: &WbppExportConfig,
    emitter: Option<&dyn ProgressEmitter>,
    frame_set_id: i64,
    cancel_flag: &Arc<AtomicBool>,
    mode: ExportMode,
    gen_opts: &CalibratedLightOptions,
) -> Result<ExportRunOutcome, ApiError> {
    // The organizer's own marker: a light is calibrated into place exactly
    // when `debayer_calibrated` is `Some` (`PlacementSource::CalibrateLight`).
    let generates = mode == ExportMode::CalibratedLights
        && data
            .groups
            .iter()
            .flat_map(|g| g.subgroups.iter())
            .flat_map(|sg| sg.frames.iter())
            .any(|f| f.debayer_calibrated.is_some());

    // Admission. `acquire` fails only when THIS flag was raised — by
    // `cancel_export`, or by the queue's own cancel, which sets the very flag it
    // was handed — so a cancelled ticket means the export is already cancelled.
    let _permit = if generates {
        match ctx.compute_queue.acquire(
            ComputeJobKind::LightCalibration,
            &format!("Export — calibrate lights (set {frame_set_id})"),
            cancel_flag.clone(),
        ) {
            Ok((permit, _job_id)) => Some(permit),
            Err(_cancelled) => {
                tracing::info!(
                    frame_set_id,
                    "export cancelled while queued for a compute slot"
                );
                return Ok(ExportRunOutcome::Cancelled);
            }
        }
    } else {
        None
    };

    // Masters inside a zip are extracted BEFORE the catalog borrow below: a connection is never held across
    // an extraction. They stay pinned in the cache until the run ends (`zip_masters` drops last).
    let zip_masters = if generates {
        match zip_masters_for_export(ctx, data, emitter, frame_set_id, cancel_flag.as_ref()) {
            Ok(found) => found,
            Err(e) => {
                tracing::error!(frame_set_id, error = %format!("{e:#}"), "export: masters inside a zip could not be listed");
                return Err(ApiError::Internal(format!("Failed to read the calibration masters: {e:#}")));
            }
        }
    } else {
        ZipMasters::none()
    };
    if generates && cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::info!(frame_set_id, "export cancelled while extracting masters from zips");
        return Ok(ExportRunOutcome::Cancelled);
    }

    // Catalog phase: one borrow resolves every marked light's plan, and the
    // guard is dropped with this block — the pixel phase below holds no
    // database connection. Not free, and not purely a catalog cost: resolving a
    // light's flat-norm divisor reads the master flat's plane when the constant
    // is not on a card. `GenerationBatch::resolve` memoizes that per (flat,
    // mosaic phase) so the batch pays it once, but the FIRST such frame still
    // reads a whole flat while this connection is held.
    let mut generation = if generates {
        let db = db(ctx)?;
        let conn = db.conn();
        Some(GenerationBatch::resolve(
            &conn,
            data,
            gen_opts.clone(),
            std::env::temp_dir(),
            &zip_masters,
        ))
    } else {
        None
    };

    let result = organize_files_wbpp(
        output_dir,
        data,
        use_symlinks,
        config,
        emitter,
        frame_set_id,
        cancel_flag.as_ref(),
        generation.as_mut(),
        Some(&ctx.image_pool),
        Some(ctx),
    )
    .map_err(|e| ApiError::Internal(format!("Failed to organize files: {e:#}")))?;
    Ok(ExportRunOutcome::Organized(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::models::{CalibrationSummary, CameraType, ExportGroup, MasterCreationPlan};
    use std::sync::atomic::Ordering;

    fn ctx_with(dir: &Path) -> ServiceContext {
        let ctx = ServiceContext::new_for_tests(dir.join("catalog.db"));
        crate::db::schema::init_db(&ctx.db.get().unwrap().conn()).unwrap();
        ctx
    }

    /// A frame set with no frames: the organizer has nothing to place, which is
    /// exactly what these tests want — they are about admission and ordering,
    /// not about pixels.
    fn empty_data() -> ExportData {
        ExportData {
            frame_set_id: 1,
            frame_set_name: "Set".to_string(),
            object_name: None,
            groups: vec![ExportGroup {
                group_key: "g".to_string(),
                filter: None,
                camera_type: CameraType::Mono,
                display_name: "g".to_string(),
                subgroups: Vec::new(),
                total_frames: 0,
                total_exposure: 0.0,
                warnings: Vec::new(),
            }],
            master_plan: MasterCreationPlan {
                masters: Vec::new(),
                master_paths: std::collections::HashMap::new(),
            },
            filters: Vec::new(),
            calibration_summary: CalibrationSummary {
                flat_count: 0,
                dark_count: 0,
                bias_count: 0,
                dark_flat_count: 0,
                flats_complete: true,
                darks_complete: true,
                bias_complete: true,
                warnings: Vec::new(),
            },
            total_light_frames: 0,
            total_exposure_seconds: 0.0,
        }
    }

    /// One light marked for generation, as the calibrated-lights transform
    /// leaves it — the only shape that takes a compute slot.
    fn marked_data() -> ExportData {
        let mut data = empty_data();
        data.groups[0]
            .subgroups
            .push(crate::export::models::CalibrationSubgroup {
                subgroup_key: "s".to_string(),
                display_name: "s".to_string(),
                frames: vec![crate::export::models::ExportFrame {
                    frame_id: 1,
                    file_id: 1,
                    file_path: "/nonexistent/L_1.fits".to_string(),
                    filename: "c_L_1.fits".to_string(),
                    exptime: None,
                    filter: None,
                    ccd_temp: None,
                    gain: None,
                    offset: None,
                    binning: None,
                    date_obs: None,
                    focallen: None,
                    xpixsz: None,
                    bayerpat: None,
                    instrume: None,
                    debayer_calibrated: Some(false),
                }],
                flat: None,
                dark: None,
                bias: None,
                warnings: Vec::new(),
            });
        data
    }

    /// A cancel raised before admission comes back as `Cancelled` — never as an
    /// organizer run, and never as an `Err` the caller would report as a
    /// failure. This is the path both host commands turn into the cancelled
    /// `ExportResult`, which still has to reach their completion event.
    #[test]
    fn cancel_before_admission_yields_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        let out = tempfile::tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(true));

        let outcome = run_export_organize(
            &ctx,
            out.path(),
            &marked_data(),
            false,
            &WbppExportConfig::default(),
            None,
            1,
            &cancel,
            ExportMode::CalibratedLights,
            &CalibratedLightOptions::default(),
        )
        .unwrap();
        assert!(matches!(outcome, ExportRunOutcome::Cancelled));
        assert!(cancel.load(Ordering::SeqCst));
    }

    /// A copy-only mode takes no compute slot: it runs to completion even while
    /// the queue's only slot is held. Pinned because taking one would make every
    /// plain export queue behind master builds for no reason.
    #[test]
    fn copy_mode_does_not_wait_for_a_compute_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        let out = tempfile::tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));

        // Occupy the only slot (default max_concurrent = 1) for the whole test.
        let (_held, _id) = ctx
            .compute_queue
            .acquire(
                ComputeJobKind::MasterBuild,
                "held",
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();

        let outcome = run_export_organize(
            &ctx,
            out.path(),
            &empty_data(),
            false,
            &WbppExportConfig::default(),
            None,
            1,
            &cancel,
            ExportMode::LightsOnly,
            &CalibratedLightOptions::default(),
        )
        .unwrap();
        match outcome {
            ExportRunOutcome::Organized(r) => assert_eq!(r.files_organized, 0),
            ExportRunOutcome::Cancelled => panic!("a copy export must not queue"),
        }
    }

    /// An attested set's calibrated-lights export marks no light for
    /// generation, so it is a plain copy: it takes no compute slot and runs
    /// to completion while the queue's only slot is held.
    #[test]
    fn an_attested_calibrated_lights_export_takes_no_compute_slot() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        let out = tempfile::tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (_held, _id) = ctx
            .compute_queue
            .acquire(
                ComputeJobKind::MasterBuild,
                "held",
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();

        // No light carries `debayer_calibrated`: what the transform leaves for
        // an attested set.
        let outcome = run_export_organize(
            &ctx,
            out.path(),
            &empty_data(),
            false,
            &WbppExportConfig::default(),
            None,
            1,
            &cancel,
            ExportMode::CalibratedLights,
            &CalibratedLightOptions::default(),
        )
        .unwrap();
        assert!(
            matches!(outcome, ExportRunOutcome::Organized(_)),
            "an attested export must not queue"
        );
    }
}

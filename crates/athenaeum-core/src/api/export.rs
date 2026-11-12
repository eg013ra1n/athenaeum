//! The blocking phase of a WBPP export: admission, plan resolution, and the
//! file placement itself.
//!
//! Both transports call [`run_export_organize`] from inside
//! `tokio::task::spawn_blocking`. It parks on a condvar waiting for a compute
//! slot and then streams pixels for minutes, so running it on an async worker
//! would block that worker for the whole export.
//!
//! [`export_to_wbpp`] is the whole command body both hosts wrap: cancel-handle
//! registration, the `export-progress` / `export-complete` events, collection,
//! the readiness gate and mode transform, this blocking phase, and the
//! `ExportResult` shaping. The WBPP config get / set / reset live here too.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::api::{db, ApiError};
use crate::events::{emit_event, ProgressEmitter};
use crate::export::file_organizer::{
    organize_files_wbpp, zip_masters_for_export, GenerationBatch, OrganizeResult,
};
use crate::export::ZipMasters;
use crate::export::models::{
    CalibratedLightOptions, CalibrationRoute, ExportCompleteEvent, ExportData, ExportMode, ExportProgressEvent,
    ExportResult, ExportSummary, WbppExportConfig,
};
use crate::export::{apply_export_mode, collect_export_data, resolve_export_mode};
use crate::fits_writer::OutputFormat;
use crate::services::compute_queue::ComputeJobKind;
use crate::services::{ExportHandle, ServiceContext};

use crate::api::lights::{
    check_mode_ready, get_export_readiness as api_get_export_readiness, FlatNormMode,
    LightCalParams,
};

const WBPP_CONFIG_KEY: &str = "export.wbpp_config";

/// Load WBPP config from the catalog or return the default. A stored value
/// that does not parse is an error (and a warning in the log).
pub fn load_wbpp_config(conn: &rusqlite::Connection) -> Result<WbppExportConfig, ApiError> {
    let result: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![WBPP_CONFIG_KEY],
            |row| row.get(0),
        )
        .ok();

    match result {
        Some(json) => serde_json::from_str(&json).map_err(|e| {
            tracing::warn!(error = %e, "failed to parse WBPP config");
            ApiError::Internal(e.to_string())
        }),
        None => Ok(WbppExportConfig::default()),
    }
}

/// WBPP export configuration (the default when none is saved).
pub fn get_wbpp_export_config(ctx: &ServiceContext) -> Result<WbppExportConfig, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    load_wbpp_config(&conn)
}

/// Save the WBPP export configuration and return it.
pub fn set_wbpp_export_config(
    ctx: &ServiceContext,
    config: WbppExportConfig,
) -> Result<WbppExportConfig, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    let json = serde_json::to_string(&config).map_err(|e| ApiError::Internal(e.to_string()))?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![WBPP_CONFIG_KEY, json],
    )
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(config)
}

/// Drop the saved WBPP export configuration and return the default.
pub fn reset_wbpp_export_config(ctx: &ServiceContext) -> Result<WbppExportConfig, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();

    conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        rusqlite::params![WBPP_CONFIG_KEY],
    )
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(WbppExportConfig::default())
}

/// Preview data for a frame set: every light frame with its calibrations.
pub fn get_export_preview(ctx: &ServiceContext, frame_set_id: i64) -> Result<ExportData, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    collect_export_data(&conn, frame_set_id).map_err(|e| ApiError::Internal(e.to_string()))
}

/// The frame sets that can be exported.
pub fn get_exportable_frame_sets(
    ctx: &ServiceContext,
) -> Result<Vec<crate::export::frame_set_queries::ExportableFrameSet>, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    crate::export::frame_set_queries::get_exportable_frame_sets(&conn)
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// The export groups and their calibration trees, for the UI.
pub fn get_calibration_route(
    ctx: &ServiceContext,
    frame_set_id: i64,
) -> Result<CalibrationRoute, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    crate::export::frame_set_queries::get_calibration_route(&conn, frame_set_id)
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Enhanced export summary for the Export tab: equipment, filter groups with
/// exposure breakdown, calibration details with match quality, the folder
/// structure preview and detailed warnings.
///
/// Drawn for the mode the tab has selected — the tree, file total and size
/// estimate describe THAT export. `export_mode` `None` falls back to the
/// persisted config like `export_to_wbpp`; the six generation options are read
/// by `calibratedLights` only (the debayer flag decides the `c_*` names in the
/// tree) and default the same way as the export's.
#[allow(clippy::too_many_arguments)]
pub fn get_export_summary(
    ctx: &ServiceContext,
    frame_set_id: i64,
    export_mode: Option<ExportMode>,
    flat_norm: Option<bool>,
    flat_norm_mode: Option<FlatNormMode>,
    params: Option<LightCalParams>,
    hot_pixel: Option<bool>,
    debayer: Option<bool>,
    format: Option<OutputFormat>,
) -> Result<ExportSummary, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let config = load_wbpp_config(&conn).unwrap_or_default();
    let mode = resolve_export_mode(export_mode, &config);
    let gen_opts = CalibratedLightOptions::resolve(
        flat_norm,
        flat_norm_mode,
        params,
        hot_pixel,
        debayer,
        format,
    );

    crate::export::collect_export_summary(&conn, frame_set_id, &config, mode, Some(&gen_opts))
        .map_err(|e| ApiError::Internal(e.to_string()))
}

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

/// The web host's sandbox for an export's output directory: a lexical
/// `starts_with` against its configured export directory (no canonicalize, no
/// normalisation). `None` — the desktop — never refuses.
fn check_export_dir(output_path: &Path, allowed_export_dir: Option<&Path>) -> Result<(), String> {
    match allowed_export_dir {
        Some(export_dir) if !output_path.starts_with(export_dir) => Err(format!(
            "Export path must be within {}",
            export_dir.display()
        )),
        _ => Ok(()),
    }
}

/// Export a frame set to the WBPP folder structure.
///
/// `export_mode` is an explicit per-invocation override; `None` falls back to
/// the persisted [`WbppExportConfig`]'s mode. `flat_norm` / `flat_norm_mode` /
/// `params` / `hot_pixel` / `debayer` / `format` are the calibrated-lights
/// GENERATION options: an omitted one takes [`CalibratedLightOptions::default`]'s
/// value for that field, and every other mode ignores them.
///
/// `allowed_export_dir` is the web host's configured export directory: an
/// output directory outside it is refused with a failed [`ExportResult`] (the
/// refusal still finishes the export and emits `export-complete`). The desktop
/// passes `None`.
///
/// Every outcome — a collection/gate failure, a queued-then-cancelled
/// compute-slot wait, a join error or a normal finish — is folded into one
/// result and passed through the single `finish_export`, so none of them can
/// strand the `active_exports` entry or skip `export-complete`.
#[allow(clippy::too_many_arguments)]
pub async fn export_to_wbpp(
    ctx: Arc<ServiceContext>,
    emitter: Arc<dyn ProgressEmitter>,
    frame_set_id: i64,
    output_dir: String,
    use_symlinks: bool,
    export_mode: Option<ExportMode>,
    flat_norm: Option<bool>,
    flat_norm_mode: Option<FlatNormMode>,
    params: Option<LightCalParams>,
    hot_pixel: Option<bool>,
    debayer: Option<bool>,
    format: Option<OutputFormat>,
    allowed_export_dir: Option<&Path>,
) -> ExportResult {
    // Create cancel flag and register export
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut exports = ctx.active_exports.lock().unwrap();
        exports.insert(
            frame_set_id,
            ExportHandle {
                cancel_flag: cancel_flag.clone(),
            },
        );
    }

    // Emit collecting phase
    emit_event(
        emitter.as_ref(),
        "export-progress",
        &ExportProgressEvent {
            frame_set_id,
            current: 0,
            total: 0,
            percent: 0.0,
            current_file: None,
            phase: "collecting".to_string(),
        },
    );

    // Collect export data and config. A DB failure here must still route
    // through `finish_export` — the export was just registered above, so a
    // bare `?` would strand the `active_exports` entry and skip
    // `export-complete`.
    let collected: Result<(ExportData, WbppExportConfig), String> = (|| {
        let db = ctx
            .db
            .get()
            .ok_or_else(|| "Database not initialized".to_string())?;
        let conn = db.conn();
        let data = collect_export_data(&conn, frame_set_id).map_err(|e| e.to_string())?;
        let cfg = load_wbpp_config(&conn).unwrap_or_default();
        Ok((data, cfg))
    })();

    let output_path = PathBuf::from(&output_dir);

    let make_fail = |error: String| ExportResult {
        success: false,
        output_dir: output_dir.clone(),
        files_organized: 0,
        scripts_generated: Vec::new(),
        warnings: Vec::new(),
        error: Some(error),
        linked: 0,
        copied: 0,
        rewritten: 0,
        failed: 0,
        zip_copied: 0,
        link_fallback: Default::default(),
    };

    let result = match collected {
        Err(e) => {
            tracing::error!(frame_set_id, error = %e, "export blocked before collecting data");
            make_fail(e)
        }
        Ok((mut export_data, config)) => {
            // Explicit per-invocation override wins over the persisted config's mode.
            let mode = resolve_export_mode(export_mode, &config);
            // Read by the calibrated-lights mode only; the transform ignores it
            // in the others (its debayer flag decides the output NAMES, so it
            // must be the same value the pixel phase later resolves against).
            // An absent (or `null`) option takes its value from the type that
            // owns the defaults — `resolve` is the one place, shared with the
            // summary preview.
            let gen_opts = CalibratedLightOptions::resolve(
                flat_norm,
                flat_norm_mode,
                params,
                hot_pixel,
                debayer,
                format,
            );

            // Validate the output path against the web host's export directory.
            // A rejection here must still finish the export (same reasoning as
            // the DB failure above) rather than short-circuit past it.
            let path_ok = check_export_dir(&output_path, allowed_export_dir);

            // Strict gate (spec §12.2) + mode transform. `prepare` returns the
            // per-set omission warnings to fold into the final result, or an
            // Err message that aborts the export before any file is written.
            let prepare = |export_data: &mut ExportData| -> Result<Vec<String>, String> {
                let readiness =
                    api_get_export_readiness(&ctx, frame_set_id).map_err(|e| e.to_string())?;
                if let Err(msg) = check_mode_ready(&readiness, mode) {
                    tracing::warn!(frame_set_id, ?mode, error = %msg, "export refused: mode not ready");
                    return Err(msg);
                }
                let db = ctx
                    .db
                    .get()
                    .ok_or_else(|| "Database not initialized".to_string())?;
                let conn = db.conn();
                apply_export_mode(&conn, export_data, mode, Some(&gen_opts))
                    .map_err(|e| e.to_string())
            };

            let cancelled = cancel_flag.load(Ordering::Relaxed);
            if cancelled {
                make_fail("Export cancelled".to_string())
            } else if let Err(e) = path_ok {
                tracing::warn!(frame_set_id, error = %e, "export refused: output directory outside the export directory");
                make_fail(e)
            } else {
                match prepare(&mut export_data) {
                    Err(e) => {
                        // The refusal returns a failed `ExportResult`, so
                        // without this the only log record of a readiness/DB
                        // failure would be the gate's own warn! — and a DB
                        // failure emits none at all.
                        tracing::error!(frame_set_id, error = %e, "export blocked before organizing");
                        make_fail(e)
                    }
                    Ok(mode_warnings) => {
                        // I3: admission (a condvar park) + plan resolution +
                        // the pixel batch must not run on the async runtime
                        // worker — a multi-minute calibration run would block
                        // it for the whole export. `run_export_organize` is
                        // the ONE body both hosts drive from spawn_blocking;
                        // it also owns the I2 ordering (permit acquired
                        // before the per-frame resolve loop, catalog
                        // connection scoped and dropped before any pixel
                        // work — see its doc comment).
                        let ctx_bg = ctx.clone();
                        let emitter_bg = emitter.clone();
                        let cancel_flag_bg = cancel_flag.clone();
                        let output_path_bg = output_path.clone();
                        let config_bg = config.clone();
                        let gen_opts_bg = gen_opts.clone();

                        let joined = tokio::task::spawn_blocking(move || {
                            run_export_organize(
                                &ctx_bg,
                                &output_path_bg,
                                &export_data,
                                use_symlinks,
                                &config_bg,
                                Some(emitter_bg.as_ref()),
                                frame_set_id,
                                &cancel_flag_bg,
                                mode,
                                &gen_opts_bg,
                            )
                        })
                        .await;

                        match joined {
                            Err(join_err) => {
                                tracing::error!(frame_set_id, error = %join_err, "export task panicked");
                                make_fail(format!("Export task panicked: {}", join_err))
                            }
                            Ok(Err(api_err)) => {
                                make_fail(format!("Failed to organize files: {}", api_err))
                            }
                            // Cancelled while queued for a compute slot —
                            // `run_export_organize` never touched the disk.
                            Ok(Ok(ExportRunOutcome::Cancelled)) => ExportResult {
                                success: false,
                                output_dir: output_dir.clone(),
                                files_organized: 0,
                                scripts_generated: Vec::new(),
                                warnings: mode_warnings,
                                error: Some("Export cancelled".to_string()),
                                linked: 0,
                                copied: 0,
                                rewritten: 0,
                                failed: 0,
                                zip_copied: 0,
                                link_fallback: Default::default(),
                            },
                            Ok(Ok(ExportRunOutcome::Organized(org_result))) => {
                                let was_cancelled = cancel_flag.load(Ordering::Relaxed);
                                let mut warnings = org_result.warnings;
                                warnings.extend(mode_warnings);
                                if was_cancelled {
                                    ExportResult {
                                        success: false,
                                        output_dir: output_dir.clone(),
                                        files_organized: org_result.files_organized,
                                        scripts_generated: Vec::new(),
                                        warnings,
                                        error: Some("Export cancelled".to_string()),
                                        linked: 0,
                                        copied: 0,
                                        rewritten: 0,
                                        failed: 0,
                                        zip_copied: org_result.zip_copied,
                                        link_fallback: org_result.link_fallback,
                                    }
                                } else {
                                    ExportResult {
                                        success: true,
                                        output_dir: output_dir.clone(),
                                        files_organized: org_result.files_organized,
                                        scripts_generated: Vec::new(),
                                        warnings,
                                        error: None,
                                        linked: 0,
                                        copied: 0,
                                        rewritten: 0,
                                        failed: 0,
                                        zip_copied: org_result.zip_copied,
                                        link_fallback: org_result.link_fallback,
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    finish_export(&ctx, emitter.as_ref(), frame_set_id, &result);
    result
}

/// Unregister the export and emit its completion event.
fn finish_export(
    ctx: &ServiceContext,
    emitter: &dyn ProgressEmitter,
    frame_set_id: i64,
    result: &ExportResult,
) {
    {
        let mut exports = ctx.active_exports.lock().unwrap();
        exports.remove(&frame_set_id);
    }
    emit_event(
        emitter,
        "export-complete",
        &ExportCompleteEvent {
            frame_set_id,
            success: result.success,
            files_organized: result.files_organized,
            warnings: result.warnings.clone(),
            error: result.error.clone(),
            output_dir: result.output_dir.clone(),
            linked: 0,
            copied: 0,
            rewritten: 0,
            failed: 0,
            zip_copied: result.zip_copied,
            link_fallback: result.link_fallback,
        },
    );
}

/// Raise the cancel flag of the export running for `frame_set_id` (a WBPP export
/// or a collab project export registered under the same key). The running
/// export notices the flag and finishes as cancelled. Nothing running for the
/// key is a `NotFound`.
pub fn cancel_export(ctx: &ServiceContext, frame_set_id: i64) -> Result<(), ApiError> {
    let exports = ctx.active_exports.lock().unwrap();
    match exports.get(&frame_set_id) {
        Some(handle) => {
            handle.cancel_flag.store(true, Ordering::SeqCst);
            Ok(())
        }
        None => Err(ApiError::NotFound(
            "No active export for this frame set".to_string(),
        )),
    }
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

    #[test]
    fn a_saved_wbpp_config_reads_back_and_reset_returns_the_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let ctx = ctx_with(tmp.path());

        let cfg = WbppExportConfig {
            keyword_order: vec!["FLAT".to_string(), "CAMERA".to_string()],
            ..WbppExportConfig::default()
        };
        set_wbpp_export_config(&ctx, cfg).unwrap();
        assert_eq!(
            get_wbpp_export_config(&ctx).unwrap().keyword_order,
            vec!["FLAT".to_string(), "CAMERA".to_string()]
        );

        let reset = reset_wbpp_export_config(&ctx).unwrap();
        assert_eq!(reset.keyword_order, WbppExportConfig::default().keyword_order);
        assert_eq!(
            get_wbpp_export_config(&ctx).unwrap().keyword_order,
            WbppExportConfig::default().keyword_order
        );
    }

    #[test]
    fn an_output_directory_outside_the_allowed_export_dir_is_refused() {
        let allowed = Path::new("/data/exports");
        assert!(check_export_dir(Path::new("/data/exports/m31"), Some(allowed)).is_ok());
        let err = check_export_dir(Path::new("/elsewhere/m31"), Some(allowed)).unwrap_err();
        assert_eq!(err, "Export path must be within /data/exports");
        // A sibling that only shares a string prefix is outside (component-wise).
        assert!(check_export_dir(Path::new("/data/exports_old/m31"), Some(allowed)).is_err());
        // No configured directory: nothing is refused on this ground.
        assert!(check_export_dir(Path::new("/elsewhere/m31"), None).is_ok());
    }

    #[test]
    fn cancel_export_without_an_active_export_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        match cancel_export(&ctx, 7) {
            Err(ApiError::NotFound(m)) => assert_eq!(m, "No active export for this frame set"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn cancel_export_raises_the_cancel_flag_of_the_active_export() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        let flag = Arc::new(AtomicBool::new(false));
        ctx.active_exports.lock().unwrap().insert(
            7,
            ExportHandle {
                cancel_flag: flag.clone(),
            },
        );
        cancel_export(&ctx, 8).expect_err("another frame set is not running");
        assert!(!flag.load(Ordering::SeqCst));
        cancel_export(&ctx, 7).expect("active export");
        assert!(flag.load(Ordering::SeqCst));
    }
}

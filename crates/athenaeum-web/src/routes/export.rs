// Export route handlers for the Axum web API.
//
// Mirrors the Tauri export commands.  Progress events are broadcast via SSE
// instead of Tauri's emit mechanism.

use athenaeum_core::events::{emit_event, ProgressEmitter};
use athenaeum_core::export::{collect_export_data, collect_export_summary, organize_files_wbpp};
use athenaeum_core::export::models::{
    CalibrationRoute, CalibrationRouteGroup, CalibrationRouteSummary, CalibrationTreeNode,
    ExportCompleteEvent, ExportData, ExportProgressEvent, ExportResult, ExportSummary,
    WbppExportConfig,
};
use athenaeum_core::services::ExportHandle;
use axum::{extract::State, http::StatusCode, Json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::events::SseProgressEmitter;
use crate::WebAppState;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn db_err(msg: impl std::fmt::Display) -> (StatusCode, String) {
    let s = msg.to_string();
    eprintln!("export error: {}", s);
    (StatusCode::INTERNAL_SERVER_ERROR, s)
}

fn no_db() -> (StatusCode, String) {
    eprintln!("export error: database not initialized");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Database not initialized".to_string(),
    )
}

const WBPP_CONFIG_KEY: &str = "export.wbpp_config";

fn load_wbpp_config(conn: &rusqlite::Connection) -> Result<WbppExportConfig, String> {
    let result: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![WBPP_CONFIG_KEY],
            |row| row.get(0),
        )
        .ok();

    match result {
        Some(json) => serde_json::from_str(&json).map_err(|e| {
            eprintln!("Failed to parse WBPP config, using default: {}", e);
            e.to_string()
        }),
        None => Ok(WbppExportConfig::default()),
    }
}

// ── Request arg structs ───────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetWbppExportConfigArgs {
    pub config: WbppExportConfig,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSetIdArgs {
    pub frame_set_id: i64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportToWbppArgs {
    pub frame_set_id: i64,
    pub output_dir: String,
    pub use_symlinks: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelExportArgs {
    pub frame_set_id: i64,
}

// ── Response types ────────────────────────────────────────────────────────────

/// Frame set summary for export selection
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportableFrameSet {
    pub id: i64,
    pub name: Option<String>,
    pub total_exposure_seconds: f64,
    pub frame_count: i32,
    pub object_name: Option<String>,
    pub filters: Vec<String>,
}

// ── Config handlers ───────────────────────────────────────────────────────────

/// Get WBPP export configuration (returns default if not set).
pub async fn get_wbpp_export_config(
    State(state): State<WebAppState>,
    Json(_args): Json<serde_json::Value>,
) -> Result<Json<WbppExportConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let config = load_wbpp_config(&conn).map_err(db_err)?;
    Ok(Json(config))
}

/// Save WBPP export configuration to the database.
pub async fn set_wbpp_export_config(
    State(state): State<WebAppState>,
    Json(args): Json<SetWbppExportConfigArgs>,
) -> Result<Json<WbppExportConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let json = serde_json::to_string(&args.config).map_err(db_err)?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        rusqlite::params![WBPP_CONFIG_KEY, json],
    )
    .map_err(db_err)?;

    Ok(Json(args.config))
}

/// Reset WBPP export configuration to defaults.
pub async fn reset_wbpp_export_config(
    State(state): State<WebAppState>,
    Json(_args): Json<serde_json::Value>,
) -> Result<Json<WbppExportConfig>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    conn.execute(
        "DELETE FROM settings WHERE key = ?1",
        rusqlite::params![WBPP_CONFIG_KEY],
    )
    .map_err(db_err)?;

    Ok(Json(WbppExportConfig::default()))
}

// ── Data collection handlers ──────────────────────────────────────────────────

/// Get export preview data for a frame set.
pub async fn get_export_preview(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<ExportData>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let data = collect_export_data(&conn, args.frame_set_id).map_err(db_err)?;
    Ok(Json(data))
}

/// Get a list of available frame sets for export.
pub async fn get_exportable_frame_sets(
    State(state): State<WebAppState>,
    Json(_args): Json<serde_json::Value>,
) -> Result<Json<Vec<ExportableFrameSet>>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let mut stmt = conn
        .prepare(
            "SELECT fs.id, fs.name, fs.total_exp_time,
                    (SELECT COUNT(*) FROM session_members sm
                     JOIN sessions s ON sm.session_id = s.id
                     JOIN imaging_nights i ON s.imaging_night_id = i.id
                     JOIN frames f ON sm.frame_id = f.id
                     WHERE i.frames_set_id = fs.id AND f.imagetyp = 'Light') as frame_count,
                    (SELECT f.object FROM session_members sm
                     JOIN sessions s ON sm.session_id = s.id
                     JOIN imaging_nights i ON s.imaging_night_id = i.id
                     JOIN frames f ON sm.frame_id = f.id
                     WHERE i.frames_set_id = fs.id AND f.object IS NOT NULL
                     LIMIT 1) as object_name,
                    (SELECT GROUP_CONCAT(DISTINCT f.filter) FROM session_members sm
                     JOIN sessions s ON sm.session_id = s.id
                     JOIN imaging_nights i ON s.imaging_night_id = i.id
                     JOIN frames f ON sm.frame_id = f.id
                     WHERE i.frames_set_id = fs.id AND f.filter IS NOT NULL) as filters
             FROM frames_set fs
             ORDER BY fs.id DESC",
        )
        .map_err(db_err)?;

    let frame_sets = stmt
        .query_map([], |row| {
            let filters_str: Option<String> = row.get(5)?;
            let filters = filters_str
                .map(|s| s.split(',').map(|f| f.to_string()).collect())
                .unwrap_or_default();

            Ok(ExportableFrameSet {
                id: row.get(0)?,
                name: row.get(1)?,
                total_exposure_seconds: row.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
                frame_count: row.get(3)?,
                object_name: row.get(4)?,
                filters,
            })
        })
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;

    Ok(Json(frame_sets))
}

/// Get calibration route for UI display.
///
/// Builds a structured view of the export groups and their calibration trees,
/// suitable for displaying in the UI.
pub async fn get_calibration_route(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<CalibrationRoute>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    // Collect export data
    let export_data = collect_export_data(&conn, args.frame_set_id).map_err(db_err)?;

    // Build calibration route from export data
    let mut groups = Vec::new();
    let mut total_lights = 0;
    let mut total_exposure = 0.0;
    let mut unique_calibration_sets = std::collections::HashSet::new();
    let mut all_warnings = Vec::new();

    for group in &export_data.groups {
        let mut calibration_tree = Vec::new();

        // Build calibration tree for this group.
        // Start with Light node and collect its children.
        let mut light_children = Vec::new();

        // Get calibration info from first subgroup
        if let Some(subgroup) = group.subgroups.first() {
            // Flat branch
            if let Some(ref flat) = subgroup.flat {
                unique_calibration_sets.insert(flat.set_id);
                let mut flat_children = Vec::new();

                // DarkFlat under Flat
                if let Some(ref dark_flat) = flat.dark_flat {
                    unique_calibration_sets.insert(dark_flat.set_id);
                    flat_children.push(CalibrationTreeNode {
                        node_type: "DarkFlat".to_string(),
                        label: format!(
                            "DarkFlat Set {} ({} frames)",
                            dark_flat.set_id, dark_flat.frame_count
                        ),
                        set_id: Some(dark_flat.set_id),
                        count: dark_flat.frame_count,
                        children: vec![],
                        warnings: dark_flat.warnings.clone(),
                        is_missing: false,
                        is_shared: false,
                    });
                }

                // Dark under Flat (alternative to DarkFlat)
                if let Some(ref dark) = flat.dark {
                    unique_calibration_sets.insert(dark.set_id);
                    let mut dark_children = Vec::new();

                    if let Some(ref bias) = dark.bias {
                        unique_calibration_sets.insert(bias.set_id);
                        dark_children.push(CalibrationTreeNode {
                            node_type: "Bias".to_string(),
                            label: format!(
                                "Bias Set {} ({} frames)",
                                bias.set_id, bias.frame_count
                            ),
                            set_id: Some(bias.set_id),
                            count: bias.frame_count,
                            children: vec![],
                            warnings: bias.warnings.clone(),
                            is_missing: false,
                            is_shared: false,
                        });
                    }

                    flat_children.push(CalibrationTreeNode {
                        node_type: "Dark".to_string(),
                        label: format!(
                            "Dark Set {} ({} frames)",
                            dark.set_id, dark.frame_count
                        ),
                        set_id: Some(dark.set_id),
                        count: dark.frame_count,
                        children: dark_children,
                        warnings: dark.warnings.clone(),
                        is_missing: false,
                        is_shared: false,
                    });
                }

                // Bias under Flat
                if let Some(ref bias) = flat.bias {
                    unique_calibration_sets.insert(bias.set_id);
                    flat_children.push(CalibrationTreeNode {
                        node_type: "Bias".to_string(),
                        label: format!(
                            "Bias Set {} ({} frames)",
                            bias.set_id, bias.frame_count
                        ),
                        set_id: Some(bias.set_id),
                        count: bias.frame_count,
                        children: vec![],
                        warnings: bias.warnings.clone(),
                        is_missing: false,
                        is_shared: false,
                    });
                }

                light_children.push(CalibrationTreeNode {
                    node_type: "Flat".to_string(),
                    label: format!(
                        "Flat Set {} ({} frames)",
                        flat.set_id, flat.frame_count
                    ),
                    set_id: Some(flat.set_id),
                    count: flat.frame_count,
                    children: flat_children,
                    warnings: flat.warnings.clone(),
                    is_missing: false,
                    is_shared: false,
                });
            }

            // Dark branch (direct for lights)
            if let Some(ref dark) = subgroup.dark {
                unique_calibration_sets.insert(dark.set_id);
                let mut dark_children = Vec::new();

                if let Some(ref bias) = dark.bias {
                    unique_calibration_sets.insert(bias.set_id);
                    dark_children.push(CalibrationTreeNode {
                        node_type: "Bias".to_string(),
                        label: format!(
                            "Bias Set {} ({} frames)",
                            bias.set_id, bias.frame_count
                        ),
                        set_id: Some(bias.set_id),
                        count: bias.frame_count,
                        children: vec![],
                        warnings: bias.warnings.clone(),
                        is_missing: false,
                        is_shared: false,
                    });
                }

                light_children.push(CalibrationTreeNode {
                    node_type: "Dark".to_string(),
                    label: format!(
                        "Dark Set {} ({} frames)",
                        dark.set_id, dark.frame_count
                    ),
                    set_id: Some(dark.set_id),
                    count: dark.frame_count,
                    children: dark_children,
                    warnings: dark.warnings.clone(),
                    is_missing: false,
                    is_shared: false,
                });
            }

            // Bias branch (direct for lights)
            if let Some(ref bias) = subgroup.bias {
                unique_calibration_sets.insert(bias.set_id);
                light_children.push(CalibrationTreeNode {
                    node_type: "Bias".to_string(),
                    label: format!(
                        "Bias Set {} ({} frames)",
                        bias.set_id, bias.frame_count
                    ),
                    set_id: Some(bias.set_id),
                    count: bias.frame_count,
                    children: vec![],
                    warnings: bias.warnings.clone(),
                    is_missing: false,
                    is_shared: false,
                });
            }

            all_warnings.extend(subgroup.warnings.clone());
        }

        // Add missing calibration placeholder nodes
        let has_flat = group
            .subgroups
            .first()
            .and_then(|s| s.flat.as_ref())
            .is_some();
        let has_dark = group
            .subgroups
            .first()
            .and_then(|s| s.dark.as_ref())
            .is_some();

        if !has_flat {
            light_children.push(CalibrationTreeNode {
                node_type: "Flat".to_string(),
                label: "No flat calibration".to_string(),
                set_id: None,
                count: 0,
                children: vec![],
                warnings: vec!["Missing flat calibration".to_string()],
                is_missing: true,
                is_shared: false,
            });
        }

        if !has_dark {
            light_children.push(CalibrationTreeNode {
                node_type: "Dark".to_string(),
                label: "No dark calibration".to_string(),
                set_id: None,
                count: 0,
                children: vec![],
                warnings: vec!["Missing dark calibration".to_string()],
                is_missing: true,
                is_shared: false,
            });
        }

        calibration_tree.push(CalibrationTreeNode {
            node_type: "Light".to_string(),
            label: format!("{} ({} frames)", group.display_name, group.total_frames),
            set_id: None,
            count: group.total_frames,
            children: light_children,
            warnings: group.warnings.clone(),
            is_missing: false,
            is_shared: false,
        });

        total_lights += group.total_frames;
        total_exposure += group.total_exposure;
        all_warnings.extend(group.warnings.clone());

        groups.push(CalibrationRouteGroup {
            name: group.display_name.clone(),
            light_count: group.total_frames,
            total_exposure: group.total_exposure,
            subgroup_count: group.subgroups.len() as i32,
            calibration_tree,
        });
    }

    let summary = CalibrationRouteSummary {
        group_count: export_data.groups.len() as i32,
        total_lights,
        total_exposure,
        unique_calibration_sets: unique_calibration_sets.len() as i32,
        masters_to_create: export_data.master_plan.masters.len() as i32,
        flats_complete: export_data.calibration_summary.flats_complete,
        darks_complete: export_data.calibration_summary.darks_complete,
        bias_complete: export_data.calibration_summary.bias_complete,
        warnings: all_warnings,
    };

    Ok(Json(CalibrationRoute { groups, summary }))
}

/// Get enhanced export summary for the new UI.
pub async fn get_export_summary(
    State(state): State<WebAppState>,
    Json(args): Json<FrameSetIdArgs>,
) -> Result<Json<ExportSummary>, (StatusCode, String)> {
    let db = state.ctx.db.get().ok_or_else(no_db)?;
    let conn = db.conn();

    let config = load_wbpp_config(&conn).unwrap_or_default();
    let summary =
        collect_export_summary(&conn, args.frame_set_id, &config).map_err(db_err)?;
    Ok(Json(summary))
}

// ── Active export handlers ────────────────────────────────────────────────────

/// Export a frame set to PixInsight WBPP folder structure.
///
/// Emits SSE progress events via the broadcast channel while the file
/// organizer runs.  The DB lock is released before the (potentially long)
/// file-copy phase so other handlers can still serve requests.
pub async fn export_to_wbpp(
    State(state): State<WebAppState>,
    Json(args): Json<ExportToWbppArgs>,
) -> Result<Json<ExportResult>, (StatusCode, String)> {
    let frame_set_id = args.frame_set_id;

    // Create cancel flag and register this export
    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut exports = state.ctx.active_exports.lock().unwrap();
        exports.insert(
            frame_set_id,
            ExportHandle {
                cancel_flag: cancel_flag.clone(),
            },
        );
    }

    // Emit "collecting" phase progress
    let emitter = SseProgressEmitter::new(state.event_tx.clone());
    emit_event(
        &emitter,
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

    // Collect export data and config — release the DB lock immediately after
    let (export_data, config) = {
        let db = state.ctx.db.get().ok_or_else(no_db)?;
        let conn = db.conn();

        let data = collect_export_data(&conn, frame_set_id).map_err(db_err)?;
        let cfg = load_wbpp_config(&conn).unwrap_or_default();
        (data, cfg)
    }; // DB lock released here

    let output_path = PathBuf::from(&args.output_dir);

    // Validate output path is within the configured export directory
    if let Some(ref export_dir) = state.export_dir {
        if !output_path.starts_with(export_dir) {
            return Ok(Json(ExportResult {
                success: false,
                output_dir: args.output_dir.clone(),
                files_organized: 0,
                scripts_generated: Vec::new(),
                warnings: Vec::new(),
                error: Some(format!(
                    "Export path must be within {}",
                    export_dir.display()
                )),
            }));
        }
    }

    // Run the file organizer (no DB lock held during file operations)
    let was_cancelled = cancel_flag.load(Ordering::Relaxed);
    let result = if was_cancelled {
        ExportResult {
            success: false,
            output_dir: args.output_dir.clone(),
            files_organized: 0,
            scripts_generated: Vec::new(),
            warnings: Vec::new(),
            error: Some("Export cancelled".to_string()),
        }
    } else {
        match organize_files_wbpp(
            &output_path,
            &export_data,
            args.use_symlinks,
            &config,
            Some(&emitter as &dyn ProgressEmitter),
            frame_set_id,
            &cancel_flag,
        ) {
            Ok(org_result) => {
                let was_cancelled = cancel_flag.load(Ordering::Relaxed);
                if was_cancelled {
                    ExportResult {
                        success: false,
                        output_dir: args.output_dir.clone(),
                        files_organized: org_result.files_organized,
                        scripts_generated: Vec::new(),
                        warnings: org_result.warnings,
                        error: Some("Export cancelled".to_string()),
                    }
                } else {
                    ExportResult {
                        success: true,
                        output_dir: args.output_dir.clone(),
                        files_organized: org_result.files_organized,
                        scripts_generated: Vec::new(),
                        warnings: org_result.warnings,
                        error: None,
                    }
                }
            }
            Err(e) => {
                let msg = format!("Failed to organize files: {}", e);
                eprintln!("export error: {}", msg);
                ExportResult {
                    success: false,
                    output_dir: args.output_dir.clone(),
                    files_organized: 0,
                    scripts_generated: Vec::new(),
                    warnings: Vec::new(),
                    error: Some(msg),
                }
            }
        }
    };

    // Unregister export
    {
        let mut exports = state.ctx.active_exports.lock().unwrap();
        exports.remove(&frame_set_id);
    }

    // Emit completion event
    emit_event(
        &emitter,
        "export-complete",
        &ExportCompleteEvent {
            frame_set_id,
            success: result.success,
            files_organized: result.files_organized,
            warnings: result.warnings.clone(),
            error: result.error.clone(),
            output_dir: result.output_dir.clone(),
        },
    );

    Ok(Json(result))
}

/// Returns the configured export directory path, or null if not set.
pub async fn get_export_dir(
    State(state): State<WebAppState>,
    Json(_args): Json<serde_json::Value>,
) -> Json<Option<String>> {
    Json(state.export_dir.as_ref().map(|p| p.to_string_lossy().to_string()))
}

/// Cancel an active export operation.
pub async fn cancel_export(
    State(state): State<WebAppState>,
    Json(args): Json<CancelExportArgs>,
) -> Result<Json<()>, (StatusCode, String)> {
    let exports = state.ctx.active_exports.lock().unwrap();
    if let Some(handle) = exports.get(&args.frame_set_id) {
        handle.cancel_flag.store(true, Ordering::SeqCst);
        Ok(Json(()))
    } else {
        Err((
            StatusCode::NOT_FOUND,
            format!("No active export for frame set {}", args.frame_set_id),
        ))
    }
}
